//! WSM command dispatch for the cooperative firmware reactor.
//!
//! One call services at most one detached host request. Publication and request
//! ownership remain in the same lane so synchronous commands return their token
//! exactly once while admitted class-0 TX transfers it to `HostTxDriver`.

use crate::configuration::Configuration;
use crate::hif::ReceivedRequest;
use crate::join;
use crate::packet_ram;
use crate::platform::{self, program_station_address};
use crate::rate_policy;
use crate::scan;
use crate::tx;
use crate::vif;
use crate::wsm::{
    ConfigurationRequest, Request, STATUS_FAILURE, TxPowerRange, TxRequest,
    encode_configuration_response, encode_join_response, encode_read_mib_data_response,
    encode_read_mib_response, encode_status_response, encode_tx_confirm, encode_xr819_tx_confirm,
};

use crate::key_change;

/// Suppress new autonomous management publications while a key barrier drains.
pub(crate) fn key_change_pending(firmware: &crate::firmware::Firmware) -> bool {
    firmware.held_request.as_ref().is_some_and(|(request, _)| key_change::is_key_change(request.id))
}

pub fn encode_debug_event(event_id: u32, data: u32, output: &mut [u8]) -> Option<usize> {
    if output.len() < 12 {
        return None;
    }
    output[..2].copy_from_slice(&12_u16.to_le_bytes());
    output[2..4].copy_from_slice(&0x0805_u16.to_le_bytes());
    output[4..8].copy_from_slice(&event_id.to_le_bytes());
    output[8..12].copy_from_slice(&data.to_le_bytes());
    Some(12)
}

fn service_management_request(
    events: &mut tx::TxEngine,
    crypto: &mut crate::crypto::Crypto,
    request: &TxRequest<'_>,
    if_id: u8,
    output: &mut [u8],
    publish_response: &mut bool,
) -> Result<usize, crate::wsm::Error> {
    let owner = tx::ManagementOwner::Host { packet_id: request.packet_id };
    match tx::service_host_management_tx(events, crypto, Some((request, if_id, owner)), 0) {
        tx::HostManagementTxReport::Published { .. } => {
            *publish_response = true;
            let mac = |offset| crate::mmio::MAC.read32(platform::mac_register(offset));
            let (edca, quantum0, quantum1) = (mac(0x0e64), mac(0x0e70), mac(0x0e74));
            let byte = |address| crate::mmio::mac_objects(address).read8(address);
            let metadata = byte(packet_ram::interface_metadata());
            let secondary = byte(packet_ram::duration_word(0));
            let event_id = 0x5852_0000 | (edca & 0xffff);
            let data = (quantum0 & 0xff)
                | ((quantum1 & 0xff) << 8)
                | (u32::from(metadata) << 16)
                | (u32::from(secondary) << 24);
            encode_debug_event(event_id, data, output).ok_or(crate::wsm::Error::Truncated)
        }
        tx::HostManagementTxReport::Failed { packet_id, .. } => {
            encode_xr819_tx_confirm(packet_id, STATUS_FAILURE, output)
        }
        _ => {
            *publish_response = false;
            encode_tx_confirm(0, STATUS_FAILURE, output)
        }
    }
}

/// MIB 0x100c, the counters table, is the only readable MIB. A failed
/// hardware CCMP self-test (word 1 not 1) answers in its place so the
/// failure stays visible to the host.
fn encode_read_mib(
    mib_id: u16,
    crypto: &crate::crypto::Crypto,
    output: &mut [u8],
) -> Result<usize, crate::wsm::Error> {
    if mib_id != 0x100c {
        return encode_read_mib_response(STATUS_FAILURE, mib_id, output);
    }
    let values = crypto.failed_self_test().unwrap_or_else(crate::counters::snapshot);
    let mut data = [0_u8; crate::counters::WORDS * 4];
    for (chunk, value) in data.chunks_exact_mut(4).zip(values) {
        chunk.copy_from_slice(&value.to_le_bytes());
    }
    encode_read_mib_data_response(0, mib_id, &data, output)
}

fn retain_configuration(
    configuration: &mut Configuration,
    request: ConfigurationRequest<'_>,
) -> Option<([u8; 6], [TxPowerRange; 2])> {
    configuration.retain(&request).ok()?;
    Some((configuration.station_id()?, configuration.tx_power_ranges()?))
}

/// Hand the association parameters the host changed to their users.
fn publish_association(configuration: &Configuration, events: &mut tx::TxEngine, radio: &mut crate::radio::Radio) {
    events.mpdu_start_spacing = configuration.mpdu_start_spacing();
    events.block_ack_tx_tids = configuration.block_ack_tx_tids();
    events.protection = configuration.tx_protection();
    radio.set_block_ack_tids(configuration.block_ack_rx_tids());
    crate::phy::set_tx_power(configuration.current_tx_power_tenths_dbm());
}

/// Apply a decoded MIB write. Request buffers and reply publication remain
/// exclusively in the dispatcher.
fn write_mib(
    req: crate::wsm::WriteMibRequest<'_>, interface: u8, configuration: &mut Configuration,
    events: &mut tx::TxEngine, radio: &mut crate::radio::Radio,
    beacons: &mut crate::beacon_loss::BeaconMonitor, ps: &mut crate::power_save::StationPowerSave,
) -> u32 {
    use crate::wsm::*;
    let data = req.data;
    let ok = match req.mib_id {
        0x1006 if data.first().is_some_and(|mode| mode & 0x0f == 0) => true,
        MIB_ID_BEACON_FILTER_TABLE => beacons.filter.set_table(data),
        MIB_ID_BEACON_FILTER_ENABLE => beacons.filter.set_control(data),
        MIB_ID_RX_FILTER => beacons.filter.set_rx_filter(data),
        MIB_ID_PEER_HT => {
            if let Some(vif) = vif::vif(interface) { vif.set_peer_ht(data); }
            true
        }
        MIB_ID_DISABLE_BSSID_FILTER => beacons.filter.set_bssid_filter(data),
        MIB_ID_RCPI_RSSI_THRESHOLD => beacons.signal.configure(data),
        0x1007 => ps.set_beacon_period(data),
        rate_policy::MIB_ID_SET_TX_RATE_RETRY_POLICY => events.rate_policies().install(data).is_ok(),
        id if configuration.retain_protection_mib(interface, id, data)
            || configuration.retain_interface_mib(id, data) => {
            publish_association(configuration, events, radio);
            true
        }
        _ => false,
    };
    if ok { 0 } else { STATUS_FAILURE }
}

/// Indications a command owes the host after its confirmation.
#[derive(Debug, Default)]
pub struct PendingIndications {
    /// JOIN with the "join complete" flag: its status.
    pub join_complete: Option<u32>,
    /// SWITCH_CHANNEL: the host holds TX until this indication.
    pub channel_switch: bool,
    /// SET_PM: the power mode in force (always active).
    pub set_pm_complete: bool,
}

/// How long a RESET or a management frame waits for the frames in flight
/// to finish.
const DRAIN_US: u32 = 1_000_000;

/// Whether `request` must wait for the host frames in flight. RESET tears
/// the interface down, which waits for them (the station's last
/// deauthentication, say); failing it instead deadlocks cw1200 while it is
/// an access point or joined. A management, EAPOL or null frame needs the
/// publication the data frames hold: failed at once, a group rekey under
/// steady traffic lost all four of its tries, and hostapd deauthenticated
/// the station. Holding the request also stops new data behind it, so the
/// frames drain; the pipe watchdog recovers a stuck one well within the
/// bound.
fn waits_for_drain(firmware: &crate::firmware::Firmware, request: &ReceivedRequest) -> bool {
    match request.id {
        crate::wsm::RESET_REQ_ID => request.link_id == 0 && !crate::tx::probe_runtime_quiescent(firmware.tx_engine),
        crate::wsm::TX_REQ_ID => {
            !firmware.host_tx_driver.management_runtime_available()
                && crate::wsm::TxRequest::parse(request.buffer.payload()).is_ok_and(|tx| !tx.takes_data_path())
        }
        _ => false,
    }
}

/// Service at most one host request from the command lane.
///
/// The caller must not re-enter this lane before the call returns.
pub fn service_one(firmware: &mut crate::firmware::Firmware) {
    // A held request bypasses poll_request's response-storage check. Leave
    // its owner in place until both queue space and a reply buffer are free.
    if !firmware.transport.output_available() {
        return;
    }
    let now = crate::platform::timer_now();
    let Some((request, since)) = firmware
        .held_request
        .take()
        .or_else(|| firmware.transport.poll_request().map(|request| (request, now)))
    else {
        return;
    };
    let key_status = if let Some(change) = key_change::KeyChange::parse(request.id, request.buffer.payload()) {
        // Confirming owners cannot emit again. RX jobs do not hold this gate:
        // their captured epochs already prevent publication after revocation.
        let quiet = firmware.host_tx_driver.key_change_drained()
            && tx::probe_runtime_quiescent(firmware.tx_engine);
        match change.progress(firmware.crypto, quiet, since, now) {
            key_change::Progress::Waiting => {
                firmware.held_request = Some((request, since));
                return;
            }
            key_change::Progress::Complete(status) => Some(status),
        }
    } else {
        if waits_for_drain(firmware, &request) && now.wrapping_sub(since) < DRAIN_US {
            firmware.held_request = Some((request, since));
            return;
        }
        None
    };
    dispatch_single_request(firmware, request, key_status);
}

/// Dispatch one request, transferring or returning its buffer exactly once.
fn dispatch_single_request(
    firmware: &mut crate::firmware::Firmware,
    request: ReceivedRequest,
    key_status: Option<u32>,
) {
    let crate::firmware::Firmware {
        transport,
        tx_engine: events,
        host_tx_driver,
        response_scratch,
        pending_indications: pending,
        beacons,
        ap,
        crypto,
        radio,
        scan,
        configuration,
        power_save,
        ..
    } = firmware;
    let output: &mut [u8] = &mut response_scratch[..];
    let mut publish_response = true;
    let request_id = request.id;
    // The host driver's only interface.
    let request_if_id = 0;
    let request_link_id = request.link_id;
    let mut request_buffer = Some(request.buffer);
    let request_payload = request_buffer
        .as_ref()
        .expect("request buffer is present")
        .payload();
    let status_response = |status: u32, output: &mut [u8]| encode_status_response(request_id | 0x0400, status, output);
    let response_length = {
        match Request::parse(request_id, request_payload) {
            Request::Configuration(request) => {
                let configured = request.ok().and_then(|request| retain_configuration(configuration, request));
                let (station_id, tx_power_ranges) = configured.unwrap_or((
                    [0; 6],
                    [
                        TxPowerRange {
                            min_power_level: -160,
                            max_power_level: 200,
                            stepping: 0,
                        },
                        TxPowerRange {
                            min_power_level: -160,
                            max_power_level: 200,
                            stepping: 0,
                        },
                    ],
                ));
                program_station_address(station_id);
                events.protection = configuration.tx_protection();
                encode_configuration_response(station_id, tx_power_ranges, output)
            }
            Request::StartScan(request) => {
                let status = match request.map(|request| scan.begin(&request, request_if_id)) {
                    Ok(Ok(())) => {
                        power_save.invalidate_beacon();
                        0
                    },
                    Ok(Err(scan::ScanError::Busy)) => 4,
                    _ => 2,
                };
                status_response(status, output)
            }
            Request::SetBssParams(parameters) => {
                // JOIN already publishes the active VIF, channel, and rate
                // state used by this firmware; what is left is the
                // beacon-loss count.
                let now = crate::platform::timer_now();
                let status = parameters
                    .map(|parameters| {
                        power_save.set_aid(parameters.aid);
                        beacons.set_lost_count(parameters.beacon_lost_count, now)
                    })
                    .map(|()| 0)
                    .unwrap_or(STATUS_FAILURE);
                status_response(status, output)
            }
            Request::TxQueueParams(parameters) => {
                let status = parameters.map(|parameters| configuration.retain_tx_queue(parameters)).map(|()| 0).unwrap_or(2);
                status_response(status, output)
            }
            Request::EdcaParams(parameters) => {
                let status = match parameters {
                    Ok(parameters) => {
                        configuration.retain_edca(parameters);
                        vif::apply_edca(request_if_id, parameters).map(|_| 0).unwrap_or(2)
                    }
                    Err(_) => 2,
                };
                status_response(status, output)
            }
            Request::WriteMib(request) => {
                let status = request.map_or(STATUS_FAILURE, |req| {
                    write_mib(req, request_if_id, configuration, events, radio, beacons, power_save)
                });
                status_response(status, output)
            }
            Request::ReadMib(mib_id) => encode_read_mib(mib_id.unwrap_or(0), crypto, output),
            // Only the bounded key gate above may mutate the key table.
            Request::AddKey(_) | Request::RemoveKey(_) => {
                status_response(key_status.unwrap_or(STATUS_FAILURE), output)
            }
            Request::Reset(request) => {
                let status = match request {
                    // One station's link: forget the station and its sessions.
                    Ok(_) if request_link_id != 0 => {
                        if let Some(peer) = vif::unmap_link(request_if_id, request_link_id) {
                            events.ba.link(request_link_id).reset();
                            radio.end_peer_block_ack(peer);
                        }
                        0
                    }
                    Ok(_) => {
                        ap.stop();
                        power_save.reset();
                        host_tx_driver.reset(crypto);
                        beacons.reset();
                        if join::reset(events, crypto, radio, request_if_id) { 0 } else { STATUS_FAILURE }
                    }
                    Err(_) => STATUS_FAILURE,
                };
                status_response(status, output)
            }
            Request::StopScan => {
                scan.request_stop();
                status_response(0, output)
            }
            Request::SetPm(request) => {
                // A failed SET_PM would deadlock cw1200 while joined; a
                // malformed one keeps the current mode.
                if let Ok((mode, idle)) = request {
                    power_save.set_mode(crate::power_save::Mode::from_set_pm(mode, idle), crate::platform::timer_now());
                }
                pending.set_pm_complete = true;
                status_response(0, output)
            }
            // Beacon and probe-response elements: the TIM, ERP information.
            // An element that does not fit leaves the template as it was; a
            // failed confirmation would deadlock cw1200 (it is an access point).
            Request::UpdateIe(request) => {
                let status = request.map_or(STATUS_FAILURE, |request| {
                    configuration.update_template_elements(request.what, request.count, request.elements);
                    0
                });
                status_response(status, output)
            }
            Request::SwitchChannel(request) => {
                // Accepted whatever the state, like the vendor's. JOIN and
                // START tune the radio, so the switch is acknowledged without
                // retuning: a joined interface keeps its channel. mac80211
                // asks for its default channel when an access point stops,
                // before the RESET that ends it, and cw1200 deadlocks on a
                // failed confirmation while it is an access point or joined.
                let status = request.map_or(STATUS_FAILURE, |_| {
                    pending.channel_switch = true;
                    0
                });
                status_response(status, output)
            }
            Request::Join(request) => {
                power_save.reset();
                let status = match request {
                    Ok(join_request) => {
                        join::activate_sta(
                            events,
                            crypto,
                            radio,
                            scan.active_interface().is_some(),
                            configuration.station_id(),
                            request_if_id,
                            &join_request,
                        )
                            .map(|_| beacons.join(join_request.beacon_interval))
                            .map(|()| 0)
                            .unwrap_or(STATUS_FAILURE)
                    },
                    Err(_) => STATUS_FAILURE,
                };
                if status == 0 && request.is_ok_and(|request| request.wants_complete_indication()) {
                    pending.join_complete = Some(0);
                }
                encode_join_response(status, -160, 200, output)
            }
            Request::Tx(Ok(tx_request)) => {
                // A BlockAckReq is a control frame: the management publisher
                // transmits it as a single frame (with the four FCS octets the
                // MAC requires) and its class-6 completion returns the host
                // packet id, which hif_startup turns into the WSM confirmation
                // mac80211 waits on. The host TX path batches into A-MPDUs and
                // never completes a lone control frame.
                // Group data goes the same way; it expects no ACK.
                if tx_request.takes_data_path() {
                    let packet_id = tx_request.packet_id;
                    let admitted = host_tx_driver.admit(
                        request_buffer.take().expect("request buffer is present"),
                        request_if_id,
                        request_link_id,
                        transport,
                        events,
                    );
                    if admitted {
                        // Its PM bit is clear: the AP takes us for awake.
                        power_save.on_traffic(crate::platform::timer_now());
                        publish_response = false;
                        Ok(0)
                    } else {
                        encode_xr819_tx_confirm(packet_id, STATUS_FAILURE, output)
                    }
                } else if !host_tx_driver.management_runtime_available() {
                    encode_xr819_tx_confirm(tx_request.packet_id, STATUS_FAILURE, output)
                } else {
                    if let Some(ap) = vif::link_peer(request_if_id, 0) {
                        power_save.on_association_request(tx_request.frame, ap);
                    }
                    // EAPOL, management, a null: its PM bit clear wakes the AP.
                    if tx_request.is_unicast() {
                        power_save.on_traffic(crate::platform::timer_now());
                    }
                    service_management_request(events, crypto, &tx_request, request_if_id, output, &mut publish_response)
                }
            }
            Request::Tx(Err(_)) => {
                let packet_id = request_payload
                    .get(..4)
                    .map(|value| u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
                    .unwrap_or(0);
                encode_xr819_tx_confirm(packet_id, STATUS_FAILURE, output)
            }
            // Do not report success for commands whose state effects are not
            // implemented. A complete status word lets cw1200 fail the
            // command cleanly instead of proceeding on false assumptions.
            Request::Start(request) => {
                let status = request
                    .ok()
                    .and_then(|request| {
                        ap.start(
                            events,
                            crypto,
                            radio,
                            scan.active_interface().is_some(),
                            configuration.station_id(),
                            request_if_id,
                            &request,
                        )
                        .ok()
                    })
                    .map_or(STATUS_FAILURE, |()| 0);
                status_response(status, output)
            }
            Request::BeaconTransmit(enable) => {
                let status = enable.ok().and_then(|enable| ap.set_beaconing(enable).ok()).map_or(STATUS_FAILURE, |()| 0);
                status_response(status, output)
            }
            // The station's link: its sequence numbers and BlockAck sessions.
            Request::MapLink(station) => {
                let status = station.map_or(STATUS_FAILURE, |station| {
                    if let Some(previous) = vif::link_peer(request_if_id, request_link_id).filter(|&peer| peer != station)
                        && request_link_id != 0
                    {
                        events.ba.link(request_link_id).reset();
                        radio.end_peer_block_ack(previous);
                    }
                    vif::map_link(request_if_id, request_link_id, station);
                    0
                });
                status_response(status, output)
            }
            Request::Unsupported(_) => status_response(STATUS_FAILURE, output),
        }
    };

    if publish_response && let Ok(length) = response_length {
        let response_id = u16::from_le_bytes([output[2], output[3]]) & 0x1fff;
        if response_id & 0x0400 != 0
            && let Some(buffer) = request_buffer.take()
        {
            transport.publish_request_in_place(
                buffer.into_release(),
                &output[..length],
                length as u16,
            );
        } else {
            transport.publish_with(|indication| {
                indication[..length].copy_from_slice(&output[..length]);
                Some(length)
            });
        }
    }

    // Synchronous commands return their owning request buffer here. Ordinary
    // class-0 TX moves it into HostTxDriver until confirmation.
    if let Some(buffer) = request_buffer {
        transport.release_request(buffer.into_release());
    }
}
