#![cfg(unix)]

use std::error::Error;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use orchestrator_exec::{
    Effort, ExecutionRequest, ExecutionRequestDraft, ExecutionRequestFingerprint, RuntimeFamily,
    SessionHandle,
};

const WORKER_ID: &str = "worker-mission-42-phase-7";
const REQUESTED_RUNTIME: &str = "claude-fallback-alias";

type TestResult = Result<(), Box<dyn Error>>;

fn baseline_draft() -> Result<ExecutionRequestDraft, Box<dyn Error>> {
    let runtime = RuntimeFamily::parse("claude")?;
    Ok(ExecutionRequestDraft {
        mission: "mission-42".to_owned(),
        phase: "phase-7-implement".to_owned(),
        attempt: 3,
        revision: 8,
        objective: "Implement the durable execution request binding.".to_owned(),
        persona: "systems-engineer".to_owned(),
        role: "implementer".to_owned(),
        domain: "dev".to_owned(),
        skills: vec!["rust-best-practices".to_owned(), "testing".to_owned()],
        dependencies: vec!["phase-6-design".to_owned()],
        expected_evidence: vec!["cargo test".to_owned(), "cargo clippy".to_owned()],
        constraints: vec![
            "preserve exact request semantics".to_owned(),
            "do not expose secrets".to_owned(),
        ],
        prior_context: "The registry resolved a compatibility fallback.".to_owned(),
        runtime: runtime.clone(),
        model: "claude-opus-4-1".to_owned(),
        effort: Effort::XHigh,
        max_turns: 96,
        worker_dir: PathBuf::from("/var/lib/nanika/workers/mission-42/phase-7"),
        target_dir: Some(PathBuf::from("/srv/repositories/nanika")),
        resume_from: Some(SessionHandle::new(runtime, "session-secret-7")?),
        hook_script: Some(PathBuf::from("/opt/nanika/hooks/post-turn.sh")),
    })
}

fn baseline_request() -> Result<ExecutionRequest, Box<dyn Error>> {
    Ok(ExecutionRequest::new(baseline_draft()?)?)
}

fn mutation_baseline_draft() -> Result<ExecutionRequestDraft, Box<dyn Error>> {
    let mut draft = baseline_draft()?;
    draft.resume_from = None;
    Ok(draft)
}

fn fingerprint(
    draft: ExecutionRequestDraft,
) -> Result<ExecutionRequestFingerprint, Box<dyn Error>> {
    Ok(ExecutionRequest::new(draft)?.fingerprint(WORKER_ID, REQUESTED_RUNTIME))
}

#[test]
fn v1_digest_matches_the_frozen_golden() -> TestResult {
    let fingerprint = baseline_request()?.fingerprint(WORKER_ID, REQUESTED_RUNTIME);

    assert_eq!(
        (
            ExecutionRequestFingerprint::schema_version(),
            fingerprint.to_lowercase_hex(),
        ),
        (
            1,
            "2485e562ce9736ddea9a47c4fdc9b25e16e3e9a838197dcbf2d1b18fe207a689".to_owned(),
        )
    );
    Ok(())
}

#[test]
fn identical_requests_have_identical_fingerprints() -> TestResult {
    let first = baseline_request()?.fingerprint(WORKER_ID, REQUESTED_RUNTIME);
    let second = baseline_request()?.fingerprint(WORKER_ID, REQUESTED_RUNTIME);

    assert_eq!(first, second);
    Ok(())
}

#[test]
fn worker_and_raw_requested_runtime_are_bound() -> TestResult {
    let request = baseline_request()?;
    let baseline = request.fingerprint(WORKER_ID, REQUESTED_RUNTIME);
    let changed_worker = request.fingerprint("worker-mission-42-phase-8", REQUESTED_RUNTIME);
    let changed_requested_runtime = request.fingerprint(WORKER_ID, "claude");

    assert_ne!(baseline, changed_worker);
    assert_ne!(baseline, changed_requested_runtime);
    Ok(())
}

#[test]
fn every_execution_request_draft_field_is_bound_independently() -> TestResult {
    let baseline = fingerprint(mutation_baseline_draft()?)?;
    let alternate_runtime = RuntimeFamily::parse("codex")?;
    let resume = SessionHandle::new(RuntimeFamily::parse("claude")?, "session-secret-8")?;

    macro_rules! assert_draft_change {
        ($field:literal, $mutation:expr) => {{
            let mut draft = mutation_baseline_draft()?;
            $mutation(&mut draft);
            let changed = fingerprint(draft)?;
            assert_ne!(
                baseline, changed,
                "{} was omitted from the fingerprint",
                $field
            );
        }};
    }

    assert_draft_change!("mission", |draft: &mut ExecutionRequestDraft| {
        draft.mission = "mission-43".to_owned();
    });
    assert_draft_change!("phase", |draft: &mut ExecutionRequestDraft| {
        draft.phase = "phase-8-review".to_owned();
    });
    assert_draft_change!("attempt", |draft: &mut ExecutionRequestDraft| {
        draft.attempt = 4;
    });
    assert_draft_change!("revision", |draft: &mut ExecutionRequestDraft| {
        draft.revision = 9;
    });
    assert_draft_change!("objective", |draft: &mut ExecutionRequestDraft| {
        draft.objective = "Review the durable execution request binding.".to_owned();
    });
    assert_draft_change!("persona", |draft: &mut ExecutionRequestDraft| {
        draft.persona = "security-engineer".to_owned();
    });
    assert_draft_change!("role", |draft: &mut ExecutionRequestDraft| {
        draft.role = "reviewer".to_owned();
    });
    assert_draft_change!("domain", |draft: &mut ExecutionRequestDraft| {
        draft.domain = "work".to_owned();
    });
    assert_draft_change!("skills", |draft: &mut ExecutionRequestDraft| {
        draft.skills.push("rust-security".to_owned());
    });
    assert_draft_change!("dependencies", |draft: &mut ExecutionRequestDraft| {
        draft.dependencies.push("phase-6b-audit".to_owned());
    });
    assert_draft_change!("expected_evidence", |draft: &mut ExecutionRequestDraft| {
        draft.expected_evidence.push("cargo doc".to_owned());
    });
    assert_draft_change!("constraints", |draft: &mut ExecutionRequestDraft| {
        draft.constraints.push("offline only".to_owned());
    });
    assert_draft_change!("prior_context", |draft: &mut ExecutionRequestDraft| {
        draft.prior_context = "No fallback was required.".to_owned();
    });
    assert_draft_change!("runtime", |draft: &mut ExecutionRequestDraft| {
        draft.runtime = alternate_runtime.clone();
    });
    assert_draft_change!("model", |draft: &mut ExecutionRequestDraft| {
        draft.model = "gpt-5.2-codex".to_owned();
    });
    assert_draft_change!("effort", |draft: &mut ExecutionRequestDraft| {
        draft.effort = Effort::High;
    });
    assert_draft_change!("max_turns", |draft: &mut ExecutionRequestDraft| {
        draft.max_turns = 97;
    });
    assert_draft_change!("worker_dir", |draft: &mut ExecutionRequestDraft| {
        draft.worker_dir = PathBuf::from("/var/lib/nanika/workers/mission-42/phase-8");
    });
    assert_draft_change!("target_dir", |draft: &mut ExecutionRequestDraft| {
        draft.target_dir = Some(PathBuf::from("/srv/repositories/other"));
    });
    assert_draft_change!("resume_from", |draft: &mut ExecutionRequestDraft| {
        draft.resume_from = Some(resume.clone());
    });
    assert_draft_change!("hook_script", |draft: &mut ExecutionRequestDraft| {
        draft.hook_script = Some(PathBuf::from("/opt/nanika/hooks/pre-turn.sh"));
    });
    Ok(())
}

#[test]
fn optional_paths_and_resume_preserve_none_some_distinctions() -> TestResult {
    let mut no_target = baseline_draft()?;
    no_target.target_dir = None;
    let mut no_hook = baseline_draft()?;
    no_hook.hook_script = None;
    let mut no_resume = baseline_draft()?;
    no_resume.resume_from = None;
    let present = fingerprint(baseline_draft()?)?;

    assert_ne!(fingerprint(no_target)?, present);
    assert_ne!(fingerprint(no_hook)?, present);
    assert_ne!(fingerprint(no_resume)?, present);
    Ok(())
}

#[test]
fn resume_runtime_and_session_id_are_bound() -> TestResult {
    let baseline = fingerprint(baseline_draft()?)?;

    let alternate_runtime = RuntimeFamily::parse("codex")?;
    let mut changed_runtime = baseline_draft()?;
    changed_runtime.runtime = alternate_runtime.clone();
    changed_runtime.resume_from = Some(SessionHandle::new(alternate_runtime, "session-secret-7")?);

    let mut changed_session = baseline_draft()?;
    changed_session.resume_from = Some(SessionHandle::new(
        RuntimeFamily::parse("claude")?,
        "session-secret-8",
    )?);

    assert_ne!(baseline, fingerprint(changed_runtime)?);
    assert_ne!(baseline, fingerprint(changed_session)?);
    Ok(())
}

#[test]
fn list_items_are_length_framed() -> TestResult {
    let mut left = mutation_baseline_draft()?;
    left.skills = vec!["ab".to_owned(), "c".to_owned()];
    let mut right = mutation_baseline_draft()?;
    right.skills = vec!["a".to_owned(), "bc".to_owned()];

    assert_ne!(fingerprint(left)?, fingerprint(right)?);
    Ok(())
}

#[test]
fn list_item_order_is_bound() -> TestResult {
    let mut ordered = mutation_baseline_draft()?;
    ordered.skills = vec!["first".to_owned(), "second".to_owned()];
    let mut reversed = mutation_baseline_draft()?;
    reversed.skills = vec!["second".to_owned(), "first".to_owned()];

    assert_ne!(fingerprint(ordered)?, fingerprint(reversed)?);
    Ok(())
}

#[test]
fn adjacent_scalar_boundaries_cannot_collide() -> TestResult {
    let request = baseline_request()?;

    assert_ne!(
        request.fingerprint("worker-a", "requested_runtimeb"),
        request.fingerprint("worker-arequested_runtime", "b")
    );
    Ok(())
}

#[test]
fn non_utf8_worker_target_and_hook_path_bytes_are_bound_exactly() -> TestResult {
    let mut worker_first = mutation_baseline_draft()?;
    worker_first.worker_dir =
        PathBuf::from(OsString::from_vec(b"/var/lib/nanika/worker-\x80".to_vec()));
    let mut worker_second = mutation_baseline_draft()?;
    worker_second.worker_dir =
        PathBuf::from(OsString::from_vec(b"/var/lib/nanika/worker-\x81".to_vec()));

    let mut target_first = mutation_baseline_draft()?;
    target_first.target_dir = Some(PathBuf::from(OsString::from_vec(
        b"/srv/repositories/target-\x80".to_vec(),
    )));
    let mut target_second = mutation_baseline_draft()?;
    target_second.target_dir = Some(PathBuf::from(OsString::from_vec(
        b"/srv/repositories/target-\x81".to_vec(),
    )));

    let mut hook_first = mutation_baseline_draft()?;
    hook_first.hook_script = Some(PathBuf::from(OsString::from_vec(
        b"/opt/nanika/hooks/hook-\x80".to_vec(),
    )));
    let mut hook_second = mutation_baseline_draft()?;
    hook_second.hook_script = Some(PathBuf::from(OsString::from_vec(
        b"/opt/nanika/hooks/hook-\x81".to_vec(),
    )));

    assert_ne!(fingerprint(worker_first)?, fingerprint(worker_second)?);
    assert_ne!(fingerprint(target_first)?, fingerprint(target_second)?);
    assert_ne!(fingerprint(hook_first)?, fingerprint(hook_second)?);
    Ok(())
}

#[test]
fn fingerprint_debug_is_redacted_and_contains_no_request_secrets() -> TestResult {
    let rendered = format!(
        "{:?}",
        baseline_request()?.fingerprint("secret-worker-id", REQUESTED_RUNTIME)
    );

    assert_eq!(rendered, "ExecutionRequestFingerprint([REDACTED])");
    for secret in [
        "secret-worker-id",
        "session-secret-7",
        "mission-42",
        "/srv/repositories/nanika",
    ] {
        assert!(!rendered.contains(secret));
    }
    Ok(())
}
