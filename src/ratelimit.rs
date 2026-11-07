//! `--limit-rate`: one budget for every media byte the transfer path writes.
//!
//! The rate is an average over the whole run, not a delay per chunk: each chunk
//! reserves the time it is worth in a queue, and the worker waits until that
//! time arrives. The reservation is arithmetic under a lock and the wait
//! happens after the lock is released, so `--concurrent-fragments` still
//! overlaps — four workers share one budget and none of them sleeps inside it.
//!
//! What is charged: the body of a progressive file and every segment of a list.
//! What is not: the page, the JSON and the thumbnail — the handful of requests
//! that decide what to fetch are not what a shared line is spent on.

use std::time::{Duration, Instant};

/// Below this a download is slower than the timeouts around it, so the number
/// is a typo rather than a request. One kilobyte a second is the floor.
pub const MIN_BYTES_PER_SECOND: u64 = 1024;

/// Above this the flag asks for nothing: a gigabyte a second is already
/// "unlimited" for one connection. Bounded so the arithmetic cannot overflow.
pub const MAX_BYTES_PER_SECOND: u64 = 1024 * 1024 * 1024;

/// `--limit-rate RATE`: bytes per second, with an optional `K`, `M` or `G`
/// suffix in powers of 1024 (`500K`, `2M`, `1.5M`, `500KB`).
///
/// Parsed and bounded before the first request, because a rate the program
/// quietly changed would be a lie about what it did.
pub fn parse(text: &str) -> Result<u64, String> {
    let trimmed = text.trim();
    let digits = match trimmed.as_bytes().last().copied() {
        Some(b'b') | Some(b'B') => &trimmed[..trimmed.len() - 1],
        _ => trimmed,
    };
    let (number, scale) = match digits.as_bytes().last().copied() {
        Some(b'k') | Some(b'K') => (&digits[..digits.len() - 1], 1024u64),
        Some(b'm') | Some(b'M') => (&digits[..digits.len() - 1], 1024u64 * 1024),
        Some(b'g') | Some(b'G') => (&digits[..digits.len() - 1], 1024u64 * 1024 * 1024),
        _ => (digits, 1u64),
    };
    let number = number.trim();
    let value: f64 = match number.parse::<f64>() {
        // Zero is allowed here and caught by the floor below, so that a rate of
        // zero is told about the floor rather than about the syntax.
        Ok(value) if value.is_finite() && value >= 0.0 => value,
        _ => return Err(wrong_shape(text)),
    };
    let bytes = (value * scale as f64).round();
    if bytes < MIN_BYTES_PER_SECOND as f64 {
        return Err(format!(
            "--limit-rate is at least {MIN_BYTES_PER_SECOND} bytes per second (1K), \
             not {text:?}"
        ));
    }
    if bytes > MAX_BYTES_PER_SECOND as f64 {
        return Err(format!(
            "--limit-rate is at most 1G ({} bytes per second), not {text:?}",
            MAX_BYTES_PER_SECOND
        ));
    }
    Ok(bytes as u64)
}

fn wrong_shape(text: &str) -> String {
    format!(
        "--limit-rate takes bytes per second with an optional K, M or G suffix \
         (500K, 2M, 1.5M), not {text:?}"
    )
}

/// The budget every transfer in one run is charged against.
///
/// `next` is the earliest instant the queue is free again. Reserving a chunk
/// pushes it forward by the chunk's worth of time and returns how long the
/// caller has to wait — which is zero while the queue is still in the past, so
/// the first chunk goes out immediately and the average settles after it.
pub struct Budget {
    bytes_per_second: u64,
    next: Instant,
}

impl Budget {
    pub fn new(bytes_per_second: u64) -> Budget {
        Budget {
            // Clamped rather than trusted: `cli` refuses a rate outside these
            // bounds, and a library caller gets the nearest sane one.
            bytes_per_second: bytes_per_second.clamp(MIN_BYTES_PER_SECOND, MAX_BYTES_PER_SECOND),
            next: Instant::now(),
        }
    }

    pub fn bytes_per_second(&self) -> u64 {
        self.bytes_per_second
    }

    /// Charges `bytes` to the budget and says how long to wait before the next
    /// read. Never sleeps: the caller decides when, and it must do so with no
    /// lock held.
    pub fn reserve(&mut self, bytes: u64) -> Duration {
        if bytes == 0 {
            return Duration::ZERO;
        }
        let now = Instant::now();
        let start = if self.next > now { self.next } else { now };
        let seconds = bytes as f64 / self.bytes_per_second as f64;
        let end = start + Duration::from_secs_f64(seconds);
        self.next = end;
        start.saturating_duration_since(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_is_bytes_with_an_optional_suffix() {
        assert_eq!(parse("20000"), Ok(20000));
        assert_eq!(parse(" 20000 "), Ok(20000));
        assert_eq!(parse("500K"), Ok(512_000));
        assert_eq!(parse("500kb"), Ok(512_000));
        assert_eq!(parse("2M"), Ok(2 * 1024 * 1024));
        assert_eq!(parse("1.5M"), Ok(1_572_864));
        assert_eq!(parse("1g"), Ok(1024 * 1024 * 1024));
        assert_eq!(parse("1K"), Ok(1024));
    }

    #[test]
    fn a_rate_outside_the_range_is_a_usage_error_that_names_the_range() {
        for bad in ["0", "1", "-5K", "5G", "fast", "", "K", "1M2"] {
            let error = parse(bad).expect_err(bad);
            assert!(error.contains("--limit-rate"), "{bad}: {error}");
        }
        assert!(parse("0").expect_err("zero").contains("at least"));
        assert!(parse("5G").expect_err("five").contains("at most"));
        assert!(parse("fast").expect_err("words").contains("500K"));
    }

    /// No sleeping: the arithmetic is the promise, so it is what is checked.
    /// At 100 kB/s a 64 KiB chunk is worth 0.655 s.
    #[test]
    fn a_chunk_reserves_its_own_time_and_the_first_one_does_not_wait() {
        let mut budget = Budget::new(100_000);
        assert_eq!(budget.reserve(0), Duration::ZERO);
        assert_eq!(
            budget.reserve(64 * 1024),
            Duration::ZERO,
            "the queue started in the past, so the first chunk owes nothing"
        );
        let second = budget.reserve(64 * 1024);
        let expected = 65_536.0 / 100_000.0;
        assert!(
            (second.as_secs_f64() - expected).abs() < 0.01,
            "second chunk waited {second:?}, expected about {expected} s"
        );
    }

    /// Two workers share one queue, so their reservations are one after the
    /// other rather than each starting from zero.
    #[test]
    fn one_budget_is_shared_by_every_reservation() {
        let mut budget = Budget::new(1000);
        let first = budget.reserve(100);
        let second = budget.reserve(100);
        assert_eq!(first, Duration::ZERO);
        assert!(
            second >= Duration::from_millis(90),
            "the second reservation is charged behind the first, got {second:?}"
        );
    }
}
