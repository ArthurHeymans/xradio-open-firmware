//! Beacon-loss detection for the joined BSS.
//!
//! cw1200 drivers set mac80211's `CONNECTION_MONITOR`, which turns off
//! mac80211's own beacon and probe monitoring: the host learns that its AP
//! is gone only from the firmware's BSS_LOST event. It then sends a null
//! frame to the AP and unjoins unless that is acknowledged; BSS_REGAINED
//! calls it off.

/// WSM event (0x0805) ids.
pub const EVENT_BSS_LOST: u32 = 1;
pub const EVENT_BSS_REGAINED: u32 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkEvent {
    Lost,
    Regained,
}

impl LinkEvent {
    pub const fn event_id(self) -> u32 {
        match self {
            Self::Lost => EVENT_BSS_LOST,
            Self::Regained => EVENT_BSS_REGAINED,
        }
    }
}

/// Times the beacons of the joined BSS (any frame from its AP counts).
#[derive(Debug, Default)]
pub struct BeaconMonitor {
    /// Beacon interval of the joined BSS, in microseconds.
    interval_us: u32,
    /// Silence after which the BSS counts as lost; `None` while not asked to
    /// watch (not joined, or no beacon-loss count yet).
    limit_us: Option<i32>,
    last_beacon: u32,
    lost: bool,
    /// Event owed to the host.
    unreported: Option<LinkEvent>,
    /// The AP's averaged beacon signal against the host's thresholds.
    pub signal: crate::signal::SignalMonitor,
    /// Which of its beacons the host wants.
    pub filter: crate::beacon_filter::BeaconFilter,
}

impl BeaconMonitor {
    pub const fn new() -> Self {
        Self {
            interval_us: 0,
            limit_us: None,
            last_beacon: 0,
            lost: false,
            unreported: None,
            signal: crate::signal::SignalMonitor::new(),
            filter: crate::beacon_filter::BeaconFilter::new(),
        }
    }

    /// JOIN: the BSS beacons every `interval_tu` time units. Watching starts
    /// with the beacon-loss count from SET_BSS_PARAMS.
    pub fn join(&mut self, interval_tu: u32) {
        self.reset();
        self.interval_us = interval_tu.saturating_mul(1024);
    }

    /// SET_BSS_PARAMS: report the BSS lost after `count` missed beacons
    /// (0: do not watch).
    pub fn set_lost_count(&mut self, count: u8, now: u32) {
        self.limit_us = (count != 0 && self.interval_us != 0).then(|| {
            i32::try_from(self.interval_us.saturating_mul(u32::from(count))).unwrap_or(i32::MAX)
        });
        if self.limit_us.is_none() {
            self.lost = false;
            self.unreported = None;
        }
        self.last_beacon = now;
    }

    /// Unjoin: stop watching and forget what was owed. The signal
    /// subscription and the beacon filter stay: the host sets them once.
    pub fn reset(&mut self) {
        let mut signal = core::mem::take(&mut self.signal);
        let mut filter = core::mem::take(&mut self.filter);
        signal.restart();
        filter.restart();
        *self = Self {
            signal,
            filter,
            ..Self::new()
        };
    }

    /// A beacon, or any other frame, from the joined BSS's AP arrived.
    pub fn on_beacon(&mut self, now: u32) {
        self.last_beacon = now;
        if self.lost {
            self.lost = false;
            self.owe(LinkEvent::Regained);
        }
    }

    /// Off channel (scanning) no beacon can arrive: count from `now` again.
    pub fn restart(&mut self, now: u32) {
        self.last_beacon = now;
    }

    /// Callers may pass a `now` sampled before the latest beacon's: that
    /// counts as no time passed.
    pub fn poll(&mut self, now: u32) {
        if let Some(limit) = self.limit_us
            && !self.lost
            && (now.wrapping_sub(self.last_beacon) as i32) > limit
        {
            self.lost = true;
            self.owe(LinkEvent::Lost);
        }
    }

    /// Microseconds until [`Self::poll`] could find the BSS lost.
    pub fn wake_in(&self, now: u32) -> Option<u32> {
        let limit = self.limit_us.filter(|_| !self.lost)?;
        Some(crate::deadline::until(
            self.last_beacon.wrapping_add(limit as u32).wrapping_add(1),
            now,
        ))
    }

    /// Only the latest state matters: a Lost and a Regained not yet reported
    /// cancel out.
    fn owe(&mut self, event: LinkEvent) {
        self.unreported = match self.unreported {
            Some(owed) if owed != event => None,
            _ => Some(event),
        };
    }

    /// The event to publish next; call [`Self::reported`] once it is out.
    pub const fn unreported(&self) -> Option<LinkEvent> {
        self.unreported
    }

    pub fn reported(&mut self) {
        self.unreported = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TU: u32 = 1024;

    fn watching(count: u8) -> BeaconMonitor {
        let mut monitor = BeaconMonitor::new();
        monitor.join(100);
        monitor.set_lost_count(count, 0);
        monitor
    }

    #[test]
    fn silence_past_the_count_loses_the_bss_once() {
        let mut monitor = watching(10);
        monitor.poll(1000 * TU);
        assert_eq!(monitor.unreported(), None);
        monitor.on_beacon(1000 * TU);
        monitor.poll(2000 * TU);
        assert_eq!(monitor.unreported(), None);
        monitor.poll(2001 * TU);
        assert_eq!(monitor.unreported(), Some(LinkEvent::Lost));
        monitor.reported();
        monitor.poll(9000 * TU);
        assert_eq!(monitor.unreported(), None);
        monitor.on_beacon(9001 * TU);
        assert_eq!(monitor.unreported(), Some(LinkEvent::Regained));
    }

    #[test]
    fn nothing_is_watched_without_a_count_or_after_reset() {
        let mut monitor = BeaconMonitor::new();
        monitor.join(100);
        monitor.poll(u32::MAX / 2);
        assert_eq!(monitor.unreported(), None);
        let mut monitor = watching(0);
        monitor.poll(u32::MAX / 2);
        assert_eq!(monitor.unreported(), None);
        let mut monitor = watching(1);
        monitor.poll(200 * TU);
        monitor.reset();
        assert_eq!(monitor.unreported(), None);
        monitor.on_beacon(300 * TU);
        assert_eq!(monitor.unreported(), None);
        // Disabling an active watch clears any unreported loss and wake deadline.
        let mut monitor = watching(1);
        monitor.poll(200 * TU);
        assert_eq!(monitor.unreported(), Some(LinkEvent::Lost));
        monitor.set_lost_count(0, 200 * TU);
        assert_eq!(monitor.unreported(), None);
        assert_eq!(monitor.wake_in(200 * TU), None);
    }

    #[test]
    fn an_unreported_loss_and_regain_cancel_out() {
        let mut monitor = watching(1);
        monitor.poll(101 * TU);
        monitor.on_beacon(102 * TU);
        assert_eq!(monitor.unreported(), None);
    }

    #[test]
    fn a_scan_restarts_the_count() {
        let mut monitor = watching(2);
        monitor.restart(500 * TU);
        monitor.poll(700 * TU);
        assert_eq!(monitor.unreported(), None);
        monitor.poll(701 * TU);
        assert_eq!(monitor.unreported(), Some(LinkEvent::Lost));
    }

    #[test]
    fn a_stale_now_is_no_silence() {
        let mut monitor = watching(1);
        monitor.on_beacon(1000 * TU);
        monitor.poll(900 * TU);
        assert_eq!(monitor.unreported(), None);
    }

    #[test]
    fn the_timer_may_wrap() {
        let mut monitor = watching(1);
        monitor.on_beacon(u32::MAX - 10);
        monitor.poll(50);
        assert_eq!(monitor.unreported(), None);
        monitor.poll(100 * TU);
        assert_eq!(monitor.unreported(), Some(LinkEvent::Lost));
    }
}
