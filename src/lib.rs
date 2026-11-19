#![no_std]
#![cfg(feature = "xr819")]
// Host builds compile the hardware-free half for tests; items only the ARM
// firmware calls are dead there. The ARM build still reports real dead code.
#![cfg_attr(not(target_arch = "arm"), allow(dead_code))]

#[cfg(test)]
extern crate std;

mod init_data;
pub mod activity;
pub mod ap;
pub(crate) mod backoff;
pub mod cpu;
pub mod beacon_filter;
pub mod beacon_loss;
pub mod signal;
pub mod counters;
pub mod deadline;
#[cfg(target_arch = "arm")]
pub mod command;
pub mod configuration;
pub mod crypto;
pub mod download;
pub mod exception;
#[cfg(target_arch = "arm")]
pub mod firmware;
pub mod frame;
pub mod global;
pub mod hif;
pub mod interrupts;
/// Halts the firmware.
///
/// Use where continuing past the detected condition would corrupt state the
/// firmware cannot repair. Two measured examples: returning instead of halting
/// in `radio::release()` when a slot falls outside the FIFO stalls the release
/// head permanently and left 3 of 4 runs unable to associate; skipping the halt
/// in `publish_host_class0_slot` let execution proceed with an unusable pipe and
/// stopped frame acceptance outright (`pending` 20, `used` 0, `TXed` 4).
///
/// The host gets an exception indication naming the file and line, so a halt
/// is never silent.
#[macro_export]
macro_rules! halt_always {
    () => {
        $crate::exception::halt_at(file!(), line!())
    };
}

pub mod join;
pub(crate) mod key_change;
pub mod loader;
pub mod mmio;
pub mod mac;
pub mod packet_ram;
pub mod phy;
pub mod platform;
pub mod power_save;
pub mod radio;
pub mod rate_policy;
pub mod rx_ba;
pub mod rx_reorder;
pub(crate) mod rx_model;
pub mod scan;
pub mod tx;
pub mod vif;
pub mod wsm;
