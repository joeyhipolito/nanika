//! Native read-only `orchestrator audit scorecard` compatibility command.

use std::{fmt::Write as _, io::Write};

use orchestrator_app::{
    AuditReadError, ReadOnlyEventLogTarget, build_scorecard, format_scorecard,
    format_scorecard_json, load_reports_from_target,
};
use thiserror::Error;

use super::go_is_print;
use super::{CmdError, flag_string_value, next_string_value, parse_go_int64, split_flag};

const EMPTY_STORE_MESSAGE: &str = "No audit reports found. Run `gyo evaluate` to generate one.\n";

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct AuditScorecardFlags {
    format: String,
    domain: String,
    last: i64,
}

impl Default for AuditScorecardFlags {
    fn default() -> Self {
        Self {
            format: "text".to_owned(),
            domain: String::new(),
            last: 0,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum AuditError {
    #[error("loading audit reports: {0}")]
    Load(#[source] AuditReadError),
    #[error(transparent)]
    Format(#[from] AuditReadError),
    #[error("cannot write command output: {0}")]
    Output(#[from] std::io::Error),
}

pub(crate) fn open_error(source: std::io::Error) -> AuditError {
    AuditError::Load(AuditReadError::Open { source })
}

/// Consumes one scorecard-local flag, wherever Cobra permits it around the
/// parent and leaf command names. `--domain` is deliberately considered
/// local before inherited persistent flags because the Go leaf shadows the
/// root flag with the same name.
pub(crate) fn parse_local_flag(
    arguments: &[String],
    cursor: &mut usize,
    flags: &mut AuditScorecardFlags,
) -> Result<bool, CmdError> {
    let (name, inline_value) = split_flag(&arguments[*cursor]);
    match name {
        "--format" => {
            flags.format = flag_string_value(arguments, cursor, "--format", inline_value)?;
        }
        "--domain" => {
            flags.domain = flag_string_value(arguments, cursor, "--domain", inline_value)?;
        }
        "--last" => {
            flags.last = audit_int_value(arguments, cursor, inline_value)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

fn audit_int_value(
    arguments: &[String],
    cursor: &mut usize,
    inline_value: Option<&str>,
) -> Result<i64, CmdError> {
    let value = match inline_value {
        Some(value) => value.to_owned(),
        None => next_string_value(arguments, cursor, "--last")?,
    };
    parse_go_int64(&value).ok_or_else(|| CmdError::InvalidFlagValue {
        flag: "--last".to_owned(),
    })
}

/// Runs the bounded scorecard reader. `None` represents a missing runtime
/// home, which Go treats identically to a missing `audits.jsonl`.
pub(crate) fn run(
    target: Option<&ReadOnlyEventLogTarget>,
    flags: &AuditScorecardFlags,
    output: &mut impl Write,
) -> Result<(), AuditError> {
    let mut reports = match target {
        Some(target) => load_reports_from_target(target).map_err(AuditError::Load)?,
        None => Vec::new(),
    };

    if reports.is_empty() {
        output.write_all(EMPTY_STORE_MESSAGE.as_bytes())?;
        return Ok(());
    }

    if !flags.domain.is_empty() {
        reports.retain(|report| report.domain == flags.domain);
        if reports.is_empty() {
            writeln!(
                output,
                "No audit reports found for domain {}.",
                quote_go_string(&flags.domain)
            )?;
            return Ok(());
        }
    }

    if flags.last > 0 {
        let last = usize::try_from(flags.last).unwrap_or(usize::MAX);
        if last < reports.len() {
            reports.drain(..reports.len() - last);
        }
    }

    let summary = build_scorecard(&reports);
    if flags.format == "json" {
        writeln!(output, "{}", format_scorecard_json(&summary)?)?;
    } else {
        output.write_all(format_scorecard(&summary).as_bytes())?;
    }
    Ok(())
}

/// Go's `%q` string spelling for command-line domain values.
fn quote_go_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '\u{0007}' => quoted.push_str(r"\a"),
            '\u{0008}' => quoted.push_str(r"\b"),
            '\u{000c}' => quoted.push_str(r"\f"),
            '\n' => quoted.push_str(r"\n"),
            '\r' => quoted.push_str(r"\r"),
            '\t' => quoted.push_str(r"\t"),
            '\u{000b}' => quoted.push_str(r"\v"),
            '\\' => quoted.push_str(r"\\"),
            '"' => quoted.push_str("\\\""),
            character if go_is_print::is_print(character) => quoted.push(character),
            character if u32::from(character) < u32::from(' ') || u32::from(character) == 0x7f => {
                let _ = write!(quoted, r"\x{:02x}", u32::from(character));
            }
            character if u32::from(character) <= 0xffff => {
                let _ = write!(quoted, r"\u{:04x}", u32::from(character));
            }
            character => {
                let _ = write!(quoted, r"\U{:08x}", u32::from(character));
            }
        }
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::quote_go_string;

    #[test]
    fn go_quote_matches_common_fmt_q_escapes() {
        assert_eq!(
            quote_go_string("dev\t\"quoted\"\n\u{0007}\u{2028}"),
            r#""dev\t\"quoted\"\n\a\u2028""#
        );
        assert_eq!(quote_go_string("\u{00ad}"), r#""\u00ad""#);
        assert_eq!(quote_go_string("日本語"), "\"日本語\"");
    }
}
