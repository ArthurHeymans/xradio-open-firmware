//! Receive reordering for one BlockAck session.
//!
//! Frames leave in sequence order. A missing frame holds the frames after it
//! until it arrives, the sender moves the window past it (a frame beyond the
//! window or a BlockAckReq), or the owner gives it up. A frame that arrives
//! after its place was passed still goes out, late; only duplicates of the
//! last 64 delivered frames are dropped: giving up a hole is our decision,
//! and the sender often retransmits its frame afterwards.

/// What happens to a frame handed to [`Reorder`].
#[derive(Debug, Eq, PartialEq)]
pub enum Released<T> {
    /// Next in sequence: pass it on.
    Deliver(T),
    /// A duplicate of a delivered frame: discard it.
    Drop(T),
}

/// A reorder window of up to `N` frames starting at the next expected
/// sequence number.
pub struct Reorder<T, const N: usize> {
    head: u16,
    window: u16,
    held: [Option<(u16, T)>; N],
    count: usize,
    /// Bit `i`: sequence number `head - 1 - i` was delivered.
    delivered: u64,
}

const SEQUENCE_MASK: u16 = 0x0fff;

/// Distance from `from` forward to `to`, modulo 4096.
const fn ahead(from: u16, to: u16) -> u16 {
    to.wrapping_sub(from) & SEQUENCE_MASK
}

/// Distances in the upper half of the sequence space lie behind.
const fn is_behind(distance: u16) -> bool {
    distance >= 0x0800
}

impl<T, const N: usize> Reorder<T, N> {
    /// A window of `window` frames (at most `N`) starting at `head`.
    pub fn new(head: u16, window: u16) -> Self {
        Self {
            head: head & SEQUENCE_MASK,
            window: window.clamp(1, N as u16),
            held: core::array::from_fn(|_| None),
            count: 0,
            delivered: 0,
        }
    }

    /// The next expected sequence number.
    pub const fn head(&self) -> u16 {
        self.head
    }

    /// Frames waiting behind a hole.
    pub const fn held(&self) -> usize {
        self.count
    }

    fn slot(sequence: u16) -> usize {
        usize::from(sequence) % N
    }

    /// Admit the frame with sequence number `sequence`.
    pub fn receive(&mut self, sequence: u16, item: T, out: &mut impl FnMut(Released<T>)) {
        let sequence = sequence & SEQUENCE_MASK;
        let mut distance = ahead(self.head, sequence);
        if is_behind(distance) {
            let back = ahead(sequence, self.head);
            let bit = 1_u64.checked_shl(u32::from(back) - 1).unwrap_or(0);
            if self.delivered & bit != 0 {
                return out(Released::Drop(item));
            }
            self.delivered |= bit;
            return out(Released::Deliver(item));
        }
        if distance >= self.window {
            // The sender moved on: the window now ends at this frame.
            self.advance_to(sequence.wrapping_sub(self.window - 1) & SEQUENCE_MASK, out);
            distance = ahead(self.head, sequence);
        }
        if distance == 0 {
            out(Released::Deliver(item));
            self.step(true);
            return self.drain(out);
        }
        let slot = &mut self.held[Self::slot(sequence)];
        if slot.is_some() {
            return out(Released::Drop(item));
        }
        *slot = Some((sequence, item));
        self.count += 1;
    }

    /// Move the head to `sequence` (a BlockAckReq, or a hole given up):
    /// frames before it go out in order, holes are skipped.
    pub fn advance_to(&mut self, sequence: u16, out: &mut impl FnMut(Released<T>)) {
        let sequence = sequence & SEQUENCE_MASK;
        let distance = ahead(self.head, sequence);
        if is_behind(distance) {
            return;
        }
        for _ in 0..distance.min(N as u16) {
            let delivered = self.take_head(out);
            self.step(delivered);
        }
        // Beyond the buffer nothing was held: those are all holes.
        let rest = u32::from(distance.saturating_sub(N as u16));
        self.delivered = self.delivered.checked_shl(rest).unwrap_or(0);
        self.head = sequence;
        self.drain(out);
    }

    /// Give up the first hole: move the head to the oldest held frame.
    pub fn skip_hole(&mut self, out: &mut impl FnMut(Released<T>)) {
        if let Some(first) = (0..self.window)
            .map(|distance| (self.head + distance) & SEQUENCE_MASK)
            .find(|&sequence| matches!(self.held[Self::slot(sequence)], Some((held, _)) if held == sequence))
        {
            self.advance_to(first, out);
        }
    }

    /// Hand out every held frame in order (session teardown).
    pub fn flush(&mut self, out: &mut impl FnMut(Released<T>)) {
        while self.count != 0 {
            self.skip_hole(out);
        }
    }

    fn take_head(&mut self, out: &mut impl FnMut(Released<T>)) -> bool {
        let slot = &mut self.held[Self::slot(self.head)];
        match slot.take() {
            Some((sequence, item)) if sequence == self.head => {
                self.count -= 1;
                out(Released::Deliver(item));
                true
            }
            other => {
                *slot = other;
                false
            }
        }
    }

    /// Move the head by one; `delivered` tells whether its frame went out.
    fn step(&mut self, delivered: bool) {
        self.delivered = self.delivered << 1 | u64::from(delivered);
        self.head = (self.head + 1) & SEQUENCE_MASK;
    }

    fn drain(&mut self, out: &mut impl FnMut(Released<T>)) {
        while self.take_head(out) {
            self.step(true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    fn run(reorder: &mut Reorder<u16, 16>, frames: &[u16]) -> (Vec<u16>, Vec<u16>) {
        let (mut delivered, mut dropped) = (Vec::new(), Vec::new());
        for &sequence in frames {
            reorder.receive(sequence, sequence, &mut |released| match released {
                Released::Deliver(item) => delivered.push(item),
                Released::Drop(item) => dropped.push(item),
            });
        }
        (delivered, dropped)
    }

    #[test]
    fn a_hole_holds_later_frames_until_it_fills() {
        let mut reorder = Reorder::new(100, 16);
        let (delivered, dropped) = run(&mut reorder, &[100, 102, 103, 101, 104]);
        assert_eq!(delivered, [100, 101, 102, 103, 104]);
        assert!(dropped.is_empty());
        assert_eq!((reorder.head(), reorder.held()), (105, 0));
    }

    #[test]
    fn duplicates_are_dropped_and_late_frames_delivered() {
        let mut reorder = Reorder::new(10, 16);
        let (delivered, dropped) = run(&mut reorder, &[10, 12, 12, 10, 9, 9]);
        assert_eq!(delivered, [10, 9]);
        assert_eq!(dropped, [12, 10, 9]);
    }

    #[test]
    fn a_frame_beyond_the_window_moves_it_past_the_hole() {
        let mut reorder = Reorder::new(0, 16);
        // 0 is lost; 1..=15 wait; 16 is beyond 0 + 16.
        let frames: Vec<u16> = (1..=16).collect();
        let (delivered, _) = run(&mut reorder, &frames);
        assert_eq!(delivered, frames);
        assert_eq!(reorder.head(), 17);
        // 0 still comes, late; a second copy of 5 does not.
        let (delivered, dropped) = run(&mut reorder, &[0, 5]);
        assert_eq!((delivered, dropped), (std::vec![0], std::vec![5]));
    }

    #[test]
    fn a_block_ack_request_releases_what_it_passes() {
        let mut reorder = Reorder::new(4090, 16);
        let (delivered, _) = run(&mut reorder, &[4091, 4093, 0]);
        assert!(delivered.is_empty());
        let mut released = Vec::new();
        // Wraps: 4090 and 4092 are given up.
        reorder.advance_to(4094, &mut |item| released.push(item));
        assert_eq!(released, [Released::Deliver(4091), Released::Deliver(4093)]);
        let (delivered, _) = run(&mut reorder, &[4094, 4095]);
        assert_eq!(delivered, [4094, 4095, 0]);
    }

    #[test]
    fn skip_hole_and_flush_deliver_in_order() {
        let mut reorder = Reorder::new(0, 16);
        run(&mut reorder, &[2, 3, 6]);
        let mut released = Vec::new();
        reorder.skip_hole(&mut |item| released.push(item));
        assert_eq!(released, [Released::Deliver(2), Released::Deliver(3)]);
        reorder.flush(&mut |item| released.push(item));
        assert_eq!(released.last(), Some(&Released::Deliver(6)));
        assert_eq!((reorder.head(), reorder.held()), (7, 0));
    }
}
