//! Frame completion: the completion ring, context return and the per-pipe
//! publication and retry bookkeeping (`SingleProbeMacBackend`).

use crate::global::Global;
use core::cell::Cell;

use super::*;

pub(crate) const COMPLETION_RING_CAPACITY: usize = 64;

/// Frames the MAC finished, in completion order (`0xcfb8` enqueue, `0xd1fc`
/// drain).
pub(crate) struct CompletionRing {
    consumer: Cell<u8>,
    producer: Cell<u8>,
    entries: [Cell<Option<ContextId>>; COMPLETION_RING_CAPACITY],
}

pub(crate) static COMPLETION_RING: Global<CompletionRing> = Global::new(CompletionRing {
    consumer: Cell::new(0),
    producer: Cell::new(0),
    entries: [const { Cell::new(None) }; COMPLETION_RING_CAPACITY],
});

impl CompletionRing {
    pub(crate) fn enqueue(&self, context: ContextId) {
        let producer = self.producer.get();
        self.entries[usize::from(producer)].set(Some(context));
        self.producer.set((producer + 1) & 0x3f);
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.consumer.get() == self.producer.get()
    }

    /// The vendor drain snapshots the producer once and drains exactly that
    /// prefix; entries enqueued meanwhile wait for the next pass.
    pub(crate) fn drain(&self) -> impl Iterator<Item = ContextId> + '_ {
        let target = self.producer.get();
        core::iter::from_fn(move || {
            let consumer = self.consumer.get();
            if consumer == target {
                return None;
            }
            self.consumer.set((consumer + 1) & 0x3f);
            self.entries[usize::from(consumer)].take()
        })
    }
}

/// A class-0 completion tied to the exact published MAC slot that owned it.
///
/// Context identity alone is insufficient in a batch: the completion drain may
/// return several contexts while another slot in the same pipe remains live.
/// The publication registry records the pipe/slot identity before GO, and the
/// completion drain consumes it once the frame has been completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostClass0Completion {
    pub context: ContextId,
    pub pipe: u8,
    pub slot: u8,
    pub status: u16,
    pub tx_rate: u8,
    pub ack_failures: u8,
}

/// Capacity for all 30 host contexts completing in a single drain pass
/// (e.g. multiple A-MPDUs plus single-frame batches), rounded to power of 2.
pub(crate) const HOST_CLASS0_COMPLETION_CAPACITY: usize = 32;

pub(crate) struct BoundedCompletionQueue<T, const N: usize> {
    pub(crate) entries: [Option<T>; N],
    head: usize,
    tail: usize,
    len: usize,
}

impl<T, const N: usize> BoundedCompletionQueue<T, N> {
    pub(crate) const fn new() -> Self {
        Self {
            entries: [const { None }; N],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    pub(crate) fn push(&mut self, value: T) -> Result<(), T> {
        if self.len == N {
            return Err(value);
        }
        self.entries[self.tail] = Some(value);
        self.tail += 1;
        if self.tail == N {
            self.tail = 0;
        }
        self.len += 1;
        Ok(())
    }

    pub(crate) fn take(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let value = self.entries[self.head].take();
        self.head += 1;
        if self.head == N {
            self.head = 0;
        }
        self.len -= 1;
        value
    }
}

#[cfg(target_arch = "arm")]
pub struct SingleProbeMacBackend {
    pub(crate) retry: [BoundedSingleTxRetry; 16],
    pub(crate) mismatch: [u8; 4],
    pub(crate) publications: [Option<PublishedSlotIdentity>; 16],
    /// Completed host frames, in order.
    pub(crate) completed:
        BoundedCompletionQueue<HostClass0Completion, HOST_CLASS0_COMPLETION_CAPACITY>,
    /// The firmware's own frame completed (a probe request, or one from the
    /// management publisher): one is in flight at a time, and it is its
    /// publisher's, whichever runtime serviced the MAC.
    pub(crate) internal: Option<HostClass0Completion>,
    /// The host's rate policies, which drive retry decisions.
    pub rate_policies: crate::rate_policy::RatePolicies,
    /// MAC events and completions serviced, wrapping.
    pub(crate) serviced: u32,
}

#[cfg(target_arch = "arm")]
impl SingleProbeMacBackend {
    pub const fn new() -> Self {
        Self {
            retry: [BoundedSingleTxRetry::new(); 16],
            mismatch: [0; 4],
            publications: [None; 16],
            completed: BoundedCompletionQueue::new(),
            internal: None,
            rate_policies: crate::rate_policy::RatePolicies::new(),
            serviced: 0,
        }
    }

    pub(crate) fn register_publication(
        &mut self,
        batch: BatchPosition,
        context: ContextId,
        pipe: u8,
        slot: u8,
    ) -> bool {
        let Some(index) = pipe_slot_state_index(pipe, slot) else {
            return false;
        };
        let pipe_index = usize::from(pipe);
        let occupied =
            core::array::from_fn(|slot| self.publications[pipe_index * 4 + slot].is_some());
        if !publication_registration_allowed(occupied, batch, slot) {
            return false;
        }
        self.retry[index].reset();
        if matches!(batch, BatchPosition::Only | BatchPosition::First) {
            self.mismatch[pipe_index] = 0;
        }
        self.publications[index] = Some(PublishedSlotIdentity {
            context,
            pipe,
            slot,
        });
        true
    }

    pub(crate) fn push_completion(&mut self, completion: HostClass0Completion) {
        if completion.context.host_index().is_none() {
            if self.internal.replace(completion).is_some() {
                terminal_probe_backend_fault(completion.pipe)
            }
        } else if let Err(completion) = self.completed.push(completion) {
            terminal_probe_backend_fault(completion.pipe)
        }
    }

    /// A completed host frame.
    pub fn take_completion(&mut self) -> Option<HostClass0Completion> {
        self.completed.take()
    }

    /// The firmware's own frame, completed.
    pub fn take_internal_completion(&mut self) -> Option<HostClass0Completion> {
        self.internal.take()
    }

    /// Redraw the backoff of the descriptor at `descriptor` for its frame.
    pub(crate) fn program_random_backoff(&mut self, descriptor: u32, context: Option<ContextId>) {
        let Some(context) = context.map(ContextId::get) else {
            return;
        };
        let mut mmio = Mmio;
        let mask = contention_window(context);
        let random = (next_retry_random24() as u16) & mask;
        let current = mmio.read(descriptor + 4);
        mmio.write(
            descriptor + 4,
            (u32::from(random) & 0x0fff) * 0x400 | (current & 0x01ff),
        );
    }

    /// Retry decision for the current slot. Host frames follow their WSM rate
    /// policy; internal frames use the bounded single-frame policy.
    pub(crate) fn decide_retry(
        &mut self,
        pipe: u8,
        slot: u8,
        id: ContextId,
    ) -> SingleTxRetryDecision {
        let Some(retry_index) = pipe_slot_state_index(pipe, slot) else {
            terminal_probe_backend_fault(pipe)
        };
        let context = id.get();
        let rate = context.tx_rate.get();
        let (true, Some(rate_try)) = (id.is_host(), context.rate_try.get(usize::from(rate >> 3)))
        else {
            return self.retry[retry_index].decide();
        };
        let shift = u32::from((rate & 7) * 4);
        let attempts = (rate_try.get() >> shift) & 0x0f;
        if attempts < 0x0f {
            rate_try.set((rate_try.get() & !(0x0f << shift)) | ((attempts + 1) << shift));
        }

        let Some(policy) = self.rate_policies.get(context.retry_policy.get()) else {
            return self.retry[retry_index].decide();
        };
        let try_count = context.try_count.get();
        let flags = context.flags.get();
        match resolve_head_retry_step(
            policy,
            rate,
            try_count,
            (flags & 0x7ff) >> 9 != 0,
            context.interface.get(),
            context.access_category.get(),
        ) {
            crate::rate_policy::RetryStep::GiveUp => SingleTxRetryDecision::GiveUp,
            crate::rate_policy::RetryStep::Rearm { rate: next_rate } => {
                let rate_changed = next_rate != rate && (flags & 0x20 == 0 || next_rate > 13);
                let first_retry = flags & 0x10 == 0;
                if first_retry || rate_changed {
                    if rate_changed {
                        context.tx_rate.set(next_rate);
                    }
                    context.flags.set(
                        flags
                            | 0x10
                            | 0x0008_0000
                            | if rate_changed && rate > 3 && next_rate < 4 {
                                0x0004_0000
                            } else {
                                0
                            },
                    );
                }
                context.try_count.set(try_count.wrapping_add(1));
                self.retry[retry_index].record_rearm();
                SingleTxRetryDecision::Rearm
            }
        }
    }

    /// Final context completion: run the class callback, return internal
    /// contexts to their pool and queue the completion for its owner.
    pub(crate) fn complete_context(&mut self, context: ContextId, status: u16) {
        let tx_rate = context.get().tx_rate.get();
        if !dispatch_completed_context(context, status) {
            terminal_probe_backend_fault(0);
        }
        let aggregate_slot = context.get().aggregate_slot.replace(NO_AGGREGATE);
        let found = if aggregate_slot != NO_AGGREGATE {
            let index = usize::from(aggregate_slot);
            self.publications
                .get(index)
                .copied()
                .flatten()
                .filter(|publication| publication.context == context)
                .map(|publication| (index, publication))
        } else {
            self.publications
                .iter()
                .enumerate()
                .find_map(|(index, publication)| {
                    publication
                        .filter(|publication| publication.context == context)
                        .map(|publication| (index, publication))
                })
        };
        let Some((index, publication)) = found else {
            // Aggregate members after the first share the first's slot and
            // publication.
            if aggregate_slot == NO_AGGREGATE {
                terminal_probe_backend_fault(0)
            }
            let (pipe, slot) = (aggregate_slot / 4, aggregate_slot % 4);
            self.push_completion(HostClass0Completion {
                context,
                pipe,
                slot,
                status,
                tx_rate,
                ack_failures: context.get().try_count.get().min(u16::from(u8::MAX)) as u8,
            });
            return;
        };
        self.publications[index] = None;
        let ack_failures = if context.is_host() {
            context.get().try_count.get().min(u16::from(u8::MAX)) as u8
        } else {
            self.retry[index].attempts()
        };
        self.push_completion(HostClass0Completion {
            context,
            pipe: publication.pipe,
            slot: publication.slot,
            status,
            tx_rate,
            ack_failures,
        });
    }
}

/// One bounded cooperative MAC pass: drain the event FIFO while the MAC needs
/// service, drain the completion ring, and hand back one completion.
///
/// Vendor dispatcher `0x9e90` is entered through FIQ source 0x16. This
/// firmware keeps FIQ masked and drains the MAC event FIFO whenever it is
/// non-empty.
/// Service MAC events and queued completions for either runtime.
#[cfg(target_arch = "arm")]
fn service_mac_runtime(backend: &mut SingleProbeMacBackend, max_events: u32) {
    let readiness = read_u32(MAC_EVENT_READINESS as usize);
    if readiness as i32 >= 0 {
        service_mac_event_fifo(backend, max_events);
    }
    service_completion_drain(backend);
}

/// Service the MAC for the firmware's own frame and hand back its
/// completion. Host completions stay queued for the host runtime.
#[cfg(target_arch = "arm")]
pub(crate) fn service_single_probe_runtime(
    backend: &mut SingleProbeMacBackend,
    max_events: u32,
) -> Option<HostClass0Completion> {
    service_mac_runtime(backend, max_events);
    backend.take_internal_completion()
}

/// Cooperatively service MAC events and class-0 completion for the ordinary
/// host runtime.
#[cfg(target_arch = "arm")]
pub fn service_host_class0_runtime(
    engine: &mut TxEngine,
    max_events: u32,
) -> Option<HostClass0Completion> {
    service_mac_runtime(&mut engine.probe.backend, max_events);
    engine.probe.backend.take_completion()
}

/// Drain completions queued since the last MAC service (aggregate members
/// resolved against a BlockAck), and hand back one.
#[cfg(target_arch = "arm")]
pub fn drain_host_class0_completions(engine: &mut TxEngine) -> Option<HostClass0Completion> {
    service_completion_drain(&mut engine.probe.backend);
    engine.probe.backend.take_completion()
}

/// Drain another completion produced by the most recent class-0 MAC service.
/// A single completion-ring pass may retire every slot in a batch.
#[cfg(target_arch = "arm")]
pub fn take_host_class0_completion(engine: &mut TxEngine) -> Option<HostClass0Completion> {
    engine.probe.backend.take_completion()
}

/// `txp_fn_2441` (`0x91ec`) for a non-aggregate slot: stamp the frame with
/// its final status and queue it for completion.
#[cfg(target_arch = "arm")]
pub(crate) fn complete_tx_pipe_slot(slot: LiveTxSlot, status: u16) {
    if is_aggregate_slot(slot.pipe, slot.slot) {
        return complete_aggregate_slot(slot, status);
    }
    let pipes = pipes();
    let Some(id) = retire_command_slot(pipes, slot.pipe, slot.slot) else {
        return;
    };
    let context = id.get();
    let class_bits = ((context.flags.get() >> 18) & 0x0c) as u16;
    context
        .auxiliary_state
        .set(context.auxiliary_state.get() | class_bits);
    context.terminal_status.set(status);
    COMPLETION_RING.enqueue(id);
}

/// Completion drain (`tx_complete_tala_adapt`, `0xd254`). Every frame in the
/// snapshotted prefix is fully processed before this function returns. The
/// vendor TALA A-MPDU length adaptation that shares this routine is not
/// ported: nothing aggregates.
#[cfg(target_arch = "arm")]
pub(crate) fn service_completion_drain(backend: &mut SingleProbeMacBackend) {
    let contexts = contexts();
    for id in COMPLETION_RING.drain() {
        let context = id.get();
        context.completion_flags.set(context.auxiliary_state.get());
        contexts
            .active_pas
            .set(contexts.active_pas.get().wrapping_sub(1));
        backend.serviced = backend.serviced.wrapping_add(1);
        backend.complete_context(id, context.terminal_status.get());
    }
}

/// Callback wrapper (`0xb6f4`) for one completed context. Class 0 (host data)
/// keeps its context until the host confirmation; class 6 (internal frames)
/// returns it to the pool here. Returns false for a context that was already
/// returned.
#[cfg(target_arch = "arm")]
pub(crate) fn dispatch_completed_context(id: ContextId, completion_status: u16) -> bool {
    let context = id.get();
    if context.is_free() {
        return false;
    }
    context.saved_status.set(context.try_count.get() as u8);
    context.terminal_status.set(completion_status);
    match context.class.get() {
        CLASS_HOST => return true,
        CLASS_INTERNAL => {}
        _ => terminal_probe_backend_fault(0),
    }

    // Vendor low-rate recovery: successes at or below the VIF retry limit
    // count up the VIF's recovery byte, retries count it down.
    let interface = context.interface.get();
    if let (true, Some(vif)) = (interface < 2, crate::vif::vif(interface)) {
        let alternate = vif.rate_bytes[6].get() != 0;
        let retry_limit = vif.default_rates[usize::from(alternate)].get();
        if retry_limit != 0xff && context.tx_rate.get() <= retry_limit {
            let retry_count = &vif.rate_bytes[7];
            let mut retries = retry_count.get();
            if completion_status == 0 && retries < 5 {
                retries += 1;
                retry_count.set(retries);
            }
            let consumed = context.saved_status.get();
            if consumed != 0 {
                retry_count.set(retries.saturating_sub(consumed));
            }
        }
    }

    contexts().free(id);
    true
}
