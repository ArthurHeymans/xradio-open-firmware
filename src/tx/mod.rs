//! TX engine: frame contexts, descriptors, MAC pipes, events, retries and
//! completions.
//!
//! - `context`: internal and host frame contexts and their pools.
//! - `descriptor`: PHY rate words, airtime and the packet-RAM TX descriptor.
//! - `protection`: RTS/CTS-to-self prefixes and protected retry ring protocol.
//! - `pipe`: the four MAC pipes, publication and the pipe watchdog.
//! - `event`: MAC event FIFO dispatch, TX start/success/status, PHY commands.
//! - `retry`: retry decisions and descriptor re-arming.
//! - `completion`: the completion ring, context completion and the backend.
//! - `probe`: firmware-originated frames (probes) and host management frames.
//! - `host`: admission and scheduling of host data frames.
//! - `driver`: the host data TX driver (`HostTxDriver`) and confirmations.
//! - `batch`: hardware-free batch and ownership planning for the driver.
//! - `hw`: raw register and packet-RAM access.

use core::cell::UnsafeCell;

use crate::configuration::MAX_TEMPLATE_FRAME_LEN;
use crate::packet_ram;

mod ack;
#[cfg(target_arch = "arm")]
pub mod aggregate;
pub mod ampdu;
pub mod ba;
mod batch;
mod completion;
mod context;
mod descriptor;
#[cfg(target_arch = "arm")]
mod driver;
mod event;
mod host;
mod hw;
mod pipe;
mod probe;
mod protection;
mod retry;
#[cfg(test)]
mod tests;

#[cfg(target_arch = "arm")]
pub(crate) use aggregate::{complete_aggregate_slot, is_aggregate_slot};
pub use batch::*;
pub use completion::*;
pub use context::*;
pub use descriptor::*;
#[cfg(target_arch = "arm")]
pub use driver::*;
pub use event::*;
pub use host::*;
pub use hw::*;
pub use pipe::*;
pub use probe::*;
pub use protection::*;
pub use retry::*;

pub(crate) const PIPE_IRQ_PENDING: u32 = crate::platform::mac_register(0x0e84) as u32;

pub(crate) const PIPE_IRQ_TRIGGER: u32 = crate::platform::mac_register(0x0e98) as u32;

pub(crate) const PIPE_QUANTUM: u32 = 0x0000_0fff;

pub(crate) const PIPE_RETRY_INACTIVE_SENTINEL: u32 = 0xff00_ffff;

pub(crate) const MAC_EVENT_READINESS: u32 = crate::platform::mac_register(0x0a24) as u32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProbeBuildError {
    MissingTemplate,
    WrongTemplateType,
    MalformedTemplate,
    SsidTooLong,
    FrameTooLarge,
    ContextPoolEmpty,
    InvalidInterface,
    InvalidContextPointer,
    PacketRamMismatch,
    PipeStateUnavailable,
    PipeSlotBusy,
    PipeSlotOwnershipMismatch,
    UnsupportedPublicationShape,
    CryptoFailure,
}

/// The TX engine's runtime: it consumes the MAC event FIFO, tracks the frames
/// it published and their completions, and owns the scratch frame used to
/// prepare probes and host management frames. The firmware creates one and
/// passes it to whichever subsystem currently services the MAC.
pub struct TxEngine {
    #[cfg(target_arch = "arm")]
    pub(crate) probe: ActiveProbeRuntime,
    pub(crate) scratch: PreparedProbe,
    pub ba: ba::PeerSessions,
    /// The peer's minimum MPDU start spacing (association MIB), 0..=7.
    pub mpdu_start_spacing: u8,
    /// TIDs the host allows TX BlockAck sessions for (MIB 0x100e).
    pub block_ack_tx_tids: u8,
    /// Per-interface host RTS threshold and ERP protection policy.
    pub protection: [ProtectionPolicy; 2],
}

impl Default for TxEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TxEngine {
    /// Changes whenever the MAC reports an event or completes a frame.
    pub fn activity(&self, activity: crate::activity::Activity) -> crate::activity::Activity {
        #[cfg(target_arch = "arm")]
        let activity = activity.with(self.probe.backend.serviced);
        activity
    }

    /// The host's rate policies, which the MAC backend's retries follow.
    #[cfg(target_arch = "arm")]
    pub fn rate_policies(&mut self) -> &mut crate::rate_policy::RatePolicies {
        &mut self.probe.backend.rate_policies
    }

    pub const fn new() -> Self {
        Self {
            #[cfg(target_arch = "arm")]
            probe: ActiveProbeRuntime::new(),
            scratch: PreparedProbe::new(),
            ba: ba::PeerSessions::new(),
            mpdu_start_spacing: 0,
            block_ack_tx_tids: 0,
            protection: [ProtectionPolicy::DISABLED; 2],
        }
    }
}

/// Exact `wsm_status_from_internal` switch table at vendor `0x0000aff8`.
pub const fn wsm_status_from_internal(status: u16) -> u32 {
    match status {
        0 => 0,
        1 => 1,
        2 => 2,
        8 => 13,
        10 => 7,
        11 => 6,
        12 => 8,
        16 => 9,
        17 => 10,
        19 => 5,
        21 => 4,
        22 => 11,
        23 => 14,
        24 => 15,
        _ => 1,
    }
}
