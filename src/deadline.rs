//! Deadlines on the free-running microsecond clock, which wraps.

/// Microseconds from `now` until `deadline`; zero once it has passed.
/// Deadlines lie less than 2^31 us (35 minutes) ahead.
pub const fn until(deadline: u32, now: u32) -> u32 {
    let left = deadline.wrapping_sub(now) as i32;
    if left > 0 { left as u32 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_left_counts_across_the_wrap_and_stops_at_zero() {
        assert_eq!(until(u32::MAX, u32::MAX - 10), 10);
        assert_eq!(until(5, u32::MAX - 4), 10);
        assert_eq!(until(100, 100), 0);
        assert_eq!(until(100, 150), 0);
    }
}
