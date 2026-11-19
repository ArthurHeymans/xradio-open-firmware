//! Fingerprints of the main loop's state, to tell a pass that moved
//! nothing from one that did: only after such a pass may the firmware
//! sleep until an interrupt source is pending.

/// A fold of the words that change whenever an owner makes progress:
/// ring cursors, job states, counts of serviced events. A collision costs
/// one sleep that ends at the wake bound.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Activity(u32);

impl Activity {
    pub const fn new() -> Self {
        Self(0x811c_9dc5)
    }

    #[must_use]
    pub const fn with(self, word: u32) -> Self {
        Self((self.0 ^ word).wrapping_mul(0x0100_0193))
    }
}

impl Default for Activity {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_and_value_both_count() {
        let a = Activity::new().with(1).with(2);
        assert_eq!(a, Activity::new().with(1).with(2));
        assert_ne!(a, Activity::new().with(2).with(1));
        assert_ne!(a, Activity::new().with(1).with(3));
    }
}
