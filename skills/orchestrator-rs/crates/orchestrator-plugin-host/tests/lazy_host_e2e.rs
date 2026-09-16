#![cfg(unix)]

use super::{
    CapabilityGrant, Command, DeclaredResourceRequest, EnrolledPlugin, FrameBytes, HostError,
    Limits, ManagerExit, PluginHost, PluginIdentity, PluginRequest, ProductionCatalog,
    ReceiptBinding, ReceiptOutcome, RetainedRuntime, Runtime, RuntimeState, RuntimeTelemetry,
    State, enroll_fixture, lock, receipt_digest, receipt_digest_with_directions,
};
use orchestrator_process::{
    CancellationToken, ProcessReport, ProcessSpec, ProcessSupervisor, ProcessTermination,
};
use serde_json::json;
use std::{
    ffi::OsString,
    fs,
    net::TcpListener,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard, atomic::Ordering, mpsc},
    thread,
    time::{Duration, Instant},
};

static PROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

fn serialize_process_test() -> MutexGuard<'static, ()> {
    PROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fake_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(std::env::current_exe()?)
}

struct Case {
    root: PathBuf,
    image: Vec<u8>,
}

impl Case {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!("plugin-host-{label}-{}", std::process::id()));
        if root.exists() {
            fs::remove_dir_all(&root)?;
        }
        fs::create_dir(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        fs::create_dir(root.join("bin"))?;
        fs::create_dir(root.join("work"))?;
        let image = fs::read(fake_path()?)?;
        fs::write(root.join("bin/plugin"), &image)?;
        fs::set_permissions(root.join("bin/plugin"), fs::Permissions::from_mode(0o700))?;
        Ok(Self { root, image })
    }

    fn enrollment(&self, limits: Limits) -> Result<EnrolledPlugin, HostError> {
        self.enrollment_with_behavior(limits, None)
    }

    fn enrollment_with_behavior(
        &self,
        limits: Limits,
        behavior: Option<&str>,
    ) -> Result<EnrolledPlugin, HostError> {
        enroll_fixture(
            &self.root,
            Path::new("bin/plugin"),
            Path::new("work"),
            &self.image,
            limits,
            behavior,
        )
    }
}

impl Drop for Case {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request(id: u64, operation: &str) -> PluginRequest {
    PluginRequest {
        id,
        operation: operation.into(),
        payload: json!({"value": id}),
        declared_resources: vec![DeclaredResourceRequest {
            name: "fixture_cpu_ms".into(),
            units: 7,
        }],
    }
}

fn wait_inactive(host: &PluginHost, maximum: Duration) -> bool {
    let deadline = Instant::now() + maximum;
    while Instant::now() < deadline {
        if !host.diagnostics().active {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    false
}

fn wait_owned_process(host: &PluginHost, maximum: Duration) -> bool {
    let deadline = Instant::now() + maximum;
    while Instant::now() < deadline {
        if host.diagnostics().supervised_runs != 0 {
            return true;
        }
        thread::sleep(Duration::from_millis(2));
    }
    false
}

fn assert_quiescent(host: &PluginHost) {
    let diagnostics = host.diagnostics();
    assert!(!diagnostics.active, "host remained active: {diagnostics:?}");
    assert_eq!(diagnostics.manager_threads, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.supervisor_threads, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.listeners, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.supervised_runs, 0, "{diagnostics:?}");
    assert!(!diagnostics.unresolved_ownership, "{diagnostics:?}");
    assert!(!diagnostics.admission_closed, "{diagnostics:?}");
    assert_eq!(diagnostics.in_flight, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.processing, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.queued_requests, 0, "{diagnostics:?}");
}

fn assert_cleanup_evidence(host: &PluginHost, minimum_proofs: u64) {
    let evidence = lock(&host.lifecycle_evidence);
    let Some(address) = evidence.last_listener_address else {
        panic!("activated host did not record its exact TCP listener");
    };
    let rebound = TcpListener::bind(address)
        .unwrap_or_else(|error| panic!("host listener {address} remained bound: {error}"));
    assert_eq!(rebound.local_addr().ok(), Some(address));
    assert!(
        evidence.cleanup_proofs >= minimum_proofs,
        "expected at least {minimum_proofs} verified cleanups, observed {}",
        evidence.cleanup_proofs
    );
    assert!(
        evidence.last_child_pid.is_some(),
        "missing child reap evidence"
    );
    assert!(
        evidence.last_child_pgid.is_some(),
        "missing process-group cleanup evidence"
    );
}

fn assert_activated_quiescent(host: &PluginHost) {
    assert_quiescent(host);
    assert_cleanup_evidence(host, 1);
}

fn receipt_golden_vector_binds_exact_portable_transcript() -> Result<(), Box<dyn std::error::Error>>
{
    let identity = PluginIdentity([0x11; 32]);
    let grants = vec![CapabilityGrant {
        name: "events.read".into(),
    }];
    let request = FrameBytes {
        exact: vec![0, 0, 0, 3, 0x10, 0x20, 0x30],
    };
    let response = FrameBytes {
        exact: vec![0, 0, 0, 2, 0x40, 0x50],
    };
    let binding = ReceiptBinding {
        receipt_schema_version: 1,
        protocol_version: 1,
        plugin_identity: &identity,
        activation_id: 0x0102_0304_0506_0708,
        request_id: 0x1112_1314_1516_1718,
        granted_capabilities: &grants,
        declared_resource_request_count: 1,
        declared_resource_request_units: 7,
        outcome: ReceiptOutcome::ResponseAccepted,
        measured_exchange_duration_ns: 123_456_789,
        request_frame: &request,
        response_frame: &response,
    };
    assert_eq!(
        receipt_digest(binding)?,
        "c6ccfae06860f6fbe4d8a9209bf1a6cedff233916a5ebfaeab4c2d329e752cc6"
    );
    Ok(())
}

fn unspawned_cancellation_is_a_complete_cleanup_without_a_child_reap()
-> Result<(), Box<dyn std::error::Error>> {
    let mut report = ProcessReport {
        termination: ProcessTermination::Cancelled,
        stdout: Vec::new(),
        stderr: Vec::new(),
        truncated: false,
        stdout_discarded_bytes: 0,
        stderr_discarded_bytes: 0,
        elapsed: Duration::ZERO,
        spawned: false,
        pid: None,
        pgid: None,
        kernel_identity: None,
        cancellation_observed: true,
        deadline_observed: false,
        stall_observed: false,
        term_sent: false,
        kill_sent: false,
        escalated_to_kill: false,
        direct_child_reaped: false,
        group_absent: true,
        cleanup_complete: true,
        infrastructure_failures: Vec::new(),
    };
    assert!(super::cleanup_is_proven(&report));
    report.group_absent = false;
    assert!(!super::cleanup_is_proven(&report));
    report.group_absent = true;
    report.spawned = true;
    assert!(!super::cleanup_is_proven(&report));
    report.direct_child_reaped = true;
    assert!(super::cleanup_is_proven(&report));
    Ok(())
}

fn receipt_rejects_tamper_direction_order_header_identity_grant_and_outcome_equivalence()
-> Result<(), Box<dyn std::error::Error>> {
    let identity = PluginIdentity([0x11; 32]);
    let changed_identity = PluginIdentity([0x12; 32]);
    let grants = vec![CapabilityGrant {
        name: "events.read".into(),
    }];
    let changed_grants = vec![CapabilityGrant {
        name: "events.write".into(),
    }];
    let request = FrameBytes {
        exact: vec![0, 0, 0, 3, 0x10, 0x20, 0x30],
    };
    let tampered_body = FrameBytes {
        exact: vec![0, 0, 0, 3, 0x10, 0x20, 0x31],
    };
    let tampered_header = FrameBytes {
        exact: vec![0, 0, 0, 4, 0x10, 0x20, 0x30],
    };
    let response = FrameBytes {
        exact: vec![0, 0, 0, 2, 0x40, 0x50],
    };
    let binding =
        |plugin_identity, granted_capabilities, outcome, request_frame, response_frame| {
            ReceiptBinding {
                receipt_schema_version: 1,
                protocol_version: 1,
                plugin_identity,
                activation_id: 1,
                request_id: 2,
                granted_capabilities,
                declared_resource_request_count: 1,
                declared_resource_request_units: 7,
                outcome,
                measured_exchange_duration_ns: 3,
                request_frame,
                response_frame,
            }
        };
    let base = binding(
        &identity,
        &grants,
        ReceiptOutcome::ResponseAccepted,
        &request,
        &response,
    );
    let expected = receipt_digest(base)?;
    let mut changed_schema = base;
    changed_schema.receipt_schema_version = 2;
    let mut changed_protocol = base;
    changed_protocol.protocol_version = 2;
    let mut changed_activation = base;
    changed_activation.activation_id = 2;
    let mut changed_request_id = base;
    changed_request_id.request_id = 3;
    let mut changed_declaration = base;
    changed_declaration.declared_resource_request_units = 8;
    let mut changed_duration = base;
    changed_duration.measured_exchange_duration_ns = 4;
    for changed in [
        receipt_digest(changed_schema),
        receipt_digest(changed_protocol),
        receipt_digest(changed_activation),
        receipt_digest(changed_request_id),
        receipt_digest(changed_declaration),
        receipt_digest(changed_duration),
        receipt_digest(binding(
            &identity,
            &grants,
            ReceiptOutcome::ResponseAccepted,
            &tampered_body,
            &response,
        )),
        receipt_digest_with_directions(
            binding(
                &identity,
                &grants,
                ReceiptOutcome::ResponseAccepted,
                &request,
                &response,
            ),
            2,
            1,
        ),
        receipt_digest(binding(
            &identity,
            &grants,
            ReceiptOutcome::ResponseAccepted,
            &response,
            &request,
        )),
        receipt_digest(binding(
            &identity,
            &grants,
            ReceiptOutcome::ResponseAccepted,
            &tampered_header,
            &response,
        )),
        receipt_digest(binding(
            &changed_identity,
            &grants,
            ReceiptOutcome::ResponseAccepted,
            &request,
            &response,
        )),
        receipt_digest(binding(
            &identity,
            &changed_grants,
            ReceiptOutcome::ResponseAccepted,
            &request,
            &response,
        )),
        receipt_digest(binding(
            &identity,
            &grants,
            ReceiptOutcome::ProtocolRejected,
            &request,
            &response,
        )),
    ] {
        assert_ne!(changed?, expected);
    }
    Ok(())
}

fn assert_stays_quiescent(host: &PluginHost) {
    assert_quiescent(host);
    let wakeups = host.diagnostics().periodic_wakeups;
    thread::sleep(Duration::from_millis(30));
    assert_quiescent(host);
    assert_eq!(
        host.diagnostics().periodic_wakeups,
        wakeups,
        "periodic wakeups continued after the host became quiescent"
    );
}

fn assert_stays_activated_quiescent(host: &PluginHost) {
    assert_stays_quiescent(host);
    assert_cleanup_evidence(host, 1);
}

fn assert_stays_closed_quiescent(host: &PluginHost) {
    let diagnostics = host.diagnostics();
    assert!(!diagnostics.active, "host remained active: {diagnostics:?}");
    assert_eq!(diagnostics.manager_threads, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.supervisor_threads, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.listeners, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.supervised_runs, 0, "{diagnostics:?}");
    assert!(diagnostics.unresolved_ownership, "{diagnostics:?}");
    assert!(diagnostics.admission_closed, "{diagnostics:?}");
    assert_eq!(diagnostics.in_flight, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.processing, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.queued_requests, 0, "{diagnostics:?}");
    let wakeups = diagnostics.periodic_wakeups;
    thread::sleep(Duration::from_millis(30));
    let after = host.diagnostics();
    assert!(!after.active, "host reactivated: {after:?}");
    assert_eq!(after.manager_threads, 0, "{after:?}");
    assert_eq!(after.supervisor_threads, 0, "{after:?}");
    assert_eq!(after.listeners, 0, "{after:?}");
    assert_eq!(after.supervised_runs, 0, "{after:?}");
    assert!(after.unresolved_ownership, "{after:?}");
    assert!(after.admission_closed, "{after:?}");
    assert_eq!(after.periodic_wakeups, wakeups);
    assert_cleanup_evidence(host, 1);
}

fn idle_manager_quiesces_its_exact_reaper_before_host_observation()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("idle-reaper")?;
    let limits = Limits {
        idle_timeout: Duration::from_millis(20),
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment(limits)?);
    host.request(request(1, "echo"))?;

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let state = lock(&host.state);
        let quiesced_before_retirement = match &state.lifecycle {
            RuntimeState::Running { runtime, .. } => {
                runtime.join.is_finished()
                    && !runtime.telemetry.manager_thread.load(Ordering::Acquire)
                    && !runtime.telemetry.supervisor_thread.load(Ordering::Acquire)
                    && !runtime.telemetry.listener.load(Ordering::Acquire)
                    && runtime.telemetry.supervised_runs.load(Ordering::Acquire) == 0
            }
            RuntimeState::Stopped { .. } => false,
        };
        drop(state);
        if quiesced_before_retirement {
            break;
        }
        if Instant::now() >= deadline {
            return Err("idle manager did not synchronously quiesce its reaper".into());
        }
        thread::sleep(Duration::from_millis(2));
    }

    assert!(wait_inactive(&host, Duration::from_secs(1)));
    assert_stays_activated_quiescent(&host);
    Ok(())
}

fn manager_unwind_cancels_owned_work_and_permanently_closes_admission()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("manager-unwind")?;
    let limits = Limits {
        request_deadline: Duration::from_secs(2),
        child_deadline: Duration::from_secs(5),
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment_with_behavior(limits, Some("manager-panic"))?);
    assert!(matches!(
        host.request(request(1, "echo")),
        Err(HostError::Manager)
    ));

    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.manager_threads, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.supervisor_threads, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.listeners, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.supervised_runs, 0, "{diagnostics:?}");
    assert!(diagnostics.unresolved_ownership, "{diagnostics:?}");
    assert!(diagnostics.admission_closed, "{diagnostics:?}");
    assert!(matches!(
        host.request(request(2, "echo")),
        Err(HostError::Supervisor)
    ));
    assert_eq!(host.diagnostics().activations, 1);
    Ok(())
}

fn cleanup_failure_wins_over_simultaneous_protocol_setup_failure()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("dual-fault")?;
    let host =
        PluginHost::new(case.enrollment_with_behavior(Limits::default(), Some("dual-fault"))?);

    assert!(matches!(
        host.request(request(1, "echo")),
        Err(HostError::Supervisor)
    ));
    assert!(wait_inactive(&host, Duration::from_secs(1)));
    assert_eq!(lock(&host.lifecycle_evidence).cleanup_proofs, 1);
    assert_stays_closed_quiescent(&host);
    assert!(matches!(
        host.request(request(2, "echo")),
        Err(HostError::Supervisor)
    ));
    assert_eq!(host.diagnostics().activations, 1);
    Ok(())
}

fn refused_reaper_shutdown_retains_handle_and_retries_when_unique()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("retained-refusal")?;
    let host = PluginHost::new(case.enrollment(Limits::default())?);
    let supervisor = Arc::new(ProcessSupervisor::new(1)?);
    let shared = Arc::clone(&supervisor);
    let telemetry = Arc::new(RuntimeTelemetry::default());
    telemetry.supervisor_thread.store(true, Ordering::Release);
    lock(&host.state).retained_failure = Some(RetainedRuntime {
        supervisor,
        telemetry: Arc::clone(&telemetry),
        unresolved_ownership: false,
    });

    assert!(matches!(
        host.request(request(1, "echo")),
        Err(HostError::Supervisor)
    ));
    assert_eq!(host.diagnostics().supervisor_threads, 1);
    drop(shared);

    let response = host.request(request(2, "echo"))?;
    assert_eq!(response.response.id, 2);
    host.shutdown();
    assert_stays_activated_quiescent(&host);
    Ok(())
}

fn replacement_waits_for_join_and_stale_generation_cannot_retire_it()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("generation-race")?;
    let host = Arc::new(PluginHost::new(case.enrollment(Limits::default())?));
    let (sender, receiver) = mpsc::sync_channel::<Command>(1);
    let (joining_tx, joining_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    *lock(&host.state) = State {
        lifecycle: RuntimeState::Running {
            generation: 1,
            runtime: Runtime {
                sender,
                cancellation: CancellationToken::new(),
                telemetry: Arc::default(),
                join: thread::spawn(move || {
                    assert!(matches!(receiver.recv(), Ok(Command::Shutdown)));
                    assert!(joining_tx.send(()).is_ok());
                    assert!(release_rx.recv().is_ok());
                    ManagerExit {
                        wakeups: 0,
                        result: Ok(()),
                        retained_supervisor: None,
                        unresolved_ownership: false,
                    }
                }),
            },
        },
        retained_failure: None,
        activations: 1,
        wakeups: 0,
        periodic_wakeups: 0,
        unresolved_ownership: false,
    };

    let shutdown_host = Arc::clone(&host);
    let shutdown = thread::spawn(move || shutdown_host.shutdown());
    joining_rx.recv_timeout(Duration::from_secs(1))?;

    let replacement_host = Arc::clone(&host);
    let replacement = thread::spawn(move || replacement_host.request(request(1, "echo")));
    thread::sleep(Duration::from_millis(50));
    assert!(!replacement.is_finished());

    release_tx.send(())?;
    shutdown.join().map_err(|_| "shutdown thread panicked")?;
    let response = replacement
        .join()
        .map_err(|_| "replacement request panicked")??;
    assert_eq!(response.receipt.activation_id, 2);
    assert_eq!(host.diagnostics().activations, 2);

    host.shutdown_generation(1);
    assert!(host.diagnostics().active);
    let follow_up = host.request(request(2, "echo"))?;
    assert_eq!(follow_up.receipt.activation_id, 2);
    assert_eq!(host.diagnostics().activations, 2);
    host.shutdown();
    Ok(())
}

fn expired_request_waiting_for_retirement_does_not_activate_a_replacement()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("expired-retirement")?;
    let limits = Limits {
        request_deadline: Duration::from_millis(80),
        ..Limits::default()
    };
    let host = Arc::new(PluginHost::new(case.enrollment(limits)?));
    let (sender, receiver) = mpsc::sync_channel::<Command>(1);
    let (joining_tx, joining_rx) = mpsc::sync_channel(1);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    *lock(&host.state) = State {
        lifecycle: RuntimeState::Running {
            generation: 1,
            runtime: Runtime {
                sender,
                cancellation: CancellationToken::new(),
                telemetry: Arc::default(),
                join: thread::spawn(move || {
                    assert!(matches!(receiver.recv(), Ok(Command::Shutdown)));
                    assert!(joining_tx.send(()).is_ok());
                    assert!(release_rx.recv().is_ok());
                    ManagerExit {
                        wakeups: 0,
                        result: Ok(()),
                        retained_supervisor: None,
                        unresolved_ownership: false,
                    }
                }),
            },
        },
        retained_failure: None,
        activations: 1,
        wakeups: 0,
        periodic_wakeups: 0,
        unresolved_ownership: false,
    };

    let shutdown_host = Arc::clone(&host);
    let shutdown = thread::spawn(move || shutdown_host.shutdown());
    joining_rx.recv_timeout(Duration::from_secs(1))?;
    let request_host = Arc::clone(&host);
    let waiting = thread::spawn(move || request_host.request(request(1, "echo")));
    let admission_deadline = Instant::now() + Duration::from_secs(1);
    while host.in_flight.load(Ordering::Acquire) != 1 && Instant::now() < admission_deadline {
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(host.in_flight.load(Ordering::Acquire), 1);
    thread::sleep(Duration::from_millis(100));
    release_tx.send(())?;
    shutdown.join().map_err(|_| "shutdown thread panicked")?;
    assert!(matches!(
        waiting.join().map_err(|_| "request thread panicked")?,
        Err(HostError::Deadline)
    ));
    // Use a fresh cleanup budget; the request's absolute deadline is already
    // expired and must never be reused as a test wait marker.
    assert!(wait_inactive(&host, Duration::from_secs(1)));
    assert_eq!(host.diagnostics().activations, 1);
    assert_quiescent(&host);
    Ok(())
}

fn disconnected_idle_receiver_is_retried_before_request_admission()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("idle-disconnect")?;
    let host = PluginHost::new(case.enrollment(Limits::default())?);
    let (sender, receiver) = mpsc::sync_channel::<Command>(1);
    drop(receiver);
    let (release_tx, release_rx) = mpsc::sync_channel(1);
    *lock(&host.state) = State {
        lifecycle: RuntimeState::Running {
            generation: 1,
            runtime: Runtime {
                sender,
                cancellation: CancellationToken::new(),
                telemetry: Arc::default(),
                join: thread::spawn(move || {
                    assert!(release_rx.recv().is_ok());
                    ManagerExit {
                        wakeups: 0,
                        result: Ok(()),
                        retained_supervisor: None,
                        unresolved_ownership: false,
                    }
                }),
            },
        },
        retained_failure: None,
        activations: 1,
        wakeups: 0,
        periodic_wakeups: 0,
        unresolved_ownership: false,
    };
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let _ = release_tx.send(());
    });
    let response = host.request(request(1, "echo"))?;
    releaser.join().map_err(|_| "releaser panicked")?;
    assert_eq!(response.receipt.activation_id, 2);
    assert_eq!(host.diagnostics().activations, 2);
    host.shutdown();
    assert_stays_activated_quiescent(&host);
    Ok(())
}

fn host_diagnostics_are_isolated_from_the_process_wide_registry()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("isolated-supervisor")?;
    let host = PluginHost::new(case.enrollment(Limits::default())?);
    let unrelated = Arc::new(ProcessSupervisor::process_wide()?);
    let unrelated_cancellation = CancellationToken::new();
    let authority = super::fixture_authority(&host.config)?;
    let spec = ProcessSpec::new(
        vec![
            OsString::from("unrelated-fixture"),
            OsString::from("127.0.0.1:9"),
            OsString::from("linger-before-connect"),
        ],
        Duration::from_secs(10),
    )?
    .with_stdin(vec![0_u8; 32])
    .with_cancellation(unrelated_cancellation.clone())
    .with_term_grace(Duration::from_millis(50))
    .with_cleanup_grace(Duration::from_secs(1));
    let runner_supervisor = Arc::clone(&unrelated);
    let runner = thread::spawn(move || runner_supervisor.run_fixture_authorized(&spec, &authority));
    let ownership_deadline = Instant::now() + Duration::from_secs(2);
    while !unrelated.has_owned_processes() && Instant::now() < ownership_deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert!(unrelated.has_owned_processes());

    let diagnostics = host.diagnostics();
    assert_eq!(diagnostics.supervised_runs, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.manager_threads, 0, "{diagnostics:?}");
    assert_eq!(diagnostics.listeners, 0, "{diagnostics:?}");
    unrelated_cancellation.cancel();
    let report = runner.join().map_err(|_| "unrelated runner panicked")??;
    assert!(report.cleanup_complete);
    assert!(!unrelated.has_owned_processes());
    assert_quiescent(&host);
    Ok(())
}

fn twenty_queue_to_idle_boundaries_reactivate_without_leaks_or_stale_retirement()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("idle-boundary-20")?;
    let limits = Limits {
        queue_depth: 1,
        max_concurrency: 2,
        idle_timeout: Duration::from_millis(20),
        ..Limits::default()
    };
    let host = Arc::new(PluginHost::new(case.enrollment(limits)?));
    for repetition in 0..20 {
        let first_id = repetition * 2 + 1;
        let first_host = Arc::clone(&host);
        let first = thread::spawn(move || first_host.request(request(first_id, "brief")));
        let processing_deadline = Instant::now() + Duration::from_secs(1);
        while host.diagnostics().processing != 1 && Instant::now() < processing_deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(host.diagnostics().processing, 1);

        let second_id = first_id + 1;
        let second_host = Arc::clone(&host);
        let second = thread::spawn(move || second_host.request(request(second_id, "echo")));
        let queue_deadline = Instant::now() + Duration::from_secs(1);
        while host.diagnostics().queued_requests != 1 && Instant::now() < queue_deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(host.diagnostics().queued_requests, 1);
        assert_eq!(
            first
                .join()
                .map_err(|_| "brief request panicked")??
                .response
                .id,
            first_id
        );
        assert_eq!(
            second
                .join()
                .map_err(|_| "queued request panicked")??
                .response
                .id,
            second_id
        );
        assert!(wait_inactive(&host, Duration::from_secs(1)));
        assert_quiescent(&host);
        assert_cleanup_evidence(&host, repetition + 1);
    }
    assert_eq!(host.diagnostics().activations, 20);
    Ok(())
}

fn unused_is_free_then_one_child_serves_requests_and_expires()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("lazy")?;
    let limits = Limits {
        idle_timeout: Duration::from_millis(80),
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment(limits)?);
    assert_eq!(host.diagnostics().activations, 0);
    assert_eq!(host.diagnostics().manager_wakeups, 0);
    assert!(wait_inactive(&host, Duration::from_secs(1)));
    assert_stays_quiescent(&host);

    let first = host.request(request(1, "echo"))?;
    assert_eq!(first.response.result, json!({"value": 1}));
    assert_eq!(first.receipt.declared_resource_request_count, 1);
    assert_eq!(first.receipt.declared_resource_request_units, 7);
    assert_eq!(first.receipt.receipt_sha256_hex.len(), 64);
    assert_eq!(first.receipt.receipt_schema_version, 1);
    assert_eq!(first.receipt.protocol_version, 1);
    assert_eq!(first.receipt.granted_capabilities[0].name, "fixture.invoke");
    let second = host.request(request(2, "echo"))?;
    assert_eq!(second.receipt.activation_id, first.receipt.activation_id);
    let mut same_length_a = request(3, "echo");
    same_length_a.payload = json!("aa");
    let mut same_length_b = request(3, "echo");
    same_length_b.payload = json!("bb");
    let receipt_a = host.request(same_length_a)?.receipt;
    let receipt_b = host.request(same_length_b)?.receipt;
    assert_ne!(receipt_a.receipt_sha256_hex, receipt_b.receipt_sha256_hex);
    assert_eq!(host.diagnostics().activations, 1);

    assert!(wait_inactive(&host, Duration::from_secs(2)));
    assert_stays_activated_quiescent(&host);
    Ok(())
}

fn receipt_identity_is_stable_and_debug_output_redacts_transcript_secrets()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("receipt-secret")?;
    let limits = Limits {
        idle_timeout: Duration::from_millis(20),
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment(limits)?);
    let secret = "receipt-must-not-contain-this-secret";
    let mut sensitive = request(1, "echo");
    sensitive.operation = secret.into();
    sensitive.payload = json!({"token": secret});
    sensitive.declared_resources[0].name = secret.into();
    assert!(!format!("{:?}", sensitive.declared_resources[0]).contains(secret));
    assert!(!format!("{sensitive:?}").contains(secret));
    let first = host.request(sensitive)?;
    assert!(!format!("{first:?}").contains(secret));
    assert!(!format!("{:?}", first.receipt).contains(secret));
    let identity = first.receipt.plugin_identity.clone();
    assert!(first.receipt.request_frame_bytes as usize >= 4);
    assert!(first.receipt.response_frame_bytes as usize >= 4);

    assert!(wait_inactive(&host, Duration::from_secs(1)));
    let second = host.request(request(2, "echo"))?;
    assert_eq!(second.receipt.plugin_identity, identity);
    assert_ne!(second.receipt.activation_id, first.receipt.activation_id);
    host.shutdown();
    assert_stays_activated_quiescent(&host);
    Ok(())
}

fn cancellation_crash_and_bad_frames_are_reaped_and_recoverable()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("recovery")?;
    let limits = Limits {
        request_deadline: Duration::from_millis(250),
        child_deadline: Duration::from_secs(10),
        idle_timeout: Duration::from_secs(2),
        ..Limits::default()
    };
    let host = Arc::new(PluginHost::new(case.enrollment(limits)?));

    assert!(matches!(
        host.request(request(1, "crash")),
        Err(HostError::Crashed | HostError::Io { .. })
    ));
    assert!(wait_inactive(&host, Duration::from_secs(2)));
    assert_stays_activated_quiescent(&host);
    assert!(host.request(request(2, "echo")).is_ok());
    host.shutdown();
    assert!(!host.diagnostics().active);

    let worker_host = Arc::clone(&host);
    let worker = thread::spawn(move || worker_host.request(request(3, "slow")));
    thread::sleep(Duration::from_millis(30));
    host.shutdown();
    assert!(
        worker
            .join()
            .map_err(|_| "request worker panicked")?
            .is_err()
    );
    assert!(!host.diagnostics().active);
    assert_stays_activated_quiescent(&host);
    assert!(fs::read_dir(&case.root)?.all(|entry| {
        !entry
            .map(|item| item.file_name().to_string_lossy().ends_with(".sock"))
            .unwrap_or(false)
    }));
    Ok(())
}

fn bounds_versions_traversal_and_fixture_escape_fail_before_spawn()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("rejections")?;
    assert!(matches!(
        ProductionCatalog::reviewed_b5().enroll("fake", Limits::default()),
        Err(HostError::NotEnrolled)
    ));

    assert!(matches!(
        enroll_fixture(
            &case.root,
            Path::new("bin/../bin/plugin"),
            Path::new("work"),
            &case.image,
            Limits::default(),
            None,
        ),
        Err(HostError::InvalidEnrollment)
    ));

    let outside = case.root.with_extension("outside");
    fs::create_dir(&outside)?;
    fs::write(outside.join("plugin"), &case.image)?;
    fs::set_permissions(outside.join("plugin"), fs::Permissions::from_mode(0o700))?;
    symlink(outside.join("plugin"), case.root.join("bin/escape"))?;
    assert!(matches!(
        enroll_fixture(
            &case.root,
            Path::new("bin/escape"),
            Path::new("work"),
            &case.image,
            Limits::default(),
            None,
        ),
        Err(HostError::InvalidEnrollment)
    ));
    fs::remove_dir_all(outside)?;

    let bounded_limits = Limits {
        max_declared_resource_requests: 1,
        max_declared_resource_units: 10,
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment(bounded_limits)?);
    let mut oversized = request(4, "echo");
    oversized.payload = json!("x".repeat(300_000));
    assert!(matches!(
        host.request(oversized),
        Err(HostError::BoundExceeded)
    ));
    let mut bad_resource = request(5, "echo");
    bad_resource.declared_resources[0].name = "../secret".into();
    assert!(matches!(
        host.request(bad_resource),
        Err(HostError::BoundExceeded)
    ));
    let mut too_many_resources = request(51, "echo");
    too_many_resources
        .declared_resources
        .push(DeclaredResourceRequest {
            name: "fixture_memory".into(),
            units: 1,
        });
    assert!(matches!(
        host.request(too_many_resources),
        Err(HostError::BoundExceeded)
    ));
    let mut too_many_units = request(52, "echo");
    too_many_units.declared_resources[0].units = 11;
    assert!(matches!(
        host.request(too_many_units),
        Err(HostError::BoundExceeded)
    ));
    assert_eq!(host.diagnostics().activations, 0);

    let malformed = host.request(request(6, "malformed"));
    assert!(
        matches!(
            malformed,
            Err(HostError::MalformedFrame | HostError::Crashed)
        ),
        "unexpected malformed-frame result: {malformed:?}"
    );
    assert!(wait_inactive(&host, Duration::from_secs(2)));
    assert!(matches!(
        host.request(request(7, "oversize")),
        Err(HostError::BoundExceeded | HostError::Crashed)
    ));
    host.shutdown();
    assert!(!host.diagnostics().active);
    Ok(())
}

fn complete_envelope_bound_is_checked_before_activation() -> Result<(), Box<dyn std::error::Error>>
{
    let _serial = serialize_process_test();
    let case = Case::new("envelope-bound")?;
    let limits = Limits {
        max_frame_bytes: 160,
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment(limits)?);
    let candidate = (1..160)
        .map(|length| {
            let mut candidate = request(1, "echo");
            candidate.payload = json!("x".repeat(length));
            candidate
        })
        .find(|candidate| {
            super::validate_request(candidate, &host.config.limits).is_ok()
                && matches!(
                    super::prepare_request(candidate.clone(), &host.config.limits),
                    Err(HostError::BoundExceeded)
                )
        })
        .ok_or("test could not find a request/envelope boundary case")?;
    assert!(matches!(
        host.request(candidate),
        Err(HostError::BoundExceeded)
    ));
    assert_eq!(host.diagnostics().activations, 0);
    assert_quiescent(&host);
    Ok(())
}

fn executable_namespace_replacement_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("attestation")?;
    let host = PluginHost::new(case.enrollment(Limits::default())?);
    fs::write(case.root.join("bin/plugin"), b"#!/bin/sh\nexit 0\n")?;
    fs::set_permissions(
        case.root.join("bin/plugin"),
        fs::Permissions::from_mode(0o700),
    )?;
    assert!(host.request(request(1, "echo")).is_err());
    assert!(!host.diagnostics().active || wait_inactive(&host, Duration::from_secs(1)));
    assert_quiescent(&host);
    Ok(())
}

fn retained_root_executable_and_cwd_cannot_be_redirected() -> Result<(), Box<dyn std::error::Error>>
{
    let _serial = serialize_process_test();
    let case = Case::new("root-retained")?;
    let host = PluginHost::new(case.enrollment(Limits::default())?);
    let retained_root = case.root.with_extension("retained");
    fs::rename(&case.root, &retained_root)?;
    fs::create_dir(&case.root)?;
    fs::set_permissions(&case.root, fs::Permissions::from_mode(0o700))?;
    fs::create_dir(case.root.join("bin"))?;
    fs::create_dir(case.root.join("work"))?;
    fs::write(
        case.root.join("bin/plugin"),
        b"#!/bin/sh\necho redirected > work/redirected\n",
    )?;
    fs::set_permissions(
        case.root.join("bin/plugin"),
        fs::Permissions::from_mode(0o700),
    )?;

    let response = host.request(request(1, "echo"));
    if let Ok(response) = response {
        assert_eq!(response.response.result, json!({"value": 1}));
    }
    assert!(!case.root.join("work/redirected").exists());
    host.shutdown();

    fs::remove_dir_all(&case.root)?;
    fs::rename(retained_root, &case.root)?;
    Ok(())
}

fn unauthenticated_racing_peer_cannot_forge_a_response() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("wrong-peer")?;
    let limits = Limits {
        request_deadline: Duration::from_secs(10),
        ..Limits::default()
    };
    let enrollment = case.enrollment_with_behavior(limits, Some("race-wrong-peer"))?;
    let host = PluginHost::new(enrollment);
    let started = Instant::now();
    let response = host.request(request(1, "echo"))?;
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(response.response.result, json!({"value": 1}));
    assert_ne!(response.response.result, json!("forged"));
    assert_eq!(host.diagnostics().activations, 1);
    host.shutdown();
    assert_activated_quiescent(&host);
    Ok(())
}

fn malformed_and_wrong_version_handshakes_fail_safely() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    for behavior in ["malformed-hello", "wrong-version"] {
        let case = Case::new(behavior)?;
        let limits = Limits {
            request_deadline: Duration::from_secs(3),
            ..Limits::default()
        };
        let host = PluginHost::new(case.enrollment_with_behavior(limits, Some(behavior))?);
        let result = host.request(request(1, "echo"));
        if behavior == "wrong-version" {
            assert!(matches!(result, Err(HostError::UnsupportedVersion)));
        } else {
            assert!(result.is_err());
        }
        assert!(wait_inactive(&host, Duration::from_secs(1)));
        assert_activated_quiescent(&host);
    }
    Ok(())
}

fn concurrency_and_deadline_budgets_cancel_the_owned_child()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("budgets")?;
    let limits = Limits {
        max_concurrency: 1,
        request_deadline: Duration::from_secs(2),
        child_deadline: Duration::from_secs(10),
        idle_timeout: Duration::from_secs(2),
        ..Limits::default()
    };
    let host = Arc::new(PluginHost::new(case.enrollment(limits)?));
    let request_started = Instant::now();
    let worker_host = Arc::clone(&host);
    let worker = thread::spawn(move || worker_host.request(request(1, "slow")));
    assert!(
        wait_owned_process(&host, Duration::from_millis(1500)),
        "this host's retained supervisor never observed its owned child"
    );
    assert!(matches!(
        host.request(request(2, "echo")),
        Err(HostError::ConcurrencyLimit)
    ));
    let worker_result = worker.join().map_err(|_| "deadline worker panicked")?;
    assert!(
        matches!(worker_result, Err(HostError::Deadline)),
        "unexpected request-deadline result after {:?}: {worker_result:?}",
        request_started.elapsed()
    );
    assert!(wait_inactive(&host, Duration::from_secs(1)));
    assert_activated_quiescent(&host);
    Ok(())
}

fn queue_and_child_output_are_bounded() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("qo")?;
    let limits = Limits {
        queue_depth: 1,
        max_concurrency: 4,
        max_output_bytes: 1024,
        request_deadline: Duration::from_secs(15),
        child_deadline: Duration::from_secs(10),
        idle_timeout: Duration::from_secs(2),
        ..Limits::default()
    };
    let host = Arc::new(PluginHost::new(case.enrollment(limits)?));
    let first_host = Arc::clone(&host);
    let first = thread::spawn(move || first_host.request(request(1, "slow")));
    let processing_deadline = Instant::now() + Duration::from_secs(5);
    while host.diagnostics().processing != 1 && Instant::now() < processing_deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(host.diagnostics().processing, 1);
    let second_host = Arc::clone(&host);
    let second = thread::spawn(move || second_host.request(request(2, "echo")));
    let queue_deadline = Instant::now() + Duration::from_secs(5);
    while host.diagnostics().queued_requests != 1 && Instant::now() < queue_deadline {
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(host.diagnostics().queued_requests, 1);
    assert!(matches!(
        host.request(request(3, "echo")),
        Err(HostError::QueueFull)
    ));
    host.shutdown();
    assert!(
        first
            .join()
            .map_err(|_| "first queue worker panicked")?
            .is_err()
    );
    assert!(
        second
            .join()
            .map_err(|_| "second queue worker panicked")?
            .is_err()
    );

    let noisy = host.request(request(4, "noisy"));
    assert!(
        matches!(noisy, Err(HostError::OutputLimit)),
        "unexpected output-limit result: {noisy:?}"
    );
    assert!(wait_inactive(&host, Duration::from_secs(2)));
    assert_activated_quiescent(&host);
    Ok(())
}

fn child_deadline_is_independent_and_reaps_before_request_deadline()
-> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("cd")?;
    let limits = Limits {
        request_deadline: Duration::from_secs(5),
        child_deadline: Duration::from_secs(2),
        idle_timeout: Duration::from_secs(2),
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment(limits)?);
    let started = Instant::now();
    let deadline_result = host.request(request(1, "slow"));
    assert!(
        matches!(deadline_result, Err(HostError::ChildDeadline)),
        "unexpected child-deadline result: {deadline_result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(4));
    assert!(wait_inactive(&host, Duration::from_secs(1)));
    assert_activated_quiescent(&host);
    Ok(())
}

fn absolute_request_deadline_rejects_drip_framing() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serialize_process_test();
    let case = Case::new("absolute-io-deadline")?;
    let limits = Limits {
        request_deadline: Duration::from_millis(500),
        child_deadline: Duration::from_secs(5),
        idle_timeout: Duration::from_secs(2),
        ..Limits::default()
    };
    let host = PluginHost::new(case.enrollment(limits)?);
    let started = Instant::now();
    let result = host.request(request(1, "drip"));
    assert!(
        matches!(result, Err(HostError::Deadline)),
        "unexpected drip-frame result: {result:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "drip framing extended the absolute request deadline"
    );
    assert!(wait_inactive(&host, Duration::from_secs(1)));
    assert_activated_quiescent(&host);
    Ok(())
}

#[allow(dead_code)]
fn _path_is_local(_path: &Path) {}

pub(crate) fn run_all() -> Result<(), Box<dyn std::error::Error>> {
    macro_rules! run {
        ($case:ident) => {{
            println!("test {} ...", stringify!($case));
            $case()?;
            println!("test {} ... ok", stringify!($case));
        }};
    }

    run!(receipt_golden_vector_binds_exact_portable_transcript);
    run!(unspawned_cancellation_is_a_complete_cleanup_without_a_child_reap);
    run!(receipt_rejects_tamper_direction_order_header_identity_grant_and_outcome_equivalence);
    run!(idle_manager_quiesces_its_exact_reaper_before_host_observation);
    run!(manager_unwind_cancels_owned_work_and_permanently_closes_admission);
    run!(cleanup_failure_wins_over_simultaneous_protocol_setup_failure);
    run!(refused_reaper_shutdown_retains_handle_and_retries_when_unique);
    run!(replacement_waits_for_join_and_stale_generation_cannot_retire_it);
    run!(expired_request_waiting_for_retirement_does_not_activate_a_replacement);
    run!(disconnected_idle_receiver_is_retried_before_request_admission);
    run!(host_diagnostics_are_isolated_from_the_process_wide_registry);
    run!(twenty_queue_to_idle_boundaries_reactivate_without_leaks_or_stale_retirement);
    run!(unused_is_free_then_one_child_serves_requests_and_expires);
    run!(receipt_identity_is_stable_and_debug_output_redacts_transcript_secrets);
    run!(cancellation_crash_and_bad_frames_are_reaped_and_recoverable);
    run!(bounds_versions_traversal_and_fixture_escape_fail_before_spawn);
    run!(complete_envelope_bound_is_checked_before_activation);
    run!(executable_namespace_replacement_fails_closed);
    run!(retained_root_executable_and_cwd_cannot_be_redirected);
    run!(unauthenticated_racing_peer_cannot_forge_a_response);
    run!(malformed_and_wrong_version_handshakes_fail_safely);
    run!(concurrency_and_deadline_budgets_cancel_the_owned_child);
    run!(queue_and_child_output_are_bounded);
    run!(child_deadline_is_independent_and_reaps_before_request_deadline);
    run!(absolute_request_deadline_rejects_drip_framing);
    Ok(())
}
