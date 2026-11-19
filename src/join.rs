//! Minimal STA JOIN activation boundary.
//!
//! Ordinary WSM TX carries the authentication and association frames Linux
//! sends after JOIN success.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinError {
    Unsupported,
    NotConfigured,
    ScanActive,
    VifState,
    Channel,
}

/// Activate the minimal vendor STA-BSS state and program its operating channel.
///
/// JOIN must exclusively own channel, VIF/PAS, and MAC address-match state.
#[cfg(target_arch = "arm")]
pub fn activate_sta(
    engine: &crate::tx::TxEngine,
    crypto: &mut crate::crypto::Crypto,
    radio: &mut crate::radio::Radio,
    scanning: bool,
    station_id: Option<[u8; 6]>,
    interface: u8,
    request: &crate::wsm::JoinRequest<'_>,
) -> Result<(), JoinError> {
    engine.ba.reset();
    radio.reset_block_ack(crypto);
    if interface > 1 || !request.supported_sta_shape() {
        return Err(JoinError::Unsupported);
    }
    if scanning {
        return Err(JoinError::ScanActive);
    }
    if !crate::tx::probe_runtime_quiescent(engine) {
        return Err(JoinError::VifState);
    }
    let station_id = station_id.ok_or(JoinError::NotConfigured)?;
    crate::vif::join_gate(interface, request.channel_number).map_err(|_| JoinError::VifState)?;

    let _ = crate::vif::teardown(interface);
    if crate::phy::run_channel_transition(request.channel_number, 100_000).is_err() {
        let _ = crate::vif::teardown(interface);
        return Err(JoinError::Channel);
    }
    crate::vif::activate_sta(interface, request, station_id).map_err(|_| JoinError::VifState)?;
    if !program_joined_mac(interface, request.bssid) {
        let _ = crate::vif::teardown(interface);
        return Err(JoinError::VifState);
    }
    Ok(())
}

/// START: an access point on `interface`, programmed as a joined station
/// is but with no BSSID entry: it receives what is sent to its own address.
#[cfg(target_arch = "arm")]
pub fn activate_ap(
    engine: &crate::tx::TxEngine,
    crypto: &mut crate::crypto::Crypto,
    radio: &mut crate::radio::Radio,
    scanning: bool,
    station_id: Option<[u8; 6]>,
    interface: u8,
    request: &crate::wsm::StartRequest<'_>,
) -> Result<(), JoinError> {
    engine.ba.reset();
    radio.reset_block_ack(crypto);
    if interface > 1 || request.mode != 0 || request.band != 0 {
        return Err(JoinError::Unsupported);
    }
    if scanning {
        return Err(JoinError::ScanActive);
    }
    if !crate::tx::probe_runtime_quiescent(engine) {
        return Err(JoinError::VifState);
    }
    let own_mac = station_id.ok_or(JoinError::NotConfigured)?;
    crate::vif::join_gate(interface, request.channel_number).map_err(|_| JoinError::VifState)?;
    let _ = crate::vif::teardown(interface);
    if crate::phy::run_channel_transition(request.channel_number, 100_000).is_err() {
        let _ = crate::vif::teardown(interface);
        return Err(JoinError::Channel);
    }
    crate::vif::activate_ap(interface, request, own_mac).map_err(|_| JoinError::VifState)?;
    crate::mac::program_joined_station_slot_timing(interface);
    if !crate::mac::program_active_vif_rate_tables(interface) {
        let _ = crate::vif::teardown(interface);
        return Err(JoinError::VifState);
    }
    crate::mac::program_immediate_response_descriptors();
    crate::mac::clear_bssid();
    crate::mac::program_joined_station_mode();
    Ok(())
}

/// The MAC state of a joined station, after the channel transition that
/// JOIN (or a scan's return to the operating channel) ran in scan mode.
///
/// The caller must exclusively own the MAC, VIF/PAS and address-match state.
#[cfg(target_arch = "arm")]
fn program_joined_mac(interface: u8, bssid: [u8; 6]) -> bool {
    crate::mac::program_joined_station_slot_timing(interface);
    if !crate::mac::program_active_vif_rate_tables(interface) {
        return false;
    }
    crate::mac::program_immediate_response_descriptors();
    crate::mac::program_joined_bssid(bssid);
    // Vendor STA mode installs the receive/address-match state required for
    // authentication and association responses after BSSID publication.
    crate::mac::program_joined_station_mode();
    true
}

/// Back in the joined state after a scan has returned to the operating
/// channel: the scan's channel transitions left the synthetic scan PAS
/// active and the MAC in scan station mode, whose receive filter drops the
/// AP's BlockAcks.
///
/// As for [`program_joined_mac`]; the operating channel must be tuned.
#[cfg(target_arch = "arm")]
pub fn return_from_scan(interface: u8) -> bool {
    let Some(state) = crate::vif::snapshot(interface) else {
        return false;
    };
    crate::mac::low_mac().pas(2_usize).activity.set(0);
    program_joined_mac(interface, state.bssid)
}

#[cfg(target_arch = "arm")]
pub fn reset(
    engine: &crate::tx::TxEngine,
    crypto: &mut crate::crypto::Crypto,
    radio: &mut crate::radio::Radio,
    interface: u8,
) -> bool {
    engine.ba.reset();
    radio.reset_block_ack(crypto);
    if !crate::vif::is_active(interface) {
        return interface < 2;
    }
    if !crate::tx::probe_runtime_quiescent(engine) {
        return false;
    }
    if !crate::vif::teardown(interface) {
        return false;
    }
    if crate::vif::any_active() {
        return true;
    }
    // Preserve the running RX/channel owner until the next scan or JOIN. The
    // full vendor stop depends on ordinary-TX scheduler drain work that is not
    // represented yet; forcing it here made the post-auth scan lose completion.
    crate::mmio::MAC.write32(crate::platform::mac_register(0x0044), 0);
    crate::mac::program_scan_station_mode();
    true
}
