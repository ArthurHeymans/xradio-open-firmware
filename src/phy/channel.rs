//! Channel programming: the PLL, TX power and gain, channel measurement and
//! the transition between channels.

use super::*;

// Vendor references to 0x040099f8, 0x040099fc, and 0x04009a06 are confined
// to `phy_apply_cfg_if_channel_match`, whose counterpart here is
// `program_channel_pll`. The adjacent force flag at 0x04009a08 has additional
// calibration and RF consumers and deliberately remains fixed.
fn cached_pll_channel() -> u16 {
    self::state().pll_channel.get()
}

pub(super) fn cached_pll_divider() -> (u32, u32) {
    (
        self::state().pll_integer.get(),
        self::state().pll_fractional.get(),
    )
}

fn set_cached_pll_divider(integer: u32, fractional: u32, channel: u16) {
    let phy = self::state();
    phy.pll_integer.set(integer);
    phy.pll_fractional.set(fractional);
    phy.pll_channel.set(channel);
}

fn set_channel_power_limits(low_rate: i16, high_rate: i16) {
    self::state().power_limit_low_rate.set(low_rate);
    self::state().power_limit_high_rate.set(high_rate);
}

fn channel_power_limit(high_rate: bool) -> i16 {
    let phy = self::state();
    if high_rate {
        phy.power_limit_high_rate.get()
    } else {
        phy.power_limit_low_rate.get()
    }
}

tock_registers::register_bitfields! {
    u32,
    PllValue [
        /// Fractional part, in units of 2⁻²¹.
        FRACTION OFFSET(0) NUMBITS(21) [],
        /// Integer part.
        INTEGER OFFSET(21) NUMBITS(11) [],
    ],
    PllControl [
        /// Cleared and set again to restart the PLL with a new divider.
        RESTART OFFSET(13) NUMBITS(1) [],
    ],
}

/// Complete `0x18f2c -> 0x1838c` PLL publication sequence. It remains
/// intentionally detached from scan execution until the surrounding
/// calibration path is ready.
pub fn commit_channel_pll(register: u32, mode: u8, extended_settle: bool) {
    const PLL_VALUE: usize = 0x0abc_00b4;
    const PLL_CONTROL: usize = 0x0abc_00b8;
    const MODE_CONTROL: usize = 0x0abc_0084;
    const BAND_CONTROL: usize = 0x0abc_0050;

    write_u32(PLL_VALUE, register);
    let control = reg_read(PLL_CONTROL) & !0x2000;
    write_u32(PLL_CONTROL, control);
    delay_units(1);
    write_u32(PLL_CONTROL, control | 0x2000);
    if extended_settle {
        delay_units(0x78);
    }
    if mode == 0 {
        write_u32(MODE_CONTROL, reg_read(MODE_CONTROL) & !0x40);
        write_u32(BAND_CONTROL, reg_read(BAND_CONTROL) & !0x200);
        delay_units(10);
        write_u32(MODE_CONTROL, reg_read(MODE_CONTROL) | 0x40);
        write_u32(BAND_CONTROL, reg_read(BAND_CONTROL) | 0x200);
        delay_units(10);
    }
}

pub fn measurement_path_value(band_mode: u8) -> u32 {
    if band_mode == 1 { 0 } else { 0x0ebf }
}

/// Result scaling from the mode-zero round in vendor `0x19f8e`.
pub fn scale_channel_measurement(raw_counter: u32, first: i16, second: i16) -> Option<i32> {
    if first == 0 {
        return None;
    }
    let numerator = (i64::from(raw_counter) * 0x47 - i64::from(second)) * 1000;
    Some((numerator / i64::from(first)) as i32)
}

/// Vendor `0x19f8e` accepts mode-zero results from 41000 through 58988 and
/// otherwise uses fallback `0x9470` (38000).
pub fn select_mode0_measurement(scaled: i32) -> (u32, bool) {
    if (41_000..=58_988).contains(&scaled) {
        (scaled as u32, true)
    } else {
        (0x9470, false)
    }
}

/// Mode-zero trigger word and completion predicate from vendor `0x19f8e`.
pub const MODE0_MEASUREMENT_CONTROL: u32 = 0x0000_e080;

pub fn channel_measurement_ready(control: u32) -> bool {
    control & 0x20 == 0
}

/// Complete MMIO sequence from vendor `0x17e92`.
pub fn set_channel_measurement_path(enabled: bool, band_mode: u8) {
    const MAC_CONTROL: usize = 0x0ab8_0c38;
    let mut control = reg_read(MAC_CONTROL);
    if enabled {
        control |= 0x1800;
        write_u32(0x0abb_81a0, 0);
        write_u32(0x0ab8_006c, measurement_path_value(band_mode));
        write_u32(0x0ab8_0068, 0);
    } else {
        control &= !0x1800;
    }
    write_u32(MAC_CONTROL, control);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelMeasurementSnapshot {
    abb800c: u32,
    abc0004: u32,
    abc0034: u32,
    abc0050: u32,
    abc006c: u32,
    abc0084: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelMeasurementError {
    InvalidCalibration,
    NotReady,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalibrationSampleError {
    Timeout,
}

/// Register save-and-override envelope from vendor `0x1a1fc`, followed by the
/// `0x19f8e` save of `0x0abb800c`.
pub fn begin_channel_measurement() -> ChannelMeasurementSnapshot {
    let snapshot = ChannelMeasurementSnapshot {
        abb800c: reg_read(0x0abb_800c),
        abc0004: reg_read(0x0abc_0004),
        abc0034: reg_read(0x0abc_0034),
        abc0050: reg_read(0x0abc_0050),
        abc006c: reg_read(0x0abc_006c),
        abc0084: reg_read(0x0abc_0084),
    };

    write_u32(0x0abb_8004, reg_read(0x0abb_8004) | 0x800);
    write_u32(0x0abc_0084, reg_read(0x0abc_0098) | 0x0700_0200);
    write_u32(0x0abc_006c, reg_read(0x0abc_0080));
    write_u32(0x0abc_0050, reg_read(0x0abc_0064));
    write_u32(0x0abc_0034, reg_read(0x0abc_0048) | 0x101);
    write_u32(0x0abc_0004, snapshot.abc0004 | 2);
    snapshot
}

/// Exact register restoration tail from vendor `0x19f8e`.
pub fn end_channel_measurement(snapshot: ChannelMeasurementSnapshot) {
    write_u32(0x0abc_0004, snapshot.abc0004);
    write_u32(0x0abc_0084, snapshot.abc0084);
    write_u32(0x0abc_006c, snapshot.abc006c);
    write_u32(0x0abc_0050, snapshot.abc0050);
    write_u32(0x0abc_0034, snapshot.abc0034);
    write_u32(0x0abb_800c, snapshot.abb800c);
}

/// Complete mode-zero measurement round from vendor `0x19f8e`. Like the
/// vendor function, a not-ready result returns before restoring the temporary
/// register envelope. Callers must treat that result as a fatal channel setup
/// failure rather than continuing with partially restored hardware.
pub fn run_mode0_channel_measurement(
    first: i16,
    second: i16,
) -> Result<u32, ChannelMeasurementError> {
    if first == 0 {
        return Err(ChannelMeasurementError::InvalidCalibration);
    }

    let snapshot = begin_channel_measurement();
    write_u32(0x0abb_800c, 0);
    write_u32(0x0abb_800c, MODE0_MEASUREMENT_CONTROL);
    delay_timer_ticks(10);
    if !channel_measurement_ready(reg_read(0x0abb_800c)) {
        return Err(ChannelMeasurementError::NotReady);
    }
    delay_timer_ticks(10);

    let raw = reg_read(0x0abb_82f0);
    let scaled = scale_channel_measurement(raw, first, second)
        .ok_or(ChannelMeasurementError::InvalidCalibration)?;
    let (selected, _) = select_mode0_measurement(scaled);
    delay_timer_ticks(1);
    end_channel_measurement(snapshot);
    Ok(selected)
}

#[derive(Clone, Copy, Debug, Eq, Immutable, IntoBytes, PartialEq)]
#[repr(C)]
pub struct ChannelProgramRequestWire {
    pub operation: u8,
    pub option: u8,
    pub control: U16,
    pub channel: U16,
    pub link_count: u8,
    pub link_id: u8,
}

/// Exact eight-byte request assembled by vendor scan callback `0x14304` before
/// entering the large channel programmer at `0xf802`.
pub fn build_scan_channel_program_request(
    band: u8,
    flags: u8,
    channel: u16,
) -> ChannelProgramRequestWire {
    ChannelProgramRequestWire {
        operation: 0,
        option: (flags & 4) >> 2,
        control: channel_control_word(band, 0, channel).into(),
        channel: channel.into(),
        link_count: 1,
        link_id: 2,
    }
}

/// Vendor `0xfdfa` channel-control word used by scan callback `0x14304`.
pub fn channel_control_word(band: u8, bandwidth_mode: u8, channel: u16) -> u16 {
    let mut control = if band == 1 { 0x26 } else { 0x17 };
    control |= match bandwidth_mode {
        0 => 0x100,
        1 => 0x200,
        2 => 0x400,
        _ => 0,
    };
    if channel & 0x100 != 0 {
        control |= 0x40;
    }
    control
}

/// Vendor `0x124c0` classifier stored at `0x04003c20` before `0xf802`.
pub fn channel_control_gate(control: u16) -> u32 {
    if (!control & 0x22) == 0 || (control & 0x7f) == 0x12 {
        0x40
    } else {
        1
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelTunePlan {
    pub phy_mode: u8,
    pub recalibrate: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModeTransitionAction {
    ReturnUnchanged,
    ClearChannelPending,
    Reinitialize,
}

/// Pure leading decisions from vendor `0xf78c`, called by `0xf802` with the
/// request channel. The resulting mode and recalibration flag are passed to
/// `0x16dd6`, the intact radio-channel transition entry.
pub fn channel_tune_plan(operation: u8, control: u16) -> ChannelTunePlan {
    let phy_mode = if control & 0x20 != 0 {
        3
    } else if control & 0x11 == 0x11 {
        2
    } else {
        0
    };
    let recalibrate = phy_mode != 3 && !matches!(operation, 1 | 2 | 4);
    ChannelTunePlan {
        phy_mode,
        recalibrate,
    }
}

/// Leading branch structure of vendor `0x16b2e`. In the normal mode-zero
/// startup state, a scan request remains in PHY mode 2 and skips the expensive
/// mode/table reinitialization path.
pub fn mode_transition_action(
    current_mode: u8,
    auxiliary_mode_active: bool,
    requested_mode: u8,
    force: bool,
) -> ModeTransitionAction {
    if current_mode == requested_mode && !force {
        if auxiliary_mode_active {
            ModeTransitionAction::ReturnUnchanged
        } else {
            ModeTransitionAction::ClearChannelPending
        }
    } else {
        ModeTransitionAction::Reinitialize
    }
}

/// Vendor `0x1682a` channel-to-frequency mapping for PHY band/mode byte zero.
/// The returned unit is kHz; channel 14 uses its dedicated 2484 MHz value.
pub fn channel_frequency_khz_2ghz(channel: u16) -> u32 {
    if channel < 14 {
        (2407 + u32::from(channel) * 5) * 1000
    } else {
        2484 * 1000
    }
}

/// Arithmetic from vendor `0x17224`, which programs `0x0ab88020` during an RF
/// channel transition. `reference_clock_khz` is the output of `0x1682a`.
pub fn channel_measurement_timing(mode: u8, reference_clock_khz: u32) -> Option<i32> {
    let clock_mhz = reference_clock_khz / 1000;
    if clock_mhz == 0 {
        return None;
    }
    let periods = if mode == 0 { 20 } else { 10 };
    Some(-(((periods << 14) / clock_mhz) as i32))
}

/// Signed MHz offset cached by vendor `0x19928` after channel calibration.
/// Mode zero is relative to channel 7's 2442 MHz center frequency.
pub fn channel_frequency_offset_mhz(mode: u8, frequency_khz: u32) -> i16 {
    let nominal_mhz = if mode == 0 { 0x098a } else { 0x157c };
    ((frequency_khz / 1000) as i32 - nominal_mhz) as i16
}

/// Live `phy_program_clock_divisor` publication.
///
/// The vendor channel state at `0x0400994c` must be initialized.
pub fn program_channel_measurement_timing() -> Option<i32> {
    let mode = self::state().profile.get();
    let frequency_khz = self::state().frequency_khz.get();
    let timing = channel_measurement_timing(mode, frequency_khz)?;
    write_u32(0x0ab8_8020, timing as u32);
    Some(timing)
}

/// Live `phy_set_freq_offset` publication.
///
/// The vendor channel and PHY software states must be initialized.
pub fn publish_channel_frequency_offset() -> i16 {
    let mode = self::state().profile.get();
    let frequency_khz = self::state().frequency_khz.get();
    let offset = channel_frequency_offset_mhz(mode, frequency_khz);
    let current = self::state().state_scale.get();
    if current != i32::from(offset) {
        self::state().state_scale.set(i32::from(offset));
    }
    offset
}

/// Exact enable/disable writes from annotated `phy_agc_enable`.
///
/// The AGC and MAC/PHY register banks must be enabled.
pub fn set_phy_agc_enabled(enabled: bool) {
    let current = reg_read(0x0ab8_0c38);
    let updated = if enabled {
        write_u32(0x0abb_81a0, 0);
        let profile = self::state().profile.get();
        write_u32(0x0ab8_006c, if profile == 1 { 0 } else { 0x0ebf });
        write_u32(0x0ab8_0068, 0);
        current | 0x1800
    } else {
        current & !0x1800
    };
    write_u32(0x0ab8_0c38, updated);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelCalibrationRequirement {
    AlreadyValid,
    RunCalibration,
    UnsupportedProfile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelPllError {
    UnsupportedProfile,
    InvalidReference,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TemperatureMeasurement {
    pub value: i32,
    pub accepted: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemperatureMeasurementError {
    HardwareFaultNoRestore,
    InvalidCalibrationNoRestore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelPowerError {
    InvalidThresholdTable,
    InvalidRateTable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GainProgrammingError {
    InvalidProfile,
    InvalidDivisor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GainComputationInput {
    pub(super) rate: u8,
    pub(super) first_limit: i32,
    pub(super) second_limit: i32,
    pub(super) rate_base: i32,
    pub(super) gain_coefficient_a: i32,
    pub(super) gain_coefficient_b: i32,
    pub(super) rssi_rate_scale: i32,
    pub(super) rssi_temperature_coefficient: i32,
    pub(super) rssi_divisor_coefficient: i32,
    pub(super) rssi_multiplier_coefficient: i32,
    pub(super) rssi_denominator: i32,
    pub(super) rssi_offset: i32,
    pub(super) measured_a: i32,
    pub(super) measured_b: i32,
    pub(super) analog_enabled: i32,
    pub(super) analog_word_2c: u32,
    pub(super) analog_word_30: u32,
    pub(super) state_scale: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct GainComputationResult {
    pub(super) selected_power: i32,
    pub(super) gain_code: u16,
    pub(super) rssi_value: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChannelTransitionError {
    Pll(ChannelPllError),
    InvalidTiming,
    Temperature(TemperatureMeasurementError),
    Calibration(IqCalibrationHardwareError),
    Power(ChannelPowerError),
    Gain(GainProgrammingError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelTransitionResult {
    pub divider: PllDivider,
    pub timing: i32,
    pub temperature: TemperatureMeasurement,
    pub calibration_ran: bool,
    pub threshold: i16,
    pub first_tx_power: i16,
    pub second_tx_power: i16,
    pub frequency_offset: i16,
}

/// Channel form of `phy_apply_cfg_pair`, including its cached
/// integer/fractional pair and vendor PLL restart. Profile zero uses the
/// 2407/2484 MHz mapping and multiplier 1250; profile one uses dispatcher case
/// zero's 5000 MHz base and multiplier 1000.
///
/// The vendor channel state and PLL register bank must be initialized.
pub fn program_channel_pll(channel: u16) -> Result<PllDivider, ChannelPllError> {
    let profile = self::state().profile.get();
    let (frequency_khz, multiplier) = match profile {
        0 => (channel_frequency_khz_2ghz(channel), 1250),
        1 => ((5000 + u32::from(channel & 0xff) * 5) * 1000, 1000),
        _ => return Err(ChannelPllError::UnsupportedProfile),
    };
    self::state().frequency_khz.set(frequency_khz);
    let reference = self::state().reference_khz.get();
    let correction = i32::from(0i16);
    let correction = i64::from(reference)
        .wrapping_mul(i64::from(correction))
        .wrapping_div(1000);
    let divisor = i64::from(reference)
        .wrapping_mul(1000)
        .wrapping_add(correction);
    if divisor <= 0 || divisor > i64::from(u32::MAX) {
        return Err(ChannelPllError::InvalidReference);
    }
    let cached_channel = cached_pll_channel();
    // The vendor extended-settle flag is set at startup and never cleared.
    let cache_forced = true;
    let divider = if cached_channel == channel && !cache_forced {
        let (integer, fractional) = cached_pll_divider();
        PllDivider {
            integer,
            fractional,
            register: integer.wrapping_shl(21) | fractional,
        }
    } else {
        let divider = pll_divider(frequency_khz, multiplier, divisor as u32)
            .ok_or(ChannelPllError::InvalidReference)?;
        set_cached_pll_divider(divider.integer, divider.fractional, channel);
        divider
    };
    commit_channel_pll(divider.register, profile, cache_forced);
    Ok(divider)
}

/// Parameter-zero path of annotated `rf_measure_temp_and_vbat`. The two fatal
/// exits intentionally skip restoration, matching the vendor routine.
///
/// The PHY state and RF/PHY register banks must be initialized.
pub fn measure_temperature_primary() -> Result<TemperatureMeasurement, TemperatureMeasurementError>
{
    write_u32(0x0abb_8004, reg_read(0x0abb_8004) | 0x800);
    let saved_abc0084 = reg_read(0x0abc_0084);
    write_u32(0x0abc_0084, reg_read(0x0abc_0098) | 0x0700_0200);
    let saved_abc006c = reg_read(0x0abc_006c);
    write_u32(0x0abc_006c, reg_read(0x0abc_0080));
    let saved_abc0050 = reg_read(0x0abc_0050);
    write_u32(0x0abc_0050, reg_read(0x0abc_0064));
    let saved_abc0034 = reg_read(0x0abc_0034);
    write_u32(0x0abc_0034, reg_read(0x0abc_0048) | 0x101);
    let saved_abc0004 = reg_read(0x0abc_0004);
    write_u32(0x0abc_0004, saved_abc0004 | 2);
    let saved_abb800c = reg_read(0x0abb_800c);
    write_u32(0x0abb_800c, 0x0000_e080);
    delay_units(10);
    let status = reg_read(0x0abb_800c);
    if status & 0x20 != 0 {
        return Err(TemperatureMeasurementError::HardwareFaultNoRestore);
    }
    delay_units(10);
    let denominator = i32::from(self::state().temperature_denominator.get());
    if denominator == 0 {
        return Err(TemperatureMeasurementError::InvalidCalibrationNoRestore);
    }
    let raw = (reg_read(0x0abb_82d8) as i32).wrapping_mul(0x47);
    let offset = i32::from(self::state().temperature_offset.get());
    let converted = raw
        .wrapping_sub(offset)
        .wrapping_mul(1000)
        .wrapping_div(denominator);
    let accepted = (converted.wrapping_add(-41_000) as u32) <= 17_900;
    let value = if accepted { converted } else { 38_000 };
    if accepted {
        self::state().temperature.set(converted as u32 as i32);
    }
    delay_units(1);

    write_u32(0x0abc_0004, saved_abc0004);
    write_u32(0x0abc_0084, saved_abc0084);
    write_u32(0x0abc_006c, saved_abc006c);
    write_u32(0x0abc_0050, saved_abc0050);
    write_u32(0x0abc_0034, saved_abc0034);
    write_u32(0x0abb_800c, saved_abb800c);
    Ok(TemperatureMeasurement { value, accepted })
}

/// Inspired by `phy_txpower_from_rate_table`.
///
/// The SDD-derived rate-table pointer in PHY software state must be valid.
pub fn channel_tx_power_from_rate_table(
    channel: u8,
    second_column: bool,
) -> Result<i16, ChannelPowerError> {
    let table = self::calibration().profiles[0];
    let records = &table.channel_records[..usize::from(table.channel_count)];
    let mut selected = 0_usize;
    for (index, record) in records.iter().enumerate() {
        let threshold = record[0];
        if channel <= threshold {
            selected = if index != 0 && channel < threshold {
                index - 1
            } else {
                index
            };
            break;
        }
        selected = index;
    }
    let column = if second_column { 1 } else { 2 };
    let encoded = i32::from(records.get(selected).map_or(0, |record| record[column]));
    let base = i32::from(self::state().threshold.get());
    Ok(base.wrapping_add(encoded.wrapping_mul(4)) as i16)
}

/// Publish threshold and both channel TX-power values to the PHY software
/// state used by the gain-table path.
///
/// Both vendor SDD-derived tables and PHY software state must be initialized.
pub fn publish_channel_power(channel: u8) -> Result<(i16, i16, i16), ChannelPowerError> {
    let profile = self::state().profile.get();
    let threshold_id = match profile {
        0 => 0x30,
        1 => 0x31,
        _ => return Err(ChannelPowerError::InvalidThresholdTable),
    };
    let threshold = self::calibration()
        .channel_threshold_correction(threshold_id, u16::from(channel))
        .ok_or(ChannelPowerError::InvalidThresholdTable)?;
    self::state().threshold.set(threshold as u16 as i16);
    let first = channel_tx_power_from_rate_table(channel, false)?;
    let second = channel_tx_power_from_rate_table(channel, true)?;
    set_channel_power_limits(first, second);
    Ok((threshold, first, second))
}

pub(super) fn gain_div(numerator: i32, denominator: i32) -> Result<i32, GainProgrammingError> {
    numerator
        .checked_div(denominator)
        .ok_or(GainProgrammingError::InvalidDivisor)
}

/// Vendor logarithmic approximation at 0x1961c. Arithmetic deliberately
/// wraps at 32 bits, matching the Thumb `adds`/`muls` implementation.
pub(super) fn gain_index_from_value(mut value: i32) -> i32 {
    let mut result = 0x1_0000i32;
    for (threshold, action) in [
        (-363_408, 0u8),
        (-181_704, 1),
        (-90_852, 2),
        (-45_426, 3),
        (-26_573, 4),
        (-14_623, 5),
        (-7_719, 6),
        (-3_973, 7),
        (-2_017, 8),
    ] {
        let reduced = value.wrapping_add(threshold);
        if reduced >= 0 {
            result = match action {
                0 => 0x100_0000,
                1 => result.wrapping_shl(4),
                2 => result.wrapping_shl(2),
                3 => result.wrapping_shl(1),
                4 => result.wrapping_add(result >> 1),
                5 => result.wrapping_add(result >> 2),
                6 => result.wrapping_add(result >> 3),
                7 => result.wrapping_add(result >> 4),
                _ => result.wrapping_add(result >> 5),
            };
            value = reduced;
        }
    }
    for (threshold, shift) in [(0x3f8, 6), (0x1fe, 7)] {
        let reduced = value.wrapping_sub(threshold);
        if reduced >= 0 {
            result = result.wrapping_add(result >> shift);
            value = reduced;
        }
    }
    for (bit, shift) in [
        (8, 8),
        (7, 9),
        (6, 10),
        (5, 11),
        (4, 12),
        (3, 13),
        (2, 14),
        (1, 15),
        (0, 16),
    ] {
        if value & (1 << bit) != 0 {
            result = result.wrapping_add(result >> shift);
        }
    }
    result
}

pub(super) fn gain_code(index: i32) -> u16 {
    let table_index = (index.clamp(4, 32) as u32) >> 2;
    (1u16 << (table_index + 2)).wrapping_sub(4)
}

pub(super) fn compute_gain_entry(
    input: GainComputationInput,
) -> Result<GainComputationResult, GainProgrammingError> {
    let measured_term = input.measured_a.wrapping_mul(0x580) >> 16;
    let first = gain_div(
        input
            .gain_coefficient_a
            .wrapping_mul(measured_term.wrapping_sub(845))
            >> 4,
        10_000,
    )?;
    let analog_value = if input.analog_enabled == 0 {
        -80
    } else {
        let magnitude = (input.analog_word_2c & 0x7ff) as i32;
        if input.analog_word_2c & 0x800 != 0 {
            -magnitude
        } else {
            magnitude
        }
    };
    let second_input = 480i32.wrapping_sub(
        (9005i32.wrapping_mul(56_445i32.wrapping_sub(input.measured_b)) >> 16)
            .wrapping_sub(analog_value),
    );
    let second = gain_div(input.gain_coefficient_b.wrapping_mul(second_input), 10_000)?;
    let lookup = first
        .wrapping_add(second)
        .wrapping_add(input.rate_base)
        .wrapping_sub(48);
    let selected_power = input.first_limit.min(input.second_limit.min(lookup));

    let raw_gain_input = ((input.analog_word_2c & 0x0fff_ffff) >> 12) as i32;
    let combined = ((input.analog_word_30 & 0xfff) << 4) | (input.analog_word_2c >> 28);
    let signed_magnitude = ((input.analog_word_2c & 0x7ff) << 12) as i32;
    let mut analog_offset = if input.analog_word_2c & 0x800 != 0 {
        -signed_magnitude
    } else {
        signed_magnitude
    };
    let mut analog_normalization = combined as i32;
    if input.analog_enabled == 0 || combined.wrapping_sub(0xf00) >= 0x2101 {
        // The second branch intentionally retains `raw_gain_input`.
        analog_offset = -327_680;
        analog_normalization = 0x2000;
    }

    let base_gain = gain_index_from_value(7545i32.wrapping_mul(raw_gain_input) >> 8);
    let temperature_input = 9005i32
        .wrapping_mul(56_445i32.wrapping_sub(input.measured_b))
        .wrapping_sub(analog_offset.wrapping_mul(16))
        .wrapping_sub(analog_normalization.wrapping_mul(4096));
    let temperature = gain_div(
        input
            .rssi_temperature_coefficient
            .wrapping_mul(temperature_input)
            >> 4,
        1000,
    )?;
    let combined_gain = base_gain.wrapping_add(temperature);
    let scaled_a = gain_div(
        combined_gain.wrapping_mul(input.rssi_multiplier_coefficient),
        100,
    )?;
    let scaled_b = gain_div(
        combined_gain
            .wrapping_mul(input.rssi_divisor_coefficient)
            .wrapping_mul(input.state_scale),
        10_000,
    )?;
    let target = selected_power.wrapping_add(if input.rate < 2 { 64 } else { 48 });
    let target_gain = gain_index_from_value(7545i32.wrapping_mul(target) >> 4);
    let ratio = gain_div(target_gain.wrapping_shl(8), scaled_a.wrapping_add(scaled_b))?;
    let ratio = gain_div(
        ratio
            .wrapping_mul(1000)
            .wrapping_add(input.rssi_offset.wrapping_shl(8)),
        input.rssi_denominator,
    )?;
    let scaled = gain_div(ratio.wrapping_mul(100), input.rssi_rate_scale)?.max(0);
    let mut index = (scaled >> 8) & 0xfffc;
    if scaled & 0x3ff != 0 {
        index = index.wrapping_add(4);
    }
    index = index.clamp(4, 32);
    let rssi_value = ((((gain_div(scaled.wrapping_mul(input.rssi_rate_scale), index)? as u32)
        & 0x00ff_ffff)
        >> 8)
        .max(100)) as u16;

    Ok(GainComputationResult {
        selected_power,
        gain_code: gain_code(index),
        rssi_value,
    })
}

pub(super) fn gain_computation_input(
    profile: u8,
    rate: u8,
    first_limit: i32,
    second_limit: i32,
) -> Result<GainComputationInput, GainProgrammingError> {
    if profile > 1 {
        return Err(GainProgrammingError::InvalidProfile);
    }
    let profile_index = usize::from(profile);
    let rate = rate.min(10);
    let coefficient_index = profile_index * 2;
    let sdd = self::calibration();
    let bank = &sdd.profiles[profile_index];
    let phy = self::state();
    Ok(GainComputationInput {
        rate,
        first_limit,
        second_limit,
        rate_base: i32::from(bank.rate_limits[usize::from(rate)]),
        gain_coefficient_a: i32::from(sdd.gain_coefficients[coefficient_index]),
        gain_coefficient_b: i32::from(sdd.gain_coefficients[coefficient_index + 1]),
        rssi_rate_scale: i32::from(bank.rssi_rate_scales[usize::from(rate)]),
        rssi_temperature_coefficient: i32::from(bank.temperature_coefficient),
        rssi_divisor_coefficient: i32::from(bank.conversion[0]),
        rssi_multiplier_coefficient: i32::from(bank.conversion[1]),
        rssi_denominator: i32::from(bank.rssi_coefficients[0]),
        rssi_offset: i32::from(bank.rssi_coefficients[1]),
        measured_a: TEMPERATURE_FALLBACK,
        measured_b: phy.temperature.get(),
        analog_enabled: i32::from(crate::platform::remap_windows_present()),
        analog_word_2c: crate::platform::remap_window(3),
        analog_word_30: crate::platform::remap_window(4),
        state_scale: phy.state_scale.get(),
    })
}

pub(super) fn encoded_gain_words(gain_code: u16, rssi_value: u16) -> (u32, u32) {
    let rssi = u32::from(rssi_value) & 0x3ff;
    let gain = u32::from(gain_code) & 0x3ff;
    let quotient = if rssi == 0 { 0x800 } else { 0x0003_2f53 / rssi };
    let companion = if quotient < 0x801 {
        0x1000 - quotient
    } else {
        0x800
    };
    ((gain << 10) | rssi, companion)
}

/// Vendor `phy_temp_compensate_all_slots`/`phy_program_gain_for_channel`
/// sequence. One entry is produced for every hardware rate slot before PHY
/// operation 1 and RX re-enable.
///
/// SDD state, analog state, and the gain MMIO banks must be initialized.
pub fn program_all_tx_gain_slots(power_tenths_dbm: i32) -> Result<(), GainProgrammingError> {
    if self::state().calibration_stage.get() < 2 {
        return Ok(());
    }
    let profile = self::state().profile.get();
    if profile > 1 {
        return Err(GainProgrammingError::InvalidProfile);
    }
    let requested_offset = gain_div(power_tenths_dbm.wrapping_shl(4), 10)?;
    let profile_index = usize::from(profile);
    for slot in 0..16usize {
        let rate = (slot as u8).min(10);
        let rate_limit =
            i32::from(self::calibration().profiles[profile_index].rate_limits[usize::from(rate)])
                .wrapping_add(requested_offset);
        let second_limit = rate_limit.min(i32::from(channel_power_limit(rate > 1)));
        let result = compute_gain_entry(gain_computation_input(
            profile,
            rate,
            requested_offset,
            second_limit,
        )?)?;

        let (entry, companion) = encoded_gain_words(result.gain_code, result.rssi_value);
        write_u32(0x0abb_801c + slot * 4, entry);
        write_u32(0x0abb_8400 + slot * 4, companion);
    }
    Ok(())
}

/// Prepare the profile-specific correction cache used by
/// `phy_set_channel_full` before its mode-2 calibration call.
///
/// The vendor channel state and correction bank must be initialized.
pub fn prepare_channel_calibration_cache() -> ChannelCalibrationRequirement {
    let profile = self::state().profile.get();
    let ready = match profile {
        0 => &self::state().profile0_ready,
        1 => &self::state().profile1_ready,
        _ => return ChannelCalibrationRequirement::UnsupportedProfile,
    };
    if ready.get() != 0 {
        return ChannelCalibrationRequirement::AlreadyValid;
    }
    let default_correction = if profile == 0 { 0x0ece_0000 } else { 0 };
    for index in 0..16_usize {
        write_u32(0x0abb_8068 + index * 4, default_correction);
    }
    ChannelCalibrationRequirement::RunCalibration
}

/// Record the channel associated with a newly valid profile cache.
///
/// The vendor channel state must be initialized.
pub fn record_calibrated_channel(_channel: u16) {
    match self::state().profile.get() {
        0 if self::state().profile0_ready.get() == 1 => (),
        1 if self::state().profile1_ready.get() == 1 => (),
        _ => {}
    }
}

pub(super) fn run_mode_calibration(
    calibration_max_polls: u32,
) -> Result<(), IqCalibrationHardwareError> {
    let _ = run_iq_calibration_core(true, calibration_max_polls)?;
    run_dynamic_mode_calibration();
    Ok(())
}

/// Keep the dynamic-IQ phase out of the preceding IQ calibration frame. The
/// recovered vendor sequence is unchanged, but sequential temporary sets no
/// longer occupy the system stack simultaneously.
#[inline(never)]
pub(super) fn run_dynamic_mode_calibration() {
    // Fixed mode-zero arguments assembled by `rf_apply_channel_settings(0,
    // 0x07ff0110)` at 0x1899c. At 0x18a54 the vendor stores 12 in snapshot
    // offset 0x290; `rf_save_band_regs` uses that field as the mode-zero index
    // into the halfword table at 0x04000dd0. The DFT phase seeds and all search
    // steps likewise come directly from the stack image built there.
    let control_configuration = 0x07ff_0110_u32;
    // The vendor table is not copied into the Rust image, so the value mode 12
    // reads (entry 12, `0x001c`) is a literal. Zero would select gain 0xc0
    // instead of 0x70c0 and suppress the dynamic-IQ calibration tone.
    let table_value = 0x001c;
    let sample_width_shift = self::state().sample_width.get() as u8;
    let configuration = DynamicIqHardwareCalibrationConfiguration {
        alternate_profile: false,
        table_value,
        synth_frequency: 12,
        calibration_command: 3,
        seed: [-2, 5, -1, -1],
        common_step: 0x100,
        first_step: 0x100,
        second_step: 0x100,
        requested_passes: 0x10,
        configuration_flags: reg_read(0x0abb_8004),
        control_configuration,
        dft: DynamicIqDftConfiguration {
            mode: 0,
            first_phase_seed: 0x10,
            second_phase_seed: 0x0c,
            third_phase_seed: 8,
            sample_width_shift,
            capture_polls: 0x2710,
        },
    };
    let mut samples = [0_u32; 64];
    let _ = run_dynamic_iq_calibration_for_profile(configuration, &mut samples);
}

pub(super) fn begin_channel_transition(
    channel: u16,
    calibration_max_polls: u32,
) -> Result<ChannelTransitionResult, ChannelTransitionError> {
    crate::mac::prepare_scan_context(channel);
    crate::mac::program_before_scan_channel(channel);
    if !set_packet_receive_enabled(false, calibration_max_polls) {
        return Err(ChannelTransitionError::InvalidTiming);
    }
    let same_mode_channel = {
        self::state().phase.get() == 2
            && self::state().profile.get() == 0
            && self::state().channel.get() == channel
            && self::state().calibration_state.get() == 0
    };
    if !same_mode_channel {
        // First channel-switch path `thunk_16c92`: initialize the detector and
        // expanded AGC table before the requested channel operation.
        if crate::mac::low_mac().receive_state.get() != 4 {
            apply_first_channel_detector_state();
        }
        // `phy_cal_advance_stage` (0x16bfa) reapplies these four initialized
        // register lists before `phy_cal_set_flag`, whose mode-zero path runs
        // the four RF initialization stages before dispatching the band mode.
        for list in COMMON_INITIALIZATION_LISTS {
            apply_register_list(list);
        }
        // Scan rate configuration 0x0117 selects dispatcher mode 2 in
        // `phy_do_channel_switch` (0xf7fc). The first channel performs a
        // pre-calibration dispatch, RF initialization, then the requested
        // mode-2 dispatch.
        program_mode2_band_hardware();
        prepare_rf_mode0_stage();
        // Vendor `phy_init_once` keeps this outside its one-shot guard and
        // rebuilds the SDD-corrected AGC table after RF initialization.
        build_mode0_gain_tables();
        program_scan_receive_band();
        self::state().channel.set(channel);
    }
    let divider = program_channel_pll(channel).map_err(ChannelTransitionError::Pll)?;

    // The vendor runs a mode calibration here when its transition gate byte
    // is set; nothing ever sets it.
    let mut calibration_ran = false;

    let timing =
        program_channel_measurement_timing().ok_or(ChannelTransitionError::InvalidTiming)?;
    let temperature = measure_temperature_primary().map_err(ChannelTransitionError::Temperature)?;

    if !same_mode_channel
        && prepare_channel_calibration_cache() == ChannelCalibrationRequirement::RunCalibration
    {
        run_mode_calibration(calibration_max_polls).map_err(ChannelTransitionError::Calibration)?;
        record_calibrated_channel(channel);
        calibration_ran = true;
    }

    set_phy_agc_enabled(true);
    let (threshold, first_tx_power, second_tx_power) =
        publish_channel_power(channel as u8).map_err(ChannelTransitionError::Power)?;
    let frequency_offset = publish_channel_frequency_offset();
    // Linux normally supplies MIB 0x0006 before channel activation. Preserve
    // its standard 20 dBm default for early scan bring-up if that write has
    // not arrived yet; the vendor path consumes the same value in deci-dBm.
    let tx_power_tenths_dbm = self::state().tx_power_tenths_dbm.get().unwrap_or(200);
    program_all_tx_gain_slots(tx_power_tenths_dbm).map_err(ChannelTransitionError::Gain)?;
    // First return from vendor `phy_cal_run_step_timed`: state 1, followed by
    // a 120-tick cooperative settle interval.
    {
        self::state().calibration_state.set(0);
    };
    Ok(ChannelTransitionResult {
        divider,
        timing,
        temperature,
        calibration_ran,
        threshold,
        first_tx_power,
        second_tx_power,
        frequency_offset,
    })
}

pub(super) fn finish_channel_transition(
    calibration_max_polls: u32,
) -> Result<(), ChannelTransitionError> {
    publish_completed_receive_state();
    set_packet_receive_enabled(true, calibration_max_polls);
    crate::mac::program_scan_station_mode();
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelTransitionScheduler {
    pub(super) state: u8,
    pub(super) deadline: u32,
    pub(super) result: ChannelTransitionResult,
    pub(super) calibration_max_polls: u32,
}

impl ChannelTransitionScheduler {
    pub const fn new() -> Self {
        Self {
            state: 0,
            deadline: 0,
            result: ChannelTransitionResult {
                divider: PllDivider {
                    integer: 0,
                    fractional: 0,
                    register: 0,
                },
                timing: 0,
                temperature: TemperatureMeasurement {
                    value: 0,
                    accepted: false,
                },
                calibration_ran: false,
                threshold: 0,
                first_tx_power: 0,
                second_tx_power: 0,
                frequency_offset: 0,
            },
            calibration_max_polls: 0,
        }
    }

    pub fn is_idle(&self) -> bool {
        self.state == 0
    }

    /// Starts the hardware-producing phase and arms the vendor 120-tick settle
    /// phase. This is cooperative rather than a Rust `Future`: no executor,
    /// allocation, or wake infrastructure is required.
    pub fn start(
        &mut self,
        channel: u16,
        calibration_max_polls: u32,
    ) -> Result<(), ChannelTransitionError> {
        if !self.is_idle() {
            return Err(ChannelTransitionError::InvalidTiming);
        }
        // Vendor `mac_set_channel` returns immediately when the requested MAC
        // channel is already active. Preserve RX/FIFO state instead of cycling
        // the analogue front end for every repeated single-channel scan.
        if crate::mac::low_mac().current_channel.get() == channel
            && crate::mac::low_mac().receive_state.get() == 4
            && self::state().phase.get() == 2
        {
            let (integer, fractional) = cached_pll_divider();
            self.result.divider = PllDivider {
                integer,
                fractional,
                register: integer.wrapping_shl(21) | fractional,
            };
            self.result.calibration_ran = false;
            self.calibration_max_polls = calibration_max_polls;
            self.state = 3;
            return Ok(());
        }
        let result = begin_channel_transition(channel, calibration_max_polls)?;
        self.calibration_max_polls = calibration_max_polls;
        self.result = result;
        self.state = 1;
        Ok(())
    }

    /// Starts the 120-tick interval after the hardware-producing phase has
    /// returned, matching the placement of `fw_read_timer()` in vendor
    /// `phy_cal_run_step_timed`.
    pub fn arm_settle(&mut self, now: u32) {
        if self.state == 1 {
            self.deadline = now.wrapping_add(0x78);
            self.state = 2;
        }
    }

    /// Advances the timer-driven phase and returns the completed transition.
    pub fn service(
        &mut self,
        now: u32,
    ) -> Result<Option<ChannelTransitionResult>, ChannelTransitionError> {
        if self.state == 3 {
            self.state = 0;
            return Ok(Some(self.result));
        }
        if self.state != 2 {
            return Ok(None);
        }
        if (now.wrapping_sub(self.deadline) as i32) < 0 {
            return Ok(None);
        }
        finish_channel_transition(self.calibration_max_polls)?;
        self.state = 0;
        Ok(Some(self.result))
    }
}

impl Default for ChannelTransitionScheduler {
    fn default() -> Self {
        Self::new()
    }
}

pub fn run_channel_transition(
    channel: u16,
    calibration_max_polls: u32,
) -> Result<ChannelTransitionResult, ChannelTransitionError> {
    let result = begin_channel_transition(channel, calibration_max_polls)?;
    delay_timer_ticks(0x78);
    finish_channel_transition(calibration_max_polls)?;
    Ok(result)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PllDivider {
    pub integer: u32,
    pub fractional: u32,
    pub register: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelThresholdCorrection {
    pub upper_channel: u16,
    pub value: i16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ChannelCalibrationStep {
    pub upper_channel: u8,
    pub first: u8,
    pub second: u8,
}

/// Inspired by vendor `0x18f2c -> 0x18ef0`. It computes the integer
/// and 21-bit fractional PLL divider later packed into `0x0abc00b4`.
pub fn pll_divider(frequency_khz: u32, multiplier: u32, crystal_khz: u32) -> Option<PllDivider> {
    if crystal_khz == 0 {
        return None;
    }
    let product = u64::from(frequency_khz) * u64::from(multiplier);
    let integer = product / u64::from(crystal_khz);
    let remainder = product % u64::from(crystal_khz);
    let fractional = (remainder << 21) / u64::from(crystal_khz);
    let integer = integer as u32;
    let fractional = fractional as u32;
    Some(PllDivider {
        integer,
        fractional,
        register: integer.wrapping_shl(21) | fractional,
    })
}

/// Pure table walk from vendor `0x19dd0`. It returns the value belonging to
/// the last threshold not greater than the channel, or the table default.
pub fn select_threshold_correction(
    channel: u16,
    default: i16,
    entries: &[ChannelThresholdCorrection],
) -> i16 {
    entries
        .iter()
        .take_while(|entry| entry.upper_channel <= channel)
        .last()
        .map_or(default, |entry| entry.value)
}

/// Pure three-byte channel-step lookup from vendor `0x1a112`.
pub fn select_channel_calibration(
    channel: u8,
    first: bool,
    base: i16,
    entries: &[ChannelCalibrationStep],
) -> Option<i16> {
    let mut selected = entries.first()?;
    for (index, entry) in entries.iter().enumerate() {
        if channel <= entry.upper_channel {
            selected = if channel < entry.upper_channel && index != 0 {
                &entries[index - 1]
            } else {
                entry
            };
            break;
        }
    }
    let value = if first {
        selected.first
    } else {
        selected.second
    };
    Some(base.wrapping_add(i16::from(value) * 4))
}
