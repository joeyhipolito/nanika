//! Gate 2 — `cargo test -p orchestrator-app --locked --offline
//! --test metrics_audit_security_e2e -- --nocapture`.
//!
//! Proves four things as one integrated story:
//!
//! 1. **Metrics have exactly one owner.** A second `MetricsOwner` against the
//!    same home fails on the kernel lease rather than interleaving writes.
//! 2. **A crashed mission's skill-less phase still lands terminal metrics.**
//!    This is the direct regression test for TRK-493. A helper subprocess
//!    commits a phase transition with its metric publication and is then
//!    `SIGKILL`ed, so no `Drop`, no flush, and no end-of-mission batch ever
//!    runs. On restart the publication is still `pending`, drains into
//!    `metrics.db`, and the phase row is present with `parsed_skills` empty.
//! 3. **The audit chain is immutable and its faults are distinguishable.**
//!    The append-only triggers refuse a direct `UPDATE` and `DELETE`; and with
//!    those triggers removed, each of the four adversary classes produces its
//!    own distinct fault.
//! 4. **Secret-bearing payloads and errors fail closed and redact.** Each
//!    denylist family is refused at admission, never reaches
//!    `knowledge_publication`, and its error prints no secret bytes under
//!    either `Display` or `Debug`.
//!
//! Everything runs under a disposable root beneath the process temp directory.
//! No live home, credential, provider, or external database is touched, and
//! nothing here opens a socket. The secret corpus is synthetic text shaped like
//! each pattern — never a real credential.

#![allow(clippy::doc_markdown, reason = "prose names Go symbols and file paths")]

use std::{
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

use orchestrator_app::{
    AppKnowledgeGateway, ChainFault, ChainRange, ChainVerification, CompatibilityProjection,
    DeliveryVerdict, ExactProcessGroupAbsence, IsolatedFixtureRoot, JournalIntent,
    KernelProcessIdentity, MetricsOwner, MetricsOwnerCapability, OwnerLeaseError,
    PHASE_METRIC_TYPE, PhaseMetricIntent, PhaseStatus, PublicationState,
    RecordedProcessIdentityStatus, TERMINAL_METRIC_TYPE, TerminalMetricIntent, TokenCounts,
    audit_chain, inspect_recorded_process_identity, open_fixture_runtime_store,
    phase_metric_payload, publication_drain, terminal_metric_payload,
};
use orchestrator_core::MissionId;
use orchestrator_knowledge::{
    CanonicalJson, Delegation, ExpectedHead, FieldMask, Governance, KnowledgeCapability,
    KnowledgeError, LifecycleState, Namespace, Operation, PrimitiveKind, Provenance,
    PublicationEvidence, RecordEnvelope, RecordId, RegistryGeneration, RevisionId, SchemaVersion,
    Sensitivity, TypeManifest, TypeName, TypeRegistry, Validity,
};
use std::collections::BTreeSet;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

const NAMESPACE: &str = "metrics";
const MISSION: &str = "mission-b3-metrics";
const SKILLLESS_PHASE: &str = "phase-no-skills";
const SKILLED_PHASE: &str = "phase-with-skills";
const AT: &str = "2026-08-26T12:00:00Z";

/// Env var selecting a helper mode when this test binary re-execs itself.
const HELPER_MODE: &str = "NANIKA_B3_METRICS_GATE_HELPER";
/// Fixture root handed to a helper.
const HELPER_ROOT: &str = "NANIKA_B3_METRICS_GATE_ROOT";
/// Publication payload handed to the crash helper.
const HELPER_PAYLOAD: &str = "NANIKA_B3_METRICS_GATE_PAYLOAD";

// ===========================================================================
// Helper subprocess
// ===========================================================================

/// Re-exec entry point.
///
/// The gate needs real subprocesses — a process group to prove reaped, and a
/// process to `SIGKILL` mid-flight — but
/// `crates/orchestrator-app/tests/fixtures/**` is not in the B3 lease, so no
/// new helper binary may be committed. Re-execing this test binary with a mode
/// selector gives a genuinely separate, genuinely compiled first-party helper
/// without adding a file.
///
/// In an ordinary run the mode variable is unset and this returns immediately.
#[test]
fn helper_entrypoint() {
    let Ok(mode) = std::env::var(HELPER_MODE) else {
        return;
    };
    let code = match mode.as_str() {
        // Hold the process group open until stdin closes, so the parent can
        // observe a live kernel identity before reaping it.
        "hold" => {
            println!("ready");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            0
        }
        // Commit a phase transition and its metric publication, then die
        // without unwinding. `SIGKILL` cannot be caught, so no destructor, no
        // WAL flush, and no end-of-mission batch runs. Reaching the exit below
        // at all means the commit failed — the parent sees a non-signal exit
        // and reports it rather than mistaking it for a crash.
        "crash-after-commit" => match crash_after_commit() {
            Ok(()) => 3,
            Err(_) => 4,
        },
        _ => 2,
    };
    std::process::exit(code);
}

fn crash_after_commit() -> TestResult {
    let root = PathBuf::from(std::env::var(HELPER_ROOT)?);
    let payload = std::env::var(HELPER_PAYLOAD)?;
    let fixture = IsolatedFixtureRoot::identify(&root)?;
    let gateway = gateway()?;
    let mission = MissionId::new(MISSION)?;
    let (evidence, canonical) = admit_phase_metric(&gateway, SKILLLESS_PHASE, &payload)?;
    let intent =
        gateway.publication_intent(&mission, Some(SKILLLESS_PHASE), 1, &evidence, &canonical)?;
    let mut store = open_fixture_runtime_store(&fixture)?;
    let transition = transition("phase.completed.skilless", &mission)?;
    store.append(&gateway.attach(transition, intent)?)?;
    // Deliberately no `store.close()`: the point is that nothing orderly runs.
    let _ = std::io::stdout().flush();
    kill_self();
    Err("SIGKILL did not take effect".into())
}

/// Sends `SIGKILL` to this process.
///
/// `rustix` rather than a raw `libc` call because the workspace forbids
/// `unsafe`, and `SIGKILL` rather than `abort` because only `SIGKILL` is
/// uncatchable — a catchable signal would leave open the objection that some
/// handler still had a chance to flush.
fn kill_self() {
    let raw = i32::try_from(std::process::id()).unwrap_or(0);
    if let Some(pid) = rustix::process::Pid::from_raw(raw) {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
    }
}

fn helper_command(mode: &str) -> TestResult<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "helper_entrypoint", "--nocapture"])
        .env(HELPER_MODE, mode);
    scrub_provider_environment(&mut command);
    Ok(command)
}

// ===========================================================================
// Disposable fixture harness
// ===========================================================================

struct Fixture {
    parent: PathBuf,
    root: IsolatedFixtureRoot,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let parent = std::fs::canonicalize(std::env::temp_dir())?.join(format!(
            "orchestrator-rs-metrics-audit-{}-{number}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700))?;
        }
        let root = IsolatedFixtureRoot::create_fresh(&parent)?;
        Ok(Self { parent, root })
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    fn runtime_database(&self) -> PathBuf {
        self.path().join("runtime.db")
    }

    fn owner(&self) -> Result<MetricsOwner, Box<dyn std::error::Error>> {
        Ok(MetricsOwner::assume(self.capability()?)?)
    }

    fn capability(&self) -> Result<MetricsOwnerCapability, Box<dyn std::error::Error>> {
        Ok(MetricsOwnerCapability::in_fixture_boundary(
            orchestrator_app::fixture_production_boundary(&self.root)?,
        )?)
    }
}

// ===========================================================================
// Knowledge plane wiring
// ===========================================================================

fn manifests() -> Result<Vec<TypeManifest>, KnowledgeError> {
    [PHASE_METRIC_TYPE, TERMINAL_METRIC_TYPE]
        .into_iter()
        .map(|name| {
            Ok(TypeManifest {
                namespace: Namespace::new(NAMESPACE)?,
                type_name: TypeName::new(name)?,
                kind: PrimitiveKind::Record,
                schema_versions: [SchemaVersion::new(1)?].into_iter().collect(),
                required_fields: BTreeSet::new(),
                sensitivity_ceiling: Sensitivity::Internal,
                allowed_operations: [
                    Operation::writing(PrimitiveKind::Record),
                    Operation::Get,
                    Operation::Query,
                ]
                .into_iter()
                .collect(),
            })
        })
        .collect()
}

fn capability(generation: RegistryGeneration) -> Result<KnowledgeCapability, KnowledgeError> {
    Ok(KnowledgeCapability {
        namespace: Namespace::new(NAMESPACE)?,
        types: [PHASE_METRIC_TYPE, TERMINAL_METRIC_TYPE]
            .into_iter()
            .map(TypeName::new)
            .collect::<Result<BTreeSet<_>, _>>()?,
        operations: [Operation::Put, Operation::Get, Operation::Query]
            .into_iter()
            .collect(),
        field_mask: FieldMask::All,
        sensitivity_ceiling: Sensitivity::Internal,
        validity: Validity::at(generation),
        delegation: Delegation::NotDelegable,
    })
}

fn gateway() -> Result<AppKnowledgeGateway, KnowledgeError> {
    let registry = TypeRegistry::activate(manifests()?)?;
    let capability = capability(registry.generation())?;
    Ok(AppKnowledgeGateway::new(registry, capability))
}

fn governance(
    type_name: &str,
    generation: RegistryGeneration,
) -> Result<Governance, KnowledgeError> {
    Ok(Governance {
        namespace: Namespace::new(NAMESPACE)?,
        type_name: TypeName::new(type_name)?,
        schema_version: SchemaVersion::new(1)?,
        provenance: Provenance::new("metrics-audit-security-gate", Some(MISSION))?,
        sensitivity: Sensitivity::Internal,
        lifecycle: LifecycleState::Active,
        registry_generation: generation,
    })
}

/// Admits one record-shaped metrics payload.
///
/// Returns the evidence *and* the canonical bytes the registry actually
/// scanned. Publishing those exact bytes is not optional: `PublicationIntent`
/// re-derives the digest over what it stores and refuses a body-carrying
/// primitive whose payload differs from the admitted envelope, so a caller
/// cannot enqueue content the secret scan never saw.
fn admit_metric(
    gateway: &AppKnowledgeGateway,
    type_name: &str,
    identity: &str,
    payload: &serde_json::Value,
) -> Result<(PublicationEvidence, String), KnowledgeError> {
    let body = CanonicalJson::encode(payload)?;
    let canonical = body.as_str().to_owned();
    let envelope = RecordEnvelope::new(
        RecordId::new(identity)?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        governance(type_name, gateway.registry().generation())?,
        body,
    );
    Ok((gateway.admit(envelope)?, canonical))
}

fn admit_phase_metric(
    gateway: &AppKnowledgeGateway,
    phase: &str,
    payload: &str,
) -> TestResult<(PublicationEvidence, String)> {
    let value: serde_json::Value = serde_json::from_str(payload)?;
    Ok(admit_metric(
        gateway,
        PHASE_METRIC_TYPE,
        &format!("{NAMESPACE}/{PHASE_METRIC_TYPE}/{phase}"),
        &value,
    )?)
}

fn admit_terminal_metric(
    gateway: &AppKnowledgeGateway,
    payload: &serde_json::Value,
) -> Result<(PublicationEvidence, String), KnowledgeError> {
    admit_metric(
        gateway,
        TERMINAL_METRIC_TYPE,
        &format!("{NAMESPACE}/{TERMINAL_METRIC_TYPE}/{MISSION}"),
        payload,
    )
}

fn transition(id: &str, mission: &MissionId) -> TestResult<JournalIntent> {
    Ok(JournalIntent::new(
        id,
        Some(mission.clone()),
        "phase.completed",
        serde_json::json!({"step": 1}),
        AT,
    )?
    .with_required_projection(CompatibilityProjection::Checkpoint))
}

// ===========================================================================
// Process-group witness
// ===========================================================================

/// Spawns a helper in its own process group, observes its live kernel identity,
/// reaps it, and returns the proof its group is gone.
///
/// This is the only way to obtain an [`ExactProcessGroupAbsence`], which is
/// exactly the point: a phase metric cannot be built without one.
fn reaped_witness() -> TestResult<ExactProcessGroupAbsence> {
    let (child, identity) = live_group()?;
    reap(child)?;
    match inspect_recorded_process_identity(
        identity.pid(),
        identity.process_group_id(),
        identity.process_start_identity(),
    )? {
        RecordedProcessIdentityStatus::ExactGroupAbsent(absence) => Ok(absence),
        other => Err(format!("expected a reaped group, observed {other:?}").into()),
    }
}

fn live_group() -> TestResult<(Child, KernelProcessIdentity)> {
    use std::os::unix::process::CommandExt;
    let mut command = helper_command("hold")?;
    command.stdin(Stdio::piped()).stdout(Stdio::piped());
    command.process_group(0);
    let mut child = command.spawn()?;
    let pid = child.id();
    // Block until the helper announces itself, so the identity observation
    // below cannot race a process that has not finished starting.
    let stdout = child.stdout.take().ok_or("helper stdout")?;
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            return Err("helper exited before announcing readiness".into());
        }
        // libtest with `--test-threads=1` prints `test <name> ... ` and no
        // newline *before* running the test, so the child's readiness token lands
        // at the end of that progress line rather than on a line of its own.
        // Comparing the whole line therefore never matches under the verification
        // lease, which sets RUST_TEST_THREADS=1, and the handshake deadlocks: the
        // parent waits for a line it will never see while the child waits for the
        // stdin EOF the parent sends only after it. The last whitespace-separated
        // token is the token in both of libtest's printing modes.
        if line.split_whitespace().next_back() == Some("ready") {
            break;
        }
    }
    let identity = KernelProcessIdentity::observe(pid, pid)?;
    Ok((child, identity))
}

fn reap(mut child: Child) -> TestResult {
    drop(child.stdin.take());
    child.wait()?;
    Ok(())
}

// ===========================================================================
// 1. One owner
// ===========================================================================

#[test]
fn a_second_metrics_owner_against_one_home_fails_on_the_writer_lease() -> TestResult {
    let fixture = Fixture::new("single-owner")?;
    let first = fixture.owner()?;

    // `flock` binds to the open file description, not the process, so this
    // second acquisition contends exactly as a second process would.
    let second = fixture.capability();
    assert!(
        matches!(
            second.as_ref().map_err(|error| error.to_string()),
            Err(message) if message.contains("already held")
        ),
        "a second owner must be refused while the first is live",
    );

    // The lease is the capability: dropping the owner releases it.
    drop(first);
    let third = fixture.owner();
    assert!(
        third.is_ok(),
        "the lease must be released when its owner drops",
    );
    Ok(())
}

#[test]
fn owner_lease_error_names_the_lease_without_naming_a_path() -> TestResult {
    let fixture = Fixture::new("lease-error")?;
    let _held = fixture.owner()?;
    let Err(error) = MetricsOwnerCapability::in_fixture_boundary(
        orchestrator_app::fixture_production_boundary(&fixture.root)?,
    ) else {
        return Err("the second mint must fail while the first owner is live".into());
    };
    assert!(matches!(error, OwnerLeaseError::Held { .. }));
    let rendered = format!("{error} {error:?}");
    assert!(
        !rendered.contains(&fixture.path().display().to_string()),
        "a lease error must not disclose the home path: {rendered}",
    );
    Ok(())
}

// ===========================================================================
// 2. Crash-consistent, skill-less phase metrics — the TRK-493 regression
// ===========================================================================

#[test]
fn a_crashed_missions_skill_less_phase_still_lands_terminal_metrics() -> TestResult {
    let fixture = Fixture::new("crash-skilless")?;
    let witness = reaped_witness()?;
    let mission = MissionId::new(MISSION)?;

    // A phase that invoked no skill at all. Under the Go engine this is the
    // exact shape that `recordPhaseSkillsDB` returns early on, leaving the row
    // to the end-of-mission batch that a crash never reaches.
    let mut intent = PhaseMetricIntent::new(mission.clone(), SKILLLESS_PHASE, 1, witness);
    intent.persona = "senior-backend-engineer".to_owned();
    intent.selection_method = "llm".to_owned();
    intent.status = PhaseStatus::Completed;
    intent.duration_s = 42;
    intent.gate_passed = true;
    intent.provider = "claude".to_owned();
    intent.model = "opus".to_owned();
    intent.tokens = TokenCounts {
        input: 100,
        output: 200,
        cache_creation: 10,
        cache_read: 20,
    };
    intent.cost_usd = 1.25;
    intent.worker_name = "worker-1".to_owned();
    assert!(
        intent.parsed_skills.is_empty(),
        "the regression case is a phase with no skills at all",
    );
    let payload = serde_json::to_string(&phase_metric_payload(&intent))?;

    // Create the store before the helper runs, so the helper only appends.
    open_fixture_runtime_store(&fixture.root)?.close()?;

    let status = helper_command("crash-after-commit")?
        .env(HELPER_ROOT, fixture.path())
        .env(HELPER_PAYLOAD, &payload)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            status.signal(),
            Some(9),
            "the helper must die by SIGKILL so no destructor runs; exit was {status:?}",
        );
    }

    // Restart. The publication survived because it was committed inside the
    // journal transaction that recorded the phase.
    let mut store = open_fixture_runtime_store(&fixture.root)?;
    let counts = store.publication_counts()?;
    assert_eq!(
        counts.pending, 1,
        "the committed publication must still be pending after the crash",
    );
    assert_eq!(counts.delivered, 0);
    assert_eq!(counts.dead_letter, 0);

    let owner = fixture.owner()?;
    let drain = publication_drain();
    let report = drain.drain(&mut store, 16, AT, |claimed| match owner.consume(claimed) {
        Ok(()) => DeliveryVerdict::Accepted,
        Err(_) => DeliveryVerdict::Refused,
    })?;
    assert_eq!(report.delivered.len(), 1, "{report:?}");
    assert!(report.dead_lettered.is_empty(), "{report:?}");

    let row = owner
        .phase_row(MISSION, SKILLLESS_PHASE)?
        .ok_or("the skill-less phase must have a metrics row")?;
    assert_eq!(row.status, "completed");
    assert!(row.gate_passed);
    assert!(
        row.parsed_skills.is_empty(),
        "the phase invoked no skills, and that is data — not a reason to skip the row",
    );

    // Redelivery is a no-op rather than a duplicate or an error. A second
    // drain finds nothing pending, and re-running the same write leaves one
    // row with the same values — which is what makes at-least-once delivery
    // safe across a crash.
    let second = drain.drain(&mut store, 16, AT, |_| DeliveryVerdict::Accepted)?;
    assert!(second.delivered.is_empty(), "{second:?}");
    assert!(second.dead_lettered.is_empty(), "{second:?}");
    assert_eq!(store.publication_counts()?.pending, 0);
    assert_eq!(
        owner.recorded_phase_names(MISSION)?,
        vec![SKILLLESS_PHASE.to_owned()],
        "redelivery must not duplicate the phase row",
    );
    assert_eq!(owner.phase_row(MISSION, SKILLLESS_PHASE)?, Some(row));
    store.close()?;
    Ok(())
}

#[test]
fn mission_aggregates_enrich_phases_that_already_exist() -> TestResult {
    let fixture = Fixture::new("terminal-enrichment")?;
    let mission = MissionId::new(MISSION)?;
    let owner = fixture.owner()?;

    // Two phases land first, one of them skill-less.
    for (phase, skills, status) in [
        (SKILLLESS_PHASE, Vec::new(), PhaseStatus::Completed),
        (
            SKILLED_PHASE,
            vec!["decomposer".to_owned()],
            PhaseStatus::Failed,
        ),
    ] {
        let mut intent = PhaseMetricIntent::new(mission.clone(), phase, 1, reaped_witness()?);
        intent.status = status;
        intent.gate_passed = matches!(status, PhaseStatus::Completed);
        intent.retries = 2;
        intent.parsed_skills = skills
            .into_iter()
            .map(|name| orchestrator_app::SkillRef {
                name,
                ..orchestrator_app::SkillRef::default()
            })
            .collect();
        owner.record_phase(&intent)?;
    }

    owner.record_terminal(&TerminalMetricIntent {
        mission: MISSION.to_owned(),
        domain: "dev".to_owned(),
        task: "b3".to_owned(),
        started_at: AT.to_owned(),
        finished_at: AT.to_owned(),
        duration_s: 90,
        status: "completed".to_owned(),
        decomp_source: "phase_lines".to_owned(),
    })?;

    let names = owner.recorded_phase_names(MISSION)?;
    assert_eq!(names.len(), 2, "{names:?}");
    let totals = owner.mission_totals(MISSION)?.ok_or("mission row")?;
    assert_eq!(totals.phases_total, 2);
    assert_eq!(totals.phases_completed, 1);
    assert_eq!(totals.phases_failed, 1);
    assert_eq!(totals.retries_total, 4);
    assert_eq!(
        totals.gate_failures, 1,
        "the failed phase must count as a gate failure",
    );
    Ok(())
}

// ===========================================================================
// 3. Audit-chain immutability and fault discrimination
// ===========================================================================

/// Runs `mutate` against `runtime.db` with the append-only triggers removed,
/// then restores them verbatim.
///
/// Restoring matters: `schema_digest` covers every trigger body, so a store
/// reopened with a missing trigger is refused as corrupt before verification
/// could ever run. Dropping and recreating leaves the schema byte-identical and
/// only the *rows* tampered with — which is the adversary this gate models.
fn tamper<F>(database: &Path, mutate: F) -> TestResult
where
    F: FnOnce(&rusqlite::Connection) -> TestResult,
{
    let connection = rusqlite::Connection::open(database)?;
    let bodies: Vec<String> = {
        let mut statement = connection.prepare(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger' AND name IN ('audit_chain_no_update', 'audit_chain_no_delete')
             ORDER BY name",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(bodies.len(), 2, "both append-only triggers must exist");
    connection
        .execute_batch("DROP TRIGGER audit_chain_no_update; DROP TRIGGER audit_chain_no_delete;")?;
    mutate(&connection)?;
    for body in bodies {
        connection.execute_batch(&body)?;
    }
    Ok(())
}

fn seed_chain(fixture: &Fixture, entries: usize) -> TestResult {
    seed_chain_of(
        fixture,
        entries,
        |index| serde_json::json!({"entry": index}),
    )
}

/// Seeds a chain whose payload for each position is `body(index)`.
///
/// Two chains built with different bodies have different payload digests and
/// therefore different link digests at every sequence, which is what makes a
/// cross-chain checkpoint detectable at all.
fn seed_chain_of<F>(fixture: &Fixture, entries: usize, body: F) -> TestResult
where
    F: Fn(usize) -> serde_json::Value,
{
    let gateway = gateway()?;
    let chain = audit_chain();
    let mut store = open_fixture_runtime_store(&fixture.root)?;
    for index in 0..entries {
        let payload = serde_json::to_string(&body(index))?;
        let (evidence, canonical) =
            admit_phase_metric(&gateway, &format!("phase-{index}"), &payload)?;
        chain.append(&mut store, &evidence, &canonical, AT)?;
    }
    let verification = chain.verify(&store, ChainRange::FromGenesis)?;
    assert!(
        matches!(verification, ChainVerification::Intact(_)),
        "a freshly built chain must verify: {verification:?}",
    );
    store.close()?;
    Ok(())
}

fn fault(fixture: &Fixture) -> TestResult<Option<ChainFault>> {
    let store = open_fixture_runtime_store(&fixture.root)?;
    let verification = audit_chain().verify(&store, ChainRange::FromGenesis)?;
    store.close()?;
    Ok(verification.fault())
}

#[test]
fn the_append_only_triggers_refuse_direct_update_and_delete() -> TestResult {
    let fixture = Fixture::new("chain-triggers")?;
    seed_chain(&fixture, 2)?;

    let connection = rusqlite::Connection::open(fixture.runtime_database())?;
    let update = connection.execute("UPDATE audit_chain SET payload_json = '{}'", []);
    assert!(
        update
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("append-only")),
        "a direct UPDATE must be refused inside the owning process: {update:?}",
    );
    let delete = connection.execute("DELETE FROM audit_chain WHERE sequence = 2", []);
    assert!(
        delete
            .as_ref()
            .err()
            .is_some_and(|error| error.to_string().contains("append-only")),
        "a direct DELETE must be refused inside the owning process: {delete:?}",
    );
    let head_delete = connection.execute("DELETE FROM audit_chain_head", []);
    assert!(
        head_delete.is_err(),
        "the head claim must be retained: {head_delete:?}",
    );
    drop(connection);

    // The store still opens: nothing was actually changed.
    assert_eq!(fault(&fixture)?, None);
    Ok(())
}

#[test]
fn a_mutated_payload_is_caught_by_its_payload_digest() -> TestResult {
    let fixture = Fixture::new("chain-payload")?;
    seed_chain(&fixture, 3)?;
    tamper(&fixture.runtime_database(), |connection| {
        connection.execute(
            "UPDATE audit_chain SET payload_json = ?1 WHERE sequence = 2",
            rusqlite::params![r#"{"entry":99}"#],
        )?;
        Ok(())
    })?;
    assert_eq!(
        fault(&fixture)?,
        Some(ChainFault::PayloadMutated { sequence: 2 })
    );
    Ok(())
}

#[test]
fn recomputing_the_payload_digest_to_hide_a_mutation_forges_the_link() -> TestResult {
    let fixture = Fixture::new("chain-forged-link")?;
    seed_chain(&fixture, 3)?;
    let replacement = r#"{"entry":99}"#;
    let digest = sha256_hex(replacement);
    tamper(&fixture.runtime_database(), |connection| {
        connection.execute(
            "UPDATE audit_chain SET payload_json = ?1, payload_digest = ?2 WHERE sequence = 2",
            rusqlite::params![replacement, digest],
        )?;
        Ok(())
    })?;
    assert_eq!(
        fault(&fixture)?,
        Some(ChainFault::ForgedLink { sequence: 2 })
    );
    Ok(())
}

#[test]
fn recomputing_the_link_digest_breaks_the_next_entrys_backward_link() -> TestResult {
    let fixture = Fixture::new("chain-link-mismatch")?;
    seed_chain(&fixture, 3)?;
    let replacement = r#"{"entry":99}"#;
    let payload_digest = sha256_hex(replacement);
    // Recompute the link exactly as the implementation does, so entry 2 is
    // internally perfect and only entry 3 can notice.
    let prev_digest: String = {
        let connection = rusqlite::Connection::open(fixture.runtime_database())?;
        connection.query_row(
            "SELECT prev_digest FROM audit_chain WHERE sequence = 2",
            [],
            |row| row.get(0),
        )?
    };
    let link = link_digest_hex(&prev_digest, &payload_digest, 2, AT);
    tamper(&fixture.runtime_database(), |connection| {
        connection.execute(
            "UPDATE audit_chain
             SET payload_json = ?1, payload_digest = ?2, link_digest = ?3
             WHERE sequence = 2",
            rusqlite::params![replacement, payload_digest, link],
        )?;
        Ok(())
    })?;
    assert_eq!(
        fault(&fixture)?,
        Some(ChainFault::LinkMismatch { sequence: 3 })
    );
    Ok(())
}

#[test]
fn deleting_the_tail_is_caught_by_the_persisted_head_claim() -> TestResult {
    let fixture = Fixture::new("chain-truncated")?;
    seed_chain(&fixture, 3)?;
    tamper(&fixture.runtime_database(), |connection| {
        connection.execute("DELETE FROM audit_chain WHERE sequence = 3", [])?;
        Ok(())
    })?;
    assert_eq!(
        fault(&fixture)?,
        Some(ChainFault::Truncated {
            last_verified: 2,
            head_claim: 3,
        }),
        "a shorter but internally consistent chain is still a fault",
    );
    Ok(())
}

#[test]
fn a_verified_checkpoint_resumes_without_rewalking_the_chain() -> TestResult {
    let fixture = Fixture::new("chain-checkpoint")?;
    seed_chain(&fixture, 4)?;
    let store = open_fixture_runtime_store(&fixture.root)?;
    let chain = audit_chain();
    let head = chain
        .verify(&store, ChainRange::FromGenesis)?
        .head()
        .ok_or("an intact chain must yield a head")?;
    assert_eq!(head.sequence(), 4);
    let resumed = chain.verify(&store, ChainRange::FromCheckpoint(head))?;
    assert!(
        matches!(resumed, ChainVerification::Intact(_)),
        "{resumed:?}"
    );
    store.close()?;
    Ok(())
}

/// B3 review W3: a checkpoint certifies the chain it was earned on, not any
/// chain of the same length.
///
/// `verify(.., FromCheckpoint(head))` where `head.sequence` already equals the
/// persisted head claim reads no rows at all. Before the fix it returned
/// `Intact` without digesting anything, so a head verified on chain A silently
/// certified an equal-length chain B whose every payload differed — the head
/// claim is only a count. The empty resume range now re-reads the row at the
/// checkpoint's own sequence and requires its stored link digest to match.
#[test]
fn a_checkpoint_from_another_chain_cannot_certify_an_equal_length_chain() -> TestResult {
    let chain_a = Fixture::new("chain-checkpoint-a")?;
    let chain_b = Fixture::new("chain-checkpoint-b")?;
    seed_chain_of(&chain_a, 3, |index| serde_json::json!({"entry": index}))?;
    seed_chain_of(
        &chain_b,
        3,
        |index| serde_json::json!({"entry": index, "chain": "b"}),
    )?;

    let chain = audit_chain();
    let store_a = open_fixture_runtime_store(&chain_a.root)?;
    let head_a = chain
        .verify(&store_a, ChainRange::FromGenesis)?
        .head()
        .ok_or("chain A must verify from genesis")?;
    store_a.close()?;
    assert_eq!(head_a.sequence(), 3);

    let store_b = open_fixture_runtime_store(&chain_b.root)?;
    let head_b = chain
        .verify(&store_b, ChainRange::FromGenesis)?
        .head()
        .ok_or("chain B must verify from genesis on its own")?;
    assert_eq!(head_b.sequence(), head_a.sequence(), "equal length");
    assert_ne!(
        head_b.link_digest(),
        head_a.link_digest(),
        "the two chains must genuinely differ, or the test proves nothing",
    );

    let transplanted = chain.verify(&store_b, ChainRange::FromCheckpoint(head_a))?;
    assert_eq!(
        transplanted.fault(),
        Some(ChainFault::CheckpointNotOnChain { sequence: 3 }),
        "chain A's head must not certify chain B: {transplanted:?}",
    );

    // Chain B's own checkpoint still resumes, so the anchor rejects the
    // transplant rather than every empty resume range.
    let resumed = chain.verify(&store_b, ChainRange::FromCheckpoint(head_b))?;
    assert!(
        matches!(resumed, ChainVerification::Intact(_)),
        "chain B's own head must still resume: {resumed:?}",
    );
    store_b.close()?;
    Ok(())
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut digest = Sha256::new();
    digest.update(text.as_bytes());
    hex(digest.finalize().as_slice())
}

fn link_digest_hex(prev: &str, payload: &str, sequence: u64, recorded_at: &str) -> String {
    use sha2::{Digest, Sha256};
    fn field(digest: &mut Sha256, bytes: &[u8]) {
        digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
        digest.update(bytes);
    }
    let mut digest = Sha256::new();
    field(&mut digest, &unhex(prev));
    field(&mut digest, &unhex(payload));
    field(&mut digest, &sequence.to_be_bytes());
    field(&mut digest, recorded_at.as_bytes());
    hex(digest.finalize().as_slice())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2)
        .map(|index| u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).unwrap_or_default())
        .collect()
}

// ===========================================================================
// 4. Secret redaction
// ===========================================================================

/// Synthetic strings shaped like each `secretPatterns` family
/// (`internal/advisorbridge/prepare.go:32-43`). None is a real credential.
fn secret_corpus() -> Vec<(&'static str, String)> {
    vec![
        (
            "private-key",
            "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----".to_owned(),
        ),
        (
            "credential-header",
            "authorization: Bearer abcdefghijklmnopqrstuvwxyz012345".to_owned(),
        ),
        ("api-token-sk", format!("sk-{}", "a".repeat(40))),
        ("api-token-github", format!("ghp_{}", "b".repeat(36))),
        ("api-token-google", format!("AIza{}", "C".repeat(35))),
        ("aws-key-id", format!("AKIA{}", "D".repeat(16))),
        (
            "assignment",
            "database_url=postgres://user:hunter2@example.invalid/db".to_owned(),
        ),
        (
            "url-userinfo",
            "https://alice:s3cr3t@example.invalid/path".to_owned(),
        ),
    ]
}

#[test]
fn every_secret_family_is_refused_before_it_can_become_durable() -> TestResult {
    let fixture = Fixture::new("secret-admission")?;
    open_fixture_runtime_store(&fixture.root)?.close()?;
    let gateway = gateway()?;

    for (label, secret) in secret_corpus() {
        let payload = serde_json::json!({"detail": secret});
        let admitted = admit_metric(
            &gateway,
            PHASE_METRIC_TYPE,
            &format!("{NAMESPACE}/{PHASE_METRIC_TYPE}/phase-secret"),
            &payload,
        );
        let error = match admitted {
            Ok(_) => return Err(format!("{label} was admitted; it must be refused").into()),
            Err(error) => error,
        };
        // The refusal must be *because of the secret*. Without this the test
        // would pass just as well if admission rejected everything for an
        // unrelated reason — an unregistered type, say — and would then prove
        // nothing about the scanner.
        assert!(
            matches!(error, KnowledgeError::SecretBearingPayload { .. }),
            "{label} was refused for the wrong reason: {error:?}",
        );
        let rendered = format!("{error}");
        let debugged = format!("{error:?}");

        // The refusal names the family, never the bytes.
        for needle in secret.split_whitespace() {
            if needle.len() < 8 {
                continue;
            }
            assert!(
                !rendered.contains(needle),
                "{label}: Display leaked {needle:?} in {rendered:?}",
            );
            assert!(
                !debugged.contains(needle),
                "{label}: Debug leaked {needle:?} in {debugged:?}",
            );
        }
    }

    // Positive control: the same envelope shape with clean content is admitted.
    // Without this the refusals above could be explained by the payload shape
    // rather than by its content.
    admit_metric(
        &gateway,
        PHASE_METRIC_TYPE,
        &format!("{NAMESPACE}/{PHASE_METRIC_TYPE}/phase-clean"),
        &serde_json::json!({"detail": "provider returned a 500 after two retries"}),
    )?;

    // Nothing reached the queue: admission runs before durability, so
    // `runtime.db` cannot accumulate secrets a later egress filter would have
    // to catch.
    let store = open_fixture_runtime_store(&fixture.root)?;
    let counts = store.publication_counts()?;
    assert_eq!(counts.pending, 0);
    assert_eq!(counts.delivered, 0);
    assert_eq!(counts.dead_letter, 0);
    store.close()?;
    Ok(())
}

#[test]
fn a_secret_bearing_phase_error_is_redacted_before_it_reaches_metrics_db() -> TestResult {
    let fixture = Fixture::new("secret-error-column")?;
    let owner = fixture.owner()?;
    let mission = MissionId::new(MISSION)?;

    // `phases.error_message` is the one column carrying free text from a
    // worker's stderr, so it is the one place a credential can reach
    // `metrics.db`.
    let token = format!("sk-{}", "e".repeat(40));
    let mut intent = PhaseMetricIntent::new(mission, SKILLLESS_PHASE, 1, reaped_witness()?);
    intent.status = PhaseStatus::Failed;
    intent.error_type = "provider_error".to_owned();
    intent.error_message = format!("provider refused the request: {token} was rejected");
    owner.record_phase(&intent)?;

    let row = owner
        .phase_row(MISSION, SKILLLESS_PHASE)?
        .ok_or("the failed phase must still be recorded")?;
    assert!(
        !row.error_message.contains(&token),
        "the stored message still carries the token: {:?}",
        row.error_message,
    );
    assert!(
        row.error_message.contains("[redacted:"),
        "the redaction must be visible, not silent: {:?}",
        row.error_message,
    );
    assert!(
        row.error_message.contains("provider refused the request"),
        "surrounding context must survive: {:?}",
        row.error_message,
    );
    Ok(())
}

#[test]
fn a_clean_payload_still_publishes_and_drains() -> TestResult {
    let fixture = Fixture::new("clean-terminal")?;
    let gateway = gateway()?;
    let mission = MissionId::new(MISSION)?;
    let owner = fixture.owner()?;

    let terminal = TerminalMetricIntent {
        mission: MISSION.to_owned(),
        domain: "dev".to_owned(),
        task: "b3 metrics".to_owned(),
        started_at: AT.to_owned(),
        finished_at: AT.to_owned(),
        duration_s: 12,
        status: "completed".to_owned(),
        decomp_source: "phase_lines".to_owned(),
    };
    let (evidence, canonical) =
        admit_terminal_metric(&gateway, &terminal_metric_payload(&terminal))?;
    let publication = gateway.publication_intent(&mission, None, 1, &evidence, &canonical)?;

    let mut store = open_fixture_runtime_store(&fixture.root)?;
    let key = publication.idempotency_key().to_owned();
    store.append(&gateway.attach(transition("mission.completed", &mission)?, publication)?)?;
    assert_eq!(
        store.publication_state(&key)?,
        Some(PublicationState::Pending)
    );

    let report =
        publication_drain().drain(&mut store, 8, AT, |claimed| match owner.consume(claimed) {
            Ok(()) => DeliveryVerdict::Accepted,
            Err(_) => DeliveryVerdict::Refused,
        })?;
    assert_eq!(report.delivered, vec![key.clone()], "{report:?}");
    assert_eq!(
        store.publication_state(&key)?,
        Some(PublicationState::Delivered)
    );
    store.close()?;

    let totals = owner.mission_totals(MISSION)?.ok_or("mission row")?;
    assert_eq!(totals.status, "completed");
    assert_eq!(totals.duration_s, 12);
    Ok(())
}

/// Provider and credential variables scrubbed from every helper subprocess.
///
/// Verbatim from `CORE-100-VERIFICATION-GAPS.md` lines 21-37. The helper reads
/// none of them, but "no credential is touched" is an absolute: a child holding
/// live provider keys in its environment is one careless `Command` away from
/// leaking them, and `env_remove` costs nothing to keep that impossible.
const SCRUBBED_PROVIDER_VARIABLES: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "OPENROUTER_API_KEY",
    "GEMINI_API_KEY",
    "ELEVENLABS_API_KEY",
    "ALLUKA_AUTH_FILE",
    "CLAUDE_CREDENTIALS_DIR",
    "CLAUDE_CREDENTIALS_FILE",
    "CODEX_PATH",
    "NANIKA_HERMETIC_PROCESS_CANARY",
];

/// Removes every provider variable from a helper command's environment.
fn scrub_provider_environment(command: &mut std::process::Command) -> &mut std::process::Command {
    for name in SCRUBBED_PROVIDER_VARIABLES {
        command.env_remove(name);
    }
    command
}
