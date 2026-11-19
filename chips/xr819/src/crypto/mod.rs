//! CCMP for the 802.11 frames the firmware sends and receives.
//!
//! [`Crypto`] owns the key table and the AES-CCM engine. The engine runs one
//! transfer at a time, in place, on a frame in packet RAM, while the CPU
//! goes on with other work: a started transfer is a [`Job`] the caller hands
//! back to [`Crypto::poll`] (or [`Crypto::abandon`]) before it may touch or
//! release the frame. RX decryption and TX encryption take turns when both
//! wait for the engine.

mod engine;
#[cfg(target_arch = "arm")]
mod selftest;

use core::num::NonZeroU32;
use engine::{Engine, Transfer};

const MAX_KEYS: usize = 24;
/// Bounded counter history for recently removed/replaced keys. These
/// records never authorize RX or TX; only the active key table does.
const KEY_HISTORY: usize = 4;
const CCMP_HEADER_LEN: usize = 8;
const MAX_PACKET_NUMBER: u64 = 0x0000_ffff_ffff_ffff;
pub(crate) const CCMP_MIC_LEN: usize = 8;
const AES_GROUP: u8 = 4;
const AES_PAIRWISE: u8 = 5;

mod hardware_kat {
    include!(concat!(env!("OUT_DIR"), "/hardware_ccmp_kat.rs"));
}

#[derive(Clone, Copy)]
struct KeyRecord {
    active: bool,
    key_type: u8,
    if_id: u8,
    peer: [u8; 6],
    key_id: u8,
    key: [u8; 16],
    generation: u32,
    tx_pn: u64,
    /// The last packet number accepted per TID (non-QoS data uses TID 0,
    /// as mac80211 does). Frames reach the host without their CCMP header,
    /// so mac80211 cannot check replays: this is the only check.
    rx_pn: [u64; 16],
}

impl KeyRecord {
    const EMPTY: Self = Self {
        active: false,
        key_type: 0,
        if_id: 0,
        peer: [0; 6],
        key_id: 0,
        key: [0; 16],
        generation: 0,
        tx_pn: 0,
        rx_pn: [0; 16],
    };

    fn same_key(&self, other: &Self) -> bool {
        self.active && other.active
            && self.key_type == other.key_type && self.if_id == other.if_id
            && self.peer == other.peer && self.key_id == other.key_id
            && self.key == other.key
    }

    fn keep_counters(&mut self, previous: &Self) {
        self.tx_pn = self.tx_pn.max(previous.tx_pn);
        for (pn, last) in self.rx_pn.iter_mut().zip(&previous.rx_pn) {
            *pn = (*pn).max(*last);
        }
    }
}

/// The key installation used by an RX job. Carry it with that frame only
/// after successful authentication, through reordering until publication.
/// A removed/replaced installation cannot authorize a delayed plaintext.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RxKeyEpoch(NonZeroU32);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyError {
    InvalidIndex,
    InvalidLength,
    UnsupportedType,
    GenerationExhausted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CcmpError {
    MalformedFrame,
    MissingKey,
    Authentication,
    HardwareTimeout,
    InvalidDmaAddress,
    PacketNumberExhausted,
}

fn frame_control(frame: &[u8]) -> Result<u16, CcmpError> {
    frame
        .get(..2)
        .map(|value| u16::from_le_bytes([value[0], value[1]]))
        .ok_or(CcmpError::MalformedFrame)
}

fn header_length(frame_control: u16) -> usize {
    let mut length = if frame_control & 0x0300 == 0x0300 {
        30
    } else {
        24
    };
    if frame_control & 0x008c == 0x0088 {
        length += if frame_control & 0x8000 != 0 { 6 } else { 2 };
    }
    length
}

pub fn is_protected_eapol(frame: &[u8]) -> bool {
    let Ok(control) = frame_control(frame) else {
        return false;
    };
    if control & 0x400c != 0x4008 {
        return false;
    }
    let llc = header_length(control) + CCMP_HEADER_LEN;
    frame.get(llc..llc + 8).is_some_and(|value| {
        value[..6] == [0xaa, 0xaa, 0x03, 0x00, 0x00, 0x00] && value[6..] == [0x88, 0x8e]
    })
}

pub fn strip_qos_control(frame: &mut [u8]) -> Result<usize, CcmpError> {
    let control = frame_control(frame)?;
    if control & 0x008f != 0x0088 {
        return Ok(frame.len());
    }
    let base = if control & 0x0300 == 0x0300 { 30 } else { 24 };
    let qos_length = if control & 0x8000 != 0 { 6 } else { 2 };
    if frame.len() < base + qos_length {
        return Err(CcmpError::MalformedFrame);
    }
    frame.copy_within(base + qos_length.., base);
    frame[0] &= !0x80;
    frame[1] &= !0x80;
    Ok(frame.len() - qos_length)
}

pub fn strip_ccmp_reservation(frame: &mut [u8]) -> Result<usize, CcmpError> {
    let control = frame_control(frame)?;
    if control & 0x400c != 0x4008 {
        return Ok(frame.len());
    }
    let header = header_length(control);
    if frame.len() < header + CCMP_HEADER_LEN + CCMP_MIC_LEN {
        return Err(CcmpError::MalformedFrame);
    }
    let plaintext_end = frame.len() - CCMP_MIC_LEN;
    frame.copy_within(header + CCMP_HEADER_LEN..plaintext_end, header);
    frame[1] &= !0x40;
    Ok(frame.len() - CCMP_HEADER_LEN - CCMP_MIC_LEN)
}

fn qos_tid(frame: &[u8], frame_control: u16) -> Result<u8, CcmpError> {
    if frame_control & 0x008c != 0x0088 {
        return Ok(0);
    }
    let offset = if frame_control & 0x0300 == 0x0300 {
        30
    } else {
        24
    };
    frame
        .get(offset)
        .map(|value| value & 0x0f)
        .ok_or(CcmpError::MalformedFrame)
}

fn build_aad(frame: &[u8], frame_control: u16) -> Result<([u8; 30], usize), CcmpError> {
    if frame.len() < 24 {
        return Err(CcmpError::MalformedFrame);
    }
    let mut aad = [0_u8; 30];
    let mut masked = frame_control & !(0x0800 | 0x1000 | 0x2000);
    if frame_control & 0x000c != 0 {
        masked &= !0x0070;
    }
    masked |= 0x4000;
    let qos = frame_control & 0x008c == 0x0088;
    if qos {
        masked &= !0x8000;
    }
    aad[..2].copy_from_slice(&masked.to_le_bytes());
    aad[2..20].copy_from_slice(&frame[4..22]);
    aad[20] = frame[22] & 0x0f;
    aad[21] = 0;
    let mut length = 22;
    if frame_control & 0x0300 == 0x0300 {
        aad[22..28].copy_from_slice(frame.get(24..30).ok_or(CcmpError::MalformedFrame)?);
        length = 28;
    }
    if qos {
        aad[length] = qos_tid(frame, frame_control)?;
        aad[length + 1] = 0;
        length += 2;
    }
    Ok((aad, length))
}

#[cfg(not(target_arch = "arm"))]
fn build_nonce(frame: &[u8], frame_control: u16, pn: [u8; 6]) -> Result<[u8; 13], CcmpError> {
    let mut nonce = [0_u8; 13];
    nonce[0] = qos_tid(frame, frame_control)?;
    nonce[1..7].copy_from_slice(frame.get(10..16).ok_or(CcmpError::MalformedFrame)?);
    nonce[7..].copy_from_slice(&pn);
    Ok(nonce)
}

/// A protected data frame's transmitter, Key ID, group addressing, TID and
/// packet number.
fn rx_packet_number(frame: &[u8]) -> Option<([u8; 6], u8, bool, u8, u64)> {
    let control = frame_control(frame).ok()?;
    if control & 0x400c != 0x4008 {
        return None;
    }
    let header = header_length(control);
    let ccmp = frame.get(header..header + CCMP_HEADER_LEN)?;
    let pn = u64::from_le_bytes([ccmp[0], ccmp[1], ccmp[4], ccmp[5], ccmp[6], ccmp[7], 0, 0]);
    let transmitter = frame.get(10..16)?.try_into().ok()?;
    Some((transmitter, ccmp[3] >> 6, frame[4] & 1 != 0, qos_tid(frame, control).ok()?, pn))
}

fn pn_bytes(value: u64) -> [u8; 6] {
    [
        (value >> 40) as u8,
        (value >> 32) as u8,
        (value >> 24) as u8,
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
    ]
}

/// Return the 48-bit CCMP packet number already embedded in a protected TX
/// frame. Diagnostics use this as the non-wrapping on-air identity; unlike the
/// 12-bit sequence number it remains unique across a sustained flow.
#[cfg(test)]
pub(crate) fn tx_frame_packet_number(frame: &[u8]) -> Option<u64> {
    let control = frame_control(frame).ok()?;
    if control & 0x400c != 0x4008 {
        return None;
    }
    let header = header_length(control);
    let ccmp = frame.get(header..header + CCMP_HEADER_LEN)?;
    Some(
        u64::from(ccmp[0])
            | (u64::from(ccmp[1]) << 8)
            | (u64::from(ccmp[4]) << 16)
            | (u64::from(ccmp[5]) << 24)
            | (u64::from(ccmp[6]) << 32)
            | (u64::from(ccmp[7]) << 40),
    )
}


#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Direction {
    Rx,
    Tx,
}

/// A transfer the engine runs on a caller's frame. The engine reads and
/// writes the frame until the job comes back from [`Crypto::poll`] or
/// [`Crypto::abandon`]; until then the caller must keep the frame and leave
/// it alone. Dropping a job instead halts the firmware.
#[must_use = "the engine owns the frame until the job is polled to completion or abandoned"]
pub struct Job {
    direction: Direction,
    rx_key_epoch: Option<RxKeyEpoch>,
}

impl Job {
    pub fn rx_key_epoch(&self) -> Option<RxKeyEpoch> {
        self.rx_key_epoch
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        crate::halt_always!();
    }
}

/// What starting a transfer did.
pub enum Started {
    /// The frame is not protected: nothing to do.
    Plain,
    /// The engine works on the frame.
    Running(Job),
    /// The engine is busy, or it is the other direction's turn: try again.
    Busy,
}

/// Starts refused to the other direction while one waits for its turn.
/// TX sealing tries twice per main-loop pass and RX once, so this covers
/// a few passes.
const RESERVATION_REFUSALS: u8 = 8;

/// The key table and the AES-CCM engine.
pub struct Crypto {
    keys: [KeyRecord; MAX_KEYS],
    history: [KeyRecord; KEY_HISTORY],
    next_history: usize,
    last_generation: u32,
    /// Slot selected by the latest new group-key install per interface.
    tx_group_slots: [Option<u8>; crate::vif::VIF_COUNT],
    engine: Engine,
    /// The direction whose transfer the engine runs.
    running: Option<Direction>,
    /// Outcomes of finished transfers not yet collected, by direction.
    finished: [Option<Result<(), CcmpError>>; 2],
    /// A direction that found the engine busy: it goes next. The other
    /// direction is refused while the reservation stands, at most
    /// `RESERVATION_REFUSALS` times, in case the waiting one stopped asking.
    waiting: Option<(Direction, u8)>,
    #[cfg(target_arch = "arm")]
    self_test: [u32; 22],
}

impl Default for Crypto {
    fn default() -> Self {
        Self::new()
    }
}

impl Crypto {
    /// Changes whenever a transfer starts, finishes or is collected.
    pub fn activity(&self, activity: crate::activity::Activity) -> crate::activity::Activity {
        let direction = |direction: Option<Direction>| match direction {
            None => 0,
            Some(Direction::Rx) => 1,
            Some(Direction::Tx) => 2,
        };
        let outcome = |outcome: &Option<Result<(), CcmpError>>| match outcome {
            None => 0,
            Some(Ok(())) => 1,
            Some(Err(_)) => 2,
        };
        let (waiting, refusals) = self.waiting.map_or((None, 0), |(direction, refusals)| (Some(direction), refusals));
        activity
            .with(direction(self.running))
            .with(outcome(&self.finished[0]) | outcome(&self.finished[1]) << 2)
            .with(direction(waiting) | u32::from(refusals) << 2)
    }

    pub const fn new() -> Self {
        Self {
            keys: [KeyRecord::EMPTY; MAX_KEYS],
            history: [KeyRecord::EMPTY; KEY_HISTORY],
            next_history: 0,
            last_generation: 0,
            tx_group_slots: [None; crate::vif::VIF_COUNT],
            engine: Engine::new(),
            running: None,
            finished: [None; 2],
            waiting: None,
            #[cfg(target_arch = "arm")]
            self_test: [0; 22],
        }
    }

    pub fn add_key(&mut self, if_id: u8, request: &crate::wsm::AddKeyRequest<'_>) -> Result<(), KeyError> {
        let record = self.keys.get_mut(usize::from(request.index)).ok_or(KeyError::InvalidIndex)?;
        let mut key = KeyRecord {
            active: true,
            key_type: request.key_type,
            if_id,
            ..KeyRecord::EMPTY
        };
        match request.key_type {
            AES_GROUP => {
                key.key.copy_from_slice(&request.data[..16]);
                key.key_id = request.data[16] & 3;
                // The receive sequence counter of the handshake, little
                // endian: the last packet number the AP sent with this key.
                let mut rsc = [0_u8; 8];
                rsc[..6].copy_from_slice(&request.data[20..26]);
                key.rx_pn = [u64::from_le_bytes(rsc); 16];
            }
            AES_PAIRWISE => {
                key.peer.copy_from_slice(&request.data[..6]);
                key.key.copy_from_slice(&request.data[8..24]);
            }
            _ => return Err(KeyError::UnsupportedType),
        }
        // Reinstalling the same key must not reuse its CCMP nonces.
        let identical = record.same_key(&key);
        if identical {
            key.keep_counters(record);
            key.generation = record.generation;
        } else {
            let generation = self.last_generation.checked_add(1).ok_or(KeyError::GenerationExhausted)?;
            let replaced = *record;
            // Restore a retained identical key's counters before remembering
            // the displaced slot, which could evict that history entry.
            if let Some(previous) = self.history.iter().find(|previous| previous.same_key(&key)) {
                key.keep_counters(previous);
            }
            key.tx_pn = key.tx_pn.max(self.successor_packet_number(&key));
            self.remember(replaced);
            key.generation = generation;
            self.last_generation = generation;
        }
        let record = &mut self.keys[usize::from(request.index)];
        *record = key;
        // Vendor ADD_KEY (0x71ee) selects the group key for AP TX. Retain
        // its slot too, so another installed key with the same Key ID cannot
        // win by table order. cw1200 re-uploads retained keys in slot order
        // after START/JOIN; identical reinstalls must not select an old GTK.
        if !identical && key.key_type == AES_GROUP
            && let Some(selected) = self.tx_group_slots.get_mut(usize::from(if_id))
        {
            *selected = Some(request.index);
        }
        Ok(())
    }

    /// An index beyond the table holds no key: removing it succeeds.
    pub fn remove_key(&mut self, index: u8) {
        if let Some(record) = self.keys.get_mut(usize::from(index)) {
            let removed = core::mem::replace(record, KeyRecord::EMPTY);
            self.remember(removed);
        }
    }

    /// Remember counter high-water marks, not receive authority. Removing
    /// a key revokes it even if no successor is ever installed.
    fn remember(&mut self, key: KeyRecord) {
        if !key.active {
            return;
        }
        let slot = self.history.iter().position(|previous| previous.same_key(&key))
            .unwrap_or_else(|| {
                let slot = self.next_history;
                self.next_history = (slot + 1) % KEY_HISTORY;
                slot
            });
        self.history[slot] = key;
    }

    /// Where a new pairwise key's packet numbers start: after its peer's
    /// replaced key's. A rekey without Extended Key ID reuses Key ID 0, and
    /// the frames sealed with the old key and still queued go out after the
    /// peer has installed the new one. A peer whose hardware decrypts them
    /// with the old key but checks replays against the new key's counters
    /// (mac80211's "can't safely" PTK0 replace) raises that TID's counter to
    /// their packet numbers, and drops every new-key frame below them as a
    /// replay until the next rekey: seconds of lost unicast on a TID, with
    /// each frame ACKed. Continuing the old numbers keeps the new key above
    /// anything the old one sealed. A receiver's counters start at zero with
    /// a new key, so it takes the higher start as it is.
    fn successor_packet_number(&self, key: &KeyRecord) -> u64 {
        if key.key_type != AES_PAIRWISE {
            return 0;
        }
        self.history.iter().chain(&self.keys)
            .filter(|previous| previous.active && previous.key_type == AES_PAIRWISE
                && previous.if_id == key.if_id && previous.peer == key.peer)
            .map(|previous| previous.tx_pn)
            .max().unwrap_or(0)
    }

    /// The pairwise key of a unicast receiver, a group key otherwise: an
    /// access point holds its group key before any client's pairwise key.
    fn tx_key(&mut self, if_id: u8, receiver: &[u8]) -> Option<&mut KeyRecord> {
        let group_addressed = receiver[0] & 1 != 0;
        if group_addressed {
            let slot = self.tx_group_slots.get(usize::from(if_id)).copied().flatten()?;
            self.keys.get_mut(usize::from(slot)).filter(|key| key.active && key.if_id == if_id && key.key_type == AES_GROUP)
        } else {
            self.keys.iter_mut().find(|key| key.active && key.if_id == if_id && key.key_type == AES_PAIRWISE && key.peer == receiver)
        }
    }

    fn rx_key(&self, if_id: u8, transmitter: &[u8], key_id: u8, group_addressed: bool) -> Option<&KeyRecord> {
        self.rx_key_index(if_id, transmitter, key_id, group_addressed).map(|index| &self.keys[index])
    }

    fn rx_key_index(&self, if_id: u8, transmitter: &[u8], key_id: u8, group_addressed: bool) -> Option<usize> {
        self.keys.iter().position(|key| {
            key.active
                && key.if_id == if_id
                // Key ID zero is valid for a GTK too. Choose the key class from
                // Address 1 before lookup, rather than letting table order decide.
                && if group_addressed {
                    key.key_type == AES_GROUP && key.key_id == key_id
                } else {
                    key.key_type == AES_PAIRWISE && key_id == 0 && key.peer == transmitter
                }
        })
    }

    /// Check authority before a decrypted frame can affect a reorder window.
    /// Do not advance the replay counter until delivery order is known.
    pub fn rx_key_epoch_is_active(&self, if_id: u8, epoch: RxKeyEpoch) -> bool {
        self.keys.iter().any(|key| key.active && key.if_id == if_id && key.generation == epoch.0.get())
    }

    /// Replay check of a decrypted frame on its way to the host, in the
    /// order the host gets it (after BlockAck reordering): its packet number
    /// must exceed the last one accepted for its key and TID. Accepting it
    /// records it. A frame whose key has gone since it was decrypted fails.
    pub fn accept_rx_packet_number(&mut self, if_id: u8, frame: &[u8], epoch: RxKeyEpoch) -> bool {
        let Some((transmitter, key_id, group_addressed, tid, pn)) = rx_packet_number(frame) else {
            return false;
        };
        let Some(index) = self.rx_key_index(if_id, &transmitter, key_id, group_addressed) else {
            return false;
        };
        let record = &mut self.keys[index];
        if record.generation != epoch.0.get() {
            return false;
        }
        let last = &mut record.rx_pn[usize::from(tid)];
        if pn <= *last {
            return false;
        }
        *last = pn;
        true
    }

    /// No transfer running, waiting or with an outcome not yet taken.
    pub fn settled(&self) -> bool {
        self.running.is_none() && self.waiting.is_none() && self.finished.iter().all(Option::is_none)
    }

    /// Whether the engine has no transfer running; a finished one's outcome
    /// is kept for its owner. A transfer that never finished halts here,
    /// before anything could reprogram an engine that may still be writing.
    fn engine_idle(&mut self) -> bool {
        if let Some(direction) = self.running
            && let Some(outcome) = self.engine.poll()
        {
            if outcome == Err(CcmpError::HardwareTimeout) {
                crate::halt_always!();
            }
            self.finished[direction as usize] = Some(outcome);
            self.running = None;
        }
        self.running.is_none()
    }

    /// Whether `direction` may start a transfer now. A direction that finds
    /// the engine busy goes next: the other one is refused until it has
    /// started, so a steady stream one way cannot starve the other.
    fn may_start(&mut self, direction: Direction) -> bool {
        if self.finished[direction as usize].is_some() {
            return false;
        }
        if !self.engine_idle() {
            if self.waiting.is_none() {
                self.waiting = Some((direction, 0));
            }
            return false;
        }
        match self.waiting {
            Some((waiting, refusals)) if waiting != direction && refusals < RESERVATION_REFUSALS => {
                self.waiting = Some((waiting, refusals + 1));
                false
            }
            _ => {
                self.waiting = None;
                true
            }
        }
    }

    /// Start encrypting a frame for transmission in place: assign the next
    /// packet number and fill the reserved CCMP header and MIC.
    ///
    /// # Safety
    /// On `Started::Running` the engine reads and writes `frame` by DMA after
    /// this call returns: the caller must keep the memory alive and leave it
    /// alone until the job comes back from [`Self::poll`] or [`Self::abandon`]
    /// of this same `Crypto`.
    pub unsafe fn start_tx(&mut self, frame: &mut [u8], if_id: u8) -> Result<Started, CcmpError> {
        let control = frame_control(frame)?;
        if control & 0x400c != 0x4008 {
            return Ok(Started::Plain);
        }
        let header = header_length(control);
        if frame.len() < header + CCMP_HEADER_LEN + CCMP_MIC_LEN {
            return Err(CcmpError::MalformedFrame);
        }
        let receiver: [u8; 6] = frame[4..10].try_into().map_err(|_| CcmpError::MalformedFrame)?;
        self.tx_key(if_id, &receiver).ok_or(CcmpError::MissingKey)?;
        if !self.may_start(Direction::Tx) {
            return Ok(Started::Busy);
        }
        self.encrypt_with_next_pn(frame, control, header, if_id, &receiver)?;
        self.running = Some(Direction::Tx);
        Ok(Started::Running(Job { direction: Direction::Tx, rx_key_epoch: None }))
    }

    fn encrypt_with_next_pn(
        &mut self,
        frame: &mut [u8],
        control: u16,
        header: usize,
        if_id: u8,
        receiver: &[u8; 6],
    ) -> Result<(), CcmpError> {
        let key = self.tx_key(if_id, receiver).ok_or(CcmpError::MissingKey)?;
        if key.tx_pn >= MAX_PACKET_NUMBER {
            return Err(CcmpError::PacketNumberExhausted);
        }
        key.tx_pn += 1;
        let (key, key_id, pn) = (key.key, key.key_id, pn_bytes(key.tx_pn));
        frame[header..header + CCMP_HEADER_LEN]
            .copy_from_slice(&[pn[5], pn[4], 0, 0x20 | (key_id << 6), pn[3], pn[2], pn[1], pn[0]]);
        let (aad, aad_length) = build_aad(frame, control)?;
        let transfer = Transfer {
            frame_control: control,
            header,
            key: &key,
            pn,
            aad: &aad,
            aad_length,
            decrypt: false,
        };
        self.engine.start(frame, &transfer, &mut |_, _| {})
    }

    /// Encrypt a frame and wait for the engine (single management frames).
    /// A transfer already running is waited out; its outcome is kept.
    pub fn encrypt_now(&mut self, frame: &mut [u8], if_id: u8) -> Result<(), CcmpError> {
        let control = frame_control(frame)?;
        if control & 0x400c != 0x4008 {
            return Ok(());
        }
        let header = header_length(control);
        if frame.len() < header + CCMP_HEADER_LEN + CCMP_MIC_LEN {
            return Err(CcmpError::MalformedFrame);
        }
        let receiver: [u8; 6] = frame[4..10].try_into().map_err(|_| CcmpError::MalformedFrame)?;
        self.tx_key(if_id, &receiver).ok_or(CcmpError::MissingKey)?;
        while !self.engine_idle() {
            core::hint::spin_loop();
        }
        self.encrypt_with_next_pn(frame, control, header, if_id, &receiver)?;
        let outcome = self.engine.wait();
        if outcome == Err(CcmpError::HardwareTimeout) {
            crate::halt_always!();
        }
        outcome
    }

    /// Start decrypting a received frame in place.
    ///
    /// # Safety
    /// On `Started::Running` the engine reads and writes `frame` by DMA after
    /// this call returns: the caller must keep the memory alive and leave it
    /// alone until the job comes back from [`Self::poll`] or [`Self::abandon`]
    /// of this same `Crypto`.
    pub unsafe fn start_rx(&mut self, frame: &mut [u8], if_id: u8) -> Result<Started, CcmpError> {
        let control = frame_control(frame)?;
        if control & 0x400c != 0x4008 {
            return Ok(Started::Plain);
        }
        let header = header_length(control);
        if frame.len() < header + CCMP_HEADER_LEN + CCMP_MIC_LEN {
            return Err(CcmpError::MalformedFrame);
        }
        let ccmp: [u8; 8] = frame[header..header + CCMP_HEADER_LEN]
            .try_into()
            .map_err(|_| CcmpError::MalformedFrame)?;
        let pn = [ccmp[7], ccmp[6], ccmp[5], ccmp[4], ccmp[1], ccmp[0]];
        let key_id = ccmp[3] >> 6;
        let transmitter: [u8; 6] = frame[10..16].try_into().map_err(|_| CcmpError::MalformedFrame)?;
        let group_addressed = frame[4] & 1 != 0;
        let record = self.rx_key(if_id, &transmitter, key_id, group_addressed).ok_or(CcmpError::MissingKey)?;
        let key = record.key;
        let epoch = RxKeyEpoch(NonZeroU32::new(record.generation).ok_or(CcmpError::MissingKey)?);
        if !self.may_start(Direction::Rx) {
            return Ok(Started::Busy);
        }
        let (aad, aad_length) = build_aad(frame, control)?;
        let transfer = Transfer {
            frame_control: control,
            header,
            key: &key,
            pn,
            aad: &aad,
            aad_length,
            decrypt: true,
        };
        self.engine.start(frame, &transfer, &mut |_, _| {})?;
        self.running = Some(Direction::Rx);
        Ok(Started::Running(Job { direction: Direction::Rx, rx_key_epoch: Some(epoch) }))
    }

    /// The job's outcome once the engine is done with its frame; the job
    /// back while it still runs. A transfer that never finishes halts: the
    /// engine might still write into a frame its owner would reuse.
    pub fn poll(&mut self, job: Job) -> Result<Result<(), CcmpError>, Job> {
        let direction = job.direction;
        if self.running == Some(direction) {
            self.engine_idle();
        }
        let Some(outcome) = self.finished[direction as usize].take() else {
            return Err(job);
        };
        core::mem::forget(job);
        if outcome == Err(CcmpError::HardwareTimeout) {
            crate::halt_always!();
        }
        Ok(outcome)
    }

    /// Give up a job: wait until the engine has left its frame alone.
    pub fn abandon(&mut self, mut job: Job) {
        loop {
            match self.poll(job) {
                Ok(_) => return,
                Err(running) => job = running,
            }
            core::hint::spin_loop();
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use aes::Aes128;
    use ccm::{
        Ccm,
        aead::{AeadInPlace, KeyInit, generic_array::GenericArray},
        consts::{U8, U13},
    };

    type AesCcmp = Ccm<Aes128, U8, U13>;

    // Every test frame outlives its job and is left alone until collected.
    fn start_tx(crypto: &mut Crypto, frame: &mut [u8], if_id: u8) -> Result<Started, CcmpError> {
        unsafe { crypto.start_tx(frame, if_id) }
    }

    fn start_rx(crypto: &mut Crypto, frame: &mut [u8], if_id: u8) -> Result<Started, CcmpError> {
        unsafe { crypto.start_rx(frame, if_id) }
    }

    fn decrypt_rx_epoch(crypto: &mut Crypto, frame: &mut [u8], if_id: u8) -> Result<Option<RxKeyEpoch>, CcmpError> {
        match start_rx(crypto, frame, if_id)? {
            Started::Plain => Ok(None),
            Started::Running(job) => {
                let epoch = job.rx_key_epoch();
                match crypto.poll(job) {
                    Ok(outcome) => outcome.map(|()| epoch),
                    Err(job) => {
                        crypto.abandon(job);
                        panic!("host engine still running");
                    }
                }
            }
            Started::Busy => panic!("engine busy"),
        }
    }

    /// Decrypt only; production must also check this frame's returned epoch.
    fn decrypt_rx_frame(crypto: &mut Crypto, frame: &mut [u8], if_id: u8) -> Result<bool, CcmpError> {
        decrypt_rx_epoch(crypto, frame, if_id).map(|epoch| epoch.is_some())
    }

    /// Counter-only fixtures model a frame authenticated with the current
    /// key. Lifecycle tests instead retain the epoch from the real RX job.
    fn accept_counter(crypto: &mut Crypto, frame: &[u8]) -> bool {
        let Some((transmitter, key_id, group, _, _)) = rx_packet_number(frame) else { return false; };
        let Some(key) = crypto.rx_key(0, &transmitter, key_id, group) else { return false; };
        let epoch = RxKeyEpoch(NonZeroU32::new(key.generation).unwrap());
        crypto.accept_rx_packet_number(0, frame, epoch)
    }

    fn pairwise_request<'a>(
        payload: &'a mut [u8; 44],
        index: u8,
        peer: [u8; 6],
    ) -> crate::wsm::AddKeyRequest<'a> {
        payload[0] = AES_PAIRWISE;
        payload[1] = index;
        payload[4..10].copy_from_slice(&peer);
        payload[12..28].copy_from_slice(&[0x11; 16]);
        crate::wsm::AddKeyRequest::parse(payload).unwrap()
    }

    #[test]
    fn identical_key_reinstall_preserves_the_next_packet_number() {
        for key_type in [AES_PAIRWISE, AES_GROUP] {
            let mut crypto = Crypto::new();
            let mut payload = [0_u8; 44];
            if key_type == AES_PAIRWISE {
                let _ = pairwise_request(&mut payload, 0, AP);
            } else {
                payload[0] = AES_GROUP;
                payload[4..20].fill(0x11);
                payload[20] = 1;
            }
            let request = crate::wsm::AddKeyRequest::parse(&payload).unwrap();
            crypto.add_key(0, &request).unwrap();
            let plain = {
                let mut frame = tx_frame();
                if key_type == AES_GROUP {
                    frame[4..10].fill(0xff);
                }
                frame
            };
            let mut first = plain;
            crypto.encrypt_now(&mut first, 0).unwrap();
            assert_eq!(tx_frame_packet_number(&first), Some(1));
            crypto.add_key(0, &request).unwrap();
            let mut second = plain;
            crypto.encrypt_now(&mut second, 0).unwrap();
            assert_eq!(tx_frame_packet_number(&second), Some(2));
        }
    }

    #[test]
    fn a_peer_s_replacement_pairwise_key_continues_its_packet_numbers() {
        // Removed then added (cw1200's order), or replaced in place.
        for remove_first in [true, false] {
            let mut crypto = Crypto::new();
            let mut payload = [0_u8; 44];
            crypto.add_key(0, &pairwise_request(&mut payload, 1, AP)).unwrap();
            crypto.keys[1].tx_pn = 0x84ef;
            if remove_first {
                crypto.remove_key(1);
            }
            payload[12] ^= 1;
            crypto.add_key(0, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
            let mut frame = tx_frame();
            crypto.encrypt_now(&mut frame, 0).unwrap();
            assert_eq!(tx_frame_packet_number(&frame), Some(0x84f0));
        }
    }

    #[test]
    fn a_replacement_or_removed_key_starts_a_new_packet_number() {
        // Another interface, another peer, a group key: nothing to continue.
        for change in [0, 1, 3] {
            let mut crypto = Crypto::new();
            let mut payload = [0_u8; 44];
            crypto.add_key(0, &pairwise_request(&mut payload, 0, AP)).unwrap();
            crypto.keys[0].tx_pn = 42;
            let if_id = match change {
                0 => 1,
                1 => { payload[4] ^= 2; 0 }
                2 => { payload[12] ^= 1; 0 }
                3 => { payload[0] = AES_GROUP; 0 }
                _ => { crypto.remove_key(0); 0 }
            };
            crypto.add_key(if_id, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
            assert_eq!(crypto.keys[0].tx_pn, 0);
        }
        let mut crypto = Crypto::new();
        let mut payload = [0_u8; 44];
        payload[0] = AES_GROUP;
        crypto.add_key(0, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
        crypto.keys[0].tx_pn = 42;
        payload[20] = 1;
        crypto.add_key(0, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
        assert_eq!(crypto.keys[0].tx_pn, 0);
    }

    #[test]
    fn tx_packet_number_decodes_ccmp_wire_order() {
        let mut frame = [0_u8; 34];
        frame[..2].copy_from_slice(&0x4088_u16.to_le_bytes());
        frame[26..34].copy_from_slice(&[0x8c, 0x31, 0, 0x20, 0x67, 0x45, 0x23, 0x01]);
        assert_eq!(tx_frame_packet_number(&frame), Some(0x0123_4567_318c));
    }

    #[test]
    fn ccmp_matches_independent_aesccm_known_answer() {
        let mut crypto = Crypto::new();
        {
            crypto.keys[0] = KeyRecord {
                active: true,
                key_type: AES_PAIRWISE,
                if_id: 0,
                peer: [0x20, 0x05, 0xb6, 0xff, 0x01, 0x43],
                key_id: 0,
                key: [
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f,
                ],
                generation: 1,
                tx_pn: 0x0123_4567_89aa,
                rx_pn: [0; 16],
            };
            crypto.keys[1] = KeyRecord {
                peer: [0x12, 0x42, 0x2a, 0x37, 0x70, 0x07],
                generation: 2,
                tx_pn: 0,
                ..crypto.keys[0]
            };
            crypto.last_generation = 2;
        }
        let mut frame = [0_u8; 26 + 8 + 16 + 8];
        frame[..26].copy_from_slice(&[
            0x88, 0x41, 0x00, 0x00, 0x20, 0x05, 0xb6, 0xff, 0x01, 0x43, 0x12, 0x42, 0x2a, 0x37,
            0x70, 0x07, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x30, 0x12, 0x03, 0x00,
        ]);
        frame[34..50].copy_from_slice(&[
            0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
            0x1e, 0x1f,
        ]);

        crypto.encrypt_now(&mut frame, 0).unwrap();
        assert_eq!(
            &frame[26..34],
            &[0xab, 0x89, 0x00, 0x20, 0x67, 0x45, 0x23, 0x01]
        );
        assert_eq!(
            &frame[34..50],
            &[
                0xc7, 0x40, 0x95, 0x9b, 0xf5, 0x9a, 0x96, 0x31, 0xa8, 0xc2, 0xc9, 0xd9, 0x91, 0x0d,
                0x11, 0x4b
            ]
        );
        assert_eq!(
            &frame[50..58],
            &[0x11, 0x85, 0x85, 0x5b, 0xce, 0x30, 0xf3, 0xe9]
        );

        let encrypted = frame;
        assert!(decrypt_rx_frame(&mut crypto, &mut frame, 0).unwrap());
        assert_eq!(
            &frame[34..50],
            &[
                0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x1b, 0x1c, 0x1d,
                0x1e, 0x1f
            ]
        );

        let mut bad_ciphertext = encrypted;
        bad_ciphertext[34] ^= 1;
        assert_eq!(
            decrypt_rx_frame(&mut crypto, &mut bad_ciphertext, 0),
            Err(CcmpError::Authentication)
        );
        let mut bad_mic = encrypted;
        bad_mic[57] ^= 1;
        assert_eq!(
            decrypt_rx_frame(&mut crypto, &mut bad_mic, 0),
            Err(CcmpError::Authentication)
        );
    }

    // Seal a From-DS frame with the specified key, independently of TX key lookup.
    fn sealed_rx_frame(receiver: [u8; 6], key_id: u8, key: &[u8; 16]) -> [u8; 52] {
        let mut frame = [0; 52];
        frame[..2].copy_from_slice(&0x4208_u16.to_le_bytes());
        frame[4..10].copy_from_slice(&receiver);
        frame[10..16].copy_from_slice(&[0x02; 6]);
        frame[16..22].copy_from_slice(&[0x04; 6]);
        frame[24..32].copy_from_slice(&[1, 0, 0, 0x20 | (key_id << 6), 0, 0, 0, 0]);
        frame[32..44].copy_from_slice(b"hello world!");
        let pn = [0, 0, 0, 0, 0, 1];
        let nonce = build_nonce(&frame, 0x4208, pn).unwrap();
        let (aad, aad_length) = build_aad(&frame, 0x4208).unwrap();
        let tag = AesCcmp::new_from_slice(key)
            .unwrap()
            .encrypt_in_place_detached(
                GenericArray::from_slice(&nonce),
                &aad[..aad_length],
                &mut frame[32..44],
            )
            .unwrap();
        frame[44..].copy_from_slice(&tag);
        frame
    }

    /// A frame from [0x02; 6] to `STATION`, sealed with `key` at `pn`.
    fn sealed_pairwise(key: &[u8; 16], pn: u64) -> [u8; 52] {
        let mut frame = [0; 52];
        frame[..2].copy_from_slice(&0x4208_u16.to_le_bytes());
        frame[4..10].copy_from_slice(&STATION);
        frame[10..16].copy_from_slice(&[0x02; 6]);
        frame[16..22].copy_from_slice(&[0x02; 6]);
        let pn = pn.to_be_bytes();
        frame[24..32].copy_from_slice(&[pn[7], pn[6], 0, 0x20, pn[5], pn[4], pn[3], pn[2]]);
        frame[32..44].copy_from_slice(b"hello world!");
        let pn = [pn[2], pn[3], pn[4], pn[5], pn[6], pn[7]];
        let nonce = build_nonce(&frame, 0x4208, pn).unwrap();
        let (aad, aad_length) = build_aad(&frame, 0x4208).unwrap();
        let tag = AesCcmp::new_from_slice(key)
            .unwrap()
            .encrypt_in_place_detached(GenericArray::from_slice(&nonce), &aad[..aad_length], &mut frame[32..44])
            .unwrap();
        frame[44..].copy_from_slice(&tag);
        frame
    }

    fn install_pairwise(crypto: &mut Crypto, index: u8, key: [u8; 16]) {
        let mut payload = [0_u8; 44];
        payload[0] = AES_PAIRWISE;
        payload[1] = index;
        payload[4..10].copy_from_slice(&[0x02; 6]);
        payload[12..28].copy_from_slice(&key);
        crypto.add_key(0, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
    }

    /// Decrypted and accepted by the replay check, as on the way to the host.
    fn received_ok(crypto: &mut Crypto, key: &[u8; 16], pn: u64) -> bool {
        let mut frame = sealed_pairwise(key, pn);
        match decrypt_rx_epoch(crypto, &mut frame, 0) {
            Ok(Some(epoch)) => crypto.accept_rx_packet_number(0, &frame, epoch),
            _ => false,
        }
    }

    #[test]
    fn removing_a_pairwise_key_revokes_receive_authority() {
        let mut crypto = Crypto::new();
        let key = [0x61; 16];
        install_pairwise(&mut crypto, 0, key);
        assert!(received_ok(&mut crypto, &key, 5000));
        crypto.remove_key(0);
        let mut next = sealed_pairwise(&key, 5001);
        assert_eq!(decrypt_rx_frame(&mut crypto, &mut next, 0), Err(CcmpError::MissingKey));
    }

    #[test]
    fn removing_then_readding_the_same_pairwise_key_keeps_replay_state() {
        let mut crypto = Crypto::new();
        let key = [0x62; 16];
        install_pairwise(&mut crypto, 0, key);
        assert!(received_ok(&mut crypto, &key, 300));
        crypto.remove_key(0);
        install_pairwise(&mut crypto, 0, key);
        assert!(!received_ok(&mut crypto, &key, 300));
        assert!(received_ok(&mut crypto, &key, 301));
    }

    #[test]
    fn plaintext_waiting_across_replacement_cannot_raise_the_new_keys_replay_counter() {
        let mut crypto = Crypto::new();
        let old = [0x63; 16];
        install_pairwise(&mut crypto, 0, old);
        assert!(received_ok(&mut crypto, &old, 300));
        let mut waiting = sealed_pairwise(&old, 301);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut waiting, 0).unwrap().unwrap();
        install_pairwise(&mut crypto, 0, [0x64; 16]);
        assert!(!crypto.rx_key_epoch_is_active(0, epoch));
        assert!(!crypto.accept_rx_packet_number(0, &waiting, epoch));
        assert!(received_ok(&mut crypto, &[0x64; 16], 1));
    }

    #[test]
    fn removing_then_readding_the_same_group_key_keeps_replay_state() {
        let mut crypto = Crypto::new();
        let key = [0x65; 16];
        install_group_key(&mut crypto, 0, 2, 1, key);
        let mut frame = sealed_rx_frame([0xff; 6], 1, &key);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut frame, 0).unwrap().unwrap();
        assert!(crypto.accept_rx_packet_number(0, &frame, epoch));
        crypto.remove_key(2);
        install_group_key(&mut crypto, 0, 2, 1, key);
        let mut replay = sealed_rx_frame([0xff; 6], 1, &key);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut replay, 0).unwrap().unwrap();
        assert!(!crypto.accept_rx_packet_number(0, &replay, epoch));
    }

    #[test]
    fn an_in_flight_rx_job_keeps_its_epoch_across_identical_remove_and_readd() {
        let mut crypto = Crypto::new();
        let key = [0x66; 16];
        install_pairwise(&mut crypto, 0, key);
        assert!(received_ok(&mut crypto, &key, 300));
        let mut waiting = sealed_pairwise(&key, 301);
        crypto.engine.held = true;
        let job = running(start_rx(&mut crypto, &mut waiting, 0));
        let epoch = job.rx_key_epoch().unwrap();
        crypto.remove_key(0);
        install_pairwise(&mut crypto, 1, key);
        finish(&mut crypto, job).unwrap();
        assert!(!crypto.accept_rx_packet_number(0, &waiting, epoch));
        assert!(received_ok(&mut crypto, &key, 301), "a fresh job uses the new installation");
    }

    #[test]
    fn an_identical_active_reinstall_keeps_pending_authenticated_rx_valid() {
        let mut crypto = Crypto::new();
        let key = [0x67; 16];
        install_pairwise(&mut crypto, 0, key);
        let mut waiting = sealed_pairwise(&key, 301);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut waiting, 0).unwrap().unwrap();
        install_pairwise(&mut crypto, 0, key);
        assert!(crypto.rx_key_epoch_is_active(0, epoch));
        assert!(crypto.accept_rx_packet_number(0, &waiting, epoch));
    }

    #[test]
    fn stale_group_plaintext_cannot_raise_a_replacement_keys_replay_counter() {
        let mut crypto = Crypto::new();
        let (old, new) = ([0x68; 16], [0x69; 16]);
        install_group_key(&mut crypto, 0, 2, 1, old);
        let mut waiting = sealed_rx_frame([0xff; 6], 1, &old);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut waiting, 0).unwrap().unwrap();
        install_group_key(&mut crypto, 0, 2, 1, new);
        assert!(!crypto.accept_rx_packet_number(0, &waiting, epoch));
        let mut fresh = sealed_rx_frame([0xff; 6], 1, &new);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut fresh, 0).unwrap().unwrap();
        assert!(crypto.accept_rx_packet_number(0, &fresh, epoch));
    }

    #[test]
    fn a_new_gtk_does_not_revoke_another_still_installed_gtk() {
        let mut crypto = Crypto::new();
        let old = [0x72; 16];
        install_group_key(&mut crypto, 0, 2, 1, old);
        let mut waiting = sealed_rx_frame([0xff; 6], 1, &old);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut waiting, 0).unwrap().unwrap();
        install_group_key(&mut crypto, 0, 3, 2, [0x73; 16]);
        assert!(crypto.accept_rx_packet_number(0, &waiting, epoch));
    }

    #[test]
    fn an_authenticated_epoch_is_not_transferable_to_another_interface() {
        let mut crypto = Crypto::new();
        let key = [0x74; 16];
        install_group_key(&mut crypto, 0, 2, 1, key);
        let mut waiting = sealed_rx_frame([0xff; 6], 1, &key);
        let epoch = decrypt_rx_epoch(&mut crypto, &mut waiting, 0).unwrap().unwrap();
        install_group_key(&mut crypto, 1, 3, 1, key);
        assert!(!crypto.rx_key_epoch_is_active(1, epoch));
        assert!(!crypto.accept_rx_packet_number(1, &waiting, epoch));
        assert!(crypto.accept_rx_packet_number(0, &waiting, epoch));
    }

    #[test]
    fn reinstall_keeps_all_tid_counters_even_when_the_key_moves_slots() {
        for group in [false, true] {
            let mut crypto = Crypto::new();
            let key = [0x6a; 16];
            if group { install_group_key(&mut crypto, 0, 0, 1, key); }
            else { install_pairwise(&mut crypto, 0, key); }
            let epoch = crypto.keys[0].generation;
            crypto.keys[0].tx_pn = 42;
            crypto.keys[0].rx_pn = core::array::from_fn(|tid| 100 + tid as u64);
            crypto.remove_key(0);
            if group { install_group_key(&mut crypto, 0, 1, 1, key); }
            else { install_pairwise(&mut crypto, 1, key); }
            assert_eq!(crypto.keys[1].tx_pn, 42);
            assert_eq!(crypto.keys[1].rx_pn, core::array::from_fn(|tid| 100 + tid as u64));
            assert_ne!(crypto.keys[1].generation, epoch);
        }
    }

    #[test]
    fn restoring_history_precedes_eviction_by_the_displaced_slot() {
        let mut crypto = Crypto::new();
        for value in 0..KEY_HISTORY {
            install_pairwise(&mut crypto, 0, [value as u8; 16]);
            crypto.keys[0].rx_pn = [100 + value as u64; 16];
            crypto.remove_key(0);
        }
        install_pairwise(&mut crypto, 0, [0x70; 16]);
        crypto.keys[0].tx_pn = 1000;
        install_pairwise(&mut crypto, 0, [0; 16]);
        assert_eq!(crypto.keys[0].rx_pn, [100; 16]);
        assert_eq!(crypto.keys[0].tx_pn, 1000, "still above this peer's other queued key");
    }

    #[test]
    fn key_epochs_never_wrap_or_mutate_a_live_key_on_exhaustion() {
        let mut crypto = Crypto::new();
        let old = [0x71; 16];
        install_pairwise(&mut crypto, 0, old);
        crypto.last_generation = u32::MAX;
        let mut payload = [0_u8; 44];
        let request = pairwise_request(&mut payload, 0, [0x02; 6]);
        assert_eq!(crypto.add_key(0, &request), Err(KeyError::GenerationExhausted));
        assert_eq!(crypto.keys[0].key, old);
        assert!(received_ok(&mut crypto, &old, 1));
        install_pairwise(&mut crypto, 0, old); // Identical active upload needs no new epoch.
        assert!(received_ok(&mut crypto, &old, 2));
        assert_eq!(core::mem::size_of::<Option<RxKeyEpoch>>(), 4);
    }

    #[test]
    fn replacement_does_not_reauthorize_old_pairwise_stragglers() {
        let (old, new) = ([0x31; 16], [0x32; 16]);
        let mut crypto = Crypto::new();
        install_pairwise(&mut crypto, 3, old);
        assert!(received_ok(&mut crypto, &old, 5000));
        crypto.remove_key(3);
        assert!(!received_ok(&mut crypto, &old, 5001));
        install_pairwise(&mut crypto, 3, new);
        assert!(received_ok(&mut crypto, &new, 1));
        for pn in [1, 5001, 10000] {
            let mut late = sealed_pairwise(&old, pn);
            assert_eq!(decrypt_rx_frame(&mut crypto, &mut late, 0), Err(CcmpError::Authentication));
        }
        assert!(received_ok(&mut crypto, &new, 2));
    }

    #[test]
    fn replacing_a_key_without_removing_it_revokes_old_rx_too() {
        let (old, new) = ([0x41; 16], [0x42; 16]);
        let mut crypto = Crypto::new();
        install_pairwise(&mut crypto, 0, old);
        assert!(received_ok(&mut crypto, &old, 9000));
        install_pairwise(&mut crypto, 0, new);
        assert!(!received_ok(&mut crypto, &old, 9001));
        assert!(received_ok(&mut crypto, &new, 1));
        // An identical active reinstall keeps its replay state.
        install_pairwise(&mut crypto, 0, new);
        assert!(!received_ok(&mut crypto, &new, 1));
    }

    #[test]
    fn received_packet_numbers_never_select_a_revoked_key() {
        let (old, new) = ([0x51; 16], [0x52; 16]);
        let mut crypto = Crypto::new();
        install_pairwise(&mut crypto, 0, old);
        assert!(received_ok(&mut crypto, &old, 300));
        install_pairwise(&mut crypto, 0, new);
        // Only the current key serves, irrespective of packet-number ranges.
        assert!(received_ok(&mut crypto, &new, 301));
        let mut straggler = sealed_pairwise(&old, 302);
        assert_eq!(decrypt_rx_frame(&mut crypto, &mut straggler, 0), Err(CcmpError::Authentication));
    }

    #[test]
    fn rx_key_class_is_selected_by_receiver_not_table_order() {
        let pairwise = KeyRecord {
            active: true,
            key_type: AES_PAIRWISE,
            peer: [0x02; 6],
            key: [0x11; 16],
            generation: 1,
            ..KeyRecord::EMPTY
        };
        for group_id in 0..4 {
            let group = KeyRecord {
                active: true,
                key_type: AES_GROUP,
                key_id: group_id,
                key: [0x22; 16],
                generation: 2,
                ..KeyRecord::EMPTY
            };
            for records in [[pairwise, group], [group, pairwise]] {
                let mut crypto = Crypto::new();
                crypto.keys[..2].copy_from_slice(&records);
                for (receiver, key_id, key) in [
                    ([0x06; 6], 0, pairwise.key),
                    ([0xff; 6], group_id, group.key),
                    ([0x01, 0, 0x5e, 0, 0, 1], group_id, group.key),
                ] {
                    let mut frame = sealed_rx_frame(receiver, key_id, &key);
                    assert_eq!(decrypt_rx_frame(&mut crypto, &mut frame, 0), Ok(true));
                    assert_eq!(&frame[32..44], b"hello world!");
                }
            }
        }
    }

    #[test]
    fn tx_key_class_is_selected_by_receiver_not_table_order() {
        let mut crypto = Crypto::new();
        crypto.keys[0] = KeyRecord { active: true, key_type: AES_GROUP, key: [0x22; 16], ..KeyRecord::EMPTY };
        crypto.tx_group_slots[0] = Some(0);
        crypto.keys[1] =
            KeyRecord { active: true, key_type: AES_PAIRWISE, peer: [0x02; 6], key: [0x11; 16], ..KeyRecord::EMPTY };
        assert_eq!(crypto.tx_key(0, &[0x02; 6]).map(|key| key.key), Some([0x11; 16]));
        assert_eq!(crypto.tx_key(0, &[0xff; 6]).map(|key| key.key), Some([0x22; 16]));
        assert!(crypto.tx_key(0, &[0x04; 6]).is_none());
    }

    fn install_group_key(crypto: &mut Crypto, if_id: u8, index: u8, key_id: u8, key: [u8; 16]) {
        let mut payload = [0_u8; 44];
        payload[0] = AES_GROUP;
        payload[1] = index;
        payload[4..20].copy_from_slice(&key);
        payload[20] = key_id;
        crypto.add_key(if_id, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
    }

    #[test]
    fn group_tx_uses_the_selected_key_while_rx_keeps_both() {
        let mut crypto = Crypto::new();
        install_group_key(&mut crypto, 0, 0, 1, [0x11; 16]);
        install_group_key(&mut crypto, 0, 5, 2, [0x22; 16]);
        let mut frame = tx_frame();
        frame[4..10].fill(0xff);
        crypto.encrypt_now(&mut frame, 0).unwrap();
        assert_eq!(frame[29] >> 6, 2);
        assert_eq!(crypto.keys[0].tx_pn, 0);
        assert_eq!(crypto.keys[5].tx_pn, 1);
        for (id, key) in [(1, [0x11; 16]), (2, [0x22; 16])] {
            let mut frame = sealed_rx_frame([0xff; 6], id, &key);
            assert_eq!(decrypt_rx_frame(&mut crypto, &mut frame, 0), Ok(true));
        }
        // Reuse a lower slot: installation order, not table order, selects TX.
        install_group_key(&mut crypto, 0, 0, 1, [0x33; 16]);
        assert_eq!(crypto.tx_key(0, &[0xff; 6]).map(|key| key.key), Some([0x33; 16]));
        crypto.remove_key(5);
        assert_eq!(crypto.tx_key(0, &[0xff; 6]).map(|key| key.key_id), Some(1));
    }

    #[test]
    fn group_tx_selection_keeps_the_slot_even_with_duplicate_key_ids() {
        let mut crypto = Crypto::new();
        install_group_key(&mut crypto, 0, 0, 1, [0x11; 16]);
        install_group_key(&mut crypto, 0, 5, 1, [0x22; 16]);
        assert_eq!(crypto.tx_key(0, &[0xff; 6]).map(|key| key.key), Some([0x22; 16]));
        crypto.remove_key(5);
        assert!(crypto.tx_key(0, &[0xff; 6]).is_none());
    }

    #[test]
    fn slot_order_key_reupload_keeps_the_current_gtk_and_packet_numbers() {
        let mut crypto = Crypto::new();
        install_group_key(&mut crypto, 0, 5, 2, [0x22; 16]);
        install_group_key(&mut crypto, 0, 0, 1, [0x11; 16]);
        crypto.keys[0].tx_pn = 42;
        crypto.keys[5].tx_pn = 17;
        // RESET preserves the table; cw1200_upload_keys walks low to high.
        for (slot, id, key) in [(0, 1, [0x11; 16]), (5, 2, [0x22; 16])] {
            install_group_key(&mut crypto, 0, slot, id, key);
        }
        let mut frame = tx_frame();
        frame[4..10].fill(0xff);
        crypto.encrypt_now(&mut frame, 0).unwrap();
        assert_eq!(frame[29] >> 6, 1);
        assert_eq!(tx_frame_packet_number(&frame), Some(43));
        assert_eq!(crypto.keys[5].tx_pn, 17);
        // Re-uploading an old key must not undo fail-closed selection either.
        crypto.remove_key(0);
        install_group_key(&mut crypto, 0, 5, 2, [0x22; 16]);
        assert!(crypto.tx_key(0, &[0xff; 6]).is_none());
        install_group_key(&mut crypto, 0, 0, 1, [0x33; 16]);
        assert_eq!(crypto.tx_key(0, &[0xff; 6]).map(|key| key.key), Some([0x33; 16]));
    }

    #[test]
    fn group_tx_selection_is_per_interface_and_never_falls_back_after_removal() {
        let mut crypto = Crypto::new();
        install_group_key(&mut crypto, 0, 0, 1, [0x11; 16]);
        install_group_key(&mut crypto, 0, 1, 2, [0x22; 16]);
        install_group_key(&mut crypto, 1, 2, 3, [0x33; 16]);
        assert_eq!(crypto.tx_key(0, &[0xff; 6]).map(|key| key.key_id), Some(2));
        assert_eq!(crypto.tx_key(1, &[0xff; 6]).map(|key| key.key_id), Some(3));
        crypto.remove_key(1);
        assert!(crypto.tx_key(0, &[0xff; 6]).is_none());
        assert_eq!(crypto.rx_key(0, &[0x02; 6], 1, true).map(|key| key.key), Some([0x11; 16]));
        assert_eq!(crypto.tx_key(1, &[0xff; 6]).map(|key| key.key_id), Some(3));
    }

    #[test]
    fn rx_does_not_fall_back_to_the_other_key_class() {
        for (key_type, receiver) in [(AES_GROUP, [0x06; 6]), (AES_PAIRWISE, [0xff; 6])] {
            let mut crypto = Crypto::new();
            crypto.keys[0] = KeyRecord {
                active: true,
                key_type,
                generation: 1,
                peer: [0x02; 6],
                key: [0x11; 16],
                ..KeyRecord::EMPTY
            };
            let mut frame = sealed_rx_frame(receiver, 0, &[0x11; 16]);
            assert_eq!(decrypt_rx_frame(&mut crypto, &mut frame, 0), Err(CcmpError::MissingKey));
        }
    }

    #[test]
    fn build_generated_hardware_kat_matches_independent_vector() {
        assert_eq!(
            &hardware_kat::HARDWARE_KAT_EXPECTED[34..50],
            &[
                0xc7, 0x40, 0x95, 0x9b, 0xf5, 0x9a, 0x96, 0x31, 0xa8, 0xc2, 0xc9, 0xd9, 0x91, 0x0d,
                0x11, 0x4b,
            ]
        );
        assert_eq!(
            &hardware_kat::HARDWARE_KAT_EXPECTED[50..58],
            &[0x11, 0x85, 0x85, 0x5b, 0xce, 0x30, 0xf3, 0xe9]
        );
        assert_eq!(
            hardware_kat::HARDWARE_KAT_MATRIX_LENGTHS,
            [1, 15, 16, 17, 1506]
        );
        assert_eq!(
            hardware_kat::HARDWARE_KAT_MATRIX_MICS.len(),
            5 * CCMP_MIC_LEN
        );
    }

    #[test]
    fn qos_control_can_be_removed_before_ccmp_processing() {
        let mut frame = [0_u8; 40];
        frame[..2].copy_from_slice(&0x4188_u16.to_le_bytes());
        frame[24..26].copy_from_slice(&[5, 0]);
        frame[26..40].copy_from_slice(b"reserved+data!");

        let length = strip_qos_control(&mut frame).unwrap();
        assert_eq!(length, 38);
        assert_eq!(u16::from_le_bytes([frame[0], frame[1]]), 0x4108);
        assert_eq!(&frame[24..38], b"reserved+data!");
    }

    #[test]
    fn ccmp_round_trip_preserves_reserved_header_and_mic_space() {
        let mut crypto = Crypto::new();
        let mut tx_request = [0_u8; 44];
        crypto.add_key(
            0,
            &pairwise_request(&mut tx_request, 0, [0x20, 0x05, 0xb6, 0xff, 0x01, 0x43]),
        )
        .unwrap();
        let mut rx_request = [0_u8; 44];
        crypto.add_key(
            0,
            &pairwise_request(&mut rx_request, 1, [0x12, 0x42, 0x2a, 0x37, 0x70, 0x07]),
        )
        .unwrap();
        let mut frame = [0_u8; 26 + 8 + 12 + 8];
        frame[..2].copy_from_slice(&0x4188_u16.to_le_bytes());
        frame[4..10].copy_from_slice(&[0x20, 0x05, 0xb6, 0xff, 0x01, 0x43]);
        frame[10..16].copy_from_slice(&[0x12, 0x42, 0x2a, 0x37, 0x70, 0x07]);
        frame[16..22].copy_from_slice(&[0x20, 0x05, 0xb6, 0xff, 0x01, 0x43]);
        frame[24] = 0;
        frame[34..46].copy_from_slice(b"hello world!");
        let plaintext: [u8; 12] = frame[34..46].try_into().unwrap();

        crypto.encrypt_now(&mut frame, 0).unwrap();
        assert_ne!(&frame[34..46], &plaintext);
        assert!(decrypt_rx_frame(&mut crypto, &mut frame, 0).unwrap());
        assert_eq!(&frame[34..46], &plaintext);
    }

    const AP: [u8; 6] = [0x20, 0x05, 0xb6, 0xff, 0x01, 0x43];
    const STATION: [u8; 6] = [0x12, 0x42, 0x2a, 0x37, 0x70, 0x07];

    /// Pairwise keys for frames to `AP` (TX) and from it (RX).
    fn crypto_with_keys() -> Crypto {
        let mut crypto = Crypto::new();
        for (index, peer) in [(0, AP), (1, STATION)] {
            let mut request = [0_u8; 44];
            crypto.add_key(0, &pairwise_request(&mut request, index, peer)).unwrap();
        }
        crypto
    }

    /// A protected QoS data frame from the station to the AP.
    fn tx_frame() -> [u8; 54] {
        let mut frame = [0_u8; 54];
        frame[..2].copy_from_slice(&0x4188_u16.to_le_bytes());
        frame[4..10].copy_from_slice(&AP);
        frame[10..16].copy_from_slice(&STATION);
        frame[16..22].copy_from_slice(&AP);
        frame[34..46].copy_from_slice(b"hello world!");
        frame
    }

    /// The same frame sealed, as the AP would send it to us.
    fn rx_frame() -> [u8; 54] {
        let mut sender = Crypto::new();
        let mut request = [0_u8; 44];
        sender.add_key(0, &pairwise_request(&mut request, 0, STATION)).unwrap();
        let mut frame = tx_frame();
        frame[4..10].copy_from_slice(&STATION);
        frame[10..16].copy_from_slice(&AP);
        sender.encrypt_now(&mut frame, 0).unwrap();
        frame
    }

    /// A protected QoS data frame from `AP` with `pn` on `tid`; Key ID 0
    /// and unicast unless `group`.
    fn received(tid: u8, pn: u64, group: Option<u8>) -> [u8; 42] {
        let mut frame = [0_u8; 42];
        frame[..2].copy_from_slice(&0x4288_u16.to_le_bytes());
        frame[4..10].copy_from_slice(&if group.is_some() { [0xff; 6] } else { STATION });
        frame[10..16].copy_from_slice(&AP);
        frame[24] = tid;
        let pn = pn.to_le_bytes();
        frame[26..34].copy_from_slice(&[pn[0], pn[1], 0, 0x20 | group.unwrap_or(0) << 6, pn[2], pn[3], pn[4], pn[5]]);
        frame
    }

    #[test]
    fn rx_packet_numbers_must_grow_per_key_and_tid() {
        let mut crypto = crypto_with_keys();
        let mut accept = |tid, pn| accept_counter(&mut crypto, &received(tid, pn, None));
        assert!(accept(0, 1));
        assert!(!accept(0, 1), "a replay");
        assert!(accept(0, 0x0102_0304_0506));
        assert!(!accept(0, 0x0102_0304_0505), "an older packet number");
        assert!(accept(5, 2), "TIDs count separately");
        assert!(!accept(5, 2));
        // An unknown transmitter has no key.
        let mut frame = received(0, 0x0200_0000_0000, None);
        frame[15] ^= 1;
        assert!(!accept_counter(&mut crypto, &frame));
    }

    #[test]
    fn group_rx_starts_after_the_handshake_counter() {
        let mut crypto = Crypto::new();
        let mut payload = [0_u8; 44];
        payload[0] = AES_GROUP;
        payload[4..20].copy_from_slice(&[0x33; 16]);
        payload[20] = 1;
        payload[24..30].copy_from_slice(&[0x10, 0, 0, 0, 0, 0]);
        let request = crate::wsm::AddKeyRequest::parse(&payload).unwrap();
        crypto.add_key(0, &request).unwrap();
        assert!(!accept_counter(&mut crypto, &received(0, 0x10, Some(1))));
        assert!(accept_counter(&mut crypto, &received(0, 0x11, Some(1))));
        // Re-uploading never lowers a counter, and a higher announced RSC
        // advances every TID even when the key bytes stay unchanged.
        crypto.add_key(0, &request).unwrap();
        assert!(!accept_counter(&mut crypto, &received(0, 0x11, Some(1))));
        payload[24] = 0x20;
        crypto.add_key(0, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
        assert!(!accept_counter(&mut crypto, &received(0, 0x20, Some(1))));
        assert!(accept_counter(&mut crypto, &received(7, 0x21, Some(1))));
        // A different key begins at its own announced RSC.
        payload[24] = 0x10;
        payload[4] ^= 1;
        crypto.add_key(0, &crate::wsm::AddKeyRequest::parse(&payload).unwrap()).unwrap();
        assert!(accept_counter(&mut crypto, &received(0, 0x11, Some(1))));
    }

    fn running(started: Result<Started, CcmpError>) -> Job {
        match started {
            Ok(Started::Running(job)) => job,
            _ => panic!("transfer did not start"),
        }
    }

    fn busy(started: Result<Started, CcmpError>) -> bool {
        matches!(started, Ok(Started::Busy))
    }

    fn finish(crypto: &mut Crypto, job: Job) -> Result<(), CcmpError> {
        crypto.engine.held = false;
        crypto.poll(job).ok().expect("finished")
    }

    #[test]
    fn rx_and_tx_take_turns_when_both_wait() {
        let mut crypto = crypto_with_keys();
        let (mut tx, mut rx) = (tx_frame(), rx_frame());
        crypto.engine.held = true;
        let job = running(start_tx(&mut crypto, &mut tx, 0));
        assert!(busy(start_rx(&mut crypto, &mut rx, 0)));
        finish(&mut crypto, job).unwrap();
        // RX waited: the next TX transfer lets it go first.
        let mut next_tx = tx_frame();
        assert!(busy(start_tx(&mut crypto, &mut next_tx, 0)));
        crypto.engine.held = true;
        let job = running(start_rx(&mut crypto, &mut rx, 0));
        assert!(busy(start_tx(&mut crypto, &mut next_tx, 0)));
        finish(&mut crypto, job).unwrap();
        assert_eq!(&rx[34..46], b"hello world!");
        let mut next_rx = rx_frame();
        assert!(busy(start_rx(&mut crypto, &mut next_rx, 0)));
        let job = running(start_tx(&mut crypto, &mut next_tx, 0));
        finish(&mut crypto, job).unwrap();
    }

    #[test]
    fn a_waiting_direction_keeps_its_turn_across_repeated_tries() {
        let mut crypto = crypto_with_keys();
        let (mut tx, mut rx) = (tx_frame(), rx_frame());
        crypto.engine.held = true;
        let job = running(start_tx(&mut crypto, &mut tx, 0));
        assert!(busy(start_rx(&mut crypto, &mut rx, 0)));
        finish(&mut crypto, job).unwrap();
        // TX sealing runs twice per pass, RX once: both TX tries yield.
        let mut next = tx_frame();
        assert!(busy(start_tx(&mut crypto, &mut next, 0)));
        assert!(busy(start_tx(&mut crypto, &mut next, 0)));
        let job = running(start_rx(&mut crypto, &mut rx, 0));
        finish(&mut crypto, job).unwrap();
        let job = running(start_tx(&mut crypto, &mut next, 0));
        finish(&mut crypto, job).unwrap();
    }

    #[test]
    fn a_turn_lapses_when_the_waiting_direction_stops_asking() {
        let mut crypto = crypto_with_keys();
        let (mut tx, mut rx) = (tx_frame(), rx_frame());
        crypto.engine.held = true;
        let job = running(start_tx(&mut crypto, &mut tx, 0));
        assert!(busy(start_rx(&mut crypto, &mut rx, 0)));
        finish(&mut crypto, job).unwrap();
        // RX never comes back for its turn.
        let mut next = tx_frame();
        for _ in 0..RESERVATION_REFUSALS {
            assert!(busy(start_tx(&mut crypto, &mut next, 0)));
        }
        let job = running(start_tx(&mut crypto, &mut next, 0));
        finish(&mut crypto, job).unwrap();
    }

    #[test]
    fn packet_numbers_advance_only_when_a_transfer_starts() {
        let mut crypto = crypto_with_keys();
        let mut rx = rx_frame();
        crypto.engine.held = true;
        let job = running(start_rx(&mut crypto, &mut rx, 0));
        let mut tx = tx_frame();
        assert!(busy(start_tx(&mut crypto, &mut tx, 0)));
        finish(&mut crypto, job).unwrap();
        let job = running(start_tx(&mut crypto, &mut tx, 0));
        finish(&mut crypto, job).unwrap();
        assert_eq!(tx_frame_packet_number(&tx), Some(1));
    }

    #[test]
    fn an_exhausted_packet_number_never_wraps_or_starts_the_engine() {
        for group in [false, true] {
            let mut crypto = crypto_with_keys();
            if group {
                install_group_key(&mut crypto, 0, 2, 1, [0x11; 16]);
            }
            let mut frame = tx_frame();
            if group {
                frame[4..10].fill(0xff);
            }
            crypto.tx_key(0, &frame[4..10]).unwrap().tx_pn = 0x0000_ffff_ffff_fffe;
            crypto.encrypt_now(&mut frame, 0).unwrap();
            assert_eq!(tx_frame_packet_number(&frame), Some(0x0000_ffff_ffff_ffff));
            let mut next = tx_frame();
            if group {
                next[4..10].fill(0xff);
            }
            let untouched = next;
            let refused = match start_tx(&mut crypto, &mut next, 0) {
                Err(CcmpError::PacketNumberExhausted) => true,
                Err(other) => panic!("unexpected error: {other:?}"),
                Ok(Started::Running(job)) => { finish(&mut crypto, job).unwrap(); false }
                Ok(_) => false,
            };
            assert!(refused, "exhaustion started another transfer at {:?}", tx_frame_packet_number(&next));
            assert_eq!(next, untouched);
            assert!(crypto.settled());
            assert_eq!(crypto.tx_key(0, &next[4..10]).unwrap().tx_pn, 0x0000_ffff_ffff_ffff);
        }
    }

    #[test]
    fn an_outcome_waits_for_its_owner() {
        let mut crypto = crypto_with_keys();
        let (mut tx, mut rx) = (tx_frame(), rx_frame());
        let tx_job = running(start_tx(&mut crypto, &mut tx, 0));
        // TX finished but is not collected yet; RX may use the engine.
        let rx_job = running(start_rx(&mut crypto, &mut rx, 0));
        assert_eq!(crypto.poll(rx_job).ok(), Some(Ok(())));
        assert_eq!(crypto.poll(tx_job).ok(), Some(Ok(())));
    }

    #[test]
    fn a_removed_tx_key_never_falls_back_to_plaintext() {
        let mut crypto = crypto_with_keys();
        crypto.remove_key(0);
        let mut frame = tx_frame();
        let untouched = frame;
        match start_tx(&mut crypto, &mut frame, 0) {
            Err(CcmpError::MissingKey) => {}
            Ok(Started::Running(job)) => { finish(&mut crypto, job).unwrap(); panic!("used a removed key"); }
            _ => panic!("protected data was not rejected"),
        }
        assert_eq!(frame, untouched);
        assert!(crypto.settled());
    }

    #[test]
    fn unprotected_frames_need_no_engine() {
        let mut crypto = crypto_with_keys();
        let mut frame = tx_frame();
        frame[1] &= !0x40;
        assert!(matches!(start_tx(&mut crypto, &mut frame, 0), Ok(Started::Plain)));
        assert!(matches!(start_rx(&mut crypto, &mut frame, 0), Ok(Started::Plain)));
    }
}
