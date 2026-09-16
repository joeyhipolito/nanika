//! Fixed experimental Codex code -> review -> operator verification cycle.

use std::fs::{self, DirBuilder};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use orchestrator_core::{
    AuthoredParseContext, AuthoredPhase, ExecutionMode, MissionProposal, ProposedPhase,
    VerificationSummary, VerificationTermination, classify_verification, compile_proposal,
};
use orchestrator_exec::AttemptOutcome;
use orchestrator_process::{
    CancellationToken, ProcessReport, ProcessSpec, ProcessSupervisor, ProcessTermination,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::*;

const REVIEW_SCHEMA: &str = "nanika.rust-first-use-review.v1";
const MAX_REVIEW_PROMPT_BYTES: usize = 512 * 1024;
pub(crate) const MAX_VERIFICATION_OUTPUT: usize = 32 * 1024 * 1024;
const MAX_VERIFICATION_REPORT_BYTES: u64 = 4096;
const REVIEW_PREFIX: &str = "Review the implementation below against the complete task. This is a read-only, tool-free review: use only the supplied task, complete unified diff, and complete changed-file contents. Modified and added files provide their complete final contents. Deleted files provide their complete original contents and are explicitly marked deleted. Old changed lines are retained in the complete diff; unchanged lines of modified files are retained in the final contents. Return exactly one JSON object with this closed schema and no markdown: {\"schema\":\"nanika.rust-first-use-review.v1\",\"verdict\":\"pass\",\"blockers\":[],\"warnings\":[]}. Set verdict to reject when any blocker exists. A pass is valid only with zero blockers.\n\n";
const TASK_HEADER: &str = "## Task\n";
const DIFF_HEADER: &str = "\n\n## Complete unified diff\n";
const CONTEXT_HEADER: &str = "\n\n## Complete changed-file contents\n";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewVerdict {
    pub schema: String,
    pub verdict: Verdict,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Verdict {
    Pass,
    Reject,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct VerificationReport {
    schema: VerificationSchema,
    discovered: u64,
    executed: u64,
    passed: u64,
    failed: u64,
    required_skipped: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
enum VerificationSchema {
    #[serde(rename = "nanika.rust-first-use-verification.v1")]
    V1,
}

pub(crate) fn run(
    options: &PilotOptions,
    cancellation: CancellationToken,
) -> Result<PilotSummary, PilotError> {
    let task = read_prompt_with_limit(&options.prompt_file, MAX_CODE_PROMPT_BYTES)?;
    validate_fixed_plan(&task).map_err(PilotError::Composition)?;
    let code_route = routing::select(options, &task);
    let layout = create_output_layout(&options.output_dir, PilotCommand::Run)?;
    write_artifact(&layout.root.join("task.md"), task.as_bytes())?;
    let mut phases = Vec::new();
    let result = run_inner(
        options,
        &task,
        &code_route,
        &layout,
        &cancellation,
        &mut phases,
    );
    match result {
        Ok(()) => terminal(&layout.root, phases, true, "completed"),
        Err(reason) => {
            while phases.len() < 3 {
                let (name, dependency) = match phases.len() {
                    0 => ("code", "preparation"),
                    1 => ("review", "code"),
                    _ => ("verification", "review"),
                };
                let record = json!({
                    "phase": name,
                    "status": "skipped",
                    "reason": format!("dependency {dependency} did not pass"),
                    "skipped_dependencies": [dependency],
                });
                write_phase_record(&layout.root, name, &record).map_err(PilotError::Composition)?;
                phases.push(record);
            }
            terminal(&layout.root, phases, false, &reason)
        }
    }
}

fn validate_fixed_plan(task: &str) -> Result<(), String> {
    let proposal = MissionProposal {
        phases: vec![
            ProposedPhase {
                name: "code".to_owned(),
                objective: task.to_owned(),
                persona: "implementer".to_owned(),
                ..ProposedPhase::default()
            },
            ProposedPhase {
                name: "review".to_owned(),
                objective: "Review the completed coding diff".to_owned(),
                persona: "reviewer".to_owned(),
                depends_on: vec!["code".to_owned()],
                ..ProposedPhase::default()
            },
            ProposedPhase {
                name: "verification".to_owned(),
                objective: "Run operator-supplied verification".to_owned(),
                persona: "operator-verifier".to_owned(),
                depends_on: vec!["review".to_owned()],
                ..ProposedPhase::default()
            },
        ],
    };
    let plan = compile_proposal(&proposal, &AuthoredParseContext::default())
        .map_err(|error| format!("compiling fixed pilot plan: {error}"))?;
    if plan.execution_mode != ExecutionMode::Sequential || plan.phases.len() != 3 {
        return Err("core planner did not preserve the fixed sequential cycle".to_owned());
    }
    Ok(())
}

fn run_inner(
    options: &PilotOptions,
    task: &str,
    code_route: &routing::Route,
    layout: &OutputLayout,
    cancellation: &CancellationToken,
    phases: &mut Vec<Value>,
) -> Result<(), String> {
    let supervisor = ProcessSupervisor::process_wide().map_err(|error| error.to_string())?;
    let repo = options
        .repo
        .as_deref()
        .ok_or_else(|| "run lost its required repository".to_owned())?;
    let snapshot = snapshot::prepare(repo, &layout.root, &supervisor, cancellation)?;
    // The designated report directory exists before coding so any attempt to
    // pre-seed the verifier's report is detected rather than consumed later.
    private_dir(&layout.root.join("verification"))?;
    let observed_version =
        probe_runtime_version(options, &snapshot.workspace, cancellation, &supervisor)
            .map_err(|error| error.to_string())?;

    private_dir(&layout.root.join("code"))?;
    let service = codex_service(options, cancellation.clone(), layout.shell_config.clone())?;
    let mut code_options = options.clone();
    code_options.command = PilotCommand::Code;
    let outcome = dispatch_review(
        &code_options,
        task,
        code_route,
        &snapshot.workspace,
        &service,
    )
    .map_err(|error| error.to_string())?;
    let observation = lock(&service.observation).take();
    save_observation(&layout.root.join("code"), observation.as_ref())?;
    let diff = snapshot.diff(&supervisor)?;
    write_artifact(&layout.root.join("changes.diff"), &diff).map_err(|error| error.to_string())?;
    snapshot.verify_source(&supervisor)?;
    let code_passed = matches!(outcome, AttemptOutcome::Completed(_));
    if let Some(answer) = outcome.output() {
        write_artifact(&layout.root.join("code/answer.md"), answer.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    let mut code_record = attempt_record(
        "code",
        code_passed,
        if code_passed {
            "Codex coding attempt completed"
        } else {
            "Codex coding attempt failed"
        },
        code_route,
        &observed_version,
        &outcome,
        observation.as_ref(),
    );
    code_record["source_repository"] = json!({
        "path": snapshot.source,
        "head": snapshot.head,
        "preserved": true,
    });
    code_record["diff_path"] = json!(layout.root.join("changes.diff"));
    write_phase_record(&layout.root, "code", &code_record)?;
    phases.push(code_record);
    if !code_passed {
        return Err("code phase failed".to_owned());
    }

    let mut review_options = options.clone();
    review_options.command = PilotCommand::Review;
    review_options.persona = Some("reviewer".to_owned());
    let review_route = routing::select(&review_options, task);
    let review_prompt = match build_review_prompt(&snapshot, task, &diff) {
        Ok(prompt) => prompt,
        Err(reason) => {
            private_dir(&layout.root.join("review"))?;
            let record = json!({
                "phase": "review", "status": "failed", "reason": reason,
                "skipped_dependencies": [], "runtime": "codex",
                "route": review_route.record(), "provider_dispatched": false,
            });
            write_phase_record(&layout.root, "review", &record)?;
            phases.push(record);
            return Err(reason);
        }
    };
    private_dir(&layout.root.join("review"))?;
    write_artifact(
        &layout.root.join("review/prompt.md"),
        review_prompt.as_bytes(),
    )
    .map_err(|error| error.to_string())?;
    let review_route = routing::select(&review_options, &review_prompt);
    let review_service = codex_service(options, cancellation.clone(), None)?;
    let review_outcome = dispatch_review(
        &review_options,
        &review_prompt,
        &review_route,
        &snapshot.workspace,
        &review_service,
    )
    .map_err(|error| error.to_string())?;
    let review_observation = lock(&review_service.observation).take();
    save_observation(&layout.root.join("review"), review_observation.as_ref())?;
    if let Some(answer) = review_outcome.output() {
        write_artifact(&layout.root.join("review/answer.md"), answer.as_bytes())
            .map_err(|error| error.to_string())?;
    }
    let (mut review_passed, mut review_reason, verdict_valid) =
        match completed_review_answer(&review_outcome) {
            Ok(answer) => match parse_verdict(answer) {
                Ok(verdict) => {
                    let bytes = serde_json::to_vec_pretty(&verdict).map_err(|e| e.to_string())?;
                    write_artifact(&layout.root.join("review/verdict.json"), &bytes)
                        .map_err(|error| error.to_string())?;
                    match verdict.verdict {
                        Verdict::Pass if verdict.blockers.is_empty() => (
                            true,
                            "structured review passed with zero blockers".to_owned(),
                            true,
                        ),
                        Verdict::Pass => (
                            false,
                            "review claimed pass while reporting blockers".to_owned(),
                            true,
                        ),
                        Verdict::Reject => {
                            (false, "review rejected the implementation".to_owned(), true)
                        }
                    }
                }
                Err(reason) => (false, reason, false),
            },
            Err(reason) => (false, reason, false),
        };
    if review_passed {
        match snapshot.diff(&supervisor) {
            Ok(after_review) if after_review == diff => {}
            Ok(_) => {
                review_passed = false;
                review_reason = "read-only review changed the coding workspace".to_owned();
            }
            Err(reason) => {
                review_passed = false;
                review_reason = reason;
            }
        }
    }
    if review_passed {
        if let Err(reason) = snapshot.verify_source(&supervisor) {
            review_passed = false;
            review_reason = reason;
        }
    }
    let mut review_record = attempt_record(
        "review",
        review_passed,
        &review_reason,
        &review_route,
        &observed_version,
        &review_outcome,
        review_observation.as_ref(),
    );
    review_record["verdict_valid"] = json!(verdict_valid);
    write_phase_record(&layout.root, "review", &review_record)?;
    phases.push(review_record);
    if !review_passed {
        return Err(review_reason);
    }

    let checkpoint = snapshot.checkpoint_workspace(&layout.root.join("reviewed-workspace"))?;

    let mut verification = match run_verification(
        options,
        &snapshot.workspace,
        &layout.root,
        cancellation,
        &supervisor,
    ) {
        Ok(result) => result,
        Err(reason) => {
            let record = json!({
                "phase": "verification", "status": "failed", "reason": reason,
                "skipped_dependencies": [], "runtime": "trusted-local-exec",
                "model": null, "effort": null, "persona": "operator-verifier",
            });
            write_phase_record(&layout.root, "verification", &record)?;
            phases.push(record);
            return Err(reason);
        }
    };
    assess_verification_workspace(
        &snapshot,
        &checkpoint,
        &layout.root.join("verification"),
        &supervisor,
        &mut verification,
    );
    if let Err(reason) = snapshot.verify_source(&supervisor) {
        verification.passed = false;
        verification.reason = reason.clone();
        verification.record["status"] = json!("failed");
        verification.record["reason"] = json!(reason);
        verification.record["source_repository_preserved"] = json!(false);
    } else {
        verification.record["source_repository_preserved"] = json!(true);
    }
    write_phase_record(&layout.root, "verification", &verification.record)?;
    phases.push(verification.record);
    if !verification.passed {
        return Err(verification.reason);
    }
    Ok(())
}

pub(crate) fn codex_service(
    options: &PilotOptions,
    cancellation: CancellationToken,
    shell_config: Option<PathBuf>,
) -> Result<PilotProcessService, String> {
    let supervisor = ProcessSupervisor::process_wide().map_err(|error| error.to_string())?;
    let service = PilotProcessService::new(
        options.codex_executable.clone(),
        cancellation,
        supervisor,
        shell_config,
    )
    .map_err(|error| error.to_string())?;
    Ok(PilotProcessService {
        executable_id: "codex".to_owned(),
        ..service
    })
}

pub(crate) fn build_review_prompt(
    snapshot: &snapshot::RepositorySnapshot,
    task: &str,
    diff: &[u8],
) -> Result<String, String> {
    let diff = std::str::from_utf8(diff).map_err(|_| "full diff is not UTF-8".to_owned())?;
    let fixed = REVIEW_PREFIX
        .len()
        .checked_add(TASK_HEADER.len())
        .and_then(|length| length.checked_add(task.len()))
        .and_then(|length| length.checked_add(DIFF_HEADER.len()))
        .and_then(|length| length.checked_add(diff.len()))
        .and_then(|length| length.checked_add(CONTEXT_HEADER.len()))
        .ok_or_else(|| "review prompt size overflow".to_owned())?;
    if fixed > MAX_REVIEW_PROMPT_BYTES {
        return Err(format!(
            "task and full diff exceed the {MAX_REVIEW_PROMPT_BYTES}-byte review bound"
        ));
    }
    let context = snapshot.changed_source_context(MAX_REVIEW_PROMPT_BYTES - fixed)?;
    let context =
        String::from_utf8(context).map_err(|_| "source context is not UTF-8".to_owned())?;
    let mut prompt = String::with_capacity(fixed + context.len());
    prompt.push_str(REVIEW_PREFIX);
    prompt.push_str(TASK_HEADER);
    prompt.push_str(task);
    prompt.push_str(DIFF_HEADER);
    prompt.push_str(diff);
    prompt.push_str(CONTEXT_HEADER);
    prompt.push_str(&context);
    if prompt.len() > MAX_REVIEW_PROMPT_BYTES {
        return Err(format!(
            "complete review context exceeds the {MAX_REVIEW_PROMPT_BYTES}-byte review bound"
        ));
    }
    Ok(prompt)
}

pub(crate) fn parse_verdict(answer: &str) -> Result<ReviewVerdict, String> {
    let verdict: ReviewVerdict = serde_json::from_str(answer).map_err(|_| {
        "review verdict is malformed or does not match the closed schema".to_owned()
    })?;
    if verdict.schema != REVIEW_SCHEMA {
        return Err("review verdict has the wrong schema".to_owned());
    }
    for finding in verdict.blockers.iter().chain(&verdict.warnings) {
        if finding.trim().is_empty() || finding.len() > 4096 {
            return Err("review verdict contains an invalid finding".to_owned());
        }
    }
    if verdict.blockers.len() > 100 || verdict.warnings.len() > 100 {
        return Err("review verdict contains too many findings".to_owned());
    }
    Ok(verdict)
}

pub(crate) fn completed_review_answer(outcome: &AttemptOutcome) -> Result<&str, String> {
    if !outcome.is_completed() {
        return Err("review provider did not complete".to_owned());
    }
    outcome
        .output()
        .ok_or_else(|| "completed review lost its output".to_owned())
}

pub(crate) struct VerificationResult {
    pub passed: bool,
    pub reason: String,
    pub record: Value,
}

fn run_verification(
    options: &PilotOptions,
    workspace: &Path,
    root: &Path,
    cancellation: &CancellationToken,
    supervisor: &ProcessSupervisor,
) -> Result<VerificationResult, String> {
    run_verification_at(
        options,
        workspace,
        &root.join("verification"),
        "verification",
        "operator-verifier",
        cancellation,
        supervisor,
    )
}

pub(crate) fn run_verification_at(
    options: &PilotOptions,
    workspace: &Path,
    phase_root: &Path,
    phase: &str,
    persona: &str,
    cancellation: &CancellationToken,
    supervisor: &ProcessSupervisor,
) -> Result<VerificationResult, String> {
    let report_path = phase_root.join("report.json");
    if report_path.symlink_metadata().is_ok() {
        return Err("verification report path was not fresh".to_owned());
    }
    let mut spec = ProcessSpec::new(
        options.verification_argv.clone(),
        options.verification_timeout,
    )
    .map_err(|error| format!("building verification request: {error}"))?
    .with_stall_timeout(options.verification_timeout.min(MAX_STALL_WINDOW))
    .with_max_output_bytes(MAX_VERIFICATION_OUTPUT)
    .with_cancellation(cancellation.clone())
    .with_env("NANIKA_VERIFICATION_REPORT", &report_path);
    for key in INHERITED_ENVIRONMENT {
        spec = spec.with_inherited_env(key);
    }
    let process = supervisor
        .run(&spec, workspace)
        .map_err(|error| format!("running verification: {error}"))?;
    write_artifact(&phase_root.join("stdout.txt"), &process.stdout)
        .map_err(|error| error.to_string())?;
    write_artifact(&phase_root.join("stderr.txt"), &process.stderr)
        .map_err(|error| error.to_string())?;
    let parsed = read_verification_report(&report_path).ok();
    let summary = parsed.map(|report| VerificationSummary {
        discovered: report.discovered,
        executed: report.executed,
        passed: report.passed,
        failed: report.failed,
        required_skipped: report.required_skipped,
    });
    let outcome = classify_verification(verification_termination(&process), summary);
    let passed = outcome.gate_passed() && process.is_success();
    let reason = if passed {
        "operator verification passed with positive structured counts".to_owned()
    } else if parsed.is_none() {
        "verification report is missing, malformed, stale, or has the wrong schema".to_owned()
    } else if !process.cleanup_complete || !process.infrastructure_failures.is_empty() {
        "verification process cleanup was incomplete".to_owned()
    } else {
        format!("verification classified as {outcome:?}")
    };
    let argv: Vec<String> = options
        .verification_argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let record = json!({
        "phase": phase,
        "status": if passed { "passed" } else { "failed" },
        "reason": reason,
        "skipped_dependencies": [],
        "runtime": "trusted-local-exec",
        "model": null,
        "effort": null,
        "persona": persona,
        "argv": argv,
        "report_path": report_path,
        "report": parsed,
        "classification": format!("{outcome:?}"),
        "process": process_record(&process),
    });
    Ok(VerificationResult {
        passed,
        reason,
        record,
    })
}

pub(crate) fn run_verification_at_durable(
    options: &PilotOptions,
    workspace: &Path,
    phase_root: &Path,
    phase: &AuthoredPhase,
    cancellation: &CancellationToken,
    owner: &durable::DurableProcessOwner,
) -> Result<VerificationResult, String> {
    let report_path = phase_root.join("report.json");
    if report_path.symlink_metadata().is_ok() {
        return Err("verification report path was not fresh".to_owned());
    }
    let executable = options
        .verification_argv
        .first()
        .ok_or_else(|| "verification argv is empty".to_owned())?;
    let service = owner.service(
        phase.id.as_str(),
        Path::new(executable),
        PathBuf::from("workspace"),
        None,
        cancellation.clone(),
    )?;
    let request = durable_verification_request(options, workspace, &report_path, &service)?;
    let hard_deadline = Instant::now() + options.verification_timeout;
    let budget = ProcessBudget::new(
        hard_deadline,
        options.verification_timeout,
        options.verification_timeout.min(MAX_STALL_WINDOW),
    );
    let receipt = service
        .execute(&request, budget)
        .map_err(|error| format!("running verification: {error}"))?;
    let observation = service.take_observation();
    let stdout = observation
        .as_ref()
        .map_or_else(|| receipt.expose_stdout(), |value| value.stdout.as_slice());
    let stderr = observation
        .as_ref()
        .map_or_else(|| receipt.expose_stderr(), |value| value.stderr.as_slice());
    write_artifact(&phase_root.join("stdout.txt"), stdout).map_err(|error| error.to_string())?;
    write_artifact(&phase_root.join("stderr.txt"), stderr).map_err(|error| error.to_string())?;
    let parsed = read_verification_report(&report_path).ok();
    let summary = parsed.map(|report| VerificationSummary {
        discovered: report.discovered,
        executed: report.executed,
        passed: report.passed,
        failed: report.failed,
        required_skipped: report.required_skipped,
    });
    let outcome = classify_verification(receipt_verification_termination(&receipt), summary);
    let passed = outcome.gate_passed() && receipt.is_success();
    let reason = if passed {
        "operator verification passed with positive structured counts".to_owned()
    } else if parsed.is_none() {
        "verification report is missing, malformed, stale, or has the wrong schema".to_owned()
    } else if !receipt.ownership_released() {
        "verification process cleanup was incomplete".to_owned()
    } else {
        format!("verification classified as {outcome:?}")
    };
    let argv: Vec<String> = options
        .verification_argv
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let process = observation.map_or(Value::Null, |value| value.summary);
    let record = json!({
        "phase": phase.name,
        "status": if passed { "passed" } else { "failed" },
        "reason": reason,
        "skipped_dependencies": [],
        "runtime": "trusted-local-exec",
        "model": null,
        "effort": null,
        "persona": phase.persona,
        "argv": argv,
        "report_path": report_path,
        "report": parsed,
        "classification": format!("{outcome:?}"),
        "process": process,
    });
    Ok(VerificationResult {
        passed,
        reason,
        record,
    })
}

pub(crate) fn durable_verification_request(
    options: &PilotOptions,
    workspace: &Path,
    report_path: &Path,
    service: &durable::DurablePilotProcessService,
) -> Result<ProcessRequest, String> {
    let mut request = ProcessRequest::new(
        ProcessPurpose::Verification,
        service.executable_id(),
        workspace,
    )
    .map_err(|error| format!("building verification request: {error}"))?;
    for argument in options.verification_argv.iter().skip(1) {
        let argument = argument
            .to_str()
            .ok_or_else(|| "verification argument is not UTF-8".to_owned())?;
        request = request
            .with_argument(argument)
            .map_err(|error| format!("building verification request: {error}"))?;
    }
    let report = report_path
        .to_str()
        .ok_or_else(|| "verification report path is not UTF-8".to_owned())?;
    request = request
        .with_environment("NANIKA_VERIFICATION_REPORT", report)
        .map_err(|error| format!("building verification request: {error}"))?;
    for (name, value) in service.process_environment() {
        request = request
            .with_environment(name, value)
            .map_err(|error| format!("building verification request: {error}"))?;
    }
    request
        .with_max_output_bytes(MAX_VERIFICATION_OUTPUT)
        .map_err(|error| format!("building verification request: {error}"))
}

pub(crate) fn assess_verification_workspace(
    snapshot: &snapshot::RepositorySnapshot,
    checkpoint: &snapshot::WorkspaceCheckpoint,
    phase_root: &Path,
    supervisor: &ProcessSupervisor,
    result: &mut VerificationResult,
) {
    match checkpoint.verify_after_verification(snapshot, supervisor) {
        Ok(outputs) => {
            let entries: Vec<Value> = outputs
                .iter()
                .map(|output| {
                    json!({
                        "path": output.path,
                        "kind": output.kind,
                        "bytes": output.bytes,
                    })
                })
                .collect();
            let artifact = json!({
                "schema": "nanika.rust-first-use-verification-outputs.v1",
                "count": entries.len(),
                "entries": entries,
            });
            let write_result = serde_json::to_vec_pretty(&artifact)
                .map_err(|error| error.to_string())
                .and_then(|mut bytes| {
                    bytes.push(b'\n');
                    write_artifact(&phase_root.join("generated-outputs.json"), &bytes)
                        .map_err(|error| error.to_string())
                });
            match write_result {
                Ok(()) => {
                    result.record["reviewed_workspace_preserved"] = json!(true);
                    result.record["generated_outputs"] = json!({
                        "count": outputs.len(),
                        "manifest_path": phase_root.join("generated-outputs.json"),
                    });
                }
                Err(reason) => fail_verification_workspace(result, reason),
            }
        }
        Err(reason) => fail_verification_workspace(result, reason),
    }
}

fn fail_verification_workspace(result: &mut VerificationResult, reason: String) {
    result.passed = false;
    result.reason = reason.clone();
    result.record["status"] = json!("failed");
    result.record["reason"] = json!(reason);
    result.record["reviewed_workspace_preserved"] = json!(false);
    result.record["generated_outputs"] = Value::Null;
}

fn read_verification_report(path: &Path) -> Result<VerificationReport, String> {
    let metadata = path
        .symlink_metadata()
        .map_err(|_| "designated verification report was not created".to_owned())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("designated verification report is not a regular file".to_owned());
    }
    if metadata.len() > MAX_VERIFICATION_REPORT_BYTES {
        return Err(format!(
            "verification report exceeds {MAX_VERIFICATION_REPORT_BYTES} bytes"
        ));
    }
    let bytes = fs::read(path).map_err(|error| format!("reading verification report: {error}"))?;
    serde_json::from_slice(&bytes).map_err(|_| "verification report is malformed".to_owned())
}

fn verification_termination(report: &ProcessReport) -> VerificationTermination {
    match report.termination {
        ProcessTermination::Exited(code) => VerificationTermination::Exited(code),
        ProcessTermination::Signaled(signal) => VerificationTermination::Signaled(signal),
        ProcessTermination::Timeout => VerificationTermination::TimedOut,
        ProcessTermination::Stalled => VerificationTermination::Stalled,
        ProcessTermination::Cancelled => VerificationTermination::Cancelled,
        ProcessTermination::OutputLimit
        | ProcessTermination::InfrastructureError
        | ProcessTermination::UnresolvedOwnership => VerificationTermination::InfrastructureError,
    }
}

fn receipt_verification_termination(receipt: &ProcessReceipt) -> VerificationTermination {
    match receipt.termination() {
        ProcessTerminationReceipt::Exited(status) => status.as_code().map_or(
            VerificationTermination::InfrastructureError,
            VerificationTermination::Exited,
        ),
        ProcessTerminationReceipt::Cancelled => VerificationTermination::Cancelled,
        ProcessTerminationReceipt::DeadlineExceeded => VerificationTermination::TimedOut,
        ProcessTerminationReceipt::Stalled => VerificationTermination::Stalled,
        ProcessTerminationReceipt::OutputLimit
        | ProcessTerminationReceipt::SupervisorFailure
        | ProcessTerminationReceipt::UnresolvedOwnership => {
            VerificationTermination::InfrastructureError
        }
    }
}

pub(crate) fn attempt_record(
    phase: &str,
    passed: bool,
    reason: &str,
    route: &routing::Route,
    version: &str,
    outcome: &AttemptOutcome,
    observation: Option<&Observation>,
) -> Value {
    let usage = if outcome.is_completed() {
        observation.and_then(|value| codex::completed_usage_value(&value.stdout))
    } else {
        None
    };
    json!({
        "phase": phase,
        "status": if passed { "passed" } else { "failed" },
        "reason": reason,
        "skipped_dependencies": [],
        "runtime": "codex",
        "runtime_version_observed": version,
        "route": route.record(),
        "termination": outcome.termination().map(|value| format!("{value:?}")),
        "elapsed_ms": u64::try_from(outcome.elapsed().as_millis()).unwrap_or(u64::MAX),
        "usage": usage,
        "cost_usd": null,
        "process": observation.map_or(Value::Null, |value| value.summary.clone()),
    })
}

pub(crate) fn save_observation(
    root: &Path,
    observation: Option<&Observation>,
) -> Result<(), String> {
    let Some(observation) = observation else {
        return Ok(());
    };
    write_artifact(&root.join("codex-stdout.jsonl"), &observation.stdout)
        .map_err(|error| error.to_string())?;
    write_artifact(&root.join("codex-stderr.txt"), &observation.stderr)
        .map_err(|error| error.to_string())
}

fn process_record(report: &ProcessReport) -> Value {
    json!({
        "termination": format!("{:?}", report.termination),
        "spawned": report.spawned,
        "cancellation_observed": report.cancellation_observed,
        "deadline_observed": report.deadline_observed,
        "stall_observed": report.stall_observed,
        "term_sent": report.term_sent,
        "kill_sent": report.kill_sent,
        "cleanup_complete": report.cleanup_complete,
        "infrastructure_failures": format!("{:?}", report.infrastructure_failures),
        "stdout_bytes": report.stdout.len(),
        "stderr_bytes": report.stderr.len(),
        "stdout_discarded_bytes": report.stdout_discarded_bytes,
        "stderr_discarded_bytes": report.stderr_discarded_bytes,
        "elapsed_ms": u64::try_from(report.elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

fn write_phase_record(root: &Path, phase: &str, record: &Value) -> Result<(), String> {
    let directory = root.join(phase);
    if !directory.exists() {
        private_dir(&directory)?;
    }
    let mut bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    bytes.push(b'\n');
    write_artifact(&directory.join("phase-result.json"), &bytes).map_err(|error| error.to_string())
}

fn terminal(
    root: &Path,
    phases: Vec<Value>,
    completed: bool,
    reason: &str,
) -> Result<PilotSummary, PilotError> {
    let phase_passed = |index: usize| {
        phases
            .get(index)
            .is_some_and(|phase| phase["status"] == "passed")
    };
    let provider_completed = phase_passed(0) && phase_passed(1);
    let tests_verified = phase_passed(2);
    let record = json!({
        "schema": RESULT_SCHEMA,
        "command": "run",
        "status": if completed { "completed" } else { "failed" },
        "reason": reason,
        "phases": phases,
        "provider_completed": provider_completed,
        "tests_verified": tests_verified,
    });
    let result_path = write_result(root, &record)?;
    Ok(PilotSummary {
        completed,
        status: if completed { "completed" } else { "failed" },
        reason: reason.to_owned(),
        result_path,
    })
}

pub(crate) fn private_dir(path: &Path) -> Result<(), String> {
    let mut builder = DirBuilder::new();
    builder.mode(0o700);
    builder
        .create(path)
        .map_err(|error| format!("creating {}: {error}", path.display()))
}
