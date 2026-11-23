//! Retained WSM scan plan and cooperative scan state.
//!
//! This module owns the host request after the borrowed HIF RX buffer is
//! recycled, iterates every retained channel, and holds each successful tuning
//! result for the host-provided maximum dwell time.

use crate::configuration::Mode0ChannelCalibration;
use crate::phy::{
    ChannelProgramRequestWire, ChannelTransitionScheduler, ChannelTunePlan, PllDivider,
    build_scan_channel_program_request, channel_control_word, channel_frequency_khz_2ghz,
    channel_tune_plan, pll_divider,
};
use crate::wsm::{ScanChannel, StartScanRequest};

pub const MAX_SCAN_CHANNELS: usize = 48;
pub const VENDOR_MAX_SCAN_CHANNELS: usize = 34;
pub const MAX_SCAN_SSIDS: usize = 2;
pub const MAX_SSID_LEN: usize = 32;
/// Probe requests on one channel at most (two probes for each of two SSIDs).
const PROBES_PER_CHANNEL: u8 = 4;

fn deadline_reached(now: u32, deadline: u32) -> bool {
    now.wrapping_sub(deadline) as i32 >= 0
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanError {
    TooManyChannels,
    TooManySsids,
    InvalidRecord,
    InvalidChannelTiming,

    Busy,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetainedScanChannel {
    pub number: u16,
    pub min_channel_time: u32,
    pub max_channel_time: u32,
}

impl From<ScanChannel> for RetainedScanChannel {
    fn from(channel: ScanChannel) -> Self {
        Self {
            number: channel.number,
            min_channel_time: channel.min_channel_time,
            max_channel_time: channel.max_channel_time,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScanCompletion {
    pub status: u32,
    pub psm: u8,
    pub num_channels: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProbeOpportunity {
    pub generation: u32,
    pub if_id: u8,
    pub channel: u8,
    pub ssid_index: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveProbePhase {
    Disabled,
    WaitingForTune,
    WaitingForDelay,
    InFlight,
    Done,
    Failed,
}

/// The scan the host asked for, from START_SCAN to its completion.
pub struct Scan {
    active: bool,
    completion_delay: u8,
    hardware_tune_pending: bool,
    hardware_status: u32,
    current_channel_index: u8,
    dwell_deadline: u32,
    transition: ChannelTransitionScheduler,
    finish_state: u8,
    /// The joined interface the scan is tuning back to.
    returning: Option<u8>,
    finish_deadline: u32,
    if_id: u8,
    band: u8,
    scan_type: u8,
    flags: u8,
    num_probes: u8,
    probe_delay: u8,
    num_channels: u8,
    channels: [RetainedScanChannel; MAX_SCAN_CHANNELS],
    num_ssids: u8,
    ssid_lengths: [u8; MAX_SCAN_SSIDS],
    ssids: [[u8; MAX_SSID_LEN]; MAX_SCAN_SSIDS],
    probe_deadline: u32,
    probe_generation: u32,
    probe_phase: ActiveProbePhase,
    probe_round: u8,
    probe_ssid_index: u8,
    probe_last_status: u16,
    /// Probes left on the current channel: the request's probes times its
    /// SSIDs, as the vendor counts them, at most [`PROBES_PER_CHANNEL`].
    probe_publication_budget: u8,
}

impl Default for Scan {
    fn default() -> Self {
        Self::new()
    }
}

impl Scan {
    pub const fn new() -> Self {
        Self {
            active: false,
            completion_delay: 0,
            hardware_tune_pending: false,
            hardware_status: 0,
            current_channel_index: 0,
            dwell_deadline: 0,
            transition: ChannelTransitionScheduler::new(),
            finish_state: 0,
            returning: None,
            finish_deadline: 0,
            if_id: 0,
            band: 0,
            scan_type: 0,
            flags: 0,
            num_probes: 0,
            probe_delay: 0,
            num_channels: 0,
            channels: [RetainedScanChannel {
                number: 0,
                min_channel_time: 0,
                max_channel_time: 0,
            }; MAX_SCAN_CHANNELS],
            num_ssids: 0,
            ssid_lengths: [0; MAX_SCAN_SSIDS],
            ssids: [[0; MAX_SSID_LEN]; MAX_SCAN_SSIDS],
            probe_deadline: 0,
            probe_generation: 0,
            probe_phase: ActiveProbePhase::Disabled,
            probe_round: 0,
            probe_ssid_index: 0,
            probe_last_status: 0,
            probe_publication_budget: 4,
        }
    }
}

impl Scan {
    /// STOP_SCAN: finish after the channel being scanned; the completion
    /// indication follows as usual.
    pub fn request_stop(&mut self) {
        if self.active {
            self.num_channels = self
                .num_channels
                .min(self.current_channel_index.wrapping_add(1));
        }
    }

    pub fn begin(&mut self, request: &StartScanRequest<'_>, if_id: u8) -> Result<(), ScanError> {
        let num_channels = usize::from(request.num_channels);
        let num_ssids = usize::from(request.num_ssids);
        if if_id > 2 {
            return Err(ScanError::InvalidRecord);
        }
        if num_channels > VENDOR_MAX_SCAN_CHANNELS {
            return Err(ScanError::TooManyChannels);
        }
        if num_ssids > MAX_SCAN_SSIDS {
            return Err(ScanError::TooManySsids);
        }

        if self.active {
            return Err(ScanError::Busy);
        }

        // Vendor 0x13d44 validates every channel before copying the request into
        // retained scan state.
        for index in 0..num_channels {
            let channel = request
                .channel(index)
                .map_err(|_| ScanError::InvalidRecord)?;
            if channel.max_channel_time == 0
                || channel.max_channel_time < channel.min_channel_time
                || u32::from(request.probe_delay) > channel.min_channel_time.wrapping_mul(0x400)
            {
                return Err(ScanError::InvalidChannelTiming);
            }
        }

        self.if_id = if_id;
        self.band = request.band;
        self.scan_type = request.scan_type;
        self.flags = request.flags;
        // Active builds retain the host's requested dwell. The no-default-features
        // image remains the conservative passive rollback.
        let passive_fallback = false;
        self.num_probes = request.num_probes;
        self.probe_delay = request.probe_delay;
        self.num_channels = request.num_channels;
        for index in 0..num_channels {
            let mut channel: RetainedScanChannel = request
                .channel(index)
                .map(Into::into)
                .map_err(|_| ScanError::InvalidRecord)?;
            if passive_fallback {
                if request.num_channels == 1 {
                    // Two beacon intervals compensate for absent probe responses.
                    channel.min_channel_time = channel.min_channel_time.max(220);
                    channel.max_channel_time = channel.max_channel_time.max(250);
                } else {
                    // Keep multi-channel batches below the driver's scan-command
                    // timeout until active probe TX is available.
                    channel.min_channel_time = channel.min_channel_time.max(110);
                    channel.max_channel_time = channel.max_channel_time.max(120);
                }
            }
            self.channels[index] = channel;
        }

        self.num_ssids = request.num_ssids;
        for index in 0..num_ssids {
            let ssid = request.ssid(index).map_err(|_| ScanError::InvalidRecord)?;
            self.ssid_lengths[index] = ssid.len() as u8;
            self.ssids[index][..ssid.len()].copy_from_slice(ssid);
            self.ssids[index][ssid.len()..].fill(0);
        }

        self.completion_delay = 0;
        self.returning = None;
        self.hardware_tune_pending = request.num_channels != 0;
        self.hardware_status = 0;
        self.current_channel_index = 0;
        self.dwell_deadline = 0;
        self.transition = ChannelTransitionScheduler::new();
        self.finish_state = 0;
        self.finish_deadline = 0;
        self.probe_publication_budget = self.channel_probe_budget();
        self.probe_deadline = 0;
        self.probe_generation = self.probe_generation.wrapping_add(1);
        self.probe_phase =
            if self.num_probes != 0 && self.num_channels != 0 && self.probe_publication_budget != 0
            {
                ActiveProbePhase::WaitingForTune
            } else {
                ActiveProbePhase::Disabled
            };
        self.probe_round = 0;
        self.probe_ssid_index = 0;
        self.probe_last_status = 0;
        self.active = true;
        Ok(())
    }

    #[cfg(target_arch = "arm")]
    fn service_unjoined_scan_finish(
        &mut self,
        engine: &crate::tx::TxEngine,
        radio: &mut crate::radio::Radio,
    ) -> bool {
        match self.finish_state {
            0 => false,
            1 => {
                if !crate::tx::probe_runtime_quiescent(engine) || radio.host_transfer_outstanding()
                {
                    return false;
                }
                crate::mac::begin_unjoined_scan_radio_stop();
                crate::phy::begin_scan_stop_rx_disable();
                self.finish_deadline = crate::platform::timer_now().wrapping_add(0x0040_0000);
                self.finish_state = 2;
                false
            }
            2 => {
                if !crate::phy::scan_stop_rx_hardware_drained() {
                    if (crate::platform::timer_now().wrapping_sub(self.finish_deadline) as i32) >= 0
                    {
                        crate::tx::fatal_scan_stop_timeout();
                    }
                    return false;
                }
                crate::tx::stop_phy_operation_7();
                crate::phy::start_scan_stop_calibration_state();
                self.finish_state = 3;
                false
            }
            3 => {
                if !radio.fifo_quiescent() {
                    let index = usize::from(self.num_channels.saturating_sub(1));
                    radio.discard_one_idle(self.channels[index].number);
                    return false;
                }
                crate::mac::finish_unjoined_scan_radio_stop();
                self.finish_state = 4;
                true
            }
            _ => true,
        }
    }

    fn publish_scan_completion(&mut self) -> Option<ScanCompletion> {
        self.active = false;
        Some(ScanCompletion {
            status: self.hardware_status,
            psm: 0,
            num_channels: self.num_channels,
        })
    }

    /// Advance tuning and dwell for the cooperative scan engine.
    pub fn service(
        &mut self,
        engine: &crate::tx::TxEngine,
        radio: &mut crate::radio::Radio,
    ) -> Option<ScanCompletion> {
        #[cfg(not(target_arch = "arm"))]
        let _ = (engine, radio);
        if !self.active {
            return None;
        }

        #[cfg(target_arch = "arm")]
        if let Some(interface) = self.returning {
            match self.transition.service(crate::platform::timer_now()) {
                Ok(None) => return None,
                Ok(Some(_)) => {}
                Err(_) => self.hardware_status = 1,
            }
            self.returning = None;
            if !crate::join::return_from_scan(interface) {
                self.hardware_status = 1;
            }
            return self.publish_scan_completion();
        }

        #[cfg(target_arch = "arm")]
        if self.finish_state != 0 {
            if !self.service_unjoined_scan_finish(engine, radio) {
                return None;
            }
            return self.publish_scan_completion();
        }

        #[cfg(target_arch = "arm")]
        if self.hardware_status == 0 && !self.transition.is_idle() {
            let now = crate::platform::timer_now();
            match self.transition.service(now) {
                Ok(Some(_result)) => {
                    let channel = self.channels[usize::from(self.current_channel_index)];
                    // Vendor arms the dwell after the transition has enabled RX.
                    let armed = crate::platform::timer_now();
                    self.dwell_deadline =
                        armed.wrapping_add(channel.max_channel_time.saturating_mul(0x400));
                    self.probe_deadline = armed.wrapping_add(u32::from(self.probe_delay));
                    self.probe_publication_budget = self.channel_probe_budget();
                    self.probe_phase = if self.num_probes == 0 || self.probe_publication_budget == 0
                    {
                        ActiveProbePhase::Disabled
                    } else {
                        ActiveProbePhase::WaitingForDelay
                    };
                    self.probe_round = 0;
                    self.probe_ssid_index = 0;
                }
                Ok(None) => return None,
                Err(_) => {
                    self.hardware_status = 1;
                    self.probe_phase = ActiveProbePhase::Failed;
                }
            }
        }

        if self.hardware_status == 0 && self.hardware_tune_pending {
            self.hardware_tune_pending = false;
            let index = usize::from(self.current_channel_index);
            let _channel = self.channels[index];
            #[cfg(target_arch = "arm")]
            {
                if self.transition.start(_channel.number, 100_000).is_err() {
                    self.hardware_status = 1;
                } else {
                    self.transition.arm_settle(crate::platform::timer_now());
                }
            }
            #[cfg(not(target_arch = "arm"))]
            {
                self.completion_delay = 1;
            }
            return None;
        }

        if self.hardware_status == 0 {
            if !matches!(
                self.probe_phase,
                ActiveProbePhase::Disabled | ActiveProbePhase::Done | ActiveProbePhase::Failed
            ) {
                return None;
            }

            #[cfg(target_arch = "arm")]
            {
                let now = crate::platform::timer_now();
                if self.num_channels != 0 && !deadline_reached(now, self.dwell_deadline) {
                    return None;
                }
            }
            #[cfg(not(target_arch = "arm"))]
            {
                if self.completion_delay != 0 {
                    self.completion_delay -= 1;
                    return None;
                }
            }

            self.current_channel_index = self.current_channel_index.wrapping_add(1);
            if self.current_channel_index < self.num_channels {
                self.hardware_tune_pending = true;
                self.probe_phase = if self.channel_probe_budget() == 0 {
                    ActiveProbePhase::Disabled
                } else {
                    ActiveProbePhase::WaitingForTune
                };
                return None;
            }
        }

        // A joined interface goes back to its channel and joined MAC state
        // (the vendor's active-VIF branch); stopping the radio is for an
        // unjoined scan only. Without this the radio stayed on the last
        // scanned channel, and the beacon monitor reported the AP lost.
        #[cfg(target_arch = "arm")]
        if let Some(interface) = crate::vif::active_interface() {
            let channel = crate::vif::snapshot(interface).map_or(0, |state| state.channel);
            match self.transition.start(channel, 100_000) {
                Ok(()) => {
                    self.transition.arm_settle(crate::platform::timer_now());
                    self.returning = Some(interface);
                    return None;
                }
                Err(_) => {
                    self.hardware_status = 1;
                    return self.publish_scan_completion();
                }
            }
        }

        #[cfg(target_arch = "arm")]
        if self.num_probes != 0 && self.hardware_status == 0 {
            self.finish_state = 1;
            return None;
        }

        self.publish_scan_completion()
    }

    /// Microseconds until [`Self::service`] has timed work: the end of the
    /// dwell or the delay before the next probe. Zero while it polls the
    /// hardware (tuning, settling, stopping the radio) or waits for a probe
    /// to go out.
    pub fn wake_in(&self, now: u32) -> Option<u32> {
        if !self.active && self.returning.is_none() {
            return None;
        }
        let dwelling = self.returning.is_none()
            && self.finish_state == 0
            && self.hardware_status == 0
            && self.transition.is_idle()
            && !self.hardware_tune_pending
            && cfg!(target_arch = "arm");
        Some(match self.probe_phase {
            _ if !dwelling => 0,
            ActiveProbePhase::WaitingForDelay => crate::deadline::until(self.probe_deadline, now),
            ActiveProbePhase::Disabled | ActiveProbePhase::Done | ActiveProbePhase::Failed => {
                crate::deadline::until(self.dwell_deadline, now)
            }
            ActiveProbePhase::WaitingForTune | ActiveProbePhase::InFlight => 0,
        })
    }

    /// Every channel gets its own probes: a budget for the whole request
    /// left all but the first two channels passive.
    fn channel_probe_budget(&self) -> u8 {
        self.num_probes
            .saturating_mul(self.num_ssids.max(1))
            .min(PROBES_PER_CHANNEL)
    }

    pub fn active_interface(&self) -> Option<u8> {
        self.active.then_some(self.if_id)
    }

    pub fn active_channel(&self) -> Option<u16> {
        (self.active
            && self.hardware_status == 0
            && !self.hardware_tune_pending
            && self.transition.is_idle()
            && self.current_channel_index < self.num_channels)
            .then_some(self.channels[usize::from(self.current_channel_index)].number)
    }

    /// Claims the next probe opportunity for the current channel. An opportunity
    /// is visible only after tuning, RX enable, and the scheduled probe delay.
    fn claim_probe_opportunity_at(&mut self, now: u32) -> Option<ProbeOpportunity> {
        if !self.active
            || self.hardware_status != 0
            || self.hardware_tune_pending
            || !self.transition.is_idle()
            || self.current_channel_index >= self.num_channels
            || self.num_probes == 0
            || self.probe_phase != ActiveProbePhase::WaitingForDelay
            || !deadline_reached(now, self.probe_deadline)
        {
            return None;
        }

        let channel =
            u8::try_from(self.channels[usize::from(self.current_channel_index)].number).ok()?;
        self.probe_phase = ActiveProbePhase::InFlight;
        Some(ProbeOpportunity {
            generation: self.probe_generation,
            if_id: self.if_id,
            channel,
            ssid_index: self.probe_ssid_index,
        })
    }

    #[cfg(target_arch = "arm")]
    pub fn claim_probe_opportunity(&mut self) -> Option<ProbeOpportunity> {
        self.claim_probe_opportunity_at(crate::platform::timer_now())
    }

    #[cfg(not(target_arch = "arm"))]
    pub const fn claim_probe_opportunity(&mut self) -> Option<ProbeOpportunity> {
        None
    }

    pub fn copy_probe_ssid(
        &self,
        opportunity: ProbeOpportunity,
        output: &mut [u8; MAX_SSID_LEN],
    ) -> Option<usize> {
        if self.probe_generation != opportunity.generation
            || self.probe_phase != ActiveProbePhase::InFlight
            || self.if_id != opportunity.if_id
            || self.probe_ssid_index != opportunity.ssid_index
        {
            return None;
        }
        if self.num_ssids == 0 {
            output.fill(0);
            return Some(0);
        }
        let index = usize::from(opportunity.ssid_index);
        if index >= usize::from(self.num_ssids) {
            return None;
        }
        let length = usize::from(self.ssid_lengths[index]);
        output[..length].copy_from_slice(&self.ssids[index][..length]);
        output[length..].fill(0);
        Some(length)
    }

    fn complete_probe_at(&mut self, opportunity: ProbeOpportunity, status: u16, now: u32) -> bool {
        if self.probe_generation != opportunity.generation
            || self.probe_phase != ActiveProbePhase::InFlight
            || self.if_id != opportunity.if_id
            || self.probe_ssid_index != opportunity.ssid_index
        {
            return false;
        }

        self.probe_last_status = status;
        self.probe_publication_budget = self.probe_publication_budget.saturating_sub(1);
        let ssid_count = self.num_ssids.max(1);
        self.probe_ssid_index = self.probe_ssid_index.wrapping_add(1);
        if self.probe_ssid_index >= ssid_count {
            self.probe_ssid_index = 0;
            self.probe_round = self.probe_round.wrapping_add(1);
        }

        if self.probe_round >= self.num_probes || self.probe_publication_budget == 0 {
            self.probe_phase = ActiveProbePhase::Done;
        } else {
            // The XR819 driver's dwell calculation budgets two milliseconds per
            // SSID/probe pair. Serialize publications through legal context return
            // and schedule the next pair at that same conservative interval.
            self.probe_deadline = now.wrapping_add(2 * 0x400);
            self.probe_phase = ActiveProbePhase::WaitingForDelay;
        }
        true
    }

    #[cfg(target_arch = "arm")]
    pub fn complete_probe(&mut self, opportunity: ProbeOpportunity, status: u16) -> bool {
        self.complete_probe_at(opportunity, status, crate::platform::timer_now())
    }

    #[cfg(not(target_arch = "arm"))]
    pub fn complete_probe(&mut self, opportunity: ProbeOpportunity, status: u16) -> bool {
        self.complete_probe_at(opportunity, status, self.probe_deadline)
    }

    /// The probe of `opportunity` could not go out yet: claim it again on a
    /// later pass.
    pub fn defer_probe(&mut self, opportunity: ProbeOpportunity) -> bool {
        if self.probe_generation != opportunity.generation
            || self.probe_phase != ActiveProbePhase::InFlight
            || self.probe_ssid_index != opportunity.ssid_index
        {
            return false;
        }
        self.probe_phase = ActiveProbePhase::WaitingForDelay;
        true
    }

    pub fn fail_probe(&mut self, opportunity: ProbeOpportunity) -> bool {
        if self.probe_generation != opportunity.generation
            || self.probe_phase != ActiveProbePhase::InFlight
        {
            return false;
        }
        self.probe_phase = ActiveProbePhase::Failed;
        self.hardware_status = 1;
        true
    }

    pub fn channel(&self, index: usize) -> Option<RetainedScanChannel> {
        (index < usize::from(self.num_channels)).then_some(self.channels[index])
    }

    pub fn channel_control(&self, index: usize) -> Option<u16> {
        (index < usize::from(self.num_channels))
            .then(|| channel_control_word(self.band, 0, self.channels[index].number))
    }

    pub fn channel_program_request(&self, index: usize) -> Option<ChannelProgramRequestWire> {
        (index < usize::from(self.num_channels)).then(|| {
            build_scan_channel_program_request(self.band, self.flags, self.channels[index].number)
        })
    }

    pub fn channel_tuning(&self, index: usize) -> Option<ChannelTunePlan> {
        self.channel_program_request(index)
            .map(|request| channel_tune_plan(request.operation, request.control.get()))
    }

    pub fn channel_frequency_khz(&self, index: usize) -> Option<u32> {
        (self.band == 0 && index < usize::from(self.num_channels))
            .then(|| channel_frequency_khz_2ghz(self.channels[index].number))
    }

    pub fn channel_pll(&self, index: usize) -> Option<PllDivider> {
        pll_divider(self.channel_frequency_khz(index)?, 1250, 26_000)
    }

    pub fn channel_calibration(&self, index: usize) -> Option<Mode0ChannelCalibration> {
        let channel = self.channel(index)?;
        crate::phy::calibration().mode0_channel_calibration(channel.number as u8)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_scan_records_after_request_buffer_is_gone() {
        let engine = crate::tx::TxEngine::new();
        let mut radio = crate::radio::Radio::new();
        let mut scan = Scan::new();
        let mut payload = [0; 12 + 32];
        payload[9] = 2;
        payload[12..14].copy_from_slice(&6_u16.to_le_bytes());
        payload[16..20].copy_from_slice(&10_u32.to_le_bytes());
        payload[20..24].copy_from_slice(&30_u32.to_le_bytes());
        payload[28..30].copy_from_slice(&11_u16.to_le_bytes());
        payload[32..36].copy_from_slice(&20_u32.to_le_bytes());
        payload[36..40].copy_from_slice(&40_u32.to_le_bytes());
        let Ok(request) = StartScanRequest::parse(&payload) else {
            panic!("valid scan request did not parse");
        };

        assert_eq!(scan.begin(&request, 1), Ok(()));
        assert_eq!(scan.active_interface(), Some(1));
        assert_eq!(scan.channel(0).map(|channel| channel.number), Some(6));
        assert_eq!(scan.channel_control(0), Some(0x0117));
        assert_eq!(
            scan.channel_program_request(0)
                .map(|request| request.control.get()),
            Some(0x0117)
        );
        assert_eq!(
            scan.channel_tuning(0),
            Some(ChannelTunePlan {
                phy_mode: 2,
                recalibrate: true,
            })
        );
        assert_eq!(scan.channel_frequency_khz(0), Some(2_437_000));
        assert_eq!(
            scan.channel_pll(0).map(|divider| divider.register),
            Some(0x356e_c4ec)
        );
        assert_eq!(scan.channel(1).map(|channel| channel.number), Some(11));
        assert_eq!(scan.service(&engine, &mut radio), None);
        assert_eq!(scan.service(&engine, &mut radio), None);
        assert_eq!(scan.service(&engine, &mut radio), None);
        assert_eq!(scan.service(&engine, &mut radio), None);
        assert_eq!(scan.service(&engine, &mut radio), None);
        assert_eq!(
            scan.service(&engine, &mut radio),
            Some(ScanCompletion {
                status: 0,
                psm: 0,
                num_channels: 2,
            })
        );

        payload[20..24].copy_from_slice(&0_u32.to_le_bytes());
        let Ok(request) = StartScanRequest::parse(&payload) else {
            panic!("invalid-timing scan request did not parse");
        };
        assert_eq!(
            scan.begin(&request, 1),
            Err(ScanError::InvalidChannelTiming)
        );
    }

    #[test]
    fn each_channel_gets_its_own_probes() {
        let engine = crate::tx::TxEngine::new();
        let mut radio = crate::radio::Radio::new();
        let mut scan = Scan::new();
        scan.active = true;
        scan.num_channels = 2;
        scan.num_probes = 2;
        scan.num_ssids = 1;
        assert_eq!(scan.channel_probe_budget(), 2);
        scan.num_ssids = 2;
        scan.num_probes = 9;
        assert_eq!(scan.channel_probe_budget(), PROBES_PER_CHANNEL);
        // The first channel used up its probes: the next one still probes.
        scan.probe_publication_budget = 0;
        scan.probe_phase = ActiveProbePhase::Done;
        assert_eq!(scan.service(&engine, &mut radio), None);
        assert_eq!(scan.current_channel_index, 1);
        assert_eq!(scan.probe_phase, ActiveProbePhase::WaitingForTune);
    }

    #[test]
    fn deferred_probe_is_claimed_again() {
        let mut scan = Scan::new();
        scan.active = true;
        scan.num_channels = 1;
        scan.channels[0].number = 13;
        scan.num_probes = 2;
        scan.probe_phase = ActiveProbePhase::WaitingForDelay;

        let first = scan.claim_probe_opportunity_at(0);
        assert!(first.is_some_and(|opportunity| scan.defer_probe(opportunity)));
        assert_eq!(scan.probe_phase, ActiveProbePhase::WaitingForDelay);
        assert_eq!(scan.claim_probe_opportunity_at(0), first);
    }

    #[test]
    fn dwell_deadline_comparison_handles_timer_wrap() {
        assert!(!deadline_reached(0xffff_fff0, 0x0000_0010));
        assert!(deadline_reached(0x0000_0010, 0xffff_fff0));
        assert!(deadline_reached(1234, 1234));
    }

    #[test]
    fn active_probe_state_serializes_requested_publications() {
        let mut storage = Scan::new();
        storage.active = true;
        storage.if_id = 1;
        storage.num_channels = 1;
        storage.channels[0].number = 1;
        storage.num_probes = 2;
        storage.probe_generation = 7;
        storage.probe_deadline = 100;
        storage.probe_phase = ActiveProbePhase::WaitingForDelay;
        storage.probe_publication_budget = 2;

        assert_eq!(storage.claim_probe_opportunity_at(99), None);
        let first = storage
            .claim_probe_opportunity_at(100)
            .unwrap_or(ProbeOpportunity {
                generation: 0,
                if_id: 0,
                channel: 0,
                ssid_index: 0,
            });
        assert_eq!(first.generation, 7);
        assert_eq!(storage.probe_phase, ActiveProbePhase::InFlight);
        assert!(storage.complete_probe_at(first, 0, 200));
        assert_eq!(storage.probe_round, 1);
        assert_eq!(storage.probe_deadline, 200 + 2 * 0x400);
        assert_eq!(storage.probe_phase, ActiveProbePhase::WaitingForDelay);

        assert_eq!(storage.claim_probe_opportunity_at(2247), None);
        let second = storage
            .claim_probe_opportunity_at(2248)
            .unwrap_or(ProbeOpportunity {
                generation: 0,
                if_id: 0,
                channel: 0,
                ssid_index: 0,
            });
        assert!(storage.complete_probe_at(second, 0, 2300));
        assert_eq!(storage.probe_round, 2);
        assert_eq!(storage.probe_publication_budget, 0);
        assert_eq!(storage.probe_phase, ActiveProbePhase::Done);
        assert_eq!(storage.claim_probe_opportunity_at(u32::MAX), None);
    }
}
