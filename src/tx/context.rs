//! TX frame contexts.
//!
//! A [`TxContext`] describes one frame from preparation until its completion
//! is handed back: where the frame sits in packet RAM, how it is sent (rate,
//! access category, flags), its airtime and its outcome. Contexts live in one
//! fixed slab: 30 for host data frames (class 0), whose frames stay in the
//! host's request buffers, and 3 for frames the firmware sends itself
//! (class 6: probes and host management frames), each with its own packet-RAM
//! buffer. A [`ContextId`] names a slab entry.

use crate::global::Global;
use core::cell::Cell;

use super::*;

pub(crate) const HOST_CONTEXT_COUNT: usize = 30;
pub(crate) const INTERNAL_CONTEXT_COUNT: usize = 3;
const CONTEXT_COUNT: usize = HOST_CONTEXT_COUNT + INTERNAL_CONTEXT_COUNT;

pub(crate) const TX_BUFFER_SIZE: usize = packet_ram::INTERNAL_TX_BUFFER_SIZE;

/// Completion class of host data frames: the host confirmation returns them.
pub(crate) const CLASS_HOST: u8 = 0;
/// Completion class of firmware frames: the completion returns them.
pub(crate) const CLASS_INTERNAL: u8 = 6;

/// Terminal status of a context that is not in use.
pub(crate) const STATUS_FREE: u16 = 0x00ff;
/// Terminal status of a context that is being prepared or sent.
pub(crate) const STATUS_PENDING: u16 = 0x00fe;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ContextId(u8);

impl ContextId {
    pub(crate) const fn host(index: usize) -> Option<Self> {
        if index < HOST_CONTEXT_COUNT {
            Some(Self(index as u8))
        } else {
            None
        }
    }

    pub(crate) const fn internal(index: usize) -> Option<Self> {
        if index < INTERNAL_CONTEXT_COUNT {
            Some(Self((HOST_CONTEXT_COUNT + index) as u8))
        } else {
            None
        }
    }

    /// Host context index, which also selects its descriptor staging area.
    pub(crate) const fn host_index(self) -> Option<usize> {
        if (self.0 as usize) < HOST_CONTEXT_COUNT {
            Some(self.0 as usize)
        } else {
            None
        }
    }

    /// Internal context index, which also selects its packet-RAM buffer.
    pub(crate) const fn internal_index(self) -> Option<usize> {
        if (self.0 as usize) >= HOST_CONTEXT_COUNT {
            Some(self.0 as usize - HOST_CONTEXT_COUNT)
        } else {
            None
        }
    }

    pub(crate) const fn is_host(self) -> bool {
        self.host_index().is_some()
    }

    /// Host-context index; the driver keys its per-frame state by it.
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub(crate) fn get(self) -> &'static TxContext {
        &contexts().all[usize::from(self.0)]
    }
}

pub(crate) struct TxContext {
    // The frame.
    /// Packet-RAM address of the 802.11 header.
    pub(crate) frame: Cell<u32>,
    /// MPDU length without FCS.
    pub(crate) frame_length: Cell<u16>,
    pub(crate) frame_control: Cell<u16>,

    // How it is sent.
    pub(crate) class: Cell<u8>,
    pub(crate) interface: Cell<u8>,
    /// Host link from the WSM header: an access point's station, else 0.
    pub(crate) host_link: Cell<u8>,
    /// A QoS data frame's BlockAck stream and sequence number, from its
    /// classification.
    pub(crate) block_ack: Cell<Option<(u8, u16)>>,
    pub(crate) access_category: Cell<u8>,
    pub(crate) tx_rate: Cell<u8>,
    /// WSM retry-policy index.
    pub(crate) retry_policy: Cell<u8>,
    /// WSM TX flags bits 1..3, passed to the PHY rate words.
    pub(crate) request_flag_rate_bits: Cell<u8>,
    /// Vendor frame flags (`tx_classify_hdr_len` classification, retry
    /// state, batch position).
    pub(crate) flags: Cell<u32>,
    /// Admission time, for PAS expiry.
    pub(crate) submit_timer: Cell<u32>,

    // Airtime, from `prepare_single_frame_pas_timing`.
    pub(crate) ack_duration: Cell<u16>,
    pub(crate) payload_extended: Cell<u16>,
    pub(crate) payload_base: Cell<u16>,
    pub(crate) total_airtime: Cell<u32>,
    pub(crate) protection: Cell<Option<ProtectionTiming>>,
    pub(crate) erp_protection: Cell<bool>,
    /// Response class the MAC reports (`0xff`: none expected).
    pub(crate) frame_kind: Cell<u8>,
    /// Duration word the MAC takes the sequence number from.
    pub(crate) duration_slot: Cell<u8>,

    // Outcome.
    pub(crate) try_count: Cell<u16>,
    /// Per-rate attempt nibbles reported in the host confirmation.
    pub(crate) rate_try: [Cell<u32>; 3],
    /// `STATUS_FREE`, `STATUS_PENDING` or the vendor completion status.
    pub(crate) terminal_status: Cell<u16>,
    pub(crate) auxiliary_state: Cell<u16>,
    /// Confirmation flags reported to the host.
    pub(crate) completion_flags: Cell<u16>,
    /// Low byte of `try_count` at completion.
    pub(crate) saved_status: Cell<u8>,
    /// `pipe * 4 + slot` of the aggregate the frame is a member of, else
    /// `NO_AGGREGATE`.
    pub(crate) aggregate_slot: Cell<u8>,
}

pub(crate) const NO_AGGREGATE: u8 = 0xff;

impl TxContext {
    const fn new() -> Self {
        Self {
            frame: Cell::new(0),
            frame_length: Cell::new(0),
            frame_control: Cell::new(0),
            class: Cell::new(0),
            interface: Cell::new(0),
            host_link: Cell::new(0),
            block_ack: Cell::new(None),
            access_category: Cell::new(0),
            tx_rate: Cell::new(0),
            retry_policy: Cell::new(0),
            request_flag_rate_bits: Cell::new(0),
            flags: Cell::new(0),
            submit_timer: Cell::new(0),
            ack_duration: Cell::new(0),
            payload_extended: Cell::new(0),
            payload_base: Cell::new(0),
            total_airtime: Cell::new(0),
            protection: Cell::new(None),
            erp_protection: Cell::new(false),
            frame_kind: Cell::new(0),
            duration_slot: Cell::new(0),
            try_count: Cell::new(0),
            rate_try: [const { Cell::new(0) }; 3],
            terminal_status: Cell::new(STATUS_FREE),
            auxiliary_state: Cell::new(0),
            completion_flags: Cell::new(0),
            saved_status: Cell::new(0),
            aggregate_slot: Cell::new(NO_AGGREGATE),
        }
    }

    pub(crate) fn is_free(&self) -> bool {
        self.terminal_status.get() == STATUS_FREE
    }
}

pub(crate) struct Contexts {
    all: [TxContext; CONTEXT_COUNT],
    /// Contexts queued for or owned by the MAC.
    pub(crate) active_pas: Cell<u16>,
    /// The last internal context lends its packet-RAM buffer to the access
    /// point's beacon: it is then neither allocated nor counted.
    beacon_reserved: Cell<bool>,
}

static CONTEXTS: Global<Contexts> = Global::new(Contexts {
    all: [const { TxContext::new() }; CONTEXT_COUNT],
    active_pas: Cell::new(0),
    beacon_reserved: Cell::new(false),
});

/// Room for a frame in an internal context's buffer.
pub(crate) const INTERNAL_FRAME_CAPACITY: usize = TX_BUFFER_SIZE - 0x40;

pub(crate) fn contexts() -> &'static Contexts {
    &CONTEXTS
}

impl Contexts {
    fn allocate(&self, ids: impl Iterator<Item = ContextId>) -> Option<ContextId> {
        let id = ids.into_iter().find(|id| id.get().is_free())?;
        id.get().terminal_status.set(STATUS_PENDING);
        Some(id)
    }

    /// Takes a free host context.
    pub(crate) fn allocate_host(&self) -> Option<ContextId> {
        self.allocate((0..HOST_CONTEXT_COUNT).filter_map(ContextId::host))
    }

    fn internal_ids(&self) -> impl Iterator<Item = ContextId> {
        (0..INTERNAL_CONTEXT_COUNT - usize::from(self.beacon_reserved.get())).filter_map(ContextId::internal)
    }

    /// The beacon's packet-RAM frame buffer, reserved until
    /// [`Self::release_beacon_buffer`]; `None` while its context is in use.
    pub(crate) fn reserve_beacon_buffer(&self) -> Option<u32> {
        let id = ContextId::internal(INTERNAL_CONTEXT_COUNT - 1)?;
        if !self.beacon_reserved.get() && !id.get().is_free() {
            return None;
        }
        self.beacon_reserved.set(true);
        Some(internal_frame_address(id))
    }

    pub(crate) fn release_beacon_buffer(&self) {
        self.beacon_reserved.set(false);
    }

    /// Takes a free internal context, pointed at its packet-RAM buffer.
    pub(crate) fn allocate_internal(&self) -> Option<ContextId> {
        let id = self.allocate(self.internal_ids())?;
        id.get().frame.set(internal_frame_address(id));
        Some(id)
    }

    /// Returns a context to its pool.
    pub(crate) fn free(&self, id: ContextId) {
        id.get().terminal_status.set(STATUS_FREE);
    }

    pub(crate) fn internal_in_use(&self) -> usize {
        self.internal_ids().filter(|id| !id.get().is_free()).count()
    }

}

/// Where an internal context's frame starts in its packet-RAM buffer.
pub(crate) fn internal_frame_address(id: ContextId) -> u32 {
    let index = id.internal_index().unwrap_or(0);
    (packet_ram::internal_tx_buffer(index) + 0x40) as u32
}

/// Packet-RAM staging area of a host context's TX descriptor.
pub(crate) fn host_descriptor_address(id: ContextId) -> Option<u32> {
    id.host_index()
        .map(|index| packet_ram::host_frame_state(index) as u32)
}
