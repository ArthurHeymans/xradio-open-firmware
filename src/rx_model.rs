//! Hardware-free ownership model for the packet-DMA RX FIFO.
//!
//! Hardware access remains in `radio`; this module owns the cursor and token
//! invariants that must hold around those accesses.

const RELEASED_OWNERSHIP: u32 = 0xcccc_cc00;
const PENDING_OWNERSHIP: u32 = 0xffff_ff00;
const OWNERSHIP_MASK: u32 = 0xffff_ff00;

/// Unique ownership proof for one claimed zero-copy RX slot.
///
/// Tokens are deliberately neither `Copy` nor `Clone`. Only the RX ring may
/// construct one, and releasing a slot consumes it.
#[derive(Debug, Eq, PartialEq)]
pub struct RxToken {
    slot_offset: u32,
    next: u32,
}

impl RxToken {
    const fn new(slot_offset: u32, next: u32) -> Self {
        Self { slot_offset, next }
    }

    pub(crate) const fn slot_offset(&self) -> u32 {
        self.slot_offset
    }

    pub(crate) const fn next(&self) -> u32 {
        self.next
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ReleaseAction {
    AlreadyReleased,
    Pending,
    MarkPending {
        low_state: u32,
        corrupt: bool,
    },
    ReleaseHead {
        low_state: u32,
        normalize_first: bool,
        corrupt: bool,
    },
}

const MAX_DEFERRED_RESYNCS: usize = 8;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ResyncGap {
    stop_at: u32,
    jump_to: u32,
}

/// Software ownership state for the RX ring.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct RxRing {
    release_offset: u32,
    claim_offset: u32,
    host_transfer_count: u32,
    live_tokens: u32,
    resync_gaps: [ResyncGap; MAX_DEFERRED_RESYNCS],
    resync_head: u8,
    resync_len: u8,
    resync_overflow: bool,
}

impl RxRing {
    pub(crate) const fn new() -> Self {
        Self {
            release_offset: 0,
            claim_offset: 0,
            host_transfer_count: 0,
            live_tokens: 0,
            resync_gaps: [ResyncGap {
                stop_at: 0,
                jump_to: 0,
            }; MAX_DEFERRED_RESYNCS],
            resync_head: 0,
            resync_len: 0,
            resync_overflow: false,
        }
    }

    const fn has_deferred_resync(&self) -> bool {
        self.resync_len != 0 || self.resync_overflow
    }

    pub(crate) fn activity(
        &self,
        activity: crate::activity::Activity,
    ) -> crate::activity::Activity {
        activity
            .with(self.release_offset)
            .with(self.claim_offset)
            .with(self.host_transfer_count)
            .with(self.live_tokens)
            .with(u32::from(self.has_deferred_resync()))
    }

    pub(crate) fn synchronize(&mut self, offset: u32) {
        self.release_offset = offset;
        self.claim_offset = offset;
        self.host_transfer_count = 0;
        self.live_tokens = 0;
        self.resync_head = 0;
        self.resync_len = 0;
        self.resync_overflow = false;
    }

    pub(crate) const fn release_offset(&self) -> u32 {
        self.release_offset
    }

    pub(crate) const fn claim_offset(&self) -> u32 {
        self.claim_offset
    }

    pub(crate) const fn host_transfer_count(&self) -> u32 {
        self.host_transfer_count
    }

    pub(crate) const fn live_tokens(&self) -> u32 {
        self.live_tokens
    }

    pub(crate) const fn sweep_limit(&self) -> u32 {
        if self.resync_len != 0 {
            self.resync_gaps[self.resync_head as usize].stop_at
        } else {
            self.claim_offset
        }
    }

    /// Claims the current ring head and advances to `next`.
    ///
    /// The caller cannot choose the token identity independently of the ring's
    /// current claim cursor.
    pub(crate) fn claim(&mut self, next: u32) -> RxToken {
        let token = RxToken::new(self.claim_offset, next);
        self.claim_offset = next;
        self.live_tokens += 1;
        token
    }

    pub(crate) fn record_token_released(&mut self) {
        self.live_tokens = self.live_tokens.saturating_sub(1);
    }

    pub(crate) fn publish_host_transfer(&mut self, limit: u32) -> bool {
        if self.host_transfer_count >= limit {
            return false;
        }
        self.host_transfer_count += 1;
        true
    }

    pub(crate) fn complete_host_transfer(&mut self) -> bool {
        if self.host_transfer_count == 0 {
            return false;
        }
        self.host_transfer_count -= 1;
        true
    }

    #[cfg(test)]
    pub(crate) fn set_claim_offset(&mut self, next: u32) {
        self.claim_offset = next;
    }

    /// Records a parser resynchronization and returns the release cursor update
    /// that may be applied after the claim-cursor hardware write.
    pub(crate) fn resynchronize(&mut self, previous_claim: u32, target: u32) -> Option<u32> {
        self.claim_offset = target;
        if !self.has_deferred_resync()
            && self.host_transfer_count == 0
            && self.live_tokens == 0
            && self.release_offset == previous_claim
        {
            return Some(target);
        }
        if !self.resync_overflow {
            let len = usize::from(self.resync_len);
            if len > 0 {
                let tail = (usize::from(self.resync_head) + len - 1) % MAX_DEFERRED_RESYNCS;
                if self.resync_gaps[tail].jump_to == previous_claim {
                    self.resync_gaps[tail].jump_to = target;
                    return None;
                }
            }
            if len < MAX_DEFERRED_RESYNCS {
                let tail = (usize::from(self.resync_head) + len) % MAX_DEFERRED_RESYNCS;
                self.resync_gaps[tail] = ResyncGap {
                    stop_at: previous_claim,
                    jump_to: target,
                };
                self.resync_len = (len + 1) as u8;
                return None;
            }
            self.resync_overflow = true;
        }
        None
    }

    pub(crate) fn set_release_offset(&mut self, next: u32) {
        self.release_offset = next;
    }

    /// Finishes the next deferred resynchronization once the release sweep
    /// reaches its `stop_at` boundary, or jumps directly to `claim_offset` if
    /// all live tokens and host transfers have drained after a queue overflow.
    /// The caller performs the matching hardware-consumer write and resumes
    /// sweeping valid slots after the returned offset.
    pub(crate) fn finish_deferred_resync(&mut self) -> Option<u32> {
        if self.resync_len != 0 {
            let head = usize::from(self.resync_head);
            let gap = self.resync_gaps[head];
            if self.release_offset == gap.stop_at {
                self.resync_head = ((head + 1) % MAX_DEFERRED_RESYNCS) as u8;
                self.resync_len -= 1;
                self.release_offset = gap.jump_to;
                return Some(gap.jump_to);
            }
        }
        if self.has_deferred_resync() && self.live_tokens == 0 && self.host_transfer_count == 0 {
            self.resync_head = 0;
            self.resync_len = 0;
            self.resync_overflow = false;
            self.release_offset = self.claim_offset;
            return Some(self.release_offset);
        }
        None
    }

    pub(crate) const fn classify_release(&self, token: &RxToken, state: u32) -> ReleaseAction {
        let ownership = state & OWNERSHIP_MASK;
        let low_state = state & 0xff;
        if ownership == RELEASED_OWNERSHIP {
            return ReleaseAction::AlreadyReleased;
        }

        let pending = ownership == PENDING_OWNERSHIP;
        let corrupt = ownership != 0 && !pending;
        if token.slot_offset != self.release_offset {
            if pending {
                ReleaseAction::Pending
            } else {
                ReleaseAction::MarkPending { low_state, corrupt }
            }
        } else {
            ReleaseAction::ReleaseHead {
                low_state,
                normalize_first: !pending,
                corrupt,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claim_token_identity_comes_from_the_current_head() {
        let mut ring = RxRing::new();
        ring.synchronize(0x100);

        let first = ring.claim(0x180);
        let second = ring.claim(0x240);

        assert_eq!(first.slot_offset(), 0x100);
        assert_eq!(first.next(), 0x180);
        assert_eq!(second.slot_offset(), 0x180);
        assert_eq!(second.next(), 0x240);
        assert_eq!(ring.claim_offset(), 0x240);
    }

    #[test]
    fn corrupt_release_head_is_reclaimed_in_the_same_call() {
        let mut ring = RxRing::new();
        ring.synchronize(0x100);
        let token = ring.claim(0x180);

        assert_eq!(
            ring.classify_release(&token, 0x1234_56a5),
            ReleaseAction::ReleaseHead {
                low_state: 0xa5,
                normalize_first: true,
                corrupt: true,
            }
        );
    }

    #[test]
    fn out_of_order_release_is_marked_pending() {
        let mut ring = RxRing::new();
        ring.synchronize(0x100);
        ring.set_claim_offset(0x180);
        let token = ring.claim(0x200);

        assert_eq!(
            ring.classify_release(&token, 0x0000_00a5),
            ReleaseAction::MarkPending {
                low_state: 0xa5,
                corrupt: false,
            }
        );
    }

    #[test]
    fn resync_never_releases_across_host_owned_slots() {
        let mut ring = RxRing::new();
        ring.synchronize(0x100);
        assert!(ring.publish_host_transfer(24));

        assert_eq!(ring.resynchronize(0x100, 0x240), None);
        assert_eq!(ring.release_offset(), 0x100);
        assert_eq!(ring.claim_offset(), 0x240);
        assert!(ring.complete_host_transfer());
        assert_eq!(ring.finish_deferred_resync(), Some(0x240));
    }

    #[test]
    fn resync_advances_immediately_when_the_ring_is_unowned() {
        let mut ring = RxRing::new();
        ring.synchronize(0x100);

        assert_eq!(ring.resynchronize(0x100, 0x240), Some(0x240));
        ring.set_release_offset(0x240);
        assert_eq!(ring.release_offset(), 0x240);
        assert_eq!(ring.claim_offset(), 0x240);
    }

    #[test]
    fn host_transfer_limit_is_exact() {
        let mut ring = RxRing::new();
        for _ in 0..24 {
            assert!(ring.publish_host_transfer(24));
        }
        assert!(!ring.publish_host_transfer(24));
        for _ in 0..24 {
            assert!(ring.complete_host_transfer());
        }
        assert!(!ring.complete_host_transfer());
    }

    #[test]
    fn stacked_non_adjacent_resync_gaps_preserve_ordered_sweeps_across_wrap() {
        let mut ring = RxRing::new();
        ring.synchronize(0x7e00);

        // Pre-gap reorder-owned slot: 0x7e00 -> 0x7f00.
        let s0 = ring.claim(0x7f00);
        // Gap 1 wraps across 0x8000: 0x7f00 -> 0x0100.
        assert_eq!(ring.resynchronize(0x7f00, 0x0100), None);
        assert_eq!(ring.sweep_limit(), 0x7f00);

        // Interleaved host-owned slot between Gap 1 and Gap 2: 0x0100 -> 0x0200.
        let s1 = ring.claim(0x0200);
        assert!(ring.publish_host_transfer(24));
        // Gap 2: 0x0200 -> 0x0380.
        assert_eq!(ring.resynchronize(0x0200, 0x0380), None);
        assert_eq!(ring.sweep_limit(), 0x7f00);

        // Post-Gap-2 reorder-owned slot: 0x0380 -> 0x0480.
        let s2 = ring.claim(0x0480);

        // Out-of-order release: s2 and s1 finish before s0.
        assert!(matches!(
            ring.classify_release(&s2, 0),
            ReleaseAction::MarkPending { .. }
        ));
        ring.record_token_released();
        assert_eq!(ring.finish_deferred_resync(), None);

        assert!(ring.complete_host_transfer());
        assert!(matches!(
            ring.classify_release(&s1, 0),
            ReleaseAction::MarkPending { .. }
        ));
        ring.record_token_released();
        assert_eq!(ring.finish_deferred_resync(), None);

        // Now release head s0: sweep reaches Gap 1 stop_at (0x7f00) and jumps to 0x0100.
        assert!(matches!(
            ring.classify_release(&s0, 0),
            ReleaseAction::ReleaseHead { .. }
        ));
        ring.record_token_released();
        ring.set_release_offset(s0.next());
        assert_eq!(ring.release_offset(), ring.sweep_limit());
        assert_eq!(ring.finish_deferred_resync(), Some(0x0100));

        // Sweep resumes at 0x0100 up to Gap 2 stop_at (0x0200), cleaning up s1!
        assert_eq!(ring.release_offset(), 0x0100);
        assert_eq!(ring.sweep_limit(), 0x0200);
        ring.set_release_offset(s1.next());
        assert_eq!(ring.release_offset(), ring.sweep_limit());
        assert_eq!(ring.finish_deferred_resync(), Some(0x0380));

        // Sweep resumes at 0x0380 up to claim_offset (0x0480), cleaning up s2!
        assert_eq!(ring.release_offset(), 0x0380);
        assert_eq!(ring.sweep_limit(), 0x0480);
        ring.set_release_offset(s2.next());
        assert_eq!(ring.release_offset(), 0x0480);
        assert_eq!(ring.finish_deferred_resync(), None);
    }
}
