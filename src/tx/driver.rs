//! Concurrent retained host-TX ownership with a pipe-indexed class-0 MAC executor.
//!
//! Each of the 30 host contexts owns its original HIF request until the inherited
//! confirmation handoff point. The current HIF transport then returns request
//! credit before it enqueues the separately copied confirmation; this module does
//! not redesign that parent ordering. Pending contexts advance independently.
//! Retained hardware owners are discovered independently per MAC pipe;
//! publication admits at most one new batch per pass and never targets a pipe
//! that still has a retained runtime owner. Already-scheduled owners advance
//! from MAC completion events, so cooperative service budget is reserved for
//! software contexts and reservations that can still make progress.
//!
//! Admission only classifies a frame and numbers it. [`HostTxDriver::seal`]
//! then encrypts admitted frames one at a time, in admission order, while
//! the AES engine works and the firmware goes on with other lanes; a frame
//! joins the pending list once sealed.

use crate::crypto::{Crypto, Job, Started};
use crate::tx::ampdu::MAX_MEMBERS;
use crate::{hif, tx};

/// Host TX contexts in the fixed vendor arena; hardware context addresses
/// define slot identity.
const HOST_CONTEXT_COUNT: usize = 30;
/// Contexts advanced per service pass.
///
/// At 4, reservations that still need publication consume the front of the
/// budget and the remainder advances software owners. Already-scheduled slots
/// are completion-driven and are deliberately skipped instead of consuming a
/// no-op service turn. Queued contexts can still wait many passes before
/// publication. Keep the bounded vendor-shaped service loop until hardware
/// concurrency is made ownership-safe.
const SERVICE_BUDGET: usize = 16;

mod states;

use states::{States, indices};

pub struct HostTxDriver {
    states: States,
    admission_orders: [u32; HOST_CONTEXT_COUNT],
    next_admission_order: u32,
    hardware_service_cursor: u8,
    next_confirmation_order: u32,
    confirm_queue: [u8; HOST_CONTEXT_COUNT],
    confirm_head: u8,
    confirm_len: u8,
    scheduler_phy_started_this_pass: bool,
    /// The context the AES engine encrypts.
    sealing: Option<(usize, Job)>,
}

enum HostTxState {
    Owned {
        retained: tx::RetainedHostTx,
        hardware: Option<HardwareOwner>,
    },
    Reserved {
        retained: tx::RetainedHostTx,
        reservation: tx::HostSchedulerReservation,
    },
    Confirming {
        owner: ConfirmationOwner,
        confirmation: HostTxConfirmation,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HardwareOwner {
    pipe: u8,
    slot: u8,
}

#[cfg(target_arch = "arm")]
struct ReservedBatchMember {
    index: usize,
    retained: tx::RetainedHostTx,
    reservation: tx::HostSchedulerReservation,
}

impl HardwareOwner {
    const fn single(pipe: u8, slot: u8) -> Self {
        Self { pipe, slot }
    }

    fn matches(self, owner: tx::ContextId, completion: tx::HostClass0Completion) -> bool {
        owner == completion.context && self.pipe == completion.pipe && self.slot == completion.slot
    }
}

enum ConfirmationOwner {
    Retained(tx::RetainedHostTx),
    Release(hif::RequestReleaseToken),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostTxConfirmation {
    pub packet_id: u32,
    pub context: tx::ContextId,
    pub status: u32,
    pub tx_rate: u8,
    pub ack_failures: u8,
    pub flags: u16,
    pub rate_try: [u32; 3],
}

impl HostTxDriver {
    /// Changes whenever a frame is admitted, sealed, scheduled, completed
    /// or confirmed.
    pub fn activity(&self, activity: crate::activity::Activity) -> crate::activity::Activity {
        activity
            .with(self.next_admission_order)
            .with(self.next_confirmation_order)
            .with(
                self.sealing
                    .as_ref()
                    .map_or(u32::MAX, |(index, _)| *index as u32),
            )
            .with(self.states.generation())
    }

    pub const fn new() -> Self {
        Self {
            states: States::new(),
            admission_orders: [0; HOST_CONTEXT_COUNT],
            next_admission_order: 0,
            hardware_service_cursor: 0,
            next_confirmation_order: 0,
            confirm_queue: [0; HOST_CONTEXT_COUNT],
            confirm_head: 0,
            confirm_len: 0,
            scheduler_phy_started_this_pass: false,
            sealing: None,
        }
    }

    /// Management and class-0 share one MAC executor. Every retained class-0
    /// owner blocks admission of a new management publication; an already
    /// active management owner is serviced separately until it completes.
    pub fn management_runtime_available(&self) -> bool {
        self.states.masks().occupied == 0
    }

    /// Only completed owners awaiting their one confirmation may remain.
    /// A hardware-idle test alone misses classified, sealing and sealed frames.
    pub fn key_change_drained(&self) -> bool {
        let masks = self.states.masks();
        self.sealing.is_none()
            && crate::key_change::owners_drained(masks.occupied, masks.confirming)
    }

    /// Admit and classify one ordinary host request; [`Self::seal`] takes it
    /// from there. The hardware context index selects the arena slot.
    pub fn admit(
        &mut self,
        buffer: hif::RequestBuffer,
        interface: u8,
        link: u8,
        transport: &mut hif::Transport,
        engine: &tx::TxEngine,
    ) -> bool {
        let mut retained = match tx::admit_host_tx(buffer, interface, link) {
            Ok(retained) => retained,
            Err((buffer, _)) => {
                transport.release_request(buffer.into_release());
                return false;
            }
        };
        let index = retained.context().index();
        if self.states[index].is_some() {
            let release = retained.abort();
            transport.release_request(release);
            return false;
        }
        match tx::classify(&mut retained) {
            Ok(Some(tid)) => {
                if let Some(link) = tx::block_ack_link(retained.context()) {
                    engine
                        .ba
                        .link(link)
                        .on_qos_data(tid, engine.block_ack_tx_tids);
                }
            }
            Ok(None) => {}
            Err(_) => {
                let release = retained.abort();
                transport.release_request(release);
                return false;
            }
        }
        self.admission_orders[index] = self.next_admission_order;
        self.next_admission_order = self.next_admission_order.wrapping_add(1);
        self.states[index] = Some(HostTxState::Owned {
            retained,
            hardware: None,
        });
        true
    }

    /// Encrypt admitted frames in admission order and queue them as pending:
    /// collect the frame the engine finished, start the next. Returns while
    /// the engine works on a frame or is busy with RX.
    pub fn seal(&mut self, crypto: &mut Crypto, ba: &tx::ba::PeerSessions) {
        loop {
            if let Some((index, job)) = self.sealing.take() {
                match crypto.poll(job) {
                    Err(job) => {
                        self.sealing = Some((index, job));
                        return;
                    }
                    Ok(outcome) => self.sealed(index, outcome.is_ok(), ba),
                }
            }
            let Some(index) = self.oldest_classified() else {
                return;
            };
            let Some(HostTxState::Owned {
                mut retained,
                hardware: None,
            }) = self.states[index].take()
            else {
                crate::halt_always!();
            };
            let started = tx::start_encryption(&mut retained, crypto);
            self.states[index] = Some(HostTxState::Owned {
                retained,
                hardware: None,
            });
            match started {
                Ok(Started::Plain) => self.sealed(index, true, ba),
                Ok(Started::Running(job)) => self.sealing = Some((index, job)),
                Ok(Started::Busy) => return,
                Err(_) => self.sealed(index, false, ba),
            }
        }
    }

    /// The admitted frame that has waited longest for encryption.
    fn oldest_classified(&self) -> Option<usize> {
        let classified = indices(self.states.masks().classified)
            .map(|index| (index, self.admission_orders[index]));
        tx::oldest_pending_context(classified, self.next_admission_order)
    }

    /// Queue a sealed frame as pending, or fail it back to the host.
    fn sealed(&mut self, index: usize, encrypted: bool, ba: &tx::ba::PeerSessions) {
        let Some(HostTxState::Owned {
            mut retained,
            hardware: None,
        }) = self.states[index].take()
        else {
            crate::halt_always!();
        };
        if encrypted
            && tx::finish_preparation(&mut retained).is_ok()
            && tx::enqueue_post_crypto(&mut retained).is_ok()
        {
            self.states[index] = Some(HostTxState::Owned {
                retained,
                hardware: None,
            });
        } else {
            Self::notify_given_up(ba, retained.context());
            self.enqueue_confirmation(index);
            self.states[index] = Some(Self::cancelled_confirmation(retained));
        }
    }

    /// Advance a bounded subset of contexts. A reserved/scheduled context is
    /// always serviced first; remaining budget visits pending contexts in
    /// admission order, at most once each per pass. Arena reuse is not FIFO.
    ///
    /// # Safety
    /// This driver must be the sole class-0 owner of pending/PAS/scheduler and
    /// MAC-event mutation.
    pub fn service(
        &mut self,
        engine: &mut tx::TxEngine,
        fifo: &mut impl tx::aggregate::BlockAckLookahead,
        allow_hardware_publication: bool,
    ) {
        let mut budget = SERVICE_BUDGET;
        self.scheduler_phy_started_this_pass = false;
        tx::aggregate::tick(fifo);

        // Service the shared MAC executor once, then route every completion
        // drained in that pass by its registered pipe/slot/frame-node owner.
        // A successful batch may enqueue all of its slots before returning.
        let Some(owners) = self.hardware_runtime_owners() else {
            crate::halt_always!();
        };
        if !owners.is_empty() {
            let mut completion = tx::service_host_class0_runtime(engine, 32);
            while let Some(completed) = completion {
                self.route_hardware_completion(completed, &engine.ba);
                completion = tx::take_host_class0_completion(engine);
            }
            // An aggregate the MAC just finished may have its BlockAck in the
            // RX FIFO already: settle its members before the next batch.
            tx::aggregate::look_for_block_acks(fifo);
            let mut completion = tx::drain_host_class0_completions(engine);
            while let Some(completed) = completion {
                self.route_hardware_completion(completed, &engine.ba);
                completion = tx::take_host_class0_completion(engine);
            }
        }

        let Some(owners) = self.hardware_runtime_owners() else {
            crate::halt_always!();
        };
        let mut eligible = owners.slot_mask();
        while budget != 0 {
            let Some((slot_index, index)) =
                owners.next_slot_owner(eligible, self.hardware_service_cursor)
            else {
                break;
            };
            eligible &= !(1_u16 << slot_index);
            self.hardware_service_cursor = slot_index.wrapping_add(1) & 15;
            // Scheduled contexts cannot advance here: their only transitions
            // are driven by the completion/requeue drain above. Do not let up
            // to four inert physical slots starve pending software contexts.
            if !matches!(self.states[index], Some(HostTxState::Reserved { .. })) {
                continue;
            }
            {
                self.service_index(index, engine, allow_hardware_publication);
            }
            budget -= 1;
        }

        let mut software_visited = 0_u32;
        while budget != 0 {
            let Some(index) = self.next_software_owner(software_visited) else {
                break;
            };
            software_visited |= 1 << index;
            {
                self.service_index(index, engine, false);
            }
            budget -= 1;
        }
        let Some(owners) = self.hardware_runtime_owners() else {
            crate::halt_always!();
        };
        if allow_hardware_publication && !owners.all_pipes_busy() {
            self.publish_ready_batch(engine, owners);
        }
    }

    fn hardware_runtime_owners(&self) -> Option<tx::Class0RuntimeOwners> {
        self.states.owners()
    }

    fn next_software_owner(&self, visited: u32) -> Option<usize> {
        let pending = indices(self.states.masks().pending & !visited)
            .map(|index| (index, self.admission_orders[index]));
        tx::oldest_pending_context(pending, self.next_admission_order)
    }

    /// No data frame is in the MAC.
    pub fn hardware_idle(&self) -> bool {
        self.hardware_runtime_owners()
            .is_some_and(|owners| owners.is_empty())
    }

    /// Per BlockAck stream, the sequence number of the oldest frame the
    /// recipient may still be missing: every frame not yet confirmed to the
    /// host, queued, in the MAC or waiting for its BlockAck.
    fn window_start_of(&self, stream: usize) -> Option<u16> {
        self.states.window_start(stream)
    }

    /// Whether `id` may go out now. Within a BlockAck session a frame at or
    /// beyond window start + window makes the recipient move its window,
    /// which releases the frames it holds behind a hole - and iwlwifi, whose
    /// reorder buffer is indexed by sequence modulo window, garbles them. A
    /// given-up frame holds the window start until its BlockAckReq is out.
    fn window_admits(&self, ba: &tx::ba::PeerSessions, id: tx::ContextId) -> bool {
        let Some((stream, sequence)) = tx::block_ack_stream(id) else {
            return true;
        };
        let (sessions, tid) = (ba.link((stream / 8) as u8), (stream % 8) as u8);
        let Some(window) = sessions.window(tid) else {
            return true;
        };
        if let Some(bar_ssn) = sessions.bar_ssn(tid) {
            let before_bar = bar_ssn.wrapping_sub(sequence) & 0x0fff;
            if before_bar == 0 || before_bar >= 0x0800 {
                return false;
            }
        }
        let start = self.window_start_of(stream).unwrap_or(sequence);
        sequence.wrapping_sub(start) & 0x0fff < window
    }

    /// Whether no frame strictly older than `owed_ssn` remains outstanding on
    /// `(link, tid)`, so a BlockAckReq can safely advance the recipient past it.
    pub fn bar_ready(&self, link: u8, tid: u8, owed_ssn: u16) -> bool {
        self.window_start_of(tx::stream(link, tid))
            .is_none_or(|start| start.wrapping_sub(owed_ssn) & 0x0fff < 0x0800)
    }

    fn notify_given_up(ba: &tx::ba::PeerSessions, context: tx::ContextId) {
        if let Some((stream, sequence)) = tx::block_ack_stream(context) {
            ba.link((stream / 8) as u8)
                .on_given_up((stream % 8) as u8, sequence);
        }
    }

    /// The originator's window start for `tid` to the peer on `link`: its
    /// oldest frame still to be acknowledged, or `next` when there is none.
    /// A new session starts here, before every frame still to come, and so
    /// does a BlockAckReq, so the recipient also releases the frames it holds
    /// after a hole.
    pub fn window_start(&self, link: u8, tid: u8, next: u16) -> u16 {
        self.window_start_of(tx::stream(link, tid)).unwrap_or(next)
    }

    fn route_hardware_completion(
        &mut self,
        completion: tx::HostClass0Completion,
        block_ack: &tx::ba::PeerSessions,
    ) {
        // A context lives at its own index.
        let Some(index) = completion.context.host_index() else {
            return;
        };
        match &self.states[index] {
            Some(HostTxState::Owned {
                retained,
                hardware: Some(hardware),
            }) if retained.phase() == tx::HostTxPhase::Scheduled => {
                // The completion names the slot this context was published
                // in. A live owner that disagrees can never complete and
                // holds every later frame back: say where, rather than stall.
                if !hardware.matches(retained.context(), completion) {
                    crate::halt_always!();
                }
            }
            // Nothing in the MAC to complete: a repeated or late completion.
            _ => return,
        }
        let Some(HostTxState::Owned {
            mut retained,
            hardware: Some(_),
            ..
        }) = self.states[index].take()
        else {
            return;
        };
        if completion.status == tx::aggregate::REQUEUED {
            // An aggregate did not deliver it; it is back on the PAS ring.
            let _ = retained.transition(tx::HostTxPhase::PasQueued);
            retained
                .context()
                .get()
                .terminal_status
                .set(tx::STATUS_PENDING);
            self.states[index] = Some(HostTxState::Owned {
                retained,
                hardware: None,
            });
            return;
        }
        let _ = retained.transition(tx::HostTxPhase::Completing);
        let status = tx::wsm_status_from_internal(completion.status);
        if status != 0 {
            Self::notify_given_up(block_ack, retained.context());
        }
        self.enqueue_confirmation(index);
        self.states[index] = Some(Self::confirmation_state(
            retained,
            status,
            completion.ack_failures,
        ));
    }

    /// Publish up to four ready PAS contexts onto a pipe without a retained
    /// runtime owner. The batch is staged into consecutive slots of one pipe
    /// and crosses the MAC trigger boundary once.
    fn publish_ready_batch(&mut self, engine: &mut tx::TxEngine, owners: tx::Class0RuntimeOwners) {
        // Context indices are reusable storage, not transmission order: walk
        // the PAS ring so the head and every batch member keep FIFO order.
        let mut candidate_pipes = [None; HOST_CONTEXT_COUNT];
        let mut standalone = [false; HOST_CONTEXT_COUNT];
        let mut candidate_order = [usize::MAX; HOST_CONTEXT_COUNT];
        let mut ready_count = 0;
        for index in tx::queued_pas_in_order().filter_map(tx::ContextId::host_index) {
            let Some(HostTxState::Owned {
                retained,
                hardware: None,
            }) = &self.states[index]
            else {
                continue;
            };
            if retained.phase() != tx::HostTxPhase::PasQueued {
                continue;
            }
            let pipe = tx::scheduled_pipe(retained);
            if pipe >= 4 {
                crate::halt_always!();
            }
            if owners.contains_pipe(pipe)
                || ready_count == HOST_CONTEXT_COUNT
                || !self.window_admits(&engine.ba, retained.context())
            {
                continue;
            }
            let context = retained.context();
            let policy = engine
                .protection
                .get(usize::from(context.get().interface.get()))
                .copied()
                .unwrap_or(tx::ProtectionPolicy::DISABLED);
            if tx::apply_protection_policy(context, policy).is_err() {
                continue;
            }
            let aggregatable = context.get().tx_rate.get() >= 14
                && context.get().flags.get() & 0x600 == 0
                && context.get().try_count.get() < tx::aggregate::AGGREGATE_ATTEMPTS
                && tx::block_ack_stream(context).is_some_and(|(stream, _)| {
                    engine
                        .ba
                        .link((stream / 8) as u8)
                        .window((stream % 8) as u8)
                        .is_some()
                });
            candidate_pipes[index] = Some(pipe);
            standalone[index] = context.get().flags.get() & 0x200 != 0
                || context.get().protection.get().is_some()
                || aggregatable;
            candidate_order[ready_count] = index;
            ready_count += 1;
        }
        let Some(&first_index) = candidate_order[..ready_count].first() else {
            return;
        };
        if self.publish_aggregate(engine, &candidate_pipes, &candidate_order[..ready_count]) {
            return;
        }

        let Some(HostTxState::Owned {
            retained: mut first,
            hardware: None,
        }) = self.states[first_index].take()
        else {
            return;
        };

        if !self.scheduler_phy_started_this_pass {
            let _ = tx::start_phy_operation_1();
            self.scheduler_phy_started_this_pass = true;
        }
        let first_reservation = match { tx::reserve_non_aggregate_scheduler(&mut first, engine) } {
            Ok(reservation) => reservation,
            Err(tx::SchedulerReserveError::Expired) => {
                if tx::reject_unscheduled_pas(&mut first).is_ok() {
                    let _ = first.transition(tx::HostTxPhase::Completing);
                    Self::notify_given_up(&engine.ba, first.context());
                    self.enqueue_confirmation(first_index);
                    self.states[first_index] = Some(Self::confirmation_state(
                        first,
                        tx::wsm_status_from_internal(10),
                        0,
                    ));
                } else {
                    self.states[first_index] = Some(HostTxState::Owned {
                        retained: first,
                        hardware: None,
                    });
                }
                return;
            }
            Err(_) => {
                self.states[first_index] = Some(HostTxState::Owned {
                    retained: first,
                    hardware: None,
                });
                return;
            }
        };
        let pipe = first_reservation.pipe();
        let first_slot = first_reservation.slot();
        let Some(occupied_slots) = owners.occupied_slots(pipe) else {
            crate::halt_always!();
        };
        let Some(plan) = tx::plan_ordinary_batch(
            &candidate_pipes,
            &standalone,
            &candidate_order[..ready_count],
            occupied_slots,
            first_index,
            pipe,
            first_slot,
        ) else {
            crate::halt_always!();
        };

        let mut members: [Option<ReservedBatchMember>; tx::MAX_ORDINARY_BATCH_DEPTH] =
            [const { None }; tx::MAX_ORDINARY_BATCH_DEPTH];
        members[0] = Some(ReservedBatchMember {
            index: first_index,
            retained: first,
            reservation: first_reservation,
        });
        // Publish one bounded FIFO cohort, then let the service-level owner
        // barrier retire every member before another cohort reaches hardware.
        // Overtaking is therefore bounded by this four-slot transaction rather
        // than growing without limit across repeated slot reuse.
        let mut member_count = 1;
        for position in 1..plan.len() {
            let (Some(index), Some(slot)) = (plan.index(position), plan.slot(position)) else {
                crate::halt_always!();
            };
            let Some(HostTxState::Owned {
                retained: mut retained_member,
                hardware: None,
            }) = self.states[index].take()
            else {
                crate::halt_always!();
            };
            let reservation = match {
                tx::reserve_non_aggregate_scheduler_in_batch(
                    &mut retained_member,
                    pipe,
                    slot,
                    position as u8,
                    engine,
                )
            } {
                Ok(reservation) => reservation,
                Err(_) => {
                    self.states[index] = Some(HostTxState::Owned {
                        retained: retained_member,
                        hardware: None,
                    });
                    break;
                }
            };
            members[position] = Some(ReservedBatchMember {
                index,
                retained: retained_member,
                reservation,
            });
            member_count += 1;
        }

        if member_count == 1 {
            let Some(member) = members[0].take() else {
                crate::halt_always!();
            };
            let ReservedBatchMember {
                index,
                mut retained,
                reservation,
            } = member;
            match reservation.publish(engine, &mut retained) {
                Ok(()) => {
                    self.states[index] = Some(HostTxState::Owned {
                        hardware: Some(HardwareOwner::single(pipe, first_slot)),
                        retained,
                    });
                }
                Err((reservation, _)) => {
                    self.states[index] = Some(HostTxState::Reserved {
                        retained,
                        reservation,
                    });
                }
            }
            return;
        }

        struct PublishedBatchMember {
            index: usize,
            retained: tx::RetainedHostTx,
            slot: u8,
        }
        let mut published: [Option<PublishedBatchMember>; tx::MAX_ORDINARY_BATCH_DEPTH] =
            [const { None }; tx::MAX_ORDINARY_BATCH_DEPTH];
        for position in 0..member_count {
            let Some(member) = members[position].take() else {
                crate::halt_always!();
            };
            let ReservedBatchMember {
                index,
                mut retained,
                reservation,
                ..
            } = member;
            let batch = if position == 0 {
                tx::BatchPosition::First
            } else if position + 1 == member_count {
                tx::BatchPosition::Last
            } else {
                tx::BatchPosition::Middle
            };
            if reservation
                .publish_in_batch(engine, &mut retained, batch)
                .is_err()
            {
                crate::halt_always!();
            }
            let Some(slot) = plan.slot(position) else {
                crate::halt_always!();
            };
            published[position] = Some(PublishedBatchMember {
                index,
                retained,
                slot,
            });
        }
        let Some(last_slot) = plan.slot(member_count - 1) else {
            crate::halt_always!();
        };
        {
            tx::finalize_staged_host_class0_pipe(pipe, first_slot, last_slot);
        }
        for position in 0..member_count {
            let Some(member) = published[position].take() else {
                crate::halt_always!();
            };
            self.states[member.index] = Some(HostTxState::Owned {
                retained: member.retained,
                hardware: Some(HardwareOwner::single(pipe, member.slot)),
            });
        }
    }

    /// Publish the head of the queue and the frames after it as one A-MPDU
    /// when its TID has a BlockAck session. Returns whether it did.
    fn publish_aggregate(
        &mut self,
        engine: &mut tx::TxEngine,
        candidate_pipes: &[Option<u8>; HOST_CONTEXT_COUNT],
        order: &[usize],
    ) -> bool {
        let queued = |state: &Option<HostTxState>| match state {
            Some(HostTxState::Owned {
                retained,
                hardware: None,
            }) if retained.phase() == tx::HostTxPhase::PasQueued => Some(retained.context()),
            _ => None,
        };
        let Some((&head, pipe)) = order
            .first()
            .and_then(|head| Some((head, candidate_pipes[*head]?)))
        else {
            return false;
        };
        let Some(head_id) = queued(&self.states[head]) else {
            return false;
        };
        let head_context = head_id.get();
        let (rate, interface) = (head_context.tx_rate.get(), head_context.interface.get());
        let Some((stream, first_sequence)) = tx::block_ack_stream(head_id) else {
            return false;
        };
        let Some(window) = engine
            .ba
            .link((stream / 8) as u8)
            .window((stream % 8) as u8)
        else {
            return false;
        };
        if rate < 14
            || head_context.flags.get() & 0x600 != 0
            || crate::mac::low_mac().receive_gate.get() != 0
        {
            return false;
        }
        let window_start = self.window_start_of(stream).unwrap_or(first_sequence);
        let Some(vif) = crate::vif::vif(interface) else {
            return false;
        };
        let limits = vif.ht_limits((stream / 8) as u8, engine.mpdu_start_spacing);
        let selector = tx::ampdu::spacing_selector(limits.density, rate);
        let budget = tx::aggregate::byte_budget(rate).min(u32::from(limits.max_bytes));
        let mut wire = tx::ampdu::WireLength::default();
        let mut joins = |id: tx::ContextId| {
            let context = id.get();
            wire.append(context.frame_length.get(), selector) <= budget
                && context.flags.get() & 0x600 == 0
                && context.try_count.get() < tx::aggregate::AGGREGATE_ATTEMPTS
                && context.tx_rate.get() == rate
                && context.interface.get() == interface
                && !tx::submission_expired(id)
                && tx::block_ack_stream(id).is_some_and(|(frame_stream, sequence)| {
                    frame_stream == stream && sequence.wrapping_sub(window_start) & 0x0fff < window
                })
        };
        // Members in FIFO order, up to the first frame that cannot join.
        let mut ids = [head_id; MAX_MEMBERS];
        let mut indices = [0; MAX_MEMBERS];
        let mut count = 0;
        for (index, id) in order
            .iter()
            .filter(|&&index| candidate_pipes[index] == Some(pipe))
            .map_while(|&index| Some((index, queued(&self.states[index])?)))
            .take_while(|&(_, id)| joins(id))
            .take(MAX_MEMBERS)
        {
            (ids[count], indices[count]) = (id, index);
            count += 1;
        }
        if count < 2 {
            return false;
        }
        if !self.scheduler_phy_started_this_pass {
            let _ = tx::start_phy_operation_1();
            self.scheduler_phy_started_this_pass = true;
        }
        let Ok(slot) = tx::aggregate::publish_aggregate(engine, &ids[..count], pipe, selector)
        else {
            return false;
        };
        for &index in &indices[..count] {
            if let Some(HostTxState::Owned {
                mut retained,
                hardware: None,
            }) = self.states[index].take()
            {
                retained.mark_scheduled();
                self.states[index] = Some(HostTxState::Owned {
                    retained,
                    hardware: Some(HardwareOwner::single(pipe, slot)),
                });
            }
        }
        true
    }

    fn service_index(
        &mut self,
        index: usize,
        engine: &mut tx::TxEngine,
        allow_hardware_publication: bool,
    ) {
        let Some(state) = self.states[index].take() else {
            return;
        };
        let next = match state {
            HostTxState::Confirming {
                owner,
                confirmation,
            } => HostTxState::Confirming {
                owner,
                confirmation,
            },
            HostTxState::Reserved {
                mut retained,
                reservation,
            } => {
                let pipe = reservation.pipe();
                let slot = reservation.slot();
                match reservation.publish(engine, &mut retained) {
                    Ok(()) => {
                        let hardware = HardwareOwner::single(pipe, slot);
                        HostTxState::Owned {
                            retained,
                            hardware: Some(hardware),
                        }
                    }
                    // Retried, but not past the frame's lifetime: it
                    // would never confirm.
                    Err((reservation, _)) if tx::submission_expired(retained.context()) => {
                        let _ = reservation.cancel(&mut retained);
                        Self::notify_given_up(&engine.ba, retained.context());
                        self.enqueue_confirmation(index);
                        Self::cancelled_confirmation(retained)
                    }
                    Err((reservation, _)) => HostTxState::Reserved {
                        retained,
                        reservation,
                    },
                }
            }
            HostTxState::Owned {
                mut retained,
                mut hardware,
            } => {
                if matches!(
                    retained.phase(),
                    tx::HostTxPhase::PostCryptoQueued | tx::HostTxPhase::PendingEligible
                ) {
                    match tx::service_pending(&mut retained) {
                        Ok(tx::PendingServiceReport::Complete(status)) => {
                            let _ = retained.transition(tx::HostTxPhase::Completing);
                            Self::notify_given_up(&engine.ba, retained.context());
                            self.enqueue_confirmation(index);
                            self.states[index] = Some(Self::confirmation_state(
                                retained,
                                tx::wsm_status_from_internal(status),
                                0,
                            ));
                            return;
                        }
                        Ok(
                            tx::PendingServiceReport::PasQueued
                            | tx::PendingServiceReport::LeaveQueued,
                        )
                        | Err(_) => {}
                    }
                }

                if retained.phase() == tx::HostTxPhase::PasQueued
                    && allow_hardware_publication
                    && self.hardware_idle()
                {
                    if !self.scheduler_phy_started_this_pass {
                        // Vendor starts PHY command 1 once before each
                        // non-empty scheduler pass.
                        let _ = tx::start_phy_operation_1();
                        self.scheduler_phy_started_this_pass = true;
                    }
                    match { tx::reserve_non_aggregate_scheduler(&mut retained, engine) } {
                        Ok(reservation) => {
                            let pipe = reservation.pipe();
                            let slot = reservation.slot();
                            match reservation.publish(engine, &mut retained) {
                                Ok(()) => {
                                    hardware = Some(HardwareOwner::single(pipe, slot));
                                    self.states[index] =
                                        Some(HostTxState::Owned { retained, hardware });
                                }
                                Err((reservation, _)) => {
                                    self.states[index] = Some(HostTxState::Reserved {
                                        retained,
                                        reservation,
                                    });
                                }
                            }
                            return;
                        }
                        Err(tx::SchedulerReserveError::Expired) => {
                            if { tx::reject_unscheduled_pas(&mut retained) }.is_ok() {
                                let _ = retained.transition(tx::HostTxPhase::Completing);
                                Self::notify_given_up(&engine.ba, retained.context());
                                self.enqueue_confirmation(index);
                                self.states[index] = Some(Self::confirmation_state(
                                    retained,
                                    tx::wsm_status_from_internal(10),
                                    0,
                                ));
                                return;
                            }
                        }
                        Err(_) => {}
                    }
                }

                HostTxState::Owned { retained, hardware }
            }
        };
        self.states[index] = Some(next);
    }

    fn enqueue_confirmation(&mut self, index: usize) {
        let len = usize::from(self.confirm_len);
        if len >= HOST_CONTEXT_COUNT {
            crate::halt_always!();
        }
        let tail = (usize::from(self.confirm_head) + len) % HOST_CONTEXT_COUNT;
        self.confirm_queue[tail] = index as u8;
        self.confirm_len = (len + 1) as u8;
        self.next_confirmation_order = self.next_confirmation_order.wrapping_add(1);
    }

    // The context normally contains exact per-rate failure nibbles. The
    // host-testable fallback reconstructs them from ack_failures only when an
    // older completion path did not populate those words.
    fn confirmation_state(
        retained: tx::RetainedHostTx,
        status: u32,
        ack_failures: u8,
    ) -> HostTxState {
        let context = retained.context();
        let fields = tx::confirmation_fields(context);
        let tx_rate = fields.tx_rate;
        HostTxState::Confirming {
            confirmation: HostTxConfirmation {
                packet_id: retained.packet_id(),
                context,
                status,
                tx_rate,
                ack_failures,
                flags: fields.flags,
                rate_try: {
                    let reported = fields.rate_try;
                    if reported == [0; 3] {
                        tx::rate_try_for_single_rate(tx_rate, ack_failures)
                    } else {
                        reported
                    }
                },
            },
            owner: ConfirmationOwner::Retained(retained),
        }
    }

    /// Publish confirmations in the order their owners reached completion.
    /// Context identity resolves the hardware owner; packet-id magnitude has
    /// no ordering meaning and may wrap independently.
    pub fn confirmation(&self) -> Option<HostTxConfirmation> {
        if self.confirm_len == 0 {
            return None;
        }
        let index = usize::from(self.confirm_queue[usize::from(self.confirm_head)]);
        let Some(HostTxState::Confirming { confirmation, .. }) = self.states[index] else {
            return None;
        };
        Some(confirmation)
    }

    pub fn confirmation_count(&self, limit: usize) -> usize {
        usize::from(self.confirm_len).min(limit)
    }

    /// Pop the oldest confirmation and its request release token in O(1)
    /// without scanning or re-evaluating `States` masks per item.
    pub fn take_confirmation(&mut self) -> Option<(HostTxConfirmation, hif::RequestReleaseToken)> {
        if self.confirm_len == 0 {
            return None;
        }
        let head = usize::from(self.confirm_head);
        let index = usize::from(self.confirm_queue[head]);
        self.confirm_head = ((head + 1) % HOST_CONTEXT_COUNT) as u8;
        self.confirm_len -= 1;
        let HostTxState::Confirming {
            confirmation,
            owner,
            ..
        } = self.states[index].take()?
        else {
            return None;
        };
        let release = match owner {
            ConfirmationOwner::Retained(retained) => retained.finish(),
            ConfirmationOwner::Release(release) => release,
        };
        Some((confirmation, release))
    }

    /// Finish the same first confirmation returned by `confirmation()` at the
    /// inherited HIF handoff point. `publish_request_in_place` subsequently
    /// returns request credit before enqueuing its copied output buffer.
    ///
    /// # Safety
    /// Completion accounting must already have removed hardware ownership.
    pub fn finish_confirmation(&mut self) -> Option<hif::RequestReleaseToken> {
        self.take_confirmation().map(|(_, release)| release)
    }

    /// Convert every reversible owner into a failure confirmation. Scheduled
    /// hardware ownership remains until normal completion; a frame the AES
    /// engine works on is waited out first.
    pub fn reset(&mut self, crypto: &mut Crypto) {
        if let Some((_, job)) = self.sealing.take() {
            crypto.abandon(job);
        }
        for index in 0..HOST_CONTEXT_COUNT {
            let Some(state) = self.states[index].take() else {
                continue;
            };
            self.states[index] = Some(match state {
                state @ HostTxState::Confirming { .. } => state,
                HostTxState::Reserved {
                    mut retained,
                    reservation,
                    ..
                } => {
                    let _ = reservation.cancel(&mut retained);
                    self.enqueue_confirmation(index);
                    Self::cancelled_confirmation(retained)
                }
                HostTxState::Owned { retained, .. }
                    if retained.phase() != tx::HostTxPhase::Scheduled =>
                {
                    self.enqueue_confirmation(index);
                    Self::cancelled_confirmation(retained)
                }
                state @ HostTxState::Owned { .. } => state,
            });
        }
    }

    fn cancelled_confirmation(retained: tx::RetainedHostTx) -> HostTxState {
        let context = retained.context();
        let packet_id = retained.packet_id();
        let fields = tx::confirmation_fields(context);
        let tx_rate = fields.tx_rate;
        let rate_try = fields.rate_try;
        let release = {
            retained
                .cancel_before_pas()
                .expect("reversible host TX cancellation")
        };
        HostTxState::Confirming {
            owner: ConfirmationOwner::Release(release),
            confirmation: HostTxConfirmation {
                packet_id,
                context,
                status: 1,
                tx_rate,
                ack_failures: 0,
                flags: 0,
                rate_try,
            },
        }
    }
}

impl Default for HostTxDriver {
    fn default() -> Self {
        Self::new()
    }
}
