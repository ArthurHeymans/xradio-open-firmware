//! Shared PAS contention-window (backoff) bank.
//!
//! One production implementation of the vendor bank arithmetic, used by every
//! mutation site: the physical-retry growth at the retry entry (`0x967e` ->
//! `0xff62`), the two vendor-mapped reset hooks (status `0x9a8c`, TX-success
//! `0x9d06..0x9d2c`), the head policy-exhaustion reset (`0x8a20..0x8a26`) and
//! the VIF JOIN reset-all.
//!
//! Each PAS record (see [`crate::mac::Pas`]) holds one bank per access
//! category: retry counter, current window, CWmin and CWmax.
//!
//! Growth: when the old counter is even the window becomes `2 * W + 1` with u32
//! wrapping; the clamp against the maximum applies on **both** parities and the
//! counter always advances by one (wrapping). Reset: counter `0`, window
//! `CWmin`.
//!
//! The vendor also has an override mode with its own reset window and clamp;
//! nothing enables it, so it is not reproduced.

use crate::mac::Pas;

/// Rejected bank selection. PAS has three banks (interface 2 is a legal
/// internal bank) and four access categories per bank. A rejected selection
/// performs no write and never halts or spins: the callers are all past the
/// destructive MAC event read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BackoffBankError {
    Interface,
    AccessCategory,
}

fn bank(banks: &[Pas], interface: u8, queue: u8) -> Result<(&Pas, usize), BackoffBankError> {
    let pas = banks
        .get(usize::from(interface))
        .ok_or(BackoffBankError::Interface)?;
    if queue >= 4 {
        return Err(BackoffBankError::AccessCategory);
    }
    Ok((pas, usize::from(queue)))
}

/// Vendor `0xff62`: one growth per qualified physical transmission retry.
pub(crate) fn grow(banks: &[Pas], interface: u8, queue: u8) -> Result<(), BackoffBankError> {
    let (pas, queue) = bank(banks, interface, queue)?;
    let maximum = u32::from(pas.cw_max[queue].get());
    let count = pas.retry_count[queue].get();
    let window = pas.contention_window[queue].get();
    let grown = if count & 1 == 0 {
        window.wrapping_mul(2).wrapping_add(1)
    } else {
        window
    };
    pas.contention_window[queue].set(grown.min(maximum));
    pas.retry_count[queue].set(count.wrapping_add(1));
    Ok(())
}

/// Vendor reset: counter cleared, window returned to CWmin. Used by the
/// status hook, the TX-success marker and head policy exhaustion.
pub(crate) fn reset(banks: &[Pas], interface: u8, queue: u8) -> Result<(), BackoffBankError> {
    let (pas, queue) = bank(banks, interface, queue)?;
    pas.retry_count[queue].set(0);
    pas.contention_window[queue].set(u32::from(pas.cw_min[queue].get()));
    Ok(())
}

/// VIF reset-all: every access category of one interface, as performed by JOIN
/// and by the channel-0 EDCA AIFS change.
pub(crate) fn reset_all_queues(banks: &[Pas], interface: u8) -> Result<(), BackoffBankError> {
    bank(banks, interface, 0)?;
    (0..4).try_for_each(|queue| reset(banks, interface, queue))
}

/// The firmware's PAS banks.
pub(crate) fn banks() -> &'static [Pas] {
    &crate::mac::low_mac().pas
}

#[cfg(test)]
mod tests {
    use super::*;

    fn banks() -> [Pas; 3] {
        [const { Pas::new() }; 3]
    }

    fn seed(banks: &[Pas], interface: u8, queue: u8, window: u32, count: u32) {
        let pas = &banks[usize::from(interface)];
        let queue = usize::from(queue);
        pas.cw_min[queue].set(15);
        pas.cw_max[queue].set(1023);
        pas.contention_window[queue].set(window);
        pas.retry_count[queue].set(count);
    }

    fn state(banks: &[Pas], interface: u8, queue: u8) -> (u32, u32) {
        let pas = &banks[usize::from(interface)];
        let queue = usize::from(queue);
        (pas.retry_count[queue].get(), pas.contention_window[queue].get())
    }

    #[test]
    fn growth_doubles_on_even_counters_and_always_advances() {
        let banks = banks();
        seed(&banks, 0, 1, 15, 0);
        grow(&banks, 0, 1).unwrap();
        assert_eq!(state(&banks, 0, 1), (1, 31));
        grow(&banks, 0, 1).unwrap();
        assert_eq!(state(&banks, 0, 1), (2, 31));
        grow(&banks, 0, 1).unwrap();
        assert_eq!(state(&banks, 0, 1), (3, 63));
    }

    #[test]
    fn growth_clamps_on_both_parities_and_wraps_the_counter() {
        let banks = banks();
        seed(&banks, 0, 0, 1023, 0);
        grow(&banks, 0, 0).unwrap();
        assert_eq!(state(&banks, 0, 0), (1, 1023));

        // An odd counter still clamps a window left above the maximum.
        seed(&banks, 1, 0, 63, 3);
        banks[1].cw_max[0].set(31);
        grow(&banks, 1, 0).unwrap();
        assert_eq!(state(&banks, 1, 0), (4, 31));

        seed(&banks, 0, 3, 15, u32::MAX);
        grow(&banks, 0, 3).unwrap();
        assert_eq!(state(&banks, 0, 3), (0, 15));
    }

    #[test]
    fn reset_restores_the_configured_minimum() {
        let banks = banks();
        seed(&banks, 2, 3, 511, 7);
        reset(&banks, 2, 3).unwrap();
        assert_eq!(state(&banks, 2, 3), (0, 15));
    }

    #[test]
    fn every_pas_bank_is_addressable_and_isolated() {
        let banks = banks();
        (0..3).for_each(|interface| seed(&banks, interface, 0, 15, 0));
        grow(&banks, 1, 0).unwrap();
        assert_eq!(state(&banks, 0, 0), (0, 15));
        assert_eq!(state(&banks, 1, 0), (1, 31));
        assert_eq!(state(&banks, 2, 0), (0, 15));
    }

    #[test]
    fn out_of_range_selections_are_rejected_without_writes() {
        let banks = banks();
        seed(&banks, 0, 0, 511, 5);
        assert_eq!(grow(&banks, 3, 0), Err(BackoffBankError::Interface));
        assert_eq!(grow(&banks, 0, 4), Err(BackoffBankError::AccessCategory));
        assert_eq!(reset(&banks, 3, 0), Err(BackoffBankError::Interface));
        assert_eq!(reset(&banks, 0, 4), Err(BackoffBankError::AccessCategory));
        assert_eq!(reset_all_queues(&banks, 3), Err(BackoffBankError::Interface));
        assert_eq!(state(&banks, 0, 0), (5, 511));
    }

    #[test]
    fn parsed_edca_minima_reach_the_production_reset_path() {
        let mut payload = [0_u8; 44];
        // Wire order is BK, BE, VI, VO; the parser reverses it into host order.
        for (wire, cwmin) in [15_u16, 15, 7, 3].into_iter().enumerate() {
            payload[wire * 2..wire * 2 + 2].copy_from_slice(&cwmin.to_le_bytes());
            payload[8 + wire * 2..8 + wire * 2 + 2].copy_from_slice(&1023_u16.to_le_bytes());
            payload[16 + wire] = 3;
        }
        let parameters = crate::wsm::EdcaParameters::parse(&payload).unwrap();

        // `apply_edca` writes the host table in wire order [3, 2, 1, 0].
        let wire = [
            parameters.queues[3],
            parameters.queues[2],
            parameters.queues[1],
            parameters.queues[0],
        ];
        let banks = banks();
        for (queue, entry) in wire.into_iter().enumerate() {
            seed(&banks, 0, queue as u8, 511, 5);
            banks[0].cw_min[queue].set(entry.cwmin);
        }

        reset_all_queues(&banks, 0).unwrap();

        for (queue, entry) in wire.into_iter().enumerate() {
            assert_eq!(state(&banks, 0, queue as u8), (0, u32::from(entry.cwmin)));
        }
    }
}
