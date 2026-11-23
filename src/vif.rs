//! The three virtual interfaces: two operating interfaces (0 and 1) and the
//! synthetic record 2 that scanning uses.

use crate::global::Global;
use core::cell::Cell;

pub const VIF_COUNT: usize = 3;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VifState {
    pub mode: u8,
    pub active: bool,
    pub control_bits: u32,
    pub basic_rates: u32,
    pub bssid: [u8; 6],
    pub channel: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JoinStateError {
    InvalidInterface,
    Busy,
    ChannelConflict,
}

/// Links with a peer: a station's AP on link 0, an access point's stations
/// on the host links the driver maps them to (cw1200: 1 to 5).
pub const LINKS: usize = 6;

/// Negotiated receive limits of one HT station. Unknown peers use the
/// smallest standard HT aggregate and the longest standard MPDU spacing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HtLimits {
    pub density: u8,
    pub max_bytes: u16,
}
impl HtLimits {
    const CONSERVATIVE: Self = Self {
        density: 7,
        max_bytes: 8191,
    };
}
type Station = ([u8; 6], HtLimits);

/// One interface.
pub(crate) struct Vif {
    /// 1 while a JOIN prepares the record, 0 when down.
    pub(crate) mode: Cell<u8>,
    /// Nonzero while the interface operates (2 for the synthetic record).
    pub(crate) active: Cell<u8>,
    /// Vendor control bits: bit 0 plus the JOIN probe selector.
    pub(crate) flags: Cell<u32>,
    /// Vendor rate configuration: byte 2 band, 3 preamble, 4 and 5 lowest
    /// basic rate, 6 alternate retry limit selector, 7 low-rate recovery
    /// count.
    pub(crate) rate_bytes: [Cell<u8>; 8],
    /// Low-rate retry limits (never configured, so 0).
    pub(crate) default_rates: [Cell<u8>; 2],
    pub(crate) basic_rates: Cell<u32>,
    pub(crate) allowed_links: Cell<u16>,
    pub(crate) effective_links: Cell<u16>,
    pub(crate) sleeping_links: Cell<u16>,
    pub(crate) buffered_links: Cell<u16>,
    pub(crate) awake_links: Cell<u16>,
    pub(crate) own_mac: [Cell<u8>; 6],
    pub(crate) bssid: [Cell<u8>; 6],
    pub(crate) channel: Cell<u16>,
    /// 3 once JOIN publishes the operating record.
    pub(crate) activity_state: Cell<u8>,
    /// Link whose per-TID sequence counters host QoS data uses.
    pub(crate) internal_link: Cell<u16>,
    /// An access point's stations by host link (MAP_LINK).
    stations: [Cell<Option<Station>>; LINKS],
}

impl Vif {
    const fn new() -> Self {
        Self {
            mode: Cell::new(0),
            active: Cell::new(0),
            flags: Cell::new(0),
            rate_bytes: [const { Cell::new(0) }; 8],
            default_rates: [const { Cell::new(0) }; 2],
            basic_rates: Cell::new(0),
            allowed_links: Cell::new(0),
            effective_links: Cell::new(0),
            sleeping_links: Cell::new(0),
            buffered_links: Cell::new(0),
            awake_links: Cell::new(0),
            own_mac: [const { Cell::new(0) }; 6],
            bssid: [const { Cell::new(0) }; 6],
            channel: Cell::new(0),
            activity_state: Cell::new(0),
            internal_link: Cell::new(0),
            stations: [const { Cell::new(None) }; LINKS],
        }
    }

    fn is_access_point(&self) -> bool {
        self.mode.get() == Role::AccessPoint as u8
    }

    fn map_station(&self, link: u8, peer: [u8; 6]) -> bool {
        let station = self
            .stations
            .get(usize::from(link))
            .filter(|_| link != 0 && self.is_access_point());
        let Some(station) = station else { return false };
        if station.get().is_none_or(|(old, _)| old != peer) {
            station.set(Some((peer, HtLimits::CONSERVATIVE)));
        }
        true
    }

    fn link_peer(&self, link: u8) -> Option<[u8; 6]> {
        if self.is_access_point() {
            Some(self.stations.get(usize::from(link))?.get()?.0)
        } else {
            (link == 0).then(|| self.bssid.each_ref().map(Cell::get))
        }
    }

    pub(crate) fn set_peer_ht(&self, data: &[u8]) {
        let [link, factor @ 0..=3, density @ 0..=7, peer @ ..] = data else {
            return;
        };
        let Some(entry) = self.stations.get(usize::from(*link)) else {
            return;
        };
        if let Some((saved, _)) = entry.get()
            && peer == saved
        {
            entry.set(Some((
                saved,
                HtLimits {
                    density: *density,
                    max_bytes: ((8192u32 << factor) - 1) as u16,
                },
            )));
        }
    }

    pub(crate) fn ht_limits(&self, link: u8, station_density: u8) -> HtLimits {
        if self.is_access_point() {
            self.stations
                .get(usize::from(link))
                .and_then(Cell::get)
                .map_or(HtLimits::CONSERVATIVE, |(_, limits)| limits)
        } else {
            HtLimits {
                density: station_density,
                max_bytes: u16::MAX,
            }
        }
    }

    fn peer_link(&self, peer: [u8; 6]) -> Option<u8> {
        (0..LINKS as u8).find(|&link| self.link_peer(link) == Some(peer))
    }

    /// Links the pending task may release frames on: allowed links that are
    /// awake or have buffered traffic.
    pub(crate) fn recompute_effective_links(&self) -> u16 {
        let effective = (self.allowed_links.get()
            & (!self.sleeping_links.get() | self.buffered_links.get()))
            | self.awake_links.get();
        self.effective_links.set(effective);
        effective
    }
}

pub(crate) struct Vifs {
    records: [Vif; VIF_COUNT],
    /// Interface that owns the radio, if any.
    radio_owner: Cell<Option<u8>>,
}

static VIFS: Global<Vifs> = Global::new(Vifs {
    records: [const { Vif::new() }; VIF_COUNT],
    radio_owner: Cell::new(None),
});

/// Interface `interface`, or `None` beyond the three records.
pub(crate) fn vif(interface: u8) -> Option<&'static Vif> {
    VIFS.records.get(usize::from(interface))
}

pub const fn is_operating_interface(interface: u8) -> bool {
    interface < 2
}

pub fn any_active() -> bool {
    active_interface().is_some()
}

pub fn active_interface() -> Option<u8> {
    (0..2).find(|&interface| is_active(interface))
}

pub fn is_active(interface: u8) -> bool {
    is_operating_interface(interface) && vif(interface).is_some_and(|vif| vif.active.get() != 0)
}

pub(crate) fn internal_link(interface: u8) -> Option<u16> {
    vif(interface).map(|vif| vif.internal_link.get())
}

/// MAP_LINK: an access point's host `link` now names the station `peer`.
pub fn map_link(interface: u8, link: u8, peer: [u8; 6]) -> bool {
    vif(interface).is_some_and(|vif| vif.map_station(link, peer))
}

/// RESET of a station's link: returns the station it named.
pub fn unmap_link(interface: u8, link: u8) -> Option<[u8; 6]> {
    Some(vif(interface)?.stations.get(usize::from(link))?.take()?.0)
}

/// The peer on `link`: a station's AP on link 0, or the station an access
/// point mapped there.
pub fn link_peer(interface: u8, link: u8) -> Option<[u8; 6]> {
    vif(interface)?.link_peer(link)
}

/// The link `peer` is on.
pub fn peer_link(interface: u8, peer: [u8; 6]) -> Option<u8> {
    vif(interface)?.peer_link(peer)
}

pub(crate) fn own_mac_word(interface: u8, word: usize) -> Option<u16> {
    let vif = vif(interface)?;
    let low = vif.own_mac.get(word * 2)?.get();
    let high = vif.own_mac.get(word * 2 + 1)?.get();
    Some(u16::from_le_bytes([low, high]))
}

pub(crate) fn rate_byte(interface: u8, index: usize) -> Option<u8> {
    vif(interface)?.rate_bytes.get(index).map(Cell::get)
}

pub(crate) fn default_rate(interface: u8, index: usize) -> Option<u8> {
    vif(interface)?.default_rates.get(index).map(Cell::get)
}

/// Scan publishes the synthetic record as active.
pub(crate) fn publish_synthetic_scan_record() {
    let record = &VIFS.records[2];
    record.active.set(2);
    record.flags.set(0x4000);
}

pub(crate) fn set_all_own_mac_byte(index: usize, value: u8) {
    VIFS.records
        .iter()
        .for_each(|vif| vif.own_mac[index].set(value));
}

/// Record the vendor rate-configuration word (bytes 0..3).
fn set_rate_configuration(vif: &Vif, value: u32) {
    for (byte, value) in vif.rate_bytes.iter().zip(value.to_le_bytes()) {
        byte.set(value);
    }
}

#[cfg(target_arch = "arm")]
fn reset_pas_backoff(interface: u8) -> Result<(), JoinStateError> {
    if interface >= VIF_COUNT as u8 {
        return Err(JoinStateError::InvalidInterface);
    }
    crate::backoff::reset_all_queues(crate::backoff::banks(), interface)
        .map_err(|_| JoinStateError::InvalidInterface)
}

/// What an operating interface is (vendor VIF role).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Role {
    Station = 1,
    AccessPoint = 4,
}

/// What JOIN or START sets up an interface record with.
struct RecordParameters {
    role: Role,
    band: u8,
    preamble_type: u8,
    /// 0: the 1, 2 and 5.5 Mbit/s defaults.
    basic_rates: u32,
    control_bits: u32,
    bssid: [u8; 6],
    channel: u16,
}

fn publish_join_pas(
    low_mac: &crate::mac::LowMac,
    interface: u8,
    role: Role,
    basic_rate_bits: u32,
    own_mac: &[u8; 6],
) {
    let pas = low_mac.pas(interface);
    low_mac.pas(2_usize).activity.set(0);
    pas.activity.set(2);
    pas.slot_bits.set(4);
    pas.mode.set(role as u8);
    pas.own_mac[5].set(low_mac.own_mac[0][5].get());
    pas.path_selector.set(0);
    pas.basic_rates.set(basic_rate_bits);
    pas.slot_timing.set(9);
    let _ = crate::backoff::reset_all_queues(&low_mac.pas, interface);
    for (byte, value) in pas.own_mac.iter().zip(own_mac) {
        byte.set(*value);
    }
}

fn publish_teardown_pas(low_mac: &crate::mac::LowMac, interface: u8) {
    let pas = low_mac.pas(interface);
    pas.activity.set(1);
    pas.mode.set(0);
    pas.path_selector.set(0);
}

/// Apply WSM EDCA through vendor `edca_apply_params` (`0x136a6`).
///
/// The MAC EDCA registers must be exclusively owned.
#[cfg(target_arch = "arm")]
pub fn apply_edca(
    interface: u8,
    parameters: crate::wsm::EdcaParameters,
) -> Result<(), JoinStateError> {
    if interface >= VIF_COUNT as u8 {
        return Err(JoinStateError::InvalidInterface);
    }
    let low_mac = crate::mac::low_mac();
    let pas = low_mac.pas(interface);
    let wire = [
        parameters.queues[3],
        parameters.queues[2],
        parameters.queues[1],
        parameters.queues[0],
    ];
    for (queue, entry) in wire.into_iter().enumerate() {
        pas.cw_min[queue].set(entry.cwmin);
        pas.cw_max[queue].set(entry.cwmax);
        pas.txop_limit[queue].set(entry.txop_limit);
    }
    // Vendor copies the WSM payload verbatim into the PAS record; the host
    // wire order is params[3], [2], [1], [0]. `edca_apply_params`
    // (annotated-main.c:24040-24043) then packs q2, q0, q1, q3 in that order.
    // Keep this expressed in wire indices so it remains visibly identical to
    // the vendor formula.
    let aifs = u32::from(wire[1].aifns)
        .wrapping_add(u32::from(wire[3].aifns.wrapping_sub(1)) << 12)
        .wrapping_add(u32::from(wire[2].aifns) << 8)
        .wrapping_add(u32::from(wire[0].aifns) << 4)
        .wrapping_sub(0x111);
    pas.packed_aifs.set(aifs);

    if low_mac.edca_slot_timing.get() != aifs {
        low_mac.edca_slot_timing.set(aifs);
        crate::mmio::MAC.write32(crate::platform::mac_register(0x0e64), aifs);
        reset_pas_backoff(interface)?;
    }
    Ok(())
}

fn prepare_record(vif: &Vif, parameters: &RecordParameters, own_mac: [u8; 6], basic_rates: u32) {
    let lowest_rate = basic_rates.trailing_zeros().min(6) as u8;
    vif.mode.set(parameters.role as u8);
    vif.flags.set(parameters.control_bits);
    set_rate_configuration(vif, 0x117);
    vif.rate_bytes[2].set(parameters.band);
    vif.rate_bytes[3].set(parameters.preamble_type);
    vif.rate_bytes[4].set(lowest_rate);
    vif.rate_bytes[5].set(lowest_rate);
    vif.rate_bytes[7].set(3);
    vif.basic_rates.set(basic_rates);
    vif.allowed_links.set(match parameters.role {
        Role::Station => 0x8001,
        Role::AccessPoint => 0xffff,
    });
    vif.sleeping_links.set(0);
    vif.awake_links.set(0);
    vif.stations.iter().for_each(|station| station.set(None));
    for (byte, value) in vif.own_mac.iter().zip(own_mac) {
        byte.set(value);
    }
    for (byte, value) in vif.bssid.iter().zip(parameters.bssid) {
        byte.set(value);
    }
    vif.channel.set(parameters.channel);
    vif.internal_link.set(9);
}

/// Take the radio for `interface`; fails while another interface owns it.
fn acquire_radio(vifs: &Vifs, interface: u8) -> bool {
    match vifs.radio_owner.get() {
        Some(owner) if owner != interface => false,
        _ => {
            vifs.radio_owner.set(Some(interface));
            true
        }
    }
}

fn publish_sta(vif: &Vif) {
    vif.active.set(2);
    vif.recompute_effective_links();
    vif.activity_state.set(3);
}

fn deactivate_record(vif: &Vif) {
    vif.stations.iter().for_each(|station| station.set(None));
    vif.active.set(0);
    vif.mode.set(0);
    vif.flags.set(0);
    vif.allowed_links.set(0);
    vif.effective_links.set(0);
    vif.activity_state.set(0);
}

#[cfg(target_arch = "arm")]
pub fn join_gate(interface: u8, channel: u16) -> Result<(), JoinStateError> {
    if !is_operating_interface(interface) {
        return Err(JoinStateError::InvalidInterface);
    }
    let conflict = (0..2)
        .filter(|&other| other != interface)
        .filter_map(vif)
        .any(|other| other.active.get() != 0 && other.channel.get() != channel);
    if conflict {
        Err(JoinStateError::ChannelConflict)
    } else {
        Ok(())
    }
}

/// Retain the minimal vendor STA-BSS state after channel programming. Activity
/// is published last so scan finish cannot observe a partially built VIF.
#[cfg(target_arch = "arm")]
pub fn activate_sta(
    interface: u8,
    request: &crate::wsm::JoinRequest<'_>,
    own_mac: [u8; 6],
) -> Result<(), JoinStateError> {
    if !is_operating_interface(interface) {
        return Err(JoinStateError::InvalidInterface);
    }
    let Some(record) = vif(interface) else {
        return Err(JoinStateError::InvalidInterface);
    };
    let parameters = RecordParameters {
        role: Role::Station,
        band: request.band,
        preamble_type: request.preamble_type,
        basic_rates: request.basic_rate_set,
        // Proven JOIN publications: bit 0 plus exactly one of bit 10/bit 11.
        control_bits: 1 | if request.probe_for_join { 0x400 } else { 0x800 },
        bssid: request.bssid,
        channel: request.channel_number,
    };
    activate(interface, record, &parameters, own_mac)
}

/// START: `interface` becomes an access point whose BSSID is its own
/// address. It accepts frames on every link until stations are mapped.
#[cfg(target_arch = "arm")]
pub fn activate_ap(
    interface: u8,
    request: &crate::wsm::StartRequest<'_>,
    own_mac: [u8; 6],
) -> Result<(), JoinStateError> {
    if !is_operating_interface(interface) {
        return Err(JoinStateError::InvalidInterface);
    }
    let Some(record) = vif(interface) else {
        return Err(JoinStateError::InvalidInterface);
    };
    let parameters = RecordParameters {
        role: Role::AccessPoint,
        band: request.band,
        preamble_type: request.preamble_type,
        basic_rates: request.basic_rate_set,
        control_bits: 1,
        bssid: own_mac,
        channel: request.channel_number,
    };
    activate(interface, record, &parameters, own_mac)
}

#[cfg(target_arch = "arm")]
fn activate(
    interface: u8,
    record: &Vif,
    parameters: &RecordParameters,
    own_mac: [u8; 6],
) -> Result<(), JoinStateError> {
    let basic_rates = if parameters.basic_rates == 0 {
        7
    } else {
        parameters.basic_rates
    };
    // Preliminary preparation is deliberately non-final. A busy radio owner
    // may leave these defaults, but cannot publish an operating VIF, effective
    // links, or activity state.
    prepare_record(record, parameters, own_mac, basic_rates);
    publish_join_pas(
        crate::mac::low_mac(),
        interface,
        parameters.role,
        basic_rates,
        &own_mac,
    );
    if !acquire_radio(&VIFS, interface) {
        return Err(JoinStateError::Busy);
    }
    publish_sta(record);
    crate::phy::state().retained_state.set(3);
    Ok(())
}

/// Inactive publication used by RESET and failed JOIN rollback.
#[cfg(target_arch = "arm")]
pub fn teardown(interface: u8) -> bool {
    if !is_operating_interface(interface) {
        return false;
    }
    let Some(record) = vif(interface) else {
        return false;
    };
    deactivate_record(record);
    if VIFS.radio_owner.get() == Some(interface) {
        VIFS.radio_owner.set(None);
    }
    publish_teardown_pas(crate::mac::low_mac(), interface);
    true
}

/// Snapshot fields needed by JOIN-time scan restoration.
#[cfg(target_arch = "arm")]
pub fn snapshot(interface: u8) -> Option<VifState> {
    let record = vif(interface)?;
    Some(VifState {
        mode: record.mode.get(),
        active: record.active.get() != 0,
        control_bits: record.flags.get(),
        basic_rates: record.basic_rates.get(),
        bssid: record.bssid.each_ref().map(Cell::get),
        channel: record.channel.get(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_semantics_exclude_synthetic_record() {
        let vifs = Vifs {
            records: [const { Vif::new() }; VIF_COUNT],
            radio_owner: Cell::new(None),
        };
        vifs.records[0].active.set(2);
        vifs.records[2].active.set(2);
        assert!(!is_operating_interface(2));
        assert_eq!(vifs.records[2].active.get(), 2);
    }

    #[test]
    fn radio_ownership_is_exclusive_until_teardown() {
        let vifs = Vifs {
            records: [const { Vif::new() }; VIF_COUNT],
            radio_owner: Cell::new(None),
        };
        assert!(acquire_radio(&vifs, 0));
        assert!(acquire_radio(&vifs, 0));
        assert!(!acquire_radio(&vifs, 1));
    }

    #[test]
    fn negotiated_limits_follow_the_mapped_station() {
        let v = Vif::new();
        v.mode.set(Role::AccessPoint as u8);
        v.map_station(1, [1; 6]);
        v.map_station(2, [2; 6]);
        v.set_peer_ht(&[1, 2, 1, 1, 1, 1, 1, 1, 1]);
        v.set_peer_ht(&[2, 1, 5, 2, 2, 2, 2, 2, 2]);
        assert_eq!(
            v.ht_limits(1, 0),
            HtLimits {
                density: 1,
                max_bytes: 32767
            }
        );
        assert_eq!(
            v.ht_limits(2, 0),
            HtLimits {
                density: 5,
                max_bytes: 16383
            }
        );
        v.map_station(1, [3; 6]);
        v.set_peer_ht(&[1, 2, 1, 1, 1, 1, 1, 1, 1]); // delayed write for previous station
        assert_eq!(v.ht_limits(1, 0), HtLimits::CONSERVATIVE);
    }

    #[test]
    fn links_name_a_stations_ap_or_an_access_points_stations() {
        let (ap, station) = ([2; 6], [3; 6]);
        let vif = Vif::new();
        vif.mode.set(Role::Station as u8);
        vif.bssid
            .iter()
            .zip(ap)
            .for_each(|(byte, value)| byte.set(value));
        assert!(!vif.map_station(1, station));
        assert_eq!((vif.link_peer(0), vif.link_peer(1)), (Some(ap), None));
        vif.mode.set(Role::AccessPoint as u8);
        assert!(
            vif.map_station(2, station)
                && !vif.map_station(0, station)
                && !vif.map_station(6, station)
        );
        assert_eq!(
            (vif.link_peer(0), vif.link_peer(2), vif.peer_link(station)),
            (None, Some(station), Some(2))
        );
        deactivate_record(&vif);
        assert_eq!(vif.peer_link(station), None);
    }

    #[test]
    fn teardown_clears_operating_fields() {
        let vif = Vif::new();
        vif.active.set(2);
        vif.mode.set(6);
        vif.flags.set(u32::MAX);
        vif.allowed_links.set(0x8001);
        vif.effective_links.set(1);
        vif.activity_state.set(3);
        deactivate_record(&vif);
        assert_eq!(
            (vif.active.get(), vif.mode.get(), vif.flags.get()),
            (0, 0, 0)
        );
        assert_eq!((vif.allowed_links.get(), vif.effective_links.get()), (0, 0));
        assert_eq!(vif.activity_state.get(), 0);
    }

    #[test]
    fn rate_configuration_word_sets_the_low_rate_bytes() {
        let vif = Vif::new();
        vif.rate_bytes[7].set(3);
        set_rate_configuration(&vif, 0x4433_2211);
        assert_eq!(
            vif.rate_bytes.each_ref().map(Cell::get),
            [0x11, 0x22, 0x33, 0x44, 0, 0, 0, 3]
        );
    }

    #[test]
    fn join_publication_activates_one_pas_record_and_resets_its_backoff() {
        let low_mac = crate::mac::LowMac::new();
        low_mac.own_mac[0][5].set(0xa5);
        low_mac.pas(2_usize).activity.set(2);
        let pas = low_mac.pas(1_usize);
        for queue in 0..4 {
            pas.cw_min[queue].set(0x10 + queue as u16);
            pas.contention_window[queue].set(0x3ff);
            pas.retry_count[queue].set(7);
        }

        publish_join_pas(&low_mac, 1, Role::Station, 0x1234, &[1, 2, 3, 4, 5, 6]);

        assert_eq!(low_mac.pas(2_usize).activity.get(), 0);
        assert_eq!(
            (pas.activity.get(), pas.slot_bits.get(), pas.mode.get()),
            (2, 4, 1)
        );
        assert_eq!(pas.basic_rates.get(), 0x1234);
        assert_eq!(pas.slot_timing.get(), 9);
        assert_eq!(pas.own_mac.each_ref().map(Cell::get), [1, 2, 3, 4, 5, 6]);
        for queue in 0..4 {
            assert_eq!(pas.retry_count[queue].get(), 0);
            assert_eq!(pas.contention_window[queue].get(), 0x10 + queue as u32);
        }

        publish_teardown_pas(&low_mac, 1);
        assert_eq!((pas.activity.get(), pas.mode.get()), (1, 0));
    }
}
