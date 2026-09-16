//! Read-only inspection command handlers: `status`, `events`, `metrics`,
//! and `audit scorecard`.
//!
//! Ported from the Go orchestrator's `internal/cmd` readers. Pure projection
//! helpers remain byte-in/struct-out; compatibility directory listings use
//! their existing bounded `std::fs` adapters; selected event logs and the
//! audit store use the app crate's retained, no-follow read capabilities.
//!
//! **Not ported in this cell** (`--help` text): Go's cobra renders dedicated
//! usage text for `status`/`events {list,replay,tail}`/`metrics
//! {personas,skills,trends,routing,routing-methods,phases}` and for the
//! `barok`/`barok status` and `discipline`/`discipline status` parent+leaf
//! pairs, plus `audit`/`audit scorecard`, on `-h`/`--help` or a parse error.
//! This module returns a plain command error instead — flag surface and data
//! output are byte-matched (this cell's DELIVERABLES), but parent/leaf
//! `--help` text byte-parity is a real, acknowledged gap for a follow-up cell,
//! not a silent omission.

mod audit;
mod barok;
mod daemon;
mod discipline;
mod events;
mod go_is_print;
#[cfg(all(unix, feature = "verification-process-canary"))]
mod hermetic_canary;
mod learning;
mod metrics;
mod status;
// `composition`'s canonical event sink stamps every worker event, so the
// reviewed UTC formatter is crate-visible rather than duplicated there.
pub(crate) mod time_fmt;

pub(crate) use audit::{
    AuditError, AuditScorecardFlags, open_error as audit_open_error,
    parse_local_flag as parse_audit_local_flag, run as run_audit_scorecard,
};
pub(crate) use daemon::{
    DaemonCommand, DaemonCommandError, is_help as daemon_is_help, parse as parse_daemon,
    run as run_daemon,
};
pub(crate) use events::{EventsError, run_list as run_events_list};
pub(crate) use events::{run_replay as run_events_replay, run_tail as run_events_tail};
#[cfg(all(unix, feature = "verification-process-canary"))]
pub use hermetic_canary::run_hermetic_canary_worker;
#[cfg(all(unix, feature = "verification-process-canary"))]
pub(crate) use hermetic_canary::{HermeticCanaryCmdError, run as run_hermetic_canary};
pub(crate) use learning::{
    LearningCommand, LearningError, parse as parse_learning, run as run_learning,
};
pub(crate) use metrics::{MetricsCommand, MetricsError, MissionsFlags, run as run_metrics};
pub(crate) use status::{StatusError, run as run_status};
pub(crate) use {barok::run as run_barok_status, discipline::run as run_discipline_status};

use thiserror::Error;

/// Which `events` subcommand to run, with its own flags (mirrors
/// `internal/cmd/events.go`'s `cobra.Command` tree).
#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) enum EventsCommand {
    List,
    Replay { mission_id: String, raw_json: bool },
    Tail { mission_id: String, raw_json: bool },
}

/// Parse/flag-surface failures for the `status`/`events`/`metrics`/`audit` command
/// group, plus each command's own runtime error.
#[derive(Debug, Error)]
pub(crate) enum CmdError {
    #[error(transparent)]
    Audit(#[from] AuditError),
    #[error(transparent)]
    Status(#[from] StatusError),
    #[error(transparent)]
    Events(#[from] EventsError),
    #[error(transparent)]
    Metrics(#[from] MetricsError),
    #[error(transparent)]
    Learning(#[from] LearningError),
    #[error(transparent)]
    Daemon(#[from] DaemonCommandError),
    #[error("{flag} requires a value")]
    FlagNeedsValue { flag: String },
    #[error("invalid value for {flag}: expected an integer")]
    InvalidFlagValue { flag: String },
    #[error("unknown flag {0:?}")]
    UnknownFlag(String),
    #[error("{command} requires an argument")]
    MissingArgument { command: &'static str },
    #[error("{command} accepts at most one argument")]
    TooManyArguments { command: &'static str },
    #[error("unknown events subcommand {0:?}")]
    UnknownEventsSubcommand(String),
    #[error("unknown metrics subcommand {0:?}")]
    UnknownMetricsSubcommand(String),
    #[error("unknown learning command {0:?}")]
    UnknownLearningSubcommand(String),
    #[cfg(all(unix, feature = "verification-process-canary"))]
    #[error(transparent)]
    HermeticCanary(#[from] HermeticCanaryCmdError),
}

/// Ports the argument shape of `internal/cmd/events.go`'s `eventsCmd`
/// (`list`/`replay <mission-id> [-j|--json]`/`tail <mission-id> [-j|--json]`).
pub(crate) fn parse_events(arguments: &[String]) -> Result<EventsCommand, CmdError> {
    let Some((sub, rest)) = arguments.split_first() else {
        return Err(CmdError::MissingArgument { command: "events" });
    };
    match sub.as_str() {
        "list" => Ok(EventsCommand::List),
        "replay" => {
            let (mission_id, raw_json) = parse_mission_id_and_json_flag(rest, "replay")?;
            Ok(EventsCommand::Replay {
                mission_id,
                raw_json,
            })
        }
        "tail" => {
            let (mission_id, raw_json) = parse_mission_id_and_json_flag(rest, "tail")?;
            Ok(EventsCommand::Tail {
                mission_id,
                raw_json,
            })
        }
        other => Err(CmdError::UnknownEventsSubcommand(other.to_owned())),
    }
}

fn parse_mission_id_and_json_flag(
    arguments: &[String],
    command: &'static str,
) -> Result<(String, bool), CmdError> {
    let mut raw_json = false;
    let mut positional = None;
    for argument in arguments {
        let (name, inline_value) = split_flag(argument);
        match name {
            "--json" | "-j" => {
                raw_json = match inline_value {
                    None => true,
                    Some(value) => parse_bool_flag_value(value, "--json")?,
                };
            }
            value if value.starts_with('-') => return Err(CmdError::UnknownFlag(argument.clone())),
            value => {
                if positional.is_some() {
                    return Err(CmdError::TooManyArguments { command });
                }
                positional = Some(value.to_owned());
            }
        }
    }
    positional
        .map(|mission_id| (mission_id, raw_json))
        .ok_or(CmdError::MissingArgument { command })
}

/// Parses pflag's `--flag=value` inline form for a boolean flag, matching
/// `strconv.ParseBool`'s accepted literals (`1`, `t`, `T`, `TRUE`, `true`,
/// `True`, `0`, `f`, `F`, `FALSE`, `false`, `False`).
pub(crate) fn parse_bool_flag_value(value: &str, flag: &str) -> Result<bool, CmdError> {
    match value {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        _ => Err(CmdError::InvalidFlagValue {
            flag: flag.to_owned(),
        }),
    }
}

/// Mirrors `strconv.ParseInt(value, 0, 64)`, which pflag uses for Go `int`
/// flags on the accepted 64-bit target.
pub(crate) fn parse_go_int64(value: &str) -> Option<i64> {
    let (negative, unsigned) = match value.as_bytes().first() {
        Some(b'+') => (false, &value[1..]),
        Some(b'-') => (true, &value[1..]),
        Some(_) => (false, value),
        None => return None,
    };
    if unsigned.is_empty() {
        return None;
    }

    let bytes = unsigned.as_bytes();
    let (base, digits, prefix_allows_underscore) = if bytes.len() >= 2 && bytes[0] == b'0' {
        match bytes[1] {
            b'b' | b'B' => (2_u32, &unsigned[2..], true),
            b'o' | b'O' => (8_u32, &unsigned[2..], true),
            b'x' | b'X' => (16_u32, &unsigned[2..], true),
            _ => (8_u32, unsigned, false),
        }
    } else {
        (10_u32, unsigned, false)
    };
    if digits.is_empty() {
        return None;
    }

    let mut magnitude = 0_u128;
    let mut saw_digit = false;
    let mut previous_was_digit = false;
    for (index, byte) in digits.bytes().enumerate() {
        if byte == b'_' {
            let leading_after_prefix = index == 0 && prefix_allows_underscore && digits.len() > 1;
            let followed_by_digit = digits
                .as_bytes()
                .get(index + 1)
                .and_then(|next| ascii_digit_value(*next))
                .is_some_and(|digit| digit < base);
            if !(followed_by_digit && (previous_was_digit || leading_after_prefix)) {
                return None;
            }
            previous_was_digit = false;
            continue;
        }
        let digit = ascii_digit_value(byte)?;
        if digit >= base {
            return None;
        }
        magnitude = magnitude
            .checked_mul(u128::from(base))?
            .checked_add(u128::from(digit))?;
        saw_digit = true;
        previous_was_digit = true;
    }
    if !saw_digit {
        return None;
    }

    let limit = if negative {
        (i64::MAX as u128) + 1
    } else {
        i64::MAX as u128
    };
    if magnitude > limit {
        return None;
    }
    if negative && magnitude == limit {
        Some(i64::MIN)
    } else {
        let magnitude = i64::try_from(magnitude).ok()?;
        Some(if negative { -magnitude } else { magnitude })
    }
}

const fn ascii_digit_value(byte: u8) -> Option<u32> {
    match byte {
        b'0'..=b'9' => Some((byte - b'0') as u32),
        b'a'..=b'f' => Some((byte - b'a' + 10) as u32),
        b'A'..=b'F' => Some((byte - b'A' + 10) as u32),
        _ => None,
    }
}

/// Ports the argument shape of `internal/cmd/metrics.go`'s `metricsCmd` tree:
/// a bare invocation (or one with only `--last`/`--domain`/`--status`/
/// `--days`/`--decomp-source`/`--worker` flags) is the top-level mission
/// list; `personas`/`skills`/`trends [--days N]`/`routing`/
/// `routing-methods`/`phases <workspace-id>` are subcommands.
pub(crate) fn parse_metrics(arguments: &[String]) -> Result<MetricsCommand, CmdError> {
    if let Some(first) = arguments.first() {
        if !first.starts_with('-') {
            let rest = &arguments[1..];
            return match first.as_str() {
                "personas" => Ok(MetricsCommand::Personas),
                "skills" => Ok(MetricsCommand::Skills),
                "trends" => parse_trends_flags(rest),
                "routing" => Ok(MetricsCommand::Routing),
                "routing-methods" => Ok(MetricsCommand::RoutingMethods),
                "phases" => parse_phases_argument(rest),
                other => Err(CmdError::UnknownMetricsSubcommand(other.to_owned())),
            };
        }
    }
    parse_missions_flags(arguments)
}

fn parse_missions_flags(arguments: &[String]) -> Result<MetricsCommand, CmdError> {
    let mut flags = MissionsFlags::default();
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let (name, inline_value) = split_flag(&arguments[cursor]);
        match name {
            "--last" => {
                flags.last = flag_int_value(arguments, &mut cursor, "--last", inline_value)?
            }
            "--domain" => {
                flags.domain = flag_string_value(arguments, &mut cursor, "--domain", inline_value)?;
            }
            "--status" => {
                flags.status = flag_string_value(arguments, &mut cursor, "--status", inline_value)?;
            }
            "--days" => {
                flags.days = flag_int_value(arguments, &mut cursor, "--days", inline_value)?
            }
            "--decomp-source" => {
                flags.decomp_source =
                    flag_string_value(arguments, &mut cursor, "--decomp-source", inline_value)?;
            }
            "--worker" => {
                flags.worker = flag_string_value(arguments, &mut cursor, "--worker", inline_value)?;
            }
            other => return Err(CmdError::UnknownFlag(other.to_owned())),
        }
        cursor += 1;
    }
    Ok(MetricsCommand::Missions(flags))
}

fn parse_trends_flags(arguments: &[String]) -> Result<MetricsCommand, CmdError> {
    let mut days = 30i64;
    let mut cursor = 0usize;
    while cursor < arguments.len() {
        let (name, inline_value) = split_flag(&arguments[cursor]);
        match name {
            "--days" => days = flag_int_value(arguments, &mut cursor, "--days", inline_value)?,
            other => return Err(CmdError::UnknownFlag(other.to_owned())),
        }
        cursor += 1;
    }
    Ok(MetricsCommand::Trends { days })
}

/// Splits `--flag=value` (pflag/cobra's inline-value form, which Go's
/// `cmd.Flags().GetInt`/`GetString` accept uniformly for every registered
/// flag) into its flag name and an optional inline value. `--flag value`
/// (two tokens) returns `(name, None)` so the caller falls back to reading
/// the next argument, matching the two-token form this parser already
/// handled.
pub(crate) fn split_flag(argument: &str) -> (&str, Option<&str>) {
    match argument.split_once('=') {
        Some((name, value)) if name.starts_with("--") => (name, Some(value)),
        _ => (argument, None),
    }
}

pub(crate) fn flag_string_value(
    arguments: &[String],
    cursor: &mut usize,
    flag: &str,
    inline_value: Option<&str>,
) -> Result<String, CmdError> {
    match inline_value {
        Some(value) => Ok(value.to_owned()),
        None => next_string_value(arguments, cursor, flag),
    }
}

fn flag_int_value(
    arguments: &[String],
    cursor: &mut usize,
    flag: &str,
    inline_value: Option<&str>,
) -> Result<i64, CmdError> {
    match inline_value {
        Some(value) => value
            .parse::<i64>()
            .map_err(|_| CmdError::InvalidFlagValue {
                flag: flag.to_owned(),
            }),
        None => next_int_value(arguments, cursor, flag),
    }
}

fn parse_phases_argument(arguments: &[String]) -> Result<MetricsCommand, CmdError> {
    let mut positional = None;
    for argument in arguments {
        if argument.starts_with('-') {
            return Err(CmdError::UnknownFlag(argument.clone()));
        }
        if positional.is_some() {
            return Err(CmdError::TooManyArguments { command: "phases" });
        }
        positional = Some(argument.clone());
    }
    positional
        .map(|workspace_id| MetricsCommand::Phases { workspace_id })
        .ok_or(CmdError::MissingArgument { command: "phases" })
}

pub(crate) fn next_string_value(
    arguments: &[String],
    cursor: &mut usize,
    flag: &str,
) -> Result<String, CmdError> {
    let Some(value) = arguments.get(*cursor + 1) else {
        return Err(CmdError::FlagNeedsValue {
            flag: flag.to_owned(),
        });
    };
    *cursor += 1;
    Ok(value.clone())
}

fn next_int_value(arguments: &[String], cursor: &mut usize, flag: &str) -> Result<i64, CmdError> {
    let value = next_string_value(arguments, cursor, flag)?;
    value
        .parse::<i64>()
        .map_err(|_| CmdError::InvalidFlagValue {
            flag: flag.to_owned(),
        })
}

/// Truncates `s` to at most `max_bytes` bytes, appending `"..."` when
/// truncated. Ports Go's `truncate` (`internal/cmd/metrics.go:91-96`), which
/// slices by byte length (Go `len(string)` is byte length, and Go string
/// slicing is byte-indexed). Go's slice can split a multi-byte UTF-8
/// sequence; Rust strings must stay valid UTF-8, so this walks back to the
/// nearest character boundary at or below the Go cut point instead — a
/// flagged divergence that only affects non-ASCII input landing exactly on a
/// truncation boundary (persona/skill/task strings in this codebase are
/// overwhelmingly ASCII).
pub(crate) fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_owned();
    }
    let mut cut = max_bytes.saturating_sub(3);
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}...", &s[..cut])
}

#[cfg(test)]
mod tests {
    use super::{
        CmdError, EventsCommand, MetricsCommand, parse_events, parse_go_int64, parse_metrics,
        truncate,
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[test]
    fn truncate_leaves_short_strings_untouched() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn truncate_cuts_and_appends_ellipsis() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn go_int64_matches_pflag_base_zero_forms_and_bounds() {
        for (value, expected) in [
            ("0", Some(0)),
            ("02", Some(2)),
            ("0x2", Some(2)),
            ("0Xf", Some(15)),
            ("0b10", Some(2)),
            ("0o10", Some(8)),
            ("+2", Some(2)),
            ("2_0", Some(20)),
            ("0x_ff", Some(255)),
            ("0b_1", Some(1)),
            ("0o_1", Some(1)),
            ("00_1", Some(1)),
            ("0_7", Some(7)),
            ("-0x8000000000000000", Some(i64::MIN)),
            ("9223372036854775807", Some(i64::MAX)),
            ("08", None),
            ("0x", None),
            ("0x_", None),
            ("_2", None),
            ("2_", None),
            ("2__0", None),
            ("9223372036854775808", None),
            ("-9223372036854775809", None),
        ] {
            assert_eq!(parse_go_int64(value), expected, "{value}");
        }
    }

    #[test]
    fn parse_events_list_has_no_arguments() -> TestResult {
        let parsed = parse_events(&["list".to_owned()])?;
        assert_eq!(parsed, EventsCommand::List);
        Ok(())
    }

    #[test]
    fn parse_events_replay_reads_mission_id_and_json_flag() -> TestResult {
        let parsed = parse_events(&["replay".to_owned(), "m1".to_owned(), "-j".to_owned()])?;
        assert_eq!(
            parsed,
            EventsCommand::Replay {
                mission_id: "m1".to_owned(),
                raw_json: true,
            }
        );
        Ok(())
    }

    #[test]
    fn parse_events_replay_without_mission_id_errors() {
        let result = parse_events(&["replay".to_owned()]);
        assert!(matches!(
            result,
            Err(CmdError::MissingArgument { command: "replay" })
        ));
    }

    #[test]
    fn parse_events_unknown_subcommand_errors() {
        let result = parse_events(&["bogus".to_owned()]);
        assert!(matches!(result, Err(CmdError::UnknownEventsSubcommand(_))));
    }

    #[test]
    fn parse_metrics_bare_invocation_uses_defaults() -> TestResult {
        let parsed = parse_metrics(&[])?;
        assert_eq!(
            parsed,
            MetricsCommand::Missions(super::MissionsFlags::default())
        );
        Ok(())
    }

    #[test]
    fn parse_metrics_last_flag_overrides_default() -> TestResult {
        let parsed = parse_metrics(&["--last".to_owned(), "5".to_owned()])?;
        let MetricsCommand::Missions(flags) = parsed else {
            return Err("expected Missions".into());
        };
        assert_eq!(flags.last, 5);
        Ok(())
    }

    #[test]
    fn parse_metrics_last_flag_accepts_equals_form() -> TestResult {
        let parsed = parse_metrics(&["--last=5".to_owned()])?;
        let MetricsCommand::Missions(flags) = parsed else {
            return Err("expected Missions".into());
        };
        assert_eq!(flags.last, 5);
        Ok(())
    }

    #[test]
    fn parse_metrics_domain_flag_accepts_equals_form() -> TestResult {
        let parsed = parse_metrics(&["--domain=dev".to_owned()])?;
        let MetricsCommand::Missions(flags) = parsed else {
            return Err("expected Missions".into());
        };
        assert_eq!(flags.domain, "dev");
        Ok(())
    }

    #[test]
    fn parse_metrics_trends_reads_days_flag() -> TestResult {
        let parsed = parse_metrics(&["trends".to_owned(), "--days".to_owned(), "7".to_owned()])?;
        assert_eq!(parsed, MetricsCommand::Trends { days: 7 });
        Ok(())
    }

    #[test]
    fn parse_metrics_trends_days_flag_accepts_equals_form() -> TestResult {
        let parsed = parse_metrics(&["trends".to_owned(), "--days=7".to_owned()])?;
        assert_eq!(parsed, MetricsCommand::Trends { days: 7 });
        Ok(())
    }

    #[test]
    fn parse_events_replay_json_flag_accepts_equals_form() -> TestResult {
        let parsed = parse_events(&[
            "replay".to_owned(),
            "m1".to_owned(),
            "--json=false".to_owned(),
        ])?;
        assert_eq!(
            parsed,
            EventsCommand::Replay {
                mission_id: "m1".to_owned(),
                raw_json: false,
            }
        );
        Ok(())
    }

    #[test]
    fn parse_metrics_phases_requires_workspace_id() {
        let result = parse_metrics(&["phases".to_owned()]);
        assert!(matches!(
            result,
            Err(CmdError::MissingArgument { command: "phases" })
        ));
    }

    #[test]
    fn parse_metrics_phases_reads_workspace_id() -> TestResult {
        let parsed = parse_metrics(&["phases".to_owned(), "ws-1".to_owned()])?;
        assert_eq!(
            parsed,
            MetricsCommand::Phases {
                workspace_id: "ws-1".to_owned()
            }
        );
        Ok(())
    }
}
