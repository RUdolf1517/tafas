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

/// The ceiling for `--max-filesize`: a petabyte, which is far past any real
/// file and well inside `u64`.
pub const MAX_SIZE: u64 = 1024 * 1024 * 1024 * 1024 * 1024;

/// `--limit-rate RATE`: bytes per second, with an optional `K`, `M`, `G` or `T`
/// suffix in powers of 1024 (`500K`, `2M`, `1.5M`, `500KB`). A terabyte per
/// second parses and is then refused by the ceiling below, which is a clearer
/// answer than "that suffix is not allowed".
///
/// Parsed and bounded before the first request, because a rate the program
/// quietly changed would be a lie about what it did.
pub fn parse(text: &str) -> Result<u64, String> {
    let bytes = scaled(text, "--limit-rate", "bytes per second")?;
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

/// `--max-filesize SIZE`: bytes, with the same suffixes and the same grammar.
///
/// The one difference from a rate is what zero means. A rate of zero is a typo,
/// so it is refused by the floor; a size of zero is a coherent request — it
/// refuses every file — and inventing a floor for it would be the program
/// deciding what the user meant. The ceiling is a petabyte, which is far past
/// any real file and well inside `u64`, so an absurd number is refused rather
/// than wrapped.
pub fn parse_size(text: &str, flag: &str) -> Result<u64, String> {
    let bytes = scaled(text, flag, "a size")?;
    if bytes > MAX_SIZE as f64 {
        return Err(format!(
            "{flag} is at most 1P ({MAX_SIZE} bytes), not {text:?}"
        ));
    }
    Ok(bytes as u64)
}

/// The number-and-suffix grammar, shared so the two flags cannot drift.
///
/// `flag` and `what` are in the message rather than in a `match` at the call
/// site: one grammar, two spellings of what it is measuring, and the number in
/// the error is the number the user typed.
fn scaled(text: &str, flag: &str, what: &str) -> Result<f64, String> {
    let trimmed = text.trim();
    let digits = match trimmed.as_bytes().last().copied() {
        Some(b'b') | Some(b'B') => &trimmed[..trimmed.len() - 1],
        _ => trimmed,
    };
    let (number, scale) = match digits.as_bytes().last().copied() {
        Some(b'k') | Some(b'K') => (&digits[..digits.len() - 1], 1024u64),
        Some(b'm') | Some(b'M') => (&digits[..digits.len() - 1], 1024u64 * 1024),
        Some(b'g') | Some(b'G') => (&digits[..digits.len() - 1], 1024u64 * 1024 * 1024),
        // `T` is here for `--max-filesize`, where a terabyte is a real answer.
        // Through `--limit-rate` it is a number the rate ceiling refuses, which
        // is the same answer in fewer words than a second grammar would give.
        Some(b't') | Some(b'T') => (&digits[..digits.len() - 1], 1024u64 * 1024 * 1024 * 1024),
        _ => (digits, 1u64),
    };
    let number = number.trim();
    let value: f64 = match number.parse::<f64>() {
        // Zero is allowed here and judged by the caller, so that a rate of zero
        // is told about the floor rather than about the syntax.
        Ok(value) if value.is_finite() && value >= 0.0 => value,
        _ => return Err(wrong_shape(text, flag, what)),
    };
    Ok((value * scale as f64).round())
}

fn wrong_shape(text: &str, flag: &str, what: &str) -> String {
    format!(
        "{flag} takes {what} with an optional K, M, G or T suffix (500K, 2M, 1.5G), \
         not {text:?}"
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
    fn a_size_shares_the_grammar_and_differs_only_in_what_zero_means() {
        assert_eq!(parse_size("0", "--max-filesize"), Ok(0));
        assert_eq!(parse_size("500K", "--max-filesize"), Ok(512_000));
        assert_eq!(parse_size("1.5G", "--max-filesize"), Ok(1_610_612_736));
        // The same number through the rate parser is refused, because a rate of
        // half a kilobyte a second is a typo and a size of it is not.
        assert!(parse("1").is_err());
        assert_eq!(parse_size("1", "--max-filesize"), Ok(1));
        // Above the petabyte ceiling it is refused rather than wrapped.
        let error = parse_size("2000T", "--max-filesize").expect_err("too large");
        assert!(error.contains("1P"), "{error}");
        // And the message names the flag that was actually used.
        let error = parse_size("fast", "--max-filesize").expect_err("not a size");
        assert!(error.contains("--max-filesize"), "{error}");
        assert!(!error.contains("--limit-rate"), "{error}");
    }

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
