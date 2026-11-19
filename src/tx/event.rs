use crate::global::Global;
use core::cell::Cell;

use super::*;

#[cfg(target_arch = "arm")]
pub(crate) const MAC_FATAL_MAGIC: u32 = 0x5852_4651;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacEvent {
    pub raw: u32,
    pub event_type: u8,
    pub pipe: u8,
    pub phase: u8,
    pub status: u8,
    pub pipe_marker: bool,
    pub completion_marker: bool,
    pub fatal_marker: bool,
    pub pipe_service_marker: bool,
    pub beacon_marker: bool,
    pub sideband_marker: bool,
}

/// Deterministic Rust postmortem record for a terminal MAC event. Vendor
/// assertion arguments for this path are `line = 222`, `code = 0x29`; the
/// additional words retain ownership-relevant state for reset-time recovery.
#[repr(C)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MacFatalPostmortem {
    pub valid: u32,
    pub version: u32,
    pub line: u32,
    pub code: u32,
    pub event: u32,
    pub saved_scheduler_word: u32,
    pub cpsr: u32,
    pub current_pipe: u32,
    pub current_pipe_record: u32,
    pub current_slot: u32,
    pub pipe_busy: u32,
    pub event_readiness: u32,
    pub pipe_irq_pending: u32,
    pub pipe_irq_trigger: u32,
}

impl MacFatalPostmortem {
    pub const fn empty() -> Self {
        Self {
            valid: 0,
            version: 1,
            line: 222,
            code: 0x29,
            event: 0,
            saved_scheduler_word: 0,
            cpsr: 0,
            current_pipe: 0,
            current_pipe_record: 0,
            current_slot: 0,
            pipe_busy: 0,
            event_readiness: 0,
            pipe_irq_pending: 0,
            pipe_irq_trigger: 0,
        }
    }
}

// Fatal publication never returns. Readers must inspect `valid` before the
// remaining words.
pub(crate) static MAC_FATAL_POSTMORTEM: Global<UnsafeCell<MacFatalPostmortem>> =
    Global::new(UnsafeCell::new(MacFatalPostmortem::empty()));

pub(crate) fn capture_mac_fatal_postmortem<M: Hw>(
    mmio: &mut M,
    pipes: &Pipes,
    event: MacEvent,
    saved_scheduler_word: SchedulerWord,
    cpsr: u32,
    output: &mut MacFatalPostmortem,
) {
    output.valid = 0;
    output.version = 1;
    output.line = 222;
    output.code = 0x29;
    output.event = event.raw;
    output.saved_scheduler_word = saved_scheduler_word.raw();
    output.cpsr = cpsr;
    output.current_pipe = u32::from(pipes.latched.get());
    output.current_pipe_record = u32::from(pipes.current_pipe.get());
    output.current_slot = u32::from(pipes.current_slot.get());
    output.pipe_busy = u32::from(pipes.busy.get());
    output.event_readiness = mmio.read(MAC_EVENT_READINESS);
    output.pipe_irq_pending = mmio.read(PIPE_IRQ_PENDING);
    output.pipe_irq_trigger = mmio.read(PIPE_IRQ_TRIGGER);
}

impl MacEvent {
    /// Decodes the fields consumed by vendor `mac_irq_handler` (`0x9eb4`).
    /// Bit 31 is the event-FIFO empty sentinel.
    pub fn decode(raw: u32) -> Option<Self> {
        (raw & 0x8000_0000 == 0).then_some(Self {
            raw,
            event_type: ((raw & 0x3fff) >> 8) as u8,
            pipe: ((raw >> 18) & 3) as u8,
            phase: ((raw >> 16) & 3) as u8,
            status: (raw & 0x3f) as u8,
            pipe_marker: raw & (1 << 25) != 0,
            completion_marker: raw & (1 << 24) != 0,
            fatal_marker: raw & (1 << 30) != 0,
            pipe_service_marker: raw & (1 << 23) != 0,
            beacon_marker: raw & (1 << 26) != 0,
            sideband_marker: raw & (1 << 7) != 0,
        })
    }

    /// Index that vendor `0x9ee4..0x9ee8` is allowed to latch globally.
    pub fn pipe_index(self) -> Option<u8> {
        (self.pipe_marker && self.event_type == 0x37).then_some(self.pipe)
    }

    /// Status consumed by the bit-24 dispatch path. Low bits in other event
    /// classes are not terminal TX status.
    pub fn completion_status(self) -> Option<u8> {
        self.completion_marker.then_some(self.status)
    }

}

/// The CPSR, for the fatal postmortem record.
#[cfg(target_arch = "arm")]
pub(crate) fn read_cpsr() -> u32 {
    aarch32_cpu::register::Cpsr::read().raw_value()
}

/// Terminal production path for MAC event bit 30. It publishes a bounded Rust
/// postmortem, then spins until reset.
///
/// # Safety
/// The caller must own the destructive MAC event consumer. This function never
/// returns and the record may only be reused after a full firmware reset.
#[cfg(target_arch = "arm")]
pub fn enter_mac_fatal_quiescence(
    event: MacEvent,
    saved_scheduler_word: SchedulerWord,
) -> ! {
    let cpsr = read_cpsr();
    let record = MAC_FATAL_POSTMORTEM.get();
    unsafe { core::ptr::addr_of_mut!((*record).valid).write_volatile(0) };
    unsafe {
        capture_mac_fatal_postmortem(
            &mut Mmio,
            pipes(),
            event,
            saved_scheduler_word,
            cpsr,
            &mut *record,
        );
    }
    unsafe { core::arch::asm!("", options(nostack, preserves_flags)) };
    unsafe { core::ptr::addr_of_mut!((*record).valid).write_volatile(MAC_FATAL_MAGIC) };
    unsafe { core::arch::asm!("", options(nostack, preserves_flags)) };
    unsafe {
        let rx = [0_u32; 11];
        // The vendor traces the raw event before treating bit 30 as terminal.
        // Capture the still-live RX producer delta at that same boundary so a
        // MAC fatal cannot preempt the later cooperative command matcher.
        // The complete pre-GO and postmortem records remain committed in BSS.
        let postmortem = &*record;
        let pipe = postmortem.current_pipe_record as u8 & 3;
        let slot = postmortem.current_slot as u8 & 3;
        let record = pipes().pipe(pipe);
        let command = packet_ram::tx_command(usize::from(pipe), usize::from(slot));
        let hardware_ring = record.hardware_ring(pipe);
        let ring_word = hardware_ring
            .map(|ring| read_u32(ring.cursor_and_pending_mask() as usize))
            .unwrap_or(0);
        // The slot the MAC itself is pointing at, which is not necessarily the
        // one we think is current.
        let cursor_slot = ((ring_word >> 27) & 3) as usize;
        let cursor_command = packet_ram::tx_command(usize::from(pipe), cursor_slot);
        let cursor_command_word_0 = read_u32(cursor_command);
        let cursor_command_word_14 = read_u32(cursor_command.wrapping_add(0x14));
        let trace_count = read_u32(0xfff0_3794);
        let prior_event = |back: u32| {
            let index = trace_count.wrapping_sub(back) & 0x1f;
            read_u32(0xfff0_3714 + index as usize * 4)
        };
        crate::hif::publish_mac_fatal_exception([
            // `trace_mac_event()` commits the raw event before dispatch. Keep
            // the preceding event sequence inside the driver's bounded trace
            // prefix: the fatal often arrives after the owning slot has
            // already been released, making postmortem frame pointers stale.
            event.raw,
            trace_count,
            prior_event(2),
            prior_event(3),
            prior_event(4),
            prior_event(5),
            command as u32,
            // The driver shows 18 words. These four slots held the command
            // setup words, which static analysis showed are well-formed and
            // constant across captures; the cursor/producer comparison is
            // what we actually lack.
            // Pipe cursors: producer, last, current and state bytes.
            u32::from(record.producer.get())
                | (u32::from(record.last.get()) << 8)
                | (u32::from(record.current.get()) << 16)
                | (u32::from(record.state.get()) << 24),
            cursor_command as u32,
            cursor_command_word_0,
            cursor_command_word_14,
            rx[0],
            rx[1],
            postmortem.current_pipe,
            (u32::from(pipe) << 8) | u32::from(slot),
            hardware_ring.map(TxHardwareRingAddress::raw).unwrap_or(0),
            ring_word,
            postmortem.event_readiness,
        ]);
    }
    loop {
        unsafe { core::arch::asm!("nop", options(nomem, nostack)) };
    }
}

/// Ordinary `txp_pipe_tx_status`: mark the current slot done, reset the
/// contention window, complete the frame and move the pipe cursors.
///
/// # Safety
/// Pipe records, frame nodes, completion state, and MMIO must be valid and
/// exclusively owned by the caller.
#[cfg(target_arch = "arm")]
pub(crate) fn service_txp_pipe_tx_status(status: u8) {
    let pipes = pipes();
    let pipe_index = pipes.latched.get() & 3;
    let pipe = pipes.pipe(pipe_index);
    let current = pipe.current.get();
    let slot = pipe.slot(current);
    pipes.publish_current(pipe_index);

    let input = OrdinaryTxPipeStatusInput {
        pipe_active: pipe.state.get() == 1,
        expected_status: slot.expected_status.get(),
        slot_state: slot.state.get(),
        global_busy: pipes.busy.get(),
        pipe_current: current,
        pipe_last: pipe.last.get(),
        pipe_status: pipe.watchdog.get(),
    };
    let plan = plan_ordinary_tx_pipe_status(input, status);
    if plan == OrdinaryTxPipeStatusPlan::Ineligible {
        return;
    }

    // This write must precede the busy/suppression decision.
    slot.state.set(5);
    let OrdinaryTxPipeStatusPlan::Complete {
        initialize_pipe_status,
        cursor,
    } = plan
    else {
        return;
    };
    if initialize_pipe_status {
        pipe.watchdog.set(1);
    }
    let Some(live) = LiveTxSlot::new(pipes, pipe_index, current) else {
        crate::halt_always!();
    };
    if slot.prefix.get().is_none() {
        let _ = crate::backoff::reset(
            crate::backoff::banks(),
            live.context.get().interface.get(),
            QUEUE_TO_ACCESS_CATEGORY[usize::from(pipe_index)],
        );
    }
    complete_tx_pipe_slot(live, 0);
    if slot.prefix.get() == Some(super::Protection::Rts) {
        // The status of an RTS slot is its CTS.
        crate::counters::bump(crate::counters::Counter::RtsSuccess);
    }
    if slot.prefix.get().is_some() {
        // Keep producer at the prefix: a data retry may rewind to it.
        pipe.current.set(current.wrapping_add(1) & 3);
    } else {
        pipe.apply_cursor(cursor);
    }
}

/// Bit-24 TX status for one popped event.
#[cfg(target_arch = "arm")]
pub(crate) fn service_tx_status(
    backend: &mut SingleProbeMacBackend,
    event: MacEvent,
    status: u8,
    saved_scheduler_word: SchedulerWord,
) {
    {
        let pipe = pipes().latched.get() & 3;
        let pending_mask = saved_scheduler_word.raw() & (0x100_u32 << pipe);
        if pending_mask != 0 && matches!(status, 4 | 0x19) {
            service_tx_retry(backend, pending_mask);
            return;
        }

        if !(event.event_type == 0x39 && status == 6) {
            service_txp_pipe_tx_status(status);
        }
        if !event.pipe_service_marker || pending_mask == 0 {
            return;
        }

        let record = pipes().pipe(pipe);
        if record.state.get() != 1 || record.current_slot().expected_status.get() == status {
            return;
        }
        let mismatch = backend.mismatch[usize::from(pipe)].wrapping_add(1);
        backend.mismatch[usize::from(pipe)] = mismatch;
        if mismatch > 2 {
            service_tx_retry(backend, pending_mask);
        }
    }
}

/// Pipe-phase effects of an event with bit 25 set (`0x9ee4..`).
#[cfg(target_arch = "arm")]
pub(crate) fn service_pipe_phase(backend: &mut SingleProbeMacBackend, event: MacEvent) {
    {
        let pipe = event.pipe_index();
        if let Some(pipe) = pipe {
            pipes().latched.set(pipe);
        }
        match event.phase {
            2 => match pipe {
                Some(pipe) => {
                    SEQUENCING.sequence_pending.set(false);
                    service_pipe_tx_start(pipe);
                }
                None => service_mac_irq_count_status(event.event_type),
            },
            3 | 1 if event.phase == 3 || event.event_type == 0x19 => {
                if SEQUENCING.sequence_pending.get() {
                    let index = usize::from(SEQUENCING.sequence_slot.get());
                    let duration = packet_ram::duration_word(index);
                    write_u16(duration, read_u16(duration).wrapping_add(0x10) & !0x0f);
                }
                if let Some(pipe) = pipe {
                    // Vendor `txp_pipe_tx_success` clears the pipe's status
                    // mismatch count with every success, so only mismatches
                    // in a row force a retry.
                    backend.mismatch[usize::from(pipe & 3)] = 0;
                    service_pipe_tx_success(pipe);
                }
            }
            _ => {}
        }
    }
}

/// Every effect of one destructively consumed MAC event. There is no
/// rejection or deferral path.
#[cfg(target_arch = "arm")]
pub(crate) fn service_mac_event(backend: &mut SingleProbeMacBackend, event: MacEvent) {
    {
        trace_mac_event(event.raw);
        if event.beacon_marker {
            crate::ap::TBTT_EVENTS.set(crate::ap::TBTT_EVENTS.get().wrapping_add(1));
        }
        if event.pipe_marker {
            service_pipe_phase(backend, event);
        }
        // One scheduler-word snapshot serves both bit-23 service and bit-24
        // status, even though bit-23 acknowledgement changes the register.
        let saved = if event.pipe_service_marker || event.completion_marker {
            capture_pipe_scheduler_word(&mut Mmio)
        } else {
            SchedulerWord::new(0)
        };
        if event.pipe_service_marker && saved.raw() & 0x0000_f0ff != 0 {
            execute_mac_pipe_service(&mut Mmio, pipes(), saved, |descriptor, frame| {
                backend.program_random_backoff(descriptor, frame)
            });
        }
        if let Some(status) = event.completion_status() {
            service_tx_status(backend, event, status, saved);
        }
        if event.sideband_marker {
            // Sideband capture (`0x9ff8`). Nothing consumes the value; the read
            // is kept because it may acknowledge the capture.
            let _ = read_u32(0x0ab8_0c50);
        }
    }
}

#[cfg(target_arch = "arm")]
pub(crate) fn terminal_probe_backend_fault(pipe: u8) -> ! {
    let event = MacEvent {
        raw: (1 << 30) | (u32::from(pipe & 3) << 18),
        event_type: 0,
        pipe: pipe & 3,
        phase: 0,
        status: 0,
        pipe_marker: false,
        completion_marker: false,
        fatal_marker: true,
        pipe_service_marker: false,
        beacon_marker: false,
        sideband_marker: false,
    };
    let saved = capture_pipe_scheduler_word(&mut Mmio);
    enter_mac_fatal_quiescence(event, saved)
}

/// Bounded destructive MAC event FIFO loop (`mac_irq_handler`, `0x9eb4`).
/// The first pop is unconditional; `0x09c00a24` gates the later ones.
///
/// # Safety
/// The caller must exclusively own `0x09c00a20/24`, all MAC pipe state, and
/// completion processing for the entire call. A fatal event never returns.
#[cfg(target_arch = "arm")]
pub(crate) fn service_mac_event_fifo(backend: &mut SingleProbeMacBackend, max_events: u32) {
    if max_events == 0 {
        return;
    }
    let mut processed = 0_u32;
    let mut raw = read_u32(crate::platform::mac_register(0x0a20));
    while let Some(event) = MacEvent::decode(raw) {
        service_mac_event(backend, event);
        backend.serviced = backend.serviced.wrapping_add(1);
        processed += 1;
        if processed == max_events
            || (read_u32(MAC_EVENT_READINESS as usize) as i32) < 0
        {
            return;
        }
        raw = read_u32(crate::platform::mac_register(0x0a20));
    }
}


/// State TX keeps across MAC events: the PHY operation it drives around a
/// scheduler pass, and the duration word whose sequence number advances when
/// the frame that used it ends.
pub(crate) struct TxSequencing {
    /// PHY operation: 1 stopped, 2 TX finished, 3 started, 4 TX programmed.
    pub(crate) phy_operation: Cell<u8>,
    /// `sequence_slot` names a duration word the next TX end advances.
    pub(crate) sequence_pending: Cell<bool>,
    pub(crate) sequence_slot: Cell<u8>,
}

static SEQUENCING: Global<TxSequencing> = Global::new(TxSequencing {
    phy_operation: Cell::new(0),
    sequence_pending: Cell::new(false),
    sequence_slot: Cell::new(0),
});

/// Records the PHY operation state the receive path leaves behind.
pub fn set_phy_operation(state: u8) {
    SEQUENCING.phy_operation.set(state);
}

/// Command-3 path through `phy_state_cmd_dispatch` (Thumb `0x16fa0`): lock
/// the programmed TX PHY.
pub fn dispatch_phy_command_3() {
    let global_state = &crate::phy::state().retained_state;
    if global_state.get() == 4 {
        global_state.set(5);
    }
}

/// Command-2 path through `phy_state_cmd_dispatch` (Thumb `0x16f92`),
/// including the `0x19fc4 -> 0x19f90` PHY register sequence. Returns the PHY
/// TX state.
///
/// # Safety
/// The PHY MMIO blocks must be exclusively owned.
pub fn dispatch_phy_command_2() -> u8 {
    let global_state = &crate::phy::state().retained_state;
    if global_state.get() != 5 {
        {
            write_u32(0x0abb_8004, read_u32(0x0abb_8004) | 0x800);
            write_u32(0x0abb_8014, 0x0014_00c8);
            write_u32(0x0abb_8010, 0x0014_00c8);
            for address in [0x0abb_8440, 0x0abb_8444, 0x0abb_8448, 0x0abb_844c] {
                write_u32(address, 0x0014_00c8);
            }
            let control = read_u32(0x0abb_800c);
            write_u32(0x0abb_800c, control & !0x3040);
            write_u32(0x0abb_800c, control | 0x00e4_0e20);
        }
        global_state.set(4);
    }
    global_state.get()
}

/// Command-1 path through `pac_phy_start_op` (`0x7f10`). Vendor TX scheduling
/// executes this before every queue/pipe pass so the pipe-start event can
/// enter command 2 and program the TX PHY.
///
/// # Safety
/// PHY operation state must be exclusively owned.
#[cfg(target_arch = "arm")]
pub fn start_phy_operation_1() {
    let global_state = &crate::phy::state().retained_state;
    if global_state.get() != 5 {
        global_state.set(3);
    }
    SEQUENCING.phy_operation.set(global_state.get());
    // The vendor also starts a 10 s PHY operation timeout here. Nothing in
    // this firmware handles its expiry, and timer 1 wakes the main loop.
}

/// Pipe TX success (`0x9cdc`): the MAC finished the current slot.
#[cfg(target_arch = "arm")]
pub(crate) fn service_pipe_tx_success(pipe_index: u8) {
    let pipes = pipes();
    let pipe = pipes.pipe(pipe_index);
    let current = pipe.current.get();
    let slot = pipe.slot(current);

    pipes.busy.set(false);
    slot.state.set(3);
    slot.transmitted_at.set(crate::platform::timer_now());
    pipe.progressed();

    let pipe_state = pipe.state.get();
    let retry_rate = slot.expected_status.get();
    // A frame expecting a response (a BlockAckReq, class 0x0c) completes later,
    // through the status event the MAC raises once the response arrives. A
    // success the watchdog's recovery overtook finds the pipe disarmed and its
    // slots empty, and is ignored, as the vendor's is.
    if pipe_state == 1 && retry_rate == 0xff {
        let Some(current_slot) = LiveTxSlot::new(pipes, pipe_index, current) else {
            crate::halt_always!();
        };
        let context = current_slot.context.get();
        let flags = context.flags.get();
        if slot.prefix.get().is_none() && success_marker_resets_backoff(pipe_state, retry_rate, flags) {
            let _ = crate::backoff::reset(
                crate::backoff::banks(),
                context.interface.get(),
                QUEUE_TO_ACCESS_CATEGORY[usize::from(pipe_index)],
            );
        }

        let last = pipe.last.get();
        if current == last {
            let mut index = pipe.producer.get();
            loop {
                if let Some(live) = LiveTxSlot::new(pipes, pipe_index, index) {
                    complete_tx_pipe_slot(live, 0);
                } else if pipe.slot(index).prefix.get().is_none() {
                    crate::halt_always!();
                }
                if index == last {
                    break;
                }
                index = index.wrapping_add(1) & 3;
            }
            pipe.producer.set(last.wrapping_add(1) & 3);
            pipe.disarm();
        } else {
            // A CTS-to-self needs no response, but is not a host completion.
            if slot.prefix.get().is_some() {
                complete_tx_pipe_slot(current_slot, 0);
            }
            pipe.current.set(current.wrapping_add(1) & 3);
        }
    }

    if SEQUENCING.phy_operation.get() == 4 {
        dispatch_phy_command_3();
        SEQUENCING.phy_operation.set(2);
    }
}

/// Pipe TX start (`txp_pipe_tx_start`, `0x9dea`): the MAC began the current
/// slot. Programs the TX PHY on the first start of a scheduler pass and
/// inserts the sequence number for frames that take it from the duration
/// words.
///
/// # Safety
/// Pipe/slot/frame records, packet headers, duration tables and PHY MMIO must
/// be valid and exclusively owned.
#[cfg(target_arch = "arm")]
pub(crate) fn service_pipe_tx_start(pipe: u8) {
    {
        let pipes = pipes();
        let record = pipes.pipe(pipe);
        let current = record.current.get();
        if record.state.get() == 0 {
            return;
        }
        let Some(live_slot) = LiveTxSlot::new(pipes, pipe, current) else {
            crate::halt_always!();
        };

        record.slot(current).state.set(2);
        record.progressed();
        let context = live_slot.context.get();
        if SEQUENCING.phy_operation.get() == 3 && dispatch_phy_command_2() == 4 {
            SEQUENCING.phy_operation.set(4);
        }

        if record.state.get() == 1
            && record.slot(current).prefix.get().is_none()
            && context.frame_control.get() & 0x0f != 4
            && context.flags.get() & 1 == 0
        {
            let rate = context.duration_slot.get();
            if usize::from(rate) >= packet_ram::DURATION_WORD_COUNT {
                crate::halt_always!();
            }
            let duration = read_u16(packet_ram::duration_word(usize::from(rate)));
            let Some(header) = validated_context_tx_frame(context) else {
                crate::halt_always!();
            };
            write_u16(header.sequence_control() as usize, duration);
            context.flags.set(context.flags.get() | 1);
            SEQUENCING.sequence_pending.set(true);
            SEQUENCING.sequence_slot.set(rate);
        }

        // Descriptor word 2 bit 27 marks a frame whose successor starts on its
        // own; otherwise the software cursor follows the MAC here.
        if read_u32(live_slot.command() as usize + 8) & (1 << 27) == 0 && current != record.last.get() {
            record.current.set(current.wrapping_add(1) & 3);
        }
    }
}

/// Event trace ring (`0x164f8`) read back by the MAC fatal postmortem.
///
/// # Safety
/// The high-alias trace RAM must be mapped and writable.
#[cfg(target_arch = "arm")]
pub(crate) fn trace_mac_event(raw: u32) {
    let index = read_u32(0xfff0_3794);
    write_u32(0xfff0_3714 + (index as usize & 0x1f) * 4, raw);
    write_u32(0xfff0_3794, index.wrapping_add(1));
}

/// Non-pipe phase-2 accounting (`0x9d9e`): event type 0x19 selects the
/// duration word the next phase-3 event advances.
///
/// # Safety
/// MAC scheduler state must be mapped.
#[cfg(target_arch = "arm")]
pub(crate) fn service_mac_irq_count_status(event_type: u8) {
    if event_type != 0x19 {
        return;
    }
    SEQUENCING.sequence_pending.set(true);
    SEQUENCING.sequence_slot.set(crate::mac::low_mac().pas(0_usize).path_selector.get());
}

/// `pac_phy_stop_op`: enter command 7. The vendor arms the operation timer
/// and cancels it immediately, which leaves no trace.
///
/// # Safety
/// PHY command and scheduler-timer state must be exclusively owned.
#[cfg(target_arch = "arm")]
pub fn stop_phy_operation_7() {
    SEQUENCING.phy_operation.set(1);
}
