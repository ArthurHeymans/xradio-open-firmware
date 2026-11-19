//! Hardware-free aggregate reply ownership. A BlockAck describes a stream
//! window, not a transaction ID: one bitmap can answer several aggregates.

use super::ampdu::{BlockAck, MAX_MEMBERS};
use core::cell::Cell;

pub const WAIT_US: u32 = 10_000;

#[derive(Clone, Copy)]
pub struct Pending {
    own: [u8; 6],
    peer: [u8; 6],
    tid: u8,
    sequences: [u16; MAX_MEMBERS],
    count: usize,
    acknowledged: u16,
    seen: bool,
    waiting: Option<u32>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum Completion {
    Waiting,
    /// One bit per member; unset members must be retried.
    Resolved(u16),
}

impl Pending {
    pub fn new(own: [u8; 6], peer: [u8; 6], tid: u8, sequences: &[u16]) -> Option<Self> {
        let (&first, _) = sequences.split_first()?;
        if sequences.len() > MAX_MEMBERS
            || sequences
                .iter()
                .any(|&seq| seq.wrapping_sub(first) & 0x0fff >= 64)
        {
            return None;
        }
        let mut pending = Self {
            own,
            peer,
            tid,
            sequences: [0; MAX_MEMBERS],
            count: sequences.len(),
            acknowledged: 0,
            seen: false,
            waiting: None,
        };
        pending.sequences[..sequences.len()].copy_from_slice(sequences);
        Some(pending)
    }

    fn observe(&mut self, ack: BlockAck) -> Option<u16> {
        if !ack.matches(self.own, self.peer, self.tid) {
            return None;
        }
        let sequences = &self.sequences[..self.count];
        let bits = sequences.iter().enumerate().fold(0, |bits, (index, &seq)| {
            bits | (u16::from(ack.acknowledges(seq)) << index)
        });
        let first = sequences[0];
        let span = sequences
            .iter()
            .map(|&seq| seq.wrapping_sub(first) & 0x0fff)
            .max()
            .unwrap_or(0);
        // Merely lying in the ACK's 64-entry window is insufficient: an
        // older ACK often covers later, not-yet-transmitted sequence numbers
        // with zero bits. Require positive evidence, or an SSN within this
        // aggregate's span (also accepts all-zero and lost-first replies).
        // An older SSN with no positive bits is ambiguous; wait/timeout
        // rather than attach its negative evidence to a later aggregate.
        if bits == 0 && ack.ssn.wrapping_sub(first) & 0x0fff > span {
            return None;
        }
        self.seen = true;
        self.acknowledged |= bits;
        self.waiting.map(|_| self.acknowledged)
    }

    pub fn complete(&mut self, success: bool, now: u32) -> Completion {
        if !success {
            Completion::Resolved(0)
        } else if self.seen {
            Completion::Resolved(self.acknowledged)
        } else {
            self.waiting = Some(now);
            Completion::Waiting
        }
    }

    pub fn wake_in(&self, now: u32) -> Option<u32> {
        self.waiting
            .map(|since| crate::deadline::until(since.wrapping_add(WAIT_US), now))
    }
}

/// Route cumulative evidence to every matching owner, not the first table
/// entry. In-flight owners retain it until MAC completion; waiting owners
/// resolve now. `resolve` also clears that owner's cell.
pub fn route<'a>(
    ack: BlockAck,
    owners: impl Iterator<Item = &'a Cell<Option<Pending>>>,
    mut resolve: impl FnMut(usize, u16),
) {
    for (index, owner) in owners.enumerate() {
        if let Some(mut pending) = owner.get() {
            let delivered = pending.observe(ack);
            owner.set(Some(pending));
            if let Some(bits) = delivered {
                resolve(index, bits);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const OWN: [u8; 6] = [2; 6];
    const PEER: [u8; 6] = [4; 6];

    fn ack(ssn: u16, bitmap: u64) -> BlockAck {
        BlockAck {
            receiver: OWN,
            transmitter: PEER,
            tid: 0,
            ssn,
            bitmap,
        }
    }

    fn pending(sequences: &[u16]) -> Cell<Option<Pending>> {
        Cell::new(Pending::new(OWN, PEER, 0, sequences))
    }

    fn complete(owner: &Cell<Option<Pending>>, now: u32) -> Completion {
        let mut state = owner.get().unwrap();
        let outcome = state.complete(true, now);
        owner.set(Some(state));
        outcome
    }

    fn deliver(owners: &[Cell<Option<Pending>>], ack: BlockAck) -> std::vec::Vec<(usize, u16)> {
        let mut resolved = std::vec::Vec::new();
        route(ack, owners.iter(), |index, bits| {
            owners[index].set(None);
            resolved.push((index, bits));
        });
        resolved
    }

    #[test]
    fn early_and_waiting_replies_stay_with_peer_tid_and_local_address() {
        let owners = [
            pending(&[10, 11]),
            Cell::new(Pending::new([6; 6], [8; 6], 0, &[10, 11])),
        ];
        assert_eq!(complete(&owners[0], 0), Completion::Waiting);
        assert!(
            deliver(
                &owners,
                BlockAck {
                    receiver: [6; 6],
                    transmitter: [8; 6],
                    ..ack(10, 3)
                }
            )
            .is_empty()
        );
        assert_eq!(complete(&owners[1], 1), Completion::Resolved(3));
        assert!(
            deliver(
                &owners,
                BlockAck {
                    tid: 1,
                    ..ack(10, 3)
                }
            )
            .is_empty()
        );
        assert_eq!(deliver(&owners, ack(10, 1)), [(0, 1)]);
    }

    #[test]
    fn stale_reply_does_not_shift_to_the_next_aggregate() {
        let owners = [pending(&[102, 103]), pending(&[100, 101])];
        assert_eq!(complete(&owners[1], 0), Completion::Waiting);
        assert_eq!(deliver(&owners, ack(100, 3)), [(1, 3)]);
        // Duplicate/stale ACK still covers 102/103, but acknowledges neither.
        assert!(deliver(&owners, ack(100, 3)).is_empty());
        assert_eq!(complete(&owners[0], 1), Completion::Waiting);
        assert_eq!(deliver(&owners, ack(102, 3)), [(0, 3)]);
    }

    #[test]
    fn cumulative_bitmap_reaches_all_overlapping_owners_in_any_table_order() {
        for order in [[100, 102], [102, 100]] {
            let owners = order.map(|seq| pending(&[seq, seq + 1]));
            for owner in &owners {
                assert_eq!(complete(owner, 0), Completion::Waiting);
            }
            assert_eq!(deliver(&owners, ack(100, 0b1111)), [(0, 3), (1, 3)]);
        }
        // The same cumulative reply reaches a waiting and an in-flight owner.
        let owners = [pending(&[102, 103]), pending(&[100, 101])];
        assert_eq!(complete(&owners[1], 0), Completion::Waiting);
        assert_eq!(deliver(&owners, ack(100, 0b1111)), [(1, 3)]);
        assert_eq!(complete(&owners[0], 1), Completion::Resolved(3));
    }

    #[test]
    fn lost_first_and_wrapped_sequences_are_not_rejected_by_first_member() {
        let owners = [pending(&[4095, 0, 1])];
        assert!(deliver(&owners, ack(0, 0b11)).is_empty());
        assert_eq!(complete(&owners[0], 0), Completion::Resolved(0b110));
        let owners = [pending(&[4095, 0, 1])];
        assert_eq!(complete(&owners[0], 0), Completion::Waiting);
        assert_eq!(deliver(&owners, ack(0, 0)), [(0, 0)]);
    }

    #[test]
    fn early_bitmaps_accumulate_positive_evidence_and_failed_mac_still_retries() {
        let owners = [pending(&[100, 101])];
        deliver(&owners, ack(100, 1));
        deliver(&owners, ack(101, 1));
        deliver(&owners, ack(100, 0));
        assert_eq!(complete(&owners[0], 0), Completion::Resolved(3));
        let mut state = owners[0].get().unwrap();
        assert_eq!(state.complete(false, 0), Completion::Resolved(0));
    }

    #[test]
    fn older_ssn_with_zero_member_bits_waits_but_positive_bits_are_usable() {
        let owners = [pending(&[102, 103])];
        deliver(&owners, ack(100, 0));
        assert_eq!(complete(&owners[0], 0), Completion::Waiting);
        assert_eq!(deliver(&owners, ack(100, 0b1100)), [(0, 3)]);
        assert!(Pending::new(OWN, PEER, 0, &[]).is_none());
        assert!(Pending::new(OWN, PEER, 0, &[100, 164]).is_none());
    }

    #[test]
    fn timeout_wrap_and_reused_record_for_another_sequence() {
        let owners = [pending(&[100, 101])];
        let start = u32::MAX - 99;
        assert_eq!(complete(&owners[0], start), Completion::Waiting);
        assert_eq!(
            owners[0]
                .get()
                .unwrap()
                .wake_in(start.wrapping_add(WAIT_US - 1)),
            Some(1)
        );
        assert_eq!(
            owners[0]
                .get()
                .unwrap()
                .wake_in(start.wrapping_add(WAIT_US)),
            Some(0)
        );
        // The runtime resolves with no bits at timeout, then reuses the cell.
        owners[0].set(Pending::new(OWN, PEER, 0, &[102, 103]));
        deliver(&owners, ack(100, 3));
        assert_eq!(complete(&owners[0], 0), Completion::Waiting);
        assert_eq!(deliver(&owners, ack(102, 3)), [(0, 3)]);
    }
}
