//! Regression proof that ordinary `orchestrator run` cannot execute a worker
//! and cannot resolve an ambient provider, even if the removed
//! `NANIKA_RUN_EXECUTE_DEV` environment variable is set.
//!
//! These tests are the Commit A safety gate required by
//! `CODEX-REVIEW-TO-OPENCODE-RUN-SLICE-REMEDIATION.md` (findings B2/B3).

use std::process::Command;

#[test]
fn run_refuses_execution_unconditionally() -> Result<(), String> {
    // A bare `orchestrator run "task"` (no --dry-run) must fail. Whether it
    // fails at composition or at the ExecutionNotEnrolled gate depends on the
    // test environment's home/routing state, but it must never reach a worker
    // spawn or provider dispatch.
    let bin = env!("CARGO_BIN_EXE_orchestrator");
    let output = Command::new(bin)
        .args(["run", "test task that must not execute"])
        .output()
        .map_err(|source| format!("orchestrator binary should spawn: {source}"))?;
    assert!(
        !output.status.success(),
        "orchestrator run must not succeed without --dry-run: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("worker stdout"),
        "the removed dev-execution path must not produce worker output"
    );
    Ok(())
}

#[test]
fn run_refuses_execution_even_with_removed_dev_env() -> Result<(), String> {
    // Even if NANIKA_RUN_EXECUTE_DEV=1 is set (the removed bypass), the
    // binary must behave identically to the unconditional refusal above.
    let bin = env!("CARGO_BIN_EXE_orchestrator");
    let output = Command::new(bin)
        .args(["run", "test task that must not execute"])
        .env("NANIKA_RUN_EXECUTE_DEV", "1")
        .output()
        .map_err(|source| format!("orchestrator binary should spawn: {source}"))?;
    assert!(
        !output.status.success(),
        "NANIKA_RUN_EXECUTE_DEV=1 must not enable execution: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("worker stdout"),
        "the removed dev-execution path must not produce worker output even with the env set"
    );
    assert!(
        stderr.contains("not enrolled") || stderr.contains("ExecutionNotEnrolled"),
        "the refusal must be the enrollment gate, not a silent bypass: {stderr}"
    );
    Ok(())
}
