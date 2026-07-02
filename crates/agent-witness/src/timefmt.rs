//! Deterministic, dependency-free time formatting for the CLI/TUI.
//!
//! All timeline and session-list times derive from event data (Unix epoch
//! milliseconds carried on each [`agent_witness_core::AgentEvent`]), never from
//! the wall-clock, so rendered output is a pure function of the input — which is
//! exactly what the golden-screen tests rely on (see docs/test-strategy.md).
//!
//! We avoid pulling in `chrono`/`time` for a single UTC formatter: the civil
//! calendar conversion below is Howard Hinnant's well-known `civil_from_days`
//! algorithm (<https://howardhinnant.github.io/date_algorithms.html>), which is
//! exact and total for the range we care about.

/// Milliseconds per second.
const MS_PER_SEC: i64 = 1_000;
/// Seconds per minute.
const SECS_PER_MIN: i64 = 60;
/// Seconds per hour.
const SECS_PER_HOUR: i64 = 3_600;
/// Seconds per day.
const SECS_PER_DAY: i64 = 86_400;
/// Day offset between the Unix epoch (1970-01-01) and the internal era epoch
/// (0000-03-01) used by the civil algorithm.
const DAYS_SHIFT_TO_ERA: i64 = 719_468;
/// Days in a 400-year era (the Gregorian leap cycle).
const DAYS_PER_ERA: i64 = 146_097;

/// Format an epoch-millisecond timestamp as `YYYY-MM-DD HH:MM:SSZ` (UTC).
///
/// Deterministic and locale/timezone independent — the same input always yields
/// the same string, so it is safe to assert on in golden tests.
pub fn format_utc(ms: i64) -> String {
    let secs = ms.div_euclid(MS_PER_SEC);
    let days = secs.div_euclid(SECS_PER_DAY);
    let secs_of_day = secs.rem_euclid(SECS_PER_DAY);

    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / SECS_PER_HOUR;
    let minute = (secs_of_day % SECS_PER_HOUR) / SECS_PER_MIN;
    let second = secs_of_day % SECS_PER_MIN;

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}Z")
}

/// Format a non-negative duration in milliseconds compactly: `850ms`, `1.2s`,
/// or `2m03s`. Negative inputs are clamped to zero (durations are never
/// negative; a clock going backwards must not produce nonsense).
pub fn format_duration_ms(ms: i64) -> String {
    let ms = ms.max(0);
    if ms < MS_PER_SEC {
        return format!("{ms}ms");
    }
    let total_secs = ms / MS_PER_SEC;
    if total_secs < SECS_PER_MIN {
        // One decimal of seconds, e.g. 1.2s.
        let tenths = (ms % MS_PER_SEC) / 100;
        return format!("{total_secs}.{tenths}s");
    }
    let minutes = total_secs / SECS_PER_MIN;
    let seconds = total_secs % SECS_PER_MIN;
    format!("{minutes}m{seconds:02}s")
}

/// Format an elapsed offset (milliseconds since a base) as `+S.mmms`, e.g.
/// `+0.000s` or `+12.345s`. Negative offsets are clamped to zero so an
/// out-of-order event never renders a negative timeline position.
pub fn format_offset_ms(ms: i64) -> String {
    let ms = ms.max(0);
    let secs = ms / MS_PER_SEC;
    let millis = ms % MS_PER_SEC;
    format!("+{secs}.{millis:03}s")
}

/// Convert a count of days since 1970-01-01 to a `(year, month, day)` civil
/// date (Hinnant's algorithm). `month` and `day` are 1-based.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + DAYS_SHIFT_TO_ERA;
    let era = if z >= 0 { z } else { z - (DAYS_PER_ERA - 1) } / DAYS_PER_ERA;
    let doe = z - era * DAYS_PER_ERA; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = year + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_utc_matches_known_epoch() {
        // 1_700_000_000 s == 2023-11-14T22:13:20Z (a widely-cited round epoch).
        assert_eq!(format_utc(1_700_000_000_000), "2023-11-14 22:13:20Z");
    }

    #[test]
    fn format_utc_at_unix_epoch() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00Z");
    }

    #[test]
    fn format_utc_handles_a_leap_day() {
        // 2024-02-29T12:00:00Z — verifies leap-year handling.
        let ms = 1_709_208_000_000;
        assert_eq!(format_utc(ms), "2024-02-29 12:00:00Z");
    }

    #[test]
    fn format_duration_renders_each_magnitude() {
        assert_eq!(format_duration_ms(850), "850ms");
        assert_eq!(format_duration_ms(1_200), "1.2s");
        assert_eq!(format_duration_ms(123_000), "2m03s");
    }

    #[test]
    fn format_duration_clamps_negative_to_zero() {
        assert_eq!(format_duration_ms(-5), "0ms");
    }

    #[test]
    fn format_offset_pads_millis() {
        assert_eq!(format_offset_ms(0), "+0.000s");
        assert_eq!(format_offset_ms(12_345), "+12.345s");
        assert_eq!(format_offset_ms(-1), "+0.000s");
    }
}
