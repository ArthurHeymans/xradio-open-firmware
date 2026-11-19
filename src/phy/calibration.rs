//! Calibration: the vendor anchors and rate tables, the calibration sample
//! engine, and IQ and dynamic-IQ calibration.

use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CalibrationAnchor {
    pub selector: u8,
    pub upper: i16,
    pub lower: i16,
}

/// Twelve gain-table indices copied by the vendor container to `0x04000e18`.
pub const IQ_CALIBRATION_GAIN_INDICES: [u32; 12] = [
    0x1a, 0x19, 0x18, 0x16, 0x15, 0x14, 0x12, 0x11, 0x10, 0x02, 0x01, 0x00,
];

/// Exact signed fixed-point cosine/sine tables used by target `0x18480`.
/// The 256-byte sequence is identical in the annotated and target images.
pub(super) const DYNAMIC_IQ_COSINE: [i16; 64] = [
    32767, 32609, 32137, 31356, 30273, 28898, 27245, 25329, 23170, 20787, 18204, 15446, 12539,
    9512, 6393, 3212, 0, -3212, -6393, -9512, -12539, -15446, -18204, -20787, -23170, -25329,
    -27245, -28898, -30273, -31356, -32137, -32609, -32767, -32609, -32137, -31356, -30273, -28898,
    -27245, -25329, -23170, -20787, -18204, -15446, -12539, -9512, -6393, -3212, 0, 3212, 6393,
    9512, 12539, 15446, 18204, 20787, 23170, 25329, 27245, 28898, 30273, 31356, 32137, 32609,
];

pub(super) const DYNAMIC_IQ_SINE: [i16; 64] = [
    0, 3212, 6393, 9512, 12539, 15446, 18204, 20787, 23170, 25329, 27245, 28898, 30273, 31356,
    32137, 32609, 32767, 32609, 32137, 31356, 30273, 28898, 27245, 25329, 23170, 20787, 18204,
    15446, 12539, 9512, 6393, 3212, 0, -3212, -6393, -9512, -12539, -15446, -18204, -20787, -23170,
    -25329, -27245, -28898, -30273, -31356, -32137, -32609, -32767, -32609, -32137, -31356, -30273,
    -28898, -27245, -25329, -23170, -20787, -18204, -15446, -12539, -9512, -6393, -3212,
];

pub const MODE0_CALIBRATION_ANCHORS: [CalibrationAnchor; 22] = [
    CalibrationAnchor {
        selector: 26,
        upper: 5,
        lower: -1017,
    },
    CalibrationAnchor {
        selector: 25,
        upper: 38,
        lower: -1049,
    },
    CalibrationAnchor {
        selector: 24,
        upper: 70,
        lower: -1054,
    },
    CalibrationAnchor {
        selector: 22,
        upper: 99,
        lower: -1085,
    },
    CalibrationAnchor {
        selector: 21,
        upper: 131,
        lower: -1117,
    },
    CalibrationAnchor {
        selector: 20,
        upper: 164,
        lower: -1147,
    },
    CalibrationAnchor {
        selector: 18,
        upper: 196,
        lower: -1180,
    },
    CalibrationAnchor {
        selector: 17,
        upper: 228,
        lower: -1211,
    },
    CalibrationAnchor {
        selector: 16,
        upper: 260,
        lower: -1244,
    },
    CalibrationAnchor {
        selector: 2,
        upper: 289,
        lower: -1272,
    },
    CalibrationAnchor {
        selector: 1,
        upper: 322,
        lower: -1305,
    },
    CalibrationAnchor {
        selector: 0,
        upper: 354,
        lower: -1317,
    },
    CalibrationAnchor {
        selector: 56,
        upper: 404,
        lower: -1387,
    },
    CalibrationAnchor {
        selector: 54,
        upper: 433,
        lower: -1420,
    },
    CalibrationAnchor {
        selector: 53,
        upper: 465,
        lower: -1449,
    },
    CalibrationAnchor {
        selector: 52,
        upper: 498,
        lower: -1461,
    },
    CalibrationAnchor {
        selector: 50,
        upper: 530,
        lower: -1493,
    },
    CalibrationAnchor {
        selector: 49,
        upper: 562,
        lower: -1510,
    },
    CalibrationAnchor {
        selector: 48,
        upper: 595,
        lower: -1519,
    },
    CalibrationAnchor {
        selector: 34,
        upper: 623,
        lower: -1532,
    },
    CalibrationAnchor {
        selector: 33,
        upper: 656,
        lower: -1539,
    },
    CalibrationAnchor {
        selector: 32,
        upper: 688,
        lower: -1544,
    },
];

pub const MODE1_CALIBRATION_ANCHORS: [CalibrationAnchor; 22] = [
    CalibrationAnchor {
        selector: 26,
        upper: 189,
        lower: -1201,
    },
    CalibrationAnchor {
        selector: 25,
        upper: 222,
        lower: -1233,
    },
    CalibrationAnchor {
        selector: 24,
        upper: 254,
        lower: -1263,
    },
    CalibrationAnchor {
        selector: 22,
        upper: 283,
        lower: -1295,
    },
    CalibrationAnchor {
        selector: 21,
        upper: 316,
        lower: -1320,
    },
    CalibrationAnchor {
        selector: 20,
        upper: 348,
        lower: -1331,
    },
    CalibrationAnchor {
        selector: 18,
        upper: 380,
        lower: -1363,
    },
    CalibrationAnchor {
        selector: 17,
        upper: 412,
        lower: -1396,
    },
    CalibrationAnchor {
        selector: 16,
        upper: 445,
        lower: -1428,
    },
    CalibrationAnchor {
        selector: 2,
        upper: 473,
        lower: -1437,
    },
    CalibrationAnchor {
        selector: 1,
        upper: 506,
        lower: -1462,
    },
    CalibrationAnchor {
        selector: 0,
        upper: 538,
        lower: -1478,
    },
    CalibrationAnchor {
        selector: 56,
        upper: 405,
        lower: -1391,
    },
    CalibrationAnchor {
        selector: 54,
        upper: 434,
        lower: -1437,
    },
    CalibrationAnchor {
        selector: 53,
        upper: 467,
        lower: -1450,
    },
    CalibrationAnchor {
        selector: 52,
        upper: 499,
        lower: -1471,
    },
    CalibrationAnchor {
        selector: 50,
        upper: 531,
        lower: -1494,
    },
    CalibrationAnchor {
        selector: 49,
        upper: 564,
        lower: -1511,
    },
    CalibrationAnchor {
        selector: 48,
        upper: 596,
        lower: -1520,
    },
    CalibrationAnchor {
        selector: 34,
        upper: 624,
        lower: -1533,
    },
    CalibrationAnchor {
        selector: 33,
        upper: 657,
        lower: -1542,
    },
    CalibrationAnchor {
        selector: 32,
        upper: 689,
        lower: -1547,
    },
];

/// First normalization pass performed by vendor `0x17008`.
pub fn normalize_anchor(anchor: CalibrationAnchor, correction: i16) -> CalibrationAnchor {
    CalibrationAnchor {
        selector: anchor.selector,
        upper: (((i32::from(anchor.upper) - i32::from(correction)) + 8) >> 4) as i16,
        lower: (((i32::from(anchor.lower) + i32::from(correction)) + 8) >> 4) as i16,
    }
}

/// Expand the 22 calibration anchors into the 80 hardware words written by
/// vendor `0x17008` to `0x0ab80800`.
pub fn build_rate_table(
    anchors: &[CalibrationAnchor; 22],
    correction: i16,
    output: &mut [u32; 80],
) {
    let mut normalized = [CalibrationAnchor {
        selector: 0,
        upper: 0,
        lower: 0,
    }; 22];
    for (destination, source) in normalized.iter_mut().zip(anchors) {
        *destination = normalize_anchor(*source, correction);
    }

    for (index, word) in output.iter_mut().enumerate() {
        let target = index as i16 - 90;
        let mut selected = 0;
        let mut best_lower = -24;
        for candidate in (0..normalized.len()).rev() {
            let anchor = normalized[candidate];
            if target < -(anchor.upper + 24) && anchor.lower < best_lower {
                selected = candidate;
                best_lower = anchor.lower;
            }
        }

        let anchor = normalized[selected];
        let halfword = u16::from(anchor.selector) | (((anchor.upper as u16) & 0x7f) << 8);
        *word = u32::from(halfword) | (u32::from(halfword) << 16);
    }
}

/// One-based index stored in the low seven bits of `0x0ab80410`.
pub fn rate_table_boundary(table: &[u32; 80]) -> u8 {
    table
        .iter()
        .position(|word| ((word & 0x7fff) >> 8) < 11)
        .map(|index| index as u8 + 1)
        .unwrap_or(80)
}

/// Complete enable/disable behavior from vendor `0x168b8` for state mode 2.
pub fn set_calibration_engine_enabled(enabled: bool) {
    let offset = 0usize;
    let control_address = 0x0abb_8004 + offset;
    let mut control = reg_read(control_address);
    if enabled {
        control |= 0x0004_8000;
    } else {
        control &= !0x0004_8000;
    }
    write_u32(control_address, control);
}

pub fn calibration_gain_value(gain: i32) -> u32 {
    if gain < 0 {
        0
    } else {
        0x40 | (gain as u32 & 0x3f)
    }
}

/// Vendor `0x17884` calibration gain selector.
pub fn set_calibration_gain(gain: i32) {
    write_u32(0x0abb_81a4, calibration_gain_value(gain));
}

pub fn calibration_mode_timing(mode: u8) -> u32 {
    if matches!(mode, 2 | 3) {
        0x0020_0078
    } else {
        0x0020_0190
    }
}

/// Deterministic low-reserved-bit form of vendor `0x17bf2`.
pub fn calibration_sample_settings(i: u8, q: u8) -> u32 {
    calibration_sample_settings_from(0, i, q)
}

tock_registers::register_bitfields! {
    u32,
    CalibrationSample [
        /// I setting.
        I OFFSET(0) NUMBITS(8) [],
        /// Always set with a new I value.
        BIT8 OFFSET(8) NUMBITS(1) [],
        /// Always cleared with a new I value.
        BIT9 OFFSET(9) NUMBITS(1) [],
        /// Q setting.
        Q OFFSET(16) NUMBITS(8) [],
        /// Always set with a new Q value.
        BIT24 OFFSET(24) NUMBITS(1) [],
        /// Always cleared with a new Q value.
        BIT25 OFFSET(25) NUMBITS(1) [],
    ],
    /// Read side; writes are commands.
    CalibrationCommand [
        /// Set when the measurement has completed.
        DONE OFFSET(4) NUMBITS(1) [],
    ],
    /// Written 0 for a negative gain.
    CalibrationGain [
        /// Gain value.
        GAIN OFFSET(0) NUMBITS(6) [],
        /// Set with a non-negative gain.
        ENABLE OFFSET(6) NUMBITS(1) [],
    ],
}

pub(super) fn calibration_sample_settings_from(current: u32, i: u8, q: u8) -> u32 {
    let with_i = (current & !0x0000_02ff) | u32::from(i);
    let with_q = (with_i & !0x02ff_0000) | (u32::from(q) << 16);
    with_q | 0x0100_0100
}

/// Fixed `0x17b70(0x0b, 1, ...)` trigger word before hardware sets bit `0x10`.
pub const CALIBRATION_SAMPLE_COMMAND: u32 = 0x0800_000d;

/// Detached bounded form of `0x17bf2 -> 0x17b70`.
pub fn run_calibration_sample_mode(
    i: u8,
    q: u8,
    measurement_mode: u8,
    max_polls: u32,
) -> Result<IqCalibrationCoefficient, CalibrationSampleError> {
    // The optimized vendor call chain deliberately carries r3 from
    // `rf_select_band_regs()` through `rf_cal_path_setup()` and
    // `rf_set_iq_dac()`. At both `rf_set_iq_dac` (0x17c46) and
    // `rf_measure_iq` (0x17bc4), that fourth argument is the register-base
    // value below, not either destination register's previous contents.
    const VENDOR_CALL_CONTEXT: u32 = 0x0abb_80c0;
    write_u32(
        0x0abb_8114,
        calibration_sample_settings_from(VENDOR_CALL_CONTEXT, i, q),
    );
    let command = (((VENDOR_CALL_CONTEXT & 0xf000_ffff & !0xf0)
        | 0x0800_0008
        | (u32::from(measurement_mode & 1) << 2))
        & !3)
        .wrapping_add(1);
    write_u32(0x0abb_80f0, command);
    for _ in 0..max_polls {
        if reg_read(0x0abb_80f0) & 0x10 != 0 {
            let i = decode_calibration_accumulator(reg_read(0x0abb_810c));
            let q = decode_calibration_accumulator(reg_read(0x0abb_8110));
            return Ok(IqCalibrationCoefficient { i, q });
        }
    }
    Err(CalibrationSampleError::Timeout)
}

pub fn run_calibration_sample(
    i: u8,
    q: u8,
    max_polls: u32,
) -> Result<IqCalibrationCoefficient, CalibrationSampleError> {
    run_calibration_sample_mode(i, q, 1, max_polls)
}

/// Complete `0x178be -> 0x17898` calibration mode programming.
pub fn configure_calibration_mode(mode: u8) {
    let timing = calibration_mode_timing(mode);
    write_u32(0x0abb_80f0, 0x0100_4008 + u32::from(mode & 3));
    write_u32(0x0abb_80f4, timing);
    write_u32(0x0abb_80f8, 0x012c_00c8);
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationSample {
    pub baseline_i: i32,
    pub baseline_q: i32,
    pub target_i: i32,
    pub target_q: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationCoefficient {
    pub i: i32,
    pub q: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationControls {
    pub during_settle: u32,
    pub active: u32,
    pub abc0034: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationRegisterSnapshot {
    abc0004: u32,
    abc0034: u32,
    abc0050: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublishedIqCoefficient {
    pub shift: u8,
    pub normalized_i: i32,
    pub normalized_q: i32,
    pub packed: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationPublication {
    pub primary_address: u32,
    pub normalized_first_address: u32,
    pub normalized_second_address: u32,
    pub primary_value: u16,
    pub normalized_value: u32,
    pub next_shift_state: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationIteration {
    pub gain_index: u32,
    pub sample: IqCalibrationSample,
    pub scale_i: i32,
    pub scale_q: i32,
    pub coefficient: IqCalibrationCoefficient,
    pub publication: Option<IqCalibrationPublication>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationSeries {
    pub iterations: [IqCalibrationIteration; 12],
    pub final_shift_state: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IqCalibrationReferences {
    pub coefficient_i: i32,
    pub coefficient_q: i32,
    pub scale_i: i32,
    pub scale_q: i32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IqCalibrationHardwareResult {
    pub series: IqCalibrationSeries,
    pub references: IqCalibrationReferences,
    pub secondary: Option<IqCalibrationCoefficient>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IqCalibrationHardwareError {
    BaselineTimeout { gain_index: u32 },
    TargetTimeout { gain_index: u32 },
    SecondaryBaselineTimeout,
    SecondaryTargetTimeout,
    InvalidSecondaryScale,
}

/// IQ calibration's working set, kept off the stack. The series is built
/// in place; `built` says it was.
pub(super) struct IqCalibrationScratch {
    samples: [IqCalibrationSample; 12],
    series: MaybeUninit<IqCalibrationSeries>,
    built: bool,
}

impl IqCalibrationScratch {
    fn series(&self) -> Option<&IqCalibrationSeries> {
        // SAFETY: `built` is set only once the series has been written.
        self.built.then(|| unsafe { self.series.assume_init_ref() })
    }
}

/// One calibration at a time: a second borrow halts.
pub(super) static IQ_CALIBRATION_SCRATCH: Global<core::cell::RefCell<IqCalibrationScratch>> =
    Global::new(core::cell::RefCell::new(IqCalibrationScratch {
        samples: [IqCalibrationSample {
            baseline_i: 0,
            baseline_q: 0,
            target_i: 0,
            target_q: 0,
        }; 12],
        series: MaybeUninit::uninit(),
        built: false,
    }));

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIqInitialCandidate {
    pub first: i8,
    pub second: i8,
    pub third: i8,
    pub fourth: i8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIqCorrectionPlan {
    pub first_address: u32,
    pub second_address: u32,
    pub first_value: u32,
    pub second_value: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqCorrelation {
    pub real: i16,
    pub imaginary: i16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqDftResult {
    pub first: DynamicIqCorrelation,
    pub second: DynamicIqCorrelation,
    pub third: DynamicIqCorrelation,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqStageExecution {
    pub capture_samples: bool,
    pub program_candidate: bool,
    pub correlate_samples: bool,
    pub initial_control_before_program: bool,
    pub initial_control_before_correlate: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqStageControl {
    pub hardware: u32,
    pub correlation: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqDftConfiguration {
    pub mode: u8,
    pub first_phase_seed: u32,
    pub second_phase_seed: u32,
    pub third_phase_seed: u32,
    pub sample_width_shift: u8,
    pub capture_polls: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqHardwareStageResult {
    pub measurement: DynamicIqDftResult,
    pub capture_ready: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqHardwareSearchResult {
    pub search: Option<DynamicIqAveragedSearch>,
    pub all_captures_ready: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqHardwareVerificationResult {
    pub metrics: DynamicIqVerificationMetrics,
    pub capture_ready: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIqHardwareCalibrationConfiguration {
    pub alternate_profile: bool,
    pub table_value: u32,
    pub synth_frequency: i32,
    pub calibration_command: i32,
    pub seed: [i32; 4],
    pub common_step: i32,
    pub first_step: i32,
    pub second_step: i32,
    pub requested_passes: u8,
    pub configuration_flags: u32,
    pub control_configuration: u32,
    pub dft: DynamicIqDftConfiguration,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqHardwareCalibrationResult {
    pub profile_shortcut: bool,
    pub synth_prepared: bool,
    pub finalization: Option<DynamicIqFinalization>,
    pub all_search_captures_ready: bool,
    pub verification_capture_ready: bool,
}

/// Typed state for annotated `rf_op_dispatch2` (`0x17f34`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIqSearchState {
    pub candidate: [i32; 4],
    pub current: [i32; 4],
    pub common_step: i32,
    pub first_step: i32,
    pub second_step: i32,
    pub reference_correlation: DynamicIqCorrelation,
    pub common_correlations: [DynamicIqCorrelation; 6],
    pub axis_correlations: [DynamicIqCorrelation; 5],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqAverage {
    pub accumulated: [i32; 4],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqVerificationMetrics {
    pub reference: i32,
    pub second: i32,
    pub third: i32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqAveragedSearch {
    pub baseline: DynamicIqVerificationMetrics,
    pub final_candidate: [i32; 4],
    pub completed_passes: u32,
    pub accepted_passes: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIqFinalization {
    pub verification: DynamicIqVerification,
    pub publication: Option<DynamicIqCorrectionPlan>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DynamicIqVerification {
    pub retained_values: [i32; 4],
    pub first_pair_rejected: bool,
    pub second_pair_rejected: bool,
    pub first_quality_failed: bool,
    pub second_quality_failed: bool,
    /// The vendor only checks rejection of the second packed pair before
    /// publishing both words. The first-pair rejection flag is calculated but
    /// not consulted; this odd behavior is preserved explicitly.
    pub publication_allowed: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqBandDerivedValues {
    pub abc0020: u32,
    pub abc0030: u32,
    pub abb801c: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqCalibrationCommandWords {
    pub abb805c: u32,
    pub abb8060: u32,
    pub abb8064: u32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DynamicIqBandRegisterSnapshot {
    pub abd0000: u32,
    pub abc0004: u32,
    pub abc0008: u32,
    pub abc0020: u32,
    pub abc0024: u32,
    pub abc0028: u32,
    pub abc0030: u32,
    pub abc0034: u32,
    pub abc0050: u32,
    pub abc006c: u32,
    pub abc0084: u32,
    pub abc00b4: u32,
    pub abc00fc: u32,
    pub abb8004: u32,
    pub abb800c: u32,
    pub abb801c: u32,
    pub abb8068: u32,
    pub abb80a8: u32,
    pub abb81a4: u32,
    pub abb81a8: u32,
    pub restore_abb8068: bool,
    pub restore_abb80a8: bool,
}

impl DynamicIqSearchState {
    pub const fn new(
        current: [i32; 4],
        common_step: i32,
        first_step: i32,
        second_step: i32,
    ) -> Self {
        Self {
            candidate: current,
            current,
            common_step,
            first_step,
            second_step,
            reference_correlation: DynamicIqCorrelation {
                real: 0,
                imaginary: 0,
            },
            common_correlations: [DynamicIqCorrelation {
                real: 0,
                imaginary: 0,
            }; 6],
            axis_correlations: [DynamicIqCorrelation {
                real: 0,
                imaginary: 0,
            }; 5],
        }
    }
}

pub(super) fn primary_iq_axis(baseline: i32, target: i32) -> i32 {
    let mut delta = target.wrapping_sub(baseline).wrapping_mul(-0x100);
    if delta == 0 {
        delta = 1;
    }
    baseline
        .wrapping_neg()
        .wrapping_shl(14)
        .wrapping_div(delta)
        .wrapping_add(0x44)
}

/// Core coefficient calculation repeated twelve times by vendor `0x17c20`.
pub fn primary_iq_calibration(sample: IqCalibrationSample) -> IqCalibrationCoefficient {
    IqCalibrationCoefficient {
        i: primary_iq_axis(sample.baseline_i, sample.target_i),
        q: primary_iq_axis(sample.baseline_q, sample.target_q),
    }
}

/// Refinement coefficient calculation at the second measurement stage of
/// vendor `0x17c20`.
pub fn secondary_iq_calibration(
    baseline_i: i32,
    baseline_q: i32,
    target_i: i32,
    target_q: i32,
    scale_i: i32,
    scale_q: i32,
) -> Option<IqCalibrationCoefficient> {
    if scale_i == 0 || scale_q == 0 {
        return None;
    }
    Some(IqCalibrationCoefficient {
        i: baseline_i
            .wrapping_sub(target_i)
            .wrapping_mul(0x4000)
            .wrapping_div(scale_i),
        q: baseline_q
            .wrapping_sub(target_q)
            .wrapping_mul(0x4000)
            .wrapping_div(scale_q),
    })
}

/// Surviving four-byte candidate seed at `0x189a2..0x189da`. Earlier bytes in
/// the branch are overwritten before the search begins.
pub fn dynamic_iq_initial_candidate(mode: u8) -> DynamicIqInitialCandidate {
    if matches!(mode, 1 | 2) {
        DynamicIqInitialCandidate {
            first: 7,
            second: -7,
            third: -5,
            fourth: 1,
        }
    } else {
        DynamicIqInitialCandidate {
            first: -4,
            second: -11,
            third: -4,
            fourth: 0,
        }
    }
}

pub(super) fn pack_dynamic_iq_pair(first: i32, second: i32) -> u32 {
    (first as u32 & 0x0fff) | ((second as u32 & 0x0fff) << 16)
}

pub fn unpack_dynamic_iq_pair(value: u32) -> (i32, i32) {
    let decode = |raw: u32| ((raw << 20) as i32) >> 20;
    (decode(value & 0x0fff), decode((value >> 16) & 0x0fff))
}

/// Pure register plan from vendor `0x18612` for four signed correction values.
pub fn dynamic_iq_correction_plan(values: [i32; 4]) -> DynamicIqCorrectionPlan {
    DynamicIqCorrectionPlan {
        first_address: 0x0abb_8068,
        second_address: 0x0abb_80a8,
        first_value: pack_dynamic_iq_pair(values[0], values[1]),
        second_value: pack_dynamic_iq_pair(values[2], values[3]),
    }
}

/// Publish both packed correction words in vendor order.
///
/// The MAC/PHY register bank must be enabled and writable.
pub fn apply_dynamic_iq_correction_plan(plan: DynamicIqCorrectionPlan) {
    write_u32(plan.first_address as usize, plan.first_value);
    write_u32(plan.second_address as usize, plan.second_value);
}

/// Replicate the final packed pair across the two sixteen-word correction banks
/// before verification, matching `0x18dae..0x18dd0`.
///
/// The MAC/PHY register bank must be enabled and writable.
pub fn replicate_dynamic_iq_correction_banks(plan: DynamicIqCorrectionPlan) {
    for index in 1..16_u32 {
        write_u32(
            plan.first_address.wrapping_add(index * 4) as usize,
            plan.first_value,
        );
        write_u32(
            plan.second_address.wrapping_add(index * 4) as usize,
            plan.second_value,
        );
    }
}

/// Mark the profile's IQ correction valid once dynamic calibration produced
/// an accepted result.
pub fn publish_dynamic_iq_final_state(
    alternate_profile: bool,
    finalization: DynamicIqFinalization,
) {
    // The vendor also stores the quality flags and values; nothing reads them.
    if finalization.publication.is_some() {
        let phy = self::state();
        if alternate_profile {
            &phy.profile1_ready
        } else {
            &phy.profile0_ready
        }
        .set(1);
    }
}

/// Range check on the search estimate after every stage (`0x18c3e..0x18cb8`).
///
/// The vendor wraps a copy (10 bits for the first pair, 12 for the second)
/// and zeroes an estimate whose wrapped copy is out of range; an in-range
/// estimate is kept unwrapped. Only the estimate is checked: the probe the
/// stages program may lie outside the range.
pub fn reject_out_of_range_dynamic_iq(values: [i32; 4]) -> [i32; 4] {
    core::array::from_fn(|index| {
        let (wrap_above, span, limit) = if index < 2 {
            (0x1ff, 0x400, 0x200)
        } else {
            (0x7ff, 0x1000, 0x800)
        };
        let value = values[index];
        let wrapped = if value > wrap_above {
            value.wrapping_sub(span)
        } else {
            value
        };
        if (-limit..=limit).contains(&wrapped) {
            value
        } else {
            0
        }
    })
}

pub(super) fn dynamic_iq_metrics<const N: usize>(
    correlations: [DynamicIqCorrelation; N],
) -> [i32; N] {
    let mut maximum = 0_i32;
    for correlation in correlations {
        maximum = maximum
            .max(i32::from(correlation.real).unsigned_abs() as i32)
            .max(i32::from(correlation.imaginary).unsigned_abs() as i32);
    }
    let component_shift = if maximum >> 15 == 0 {
        0
    } else {
        32 - (maximum as u32 >> 15).leading_zeros()
    };
    let mut metrics = [0_i32; N];
    let mut metric_maximum = 0_i32;
    for (index, correlation) in correlations.iter().enumerate() {
        let real = i32::from(correlation.real) >> component_shift;
        let imaginary = i32::from(correlation.imaginary) >> component_shift;
        metrics[index] = real
            .wrapping_mul(real)
            .wrapping_add(imaginary.wrapping_mul(imaginary));
        metric_maximum = metric_maximum.max(metrics[index]);
    }
    let metric_shift = if metric_maximum >> 13 == 0 {
        0
    } else {
        32 - (metric_maximum as u32 >> 13).leading_zeros()
    };
    for metric in &mut metrics {
        *metric >>= metric_shift;
    }
    metrics
}

pub fn dynamic_iq_verification_metrics(
    measurement: DynamicIqDftResult,
) -> DynamicIqVerificationMetrics {
    let [reference, second, third] =
        dynamic_iq_metrics([measurement.first, measurement.second, measurement.third]);
    DynamicIqVerificationMetrics {
        reference,
        second,
        third,
    }
}

/// Analytical stage 6 of the thirteen-stage dynamic IQ search.
pub fn refine_dynamic_iq_common_pair(
    current_third: i32,
    current_fourth: i32,
    step: i32,
    correlations: [DynamicIqCorrelation; 6],
) -> (i32, i32) {
    let [m0, m1, m2, m3, m4, m5] = dynamic_iq_metrics(correlations);
    let denominator = m2
        .wrapping_mul(m2)
        .wrapping_add(m3.wrapping_mul(m0.wrapping_mul(2).wrapping_sub(m1).wrapping_sub(m2)))
        .wrapping_add(m4.wrapping_mul(m4.wrapping_add(m2).wrapping_sub(m1)))
        .wrapping_add(
            m5.wrapping_mul(
                m5.wrapping_add(m0.wrapping_mul(2))
                    .wrapping_sub(m2.wrapping_mul(2))
                    .wrapping_sub(m4.wrapping_mul(2)),
            ),
        )
        .wrapping_mul(2)
        .wrapping_add(m0.wrapping_mul(m1.wrapping_mul(4).wrapping_sub(m0.wrapping_mul(6))));

    // Vendor case 6 scales both sides before division by the bit length of
    // the common search step. Besides bounding the product, this deliberately
    // changes integer rounding and suppresses refinement when the shifted
    // denominator becomes zero.
    // Search steps are positive powers of two in every vendor caller.
    let scale_shift = 32 - (step as u32).leading_zeros();
    let scaled_denominator = denominator >> scale_shift;
    if scaled_denominator == 0 {
        return (current_third, current_fourth);
    }

    let mut third = current_third;
    if m1.wrapping_sub(m0.wrapping_mul(2)).wrapping_add(m2) > 0 {
        let numerator = m0
            .wrapping_mul(2)
            .wrapping_mul(m2.wrapping_sub(m1))
            .wrapping_add(m3.wrapping_mul(m1.wrapping_sub(m0).wrapping_sub(m5)))
            .wrapping_add(
                m4.wrapping_mul(
                    m0.wrapping_add(m1)
                        .wrapping_sub(m2.wrapping_mul(2))
                        .wrapping_add(m5)
                        .wrapping_add(m3)
                        .wrapping_sub(m4),
                ),
            );
        third = third.wrapping_sub(
            step.wrapping_mul(numerator >> scale_shift)
                .wrapping_div(scaled_denominator),
        );
    }

    let mut fourth = current_fourth;
    if m3.wrapping_sub(m0.wrapping_mul(2)).wrapping_add(m4) > 0 {
        let numerator = m0
            .wrapping_mul(
                m2.wrapping_sub(m1)
                    .wrapping_add(m4.wrapping_sub(m3).wrapping_mul(2)),
            )
            .wrapping_add(
                m2.wrapping_mul(
                    m1.wrapping_sub(m2)
                        .wrapping_add(m3)
                        .wrapping_sub(m4.wrapping_mul(2))
                        .wrapping_add(m5),
                ),
            )
            .wrapping_add(m1.wrapping_mul(m3.wrapping_sub(m5)));
        fourth = fourth.wrapping_sub(
            step.wrapping_mul(numerator >> scale_shift)
                .wrapping_div(scaled_denominator),
        );
    }
    (third, fourth)
}

pub(super) fn clamp_dynamic_iq_delta(delta: i32, step: i32) -> i32 {
    let limit = step.wrapping_mul(4);
    (delta / 2).clamp(limit.wrapping_neg(), limit)
}

/// Analytical stage 12 of the thirteen-stage dynamic IQ search.
pub fn refine_dynamic_iq_axis_pair(
    current_first: i32,
    current_second: i32,
    first_step: i32,
    second_step: i32,
    correlations: [DynamicIqCorrelation; 5],
) -> (i32, i32) {
    let [m0, m1, m2, m3, m4] = dynamic_iq_metrics(correlations);
    let mut first = current_first;
    let first_denominator = m1.wrapping_sub(m0.wrapping_mul(2)).wrapping_add(m2);
    if first_denominator > 0 {
        let delta = first_step
            .wrapping_mul(m1.wrapping_sub(m2))
            .wrapping_div(first_denominator);
        first = first.wrapping_add(clamp_dynamic_iq_delta(delta, first_step));
    }
    let mut second = current_second;
    let second_denominator = m3.wrapping_sub(m0.wrapping_mul(2)).wrapping_add(m4);
    if second_denominator > 0 {
        let delta = second_step
            .wrapping_mul(m3.wrapping_sub(m4))
            .wrapping_div(second_denominator);
        second = second.wrapping_add(clamp_dynamic_iq_delta(delta, second_step));
    }
    (first, second)
}

/// Hardware-operation schedule around one dispatcher stage in annotated
/// `rf_apply_channel_settings`. Operations occur in field order: capture,
/// candidate publication, then DFT correlation. Stages 0 and 7 intentionally
/// dispatch without a fresh measurement.
pub fn dynamic_iq_stage_execution(stage: u8, first_pass: bool) -> Option<DynamicIqStageExecution> {
    if stage >= 13 {
        return None;
    }
    let measure = stage != 0 && stage != 7;
    Some(DynamicIqStageExecution {
        capture_samples: measure,
        program_candidate: stage != 6 && stage != 12,
        correlate_samples: measure,
        initial_control_before_program: first_pass && stage == 0,
        initial_control_before_correlate: first_pass && stage == 1,
    })
}

/// Stage-local control-word construction from annotated
/// `rf_apply_channel_settings`. Stage 1's first-pass override affects the DFT
/// input but is deliberately not republished to the hardware register.
pub fn dynamic_iq_stage_control(
    configuration: u32,
    stage: u8,
    first_pass: bool,
) -> Option<DynamicIqStageControl> {
    if stage >= 13 {
        return None;
    }
    let phase = configuration >> 22;
    let stage_phase = if stage < 7 { phase >> 1 } else { phase };
    let base = (configuration & 0xffff_9fff) | 0x003f_0000;
    let control =
        ((base & 0x003f_ffff) | stage_phase.wrapping_shl(22)) & !0xff | (stage_phase & 0xff);
    let initial = |value: u32| (value & !0xff) | 0xff | 0xffc0_0000;
    let hardware = if first_pass && stage == 0 {
        initial(control)
    } else {
        control
    };
    let correlation = if first_pass && stage == 1 {
        initial(control)
    } else {
        hardware
    };
    Some(DynamicIqStageControl {
        hardware,
        correlation,
    })
}

/// Control word used by the post-average verification capture. The vendor
/// retains the full-phase stage-12 fields, then forces the low byte and upper
/// control mask before both hardware publication and DFT.
pub fn dynamic_iq_verification_control(configuration: u32) -> u32 {
    let control = dynamic_iq_stage_control(configuration, 12, false)
        .map(|value| value.hardware)
        .unwrap_or(0);
    (control & !0xff) | 0xff | 0xffc0_0000
}

/// Apply the persistent search-state effects of one switch case from annotated
/// `rf_op_dispatch2`.
///
/// Cases 1 and 8 consume all three DFT outputs for their local magnitude
/// calculations. The retained correlations below mirror the vendor workspace;
/// transient normalized magnitudes remain internal to the refinement helpers.
/// Cases 6 and 12 perform the two analytical refinements.
pub fn run_dynamic_iq_search_pass<F>(
    state: &mut DynamicIqSearchState,
    first_pass: bool,
    mut execute: F,
) -> bool
where
    F: FnMut(u8, [i32; 4], DynamicIqStageExecution) -> Option<DynamicIqDftResult>,
{
    for stage in 0..13 {
        let Some(execution) = dynamic_iq_stage_execution(stage, first_pass) else {
            return false;
        };
        let Some(measurement) = execute(stage, state.candidate, execution) else {
            return false;
        };
        if !apply_dynamic_iq_search_stage(state, stage, measurement) {
            return false;
        }
        state.current = reject_out_of_range_dynamic_iq(state.current);
    }
    true
}

pub fn run_dynamic_iq_averaged_search<F>(
    initial: [i32; 4],
    seed: [i32; 4],
    common_step: i32,
    first_step: i32,
    second_step: i32,
    requested_passes: u8,
    configuration_flags: u32,
    mut execute: F,
) -> Option<DynamicIqAveragedSearch>
where
    F: FnMut(u32, u8, [i32; 4], DynamicIqStageExecution) -> Option<DynamicIqDftResult>,
{
    let completed_passes = dynamic_iq_search_pass_count(requested_passes, configuration_flags);
    if completed_passes < 2 {
        return None;
    }
    let mut average = DynamicIqAverage::default();
    let mut baseline = None;
    let mut previous = initial;
    for pass_index in 0..completed_passes {
        let candidate = average.candidate_for_pass(pass_index, previous);
        let mut state = DynamicIqSearchState::new(candidate, common_step, first_step, second_step);
        if !run_dynamic_iq_search_pass(&mut state, pass_index == 0, |stage, values, plan| {
            let measurement = execute(pass_index, stage, values, plan)?;
            if pass_index == 0 && stage == 1 {
                baseline = Some(dynamic_iq_verification_metrics(measurement));
            }
            Some(measurement)
        }) {
            return None;
        }
        average.retain_pass(pass_index, state.current);
        previous = state.current;
    }
    let accepted_passes = completed_passes - 1;
    Some(DynamicIqAveragedSearch {
        baseline: baseline?,
        final_candidate: average.final_candidate(accepted_passes, seed)?,
        completed_passes,
        accepted_passes,
    })
}

pub fn apply_dynamic_iq_search_stage(
    state: &mut DynamicIqSearchState,
    stage: u8,
    measurement: DynamicIqDftResult,
) -> bool {
    match stage {
        0 => {
            state.candidate = state.current;
            state.candidate[2] = state.current[2].wrapping_sub(state.common_step);
        }
        1 => {
            state.reference_correlation = measurement.first;
            state.common_correlations[0] = measurement.second;
            state.axis_correlations[0] = measurement.third;
            state.candidate[2] = state.current[2].wrapping_add(state.common_step);
        }
        2 => {
            state.common_correlations[1] = measurement.second;
            state.candidate[2] = state.current[2].wrapping_add(state.common_step);
            state.candidate[3] = state.current[3].wrapping_add(state.common_step);
        }
        3 => {
            state.common_correlations[2] = measurement.second;
            state.candidate[2] = state.current[2];
            state.candidate[3] = state.current[3].wrapping_sub(state.common_step);
        }
        4 => {
            state.common_correlations[5] = measurement.second;
            state.candidate[3] = state.current[3].wrapping_add(state.common_step);
        }
        5 => state.common_correlations[3] = measurement.second,
        6 => {
            state.common_correlations[4] = measurement.second;
            let (third, fourth) = refine_dynamic_iq_common_pair(
                state.current[2],
                state.current[3],
                state.common_step,
                state.common_correlations,
            );
            state.current[2] = third;
            state.current[3] = fourth;
            state.candidate[2] = third;
            state.candidate[3] = fourth;
        }
        7 => state.candidate[1] = state.current[1].wrapping_sub(state.second_step),
        8 => {
            state.common_correlations[0] = measurement.second;
            state.axis_correlations[0] = measurement.third;
            state.candidate[1] = state.current[1].wrapping_add(state.second_step);
        }
        9 => {
            state.axis_correlations[3] = measurement.third;
            state.candidate[0] = state.current[0].wrapping_sub(state.first_step);
            state.candidate[1] = state.current[1];
        }
        10 => {
            state.axis_correlations[4] = measurement.third;
            state.candidate[0] = state.current[0].wrapping_add(state.first_step);
        }
        11 => state.axis_correlations[1] = measurement.third,
        12 => {
            state.axis_correlations[2] = measurement.third;
            let (first, second) = refine_dynamic_iq_axis_pair(
                state.current[0],
                state.current[1],
                state.first_step,
                state.second_step,
                state.axis_correlations,
            );
            state.current[0] = first;
            state.current[1] = second;
            state.candidate[0] = first;
            state.candidate[1] = second;
        }
        13 => {}
        _ => return false,
    }
    true
}

/// Outer search-pass count from annotated `rf_apply_channel_settings`.
/// Configuration bit 1 doubles the low-byte pass count.
pub const fn dynamic_iq_search_pass_count(requested: u8, configuration_flags: u32) -> u32 {
    requested as u32 * if configuration_flags & 2 == 0 { 1 } else { 2 }
}

impl DynamicIqAverage {
    /// Estimate a vendor averaging pass starts from. Passes zero and one
    /// continue from `previous`, the estimate the last pass ended with (the
    /// saved registers for pass zero); later passes start from the average of
    /// the accepted passes 1..pass_index-1.
    pub fn candidate_for_pass(&self, pass_index: u32, previous: [i32; 4]) -> [i32; 4] {
        if pass_index <= 1 {
            return previous;
        }
        self.accumulated
            .map(|value| value.wrapping_div(pass_index as i32 - 1))
    }

    /// Pass zero is the vendor baseline pass and is deliberately excluded from
    /// the accepted-candidate accumulator.
    pub fn retain_pass(&mut self, pass_index: u32, values: [i32; 4]) {
        if pass_index == 0 {
            return;
        }
        for (sum, value) in self.accumulated.iter_mut().zip(values) {
            *sum = sum.wrapping_add(value);
        }
    }

    pub fn final_candidate(&self, accepted_count: u32, seed: [i32; 4]) -> Option<[i32; 4]> {
        if accepted_count == 0 {
            return None;
        }
        let divisor = accepted_count as i32;
        Some(core::array::from_fn(|index| {
            self.accumulated[index]
                .wrapping_div(divisor)
                .wrapping_add(seed[index])
        }))
    }
}

/// Final additive seed at annotated `0x18d4e..0x18db0`.
pub fn dynamic_iq_final_seed(
    alternate_profile: bool,
    initial: DynamicIqInitialCandidate,
) -> [i32; 4] {
    if alternate_profile {
        [
            i32::from(initial.first),
            i32::from(initial.second),
            i32::from(initial.third),
            i32::from(initial.fourth),
        ]
    } else {
        [-2, 5, -1, -1]
    }
}

/// Pure verification and restoration decisions from annotated
/// `0x18dfe..0x18e9e`, cross-checked against Radare2 `pd:g` and Thumb
/// disassembly. `baseline` is the pass-zero stage-one metric triplet.
pub fn verify_dynamic_iq_candidate(
    values: [i32; 4],
    saved_values: [i32; 4],
    baseline: DynamicIqVerificationMetrics,
    measured: DynamicIqVerificationMetrics,
) -> DynamicIqVerification {
    let second_pair_rejected =
        baseline.second <= measured.second >> 2 || measured.reference <= measured.second;
    let first_pair_rejected =
        baseline.third <= measured.third >> 2 || measured.reference <= measured.third;
    let mut retained_values = values;
    if second_pair_rejected {
        retained_values[2] = saved_values[2];
        retained_values[3] = saved_values[3];
    }
    if first_pair_rejected {
        retained_values[0] = saved_values[0];
        retained_values[1] = saved_values[1];
    }
    DynamicIqVerification {
        retained_values,
        first_pair_rejected,
        second_pair_rejected,
        first_quality_failed: measured.reference <= measured.third.wrapping_shl(10),
        second_quality_failed: measured.reference <= measured.second.wrapping_shl(10),
        // This intentionally preserves the vendor's asymmetric final test at
        // 0x18e82: only the second-pair rejection flag suppresses publication.
        publication_allowed: !second_pair_rejected,
    }
}

pub fn finalize_dynamic_iq_search(
    averaged: DynamicIqAveragedSearch,
    saved_values: [i32; 4],
    measured: DynamicIqVerificationMetrics,
) -> DynamicIqFinalization {
    let verification = verify_dynamic_iq_candidate(
        averaged.final_candidate,
        saved_values,
        averaged.baseline,
        measured,
    );
    // The vendor snapshots the packed candidate before verification and
    // conditionally publishes those original words to software state. A
    // rejected first pair is restored in hardware later but is not substituted
    // in this asymmetric software publication.
    let publication = verification
        .publication_allowed
        .then(|| dynamic_iq_correction_plan(averaged.final_candidate));
    DynamicIqFinalization {
        verification,
        publication,
    }
}

/// Forced switch case 1 of `phy_cal_cmd_dispatch` (`0x16990`). Radare2 `pd:g`
/// confirms that the wrapper's fixed argument word `8` contributes bits 27 and
/// 31, producing `0x88000000 | (command & 0xff)`.
pub fn dynamic_iq_calibration_start_words(command: i32) -> DynamicIqCalibrationCommandWords {
    DynamicIqCalibrationCommandWords {
        abb805c: 0x12,
        abb8060: 0x8800_0000 | (command as u32 & 0xff),
        abb8064: 0,
    }
}

/// Forced switch case 0 of `phy_cal_cmd_dispatch`: clear all three command
/// words at `0x0abb805c..0x0abb8064`.
pub const fn dynamic_iq_calibration_stop_words() -> DynamicIqCalibrationCommandWords {
    DynamicIqCalibrationCommandWords {
        abb805c: 0,
        abb8060: 0,
        abb8064: 0,
    }
}

pub(super) fn write_dynamic_iq_calibration_command(words: DynamicIqCalibrationCommandWords) {
    write_u32(0x0abb_805c, words.abb805c);
    write_u32(0x0abb_8060, words.abb8060);
    write_u32(0x0abb_8064, words.abb8064);
}

/// Simple profile branch of annotated `rf_program_synth_freq` (`0x1869e`).
/// It performs the vendor's 21-step restoring division rather than replacing
/// it with wider host arithmetic, preserving all 32-bit wraparound behavior.
pub fn dynamic_iq_synth_register_mode1(frequency: i32, integer: i32, reference: u32) -> u32 {
    let mut remainder = 0x0004_c4b4_u32.wrapping_mul(frequency as u32);
    let mut divisor = reference.wrapping_mul(1000);
    let mut fractional = 0_u32;
    for bit in (0..=20).rev() {
        divisor >>= 1;
        if divisor <= remainder {
            remainder = remainder.wrapping_sub(divisor);
            fractional |= 1 << bit;
        }
    }
    fractional | (integer as u32).wrapping_shl(21)
}

/// Larger fixed-point branch of annotated `rf_program_synth_freq`, expressed
/// as the same wrapping 64-bit operations performed by the firmware helpers.
pub fn dynamic_iq_synth_register_mode0(
    frequency: u32,
    current_register: u32,
    reference: u32,
) -> Option<u32> {
    let first = u64::from(current_register.wrapping_add(10)).wrapping_mul(u64::from(reference))
        / 0x9c40_0000;
    let scaled_reference = u64::from(reference).wrapping_shl(25) / 1000;
    if scaled_reference == 0 {
        return None;
    }
    let ratio = first.wrapping_shl(25) / scaled_reference;
    let combined = scaled_reference
        .wrapping_mul(ratio)
        .wrapping_add(0x00a0_0000_u64.wrapping_mul(u64::from(frequency)));
    let denominator_base = scaled_reference.wrapping_shl(3) / 10;
    let denominator = denominator_base.wrapping_shl(3);
    if denominator == 0 {
        return None;
    }
    let combined = combined.wrapping_shl(3);
    let quotient = combined / denominator;
    let remainder = combined.wrapping_sub(quotient.wrapping_mul(denominator));
    let fraction = remainder.wrapping_shl(28) / denominator;
    let scaled_fraction = fraction.wrapping_mul(0x20_0000);
    let packed =
        scaled_fraction.wrapping_add((quotient & 0x7ff).wrapping_mul(0x2_0000).wrapping_shl(32));
    Some((packed / 0x1000_0000) as u32)
}

pub fn dynamic_iq_synth_register(
    mode: u8,
    frequency: i32,
    current_register: u32,
    reference: u32,
) -> Option<u32> {
    if mode == 1 {
        Some(dynamic_iq_synth_register_mode1(
            frequency,
            (current_register >> 21) as i32,
            reference,
        ))
    } else {
        dynamic_iq_synth_register_mode0(frequency as u32, current_register, reference)
    }
}

/// Read the live mode, reference and current PLL word and calculate the synth
/// result before any band register is modified.
///
/// The calibration state and PLL register must be initialized and readable.
pub fn prepare_dynamic_iq_synth_register(frequency: i32) -> Option<u32> {
    let mode = self::state().profile.get();
    let reference = self::state().reference_khz.get();
    let current_register = reg_read(0x0abc_00b4);
    dynamic_iq_synth_register(mode, frequency, current_register, reference)
}

pub fn dynamic_iq_band_derived_values(
    alternate_profile: bool,
    table_value: u32,
    source_04001ff4: u32,
) -> DynamicIqBandDerivedValues {
    let abc0020 = if alternate_profile {
        0x1f00_d000
    } else {
        ((source_04001ff4 & 0x07ff_ffff) >> 26)
            .wrapping_mul(0x2000_0000)
            .wrapping_add(0x0f00_c000)
    };
    DynamicIqBandDerivedValues {
        abc0020,
        abc0030: 0x3ff_u32.wrapping_sub(table_value),
        abb801c: table_value
            .wrapping_mul(0x400)
            .wrapping_add(if alternate_profile { 0x100 } else { 0xc0 }),
    }
}

/// Detached acquisition envelope from annotated `rf_save_band_regs`
/// (`0x187f4`), cross-checked against uninterrupted Thumb disassembly.
///
/// The MAC/PHY banks must be enabled. `synth_register` must have been prepared
/// from the still-unmodified live state. Pair this with
/// [`restore_dynamic_iq_band_registers`] on every non-fatal path.
pub fn begin_dynamic_iq_band_registers(
    alternate_profile: bool,
    table_value: u32,
    synth_register: u32,
    calibration_command: i32,
) -> DynamicIqBandRegisterSnapshot {
    let mut snapshot = DynamicIqBandRegisterSnapshot::default();
    delay_units(1);
    snapshot.abd0000 = reg_read(0x0abd_0000);
    write_u32(0x0abd_0000, 0x0000_4000);
    snapshot.abc0004 = reg_read(0x0abc_0004);
    write_u32(0x0abc_0004, 0x0000_0306);
    snapshot.abc0008 = reg_read(0x0abc_0008);
    write_u32(0x0abc_0008, 0x0000_8000);

    let derived;
    if alternate_profile {
        derived = dynamic_iq_band_derived_values(true, table_value, 0);
        delay_units(1);
        snapshot.abc0028 = reg_read(0x0abc_0028);
        write_u32(0x0abc_0028, 0x0be0_1f01);
        snapshot.abc00fc = reg_read(0x0abc_00fc);
        write_u32(0x0abc_00fc, 0);
        snapshot.abc006c = reg_read(0x0abc_006c);
        write_u32(0x0abc_006c, 0x0001_70fe);
        snapshot.abc0084 = reg_read(0x0abc_0084);
        write_u32(0x0abc_0084, 0x0840_7872);
        snapshot.abc0034 = reg_read(0x0abc_0034);
        write_u32(0x0abc_0034, 0x0000_0ecf);
        snapshot.abc0020 = reg_read(0x0abc_0020);
        write_u32(0x0abc_0020, derived.abc0020);
        snapshot.abc0050 = reg_read(0x0abc_0050);
        write_u32(0x0abc_0050, 0x0021_7c63);
        snapshot.abc0024 = reg_read(0x0abc_0024);
        write_u32(0x0abc_0024, 0x09ff_d803);
        delay_units(10);
        write_u32(0x0abc_0050, 0x0020_7c63);
        write_u32(0x0abc_0084, 0x0840_6872);
        // The vendor leaves the zeroed workspace slot as the restore value
        // in this branch instead of capturing the live register.
        write_u32(0x0abb_81a4, 0x5a);
    } else {
        snapshot.abc0028 = reg_read(0x0abc_0028);
        write_u32(0x0abc_0028, 0x0900_1f01);
        snapshot.abc00fc = reg_read(0x0abc_00fc);
        write_u32(0x0abc_00fc, 0);
        snapshot.abc006c = reg_read(0x0abc_006c);
        write_u32(0x0abc_006c, 0x0002_4f36);
        snapshot.abc0084 = reg_read(0x0abc_0084);
        write_u32(0x0abc_0084, 0x0840_b872);
        snapshot.abc0034 = reg_read(0x0abc_0034);
        write_u32(0x0abc_0034, 0x0000_06c3);
        snapshot.abc0020 = reg_read(0x0abc_0020);
        let source_04001ff4 = crate::platform::remap_window(0);
        derived = dynamic_iq_band_derived_values(false, table_value, source_04001ff4);
        write_u32(0x0abc_0020, derived.abc0020);
        snapshot.abc0050 = reg_read(0x0abc_0050);
        write_u32(0x0abc_0050, 0x0021_7c63);
        snapshot.abc0024 = reg_read(0x0abc_0024);
        write_u32(0x0abc_0024, 0x0987_1800);
        delay_units(10);
        write_u32(0x0abc_0050, 0x0020_7c63);
        write_u32(0x0abc_0084, 0x0840_a872);
        snapshot.abb81a4 = reg_read(0x0abb_81a4);
        write_u32(0x0abb_81a4, 0x56);
    }

    snapshot.abb801c = reg_read(0x0abb_801c);
    write_u32(0x0abb_801c, derived.abb801c);
    snapshot.abc0030 = reg_read(0x0abc_0030);
    write_u32(0x0abc_0030, derived.abc0030);
    snapshot.abc00b4 = reg_read(0x0abc_00b4);
    let mode = self::state().profile.get();
    let extended_settle = true;
    commit_channel_pll(synth_register, mode, extended_settle);
    snapshot.abb800c = reg_read(0x0abb_800c);
    write_u32(0x0abb_800c, 1);
    snapshot.abb8068 = reg_read(0x0abb_8068);
    snapshot.abb80a8 = reg_read(0x0abb_80a8);
    snapshot.abb81a8 = reg_read(0x0abb_81a8);
    write_u32(0x0abb_81a8, 2);
    write_dynamic_iq_calibration_command(dynamic_iq_calibration_start_words(calibration_command));
    snapshot.abb8004 = reg_read(0x0abb_8004);
    write_u32(0x0abb_8004, 0x0019_8600);
    delay_units(6);
    snapshot
}

/// Detached restoration envelope from annotated `rf_load_band_regs`
/// (`0x1843e`). The correction-register restore flags intentionally remain
/// mutable because the vendor workspace can change them during acquisition.
///
/// The snapshot must come from the matching acquisition.
pub fn restore_dynamic_iq_band_registers(snapshot: &DynamicIqBandRegisterSnapshot) {
    write_u32(0x0abc_0004, snapshot.abc0004);
    write_u32(0x0abc_0008, snapshot.abc0008);
    write_u32(0x0abc_0028, snapshot.abc0028);
    write_u32(0x0abc_0030, snapshot.abc0030);
    write_u32(0x0abc_00fc, snapshot.abc00fc);
    write_u32(0x0abc_006c, snapshot.abc006c);
    write_u32(0x0abc_0084, snapshot.abc0084);
    write_u32(0x0abc_0034, snapshot.abc0034);
    write_u32(0x0abc_0020, snapshot.abc0020);
    write_u32(0x0abc_0050, snapshot.abc0050);
    write_u32(0x0abc_0024, snapshot.abc0024);
    delay_units(10);
    write_u32(0x0abb_81a4, snapshot.abb81a4);
    write_dynamic_iq_calibration_command(dynamic_iq_calibration_stop_words());
    write_u32(0x0abb_800c, snapshot.abb800c);
    write_u32(0x0abb_801c, snapshot.abb801c);
    write_u32(0x0abb_81a8, snapshot.abb81a8);
    if snapshot.restore_abb80a8 {
        write_u32(0x0abb_80a8, snapshot.abb80a8);
    }
    if snapshot.restore_abb8068 {
        write_u32(0x0abb_8068, snapshot.abb8068);
    }
    write_u32(0x0abb_8004, snapshot.abb8004);
    let mode = self::state().profile.get();
    let extended_settle = true;
    commit_channel_pll(snapshot.abc00b4, mode, extended_settle);
    write_u32(0x0abd_0000, snapshot.abd0000);
    delay_units(6);
}

/// Control update from vendor `0x18600`, named `phy_set_reg2c_bit8` in the
/// annotated firmware archive.
pub fn dynamic_iq_capture_control(control: u32) -> u32 {
    control | 0x100
}

/// Publish the stage control to `0x0abb81ac`, matching
/// `phy_set_reg2c_bit8` (`0x18654`).
///
/// The MAC/PHY register bank must be enabled and writable.
pub fn apply_dynamic_iq_capture_control(control: u32) -> u32 {
    let published = dynamic_iq_capture_control(control);
    write_u32(0x0abb_81ac, published);
    published
}

/// Detached vendor `0x185bc`/annotated `rf_capture_adc_samples`: wait up to the
/// requested poll count for status bit 15 to clear, then copy all 64 ADC words.
/// The vendor proceeds with the copy after timeout, so readiness is returned
/// separately rather than suppressing the captured data.
pub fn capture_dynamic_iq_samples(output: &mut [u32; 64], max_polls: u32) -> bool {
    let mut ready = false;
    for _ in 0..max_polls {
        let status = reg_read(0x0abb_81ac);
        delay_units(1);
        if status & 0x8000 == 0 {
            ready = true;
            break;
        }
    }
    for (index, sample) in output.iter_mut().enumerate() {
        *sample = reg_read(0x0abb_81c4 + index * 4);
    }
    ready
}

pub(super) fn dynamic_iq_table_scale(table: &[i16; 64], phase: u32, sample: i32) -> i32 {
    sample.wrapping_mul(i32::from(table[(phase & 0x3f) as usize])) >> 5
}

pub(super) fn dynamic_iq_accumulate(
    correlation: &mut (i32, i32),
    phase: &mut u32,
    step: u32,
    sample: i32,
) {
    correlation.0 =
        correlation
            .0
            .wrapping_add(dynamic_iq_table_scale(&DYNAMIC_IQ_COSINE, *phase, sample));
    correlation.1 =
        correlation
            .1
            .wrapping_sub(dynamic_iq_table_scale(&DYNAMIC_IQ_SINE, *phase, sample));
    *phase = phase.wrapping_add(step) & 0x3f;
}

/// Pure translation of target `0x18480` (`rf_dft_correlate_samples` in the
/// annotated archive). It computes up to three signed complex correlations
/// from one 64-word ADC capture according to the vendor measurement mode.
pub fn dynamic_iq_dft(
    samples: &[u32; 64],
    control: u32,
    mode: u8,
    first_phase_seed: u32,
    second_phase_seed: u32,
    third_phase_seed: u32,
    sample_width_shift: u8,
) -> DynamicIqDftResult {
    let multiplier = ((control >> 13) & 3).wrapping_add(1);
    let first_step = (multiplier.wrapping_mul(first_phase_seed) & 0xff) >> 1;
    let second_step = (multiplier.wrapping_mul(second_phase_seed) & 0xff) >> 1;
    let third_step = (multiplier.wrapping_mul(third_phase_seed) & 0xff) >> 1;
    let encoded_width = control >> 22;
    let sample_shift = if encoded_width == 0 {
        0
    } else {
        32 - encoded_width.leading_zeros()
    };
    let sign_threshold = 0x7ff_u32.wrapping_shl(u32::from(sample_width_shift));
    let sample_modulus = 0x1000_u32.wrapping_shl(u32::from(sample_width_shift));

    let mut first = (0_i32, 0_i32);
    let mut second = (0_i32, 0_i32);
    let mut third = (0_i32, 0_i32);
    let mut first_phase = 0_u32;
    let mut second_phase = 0_u32;
    let mut third_phase = 0_u32;

    for &raw in samples {
        let raw = if raw > sign_threshold {
            raw.wrapping_sub(sample_modulus)
        } else {
            raw
        };
        let sample = ((raw.wrapping_shl(4) as i32) >> sample_shift) as i16 as i32;
        if matches!(mode, 1 | 8) {
            dynamic_iq_accumulate(&mut first, &mut first_phase, first_step, sample);
        }
        if matches!(mode, 1 | 8) || mode > 8 {
            dynamic_iq_accumulate(&mut third, &mut third_phase, third_step, sample);
        }
        if mode <= 8 {
            dynamic_iq_accumulate(&mut second, &mut second_phase, second_step, sample);
        }
    }

    DynamicIqDftResult {
        first: DynamicIqCorrelation {
            real: (first.0 >> 16) as i16,
            imaginary: (first.1 >> 16) as i16,
        },
        second: DynamicIqCorrelation {
            real: (second.0 >> 16) as i16,
            imaginary: (second.1 >> 16) as i16,
        },
        third: DynamicIqCorrelation {
            real: (third.0 >> 16) as i16,
            imaginary: (third.1 >> 16) as i16,
        },
    }
}

/// Execute one detached hardware stage in exact vendor order. Capture timeout
/// is reported but does not suppress the 64-word copy or DFT, matching
/// `rf_capture_adc_samples`.
///
/// The calibration engine and MAC/PHY register banks must already be enabled.
pub fn execute_dynamic_iq_hardware_stage(
    candidate: [i32; 4],
    execution: DynamicIqStageExecution,
    control: DynamicIqStageControl,
    configuration: DynamicIqDftConfiguration,
    samples: &mut [u32; 64],
) -> DynamicIqHardwareStageResult {
    let mut capture_ready = true;
    if execution.capture_samples {
        capture_ready = capture_dynamic_iq_samples(samples, configuration.capture_polls);
    }
    if execution.program_candidate {
        apply_dynamic_iq_correction_plan(dynamic_iq_correction_plan(candidate));
        apply_dynamic_iq_capture_control(control.hardware);
    }
    let measurement = if execution.correlate_samples {
        dynamic_iq_dft(
            samples,
            control.correlation,
            configuration.mode,
            configuration.first_phase_seed,
            configuration.second_phase_seed,
            configuration.third_phase_seed,
            configuration.sample_width_shift,
        )
    } else {
        DynamicIqDftResult::default()
    };
    DynamicIqHardwareStageResult {
        measurement,
        capture_ready,
    }
}

/// Connect the pass/dispatcher state machine to the detached MMIO acquisition
/// path. Readiness timeouts are accumulated for diagnostics but retain vendor
/// behavior by allowing every pass to continue with the copied sample window.
///
/// The dynamic-IQ band snapshot must already be active and all referenced
/// MAC/PHY registers must be accessible.
pub fn run_dynamic_iq_hardware_search(
    initial: [i32; 4],
    seed: [i32; 4],
    common_step: i32,
    first_step: i32,
    second_step: i32,
    requested_passes: u8,
    configuration_flags: u32,
    control_configuration: u32,
    dft_configuration: DynamicIqDftConfiguration,
    samples: &mut [u32; 64],
) -> DynamicIqHardwareSearchResult {
    let mut all_captures_ready = true;
    let search = run_dynamic_iq_averaged_search(
        initial,
        seed,
        common_step,
        first_step,
        second_step,
        requested_passes,
        configuration_flags,
        |pass_index, stage, candidate, execution| {
            let control = dynamic_iq_stage_control(control_configuration, stage, pass_index == 0)?;
            // Vendor `rf_dft_correlate_samples` reads the current dispatcher
            // stage from calibration-state offset 0x94. It is not a fixed
            // caller-supplied DFT mode.
            let mut stage_dft = dft_configuration;
            stage_dft.mode = stage;
            let result = execute_dynamic_iq_hardware_stage(
                candidate, execution, control, stage_dft, samples,
            );
            all_captures_ready &= result.capture_ready;
            Some(result.measurement)
        },
    );
    DynamicIqHardwareSearchResult {
        search,
        all_captures_ready,
    }
}

/// Publish the averaged candidate and perform the vendor's final stage-one
/// verification capture.
///
/// The dynamic-IQ band snapshot must remain active and the calibration engine
/// must still own the ADC path.
pub fn execute_dynamic_iq_hardware_verification(
    candidate: [i32; 4],
    control_configuration: u32,
    dft_configuration: DynamicIqDftConfiguration,
    samples: &mut [u32; 64],
) -> DynamicIqHardwareVerificationResult {
    let publication = dynamic_iq_correction_plan(candidate);
    apply_dynamic_iq_correction_plan(publication);
    replicate_dynamic_iq_correction_banks(publication);
    let control = dynamic_iq_verification_control(control_configuration);
    apply_dynamic_iq_capture_control(control);
    let capture_ready = capture_dynamic_iq_samples(samples, dft_configuration.capture_polls);
    // The vendor verification path stores stage 1 before its final capture.
    let measurement = dynamic_iq_dft(
        samples,
        control,
        1,
        dft_configuration.first_phase_seed,
        dft_configuration.second_phase_seed,
        dft_configuration.third_phase_seed,
        dft_configuration.sample_width_shift,
    );
    DynamicIqHardwareVerificationResult {
        metrics: dynamic_iq_verification_metrics(measurement),
        capture_ready,
    }
}

/// Complete detached hardware envelope for annotated
/// `rf_apply_channel_settings` (`0x1899c`). Every normal return restores the
/// saved band registers. Failed searches force restoration of both original IQ
/// words; successful verification maps each rejection flag to its matching
/// conditional restore before applying the optional vendor publication.
///
/// The caller must provide exclusive ownership of the calibration engine and
/// initialized live synth inputs for the active hardware profile.
pub fn run_dynamic_iq_hardware_calibration(
    configuration: DynamicIqHardwareCalibrationConfiguration,
    samples: &mut [u32; 64],
) -> DynamicIqHardwareCalibrationResult {
    let Some(synth_register) = prepare_dynamic_iq_synth_register(configuration.synth_frequency)
    else {
        return DynamicIqHardwareCalibrationResult::default();
    };
    let mut snapshot = {
        begin_dynamic_iq_band_registers(
            configuration.alternate_profile,
            configuration.table_value,
            synth_register,
            configuration.calibration_command,
        )
    };
    let first_saved = unpack_dynamic_iq_pair(snapshot.abb8068);
    let second_saved = unpack_dynamic_iq_pair(snapshot.abb80a8);
    let saved_values = [first_saved.0, first_saved.1, second_saved.0, second_saved.1];
    let hardware_search = {
        run_dynamic_iq_hardware_search(
            saved_values,
            configuration.seed,
            configuration.common_step,
            configuration.first_step,
            configuration.second_step,
            configuration.requested_passes,
            configuration.configuration_flags,
            configuration.control_configuration,
            configuration.dft,
            samples,
        )
    };
    let mut result = DynamicIqHardwareCalibrationResult {
        profile_shortcut: false,
        synth_prepared: true,
        finalization: None,
        all_search_captures_ready: hardware_search.all_captures_ready,
        verification_capture_ready: false,
    };
    if let Some(search) = hardware_search.search {
        let verification = {
            execute_dynamic_iq_hardware_verification(
                search.final_candidate,
                configuration.control_configuration,
                configuration.dft,
                samples,
            )
        };
        let finalization = finalize_dynamic_iq_search(search, saved_values, verification.metrics);
        snapshot.restore_abb8068 = finalization.verification.first_pair_rejected;
        snapshot.restore_abb80a8 = finalization.verification.second_pair_rejected;
        publish_dynamic_iq_final_state(configuration.alternate_profile, finalization);
        result.finalization = Some(finalization);
        result.verification_capture_ready = verification.capture_ready;
    } else {
        snapshot.restore_abb8068 = true;
        snapshot.restore_abb80a8 = true;
    }
    restore_dynamic_iq_band_registers(&snapshot);
    result
}

/// Exact outer profile gate from annotated `rf_apply_channel_settings`. A
/// nonzero profile byte skips acquisition and marks the alternate-profile state
/// valid; profile zero executes the primary path with its fixed final seed.
///
/// The PHY state and calibration hardware must be initialized.
pub fn run_dynamic_iq_calibration_for_profile(
    mut configuration: DynamicIqHardwareCalibrationConfiguration,
    samples: &mut [u32; 64],
) -> DynamicIqHardwareCalibrationResult {
    let profile = self::state().profile.get();
    if profile != 0 {
        self::state().profile1_ready.set(1);
        return DynamicIqHardwareCalibrationResult {
            profile_shortcut: true,
            ..DynamicIqHardwareCalibrationResult::default()
        };
    }
    configuration.alternate_profile = false;
    configuration.seed = [-2, 5, -1, -1];
    run_dynamic_iq_hardware_calibration(configuration, samples)
}

/// Vendor `0x19534 -> 0x19518`: rounded signed down-scaling followed by a
/// clamp to the requested output width.
pub fn rescale_signed(mut value: i32, output_bits: u8, input_bits: u8) -> i32 {
    let shift = input_bits.saturating_sub(output_bits);
    if shift != 0 {
        value = (value >> (shift - 1)).wrapping_add(1) >> 1;
    }
    let limit = 1_i32 << (output_bits - 1);
    value.clamp(-limit, limit - 1)
}

/// Vendor `0x17b70` sign-extends a 23-bit accumulator and converts it to a
/// rounded, saturated signed 12-bit sample through `0x19534`.
pub fn decode_calibration_accumulator(raw: u32) -> i32 {
    let signed = ((raw << 9) as i32) >> 9;
    rescale_signed(signed, 12, 23)
}

/// Low 16-bit I/Q coefficient packing performed by vendor `0x17ac8` after
/// signed 8-bit saturation.
pub fn pack_iq_signed8_pair(coefficient: IqCalibrationCoefficient) -> u16 {
    let i = rescale_signed(coefficient.i, 8, 8) as u8;
    let q = rescale_signed(coefficient.q, 8, 8) as u8;
    u16::from(i) | (u16::from(q) << 8)
}

/// Pure control-word construction from the enable branch of vendor `0x178ce`.
/// Pure coefficient normalization and packing from vendor `0x179ea`.
pub fn publish_iq_coefficient(
    mut coefficient: IqCalibrationCoefficient,
) -> Option<PublishedIqCoefficient> {
    if coefficient.i == 0 || coefficient.q == 0 {
        return None;
    }
    let mut magnitude = coefficient
        .i
        .unsigned_abs()
        .max(coefficient.q.unsigned_abs());
    let mut shift = 0_u8;
    while magnitude < 0x0004_0000 {
        magnitude = magnitude.wrapping_shl(1);
        coefficient.i = coefficient.i.wrapping_shl(1);
        coefficient.q = coefficient.q.wrapping_shl(1);
        shift = shift.wrapping_add(1);
    }
    let i = rescale_signed((-0x2000_0000_i32).wrapping_div(coefficient.i), 10, 12);
    let q = rescale_signed((-0x2000_0000_i32).wrapping_div(coefficient.q), 10, 12);
    Some(PublishedIqCoefficient {
        shift,
        normalized_i: coefficient.i,
        normalized_q: coefficient.q,
        packed: (i as u32 & 0x1ff) | ((q as u32 & 0x1ff) << 16),
    })
}

/// Exact rotate/subtract shift-state update at the tail of each `0x179ea`
/// publication iteration.
pub fn update_iq_shift_state(mut state: u32, shift: u8) -> u32 {
    let encoded_shift = u32::from(shift).wrapping_shl(28);
    state = state.rotate_right(8);
    state = state.wrapping_sub(encoded_shift);
    state = state.rotate_right(24);
    state = state.rotate_right(4);
    state = state.wrapping_sub(encoded_shift);
    state.rotate_right(28)
}

pub(super) fn iq_calibration_publication_from_source(
    gain_index: u32,
    coefficient: IqCalibrationCoefficient,
    normalization_source: IqCalibrationCoefficient,
    shift_state: u32,
) -> Option<IqCalibrationPublication> {
    let normalized = publish_iq_coefficient(normalization_source)?;
    let gain_offset = gain_index.wrapping_mul(4);
    Some(IqCalibrationPublication {
        primary_address: 0x0abb_8118_u32.wrapping_add(gain_offset),
        normalized_first_address: 0x0abb_8600_u32.wrapping_add(gain_offset),
        normalized_second_address: 0x0abb_8680_u32.wrapping_add(gain_offset),
        primary_value: pack_iq_signed8_pair(coefficient),
        normalized_value: normalized.packed,
        next_shift_state: update_iq_shift_state(shift_state, normalized.shift),
    })
}

pub fn iq_calibration_publication(
    gain_index: u32,
    coefficient: IqCalibrationCoefficient,
    shift_state: u32,
) -> Option<IqCalibrationPublication> {
    iq_calibration_publication_from_source(gain_index, coefficient, coefficient, shift_state)
}

/// Allocation-free twelve-gain arithmetic and publication plan from the main
/// loop of vendor `0x17c20`. Hardware sample acquisition remains separate.
pub(super) fn build_iq_calibration_series_in<'a>(
    samples: &[IqCalibrationSample; 12],
    initial_shift_state: u32,
    output: &'a mut MaybeUninit<IqCalibrationSeries>,
) -> &'a mut IqCalibrationSeries {
    let series = output.as_mut_ptr();
    // SAFETY: field projection of `output`'s storage; nothing is read.
    let iterations =
        unsafe { core::ptr::addr_of_mut!((*series).iterations).cast::<IqCalibrationIteration>() };
    let mut final_shift_state = initial_shift_state;
    for index in 0..12 {
        let sample = samples[index];
        let scale_i = sample
            .target_i
            .wrapping_sub(sample.baseline_i)
            .wrapping_mul(-0x100);
        let scale_q = sample
            .target_q
            .wrapping_sub(sample.baseline_q)
            .wrapping_mul(-0x100);
        let coefficient = primary_iq_calibration(sample);
        // Vendor `rf_compute_iq_gain_corr` normalizes the measurement deltas
        // at calibration-state offsets +0x14/+0x18. The signed-eight primary
        // coefficients published later come from the separate +0x114/+0x118
        // fields. Using the primary coefficients here left most normalized IQ
        // entries at their reset values whenever one rounded component was 0.
        // The profile shift word is reloaded for every gain.
        let publication = iq_calibration_publication_from_source(
            IQ_CALIBRATION_GAIN_INDICES[index],
            coefficient,
            IqCalibrationCoefficient {
                i: scale_i,
                q: scale_q,
            },
            initial_shift_state,
        );
        if let Some(publication) = publication {
            final_shift_state = publication.next_shift_state;
        }
        // SAFETY: `index` < 12, the length of `iterations`; written once.
        unsafe {
            iterations.add(index).write(IqCalibrationIteration {
                gain_index: IQ_CALIBRATION_GAIN_INDICES[index],
                sample,
                scale_i,
                scale_q,
                coefficient,
                publication,
            });
        }
    }
    // SAFETY: every field is now written: the twelve iterations above and
    // the final shift state here.
    unsafe {
        core::ptr::addr_of_mut!((*series).final_shift_state).write(final_shift_state);
        output.assume_init_mut()
    }
}

pub fn build_iq_calibration_series(
    samples: &[IqCalibrationSample; 12],
    initial_shift_state: u32,
) -> IqCalibrationSeries {
    let mut output = MaybeUninit::uninit();
    *build_iq_calibration_series_in(samples, initial_shift_state, &mut output)
}

pub fn apply_iq_calibration_primary(series: &IqCalibrationSeries) {
    for iteration in &series.iterations {
        let address = 0x0abb_8118_u32.wrapping_add(iteration.gain_index.wrapping_mul(4));
        write_u16(
            address as usize,
            pack_iq_signed8_pair(iteration.coefficient),
        );
    }
}

pub fn apply_iq_calibration_normalized(series: &IqCalibrationSeries) {
    for iteration in &series.iterations {
        if let Some(publication) = iteration.publication {
            write_u32(
                publication.normalized_first_address as usize,
                publication.normalized_value,
            );
            write_u32(
                publication.normalized_second_address as usize,
                publication.next_shift_state,
            );
        }
    }
}

/// Detached publication half of `0x17ac8 -> 0x179ea`.
pub fn apply_iq_calibration_series(series: &IqCalibrationSeries) {
    apply_iq_calibration_primary(series);
    apply_iq_calibration_normalized(series);
}

pub fn iq_calibration_controls(
    original_abc0050: u32,
    path: u8,
    band_mode: u8,
) -> IqCalibrationControls {
    let mut control = (original_abc0050 | 1) & !0x0e;
    control |= 0x1df0 | 0x2000 | 0x4000;
    control &= !0x0006_8000;
    control = control.wrapping_add(0x0004_0000);
    control &= !0x0018_0000;
    control = control.wrapping_add(0x0010_0000);
    control |= 0x0020_0000;
    control &= !0x7f00_0000;
    control &= 0x7fff_ffff;

    if path == 0 {
        control |= 0x0001_0000;
        control &= !0x00c0_0200;
    } else if path == 1 {
        control &= !0x0001_0000;
        if band_mode == 1 {
            control |= 0x0040_0000;
            control &= !0x0080_0000;
        } else {
            control |= 0x0080_0000;
            control &= !0x0040_0000;
        }
        control |= 0x200;
    }

    IqCalibrationControls {
        during_settle: control,
        active: if path == 0 {
            control & !0x0001_0000
        } else {
            control
        },
        abc0034: 0x1fff,
    }
}

/// Complete enable branch of vendor `0x178ce` with a returned restoration
/// snapshot replacing its vendor workspace slots.
pub fn begin_iq_calibration_path(path: u8, band_mode: u8) -> IqCalibrationRegisterSnapshot {
    let snapshot = IqCalibrationRegisterSnapshot {
        abc0004: reg_read(0x0abc_0004),
        abc0034: reg_read(0x0abc_0034),
        abc0050: reg_read(0x0abc_0050),
    };
    let controls = iq_calibration_controls(snapshot.abc0050, path, band_mode);
    write_u32(0x0abc_0004, snapshot.abc0004 | 2);
    write_u32(0x0abc_0050, controls.during_settle);
    write_u32(0x0abc_0034, controls.abc0034);
    delay_timer_ticks(10);
    if controls.active != controls.during_settle {
        write_u32(0x0abc_0050, controls.active);
    }
    snapshot
}

/// Exact disable/restore branch of vendor `0x178ce`.
pub fn end_iq_calibration_path(snapshot: IqCalibrationRegisterSnapshot) {
    write_u32(0x0abc_0004, snapshot.abc0004);
    write_u32(0x0abc_0050, snapshot.abc0050);
    write_u32(0x0abc_0034, snapshot.abc0034);
}

/// Vendor `0x179c0` clears the sample fields while preserving unrelated bits.
pub fn clear_calibration_sample_settings() {
    let value = reg_read(0x0abb_8114);
    write_u32(0x0abb_8114, value & !0x03ff_03ff);
}

pub(super) fn finish_iq_calibration_hardware(
    snapshot: IqCalibrationRegisterSnapshot,
    selected_mode: u8,
) {
    clear_calibration_sample_settings();
    end_iq_calibration_path(snapshot);
    configure_calibration_mode(selected_mode);
    set_calibration_gain(-1);
    set_calibration_engine_enabled(false);
}

/// Complete acquisition/publication envelope of annotated `rf_calibrate_iq_dc`
/// (`0x17c74`, target `0x17c20`). All timeout exits restore the path, band
/// selector, test tone and calibration-engine gate.
///
/// The caller must exclusively own the RF calibration engine and MAC/PHY
/// register banks.
#[inline(never)]
pub(super) fn run_iq_calibration_core(
    include_secondary: bool,
    max_polls: u32,
) -> Result<(IqCalibrationReferences, Option<IqCalibrationCoefficient>), IqCalibrationHardwareError>
{
    const EMPTY_SAMPLE: IqCalibrationSample = IqCalibrationSample {
        baseline_i: 0,
        baseline_q: 0,
        target_i: 0,
        target_q: 0,
    };

    set_calibration_engine_enabled(true);
    let selected_mode = reg_read(0x0abb_80f0) as u8 & 3;
    if self::state().auxiliary_state.get() == 0 {
        let shift_state = reg_read(0x0abb_8680);
        self::state().measurement_shift_state.set(shift_state);
    }
    let initial_shift_state = { self::state().measurement_shift_state.get() };
    configure_calibration_mode(0);
    let snapshot = begin_iq_calibration_path(0, 0);
    let mut scratch = IQ_CALIBRATION_SCRATCH.borrow_mut();
    let IqCalibrationScratch {
        samples,
        series: series_storage,
        built,
    } = &mut *scratch;
    *built = false;
    samples.fill(EMPTY_SAMPLE);
    for (index, gain_index) in IQ_CALIBRATION_GAIN_INDICES.iter().copied().enumerate() {
        set_calibration_gain(gain_index as i32);
        let baseline = match run_calibration_sample(0x11, 0x11, max_polls) {
            Ok(value) => value,
            Err(_) => {
                finish_iq_calibration_hardware(snapshot, selected_mode);
                return Err(IqCalibrationHardwareError::BaselineTimeout { gain_index });
            }
        };
        let target = match run_calibration_sample(1, 1, max_polls) {
            Ok(value) => value,
            Err(_) => {
                finish_iq_calibration_hardware(snapshot, selected_mode);
                return Err(IqCalibrationHardwareError::TargetTimeout { gain_index });
            }
        };
        samples[index] = IqCalibrationSample {
            baseline_i: baseline.i,
            baseline_q: baseline.q,
            target_i: target.i,
            target_q: target.q,
        };
    }
    let series = build_iq_calibration_series_in(samples, initial_shift_state, series_storage);
    *built = true;
    let final_iteration = &series.iterations[11];
    let references = IqCalibrationReferences {
        coefficient_i: final_iteration.coefficient.i,
        coefficient_q: final_iteration.coefficient.q,
        scale_i: final_iteration.scale_i,
        scale_q: final_iteration.scale_q,
    };
    self::state().auxiliary_state.set(1);

    let secondary = if include_secondary {
        let dac_i = rescale_signed(references.coefficient_i, 6, 8) as u8;
        let dac_q = rescale_signed(references.coefficient_q, 6, 8) as u8;
        set_calibration_gain(0x20);
        let baseline = match run_calibration_sample_mode(dac_i, dac_q, 1, max_polls) {
            Ok(value) => value,
            Err(_) => {
                finish_iq_calibration_hardware(snapshot, selected_mode);
                return Err(IqCalibrationHardwareError::SecondaryBaselineTimeout);
            }
        };
        let profile = self::state().profile.get();
        let secondary_snapshot = begin_iq_calibration_path(1, profile);
        let target = match run_calibration_sample_mode(dac_i, dac_q, 0, max_polls) {
            Ok(value) => value,
            Err(_) => {
                end_iq_calibration_path(secondary_snapshot);
                finish_iq_calibration_hardware(snapshot, selected_mode);
                return Err(IqCalibrationHardwareError::SecondaryTargetTimeout);
            }
        };
        end_iq_calibration_path(secondary_snapshot);
        let Some(correction) = secondary_iq_calibration(
            baseline.i,
            baseline.q,
            target.i,
            target.q,
            references.scale_i,
            references.scale_q,
        ) else {
            finish_iq_calibration_hardware(snapshot, selected_mode);
            return Err(IqCalibrationHardwareError::InvalidSecondaryScale);
        };
        Some(correction)
    } else {
        None
    };

    apply_iq_calibration_primary(series);
    if let Some(correction) = secondary {
        write_u16(0x0abb_8198, pack_iq_signed8_pair(correction));
        write_u16(0x0abb_819c, 0);
    }
    apply_iq_calibration_normalized(series);
    finish_iq_calibration_hardware(snapshot, selected_mode);
    Ok((references, secondary))
}

pub fn run_iq_calibration(
    include_secondary: bool,
    max_polls: u32,
) -> Result<IqCalibrationHardwareResult, IqCalibrationHardwareError> {
    let (references, secondary) = run_iq_calibration_core(include_secondary, max_polls)?;
    let Some(&series) = IQ_CALIBRATION_SCRATCH.borrow().series() else {
        crate::halt_always!();
    };
    Ok(IqCalibrationHardwareResult {
        series,
        references,
        secondary,
    })
}

pub fn run_primary_iq_calibration(
    max_polls: u32,
) -> Result<IqCalibrationHardwareResult, IqCalibrationHardwareError> {
    run_iq_calibration(false, max_polls)
}
