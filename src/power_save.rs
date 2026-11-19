//! A station's power save (SET_PM), dynamic: the station tells its AP it
//! sleeps (a null-data frame with the PM bit) once it has been idle, and that
//! it is awake again (PM clear) when a beacon's TIM says the AP holds frames
//! for it. Its own data frames carry PM clear and wake it too.
//!
//! While the AP takes it for asleep and nothing is owed, the receiver dozes
//! (RX off) for at most two beacon intervals, never past a DTIM, using the
//! beacon's timestamp. After a DTIM beacon announcing group frames it stays
//! on until the last of them; after a missed beacon until the next one.

/// A power save mode the host asked for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    Active,
    /// Sleep after `idle_us` without traffic.
    PowerSave {
        idle_us: u32,
    },
}

/// SET_PM modes: active, legacy PS and fast PS (bit 7).
const PSM_PS: u8 = 1;
const PSM_FAST_PS: u8 = 0x81;
/// Idle time before sleeping when the host gives none: mac80211's default
/// dynamic power save timeout.
pub const DEFAULT_IDLE_US: u32 = 100_000;
/// A null frame that got no ACK is sent again after this.
const RETRY_US: u32 = 20_000;
/// The receiver wakes this long before a predicted beacon begins: 3.6 ms to
/// bring the PHY back, then 1 ms listening.
pub const WAKE_MARGIN_US: u32 = 5_000;
/// Our clock and the AP's TSF agree this well between two beacons.
const CLOCK_TOLERANCE_US: u32 = 1_000;
/// Bring up skipping conservatively; one intervening beacon at most.
const MAX_PERIODS: u8 = 2;
/// A doze shorter than this is not worth it.
const MIN_DOZE_US: u32 = 5_000;
/// The receiver listens this long after a DTIM beacon for its group frames
/// unless the last one comes earlier.
const GROUP_WINDOW_US: u32 = 10_000;

impl Mode {
    /// From SET_PM's mode byte and fast PS idle period (500 us units). Legacy
    /// PS polls with PS-Polls in the vendor; here it is dynamic too.
    pub fn from_set_pm(mode: u8, fast_idle_period: u8) -> Self {
        match mode {
            PSM_PS => Mode::PowerSave {
                idle_us: DEFAULT_IDLE_US,
            },
            PSM_FAST_PS => Mode::PowerSave {
                idle_us: if fast_idle_period == 0 {
                    DEFAULT_IDLE_US
                } else {
                    u32::from(fast_idle_period) * 500
                },
            },
            _ => Mode::Active,
        }
    }

    /// The mode byte SET_PM's completion reports.
    pub const fn psm(self) -> u8 {
        match self {
            Mode::Active => 0,
            Mode::PowerSave { .. } => PSM_FAST_PS,
        }
    }
}

/// What the AP last acknowledged hearing from us.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Ap {
    Awake,
    Asleep,
}

/// A null frame owed or on its way: its PM bit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Notice {
    pm: bool,
    /// When it went out, while it is in flight.
    sent_at: Option<u32>,
}

/// The receiver's state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Receiver {
    On,
    Dozing { wake_at: u32 },
}

/// One valid TIM and when its beacon began, in local time.
#[derive(Clone, Copy)]
struct BeaconClock {
    timestamp: u64,
    received: u32,
    interval: u32,
    count: u8,
    period: u8,
}

impl BeaconClock {
    fn next(self) -> u32 {
        self.received
            .wrapping_sub((self.timestamp % u64::from(self.interval)) as u32)
            .wrapping_add(self.interval)
    }

    fn follows(self, previous: Self) -> bool {
        if self.interval != previous.interval
            || self.period != previous.period
            || self.timestamp <= previous.timestamp
        {
            return false;
        }
        let steps = self.timestamp / u64::from(self.interval)
            - previous.timestamp / u64::from(self.interval);
        let elapsed = self.timestamp - previous.timestamp;
        let local = self.received.wrapping_sub(previous.received);
        (1..=u64::from(MAX_PERIODS)).contains(&steps)
            && elapsed < i32::MAX as u64
            && (i64::from(local) - elapsed as i64).abs() <= i64::from(CLOCK_TOLERANCE_US)
            && self.count
                == ((u64::from(previous.count) + u64::from(self.period)
                    - steps % u64::from(self.period))
                    % u64::from(self.period)) as u8
    }

    fn margin(self, periods: u8) -> u32 {
        // Keep the measured RX/airtime allowance, plus 1000 ppm of extra
        // prediction horizon. This is a bring-up guard, not a measured drift.
        WAKE_MARGIN_US + u32::from(periods - 1) * self.interval.div_ceil(1000)
    }
}

pub struct StationPowerSave {
    mode: Mode,
    receiver: Receiver,
    clock: Option<BeaconClock>,
    /// A coherent beacon arrived since the receiver last woke.
    beacon_seen: bool,
    /// Last scheduled wake, to detect a missed target rather than roll it on.
    scheduled_wake: Option<u32>,
    /// MIB 0x1007's fixed period, capped locally; not the association promise.
    periods: u8,
    /// The actual (re)association request's listen interval, or 1 if unknown.
    association_periods: u16,
    /// Group frames after a DTIM beacon are expected until then.
    group_until: Option<u32>,
    /// Our association ID, from SET_BSS_PARAMS; 0 when not associated.
    aid: u16,
    ap: Ap,
    notice: Option<Notice>,
    /// When a frame last went to or came from the AP.
    last_activity: u32,
    /// A null frame that got no ACK goes again after `RETRY_US`.
    retry_at: Option<u32>,
}

impl Default for StationPowerSave {
    fn default() -> Self {
        Self::new()
    }
}

impl StationPowerSave {
    pub const fn new() -> Self {
        Self {
            mode: Mode::Active,
            receiver: Receiver::On,
            clock: None,
            beacon_seen: false,
            scheduled_wake: None,
            periods: 1,
            association_periods: 1,
            group_until: None,
            aid: 0,
            ap: Ap::Awake,
            notice: None,
            last_activity: 0,
            retry_at: None,
        }
    }

    pub const fn mode(&self) -> Mode {
        self.mode
    }

    /// SET_PM.
    pub fn set_mode(&mut self, mode: Mode, now: u32) {
        self.mode = mode;
        self.last_activity = now;
    }

    /// SET_BSS_PARAMS: the association ID the AP gave us.
    pub fn set_aid(&mut self, aid: u16) {
        self.aid = aid & 0x3fff;
    }

    /// MIB 0x1007: byte 0 is the fixed wake period (vendor 0xfff004f2).
    /// The halfword at 2 is an adaptive ceiling, NOT the association listen
    /// interval. Accept it but do not implement adaptation (vendor 0x1960).
    pub fn set_beacon_period(&mut self, data: &[u8]) -> bool {
        let [periods, _, _, _] = data else {
            return false;
        };
        self.periods = (*periods).clamp(1, MAX_PERIODS);
        self.invalidate_beacon();
        true
    }

    /// A (re)association request the host sends `ap`: the listen interval in
    /// it is our promise to the AP. SET_BSS_PARAMS supplies the AID.
    pub fn on_association_request(&mut self, frame: &[u8], ap: [u8; 6]) {
        // Frame control 0x00 (association) or 0x20 (reassociation); the
        // listen interval follows the capabilities in both.
        let request = matches!(frame.first(), Some(0x00 | 0x20));
        if !request
            || frame.get(4..10) != Some(ap.as_slice())
            || frame.get(16..22) != Some(ap.as_slice())
        {
            return;
        }
        // No new sleep notice until SET_BSS_PARAMS confirms an association.
        self.aid = 0;
        self.association_periods = frame.get(26..28).map_or(1, |interval| {
            u16::from_le_bytes([interval[0], interval[1]]).max(1)
        });
        self.invalidate_beacon();
    }

    /// Scan or uncertain timing: stay on until two coherent beacons arrive.
    pub fn invalidate_beacon(&mut self) {
        self.clock = None;
        self.beacon_seen = false;
        self.scheduled_wake = None;
    }

    /// The association is gone: the next AP knows nothing of our sleep. The
    /// caller wakes a dozing receiver first.
    pub fn reset(&mut self) {
        *self = Self {
            mode: self.mode,
            receiver: self.receiver,
            ..Self::new()
        };
    }

    pub const fn receiver(&self) -> Receiver {
        self.receiver
    }

    /// Whether [`Self::doze_until`] may have an answer: cheap, for every
    /// pass.
    pub fn may_doze(&self) -> bool {
        self.receiver == Receiver::On
            && self.mode != Mode::Active
            && self.aid != 0
            && self.ap == Ap::Asleep
            && self.notice.is_none()
            && self.beacon_seen
    }

    /// The receiver may doze now: until when. `loss_in` is the monitor's
    /// remaining silence budget, if enabled. The caller has nothing in flight.
    pub fn doze_until(&self, now: u32, loss_in: Option<u32>) -> Option<u32> {
        let clock = self.clock?;
        let group_done = self
            .group_until
            .is_none_or(|until| now.wrapping_sub(until) as i32 >= 0);
        if !self.may_doze() || !group_done {
            return None;
        }
        // Too close to the next beacon, or past it unseen: listen for it
        // rather than sleep through it.
        if (clock.next().wrapping_sub(WAKE_MARGIN_US).wrapping_sub(now) as i32) < MIN_DOZE_US as i32
        {
            return None;
        }
        // Vendor 0x49b8 uses the TIM count when nonzero. Unlike the vendor,
        // also cap the fixed period by the association promise and by two.
        let dtim = if clock.count == 0 {
            clock.period
        } else {
            clock.count
        };
        let periods = self
            .periods
            .min(self.association_periods.min(u16::from(MAX_PERIODS)) as u8)
            .min(dtim);
        (1..=periods).rev().find_map(|periods| {
            let beacon = clock
                .next()
                .wrapping_add(u32::from(periods - 1) * clock.interval);
            let margin = clock.margin(periods);
            // Leave a margin for receiving the target before BSS_LOST, not
            // merely for switching RX on. Never move the monitor's deadline.
            let fits =
                loss_in.is_none_or(|left| beacon.wrapping_sub(now).saturating_add(margin) < left);
            let wake_at = beacon.wrapping_sub(margin);
            (fits && wake_at.wrapping_sub(now) as i32 >= MIN_DOZE_US as i32).then_some(wake_at)
        })
    }

    /// The dozing receiver must be on: its beacon is due, or the AP takes us
    /// for awake (a frame from it, one of ours) and may send any moment.
    pub fn wants_receiver(&self, now: u32) -> bool {
        match self.receiver {
            Receiver::Dozing { wake_at } => {
                wake_at.wrapping_sub(now) as i32 <= 0
                    || self.mode == Mode::Active
                    || self.ap == Ap::Awake
                    || self.notice.is_some()
            }
            Receiver::On => false,
        }
    }

    /// The receiver went off until `wake_at`.
    pub fn dozed(&mut self, wake_at: u32) {
        self.receiver = Receiver::Dozing { wake_at };
        self.scheduled_wake = Some(wake_at);
    }

    /// The receiver is on again; it dozes next after a beacon.
    pub fn woke(&mut self) {
        self.receiver = Receiver::On;
        self.beacon_seen = false;
    }

    /// A group frame from our AP: the last after a DTIM beacon has More
    /// Data clear.
    pub fn on_group_frame(&mut self, more_data: bool) {
        if !more_data {
            self.group_until = None;
        }
    }

    /// A frame went to the AP (its PM bit clear wakes us) or came from it.
    pub fn on_traffic(&mut self, now: u32) {
        self.last_activity = now;
        self.ap = Ap::Awake;
        if self
            .notice
            .is_some_and(|notice| notice.pm && notice.sent_at.is_none())
        {
            self.notice = None;
        }
    }

    /// A beacon from our AP that began at `now`: wake up when it holds frames
    /// for us. Group frames follow a DTIM beacon whatever our PM bit says;
    /// the receiver stays on for them.
    pub fn on_beacon(&mut self, beacon: &[u8], now: u32) {
        let tim = tim(beacon);
        let clock = beacon_timing(beacon, now)
            .zip(tim)
            .map(|((_, interval), tim)| BeaconClock {
                timestamp: u64::from_le_bytes(beacon[24..32].try_into().unwrap()),
                received: now,
                interval,
                count: tim.dtim_count,
                period: tim.dtim_period,
            });
        // Invalid TIM/clock, changed interval/DTIM phase, or a missed target:
        // this beacon is only an anchor; the next coherent one permits doze.
        self.beacon_seen = clock.zip(self.clock).is_some_and(|(clock, previous)| {
            let deadline = self
                .scheduled_wake
                .map_or(previous.next(), |wake| {
                    wake.wrapping_add(previous.margin(MAX_PERIODS))
                })
                .wrapping_add(previous.interval / 2);
            clock.follows(previous) && (now.wrapping_sub(deadline) as i32) < 0
        });
        self.clock = clock;
        self.scheduled_wake = None;
        if tim.is_some_and(|tim| tim.group_follows()) {
            self.group_until = Some(now.wrapping_add(GROUP_WINDOW_US));
        }
        if self.ap == Ap::Asleep
            && self.notice.is_none()
            && tim.is_some_and(|tim| tim.buffered_for(self.aid))
        {
            self.notice = Some(Notice {
                pm: false,
                sent_at: None,
            });
        }
    }

    /// The null frame to send now, if any: its PM bit.
    pub fn outgoing(&mut self, now: u32) -> Option<bool> {
        if self.aid == 0 {
            return None;
        }
        if let Some(notice) = self.notice {
            return (notice.sent_at.is_none()
                && self
                    .retry_at
                    .is_none_or(|at| now.wrapping_sub(at) as i32 >= 0))
            .then_some(notice.pm);
        }
        let pm = match (self.mode, self.ap) {
            (Mode::PowerSave { idle_us }, Ap::Awake)
                if now.wrapping_sub(self.last_activity) >= idle_us =>
            {
                true
            }
            (Mode::Active, Ap::Asleep) => false,
            _ => return None,
        };
        self.notice = Some(Notice { pm, sent_at: None });
        Some(pm)
    }

    /// The null frame went to the MAC at `now`.
    pub fn published(&mut self, now: u32) {
        if let Some(notice) = self.notice.as_mut() {
            notice.sent_at = Some(now);
        }
    }

    /// The null frame completed: acknowledged by the AP or not.
    pub fn completed(&mut self, acknowledged: bool, now: u32) {
        let Some(notice) = self.notice.take() else {
            return;
        };
        if acknowledged {
            // A frame of ours after the null (PM clear) woke the AP again.
            let traffic_since = notice
                .sent_at
                .is_some_and(|sent| self.last_activity.wrapping_sub(sent) as i32 > 0);
            self.ap = if notice.pm && !traffic_since {
                Ap::Asleep
            } else {
                Ap::Awake
            };
            self.retry_at = None;
            self.last_activity = now;
        } else {
            self.notice = Some(Notice {
                sent_at: None,
                ..notice
            });
            self.retry_at = Some(now.wrapping_add(RETRY_US));
        }
    }

    /// Microseconds until [`Self::outgoing`] has something to send, or the
    /// dozing receiver wakes.
    pub fn wake_in(&self, now: u32) -> Option<u32> {
        if let Receiver::Dozing { wake_at } = self.receiver {
            return Some(crate::deadline::until(wake_at, now));
        }
        if self.aid == 0 || self.notice.is_some_and(|notice| notice.sent_at.is_some()) {
            return None;
        }
        if let Some(at) = self.retry_at.filter(|_| self.notice.is_some()) {
            return Some(crate::deadline::until(at, now));
        }
        match (self.mode, self.ap) {
            (Mode::PowerSave { idle_us }, Ap::Awake) => Some(crate::deadline::until(
                self.last_activity.wrapping_add(idle_us),
                now,
            )),
            _ => self.notice.map(|_| 0),
        }
    }

    /// Whether nothing is owed to the AP.
    pub fn settled(&self) -> bool {
        self.notice.is_none()
    }
}

/// A beacon's traffic indication map.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tim<'a> {
    pub dtim_count: u8,
    pub dtim_period: u8,
    bitmap_control: u8,
    bitmap: &'a [u8],
}

impl Tim<'_> {
    /// A DTIM beacon announcing buffered group frames: they follow it.
    pub fn group_follows(&self) -> bool {
        self.dtim_count == 0 && self.bitmap_control & 1 != 0
    }

    /// Whether the AP holds frames for `aid`.
    pub fn buffered_for(&self, aid: u16) -> bool {
        // The partial bitmap starts at octet N1, an even number.
        let first = usize::from(self.bitmap_control & 0xfe);
        aid != 0
            && usize::from(aid / 8)
                .checked_sub(first)
                .and_then(|index| self.bitmap.get(index))
                .is_some_and(|bits| bits & (1 << (aid % 8)) != 0)
    }
}

/// The local time of the beacon after this one, which began at `now`, and the
/// beacon interval: the AP sends beacons at multiples of the interval of its
/// TSF, and the beacon's timestamp says how far past one it went out.
pub fn beacon_timing(beacon: &[u8], now: u32) -> Option<(u32, u32)> {
    let timestamp = u64::from_le_bytes(beacon.get(24..32)?.try_into().ok()?);
    let interval = u32::from(u16::from_le_bytes(beacon.get(32..34)?.try_into().ok()?)) * 1024;
    if interval == 0 {
        return None;
    }
    let past = (timestamp % u64::from(interval)) as u32;
    Some((now.wrapping_sub(past).wrapping_add(interval), interval))
}

/// The TIM element of a beacon frame.
pub fn tim(beacon: &[u8]) -> Option<Tim<'_>> {
    // Header, then timestamp, beacon interval and capabilities.
    let mut elements = beacon.get(24 + 12..)?;
    while let [id, length, rest @ ..] = elements {
        let (body, tail) = rest.split_at_checked(usize::from(*length))?;
        if *id == 5 {
            return match body {
                [dtim_count, period, bitmap_control, bitmap @ ..]
                    if *period != 0 && dtim_count < period && !bitmap.is_empty() =>
                {
                    Some(Tim {
                        dtim_count: *dtim_count,
                        dtim_period: *period,
                        bitmap_control: *bitmap_control,
                        bitmap,
                    })
                }
                _ => None,
            };
        }
        elements = tail;
    }
    None
}

/// A null-data frame to `ap` from `own` with the PM bit `pm`.
pub fn null_data(ap: [u8; 6], own: [u8; 6], pm: bool) -> [u8; 24] {
    let mut frame = [0; 24];
    // Data, null function, to the DS; PM is bit 12.
    let control: u16 = 0x0148 | if pm { 0x1000 } else { 0 };
    frame[..2].copy_from_slice(&control.to_le_bytes());
    frame[4..10].copy_from_slice(&ap);
    frame[10..16].copy_from_slice(&own);
    frame[16..22].copy_from_slice(&ap);
    frame
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 100 TU beacon, `timestamp` microseconds after TBTT number ten.
    fn timed_beacon(timestamp: u64, tim_body: &[u8]) -> std::vec::Vec<u8> {
        let mut frame = beacon(tim_body);
        frame[24..32].copy_from_slice(&(timestamp + 10 * 102_400).to_le_bytes());
        frame[32..34].copy_from_slice(&100_u16.to_le_bytes());
        frame
    }

    fn beacon(tim_body: &[u8]) -> std::vec::Vec<u8> {
        let mut frame = std::vec![0x80, 0, 0, 0];
        frame.resize(36, 0);
        frame.extend_from_slice(&[0, 3, b'a', b'b', b'c']);
        frame.extend_from_slice(&[5, tim_body.len() as u8]);
        frame.extend_from_slice(tim_body);
        frame
    }

    #[test]
    fn the_tim_names_our_aid() {
        // AID 9: octet 1, bit 1, in a bitmap starting at octet 0.
        assert!(tim(&beacon(&[1, 3, 0, 0, 0x02])).unwrap().buffered_for(9));
        assert!(!tim(&beacon(&[1, 3, 0, 0, 0x04])).unwrap().buffered_for(9));
        // A bitmap starting at octet 2 does not cover AID 9.
        assert!(!tim(&beacon(&[1, 3, 2, 0x02])).unwrap().buffered_for(9));
        // Buffered group frames do not wake us.
        assert!(!tim(&beacon(&[0, 3, 1, 0])).unwrap().buffered_for(9));
        // Truncated elements are not trusted.
        assert_eq!(tim(&beacon(&[0, 3, 1])), None);
        let mut cut = beacon(&[0, 3, 1, 0]);
        cut.truncate(cut.len() - 1);
        assert_eq!(tim(&cut), None);
    }

    #[test]
    fn idle_sleeps_after_an_acknowledged_null_and_the_tim_wakes() {
        let mut ps = StationPowerSave::new();
        ps.set_aid(9);
        ps.set_mode(Mode::PowerSave { idle_us: 1_000 }, 0);
        assert_eq!(ps.outgoing(999), None);
        assert_eq!(ps.wake_in(500), Some(500));
        assert_eq!(ps.outgoing(1_000), Some(true));
        ps.published(1_000);
        assert_eq!(ps.outgoing(1_001), None);
        ps.completed(false, 1_100);
        assert_eq!(ps.outgoing(1_200), None);
        assert_eq!(ps.outgoing(1_100 + RETRY_US), Some(true));
        ps.published(30_000);
        ps.completed(true, 30_100);
        assert_eq!(ps.outgoing(90_000), None);
        ps.on_beacon(&beacon(&[1, 3, 0, 0, 0x02]), 90_000);
        assert_eq!(ps.outgoing(90_001), Some(false));
        ps.published(90_001);
        ps.completed(true, 90_100);
        assert_eq!(ps.outgoing(90_500), None);
    }

    #[test]
    fn traffic_keeps_us_awake_and_active_mode_wakes() {
        let mut ps = StationPowerSave::new();
        ps.set_aid(1);
        ps.set_mode(Mode::PowerSave { idle_us: 1_000 }, 0);
        ps.on_traffic(900);
        assert_eq!(ps.outgoing(1_500), None);
        assert_eq!(ps.outgoing(1_900), Some(true));
        ps.published(1_900);
        ps.completed(true, 1_950);
        ps.set_mode(Mode::Active, 2_000);
        assert_eq!(ps.outgoing(2_000), Some(false));
        // Without an association there is nobody to tell.
        let mut unassociated = StationPowerSave::new();
        unassociated.set_mode(Mode::PowerSave { idle_us: 0 }, 0);
        assert_eq!(unassociated.outgoing(10), None);
    }

    #[test]
    fn the_next_beacon_follows_from_the_timestamp() {
        // 300 us past a TBTT of a 102_400 us interval.
        assert_eq!(
            beacon_timing(&timed_beacon(10 * 102_400 + 300, &[1, 3, 0, 0]), 50_000),
            Some((152_100, 102_400))
        );
        assert_eq!(beacon_timing(&beacon(&[1, 3, 0, 0]), 50_000), None);
    }

    /// A station asleep at the AP, its receiver on, which last saw a beacon
    /// at `now` 300 us after its TBTT.
    fn asleep(now: u32, tim_body: &[u8]) -> StationPowerSave {
        let mut ps = StationPowerSave::new();
        ps.set_aid(9);
        ps.set_mode(Mode::PowerSave { idle_us: 1_000 }, 0);
        assert_eq!(ps.outgoing(1_000), Some(true));
        ps.published(1_000);
        ps.completed(true, 1_100);
        let mut previous_tim = tim_body.to_vec();
        if previous_tim.len() >= 2 && previous_tim[1] != 0 {
            previous_tim[0] = (previous_tim[0] + 1) % previous_tim[1];
        }
        let mut previous = timed_beacon(300, &previous_tim);
        previous[24..32].copy_from_slice(&(9 * 102_400 + 300_u64).to_le_bytes());
        ps.on_beacon(&previous, now.wrapping_sub(102_400));
        ps.on_beacon(&timed_beacon(300, tim_body), now);
        ps
    }

    #[test]
    fn the_receiver_dozes_until_just_before_the_next_beacon() {
        let mut ps = asleep(10_000, &[1, 3, 0, 0]);
        let next = 10_000 - 300 + 102_400;
        assert_eq!(ps.doze_until(10_100, None), Some(next - WAKE_MARGIN_US));
        ps.dozed(next - WAKE_MARGIN_US);
        assert_eq!(ps.wake_in(50_000), Some(next - WAKE_MARGIN_US - 50_000));
        assert_eq!(ps.doze_until(50_000, None), None);
        assert!(!ps.wants_receiver(50_000));
        assert!(ps.wants_receiver(next - WAKE_MARGIN_US));
        // Woken, it waits for the beacon before dozing again.
        ps.woke();
        assert_eq!(ps.doze_until(next - 2_000, None), None);
        ps.on_beacon(&timed_beacon(102_400 + 400, &[0, 3, 0, 0]), next + 100);
        assert_eq!(
            ps.doze_until(next + 200, None),
            Some(next - 300 + 102_400 - WAKE_MARGIN_US)
        );
    }

    #[test]
    fn the_receiver_stays_on_when_it_could_miss_frames() {
        let ps = asleep(10_000, &[1, 3, 0, 0]);
        let next = 10_000 - 300 + 102_400;
        // Idle only within the margin, or past an unseen beacon: listen.
        assert_eq!(ps.doze_until(next - WAKE_MARGIN_US + 1, None), None);
        assert_eq!(ps.doze_until(next + 1_000, None), None);
        // A frame from the AP while dozing: it takes us for awake.
        let mut dozing = asleep(10_000, &[1, 3, 0, 0]);
        dozing.dozed(next - WAKE_MARGIN_US);
        dozing.on_traffic(50_000);
        assert!(dozing.wants_receiver(50_000));
        // A beacon without a TIM could hide frames for us.
        let ps = asleep(10_000, &[]);
        assert_eq!(ps.doze_until(10_100, None), None);
    }

    #[test]
    fn group_frames_and_buffered_unicast_keep_the_receiver_on() {
        // A DTIM beacon announcing group frames: on until the last.
        let mut ps = asleep(10_000, &[0, 3, 1, 0]);
        assert_eq!(ps.doze_until(10_100, None), None);
        ps.on_group_frame(true);
        assert_eq!(ps.doze_until(10_200, None), None);
        ps.on_group_frame(false);
        assert!(ps.doze_until(10_300, None).is_some());
        // Or until the window closes.
        let ps = asleep(10_000, &[0, 3, 1, 0]);
        assert!(ps.doze_until(10_000 + GROUP_WINDOW_US, None).is_some());
        // Our AID's bit: the station wakes the AP instead.
        let ps = asleep(10_000, &[1, 3, 0, 0, 0x02]);
        assert_eq!(ps.doze_until(10_100, None), None);
    }

    fn skipping(now: u32, count: u8, period: u8) -> StationPowerSave {
        let mut ps = asleep(now, &[count, period, 0, 0]);
        ps.periods = 2;
        ps.association_periods = 10;
        ps
    }

    #[test]
    fn fixed_period_is_bounded_by_dtim_and_the_association_promise() {
        for period in 1..=8 {
            for count in 0..period {
                for association in 1..=3 {
                    let mut ps = skipping(10_000, count, period);
                    ps.association_periods = association;
                    let steps =
                        2.min(association as u8)
                            .min(if count == 0 { period } else { count });
                    let target = 9_700 + u32::from(steps) * 102_400;
                    assert_eq!(
                        ps.doze_until(10_100, None),
                        Some(target - WAKE_MARGIN_US - u32::from(steps - 1) * 103)
                    );
                }
            }
        }
        let ps = skipping(10_000, 2, 3);
        // Cannot decide late to skip a beacon we were already due to attend.
        assert_eq!(ps.doze_until(112_100 - WAKE_MARGIN_US, None), None);
    }

    #[test]
    fn mib_and_on_wire_listen_interval_are_different_contracts() {
        let mut ps = skipping(10_000, 2, 3);
        assert!(ps.set_beacon_period(&[255, 0, 0xff, 0xff]));
        assert_eq!(ps.periods, 2);
        assert!(!ps.beacon_seen);
        assert!(!ps.set_beacon_period(&[2, 0]));
        assert!(ps.set_beacon_period(&[0, 0, 0, 0]));
        assert_eq!(ps.periods, 1);
        for subtype in [0, 0x20] {
            let mut frame = [0; 34];
            frame[0] = subtype;
            frame[4..10].fill(7);
            frame[16..22].fill(7);
            frame[26..28].copy_from_slice(&5_u16.to_le_bytes());
            ps.on_association_request(&frame, [8; 6]);
            assert_eq!(ps.association_periods, 10);
            ps.on_association_request(&frame, [7; 6]);
            assert_eq!(ps.association_periods, 5);
            assert_eq!(ps.aid, 0);
            ps.on_association_request(&frame[..27], [7; 6]);
            assert_eq!(ps.association_periods, 1);
            ps.association_periods = 10;
        }
        ps.reset();
        assert_eq!((ps.periods, ps.association_periods), (1, 1));
    }

    #[test]
    fn loss_deadline_shortens_sleep_without_fabricating_beacons() {
        let mut ps = skipping(10_000, 2, 3);
        let mut monitor = crate::beacon_loss::BeaconMonitor::new();
        monitor.join(100);
        monitor.set_lost_count(2, 10_000);
        // Two intervals leaves no RX allowance before the loss deadline.
        let one = 9_700 + 102_400 - WAKE_MARGIN_US;
        assert_eq!(ps.doze_until(10_100, monitor.wake_in(10_100)), Some(one));
        assert_eq!(ps.doze_until(10_100, Some(50_000)), None);
        ps.dozed(one);
        ps.woke();
        assert_eq!(ps.doze_until(200_000, monitor.wake_in(200_000)), None);
        monitor.poll(10_000 + 2 * 102_400 + 1);
        assert_eq!(
            monitor.unreported(),
            Some(crate::beacon_loss::LinkEvent::Lost)
        );
    }

    #[test]
    fn a_two_interval_wake_attends_dtim_and_keeps_group_delivery_awake() {
        let mut ps = skipping(10_000, 2, 3);
        let wake = ps.doze_until(10_100, None).unwrap();
        assert_eq!(wake, 214_500 - WAKE_MARGIN_US - 103);
        ps.dozed(wake);
        assert!(!ps.wants_receiver(wake - 1));
        assert!(ps.wants_receiver(wake));
        ps.woke();
        assert_eq!(ps.doze_until(wake, None), None);
        ps.on_beacon(&timed_beacon(2 * 102_400 + 300, &[0, 3, 1, 0]), 214_800);
        assert!(ps.beacon_seen);
        assert_eq!(ps.doze_until(214_900, None), None);
        ps.on_group_frame(false);
        assert!(ps.doze_until(215_000, None).is_some());
    }

    #[test]
    fn missing_the_scheduled_beacon_requires_reacquisition() {
        let mut ps = skipping(10_000, 2, 3);
        ps.dozed(ps.doze_until(10_100, None).unwrap());
        ps.woke();
        ps.on_beacon(&timed_beacon(3 * 102_400 + 300, &[2, 3, 0, 0]), 317_200);
        assert_eq!(ps.doze_until(317_300, None), None);
        ps.on_beacon(&timed_beacon(4 * 102_400 + 300, &[1, 3, 0, 0]), 419_600);
        assert!(ps.doze_until(419_700, None).is_some());
        // A one-interval plan must not accept the following beacon as its
        // target, even though two intervals are generally allowed.
        let mut ps = skipping(10_000, 1, 3);
        ps.dozed(ps.doze_until(10_100, None).unwrap());
        ps.woke();
        ps.on_beacon(&timed_beacon(2 * 102_400 + 300, &[2, 3, 0, 0]), 214_800);
        assert_eq!(ps.doze_until(214_900, None), None);
    }

    #[test]
    fn invalid_or_changed_timing_and_scan_require_a_coherent_pair() {
        for body in [
            &[][..],
            &[0, 0, 0, 0],
            &[3, 3, 0, 0],
            &[1, 4, 0, 0],
            &[2, 3, 0, 0],
        ] {
            let mut ps = skipping(10_000, 2, 3);
            ps.on_beacon(&timed_beacon(102_400 + 300, body), 112_400);
            assert_eq!(ps.doze_until(112_500, None), None);
        }
        let mut ps = skipping(10_000, 2, 3);
        let mut changed = timed_beacon(102_400 + 300, &[1, 3, 0, 0]);
        changed[32..34].copy_from_slice(&200_u16.to_le_bytes());
        ps.on_beacon(&changed, 112_400);
        assert_eq!(ps.doze_until(112_500, None), None);
        ps.invalidate_beacon(); // successful StartScan
        ps.on_beacon(&timed_beacon(2 * 102_400 + 300, &[0, 3, 0, 0]), 214_800);
        assert_eq!(ps.doze_until(214_900, None), None);
        ps.on_beacon(&timed_beacon(3 * 102_400 + 300, &[2, 3, 0, 0]), 317_200);
        assert!(ps.doze_until(317_300, None).is_some());
    }

    #[test]
    fn active_mode_cannot_sleep_while_waiting_to_clear_pm() {
        let mut ps = skipping(10_000, 2, 3);
        ps.set_mode(Mode::Active, 10_100);
        assert_eq!(ps.doze_until(10_100, None), None);
        ps.dozed(211_397);
        assert!(ps.wants_receiver(10_200));
    }

    #[test]
    fn a_skipped_interval_can_cross_local_timer_wrap() {
        let now = u32::MAX - 50_000;
        let ps = skipping(now, 2, 3);
        assert_eq!(
            ps.doze_until(now + 100, None),
            Some(
                now.wrapping_sub(300)
                    .wrapping_add(204_800 - WAKE_MARGIN_US - 103)
            )
        );
    }

    #[test]
    fn set_pm_modes() {
        assert_eq!(Mode::from_set_pm(0, 0), Mode::Active);
        assert_eq!(
            Mode::from_set_pm(1, 0),
            Mode::PowerSave {
                idle_us: DEFAULT_IDLE_US
            }
        );
        assert_eq!(
            Mode::from_set_pm(0x81, 20),
            Mode::PowerSave { idle_us: 10_000 }
        );
        assert_eq!(
            Mode::from_set_pm(0x81, 0),
            Mode::PowerSave {
                idle_us: DEFAULT_IDLE_US
            }
        );
    }
}
