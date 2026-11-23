#[allow(unused_imports)] // host builds compile only the hardware-free half
use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SingleTxRetryDecision {
    /// Rebuild the descriptor, re-arm the MAC and keep slot/context ownership.
    Rearm,
    /// Complete the current frame with vendor status `0x0b`.
    GiveUp,
}

/// Re-arms after the original transmission before a frame gives up.
pub(crate) const SINGLE_TX_MAX_RETRIES: u8 = 2;

/// Replacement for vendor TALA/retry policy for internal frames.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundedSingleTxRetry {
    pub(crate) attempts: u8,
}

impl Default for BoundedSingleTxRetry {
    fn default() -> Self {
        Self::new()
    }
}

impl BoundedSingleTxRetry {
    pub const fn new() -> Self {
        Self { attempts: 0 }
    }

    pub const fn attempts(self) -> u8 {
        self.attempts
    }

    pub fn decide(&mut self) -> SingleTxRetryDecision {
        self.decide_with_limit(SINGLE_TX_MAX_RETRIES)
    }

    pub(crate) fn decide_with_limit(&mut self, limit: u8) -> SingleTxRetryDecision {
        if self.attempts >= limit {
            SingleTxRetryDecision::GiveUp
        } else {
            self.record_rearm();
            SingleTxRetryDecision::Rearm
        }
    }

    pub(crate) fn record_rearm(&mut self) {
        self.attempts = self.attempts.saturating_add(1);
    }

    pub fn reset(&mut self) {
        self.attempts = 0;
    }
}

/// One head growth for a physical retry event.
///
/// Scope is the host mode-0 path: internal frames run vendor policy
/// `0xf`/mode-2 rules that are deliberately not implemented here, so they leave
/// the bank alone. The bank is the one the random backoff is actually drawn
/// from in `program_random_backoff` - the head frame's interface and access
/// category. Out-of-range selections are rejected without a write; this site
/// is past the destructive MAC event read and must never halt or spin.
#[cfg(target_arch = "arm")]
pub(crate) fn grow_head_backoff_for_retry_event(id: ContextId) {
    if !id.is_host() {
        return;
    }
    let context = id.get();
    let _ = crate::backoff::grow(
        crate::backoff::banks(),
        context.interface.get(),
        context.access_category.get(),
    );
}

/// Head retry-policy step, with the vendor exhaustion reset at `0x8a20`.
pub(crate) fn resolve_head_retry_step(
    policy: crate::rate_policy::TxRatePolicy,
    rate: u8,
    try_count: u16,
    long_frame: bool,
    interface: u8,
    queue: u8,
) -> crate::rate_policy::RetryStep {
    let step = crate::rate_policy::retry_step(policy, rate, try_count, long_frame);
    if step == crate::rate_policy::RetryStep::GiveUp {
        let _ = crate::backoff::reset(crate::backoff::banks(), interface, queue);
    }
    step
}

/// TX-success marker reset gate (`0x9d06..0x9d2c`): active pipe, slot retry
/// rate `0xff` and PAS bit 15 clear.
pub(crate) const fn success_marker_resets_backoff(
    pipe_state: u8,
    retry_rate: u8,
    control_bits: u32,
) -> bool {
    pipe_state == 1 && retry_rate == 0xff && control_bits & (1 << 15) == 0
}

/// Re-arm one non-aggregate frame after a retry event (`0x97de..0x9a2e`):
/// rebuild the rate descriptor when the rate changed, redraw the backoff,
/// re-trigger the pipe and release the command-mask bit.
///
/// # Safety
/// The slot must be the live, retry-owned current slot of `live.pipe`.
#[cfg(target_arch = "arm")]
pub(crate) fn rearm_single_frame(live: LiveTxSlot, pending_mask: u32) {
    let mmio = &mut Mmio;
    let pipe_index = live.pipe;
    let pipe = pipes().pipe(pipe_index);
    let Some(ring) = pipe.hardware_ring(pipe_index) else {
        terminal_probe_backend_fault(pipe_index);
    };
    let context = live.context.get();

    let flags = context.flags.get();
    if flags & 0x0008_0000 != 0 {
        if is_aggregate_slot(live.pipe, live.slot) {
            // An aggregate is retried whole at its rate; rebuilding would
            // replace it with its first member.
            context.flags.set(flags & !0x000c_0000);
        } else {
            rebuild_rate_descriptor(live);
        }
    }

    let descriptor = TxDescriptorAddress::new(live.command());
    let status = pipe.slot(live.slot).expected_status.get();
    build_single_frame_duration(mmio, descriptor.raw(), context, true);
    let descriptor_flags = mmio.read(descriptor.flags()) | u32::from(status).wrapping_add(0x80);
    mmio.write(descriptor.flags(), descriptor_flags);
    crate::cpu::drain_write_buffer();

    mmio.write(PIPE_IRQ_TRIGGER, (1_u32 << pipe_index) << 25);

    let current = pipe.current.get() & 3;
    if pipe.producer.get() & 3 != current {
        // Vendor's multi-slot branch first checks a mapped companion slot. For
        // an ordinary batch that slot has a different PAS; with bit 15 clear it
        // takes the simple fallback: clear only the current command-mask bit
        // and acknowledge, without forcing producer to follow the hardware
        // cursor.
        if flags & (1 << 15) != 0 {
            terminal_probe_backend_fault(pipe_index);
        }
    } else if context.frame_kind.get() != 0xff {
        // The vendor repeats descriptor-flag publication for a frame expecting
        // a response before clearing the command ownership bit.
        let descriptor_flags = mmio.read(descriptor.flags()) | u32::from(status).wrapping_add(0x80);
        mmio.write(descriptor.flags(), descriptor_flags);
        crate::cpu::drain_write_buffer();
    }
    let command_mask = mmio.read(ring.cursor_and_pending_mask());
    mmio.write(
        ring.cursor_and_pending_mask(),
        command_mask & !(1_u32 << current),
    );
    mmio.write(
        PIPE_IRQ_PENDING,
        0_u32.wrapping_sub(pending_mask.wrapping_add(1)),
    );
}

/// Rebuild the descriptor of a host frame whose retry changed its rate.
#[cfg(target_arch = "arm")]
pub(crate) fn rebuild_rate_descriptor(live: LiveTxSlot) {
    let context = live.context.get();
    if !live.context.is_host()
        || prepare_single_frame_pas_timing(context).is_err()
        || emit_descriptor(context, live.command() + 0x0c).is_err()
    {
        terminal_probe_backend_fault(live.pipe);
    }
    context.flags.set(context.flags.get() & !0x000c_0000);
}

/// Rebuild the failed exchange's commands without publishing new FIFO entries.
/// Prefix failures do not mark the data as retransmitted or consume its rate tries.
#[cfg(target_arch = "arm")]
fn rearm_protected_pair(live: LiveTxSlot, pending_mask: u32) -> bool {
    let pipes = pipes();
    let pipe = pipes.pipe(live.pipe);
    let data_slot = pipe.last.get();
    let prefix_slot = data_slot.wrapping_sub(1) & 3;
    let context = live.context.get();
    if pipe.slot(data_slot).frame.get() != Some(live.context) {
        terminal_probe_backend_fault(live.pipe);
    }
    if live.slot == data_slot {
        context.flags.set(context.flags.get() | 0x10);
    }
    if prepare_single_frame_pas_timing(context).is_err() {
        return false;
    }
    let replay_prefix = context
        .protection
        .get()
        .is_some_and(|timing| timing.kind == Protection::Rts || context.tx_rate.get() > 3);
    if !replay_prefix {
        context.flags.set(context.flags.get() & !(1 << 15));
    }
    if live.slot == data_slot && emit_descriptor(context, live.command() + 0x0c).is_err() {
        return false;
    }
    if emit_protection_descriptor(
        context,
        packet_ram::tx_command(usize::from(live.pipe), usize::from(prefix_slot)) as u32,
    )
    .is_err()
    {
        return false;
    }
    context.flags.set(context.flags.get() & !0x000c_0000);
    execute_protected_retry(
        &mut Mmio,
        pipes,
        live.pipe,
        live.context,
        pending_mask,
        replay_prefix,
    );
    true
}

#[cfg(target_arch = "arm")]
fn give_up_protected_pair(live: LiveTxSlot, pending_mask: u32) {
    let pipes = pipes();
    let pipe = pipes.pipe(live.pipe);
    let Some(ring) = pipe.hardware_ring(live.pipe) else {
        terminal_probe_backend_fault(live.pipe)
    };
    Mmio.write(PIPE_IRQ_TRIGGER, (1_u32 << live.pipe) << 25);
    for slot in unfinished_slots(pipe.current.get(), pipe.last.get()) {
        if let Some(live) = LiveTxSlot::new(pipes, live.pipe, slot) {
            complete_tx_pipe_slot(live, 0x0b);
        }
        Mmio.write(ring.completion_word(), 1);
    }
    pipe.current.set(pipe.last.get());
    pipe.apply_cursor(OrdinaryTxPipeCursorPlan::Recycle {
        next_head: (pipe.last.get() + 1) & 3,
    });
    pipes.busy.set(false);
    Mmio.write(
        PIPE_IRQ_PENDING,
        0_u32.wrapping_sub(pending_mask.wrapping_add(0x10)),
    );
}

/// `txp_pipe_tx_done_retry` (`0x9550`) for the current slot of the latched
/// pipe. `owned_mask` is the pipe's retry bit from the scheduler word captured
/// before bit-23 service.
///
/// # Safety
/// Pipe, slot, command, and frame-node ownership must match the saved word.
#[cfg(target_arch = "arm")]
pub(crate) fn service_tx_retry(backend: &mut SingleProbeMacBackend, owned_mask: u32) {
    let mmio = &mut Mmio;
    let pipes = pipes();
    let pipe_index = pipes.latched.get() & 3;
    let pipe = pipes.pipe(pipe_index);
    pipes.publish_current(pipe_index);
    let current = pipe.current.get();
    let slot = pipe.slot(current);

    if slot.state.get() != 3 {
        return;
    }
    let Some(ring) = pipe.hardware_ring(pipe_index) else {
        terminal_probe_backend_fault(pipe_index);
    };
    // Vendor ownership transition: started -> retry-owned, then globally busy.
    slot.state.set(4);
    pipes.busy.set(true);

    if pipe.state.get() != 1 {
        pipes.busy.set(false);
        mmio.write(ring.inactive_sentinel(), PIPE_RETRY_INACTIVE_SENTINEL);
        mmio.write(
            PIPE_IRQ_PENDING,
            0_u32.wrapping_sub(owned_mask.wrapping_add(1)),
        );
        return;
    }

    if slot.expected_status.get() == 6 && slot.prefix.get() != Some(Protection::Rts) {
        terminal_probe_backend_fault(pipe_index);
    }
    if (pipe.watchdog.get() as i8) < 1 {
        pipe.watchdog.set(1);
    }

    // Vendor `0x967e`: exactly one contention-window growth per physical
    // retry event, on the head interface/AC, before the retry decision.
    let Some(live) = LiveTxSlot::new(pipes, pipe_index, current) else {
        terminal_probe_backend_fault(pipe_index);
    };
    grow_head_backoff_for_retry_event(live.context);

    let protected = live.context.get().protection.get().is_some();
    if slot.prefix.get() == Some(Protection::Rts) {
        crate::counters::bump(crate::counters::Counter::RtsFailures);
    }
    let decision = if slot.prefix.get().is_some() {
        let index = pipe_slot_state_index(pipe_index, current).unwrap();
        let limit = backend
            .rate_policies
            .get(live.context.get().retry_policy.get())
            .map_or(SINGLE_TX_MAX_RETRIES, |policy| policy.long_retries());
        let decision = backend.retry[index].decide_with_limit(limit);
        if decision == SingleTxRetryDecision::GiveUp {
            let _ = crate::backoff::reset(
                crate::backoff::banks(),
                live.context.get().interface.get(),
                live.context.get().access_category.get(),
            );
        }
        decision
    } else if is_aggregate_slot(pipe_index, current) {
        // An aggregate slot cannot be rebuilt at a stepped-down rate in place,
        // and mutating only member 0's tx_rate/try_count desyncs it from the
        // rest of the A-MPDU. Complete the slot as failed so `aggregate::resolve`
        // requeues every member uniformly for one more aggregate attempt and
        // then single-frame rate-policy fallback.
        SingleTxRetryDecision::GiveUp
    } else {
        backend.decide_retry(pipe_index, current, live.context)
    };
    if protected {
        if decision == SingleTxRetryDecision::Rearm && rearm_protected_pair(live, owned_mask) {
            return;
        }
        give_up_protected_pair(live, owned_mask);
        return;
    }
    match decision {
        SingleTxRetryDecision::Rearm => rearm_single_frame(live, owned_mask),
        SingleTxRetryDecision::GiveUp => {
            // The trigger precedes completion and the command completion word
            // follows `txp_fn_2441`.
            mmio.write(PIPE_IRQ_TRIGGER, (1_u32 << pipe_index) << 25);
            let last = pipe.last.get();
            complete_tx_pipe_slot(live, 0x0b);
            mmio.write(ring.completion_word(), 1);

            let next = current.wrapping_add(1) & 3;
            pipe.current.set(next);
            pipe.producer.set(next);
            if current == last {
                pipe.disarm();
            }
            pipes.busy.set(false);
            mmio.write(
                PIPE_IRQ_PENDING,
                0_u32.wrapping_sub(owned_mask.wrapping_add(0x10)),
            );
        }
    }
}
