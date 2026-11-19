//! The four MAC TX pipes.
//!
//! Each pipe has a hardware ring (MMIO: duration FIFO, GO, command mask) and
//! four command slots whose descriptors live in packet RAM
//! (`packet_ram::tx_command(pipe, slot)`). [`Pipes`] is the CPU's bookkeeping
//! for them: which slot the scheduler fills next, which one the MAC is on, and
//! which frame owns each slot.

use crate::global::Global;
use core::cell::Cell;

use super::*;

/// One command slot of a pipe.
pub(crate) struct Slot {
    /// Response class the MAC reports for the frame (`0xff`: no response).
    pub(crate) expected_status: Cell<u8>,
    /// 0 reserved, 1 published, 2 started, 3 transmitted, 4 retry-owned,
    /// 5 status seen.
    pub(crate) state: Cell<u8>,
    /// Airtime word pushed into the ring's duration FIFO at publication.
    pub(crate) duration: Cell<u32>,
    /// Context that owns the slot.
    pub(crate) frame: Cell<Option<ContextId>>,
    /// Prefixes borrow the context; only the data slot may complete it.
    pub(crate) prefix: Cell<Option<Protection>>,
    /// Timer when the MAC last reported the slot transmitted (state 3).
    pub(crate) transmitted_at: Cell<u32>,
}

impl Slot {
    pub(crate) const fn new() -> Self {
        Self {
            expected_status: Cell::new(0),
            state: Cell::new(0),
            duration: Cell::new(0),
            frame: Cell::new(None),
            prefix: Cell::new(None),
            transmitted_at: Cell::new(0),
        }
    }

    /// Point the slot at a frame whose descriptor is being built. Nothing is
    /// visible to the MAC until the pipe is triggered.
    pub(crate) fn reserve(&self, context: ContextId, frame_kind: u8) -> SlotImage {
        let image = SlotImage {
            expected_status: self.expected_status.get(),
            state: self.state.get(),
            frame: self.frame.get(),
            prefix: self.prefix.get(),
        };
        self.expected_status.set(frame_kind);
        self.state.set(0);
        self.frame.set(Some(context));
        self.prefix.set(None);
        image
    }

    /// Undo a reservation that was never published.
    pub(crate) fn restore(&self, image: SlotImage) {
        self.expected_status.set(image.expected_status);
        self.state.set(image.state);
        self.frame.set(image.frame);
        self.prefix.set(image.prefix);
    }
}

/// Slot bookkeeping saved by [`Slot::reserve`].
#[derive(Clone, Copy)]
pub(crate) struct SlotImage {
    expected_status: u8,
    state: u8,
    frame: Option<ContextId>,
    prefix: Option<Protection>,
}

/// Retire one hardware command, returning its context only for a data owner.
/// Shared by status, no-response completion and watchdog recovery.
pub(crate) fn retire_command_slot(pipes: &Pipes, pipe: u8, slot: u8) -> Option<ContextId> {
    let slot = pipes.pipe(pipe).slot(slot);
    let context = slot.frame.take()?;
    pipes.active_tx.set(pipes.active_tx.get().wrapping_sub(1));
    slot.prefix.get().is_none().then_some(context)
}

pub(crate) struct Pipe {
    /// Next slot the scheduler fills.
    pub(crate) producer: Cell<u8>,
    /// Last slot of the published batch.
    pub(crate) last: Cell<u8>,
    /// Slot the MAC is working on.
    pub(crate) current: Cell<u8>,
    /// 0 idle, 1 armed.
    pub(crate) state: Cell<u8>,
    /// Bit 0: programmed by the scheduler (supervised by the watchdog).
    pub(crate) control: Cell<u8>,
    /// Signed countdown of watchdog ticks before recovery.
    pub(crate) watchdog: Cell<u8>,
    /// Hardware ring registers, 0 before `reset`.
    pub(crate) ring: Cell<u32>,
    /// Timer when the MAC last moved the pipe on: armed, a slot started or
    /// transmitted.
    pub(crate) progress_at: Cell<u32>,
    pub(crate) slots: [Slot; 4],
}

impl Pipe {
    const fn new() -> Self {
        Self {
            producer: Cell::new(0),
            last: Cell::new(0),
            current: Cell::new(0),
            state: Cell::new(0),
            control: Cell::new(0),
            watchdog: Cell::new(0),
            ring: Cell::new(0),
            progress_at: Cell::new(0),
            slots: [const { Slot::new() }; 4],
        }
    }

    pub(crate) fn slot(&self, slot: u8) -> &Slot {
        &self.slots[usize::from(slot & 3)]
    }

    pub(crate) fn current_slot(&self) -> &Slot {
        self.slot(self.current.get())
    }

    /// The programmed hardware ring of pipe `pipe`, if any.
    pub(crate) fn hardware_ring(&self, pipe: u8) -> Option<TxHardwareRingAddress> {
        TxHardwareRingAddress::for_pipe(pipe, self.ring.get())
    }

    /// Moves the software cursors after a slot completed through the status
    /// path.
    pub(crate) fn apply_cursor(&self, cursor: OrdinaryTxPipeCursorPlan) {
        match cursor {
            OrdinaryTxPipeCursorPlan::AdvanceCurrent { next_current } => {
                self.current.set(next_current);
                self.producer.set(next_current);
            }
            OrdinaryTxPipeCursorPlan::Recycle { next_head } => {
                self.producer.set(next_head);
                self.disarm();
            }
        }
    }

    /// The MAC moved the pipe on (see [`Self::progress_at`]).
    pub(crate) fn progressed(&self) {
        #[cfg(target_arch = "arm")]
        self.progress_at.set(crate::platform::timer_now());
    }

    pub(crate) fn disarm(&self) {
        self.state.set(0);
        self.control.set(0);
        self.watchdog.set(PIPE_WATCHDOG_RELOAD);
    }
}

pub(crate) struct Pipes {
    pub(crate) pipes: [Pipe; 4],
    /// Pipe latched by the last type-0x37 pipe event.
    pub(crate) latched: Cell<u8>,
    /// Pipe and slot last published as current, for the fatal postmortem.
    pub(crate) current_pipe: Cell<u8>,
    pub(crate) current_slot: Cell<u8>,
    /// Set while a retry owns a slot; ordinary status is suppressed then.
    pub(crate) busy: Cell<bool>,
    /// Frames on hardware across all pipes.
    pub(crate) active_tx: Cell<u8>,
}

impl Pipes {
    pub(crate) const fn new() -> Self {
        Self {
            pipes: [const { Pipe::new() }; 4],
            latched: Cell::new(0),
            current_pipe: Cell::new(0),
            current_slot: Cell::new(0),
            busy: Cell::new(false),
            active_tx: Cell::new(0),
        }
    }

    pub(crate) fn pipe(&self, pipe: u8) -> &Pipe {
        &self.pipes[usize::from(pipe & 3)]
    }

    /// Records `pipe`'s current slot as the one being serviced.
    pub(crate) fn publish_current(&self, pipe: u8) {
        self.current_pipe.set(pipe & 3);
        self.current_slot.set(self.pipe(pipe).current.get());
    }

    /// Startup: synchronize a pipe's cursors with its hardware ring.
    pub(crate) fn reset(&self, pipe: u8, hardware_slot: u8, ring: u32) {
        let record = self.pipe(pipe);
        record.producer.set(hardware_slot);
        record.last.set(hardware_slot);
        record.current.set(hardware_slot);
        record.state.set(0);
        record.ring.set(ring);
    }

    /// Startup: clear the scheduler and watchdog state of every pipe.
    pub(crate) fn reset_control(&self) {
        for pipe in &self.pipes {
            pipe.control.set(0);
            pipe.watchdog.set(PIPE_WATCHDOG_RELOAD);
        }
    }
}

static PIPES: Global<Pipes> = Global::new(Pipes::new());

pub(crate) fn pipes() -> &'static Pipes {
    &PIPES
}

/// The access category each hardware queue (pipe) serves, and back.
pub(crate) const QUEUE_TO_ACCESS_CATEGORY: [u8; 4] = [1, 0, 2, 3];
pub(crate) const ACCESS_CATEGORY_TO_QUEUE: [u8; 4] = [1, 0, 2, 3];

/// TXOP quantum register of a pipe.
pub(crate) const fn quantum_register(pipe: u8) -> u32 {
    crate::platform::mac_register(0x0e70) as u32 + (pipe as u32 & 3) * 4
}

/// A slot that holds a frame.
#[derive(Clone, Copy)]
pub(crate) struct LiveTxSlot {
    pub(crate) pipe: u8,
    pub(crate) slot: u8,
    pub(crate) context: ContextId,
}

impl LiveTxSlot {
    pub(crate) fn new(pipes: &Pipes, pipe: u8, slot: u8) -> Option<Self> {
        if pipe >= 4 || slot >= 4 {
            return None;
        }
        let context = pipes.pipe(pipe).slot(slot).frame.get()?;
        Some(Self {
            pipe,
            slot,
            context,
        })
    }

    pub(crate) fn command(self) -> u32 {
        packet_ram::tx_command(usize::from(self.pipe), usize::from(self.slot)) as u32
    }
}

#[derive(Clone, Copy)]
pub(crate) struct PublishedSlotIdentity {
    pub(crate) context: ContextId,
    pub(crate) pipe: u8,
    pub(crate) slot: u8,
}

/// One immutable snapshot of `0x09c00e84`, taken before bit-23 service.
/// Its value must be reused for the later bit-24 pending decision even though
/// the bit-23 acknowledgement can change the hardware register.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SchedulerWord(pub(crate) u32);

impl SchedulerWord {
    pub const fn new(raw: u32) -> Self {
        Self(raw)
    }

    pub const fn raw(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MacPipeServicePlan {
    None,
    LowNibble {
        asserted: u8,
        backoff_pipe_mask: u8,
        trigger: u32,
        acknowledgement: u32,
    },
    MiddleNibble {
        selected_pipe: u8,
        trigger: u32,
        acknowledgement: u32,
    },
    HighNibble {
        selected_pipe: u8,
        acknowledgement: u32,
    },
}

/// Pure selection/acknowledgement plan for `mac_pipe_irq_service`
/// (`0x9b6e..0x9c28`). Hardware backoff programming remains an executor leaf.
pub fn plan_mac_pipe_service(
    scheduler_word: SchedulerWord,
    latched_pipe: u8,
) -> MacPipeServicePlan {
    let scheduler_word = scheduler_word.raw();
    let low = (scheduler_word & 0x0f) as u8;
    if low != 0 {
        return MacPipeServicePlan::LowNibble {
            asserted: low,
            backoff_pipe_mask: low & 0x07,
            trigger: u32::from(low) << 25,
            acknowledgement: u32::from(low).wrapping_add(1).wrapping_neg(),
        };
    }
    let middle = ((scheduler_word >> 4) & 0x0f) as u8;
    if middle != 0 {
        let selected_pipe = middle.trailing_zeros() as u8;
        return MacPipeServicePlan::MiddleNibble {
            selected_pipe,
            trigger: 1_u32 << (u32::from(selected_pipe) + 25),
            acknowledgement: (0x10_u32 << selected_pipe).wrapping_add(1).wrapping_neg(),
        };
    }
    if scheduler_word & 0xf000 != 0 {
        let selected_pipe = latched_pipe & 3;
        return MacPipeServicePlan::HighNibble {
            selected_pipe,
            acknowledgement: (0x1000_u32 << selected_pipe).wrapping_add(1).wrapping_neg(),
        };
    }
    MacPipeServicePlan::None
}

/// Captures the sole pre-service scheduler word for one event. Call this once,
/// before bit-23 service, and pass the returned value to both service planning
/// and bit-24 status resolution.
pub fn capture_pipe_scheduler_word<M: Hw>(mmio: &mut M) -> SchedulerWord {
    SchedulerWord::new(mmio.read(PIPE_IRQ_PENDING))
}

#[repr(C)]
pub(crate) struct TxHardwareRingLayout {
    pub(crate) duration_fifo: u32,
    pub(crate) opaque_04: [u8; 0x10],
    pub(crate) go: u32,
    pub(crate) inactive_sentinel: u32,
    pub(crate) completion_word: u32,
    pub(crate) cursor_and_pending_mask: u32,
}

#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct TxHardwareRingAddress(pub(crate) u32);

impl TxHardwareRingAddress {
    pub(crate) const fn for_pipe(pipe: u8, address: u32) -> Option<Self> {
        if pipe < 4 && address == crate::platform::tx_ring_register(pipe as usize, 0) as u32 {
            Some(Self(address))
        } else {
            None
        }
    }
    pub(crate) const fn raw(self) -> u32 {
        self.0
    }
    pub(crate) const fn field(self, offset: usize) -> u32 {
        self.0.wrapping_add(offset as u32)
    }
    pub(crate) const fn duration_fifo(self) -> u32 {
        self.field(core::mem::offset_of!(TxHardwareRingLayout, duration_fifo))
    }
    pub(crate) const fn go(self) -> u32 {
        self.field(core::mem::offset_of!(TxHardwareRingLayout, go))
    }
    pub(crate) const fn inactive_sentinel(self) -> u32 {
        self.field(core::mem::offset_of!(
            TxHardwareRingLayout,
            inactive_sentinel
        ))
    }
    pub(crate) const fn completion_word(self) -> u32 {
        self.field(core::mem::offset_of!(TxHardwareRingLayout, completion_word))
    }
    pub(crate) const fn cursor_and_pending_mask(self) -> u32 {
        self.field(core::mem::offset_of!(
            TxHardwareRingLayout,
            cursor_and_pending_mask
        ))
    }
}

/// Per-pipe watchdog reload the firmware writes at GO.
pub(crate) const PIPE_WATCHDOG_RELOAD: u8 = 5;

/// Executes one already-snapshotted `mac_pipe_irq_service` word. The caller
/// must read `0x09c00e84` once before bit-23 service and retain that word for
/// the later bit-24 decision.
pub(crate) fn execute_mac_pipe_service<M: Hw>(
    mmio: &mut M,
    pipes: &Pipes,
    scheduler_word: SchedulerWord,
    mut program_random_backoff: impl FnMut(u32, Option<ContextId>),
) {
    let latched_pipe = pipes.latched.get() & 3;
    pipes.publish_current(latched_pipe);
    match plan_mac_pipe_service(scheduler_word, latched_pipe) {
        MacPipeServicePlan::None => {}
        MacPipeServicePlan::LowNibble {
            asserted,
            trigger,
            acknowledgement,
            ..
        } => {
            for pipe in 0..3_u8 {
                if asserted & (1 << pipe) != 0 {
                    let current = pipes.pipe(pipe).current.get();
                    let slot = pipes.pipe(pipe).slot(current);
                    let descriptor =
                        packet_ram::tx_command(usize::from(pipe), usize::from(current & 3)) as u32;
                    program_random_backoff(descriptor, slot.frame.get());
                }
            }
            mmio.write(PIPE_IRQ_TRIGGER, trigger);
            mmio.write(PIPE_IRQ_PENDING, acknowledgement);
        }
        MacPipeServicePlan::MiddleNibble {
            selected_pipe,
            trigger,
            acknowledgement,
        } => {
            mmio.write(quantum_register(selected_pipe), PIPE_QUANTUM);
            mmio.write(PIPE_IRQ_TRIGGER, trigger);
            pipes.publish_current(selected_pipe);
            mmio.write(PIPE_IRQ_PENDING, acknowledgement);
        }
        MacPipeServicePlan::HighNibble {
            acknowledgement, ..
        } => {
            mmio.write(PIPE_IRQ_PENDING, acknowledgement);
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OrdinaryTxPipeStatusInput {
    pub pipe_active: bool,
    pub expected_status: u8,
    pub slot_state: u8,
    pub global_busy: bool,
    pub pipe_current: u8,
    pub pipe_last: u8,
    pub pipe_status: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryTxPipeCursorPlan {
    AdvanceCurrent { next_current: u8 },
    Recycle { next_head: u8 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OrdinaryTxPipeStatusPlan {
    Ineligible,
    SuppressedAfterSlotMark,
    Complete {
        initialize_pipe_status: bool,
        cursor: OrdinaryTxPipeCursorPlan,
    },
}

/// Pure part of `txp_pipe_tx_status` (`0x9a32`). The suppressed result
/// follows the vendor ordering: the slot has already moved from state 3 to
/// state 5 before the global busy byte is examined.
pub fn plan_ordinary_tx_pipe_status(
    input: OrdinaryTxPipeStatusInput,
    status: u8,
) -> OrdinaryTxPipeStatusPlan {
    if !input.pipe_active || input.expected_status != status || input.slot_state != 3 {
        return OrdinaryTxPipeStatusPlan::Ineligible;
    }
    if input.global_busy {
        return OrdinaryTxPipeStatusPlan::SuppressedAfterSlotMark;
    }
    let cursor = if input.pipe_current == input.pipe_last {
        OrdinaryTxPipeCursorPlan::Recycle {
            next_head: (input.pipe_last + 1) & 3,
        }
    } else {
        OrdinaryTxPipeCursorPlan::AdvanceCurrent {
            next_current: (input.pipe_current + 1) & 3,
        }
    };
    OrdinaryTxPipeStatusPlan::Complete {
        initialize_pipe_status: i8::from_ne_bytes([input.pipe_status]) < 1,
        cursor,
    }
}

/// The slots of a published batch the MAC has not finished: `current`
/// through `last`. Earlier slots completed when their status arrived (a
/// frame that expects no response goes out alone).
pub fn unfinished_slots(current: u8, last: u8) -> impl Iterator<Item = u8> {
    core::iter::successors(Some(current & 3), move |&slot| {
        (slot != last & 3).then_some((slot + 1) & 3)
    })
}

/// Stops a pipe's hardware ring before recovery takes its slots back
/// (vendor `txp_pipe_advance_slot`): the inactive sentinel, the pipe's
/// pending bits cleared, and both ring cursors moved past the batch.
/// Returns the slot the next batch starts at.
pub(crate) fn stop_pipe_ring<M: Hw>(mmio: &mut M, pipes: &Pipes, pipe: u8) -> Option<u8> {
    let record = pipes.pipe(pipe);
    let ring = record.hardware_ring(pipe)?;
    let next = if record.state.get() == 1 {
        record.last.get().wrapping_add(1)
    } else {
        record.producer.get()
    } & 3;
    mmio.write(ring.inactive_sentinel(), PIPE_RETRY_INACTIVE_SENTINEL);
    mmio.write(
        PIPE_IRQ_PENDING,
        0_u32.wrapping_sub((0x1110 << pipe) + 0x10),
    );
    let cursor = mmio.read(ring.cursor_and_pending_mask());
    mmio.write(
        ring.cursor_and_pending_mask(),
        cursor & 0xc0ff_ffff | u32::from(next) << 24 | u32::from(next) << 27,
    );
    Some(next)
}

/// Completes every slot of a stuck batch with the give-up status `0x0b`, so
/// the host receives truthful TX failures instead of losing the frames, and
/// recycles the pipe from `next_head`. Vendor `txp_fn_4155` walks the same
/// slots; it requeues frames whose lifetime has not expired instead.
///
/// # Safety
/// The caller must exclusively own the pipe records and completion state.
#[cfg(target_arch = "arm")]
fn retire_stuck_batch(pipe: u8, next_head: u8) {
    if pipe >= 4 {
        crate::halt_always!();
    }
    let pipes = pipes();
    let record = pipes.pipe(pipe);
    for slot in unfinished_slots(record.current.get(), record.last.get()) {
        // A retry owned the slot and holds every pipe's ordinary status
        // back (`busy`) until a TX success it may never see now.
        if record.slot(slot).state.get() == 4 {
            pipes.busy.set(false);
        }
        if record.slot(slot).frame.get().is_some() {
            let Some(live) = LiveTxSlot::new(pipes, pipe, slot) else {
                crate::halt_always!();
            };
            complete_tx_pipe_slot(live, 0x0b);
        }
    }
    record.current.set(record.last.get());
    record.apply_cursor(OrdinaryTxPipeCursorPlan::Recycle { next_head });
}

/// How long a transmitted frame may wait for its response (an ACK or a
/// BlockAck) before its pipe is recovered. The MAC answers within
/// microseconds or retries; a slot left waiting longer will wait for the
/// watchdog's second, holding its pipe and every later batch on it. A peer
/// that dropped its BlockAck agreement (on a pairwise rekey, say) never
/// answers an aggregate or a BlockAckReq.
pub const RESPONSE_WAIT_US: u32 = 30_000;

/// How long an armed pipe may go without the MAC starting or finishing a
/// slot before it is recovered: well beyond any wait for the medium, well
/// short of the watchdog's second.
pub const PROGRESS_WAIT_US: u32 = 100_000;

/// Recover a pipe, as the watchdog does on expiry, whose current slot was
/// transmitted more than [`RESPONSE_WAIT_US`] ago and still waits for its
/// response, or which the MAC has not moved on for [`PROGRESS_WAIT_US`].
/// Returns the number of pipes recovered.
#[cfg(target_arch = "arm")]
pub fn service_response_timeouts(now: u32) -> u32 {
    let pipes = pipes();
    let mut recovered = 0;
    for index in 0..4_u8 {
        let pipe = pipes.pipe(index);
        let slot = pipe.current_slot();
        if pipe.state.get() != 1 || pipe.control.get() & 1 == 0 || slot.frame.get().is_none() {
            continue;
        }
        let unanswered = slot.state.get() == 3
            && now.wrapping_sub(slot.transmitted_at.get()) >= RESPONSE_WAIT_US;
        let stalled = now.wrapping_sub(pipe.progress_at.get()) >= PROGRESS_WAIT_US;
        if !unanswered && !stalled {
            continue;
        }
        recover_pipe(pipes, index);
        recovered += 1;
    }
    recovered
}

/// Stop a pipe's ring, fail the rest of its batch to the host and leave it
/// ready for the next batch.
#[cfg(target_arch = "arm")]
fn recover_pipe(pipes: &Pipes, index: u8) {
    let Some(next_head) = stop_pipe_ring(&mut Mmio, pipes, index) else {
        crate::halt_always!();
    };
    retire_stuck_batch(index, next_head);
    // Vendor `txp_fn_4155` clears the programmed/abort bits and reloads the
    // counter after a recovery pass.
    let pipe = pipes.pipe(index);
    pipe.control.set(pipe.control.get() & 0xf6);
    pipe.watchdog.set(PIPE_WATCHDOG_RELOAD);
}

/// Number of cooperative main-loop passes between expensive hardware-timer
/// samples. The timer still determines elapsed time; this only gates MMIO.
pub const WATCHDOG_TIMER_SAMPLE_DIVIDER: u8 = 16;

pub const fn advance_watchdog_timer_divider(countdown: u8) -> (u8, bool) {
    if countdown == 0 {
        (WATCHDOG_TIMER_SAMPLE_DIVIDER - 1, true)
    } else {
        (countdown - 1, false)
    }
}

/// One pipe's watchdog decision, from vendor `FUN_00003bac` (the 200 ms timer
/// that Ghidra never turned into a function, recovered by disassembly).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PipeWatchdogAction {
    /// Not programmed, or not armed: nothing to supervise.
    Idle,
    /// Healthy; the counter was decremented.
    Tick,
    /// One or two ticks from expiry. Vendor nudges the radio to let a nearly
    /// stuck pipe drain (`mac_rx_pause_briefly`).
    Nudge,
    /// Expired: the pipe has been armed without completing for the whole
    /// watchdog window and must be recovered.
    Expired,
}

/// Pure per-pipe watchdog decision.
///
/// Vendor decrements a signed per-pipe counter every 200 ms for any pipe
/// whose "programmed by the scheduler" bit is set, and recovers the pipe once
/// the counter reaches zero. Arming reloads the counter to 5, so a pipe that
/// never completes is recovered after roughly a second. Without it an armed
/// pipe whose completion never arrives would stay armed forever: retirement
/// only runs from a delivered status, and the MAC stops delivering statuses
/// for a wedged pipe.
pub const fn plan_pipe_watchdog(programmed: bool, armed: bool, counter: i8) -> PipeWatchdogAction {
    if !programmed || !armed {
        return PipeWatchdogAction::Idle;
    }
    if counter > 2 {
        PipeWatchdogAction::Tick
    } else if counter > 0 {
        PipeWatchdogAction::Nudge
    } else {
        PipeWatchdogAction::Expired
    }
}

/// Runs one 200 ms watchdog tick across all four pipes.
///
/// Recovery completes every unfinished slot of the expired pipe's batch,
/// which releases their host slots from `Owned { Scheduled }`: a slot left
/// behind would hold every later host frame back.
///
/// # Safety
/// Must run from the single-threaded firmware context that owns pipe state.
#[cfg(target_arch = "arm")]
pub fn service_pipe_watchdog_tick() {
    let pipes = pipes();
    let mut nudge = false;
    let mut expired = false;
    for index in 0..4_u8 {
        let pipe = pipes.pipe(index);
        let counter = pipe.watchdog.get() as i8;
        match plan_pipe_watchdog(pipe.control.get() & 1 != 0, pipe.state.get() == 1, counter) {
            PipeWatchdogAction::Idle => (),
            PipeWatchdogAction::Tick => {
                pipe.watchdog.set(counter.wrapping_sub(1) as u8);
            }
            PipeWatchdogAction::Nudge => {
                pipe.watchdog.set(counter.wrapping_sub(1) as u8);
                nudge = true;
            }
            PipeWatchdogAction::Expired => {
                expired = true;
                recover_pipe(pipes, index);
            }
        }
    }
    // Vendor `FUN_00003bac` nudges only when no pipe expired in this tick.
    if nudge && !expired {
        crate::mac::pause_receive_briefly();
    }
}

pub(crate) const fn pipe_slot_state_index(pipe: u8, slot: u8) -> Option<usize> {
    if pipe < 4 && slot < 4 {
        Some(pipe as usize * 4 + slot as usize)
    } else {
        None
    }
}

pub(crate) fn publication_registration_allowed(
    occupied: [bool; 4],
    batch: BatchPosition,
    slot: u8,
) -> bool {
    if slot >= 4 || occupied[usize::from(slot)] {
        return false;
    }
    let count = occupied.into_iter().filter(|occupied| *occupied).count();
    let prior_slots_are_contiguous =
        (1..=count).all(|offset| occupied[(usize::from(slot) + 4 - offset) & 3]);
    match batch {
        BatchPosition::Only | BatchPosition::First => count == 0,
        BatchPosition::Middle => (1..=2).contains(&count) && prior_slots_are_contiguous,
        BatchPosition::Last => (1..=3).contains(&count) && prior_slots_are_contiguous,
    }
}

/// Irreversibly advance one reserved class-0 slot and trigger its MAC pipe.
/// All fallible descriptor/ring work must have completed before this boundary.
///
/// # Safety
/// The slot, command storage, pipe producer, and class-0 context must be
/// exclusively owned by the host runtime.
#[cfg(target_arch = "arm")]
pub fn publish_host_class0_slot(
    engine: &mut TxEngine,
    context: ContextId,
    pipe: u8,
    slot: u8,
    batch: BatchPosition,
) -> Result<(), ProbeBuildError> {
    if !context.is_host() || pipe >= 4 || slot >= 4 {
        return Err(ProbeBuildError::UnsupportedPublicationShape);
    }
    let pipes = pipes();
    if !single_frame_slot_matches(pipes.pipe(pipe).slot(slot), context) {
        return Err(ProbeBuildError::PipeSlotOwnershipMismatch);
    }
    if pipes.pipe(pipe).hardware_ring(pipe).is_none() {
        return Err(ProbeBuildError::PipeStateUnavailable);
    }

    let protected = context.get().protection.get();
    if let Some(timing) = protected {
        let prefix = pipes.pipe(pipe).slot(slot.wrapping_sub(1) & 3);
        if batch != BatchPosition::Only
            || prefix.frame.get() != Some(context)
            || prefix.prefix.get() != Some(timing.kind)
            || prefix.expected_status.get() != timing.kind.expected_status()
        {
            return Err(ProbeBuildError::PipeSlotOwnershipMismatch);
        }
    }
    let runtime = &mut engine.probe;
    if !runtime
        .backend
        .register_publication(batch, context, pipe, slot)
    {
        return Err(ProbeBuildError::InvalidContextPointer);
    }
    if protected.is_some() {
        let prefix_index = usize::from(pipe * 4 + (slot.wrapping_sub(1) & 3));
        runtime.backend.retry[prefix_index].reset();
        execute_protected_publication(&mut Mmio, pipes, pipe, slot, context);
    } else {
        execute_single_probe_publication(
            &mut Mmio,
            pipes,
            SingleProbePublicationInput {
                pipe,
                slot,
                context,
                batch,
            },
        );
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SingleProbePublicationInput {
    pub pipe: u8,
    pub slot: u8,
    pub context: ContextId,
    /// Batch position. `txp_scheduler_run` sets `current` to the producer once,
    /// then loops slot state and ring duration from there to `last`, and only
    /// afterwards arms the pipe and writes GO. Staging a second frame must
    /// therefore leave `current` alone (it tracks hardware progress) and must
    /// not arm; the final frame of a batch arms once for all of them.
    pub batch: BatchPosition,
}

/// Where a staged frame sits in a pipe batch. `Only` reproduces the exact
/// single-frame sequence, so a depth-1 build is unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BatchPosition {
    Only,
    First,
    Middle,
    Last,
}

impl BatchPosition {
    /// Vendor writes `current = producer` once, before the publish loop.
    pub const fn sets_current(self) -> bool {
        matches!(self, Self::Only | Self::First)
    }
}

/// A reserved slot must hold exactly the frame being published, with the
/// frame's response class.
pub(crate) fn single_frame_slot_matches(slot: &Slot, context: ContextId) -> bool {
    slot.prefix.get().is_none()
        && slot.frame.get() == Some(context)
        && slot.expected_status.get() == context.get().frame_kind.get()
}

/// Final publication sequence for one frame. The caller must validate
/// software ownership before entering this function; after
/// `PIPE_IRQ_TRIGGER` is written, no recoverable error is permitted.
pub(crate) fn execute_single_probe_publication<M: Hw>(
    mmio: &mut M,
    pipes: &Pipes,
    input: SingleProbePublicationInput,
) -> u8 {
    if input.pipe >= 4 || input.slot >= 4 {
        return u8::MAX;
    }
    let pipe_index = input.pipe;
    let slot_index = input.slot;
    let pipe = pipes.pipe(pipe_index);
    let slot = pipe.slot(slot_index);
    let Some(ring) = pipe.hardware_ring(pipe_index) else {
        return u8::MAX;
    };
    let descriptor = TxDescriptorAddress::new(packet_ram::tx_command(
        usize::from(pipe_index),
        usize::from(slot_index),
    ) as u32);
    let context = input.context.get();

    // `current` tracks hardware progress through the batch, so only the first
    // staged frame sets it (vendor: once, before the publish loop). The
    // first-submission branch of `txp_scheduler_run` does not advance the
    // slot; startup already synchronized the ring and software cursors.
    if input.batch.sets_current() {
        pipe.current.set(slot_index);
    }

    let timing = SingleFramePasTiming {
        payload_extended: context.payload_extended.get(),
        payload_base: context.payload_base.get(),
        ack: context.ack_duration.get(),
        total_airtime: context.total_airtime.get(),
        frame_kind: context.frame_kind.get(),
    };
    let (response, rate) = if let Some(kind) = slot.prefix.get() {
        let Some(prefix) = context
            .protection
            .get()
            .filter(|prefix| prefix.kind == kind)
        else {
            crate::halt_always!();
        };
        slot.duration.set(prefix.slot_duration());
        (kind.expected_status(), prefix.rate)
    } else {
        slot.duration.set(single_frame_slot_duration(timing));
        (timing.frame_kind, context.tx_rate.get())
    };
    build_frame_duration(mmio, descriptor.raw(), context, response != 0xff, rate);
    if response != 0xff {
        let flags = mmio.read(descriptor.flags()) | u32::from(response).wrapping_add(0x80);
        mmio.write(descriptor.flags(), flags);
    }
    pipe.last.set(slot_index);
    mmio.write(ring.go(), 0);

    let low_mac = crate::mac::low_mac();
    let pas = low_mac.pas(context.interface.get());
    let edca_slot_timing = pas.packed_aifs.get();
    if low_mac.edca_slot_timing.get() != edca_slot_timing {
        mmio.write(
            crate::platform::mac_register(0x0e64) as u32,
            edca_slot_timing,
        );
        low_mac.edca_slot_timing.set(edca_slot_timing);
    }

    let queue = usize::from(QUEUE_TO_ACCESS_CATEGORY[usize::from(pipe_index)]);
    let mut quantum = u32::from(pas.txop_limit[queue].get());
    let airtime = context.total_airtime.get() & 0xffff;
    if quantum == 0 {
        if (context.flags.get() & 0x0fff) >> 10 != 0 {
            quantum = airtime;
        }
    } else if quantum <= airtime {
        context
            .auxiliary_state
            .set(context.auxiliary_state.get() | 8);
        quantum = airtime;
    }
    mmio.write(
        quantum_register(pipe_index),
        quantum.wrapping_add(0x1f) >> 5,
    );

    if !matches!(input.batch, BatchPosition::Only) {
        // A staged batch remains entirely software-owned until
        // `finalize_staged_pipe`: no trigger, published slot state, duration
        // FIFO write, arm, or GO is allowed per descriptor.
        return 0;
    }

    mmio.write(PIPE_IRQ_TRIGGER, (1_u32 << pipe_index) << 25);
    pipes.active_tx.set(pipes.active_tx.get().wrapping_add(1));
    // Vendor's publish loop body: mark the slot published and push its duration
    // into the ring, once per slot from producer to `last`.
    slot.state.set(1);
    mmio.write(ring.duration_fifo(), slot.duration.get());
    pipe.state.set(1);
    pipe.control.set(pipe.control.get() | 1);
    pipe.watchdog.set(PIPE_WATCHDOG_RELOAD);
    pipe.progressed();
    mmio.write(ring.go(), 1);
    0
}

/// Publish every slot staged by a non-aggregate scheduler batch and arm the
/// pipe once. Descriptor construction and all fallible ownership checks must
/// complete before this boundary.
pub(crate) fn finalize_staged_pipe<M: Hw>(
    mmio: &mut M,
    pipes: &Pipes,
    pipe_index: u8,
    first_slot: u8,
    last_slot: u8,
) -> bool {
    if pipe_index >= 4 || first_slot >= 4 || last_slot >= 4 {
        return false;
    }
    let pipe = pipes.pipe(pipe_index);
    let Some(ring) = pipe.hardware_ring(pipe_index) else {
        return false;
    };
    mmio.write(PIPE_IRQ_TRIGGER, (1_u32 << pipe_index) << 25);

    let mut slot_index = first_slot;
    loop {
        let slot = pipe.slot(slot_index);
        pipes.active_tx.set(pipes.active_tx.get().wrapping_add(1));
        slot.state.set(1);
        mmio.write(ring.duration_fifo(), slot.duration.get());
        if slot_index == last_slot {
            break;
        }
        slot_index = slot_index.wrapping_add(1) & 3;
    }

    pipe.state.set(1);
    pipe.control.set(pipe.control.get() | 1);
    pipe.watchdog.set(PIPE_WATCHDOG_RELOAD);
    pipe.progressed();
    mmio.write(ring.go(), 1);
    true
}

/// Cross the shared MAC trigger boundary for a fully staged class-0 batch.
/// Every slot from `first_slot` through `last_slot` must be reserved by the
/// caller in this pipe.
#[cfg(target_arch = "arm")]
pub fn finalize_staged_host_class0_pipe(pipe: u8, first_slot: u8, last_slot: u8) {
    if !finalize_staged_pipe(&mut Mmio, pipes(), pipe, first_slot, last_slot) {
        crate::halt_always!();
    }
}
