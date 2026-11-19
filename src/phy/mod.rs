//! The PHY: its software state, the vendor initialization register lists
//! (from the initialized-SRAM container segments, vendor `0x16ac6`), RF and
//! MAC-core initialization, and receive control. Channel programming lives
//! in `channel`, calibration in `calibration`.

mod calibration;
mod channel;
#[cfg(test)]
mod tests;

pub use calibration::*;
pub use channel::*;

use crate::global::Global;
use core::cell::Cell;
use core::mem::MaybeUninit;
use tock_registers::interfaces::{ReadWriteable, Readable};

use zerocopy::byteorder::little_endian::U16;
use zerocopy::{Immutable, IntoBytes};

/// Software PHY state. The firmware is single-threaded and never takes
/// interrupts, so plain `Cell`s give shared mutable access without locking.
pub struct PhyState {
    /// Always 2 once initialized (vendor mode-state byte).
    pub mode_state: Cell<u8>,
    /// Band profile: 0 = 2.4 GHz, 1 = 5 GHz.
    pub profile: Cell<u8>,
    pub phase: Cell<u8>,
    pub channel: Cell<u16>,
    /// The profile's IQ correction has been calibrated.
    pub profile0_ready: Cell<u8>,
    pub profile1_ready: Cell<u8>,
    pub profile0_state: Cell<u8>,
    /// Primary IQ calibration has run; its shift state is then retained.
    pub auxiliary_state: Cell<u8>,
    pub calibration_stage: Cell<u8>,
    pub calibration_state: Cell<u8>,
    pub reference_khz: Cell<u32>,
    pub frequency_khz: Cell<u32>,
    pub measurement_shift_state: Cell<u32>,
    pub sample_width: Cell<u16>,
    pub silicon_variant: Cell<u8>,
    /// Temperature ADC conversion (SDD element 0x46).
    pub temperature_denominator: Cell<i16>,
    pub temperature_offset: Cell<i16>,
    /// Last accepted temperature measurement, milli-degrees.
    pub temperature: Cell<i32>,
    /// PHY TX state: 3 idle, 4 TX programmed, 5 TX locked.
    pub retained_state: Cell<u8>,
    pub state_scale: Cell<i32>,
    pub threshold: Cell<i16>,
    /// The PLL divider last programmed (integer, fractional) and its channel.
    pub pll_integer: Cell<u32>,
    pub pll_fractional: Cell<u32>,
    pub pll_channel: Cell<u16>,
    /// The channel's TX power limits (vendor 0x040099d4/0x040099d6), for
    /// the low and high rates.
    pub power_limit_low_rate: Cell<i16>,
    pub power_limit_high_rate: Cell<i16>,
    /// The host's TX power (MIB 0x0006), tenths of a dBm.
    pub tx_power_tenths_dbm: Cell<Option<i32>>,
}

/// Temperature the gain computation uses as its fixed reference point.
const TEMPERATURE_FALLBACK: i32 = 38_000;

impl PhyState {
    /// Values vendor `0x16a38`/`0x198f2`/`0x16ca4` establish at startup.
    const fn startup() -> Self {
        Self {
            mode_state: Cell::new(2),
            profile: Cell::new(0),
            phase: Cell::new(2),
            channel: Cell::new(7),
            profile0_ready: Cell::new(0),
            profile1_ready: Cell::new(0),
            profile0_state: Cell::new(0),
            auxiliary_state: Cell::new(0),
            calibration_stage: Cell::new(0),
            calibration_state: Cell::new(1),
            reference_khz: Cell::new(26_000),
            frequency_khz: Cell::new(2_442_000),
            measurement_shift_state: Cell::new(0),
            sample_width: Cell::new(10),
            silicon_variant: Cell::new(0),
            temperature_denominator: Cell::new(1000),
            temperature_offset: Cell::new(0),
            temperature: Cell::new(48_000),
            retained_state: Cell::new(0),
            state_scale: Cell::new(-1),
            threshold: Cell::new(0),
            pll_integer: Cell::new(0),
            pll_fractional: Cell::new(0),
            pll_channel: Cell::new(0),
            power_limit_low_rate: Cell::new(0),
            power_limit_high_rate: Cell::new(0),
            tx_power_tenths_dbm: Cell::new(None),
        }
    }
    /// Back to the startup values.
    fn reset(&self) {
        let startup = Self::startup();
        self.mode_state.set(startup.mode_state.get());
        self.profile.set(startup.profile.get());
        self.phase.set(startup.phase.get());
        self.channel.set(startup.channel.get());
        self.profile0_ready.set(startup.profile0_ready.get());
        self.profile1_ready.set(startup.profile1_ready.get());
        self.profile0_state.set(startup.profile0_state.get());
        self.auxiliary_state.set(startup.auxiliary_state.get());
        self.calibration_stage.set(startup.calibration_stage.get());
        self.calibration_state.set(startup.calibration_state.get());
        self.reference_khz.set(startup.reference_khz.get());
        self.frequency_khz.set(startup.frequency_khz.get());
        self.measurement_shift_state
            .set(startup.measurement_shift_state.get());
        self.sample_width.set(startup.sample_width.get());
        self.silicon_variant.set(startup.silicon_variant.get());
        self.temperature_denominator
            .set(startup.temperature_denominator.get());
        self.temperature_offset
            .set(startup.temperature_offset.get());
        self.temperature.set(startup.temperature.get());
        self.retained_state.set(startup.retained_state.get());
        self.state_scale.set(startup.state_scale.get());
        self.threshold.set(startup.threshold.get());
        self.pll_integer.set(startup.pll_integer.get());
        self.pll_fractional.set(startup.pll_fractional.get());
        self.pll_channel.set(startup.pll_channel.get());
        self.power_limit_low_rate
            .set(startup.power_limit_low_rate.get());
        self.power_limit_high_rate
            .set(startup.power_limit_high_rate.get());
    }
}

static PHY_STATE: Global<PhyState> = Global::new(PhyState::startup());

pub fn state() -> &'static PhyState {
    &PHY_STATE
}

/// Calibration from the host's SDD file, parsed when it arrives; kept apart
/// from the PHY's runtime state.
static CALIBRATION: Global<Cell<crate::configuration::SddCalibration>> =
    Global::new(Cell::new(crate::configuration::SddCalibration::ZERO));

pub fn calibration() -> crate::configuration::SddCalibration {
    CALIBRATION.get()
}

/// Take the calibration elements present in the SDD file `sdd`; the others
/// keep their values, as the vendor's per-element callbacks do.
pub fn calibrate(sdd: &[u8]) {
    let mut calibration = CALIBRATION.get();
    calibration.apply(sdd);
    CALIBRATION.set(calibration);
    let element = |id| crate::configuration::sdd_element(sdd, id);
    let phy = state();
    if let Some(reference) = element(0xc5).and_then(|data| <[u8; 2]>::try_from(data).ok()) {
        phy.reference_khz
            .set(u32::from(u16::from_le_bytes(reference)));
    }
    // Parameter-zero temperature fallback.
    if let Some([a, b, c, d, ..]) = element(0x46) {
        phy.temperature_denominator
            .set(i16::from_le_bytes([*a, *b]));
        phy.temperature_offset.set(i16::from_le_bytes([*c, *d]));
    }
}

/// The host's TX power, in tenths of a dBm.
pub fn set_tx_power(tenths_dbm: Option<i32>) {
    state().tx_power_tenths_dbm.set(tenths_dbm);
}

pub const COMMON_INITIALIZATION_LISTS: [&[(u32, u32)]; 4] = [
    &[(0x0ab8_0108, 0x0020_0300)],
    &[
        (0x0ab8_807c, 0x0000_0001),
        (0x0ab8_8058, 0x0000_63d9),
        (0x0ab8_808c, 0x0000_103f),
        (0x0ab8_8090, 0x1010_103f),
    ],
    &[(0x0ab9_0000, 0), (0x0ab9_0014, 0x0fff_ffff)],
    &[
        (0x0aba_0004, 0x0000_0033),
        (0x0aba_0008, 0x0000_013e),
        (0x0aba_000c, 0x0000_00a6),
        (0x0aba_0010, 0x0000_0001),
        (0x0aba_0014, 0x0000_0040),
        (0x0ab8_0c2c, 0x0000_004f),
        (0x0aba_8540, 0x0000_017f),
    ],
];

pub const MODE0_INITIALIZATION_LIST: &[(u32, u32)] = &[
    (0x0ab8_0400, 0x55f4_282b),
    (0x0ab8_0410, 0x0000_003a),
    (0x0ab8_0c20, 0),
    (0x0aba_803c, 0x7787_1f1b),
    (0x0ab8_0404, 0x0010_137a),
    (0x0ab8_0414, 0x0000_00ab),
    (0x0ab8_0418, 0),
    (0x0ab8_041c, 0x0008_0ea0),
    (0x0abc_8004, 0x0000_0109),
    (0x0aba_8048, 0x031f_d6f0),
];

fn reg_read(address: usize) -> u32 {
    crate::mmio::phy_window(address).read32(address)
}

fn reg_write(address: usize, value: u32) {
    crate::mmio::phy_window(address).write32(address, value);
}

fn reg_write16(address: usize, value: u16) {
    crate::mmio::phy_window(address).write16(address, value);
}

fn write_u16(address: usize, value: u16) {
    reg_write16(address, value);
}

fn write_u32(address: usize, value: u32) {
    reg_write(address, value);
}

fn delay_units(units: u32) {
    let iterations = units.saturating_mul(0x22);
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

fn delay_timer_ticks(ticks: u32) {
    let target = crate::platform::timer_now().wrapping_add(ticks);
    while crate::platform::timer_now().wrapping_sub(target) as i32 <= 0 {}
}

fn apply_register_list(list: &[(u32, u32)]) {
    for &(address, value) in list {
        reg_write(address as usize, value);
    }
}

fn rf_init_stage_d_mode0() {
    const BASE: usize = 0x0abc_0040;
    let alternate = self::state().silicon_variant.get() == 1;
    write_u32(BASE - 0x3c, 0x304);
    write_u32(BASE - 0x38, if alternate { 0x9200 } else { 0x9000 });
    let remap = crate::platform::remap_window(0);
    write_u32(
        BASE - 0x20,
        ((remap & 0x07ff_ffff) >> 26)
            .wrapping_mul(0x2000_0000)
            .wrapping_add(0x0f4a_c008),
    );
    write_u32(
        BASE - 0x1c,
        if alternate { 0x29ff_1800 } else { 0x2987_1800 },
    );
    write_u32(BASE - 0x28, 0);
    write_u32(BASE - 0x34, 0x0050_0100);
    write_u32(BASE + 0x10, 0x0095_0000);
    write_u32(BASE + 0x14, 0x00b5_3830);
    write_u32(BASE + 0x18, 0x00b5_7ff1);
    for offset in [0x1c, 0x20, 0x24, 0x28] {
        write_u32(BASE + offset, 0x00b4_7ff1);
    }
    write_u32(BASE - 0x0c, 0x0000_1c04);
    write_u32(BASE - 8, 0x0000_1e55);
    write_u32(BASE - 4, 0x0000_1e55);
    for offset in [0, 4, 8, 0x0c] {
        write_u32(BASE + offset, 0x0000_1fff);
    }
}

fn rf_init_stage_a_mode0() {
    const BASE: usize = 0x0abc_00c0;
    write_u32(BASE + 0x30, 0xa0);
    write_u32(BASE - 0x98, 0x0800_1f01);
    write_u32(BASE - 0x90, 0x001c_0000);
    write_u32(BASE - 0xa4, 9);
    write_u32(BASE + 0x14, 0x0703_0100);
    write_u32(BASE + 0x18, 0x7f3f_1f0f);
    write_u32(BASE + 0x24, 0x0000_ffff);
    write_u32(BASE + 0x1c, 0x0703_0100);
    write_u32(BASE + 0x20, 0x7f3f_1f0f);
    // Like BASE-0x28 below, a vendor value that happens to look like a
    // DTCM address. Keep it exact.
    write_u32(BASE - 0x94, 0x0400_2730);
    if !crate::platform::remap_windows_present() {
        write_u32(0x0ac8_005c, 0x6a25_5800);
        write_u32(0x0ac8_00e8, 0x10c);
    }
    let override_value = 0u8; // vendor RF override byte, never set
    if override_value != 0 {
        write_u32(
            0x0ac8_005c,
            reg_read(0x0ac8_005c) & !0x7f | u32::from(override_value & 0x7f),
        );
    }
    write_u32(BASE - 0xac, 0x0027_0047);
    write_u32(BASE - 0x54, 0);
    write_u32(BASE - 0x40, 0x0002_0006);
    write_u32(BASE - 0x50, 0x0002_0516);
    write_u32(BASE - 0x4c, 0x0002_0516);
    write_u32(BASE - 0x48, 0x0002_4f36);
    write_u32(BASE - 0x44, 0x0002_4f36);
    write_u32(BASE + 0x38, 0x0002_4f36);
    write_u32(BASE - 0x3c, 0x1000);
    write_u32(BASE - 0x28, 0x0400_1000);
    write_u32(BASE - 0x38, 0x0e2c_aa32);
    write_u32(BASE - 0x34, 0x0e7c_aef2);
    write_u32(BASE - 0x30, 0x0f7c_aef2);
    write_u32(BASE - 0x2c, 0x0ffc_aef2);
    write_u32(BASE + 0x34, 0x0f6c_a8f2);
}

fn rf_init_stage_b_mode0() {
    const BASE: usize = 0x0abc_00c0;
    write_u32(BASE - 0x18, 0);
    write_u32(BASE - 0x1c, 0);
    write_u32(BASE - 0x20, 0x0000_140a);
    write_u32(BASE - 0x24, 0x0030_0000);
    write_u32(BASE, 0x1350_381e);
    let alternate = self::state().silicon_variant.get() == 1;
    write_u32(BASE - 0xb8, if alternate { 0x8202 } else { 0x8002 });
    write_u32(BASE - 0xbc, 0x0008_0206);
    write_u32(BASE - 0x8c, 0x0000_0201);
    delay_timer_ticks(10);
    write_u32(BASE - 0x24, 0x0030_0001);
    delay_timer_ticks(0x87);
    write_u32(BASE - 0x24, 0x0030_0030);
    write_u32(BASE - 0x24, 0x0030_0000);
    write_u32(BASE - 0xb8, if alternate { 0x9200 } else { 0x9000 });
    write_u32(BASE - 0x8c, 0x0000_1e05);
    write_u32(BASE - 0xbc, 0x304);
    write_u32(BASE - 0xbc, 0x0008_0306);
    write_u32(BASE - 0x70, 0x0823_5801);
    write_u32(BASE - 0x8c, 0x4c0);
    delay_timer_ticks(10);
    write_u32(BASE - 0x70, 0x0822_5801);
    write_u32(BASE, 0x1350_381c);
    write_u32(0x0abb_8004, 0x0008_8200);
    write_u32(BASE, 0x1350_381d);
    delay_timer_ticks(0x32);
    write_u32(BASE - 0xbc, 0x304);
    write_u32(BASE, 0x1350_381c);
    write_u32(BASE - 0x70, 0x0095_0000);
    write_u32(BASE - 0x8c, 0x0000_1c04);
    write_u32(0x0abb_8004, 0);
    delay_timer_ticks(10);
}

fn rf_init_stage_c_mode0() {
    const BASE: usize = 0x0abc_00c0;
    let reference = self::state().reference_khz.get();
    let range = u32::from(reference >= 0x5dc0) + u32::from(reference >= 0xbb80);
    let gain = if reference >= 0xbb80 {
        2
    } else if reference >= 0x8340 {
        3
    } else if reference >= 0x4e20 {
        4
    } else {
        5
    } * 0x4000;
    write_u32(BASE - 0x14, 0x001a_3080 | gain);
    write_u32(BASE - 0x10, range + 0x200);
    write_u32(BASE - 4, 0);
    write_u32(BASE - 8, 0);
    write_u32(BASE - 0x14, 0x001a_30b8 | gain);
    write_u32(BASE - 0x10, range + 0x200);
    write_u32(BASE - 4, 0);
    write_u32(BASE - 8, 0x0020_0000);
    delay_timer_ticks(10);
    write_u32(BASE - 0x14, 0x001a_30fb | gain);
    write_u32(BASE - 0x10, range + 0x7200);
    write_u32(BASE - 4, 0);
    write_u32(BASE - 8, 0x0020_0412);
    delay_timer_ticks(5);
    write_u32(BASE - 0x14, (gain + 0x0100_0000) | 0x03fa_30fb);
    write_u32(BASE - 0x10, 0x0000_723c | range);
    write_u32(BASE - 4, 0);
    write_u32(BASE - 8, 0x0020_0412);
    delay_timer_ticks(1);
    write_u32(BASE - 0x10, 0x0035_723c | range);
    delay_timer_ticks(1);
    write_u32(BASE - 4, 0x0004_0000);
    write_u32(BASE - 8, 0x0020_2412);
    delay_timer_ticks(0x78);
}

fn apply_first_channel_detector_state() {
    const SUBSTATE: &[(u32, u32)] = &[
        (0x0abb_8004, 0x0000_0034),
        (0x0abb_8008, 0x001f_0077),
        (0x0abb_80e8, 0x000f_0190),
        (0x0abb_80ec, 0x0000_001f),
        (0x0abb_80f4, 0x0020_0190),
        (0x0abb_80f8, 0x012c_00c8),
        (0x0abb_80fc, 0x0036_8ccc),
        (0x0abb_81a0, 0),
    ];
    const EXPANDED: [u8; 32] = [
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1, 0, 0, 0, 4, 8, 1, 2, 0, 0, 0, 0,
        2, 8,
    ];
    const TAIL: &[(u32, u32)] = &[
        (0x0abb_82c4, 0x0005_0082),
        (0x0abb_82c8, 0x0005_0070),
        (0x0abb_82cc, 0x000d_0010),
        (0x0abb_82d0, 0),
        (0x0abb_82d4, 0),
        (0x0abb_8388, 1),
    ];
    prepare_rf_mode0_stage();
    if self::state().mode_state.get() == 2 {
        apply_register_list(SUBSTATE);
    }
    for (index, value) in EXPANDED.into_iter().enumerate() {
        write_u32(0x0abb_8300 + index * 4, u32::from(value));
    }
    apply_register_list(TAIL);
}

fn prepare_rf_mode0_stage() {
    let control = reg_read(0x0ac8_0064);
    if control & 1 == 0 {
        write_u32(0x0ac8_0064, 0x11);
        delay_timer_ticks(0x28);
        write_u32(0x0ac8_0064, 1);
    }
    rf_init_stage_d_mode0();
    rf_init_stage_a_mode0();
    rf_init_stage_b_mode0();
    rf_init_stage_c_mode0();
    if self::state().profile0_state.get() == 0 {
        self::state().profile0_state.set(2);
        initialize_mac_core_mode0();
    }
}

fn program_mode2_band_hardware() {
    let bandwidth = reg_read(0x0aba_8040);
    write_u32(0x0aba_8040, bandwidth | 0x0002_0000);
    let timing = reg_read(0x0ab8_0c00);
    write_u32(
        0x0ab8_0c00,
        (timing & 0xfc00_ffff).wrapping_add(0x00b4_0000),
    );
    write_u32(0x0abd_0004, 4);

    let mut control = reg_read(0x0abb_8004);
    control = ((control & !1) | 0x2000_0000) & 0xe000_7fff | 0x800;
    // Dispatcher case 2 at 0x1745c clears r0 (bit 1), sets r2 (bit 2),
    // and selects bandwidth mode 2. The shared tail clears bit 4 for PHY
    // profile 2.
    control = (control & !2) | 4;
    if self::state().mode_state.get() == 2 {
        control &= !0x10;
    }
    write_u32(0x0abb_8004, control);
}

fn program_scan_receive_band() {
    // `phy_do_channel_switch` derives dispatcher mode 2 from scan rate
    // configuration 0x0117: bits 0 and 4 are both set and bit 5 is clear.
    // Profile byte +2 remains zero for the 2.4 GHz synth/calibration path.
    self::state().profile.set(0);
    self::state().phase.set(2);
    self::state().calibration_stage.set(3);
    self::state().calibration_state.set(1);
    program_mode2_band_hardware();
}

fn publish_completed_receive_state() {
    // The active channel path leaves the PHY controller running at
    // 0x0ac80064 == 1. Writing 0x10 here is vendor radio-stop behavior.
    self::state().calibration_stage.set(3);

    let mut state = self::state().retained_state.get();
    if state != 5 {
        state = 3;
        self::state().retained_state.set(state);
    }
    crate::tx::set_phy_operation(state);
}

/// A station's doze: the receiver off (vendor `phy_enter_state3`), then the
/// PHY controller halted as the vendor's `task_df4` does: command 7, and
/// 0x10 to 0x0ac80064. Fails, the receiver on, while a frame is still
/// coming in.
#[cfg(target_arch = "arm")]
pub fn doze_receiver() -> bool {
    if !set_packet_receive_enabled(false, 1_000) {
        set_packet_receive_enabled(true, 0);
        return false;
    }
    crate::mac::low_mac().receive_state.set(3);
    crate::tx::stop_phy_operation_7();
    start_scan_stop_calibration_state();
    // The PHY must come back through RF initialization, not the
    // same-channel shortcut.
    self::state().calibration_state.set(1);
    true
}

/// The receiver on again after [`doze_receiver`]: the operating channel
/// tuned again and the joined MAC state of `interface` restored, as at the
/// end of a scan. Takes 3.6 ms (measured on channel 1).
#[cfg(target_arch = "arm")]
pub fn wake_receiver(interface: Option<u8>) -> bool {
    run_channel_transition(self::state().channel.get(), 100_000).is_ok()
        && interface.is_none_or(crate::join::return_from_scan)
}

fn set_packet_receive_enabled(enabled: bool, max_polls: u32) -> bool {
    use crate::platform::RxDmaControl::{ACTIVE, ENABLE};
    let control = &crate::platform::rx_dma().control;
    if enabled {
        control.modify(ENABLE::SET);
        let low_mac = crate::mac::low_mac();
        low_mac.receive_gate.set(low_mac.receive_gate.get() & !1);
        // Tail of `phy_resume_state4` (`0x2864`). The TX scheduler call is
        // intentionally omitted until its queues are translated.
        low_mac.receive_state.set(4);
        true
    } else {
        control.modify(ENABLE::CLEAR);
        let mut polls = 0;
        while control.is_set(ACTIVE) {
            if polls >= max_polls {
                return false;
            }
            polls += 1;
            core::hint::spin_loop();
        }
        let low_mac = crate::mac::low_mac();
        low_mac.receive_gate.set(low_mac.receive_gate.get() | 1);
        true
    }
}

/// Awake-station subset of vendor `phy_state_advance(0)` (`0x820a`).
///
/// JOIN has already completed channel wake and cannot reach the state-1 wake
/// branch here. The retained reprogram latch and state-3 resume tail must still
/// run before the first ordinary class-0 active-TX claim.
///
/// PHY state and MAC channel registers must be exclusively owned.
pub fn advance_awake_station_tx() -> bool {
    self::state().phase.set(0);
    let state = crate::mac::low_mac().receive_state.get();
    if state == 1 {
        return false;
    }
    let retained_state = self::state().retained_state.get();
    // Vendor `phy_state_advance` never writes `0x040099a9`; it only reads
    // it to decide whether to run the reprogram tail. A hardware capture of
    // the accepted class-6 publication versus the refused class-0 one shows
    // the working path publishes with retained state 5, so forcing it to 3
    // here put the PHY into a configuration the MAC does not accept.
    if retained_state != 0 {
        crate::mac::reprogram_after_channel();
    }
    !(state == 3 && !set_packet_receive_enabled(true, 100_000))
}

/// Exact `phy_cal_step_start` used by `mac_radio_stop` after command 7 has
/// been stopped.
#[cfg(target_arch = "arm")]
pub fn start_scan_stop_calibration_state() {
    reg_write(0x0ac8_0064, 0x10);
    self::state().calibration_stage.set(1);
    if self::state().phase.get() == 3 {
        self::state().profile1_ready.set(0);
        self::state().calibration_state.set(1);
    }
}

#[cfg(target_arch = "arm")]
pub fn begin_scan_stop_rx_disable() {
    crate::platform::rx_dma()
        .control
        .modify(crate::platform::RxDmaControl::ENABLE::CLEAR);
}

#[cfg(target_arch = "arm")]
pub fn scan_stop_rx_hardware_drained() -> bool {
    let drained = !crate::platform::rx_dma()
        .control
        .is_set(crate::platform::RxDmaControl::ACTIVE);
    if drained {
        let low_mac = crate::mac::low_mac();
        low_mac.receive_gate.set(low_mac.receive_gate.get() | 1);
    }
    drained
}

pub fn derive_remap_timing(remap: u32) -> (u16, u16) {
    let mut first = ((remap << 2) >> 23) as i16;
    let mut second = ((remap << 11) >> 24) as i16;
    if first & 0x100 != 0 {
        first = -(first & !0x100);
    }
    if second & 0x80 != 0 {
        second = -(second & !0x80);
    }
    (
        first.wrapping_add(1000) as u16,
        second.wrapping_mul(71) as u16,
    )
}

/// Software-state translation of vendor `0x16a38`, `0x198f2`, and `0x16ca4`
/// for the XR819 mode selected by the vendor version string.
///
/// The vendor BSS range and boot/remap state must already be initialized.
/// The RF silicon variant, from the version string the vendor reads at
/// 0xfff1_7f90 in high memory (not a register).
fn detect_rf_silicon_variant() -> u8 {
    // SAFETY: high memory is always mapped; these words are only read.
    let word = |address: usize| unsafe { (address as *const u32).read_volatile() };
    let (first, second, third) = (word(0xfff1_7f90), word(0xfff1_7f94), word(0xfff1_7f98));
    if first == 0x302e_3530 && second == 0x3130_2e36 && third == 0x0000_3631 {
        1
    } else if first == 0x302e_3830 && second == 0x3130_2e32 && third == 0x0000_3036 {
        2
    } else {
        6
    }
}

pub fn initialize_mac_software_state() {
    let phy = self::state();
    phy.reset();
    phy.silicon_variant.set(detect_rf_silicon_variant());
    // Vendor startup mode bit 1 clears the profile-one compatibility flag.
    phy.profile1_ready
        .set(u8::from(crate::platform::startup_mode() & 2 == 0));
    // Vendor 0x16ca4 derives the temperature conversion from remap window two.
    let (denominator, offset) = derive_remap_timing(crate::platform::remap_window(2));
    phy.temperature_denominator.set(denominator as i16);
    phy.temperature_offset.set(offset as i16);
}

/// Hardware-producing portion of vendor `0x171ce -> 0x17008` for mode zero.
///
/// Packet DMA, interrupt routing, and callback-table entries must already be
/// installed. The current startup calls this only for static mode-zero hardware
/// preparation; channel policy remains inactive while callbacks are diagnostic
/// stubs.
fn build_mode0_gain_tables() {
    let correction = self::calibration().profiles[0].agc_correction;
    let mut table = [0; 80];
    build_rate_table(&MODE0_CALIBRATION_ANCHORS, correction, &mut table);
    for (index, value) in table.iter().copied().enumerate() {
        reg_write(0x0ab8_0800 + index * 4, value);
    }

    apply_register_list(MODE0_INITIALIZATION_LIST);

    let control = reg_read(0x0ab8_0400);
    reg_write(
        0x0ab8_0400,
        (control & 0xffff_80ff) | (table[0] & 0x0000_7f00),
    );
    let boundary = reg_read(0x0ab8_0410);
    reg_write(
        0x0ab8_0410,
        (boundary & 0xffff_ff80) | u32::from(rate_table_boundary(&table)),
    );
}

pub fn initialize_mac_core_mode0() {
    for index in 0..=0x40 {
        reg_write(0x0aba_2000 + index * 4, 0x00ed_00ed);
    }

    reg_write(0x0aba_805c, 0x2708_2026);
    // Vendor selects 0x03a0_00a0 for silicon mode 2; nothing sets it.
    reg_write(0x0ab8_0c08, 0x03a0_0710);
    reg_write(0x0ab8_0c3c, 0xff03_f7a0);
    reg_write(0x0ab8_0064, 0x0021_0280);

    for list in COMMON_INITIALIZATION_LISTS {
        apply_register_list(list);
    }

    build_mode0_gain_tables();

    let mac_control = reg_read(0x0ab8_0c00);
    reg_write(0x0ab8_0c00, mac_control | (1 << 11));
}
