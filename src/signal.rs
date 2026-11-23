//! The joined AP's signal against the host's thresholds (MIB 0x1009,
//! `wsm_rcpi_rssi_threshold`): cw1200 turns the RCPI_RSSI event into
//! mac80211's connection-quality (CQM) RSSI notifications.
//!
//! Vendor `0xbb9a` (annotated as `vif_reset_state`), called for each beacon
//! of the joined BSS on its channel: a rolling average over the last
//! `count` beacons (at most 16) of RCPI, or RSSI with mode bit 1. Once the
//! window is full, an average below the lower threshold or above the upper
//! one is reported once, until it crosses the other.

/// WSM event (0x0805) id.
pub const EVENT_RCPI_RSSI: u32 = 4;

const ENABLE: u8 = 1 << 0;
const USE_RSSI: u8 = 1 << 1;
const DONT_USE_UPPER: u8 = 1 << 2;
const DONT_USE_LOWER: u8 = 1 << 3;
const WINDOW: usize = 16;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Side {
    /// Nothing reported yet.
    #[default]
    Unknown,
    Below,
    Above,
}

#[derive(Debug, Default)]
pub struct SignalMonitor {
    mode: u8,
    lower: u8,
    upper: u8,
    count: u8,
    samples: [i16; WINDOW],
    next: usize,
    filled: usize,
    side: Side,
    /// The average owed to the host, in the subscribed unit.
    unreported: Option<u8>,
}

impl SignalMonitor {
    pub const fn new() -> Self {
        Self {
            mode: 0,
            lower: 0,
            upper: 0,
            count: 0,
            samples: [0; WINDOW],
            next: 0,
            filled: 0,
            side: Side::Unknown,
            unreported: None,
        }
    }

    /// MIB 0x1009: mode, lower and upper threshold, averaging count.
    pub fn configure(&mut self, data: &[u8]) -> bool {
        let [mode, lower, upper, count] = *data else {
            return false;
        };
        *self = Self {
            mode,
            lower,
            upper,
            count: count.clamp(1, WINDOW as u8),
            ..Self::new()
        };
        true
    }

    /// Joined anew: average from scratch, keeping the subscription.
    pub fn restart(&mut self) {
        *self = Self {
            mode: self.mode,
            lower: self.lower,
            upper: self.upper,
            count: self.count,
            ..Self::new()
        };
    }

    /// A beacon of the joined BSS arrived with `rcpi` (0.5 dB units from
    /// -110 dBm).
    pub fn on_beacon(&mut self, rcpi: u8) {
        if self.mode & ENABLE == 0 {
            return;
        }
        let rssi = self.mode & USE_RSSI != 0;
        self.samples[self.next] = if rssi {
            i16::from(rcpi) / 2 - 110
        } else {
            i16::from(rcpi)
        };
        self.next = (self.next + 1) % usize::from(self.count);
        self.filled = (self.filled + 1).min(usize::from(self.count));
        if self.filled < usize::from(self.count) {
            return;
        }
        let sum: i32 = self.samples[..self.filled]
            .iter()
            .map(|&sample| i32::from(sample))
            .sum();
        let average = (sum / self.filled as i32) as i16;
        // Thresholds are RCPI octets, or signed dBm octets for RSSI.
        let threshold = |value: u8| {
            if rssi {
                i16::from(value as i8)
            } else {
                i16::from(value)
            }
        };
        let (lower, upper) = (threshold(self.lower), threshold(self.upper));
        let side = match self.mode & (DONT_USE_UPPER | DONT_USE_LOWER) {
            0 if average < lower && self.side != Side::Below => Side::Below,
            0 if average > upper && self.side != Side::Above => Side::Above,
            // Only the lower threshold: report going below it once, and
            // rearm silently once back at or above it.
            DONT_USE_UPPER if average < lower && self.side == Side::Unknown => Side::Below,
            DONT_USE_UPPER if average >= lower && self.side == Side::Below => {
                self.side = Side::Unknown;
                return;
            }
            // Only the upper threshold: report going above it once, and
            // rearm silently once back at or below it.
            DONT_USE_LOWER if average > upper && self.side == Side::Unknown => Side::Above,
            DONT_USE_LOWER if average <= upper && self.side == Side::Above => {
                self.side = Side::Unknown;
                return;
            }
            _ => return,
        };
        self.side = side;
        self.unreported = Some(average as u8);
    }

    /// The event data to publish next; call [`Self::reported`] once it is out.
    pub const fn unreported(&self) -> Option<u8> {
        self.unreported
    }

    pub fn reported(&mut self) {
        self.unreported = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(monitor: &mut SignalMonitor, rcpi: u8, beacons: usize) -> Option<u8> {
        (0..beacons).for_each(|_| monitor.on_beacon(rcpi));
        let owed = monitor.unreported();
        monitor.reported();
        owed
    }

    #[test]
    fn crossings_are_reported_once_after_a_full_window() {
        // cw1200's CQM subscription at -70 dBm, hysteresis 4: RCPI 80..88.
        let mut monitor = SignalMonitor::new();
        assert!(monitor.configure(&[ENABLE, 80, 88, 8]));
        assert_eq!(
            feed(&mut monitor, 70, 7),
            None,
            "the window is not full yet"
        );
        assert_eq!(feed(&mut monitor, 70, 1), Some(70));
        assert_eq!(feed(&mut monitor, 70, 20), None, "reported once");
        assert_eq!(feed(&mut monitor, 84, 8), None, "inside the hysteresis");
        assert_eq!(
            feed(&mut monitor, 100, 2),
            None,
            "the average (88) still lags"
        );
        assert_eq!(
            feed(&mut monitor, 100, 6),
            Some(90),
            "the first average above"
        );
        assert_eq!(feed(&mut monitor, 60, 8), Some(75));
    }

    #[test]
    fn disabled_or_dummy_subscriptions_report_nothing() {
        let mut monitor = SignalMonitor::new();
        assert_eq!(feed(&mut monitor, 10, 40), None);
        // cw1200's dummy subscription: enabled, both thresholds unused.
        assert!(monitor.configure(&[ENABLE | DONT_USE_UPPER | DONT_USE_LOWER, 0, 0, 16]));
        assert_eq!(feed(&mut monitor, 10, 40), None);
        assert!(!monitor.configure(&[ENABLE, 0, 0]));
    }

    #[test]
    fn rssi_mode_averages_signed_dbm() {
        let mut monitor = SignalMonitor::new();
        assert!(monitor.configure(&[ENABLE | USE_RSSI, -70_i8 as u8, -66_i8 as u8, 1]));
        // RCPI 60 is -80 dBm.
        assert_eq!(feed(&mut monitor, 60, 1), Some(-80_i8 as u8));
        assert_eq!(feed(&mut monitor, 100, 1), Some(-60_i8 as u8));
    }

    #[test]
    fn lower_only_rearms_silently() {
        let mut monitor = SignalMonitor::new();
        assert!(monitor.configure(&[ENABLE | DONT_USE_UPPER, 80, 0, 1]));
        assert_eq!(feed(&mut monitor, 70, 1), Some(70));
        assert_eq!(feed(&mut monitor, 90, 1), None);
        assert_eq!(feed(&mut monitor, 70, 1), Some(70));
    }

    #[test]
    fn a_new_join_keeps_the_subscription() {
        let mut monitor = SignalMonitor::new();
        assert!(monitor.configure(&[ENABLE, 80, 88, 2]));
        assert_eq!(feed(&mut monitor, 70, 2), Some(70));
        monitor.restart();
        assert_eq!(feed(&mut monitor, 70, 1), None);
        assert_eq!(feed(&mut monitor, 70, 1), Some(70));
    }
}
