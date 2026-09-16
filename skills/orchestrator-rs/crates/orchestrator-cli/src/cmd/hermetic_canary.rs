#![cfg(all(unix, feature = "verification-process-canary"))]

//! `orchestrator hermetic-canary`: disposable verification-canary command.
//!
//! The command is gated three ways:
//!
//! 1. **Compile time** — only built when the `verification-process-canary`
//!    feature is on, so a stock orchestrator binary does not even register
//!    the command.
//! 2. **Runtime env** — requires `NANIKA_HERMETIC_PROCESS_CANARY=1`; any other
//!    value (or unset) is refused before the canary touches the filesystem.
//! 3. **Exact root** — requires `NANIKA_HERMETIC_RUN_ROOT=<fresh or enrolled
//!    exact path>`. A fresh path is published; an enrolled pending attempt may
//!    continue, while a proven terminal attempt replays without relaunching.
//! 4. **Closed enrollment** — the canary enrolls `current_exe()` itself; no
//!    caller argument can substitute a helper path or a live home.
//!
//! Normal `orchestrator run` is untouched and stays denied for Rust.

use std::io::Write;

use orchestrator_app::{
    FixtureAdmissionPolicy, HermeticProcessCanaryError, HermeticProcessCanaryReport,
    run_hermetic_process_canary,
};
use thiserror::Error;

/// The runtime opt-in environment variable. Only the exact value `"1"`
/// enrolls the canary, matching the live-home enrollment selector's
/// strictness.
pub(crate) const OPT_IN_ENV: &str = "NANIKA_HERMETIC_PROCESS_CANARY";

#[derive(Debug, Error)]
pub(crate) enum HermeticCanaryCmdError {
    #[error(
        "hermetic-canary requires {env}=1; the command never runs without explicit operator opt-in"
    )]
    NotOptedIn { env: &'static str },
    #[error(
        "hermetic-canary requires {env}=<fresh or previously enrolled exact root>; the canary never runs without an explicit operator-owned root"
    )]
    MissingRunRoot { env: &'static str },
    #[error("hermetic-canary could not resolve the operator environment: {0}")]
    Environment(String),
    #[error("hermetic-canary output failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("hermetic process canary did not prove a successful terminal decision")]
    CanaryFailed,
    #[error(transparent)]
    Canary(#[from] HermeticProcessCanaryError),
}

/// Runs the disposable hermetic process canary. Builds the admission policy
/// from the operator's `HOME`, current working directory, and canonical
/// temporary directory so the canary's leaf can neither overlap a live home
/// nor the repository checkout.
pub(crate) fn run<W: Write>(output: &mut W) -> Result<(), HermeticCanaryCmdError> {
    if std::env::var(OPT_IN_ENV).ok().as_deref() != Some("1") {
        return Err(HermeticCanaryCmdError::NotOptedIn { env: OPT_IN_ENV });
    }
    if std::env::var_os(orchestrator_app::HERMETIC_RUN_ROOT_ENV).is_none() {
        return Err(HermeticCanaryCmdError::MissingRunRoot {
            env: orchestrator_app::HERMETIC_RUN_ROOT_ENV,
        });
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| HermeticCanaryCmdError::Environment("HOME is not set".to_owned()))?;
    let cwd = std::env::current_dir().map_err(|source| {
        HermeticCanaryCmdError::Environment(format!("current directory is unavailable: {source}"))
    })?;
    let temp = std::env::temp_dir();
    let policy = FixtureAdmissionPolicy::new(home, cwd, &temp);

    let report = run_hermetic_process_canary(&policy)?;
    write_report(output, &report)
}

fn write_report<W: Write>(
    output: &mut W,
    report: &HermeticProcessCanaryReport,
) -> Result<(), HermeticCanaryCmdError> {
    writeln!(
        output,
        "hermetic process canary: {}",
        report.terminal_status
    )?;
    writeln!(
        output,
        "  spawned:       {}",
        match (report.spawned, report.terminal_status.as_str()) {
            (true, _) => "yes",
            (false, "succeeded") => "no (terminal replay)",
            (false, _) => "no",
        }
    )?;
    writeln!(output, "  leaf:          {}", report.leaf_label)?;
    writeln!(
        output,
        "  helper:        {} ({} bytes, sha256 {})",
        report.helper_label, report.helper_length, report.helper_sha256_hex
    )?;
    writeln!(output, "  mission:       {}", report.mission_id)?;
    writeln!(output, "  worker:        {}", report.worker_id)?;
    writeln!(output, "  executable id: {}", report.executable_logical_id)?;
    if report.terminal_status != "succeeded" {
        return Err(HermeticCanaryCmdError::CanaryFailed);
    }
    Ok(())
}

/// Hidden canary worker mode: the orchestrator binary, invoked with the exact
/// flag `--hermetic-canary-worker`, consumes one exact challenge frame from
/// stdin and emits only its PID-bound proof. It never mutates the filesystem.
pub fn run_hermetic_canary_worker() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    if let Err(error) =
        orchestrator_app::run_hermetic_canary_worker_protocol(stdin.lock(), stdout.lock())
    {
        eprintln!("hermetic-canary-worker: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(status: &str) -> HermeticProcessCanaryReport {
        HermeticProcessCanaryReport {
            leaf_label: "leaf".to_owned(),
            helper_label: "helper".to_owned(),
            mission_id: "mission".to_owned(),
            worker_id: "worker".to_owned(),
            helper_length: 7,
            helper_sha256_hex: "00".repeat(32),
            executable_logical_id: "logical-helper".to_owned(),
            terminal_status: status.to_owned(),
            spawned: true,
        }
    }

    #[test]
    fn failed_terminal_report_cannot_exit_successfully() {
        let mut output = Vec::new();

        let result = write_report(&mut output, &report("failed"));

        assert!(matches!(result, Err(HermeticCanaryCmdError::CanaryFailed)));
        assert!(String::from_utf8_lossy(&output).contains("canary: failed"));
    }

    #[test]
    fn succeeded_terminal_report_is_accepted() {
        let mut output = Vec::new();

        let result = write_report(&mut output, &report("succeeded"));

        assert!(result.is_ok(), "successful report was rejected: {result:?}");
    }
}
