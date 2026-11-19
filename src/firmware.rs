//! The cooperative main loop and the state it owns.
//!
//! Every pass services the interrupt sources, the TX lane, the indications
//! owed to the host, TX confirmations, scanning, the RX lane, at most one
//! host command and an access point's next beacon, in that order. Nothing preempts it but the IRQ handler,
//! which only takes note for the sources' owners (`interrupts`).
//!
//! Once passes have moved nothing for [`LINGER_US`], each idle pass is
//! followed by a sleep until an interrupt source is pending or the next
//! timed work is due (`sleep_budget`). A received frame ends the sleep
//! through the MAC's FIQ line, but with the minimal host driver sleeping
//! between the frames of a flow loses some of them
//! (`docs/reference/measurements.md`); the linger keeps the loop awake while
//! traffic flows.

use crate::activity::Activity;
use crate::command::PendingIndications;
use crate::configuration::Configuration;
use crate::crypto::Crypto;
use crate::hif::{SHARED_BUFFER_SIZE, Transport};
use crate::platform::service_masked_packet_dma_interrupt;
use crate::radio::Radio;
use crate::scan::Scan;
use crate::tx::HostTxDriver;
use crate::wsm::{
    encode_join_complete_indication, encode_multi_tx_confirm_header,
    encode_scan_complete_indication, encode_set_pm_complete_indication,
    encode_switch_channel_indication, encode_tx_confirm, encode_tx_confirm_entry,
};
use crate::{beacon_loss, command, scan, tx, vif};

/// The longest sleep while work is in flight: frames queued for the MAC, in
/// decryption or held for reordering, output the host has not read. Their
/// owners do not all name a deadline or get an interrupt for each step.
const WAKE_BOUND_US: u32 = 1_000;

/// The vendor's pipe watchdog period.
const WATCHDOG_PERIOD_US: u32 = 200_000;

/// How long passes must move nothing before the loop sleeps. Frames of a
/// flow arrive far closer together: with the minimal host driver, sleeping
/// between them lost 0.5-7% of a 30 Mbit/s downlink, waiting awake on the
/// same wake conditions nothing.
const LINGER_US: u32 = 5_000;

/// The firmware after startup: the HIF transport, the TX engine and the
/// lane state retained between passes.
pub struct Firmware {
    pub(crate) transport: &'static mut Transport,
    pub(crate) tx_engine: &'static mut tx::TxEngine,
    pub(crate) response_scratch: &'static mut [u8; SHARED_BUFFER_SIZE],
    pub(crate) host_tx_driver: &'static mut HostTxDriver,
    pub(crate) crypto: &'static mut Crypto,
    pub(crate) radio: &'static mut Radio,
    pub(crate) scan: &'static mut Scan,
    pub(crate) configuration: &'static mut Configuration,
    pending_scan_completion: Option<scan::ScanCompletion>,
    pub(crate) pending_indications: PendingIndications,
    pub(crate) beacons: beacon_loss::BeaconMonitor,
    pub(crate) ap: crate::ap::AccessPoint,
    pub(crate) power_save: crate::power_save::StationPowerSave,
    pending_tx_confirmation: Option<(u32, u32, u8, u8)>,
    /// A RESET or management frame waiting for the frames in flight, and
    /// when it arrived.
    pub(crate) held_request: Option<(crate::hif::ReceivedRequest, u32)>,
    last_watchdog_tick: u32,
    watchdog_timer_countdown: u8,
    /// Timer value of the latest watchdog sample.
    now: u32,
}

impl Firmware {
    pub fn new(
        transport: &'static mut Transport,
        tx_engine: &'static mut tx::TxEngine,
        response_scratch: &'static mut [u8; SHARED_BUFFER_SIZE],
        host_tx_driver: &'static mut HostTxDriver,
        crypto: &'static mut Crypto,
        radio: &'static mut Radio,
        scan: &'static mut Scan,
        configuration: &'static mut Configuration,
    ) -> Self {
        Self {
            transport,
            tx_engine,
            response_scratch,
            host_tx_driver,
            crypto,
            radio,
            scan,
            configuration,
            pending_scan_completion: None,
            pending_indications: PendingIndications::default(),
            beacons: beacon_loss::BeaconMonitor::new(),
            ap: crate::ap::AccessPoint::new(),
            power_save: crate::power_save::StationPowerSave::new(),
            pending_tx_confirmation: None,
            last_watchdog_tick: 0,
            watchdog_timer_countdown: 0,
            now: 0,
            held_request: None,
        }
    }

    pub fn run(mut self) -> ! {
        // SAFETY: the IRQ handler touches only the interrupt records, which
        // every user reads in a critical section.
        unsafe { aarch32_cpu::interrupt::enable() };
        let mut last_progress = crate::platform::timer_now();
        loop {
            let before = self.activity();
            self.pass();
            let now = crate::platform::timer_now();
            if self.activity() != before {
                last_progress = now;
            } else if now.wrapping_sub(last_progress) >= LINGER_US {
                let budget = self.sleep_budget(now);
                if budget != 0 {
                    crate::platform::wait_for_event(budget);
                    // Read the clock in the next pass.
                    self.watchdog_timer_countdown = 0;
                }
            }
        }
    }

    /// Changes whenever a pass makes progress.
    fn activity(&self) -> Activity {
        let own = u32::from(self.pending_scan_completion.is_some())
            | u32::from(self.pending_indications.join_complete.is_some()) << 1
            | u32::from(self.pending_indications.channel_switch) << 2
            | u32::from(self.pending_indications.set_pm_complete) << 3
            | u32::from(self.pending_tx_confirmation.is_some()) << 4
            | u32::from(self.beacons.unreported().is_some()) << 5
            | u32::from(tx::host_management_runtime_active(self.tx_engine)) << 6
            | u32::from(self.beacons.signal.unreported().is_some()) << 7;
        let activity = self.transport.activity(Activity::new().with(own));
        let activity = self.radio.activity(activity);
        let activity = self.crypto.activity(activity);
        let activity = self.host_tx_driver.activity(activity);
        self.tx_engine.activity(activity)
    }

    /// Work that advances with time rather than with an interrupt: scan
    /// dwells and a BlockAck the RX FIFO may hold any moment.
    /// How long the loop may sleep at `now`: until the earliest timed work,
    /// and at most [`WAKE_BOUND_US`] unless every lane is settled.
    fn sleep_budget(&self, now: u32) -> u32 {
        let watchdog = crate::deadline::until(
            self.last_watchdog_tick.wrapping_add(WATCHDOG_PERIOD_US),
            now,
        );
        let timed = [
            self.beacons.wake_in(now),
            self.tx_engine.ba.wake_in(now),
            tx::aggregate::wake_in(now),
            self.scan.wake_in(now),
            self.ap.wake_in(),
            self.power_save.wake_in(now),
        ]
        .into_iter()
        .flatten()
        .fold(watchdog, u32::min);
        if self.settled() {
            timed
        } else {
            timed.min(WAKE_BOUND_US)
        }
    }

    /// Nothing queued or in flight in any lane: what is left waits for an
    /// interrupt (a host request, a received frame) or for timed work.
    fn settled(&self) -> bool {
        self.transport.settled() && self.lanes_settled()
    }

    /// [`Self::settled`] but for the host interface.
    fn lanes_settled(&self) -> bool {
        let indications = &self.pending_indications;
        self.radio.settled()
            && self.crypto.settled()
            && self.host_tx_driver.management_runtime_available()
            && tx::probe_runtime_quiescent(self.tx_engine)
            && self.pending_tx_confirmation.is_none()
            && self.held_request.is_none()
            && self.power_save.settled()
            && self.pending_scan_completion.is_none()
            && indications.join_complete.is_none()
            && !indications.channel_switch
            && !indications.set_pm_complete
            && !self.ap.multicast_start_owed()
            && self.beacons.unreported().is_none()
            && self.beacons.signal.unreported().is_none()
    }

    fn pass(&mut self) {
        let _ = service_masked_packet_dma_interrupt();
        let _ = self.transport.service_interrupt();
        self.wake_receiver_if_needed();
        self.service_watchdog();
        // Include commands blocked on storage and a RESET already taken
        // off the request ring. Keep the next free reply's worth of capacity
        // for them instead of letting asynchronous output take it first.
        let host_request_waiting = self.held_request.is_some() || self.transport.request_pending();
        self.service_tx();
        self.publish_indications(host_request_waiting);
        self.publish_tx_confirmations(host_request_waiting);
        self.service_scan(host_request_waiting);
        self.service_rx(host_request_waiting);
        // Not with the receiver dozing: a scan would start from a halted
        // PHY, and a frame would go out unheard. The next pass wakes it.
        if self.power_save.receiver() == crate::power_save::Receiver::On {
            self.service_command();
        }
        self.ap.service(self.configuration);
        // Start encrypting what the command lane admitted.
        self.host_tx_driver.seal(self.crypto);
        self.doze_receiver_if_idle();
    }

    /// A dozing receiver wakes for its beacon and before anything else
    /// happens: a host request (one that cannot be answered yet too, since
    /// answering it reclaims output first), a frame to send, a BlockAck
    /// request to resend. The host reading what we sent it is no reason: the
    /// receiver would stay on until the next beacon.
    fn wake_receiver_if_needed(&mut self) {
        if let crate::power_save::Receiver::Dozing { .. } = self.power_save.receiver() {
            let now = crate::platform::timer_now();
            if self.power_save.wants_receiver(now)
                || self.transport.request_pending()
                || !self.lanes_settled()
                || self.tx_engine.ba.wake_in(now) == Some(0)
                || self.beacons.wake_in(now) == Some(0)
            {
                // A radio that does not come back is as good as dead.
                #[cfg(target_arch = "arm")]
                if !crate::phy::wake_receiver(vif::active_interface()) {
                    crate::halt_always!();
                }
                self.power_save.woke();
            }
        }
    }

    /// A station asleep at its AP with nothing in flight dozes its receiver
    /// and PHY until just before a selected beacon, bounded by DTIM and BSS
    /// loss.
    fn doze_receiver_if_idle(&mut self) {
        if !self.power_save.may_doze() || self.scan.active_interface().is_some() || !self.settled()
        {
            return;
        }
        let now = crate::platform::timer_now();
        if let Some(wake_at) = self.power_save.doze_until(now, self.beacons.wake_in(now)) {
            #[cfg(target_arch = "arm")]
            if crate::phy::doze_receiver() {
                self.power_save.dozed(wake_at);
            }
        }
    }

    fn service_watchdog(&mut self) {
        // Vendor runs a 200 ms timer (`FUN_00003bac`) that decrements each
        // programmed pipe's watchdog byte and recovers a pipe that has stayed
        // armed without completing. Without it an armed pipe is unrecoverable:
        // retirement only ever runs from a delivered status, and the MAC stops
        // delivering statuses for a wedged pipe.
        #[cfg(target_arch = "arm")]
        {
            let (next_countdown, sample_timer) =
                tx::advance_watchdog_timer_divider(self.watchdog_timer_countdown);
            self.watchdog_timer_countdown = next_countdown;
            if sample_timer {
                let now = crate::platform::timer_now();
                self.now = now;
                // The vendor-shaped five-tick expiry takes about one second. A
                // stuck pipe remains armed during that window and blocks every
                // new reservation for the pipe. Retirement cannot help because
                // it only runs from a delivered status.
                if now.wrapping_sub(self.last_watchdog_tick) >= WATCHDOG_PERIOD_US {
                    self.last_watchdog_tick = now;
                    tx::service_pipe_watchdog_tick();
                }
                tx::service_response_timeouts(now);
            }
        }
    }

    fn service_tx(&mut self) {
        // A firmware BlockAck frame (ADDBA request, BlockAckReq) holds back new
        // data batches until the MAC has no data frame, then goes out through
        // the management publisher.
        let key_change_pending = command::key_change_pending(self);
        let block_ack_waiting = !key_change_pending
            && vif::active_interface().is_some()
            && (self.radio.block_ack_response().is_some()
                || self.tx_engine.ba.outgoing(self.now).is_some());
        self.host_tx_driver.seal(self.crypto);
        self.host_tx_driver.service(
            self.tx_engine,
            self.radio.fifo(),
            !tx::host_management_runtime_active(self.tx_engine) && !block_ack_waiting,
        );

        let management_runtime_available = self.host_tx_driver.management_runtime_available();

        if (management_runtime_available || tx::host_management_runtime_active(self.tx_engine))
            && self.pending_tx_confirmation.is_none()
            && let tx::HostManagementTxReport::Completed {
                owner,
                status,
                tx_rate,
                ack_failures,
            } = tx::service_host_management_tx(self.tx_engine, self.crypto, None, 32)
        {
            match owner {
                tx::ManagementOwner::BlockAck => self.tx_engine.ba.completed(self.now),
                tx::ManagementOwner::PowerSave => self
                    .power_save
                    .completed(status == 0, crate::platform::timer_now()),
                tx::ManagementOwner::Host { packet_id } => {
                    self.pending_tx_confirmation = Some((packet_id, status, tx_rate, ack_failures));
                }
            }
        }

        // Our own frames wait out a scan: off the AP's channel they go
        // unheard, and their retries keep the publisher from the scan's
        // probes.
        let scanning = self.scan.active_interface().is_some();
        // Ask again: the completion just handled may have been this very
        // frame, and publishing the stale answer sent every one twice.
        if !key_change_pending
            && !scanning
            && self.pending_tx_confirmation.is_none()
            && !tx::host_management_runtime_active(self.tx_engine)
            && self.host_tx_driver.hardware_idle()
            && let Some(if_id) = vif::active_interface()
            && let Some((link, frame)) = self
                .radio
                .block_ack_response()
                .map(|frame| (0, frame))
                .or_else(|| self.tx_engine.ba.outgoing(self.now))
        {
            let tid = frame.tid();
            let ssn =
                self.host_tx_driver
                    .window_start(link, tid, tx::next_sequence(if_id, link, tid));
            // A response names its requester; a request goes to the link's peer.
            let recipient = vif::link_peer(if_id, link);
            let published = match (frame, recipient) {
                (tx::ba::Outgoing::AddbaResponse { peer, .. }, _) | (_, Some(peer)) => {
                    tx::publish_block_ack_frame(
                        self.tx_engine,
                        self.crypto,
                        frame,
                        if_id,
                        peer,
                        ssn,
                    )
                }
                (_, None) => false,
            };
            let sessions = self.tx_engine.ba.link(link);
            match frame {
                // Answering the peer: nothing to track once it is out.
                tx::ba::Outgoing::AddbaResponse { .. } => self.radio.block_ack_response_sent(),
                _ if published => {
                    // Its PM bit is clear: the AP takes us for awake.
                    self.power_save.on_traffic(crate::platform::timer_now());
                    sessions.published(frame)
                }
                // Unsendable: drop it rather than hold data back forever.
                _ => sessions.sent(frame, self.now),
            }
        } else if !key_change_pending
            && !scanning
            && self.pending_tx_confirmation.is_none()
            && !tx::host_management_runtime_active(self.tx_engine)
            && self.host_tx_driver.hardware_idle()
            && let Some(if_id) = vif::active_interface()
            && let Some(pm) = self.power_save.outgoing(crate::platform::timer_now())
        {
            // Not `self.now`, the watchdog's sample: traffic is timed with
            // the timer itself, and an older `now` reads as a long idle.
            let now = crate::platform::timer_now();
            if tx::publish_power_save_null(self.tx_engine, self.crypto, if_id, pm) {
                self.power_save.published(now);
            } else {
                self.power_save.completed(false, now);
            }
        }
    }

    fn publish_indications(&mut self, host_request_waiting: bool) {
        if let Some(status) = self.pending_indications.join_complete
            && !host_request_waiting
            && self
                .transport
                .publish_with(|output| encode_join_complete_indication(status, output).ok())
        {
            self.pending_indications.join_complete = None;
        }
        if self.pending_indications.channel_switch
            && !host_request_waiting
            && self
                .transport
                .publish_with(|output| encode_switch_channel_indication(0, output).ok())
        {
            self.pending_indications.channel_switch = false;
        }
        // Off channel no beacon can arrive.
        if self.scan.active_interface().is_some() {
            self.beacons.restart(self.now);
        } else {
            self.beacons.poll(self.now);
        }
        if let Some(event) = self.beacons.unreported()
            && !host_request_waiting
            && self
                .transport
                .publish_with(|output| command::encode_debug_event(event.event_id(), 0, output))
        {
            self.beacons.reported();
        }
        if let Some(average) = self.beacons.signal.unreported()
            && !host_request_waiting
            && self.transport.publish_with(|output| {
                command::encode_debug_event(
                    crate::signal::EVENT_RCPI_RSSI,
                    u32::from(average),
                    output,
                )
            })
        {
            self.beacons.signal.reported();
        }
        if self.ap.multicast_start_owed()
            && !host_request_waiting
            && self
                .transport
                .publish_with(|output| crate::wsm::encode_multicast_start_indication(output).ok())
        {
            self.ap.multicast_start_published();
        }
        if self.pending_indications.set_pm_complete
            && !host_request_waiting
            && self.transport.publish_with(|output| {
                encode_set_pm_complete_indication(self.power_save.mode().psm(), output).ok()
            })
        {
            self.pending_indications.set_pm_complete = false;
        }
    }

    fn publish_tx_confirmations(&mut self, host_request_waiting: bool) {
        if let Some((packet_id, status, tx_rate, ack_failures)) = self.pending_tx_confirmation
            && !host_request_waiting
            && self.transport.publish_with(|output| {
                encode_tx_confirm(
                    self.configuration.tx_confirm_format(),
                    packet_id,
                    status,
                    tx_rate,
                    ack_failures,
                    output,
                )
                .ok()
            })
        {
            self.pending_tx_confirmation = None;
            crate::counters::tx_completed(status, ack_failures);
        }

        self.publish_coalesced_host_tx_confirmations(host_request_waiting);
    }

    /// Confirm as many completed frames as one output buffer holds: an A-MPDU
    /// completes up to 16 at once, and each message costs the host a transfer.
    ///
    /// A waiting host request keeps one reply's worth of output for itself,
    /// but confirmations must not wait for the request lane to be empty:
    /// under continuous uplink there is always a TX request, and the host
    /// gets its TX credits back only with the confirmations.
    fn publish_coalesced_host_tx_confirmations(&mut self, host_request_waiting: bool) {
        let reserve = u32::from(host_request_waiting);
        if self.transport.spare_responses() <= reserve {
            return;
        }
        let format = self.configuration.tx_confirm_format();
        let count = self
            .host_tx_driver
            .confirmation_count((self.response_scratch.len() - 8) / format.entry_len());
        if count == 0 {
            return;
        }
        let Ok(length) = encode_multi_tx_confirm_header(format, count, self.response_scratch)
        else {
            return;
        };
        let mut first_release = None;
        for index in 0..count {
            let Some(confirmation) = self.host_tx_driver.confirmation() else {
                return;
            };
            if encode_tx_confirm_entry(
                format,
                confirmation.packet_id,
                confirmation.status,
                confirmation.tx_rate,
                confirmation.ack_failures,
                confirmation.flags,
                confirmation.rate_try,
                8 + index * format.entry_len(),
                self.response_scratch,
            )
            .is_err()
            {
                return;
            }
            let Some(release) = self.host_tx_driver.finish_confirmation() else {
                return;
            };
            crate::counters::tx_completed(confirmation.status, confirmation.ack_failures);
            if first_release.is_none() {
                first_release = Some(release);
            } else {
                self.transport.release_request(release);
            }
        }
        if let Some(release) = first_release {
            self.transport.publish_request_in_place(
                release,
                &self.response_scratch[..length],
                length as u16,
            );
        }
    }

    fn service_scan(&mut self, host_request_waiting: bool) {
        // Retain completion until a HIF descriptor is available. This prevents
        // a full ring from overwriting an unreclaimed zero-copy RX token.
        if self.pending_scan_completion.is_none() {
            self.radio.reclaim(self.transport);
            self.pending_scan_completion = self.scan.service(self.tx_engine, self.radio);
        }

        let mut probe_ssid = [0_u8; scan::MAX_SSID_LEN];
        let mut opportunity = self.scan.claim_probe_opportunity();
        let ssid_length =
            opportunity.and_then(|value| self.scan.copy_probe_ssid(value, &mut probe_ssid));
        if let (Some(value), None) = (opportunity, ssid_length) {
            opportunity = None;
            let _ = self.scan.fail_probe(value);
        }
        let report = {
            tx::service_active_probe(
                self.tx_engine,
                self.configuration
                    .template(crate::configuration::TEMPLATE_PROBE_REQUEST),
                opportunity,
                &probe_ssid[..ssid_length.unwrap_or(0)],
                32,
            )
        };
        match report {
            tx::ActiveProbeReport::Published {
                opportunity,
                publication,
            } => {
                let _ = (opportunity, publication);
            }
            tx::ActiveProbeReport::Completed {
                opportunity,
                context,
                status,
            } => {
                if !self.scan.complete_probe(opportunity, status) {
                    loop {
                        core::hint::spin_loop();
                    }
                }
                let _ = context;
            }
            tx::ActiveProbeReport::Deferred { opportunity } => {
                let _ = self.scan.defer_probe(opportunity);
            }
            tx::ActiveProbeReport::Failed { opportunity, error } => {
                let _ = self.scan.fail_probe(opportunity);
                let _ = error;
            }
            tx::ActiveProbeReport::Idle | tx::ActiveProbeReport::Servicing => {}
        }

        let diagnostics =
            self.radio.diagnostic_word() | tx::active_probe_diagnostic_word(self.tx_engine);
        if let Some(completion) = self.pending_scan_completion
            && !host_request_waiting
            && self.transport.publish_with(|output| {
                encode_scan_complete_indication(
                    completion.status,
                    completion.psm,
                    completion.num_channels,
                    diagnostics,
                    output,
                )
                .ok()
            })
        {
            self.pending_scan_completion = None;
        }
    }

    fn service_rx(&mut self, host_request_waiting: bool) {
        self.radio.reclaim(self.transport);
        // A waiting host request keeps the last firmware-to-host descriptor:
        // otherwise a steady joined RX stream could take it every pass, and
        // `poll_request()` would never detach an ordinary TX request even
        // though the driver accounts its input buffer as used. RX must not
        // wait for the request lane to be empty, though: under continuous
        // uplink there is always a request, and RX (beacons included) starved
        // until the host declared the AP lost.
        if self.pending_scan_completion.is_none()
            && (!host_request_waiting || self.transport.spare_publications() > 1)
        {
            if let (Some(if_id), Some(channel)) =
                (self.scan.active_interface(), self.scan.active_channel())
            {
                if self.transport.publication_available() {
                    if let Some(indication) = self.radio.poll_scan_indication(if_id, channel) {
                        self.transport.publish_radio(indication);
                    }
                }
            } else if let Some(if_id) = vif::active_interface() {
                let channel = vif::snapshot(if_id).map(|state| state.channel).unwrap_or(0);
                let publication_available = self.transport.publication_available();
                if publication_available
                    && let Some(indication) = self.radio.poll_joined_indication(
                        if_id,
                        channel,
                        &self.tx_engine.ba,
                        &mut self.beacons,
                        &mut self.power_save,
                        self.crypto,
                    )
                {
                    self.transport.publish_radio(indication);
                }
            } else {
                // Vendor RX processing never stops between scans. Recycle one
                // slot per cooperative pass so a later dwell cannot consume
                // idle-era beacons as if they had just arrived.
                let channel = crate::mac::current_channel();
                self.radio.discard_one_idle(channel);
            }
        }
    }

    fn service_command(&mut self) {
        // Vendor `hif_rx_process()` dispatches exactly one completed request
        // per invocation, then reschedules itself if another descriptor is
        // already ready. Keep that cooperative boundary instead of batching
        // request execution in one main-loop pass. Class 0 and class 6 also
        // converge on one vendor scheduler, so leave the host descriptor owned
        // by the transport while class-0 hardware publication is active rather
        // than entering the independent management publisher.
        // Answering host requests must not depend on a data frame completing.
        // `management_runtime_available` is false while any class-0 frame is in
        // flight, so gating the poll on it starves the command lane under
        // continuous TX and forever if a frame sticks: the host stops getting
        // replies and declares `[BH] Fatal error` long before the 5 x 200 ms
        // pipe watchdog can recover the slot. Vendor has no such coupling,
        // which is why it can discard unmatched statuses and lean on the
        // watchdog. The management publisher this gate was protecting is
        // already gated separately where it is serviced.
        crate::command::service_one(self);
    }
}
