use super::*;

fn channel6_gain_input(rate: u8, power_tenths_dbm: i32) -> GainComputationInput {
    let rate_index = rate.min(10);
    let rate_base = if rate_index < 2 {
        304
    } else if rate_index < 10 {
        288
    } else {
        272
    };
    let rate_scale = match rate_index {
        0 | 1 => 380,
        2..=6 => 348,
        7 => 310,
        8 => 276,
        9 => 246,
        _ => 220,
    };
    GainComputationInput {
        rate: rate_index,
        first_limit: power_tenths_dbm * 16 / 10,
        second_limit: 480.min(rate_base + power_tenths_dbm * 16 / 10),
        rate_base,
        gain_coefficient_a: 23_000,
        gain_coefficient_b: 45,
        rssi_rate_scale: rate_scale,
        rssi_temperature_coefficient: -14,
        rssi_divisor_coefficient: -30,
        rssi_multiplier_coefficient: 100,
        rssi_denominator: 125,
        rssi_offset: 0,
        measured_a: 38_000,
        measured_b: 48_000,
        analog_enabled: 0x9600,
        analog_word_2c: 0,
        analog_word_30: 0,
        state_scale: -1,
    }
}

#[test]
fn cooperative_transition_waits_across_timer_wrap() {
    let mut scheduler = ChannelTransitionScheduler::new();
    assert!(scheduler.is_idle());
    scheduler.state = 1;
    scheduler.arm_settle(0xffff_fff0);
    assert_eq!(scheduler.deadline, 0x68);
    assert_eq!(scheduler.service(0x20), Ok(None));
    assert!(!scheduler.is_idle());
}

#[test]
fn gain_entry_encoding_matches_vendor_oracle() {
    for (rate, power, gain, rssi, entry, companion) in [
        (0, 249, 1020, 653, 0x000f_f28d, 0x0ec1),
        (2, 233, 1020, 519, 0x000f_f207, 0x0e6e),
        (10, 217, 1020, 462, 0x000f_f1ce, 0x0e3d),
    ] {
        let result = compute_gain_entry(channel6_gain_input(rate, 200)).unwrap();
        assert_eq!(result.selected_power, power);
        assert_eq!(result.gain_code, gain);
        assert_eq!(result.rssi_value, rssi);
        assert_eq!(encoded_gain_words(gain, rssi), (entry, companion));
    }
}

#[test]
fn gain_entries_match_vendor_across_power_levels() {
    for (power, rate, entry, companion) in [
        (-100, 0, 0x3113, 0x0d0a),
        (-100, 2, 0x3114, 0x0d0c),
        (-100, 10, 0x70b8, 0x0b92),
        (0, 0, 0x7122, 0x0d31),
        (0, 2, 0x7102, 0x0cd7),
        (0, 10, 0xf0c2, 0x0bcd),
        (100, 0, 0x000f_f158, 0x0da2),
        (100, 2, 0x000f_f132, 0x0d56),
        (100, 10, 0x000f_f132, 0x0d56),
    ] {
        let result = compute_gain_entry(channel6_gain_input(rate, power)).unwrap();
        assert_eq!(
            encoded_gain_words(result.gain_code, result.rssi_value),
            (entry, companion),
            "power={power}, rate={rate}",
        );
    }
}

#[test]
fn gain_index_uses_vendor_mapping_boundaries() {
    assert_eq!(gain_index_from_value(0), 0x1_0000);
    assert_eq!(gain_code(4), 4);
    assert_eq!(gain_code(32), 1020);
}

#[test]
fn vendor_calibration_tables_have_all_anchors() {
    assert_eq!(IQ_CALIBRATION_GAIN_INDICES.len(), 12);
    assert_eq!(IQ_CALIBRATION_GAIN_INDICES[0], 0x1a);
    assert_eq!(IQ_CALIBRATION_GAIN_INDICES[11], 0);
    assert_eq!(MODE0_CALIBRATION_ANCHORS.len(), 22);
    assert_eq!(MODE1_CALIBRATION_ANCHORS.len(), 22);
    assert_eq!(MODE0_CALIBRATION_ANCHORS[0].selector, 26);
    assert_eq!(MODE1_CALIBRATION_ANCHORS[21].lower, -1547);
}

#[test]
fn normalization_matches_vendor_signed_rounding() {
    assert_eq!(
        normalize_anchor(MODE0_CALIBRATION_ANCHORS[0], 0),
        CalibrationAnchor {
            selector: 26,
            upper: 0,
            lower: -64,
        }
    );
}

#[test]
fn channel_control_matches_vendor_scan_mode() {
    assert_eq!(channel_control_word(0, 0, 1), 0x0117);
    assert_eq!(channel_control_word(1, 2, 0x101), 0x0466);
    assert_eq!(channel_control_gate(0x0117), 1);
    assert_eq!(channel_control_gate(0x0122), 0x40);
    assert_eq!(channel_control_gate(0x0112), 0x40);
    assert_eq!(
        channel_tune_plan(0, 0x0117),
        ChannelTunePlan {
            phy_mode: 2,
            recalibrate: true,
        }
    );
    assert_eq!(
        channel_tune_plan(0, 0x0137),
        ChannelTunePlan {
            phy_mode: 3,
            recalibrate: false,
        }
    );
    assert!(!channel_tune_plan(2, 0x0117).recalibrate);
    assert_eq!(
        mode_transition_action(2, false, 2, false),
        ModeTransitionAction::ClearChannelPending
    );
    assert_eq!(
        mode_transition_action(2, true, 2, false),
        ModeTransitionAction::ReturnUnchanged
    );
    assert_eq!(
        mode_transition_action(2, false, 3, false),
        ModeTransitionAction::Reinitialize
    );
    assert_eq!(channel_frequency_khz_2ghz(1), 2_412_000);
    assert_eq!(channel_frequency_khz_2ghz(6), 2_437_000);
    assert_eq!(channel_frequency_khz_2ghz(13), 2_472_000);
    assert_eq!(channel_frequency_khz_2ghz(14), 2_484_000);
    assert_eq!(
        channel_measurement_timing(0, channel_frequency_khz_2ghz(7)),
        Some(-134)
    );
    assert_eq!(channel_frequency_offset_mhz(0, 2_442_000), 0);
    assert_eq!(channel_frequency_offset_mhz(0, 2_437_000), -5);
    assert_eq!(channel_frequency_offset_mhz(1, 2_442_000), -3058);
    assert_eq!(measurement_path_value(0), 0x0ebf);
    assert_eq!(measurement_path_value(1), 0);
    assert_eq!(scale_channel_measurement(715, 1032, -852), Some(50_016));
    assert_eq!(select_mode0_measurement(50_016), (50_016, true));
    assert_eq!(select_mode0_measurement(40_999), (38_000, false));
    assert_eq!(select_mode0_measurement(58_989), (38_000, false));
    assert!(channel_measurement_ready(MODE0_MEASUREMENT_CONTROL));
    assert!(!channel_measurement_ready(MODE0_MEASUREMENT_CONTROL | 0x20));
    assert_eq!(
        pll_divider(channel_frequency_khz_2ghz(6), 1250, 26_000),
        Some(PllDivider {
            integer: 117_163,
            fractional: 967_916,
            register: 0x356e_c4ec,
        })
    );
    let corrections = [
        ChannelThresholdCorrection {
            upper_channel: 3,
            value: 10,
        },
        ChannelThresholdCorrection {
            upper_channel: 8,
            value: 20,
        },
    ];
    assert_eq!(select_threshold_correction(2, -1, &corrections), -1);
    assert_eq!(select_threshold_correction(6, -1, &corrections), 10);
    assert_eq!(select_threshold_correction(11, -1, &corrections), 20);
    let steps = [
        ChannelCalibrationStep {
            upper_channel: 3,
            first: 2,
            second: 4,
        },
        ChannelCalibrationStep {
            upper_channel: 8,
            first: 5,
            second: 7,
        },
    ];
    assert_eq!(select_channel_calibration(2, true, 100, &steps), Some(108));
    assert_eq!(select_channel_calibration(6, false, 100, &steps), Some(116));
    assert_eq!(
        primary_iq_calibration(IqCalibrationSample {
            baseline_i: 10,
            baseline_q: -10,
            target_i: 20,
            target_q: 0,
        }),
        IqCalibrationCoefficient { i: 132, q: 4 }
    );
    assert_eq!(
        secondary_iq_calibration(20, -10, 10, -20, 5, 5),
        Some(IqCalibrationCoefficient {
            i: 32_768,
            q: 32_768,
        })
    );
    assert_eq!(
        dynamic_iq_initial_candidate(0),
        DynamicIqInitialCandidate {
            first: -4,
            second: -11,
            third: -4,
            fourth: 0,
        }
    );
    assert_eq!(
        dynamic_iq_initial_candidate(2),
        DynamicIqInitialCandidate {
            first: 7,
            second: -7,
            third: -5,
            fourth: 1,
        }
    );
    assert_eq!(
        dynamic_iq_correction_plan([-4, -11, -5, 1]),
        DynamicIqCorrectionPlan {
            first_address: 0x0abb_8068,
            second_address: 0x0abb_80a8,
            first_value: 0x0ff5_0ffc,
            second_value: 0x0001_0ffb,
        }
    );
    assert_eq!(dynamic_iq_capture_control(0x1234), 0x1334);
    assert_eq!(dynamic_iq_capture_control(0x1334), 0x1334);
    assert_eq!(
        reject_out_of_range_dynamic_iq([0x3ff, 0x801, 0xfff, 0x1801]),
        [0x3ff, 0, 0xfff, 0]
    );
    let mut impulse = [0_u32; 64];
    impulse[0] = 0x7ff;
    let all_correlations = dynamic_iq_dft(&impulse, 0, 1, 1, 1, 1, 0);
    assert_eq!(
        all_correlations,
        DynamicIqDftResult {
            first: DynamicIqCorrelation {
                real: 511,
                imaginary: 0,
            },
            second: DynamicIqCorrelation {
                real: 511,
                imaginary: 0,
            },
            third: DynamicIqCorrelation {
                real: 511,
                imaginary: 0,
            },
        }
    );
    assert_eq!(
        dynamic_iq_dft(&impulse, 0, 2, 1, 1, 1, 0),
        DynamicIqDftResult {
            second: DynamicIqCorrelation {
                real: 511,
                imaginary: 0,
            },
            ..DynamicIqDftResult::default()
        }
    );
    assert_eq!(
        dynamic_iq_dft(&impulse, 0, 9, 1, 1, 1, 0),
        DynamicIqDftResult {
            third: DynamicIqCorrelation {
                real: 511,
                imaginary: 0,
            },
            ..DynamicIqDftResult::default()
        }
    );
    assert_eq!(
        refine_dynamic_iq_common_pair(
            100,
            200,
            8,
            [
                DynamicIqCorrelation {
                    real: 1,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 2,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 3,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 4,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 5,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 6,
                    imaginary: 0
                },
            ],
        ),
        (99, 200)
    );
    assert_eq!(
        refine_dynamic_iq_axis_pair(
            100,
            200,
            8,
            8,
            [
                DynamicIqCorrelation {
                    real: 1,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 3,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 1,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 3,
                    imaginary: 0
                },
                DynamicIqCorrelation {
                    real: 1,
                    imaginary: 0
                },
            ],
        ),
        (104, 204)
    );

    let measurement = |first, second, third| DynamicIqDftResult {
        first: DynamicIqCorrelation {
            real: first,
            imaginary: 0,
        },
        second: DynamicIqCorrelation {
            real: second,
            imaginary: 0,
        },
        third: DynamicIqCorrelation {
            real: third,
            imaginary: 0,
        },
    };
    assert_eq!(
        dynamic_iq_stage_execution(0, true),
        Some(DynamicIqStageExecution {
            capture_samples: false,
            program_candidate: true,
            correlate_samples: false,
            initial_control_before_program: true,
            initial_control_before_correlate: false,
        })
    );
    assert_eq!(
        dynamic_iq_stage_execution(6, false),
        Some(DynamicIqStageExecution {
            capture_samples: true,
            program_candidate: false,
            correlate_samples: true,
            initial_control_before_program: false,
            initial_control_before_correlate: false,
        })
    );
    assert_eq!(
        dynamic_iq_stage_execution(7, false),
        Some(DynamicIqStageExecution {
            capture_samples: false,
            program_candidate: true,
            correlate_samples: false,
            initial_control_before_program: false,
            initial_control_before_correlate: false,
        })
    );
    assert_eq!(dynamic_iq_stage_execution(13, false), None);
    assert_eq!(
        dynamic_iq_stage_control(0x1234_5678, 0, true),
        Some(DynamicIqStageControl {
            hardware: 0xffff_16ff,
            correlation: 0xffff_16ff,
        })
    );
    assert_eq!(
        dynamic_iq_stage_control(0x1234_5678, 1, true),
        Some(DynamicIqStageControl {
            hardware: u32::from_be_bytes([0x09, 0x3f, 0x16, 0x24]),
            correlation: 0xffff_16ff,
        })
    );
    assert_eq!(
        dynamic_iq_stage_control(0x1234_5678, 7, false),
        Some(DynamicIqStageControl {
            hardware: 0x123f_1648,
            correlation: 0x123f_1648,
        })
    );
    assert_eq!(dynamic_iq_stage_control(0, 13, false), None);
    assert_eq!(dynamic_iq_verification_control(0x1234_5678), 0xffff_16ff);

    let mut pass_search = DynamicIqSearchState::new([10, 20, 30, 40], 2, 3, 4);
    let mut executed_stages = 0_u8;
    assert!(run_dynamic_iq_search_pass(
        &mut pass_search,
        true,
        |stage, _candidate, execution| {
            assert_eq!(stage, executed_stages);
            assert_eq!(execution, dynamic_iq_stage_execution(stage, true).unwrap());
            executed_stages += 1;
            Some(measurement(
                i16::from(stage) + 1,
                i16::from(stage) + 2,
                i16::from(stage) + 3,
            ))
        }
    ));
    assert_eq!(executed_stages, 13);

    let mut averaged_executions = 0;
    let averaged = run_dynamic_iq_averaged_search(
        [1, 2, 3, 4],
        [10, 20, 30, 40],
        0,
        0,
        0,
        3,
        0,
        |_pass, _stage, _candidate, _execution| {
            averaged_executions += 1;
            Some(DynamicIqDftResult::default())
        },
    )
    .unwrap_or_default();
    assert_eq!(averaged_executions, 39);
    assert_eq!(averaged.completed_passes, 3);
    assert_eq!(averaged.accepted_passes, 2);
    assert_eq!(averaged.final_candidate, [11, 22, 33, 44]);
    assert_eq!(averaged.baseline, DynamicIqVerificationMetrics::default());

    let mut search = DynamicIqSearchState::new([10, 20, 30, 40], 2, 3, 4);
    assert!(apply_dynamic_iq_search_stage(
        &mut search,
        0,
        DynamicIqDftResult::default()
    ));
    assert_eq!(search.candidate, [10, 20, 28, 40]);
    apply_dynamic_iq_search_stage(&mut search, 1, measurement(11, 12, 13));
    assert_eq!(search.reference_correlation.real, 11);
    assert_eq!(search.common_correlations[0].real, 12);
    assert_eq!(search.axis_correlations[0].real, 13);
    assert_eq!(search.candidate, [10, 20, 32, 40]);
    apply_dynamic_iq_search_stage(&mut search, 2, measurement(0, 22, 0));
    assert_eq!(search.candidate, [10, 20, 32, 42]);
    apply_dynamic_iq_search_stage(&mut search, 3, measurement(0, 32, 0));
    assert_eq!(search.candidate, [10, 20, 30, 38]);
    apply_dynamic_iq_search_stage(&mut search, 4, measurement(0, 42, 0));
    assert_eq!(search.candidate, [10, 20, 30, 42]);
    apply_dynamic_iq_search_stage(&mut search, 5, measurement(0, 52, 0));
    assert_eq!(
        search.common_correlations.map(|value| value.real),
        [12, 22, 32, 52, 0, 42]
    );
    apply_dynamic_iq_search_stage(&mut search, 7, DynamicIqDftResult::default());
    assert_eq!(search.candidate, [10, 16, 30, 42]);
    apply_dynamic_iq_search_stage(&mut search, 8, measurement(81, 82, 83));
    assert_eq!(search.reference_correlation.real, 11);
    assert_eq!(search.candidate, [10, 24, 30, 42]);
    apply_dynamic_iq_search_stage(&mut search, 9, measurement(0, 0, 93));
    assert_eq!(search.candidate, [7, 20, 30, 42]);
    apply_dynamic_iq_search_stage(&mut search, 10, measurement(0, 0, 103));
    assert_eq!(search.candidate, [13, 20, 30, 42]);
    apply_dynamic_iq_search_stage(&mut search, 11, measurement(0, 0, 113));
    assert_eq!(
        search.axis_correlations.map(|value| value.real),
        [83, 113, 0, 93, 103]
    );
    assert!(!apply_dynamic_iq_search_stage(
        &mut search,
        14,
        DynamicIqDftResult::default()
    ));

    // A probe past the estimate's range is still programmed; only the
    // estimate is range-checked (an out-of-range estimate is zeroed).
    let mut edge = DynamicIqSearchState::new([0, -300, 0, 0], 0x100, 0x100, 0x100);
    let mut probes = [[0; 4]; 13];
    assert!(run_dynamic_iq_search_pass(&mut edge, false, |stage, candidate, _| {
        probes[usize::from(stage)] = candidate;
        Some(DynamicIqDftResult::default())
    }));
    // Stage 8 programs the probe stage 7 chose: -300 - 0x100.
    assert_eq!(probes[8][1], -556);
    assert_eq!(edge.current[1], -300);
    let mut outside = DynamicIqSearchState::new([0, -600, 0, 0], 1, 1, 1);
    assert!(run_dynamic_iq_search_pass(&mut outside, false, |_, _, _| {
        Some(DynamicIqDftResult::default())
    }));
    assert_eq!(outside.current[1], 0);

    assert_eq!(dynamic_iq_search_pass_count(3, 0), 3);
    assert_eq!(dynamic_iq_search_pass_count(3, 2), 6);
    assert_eq!(
        unpack_dynamic_iq_pair(dynamic_iq_correction_plan([-4, -11, -5, 1]).first_value),
        (-4, -11)
    );
    assert_eq!(
        dynamic_iq_calibration_start_words(7),
        DynamicIqCalibrationCommandWords {
            abb805c: 0x12,
            abb8060: 0x8800_0007,
            abb8064: 0,
        }
    );
    assert_eq!(
        dynamic_iq_calibration_stop_words(),
        DynamicIqCalibrationCommandWords::default()
    );
    assert_eq!(dynamic_iq_synth_register_mode1(1, 3, 24_000), 0x0060_6aab);
    assert_eq!(dynamic_iq_synth_register_mode1(10, 7, 24_000), 0x00e4_2aab);
    assert_eq!(
        dynamic_iq_synth_register(1, 1, 3 << 21, 24_000),
        Some(0x0060_6aab)
    );
    assert_eq!(
        dynamic_iq_synth_register_mode0(1, 0x0012_3456, 24_000),
        Some(0x0000_8555)
    );
    assert_eq!(
        dynamic_iq_synth_register_mode0(64, 0x0065_4321, 24_000),
        Some(0x0071_5555)
    );
    assert_eq!(
        dynamic_iq_band_derived_values(false, 0x10, 0x0400_0000),
        DynamicIqBandDerivedValues {
            abc0020: 0x2f00_c000,
            abc0030: 0x3ef,
            abb801c: 0x40c0,
        }
    );
    assert_eq!(
        dynamic_iq_band_derived_values(true, 0x10, 0),
        DynamicIqBandDerivedValues {
            abc0020: 0x1f00_d000,
            abc0030: 0x3ef,
            abb801c: 0x4100,
        }
    );
    let mut average = DynamicIqAverage::default();
    average.retain_pass(0, [100, 100, 100, 100]);
    assert_eq!(average.accumulated, [0; 4]);
    average.retain_pass(1, [10, 20, 30, 40]);
    assert_eq!(
        average.candidate_for_pass(2, [1, 2, 3, 4]),
        [10, 20, 30, 40]
    );
    average.retain_pass(2, [20, 30, 40, 50]);
    assert_eq!(
        average.candidate_for_pass(3, [1, 2, 3, 4]),
        [15, 25, 35, 45]
    );
    assert_eq!(
        average.final_candidate(2, [-2, 5, -1, -1]),
        Some([13, 30, 34, 44])
    );
    assert_eq!(
        dynamic_iq_final_seed(false, dynamic_iq_initial_candidate(2)),
        [-2, 5, -1, -1]
    );
    assert_eq!(
        dynamic_iq_final_seed(true, dynamic_iq_initial_candidate(2)),
        [7, -7, -5, 1]
    );
    assert_eq!(
        verify_dynamic_iq_candidate(
            [1, 2, 3, 4],
            [10, 20, 30, 40],
            DynamicIqVerificationMetrics {
                reference: 0,
                second: 100,
                third: 100,
            },
            DynamicIqVerificationMetrics {
                reference: 50,
                second: 60,
                third: 10,
            },
        ),
        DynamicIqVerification {
            retained_values: [1, 2, 30, 40],
            first_pair_rejected: false,
            second_pair_rejected: true,
            first_quality_failed: true,
            second_quality_failed: true,
            publication_allowed: false,
        }
    );
    let asymmetric_verification = verify_dynamic_iq_candidate(
        [1, 2, 3, 4],
        [10, 20, 30, 40],
        DynamicIqVerificationMetrics {
            reference: 0,
            second: 100,
            third: 100,
        },
        DynamicIqVerificationMetrics {
            reference: 100,
            second: 10,
            third: 120,
        },
    );
    assert_eq!(asymmetric_verification.retained_values, [10, 20, 3, 4]);
    assert!(asymmetric_verification.first_pair_rejected);
    assert!(!asymmetric_verification.second_pair_rejected);
    assert!(asymmetric_verification.publication_allowed);
    let finalized = finalize_dynamic_iq_search(
        DynamicIqAveragedSearch {
            baseline: DynamicIqVerificationMetrics {
                reference: 0,
                second: 100,
                third: 100,
            },
            final_candidate: [1, 2, 3, 4],
            completed_passes: 3,
            accepted_passes: 2,
        },
        [10, 20, 30, 40],
        DynamicIqVerificationMetrics {
            reference: 100,
            second: 10,
            third: 120,
        },
    );
    assert_eq!(finalized.verification, asymmetric_verification);
    assert_eq!(
        finalized.publication,
        Some(dynamic_iq_correction_plan([1, 2, 3, 4]))
    );

    assert_eq!(rescale_signed(100, 6, 8), 25);
    assert_eq!(rescale_signed(200, 6, 8), 31);
    assert_eq!(rescale_signed(-200, 6, 8), -32);
    assert_eq!(calibration_gain_value(-1), 0);
    assert_eq!(calibration_gain_value(0x20), 0x60);
    assert_eq!(calibration_gain_value(0x7f), 0x7f);
    assert_eq!(calibration_mode_timing(0), 0x0020_0190);
    assert_eq!(calibration_mode_timing(2), 0x0020_0078);
    assert_eq!(calibration_sample_settings(0x11, 0x11), 0x0111_0111);
    assert_eq!(calibration_sample_settings(1, 1), 0x0101_0101);
    assert_eq!(CALIBRATION_SAMPLE_COMMAND, 0x0800_000d);
    assert_eq!(decode_calibration_accumulator(0), 0);
    assert_eq!(decode_calibration_accumulator(0x1000), 2);
    assert_eq!(decode_calibration_accumulator(0x3f_ffff), 2047);
    assert_eq!(decode_calibration_accumulator(0x40_0000), -2048);
    assert_eq!(
        pack_iq_signed8_pair(IqCalibrationCoefficient { i: 132, q: 4 }),
        0x047f
    );
    assert_eq!(
        pack_iq_signed8_pair(IqCalibrationCoefficient { i: -1, q: -128 }),
        0x80ff
    );
    assert_eq!(
        publish_iq_coefficient(IqCalibrationCoefficient { i: 132, q: 4 }),
        Some(PublishedIqCoefficient {
            shift: 11,
            normalized_i: 270_336,
            normalized_q: 8_192,
            packed: 0x10,
        })
    );
    assert_eq!(update_iq_shift_state(0x0025_4310, 11), 0x0025_4365);
    assert_eq!(
        iq_calibration_publication(3, IqCalibrationCoefficient { i: 132, q: 4 }, 0x0025_4310,),
        Some(IqCalibrationPublication {
            primary_address: 0x0abb_8124,
            normalized_first_address: 0x0abb_860c,
            normalized_second_address: 0x0abb_868c,
            primary_value: 0x047f,
            normalized_value: 0x10,
            next_shift_state: 0x0025_4365,
        })
    );
    let repeated_sample = IqCalibrationSample {
        baseline_i: 10,
        baseline_q: -10,
        target_i: 20,
        target_q: 0,
    };
    let series = build_iq_calibration_series(&[repeated_sample; 12], 0x0025_4310);
    assert_eq!(series.iterations[0].gain_index, 0x1a);
    assert_eq!(series.iterations[11].gain_index, 0);
    assert_eq!(series.iterations[0].scale_i, -2560);
    assert_eq!(series.iterations[0].scale_q, -2560);
    assert_eq!(
        series.iterations[0].coefficient,
        IqCalibrationCoefficient { i: 132, q: 4 }
    );
    assert_eq!(
        series.iterations[0].publication.map(|plan| (
            plan.primary_address,
            plan.normalized_first_address,
            plan.normalized_second_address,
            plan.primary_value,
            plan.normalized_value,
        )),
        Some((0x0abb_8180, 0x0abb_8668, 0x0abb_86e8, 0x047f, 0x019a_019a))
    );
    assert_eq!(
        series.iterations[11].publication.map(|plan| (
            plan.primary_address,
            plan.normalized_first_address,
            plan.normalized_second_address,
        )),
        Some((0x0abb_8118, 0x0abb_8600, 0x0abb_8680))
    );
    assert_ne!(series.final_shift_state, 0x0025_4310);
    assert_eq!(
        series.iterations[0]
            .publication
            .map(|publication| publication.next_shift_state),
        series.iterations[11]
            .publication
            .map(|publication| publication.next_shift_state)
    );
    assert_eq!(
        iq_calibration_controls(0, 0, 0),
        IqCalibrationControls {
            during_settle: 0x0035_7df1,
            active: 0x0034_7df1,
            abc0034: 0x1fff,
        }
    );
    assert_eq!(iq_calibration_controls(0, 1, 0).active, 0x00b4_7ff1);
    assert_eq!(iq_calibration_controls(0, 1, 1).active, 0x0074_7ff1);

    let request = build_scan_channel_program_request(0, 4, 6);
    assert_eq!(request.as_bytes(), &[0, 1, 0x17, 0x01, 6, 0, 1, 2]);
}

#[test]
fn remap_timing_matches_vendor_window_two() {
    assert_eq!(derive_remap_timing(0x0411_8000), (1032, (-852_i16) as u16));
}

#[test]
fn mode0_rate_table_matches_vendor_generation() {
    let mut table = [0; 80];
    build_rate_table(&MODE0_CALIBRATION_ANCHORS, 0, &mut table);

    assert_eq!(table[0], 0x2b20_2b20);
    assert_eq!(table[54], 0x0a14_0a14);
    assert_eq!(table[62], 0x0219_0219);
    assert_eq!(table[79], 0x001a_001a);
    assert_eq!(rate_table_boundary(&table), 55);
}
