//! Retained host configuration, and the SDD calibration file parsed for the
//! PHY. The firmware loop owns the [`Configuration`]; the PHY keeps its own
//! copy of the parsed calibration (`phy::calibrate`), and association
//! parameters go to the TX engine and the radio when they change.

use crate::wsm::{ConfigurationRequest, EdcaParameters, TxPowerRange, TxQueueParameters};

pub const MAX_DPD_DATA_LEN: usize = 1536;
pub const MAX_TEMPLATE_FRAME_LEN: usize = 2304;
/// Beacon and probe-response templates: mac80211's with HT and WPA2 IEs
/// are a few hundred octets.
pub const MAX_BEACON_TEMPLATE_LEN: usize = 1024;

/// Template frame types (WSM_FRAME_TYPE_*).
pub const TEMPLATE_PROBE_REQUEST: u8 = 0;
pub const TEMPLATE_BEACON: u8 = 1;
pub const TEMPLATE_PROBE_RESPONSE: u8 = 5;

/// A template frame MIB (0x1002) as the host wrote it: frame type, rate,
/// frame length, then the frame from its frame control field.
#[derive(Clone, Copy)]
struct Template<const N: usize> {
    len: usize,
    mib: [u8; N],
}

impl<const N: usize> Template<N> {
    const fn new() -> Self {
        Self {
            len: 0,
            mib: [0; N],
        }
    }

    fn get(&self) -> Option<&[u8]> {
        (self.len != 0).then_some(&self.mib[..self.len])
    }

    fn set(&mut self, mib: &[u8]) -> bool {
        let Some(slot) = self.mib.get_mut(..mib.len()) else {
            return false;
        };
        slot.copy_from_slice(mib);
        self.mib[mib.len()..self.len.max(mib.len())].fill(0);
        self.len = mib.len();
        true
    }
}

/// Where a beacon's or probe response's elements start in its template
/// MIB: the MIB header, the 24-octet frame header, timestamp, beacon
/// interval and capability.
const TEMPLATE_ELEMENTS: usize = 4 + 36;

/// The octets of element `id` in `elements`, if it is there whole.
fn find_element(elements: &[u8], id: u8) -> Option<core::ops::Range<usize>> {
    let mut start = 0;
    while let [element_id, length, ..] = elements[start..] {
        let end = start + 2 + usize::from(length);
        if end > elements.len() {
            return None;
        }
        if element_id == id {
            return Some(start..end);
        }
        start = end;
    }
    None
}

impl<const N: usize> Template<N> {
    /// Put `element` in place of the template's element of the same ID, or
    /// after the last one. Without a template there is nothing to update.
    fn replace_element(&mut self, element: &[u8]) -> bool {
        if self.len < TEMPLATE_ELEMENTS {
            return true;
        }
        let elements = &self.mib[TEMPLATE_ELEMENTS..self.len];
        let old = find_element(elements, element[0]).unwrap_or(elements.len()..elements.len());
        let (start, end) = (TEMPLATE_ELEMENTS + old.start, TEMPLATE_ELEMENTS + old.end);
        let len = self.len - (end - start) + element.len();
        if len > N {
            return false;
        }
        self.mib.copy_within(end..self.len, start + element.len());
        self.mib[start..start + element.len()].copy_from_slice(element);
        if len < self.len {
            self.mib[len..self.len].fill(0);
        }
        self.len = len;
        self.mib[2..4].copy_from_slice(&((len - 4) as u16).to_le_bytes());
        true
    }
}

/// The frame length a template MIB declares, if it matches the MIB's.
fn template_frame_len(mib: &[u8]) -> Option<usize> {
    let declared = usize::from(u16::from_le_bytes([*mib.get(2)?, *mib.get(3)?]));
    (declared + 4 == mib.len()).then_some(declared)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigurationError {
    DpdTooLarge,
    MalformedSdd,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelCalibrationValues {
    pub first: i16,
    pub second: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Mode0ChannelCalibration {
    pub base: i16,
    pub first: i16,
    pub second: i16,
}

/// Calibration the PHY takes from the SDD file for one band profile
/// (0 = 2.4 GHz, 1 = 5 GHz). Comments give the SDD element types.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SddProfile {
    /// 0xe3/0xe4: per-rate TX power limits.
    pub rate_limits: [i16; 11],
    /// 0xec/0xed: `(upper_channel, first, second)` TX-power records.
    pub channel_records: [[u8; 3]; SDD_CHANNEL_RECORDS],
    pub channel_count: u8,
    /// 0xe0/0xe1: AGC threshold correction.
    pub agc_correction: i16,
    /// 0x40/0x41: RSSI denominator and offset.
    pub rssi_coefficients: [i16; 2],
    /// 0x42/0x43: RSSI temperature coefficient.
    pub temperature_coefficient: i16,
    /// 0x46/0x47: ADC conversion divisor and multiplier.
    pub conversion: [i16; 2],
    /// 0x48/0x49: per-rate RSSI scales.
    pub rssi_rate_scales: [i16; 11],
}

pub const SDD_CHANNEL_RECORDS: usize = 16;

/// SDD-derived PHY calibration. An element missing from a configuration keeps
/// the previous value, as the vendor's per-element callbacks do.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SddCalibration {
    pub profiles: [SddProfile; 2],
    /// Gain-pair coefficients: elements 0x20, 0x22, 0x21, 0x23 in that order,
    /// so each profile's pair is `[2 * profile]` and `[2 * profile + 1]`.
    pub gain_coefficients: [u16; 4],
    /// 0x30/0x31: per-channel threshold corrections.
    pub thresholds: [ThresholdTable; 2],
}

/// Vendor `0x176bc` loads SDD elements `0x30` and `0x31` as a signed default
/// followed by four-byte `(upper_channel, correction)` records.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ThresholdTable {
    pub present: bool,
    pub default: i16,
    pub records: [(u16, i16); SDD_THRESHOLD_RECORDS],
    pub count: u8,
}

pub const SDD_THRESHOLD_RECORDS: usize = 16;

impl ThresholdTable {
    const ZERO: Self = Self {
        present: false,
        default: 0,
        records: [(0, 0); SDD_THRESHOLD_RECORDS],
        count: 0,
    };

    fn parse(element: &[u8]) -> Option<Self> {
        if element.len() < 2 || !(element.len() - 2).is_multiple_of(4) {
            return None;
        }
        let mut table = Self {
            present: true,
            default: i16::from_le_bytes([element[0], element[1]]),
            ..Self::ZERO
        };
        for (slot, record) in table.records.iter_mut().zip(element[2..].chunks_exact(4)) {
            *slot = (
                u16::from_le_bytes([record[0], record[1]]),
                i16::from_le_bytes([record[2], record[3]]),
            );
            table.count += 1;
        }
        Some(table)
    }
}

impl SddProfile {
    const ZERO: Self = Self {
        rate_limits: [0; 11],
        channel_records: [[0; 3]; SDD_CHANNEL_RECORDS],
        channel_count: 0,
        agc_correction: 0,
        rssi_coefficients: [0; 2],
        temperature_coefficient: 0,
        conversion: [0; 2],
        rssi_rate_scales: [0; 11],
    };
}

impl SddCalibration {
    pub const ZERO: Self = Self {
        profiles: [SddProfile::ZERO; 2],
        gain_coefficients: [0; 4],
        thresholds: [ThresholdTable::ZERO; 2],
    };

    /// `0x19dd0`: the last correction of element `0x30` or `0x31` whose
    /// threshold is not greater than the channel.
    pub fn channel_threshold_correction(&self, element_id: u8, channel: u16) -> Option<i16> {
        let table = self
            .thresholds
            .get(usize::from(element_id.checked_sub(0x30)?))?;
        table.present.then(|| {
            table.records[..usize::from(table.count)]
                .iter()
                .take_while(|&&(upper_channel, _)| upper_channel <= channel)
                .last()
                .map_or(table.default, |&(_, correction)| correction)
        })
    }

    /// Vendor `0x1a112`: the profile-0 channel record (element `0xec`) for
    /// `channel`, as offsets from `base`.
    pub fn channel_calibration_values(
        &self,
        channel: u8,
        base: i16,
    ) -> Option<ChannelCalibrationValues> {
        let profile = &self.profiles[0];
        let records = &profile.channel_records[..usize::from(profile.channel_count)];
        let mut selected = records.first()?;
        for (index, record) in records.iter().enumerate() {
            if channel <= record[0] {
                selected = if channel < record[0] && index != 0 {
                    &records[index - 1]
                } else {
                    record
                };
                break;
            }
        }
        Some(ChannelCalibrationValues {
            first: base.wrapping_add(i16::from(selected[1]) * 4),
            second: base.wrapping_add(i16::from(selected[2]) * 4),
        })
    }

    pub fn mode0_channel_calibration(&self, channel: u8) -> Option<Mode0ChannelCalibration> {
        let base = self.channel_threshold_correction(0x30, u16::from(channel))?;
        let values = self.channel_calibration_values(channel, base)?;
        Some(Mode0ChannelCalibration {
            base,
            first: values.first,
            second: values.second,
        })
    }

    /// Apply the calibration elements present in `sdd` on top of `self`.
    pub fn apply(&mut self, sdd: &[u8]) {
        let element = |id| sdd_element(sdd, id);
        fn halfwords(data: &[u8]) -> impl Iterator<Item = i16> + '_ {
            data.as_chunks::<2>().0.iter()
                .map(|pair| i16::from_le_bytes([pair[0], pair[1]]))
        }
        // Single values and pairs need the whole element; per-rate tables copy
        // as many entries as the element holds.
        let copy = |id, destination: &mut [i16]| {
            let Some(data) = element(id) else { return };
            if destination.len() <= 2 && data.len() < 2 * destination.len() {
                return;
            }
            destination
                .iter_mut()
                .zip(halfwords(data))
                .for_each(|(slot, value)| *slot = value);
        };
        for (profile, [limits, agc, rssi, temperature, conversion, scales]) in [
            [0xe3, 0xe0, 0x40, 0x42, 0x46, 0x48],
            [0xe4, 0xe1, 0x41, 0x43, 0x47, 0x49],
        ]
        .into_iter()
        .enumerate()
        {
            let bank = &mut self.profiles[profile];
            copy(limits, &mut bank.rate_limits);
            copy(agc, core::slice::from_mut(&mut bank.agc_correction));
            copy(rssi, &mut bank.rssi_coefficients);
            copy(
                temperature,
                core::slice::from_mut(&mut bank.temperature_coefficient),
            );
            copy(conversion, &mut bank.conversion);
            copy(scales, &mut bank.rssi_rate_scales);
        }
        for (index, id) in [0x20, 0x22, 0x21, 0x23].into_iter().enumerate() {
            if let Some(value) = element(id).and_then(|data| halfwords(data).next()) {
                self.gain_coefficients[index] = value as u16;
            }
        }
        // Channel records: a 16-bit count, then three-byte records for
        // profile 0 (0xec) and two-byte records for profile 1 (0xed).
        for (profile, id, width) in [(0, 0xec, 3), (1, 0xed, 2)] {
            let Some(data) = element(id).filter(|data| data.len() >= 2) else {
                continue;
            };
            let count = usize::from(u16::from_le_bytes([data[0], data[1]]));
            let Some(records) = data.get(2..2 + count * width) else {
                continue;
            };
            if count > usize::from(u8::MAX) {
                continue;
            }
            let bank = &mut self.profiles[profile];
            for (slot, record) in bank
                .channel_records
                .iter_mut()
                .zip(records.chunks_exact(width))
            {
                slot[..width].copy_from_slice(record);
            }
            bank.channel_count = count.min(SDD_CHANNEL_RECORDS) as u8;
        }
        for (table, id) in self.thresholds.iter_mut().zip([0x30, 0x31]) {
            if let Some(parsed) = element(id).and_then(ThresholdTable::parse) {
                *table = parsed;
            }
        }
    }
}

/// What the host configured: CONFIGURATION, EDCA and queue parameters and
/// the MIBs the firmware retains.
pub struct Configuration {
    configured: bool,
    max_tx_msdu_lifetime: u32,
    max_rx_lifetime: u32,
    protection: [crate::tx::ProtectionPolicy; 2],
    dpd_version: u16,
    station_id: [u8; 6],
    dpd_flags: u16,
    /// From the SDD file, for the CONFIGURATION reply.
    tx_power_ranges: Option<[TxPowerRange; 2]>,
    edca_valid: bool,
    edca: EdcaParameters,
    uapsd_information: [u8; 8],
    tx_queues: [TxQueueParameters; 4],
    probe_request: Template<MAX_TEMPLATE_FRAME_LEN>,
    beacon: Template<MAX_BEACON_TEMPLATE_LEN>,
    probe_response: Template<MAX_BEACON_TEMPLATE_LEN>,
    current_tx_power_encoded: u32,
    block_ack_tx_tids: u8,
    block_ack_rx_tids: u8,
    tx_confirm_format: crate::wsm::TxConfirmFormat,
    mpdu_start_spacing: u8,
    operational_tx_ba_tids: u8,
}

impl Default for Configuration {
    fn default() -> Self {
        Self::new()
    }
}

impl Configuration {
    pub const fn new() -> Self {
        Self {
            configured: false,
            max_tx_msdu_lifetime: 0,
            max_rx_lifetime: 0,
            protection: [crate::tx::ProtectionPolicy::DISABLED; 2],
            dpd_version: 0,
            station_id: [0; 6],
            dpd_flags: 0,
            tx_power_ranges: None,
            edca_valid: false,
            edca: EdcaParameters {
                queues: [crate::wsm::EdcaQueueParameters {
                    cwmin: 0,
                    cwmax: 0,
                    aifns: 0,
                    txop_limit: 0,
                    max_rx_lifetime: 0,
                }; 4],
            },
            uapsd_information: [0; 8],
            tx_queues: [TxQueueParameters {
                queue_id: 0,
                ack_policy: 0,
                max_transmit_lifetime: 0,
                allowed_medium_time: 0,
            }; 4],
            probe_request: Template::new(),
            beacon: Template::new(),
            probe_response: Template::new(),
            current_tx_power_encoded: 0,
            block_ack_tx_tids: 0,
            block_ack_rx_tids: 0,
            tx_confirm_format: crate::wsm::TxConfirmFormat::Xr819,
            mpdu_start_spacing: 0,
            operational_tx_ba_tids: 0,
        }
    }

    /// CONFIGURATION: keep the parameters and hand the SDD file's
    /// calibration to the PHY.
    pub fn retain(&mut self, request: &ConfigurationRequest<'_>) -> Result<(), ConfigurationError> {
        if request.dpd_data.len() > MAX_DPD_DATA_LEN {
            return Err(ConfigurationError::DpdTooLarge);
        }
        validate_sdd(request.dpd_data)?;
        self.max_tx_msdu_lifetime = request.max_tx_msdu_lifetime;
        self.max_rx_lifetime = request.max_rx_lifetime;
        // cw1200 leaves this field zero. Do not enable RTS implicitly while
        // protected aggregates are unsupported (vendor substitutes 300 here).
        let threshold = if request.rts_threshold == 0 {
            u32::MAX
        } else {
            request.rts_threshold
        };
        self.protection
            .iter_mut()
            .for_each(|policy| policy.rts_threshold = threshold);
        self.dpd_version = request.dpd_version;
        self.station_id = request.station_id;
        self.dpd_flags = request.dpd_flags;
        self.tx_power_ranges = tx_power_ranges(request.dpd_data);
        self.configured = true;
        self.tx_confirm_format = if request.padded {
            crate::wsm::TxConfirmFormat::Cw1200
        } else {
            crate::wsm::TxConfirmFormat::Xr819
        };
        crate::phy::calibrate(request.dpd_data);
        Ok(())
    }

    /// The station address, once CONFIGURATION has arrived.
    pub fn station_id(&self) -> Option<[u8; 6]> {
        self.configured.then_some(self.station_id)
    }

    pub fn tx_power_ranges(&self) -> Option<[TxPowerRange; 2]> {
        self.tx_power_ranges
    }

    pub fn retain_edca(&mut self, parameters: EdcaParameters) {
        self.edca = parameters;
        self.edca_valid = true;
    }

    pub fn edca(&self) -> Option<EdcaParameters> {
        self.edca_valid.then_some(self.edca)
    }

    pub fn retain_tx_queue(&mut self, parameters: TxQueueParameters) {
        self.tx_queues[usize::from(parameters.queue_id)] = parameters;
    }

    pub fn tx_queue(&self, queue: usize) -> Option<TxQueueParameters> {
        self.tx_queues.get(queue).copied()
    }

    /// The template MIB of `frame_type`, header included, as written.
    pub fn template(&self, frame_type: u8) -> Option<&[u8]> {
        match frame_type {
            TEMPLATE_PROBE_REQUEST => self.probe_request.get(),
            TEMPLATE_BEACON => self.beacon.get(),
            TEMPLATE_PROBE_RESPONSE => self.probe_response.get(),
            _ => None,
        }
    }

    /// UPDATE_IE: replace `count` elements in the beacon (`what` bit 0) and
    /// probe-response (bit 1) templates. What follows them is padding.
    pub fn update_template_elements(&mut self, what: u16, count: u16, elements: &[u8]) -> bool {
        let mut rest = elements;
        (0..count).all(|_| {
            let Some((element, tail)) = rest
                .get(1)
                .and_then(|&length| rest.split_at_checked(2 + usize::from(length)))
            else {
                return false;
            };
            rest = tail;
            (what & 1 == 0 || self.beacon.replace_element(element))
                && (what & 2 == 0 || self.probe_response.replace_element(element))
        })
    }

    /// Consume the beacon template MIB for the `number`th TBTT. Its DTIM
    /// count is 0 on every period's last TBTT; the multicast bit is clear
    /// on every non-DTIM, as in vendor `beacon_set_tim_bcast_bit`.
    /// Restore the host's pending-group bit after the synchronous consumer
    /// copies it, including when that consumer reports failure. Clearing
    /// the retained bit would forget queued traffic at the next DTIM.
    /// Return the consumer's result and whether this beacon announces group traffic.
    pub fn with_beacon<R>(
        &mut self,
        number: u64,
        consume: impl FnOnce(&[u8]) -> R,
    ) -> Option<(R, bool)> {
        let beacon = &mut self.beacon;
        let mut multicast = false;
        let mut restore_control = None;
        if beacon.len >= TEMPLATE_ELEMENTS
            && let Some(tim) = find_element(&beacon.mib[TEMPLATE_ELEMENTS..beacon.len], 5)
            && tim.len() >= 6
        {
            let count = TEMPLATE_ELEMENTS + tim.start + 2;
            let period = u64::from(beacon.mib[count + 1].max(1));
            beacon.mib[count] = ((period - number % period) % period) as u8;
            let control = beacon.mib[count + 2];
            multicast = beacon.mib[count] == 0 && control & 1 != 0;
            restore_control = Some((count + 2, control));
            beacon.mib[count + 2] = (control & !1) | u8::from(multicast);
        }
        let result = beacon.get().map(|mib| (consume(mib), multicast));
        if let Some((index, control)) = restore_control {
            beacon.mib[index] = control;
        }
        result
    }

    /// Keep a template the firmware uses; accept the others (null data and
    /// PS-Poll frames, which it never sends itself).
    fn retain_template(&mut self, mib: &[u8]) -> bool {
        let Some(frame_len) = template_frame_len(mib) else {
            return false;
        };
        // A beacon or probe response carries timestamp, interval and
        // capability after its 24-octet header.
        let beacon_like = frame_len >= 36;
        match mib[0] {
            TEMPLATE_PROBE_REQUEST => self.probe_request.set(mib),
            TEMPLATE_BEACON => beacon_like && self.beacon.set(mib),
            TEMPLATE_PROBE_RESPONSE => beacon_like && self.probe_response.set(mib),
            _ => true,
        }
    }

    /// The peer's minimum MPDU start spacing (association MIB), 0..=7.
    pub fn mpdu_start_spacing(&self) -> u8 {
        self.mpdu_start_spacing
    }

    /// The multi-TX confirm layout the host driver reads.
    pub fn tx_confirm_format(&self) -> crate::wsm::TxConfirmFormat {
        self.tx_confirm_format
    }

    /// TIDs the host allows the peer's BlockAck sessions for (MIB 0x100e).
    pub fn block_ack_rx_tids(&self) -> u8 {
        self.block_ack_rx_tids
    }

    /// TIDs the host allows TX BlockAck sessions for (MIB 0x100e).
    pub fn block_ack_tx_tids(&self) -> u8 {
        self.block_ack_tx_tids
    }

    pub fn current_tx_power_tenths_dbm(&self) -> Option<i32> {
        (self.current_tx_power_encoded != 0)
            .then_some((self.current_tx_power_encoded ^ 0x8000_0000) as i32)
    }

    pub fn tx_protection(&self) -> [crate::tx::ProtectionPolicy; 2] {
        self.protection
    }

    /// Protection MIBs are VIF-local, unlike the shared calibration/configuration.
    pub fn retain_protection_mib(&mut self, interface: u8, mib_id: u16, data: &[u8]) -> bool {
        let Some(policy) = self.protection.get_mut(usize::from(interface)) else {
            return false;
        };
        match (mib_id, data) {
            (crate::tx::MIB_RTS_THRESHOLD, [a, b, c, d]) => {
                // cw1200_set_rts_threshold encodes disabled as zero.
                let threshold = u32::from_le_bytes([*a, *b, *c, *d]);
                policy.rts_threshold = if threshold == 0 { u32::MAX } else { threshold };
                true
            }
            (crate::tx::MIB_NON_ERP_PROTECTION, [enabled @ 0..=1, 0, 0, 0]) => {
                policy.non_erp = *enabled != 0;
                true
            }
            _ => false,
        }
    }

    pub fn retain_interface_mib(&mut self, mib_id: u16, data: &[u8]) -> bool {
        match (mib_id, data) {
            (crate::wsm::MIB_ID_DOT11_CURRENT_TX_POWER_LEVEL, [a, b, c, d]) => {
                self.current_tx_power_encoded = u32::from_le_bytes([*a, *b, *c, *d]) ^ 0x8000_0000;
                true
            }
            (crate::wsm::MIB_ID_SET_UAPSD_INFORMATION, [a, b, c, d, e, f, g, h]) => {
                self.uapsd_information = [*a, *b, *c, *d, *e, *f, *g, *h];
                true
            }
            (crate::wsm::MIB_ID_BLOCK_ACK_POLICY, [tx, 0, rx, 0]) => {
                self.block_ack_tx_tids = *tx;
                self.block_ack_rx_tids = *rx;
                true
            }
            (crate::wsm::MIB_ID_SET_ASSOCIATION_MODE, [_, _, _, spacing, _, _, _, _])
                if *spacing < 8 =>
            {
                self.mpdu_start_spacing = *spacing;
                true
            }
            (crate::wsm::MIB_ID_PRIVATE_TX_BA_SESSION, [tid, enabled]) if *tid < 8 => {
                let bit = 1_u8 << *tid;
                if *enabled == 0 {
                    self.operational_tx_ba_tids &= !bit;
                } else {
                    self.operational_tx_ba_tids |= bit;
                }
                true
            }
            // The joined RX path delivers every group address and lets
            // mac80211 filter them.
            (crate::wsm::MIB_ID_GROUP_ADDRESSES_TABLE, _) => true,
            // Written by upstream cw1200 and not acted on yet: slot time (the
            // join programs 9 us), HT protection, the keep-alive period
            // (mac80211 monitors the
            // connection) and the rate for firmware-generated frames.
            (0x0003 | 0x100f | 0x1021 | 0x1025, _) => true,
            (crate::wsm::MIB_ID_TEMPLATE_FRAME, data) => self.retain_template(data),
            _ => false,
        }
    }
}

pub(crate) fn sdd_element(sdd: &[u8], id: u8) -> Option<&[u8]> {
    let mut remaining = sdd;
    while remaining.len() >= 2 {
        let element_id = remaining[0];
        let length = usize::from(remaining[1]);
        let end = 2 + length;
        if end > remaining.len() {
            return None;
        }
        if element_id == id {
            return Some(&remaining[2..end]);
        }
        remaining = &remaining[end..];
    }
    None
}

fn tx_power_profile_maximum(sdd: &[u8], element_id: u8) -> Option<i32> {
    let element = sdd_element(sdd, element_id)?;
    // Annotated SDD callback `0x00017626` copies eleven little-endian u16
    // entries from TLVs e3/e4 to runtime profiles at 0x040034b0 and +0x92.
    // `phy_get_tx_power_range` reads entry ten at profile offsets 0x14/0xa6.
    let bytes: [u8; 2] = element.get(20..22)?.try_into().ok()?;
    Some(i32::from(u16::from_le_bytes(bytes)))
}

fn tx_power_ranges(sdd: &[u8]) -> Option<[TxPowerRange; 2]> {
    let range = |element| {
        Some(TxPowerRange {
            min_power_level: -160,
            max_power_level: tx_power_profile_maximum(sdd, element)?,
            stepping: 0,
        })
    };
    Some([range(0xe3)?, range(0xe4)?])
}

fn validate_sdd(mut data: &[u8]) -> Result<(), ConfigurationError> {
    while !data.is_empty() {
        if data.len() < 2 {
            return Err(ConfigurationError::MalformedSdd);
        }
        let length = usize::from(data[1]);
        let end = 2 + length;
        if end > data.len() {
            return Err(ConfigurationError::MalformedSdd);
        }
        data = &data[end..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template_mib(frame_type: u8, frame_len: usize) -> [u8; 64] {
        let mut mib = [0; 64];
        mib[0] = frame_type;
        mib[2..4].copy_from_slice(&(frame_len as u16).to_le_bytes());
        mib
    }

    #[test]
    fn elements_are_replaced_in_place_or_appended_and_dtim_counts_down() {
        let mut configuration = Configuration::new();
        // SSID "ab", TIM (count 0, period 3, control 0, bitmap 0), DS channel 11.
        let elements = [0, 2, b'a', b'b', 5, 4, 0, 3, 0, 0, 3, 1, 11];
        let mut mib = template_mib(TEMPLATE_BEACON, 36 + elements.len());
        mib[40..40 + elements.len()].copy_from_slice(&elements);
        assert!(configuration.retain_interface_mib(crate::wsm::MIB_ID_TEMPLATE_FRAME, &mib[..53]));
        // A longer TIM (one more bitmap octet, multicast bit) and a new ERP.
        assert!(configuration.update_template_elements(
            1,
            2,
            &[5, 5, 0, 3, 1, 0, 2, 42, 1, 0, 0, 0]
        ));
        let beacon = configuration.template(TEMPLATE_BEACON).unwrap();
        assert_eq!(beacon[2..4], 53_u16.to_le_bytes());
        assert_eq!(
            beacon[40..],
            [0, 2, b'a', b'b', 5, 5, 0, 3, 1, 0, 2, 3, 1, 11, 42, 1, 0]
        );
        let beacons = [3, 4, 5, 6].map(|number| configuration.with_beacon(number, |mib| mib[46]));
        assert_eq!(
            beacons,
            [
                Some((0, true)),
                Some((2, false)),
                Some((1, false)),
                Some((0, true))
            ]
        );
        // Truncated elements change nothing.
        assert!(!configuration.update_template_elements(1, 1, &[5, 5, 0]));
        // No probe-response template: nothing to update.
        assert!(configuration.update_template_elements(2, 1, &[42, 1, 0]));
        assert!(configuration.template(TEMPLATE_PROBE_RESPONSE).is_none());
    }

    #[test]
    fn multicast_advertisement_is_only_on_dtim_beacons() {
        let mut configuration = Configuration::new();
        let mut mib = template_mib(TEMPLATE_BEACON, 42);
        // Nonzero bitmap offset: only control bit 0 may change on air.
        mib[40..46].copy_from_slice(&[5, 4, 0, 3, 0xa7, 0x12]);
        assert!(configuration.retain_interface_mib(crate::wsm::MIB_ID_TEMPLATE_FRAME, &mib[..46]));
        let beacons = [3, 4, 5, 6].map(|number| {
            configuration
                .with_beacon(number, |mib| (mib[42], mib[44]))
                .map(|((count, control), multicast)| (count, control, multicast))
        });
        assert_eq!(
            beacons,
            [
                Some((0, 0xa7, true)),
                Some((2, 0xa6, false)),
                Some((1, 0xa6, false)),
                Some((0, 0xa7, true))
            ]
        );
        // Formatting a non-DTIM must not forget the host's queued-group request.
        assert_eq!(configuration.template(TEMPLATE_BEACON).unwrap()[44], 0xa7);
    }

    #[test]
    fn beacon_copy_failure_and_host_updates_preserve_group_intent() {
        let mut configuration = Configuration::new();
        let mut mib = template_mib(TEMPLATE_BEACON, 42);
        mib[40..46].copy_from_slice(&[5, 4, 0, 3, 0xa7, 0x12]);
        assert!(configuration.retain_interface_mib(crate::wsm::MIB_ID_TEMPLATE_FRAME, &mib[..46]));
        // A failed/non-DTIM publication cannot consume the pending-group request.
        assert_eq!(
            configuration.with_beacon(4, |mib| {
                assert_eq!(mib[44], 0xa6);
                assert_eq!(mib[45], 0x12);
                false
            }),
            Some((false, false))
        );
        assert_eq!(configuration.template(TEMPLATE_BEACON).unwrap()[44], 0xa7);
        assert_eq!(
            configuration.with_beacon(6, |mib| mib[44]),
            Some((0xa7, true))
        );
        // An unrelated element update must not lose that request either.
        assert!(configuration.update_template_elements(1, 1, &[3, 1, 6]));
        assert_eq!(
            configuration.with_beacon(7, |mib| mib[44]),
            Some((0xa6, false))
        );
        assert_eq!(
            configuration.with_beacon(9, |mib| mib[44]),
            Some((0xa7, true))
        );
        // The host, not beacon formatting, clears the pending request.
        assert!(configuration.update_template_elements(1, 1, &[5, 4, 0, 3, 0xa6, 0x12]));
        assert_eq!(
            configuration.with_beacon(12, |mib| mib[44]),
            Some((0xa6, false))
        );
        // A new full template may request group traffic again.
        assert!(configuration.retain_interface_mib(crate::wsm::MIB_ID_TEMPLATE_FRAME, &mib[..46]));
        assert_eq!(
            configuration.with_beacon(13, |mib| mib[44]),
            Some((0xa6, false))
        );
        assert_eq!(
            configuration.with_beacon(15, |mib| mib[44]),
            Some((0xa7, true))
        );
    }

    #[test]
    fn beacon_without_a_complete_tim_is_not_patched() {
        let mut configuration = Configuration::new();
        assert_eq!(
            configuration.with_beacon(0, |_| panic!("no beacon")),
            None::<((), bool)>
        );
        for elements in [&[][..], &[5, 1, 0xff][..]] {
            let mut mib = template_mib(TEMPLATE_BEACON, 36 + elements.len());
            let len = 40 + elements.len();
            mib[40..len].copy_from_slice(elements);
            assert!(
                configuration.retain_interface_mib(crate::wsm::MIB_ID_TEMPLATE_FRAME, &mib[..len])
            );
            let result = configuration.with_beacon(0, |beacon| {
                assert_eq!(beacon, &mib[..len]);
            });
            assert_eq!(result, Some(((), false)));
        }
    }

    #[test]
    fn templates_are_kept_by_type_and_checked() {
        let mut configuration = Configuration::new();
        let probe = template_mib(TEMPLATE_PROBE_REQUEST, 30);
        let beacon = template_mib(TEMPLATE_BEACON, 40);
        let null = template_mib(2, 24);
        for mib in [&probe[..34], &beacon[..44], &null[..28]] {
            assert!(configuration.retain_interface_mib(crate::wsm::MIB_ID_TEMPLATE_FRAME, mib));
        }
        assert_eq!(
            configuration.template(TEMPLATE_PROBE_REQUEST),
            Some(&probe[..34])
        );
        assert_eq!(configuration.template(TEMPLATE_BEACON), Some(&beacon[..44]));
        assert_eq!(configuration.template(TEMPLATE_PROBE_RESPONSE), None);
        // The declared length must match; a beacon needs its fixed fields.
        for mib in [
            &beacon[..43],
            &template_mib(TEMPLATE_PROBE_RESPONSE, 30)[..34],
        ] {
            assert!(!configuration.retain_interface_mib(crate::wsm::MIB_ID_TEMPLATE_FRAME, mib));
        }
    }

    #[test]
    fn sdd_calibration_takes_present_elements_and_keeps_the_rest() {
        let sdd = [
            0xe3, 4, 0x10, 0x00, 0x20, 0x00, // profile-0 rate limits, two entries
            0x41, 4, 0xff, 0xff, 0x05, 0x00, // profile-1 RSSI pair
            0x43, 1, 0x07, // too short for a value: ignored
            0x22, 2, 0x34, 0x12, // gain coefficient slot 1
            0xec, 8, 2, 0, 1, 2, 3, 11, 12, 13, // two 3-byte channel records
            0xed, 6, 1, 0, 36, 5, 0, 0, // one 2-byte channel record
        ];
        let mut calibration = SddCalibration::ZERO;
        calibration.profiles[1].temperature_coefficient = 9;
        calibration.apply(&sdd);

        assert_eq!(calibration.profiles[0].rate_limits[..3], [0x10, 0x20, 0]);
        assert_eq!(calibration.profiles[1].rssi_coefficients, [-1, 5]);
        assert_eq!(calibration.profiles[1].temperature_coefficient, 9);
        assert_eq!(calibration.gain_coefficients, [0, 0x1234, 0, 0]);
        assert_eq!(calibration.profiles[0].channel_count, 2);
        assert_eq!(
            calibration.profiles[0].channel_records[..2],
            [[1, 2, 3], [11, 12, 13]]
        );
        assert_eq!(calibration.profiles[1].channel_count, 1);
        assert_eq!(calibration.profiles[1].channel_records[0], [36, 5, 0]);
    }

    #[test]
    fn protection_mibs_are_validated_and_vif_local() {
        use crate::tx::{MIB_NON_ERP_PROTECTION, MIB_RTS_THRESHOLD, ProtectionPolicy};
        let mut configuration = Configuration::new();
        assert!(configuration.retain_protection_mib(1, MIB_RTS_THRESHOLD, &300_u32.to_le_bytes()));
        assert!(configuration.retain_protection_mib(1, MIB_NON_ERP_PROTECTION, &[1, 0, 0, 0]));
        let expected = [
            ProtectionPolicy::DISABLED,
            ProtectionPolicy {
                rts_threshold: 300,
                non_erp: true,
            },
        ];
        assert_eq!(configuration.tx_protection(), expected);
        assert!(!configuration.retain_protection_mib(2, MIB_RTS_THRESHOLD, &[0; 4]));
        assert!(!configuration.retain_protection_mib(1, MIB_RTS_THRESHOLD, &[0; 3]));
        assert!(!configuration.retain_protection_mib(1, MIB_NON_ERP_PROTECTION, &[2, 0, 0, 0]));
        assert_eq!(configuration.tx_protection(), expected);
        assert!(configuration.retain_protection_mib(1, MIB_RTS_THRESHOLD, &0_u32.to_le_bytes()));
        assert_eq!(configuration.tx_protection()[1].rts_threshold, u32::MAX);
    }

    #[test]
    fn retains_configuration_and_parses_channel_calibration() {
        let request = ConfigurationRequest {
            max_tx_msdu_lifetime: 1,
            max_rx_lifetime: 2,
            rts_threshold: 3,
            dpd_version: 1,
            station_id: [0, 1, 2, 3, 4, 5],
            dpd_flags: 5,
            dpd_data: &[
                0xc5, 2, 0xc0, 0x5d, 0xfe, 2, 0, 0, 0x30, 10, 0xff, 0xff, 3, 0, 10, 0, 8, 0, 20, 0,
                0xec, 8, 2, 0, 3, 2, 4, 8, 5, 7,
            ],
            padded: false,
        };
        let mut configuration = Configuration::new();
        assert_eq!(configuration.station_id(), None);
        configuration.retain(&request).unwrap();
        assert_eq!(configuration.tx_protection()[0].rts_threshold, 3);
        configuration
            .retain(&ConfigurationRequest {
                rts_threshold: 0,
                ..request
            })
            .unwrap();
        assert_eq!(
            configuration
                .tx_protection()
                .map(|policy| policy.rts_threshold),
            [u32::MAX; 2]
        );
        assert_eq!(configuration.station_id(), Some(request.station_id));
        assert_eq!(sdd_element(request.dpd_data, 0xc5), Some(&[0xc0, 0x5d][..]));
        assert_eq!(sdd_element(request.dpd_data, 0xeb), None);

        let mut calibration = SddCalibration::ZERO;
        calibration.apply(request.dpd_data);
        assert_eq!(calibration.channel_threshold_correction(0x30, 2), Some(-1));
        assert_eq!(calibration.channel_threshold_correction(0x30, 6), Some(10));
        assert_eq!(calibration.channel_threshold_correction(0x30, 11), Some(20));
        assert_eq!(calibration.channel_threshold_correction(0x31, 6), None);
        assert_eq!(
            calibration.channel_calibration_values(6, 100),
            Some(ChannelCalibrationValues {
                first: 108,
                second: 116
            })
        );
        assert_eq!(
            calibration.mode0_channel_calibration(6),
            Some(Mode0ChannelCalibration {
                base: 10,
                first: 18,
                second: 26
            })
        );

        let mut power_sdd = [0_u8; 56];
        power_sdd[0] = 0xe3;
        power_sdd[1] = 26;
        power_sdd[22..24].copy_from_slice(&272_u16.to_le_bytes());
        power_sdd[28] = 0xe4;
        power_sdd[29] = 26;
        power_sdd[50..52].copy_from_slice(&212_u16.to_le_bytes());
        configuration
            .retain(&ConfigurationRequest {
                dpd_data: &power_sdd,
                ..request
            })
            .unwrap();
        assert_eq!(
            configuration.tx_power_ranges(),
            Some([
                TxPowerRange {
                    min_power_level: -160,
                    max_power_level: 272,
                    stepping: 0
                },
                TxPowerRange {
                    min_power_level: -160,
                    max_power_level: 212,
                    stepping: 0
                },
            ])
        );
    }

    #[test]
    fn retains_interface_mibs() {
        let mut configuration = Configuration::new();
        assert!(configuration.retain_interface_mib(
            crate::wsm::MIB_ID_DOT11_CURRENT_TX_POWER_LEVEL,
            &(-135_i32).to_le_bytes()
        ));
        assert_eq!(configuration.current_tx_power_tenths_dbm(), Some(-135));
        assert!(
            configuration
                .retain_interface_mib(crate::wsm::MIB_ID_BLOCK_ACK_POLICY, &[0x3f, 0, 0x0f, 0])
        );
        assert_eq!(
            (
                configuration.block_ack_tx_tids(),
                configuration.block_ack_rx_tids()
            ),
            (0x3f, 0x0f)
        );
        assert!(
            !configuration
                .retain_interface_mib(crate::wsm::MIB_ID_BLOCK_ACK_POLICY, &[0x3f, 1, 0x0f, 0])
        );
        assert!(configuration.retain_interface_mib(
            crate::wsm::MIB_ID_SET_ASSOCIATION_MODE,
            &[0x1f, 1, 0, 4, 7, 0, 0, 0]
        ));
        assert_eq!(configuration.mpdu_start_spacing(), 4);
        assert!(!configuration.retain_interface_mib(
            crate::wsm::MIB_ID_SET_ASSOCIATION_MODE,
            &[0x1f, 1, 0, 8, 7, 0, 0, 0]
        ));
        assert!(
            configuration.retain_interface_mib(crate::wsm::MIB_ID_PRIVATE_TX_BA_SESSION, &[3, 1])
        );
        assert!(
            configuration.retain_interface_mib(crate::wsm::MIB_ID_PRIVATE_TX_BA_SESSION, &[3, 0])
        );
    }
}
