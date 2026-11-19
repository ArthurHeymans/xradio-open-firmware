//! The host TX context table, and what the main loop asks about it every
//! pass, kept as per-context contributions: a change re-evaluates only the
//! contexts it touched. Every mutable access goes through `IndexMut`, which
//! marks its context dirty, so nothing can change the table behind them.

use super::{HOST_CONTEXT_COUNT, HostTxState};
use crate::tx;
use core::cell::Cell;
use core::ops::{Index, IndexMut};

/// Context sets as bit masks, by context index.
#[derive(Clone, Copy)]
pub(super) struct Masks {
    pub occupied: u32,
    /// Admitted and classified, waiting to be sealed.
    pub classified: u32,
    /// Sealed, waiting for the scheduler.
    pub pending: u32,
    pub confirming: u32,
}

/// What one context contributes to the answers, kept per index.
#[derive(Clone, Copy)]
enum Claim {
    None,
    /// Owns this hardware pipe and slot.
    Slot(u8, u8),
    /// Scheduled without a hardware owner: the table is inconsistent.
    Broken,
}

/// The cached answers. A change marks its index dirty; the next question
/// re-evaluates only the dirty entries, then derives the owner set and the
/// window starts from the per-index contributions.
struct Summary {
    dirty: Cell<u32>,
    masks: Cell<Masks>,
    claims: [Cell<Claim>; HOST_CONTEXT_COUNT],
    /// Per index, the frame's BlockAck stream and sequence number, if it
    /// has them.
    sequences: [Cell<Option<(usize, u16)>>; HOST_CONTEXT_COUNT],
    /// Derived; `None` when stale.
    owners: Cell<Option<Option<tx::Class0RuntimeOwners>>>,
    window_starts: [Cell<Option<u16>>; tx::STREAMS],
    window_starts_valid: Cell<bool>,
}

pub(super) struct States {
    states: [Option<HostTxState>; HOST_CONTEXT_COUNT],
    summary: Summary,
    /// Counts changes: the driver's activity fingerprint.
    generation: u32,
}

impl States {
    pub const fn new() -> Self {
        Self {
            states: [const { None }; HOST_CONTEXT_COUNT],
            summary: Summary {
                dirty: Cell::new(0),
                masks: Cell::new(Masks {
                    occupied: 0,
                    classified: 0,
                    pending: 0,
                    confirming: 0,
                }),
                claims: [const { Cell::new(Claim::None) }; HOST_CONTEXT_COUNT],
                sequences: [const { Cell::new(None) }; HOST_CONTEXT_COUNT],
                owners: Cell::new(None),
                window_starts: [const { Cell::new(None) }; tx::STREAMS],
                window_starts_valid: Cell::new(false),
            },
            generation: 0,
        }
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }

    pub fn masks(&self) -> Masks {
        self.refresh();
        self.summary.masks.get()
    }

    pub fn owners(&self) -> Option<tx::Class0RuntimeOwners> {
        self.refresh();
        if let Some(owners) = self.summary.owners.get() {
            return owners;
        }
        let mut owners = Some(tx::Class0RuntimeOwners::new());
        for index in indices(self.summary.masks.get().occupied) {
            match self.summary.claims[index].get() {
                Claim::None => {}
                Claim::Slot(pipe, slot) => {
                    if let Some(set) = owners.as_mut()
                        && !set.observe(index, pipe, slot)
                    {
                        owners = None;
                    }
                }
                Claim::Broken => owners = None,
            }
        }
        self.summary.owners.set(Some(owners));
        owners
    }

    /// The oldest sequence number of `stream` still outstanding.
    pub fn window_start(&self, stream: usize) -> Option<u16> {
        self.refresh();
        let starts = &self.summary.window_starts;
        if !self.summary.window_starts_valid.replace(true) {
            starts.iter().for_each(|start| start.set(None));
            for index in indices(self.summary.masks.get().occupied) {
                if let Some((stream, sequence)) = self.summary.sequences[index].get() {
                    let start = &starts[stream];
                    // Older, counting modulo 4096.
                    if start
                        .get()
                        .is_none_or(|start| start.wrapping_sub(sequence) & 0x0fff < 0x0800)
                    {
                        start.set(Some(sequence));
                    }
                }
            }
        }
        starts.get(stream).and_then(Cell::get)
    }

    /// Re-evaluate the entries changed since the last question.
    fn refresh(&self) {
        let dirty = self.summary.dirty.replace(0);
        if dirty == 0 {
            return;
        }
        let mut masks = self.summary.masks.get();
        for index in indices(dirty) {
            let bit = 1 << index;
            masks.occupied &= !bit;
            masks.classified &= !bit;
            masks.pending &= !bit;
            masks.confirming &= !bit;
            let (claim, sequence) = evaluate(&self.states[index], bit, &mut masks);
            self.summary.claims[index].set(claim);
            self.summary.sequences[index].set(sequence);
        }
        self.summary.masks.set(masks);
    }
}

impl Index<usize> for States {
    type Output = Option<HostTxState>;

    fn index(&self, index: usize) -> &Self::Output {
        &self.states[index]
    }
}

impl IndexMut<usize> for States {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        let summary = &self.summary;
        summary.dirty.set(summary.dirty.get() | 1 << index);
        summary.owners.set(None);
        summary.window_starts_valid.set(false);
        self.generation = self.generation.wrapping_add(1);
        &mut self.states[index]
    }
}

/// One entry's contribution: its mask bits (set in `masks`), its hardware
/// slot claim and its BlockAck stream and sequence number.
fn evaluate(
    state: &Option<HostTxState>,
    bit: u32,
    masks: &mut Masks,
) -> (Claim, Option<(usize, u16)>) {
    let Some(state) = state else {
        return (Claim::None, None);
    };
    masks.occupied |= bit;
    let (retained, claim) = match state {
        HostTxState::Reserved {
            retained,
            reservation,
        } => (
            retained,
            Claim::Slot(reservation.pipe(), reservation.slot()),
        ),
        HostTxState::Owned { retained, hardware } => {
            match retained.phase() {
                tx::HostTxPhase::Classified if hardware.is_none() => masks.classified |= bit,
                tx::HostTxPhase::PostCryptoQueued | tx::HostTxPhase::PendingEligible
                    if hardware.is_none() =>
                {
                    masks.pending |= bit
                }
                _ => {}
            }
            let claim = match (retained.phase() == tx::HostTxPhase::Scheduled, hardware) {
                (false, _) => Claim::None,
                (true, Some(hardware)) => Claim::Slot(hardware.pipe, hardware.slot),
                (true, None) => Claim::Broken,
            };
            (retained, claim)
        }
        HostTxState::Confirming { .. } => {
            masks.confirming |= bit;
            return (Claim::None, None);
        }
    };
    (claim, tx::block_ack_stream(retained.context()))
}

/// The context indices in `mask`, lowest first.
pub(super) fn indices(mut mask: u32) -> impl Iterator<Item = usize> {
    core::iter::from_fn(move || {
        let index = mask.trailing_zeros() as usize;
        (mask != 0).then(|| {
            mask &= mask - 1;
            index
        })
    })
}
