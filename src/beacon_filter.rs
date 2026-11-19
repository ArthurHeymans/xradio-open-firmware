//! Beacon filtering for the joined BSS (MIBs 0x1004 and 0x1005).
//!
//! cw1200 (with its `disable_beacon_filter` fix) enables it while mac80211
//! does not ask for every beacon, and lists the elements it cares about: HT
//! operation, ERP information and the Wi-Fi Alliance P2P element, each to
//! be reported when it changes, appears or disappears. Beacons that change
//! none of them stay in the firmware, so the host is not woken ten times a
//! second for nothing; beacons announcing a channel switch always go. The
//! vendor (`beacon_filter_check_and_store`, 0x33e6) keeps the last
//! forwarded beacon to compare against; one fingerprint per listed element
//! does as well here.
//!
//! Beacons of other BSSes on the channel reach the host only when it asks
//! for them, by RX_FILTER (MIB 0x1003: promiscuous or other BSSes) or
//! DISABLE_BSSID_FILTER (MIB 0x1026). The vendor's station drops them
//! (`rx_mgmt_frame_handler`, 0xc4f0: a joined station's beacon without the
//! MAC's BSSID match is freed); with neighbours around they otherwise wake
//! the host several times as often as the joined AP's. Other stations'
//! probe requests go the same way: only an access point answers them.

const MAX_ENTRIES: usize = 10;
const ENTRY_LEN: usize = 8;
const ENABLE: u32 = 1 << 0;
const HAS_CHANGED: u8 = 1 << 0;
const NO_LONGER_PRESENT: u8 = 1 << 1;
const HAS_APPEARED: u8 = 1 << 2;
const VENDOR_SPECIFIC: u8 = 0xdd;
/// Channel Switch Announcement and its extended form: mac80211 has to see
/// every beacon that carries one, listed or not.
const CHANNEL_SWITCH: [u8; 2] = [37, 60];
/// Header and fixed fields (timestamp, interval, capabilities) of a beacon.
const ELEMENTS_OFFSET: usize = 24 + 12;
/// RX_FILTER bits that let other BSSes' frames through.
const RX_FILTER_PROMISCUOUS: u32 = 1 << 0;
const RX_FILTER_OTHER_BSS: u32 = 1 << 1;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Entry {
    id: u8,
    flags: u8,
    oui: [u8; 3],
}

#[derive(Debug, Default)]
pub struct BeaconFilter {
    enabled: bool,
    entries: [Entry; MAX_ENTRIES],
    count: usize,
    /// Each listed element's fingerprint in the last forwarded beacon
    /// (`None`: absent); `None` overall until one was forwarded.
    seen: Option<[Option<u32>; MAX_ENTRIES]>,
    /// The host wants other BSSes' beacons too.
    other_bss_by_rx_filter: bool,
    other_bss_by_bssid_filter: bool,
}

impl BeaconFilter {
    pub const fn new() -> Self {
        Self {
            enabled: false,
            entries: [Entry {
                id: 0,
                flags: 0,
                oui: [0; 3],
            }; MAX_ENTRIES],
            count: 0,
            seen: None,
            other_bss_by_rx_filter: false,
            other_bss_by_bssid_filter: false,
        }
    }

    /// MIB 0x1003: filter flags.
    pub fn set_rx_filter(&mut self, data: &[u8]) -> bool {
        let Some(flags) = data.first_chunk::<4>() else {
            return false;
        };
        self.other_bss_by_rx_filter =
            u32::from_le_bytes(*flags) & (RX_FILTER_PROMISCUOUS | RX_FILTER_OTHER_BSS) != 0;
        true
    }

    /// MIB 0x1026: non-zero turns the BSSID filter off.
    pub fn set_bssid_filter(&mut self, data: &[u8]) -> bool {
        let Some(&disabled) = data.first() else {
            return false;
        };
        self.other_bss_by_bssid_filter = disabled != 0;
        true
    }

    /// Whether a joined station passes another BSS's beacon, or a probe
    /// request, to the host.
    pub const fn forward_other_bss(&self) -> bool {
        self.other_bss_by_rx_filter || self.other_bss_by_bssid_filter
    }

    /// MIB 0x1004: a count, then that many eight-octet entries (element ID,
    /// flags, OUI, match data).
    pub fn set_table(&mut self, data: &[u8]) -> bool {
        let Some((count, entries)) = data.split_first_chunk::<4>() else {
            return false;
        };
        let count = u32::from_le_bytes(*count) as usize;
        if count > MAX_ENTRIES || entries.len() < count * ENTRY_LEN {
            return false;
        }
        entries
            .chunks_exact(ENTRY_LEN)
            .take(count)
            .zip(&mut self.entries)
            .for_each(|(raw, entry)| {
                *entry = Entry {
                    id: raw[0],
                    flags: raw[1],
                    oui: [raw[2], raw[3], raw[4]],
                };
            });
        self.count = count;
        self.seen = None;
        true
    }

    /// MIB 0x1005: enable flags, then the beacon count, which cw1200 leaves
    /// zero while filtering.
    pub fn set_control(&mut self, data: &[u8]) -> bool {
        let Some(enabled) = data.first_chunk::<4>() else {
            return false;
        };
        self.enabled = u32::from_le_bytes(*enabled) & ENABLE != 0;
        self.seen = None;
        true
    }

    /// Joined anew: the next beacon goes to the host.
    pub fn restart(&mut self) {
        self.seen = None;
    }

    /// Whether a beacon of the joined BSS goes to the host.
    pub fn forward(&mut self, beacon: &[u8]) -> bool {
        if !self.enabled {
            return true;
        }
        let elements = beacon.get(ELEMENTS_OFFSET..).unwrap_or(&[]);
        let switching = CHANNEL_SWITCH.iter().any(|&id| {
            fingerprint(
                elements,
                Entry {
                    id,
                    flags: 0,
                    oui: [0; 3],
                },
            )
            .is_some()
        });
        let now: [Option<u32>; MAX_ENTRIES] = core::array::from_fn(|index| {
            (index < self.count)
                .then(|| fingerprint(elements, self.entries[index]))
                .flatten()
        });
        let changed = self.seen.is_none_or(|seen| {
            self.entries[..self.count]
                .iter()
                .zip(seen.iter().zip(&now))
                .any(|(entry, (before, after))| match (before, after) {
                    (Some(before), Some(after)) => {
                        entry.flags & HAS_CHANGED != 0 && before != after
                    }
                    (Some(_), None) => entry.flags & NO_LONGER_PRESENT != 0,
                    (None, Some(_)) => entry.flags & HAS_APPEARED != 0,
                    (None, None) => false,
                })
        });
        if changed {
            self.seen = Some(now);
        }
        changed || switching
    }
}

/// FNV-1a over the first element matching `entry` (a vendor-specific one
/// by its OUI), or `None` if the beacon has none.
fn fingerprint(mut elements: &[u8], entry: Entry) -> Option<u32> {
    while let [id, length, rest @ ..] = elements {
        let body = rest.get(..usize::from(*length))?;
        if *id == entry.id && (entry.id != VENDOR_SPECIFIC || body.starts_with(&entry.oui)) {
            return Some(body.iter().fold(0x811c_9dc5_u32, |hash, &byte| {
                (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
            }));
        }
        elements = &rest[usize::from(*length)..];
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// cw1200's station table: P2P vendor element, HT operation, ERP.
    fn station_filter() -> BeaconFilter {
        let mut table = [0_u8; 4 + 3 * ENTRY_LEN];
        table[0] = 3;
        let all = HAS_CHANGED | NO_LONGER_PRESENT | HAS_APPEARED;
        table[4..9].copy_from_slice(&[VENDOR_SPECIFIC, all, 0x50, 0x6f, 0x9a]);
        table[12..14].copy_from_slice(&[61, all]);
        table[20..22].copy_from_slice(&[42, all]);
        let mut filter = BeaconFilter::new();
        assert!(filter.set_table(&table));
        assert!(filter.set_control(&[1, 0, 0, 0, 0, 0, 0, 0]));
        filter
    }

    fn beacon(elements: &[u8]) -> [u8; 80] {
        let mut frame = [0_u8; 80];
        frame[0] = 0x80;
        frame[ELEMENTS_OFFSET..ELEMENTS_OFFSET + elements.len()].copy_from_slice(elements);
        frame
    }

    // SSID "x", TIM (changes every beacon), ERP, HT operation.
    const BASE: [u8; 17] = [0, 1, b'x', 5, 4, 0, 1, 0, 0, 42, 1, 0, 61, 3, 1, 0, 0];

    #[test]
    fn other_bsses_only_when_asked_for() {
        let mut filter = BeaconFilter::new();
        assert!(!filter.forward_other_bss());
        assert!(filter.set_rx_filter(&2_u32.to_le_bytes()));
        assert!(filter.forward_other_bss(), "RX_FILTER: other BSSes");
        assert!(filter.set_rx_filter(&4_u32.to_le_bytes()));
        assert!(!filter.forward_other_bss(), "FCS failures only");
        assert!(filter.set_bssid_filter(&[1, 0, 0, 0]));
        assert!(filter.forward_other_bss(), "BSSID filter off");
        assert!(filter.set_bssid_filter(&[0, 0, 0, 0]));
        assert!(!filter.forward_other_bss());
        assert!(!filter.set_rx_filter(&[2]));
    }

    #[test]
    fn only_listed_elements_wake_the_host() {
        let mut filter = station_filter();
        assert!(filter.forward(&beacon(&BASE)), "the first beacon");
        assert!(!filter.forward(&beacon(&BASE)));
        let mut tim = BASE;
        tim[6] = 0;
        assert!(!filter.forward(&beacon(&tim)), "the TIM is not listed");
        let mut erp = BASE;
        erp[11] = 2;
        assert!(filter.forward(&beacon(&erp)), "ERP changed");
        assert!(!filter.forward(&beacon(&erp)));
        assert!(filter.forward(&beacon(&BASE[..12])), "HT operation gone");
        assert!(filter.forward(&beacon(&BASE)), "and back");
    }

    #[test]
    fn vendor_elements_match_by_oui() {
        let mut filter = station_filter();
        assert!(filter.forward(&beacon(&BASE)));
        let mut wmm = [0_u8; 23];
        wmm[..17].copy_from_slice(&BASE);
        wmm[17..].copy_from_slice(&[VENDOR_SPECIFIC, 4, 0x00, 0x50, 0xf2, 2]);
        assert!(!filter.forward(&beacon(&wmm)), "another vendor's element");
        let mut p2p = wmm;
        p2p[19..22].copy_from_slice(&[0x50, 0x6f, 0x9a]);
        assert!(filter.forward(&beacon(&p2p)), "the P2P element appeared");
    }

    #[test]
    fn channel_switches_always_reach_the_host() {
        let mut filter = station_filter();
        assert!(filter.forward(&beacon(&BASE)));
        let mut csa = [0_u8; 22];
        csa[..17].copy_from_slice(&BASE);
        csa[17..].copy_from_slice(&[37, 3, 1, 11, 5]);
        assert!(filter.forward(&beacon(&csa)));
        assert!(filter.forward(&beacon(&csa)), "every one of them");
        assert!(!filter.forward(&beacon(&BASE)));
    }

    #[test]
    fn disabled_or_reset_filters_forward() {
        let mut filter = station_filter();
        assert!(filter.forward(&beacon(&BASE)));
        filter.restart();
        assert!(filter.forward(&beacon(&BASE)), "first beacon after a join");
        assert!(filter.set_control(&[0, 0, 0, 0, 1, 0, 0, 0]));
        assert!(filter.forward(&beacon(&BASE)));
        assert!(filter.forward(&beacon(&BASE)));
        assert!(!filter.set_table(&[11, 0, 0, 0]), "too many entries");
        assert!(!filter.set_table(&[1, 0, 0, 0, 42]), "truncated");
    }
}
