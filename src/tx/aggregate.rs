//! A-MPDU publication and completion (vendor kind-1 slots).
//!
//! An aggregate occupies one pipe slot. The slot command carries the PHY
//! words shared by all members and jumps to a software record in packet RAM,
//! which transfers each member's own descriptor (from its length word on).
//! The slot expects a BlockAck (response class 0x0c); the MAC reports that
//! status once the BlockAck arrives, and the BlockAck's bitmap, read from the
//! RX FIFO, then decides each member's outcome: delivered members complete,
//! missing ones go back to the head of the PAS ring for another aggregate,
//! then out alone under the host's retry policy.

use crate::global::Global;
use core::cell::Cell;

use super::ampdu::MAX_MEMBERS;
use super::hw::{read_u8, write_u32};
use super::*;

/// Longest A-MPDU PPDU to build, in microseconds; HT allows 5,484, and 4,000
/// already fits all 16 full-size MPDUs at MCS 5..7 while keeping lower-MCS
/// PPDUs shorter on noisy 2.4 GHz channels.
const MAX_AIRTIME_US: u32 = 4_000;

/// A-MPDU bytes that fit in `MAX_AIRTIME_US` at HT rate index `rate`
/// (14..=21 for MCS0..7, 20 MHz, long guard interval).
pub fn byte_budget(rate: u8) -> u32 {
    const KBIT_PER_S: [u32; 8] = [
        6_500, 13_000, 19_500, 26_000, 39_000, 52_000, 58_500, 65_000,
    ];
    let kbit = KBIT_PER_S
        .get(usize::from(rate.wrapping_sub(14)))
        .copied()
        .unwrap_or(0);
    (kbit * MAX_AIRTIME_US / 8_000).min(u32::from(u16::MAX))
}

/// One aggregate in flight, per software record.
struct Record {
    /// `pipe * 4 + slot`, while the MAC owns the aggregate.
    slot: Cell<Option<u8>>,
    members: [Cell<Option<ContextId>>; MAX_MEMBERS],
    /// Hardware-free reply identity, early evidence and completion deadline.
    reply: Cell<Option<ack::Pending>>,
}

impl Record {
    const fn new() -> Self {
        Self {
            slot: Cell::new(None),
            members: [const { Cell::new(None) }; MAX_MEMBERS],
            reply: Cell::new(None),
        }
    }
}

struct Aggregates {
    records: [Record; packet_ram::SOFTWARE_RECORD_COUNT],
}

static AGGREGATES: Global<Aggregates> = Global::new(Aggregates {
    records: [const { Record::new() }; packet_ram::SOFTWARE_RECORD_COUNT],
});

fn record_for(pipe: u8, slot: u8) -> Option<&'static Record> {
    let key = pipe * 4 + slot;
    AGGREGATES
        .records
        .iter()
        .find(|record| record.slot.get() == Some(key))
}

fn software_record(index: usize) -> u32 {
    (packet_ram::software_records().start + index * packet_ram::SOFTWARE_RECORD_SIZE) as u32
}

/// Whether the slot holds an aggregate.
pub(crate) fn is_aggregate_slot(pipe: u8, slot: u8) -> bool {
    record_for(pipe, slot).is_some()
}

/// Publish `members` (queued QoS data of one TID and rate, in FIFO order) as
/// one A-MPDU on `pipe`. Returns the slot. On error ownership is unchanged;
/// single-frame CTS policy is refreshed at reservation if staging failed.
pub fn publish_aggregate(
    engine: &mut TxEngine,
    members: &[ContextId],
    pipe: u8,
    selector: u8,
) -> Result<u8, ProbeBuildError> {
    let count = members.len();
    let (record_index, record) = AGGREGATES
        .records
        .iter()
        .enumerate()
        .find(|(_, record)| record.slot.get().is_none() && record.members[0].get().is_none())
        .ok_or(ProbeBuildError::PipeSlotBusy)?;
    if !(2..=MAX_MEMBERS).contains(&count)
        || pipe >= 4
        || members.iter().any(|id| id.get().flags.get() & 0x600 != 0)
    {
        return Err(ProbeBuildError::UnsupportedPublicationShape);
    }
    let pipe_record = pipes().pipe(pipe);
    if pipe_record.hardware_ring(pipe).is_none() || pipe_record.state.get() != 0 {
        return Err(ProbeBuildError::PipeStateUnavailable);
    }
    let slot = pipe_record.producer.get() & 3;
    let first = members[0];
    let context = first.get();
    let rate = context.tx_rate.get();
    // Snapshot identity before touching the PAS ring or publishing anything.
    let frame = context.frame.get() as usize;
    let own = core::array::from_fn(|index| read_u8(frame + 10 + index));
    let peer = core::array::from_fn(|index| read_u8(frame + 4 + index));
    // Frames can outlive their host link mapping. Never publish a stale
    // recipient using the new occupant's negotiated limits and BA window.
    if crate::vif::link_peer(context.interface.get(), context.host_link.get()) != Some(peer) {
        return Err(ProbeBuildError::UnsupportedPublicationShape);
    }
    let (tid, _) = qos_sequence(first).ok_or(ProbeBuildError::UnsupportedPublicationShape)?;
    let mut sequences = [0; MAX_MEMBERS];
    for (sequence, &member) in sequences.iter_mut().zip(members) {
        let (member_tid, seq) =
            qos_sequence(member).ok_or(ProbeBuildError::UnsupportedPublicationShape)?;
        let member_frame = member.get().frame.get() as usize;
        if member_tid != tid
            || (member != first && peer != core::array::from_fn(|i| read_u8(member_frame + 4 + i)))
        {
            return Err(ProbeBuildError::UnsupportedPublicationShape);
        }
        *sequence = seq;
    }
    let reply = ack::Pending::new(own, peer, tid, &sequences[..count])
        .ok_or(ProbeBuildError::UnsupportedPublicationShape)?;

    // Build and validate the software-record command stream before mutating
    // the PAS ring so an oversized or malformed stream never leaks entries.
    let spacing = (selector != 0).then(|| {
        let end = packet_ram::automatic_response_list().end as u32;
        ampdu::transfer_word(end - 4 - u32::from(selector) * 4)
    });
    let mut list = [ampdu::Member {
        frame_state: 0,
        frame_length: 0,
    }; MAX_MEMBERS];
    for (entry, &id) in list.iter_mut().zip(members) {
        *entry = ampdu::Member {
            frame_state: host_descriptor_address(id)
                .ok_or(ProbeBuildError::UnsupportedPublicationShape)?,
            frame_length: id.get().frame_length.get(),
        };
    }
    let stream = ampdu::command_stream(&list[..count], selector, spacing)
        .ok_or(ProbeBuildError::UnsupportedPublicationShape)?;

    // Take every member off the PAS ring and stage its descriptor; undo in
    // reverse on failure.
    let mut taken: [Option<(u8, u8)>; MAX_MEMBERS] = [None; MAX_MEMBERS];
    let mut failure = None;
    for (index, &id) in members.iter().enumerate() {
        let Some(ring) = take_from_pas(id) else {
            failure = Some(ProbeBuildError::UnsupportedPublicationShape);
            break;
        };
        taken[index] = Some(ring);
        // Vendor kind 1 does not prepend CTS-to-self. Keep aggregation
        // available in an ERP-protected BSS; singles reapply policy at reserve.
        if !omit_aggregate_cts(id.get()) {
            crate::halt_always!();
        }
        if let Err(error) = build_host_frame_descriptor(id) {
            failure = Some(error);
            break;
        }
    }
    if let Some(error) = failure {
        for (&member, ring) in members.iter().zip(taken).rev() {
            if let Some((position, head)) = ring {
                restore_to_pas(member, position, head);
            }
        }
        return Err(error);
    }

    // Software record: member transfers, delimiters, spacing, terminator.
    let record_address = software_record(record_index);
    for (index, &word) in stream.words().iter().enumerate() {
        write_u32(record_address as usize + index * 4, word);
    }

    // Vendor `txq_try_append_to_aggregate` marks every member 0x20: the PHY
    // control word then selects aggregation (6 instead of 2), and a retry
    // keeps the member at an HT rate.
    for member in members {
        let context = member.get();
        context.flags.set(context.flags.get() | 0x20);
    }

    // Slot command: duration words (filled at publication), the shared PHY
    // words, and the jump to the software record.
    let command = packet_ram::tx_command(usize::from(pipe), usize::from(slot));
    (0..16).for_each(|word| write_u32(command + word * 4, 0));
    write_u32(command + 8, 0xdc00_0000);
    let phy = build_phy_rate_words(
        rate,
        0,
        context.flags.get(),
        context.request_flag_rate_bits.get(),
        crate::mac::RATE_ENCODING
            .get(usize::from(rate))
            .copied()
            .unwrap_or(0),
    );
    let hardware_rate = crate::mac::RATE_ATTRIBUTE
        .get(usize::from(rate))
        .copied()
        .unwrap_or(0);
    let words = ampdu::phy_words(
        phy.rate,
        finalize_aggregate_phy_control(phy, rate, stream.aggregate_length),
        hardware_rate,
        stream.aggregate_length,
    );
    for (index, word) in words.into_iter().enumerate() {
        write_u32(command + 0x0c + index * 4, word);
    }
    write_u32(command + 0x18, ampdu::transfer_word(record_address));

    // The first member stands for the slot: it expects the BlockAck, and its
    // timing describes the full aggregate (vendor `txp_build_pipe_descriptor`
    // kind 1: duration word payload << 15 | 0x20b4, airtime payload + 0xd4).
    let phy_config = crate::mac::low_mac().rate_config.get();
    let payload = crate::mac::extended_airtime(
        phy_config,
        rate,
        stream.aggregate_length,
        context.flags.get() & 8 != 0,
    );
    context.frame_kind.set(0x0c);
    context.payload_base.set(payload);
    context.ack_duration.set(0xb4);
    context.total_airtime.set(u32::from(payload) + 0xb4 + 0x20);
    context.flags.set(context.flags.get() | (1 << 26));
    pipe_record.slot(slot).reserve(first, 0x0c);

    record.reply.set(Some(reply));
    record.slot.set(Some(pipe * 4 + slot));
    for (cell, member) in record.members.iter().zip(
        members
            .iter()
            .copied()
            .map(Some)
            .chain(core::iter::repeat(None)),
    ) {
        cell.set(member);
    }
    for member in members {
        member.get().aggregate_slot.set(pipe * 4 + slot);
    }

    if let Err(error) = publish_host_class0_slot(engine, first, pipe, slot, BatchPosition::Only) {
        // Publication never fails after reservation on this path; if it does,
        // the MAC state is unknown.
        let _ = error;
        crate::halt_always!();
    }
    Ok(slot)
}

/// Internal completion status of a member put back on the PAS ring.
pub const REQUEUED: u16 = 0x7e;
/// Aggregate transmissions a member gets. After that it goes out alone,
/// where the host's retry policy steps it down to lower rates and finally
/// gives up.
pub const AGGREGATE_ATTEMPTS: u16 = 2;
/// The MAC finished the aggregate in `live`'s slot with `status`: 0 once the
/// BlockAck arrived, a failure otherwise. Frees the slot; the members are
/// resolved against a BlockAck the RX path already handed over, or wait for
/// [`look_for_block_acks`] or the RX path to find theirs.
pub(crate) fn complete_aggregate_slot(live: LiveTxSlot, status: u16) {
    let Some(record) = record_for(live.pipe, live.slot) else {
        crate::halt_always!();
    };
    record.slot.set(None);
    let pipes = pipes();
    pipes.pipe(live.pipe).slot(live.slot).frame.set(None);
    pipes.active_tx.set(pipes.active_tx.get().wrapping_sub(1));
    let Some(mut reply) = record.reply.get() else {
        crate::halt_always!();
    };
    match reply.complete(status == 0, crate::platform::timer_now()) {
        ack::Completion::Resolved(bits) => resolve(record, bits),
        ack::Completion::Waiting => record.reply.set(Some(reply)),
    }
}

/// Look ahead in FIFO order across all peers and local addresses.
pub trait BlockAckLookahead {
    fn next_block_ack(&mut self) -> Option<ampdu::BlockAck>;
}

/// Route a bounded number of FIFO BlockAcks to waiting or active owners.
pub fn look_for_block_acks(fifo: &mut impl BlockAckLookahead) {
    for _ in 0..AGGREGATES.records.len() {
        let Some(block_ack) = fifo.next_block_ack() else {
            break;
        };
        on_block_ack(block_ack);
    }
}

/// One cumulative bitmap may answer multiple aggregates of the same stream.
pub fn on_block_ack(block_ack: ampdu::BlockAck) {
    ack::route(
        block_ack,
        AGGREGATES.records.iter().map(|record| &record.reply),
        |index, bits| {
            resolve(&AGGREGATES.records[index], bits);
        },
    );
}

/// Once per main-loop pass: look again for BlockAcks still missing, and give
/// up on those that do not come.
pub fn tick(fifo: &mut impl BlockAckLookahead) {
    look_for_block_acks(fifo);
    for record in &AGGREGATES.records {
        if record
            .reply
            .get()
            .and_then(|reply| reply.wake_in(crate::platform::timer_now()))
            == Some(0)
        {
            resolve(record, 0);
        }
    }
}

/// Microseconds until [`tick`] gives up on a missing BlockAck. One that
/// arrives wakes the loop as any received frame does.
pub fn wake_in(now: u32) -> Option<u32> {
    AGGREGATES
        .records
        .iter()
        .filter_map(|record| record.reply.get()?.wake_in(now))
        .min()
}

/// Settle every member: delivered ones complete, missing ones go back to the
/// head of the PAS ring, oldest first.
fn resolve(record: &Record, delivered: u16) {
    record.reply.set(None);
    let members: [Option<ContextId>; MAX_MEMBERS] =
        core::array::from_fn(|index| record.members[index].take());
    let mut statuses: [_; MAX_MEMBERS] = core::array::from_fn(|index| {
        members[index].map(|_| {
            if delivered >> index & 1 != 0 {
                0
            } else {
                REQUEUED
            }
        })
    });
    // Push the retries newest first so the oldest ends up at the head.
    for (member, status) in members.iter().zip(statuses.iter_mut()).rev() {
        if let (Some(id), Some(status)) = (member, status)
            && *status == REQUEUED
        {
            let context = id.get();
            let rate = context.tx_rate.get();
            if let Some(rate_try) = context.rate_try.get(usize::from(rate >> 3)) {
                let shift = u32::from((rate & 7) * 4);
                let attempts = (rate_try.get() >> shift) & 0x0f;
                if attempts < 0x0f {
                    rate_try.set((rate_try.get() & !(0x0f << shift)) | ((attempts + 1) << shift));
                }
            }
            if !requeue_at_pas_head(*id) {
                *status = 0x0b;
            }
        }
    }
    for (id, status) in members
        .into_iter()
        .zip(statuses)
        .filter_map(|(id, status)| Some((id?, status?)))
    {
        let context = id.get();
        let class_bits = ((context.flags.get() >> 18) & 0x0c) as u16;
        context
            .auxiliary_state
            .set(context.auxiliary_state.get() | class_bits);
        context.terminal_status.set(status);
        COMPLETION_RING.enqueue(id);
    }
}
