//! Admission and scheduling of host data frames (WSM TX requests, class 0).
//!
//! A request keeps its HIF buffer: the frame is classified, encrypted and
//! sent from there, and the buffer is released with the host confirmation.
//! On its way a frame passes the vendor stages: pending (waiting for its
//! link), PAS-queued (FIFO in the PAS ring, counted as active), reserved on a
//! pipe slot and finally scheduled on the MAC.

use crate::global::Global;
use core::cell::Cell;

use super::ContextId;
#[cfg(target_arch = "arm")]
use super::{
    ACCESS_CATEGORY_TO_QUEUE, CLASS_HOST, NO_AGGREGATE, QUEUE_TO_ACCESS_CATEGORY, STATUS_PENDING,
    Protection, SlotImage, contexts, emit_protection_descriptor, pipes,
};
#[cfg(target_arch = "arm")]
use super::hw::{read_u8, read_u16, write_u8, write_u16, write_u32};
#[cfg(target_arch = "arm")]
use crate::packet_ram;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HostConfirmationFields {
    pub(crate) tx_rate: u8,
    pub(crate) flags: u16,
    pub(crate) rate_try: [u32; 3],
}

/// The fields a class-0 confirmation reports.
#[cfg(target_arch = "arm")]
pub(crate) fn confirmation_fields(id: ContextId) -> HostConfirmationFields {
    let context = id.get();
    HostConfirmationFields {
        tx_rate: context.tx_rate.get(),
        flags: context.completion_flags.get(),
        rate_try: core::array::from_fn(|index| context.rate_try[index].get()),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostTxPhase {
    Submitted,
    Classified,
    PostCryptoQueued,
    PendingEligible,
    PasQueued,
    SchedulerReserved,
    Scheduled,
    Completing,
}

pub const fn valid_phase_transition(current: HostTxPhase, next: HostTxPhase) -> bool {
    matches!(
        (current, next),
        (HostTxPhase::Submitted, HostTxPhase::Classified)
            | (HostTxPhase::Classified, HostTxPhase::PostCryptoQueued)
            | (HostTxPhase::PostCryptoQueued, HostTxPhase::PendingEligible)
            | (HostTxPhase::PendingEligible, HostTxPhase::PasQueued)
            | (HostTxPhase::PendingEligible, HostTxPhase::Completing)
            | (HostTxPhase::PasQueued, HostTxPhase::SchedulerReserved)
            | (HostTxPhase::SchedulerReserved, HostTxPhase::PasQueued)
            | (HostTxPhase::SchedulerReserved, HostTxPhase::Scheduled)
            | (HostTxPhase::Scheduled, HostTxPhase::PasQueued)
            | (HostTxPhase::Scheduled, HostTxPhase::Completing)
    )
}

/// One borrowed HIF request and its class-0 context. The release token stays
/// here until the confirmation handoff or an explicit abort.
#[cfg(target_arch = "arm")]
pub struct RetainedHostTx {
    context: ContextId,
    release: crate::hif::RequestReleaseToken,
    packet_id: u32,
    phase: HostTxPhase,
}

#[cfg(target_arch = "arm")]
impl RetainedHostTx {
    pub const fn context(&self) -> ContextId {
        self.context
    }

    pub const fn packet_id(&self) -> u32 {
        self.packet_id
    }

    /// An aggregate publication took the frame to the MAC.
    #[cfg(target_arch = "arm")]
    pub(crate) fn mark_scheduled(&mut self) {
        self.phase = HostTxPhase::Scheduled;
    }

    pub const fn phase(&self) -> HostTxPhase {
        self.phase
    }

    pub fn transition(&mut self, next: HostTxPhase) -> bool {
        if !valid_phase_transition(self.phase, next) {
            return false;
        }
        self.phase = next;
        true
    }

    /// Undo admission before the frame was queued anywhere.
    pub fn abort(self) -> crate::hif::RequestReleaseToken {
        contexts().free(self.context);
        self.release
    }

    /// Cancel a context that is not yet on hardware.
    pub fn cancel_before_pas(self) -> Result<crate::hif::RequestReleaseToken, CancelError> {
        match self.phase {
            HostTxPhase::Submitted
            | HostTxPhase::Classified
            | HostTxPhase::PostCryptoQueued
            | HostTxPhase::PendingEligible => {}
            HostTxPhase::PasQueued => {
                PAS_RING.remove(self.context)?;
                release_pas_accounting();
            }
            HostTxPhase::SchedulerReserved | HostTxPhase::Scheduled | HostTxPhase::Completing => {
                return Err(CancelError::HardwareOwned);
            }
        }
        contexts().free(self.context);
        Ok(self.release)
    }

    /// Return the context after completion and hand the HIF request token to
    /// the confirmation handoff.
    pub fn finish(self) -> crate::hif::RequestReleaseToken {
        contexts().free(self.context);
        self.release
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostPrepareError {
    WrongPhase,
    Header(HeaderClassificationError),
    Crypto(crate::crypto::CcmpError),
    Timing(crate::tx::ProbeBuildError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PostCryptoError {
    WrongPhase,
    ExistingStatus(u16),
    Descriptor(crate::tx::ProbeBuildError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CancelError {
    HardwareOwned,
    MissingFromPasRing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderClassificationError {
    Truncated,
    InvalidLength,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HeaderClassification {
    pub frame_control: u16,
    pub header_length: u16,
    pub payload_length: u16,
    pub qos_control: u16,
    pub tid: u8,
    pub flags: u32,
    pub assign_sequence: bool,
}

/// Pure ordinary-data subset of vendor `tx_classify_hdr_len`. It preserves
/// four-address, QoS, HT-control, multicast/no-ACK, QoS ACK-policy, and
/// sequence-assignment decisions without touching VIF counters or hardware.
pub fn classify_header(
    frame: &[u8],
    initial_flags: u32,
) -> Result<HeaderClassification, HeaderClassificationError> {
    if frame.len() < 2 || frame.len() > u16::MAX as usize {
        return Err(HeaderClassificationError::Truncated);
    }
    let frame_control = u16::from_le_bytes([frame[0], frame[1]]);
    // A BlockAckReq is a 20-octet control frame, shorter than the 24-octet data
    // header, so its shape has to be recognised before any data-frame minimum is
    // applied. mac80211 sends one through this path whenever an aggregate member
    // has to be abandoned; it carries no payload, no QoS control, and no sequence
    // to assign, because its own start sequence is what moves the peer's reorder
    // window. It must reach the air unaltered.
    if frame_control & 0x00fc == 0x0084 {
        if frame.len() < 20 {
            return Err(HeaderClassificationError::Truncated);
        }
        let bar_control = u16::from_le_bytes([frame[16], frame[17]]);
        return Ok(HeaderClassification {
            frame_control,
            header_length: 20,
            // The MPDU is the frame as retained, which includes the four octets of
            // FCS space appended above for a control frame.
            payload_length: (frame.len() - 20) as u16,
            qos_control: 0,
            // mac80211 encodes the compressed BAR control as
            // `CBMTID_COMPRESSED_BA (0x0004) | (tid << 12)`, so the TID is the
            // high nibble of the little-endian control word, not the low one.
            tid: ((bar_control >> 12) & 0x0f) as u8,
            // Bit 14 selects the BlockAck response class (`frame_kind` 0x0c in
            // `compute_single_frame_pas_timing`), which is what a BlockAckReq is
            // answered with - a compressed BlockAck after SIFS, not an ACK. It is
            // also the class the host TX path retires frames on: publishing the
            // BAR with the no-response class (0x0200) left it uncompleted and the
            // host's TID queue stalled behind it (measured: 261 datagrams in 313
            // seconds against roughly 31,000 in 30).
            flags: initial_flags | 0x1000 | 0x4000,
            assign_sequence: false,
        });
    }
    if frame.len() < 24 {
        return Err(HeaderClassificationError::Truncated);
    }
    let four_address = frame_control & 0x0300 == 0x0300;
    let base_header = if four_address { 30 } else { 24 };
    if frame.len() < base_header {
        return Err(HeaderClassificationError::Truncated);
    }

    let mut flags = initial_flags | 0x1000;
    if frame[4] & 1 != 0 {
        flags |= 0x300;
    }

    let qos = frame_control & 0x008f == 0x0088;
    let mut header_length = base_header;
    let mut qos_control = 0;
    let mut tid = 0xff;
    let mut assign_sequence = false;
    if qos {
        let qos_offset = base_header;
        if frame.len() < qos_offset + 2 {
            return Err(HeaderClassificationError::Truncated);
        }
        qos_control = u16::from_le_bytes([frame[qos_offset], frame[qos_offset + 1]]);
        tid = (qos_control & 0x0f) as u8;
        flags |= 0x0040_0000;
        if frame[4] & 1 == 0 {
            flags |= 1;
            // Vendor assigns a sequence number for ordinary QoS data unless
            // subtype bit 6 selects the exceptional shape.
            if frame_control & 0x0040 == 0 {
                flags |= 0x2000_0000;
                assign_sequence = true;
            }
        }
        let ack_policy = u32::from((qos_control & 0x7f) >> 5);
        flags = (flags & 0xffcf_ffff) | (ack_policy << 20);
        if ack_policy == 1 || ack_policy == 3 {
            flags |= 0x200;
        }
        header_length += if frame_control & 0x8000 != 0 { 6 } else { 2 };
    }
    if frame.len() < header_length {
        return Err(HeaderClassificationError::InvalidLength);
    }

    Ok(HeaderClassification {
        frame_control,
        header_length: header_length as u16,
        payload_length: (frame.len() - header_length) as u16,
        qos_control,
        tid,
        flags,
        assign_sequence,
    })
}

/// Next 802.11 sequence control word per internal link and TID.
pub(crate) struct SequenceCounters([[core::cell::Cell<u16>; 16]; 10]);

static SEQUENCE_COUNTERS: Global<SequenceCounters> = Global::new(SequenceCounters::new());

impl SequenceCounters {
    pub(crate) const fn new() -> Self {
        Self([const { [const { core::cell::Cell::new(0) }; 16] }; 10])
    }

    /// The sequence control word the next frame on `link`/`tid` gets.
    pub(crate) fn peek(&self, link: u16, tid: u8) -> u16 {
        self.0
            .get(usize::from(link))
            .and_then(|row| row.get(usize::from(tid)))
            .map_or(0, core::cell::Cell::get)
    }

    /// Return the sequence control word for the next frame on `link`/`tid`
    /// and advance the counter.
    pub(crate) fn next(&self, link: u16, tid: u8) -> u16 {
        let Some(counter) = self
            .0
            .get(usize::from(link))
            .and_then(|row| row.get(usize::from(tid)))
        else {
            return 0;
        };
        let sequence = counter.get();
        counter.set(sequence.wrapping_add(0x10) & 0xfff0);
        sequence
    }
}

/// Sequence counter row of host `link` on `interface`: an access point
/// numbers each station's frames on its own row (the station's host link),
/// a station its AP's on the interface's internal link.
#[cfg(target_arch = "arm")]
fn sequence_row(interface: u8, link: u8) -> u16 {
    match crate::vif::link_peer(interface, link) {
        Some(_) if link != 0 => u16::from(link),
        _ => crate::vif::internal_link(interface).unwrap_or(0),
    }
}

/// Sequence number the next QoS data frame of `tid` to the peer on `link`
/// gets.
#[cfg(target_arch = "arm")]
pub fn next_sequence(interface: u8, link: u8, tid: u8) -> u16 {
    SEQUENCE_COUNTERS.peek(sequence_row(interface, link), tid) >> 4
}

/// Assign the per-peer/TID sequence number to a QoS data frame: a receiver
/// keeps its BlockAck window per transmitter, so frames to another station
/// must not leave holes.
#[cfg(target_arch = "arm")]
fn assign_sequence_number(id: ContextId, frame_address: u32, tid: u8) {
    let context = id.get();
    let row = sequence_row(context.interface.get(), context.host_link.get());
    let sequence = SEQUENCE_COUNTERS.next(row, tid);
    write_u16(frame_address.wrapping_add(0x16) as usize, sequence);
}

/// The link of the peer a frame's TX BlockAck session is with: a station's
/// AP (0) or the access point's station the frame is for.
pub fn block_ack_link(id: ContextId) -> Option<u8> {
    let context = id.get();
    let link = context.host_link.get();
    crate::vif::link_peer(context.interface.get(), link).map(|_| link)
}

/// TX BlockAck streams: one per peer link and TID.
pub const STREAMS: usize = crate::vif::LINKS * 8;

/// A stream's index.
pub const fn stream(link: u8, tid: u8) -> usize {
    link as usize * 8 + (tid & 7) as usize
}

/// A QoS data frame's stream and sequence number, when its link has a peer.
pub(crate) fn block_ack_stream(id: ContextId) -> Option<(usize, u16)> {
    id.get().block_ack.get().map(|(stream, sequence)| (usize::from(stream), sequence))
}

/// Apply ordinary `tx_classify_hdr_len` and assign the vendor per-link/TID
/// sequence number in the retained HIF frame. Returns the TID of a QoS data
/// frame. Encryption follows ([`start_encryption`]), in admission order.
#[cfg(target_arch = "arm")]
pub fn classify(retained: &mut RetainedHostTx) -> Result<Option<u8>, HostPrepareError> {
    if retained.phase != HostTxPhase::Submitted {
        return Err(HostPrepareError::WrongPhase);
    }
    let context = retained.context.get();
    let frame_address = context.frame.get();
    let mut frame_length = context.frame_length.get();
    // A 20-octet control MPDU is not transmitted by the MAC, while the same frame at
    // 24 octets goes out - those four octets are where the FCS sits, so the MPDU the
    // peer sees is a complete BlockAckReq (measured: 0 on air at 20 octets, 51 at 24).
    // Extend the retained frame in place and let the classification report the real
    // MPDU length rather than trimming it back to 20.
    if frame_length >= 20
        && read_u8(frame_address as usize) & 0xfc == 0x84
    {
        let fcs = frame_address as usize + usize::from(frame_length);
        (fcs..fcs + 4).for_each(|address| write_u8(address, 0));
        frame_length += 4;
        context.frame_length.set(frame_length);
    }
    // SAFETY: the retained context owns its packet-RAM frame buffer until
    // completion, and nothing else accesses it while it is classified in
    // place.
    let frame = unsafe {
        core::slice::from_raw_parts_mut(frame_address as *mut u8, usize::from(frame_length))
    };
    let classification =
        classify_header(frame, context.flags.get()).map_err(HostPrepareError::Header)?;

    if classification.assign_sequence {
        assign_sequence_number(retained.context, frame_address, classification.tid);
    }
    let block_ack = block_ack_link(retained.context).zip(qos_sequence(retained.context));
    context.block_ack.set(block_ack.map(|(link, (tid, sequence))| (stream(link, tid) as u8, sequence)));
    let interface = context.interface.get();
    context.frame_control.set(classification.frame_control);
    context.flags.set(classification.flags);
    context.frame_kind.set(0xff);
    context.duration_slot.set(crate::mac::low_mac().pas(interface).slot_bits.get() & 1);
    retained.phase = HostTxPhase::Classified;
    Ok(classification.assign_sequence.then_some(classification.tid))
}

/// Start encrypting a classified frame in place.
#[cfg(target_arch = "arm")]
pub fn start_encryption(
    retained: &mut RetainedHostTx,
    crypto: &mut crate::crypto::Crypto,
) -> Result<crate::crypto::Started, HostPrepareError> {
    if retained.phase != HostTxPhase::Classified {
        return Err(HostPrepareError::WrongPhase);
    }
    let context = retained.context.get();
    // SAFETY: the retained context owns its packet-RAM frame buffer until
    // completion. The host TX driver keeps the job with the context and
    // touches neither the frame nor the context's release until `poll` or
    // `abandon` returns the job.
    unsafe {
        let frame =
            core::slice::from_raw_parts_mut(context.frame.get() as *mut u8, usize::from(context.frame_length.get()));
        crypto.start_tx(frame, context.interface.get())
    }
    .map_err(HostPrepareError::Crypto)
}

/// Time an encrypted (or unprotected) frame for the PAS.
#[cfg(target_arch = "arm")]
pub fn finish_preparation(retained: &mut RetainedHostTx) -> Result<(), HostPrepareError> {
    crate::tx::prepare_single_frame_pas_timing(retained.context.get()).map_err(HostPrepareError::Timing)
}

/// Sequence number and TID of a retained QoS data frame, read from its header.
#[cfg(target_arch = "arm")]
pub(crate) fn qos_sequence(id: ContextId) -> Option<(u8, u16)> {
    let context = id.get();
    let frame = context.frame.get() as usize;
    let frame_control = read_u16(frame);
    // QoS data carrying a payload (not QoS-null): the frames we number.
    (frame_control & 0x00cc == 0x0088).then(|| {
        let tid = (read_u16(frame + 24) & 0x0f) as u8;
        (tid, read_u16(frame + 22) >> 4)
    })
}

/// Stage the frame's descriptor and queue the frame as pending (the mode-0
/// tail insertion of the post-crypto callback).
///
/// # Safety
/// The context must be classified, encrypted and timed.
#[cfg(target_arch = "arm")]
pub fn enqueue_post_crypto(retained: &mut RetainedHostTx) -> Result<(), PostCryptoError> {
    if retained.phase != HostTxPhase::Classified {
        return Err(PostCryptoError::WrongPhase);
    }
    let context = retained.context.get();
    let status = context.terminal_status.get();
    if status < STATUS_PENDING {
        return Err(PostCryptoError::ExistingStatus(status));
    }
    crate::tx::build_host_frame_descriptor(retained.context)
        .map_err(PostCryptoError::Descriptor)?;
    retained.phase = HostTxPhase::PostCryptoQueued;
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingServiceReport {
    LeaveQueued,
    Complete(u16),
    PasQueued,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingServiceError {
    WrongPhase,
    Timing(crate::tx::ProbeBuildError),
    PhyState,
    PasRingFull,
}

/// A PAS-queued frame expires this long after admission.
#[cfg(any(test, target_arch = "arm"))]
const fn pas_submission_expired(submitted: u32, now: u32) -> bool {
    (submitted.wrapping_sub(now).wrapping_add(0x007a_1200) as i32) < 0
}

const PAS_RING_SIZE: usize = 64;

/// FIFO of frames released to PAS scheduling. Reservation takes entries out
/// of the middle and leaves holes; the next insertion compacts them.
struct PasRing {
    head: Cell<u8>,
    tail: Cell<u8>,
    entries: [Cell<Option<ContextId>>; PAS_RING_SIZE],
}

#[cfg(target_arch = "arm")]
static PAS_RING: Global<PasRing> = Global::new(PasRing {
    head: Cell::new(0),
    tail: Cell::new(0),
    entries: [const { Cell::new(None) }; PAS_RING_SIZE],
});

const fn next_index(index: u8) -> u8 {
    index.wrapping_add(1) & (PAS_RING_SIZE as u8 - 1)
}

impl PasRing {
    #[cfg(test)]
    const fn new() -> Self {
        Self { head: Cell::new(0), tail: Cell::new(0), entries: [const { Cell::new(None) }; PAS_RING_SIZE] }
    }

    fn entry(&self, index: u8) -> &Cell<Option<ContextId>> {
        &self.entries[usize::from(index) & (PAS_RING_SIZE - 1)]
    }

    fn position(&self, id: ContextId) -> Option<u8> {
        let (head, tail) = (self.head.get(), self.tail.get());
        let mut index = head;
        while index != tail {
            if self.entry(index).get() == Some(id) {
                return Some(index);
            }
            index = next_index(index);
        }
        None
    }

    /// Compact the live entries to start at the old tail, then append `id`.
    fn push(&self, id: ContextId) -> Result<(), PendingServiceError> {
        let old_tail = self.tail.get();
        let mut scan = self.head.get();
        let mut write = old_tail;
        while scan != old_tail {
            if let Some(value) = self.entry(scan).take() {
                self.entry(write).set(Some(value));
                write = next_index(write);
            }
            scan = next_index(scan);
        }
        self.head.set(old_tail);
        self.tail.set(write);
        let next = next_index(write);
        if next == old_tail {
            return Err(PendingServiceError::PasRingFull);
        }
        self.entry(write).set(Some(id));
        self.tail.set(next);
        Ok(())
    }

    /// Insert `id` before the head.
    fn push_front(&self, id: ContextId) -> bool {
        let head = self.head.get().wrapping_sub(1) & (PAS_RING_SIZE as u8 - 1);
        if head == self.tail.get() || self.entry(head).get().is_some() {
            return false;
        }
        self.entry(head).set(Some(id));
        self.head.set(head);
        true
    }

    /// Leave a hole where `id` was; the next insertion compacts it.
    fn remove(&self, id: ContextId) -> Result<(), CancelError> {
        let position = self.position(id).ok_or(CancelError::MissingFromPasRing)?;
        self.entry(position).set(None);
        Ok(())
    }

    /// Take `id` out for a reservation and move the head past leading holes.
    /// Returns what [`Self::restore`] needs to undo it.
    fn take(&self, position: u8) -> u8 {
        let original_head = self.head.get();
        self.entry(position).set(None);
        let tail = self.tail.get();
        let mut head = original_head;
        while head != tail && self.entry(head).get().is_none() {
            head = next_index(head);
        }
        self.head.set(head);
        original_head
    }

    fn restore(&self, id: ContextId, position: u8, original_head: u8) {
        self.entry(position).set(Some(id));
        self.head.set(original_head);
    }
}

#[cfg(target_arch = "arm")]
fn claim_pas_accounting() -> bool {
    let contexts = contexts();
    let active = contexts.active_pas.get();
    if active == 0 && !crate::phy::advance_awake_station_tx() {
        return false;
    }
    contexts.active_pas.set(active.wrapping_add(1));
    true
}

#[cfg(target_arch = "arm")]
fn release_pas_accounting() {
    let contexts = contexts();
    contexts.active_pas.set(contexts.active_pas.get().wrapping_sub(1));
}

#[cfg(target_arch = "arm")]
fn release_pending_to_pas(retained: &mut RetainedHostTx) -> Result<(), PendingServiceError> {
    crate::tx::prepare_single_frame_pas_timing(retained.context.get())
        .map_err(PendingServiceError::Timing)?;
    if !claim_pas_accounting() {
        return Err(PendingServiceError::PhyState);
    }
    if let Err(error) = PAS_RING.push(retained.context) {
        release_pas_accounting();
        return Err(error);
    }
    retained.phase = HostTxPhase::PasQueued;
    Ok(())
}

/// The pending task (`task_b88e`) for one frame: wait for its link, reject it
/// or release it to PAS scheduling.
#[cfg(target_arch = "arm")]
pub fn service_pending(
    retained: &mut RetainedHostTx,
) -> Result<PendingServiceReport, PendingServiceError> {
    if retained.phase == HostTxPhase::PendingEligible {
        // A frame the PAS cannot take is retried, but not past its lifetime,
        // and not at all when it cannot be timed: it would never confirm.
        return match release_pending_to_pas(retained) {
            Ok(()) => Ok(PendingServiceReport::PasQueued),
            Err(PendingServiceError::Timing(_)) => Ok(PendingServiceReport::Complete(10)),
            Err(_) if submission_expired(retained.context) => Ok(PendingServiceReport::Complete(10)),
            Err(error) => Err(error),
        };
    }
    if retained.phase != HostTxPhase::PostCryptoQueued {
        return Err(PendingServiceError::WrongPhase);
    }

    let context = retained.context.get();
    let interface = context.interface.get();
    let link_bit = 1_u16.wrapping_shl(u32::from(context.host_link.get()));
    let now = crate::platform::timer_now();
    let expired = (context.submit_timer.get().wrapping_sub(now).wrapping_add(0x004c_4b40) as i32) < 0;

    let Some(vif) = crate::vif::vif(interface) else {
        return Err(PendingServiceError::WrongPhase);
    };
    let active_mask = vif.allowed_links.get();
    let effective_mask = vif.effective_links.get();
    let mut effective_link = effective_mask & link_bit != 0;
    let frame_kind = context.frame_control.get() & 0xff;
    if !effective_link && frame_kind != 0xd0 && effective_mask == 0 {
        let radio_state = vif.activity_state.get();
        if radio_state == 2 || radio_state == 3 {
            let effective_mask = if interface < 2 {
                vif.recompute_effective_links()
            } else {
                vif.effective_links.set(active_mask);
                active_mask
            };
            effective_link = effective_mask & link_bit != 0;
        }
    }
    let state = vif.mode.get();
    let decision = pending_task_decision(PendingTaskInput {
        active_link: active_mask & link_bit != 0,
        link_gate_requests_removal: effective_link || frame_kind == 0xd0,
        vif_operating: state == 4 || state == 6,
        expired,
    });

    match decision {
        PendingTaskDecision::LeaveQueued => Ok(PendingServiceReport::LeaveQueued),
        PendingTaskDecision::Complete(status) => {
            retained.phase = HostTxPhase::PendingEligible;
            Ok(PendingServiceReport::Complete(status))
        }
        PendingTaskDecision::ReleaseToPas => {
            retained.phase = HostTxPhase::PendingEligible;
            release_pending_to_pas(retained)?;
            Ok(PendingServiceReport::PasQueued)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NonAggregateSchedulerInput {
    pub expired: bool,
    pub pipe: u8,
    pub idle_pipe_mask: u8,
    pub ring_contains_frame: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NonAggregateSchedulerDecision {
    LeaveQueued,
    Complete(u16),
    ReservePipe(u8),
}

pub const fn non_aggregate_scheduler_decision(
    input: NonAggregateSchedulerInput,
) -> NonAggregateSchedulerDecision {
    if input.expired {
        return NonAggregateSchedulerDecision::Complete(10);
    }
    if !input.ring_contains_frame
        || input.pipe >= 4
        || input.idle_pipe_mask & (1 << input.pipe) == 0
    {
        return NonAggregateSchedulerDecision::LeaveQueued;
    }
    NonAggregateSchedulerDecision::ReservePipe(input.pipe)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerReserveError {
    WrongPhase,
    SchedulerBlocked,
    LeaveQueued,
    Expired,
    PipeStateUnavailable,
    Descriptor(crate::tx::ProbeBuildError),
}

/// The pipe a queued frame's access category maps to.
#[cfg(target_arch = "arm")]
pub fn scheduled_pipe(retained: &RetainedHostTx) -> u8 {
    let access_category = retained.context.get().access_category.get();
    ACCESS_CATEGORY_TO_QUEUE
        .get(usize::from(access_category))
        .copied()
        .unwrap_or(u8::MAX)
}

/// Remove an unscheduled PAS frame for rejection/expiry confirmation.
#[cfg(target_arch = "arm")]
pub fn reject_unscheduled_pas(retained: &mut RetainedHostTx) -> Result<(), CancelError> {
    if retained.phase != HostTxPhase::PasQueued {
        return Err(CancelError::HardwareOwned);
    }
    PAS_RING.remove(retained.context)?;
    release_pas_accounting();
    retained.phase = HostTxPhase::PendingEligible;
    Ok(())
}

/// Software-owned reservation produced by the non-aggregate scheduler. It has
/// taken the frame out of the PAS ring and built the pipe-slot descriptor,
/// but it has not advanced the producer or triggered hardware.
#[cfg(target_arch = "arm")]
pub struct HostSchedulerReservation {
    context: ContextId,
    pipe: u8,
    slot: u8,
    ring_position: u8,
    original_ring_head: u8,
    original_flags: u32,
    original_slot: SlotImage,
    original_prefix: Option<(u8, SlotImage)>,
}

#[cfg(target_arch = "arm")]
impl HostSchedulerReservation {
    pub const fn pipe(&self) -> u8 {
        self.pipe
    }

    pub const fn slot(&self) -> u8 {
        self.slot
    }

    /// Cross the irreversible producer/MAC-trigger boundary.
    ///
    /// # Safety
    /// This reservation must exclusively own the pipe slot and retained frame.
    pub fn publish(
        self,
        engine: &mut crate::tx::TxEngine,
        retained: &mut RetainedHostTx,
    ) -> Result<(), (Self, crate::tx::ProbeBuildError)> {
        self.publish_in_batch(engine, retained, crate::tx::BatchPosition::Only)
    }

    /// Stage this reservation as part of a pipe batch. `Only` is the single
    /// frame path; `First`/`Middle`/`Last` write slots without arming. After
    /// every member succeeds, the batch owner arms them together through
    /// `finalize_staged_host_class0_pipe`.
    ///
    /// # Safety
    /// Same as `publish`.
    pub fn publish_in_batch(
        self,
        engine: &mut crate::tx::TxEngine,
        retained: &mut RetainedHostTx,
        batch: crate::tx::BatchPosition,
    ) -> Result<(), (Self, crate::tx::ProbeBuildError)> {
        if retained.context != self.context || retained.phase != HostTxPhase::SchedulerReserved {
            return Err((self, crate::tx::ProbeBuildError::PipeSlotOwnershipMismatch));
        }
        if let Err(error) =
            crate::tx::publish_host_class0_slot(engine, self.context, self.pipe, self.slot, batch)
        {
            return Err((self, error));
        }
        retained.phase = HostTxPhase::Scheduled;
        Ok(())
    }

    /// Restore software ownership before any pipe producer or hardware trigger.
    pub fn cancel(self, retained: &mut RetainedHostTx) -> bool {
        if retained.context != self.context || retained.phase != HostTxPhase::SchedulerReserved {
            return false;
        }
        self.context.get().flags.set(self.original_flags);
        pipes().pipe(self.pipe).slot(self.slot).restore(self.original_slot);
        if let Some((slot, image)) = self.original_prefix {
            pipes().pipe(self.pipe).slot(slot).restore(image);
        }
        PAS_RING.restore(self.context, self.ring_position, self.original_ring_head);
        retained.phase = HostTxPhase::PasQueued;
        true
    }
}

/// Put a frame an aggregate did not deliver back at the head of the PAS ring,
/// so it goes out before newer frames: the recipient only accepts frames
/// within its window of the oldest one it is missing.
#[cfg(target_arch = "arm")]
pub(crate) fn requeue_at_pas_head(id: ContextId) -> bool {
    let context = id.get();
    // Undo the aggregate's changes; the retry bit marks the retransmission.
    context.flags.set((context.flags.get() & !(0x20 | 1 << 26)) | 0x10);
    context.try_count.set(context.try_count.get().wrapping_add(1));
    if crate::tx::prepare_single_frame_pas_timing(context).is_err() || !claim_pas_accounting() {
        return false;
    }
    if !PAS_RING.push_front(id) {
        release_pas_accounting();
        return false;
    }
    true
}

/// A queued frame whose submission is too old to publish.
#[cfg(target_arch = "arm")]
pub fn submission_expired(id: ContextId) -> bool {
    pas_submission_expired(id.get().submit_timer.get(), crate::platform::timer_now())
}

/// Take a queued frame off the PAS ring; returns what `restore_to_pas` needs.
#[cfg(target_arch = "arm")]
pub(crate) fn take_from_pas(id: ContextId) -> Option<(u8, u8)> {
    let position = PAS_RING.position(id)?;
    Some((position, PAS_RING.take(position)))
}

#[cfg(target_arch = "arm")]
pub(crate) fn restore_to_pas(id: ContextId, position: u8, original_head: u8) {
    PAS_RING.restore(id, position, original_head);
}

/// Contexts on the PAS ring, head first.
#[cfg(target_arch = "arm")]
pub fn queued_pas_in_order() -> impl Iterator<Item = ContextId> {
    let tail = PAS_RING.tail.get();
    core::iter::successors(Some(PAS_RING.head.get()), |&index| Some(next_index(index)))
        .take_while(move |&index| index != tail)
        .filter_map(|index| PAS_RING.entry(index).get())
}

/// Vendor marks the first ordinary descriptor with bit 26 and every later
/// descriptor in the same scheduler batch with bit 27 before
/// `txp_build_pipe_descriptor(..., 0)`.
const fn scheduler_batch_control_bits(original: u32, staged: u8) -> u32 {
    original
        | if staged == 0 {
            1 << 26
        } else {
            0x0800_0000
        }
}

/// Select one PAS-ring frame, reserve its mapped idle pipe slot, and emit the
/// descriptor without starting hardware.
///
/// # Safety
/// Global PAS and pipe state must be exclusively runtime-owned.
#[cfg(target_arch = "arm")]
pub fn reserve_non_aggregate_scheduler(
    retained: &mut RetainedHostTx,
    engine: &crate::tx::TxEngine,
) -> Result<HostSchedulerReservation, SchedulerReserveError> {
    reserve_non_aggregate_scheduler_at(retained, None, engine)
}

/// Reserve a later slot in the same software-owned non-aggregate batch.
///
/// # Safety
/// `pipe` must be the pipe reserved by the first batch member, and `slot`
/// must be its next unowned ring slot. No staged slot may have crossed the MAC
/// trigger boundary.
#[cfg(target_arch = "arm")]
pub fn reserve_non_aggregate_scheduler_in_batch(
    retained: &mut RetainedHostTx,
    pipe: u8,
    slot: u8,
    staged: u8,
    engine: &crate::tx::TxEngine,
) -> Result<HostSchedulerReservation, SchedulerReserveError> {
    reserve_non_aggregate_scheduler_at(retained, Some((pipe, slot, staged)), engine)
}

#[cfg(target_arch = "arm")]
fn reserve_non_aggregate_scheduler_at(
    retained: &mut RetainedHostTx,
    batch: Option<(u8, u8, u8)>,
    engine: &crate::tx::TxEngine,
) -> Result<HostSchedulerReservation, SchedulerReserveError> {
    if retained.phase != HostTxPhase::PasQueued {
        return Err(SchedulerReserveError::WrongPhase);
    }
    if crate::mac::low_mac().receive_gate.get() != 0 {
        return Err(SchedulerReserveError::SchedulerBlocked);
    }

    let id = retained.context;
    let context = id.get();
    let policy = engine.protection.get(usize::from(context.interface.get()))
        .copied().unwrap_or(crate::tx::ProtectionPolicy::DISABLED);
    crate::tx::apply_protection_policy(id, policy).map_err(SchedulerReserveError::Descriptor)?;
    let pipe = scheduled_pipe(retained);
    let ring_position = PAS_RING.position(id);
    let decision = non_aggregate_scheduler_decision(NonAggregateSchedulerInput {
        expired: pas_submission_expired(context.submit_timer.get(), crate::platform::timer_now()),
        pipe,
        idle_pipe_mask: idle_pipe_mask(),
        ring_contains_frame: ring_position.is_some(),
    });
    match decision {
        NonAggregateSchedulerDecision::LeaveQueued => {
            return Err(SchedulerReserveError::LeaveQueued);
        }
        NonAggregateSchedulerDecision::Complete(_) => {
            return Err(SchedulerReserveError::Expired);
        }
        NonAggregateSchedulerDecision::ReservePipe(_) => {}
    }
    let Some(ring_position) = ring_position else {
        return Err(SchedulerReserveError::LeaveQueued);
    };

    let (slot, staged) = if let Some((batch_pipe, slot, staged)) = batch {
        if batch_pipe != pipe {
            return Err(SchedulerReserveError::LeaveQueued);
        }
        (slot & 3, staged)
    } else {
        (pipes().pipe(pipe).producer.get() & 3, 0)
    };
    let protected = context.protection.get();
    if protected.is_some() && batch.is_some() {
        return Err(SchedulerReserveError::LeaveQueued);
    }
    let prefix_slot = protected.map(|_| slot);
    let slot = if protected.is_some() { slot.wrapping_add(1) & 3 } else { slot };
    let record = pipes().pipe(pipe);
    if record.slot(slot).frame.get().is_some()
        || prefix_slot.is_some_and(|prefix| record.slot(prefix).frame.get().is_some())
    {
        return Err(SchedulerReserveError::LeaveQueued);
    }
    if record.hardware_ring(pipe).is_none() {
        return Err(SchedulerReserveError::PipeStateUnavailable);
    }
    let command = packet_ram::tx_command(usize::from(pipe), usize::from(slot)) as u32;
    let original_flags = context.flags.get();

    let original_ring_head = PAS_RING.take(ring_position);
    context.flags.set(scheduler_batch_control_bits(original_flags, staged)
        | if protected.is_some_and(|timing| timing.kind == Protection::CtsToSelf) { 1 << 15 } else { 0 });
    let original_slot = record.slot(slot).reserve(id, context.frame_kind.get());
    let original_prefix = protected.map(|timing| {
        let prefix = prefix_slot.unwrap();
        let image = record.slot(prefix).reserve(id, timing.kind.expected_status());
        record.slot(prefix).prefix.set(Some(timing.kind));
        (prefix, image)
    });
    write_u32(command as usize, 0);
    write_u32(command as usize + 4, 0);
    write_u32(command as usize + 8, 0xdc00_0000);
    let emitted = crate::tx::emit_descriptor(context, command + 0x0c).and_then(|_| {
        if let Some(prefix) = prefix_slot {
            emit_protection_descriptor(context, packet_ram::tx_command(usize::from(pipe), usize::from(prefix)) as u32)?;
        }
        Ok(())
    });
    if let Err(error) = emitted {
        PAS_RING.restore(id, ring_position, original_ring_head);
        context.flags.set(original_flags);
        record.slot(slot).restore(original_slot);
        if let Some((prefix, image)) = original_prefix { record.slot(prefix).restore(image); }
        return Err(SchedulerReserveError::Descriptor(error));
    }
    retained.phase = HostTxPhase::SchedulerReserved;
    Ok(HostSchedulerReservation {
        context: id,
        pipe,
        slot,
        ring_position,
        original_ring_head,
        original_flags,
        original_slot,
        original_prefix,
    })
}

/// Pipes that are not armed, as a bit mask.
#[cfg(target_arch = "arm")]
fn idle_pipe_mask() -> u8 {
    (0..4_u8)
        .filter(|&pipe| pipes().pipe(pipe).state.get() == 0)
        .fold(0, |mask, pipe| mask | (1 << pipe))
}

#[cfg(not(target_arch = "arm"))]
pub fn initialize_host_pool() {}

/// Map a requested host rate to one the station path supports.
const fn station_data_rate(requested: u8) -> u8 {
    // XR819 indices 4/5 are unsupported ERP-PBCC gaps and are not emitted by
    // cw1200. Keep a safe fallback for malformed requests while preserving the
    // host-selected CCK, OFDM, and HT rate.
    if requested <= 21 && requested != 4 && requested != 5 {
        requested
    } else {
        0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostAdmissionError {
    MalformedRequest,
    PoolEmpty,
}

/// Admit an ordinary WSM TX request into a host context while retaining the
/// original HIF buffer. This stops at the `tx_lmac_req_submit` boundary.
#[cfg(target_arch = "arm")]
pub fn admit_host_tx(
    buffer: crate::hif::RequestBuffer,
    interface: u8,
    link: u8,
) -> Result<RetainedHostTx, (crate::hif::RequestBuffer, HostAdmissionError)> {
    let request = match crate::wsm::TxRequest::parse(buffer.payload()) {
        Ok(request) => request,
        Err(_) => return Err((buffer, HostAdmissionError::MalformedRequest)),
    };
    let Some(id) = contexts().allocate_host() else {
        return Err((buffer, HostAdmissionError::PoolEmpty));
    };
    let context = id.get();
    let flags = 0x0080_0000
        | ((request.ht_tx_parameters >> 11) & 0xe0)
        | if request.flags & 1 != 0 { 0x0001_0000 } else { 0 }
        | if request.ht_tx_parameters & 3 == 1 { 8 } else { 0 };
    context.class.set(CLASS_HOST);
    context.frame.set(request.frame.as_ptr() as u32);
    context.frame_length.set(request.frame.len().min(u16::MAX as usize) as u16);
    context.interface.set(interface);
    // The WSM header's link: the station a frame of an access point is for.
    context.host_link.set(link);
    context.block_ack.set(None);
    context.protection.set(None);
    context.erp_protection.set(false);
    context.access_category.set(QUEUE_TO_ACCESS_CATEGORY[usize::from(request.queue_id & 3)]);
    context.tx_rate.set(station_data_rate(request.max_tx_rate));
    context.retry_policy.set((request.flags & 0x7f) >> 4);
    context.request_flag_rate_bits.set((request.flags & 0x0f) >> 1);
    context.flags.set(flags);
    context.submit_timer.set(crate::platform::timer_now());
    context.try_count.set(0);
    context.rate_try.iter().for_each(|rate_try| rate_try.set(0));
    context.auxiliary_state.set(0);
    context.completion_flags.set(0);
    context.saved_status.set(0);
    context.aggregate_slot.set(NO_AGGREGATE);
    let packet_id = request.packet_id;
    Ok(RetainedHostTx {
        context: id,
        release: buffer.into_release(),
        packet_id,
        phase: HostTxPhase::Submitted,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingTaskInput {
    pub active_link: bool,
    /// The frame's link is effective (or it is an action frame): release it.
    pub link_gate_requests_removal: bool,
    pub vif_operating: bool,
    pub expired: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingTaskDecision {
    LeaveQueued,
    Complete(u16),
    ReleaseToPas,
}

/// `task_b88e` for class-0 traffic.
pub const fn pending_task_decision(input: PendingTaskInput) -> PendingTaskDecision {
    if !input.active_link {
        return PendingTaskDecision::Complete(0x14);
    }
    if input.link_gate_requests_removal || input.vif_operating {
        return PendingTaskDecision::ReleaseToPas;
    }
    if input.expired {
        return PendingTaskDecision::Complete(10);
    }
    PendingTaskDecision::LeaveQueued
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pas_ring_requeues_before_the_head() {
        let ring = PasRing::new();
        let id = |index| ContextId::host(index).unwrap();
        let order = |ring: &PasRing| {
            let mut index = ring.head.get();
            core::iter::from_fn(|| {
                while index != ring.tail.get() {
                    let entry = ring.entry(index).get();
                    index = next_index(index);
                    if entry.is_some() {
                        return entry;
                    }
                }
                None
            })
            .collect::<std::vec::Vec<_>>()
        };
        (0..4).for_each(|index| ring.push(id(index)).unwrap());
        // An aggregate takes 0 and 1; both come back, oldest first.
        ring.take(ring.position(id(0)).unwrap());
        ring.take(ring.position(id(1)).unwrap());
        assert!(ring.push_front(id(1)) && ring.push_front(id(0)));
        ring.push(id(4)).unwrap();
        assert_eq!(order(&ring), (0..5).map(id).collect::<std::vec::Vec<_>>());
        // Full: the slot before the head is the tail.
        let full = PasRing::new();
        (0..PAS_RING_SIZE - 1).for_each(|index| full.push(id(index % 30)).unwrap());
        assert!(!full.push_front(id(0)));
    }

    #[test]
    fn host_tx_phases_cannot_skip_queue_or_scheduler_ownership() {
        assert!(valid_phase_transition(
            HostTxPhase::Submitted,
            HostTxPhase::Classified
        ));
        assert!(valid_phase_transition(
            HostTxPhase::PasQueued,
            HostTxPhase::SchedulerReserved
        ));
        assert!(valid_phase_transition(
            HostTxPhase::SchedulerReserved,
            HostTxPhase::Scheduled
        ));
        assert!(valid_phase_transition(
            HostTxPhase::Scheduled,
            HostTxPhase::PasQueued
        ));
        assert!(!valid_phase_transition(
            HostTxPhase::Submitted,
            HostTxPhase::Scheduled
        ));
        assert!(!valid_phase_transition(
            HostTxPhase::Completing,
            HostTxPhase::Submitted
        ));
    }

    #[test]
    fn header_classifier_preserves_qos_ack_policy_and_payload_split() {
        let mut frame = [0_u8; 40];
        frame[..2].copy_from_slice(&0x0188_u16.to_le_bytes());
        frame[4] = 0x02;
        frame[24..26].copy_from_slice(&0x0065_u16.to_le_bytes());

        let classification = classify_header(&frame, 0x0080_0000);
        assert_eq!(
            classification,
            Ok(HeaderClassification {
                frame_control: 0x0188,
                header_length: 26,
                payload_length: 14,
                qos_control: 0x0065,
                tid: 5,
                flags: 0x20f0_1201,
                assign_sequence: true,
            })
        );
    }

    #[test]
    fn header_classifier_accepts_a_compressed_block_ack_request() {
        // 20-octet control frame: FC, duration, RA, TA, BAR control, start
        // sequence. mac80211 encodes compressed TID 3 as 0x3004
        // (CBMTID_COMPRESSED_BA 0x0004 | tid << 12) and the ordinary-data path
        // would reject the frame as truncated.
        let mut frame = [0_u8; 20];
        frame[..2].copy_from_slice(&0x0084_u16.to_le_bytes());
        frame[4] = 0x02;
        frame[16..18].copy_from_slice(&0x3004_u16.to_le_bytes());
        frame[18..20].copy_from_slice(&0x0120_u16.to_le_bytes());

        assert_eq!(
            classify_header(&frame, 0),
            Ok(HeaderClassification {
                frame_control: 0x0084,
                header_length: 20,
                payload_length: 0,
                qos_control: 0,
                tid: 3,
                // Bit 14 marks the BlockAck response class, which is what a
                // BlockAckReq is answered with and what the host path retires on.
                flags: 0x5000,
                assign_sequence: false,
            })
        );
    }

    #[test]
    fn header_classifier_handles_four_address_ht_control() {
        let mut frame = [0_u8; 48];
        frame[..2].copy_from_slice(&0x8388_u16.to_le_bytes());
        frame[4] = 0x03;
        frame[30..32].copy_from_slice(&0x0002_u16.to_le_bytes());

        let classification = classify_header(&frame, 0);
        assert_eq!(classification.map(|value| value.header_length), Ok(36));
        assert_eq!(classification.map(|value| value.payload_length), Ok(12));
        assert_eq!(classification.map(|value| value.tid), Ok(2));
        assert_eq!(classification.map(|value| value.assign_sequence), Ok(false));
    }

    #[test]
    fn station_data_rate_rejects_only_unsupported_indices() {
        assert_eq!(station_data_rate(0), 0);
        assert_eq!(station_data_rate(3), 3);
        assert_eq!(station_data_rate(4), 0);
        assert_eq!(station_data_rate(5), 0);
        assert_eq!(station_data_rate(6), 6);
        assert_eq!(station_data_rate(21), 21);
        assert_eq!(station_data_rate(u8::MAX), 0);
    }

    #[test]
    fn pas_expiry_preserves_vendor_deadline_and_timer_wrap() {
        assert!(!pas_submission_expired(100, 100 + 0x007a_1200));
        assert!(pas_submission_expired(100, 101 + 0x007a_1200));
        assert!(!pas_submission_expired(u32::MAX - 10, 5));
    }

    #[test]
    fn non_aggregate_scheduler_requires_ring_pipe_and_idle_ownership() {
        let ready = NonAggregateSchedulerInput {
            expired: false,
            pipe: 2,
            idle_pipe_mask: 0b0100,
            ring_contains_frame: true,
        };
        assert_eq!(
            non_aggregate_scheduler_decision(ready),
            NonAggregateSchedulerDecision::ReservePipe(2)
        );
        assert_eq!(
            non_aggregate_scheduler_decision(NonAggregateSchedulerInput {
                idle_pipe_mask: 0,
                ..ready
            }),
            NonAggregateSchedulerDecision::LeaveQueued
        );
        assert_eq!(
            non_aggregate_scheduler_decision(NonAggregateSchedulerInput {
                expired: true,
                ..ready
            }),
            NonAggregateSchedulerDecision::Complete(10)
        );
    }

    #[test]
    fn ordinary_batch_marks_first_and_later_descriptors_differently() {
        let original = 0x0000_1234;
        assert_eq!(scheduler_batch_control_bits(original, 0), original | 0x0400_0000);
        assert_eq!(scheduler_batch_control_bits(original, 1), original | 0x0800_0000);
        assert_eq!(scheduler_batch_control_bits(original, 3), original | 0x0800_0000);
    }

    #[test]
    fn pending_task_distinguishes_wait_reject_and_release() {
        let base = PendingTaskInput {
            active_link: true,
            link_gate_requests_removal: false,
            vif_operating: false,
            expired: false,
        };
        assert_eq!(pending_task_decision(base), PendingTaskDecision::LeaveQueued);
        assert_eq!(
            pending_task_decision(PendingTaskInput { active_link: false, ..base }),
            PendingTaskDecision::Complete(0x14)
        );
        assert_eq!(
            pending_task_decision(PendingTaskInput { vif_operating: true, ..base }),
            PendingTaskDecision::ReleaseToPas
        );
        assert_eq!(
            pending_task_decision(PendingTaskInput { link_gate_requests_removal: true, ..base }),
            PendingTaskDecision::ReleaseToPas
        );
        assert_eq!(
            pending_task_decision(PendingTaskInput { expired: true, ..base }),
            PendingTaskDecision::Complete(10)
        );
    }
}
