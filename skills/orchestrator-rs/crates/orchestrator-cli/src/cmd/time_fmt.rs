//! Pure RFC3339 (UTC, `Z`-suffixed) timestamp parsing and Go-style
//! duration/date-time formatting.
//!
//! `status.go` and `events.go` render timestamps through `time.Time.Local()`
//! (`internal/cmd/status.go:181`, `internal/cmd/events.go:281`) — the host's
//! configured timezone. This workspace has no timezone-database dependency
//! (`PORTING.md` §2.9/WORKER RULE 2: `std::time` only, no chrono/time-rs), so
//! this module renders in UTC instead. **Flagged divergence**: on a host
//! whose local timezone is not UTC, the wall-clock strings this module
//! prints differ from Go's. Every timestamp this codebase's writers produce
//! is itself UTC (`Z`-suffixed), so only the *display* differs, not the
//! underlying instant.

/// Parses a `YYYY-MM-DDTHH:MM:SS[.fraction]Z` timestamp into Unix seconds.
/// Returns `None` for anything else (non-UTC offsets, malformed input, or a
/// non-`Z` timezone suffix) — every timestamp this crate's callers observe
/// (event/checkpoint timestamps) is `Z`-suffixed UTC.
pub(crate) fn parse_rfc3339_utc_to_unix_seconds(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        std::str::from_utf8(bytes.get(range)?).ok()?.parse().ok()
    };
    let year = number(0..4)?;
    let month = number(5..7)?;
    let day = number(8..10)?;
    let hour = number(11..13)?;
    let minute = number(14..16)?;
    let second = number(17..19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut offset = 19;
    if bytes.get(offset) == Some(&b'.') {
        offset += 1;
        let fraction_start = offset;
        while bytes.get(offset).is_some_and(u8::is_ascii_digit) {
            offset += 1;
        }
        if offset == fraction_start {
            return None;
        }
    }
    if bytes.get(offset) != Some(&b'Z') || offset + 1 != bytes.len() {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Howard Hinnant's `days_from_civil`: days since the Unix epoch
/// (1970-01-01) for a proleptic-Gregorian calendar date. `m` is 1-12.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let year_of_era = y - era * 400; // [0, 399]
    let month_prime = (month + 9) % 12; // [0, 11], Mar=0
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1; // [0, 365]
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`]: the proleptic-Gregorian `(year, month, day)`
/// for `days` since the Unix epoch.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

/// Formats Unix seconds as `YYYY-MM-DD HH:MM:SS` — matches the layout of
/// Go's `"2006-01-02 15:04:05"` (`internal/cmd/status.go:181`), rendered in
/// UTC rather than the host's local zone (see module doc comment).
pub(crate) fn format_unix_seconds_ymd_hms(total_seconds: i64) -> String {
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` — Go's `time.RFC3339` for a
/// UTC instant with no sub-second part, which is the layout
/// `learning.DB.Cleanup` and `CountEmbeddingBackfill` compare `created_at`
/// against (`internal/learning/db.go`). The two layouts differ by more than
/// cosmetics: SQLite compares `created_at` as text, so the `T`/`Z` here and
/// the space in [`format_unix_seconds_ymd_hms`] (SQLite's own
/// `datetime('now', ...)` layout, used by `ArchiveDeadWeight`) must each be
/// reproduced where Go uses them.
pub(crate) fn format_unix_seconds_rfc3339(total_seconds: i64) -> String {
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3600;
    let minute = (seconds_of_day % 3600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Formats a whole-second, non-negative duration the way Go's
/// `time.Duration.String()` renders a value already `Truncate`d to the
/// second (`internal/cmd/status.go:174`): `h`/`m`/`s` unit letters, hours can
/// exceed 24 (no calendar rollover), and units below the leading nonzero one
/// are always shown while units above an all-zero duration are omitted
/// entirely (`0s` for zero). Negative input is clamped to zero — Go's
/// `time.Since` of a future `startedAt` cannot occur for this codebase's
/// call sites (mission start always precedes "now").
pub(crate) fn format_go_duration_seconds(total_seconds: i64) -> String {
    let total_seconds = total_seconds.max(0);
    let hours = total_seconds / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h{minutes}m{seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m{seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// Extracts `HH:MM:SS.mmm` directly from an RFC3339 string's own text
/// (bytes 11..19 for `HH:MM:SS`, then up to 3 fractional-second digits,
/// zero-padded) rather than re-deriving it through epoch math. Ports the
/// intent of Go's `ev.Timestamp.Local().Format("15:04:05.000")`
/// (`internal/cmd/events.go:281`) for the one field `formatEventLine` needs;
/// same UTC-not-Local divergence as the rest of this module.
pub(crate) fn hms_millis_from_rfc3339(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() < 19
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || !value.is_char_boundary(19)
    {
        return "00:00:00.000".to_owned();
    }
    let hms = &value[11..19.min(value.len())];
    let mut millis = String::from("000");
    if bytes.get(19) == Some(&b'.') {
        let mut end = 20;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) && end < 23 {
            end += 1;
        }
        if end > 20 {
            let mut digits = value[20..end].to_owned();
            while digits.len() < 3 {
                digits.push('0');
            }
            millis = digits;
        }
    }
    format!("{hms}.{millis}")
}

#[cfg(test)]
mod tests {
    use super::{
        format_go_duration_seconds, format_unix_seconds_rfc3339, format_unix_seconds_ymd_hms,
        hms_millis_from_rfc3339, parse_rfc3339_utc_to_unix_seconds,
    };

    #[test]
    fn rfc3339_and_sqlite_layouts_differ_only_in_their_separators() {
        // Both are consumed as *text* by SQLite comparisons, so the separator
        // is load-bearing, not cosmetic.
        assert_eq!(format_unix_seconds_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(
            format_unix_seconds_rfc3339(1_770_000_000),
            "2026-02-02T02:40:00Z"
        );
        assert_eq!(
            format_unix_seconds_ymd_hms(1_770_000_000),
            "2026-02-02 02:40:00"
        );
    }

    #[test]
    fn epoch_round_trips() {
        assert_eq!(
            parse_rfc3339_utc_to_unix_seconds("1970-01-01T00:00:00Z"),
            Some(0)
        );
        assert_eq!(
            format_unix_seconds_ymd_hms(0),
            "1970-01-01 00:00:00".to_owned()
        );
    }

    #[test]
    fn parses_known_instant() -> Result<(), Box<dyn std::error::Error>> {
        // 2026-07-13T00:00:01Z — used throughout the orchestrator-exec
        // event-log test fixtures.
        let seconds = parse_rfc3339_utc_to_unix_seconds("2026-07-13T00:00:01Z")
            .ok_or("expected a valid timestamp")?;
        assert_eq!(format_unix_seconds_ymd_hms(seconds), "2026-07-13 00:00:01");
        Ok(())
    }

    #[test]
    fn rejects_non_utc_offset() {
        assert_eq!(
            parse_rfc3339_utc_to_unix_seconds("2026-07-13T00:00:01+02:00"),
            None
        );
    }

    #[test]
    fn tolerates_fractional_seconds() {
        assert_eq!(
            parse_rfc3339_utc_to_unix_seconds("2026-07-13T00:00:01.123456Z"),
            parse_rfc3339_utc_to_unix_seconds("2026-07-13T00:00:01Z")
        );
    }

    #[test]
    fn duration_formatting_matches_go_shape() {
        assert_eq!(format_go_duration_seconds(0), "0s");
        assert_eq!(format_go_duration_seconds(5), "5s");
        assert_eq!(format_go_duration_seconds(65), "1m5s");
        assert_eq!(format_go_duration_seconds(3661), "1h1m1s");
        assert_eq!(format_go_duration_seconds(90_000), "25h0m0s");
        assert_eq!(format_go_duration_seconds(-5), "0s");
    }

    #[test]
    fn hms_millis_extracts_time_of_day() {
        assert_eq!(
            hms_millis_from_rfc3339("2026-07-13T15:04:05Z"),
            "15:04:05.000"
        );
        assert_eq!(
            hms_millis_from_rfc3339("2026-07-13T15:04:05.5Z"),
            "15:04:05.500"
        );
        assert_eq!(
            hms_millis_from_rfc3339("2026-07-13T15:04:05.123456Z"),
            "15:04:05.123"
        );
    }
}
