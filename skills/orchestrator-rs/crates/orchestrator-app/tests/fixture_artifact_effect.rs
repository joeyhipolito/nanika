use orchestrator_app::{
    FixtureAdmissionPolicy, FixtureArtifactEffectService, FixtureEvidenceVerifier,
    FixtureWorkspaceSeed, FreshFixtureAuthority, IsolatedFixtureRoot,
};
use orchestrator_core::{CheckpointProjection, MissionId, PhaseId};
use orchestrator_exec::{
    ArtifactReceipt, AttemptEvidence, AttemptOutcome, Cancellation, Clock, DispatchRequest,
    EffectKind, EffectRequest, EffectServiceErrorKind, EffectStatus, Effort, EventReceipt,
    EventSink, EventSinkError, EventSinkErrorKind, ExecutionContext, ExecutionRequest,
    ExecutionRequestDraft, ExecutorRegistry, MechanicalTermination, PartialWork, PhaseExecutor,
    ProcessBudget, ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError,
    ProcessServiceErrorKind, RuntimeCaps, RuntimeDescriptor, RuntimeFamily, WatchdogDecision,
    WatchdogPolicy, WorkerEventDraft, WorkerIdentity,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "artifact-effect-mission";
const PHASE: &str = "artifact-phase";
const WORKER: &str = "artifact-worker";
const ARTIFACT: &str = "result.md";
const INPUT: &[u8] = b"authority-backed fixture output\n";
const RUNTIME: &str = "fixture-runtime";

static CASE: AtomicU64 = AtomicU64::new(1);

struct FixtureCase {
    parent: PathBuf,
    root: PathBuf,
    authority: FreshFixtureAuthority,
}

impl Drop for FixtureCase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn fixture_case(label: &str) -> TestResult<FixtureCase> {
    let nonce = CASE.fetch_add(1, Ordering::Relaxed);
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = temporary.join(format!(
        "orchestrator-rs-artifact-effect-{}-{nonce}-{label}",
        std::process::id()
    ));
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    Ok(FixtureCase {
        parent,
        root,
        authority,
    })
}

struct ServiceFixture {
    case: FixtureCase,
    effect: FixtureArtifactEffectService,
    verifier: FixtureEvidenceVerifier,
    resource: String,
    expectation: String,
    idempotency_key: String,
    worker_root: PathBuf,
    target_root: PathBuf,
    artifact_path: PathBuf,
}

fn service_fixture(label: &str) -> TestResult<ServiceFixture> {
    let case = fixture_case(label)?;
    let checkpoint = CheckpointProjection {
        workspace_id: MISSION.to_owned(),
        status: "pending".to_owned(),
        ..CheckpointProjection::default()
    };
    let workspace = case.authority.create_workspace(
        MissionId::new(MISSION)?,
        FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint, b"{}".to_vec())?,
    )?;
    let phase = PhaseId::new(PHASE)?;
    let artifact = workspace.bind_fixture_artifact(&phase, 1, ARTIFACT, INPUT)?;
    let worker_root = std::fs::canonicalize(case.root.join("workspaces").join(MISSION))?;
    let target_root = case.parent.join("target");
    private_dir(&target_root)?;
    let target_root = std::fs::canonicalize(target_root)?;
    let artifact_path = worker_root
        .join("artifacts")
        .join(PHASE)
        .join("attempt-1")
        .join(ARTIFACT);
    let (effect, verifier) =
        FixtureArtifactEffectService::new(artifact, INPUT, Some(target_root.clone()))?;
    let resource = effect.resource().to_owned();
    let expectation = effect.expectation().to_owned();
    let idempotency_key = effect.idempotency_key().to_owned();
    Ok(ServiceFixture {
        case,
        effect,
        verifier,
        resource,
        expectation,
        idempotency_key,
        worker_root,
        target_root,
        artifact_path,
    })
}

struct PassiveProcess {
    error: ProcessServiceError,
}

impl PassiveProcess {
    fn new() -> TestResult<Self> {
        Ok(Self {
            error: ProcessServiceError::new(
                ProcessServiceErrorKind::Denied,
                "fixture test does not admit process execution",
            )?,
        })
    }
}

impl Cancellation for PassiveProcess {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl ProcessService for PassiveProcess {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: orchestrator_exec::ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        if preflight.bind(self, request).is_some() {
            Ok(())
        } else {
            Err(self.error.clone())
        }
    }

    fn execute(
        &self,
        _request: &ProcessRequest,
        _budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        Err(self.error.clone())
    }
}

struct FixedClock(Instant);

impl Clock for FixedClock {
    fn now(&self) -> Instant {
        self.0
    }
}

struct FixedWatchdog;

impl WatchdogPolicy for FixedWatchdog {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity + Duration::from_secs(30),
        }
    }

    fn stall_window(&self) -> Duration {
        Duration::from_secs(30)
    }
}

struct RecordingSink {
    sequence: i64,
    error: EventSinkError,
}

impl RecordingSink {
    fn new() -> TestResult<Self> {
        Ok(Self {
            sequence: 1,
            error: EventSinkError::new(
                EventSinkErrorKind::Unavailable,
                "fixture event sequence cannot be represented",
            )?,
        })
    }
}

impl EventSink for RecordingSink {
    fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        let sequence = self.sequence;
        let receipt = EventReceipt::new(
            format!("evt_fixture_{sequence:019}"),
            format!("2000-01-01T00:00:00.{sequence:019}Z"),
            sequence,
        )
        .map_err(|_| self.error.clone())?;
        self.sequence = sequence.checked_add(1).ok_or_else(|| self.error.clone())?;
        Ok(receipt)
    }
}

#[derive(Clone, Copy)]
enum ArtifactBehavior {
    Publish,
    ClaimWithoutPublish,
    WrongPath,
    WrongDigest,
    WrongSize,
    TamperAfterPublish,
}

struct ArtifactExecutor {
    runtime: RuntimeFamily,
    behavior: ArtifactBehavior,
    resource: String,
    idempotency_key: String,
    artifact_path: PathBuf,
}

impl ArtifactExecutor {
    fn incomplete() -> AttemptOutcome {
        AttemptOutcome::incomplete(
            MechanicalTermination::ContractViolation,
            None,
            PartialWork::empty(),
            Duration::ZERO,
        )
    }
}

impl PhaseExecutor for ArtifactExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        let mut digest = "unpublished-digest".to_owned();
        if !matches!(self.behavior, ArtifactBehavior::ClaimWithoutPublish) {
            let request = match EffectRequest::new(
                EffectKind::ArtifactWrite,
                self.resource.clone(),
                self.idempotency_key.clone(),
            )
            .and_then(|request| request.with_input(INPUT.to_vec()))
            {
                Ok(request) => request,
                Err(_) => return Self::incomplete(),
            };
            let receipt = match context.run_effect(&request) {
                Ok(receipt) => receipt,
                Err(_) => return Self::incomplete(),
            };
            digest = receipt
                .expose_output()
                .and_then(|bytes| std::str::from_utf8(bytes).ok())
                .unwrap_or("invalid-effect-digest")
                .to_owned();
        }

        if matches!(self.behavior, ArtifactBehavior::TamperAfterPublish)
            && std::fs::write(&self.artifact_path, b"tampered after publication\n").is_err()
        {
            return Self::incomplete();
        }

        let path = if matches!(self.behavior, ArtifactBehavior::WrongPath) {
            PathBuf::from("artifacts/artifact-phase/attempt-1/other.md")
        } else {
            PathBuf::from(&self.resource)
        };
        if matches!(self.behavior, ArtifactBehavior::WrongDigest) {
            digest = "wrong-digest".to_owned();
        }
        let bytes = if matches!(self.behavior, ArtifactBehavior::WrongSize) {
            INPUT.len() as u64 + 1
        } else {
            INPUT.len() as u64
        };
        let artifact = match ArtifactReceipt::new(path, digest, bytes) {
            Ok(artifact) => artifact,
            Err(_) => return Self::incomplete(),
        };
        let evidence = match AttemptEvidence::new().with_artifact_receipt(artifact) {
            Ok(evidence) => evidence,
            Err(_) => return Self::incomplete(),
        };
        match AttemptOutcome::completed(
            "fixture artifact complete",
            evidence,
            Duration::from_secs(1),
        ) {
            Ok(outcome) => outcome,
            Err(_) => Self::incomplete(),
        }
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        Some(RuntimeDescriptor::new(
            self.runtime.clone(),
            RuntimeCaps {
                tool_use: false,
                session_resume: false,
                streaming: false,
                cost_report: false,
                artifacts: true,
            },
        ))
    }
}

fn execution_request(
    fixture: &ServiceFixture,
    attempt: u32,
    expected: Vec<String>,
    target_root: PathBuf,
) -> TestResult<ExecutionRequest> {
    Ok(ExecutionRequest::new(ExecutionRequestDraft {
        mission: MISSION.to_owned(),
        phase: PHASE.to_owned(),
        attempt,
        revision: 1,
        objective: "publish the exact fixture artifact".to_owned(),
        persona: "fixture".to_owned(),
        role: "implementer".to_owned(),
        domain: "code".to_owned(),
        skills: Vec::new(),
        dependencies: Vec::new(),
        expected_evidence: expected,
        constraints: vec!["fixture-only".to_owned()],
        prior_context: String::new(),
        runtime: RuntimeFamily::parse(RUNTIME)?,
        model: "fixture-model".to_owned(),
        effort: Effort::High,
        max_turns: 1,
        worker_dir: fixture.worker_root.clone(),
        target_dir: Some(target_root),
        resume_from: None,
        hook_script: None,
    })?)
}

fn dispatch(
    fixture: &ServiceFixture,
    behavior: ArtifactBehavior,
    expected: Vec<String>,
    target_root: PathBuf,
) -> TestResult<AttemptOutcome> {
    dispatch_attempt(fixture, 1, behavior, expected, target_root)
}

fn dispatch_attempt(
    fixture: &ServiceFixture,
    attempt: u32,
    behavior: ArtifactBehavior,
    expected: Vec<String>,
    target_root: PathBuf,
) -> TestResult<AttemptOutcome> {
    let runtime = RuntimeFamily::parse(RUNTIME)?;
    let mut registry = ExecutorRegistry::new();
    let _previous = registry.register(
        RUNTIME,
        Arc::new(ArtifactExecutor {
            runtime,
            behavior,
            resource: fixture.resource.clone(),
            idempotency_key: fixture.idempotency_key.clone(),
            artifact_path: fixture.artifact_path.clone(),
        }),
    )?;
    let resolved = registry.resolve(RUNTIME)?;
    let process = PassiveProcess::new()?;
    let now = Instant::now();
    let clock = FixedClock(now);
    let watchdog = FixedWatchdog;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &process,
        &clock,
        &watchdog,
        &fixture.effect,
        &mut sink,
        WorkerIdentity::new(MISSION, PHASE, WORKER)?,
        now + Duration::from_secs(60),
    )
    .with_evidence_verifier(&fixture.verifier);
    Ok(resolved.execute(
        &execution_request(fixture, attempt, expected, target_root)?,
        &mut context,
    )?)
}

#[test]
fn exact_effect_is_idempotent_and_completion_uses_fresh_attestation() -> TestResult {
    let fixture = service_fixture("success")?;
    let first = dispatch(
        &fixture,
        ArtifactBehavior::Publish,
        vec![fixture.expectation.clone()],
        fixture.target_root.clone(),
    )?;
    assert!(first.is_completed());
    assert_eq!(first.evidence().artifact_receipts().len(), 1);
    assert!(first.evidence().artifact_receipts()[0].is_authority_verified());
    assert_eq!(first.evidence().effect_receipts().len(), 1);
    assert_eq!(
        first.evidence().effect_receipts()[0].status(),
        EffectStatus::Applied
    );
    assert_eq!(std::fs::read(&fixture.artifact_path)?, INPUT);

    let replay = dispatch(
        &fixture,
        ArtifactBehavior::Publish,
        vec![fixture.expectation.clone()],
        fixture.target_root.clone(),
    )?;
    assert!(replay.is_completed());
    assert_eq!(
        replay.evidence().effect_receipts()[0].status(),
        EffectStatus::AlreadyApplied
    );
    assert!(replay.evidence().artifact_receipts()[0].is_authority_verified());
    Ok(())
}

#[test]
fn effect_denies_every_unenrolled_kind_binding_and_input() -> TestResult {
    let fixture = service_fixture("deny")?;
    let process = PassiveProcess::new()?;
    let now = Instant::now();
    let clock = FixedClock(now);
    let watchdog = FixedWatchdog;
    let mut sink = RecordingSink::new()?;
    let mut context = ExecutionContext::new(
        &process,
        &clock,
        &watchdog,
        &fixture.effect,
        &mut sink,
        WorkerIdentity::new(MISSION, PHASE, WORKER)?,
        now + Duration::from_secs(60),
    );
    for request in [
        EffectRequest::new(
            EffectKind::FileWrite,
            fixture.resource.clone(),
            fixture.idempotency_key.clone(),
        )?
        .with_input(INPUT.to_vec())?,
        EffectRequest::new(
            EffectKind::ArtifactWrite,
            "artifacts/other.md",
            fixture.idempotency_key.clone(),
        )?
        .with_input(INPUT.to_vec())?,
        EffectRequest::new(
            EffectKind::ArtifactWrite,
            fixture.resource.clone(),
            "other-key",
        )?
        .with_input(INPUT.to_vec())?,
    ] {
        let error = match context.run_effect(&request) {
            Err(error) => error,
            Ok(_) => return Err("unenrolled effect request was applied".into()),
        };
        assert_eq!(error.kind(), EffectServiceErrorKind::Denied);
    }
    let wrong_input = EffectRequest::new(
        EffectKind::ArtifactWrite,
        fixture.resource.clone(),
        fixture.idempotency_key.clone(),
    )?
    .with_input(b"wrong bytes".to_vec())?;
    let error = match context.run_effect(&wrong_input) {
        Err(error) => error,
        Ok(_) => return Err("wrong artifact bytes were applied".into()),
    };
    assert_eq!(error.kind(), EffectServiceErrorKind::InvalidRequest);
    assert!(!fixture.artifact_path.exists());
    Ok(())
}

#[test]
fn verifier_fails_closed_before_publish_and_for_unknown_expectation_or_root() -> TestResult {
    let missing = service_fixture("missing")?;
    let outcome = dispatch(
        &missing,
        ArtifactBehavior::ClaimWithoutPublish,
        vec![missing.expectation.clone()],
        missing.target_root.clone(),
    )?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );
    assert!(!outcome.evidence().artifact_receipts()[0].is_authority_verified());

    let expectation = service_fixture("unknown-expectation")?;
    let outcome = dispatch(
        &expectation,
        ArtifactBehavior::Publish,
        vec!["artifact:unenrolled.md".to_owned()],
        expectation.target_root.clone(),
    )?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::ContractViolation)
    );

    let wrong_root = service_fixture("wrong-root")?;
    let other_root = wrong_root.case.parent.join("other-target");
    private_dir(&other_root)?;
    let other_root = std::fs::canonicalize(other_root)?;
    let outcome = dispatch(
        &wrong_root,
        ArtifactBehavior::Publish,
        vec![wrong_root.expectation.clone()],
        other_root,
    )?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::SupervisorFailure)
    );

    let wrong_attempt = service_fixture("wrong-attempt")?;
    let outcome = dispatch_attempt(
        &wrong_attempt,
        2,
        ArtifactBehavior::Publish,
        vec![wrong_attempt.expectation.clone()],
        wrong_attempt.target_root.clone(),
    )?;
    assert_eq!(
        outcome.termination(),
        Some(MechanicalTermination::SupervisorFailure)
    );
    Ok(())
}

#[test]
fn verifier_rejects_path_digest_size_and_same_uid_tamper() -> TestResult {
    for (label, behavior) in [
        ("wrong-path", ArtifactBehavior::WrongPath),
        ("wrong-digest", ArtifactBehavior::WrongDigest),
        ("wrong-size", ArtifactBehavior::WrongSize),
        ("tamper", ArtifactBehavior::TamperAfterPublish),
    ] {
        let fixture = service_fixture(label)?;
        let outcome = dispatch(
            &fixture,
            behavior,
            vec![fixture.expectation.clone()],
            fixture.target_root.clone(),
        )?;
        assert_eq!(
            outcome.termination(),
            Some(MechanicalTermination::ContractViolation),
            "case {label} unexpectedly completed"
        );
        assert!(!outcome.evidence().artifact_receipts()[0].is_authority_verified());
    }
    Ok(())
}
