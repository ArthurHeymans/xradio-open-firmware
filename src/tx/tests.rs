use super::*;

#[test]
fn publication_registration_rejects_overlapping_pipe_owners() {
    assert!(publication_registration_allowed(
        [false; 4],
        BatchPosition::Only,
        2,
    ));
    assert!(publication_registration_allowed(
        [false; 4],
        BatchPosition::First,
        3,
    ));
    assert!(!publication_registration_allowed(
        [false; 4],
        BatchPosition::Last,
        1,
    ));
    assert!(publication_registration_allowed(
        [true, false, false, false],
        BatchPosition::Last,
        1,
    ));
    assert!(!publication_registration_allowed(
        [true, false, false, false],
        BatchPosition::Only,
        1,
    ));
    assert!(!publication_registration_allowed(
        [true, false, false, false],
        BatchPosition::Last,
        0,
    ));
    {
        assert!(publication_registration_allowed(
            [true, false, false, false],
            BatchPosition::Middle,
            1,
        ));
        assert!(publication_registration_allowed(
            [true, true, false, false],
            BatchPosition::Middle,
            2,
        ));
        assert!(publication_registration_allowed(
            [true, true, true, false],
            BatchPosition::Last,
            3,
        ));
        assert!(publication_registration_allowed(
            [true, false, false, true],
            BatchPosition::Middle,
            1,
        ));
    }
    assert!(!publication_registration_allowed(
        [true, false, true, false],
        BatchPosition::Last,
        3,
    ));
    assert!(!publication_registration_allowed(
        [false; 4],
        BatchPosition::Only,
        4,
    ));
}

#[test]
fn class_zero_completion_queue_holds_the_configured_pipe_depth() {
    let mut completed = BoundedCompletionQueue::<u8, HOST_CLASS0_COMPLETION_CAPACITY>::new();
    for value in 0..HOST_CLASS0_COMPLETION_CAPACITY as u8 {
        assert_eq!(completed.push(value), Ok(()));
    }
    assert_eq!(completed.push(0xff), Err(0xff));
    for value in 0..HOST_CLASS0_COMPLETION_CAPACITY as u8 {
        assert_eq!(completed.take(), Some(value));
    }
    assert_eq!(completed.take(), None);
    assert_eq!(completed.push(0xfe), Ok(()));
    assert_eq!(completed.take(), Some(0xfe));
}

struct MockHw {
    values: [(u32, u32); 64],
    value_count: usize,
    writes: [(u32, u32); 64],
    write_count: usize,
}

impl MockHw {
    fn new() -> Self {
        Self {
            values: [(0, 0); 64],
            value_count: 0,
            writes: [(0, 0); 64],
            write_count: 0,
        }
    }

    fn set(&mut self, address: u32, value: u32) {
        self.values[self.value_count] = (address, value);
        self.value_count += 1;
    }

    fn get(&self, address: u32) -> u32 {
        self.values[..self.value_count]
            .iter()
            .rev()
            .find_map(|(stored, value)| (*stored == address).then_some(*value))
            .unwrap_or(0)
    }
}

impl Hw for MockHw {
    fn read(&mut self, address: u32) -> u32 {
        self.get(address)
    }

    fn write(&mut self, address: u32, value: u32) {
        self.writes[self.write_count] = (address, value);
        self.write_count += 1;
        self.set(address, value);
    }
}

/// Pipes with every ring programmed, as after startup.
fn programmed_pipes() -> Pipes {
    let pipes = Pipes::new();
    for pipe in 0..4_u8 {
        pipes.reset(
            pipe,
            0,
            crate::platform::tx_ring_register(usize::from(pipe), 0) as u32,
        );
    }
    pipes
}

#[test]
fn mac_pipe_service_executor_preserves_pointer_and_low_nibble_order() {
    let mut mmio = MockHw::new();
    let pipes = programmed_pipes();
    pipes.latched.set(2);
    pipes.pipe(2).current.set(1);
    let context = ContextId::host(7);
    pipes.pipe(2).slot(1).frame.set(context);
    let mut calls = std::vec::Vec::new();

    execute_mac_pipe_service(
        &mut mmio,
        &pipes,
        SchedulerWord::new(0x0f),
        |descriptor, frame| calls.push((descriptor, frame)),
    );

    assert_eq!((pipes.current_pipe.get(), pipes.current_slot.get()), (2, 1));
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2], (packet_ram::tx_command(2, 1) as u32, context));
    assert_eq!(mmio.writes[0], (PIPE_IRQ_TRIGGER, 0x1e00_0000));
    assert_eq!(mmio.writes[1], (PIPE_IRQ_PENDING, !0x0f_u32));
}

#[test]
fn mac_pipe_service_executor_middle_publishes_quantum_before_ack() {
    let mut mmio = MockHw::new();
    let pipes = programmed_pipes();
    pipes.pipe(3).current.set(2);
    let mut calls = 0;

    execute_mac_pipe_service(&mut mmio, &pipes, SchedulerWord::new(0x0080), |_, _| {
        calls += 1
    });

    assert_eq!(mmio.writes[0], (quantum_register(3), PIPE_QUANTUM));
    assert_eq!(mmio.writes[1], (PIPE_IRQ_TRIGGER, 1 << 28));
    assert_eq!((pipes.current_pipe.get(), pipes.current_slot.get()), (3, 2));
    assert_eq!(mmio.writes[2], (PIPE_IRQ_PENDING, !0x80_u32));
    assert_eq!(calls, 0);
}

fn unmatched_input(current: u8, last: u8) -> OrdinaryTxPipeStatusInput {
    OrdinaryTxPipeStatusInput {
        pipe_active: true,
        expected_status: 0x11,
        slot_state: 3,
        global_busy: false,
        pipe_current: current,
        pipe_last: last,
        pipe_status: 5,
    }
}

#[test]
fn matching_status_still_uses_the_vendor_plan() {
    // The recovery path must never shadow a real completion.
    assert!(matches!(
        plan_ordinary_tx_pipe_status(unmatched_input(2, 2), 0x11),
        OrdinaryTxPipeStatusPlan::Complete { .. }
    ));
}

#[test]
fn watchdog_timer_divider_samples_once_per_sixteen_passes() {
    let mut countdown = 0;
    let mut samples = 0;
    for _ in 0..64 {
        let (next, due) = advance_watchdog_timer_divider(countdown);
        countdown = next;
        samples += usize::from(due);
    }
    assert_eq!(samples, 4);
    assert_eq!(countdown, 0);
}

#[test]
fn pipe_watchdog_ignores_unprogrammed_or_idle_pipes() {
    assert_eq!(plan_pipe_watchdog(false, true, 5), PipeWatchdogAction::Idle);
    assert_eq!(plan_pipe_watchdog(true, false, 5), PipeWatchdogAction::Idle);
}

#[test]
fn stopping_a_ring_moves_both_cursors_past_the_armed_batch() {
    let pipes = programmed_pipes();
    let record = pipes.pipe(2);
    record.state.set(1);
    record.last.set(3);
    let ring = record.hardware_ring(2).unwrap();
    let mut mmio = MockHw::new();
    mmio.set(
        ring.cursor_and_pending_mask(),
        0xc000_0abc | 3 << 24 | 2 << 27,
    );
    assert_eq!(stop_pipe_ring(&mut mmio, &pipes, 2), Some(0));
    assert_eq!(
        mmio.writes[0],
        (ring.inactive_sentinel(), PIPE_RETRY_INACTIVE_SENTINEL)
    );
    assert_eq!(
        mmio.writes[1],
        (PIPE_IRQ_PENDING, 0_u32.wrapping_sub(0x4450))
    );
    assert_eq!(
        mmio.writes[2],
        (ring.cursor_and_pending_mask(), 0xc000_0abc)
    );
}

#[test]
fn a_stuck_batch_retires_current_through_last_across_the_wrap() {
    let slots = |current, last| unfinished_slots(current, last).collect::<std::vec::Vec<_>>();
    assert_eq!(slots(2, 2), [2]);
    assert_eq!(slots(1, 3), [1, 2, 3]);
    assert_eq!(slots(3, 1), [3, 0, 1]);
}

#[test]
fn pipe_watchdog_counts_down_then_nudges_then_expires() {
    // Arming reloads the counter to 5, so an armed pipe that never
    // completes walks 5 -> 3 healthy, 2 -> 1 nudging, then expires.
    assert_eq!(plan_pipe_watchdog(true, true, 5), PipeWatchdogAction::Tick);
    assert_eq!(plan_pipe_watchdog(true, true, 3), PipeWatchdogAction::Tick);
    assert_eq!(plan_pipe_watchdog(true, true, 2), PipeWatchdogAction::Nudge);
    assert_eq!(plan_pipe_watchdog(true, true, 1), PipeWatchdogAction::Nudge);
    assert_eq!(
        plan_pipe_watchdog(true, true, 0),
        PipeWatchdogAction::Expired
    );
    // The vendor read is signed, so an overshoot stays expired rather than
    // wrapping back into the healthy range.
    assert_eq!(
        plan_pipe_watchdog(true, true, -1),
        PipeWatchdogAction::Expired
    );
}

#[test]
fn hardware_ring_identity_is_exact_for_each_pipe() {
    for pipe in 0..4 {
        let expected = crate::platform::tx_ring_register(usize::from(pipe), 0) as u32;
        assert_eq!(
            TxHardwareRingAddress::for_pipe(pipe, expected),
            Some(TxHardwareRingAddress(expected)),
        );
        assert_eq!(TxHardwareRingAddress::for_pipe(pipe, expected + 0x80), None);
    }
    assert_eq!(
        TxHardwareRingAddress::for_pipe(4, crate::platform::tx_ring_register(0, 0) as u32,),
        None,
    );
}

#[test]
fn success_marker_gate_ignores_slot_kind_but_not_its_own_conditions() {
    assert!(success_marker_resets_backoff(1, 0xff, 0));
    // Bit 4 (lifetime) is a different gate and must not block the reset.
    assert!(success_marker_resets_backoff(1, 0xff, 1 << 4));
    assert!(!success_marker_resets_backoff(0, 0xff, 0));
    assert!(!success_marker_resets_backoff(1, 0x0c, 0));
    assert!(!success_marker_resets_backoff(1, 0xff, 1 << 15));
}

#[test]
fn status_hook_reset_follows_the_completion_gates_only() {
    let input = OrdinaryTxPipeStatusInput {
        pipe_active: true,
        expected_status: 4,
        slot_state: 3,
        global_busy: false,
        pipe_current: 0,
        pipe_last: 0,
        pipe_status: 1,
    };
    assert!(matches!(
        plan_ordinary_tx_pipe_status(input, 4),
        OrdinaryTxPipeStatusPlan::Complete { .. }
    ));
    for ineligible in [
        OrdinaryTxPipeStatusInput {
            pipe_active: false,
            ..input
        },
        OrdinaryTxPipeStatusInput {
            slot_state: 5,
            ..input
        },
        OrdinaryTxPipeStatusInput {
            expected_status: 6,
            ..input
        },
    ] {
        assert_eq!(
            plan_ordinary_tx_pipe_status(ineligible, 4),
            OrdinaryTxPipeStatusPlan::Ineligible
        );
    }
}

#[test]
fn bounded_single_retry_counts_only_hardware_rearms() {
    let mut retry = BoundedSingleTxRetry::new();
    assert_eq!(retry.attempts(), 0);
    assert_eq!(retry.decide(), SingleTxRetryDecision::Rearm);
    assert_eq!(retry.attempts(), 1);
    assert_eq!(retry.decide(), SingleTxRetryDecision::Rearm);
    assert_eq!(retry.attempts(), 2);
    assert_eq!(retry.decide(), SingleTxRetryDecision::GiveUp);
    assert_eq!(retry.attempts(), 2);
    retry.reset();
    assert_eq!(retry.attempts(), 0);
}

#[test]
fn single_probe_publication_preserves_vendor_trigger_and_go_order() {
    let mut mmio = MockHw::new();
    let pipes = programmed_pipes();
    let hardware_ring = crate::platform::tx_ring_register(0, 0) as u32;
    let command = packet_ram::tx_command(0, 0) as u32;
    // A host context no other test uses.
    let id = ContextId::host(29).unwrap();
    let context = id.get();
    context.payload_base.set(0x20);
    context.frame_kind.set(0xff);
    context.interface.set(0);
    context.access_category.set(0);
    RETRY_RANDOM_STATE.set(1);
    let low_mac = crate::mac::low_mac();
    let pas = low_mac.pas(0_usize);
    pas.contention_window[0].set(0);
    pas.packed_aifs.set(0x55);
    pas.txop_limit[1].set(64);
    low_mac.edca_slot_timing.set(0x44);
    pipes.pipe(0).control.set(8);
    pipes.active_tx.set(2);

    let result = execute_single_probe_publication(
        &mut mmio,
        &pipes,
        SingleProbePublicationInput {
            pipe: 0,
            slot: 0,
            context: id,
            batch: BatchPosition::Only,
        },
    );

    assert_eq!(result, 0);
    assert_eq!(mmio.get(command + 8), 0xdc00_0000);
    assert_eq!(mmio.get(crate::platform::mac_register(0x0e64) as u32), 0x55);
    assert_eq!(mmio.get(quantum_register(0)), 2);
    assert_eq!(mmio.get(PIPE_IRQ_TRIGGER), 1 << 25);
    assert_eq!(pipes.active_tx.get(), 3);
    let pipe = pipes.pipe(0);
    assert_eq!(pipe.slot(0).state.get(), 1);
    assert_eq!(pipe.slot(0).duration.get(), 0x0010_0000);
    assert_eq!(mmio.get(hardware_ring), 0x0010_0000);
    assert_eq!((pipe.last.get(), pipe.current.get()), (0, 0));
    assert_eq!(pipe.state.get(), 1);
    assert_eq!(pipe.control.get(), 9);
    assert_eq!(pipe.watchdog.get(), PIPE_WATCHDOG_RELOAD);
    assert_eq!(mmio.get(hardware_ring + 0x14), 1);

    let trigger_index = mmio.writes[..mmio.write_count]
        .iter()
        .position(|&(address, _)| address == PIPE_IRQ_TRIGGER)
        .unwrap_or_else(|| panic!("missing trigger write"));
    let go_index = mmio.writes[..mmio.write_count]
        .iter()
        .rposition(|&(address, value)| address == hardware_ring + 0x14 && value == 1)
        .unwrap_or_else(|| panic!("missing GO write"));
    assert!(trigger_index < go_index);
}

#[test]
fn staged_two_slot_batch_triggers_once_and_publishes_both_durations() {
    let mut mmio = MockHw::new();
    let pipes = programmed_pipes();
    let pipe = 1_u8;
    let hardware_ring = crate::platform::tx_ring_register(usize::from(pipe), 0) as u32;
    let record = pipes.pipe(pipe);
    record.slot(3).duration.set(0x1111);
    record.slot(0).duration.set(0x2222);
    record.control.set(8);
    pipes.active_tx.set(4);

    assert!(finalize_staged_pipe(&mut mmio, &pipes, pipe, 3, 0));

    assert_eq!(mmio.get(PIPE_IRQ_TRIGGER), (1_u32 << pipe) << 25);
    assert_eq!(record.slot(3).state.get(), 1);
    assert_eq!(record.slot(0).state.get(), 1);
    assert_eq!(pipes.active_tx.get(), 6);
    assert_eq!((record.state.get(), record.control.get()), (1, 9));
    assert_eq!(record.watchdog.get(), PIPE_WATCHDOG_RELOAD);
    assert_eq!(mmio.get(hardware_ring + 0x14), 1);
    let duration_writes = mmio.writes[..mmio.write_count]
        .iter()
        .filter(|&&(address, _)| address == hardware_ring)
        .map(|&(_, value)| value)
        .collect::<std::vec::Vec<_>>();
    assert_eq!(duration_writes, [0x1111, 0x2222]);
}

#[test]
fn protected_pair_wraps_arms_once_and_retires_only_one_context() {
    let id = ContextId::host(26).unwrap();
    let context = id.get();
    context.interface.set(0);
    context.access_category.set(0);
    context.payload_base.set(100);
    context.payload_extended.set(110);
    context.ack_duration.set(44);
    context.total_airtime.set(144);
    context.terminal_status.set(STATUS_PENDING);
    for (kind, response) in [
        (Protection::Rts, 0x11),
        (Protection::CtsToSelf, 0x11),
        (Protection::CtsToSelf, 0xff),
    ] {
        let mut mmio = MockHw::new();
        let pipes = programmed_pipes();
        let pipe = pipes.pipe(0);
        context.frame_kind.set(response);
        let data = SingleFramePasTiming {
            payload_base: 100,
            payload_extended: 110,
            ack: 44,
            total_airtime: 144,
            frame_kind: response,
        };
        let prefix = ProtectionTiming::compute(kind, 0, 0x17, data);
        context.protection.set(Some(prefix));
        pipe.producer.set(3);
        pipe.slot(3).reserve(id, kind.expected_status());
        pipe.slot(3).prefix.set(Some(kind));
        pipe.slot(0).reserve(id, response);
        execute_protected_publication(&mut mmio, &pipes, 0, 0, id);
        assert_eq!((pipe.current.get(), pipe.last.get()), (3, 0));
        assert_eq!(pipes.active_tx.get(), 2);
        assert_eq!(pipe.slot(3).duration.get(), prefix.slot_duration());
        let ring = pipe.hardware_ring(0).unwrap();
        assert_eq!(
            mmio.writes[..mmio.write_count]
                .iter()
                .filter(|&&(address, _)| address == PIPE_IRQ_TRIGGER)
                .count(),
            1
        );
        assert_eq!(
            mmio.writes[..mmio.write_count]
                .iter()
                .filter(|&&(address, value)| address == ring.go() && value == 1)
                .count(),
            1
        );
        assert_eq!(
            mmio.get(packet_ram::tx_command(0, 3) as u32 + 4) & 0xff,
            if kind == Protection::Rts { 0x86 } else { 0 }
        );
        assert_eq!(retire_command_slot(&pipes, 0, 3), None);
        assert_eq!(pipes.active_tx.get(), 1);
        assert_eq!(context.terminal_status.get(), STATUS_PENDING);
        // A data retry replays the borrowed prefix using the old FIFO entries.
        pipe.current.set(0);
        let mut retry_mmio = MockHw::new();
        let cursor = 0x4000_ff0f;
        retry_mmio.set(ring.cursor_and_pending_mask(), cursor);
        execute_protected_retry(&mut retry_mmio, &pipes, 0, id, 0x100, true);
        assert_eq!(retry_mmio.get(ring.cursor_and_pending_mask()), 0x5800_0006);
        assert_eq!(
            retry_mmio.get(PIPE_IRQ_PENDING),
            0_u32.wrapping_sub(0x100 + 0xf010)
        );
        assert_eq!(pipe.current.get(), 3);
        assert_eq!(pipes.active_tx.get(), 2);
        assert!(
            !retry_mmio.writes[..retry_mmio.write_count]
                .iter()
                .any(
                    |&(address, _)| [ring.duration_fifo(), ring.go(), ring.inactive_sentinel()]
                        .contains(&address)
                )
        );
        if kind == Protection::Rts {
            // A failed RTS retries just that slot and leaves data accounting alone.
            let mut rts_mmio = MockHw::new();
            rts_mmio.set(ring.cursor_and_pending_mask(), 0x5800_000f);
            execute_protected_retry(&mut rts_mmio, &pipes, 0, id, 0x100, true);
            assert_eq!(rts_mmio.get(ring.cursor_and_pending_mask()), 0x5800_0007);
            assert_eq!(rts_mmio.get(PIPE_IRQ_PENDING), 0_u32.wrapping_sub(0x101));
            assert_eq!(pipes.active_tx.get(), 2);
        }
        assert_eq!(retire_command_slot(&pipes, 0, 3), None);
        pipe.current.set(0);
        if kind == Protection::CtsToSelf {
            let mut cck_mmio = MockHw::new();
            cck_mmio.set(ring.cursor_and_pending_mask(), cursor);
            execute_protected_retry(&mut cck_mmio, &pipes, 0, id, 0x100, false);
            assert_eq!(cck_mmio.get(ring.cursor_and_pending_mask()), cursor & !1);
            assert_eq!(
                cck_mmio.get(packet_ram::tx_command(0, 3) as u32 + 8),
                0xc400_0000
            );
            assert_eq!(pipe.current.get(), 0);
            assert_eq!(pipes.active_tx.get(), 1);
        }
        assert_eq!(retire_command_slot(&pipes, 0, 0), Some(id));
        assert_eq!(pipes.active_tx.get(), 0);
        assert_eq!(retire_command_slot(&pipes, 0, 3), None);
        assert_eq!(retire_command_slot(&pipes, 0, 0), None);
        assert_eq!(pipes.active_tx.get(), 0);
    }
    context.protection.set(None);
}

#[test]
fn aggregates_omit_cts_but_never_discard_requested_rts() {
    let context = ContextId::host(25).unwrap().get();
    let data = SingleFramePasTiming {
        payload_base: 100,
        payload_extended: 110,
        ack: 44,
        total_airtime: 144,
        frame_kind: 0x11,
    };
    context.payload_base.set(data.payload_base);
    context.ack_duration.set(data.ack);
    context.flags.set(0x8808);
    context.protection.set(Some(ProtectionTiming::compute(
        Protection::CtsToSelf,
        0,
        0x17,
        data,
    )));
    assert!(omit_aggregate_cts(context));
    assert_eq!(context.flags.get(), 8);
    assert_eq!(context.protection.get(), None);
    assert_eq!(context.total_airtime.get(), data.total_airtime);
    context.flags.set(0xc08);
    let rts = ProtectionTiming::compute(Protection::Rts, 6, 0x17, data);
    context.protection.set(Some(rts));
    assert!(!omit_aggregate_cts(context));
    assert_eq!(context.flags.get(), 0xc08);
    assert_eq!(context.protection.get(), Some(rts));
    context.protection.set(None);
}

#[test]
fn staged_four_slot_batch_wraps_once_and_triggers_once() {
    let mut mmio = MockHw::new();
    let pipes = programmed_pipes();
    let pipe = 2_u8;
    let hardware_ring = crate::platform::tx_ring_register(usize::from(pipe), 0) as u32;
    let record = pipes.pipe(pipe);
    for (slot, duration) in [(2, 0x1111), (3, 0x2222), (0, 0x3333), (1, 0x4444)] {
        record.slot(slot).duration.set(duration);
    }
    pipes.active_tx.set(7);

    assert!(finalize_staged_pipe(&mut mmio, &pipes, pipe, 2, 1));

    assert_eq!(mmio.get(PIPE_IRQ_TRIGGER), (1_u32 << pipe) << 25);
    assert!(record.slots.iter().all(|slot| slot.state.get() == 1));
    assert_eq!(pipes.active_tx.get(), 11);
    assert_eq!(mmio.get(hardware_ring + 0x14), 1);
    let duration_writes = mmio.writes[..mmio.write_count]
        .iter()
        .filter(|&&(address, _)| address == hardware_ring)
        .map(|&(_, value)| value)
        .collect::<std::vec::Vec<_>>();
    assert_eq!(duration_writes, [0x1111, 0x2222, 0x3333, 0x4444]);
}

#[test]
fn ordinary_status_plan_marks_before_suppression_and_reuses_completion_helper_boundary() {
    let input = OrdinaryTxPipeStatusInput {
        pipe_active: true,
        expected_status: 0x0b,
        slot_state: 3,
        global_busy: true,
        pipe_current: 1,
        pipe_last: 2,
        pipe_status: 0,
    };
    assert_eq!(
        plan_ordinary_tx_pipe_status(input, 0x0b),
        OrdinaryTxPipeStatusPlan::SuppressedAfterSlotMark
    );

    let input = OrdinaryTxPipeStatusInput {
        global_busy: false,
        pipe_current: 2,
        pipe_last: 2,
        ..input
    };
    assert_eq!(
        plan_ordinary_tx_pipe_status(input, 0x0b),
        OrdinaryTxPipeStatusPlan::Complete {
            initialize_pipe_status: true,
            cursor: OrdinaryTxPipeCursorPlan::Recycle { next_head: 3 },
        }
    );
}

#[test]
fn mac_event_decoder_matches_vendor_bitfields() {
    let raw = (1 << 25) | (1 << 24) | (2 << 18) | (3 << 16) | (0x37 << 8) | 0x19;
    assert_eq!(
        MacEvent::decode(raw),
        Some(MacEvent {
            raw,
            event_type: 0x37,
            pipe: 2,
            phase: 3,
            status: 0x19,
            pipe_marker: true,
            completion_marker: true,
            fatal_marker: false,
            pipe_service_marker: false,
            beacon_marker: false,
            sideband_marker: false,
        })
    );
    assert_eq!(MacEvent::decode(u32::MAX), None);
}

#[test]
fn fatal_postmortem_captures_pre_acknowledgement_ownership_state() {
    let mut mmio = MockHw::new();
    let pipes = Pipes::new();
    pipes.latched.set(2);
    pipes.current_pipe.set(1);
    pipes.current_slot.set(3);
    pipes.busy.set(true);
    mmio.set(MAC_EVENT_READINESS, 0xffff_ffff);
    mmio.set(PIPE_IRQ_PENDING, 0x400);
    mmio.set(PIPE_IRQ_TRIGGER, 1 << 27);
    let event = MacEvent::decode((1 << 30) | (1 << 24) | 4)
        .unwrap_or_else(|| panic!("fatal event decoded empty"));
    let mut record = MacFatalPostmortem::empty();

    capture_mac_fatal_postmortem(
        &mut mmio,
        &pipes,
        event,
        SchedulerWord::new(0x0400),
        0x6000_00d3,
        &mut record,
    );

    assert_eq!(record.valid, 0);
    assert_eq!(record.version, 1);
    assert_eq!(record.line, 222);
    assert_eq!(record.code, 0x29);
    assert_eq!(record.event, event.raw);
    assert_eq!(record.saved_scheduler_word, 0x400);
    assert_eq!(record.cpsr, 0x6000_00d3);
    assert_eq!(record.current_pipe, 2);
    assert_eq!(record.current_pipe_record, 1);
    assert_eq!(record.current_slot, 3);
    assert_eq!(record.pipe_busy, 1);
    assert_eq!(record.event_readiness, u32::MAX);
    assert_eq!(record.pipe_irq_pending, 0x400);
    assert_eq!(record.pipe_irq_trigger, 1 << 27);
}

#[test]
fn pipe_service_plan_preserves_vendor_priority_and_acknowledgements() {
    assert_eq!(
        plan_mac_pipe_service(SchedulerWord::new(0x1095), 3),
        MacPipeServicePlan::LowNibble {
            asserted: 5,
            backoff_pipe_mask: 5,
            trigger: 5 << 25,
            acknowledgement: !5_u32,
        }
    );
    assert_eq!(
        plan_mac_pipe_service(SchedulerWord::new(0x1080), 3),
        MacPipeServicePlan::MiddleNibble {
            selected_pipe: 3,
            trigger: 1 << 28,
            acknowledgement: !0x80_u32,
        }
    );
    assert_eq!(
        plan_mac_pipe_service(SchedulerWord::new(0x2000), 1),
        MacPipeServicePlan::HighNibble {
            selected_pipe: 1,
            acknowledgement: !0x2000_u32,
        }
    );
}

#[test]
fn context_ids_split_host_and_internal_contexts() {
    let host = ContextId::host(HOST_CONTEXT_COUNT - 1).unwrap();
    assert!(host.is_host());
    assert_eq!(host.host_index(), Some(HOST_CONTEXT_COUNT - 1));
    assert_eq!(host.internal_index(), None);
    assert_eq!(ContextId::host(HOST_CONTEXT_COUNT), None);

    let internal = ContextId::internal(0).unwrap();
    assert!(!internal.is_host());
    assert_eq!(internal.internal_index(), Some(0));
    assert_eq!(host_descriptor_address(internal), None);
    assert_eq!(ContextId::internal(INTERNAL_CONTEXT_COUNT), None);
}

#[test]
fn single_frame_secondary_command_matches_vendor_qos_branch() {
    assert_eq!(
        single_frame_secondary_command(0, 0x50, 0),
        0x2100_0000 | (packet_ram::duration_word(0) as u32 & 0x007f_ffff)
    );
    assert_eq!(
        single_frame_secondary_command(0, 0x50, 1),
        0x2100_0000 | (packet_ram::duration_word(1) as u32 & 0x007f_ffff)
    );
    assert_eq!(single_frame_secondary_command(1, 0x50, 0), 0x3200_0050);
}

#[test]
fn single_frame_pas_timing_distinguishes_probe_and_ack_classes() {
    let probe = compute_single_frame_pas_timing(0x117, 0, 50, 0x1300, 0x2c, false)
        .unwrap_or_else(|| panic!("missing probe timing"));
    assert_eq!(probe.frame_kind, 0xff);
    assert_eq!(probe.ack, 0);
    assert_ne!(probe.payload_base, 0);
    assert_eq!(probe.total_airtime, u32::from(probe.payload_base));
    assert_eq!(
        single_frame_slot_duration(probe),
        u32::from(probe.payload_base) * 0x8000
    );

    let authentication = compute_single_frame_pas_timing(0x117, 0, 30, 0x0080_1000, 0x2c, false)
        .unwrap_or_else(|| panic!("missing authentication timing"));
    assert_eq!(authentication.frame_kind, 0x11);
    assert_eq!(authentication.ack, 0x2c);
    assert_eq!(
        authentication.total_airtime,
        u32::from(authentication.payload_base) + 0x2c
    );
    assert_eq!(
        single_frame_slot_duration(authentication),
        u32::from(authentication.payload_base) * 0x8000 + 0x202c
    );
}

#[test]
fn single_frame_slot_accepts_both_ack_and_no_ack_frame_kinds() {
    let slot = Slot::new();
    let id = ContextId::host(28).unwrap();
    let other = ContextId::host(27).unwrap();
    id.get().frame_kind.set(0xff);
    slot.reserve(id, 0xff);
    assert!(single_frame_slot_matches(&slot, id));
    assert!(!single_frame_slot_matches(&slot, other));
    id.get().frame_kind.set(0x11);
    assert!(!single_frame_slot_matches(&slot, id));
    slot.reserve(id, 0x11);
    assert!(single_frame_slot_matches(&slot, id));
}

#[test]
fn dot11_header_view_and_classification_make_split_boundaries_explicit() {
    assert_eq!(core::mem::size_of::<Dot11FixedHeaderLayout>(), 24);
    assert_eq!(
        core::mem::offset_of!(Dot11FixedHeaderLayout, frame_control),
        0
    );
    assert_eq!(core::mem::offset_of!(Dot11FixedHeaderLayout, duration), 2);
    assert_eq!(core::mem::offset_of!(Dot11FixedHeaderLayout, address_1), 4);
    assert_eq!(core::mem::offset_of!(Dot11FixedHeaderLayout, address_2), 10);
    assert_eq!(core::mem::offset_of!(Dot11FixedHeaderLayout, address_3), 16);
    assert_eq!(
        core::mem::offset_of!(Dot11FixedHeaderLayout, sequence_control),
        22
    );

    let frame = TxFrameAddress::new(0x0901_4fe8);
    assert_eq!(frame.frame_control(), 0x0901_4fe8);
    assert_eq!(frame.address_1(), 0x0901_4fec);
    assert_eq!(frame.address_2_byte_unchecked(5), 0x0901_4ff7);
    assert_eq!(frame.address_2_halfword_unchecked(2), 0x0901_4ff6);
    assert_eq!(frame.sequence_control(), 0x0901_4ffe);
    assert_eq!(frame.field(24), 0x0901_5000);

    assert_eq!(
        classify_dot11_header(0x0008, false),
        Dot11HeaderShape {
            length: 24,
            qos_data: false
        }
    );
    assert_eq!(
        classify_dot11_header(0x0088, false),
        Dot11HeaderShape {
            length: 26,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x8088, false),
        Dot11HeaderShape {
            length: 30,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x0308, false),
        Dot11HeaderShape {
            length: 30,
            qos_data: false
        }
    );
    assert_eq!(
        classify_dot11_header(0x0388, false),
        Dot11HeaderShape {
            length: 32,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x8388, false),
        Dot11HeaderShape {
            length: 36,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x0088, true),
        Dot11HeaderShape {
            length: 24,
            qos_data: false
        }
    );
}

#[test]
fn tx_frame_validation_rejects_short_frames_aliases_and_range_ends() {
    assert_eq!(
        validated_tx_frame(0x0901_4fe8, 42).map(TxFrameAddress::raw),
        Some(0x0901_4fe8),
    );
    assert_eq!(
        validated_tx_frame(0x0901_4fe8, CONTROL_FRAME_MIN_LENGTH).map(TxFrameAddress::raw),
        Some(0x0901_4fe8),
    );
    assert_eq!(
        validated_tx_frame(0x0901_4fe8, CONTROL_FRAME_MIN_LENGTH - 1),
        None
    );
    assert_eq!(validated_tx_frame(0x0001_4fe8, 42), None);
    assert_eq!(
        validated_tx_frame(packet_ram::RUNTIME_CPU_END - 24, 24).map(TxFrameAddress::raw),
        Some(packet_ram::RUNTIME_CPU_END - 24),
    );
    assert_eq!(
        validated_tx_frame(packet_ram::RUNTIME_CPU_END - 23, 24),
        None
    );
}

#[test]
fn dot11_header_shapes_keep_software_parsing_separate_from_descriptor_split() {
    assert_eq!(core::mem::size_of::<Dot11FixedHeaderLayout>(), 24);
    let frame = TxFrameAddress::new(0x0901_4fe8);
    assert_eq!(frame.frame_control(), 0x0901_4fe8);
    assert_eq!(frame.address_1(), 0x0901_4fec);
    assert_eq!(frame.address_2_byte_unchecked(0), 0x0901_4ff2);
    assert_eq!(frame.sequence_control(), 0x0901_4ffe);
    assert_eq!(frame.field(24), 0x0901_5000);

    assert_eq!(
        classify_dot11_header(0x0008, false),
        Dot11HeaderShape {
            length: 24,
            qos_data: false
        }
    );
    assert_eq!(
        classify_dot11_header(0x0308, false),
        Dot11HeaderShape {
            length: 30,
            qos_data: false
        }
    );
    assert_eq!(
        classify_dot11_header(0x0088, false),
        Dot11HeaderShape {
            length: 26,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x8088, false),
        Dot11HeaderShape {
            length: 30,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x0388, false),
        Dot11HeaderShape {
            length: 32,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x8388, false),
        Dot11HeaderShape {
            length: 36,
            qos_data: true
        }
    );
    assert_eq!(
        classify_dot11_header(0x0088, true),
        Dot11HeaderShape {
            length: 24,
            qos_data: false
        }
    );
}

#[test]
fn phy_rate_words_match_legacy_and_ht_classes() {
    let legacy = build_phy_rate_words(0, 2, 0, 3, 5);
    assert_eq!(
        legacy,
        PhyRateWords {
            control: 2,
            rate: 0x0003_0405,
        }
    );
    assert_eq!(finalize_phy_control(legacy, 0, 100), 2);

    let ht_mixed = build_phy_rate_words(14, 0, 0x28, 3, 7);
    assert_eq!(
        ht_mixed,
        PhyRateWords {
            control: 6,
            rate: 0x0003_1407,
        }
    );
    assert_eq!(finalize_phy_control(ht_mixed, 14, 100), 0x0006_c006);

    let ht_greenfield = build_phy_rate_words(14, 0, 0x20, 3, 7);
    assert_eq!(finalize_phy_control(ht_greenfield, 14, 100), 6);
}

#[test]
fn internal_completion_status_matches_vendor_wsm_switch_table() {
    assert_eq!(wsm_status_from_internal(0), 0);
    assert_eq!(wsm_status_from_internal(11), 6);
    assert_eq!(wsm_status_from_internal(10), 7);
    assert_eq!(wsm_status_from_internal(21), 4);
    assert_eq!(wsm_status_from_internal(24), 15);
    assert_eq!(wsm_status_from_internal(25), 1);
}

#[test]
fn sequence_counters_advance_per_link_and_tid_and_wrap() {
    let counters = SequenceCounters::new();
    assert_eq!(
        [
            counters.next(9, 0),
            counters.next(9, 0),
            counters.next(9, 5)
        ],
        [0, 0x10, 0]
    );
    for _ in 2..4096 {
        counters.next(0, 1);
    }
    assert_eq!([counters.next(0, 1), counters.next(0, 1)], [0xffe0, 0xfff0]);
    assert_eq!(counters.next(0, 1), 0);
    assert_eq!(counters.next(10, 0), 0);
}

#[test]
fn slot_owners_are_served_round_robin_from_the_cursor() {
    let mut owners = Class0RuntimeOwners::new();
    assert!(owners.observe(3, 0, 1));
    assert!(owners.observe(7, 2, 2));
    let eligible = owners.slot_mask();
    assert_eq!(owners.next_slot_owner(eligible, 0), Some((1, 3)));
    assert_eq!(owners.next_slot_owner(eligible, 2), Some((10, 7)));
    assert_eq!(owners.next_slot_owner(eligible, 11), Some((1, 3)));
    assert_eq!(
        owners.next_slot_owner(eligible & !(1 << 1), 11),
        Some((10, 7))
    );
    assert_eq!(owners.next_slot_owner(0, 0), None);
}
