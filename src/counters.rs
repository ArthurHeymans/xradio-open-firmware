//! The WSM counters table (MIB 0x100c), cw1200 `wsm_mib_counters_table`:
//! 22 little-endian words, shown by the driver's debugfs `counters`. The
//! firmware fills what it observes; the rest stay zero.

use crate::global::Global;
use core::cell::Cell;

#[derive(Clone, Copy)]
#[repr(usize)]
pub enum Counter {
    TxPackets = 2,
    RxPackets = 3,
    RxPacketErrors = 4,
    RxDecryptionFailures = 5,
    RxMicFailures = 6,
    RxNoKeyFailures = 7,
    TxFramesSuccess = 9,
    TxFrameFailures = 10,
    TxFramesRetried = 11,
    TxFramesMultiRetried = 12,
    /// Protected frames rejected by the final freshness check: replays,
    /// or authenticated frames whose key installation was revoked.
    RxFrameDuplicates = 13,
    /// An RTS answered by a CTS, and an RTS attempt left unanswered.
    RtsSuccess = 14,
    RtsFailures = 15,
    AckFailures = 16,
    RxMulticastFrames = 17,
    RxFramesSuccess = 18,
}

pub const WORDS: usize = 22;

static TABLE: Global<[Cell<u32>; WORDS]> = Global::new([const { Cell::new(0) }; WORDS]);

pub fn add(counter: Counter, amount: u32) {
    let cell = &TABLE[counter as usize];
    cell.set(cell.get().wrapping_add(amount));
}

pub fn bump(counter: Counter) {
    add(counter, 1);
}

/// A host frame's transmission ended with `status` after `ack_failures`
/// unacknowledged attempts.
pub fn tx_completed(status: u32, ack_failures: u8) {
    bump(Counter::TxPackets);
    bump(if status == 0 { Counter::TxFramesSuccess } else { Counter::TxFrameFailures });
    if ack_failures > 0 {
        bump(Counter::TxFramesRetried);
    }
    if ack_failures > 1 {
        bump(Counter::TxFramesMultiRetried);
    }
    add(Counter::AckFailures, u32::from(ack_failures));
}

pub fn snapshot() -> [u32; WORDS] {
    core::array::from_fn(|index| TABLE[index].get())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transmissions_land_in_the_cw1200_fields() {
        let before = snapshot();
        tx_completed(0, 0);
        tx_completed(0, 2);
        tx_completed(2, 4);
        let after = snapshot();
        let delta = |index: usize| after[index] - before[index];
        assert_eq!([delta(2), delta(9), delta(10), delta(11), delta(12), delta(16)], [3, 2, 1, 2, 2, 6]);
    }
}
