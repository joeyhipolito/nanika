//! Bounded decoding of Claude Code status-line quota observations.
//!
//! This module stops at the provider wire boundary. It deliberately does not
//! attach a Nanika account, local clock, confidence, TTL, or recent-capacity
//! score: none of those values are authenticated by Claude's status-line JSON.
//! A trusted multiplexer must bind this sanitized observation to those local
//! capabilities before it can become an adaptive-routing `UsageSnapshotV1`.
//! Raw status-line JSON is bounded before allocation and is never retained in
//! the normalized observation or any error.

use orchestrator_core::{
    BasisPointsV1, DurationMillisV1, UsageHealthV1, UsageLimitIdV1, UsageReasonCodeV1,
    UsageSourceIdV1, UsageWindowKindV1, UtcMillisV1,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use thiserror::Error;

/// Maximum accepted Claude status-line envelope before JSON allocation.
pub const MAX_CLAUDE_STATUSLINE_INPUT_BYTES: usize = 256 * 1024;

/// Exact Claude Code build whose status-line schema has been reviewed.
pub const SUPPORTED_CLAUDE_STATUSLINE_VERSION: &str = "2.1.211";

/// Sanitized source identity for the pinned Claude status-line wire.
pub const CLAUDE_STATUSLINE_SOURCE_ID: &str = "claude_statusline_2_1_211_v1";

const BASIS_POINTS_SCALE: u16 = 10_000;
const FIVE_HOURS_MS: u64 = 5 * 60 * 60 * 1_000;
const SEVEN_DAYS_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_PERCENTAGE_LEXEME_BYTES: usize = 128;

const REASON_RATE_LIMITS_ABSENT: &str = "claude_statusline_rate_limits_absent";
const REASON_WINDOWS_ABSENT: &str = "claude_statusline_windows_absent";
const REASON_FIVE_HOUR_ABSENT: &str = "claude_statusline_five_hour_absent";
const REASON_SEVEN_DAY_ABSENT: &str = "claude_statusline_seven_day_absent";

/// One quota window decoded only from Claude's documented status-line fields.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeStatuslineQuotaWindowV1 {
    limit_id: UsageLimitIdV1,
    kind: UsageWindowKindV1,
    used_bps: BasisPointsV1,
    remaining_bps: BasisPointsV1,
    resets_at_utc_ms: UtcMillisV1,
    duration_ms: DurationMillisV1,
}

impl ClaudeStatuslineQuotaWindowV1 {
    /// Stable identity of this provider quota window.
    #[must_use]
    pub fn limit_id(&self) -> &UsageLimitIdV1 {
        &self.limit_id
    }

    /// Stable provider-defined window kind.
    #[must_use]
    pub fn kind(&self) -> &UsageWindowKindV1 {
        &self.kind
    }

    /// Conservatively rounded consumed capacity.
    #[must_use]
    pub const fn used_bps(&self) -> BasisPointsV1 {
        self.used_bps
    }

    /// Remaining capacity after conservative consumed-capacity rounding.
    #[must_use]
    pub const fn remaining_bps(&self) -> BasisPointsV1 {
        self.remaining_bps
    }

    /// Claude's reset epoch converted from checked seconds to milliseconds.
    #[must_use]
    pub const fn resets_at_utc_ms(&self) -> UtcMillisV1 {
        self.resets_at_utc_ms
    }

    /// Reviewed nominal duration of this provider window.
    #[must_use]
    pub const fn duration_ms(&self) -> DurationMillisV1 {
        self.duration_ms
    }
}

/// Payload-free, provider-only observation awaiting trusted local binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeStatuslineQuotaObservationV1 {
    source: UsageSourceIdV1,
    health: UsageHealthV1,
    reason_code: Option<UsageReasonCodeV1>,
    windows: Vec<ClaudeStatuslineQuotaWindowV1>,
}

impl ClaudeStatuslineQuotaObservationV1 {
    /// Sanitized source that includes the pinned Claude Code schema version.
    #[must_use]
    pub fn source(&self) -> &UsageSourceIdV1 {
        &self.source
    }

    /// Completeness of the provider-only evidence.
    #[must_use]
    pub const fn health(&self) -> UsageHealthV1 {
        self.health
    }

    /// Stable reason when the documented windows are incomplete or absent.
    #[must_use]
    pub fn reason_code(&self) -> Option<&UsageReasonCodeV1> {
        self.reason_code.as_ref()
    }

    /// Decoded windows in stable five-hour, then seven-day order.
    #[must_use]
    pub fn windows(&self) -> &[ClaudeStatuslineQuotaWindowV1] {
        &self.windows
    }
}

/// Stable, payload-free status-line decoding failures.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ClaudeStatuslineUsageError {
    #[error("Claude status-line input is empty")]
    EmptyInput,
    #[error("Claude status-line input exceeds the bounded adapter contract")]
    InputTooLarge,
    #[error("Claude status-line input is not valid duplicate-free bounded JSON")]
    InvalidJson,
    #[error("Claude status-line schema version is unsupported")]
    UnsupportedVersion,
    #[error("Claude status-line used percentage is outside 0 through 100")]
    PercentageOutOfRange,
    #[error("Claude status-line reset epoch is invalid")]
    InvalidResetTimestamp,
    #[error("Claude status-line observation violates its sanitized contract")]
    InvalidNormalizedObservation,
}

#[derive(Deserialize)]
struct StatuslineEnvelopeWire<'a> {
    #[serde(borrow)]
    version: &'a str,
    #[serde(borrow)]
    rate_limits: Option<RateLimitsWire<'a>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RateLimitsWire<'a> {
    #[serde(borrow)]
    five_hour: Option<QuotaWindowWire<'a>>,
    #[serde(borrow)]
    seven_day: Option<QuotaWindowWire<'a>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuotaWindowWire<'a> {
    #[serde(borrow)]
    used_percentage: &'a RawValue,
    resets_at: u64,
}

/// Decodes the reviewed Claude Code status-line quota wire without attaching
/// caller-asserted routing authority.
///
/// Top-level fields outside `version` and `rate_limits` belong to the human
/// renderer and are ignored. Known semantic fields reject duplicates through
/// Serde's struct decoder; the rate-limit and window shapes reject unknown
/// fields. Either documented quota window may be absent. Such observations are
/// represented as `Partial` or `Unavailable`, never silently promoted to
/// healthy routing evidence.
pub fn decode_claude_statusline_usage(
    input: &[u8],
) -> Result<ClaudeStatuslineQuotaObservationV1, ClaudeStatuslineUsageError> {
    if input.is_empty() {
        return Err(ClaudeStatuslineUsageError::EmptyInput);
    }
    if input.len() > MAX_CLAUDE_STATUSLINE_INPUT_BYTES {
        return Err(ClaudeStatuslineUsageError::InputTooLarge);
    }

    let wire: StatuslineEnvelopeWire<'_> =
        serde_json::from_slice(input).map_err(|_| ClaudeStatuslineUsageError::InvalidJson)?;
    if wire.version != SUPPORTED_CLAUDE_STATUSLINE_VERSION {
        return Err(ClaudeStatuslineUsageError::UnsupportedVersion);
    }

    let source = UsageSourceIdV1::new(CLAUDE_STATUSLINE_SOURCE_ID)
        .map_err(|_| ClaudeStatuslineUsageError::InvalidNormalizedObservation)?;
    let Some(rate_limits) = wire.rate_limits else {
        return Ok(ClaudeStatuslineQuotaObservationV1 {
            source,
            health: UsageHealthV1::Unavailable,
            reason_code: Some(reason_code(REASON_RATE_LIMITS_ABSENT)?),
            windows: Vec::new(),
        });
    };

    let has_five_hour = rate_limits.five_hour.is_some();
    let has_seven_day = rate_limits.seven_day.is_some();
    let mut windows = Vec::with_capacity(2);
    if let Some(window) = rate_limits.five_hour {
        windows.push(normalize_window("five_hour", FIVE_HOURS_MS, window)?);
    }
    if let Some(window) = rate_limits.seven_day {
        windows.push(normalize_window("seven_day", SEVEN_DAYS_MS, window)?);
    }

    let (health, reason_code) = match (has_five_hour, has_seven_day) {
        (true, true) => (UsageHealthV1::Healthy, None),
        (false, true) => (
            UsageHealthV1::Partial,
            Some(reason_code(REASON_FIVE_HOUR_ABSENT)?),
        ),
        (true, false) => (
            UsageHealthV1::Partial,
            Some(reason_code(REASON_SEVEN_DAY_ABSENT)?),
        ),
        (false, false) => (
            UsageHealthV1::Unavailable,
            Some(reason_code(REASON_WINDOWS_ABSENT)?),
        ),
    };

    Ok(ClaudeStatuslineQuotaObservationV1 {
        source,
        health,
        reason_code,
        windows,
    })
}

fn reason_code(value: &'static str) -> Result<UsageReasonCodeV1, ClaudeStatuslineUsageError> {
    UsageReasonCodeV1::new(value)
        .map_err(|_| ClaudeStatuslineUsageError::InvalidNormalizedObservation)
}

fn normalize_window(
    name: &'static str,
    duration_ms: u64,
    wire: QuotaWindowWire<'_>,
) -> Result<ClaudeStatuslineQuotaWindowV1, ClaudeStatuslineUsageError> {
    let used = percent_lexeme_to_used_bps(wire.used_percentage.get())?;
    let remaining = BASIS_POINTS_SCALE
        .checked_sub(used)
        .ok_or(ClaudeStatuslineUsageError::PercentageOutOfRange)?;
    let reset_ms = wire
        .resets_at
        .checked_mul(1_000)
        .ok_or(ClaudeStatuslineUsageError::InvalidResetTimestamp)?;

    Ok(ClaudeStatuslineQuotaWindowV1 {
        limit_id: UsageLimitIdV1::new(name)
            .map_err(|_| ClaudeStatuslineUsageError::InvalidNormalizedObservation)?,
        kind: UsageWindowKindV1::new(name)
            .map_err(|_| ClaudeStatuslineUsageError::InvalidNormalizedObservation)?,
        used_bps: BasisPointsV1::new(used)
            .map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)?,
        remaining_bps: BasisPointsV1::new(remaining)
            .map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)?,
        resets_at_utc_ms: UtcMillisV1::new(reset_ms),
        duration_ms: DurationMillisV1::new(duration_ms)
            .map_err(|_| ClaudeStatuslineUsageError::InvalidNormalizedObservation)?,
    })
}

/// Converts an exact JSON percentage lexeme to basis points, rounding consumed
/// capacity up. At most 128 bytes are examined and no floating-point value is
/// constructed, so a fraction above a basis-point boundary cannot round down.
fn percent_lexeme_to_used_bps(text: &str) -> Result<u16, ClaudeStatuslineUsageError> {
    if text.is_empty() || text.len() > MAX_PERCENTAGE_LEXEME_BYTES {
        return Err(ClaudeStatuslineUsageError::PercentageOutOfRange);
    }
    let (negative, unsigned) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let (mantissa, exponent) = match unsigned.find(['e', 'E']) {
        Some(index) => {
            let exponent = unsigned[index + 1..]
                .parse::<i32>()
                .map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)?;
            (&unsigned[..index], exponent)
        }
        None => (unsigned, 0),
    };

    let mut digits = Vec::with_capacity(mantissa.len());
    let mut fractional_digits = 0_i64;
    let mut seen_decimal = false;
    for byte in mantissa.bytes() {
        match byte {
            b'0'..=b'9' => {
                digits.push(byte - b'0');
                if seen_decimal {
                    fractional_digits = fractional_digits
                        .checked_add(1)
                        .ok_or(ClaudeStatuslineUsageError::PercentageOutOfRange)?;
                }
            }
            b'.' if !seen_decimal => seen_decimal = true,
            _ => return Err(ClaudeStatuslineUsageError::PercentageOutOfRange),
        }
    }
    if digits.is_empty() {
        return Err(ClaudeStatuslineUsageError::PercentageOutOfRange);
    }

    let first_nonzero = digits.iter().position(|digit| *digit != 0);
    let Some(first_nonzero) = first_nonzero else {
        return Ok(0);
    };
    if negative {
        return Err(ClaudeStatuslineUsageError::PercentageOutOfRange);
    }
    let significant = &digits[first_nonzero..];
    let power = i64::from(exponent)
        .checked_sub(fractional_digits)
        .and_then(|value| value.checked_add(2))
        .ok_or(ClaudeStatuslineUsageError::PercentageOutOfRange)?;

    let scaled = if power >= 0 {
        let power =
            usize::try_from(power).map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)?;
        if significant.len().saturating_add(power) > 5 {
            return Err(ClaudeStatuslineUsageError::PercentageOutOfRange);
        }
        decimal_digits_to_u32(significant)?
            .checked_mul(
                10_u32
                    .checked_pow(
                        u32::try_from(power)
                            .map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)?,
                    )
                    .ok_or(ClaudeStatuslineUsageError::PercentageOutOfRange)?,
            )
            .ok_or(ClaudeStatuslineUsageError::PercentageOutOfRange)?
    } else {
        let divisor_digits = power.unsigned_abs();
        let significant_len = u64::try_from(significant.len())
            .map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)?;
        if divisor_digits >= significant_len {
            1
        } else {
            let quotient_len = usize::try_from(significant_len - divisor_digits)
                .map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)?;
            if quotient_len > 5 {
                return Err(ClaudeStatuslineUsageError::PercentageOutOfRange);
            }
            let quotient = decimal_digits_to_u32(&significant[..quotient_len])?;
            let has_remainder = significant[quotient_len..].iter().any(|digit| *digit != 0);
            quotient
                .checked_add(u32::from(has_remainder))
                .ok_or(ClaudeStatuslineUsageError::PercentageOutOfRange)?
        }
    };

    if scaled > u32::from(BASIS_POINTS_SCALE) {
        return Err(ClaudeStatuslineUsageError::PercentageOutOfRange);
    }
    u16::try_from(scaled).map_err(|_| ClaudeStatuslineUsageError::PercentageOutOfRange)
}

fn decimal_digits_to_u32(digits: &[u8]) -> Result<u32, ClaudeStatuslineUsageError> {
    digits.iter().try_fold(0_u32, |value, digit| {
        value
            .checked_mul(10)
            .and_then(|value| value.checked_add(u32::from(*digit)))
            .ok_or(ClaudeStatuslineUsageError::PercentageOutOfRange)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn official_shape() -> Vec<u8> {
        br#"{
          "version":"2.1.211",
          "workspace":{"current_dir":"/secret/project"},
          "context_window":{"remaining_percentage":1},
          "rate_limits":{
            "five_hour":{"used_percentage":23.5,"resets_at":1738425600},
            "seven_day":{"used_percentage":41.2,"resets_at":1738857600}
          }
        }"#
        .to_vec()
    }

    #[test]
    fn decodes_official_epoch_second_shape_and_ignores_session_context()
    -> Result<(), Box<dyn std::error::Error>> {
        let observation = decode_claude_statusline_usage(&official_shape())?;

        assert_eq!(observation.source().as_str(), CLAUDE_STATUSLINE_SOURCE_ID);
        assert_eq!(observation.health(), UsageHealthV1::Healthy);
        assert_eq!(observation.reason_code(), None);
        assert_eq!(observation.windows().len(), 2);
        assert_eq!(observation.windows()[0].limit_id().as_str(), "five_hour");
        assert_eq!(observation.windows()[0].kind().as_str(), "five_hour");
        assert_eq!(observation.windows()[0].used_bps().get(), 2_350);
        assert_eq!(observation.windows()[0].remaining_bps().get(), 7_650);
        assert_eq!(
            observation.windows()[0].resets_at_utc_ms().get(),
            1_738_425_600_000
        );
        assert_eq!(observation.windows()[0].duration_ms().get(), FIVE_HOURS_MS);
        assert_eq!(observation.windows()[1].used_bps().get(), 4_120);
        assert_eq!(
            observation.windows()[1].resets_at_utc_ms().get(),
            1_738_857_600_000
        );
        Ok(())
    }

    #[test]
    fn decimal_lexemes_remain_exact_and_conservatively_rounded()
    -> Result<(), Box<dyn std::error::Error>> {
        for (wire, expected) in [
            ("0", 0),
            ("-0.0", 0),
            ("0.001", 1),
            ("0.01", 1),
            ("0.0100000000000000001", 2),
            ("1e-2", 1),
            ("12.34", 1_234),
            ("99.9900000000000001", 10_000),
            ("99.999", 10_000),
            ("100", 10_000),
        ] {
            assert_eq!(percent_lexeme_to_used_bps(wire), Ok(expected), "{wire}");
        }
        for wire in ["-0.01", "100.001", "101", "1e40"] {
            assert_eq!(
                percent_lexeme_to_used_bps(wire),
                Err(ClaudeStatuslineUsageError::PercentageOutOfRange),
                "{wire}"
            );
        }
        Ok(())
    }

    #[test]
    fn full_adapter_preserves_just_over_boundary_decimal() -> Result<(), Box<dyn std::error::Error>>
    {
        let input = br#"{"version":"2.1.211","rate_limits":{"five_hour":{"used_percentage":0.0100000000000000001,"resets_at":1738425600}}}"#;
        let observation = decode_claude_statusline_usage(input)?;
        assert_eq!(observation.health(), UsageHealthV1::Partial);
        assert_eq!(observation.windows()[0].used_bps().get(), 2);
        Ok(())
    }

    #[test]
    fn independently_missing_windows_are_typed_evidence() -> Result<(), Box<dyn std::error::Error>>
    {
        let only_five = br#"{"version":"2.1.211","rate_limits":{"five_hour":{"used_percentage":1,"resets_at":1738425600}}}"#;
        let observation = decode_claude_statusline_usage(only_five)?;
        assert_eq!(observation.health(), UsageHealthV1::Partial);
        assert_eq!(
            observation.reason_code().map(UsageReasonCodeV1::as_str),
            Some(REASON_SEVEN_DAY_ABSENT)
        );
        assert_eq!(observation.windows().len(), 1);

        let only_seven = br#"{"version":"2.1.211","rate_limits":{"seven_day":{"used_percentage":1,"resets_at":1738857600}}}"#;
        let observation = decode_claude_statusline_usage(only_seven)?;
        assert_eq!(observation.health(), UsageHealthV1::Partial);
        assert_eq!(
            observation.reason_code().map(UsageReasonCodeV1::as_str),
            Some(REASON_FIVE_HOUR_ABSENT)
        );

        let no_windows = br#"{"version":"2.1.211","rate_limits":{}}"#;
        let observation = decode_claude_statusline_usage(no_windows)?;
        assert_eq!(observation.health(), UsageHealthV1::Unavailable);
        assert_eq!(
            observation.reason_code().map(UsageReasonCodeV1::as_str),
            Some(REASON_WINDOWS_ABSENT)
        );

        let no_rate_limits = br#"{"version":"2.1.211"}"#;
        let observation = decode_claude_statusline_usage(no_rate_limits)?;
        assert_eq!(observation.health(), UsageHealthV1::Unavailable);
        assert_eq!(
            observation.reason_code().map(UsageReasonCodeV1::as_str),
            Some(REASON_RATE_LIMITS_ABSENT)
        );
        Ok(())
    }

    #[test]
    fn duplicate_semantic_keys_and_schema_drift_fail_closed()
    -> Result<(), Box<dyn std::error::Error>> {
        for input in [
            br#"{"version":"2.1.211","version":"2.1.211"}"#.as_slice(),
            br#"{"version":"2.1.211","rate_limits":{"five_hour":{"used_percentage":1,"used_percentage":2,"resets_at":1738425600}}}"#.as_slice(),
            br#"{"version":"2.1.211","rate_limits":{"five_hour":{"used_percentage":1,"resets_at":1738425600,"extra":1}}}"#.as_slice(),
            br#"{"version":"2.1.211","rate_limits":{"monthly":{"used_percentage":1,"resets_at":1738425600}}}"#.as_slice(),
        ] {
            assert_eq!(
                decode_claude_statusline_usage(input),
                Err(ClaudeStatuslineUsageError::InvalidJson)
            );
        }
        assert_eq!(
            decode_claude_statusline_usage(br#"{"version":"2.1.212"}"#),
            Err(ClaudeStatuslineUsageError::UnsupportedVersion)
        );
        Ok(())
    }

    #[test]
    fn rejects_oversize_malformed_wrong_typed_and_overflowing_values()
    -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            decode_claude_statusline_usage(&[]),
            Err(ClaudeStatuslineUsageError::EmptyInput)
        );
        assert_eq!(
            decode_claude_statusline_usage(&vec![b' '; MAX_CLAUDE_STATUSLINE_INPUT_BYTES + 1]),
            Err(ClaudeStatuslineUsageError::InputTooLarge)
        );
        for input in [
            br#"{"version":"2.1.211","rate_limits":{"five_hour":{"used_percentage":"1","resets_at":1738425600}}}"#.as_slice(),
            br#"{"version":"2.1.211","rate_limits":{"five_hour":{"used_percentage":1,"resets_at":"1738425600"}}}"#.as_slice(),
            br#"{"version":"2.1.211","rate_limits":{"five_hour":{"used_percentage":1,"resets_at":18446744073709552}}}"#.as_slice(),
            b"{".as_slice(),
        ] {
            assert!(decode_claude_statusline_usage(input).is_err());
        }
        Ok(())
    }

    #[test]
    fn errors_never_echo_statusline_payloads() -> Result<(), Box<dyn std::error::Error>> {
        let secret = "never-echo-this-statusline-secret";
        let input = format!(
            r#"{{"version":"2.1.211","secret":"{secret}","rate_limits":{{"five_hour":{{"used_percentage":101,"resets_at":1738425600}}}}}}"#
        );
        let error = decode_claude_statusline_usage(input.as_bytes())
            .err()
            .ok_or_else(|| std::io::Error::other("invalid fixture unexpectedly decoded"))?;
        assert!(!error.to_string().contains(secret));
        assert!(!format!("{error:?}").contains(secret));
        Ok(())
    }
}
