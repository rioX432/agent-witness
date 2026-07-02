//! Time source abstraction. Core logic never reads the wall-clock directly;
//! callers inject a [`Clock`] so pure paths stay deterministic and testable.

/// A source of the current time, in Unix epoch milliseconds.
pub trait Clock {
    /// Current time as Unix epoch milliseconds.
    fn now_ms(&self) -> i64;
}

/// Wall-clock [`Clock`] backed by the operating system. Use only at the edges
/// (the binary), never inside pure core logic.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        // Times before the epoch are not expected on real systems; clamp to 0
        // rather than panic to keep this total.
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_millis()).unwrap_or(i64::MAX),
            Err(_) => 0,
        }
    }
}

/// A [`Clock`] that always returns a fixed time. Useful in tests.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub i64);

impl Clock for FixedClock {
    fn now_ms(&self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_returns_its_value() {
        let clock = FixedClock(42);
        assert_eq!(clock.now_ms(), 42);
    }

    #[test]
    fn system_clock_is_positive_and_after_2020() {
        // 2020-01-01T00:00:00Z in ms; sanity floor, not an exact assertion.
        const YEAR_2020_MS: i64 = 1_577_836_800_000;
        assert!(SystemClock.now_ms() > YEAR_2020_MS);
    }
}
