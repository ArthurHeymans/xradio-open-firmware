//! Polling receive path for the XR819 packet-DMA RX FIFO.
//!
//! This follows vendor `rxfifo_next_frame` (`0x08af2`),
//! `rx_indication_build_and_send` (`0xb054`), and `rxfifo_release_slot`
//! (`0x08c22`). Accepted frames are published zero-copy: the 16-byte WSM
//! receive header is written into the FIFO slot's existing headroom and the
//! slot remains owned by firmware until the host has read it and the HIF
//! transport hands its token back ([`Radio::reclaim`]).
//!
//! [`Radio`] owns the FIFO cursors ([`Fifo`]), the peer's BlockAck sessions
//! and the frame on its way through decryption.

use crate::crypto::{Crypto, Job, RxKeyEpoch, Started};
use crate::packet_ram;
pub use crate::rx_model::RxToken;
use crate::frame::{BlockAckRequest, Header, QosData};
use crate::rx_ba::RxBlockAck;
use crate::rx_model::{ReleaseAction, RxRing};
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::register_structs;
use tock_registers::registers::{ReadOnly, ReadWrite};

const FIFO_SIZE: u32 = packet_ram::RX_FIFO_LOGICAL_SIZE as u32;
// Vendor `rxfifo_off_to_addr()` wraps slot starts at 0x7000, but the current
// slot remains linearly addressable in the following packet-RAM spill area.
// The ring keeps 0x1000 bytes outside its logical cursor range for this.
const FIFO_STORAGE_SIZE: usize = packet_ram::RX_FIFO_STORAGE_SIZE;
const FIFO_MASK: u32 = 0x0001_fffc;
const FIFO_MAGIC: u32 = 0x00aa_55ff;
const FIFO_RELEASED: u32 = 0xcccc_cc00;

#[inline(always)]
fn fifo_base() -> usize {
    packet_ram::rx_fifo_base()
}

fn dma_producer() -> u32 {
    crate::platform::rx_dma().producer.get()
}

fn set_dma_consumer(offset: u32) {
    crate::platform::rx_dma().consumer.set(offset);
}

register_structs! {
    /// The header packet DMA writes in front of each received frame. The
    /// frame follows at 0x20 (its length counts the FCS), then the receive
    /// trailer at the next word boundary.
    SlotHeader {
        (0x00 => magic: ReadWrite<u32>),
        (0x04 => next: ReadWrite<u32>),
        (0x08 => state: ReadWrite<u32>),
        (0x0c => _reserved0),
        /// The low word of the TSF when the frame began (measured: a beacon
        /// is processed its airtime later).
        (0x14 => rx_tsf: ReadOnly<u32>),
        (0x18 => length: ReadOnly<u16>),
        (0x1a => _reserved1),
        (0x20 => @END),
    }
}

register_structs! {
    /// The MAC's receive status after a frame.
    Trailer {
        (0x00 => _reserved0),
        (0x06 => rate: ReadOnly<u8>),
        (0x07 => rcpi: ReadOnly<u8>),
        (0x08 => @END),
    }
}

/// The slot at FIFO `offset`. A slot past the producer holds whatever packet
/// DMA left there; callers check the magic and length before trusting it.
fn slot(offset: u32) -> &'static SlotHeader {
    // SAFETY: `normalize_offset` keeps the header inside the FIFO window of
    // packet RAM, which is always mapped; its fields are registers, accessed
    // volatilely only.
    unsafe { &*((fifo_base() + normalize_offset(offset) as usize) as *const SlotHeader) }
}

impl SlotHeader {
    fn frame_address(&self) -> usize {
        core::ptr::from_ref(self) as usize + size_of::<Self>()
    }
}

/// The trailer after the frame of `length` octets (with FCS) in the slot at
/// `offset`, if the slot fits the packet RAM.
fn trailer(offset: u32, length: u16) -> Option<&'static Trailer> {
    if !slot_data_fits_packet_ram(offset, length) {
        return None;
    }
    let address = (slot(offset).frame_address() + usize::from(length) + 3) & !3;
    // SAFETY: the check above keeps the trailer inside the FIFO storage.
    Some(unsafe { &*(address as *const Trailer) })
}

/// The frame of a claimed slot.
///
/// # Safety
/// The slot must be claimed (packet DMA does not write it again until it is
/// released), fit the packet RAM, and no `&mut` to it may be live.
unsafe fn frame_bytes<'a>(frame_address: usize, frame_len: usize) -> &'a [u8] {
    unsafe { core::slice::from_raw_parts(frame_address as *const u8, frame_len) }
}

const WSM_RX_HEADROOM: usize = 16;
// The vendor accepts any complete FIFO slot whose length fits the HIF
// descriptor. A 1600-byte cap incorrectly rejected valid 1840-byte slots and
// resynchronized the release cursor across host-owned zero-copy indications.
const MAX_WSM_RX_MESSAGE_LEN: usize = 0x1ffe;
const MAX_FRAME_LEN: usize = MAX_WSM_RX_MESSAGE_LEN - WSM_RX_HEADROOM;
const MAX_HOST_TRANSFERS: u32 = 24;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReceiveDiagnostics {
    pub producer_changes: u32,
    pub valid_slots: u32,
    pub bad_magic: u32,
    pub malformed_slots: u32,
    pub filtered_frames: u32,
    pub oversized_frames: u32,
    pub indications: u32,
    pub released_slots: u32,
    pub last_producer: u32,
}

impl ReceiveDiagnostics {
    const ZERO: Self = Self {
        producer_changes: 0,
        valid_slots: 0,
        bad_magic: 0,
        malformed_slots: 0,
        filtered_frames: 0,
        oversized_frames: 0,
        indications: 0,
        released_slots: 0,
        last_producer: 0,
    };
}

#[derive(Debug, Eq, PartialEq)]
pub struct PendingIndication {
    pub address: u32,
    pub length: u16,
    pub release: RxToken,
    /// Authentication's key installation, retained through RX reordering.
    key_epoch: Option<RxKeyEpoch>,
}

fn receive_indication_id(if_id: u8) -> u16 {
    0x0804 | (u16::from(if_id.min(2)) << 6)
}

fn scan_frame_flags(frame_control: u16) -> Option<u32> {
    match frame_control & 0x00fc {
        0x0080 => Some(1 << 7),
        0x0050 => Some(0),
        _ => None,
    }
}

/// The DS Parameter Set channel of a beacon or probe response.
fn management_ds_channel(frame: &[u8]) -> Option<u8> {
    // Beacon and probe-response bodies both begin with twelve fixed bytes after
    // the 24-byte management header.
    let mut elements = frame.get(36..)?;
    while let [id, length, rest @ ..] = elements {
        let value = rest.get(..usize::from(*length))?;
        if *id == 3 && !value.is_empty() {
            return Some(value[0]);
        }
        elements = &rest[value.len()..];
    }
    None
}

fn normalize_offset(mut offset: u32) -> u32 {
    offset &= FIFO_MASK;
    if offset >= FIFO_SIZE {
        offset -= FIFO_SIZE;
    }
    offset
}

fn available_bytes(consumer: u32, producer: u32) -> u32 {
    if producer < consumer {
        producer.wrapping_add(FIFO_SIZE).wrapping_sub(consumer)
    } else {
        producer - consumer
    }
}

fn next_offset(offset: u32, slot_length: u16) -> u32 {
    normalize_offset(
        offset
            .wrapping_add(u32::from(slot_length))
            .wrapping_add(0x2f),
    )
}

fn repairable_tail_slot(
    slot_length: u16,
    available: u32,
    next: u32,
    producer: u32,
    next_has_magic: bool,
) -> bool {
    u32::from(slot_length) <= available && next == producer && !next_has_magic
}

fn slot_data_fits_packet_ram(offset: u32, slot_length: u16) -> bool {
    let frame_end = offset as usize + 0x20 + usize::from(slot_length);
    let trailer = (frame_end + 3) & !3;
    trailer + 8 <= FIFO_STORAGE_SIZE
}

const fn claimed_slot_state(value: u32) -> u32 {
    value & 0xff
}

const fn pending_release_state(value: u32) -> u32 {
    claimed_slot_state(value).wrapping_sub(0x100)
}

// Used by the vendor-shaped resync scan in every build, not just diagnostics.
fn fifo_word(offset: u32) -> u32 {
    slot(offset).magic.get()
}

/// Shared lookahead/dedup state, independent of packet-RAM access.
struct BlockAckCursor {
    routed: Option<u32>,
    scan: Option<u32>,
    claim: Option<u32>,
}

impl BlockAckCursor {
    const fn new() -> Self {
        Self { routed: None, scan: None, claim: None }
    }

    /// Observe every RX claim head, even for non-ACK frames. An offset is
    /// meaningful only until RX reaches it; otherwise a full FIFO cycle can
    /// make an old marker look like lookahead into the new unread span.
    fn follow_claim(&mut self, claim: u32) {
        if let Some(previous) = self.claim.replace(claim) {
            let advanced = available_bytes(previous, claim);
            if advanced != 0 {
                self.routed = self.routed.filter(|&offset| available_bytes(previous, offset) > advanced);
                self.scan = self.scan.filter(|&offset| available_bytes(previous, offset) > advanced);
            }
        }
    }

    fn before(&self, claim: u32, producer: u32, offset: u32) -> bool {
        self.routed.is_some_and(|cursor| {
            let to_cursor = available_bytes(claim, cursor);
            to_cursor <= available_bytes(claim, producer) && available_bytes(claim, offset) < to_cursor
        })
    }

    /// Mark an ACK reached by normal RX; lookahead may already have routed it.
    fn take(&mut self, claim: u32, producer: u32, offset: u32, next: u32) -> bool {
        self.follow_claim(claim);
        if self.before(claim, producer, offset) {
            return false;
        }
        self.routed = Some(next);
        true
    }

    fn next(
        &mut self,
        claim: u32,
        producer: u32,
        mut read: impl FnMut(u32) -> Option<(u32, Option<crate::tx::ampdu::BlockAck>)>,
    ) -> Option<crate::tx::ampdu::BlockAck> {
        self.follow_claim(claim);
        let pending = available_bytes(claim, producer);
        let mut offset = self.scan.filter(|&scan| available_bytes(claim, scan) <= pending).unwrap_or(claim);
        // Every slot takes at least 0x30 bytes. Bound malformed/cyclic walks.
        for _ in 0..FIFO_SIZE / 0x30 {
            self.scan = Some(offset);
            if offset == producer {
                return None;
            }
            let (next, ack) = read(offset)?;
            if next == offset || available_bytes(offset, next) > available_bytes(offset, producer) {
                return None;
            }
            if let Some(ack) = ack
                && self.take(claim, producer, offset, next)
            {
                self.scan = Some(next);
                return Some(ack);
            }
            offset = next;
        }
        self.scan = Some(offset);
        None
    }
}

/// The RX FIFO as the firmware consumes it: claim and release cursors, the
/// BlockAck lookahead and the counters. A resync may move the parser past
/// corrupt bytes while older zero-copy slots remain host-owned; one owner
/// keeps release/claim cursors, live-token count, and the deferred
/// hardware-consumer update coherent.
pub struct Fifo {
    ring: RxRing,
    diagnostics: ReceiveDiagnostics,
    /// All addresses share a cursor: never skip a different VIF/peer's ACK.
    block_ack: BlockAckCursor,
}

impl Fifo {
    const fn new() -> Self {
        Self {
            ring: RxRing::new(),
            diagnostics: ReceiveDiagnostics::ZERO,
            block_ack: BlockAckCursor::new(),
        }
    }

    fn filtered(&mut self) {
        self.diagnostics.filtered_frames = self.diagnostics.filtered_frames.wrapping_add(1);
    }

    fn set_release_offset(&mut self, next: u32) {
        self.ring.set_release_offset(next);
        set_dma_consumer(next);
    }

    /// Vendor-style recovery for a corrupt current header: walk four-byte-aligned
    /// candidates up to the producer and adopt the next magic slot, or discard the
    /// unread region if no valid slot remains.
    /// Vendor `rxfifo_next_frame` (`0x8af2`, `annotated-main.c:10331`) scan path.
    ///
    /// Vendor does not simply take the first word matching the magic. Having found
    /// a candidate it also requires the slot's declared length to fit the remaining
    /// span AND the following slot to carry the magic too, and keeps scanning when
    /// either fails. Without that two-slot lookahead any payload word that happens
    /// to equal `0x00aa55ff` resynchronises us onto garbage, which desynchronises
    /// the consumer further and costs another resync — a plausible amplifier for
    /// runs where resyncs climb from 1 to 21.
    ///
    /// The total flush when the span is exhausted IS vendor behaviour
    /// (`annotated-main.c:10373-10380` sets release, consumer and the DMA consumer
    /// at `0x09c00608` all to the producer), so it is kept as-is.
    fn apply_resynchronized_offset(&mut self, previous_claim: u32, target: u32) {
        self.block_ack.follow_claim(target);
        if let Some(target) = self.ring.resynchronize(previous_claim, target) {
            self.set_release_offset(target);
        }
    }

    fn resynchronize_consumer(&mut self, consumer: u32, producer: u32) {
        let mut remaining = available_bytes(consumer, producer);
        let mut candidate = consumer;
        loop {
            // Vendor's inner loop: step one word at a time to the next slot
            // that carries the magic, flushing if the span runs out.
            loop {
                if remaining < 4 || candidate == producer {
                    self.apply_resynchronized_offset(consumer, producer);
                    return;
                }
                remaining -= 4;
                candidate = normalize_offset(candidate.wrapping_add(4));
                if fifo_word(candidate) == FIFO_MAGIC {
                    break;
                }
            }
            let slot_length = slot(candidate).length.get();
            let next = next_offset(candidate, slot_length);
            if u32::from(slot_length) <= remaining
                && slot_length >= 4
                && fifo_word(next) == FIFO_MAGIC
            {
                self.apply_resynchronized_offset(consumer, candidate);
                return;
            }
            // Candidate carried the magic but failed vendor's validation: a
            // false positive we would previously have resynchronised onto.
        }
    }

    fn release_head_slot(&mut self, slot: &SlotHeader, next: u32, low_state: u32) {
        // Preserve vendor `rxfifo_release_slot()` ordering: advance the
        // software release cursor, mark and clear the slot, then expose the
        // new consumer pointer to packet DMA.
        self.ring.set_release_offset(next);
        slot.state.set(FIFO_RELEASED | low_state);
        slot.magic.set(0);
        set_dma_consumer(next);
        self.diagnostics.released_slots = self.diagnostics.released_slots.wrapping_add(1);
    }

    fn release(&mut self, token: RxToken) {
        let slot_offset = token.slot_offset();
        if slot_offset >= FIFO_SIZE {
            // SAFETY: the firmware halts right after.
            unsafe {
            crate::hif::publish_halting_exception(
                [
                    0x5258_524c,
                    (fifo_base() + slot_offset as usize) as u32,
                    token.next(),
                    self.ring.release_offset(),
                    self.ring.claim_offset(),
                    dma_producer(),
                    self.ring.host_transfer_count(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    fifo_base() as u32,
                    FIFO_SIZE,
                ],
                b"xr819-rx-release-address",
            );
            }
            // Do NOT convert this into a return: skipping the release stalls
            // the release head permanently, the FIFO never drains, and every
            // run dies harder than the hang it was meant to avoid (measured:
            // 3 of 4 runs failed to associate at all).
            crate::halt_always!();
        }
        let header = slot(slot_offset);
        let value = header.state.get();
        let low = match self.ring.classify_release(&token, value) {
            ReleaseAction::AlreadyReleased | ReleaseAction::Pending => return,
            ReleaseAction::MarkPending { .. } => {
                header.state.set(pending_release_state(value));
                return;
            }
            ReleaseAction::ReleaseHead {
                low_state,
                normalize_first,
                ..
            } => {
                if normalize_first {
                    header.state.set(pending_release_state(value));
                }
                low_state
            }
        };

        self.release_head_slot(header, token.next(), low);
        while self.ring.release_offset() != self.ring.claim_offset() {
            let next_slot = slot(self.ring.release_offset());
            let next_state = next_slot.state.get();
            if next_state & 0xffff_ff00 != 0xffff_ff00 {
                break;
            }
            self.release_head_slot(next_slot, normalize_offset(next_slot.next.get()), next_state & 0xff);
        }
        if let Some(target) = self.ring.finish_deferred_resync() {
            // `finish_deferred_resync` already updates the software cursor;
            // preserve the existing hardware write order here.
            set_dma_consumer(target);
        }
    }

    fn complete_host_transfer(&mut self, token: RxToken) {
        if self.ring.complete_host_transfer() {
            self.release(token);
        }
    }

    /// Whether `offset` lies before the cursor within the unhandled region that
    /// starts at `claim`, i.e. its BlockAck was already used.
    /// Publish a claimed frame's slot to the host.
    fn publish(&mut self) {
        let published = self.ring.publish_host_transfer(MAX_HOST_TRANSFERS);
        debug_assert!(published);
        self.diagnostics.indications = self.diagnostics.indications.wrapping_add(1);
    }
}

#[cfg(target_arch = "arm")]
impl crate::tx::aggregate::BlockAckLookahead for Fifo {
    /// Read every compressed BlockAck in FIFO order. TX validates its local
    /// address, peer, TID and sequence evidence. Nothing is consumed here.
    fn next_block_ack(&mut self) -> Option<crate::tx::ampdu::BlockAck> {
        let producer = normalize_offset(dma_producer());
        self.block_ack.next(self.ring.claim_offset(), producer, |offset| {
            if fifo_word(offset) != FIFO_MAGIC {
                return None;
            }
            let header = slot(offset);
            let length = header.length.get();
            let next = next_offset(offset, length);
            if length < 4
                || usize::from(length) > MAX_FRAME_LEN + 4
                || !slot_data_fits_packet_ram(offset, length)
                || available_bytes(offset, next) > available_bytes(offset, producer)
            {
                return None;
            }
            // SAFETY: packet DMA has finished the bounded slot before producer.
            let frame = unsafe { frame_bytes(header.frame_address(), usize::from(length) - 4) };
            Some((next, crate::tx::ampdu::parse_block_ack(frame)))
        })
    }
}

/// Writes the WSM receive header into the 16 octets in front of the frame.
fn write_indication_header(
    frame_address: usize,
    message_length: usize,
    if_id: u8,
    channel: u16,
    rx_rate: u8,
    rcpi: u8,
    flags: u32,
) {
    let message = frame_address - WSM_RX_HEADROOM;
    // SAFETY: the headroom is the last 16 octets of the claimed slot's own
    // header in packet RAM, which nothing else reads until the host does.
    unsafe {
        (message as *mut u16).write_volatile(message_length as u16);
        ((message + 2) as *mut u16).write_volatile(receive_indication_id(if_id));
        ((message + 4) as *mut u32).write_volatile(0);
        // XR819 trailer channel metadata includes PHY status bits (for example
        // channel 11 appears as 0x010b). CW1200 WSM requires the plain channel
        // number, which is authoritative from the active scan request.
        ((message + 8) as *mut u16).write_volatile(channel);
        ((message + 10) as *mut u8).write_volatile(rx_rate);
        ((message + 11) as *mut u8).write_volatile(rcpi);
        ((message + 12) as *mut u32).write_volatile(flags);
    }
}

/// The receive side: the FIFO, the peer's BlockAck sessions and the frame
/// on its way through decryption.
pub struct Radio {
    fifo: Fifo,
    sessions: RxBlockAck<PendingIndication>,
    /// Decryption takes about 130 us for a frame carrying 1200 octets.
    /// Waiting for it held the receive path to about 4800 frames/s, the RX
    /// FIFO overflowed during aggregates, and the peer retried the lost
    /// tails at lower rates. The engine now decrypts one frame while the
    /// previous one is delivered.
    in_flight: Option<InFlight>,
}

impl Default for Radio {
    fn default() -> Self {
        Self::new()
    }
}

impl Radio {
    /// Changes whenever a frame is claimed, released or decrypted.
    pub fn activity(&self, activity: crate::activity::Activity) -> crate::activity::Activity {
        let in_flight = match &self.in_flight {
            None => 0,
            Some(InFlight { decrypt: Decrypt::Waiting, .. }) => 1,
            Some(InFlight { decrypt: Decrypt::Running(_), .. }) => 2,
            Some(InFlight { decrypt: Decrypt::Ready, .. }) => 3,
        };
        self.fifo
            .ring
            .activity(activity)
            .with(in_flight)
            .with(self.fifo.block_ack.routed.unwrap_or(u32::MAX))
            .with(self.fifo.block_ack.scan.unwrap_or(u32::MAX))
    }

    pub const fn new() -> Self {
        Self {
            fifo: Fifo::new(),
            sessions: RxBlockAck::new(),
            in_flight: None,
        }
    }

    /// Synchronizes the FIFO with the live packet-DMA producer like vendor
    /// `sync_regs_10` (`0x10024`), before RX is enabled. No FIFO-backed host
    /// transfer may be outstanding.
    pub fn initialize(&mut self) {
        self.fifo.ring.synchronize(0);
        self.fifo.block_ack = BlockAckCursor::new();
        let dma = crate::platform::rx_dma();
        dma.consumer.set(0);
        dma.control.set(dma.control.get());
        self.fifo.diagnostics = ReceiveDiagnostics::default();
    }

    /// The FIFO, for the TX path's BlockAck lookahead.
    pub fn fifo(&mut self) -> &mut Fifo {
        &mut self.fifo
    }

    /// Compact scan-completion telemetry: producer changes, valid slots,
    /// indications, and malformed/bad-magic flags in four nibbles.
    pub fn diagnostic_word(&self) -> u16 {
        let value = self.fifo.diagnostics;
        let errors = u16::from(value.malformed_slots != 0)
            | (u16::from(value.bad_magic != 0) << 1)
            | (u16::from(value.oversized_frames != 0) << 2);
        (value.producer_changes.min(15) as u16)
            | ((value.valid_slots.min(15) as u16) << 4)
            | ((value.indications.min(15) as u16) << 8)
            | (errors << 12)
    }

    pub fn host_transfer_outstanding(&self) -> bool {
        self.fifo.ring.host_transfer_count() != 0
    }

    /// Every received frame delivered and released, none in decryption,
    /// no ADDBA response owed.
    pub fn settled(&self) -> bool {
        self.in_flight.is_none() && self.sessions.response().is_none() && self.fifo_quiescent()
    }

    pub fn fifo_quiescent(&self) -> bool {
        let ring = &self.fifo.ring;
        ring.host_transfer_count() == 0
            && ring.release_offset() == ring.claim_offset()
            && ring.claim_offset() == dma_producer() & FIFO_MASK
    }

    /// Release the slots the host has finished reading.
    pub fn reclaim(&mut self, transport: &mut crate::hif::Transport) {
        while let Some(token) = transport.take_returned_rx() {
            self.fifo.complete_host_transfer(token);
        }
    }

    /// Continuously consumes receive FIFO entries when no host scan owns them.
    /// Vendor `rx_handler_main_loop` runs independently of scan state; without
    /// this path, old beacons accumulate and contaminate the next scan dwell.
    pub fn discard_one_idle(&mut self, channel: u16) {
        if let Some(indication) = self.poll_scan_indication(0, channel) {
            self.fifo.complete_host_transfer(indication.release);
        }
    }

    /// Polls one packet-DMA frame and prepares a zero-copy WSM receive
    /// indication. Unwanted frames are released immediately.
    pub fn poll_scan_indication(&mut self, if_id: u8, active_channel: u16) -> Option<PendingIndication> {
        let claimed = claim_frame(&mut self.fifo, if_id, active_channel, None, None)?;
        deliver(&mut self.fifo, None, None, claimed, if_id, active_channel)
    }

    /// TIDs the host allows the peer's BlockAck sessions for (MIB 0x100e).
    pub fn set_block_ack_tids(&mut self, tids: u8) {
        self.sessions.set_allowed_tids(tids);
    }

    /// The ADDBA response owed to the peer, if any.
    pub fn block_ack_response(&self) -> Option<crate::tx::ba::Outgoing> {
        self.sessions.response()
    }

    /// The ADDBA response went to the MAC.
    pub fn block_ack_response_sent(&mut self) {
        let _ = self.sessions.take_response();
    }

    /// End every receive BlockAck session and drop the frames they hold, and
    /// the frame in flight (join, unjoin).
    /// An access point's station left: end its receive sessions and free
    /// their pipes. Frames they hold go to the host.
    pub fn end_peer_block_ack(&mut self, peer: [u8; 6]) {
        let fifo = &mut self.fifo;
        let freed = self.sessions.end_peer(peer, &mut |indication: PendingIndication| fifo.release(indication.release));
        #[cfg(target_arch = "arm")]
        (0..crate::rx_ba::SESSIONS).filter(|slot| freed & 1 << slot != 0).for_each(crate::rx_ba::clear_pipe);
        let _ = freed;
    }

    pub fn reset_block_ack(&mut self, crypto: &mut Crypto) {
        let fifo = &mut self.fifo;
        self.sessions.reset(&mut |indication: PendingIndication| fifo.release(indication.release));
        if let Some(in_flight) = self.in_flight.take() {
            if let Decrypt::Running(job) = in_flight.decrypt {
                crypto.abandon(job);
            }
            fifo.release(in_flight.claimed.token);
        }
        // Every pipe, not just those used: out of reset an entry reads as
        // valid (TID 15, peer ff:ff:ff:ff:ff:ff) and the MAC then builds
        // BlockAcks from it.
        #[cfg(target_arch = "arm")]
        (0..crate::rx_ba::SESSIONS).for_each(crate::rx_ba::clear_pipe);
    }

    /// Like [`Self::poll_scan_indication`], but BlockAck action frames for
    /// our TX sessions are consumed by `block_ack`, the peer's BlockAck
    /// signalling by the receive sessions, and frames of the peer's sessions
    /// are reordered.
    pub fn poll_joined_indication(
        &mut self,
        if_id: u8,
        active_channel: u16,
        block_ack: &crate::tx::ba::PeerSessions,
        beacons: &mut crate::beacon_loss::BeaconMonitor,
        power_save: &mut crate::power_save::StationPowerSave,
        crypto: &mut Crypto,
    ) -> Option<PendingIndication> {
        let Self { fifo, sessions, in_flight } = self;
        #[cfg(target_arch = "arm")]
        let now = crate::platform::timer_now();
        #[cfg(not(target_arch = "arm"))]
        let now = 0;
        // The frame in flight first: decrypted, it may fill a hole that the
        // sessions would otherwise give up just below, and go out late.
        if let Some(current) = in_flight.take() {
            match advance(fifo, current, if_id, crypto) {
                Some(Ok(claimed)) => {
                    return admit_and_prefetch(fifo, sessions, in_flight, claimed, if_id, active_channel, block_ack, beacons, power_save, crypto, now);
                }
                Some(Err(current)) => *in_flight = Some(current),
                None => {}
            }
        }
        // Held frames pin the RX FIFO from the oldest one on. Keep room for
        // about three frames: the MAC acknowledges frames it cannot store.
        let producer = normalize_offset(dma_producer());
        let pressure = available_bytes(fifo.ring.release_offset(), producer) > FIFO_SIZE - 0x1000;
        sessions.service(now, pressure, &mut |indication: PendingIndication| fifo.release(indication.release));
        // Released frames are older than anything still in the FIFO.
        if sessions.ready_len() != 0 {
            return take_ready(fifo, sessions, crypto, if_id);
        }
        // Frames leave in FIFO order: nothing passes the one in flight.
        if in_flight.is_some() {
            return None;
        }
        let claimed =
            claim_frame(fifo, if_id, active_channel, Some((block_ack, &mut *sessions)), Some((&mut *beacons, &mut *power_save, now)))?;
        match advance(fifo, InFlight { claimed, decrypt: Decrypt::Waiting }, if_id, crypto)? {
            Ok(claimed) => admit_and_prefetch(fifo, sessions, in_flight, claimed, if_id, active_channel, block_ack, beacons, power_save, crypto, now),
            Err(current) => {
                *in_flight = Some(current);
                None
            }
        }
    }
}

/// Hand a ready frame to the host or its session, then claim the next frame
/// and start decrypting it, so the engine works while this one goes out.
/// The next frame is claimed only after this one is admitted: a BlockAckReq
/// among the next frames moves the session window, and would leave this
/// frame behind it.
#[allow(clippy::too_many_arguments)]
fn admit_and_prefetch(
    fifo: &mut Fifo,
    sessions: &mut RxBlockAck<PendingIndication>,
    in_flight: &mut Option<InFlight>,
    claimed: Claimed,
    if_id: u8,
    active_channel: u16,
    block_ack: &crate::tx::ba::PeerSessions,
    beacons: &mut crate::beacon_loss::BeaconMonitor,
    power_save: &mut crate::power_save::StationPowerSave,
    crypto: &mut Crypto,
    now: u32,
) -> Option<PendingIndication> {
    let indication = deliver(fifo, Some(&mut *sessions), Some(&mut *crypto), claimed, if_id, active_channel);
    if let Some(next) = claim_frame(fifo, if_id, active_channel, Some((block_ack, sessions)), Some((beacons, power_save, now))) {
        *in_flight = match advance(fifo, InFlight { claimed: next, decrypt: Decrypt::Waiting }, if_id, crypto) {
            Some(Ok(ready)) => Some(InFlight { claimed: ready, decrypt: Decrypt::Ready }),
            Some(Err(next)) => Some(next),
            None => None,
        };
    }
    indication
}

/// The next session frame released in order, if the host can take one.
fn take_ready(
    fifo: &mut Fifo,
    sessions: &mut RxBlockAck<PendingIndication>,
    crypto: &mut Crypto,
    if_id: u8,
) -> Option<PendingIndication> {
    if fifo.ring.host_transfer_count() >= MAX_HOST_TRANSFERS {
        return None;
    }
    let indication = sessions.pop_ready()?;
    if !fresh(crypto, if_id, &indication) {
        drop_replay(fifo, indication.release);
        return None;
    }
    fifo.publish();
    // Session frames are unicast.
    crate::counters::bump(crate::counters::Counter::RxFramesSuccess);
    Some(indication)
}

/// Check the authenticating key installation and its replay counter just
/// before publication; frames we did not decrypt are not ours to check.
fn fresh(crypto: &mut Crypto, if_id: u8, indication: &PendingIndication) -> bool {
    let message = indication.address as usize;
    // SAFETY: the indication's slot is claimed and holds its WSM header and
    // frame, which nothing writes until the host has read them.
    let (flags, frame) = unsafe {
        (
            ((message + 12) as *const u32).read_volatile(),
            frame_bytes(message + WSM_RX_HEADROOM, usize::from(indication.length) - WSM_RX_HEADROOM),
        )
    };
    flags & 7 != 3 || indication.key_epoch.is_some_and(|epoch| crypto.accept_rx_packet_number(if_id, frame, epoch))
}

/// A replay or a frame authenticated under a revoked key never reaches the host.
fn drop_replay(fifo: &mut Fifo, token: RxToken) {
    crate::counters::bump(crate::counters::Counter::RxFrameDuplicates);
    fifo.filtered();
    fifo.release(token);
}

/// A claimed frame on its way through decryption.
struct InFlight {
    claimed: Claimed,
    decrypt: Decrypt,
}

enum Decrypt {
    /// Not started: the engine was busy.
    Waiting,
    /// The engine works on the frame.
    Running(Job),
    /// Decrypted, or not protected.
    Ready,
}

fn count_decrypt_failure(error: crate::crypto::CcmpError) {
    use crate::counters::{Counter, bump};
    match error {
        crate::crypto::CcmpError::MissingKey => bump(Counter::RxNoKeyFailures),
        crate::crypto::CcmpError::Authentication => {
            bump(Counter::RxMicFailures);
            bump(Counter::RxDecryptionFailures);
        }
        _ => bump(Counter::RxDecryptionFailures),
    }
}

/// Move a frame through decryption: `Ok` once it is ready for the host,
/// `Err` while the engine is busy or works on it, `None` if it failed and
/// was dropped.
fn advance(fifo: &mut Fifo, in_flight: InFlight, if_id: u8, crypto: &mut Crypto) -> Option<Result<Claimed, InFlight>> {
    let InFlight { mut claimed, decrypt } = in_flight;
    let job = match decrypt {
        Decrypt::Ready => return Some(Ok(claimed)),
        Decrypt::Running(job) => job,
        Decrypt::Waiting => {
            // SAFETY: the claimed slot is ours until released, and no other
            // reference to its frame is live. The job stays with `claimed`
            // in the frame in flight, which is released only after `poll`
            // or `abandon` returns the job.
            let started = unsafe {
                let frame = core::slice::from_raw_parts_mut(claimed.frame_address as *mut u8, claimed.frame_len);
                crypto.start_rx(frame, if_id)
            };
            match started {
                Ok(Started::Plain) => return Some(Ok(claimed)),
                Ok(Started::Busy) => return Some(Err(InFlight { claimed, decrypt: Decrypt::Waiting })),
                Ok(Started::Running(job)) => job,
                Err(error) => {
                    drop_undecrypted(fifo, claimed, error);
                    return None;
                }
            }
        }
    };
    let key_epoch = job.rx_key_epoch();
    match crypto.poll(job) {
        Err(job) => Some(Err(InFlight { claimed, decrypt: Decrypt::Running(job) })),
        Ok(Ok(())) => {
            claimed.key_epoch = key_epoch;
            claimed.indication_flags |= 3;
            Some(Ok(claimed))
        }
        Ok(Err(error)) => {
            drop_undecrypted(fifo, claimed, error);
            None
        }
    }
}

fn drop_undecrypted(fifo: &mut Fifo, claimed: Claimed, error: crate::crypto::CcmpError) {
    count_decrypt_failure(error);
    fifo.filtered();
    fifo.release(claimed.token);
}

/// Group-receiver data not from our joined AP, or any such data in AP mode.
// A group receiver is an AP's downlink: keep it only from our joined AP.
// As an AP we have no such peer. Our stations send even multicast MSDUs
// ToDS with a unicast receiver (us), and use their pairwise key.
fn foreign_group_data(frame: &[u8], joined_ap: Option<[u8; 6]>) -> bool {
    Header::parse(frame).is_some_and(|header| {
        header.is_data() && header.group_addressed() && joined_ap.is_none_or(|ap| header.addr2 != ap)
    })
}

/// Sender, TID and sequence number of a unicast QoS data frame to `own` that
/// a receive BlockAck session may cover: from the AP `joined` to us as its
/// station, or from any of our stations when `joined` is `None` (we are the
/// access point).
fn session_frame(frame: &[u8], own: [u8; 6], joined: Option<[u8; 6]>) -> Option<([u8; 6], u8, u16)> {
    let data = QosData::parse(frame)?;
    let header = &data.header;
    let from_peer = match joined {
        Some(ap) => header.from_ds() && header.addr2 == ap,
        None => header.to_ds(),
    };
    (from_peer && header.addr1 == own).then(|| (header.addr2, data.tid(), data.sequence()))
}

/// The AP a station's interface joined; `None` for an access point.
fn joined_ap(vif: &crate::vif::Vif) -> Option<[u8; 6]> {
    (vif.mode.get() != crate::vif::Role::AccessPoint as u8).then(|| vif.bssid.each_ref().map(core::cell::Cell::get))
}

/// A claimed frame on its way to the host.
struct Claimed {
    token: RxToken,
    frame_address: usize,
    frame_len: usize,
    rx_rate: u8,
    rcpi: u8,
    indication_flags: u32,
    key_epoch: Option<RxKeyEpoch>,
}

/// Claim the next frame in the FIFO. Frames the firmware consumes itself
/// (BlockAck signalling) or filters out are released here and give `None`.
/// `joined`: our TX BlockAck sessions and the peer's receive sessions.
/// Beacons of the joined BSS are timed by `beacons` (at `now`), and a
/// station's power save watches its AP's beacons and frames.
/// The timer reading when a frame stamped `rx_tsf` began: the timer and the
/// TSF count the same microseconds from different origins.
fn started_at(rx_tsf: u32) -> u32 {
    let now = crate::platform::timer_now();
    now.wrapping_sub((crate::mac::tsf() as u32).wrapping_sub(rx_tsf))
}

fn claim_frame(
    fifo: &mut Fifo,
    if_id: u8,
    active_channel: u16,
    mut joined: Option<(&crate::tx::ba::PeerSessions, &mut RxBlockAck<PendingIndication>)>,
    beacons: Option<(&mut crate::beacon_loss::BeaconMonitor, &mut crate::power_save::StationPowerSave, u32)>,
) -> Option<Claimed> {
    let scan_only = joined.is_none();
    let consumer = fifo.ring.claim_offset();
    fifo.block_ack.follow_claim(consumer);
    let producer = normalize_offset(dma_producer());
    if consumer == producer {
        return None;
    }
    if fifo.diagnostics.last_producer != producer {
        fifo.diagnostics.last_producer = producer;
        fifo.diagnostics.producer_changes = fifo.diagnostics.producer_changes.wrapping_add(1);
    }

    let header = slot(consumer);
    let slot_length = header.length.get();
    if header.magic.get() != FIFO_MAGIC {
        let available = available_bytes(consumer, producer);
        let next = next_offset(consumer, slot_length);
        let next_has_magic = fifo_word(next) == FIFO_MAGIC;
        if repairable_tail_slot(slot_length, available, next, producer, next_has_magic) {
            // Vendor `rxfifo_next_frame` repair branch: the current slot's
            // header marker was damaged, but its bounded length lands
            // exactly on the producer. Preserve that frame, rebuild the
            // producer sentinel, and advance ownership normally instead of
            // destructively scanning or flushing the unread span.
            header.next.set(next);
            header.state.set(claimed_slot_state(header.state.get()));
            fifo.ring.set_claim_offset(next);
            slot(next).magic.set(FIFO_MAGIC);
        } else {
            fifo.diagnostics.bad_magic = fifo.diagnostics.bad_magic.wrapping_add(1);
            crate::counters::bump(crate::counters::Counter::RxPacketErrors);
            fifo.resynchronize_consumer(consumer, producer);
            return None;
        }
    } else {
        fifo.diagnostics.valid_slots = fifo.diagnostics.valid_slots.wrapping_add(1);
        crate::counters::bump(crate::counters::Counter::RxPackets);
    }

    let available = available_bytes(consumer, producer);
    if slot_length < 4 || usize::from(slot_length) > MAX_FRAME_LEN + 4 || available < u32::from(slot_length) {
        fifo.diagnostics.malformed_slots = fifo.diagnostics.malformed_slots.wrapping_add(1);
        crate::counters::bump(crate::counters::Counter::RxPacketErrors);
        if usize::from(slot_length) > MAX_FRAME_LEN + 4 {
            fifo.diagnostics.oversized_frames = fifo.diagnostics.oversized_frames.wrapping_add(1);
        }
        fifo.resynchronize_consumer(consumer, producer);
        return None;
    }

    let frame_len = usize::from(slot_length) - 4;
    let next = next_offset(consumer, slot_length);
    header.next.set(next);
    header.state.set(claimed_slot_state(header.state.get()));
    let token = fifo.ring.claim(next);

    let frame_address = header.frame_address();
    let Some(trailer) = trailer(consumer, slot_length) else {
        fifo.diagnostics.malformed_slots = fifo.diagnostics.malformed_slots.wrapping_add(1);
        fifo.release(token);
        return None;
    };
    let rcpi = trailer.rcpi.get().max(1);
    // Vendor copies two adjacent descriptor bytes into the indication's rate
    // and RCPI fields (`annotated-main.c:13085-13125`: `param_1+0xe` -> +10,
    // `param_1+0xf` -> +11). Our RCPI is `trailer+7`, matching vendor's +0xf,
    // so the rate byte is `trailer+6`. We previously hard-coded 0 here, and
    // `txrx.c:1241-1247` maps that straight to rate_idx 0, so every received
    // frame was reported to mac80211 as 1 Mbit/s regardless of its real rate.
    // Clamp before reporting. `txrx.c:1241-1247` treats >=14 as HT with
    // `rate_idx = rx_rate - 14` and mac80211 then *drops the frame* with a
    // WARN when that exceeds MCS 76, so one stray descriptor byte costs a
    // received frame. This chip is 1x1 802.11n, so the legal space is 0..=21:
    // 0..=3 legacy direct, 4..=13 legacy `rate_idx - 2`, 14..=21 HT MCS0..7.
    // Anything else is not a rate and is reported as unknown rather than
    // silently discarding the frame. Measured cost of passing it through raw:
    // a continuous WARN storm from ieee80211_rx_list and association failing
    // at AUTHENTICATING because the unicast auth response never survived RX.
    let rx_rate = match trailer.rate.get() {
        rate @ 0..=21 => rate,
        _ => 0,
    };
    // SAFETY: the slot is claimed and fits the packet RAM (trailer above);
    // the frame is only read here.
    let frame = unsafe { frame_bytes(frame_address, frame_len) };
    let frame_control = frame.get(..2).map_or(0xffff, |bytes| u16::from_le_bytes([bytes[0], bytes[1]]));
    let indication_flags = scan_frame_flags(frame_control).unwrap_or(0);

    // A joined station's host hears other BSSes' beacons, and other
    // stations' probe requests, only on request.
    if let Some((beacons, _, _)) = &beacons
        && !beacons.filter.forward_other_bss()
        && let Some(header) = Header::parse(frame)
        && let Some(vif) = crate::vif::vif(if_id)
        && vif.mode.get() == crate::vif::Role::Station as u8
        && match header.type_subtype() {
            0x0080 => header.addr3 != vif.bssid.each_ref().map(core::cell::Cell::get),
            0x0040 => true,
            _ => false,
        }
    {
        fifo.filtered();
        fifo.release(token);
        return None;
    }

    // Any frame the AP sent proves it is there, as for mac80211's own
    // monitor: under a heavy downlink the RX FIFO can be too full for
    // the MAC to keep every beacon.
    if let Some((beacons, power_save, now)) = beacons
        && let Some(header) = Header::parse(frame)
        && let Some(vif) = crate::vif::vif(if_id)
        && header.addr2 == vif.bssid.each_ref().map(core::cell::Cell::get)
    {
        beacons.on_beacon(now);
        if header.type_subtype() == 0x0080 {
            beacons.signal.on_beacon(rcpi);
            // Timed from its start, not from when we got to it: its airtime
            // and our latency vary.
            power_save.on_beacon(frame, started_at(slot(consumer).rx_tsf.get()));
            if !beacons.filter.forward(frame) {
                fifo.filtered();
                fifo.release(token);
                return None;
            }
        } else if header.is_data() && header.group_addressed() {
            power_save.on_group_frame(header.more_data());
        } else if header.is_data() && header.addr1 == vif.own_mac.each_ref().map(core::cell::Cell::get) {
            power_save.on_traffic(now);
        }
    }

    // The peer's BlockAckReq (20 octets, below the data-frame minimum):
    // the MAC answered it; move the session past what it gives up.
    if let Some((_, sessions)) = &mut joined
        && let Some(request) = BlockAckRequest::parse(frame)
        && let Some(vif) = crate::vif::vif(if_id)
        && request.header.receiver == vif.own_mac.each_ref().map(core::cell::Cell::get)
        && joined_ap(vif).is_none_or(|ap| request.header.transmitter == ap)
    {
        let peer = request.header.transmitter;
        sessions.on_block_ack_request(peer, request.tid(), request.ssn(), crate::platform::timer_now(), &mut |indication: PendingIndication| {
            fifo.release(indication.release)
        });
        fifo.release(token);
        return None;
    }

    // The vendor drains old frames before retuning. Reject a management frame
    // whose on-air DS element proves it belongs to a previous channel rather
    // than relabeling it with the active CW1200 dwell.
    let wrong_ds_channel =
        scan_only && management_ds_channel(frame).is_some_and(|channel| u16::from(channel) != active_channel);
    // An access point's sleeping stations poll for their frames with a
    // 16-octet PS-Poll; the host releases one buffered frame per poll.
    let ps_poll = frame_control & 0x00fc == 0x00a4
        && frame_len >= 16
        && crate::vif::vif(if_id).is_some_and(|vif| vif.mode.get() == crate::vif::Role::AccessPoint as u8);
    if (frame_len < 24 && !ps_poll) || (scan_only && scan_frame_flags(frame_control).is_none()) || wrong_ds_channel {
        fifo.filtered();
        fifo.release(token);
        return None;
    }

    // Use the same unfiltered decoder as lookahead: an ACK for another local
    // address must not be marked consumed without routing it to that owner.
    if let Some(received) = crate::tx::ampdu::parse_block_ack(frame) {
        if fifo.block_ack.take(consumer, producer, consumer, next) {
            #[cfg(target_arch = "arm")]
            crate::tx::aggregate::on_block_ack(received);
        }
        let _ = received;
        fifo.release(token);
        return None;
    }

    if let Some((block_ack, sessions)) = joined
        && let Some(vif) = crate::vif::vif(if_id)
    {
        let own = vif.own_mac.each_ref().map(core::cell::Cell::get);
        // An access point's peer is whichever station sent the frame.
        let peer = match (joined_ap(vif), frame.get(10..16).and_then(|address| <[u8; 6]>::try_from(address).ok())) {
            (Some(ap), _) => ap,
            (None, Some(transmitter)) => transmitter,
            (None, None) => [0; 6],
        };
        if let Some(received) = crate::tx::ba::parse(frame, own, peer) {
            let mut discard = |indication: PendingIndication| fifo.release(indication.release);
            match received {
                crate::tx::ba::Received::AddbaRequest { token, tid, immediate, buffer_size, ssn } => {
                    let outcome = sessions.on_addba_request(
                        peer,
                        token,
                        tid,
                        immediate,
                        buffer_size,
                        ssn,
                        sessions.allowed_tids(),
                        &mut discard,
                        &mut |slot| {
                            #[cfg(target_arch = "arm")]
                            crate::rx_ba::clear_pipe(slot);
                            let _ = slot;
                        },
                    );
                    #[cfg(target_arch = "arm")]
                    if let crate::rx_ba::Addba::Accepted { slot, .. } = outcome {
                        crate::rx_ba::program_pipe(slot, if_id, peer, tid, ssn);
                    }
                    let _ = outcome;
                }
                crate::tx::ba::Received::PeerDelba { tid } => {
                    let slot = sessions.end_session(peer, tid, &mut discard);
                    #[cfg(target_arch = "arm")]
                    if let Some(slot) = slot {
                        crate::rx_ba::clear_pipe(slot);
                    }
                    let _ = slot;
                }
                // Our sessions with the peer, on its link.
                received => {
                    if let Some(link) = crate::vif::peer_link(if_id, peer) {
                        block_ack.link(link).on_received(received);
                    }
                }
            }
            fifo.release(token);
            return None;
        }
        // Neighbouring networks' broadcasts would only fail decryption
        // (about 66 us of the AES engine each) and be dropped. In AP mode,
        // do not use the frame's own transmitter as its qualifying peer.
        if foreign_group_data(frame, joined_ap(vif)) {
            fifo.filtered();
            fifo.release(token);
            return None;
        }
    }

    Some(Claimed { token, frame_address, frame_len, rx_rate, rcpi, indication_flags, key_epoch: None })
}

/// Publish a claimed (and, if protected, decrypted) frame to the host, or
/// hand it to its BlockAck session.
fn deliver(
    fifo: &mut Fifo,
    sessions: Option<&mut RxBlockAck<PendingIndication>>,
    crypto: Option<&mut Crypto>,
    claimed: Claimed,
    if_id: u8,
    active_channel: u16,
) -> Option<PendingIndication> {
    let Claimed { token, frame_address, frame_len, rx_rate, rcpi, indication_flags, key_epoch } = claimed;
    // SAFETY: the slot is claimed and was checked to fit the packet RAM; the
    // engine is done with it, and it is only read here.
    let frame = unsafe { frame_bytes(frame_address, frame_len) };
    // A completed/prefetched decrypt can cross a key change before delivery.
    // Reject revoked authority before it can move a peer's reorder window.
    if indication_flags & 7 == 3
        && !key_epoch.is_some_and(|epoch| crypto.as_ref().is_some_and(|crypto| crypto.rx_key_epoch_is_active(if_id, epoch)))
    {
        count_decrypt_failure(crate::crypto::CcmpError::MissingKey);
        fifo.filtered();
        fifo.release(token);
        return None;
    }
    // A frame of one of the peer's BlockAck sessions waits its turn.
    let session = sessions.and_then(|sessions| {
        let vif = crate::vif::vif(if_id)?;
        let own = vif.own_mac.each_ref().map(core::cell::Cell::get);
        session_frame(frame, own, joined_ap(vif))
            .filter(|&(peer, tid, _)| sessions.has_session(peer, tid))
            .map(|session| (session, sessions))
    });
    let group_addressed = frame.get(4).is_some_and(|address| address & 1 != 0);

    // Vendor keeps draining and filtering RX FIFO slots while 24 receive
    // indications are host-owned; only an otherwise publishable frame is
    // dropped at this admission boundary.
    if session.is_none() && fifo.ring.host_transfer_count() >= MAX_HOST_TRANSFERS {
        fifo.filtered();
        fifo.release(token);
        return None;
    }

    let message_length = frame_len + WSM_RX_HEADROOM;
    write_indication_header(frame_address, message_length, if_id, active_channel, rx_rate, rcpi, indication_flags);
    let mut indication = PendingIndication {
        address: (frame_address - WSM_RX_HEADROOM) as u32,
        length: message_length as u16,
        release: token,
        key_epoch,
    };

    if let Some(((peer, tid, sequence), sessions)) = session {
        #[cfg(target_arch = "arm")]
        let now = crate::platform::timer_now();
        #[cfg(not(target_arch = "arm"))]
        let now = 0;
        match sessions.admit(peer, tid, sequence, indication, now, &mut |indication: PendingIndication| {
            fifo.release(indication.release)
        }) {
            None => return crypto.and_then(|crypto| take_ready(fifo, sessions, crypto, if_id)),
            Some(returned) => indication = returned,
        }
    }
    // The scan path, without keys, passes beacons and probe responses
    // alone; any frame it decrypted would be dropped here.
    if !crypto.map_or(indication_flags & 7 != 3, |crypto| fresh(crypto, if_id, &indication)) {
        drop_replay(fifo, indication.release);
        return None;
    }

    fifo.publish();
    crate::counters::bump(crate::counters::Counter::RxFramesSuccess);
    if frame_len >= 10 && group_addressed {
        crate::counters::bump(crate::counters::Counter::RxMulticastFrames);
    }
    Some(indication)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ack_frame(own: u8, peer: u8) -> [u8; 28] {
        let mut frame = [0; 28];
        frame[0] = 0x94;
        frame[4..10].fill(own);
        frame[10..16].fill(peer);
        frame[16] = 4;
        frame[20] = 3;
        frame
    }

    #[test]
    fn block_ack_lookahead_routes_all_addresses_once_across_fifo_wrap() {
        let mut cursor = BlockAckCursor::new();
        let frames = [ack_frame(2, 4), [0; 28], ack_frame(6, 8), ack_frame(2, 10)];
        let offsets = [FIFO_SIZE - 0x100, FIFO_SIZE - 0x80, 0, 0x80, 0x100];
        let read = |offset| {
            let index = offsets[..4].iter().position(|&slot| slot == offset)?;
            Some((offsets[index + 1], crate::tx::ampdu::parse_block_ack(&frames[index])))
        };
        let claim = offsets[0];
        let producer = offsets[4];
        let mut routed = std::vec::Vec::new();
        while let Some(ack) = cursor.next(claim, producer, read) {
            routed.push((ack.receiver, ack.transmitter));
        }
        assert_eq!(routed, [([2; 6], [4; 6]), ([6; 6], [8; 6]), ([2; 6], [10; 6])]);
        // Normal RX reaches those slots later: none can route a second time.
        for index in [0, 2, 3] {
            assert!(!cursor.take(offsets[index], producer, offsets[index], offsets[index + 1]));
        }
        assert!(cursor.next(offsets[3], producer, read).is_none());
        // A newly produced ACK after the cursor is routed by normal RX once.
        assert!(cursor.take(producer, 0x180, producer, 0x180));
        assert!(!cursor.take(producer, 0x180, producer, 0x180));
    }

    #[test]
    fn block_ack_offsets_reused_after_a_full_fifo_cycle_are_new_frames() {
        let mut cursor = BlockAckCursor::new();
        let ack = crate::tx::ampdu::parse_block_ack(&ack_frame(2, 4)).unwrap();
        assert_eq!(cursor.next(0, 0x180, |offset| match offset {
            0 => Some((0x100, None)),
            0x100 => Some((0x180, Some(ack))),
            _ => None,
        }), Some(ack));
        // Ordinary RX consumes a full turn of non-ACK traffic. Lookahead
        // sees each new claim head, just as it does in the firmware loop.
        for claim in (0x180..FIFO_SIZE).step_by(0x80) {
            let producer = (claim + 0x80) % FIFO_SIZE;
            assert!(cursor.next(claim, producer, |_| Some((producer, None))).is_none());
        }
        // The old routed=0x180 now lies numerically inside a new backlog.
        // This ACK is not a duplicate even though it is before that offset.
        assert_eq!(cursor.next(0, 0x200, |offset| match offset {
            0 => Some((0x80, None)),
            0x80 => Some((0x100, Some(ack))),
            0x100 => Some((0x200, None)),
            _ => None,
        }), Some(ack));
        assert!(!cursor.take(0x80, 0x200, 0x80, 0x100));
    }

    #[test]
    fn normal_rx_expires_markers_at_the_claim_boundary_without_lookahead() {
        let mut cursor = BlockAckCursor::new();
        assert!(cursor.take(0x6f00, 0x200, 0x6f80, 0));
        cursor.scan = Some(0x100);
        cursor.follow_claim(0x6f80);
        assert_eq!((cursor.routed, cursor.scan), (Some(0), Some(0x100)));
        // Reaching (not just passing) a marker ends its lifetime.
        cursor.follow_claim(0);
        assert_eq!((cursor.routed, cursor.scan), (None, Some(0x100)));
        cursor.follow_claim(0x100);
        assert_eq!((cursor.routed, cursor.scan), (None, None));
        // Without a lookahead call, normal RX still sees the reused offset
        // as new after one full turn of non-ACK frames.
        for claim in (0x180..FIFO_SIZE).step_by(0x80) {
            cursor.follow_claim(claim);
        }
        cursor.follow_claim(0);
        assert!(cursor.take(0, 0x200, 0x80, 0x100));
        assert!(!cursor.take(0, 0x200, 0x80, 0x100));
    }

    #[test]
    fn resync_expires_passed_markers_but_preserves_unread_ack_dedup() {
        let mut fifo = Fifo::new();
        let ack = crate::tx::ampdu::parse_block_ack(&ack_frame(2, 4)).unwrap();
        assert_eq!(fifo.block_ack.next(0, 0x300, |offset| match offset {
            0 => Some((0x180, None)),
            0x180 => Some((0x200, Some(ack))),
            _ => None,
        }), Some(ack));
        // Keep the release deferred, so this calls the production resync
        // adapter without touching the DMA consumer register on the host.
        assert!(fifo.ring.publish_host_transfer(1));
        fifo.apply_resynchronized_offset(0, 0x100);
        assert_eq!(fifo.ring.claim_offset(), 0x100);
        assert_eq!(fifo.ring.release_offset(), 0);
        assert!(!fifo.block_ack.take(0x100, 0x300, 0x180, 0x200));
        fifo.apply_resynchronized_offset(0x100, 0x200);
        assert_eq!((fifo.block_ack.routed, fifo.block_ack.scan), (None, None));
    }

    #[test]
    fn block_ack_lookahead_retries_incomplete_slots_without_skipping_them() {
        let mut cursor = BlockAckCursor::new();
        let ack = crate::tx::ampdu::parse_block_ack(&ack_frame(2, 4)).unwrap();
        assert!(cursor.next(0, 0x100, |_| None).is_none());
        assert_eq!(cursor.scan, Some(0));
        assert!(cursor.next(0, 0x40, |_| Some((0x80, Some(ack)))).is_none());
        assert_eq!(cursor.routed, None);
        assert_eq!(cursor.next(0, 0x80, |_| Some((0x80, Some(ack)))), Some(ack));
        assert!(cursor.next(0, 0x80, |_| panic!("at producer")).is_none());
    }

    #[test]
    fn only_our_aps_group_data_is_kept() {
        let ap = [0x94, 0x83, 0xc4, 0xba, 0x5b, 0x0e];
        let frame = |control: u8, receiver: u8, transmitter: [u8; 6]| {
            let mut frame = [0_u8; 24];
            frame[0] = control;
            frame[4] = receiver;
            frame[10..16].copy_from_slice(&transmitter);
            frame
        };
        assert!(!foreign_group_data(&frame(0x08, 0xff, ap), Some(ap)));
        assert!(foreign_group_data(&frame(0x08, 0xff, [2; 6]), Some(ap)));
        assert!(foreign_group_data(&frame(0x88, 0x01, [2; 6]), Some(ap)));
        // Unicast and management frames are left alone.
        assert!(!foreign_group_data(&frame(0x08, 0x12, [2; 6]), Some(ap)));
        assert!(!foreign_group_data(&frame(0x80, 0xff, [2; 6]), Some(ap)));
    }

    #[test]
    fn an_access_point_does_not_decrypt_group_addressed_data() {
        let mut frame = [0_u8; 26];
        frame[..2].copy_from_slice(&[0x88, 0x42]); // Protected QoS, FromDS.
        frame[4..10].fill(0xff); // A neighboring AP's broadcast receiver.
        frame[10..16].fill(2);
        assert!(foreign_group_data(&frame, None));
        // Plaintext foreign broadcasts are not ours either.
        frame[1] &= !0x40;
        assert!(foreign_group_data(&frame, None));
        // A station's multicast MSDU arrives ToDS with a unicast receiver
        // (us) and its multicast destination in address 3. Keep it: CCMP
        // uses the station's PTK, not our GTK.
        frame[1] = 0x41;
        frame[4..10].copy_from_slice(&[0x12, 0x42, 0x2a, 0x37, 0x70, 0x07]);
        frame[16..22].fill(0xff);
        assert!(!foreign_group_data(&frame, None));
        // Broadcast management frames, including probe requests, still
        // reach the management path.
        frame[0] = 0x40;
        frame[1] = 0;
        frame[4..10].fill(0xff);
        assert!(!foreign_group_data(&frame, None));
    }

    #[test]
    fn session_frames_come_from_the_joined_ap_or_from_our_stations() {
        let (own, ap, station) = ([1; 6], [2; 6], [3; 6]);
        let qos_data = |flags: u8, receiver: [u8; 6], transmitter: [u8; 6]| {
            let mut frame = [0_u8; 26];
            frame[..2].copy_from_slice(&[0x88, flags]);
            frame[4..10].copy_from_slice(&receiver);
            frame[10..16].copy_from_slice(&transmitter);
            frame[22..24].copy_from_slice(&(7_u16 << 4).to_le_bytes());
            frame[24] = 5;
            frame
        };
        // As a station: from our AP (FromDS) only.
        assert_eq!(session_frame(&qos_data(0x02, own, ap), own, Some(ap)), Some((ap, 5, 7)));
        assert_eq!(session_frame(&qos_data(0x02, own, station), own, Some(ap)), None);
        // As an access point: from any station (ToDS), to us.
        assert_eq!(session_frame(&qos_data(0x01, own, station), own, None), Some((station, 5, 7)));
        assert_eq!(session_frame(&qos_data(0x02, own, station), own, None), None);
        assert_eq!(session_frame(&qos_data(0x01, [9; 6], station), own, None), None);
    }

    #[test]
    fn vendor_fifo_advance_alignment_and_wrap() {
        assert_eq!(next_offset(0, 100), 0x90);
        assert_eq!(next_offset(0x6fc0, 64), 0x2c);
        assert_eq!(normalize_offset(0x7000), 0);
        assert_eq!(available_bytes(0x6ff0, 0x20), 0x30);
        assert!(slot_data_fits_packet_ram(0x6f00, 100));
        assert!(slot_data_fits_packet_ram(0x6fc0, 64));
        assert!(slot_data_fits_packet_ram(0x6a00, 1552));
        assert!(!slot_data_fits_packet_ram(0x6fc0, 0x1100));
    }

    #[test]
    fn vendor_tail_repair_requires_exact_producer_boundary() {
        assert!(repairable_tail_slot(100, 100, 0x240, 0x240, false));
        assert!(!repairable_tail_slot(101, 100, 0x240, 0x240, false));
        assert!(!repairable_tail_slot(100, 100, 0x23c, 0x240, false));
        assert!(!repairable_tail_slot(100, 100, 0x240, 0x240, true));
    }

    #[test]
    fn scan_filter_accepts_only_beacons_and_probe_responses() {
        assert_eq!(scan_frame_flags(0x0080), Some(1 << 7));
        assert_eq!(scan_frame_flags(0x0050), Some(0));
        assert_eq!(scan_frame_flags(0x0008), None);
        assert_eq!(scan_frame_flags(0x00d0), None);
    }

    #[test]
    fn receive_indication_preserves_xr819_interface_bits() {
        assert_eq!(receive_indication_id(0), 0x0804);
        assert_eq!(receive_indication_id(1), 0x0844);
        assert_eq!(receive_indication_id(2), 0x0884);
    }

    #[test]
    fn zero_copy_indication_fits_hif_descriptor_length() {
        assert_eq!(MAX_FRAME_LEN + WSM_RX_HEADROOM, 0x1ffe);
        assert!(MAX_FRAME_LEN > 1840);
    }

    #[test]
    fn vendor_slot_ownership_preserves_low_metadata_byte() {
        assert_eq!(claimed_slot_state(0x1234_56a5), 0x0000_00a5);
        assert_eq!(pending_release_state(0x1234_56a5), 0xffff_ffa5);
        assert_eq!(FIFO_RELEASED | claimed_slot_state(0x1234_56a5), 0xcccc_cca5);
    }
}
