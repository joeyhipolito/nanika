//! Small UTC time helpers used by the lock and trash metadata.
//!
//! These avoid pulling in a full date crate by implementing the well-known
//! civil-from-days conversion. All arithmetic is overflow-safe on `i64` for
//! any epoch second representable in practice and never panics.

use std::time::{SystemTime, UNIX_EPOCH};

/// Returns the current UTC time formatted as RFC3339 with a `Z` offset, e.g.
/// `2026-07-14T17:10:00Z`. Matches Go's `time.Now().UTC().Format(time.RFC3339)`.
#[must_use]
pub fn utc_rfc3339_now() -> String {
    format_utc(epoch_secs_now(), false)
}

/// Returns the current UTC time as a compact sortable stamp, e.g.
/// `20260714T171000Z`. Matches Go's `now.Format("20060102T150405Z")`.
#[must_use]
pub fn utc_compact_stamp_now() -> String {
    format_utc(epoch_secs_now(), true)
}

fn epoch_secs_now() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs().min(i64::MAX as u64) as i64,
        Err(earlier) => -(earlier.duration().as_secs().min(i64::MAX as u64) as i64),
    }
}

/// Formats signed epoch seconds as either RFC3339 (`compact == false`) or the
/// compact stamp (`compact == true`), both with a trailing `Z`.
fn format_utc(epoch_secs: i64, compact: bool) -> String {
    let (year, month, day, hour, minute, second) = broken_down_utc(epoch_secs);
    if compact {
        format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
    } else {
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
    }
}

/// Splits signed epoch seconds into UTC (year, month, day, hour, minute, second).
fn broken_down_utc(epoch_secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = epoch_secs.div_euclid(86_400);
    let secs_of_day = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = (secs_of_day / 3600) as u32;
    let minute = ((secs_of_day % 3600) / 60) as u32;
    let second = (secs_of_day % 60) as u32;
    (year, month, day, hour, minute, second)
}

/// Howard Hinnant's civil-from-days algorithm: maps days-since-epoch
/// (1970-01-01) to the proleptic Gregorian (year, month, day).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_now_is_well_formed() {
        let s = utc_rfc3339_now();
        // YYYY-MM-DDTHH:MM:SSZ
        assert_eq!(s.len(), 20);
        let bytes = s.as_bytes();
        assert_eq!(bytes[4], b'-');
        assert_eq!(bytes[7], b'-');
        assert_eq!(bytes[10], b'T');
        assert_eq!(bytes[13], b':');
        assert_eq!(bytes[16], b':');
        assert_eq!(bytes[19], b'Z');
    }

    #[test]
    fn compact_stamp_now_is_well_formed() {
        let s = utc_compact_stamp_now();
        assert_eq!(s.len(), 16);
        let bytes = s.as_bytes();
        assert_eq!(bytes[8], b'T');
        assert_eq!(bytes[15], b'Z');
    }

    #[test]
    fn epoch_zero_is_unix_epoch() {
        assert_eq!(broken_down_utc(0), (1970, 1, 1, 0, 0, 0));
        assert_eq!(format_utc(0, false), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(0, true), "19700101T000000Z");
    }

    #[test]
    fn one_day_after_epoch() {
        assert_eq!(broken_down_utc(86_400), (1970, 1, 2, 0, 0, 0));
    }

    #[test]
    fn known_millennium_stamp() {
        // 2000-01-01T00:00:00Z == 946684800 epoch seconds.
        assert_eq!(broken_down_utc(946_684_800), (2000, 1, 1, 0, 0, 0));
        assert_eq!(format_utc(946_684_800, false), "2000-01-01T00:00:00Z");
    }
}
