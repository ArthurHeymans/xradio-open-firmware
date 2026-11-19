//! XR819 mode-0 host rate-policy storage and retry selection.
//!
//! Linux uploads eight packed WSM policies through MIB `0x1016`. The XR819
//! host path uses the 24-nibble policy layout even when the open firmware label
//! selects CW1200-compatible confirmation messages. The driver uses flags
//! `0x0c`, so firmware executes the per-frame host series rather than running
//! an independent adaptive rate controller.

pub const MIB_ID_SET_TX_RATE_RETRY_POLICY: u16 = 0x1016;
const POLICY_COUNT: usize = 8;
const POLICY_SIZE: usize = 20;
const RATE_COUNT: u8 = 24;
const TERMINATE_WHEN_FINISHED: u8 = 1 << 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TxRatePolicy {
    bytes: [u8; POLICY_SIZE],
}

impl TxRatePolicy {
    pub const fn index(self) -> u8 {
        self.bytes[0]
    }

    pub const fn short_retries(self) -> u8 {
        self.bytes[1]
    }

    pub const fn long_retries(self) -> u8 {
        self.bytes[2]
    }

    pub const fn flags(self) -> u8 {
        self.bytes[3]
    }

    pub const fn retries_for_rate(self, rate: u8) -> u8 {
        if rate >= RATE_COUNT {
            return 0;
        }
        let packed = self.bytes[8 + (rate >> 1) as usize];
        (packed >> ((rate & 1) * 4)) & 0x0f
    }

    pub const fn terminates(self) -> bool {
        self.flags() & TERMINATE_WHEN_FINISHED != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InstallError {
    InvalidLength,
    TooManyPolicies,
    InvalidIndex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryStep {
    Rearm { rate: u8 },
    GiveUp,
}

/// The host's rate policies, by index.
#[derive(Clone, Copy, Default)]
pub struct RatePolicies {
    entries: [Option<TxRatePolicy>; POLICY_COUNT],
}

impl RatePolicies {
    pub const fn new() -> Self {
        Self {
            entries: [None; POLICY_COUNT],
        }
    }

    /// Install the payload of WSM MIB `0x1016`.
    ///
    /// The wire format is a four-byte header followed by `num` packed 20-byte
    /// policies. Existing slots not named by the upload remain cached, matching
    /// the host driver's incremental policy upload.
    pub fn install(&mut self, data: &[u8]) -> Result<(), InstallError> {
        if data.len() < 4 {
            return Err(InstallError::InvalidLength);
        }
        let count = usize::from(data[0]);
        if count > POLICY_COUNT {
            return Err(InstallError::TooManyPolicies);
        }
        if data.len() != 4 + count * POLICY_SIZE {
            return Err(InstallError::InvalidLength);
        }

        if data[4..]
            .as_chunks::<POLICY_SIZE>().0.iter()
            .any(|entry| usize::from(entry[0]) >= POLICY_COUNT)
        {
            return Err(InstallError::InvalidIndex);
        }

        for entry in data[4..].as_chunks::<POLICY_SIZE>().0 {
            let index = usize::from(entry[0]);
            let mut bytes = [0_u8; POLICY_SIZE];
            bytes.copy_from_slice(entry);
            self.entries[index] = Some(TxRatePolicy { bytes });
        }
        Ok(())
    }

    pub fn get(&self, index: u8) -> Option<TxRatePolicy> {
        self.entries.get(usize::from(index)).copied().flatten()
    }
}

/// Select the rate covering the next transmission attempt.
///
/// `failed_retries` is the frame's vendor `wTryCount` before it is incremented
/// for a new re-arm. The initial transmission is represented by `+1`, matching
/// `pas_rate_for_try_count` and COUNT_INITIAL_TRANSMIT policies.
pub const fn rate_for_try_count(policy: TxRatePolicy, failed_retries: u16) -> Option<(u8, u8)> {
    let mut remaining = (failed_retries as u32).wrapping_add(1);
    let mut rate = RATE_COUNT;
    while rate != 0 {
        rate -= 1;
        let count = policy.retries_for_rate(rate);
        if count == 0 {
            continue;
        }
        if remaining < count as u32 {
            return Some((rate, count - remaining as u8));
        }
        remaining -= count as u32;
    }
    None
}

pub const fn retry_step(
    policy: TxRatePolicy,
    current_rate: u8,
    failed_retries: u16,
    long_frame: bool,
) -> RetryStep {
    let limit = if long_frame {
        policy.long_retries()
    } else {
        policy.short_retries()
    };
    if limit as u16 <= failed_retries {
        return RetryStep::GiveUp;
    }
    match rate_for_try_count(policy, failed_retries) {
        Some((rate, _)) => RetryStep::Rearm { rate },
        None if policy.terminates() => RetryStep::GiveUp,
        None => RetryStep::Rearm { rate: current_rate },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(counts: &[(u8, u8)], short_retries: u8, flags: u8) -> TxRatePolicy {
        let mut bytes = [0_u8; POLICY_SIZE];
        bytes[0] = 3;
        bytes[1] = short_retries;
        bytes[2] = short_retries;
        bytes[3] = flags;
        for &(rate, count) in counts {
            let byte = &mut bytes[8 + (rate >> 1) as usize];
            *byte |= (count & 0x0f) << ((rate & 1) * 4);
        }
        TxRatePolicy { bytes }
    }

    #[test]
    fn packed_nibbles_cover_all_rate_indices() {
        let counts = (0..24_u8).map(|rate| (rate, rate & 0x0f));
        let mut bytes = [0_u8; POLICY_SIZE];
        for (rate, count) in counts {
            bytes[8 + (rate >> 1) as usize] |= count << ((rate & 1) * 4);
        }
        let policy = TxRatePolicy { bytes };
        for rate in 0..24_u8 {
            assert_eq!(policy.retries_for_rate(rate), rate & 0x0f);
        }
    }

    #[test]
    fn mode_zero_walks_host_series_from_high_to_low() {
        let policy = policy(&[(13, 2), (12, 2), (10, 2)], 7, 0x0c);
        assert_eq!(rate_for_try_count(policy, 0), Some((13, 1)));
        assert_eq!(rate_for_try_count(policy, 1), Some((12, 2)));
        assert_eq!(rate_for_try_count(policy, 2), Some((12, 1)));
        assert_eq!(rate_for_try_count(policy, 3), Some((10, 2)));
        assert_eq!(rate_for_try_count(policy, 4), Some((10, 1)));
        assert_eq!(rate_for_try_count(policy, 5), None);
    }

    #[test]
    fn terminate_and_retry_limit_bound_the_series() {
        let terminating = policy(&[(13, 1)], 6, 0x0c);
        assert_eq!(retry_step(terminating, 13, 0, false), RetryStep::GiveUp);

        let repeating = policy(&[(13, 1)], 6, 0x08);
        assert_eq!(
            retry_step(repeating, 13, 0, false),
            RetryStep::Rearm { rate: 13 }
        );
        assert_eq!(retry_step(repeating, 13, 6, false), RetryStep::GiveUp);
    }

    #[test]
    fn intel_unicast_retry_limit_cuts_off_the_uploaded_series() {
        // Captured ordinary frame: initial WSM rate 17, slot 2, rate counts
        // 17:5, 16:5, 15:4. The cap is six re-arms, not six total starts.
        let mut mib = [0_u8; 24];
        mib[0] = 1;
        mib[4..8].copy_from_slice(&[2, 6, 6, 0x0c]);
        mib[12 + 7] = 0x40;
        mib[12 + 8] = 0x55;
        let mut policies = RatePolicies::new();
        policies.install(&mib).unwrap();
        let policy = policies.get(2).unwrap();
        let mut rate = 17;
        for (failed_retries, next) in [17, 17, 17, 17, 16, 16].into_iter().enumerate() {
            assert_eq!(
                retry_step(policy, rate, failed_retries as u16, false),
                RetryStep::Rearm { rate: next }
            );
            rate = next;
        }
        assert_eq!(retry_step(policy, rate, 6, false), RetryStep::GiveUp);
    }

    #[test]
    fn incremental_upload_preserves_unnamed_slots() {
        let mut first = [0_u8; 24];
        first[0] = 1;
        first[4] = 2;
        first[5] = 6;
        first[6] = 6;
        first[7] = 0x0c;
        first[12] = 3;
        let mut policies = RatePolicies::new();
        policies.install(&first).unwrap();
        assert_eq!(policies.get(2).unwrap().retries_for_rate(0), 3);

        let mut second = [0_u8; 24];
        second[0] = 1;
        second[4] = 4;
        second[5] = 6;
        second[6] = 6;
        second[7] = 0x0c;
        second[12] = 5;
        policies.install(&second).unwrap();
        assert_eq!(policies.get(2).unwrap().retries_for_rate(0), 3);
        assert_eq!(policies.get(4).unwrap().retries_for_rate(0), 5);
    }
}
