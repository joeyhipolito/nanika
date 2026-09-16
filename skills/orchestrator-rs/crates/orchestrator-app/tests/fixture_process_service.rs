use orchestrator_app::{
    CancellationToken, FixtureAdmissionPolicy, FixtureProcessAuthority, FixtureProcessService,
    FixtureWorkspaceSeed, FreshFixtureAuthority, IsolatedFixtureRoot, SupervisorLimits,
};
use orchestrator_core::{CheckpointProjection, MissionId};
use orchestrator_exec::{
    Clock, EffectBudget, EffectReceipt, EffectRequest, EffectService, EffectServiceError,
    EffectServiceErrorKind, EventReceipt, EventSink, EventSinkError, EventSinkErrorKind,
    ExecutionContext, ProcessExitStatus, ProcessPurpose, ProcessReceipt, ProcessRequest,
    ProcessServiceErrorKind, ProcessTerminationReceipt, WatchdogDecision, WatchdogPolicy,
    WorkerEventDraft, WorkerIdentity,
};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MISSION: &str = "process-service-mission";
const EXECUTABLE: &str = "native-helper";

static PROCESS_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct ProcessFixture {
    guard: Option<MutexGuard<'static, ()>>,
    parent: PathBuf,
    worker_root: PathBuf,
    service: Option<Arc<FixtureProcessService>>,
}

impl ProcessFixture {
    fn service(&self) -> TestResult<Arc<FixtureProcessService>> {
        self.service
            .as_ref()
            .cloned()
            .ok_or_else(|| "fixture process service is unavailable".into())
    }
}

impl Drop for ProcessFixture {
    fn drop(&mut self) {
        self.service.take();
        let _ = std::fs::remove_dir_all(&self.parent);
        self.guard.take();
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

fn fixture(label: &str) -> TestResult<ProcessFixture> {
    let guard = match PROCESS_LOCK.get_or_init(|| Mutex::new(())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let parent = temporary.join(format!(
        "orchestrator-rs-process-service-{}-{label}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&parent);
    private_dir(&parent)?;
    let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
    let root = isolated.path().to_path_buf();
    let helper = std::fs::read(env!("CARGO_BIN_EXE_orchestrator-owned-process-fixture"))?;
    let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
    let policy = FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &temporary)
        .with_expected_fixture_helper(&helper);
    let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
    let checkpoint = CheckpointProjection {
        workspace_id: MISSION.to_owned(),
        status: "pending".to_owned(),
        ..CheckpointProjection::default()
    };
    let workspace = authority.create_workspace(
        MissionId::new(MISSION)?,
        FixtureWorkspaceSeed::new(b"fixture\n".to_vec(), &checkpoint, b"{}".to_vec())?,
    )?;
    let executable = authority.install_fixture_executable(EXECUTABLE, &helper)?;
    let limits =
        SupervisorLimits::for_tests(Duration::from_millis(80), Duration::from_millis(500))?;
    let process_authority = FixtureProcessAuthority::new(executable, &workspace, limits)?;
    let worker_root = std::fs::canonicalize(root.join("workspaces").join(MISSION))?;
    let service = Arc::new(FixtureProcessService::new(
        process_authority,
        CancellationToken::new(),
    )?);
    Ok(ProcessFixture {
        guard: Some(guard),
        parent,
        worker_root,
        service: Some(service),
    })
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct CancellationAtBudgetClock {
    start: Instant,
    deadline: Instant,
    calls: AtomicUsize,
    service: Arc<FixtureProcessService>,
}

impl Clock for CancellationAtBudgetClock {
    fn now(&self) -> Instant {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if call >= 2 {
            let _ = self.service.cancel();
            self.deadline
        } else {
            self.start
        }
    }
}

struct ProcessWatchdog;

impl WatchdogPolicy for ProcessWatchdog {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity + Duration::from_secs(30),
        }
    }

    fn stall_window(&self) -> Duration {
        Duration::from_secs(30)
    }
}

struct DeniedEffect(EffectServiceError);

impl DeniedEffect {
    fn new() -> TestResult<Self> {
        Ok(Self(EffectServiceError::new(
            EffectServiceErrorKind::Denied,
            "process fixture does not admit effects",
        )?))
    }
}

impl EffectService for DeniedEffect {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        Err(self.0.clone())
    }
}

struct UnusedSink(EventSinkError);

impl UnusedSink {
    fn new() -> TestResult<Self> {
        Ok(Self(EventSinkError::new(
            EventSinkErrorKind::Rejected,
            "process fixture does not emit worker events",
        )?))
    }
}

impl EventSink for UnusedSink {
    fn emit(&mut self, _event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        Err(self.0.clone())
    }
}

fn run_process(
    service: &FixtureProcessService,
    request: &ProcessRequest,
    timeout: Duration,
) -> TestResult<Result<ProcessReceipt, orchestrator_exec::ProcessServiceError>> {
    let clock = SystemClock;
    let watchdog = ProcessWatchdog;
    let effect = DeniedEffect::new()?;
    let mut sink = UnusedSink::new()?;
    let mut context = ExecutionContext::new(
        service,
        &clock,
        &watchdog,
        &effect,
        &mut sink,
        WorkerIdentity::new(MISSION, "process-phase", "process-worker")?,
        Instant::now() + timeout,
    );
    Ok(context.run_process(request))
}

fn exact_request(root: &Path, mode: &str) -> TestResult<ProcessRequest> {
    Ok(
        ProcessRequest::new(ProcessPurpose::ProviderWorker, EXECUTABLE, root)?
            .with_argument(mode)?,
    )
}

#[test]
fn exact_request_maps_argv_output_exit_and_cleanup_receipt() -> TestResult {
    let fixture = fixture("exact")?;
    let service = fixture.service()?;
    let request = exact_request(&fixture.worker_root, "inspect")?
        .with_argument("literal spaces")?
        .with_argument("$(not-a-shell)")?;
    let receipt = run_process(&service, &request, Duration::from_secs(3))??;
    assert!(receipt.is_success());
    assert!(receipt.ownership_released());
    let line = receipt
        .expose_stdout()
        .split(|byte| *byte == b'\n')
        .next()
        .ok_or("helper omitted inspection output")?;
    let value: serde_json::Value = serde_json::from_slice(line)?;
    assert_eq!(
        value["arguments"],
        serde_json::json!(["inspect", "literal spaces", "$(not-a-shell)"])
    );
    assert_eq!(
        value["cwd"].as_str(),
        Some(fixture.worker_root.to_string_lossy().as_ref())
    );
    assert!(!service.has_unresolved_processes());
    Ok(())
}

#[test]
fn purpose_executable_root_and_environment_are_exact_deny_boundaries() -> TestResult {
    let fixture = fixture("deny")?;
    let service = fixture.service()?;
    let other_root = fixture.parent.join("other-root");
    private_dir(&other_root)?;
    let requests = [
        (
            ProcessRequest::new(ProcessPurpose::Tool, EXECUTABLE, &fixture.worker_root)?,
            ProcessServiceErrorKind::Denied,
        ),
        (
            ProcessRequest::new(
                ProcessPurpose::ProviderWorker,
                "other-helper",
                &fixture.worker_root,
            )?,
            ProcessServiceErrorKind::NotEnrolled,
        ),
        (
            ProcessRequest::new(ProcessPurpose::ProviderWorker, EXECUTABLE, &other_root)?,
            ProcessServiceErrorKind::OutsideRoot,
        ),
        (
            ProcessRequest::new(
                ProcessPurpose::ProviderWorker,
                EXECUTABLE,
                &fixture.worker_root,
            )?
            .with_environment("POISON", "1")?,
            ProcessServiceErrorKind::Denied,
        ),
    ];
    for (request, expected) in requests {
        let error = match run_process(&service, &request, Duration::from_secs(1))? {
            Err(error) => error,
            Ok(_) => return Err("unenrolled process request was admitted".into()),
        };
        assert_eq!(error.kind(), expected);
        assert!(!service.has_unresolved_processes());
    }
    Ok(())
}

#[test]
fn pre_cancelled_service_returns_cancelled_without_owning_a_process() -> TestResult {
    let fixture = fixture("pre-cancelled")?;
    let service = fixture.service()?;
    assert!(service.cancel());

    let receipt = run_process(
        &service,
        &exact_request(&fixture.worker_root, "zero")?,
        Duration::from_secs(3),
    )??;

    assert_eq!(receipt.termination(), ProcessTerminationReceipt::Cancelled);
    assert!(receipt.ownership_released());
    assert!(!service.has_unresolved_processes());
    Ok(())
}

#[test]
fn cancellation_wins_when_budget_translation_reaches_the_deadline() -> TestResult {
    let fixture = fixture("cancel-deadline-translation")?;
    let service = fixture.service()?;
    let start = Instant::now();
    let deadline = start + Duration::from_secs(1);
    let clock = CancellationAtBudgetClock {
        start,
        deadline,
        calls: AtomicUsize::new(0),
        service: Arc::clone(&service),
    };
    let watchdog = ProcessWatchdog;
    let effect = DeniedEffect::new()?;
    let mut sink = UnusedSink::new()?;
    let mut context = ExecutionContext::new(
        service.as_ref(),
        &clock,
        &watchdog,
        &effect,
        &mut sink,
        WorkerIdentity::new(MISSION, "process-phase", "process-worker")?,
        deadline,
    );

    let receipt = context.run_process(&exact_request(&fixture.worker_root, "zero")?)?;
    assert_eq!(receipt.termination(), ProcessTerminationReceipt::Cancelled);
    assert!(receipt.ownership_released());
    assert!(clock.calls.load(Ordering::SeqCst) >= 3);
    assert!(!service.has_unresolved_processes());
    Ok(())
}

#[test]
fn termination_and_output_bounds_map_without_losing_cleanup() -> TestResult {
    let fixture = fixture("mapping")?;
    let service = fixture.service()?;
    let nonzero = run_process(
        &service,
        &exact_request(&fixture.worker_root, "nonzero")?,
        Duration::from_secs(3),
    )??;
    assert_eq!(
        nonzero.termination(),
        ProcessTerminationReceipt::Exited(ProcessExitStatus::code(7)?)
    );
    assert!(nonzero.ownership_released());

    let output_request =
        exact_request(&fixture.worker_root, "output")?.with_max_output_bytes(4 * 1024)?;
    let output = run_process(&service, &output_request, Duration::from_secs(3))??;
    assert_eq!(
        output.termination(),
        ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?)
    );
    assert!(output.stdout_discarded() > 0 || output.stderr_discarded() > 0);
    assert!(!output.is_success());
    assert!(output.ownership_released());

    let timed_out = run_process(
        &service,
        &exact_request(&fixture.worker_root, "hang-tree")?,
        Duration::from_millis(150),
    )??;
    assert_eq!(
        timed_out.termination(),
        ProcessTerminationReceipt::DeadlineExceeded
    );
    assert!(timed_out.ownership_released());
    assert!(!service.has_unresolved_processes());
    Ok(())
}

#[test]
fn clean_concurrent_receipt_is_released_while_another_group_remains_owned() -> TestResult {
    let fixture = fixture("concurrent")?;
    let service = fixture.service()?;
    let long_service = Arc::clone(&service);
    let long_request = exact_request(&fixture.worker_root, "hang-tree")?;
    let long = std::thread::spawn(move || {
        run_process(&long_service, &long_request, Duration::from_secs(20))
            .map_err(|error| error.to_string())
    });

    let wait_deadline = Instant::now() + Duration::from_secs(3);
    while !service.has_unresolved_processes() {
        if Instant::now() >= wait_deadline {
            service.cancel();
            let _ = long.join();
            return Err("long fixture process was never registered as owned".into());
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let clean = run_process(
        &service,
        &exact_request(&fixture.worker_root, "zero")?,
        Duration::from_secs(3),
    )??;
    assert!(clean.is_success());
    assert!(clean.ownership_released());
    assert!(
        service.has_unresolved_processes(),
        "the separate long-running group should remain globally owned"
    );

    assert!(service.cancel());
    let long = long
        .join()
        .map_err(|_| std::io::Error::other("long process thread panicked"))?
        .map_err(std::io::Error::other)??;
    assert_eq!(long.termination(), ProcessTerminationReceipt::Cancelled);
    assert!(long.ownership_released());
    assert!(!service.has_unresolved_processes());
    Ok(())
}
