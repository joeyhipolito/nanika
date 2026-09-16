//! Gate 7 — `cargo test -p orchestrator-app --locked --test authority_adversarial_e2e`.
//!
//! Every other B3 gate proves a good path works. This one proves the bad paths
//! fail, with an adversary actually attempting them rather than an assertion
//! that nothing was observed. The difference matters: a fail-closed property
//! that is never attacked is an assumption, and two of the subcases below were
//! *escapes* when they were first written — the fixture boundary was defeated
//! by a symbolic link, and the additive writers stored credentials verbatim.
//! Both are now regression tests rather than descriptions.
//!
//! Six attack classes, each with at least one subcase:
//!
//! 1. **Path traversal** — a `..`, a separator, or an absolute segment pushed
//!    through a persona name, a project key, or a Go-layout path.
//! 2. **Symlink swap** — a link planted at a leaf or an intermediate directory,
//!    and a root swapped *between* minting a capability and using it (TOCTOU).
//! 3. **Forged receipts and replays** — a digest that does not recompute, a
//!    checkpoint from another chain, an in-place rewrite dressed as an append,
//!    and a replayed idempotency key.
//! 4. **Outside-root process and network** — a subprocess whose `PATH` is a
//!    minefield of trap executables and whose proxy variables point at a
//!    loopback listener, exercising the whole B3 surface.
//! 5. **Secret-bearing values** — pushed through admission, through an additive
//!    write, through an error's `Display` and `Debug`, and through egress.
//! 6. **Hermetic fixture audit** — a static audit of *every* B3 gate file for
//!    live-home reads, credential names, network types, and unscrubbed
//!    subprocess environments.
//!
//! **Hermeticity.** Every fixture lives under a mode-0700 directory beneath the
//! canonical process temp directory and is removed on drop. Nothing here reads
//! the operator's home, no credential is read or passed to a child, no provider
//! is contacted, and the only database files touched are the ones these tests
//! create. Each refusal subcase records a whole-tree digest of its fixture
//! before the attack and asserts it byte-identical afterwards, so "the attack
//! was refused" also means "the attack applied no effect".

#![allow(clippy::doc_markdown, reason = "prose names Go symbols and file paths")]

use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write as _,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use orchestrator_app::{
    AdditiveFixtureGrant, AppKnowledgeGateway, AuditChainError, CapabilityError, ChainFault,
    ChainRange, ChainVerification, CompatibilityProjection, GoAdapterError, GoLearningAdapter,
    GoLearningAppender, GoLearningReader, GoMemoryAdapter, GoMemoryAppender, GoMemoryReader,
    IsolatedFixtureRoot, JournalIntent, LearningListQuery, MemoryEntry, MemoryFileKind,
    MetricsOwner, MetricsOwnerCapability, NewLearningRow, PersonaName, ProjectKey,
    TerminalMetricIntent, audit_chain, open_fixture_runtime_store,
};
use orchestrator_core::MissionId;
use orchestrator_knowledge::{
    CanonicalJson, Delegation, ExpectedHead, FieldMask, Governance, KnowledgeCapability,
    KnowledgeError, LifecycleState, Namespace, Operation, PrimitiveKind, Provenance,
    PublicationEvidence, RecordEnvelope, RecordId, Redacted, Redactor, RegistryGeneration,
    RevisionId, SchemaVersion, SecretKind, SecretVerdict, Sensitivity, TypeManifest, TypeName,
    TypeRegistry, Validity,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static CASE: AtomicU64 = AtomicU64::new(1);

const NAMESPACE: &str = "adversary";
const TYPE_NAME: &str = "adversary/probe";
const MISSION: &str = "mission-b3-adversary";
const AT: &str = "2026-08-26T12:00:00Z";

/// A pristine file planted outside every fixture, standing in for a live home.
///
/// Its content is asserted unchanged after each escape attempt. This is what
/// separates "the call returned an error" from "the call had no effect": an
/// implementation could refuse *and* have already written.
const OUTSIDE_SENTINEL: &str = "PRISTINE-OUTSIDE-THE-FIXTURE\n";

// ===========================================================================
// Helper subprocess
// ===========================================================================

/// Env var selecting a helper mode when this test binary re-execs itself.
const HELPER_MODE: &str = "NANIKA_B3_ADVERSARY_GATE_HELPER";
/// Fixture root handed to a helper.
const HELPER_ROOT: &str = "NANIKA_B3_ADVERSARY_GATE_ROOT";

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

/// Re-exec entry point.
///
/// `crates/orchestrator-app/tests/fixtures/**` is outside the B3 lease, so no
/// new helper binary may be committed. Re-execing this test binary with a mode
/// selector gives a genuinely separate, genuinely compiled first-party helper
/// without adding a file. In an ordinary run the mode variable is unset and
/// this returns immediately.
#[test]
fn helper_entrypoint() {
    let Ok(mode) = std::env::var(HELPER_MODE) else {
        return;
    };
    let code = match mode.as_str() {
        // Drive the whole B3 surface once. The parent has replaced `PATH` with
        // a directory of trap executables and pointed every proxy variable at a
        // listener it owns, so any spawn or connect this surface performs is
        // recorded by the parent rather than merely unobserved.
        "exercise-surface" => match exercise_b3_surface() {
            Ok(()) => 0,
            Err(error) => {
                let _ = writeln!(std::io::stderr(), "helper failed: {error}");
                1
            }
        },
        _ => 2,
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

/// Exercises every B3 durability and read path against one fixture root.
fn exercise_b3_surface() -> TestResult {
    let root = PathBuf::from(std::env::var(HELPER_ROOT)?);
    let fixture = IsolatedFixtureRoot::identify(&root)?;
    let key = project_key();

    // Go compatibility reads.
    let learning = GoLearningAdapter::in_fixture(&fixture)?;
    learning.schema_version()?;
    learning.list(&list_query(4))?;
    learning.stats()?;
    learning.row_set_digest()?;
    let memory = GoMemoryAdapter::in_fixture(&fixture)?;
    memory.read_project_memory(&key)?;
    memory.file_digest(&key, MemoryFileKind::New, None)?;

    // Declared additive writes. The grant requires a root *this* process
    // created: the handed root was created by the parent and only adopted
    // here, and an adopted root carries no proof of disposability. Create a
    // fresh child root and seed it with the same Go shape.
    let additive = IsolatedFixtureRoot::create_fresh(&root)?;
    seed_learnings(additive.path())?;
    seed_memory(additive.path(), &key)?;
    let grant = AdditiveFixtureGrant::in_fixture(&additive)?;
    GoLearningAppender::for_grant(&grant)?.insert_new(&grant, &[new_row("helper-insert")])?;
    GoMemoryAppender::for_grant(&grant)?.append_new(&grant, &key, &[entry("helper append")])?;

    // Knowledge admission, durable publication, and the audit chain.
    let gateway = gateway()?;
    let mission = MissionId::new(MISSION)?;
    let (evidence, canonical) = admit(&gateway, "helper", &serde_json::json!({"probe": 1}))?;
    let intent = gateway.publication_intent(&mission, Some("phase-1"), 1, &evidence, &canonical)?;
    let mut store = open_fixture_runtime_store(&fixture)?;
    store.append(&gateway.attach(transition("helper-1", &mission)?, intent)?)?;
    let chain = audit_chain();
    chain.append(&mut store, &evidence, &canonical, AT)?;
    let verification = chain.verify(&store, ChainRange::FromGenesis)?;
    if verification.fault().is_some() {
        return Err("the helper's own chain must verify".into());
    }
    store.close()?;

    // The single metrics owner.
    let owner = MetricsOwner::assume(MetricsOwnerCapability::in_fixture_boundary(
        orchestrator_app::fixture_production_boundary(&fixture)?,
    )?)?;
    owner.record_terminal(&terminal_intent())?;
    owner.mission_totals(MISSION)?;
    Ok(())
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
            "orchestrator-rs-adversary-{}-{number}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&parent);
        private_dir(&parent)?;
        let root = IsolatedFixtureRoot::create_fresh(&parent)?;
        Ok(Self { parent, root })
    }

    fn path(&self) -> &Path {
        self.root.path()
    }

    /// A directory beside the fixture standing in for a live home.
    ///
    /// Deliberately a *sibling*, not a child: an escape that stays inside the
    /// harness parent is still an escape from the fixture root, and keeping the
    /// target reachable lets each subcase prove it was left untouched.
    fn outside(&self) -> TestResult<PathBuf> {
        let outside = self.parent.join("live-home");
        private_dir(&outside)?;
        Ok(outside)
    }

    fn sentinel(&self, relative: &Path) -> TestResult<PathBuf> {
        let target = self.outside()?.join(relative);
        if let Some(directory) = target.parent() {
            private_dir(directory)?;
        }
        std::fs::write(&target, OUTSIDE_SENTINEL)?;
        Ok(target)
    }

    fn grant(&self) -> TestResult<AdditiveFixtureGrant> {
        Ok(AdditiveFixtureGrant::in_fixture(&self.root)?)
    }

    /// Digest of the fixture tree that never follows a symbolic link.
    ///
    /// Covers names, entry kinds, link targets, file sizes, and file bytes, so
    /// a refused subcase can assert not merely "no new file" but "not one byte
    /// changed anywhere beneath the root".
    fn tree_digest(&self) -> TestResult<String> {
        use sha2::Digest as _;
        let mut digest = sha2::Sha256::new();
        hash_tree(&mut digest, self.path())?;
        Ok(hex(&sha2::Digest::finalize(digest)))
    }
}

fn hash_tree(digest: &mut sha2::Sha256, directory: &Path) -> TestResult {
    use sha2::Digest as _;
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    entries.sort();
    for entry in entries {
        let name = entry
            .file_name()
            .map(std::ffi::OsStr::to_string_lossy)
            .unwrap_or_default()
            .into_owned();
        let metadata = std::fs::symlink_metadata(&entry)?;
        digest.update(name.as_bytes());
        if metadata.file_type().is_symlink() {
            digest.update(b"L");
            digest.update(std::fs::read_link(&entry)?.as_os_str().as_encoded_bytes());
        } else if metadata.is_dir() {
            digest.update(b"D");
            hash_tree(digest, &entry)?;
            digest.update(b"/");
        } else {
            digest.update(b"F");
            digest.update(metadata.len().to_be_bytes());
            digest.update(std::fs::read(&entry)?);
        }
    }
    Ok(())
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

// ===========================================================================
// Go-shaped fixture content
// ===========================================================================

/// The `learnings` DDL quoted from `internal/learning/db.go:85-186`.
///
/// Seeded through the real shape so the row-set digest and the adapter's schema
/// guard both mean what they claim.
fn seed_learnings(root: &Path) -> TestResult<PathBuf> {
    let directory = root.join(".alluka");
    private_dir(&directory)?;
    let database = directory.join("learnings.db");
    let connection = rusqlite::Connection::open(&database)?;
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (version INTEGER NOT NULL);
         INSERT INTO schema_version (version)
            SELECT 1 WHERE NOT EXISTS (SELECT 1 FROM schema_version);
         CREATE TABLE IF NOT EXISTS learnings (
            id TEXT PRIMARY KEY,
            type TEXT NOT NULL,
            content TEXT NOT NULL,
            context TEXT DEFAULT '',
            domain TEXT NOT NULL,
            worker_name TEXT DEFAULT '',
            workspace_id TEXT DEFAULT '',
            tags TEXT DEFAULT '',
            seen_count INTEGER DEFAULT 1,
            used_count INTEGER DEFAULT 0,
            quality_score REAL DEFAULT 0.0,
            created_at DATETIME NOT NULL,
            last_used_at DATETIME,
            embedding BLOB,
            injection_count INTEGER DEFAULT 0,
            compliance_count INTEGER DEFAULT 0,
            compliance_rate REAL DEFAULT 0.0,
            archived INTEGER DEFAULT 0,
            promoted_at DATETIME
         );
         INSERT OR IGNORE INTO learnings (id, type, content, context, domain, created_at)
            VALUES ('seed-1', 'insight', 'a pre-existing row', '', 'dev', '2026-08-26T00:00:00Z');",
    )?;
    drop(connection);
    Ok(database)
}

fn seed_memory(root: &Path, key: &ProjectKey) -> TestResult<PathBuf> {
    let directory = root
        .join(".claude")
        .join("projects")
        .join(key.as_str())
        .join("memory");
    private_dir(&directory)?;
    std::fs::write(directory.join("MEMORY.md"), "- a pre-existing memory\n")?;
    std::fs::write(directory.join("MEMORY_NEW.md"), "- first line\n")?;
    Ok(directory)
}

fn seed_persona(root: &Path, persona: &str) -> TestResult {
    let directory = root.join("nanika").join("personas").join(persona);
    private_dir(&directory)?;
    std::fs::write(directory.join("MEMORY.md"), "- a persona memory\n")?;
    Ok(())
}

fn project_key() -> ProjectKey {
    ProjectKey::encode("/fixture/project")
}

fn list_query(limit: usize) -> LearningListQuery {
    LearningListQuery {
        domain: None,
        learning_type: None,
        include_archived: true,
        limit,
    }
}

fn new_row(id: &str) -> NewLearningRow {
    NewLearningRow {
        id: id.to_owned(),
        learning_type: "insight".to_owned(),
        content: "an additive row".to_owned(),
        context: String::new(),
        domain: "dev".to_owned(),
        created_at: AT.to_owned(),
    }
}

fn entry(content: &str) -> MemoryEntry {
    MemoryEntry {
        content: content.to_owned(),
        ..MemoryEntry::default()
    }
}

fn terminal_intent() -> TerminalMetricIntent {
    TerminalMetricIntent {
        mission: MISSION.to_owned(),
        domain: "dev".to_owned(),
        task: "b3 adversarial gate".to_owned(),
        started_at: AT.to_owned(),
        finished_at: AT.to_owned(),
        duration_s: 1,
        status: "completed".to_owned(),
        decomp_source: "phase_lines".to_owned(),
    }
}

// ===========================================================================
// Knowledge plane wiring
// ===========================================================================

fn manifests() -> Result<Vec<TypeManifest>, KnowledgeError> {
    Ok(vec![TypeManifest {
        namespace: Namespace::new(NAMESPACE)?,
        type_name: TypeName::new(TYPE_NAME)?,
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
    }])
}

fn holder_capability(
    generation: RegistryGeneration,
) -> Result<KnowledgeCapability, KnowledgeError> {
    Ok(KnowledgeCapability {
        namespace: Namespace::new(NAMESPACE)?,
        types: [TypeName::new(TYPE_NAME)?].into_iter().collect(),
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
    let capability = holder_capability(registry.generation())?;
    Ok(AppKnowledgeGateway::new(registry, capability))
}

fn admit(
    gateway: &AppKnowledgeGateway,
    identity: &str,
    payload: &serde_json::Value,
) -> Result<(PublicationEvidence, String), KnowledgeError> {
    let body = CanonicalJson::encode(payload)?;
    let canonical = body.as_str().to_owned();
    let envelope = RecordEnvelope::new(
        RecordId::new(format!("{NAMESPACE}/{TYPE_NAME}/{identity}"))?,
        RevisionId::GENESIS,
        ExpectedHead::absent(),
        Governance {
            namespace: Namespace::new(NAMESPACE)?,
            type_name: TypeName::new(TYPE_NAME)?,
            schema_version: SchemaVersion::new(1)?,
            provenance: Provenance::new("authority-adversarial-gate", Some(MISSION))?,
            sensitivity: Sensitivity::Internal,
            lifecycle: LifecycleState::Active,
            registry_generation: gateway.registry().generation(),
        },
        body,
    );
    Ok((gateway.admit(envelope)?, canonical))
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
// 1. Path traversal
// ===========================================================================

#[test]
fn traversal_spellings_in_a_persona_name_are_refused_before_any_read() -> TestResult {
    let fixture = Fixture::new("persona-traversal")?;
    seed_persona(fixture.path(), "auditor")?;
    let sentinel = fixture.sentinel(Path::new("MEMORY.md"))?;
    let adapter = GoMemoryAdapter::in_fixture(&fixture.root)?;
    let before = fixture.tree_digest()?;

    for spelling in [
        "..",
        ".",
        "../live-home",
        "../../live-home",
        "auditor/../../live-home",
        "/absolute",
        "nested/name",
        "name\\with\\backslash",
        "name with space",
        "name\u{0}nul",
    ] {
        let refusal = PersonaName::new(spelling);
        assert!(
            matches!(refusal, Err(GoAdapterError::EscapesFixtureRoot)),
            "persona name {spelling:?} was not refused as an escape: {:?}",
            refusal.map(|name| name.as_str().to_owned()),
        );
    }

    // Positive control: a legitimate persona still reads, so the refusals above
    // are about the *spelling* and not about the adapter refusing everything.
    let entries = adapter.read_persona_memory(&PersonaName::new("auditor")?)?;
    assert_eq!(entries.len(), 1, "{entries:?}");

    assert_eq!(
        std::fs::read_to_string(&sentinel)?,
        OUTSIDE_SENTINEL,
        "a refused traversal must not have read or rewritten the outside file",
    );
    assert_eq!(fixture.tree_digest()?, before);
    Ok(())
}

#[test]
fn a_project_key_cannot_carry_a_separator_into_the_go_layout() -> TestResult {
    let fixture = Fixture::new("project-key-traversal")?;
    let honest = project_key();
    seed_memory(fixture.path(), &honest)?;
    let sentinel = fixture.sentinel(Path::new("memory/MEMORY_NEW.md"))?;
    let adapter = GoMemoryAdapter::in_fixture(&fixture.root)?;
    let grant = fixture.grant()?;
    let appender = GoMemoryAppender::for_grant(&grant)?;
    let before = fixture.tree_digest()?;

    // `encodeProjectKey` maps both `/` and `.` to `-`, so no traversal spelling
    // survives encoding. Asserting the *encoding* rather than the refusal is
    // the stronger claim: there is no key value that names a second directory.
    for hostile in [
        "../../live-home/memory",
        "/../live-home",
        "..",
        "/absolute/someone/.claude",
    ] {
        let encoded = ProjectKey::encode(hostile);
        assert!(
            !encoded.as_str().contains('/') && !encoded.as_str().contains('.'),
            "{hostile:?} encoded to {:?}, which still names a path",
            encoded.as_str(),
        );
        // The encoded key names a directory that does not exist, so the read is
        // an absence — never a read of something outside the root.
        assert!(
            matches!(
                adapter.read_project_memory(&encoded),
                Err(GoAdapterError::SourceAbsent { .. })
            ),
            "{hostile:?} resolved to something readable",
        );
        assert!(
            matches!(
                appender.append_new(&grant, &encoded, &[entry("traversal payload")]),
                Err(GoAdapterError::SourceAbsent { .. })
            ),
            "{hostile:?} resolved to something writable",
        );
    }

    assert_eq!(std::fs::read_to_string(&sentinel)?, OUTSIDE_SENTINEL);
    assert_eq!(
        fixture.tree_digest()?,
        before,
        "a refused traversal must leave the fixture byte-identical",
    );
    Ok(())
}

// ===========================================================================
// 2. Symlink swap
// ===========================================================================

#[test]
fn a_symlinked_append_target_is_refused_and_the_outside_file_is_untouched() -> TestResult {
    // REGRESSION. `GoMemoryAppender::append_new` used to resolve its target
    // lexically with `resolve_within` and then open it with ambient
    // `std::fs::OpenOptions`, which follows links. A `MEMORY_NEW.md` symlink
    // planted inside the fixture appended the caller's entry to whatever it
    // pointed at, defeating the entire purpose of `AdditiveFixtureGrant`.
    let fixture = Fixture::new("symlink-append-leaf")?;
    let key = project_key();
    let directory = seed_memory(fixture.path(), &key)?;
    let sentinel = fixture.sentinel(Path::new("MEMORY_NEW.md"))?;
    std::fs::remove_file(directory.join("MEMORY_NEW.md"))?;
    std::os::unix::fs::symlink(&sentinel, directory.join("MEMORY_NEW.md"))?;

    let grant = fixture.grant()?;
    let appender = GoMemoryAppender::for_grant(&grant)?;
    let before = fixture.tree_digest()?;

    let refusal = appender.append_new(&grant, &key, &[entry("escaping payload")]);
    assert!(
        matches!(refusal, Err(GoAdapterError::EscapesFixtureRoot)),
        "a symlinked append target must be refused as an escape: {refusal:?}",
    );
    assert_eq!(
        std::fs::read_to_string(&sentinel)?,
        OUTSIDE_SENTINEL,
        "the outside file was written through the link",
    );
    assert_eq!(fixture.tree_digest()?, before);
    Ok(())
}

#[test]
fn a_symlinked_directory_component_cannot_redirect_an_additive_append() -> TestResult {
    // REGRESSION, second half. Even with a real file at the leaf, a link at any
    // *directory* component redirected the whole subtree.
    let fixture = Fixture::new("symlink-append-dir")?;
    let key = project_key();
    let outside = fixture.outside()?;
    let victim = outside
        .join("projects")
        .join(key.as_str())
        .join("memory")
        .join("MEMORY_NEW.md");
    private_dir(victim.parent().unwrap_or(&outside))?;
    std::fs::write(&victim, OUTSIDE_SENTINEL)?;
    std::os::unix::fs::symlink(&outside, fixture.path().join(".claude"))?;

    let grant = fixture.grant()?;
    let appender = GoMemoryAppender::for_grant(&grant)?;
    let reader = GoMemoryAdapter::in_fixture(&fixture.root)?;
    let before = fixture.tree_digest()?;

    let write = appender.append_new(&grant, &key, &[entry("escaping payload")]);
    assert!(
        matches!(write, Err(GoAdapterError::EscapesFixtureRoot)),
        "a symlinked directory component must be refused as an escape: {write:?}",
    );
    // The read side must refuse identically: a boundary that only holds for
    // writes still leaks whatever the link points at.
    let read = reader.read_project_memory(&key);
    assert!(
        matches!(read, Err(GoAdapterError::EscapesFixtureRoot)),
        "the read side followed the link: {read:?}",
    );
    assert_eq!(std::fs::read_to_string(&victim)?, OUTSIDE_SENTINEL);
    assert_eq!(fixture.tree_digest()?, before);
    Ok(())
}

#[test]
fn a_symlinked_learnings_db_is_refused_before_a_connection_is_opened() -> TestResult {
    // REGRESSION. Both learning adapters resolved their path lexically and then
    // asked `is_file()`, which follows links, so construction succeeded over a
    // store outside the root. The refusal that actually happened came from
    // SQLite's `SQLITE_OPEN_NOFOLLOW` — an incidental property of a dependency,
    // not a boundary this workspace owns.
    for (label, plant_link_at_directory) in [("learn-link-leaf", false), ("learn-link-dir", true)] {
        let fixture = Fixture::new(label)?;
        let outside = fixture.outside()?;
        seed_learnings(&outside)?;
        if plant_link_at_directory {
            std::os::unix::fs::symlink(&outside, fixture.path().join(".alluka"))?;
        } else {
            private_dir(&fixture.path().join(".alluka"))?;
            std::os::unix::fs::symlink(
                outside.join(".alluka").join("learnings.db"),
                fixture.path().join(".alluka").join("learnings.db"),
            )?;
        }
        let before = fixture.tree_digest()?;

        let reader = GoLearningAdapter::in_fixture(&fixture.root);
        assert!(
            matches!(
                reader.as_ref().err(),
                Some(GoAdapterError::EscapesFixtureRoot)
            ),
            "{label}: the reader was constructed over an escaped store",
        );
        let grant = fixture.grant()?;
        let appender = GoLearningAppender::for_grant(&grant);
        assert!(
            matches!(
                appender.as_ref().err(),
                Some(GoAdapterError::EscapesFixtureRoot)
            ),
            "{label}: the appender was constructed over an escaped store",
        );
        assert_eq!(fixture.tree_digest()?, before, "{label}");
    }
    Ok(())
}

#[test]
fn a_symlinked_metrics_db_is_refused_by_the_owner() -> TestResult {
    let fixture = Fixture::new("symlink-metrics")?;
    let outside = fixture.outside()?;
    let victim = outside.join("metrics.db");
    rusqlite::Connection::open(&victim)?
        .execute_batch("CREATE TABLE IF NOT EXISTS canary (id TEXT PRIMARY KEY);")?;
    let victim_before = std::fs::read(&victim)?;
    std::os::unix::fs::symlink(&victim, fixture.path().join("metrics.db"))?;

    let capability = MetricsOwnerCapability::in_fixture_boundary(
        orchestrator_app::fixture_production_boundary(&fixture.root)?,
    )?;
    let owner = MetricsOwner::assume(capability);
    assert!(
        owner.is_err(),
        "the owner opened metrics.db through a symbolic link",
    );
    assert_eq!(
        std::fs::read(&victim)?,
        victim_before,
        "the outside metrics store was modified",
    );
    Ok(())
}

#[test]
fn a_root_swapped_between_minting_and_use_is_caught_by_grant_verification() -> TestResult {
    // TOCTOU. The grant is minted against a canonical root; the adversary then
    // replaces that root with a link to somewhere else. Every additive write
    // re-verifies, so the swap is caught at use rather than trusted from
    // admission.
    let fixture = Fixture::new("toctou-grant")?;
    let key = project_key();
    seed_memory(fixture.path(), &key)?;
    seed_learnings(fixture.path())?;
    let grant = fixture.grant()?;
    let memory_appender = GoMemoryAppender::for_grant(&grant)?;
    let learning_appender = GoLearningAppender::for_grant(&grant)?;

    // Swap: move the real root aside and leave a link with the same name.
    let decoy = fixture.parent.join("decoy-root");
    private_dir(&decoy)?;
    let real = fixture.parent.join("real-root");
    std::fs::rename(fixture.path(), &real)?;
    std::os::unix::fs::symlink(&decoy, fixture.path())?;

    let memory_write = memory_appender.append_new(&grant, &key, &[entry("post-swap payload")]);
    assert!(
        matches!(memory_write, Err(GoAdapterError::GrantIdentityChanged)),
        "the memory appender did not notice the swap: {memory_write:?}",
    );
    let learning_write = learning_appender.insert_new(&grant, &[new_row("post-swap")]);
    assert!(
        matches!(learning_write, Err(GoAdapterError::GrantIdentityChanged)),
        "the learning appender did not notice the swap: {learning_write:?}",
    );
    assert!(
        std::fs::read_dir(&decoy)?.next().is_none(),
        "the decoy root received a write",
    );

    // Restore so the fixture's own cleanup removes the real tree.
    std::fs::remove_file(fixture.path())?;
    std::fs::rename(&real, fixture.path())?;
    Ok(())
}

#[test]
fn a_grant_cannot_be_minted_over_a_root_this_process_did_not_create() -> TestResult {
    // B3 review W1 / TRK-1249. `IsolatedFixtureRoot::identify` names *any*
    // existing directory, an operator's live home included, and until now that
    // was enough to mint the additive grant — the only thing separating a
    // disposable fixture from a real store was the caller's intent. The
    // directory below is shaped exactly like the real thing — an
    // `.alluka/learnings.db` carrying the Go DDL and a seeded row — so
    // provenance is the only thing that can refuse it.
    let fixture = Fixture::new("adopted-root")?;
    let adopted = fixture.parent.join("adopted-home");
    private_dir(&adopted)?;
    let database = seed_learnings(&adopted)?;
    let before = std::fs::read(&database)?;

    let root = IsolatedFixtureRoot::identify(&adopted)?;
    let refusal = AdditiveFixtureGrant::in_fixture(&root);
    assert!(
        matches!(refusal, Err(CapabilityError::RootNotFreshlyCreated)),
        "a grant was minted over a root this process only adopted: {refusal:?}",
    );
    assert_eq!(
        std::fs::read(&database)?,
        before,
        "the adopted store was modified by a refused mint",
    );
    Ok(())
}

#[test]
fn a_root_replaced_by_a_real_directory_between_minting_and_use_is_refused() -> TestResult {
    // The second half of the same TOCTOU. The symlink subcase above is caught
    // by canonicalization; this one is not. The real root is moved aside and a
    // *real* directory takes its exact name, so the grant's path canonicalizes
    // to the same string it did at minting. Only the `(dev, ino)` recorded at
    // mint time can tell the two directories apart.
    let fixture = Fixture::new("toctou-real-dir")?;
    let key = project_key();
    seed_memory(fixture.path(), &key)?;
    seed_learnings(fixture.path())?;
    let grant = fixture.grant()?;
    let memory_appender = GoMemoryAppender::for_grant(&grant)?;
    let learning_appender = GoLearningAppender::for_grant(&grant)?;

    // Swap: the real tree moves aside, an impostor with the same Go shape
    // takes its place at the same name.
    let real = fixture.parent.join("real-tree");
    std::fs::rename(fixture.path(), &real)?;
    private_dir(fixture.path())?;
    seed_memory(fixture.path(), &key)?;
    seed_learnings(fixture.path())?;
    assert_eq!(
        std::fs::canonicalize(fixture.path())?,
        fixture.path(),
        "the swap must be invisible to canonicalization, or it proves nothing",
    );
    let planted = fixture.tree_digest()?;

    let memory_write = memory_appender.append_new(&grant, &key, &[entry("post-swap payload")]);
    assert!(
        matches!(memory_write, Err(GoAdapterError::GrantIdentityChanged)),
        "the memory appender wrote into a replaced root: {memory_write:?}",
    );
    let learning_write = learning_appender.insert_new(&grant, &[new_row("post-swap")]);
    assert!(
        matches!(learning_write, Err(GoAdapterError::GrantIdentityChanged)),
        "the learning appender wrote into a replaced root: {learning_write:?}",
    );
    assert_eq!(
        fixture.tree_digest()?,
        planted,
        "the planted tree was modified by a refused write",
    );

    // Restore so the fixture's own cleanup removes the real tree.
    std::fs::remove_dir_all(fixture.path())?;
    std::fs::rename(&real, fixture.path())?;
    Ok(())
}

#[test]
fn a_home_swapped_under_a_live_metrics_owner_is_caught_at_the_next_write() -> TestResult {
    let fixture = Fixture::new("toctou-metrics")?;
    let owner = MetricsOwner::assume(MetricsOwnerCapability::in_fixture_boundary(
        orchestrator_app::fixture_production_boundary(&fixture.root)?,
    )?)?;
    owner.record_terminal(&terminal_intent())?;

    let decoy = fixture.parent.join("decoy-home");
    private_dir(&decoy)?;
    let real = fixture.parent.join("real-home");
    std::fs::rename(fixture.path(), &real)?;
    std::os::unix::fs::symlink(&decoy, fixture.path())?;

    let write = owner.record_terminal(&terminal_intent());
    assert!(
        write.is_err(),
        "the owner kept writing after its home was replaced",
    );
    assert!(
        !decoy.join("metrics.db").exists(),
        "the decoy home received a metrics store",
    );

    std::fs::remove_file(fixture.path())?;
    std::fs::rename(&real, fixture.path())?;
    Ok(())
}

// ===========================================================================
// 3. Forged receipts and replays
// ===========================================================================

#[test]
fn a_forged_audit_receipt_does_not_recompute() -> TestResult {
    let fixture = Fixture::new("forged-receipt")?;
    let gateway = gateway()?;
    let mission = MissionId::new(MISSION)?;
    let chain = audit_chain();
    let mut store = open_fixture_runtime_store(&fixture.root)?;
    for step in 1..=3u32 {
        let (evidence, canonical) = admit(
            &gateway,
            &format!("entry-{step}"),
            &serde_json::json!({"n": step}),
        )?;
        chain.append(&mut store, &evidence, &canonical, AT)?;
    }
    let intact = chain.verify(&store, ChainRange::FromGenesis)?;
    assert!(matches!(intact, ChainVerification::Intact(_)), "{intact:?}");
    store.close()?;
    let _ = mission;

    // The adversary rewrites an entry's body *and* recomputes its payload
    // digest, so the row is internally consistent — exactly the forgery a naive
    // "does the digest match the payload" check would accept.
    let replacement = r#"{"n":99}"#;
    tamper(&fixture, |connection| {
        connection.execute(
            "UPDATE audit_chain SET payload_json = ?1, payload_digest = ?2 WHERE sequence = 2",
            rusqlite::params![replacement, sha256_hex(replacement)],
        )?;
        Ok(())
    })?;

    let store = open_fixture_runtime_store(&fixture.root)?;
    let fault = chain.verify(&store, ChainRange::FromGenesis)?.fault();
    store.close()?;
    assert_eq!(
        fault,
        Some(ChainFault::ForgedLink { sequence: 2 }),
        "the forged receipt was accepted",
    );
    Ok(())
}

#[test]
fn a_checkpoint_from_another_chain_is_refused_rather_than_trusted() -> TestResult {
    // A `VerifiedChainHead` is a receipt: it asserts "everything up to here was
    // checked". Replaying one against a shorter chain would let an adversary
    // skip verification of the entries that chain actually holds.
    let donor = Fixture::new("checkpoint-donor")?;
    let gateway = gateway()?;
    let chain = audit_chain();
    let mut store = open_fixture_runtime_store(&donor.root)?;
    for step in 1..=4u32 {
        let (evidence, canonical) = admit(
            &gateway,
            &format!("donor-{step}"),
            &serde_json::json!({"n": step}),
        )?;
        chain.append(&mut store, &evidence, &canonical, AT)?;
    }
    let head = chain
        .verify(&store, ChainRange::FromGenesis)?
        .head()
        .ok_or("an intact chain must yield a head")?;
    assert_eq!(head.sequence(), 4);
    store.close()?;

    let target = Fixture::new("checkpoint-target")?;
    let mut store = open_fixture_runtime_store(&target.root)?;
    let (evidence, canonical) = admit(&gateway, "target-1", &serde_json::json!({"n": 1}))?;
    chain.append(&mut store, &evidence, &canonical, AT)?;
    let replayed = chain.verify(&store, ChainRange::FromCheckpoint(head));
    store.close()?;
    assert!(
        matches!(replayed, Err(AuditChainError::CheckpointBeyondHead)),
        "a checkpoint past the head was accepted: {replayed:?}",
    );
    Ok(())
}

#[test]
fn an_in_place_rewrite_cannot_pass_as_an_append() -> TestResult {
    // The additive-write contract is "the earlier bytes are still there". A
    // digest over the whole file would flag any change; the prefix digest is
    // what distinguishes an append from an edit, and an adversary rewriting
    // history while *growing* the file is the case that separates them.
    let fixture = Fixture::new("forged-append")?;
    let key = project_key();
    let directory = seed_memory(fixture.path(), &key)?;
    let reader = GoMemoryAdapter::in_fixture(&fixture.root)?;
    let grant = fixture.grant()?;
    let appender = GoMemoryAppender::for_grant(&grant)?;

    let before = reader.file_digest(&key, MemoryFileKind::New, None)?;
    appender.append_new(&grant, &key, &[entry("an honest append")])?;
    let honest = reader.file_digest(&key, MemoryFileKind::New, Some(before.byte_len()))?;
    assert!(
        honest.is_append_of(&before),
        "an honest append failed its own receipt",
    );

    // Now rewrite the first line and add a fourth, so the file is strictly
    // longer than it was — a length check alone would call this an append.
    std::fs::write(
        directory.join("MEMORY_NEW.md"),
        "- REWRITTEN HISTORY\n- an honest append\n- and one more line\n",
    )?;
    let forged = reader.file_digest(&key, MemoryFileKind::New, Some(before.byte_len()))?;
    assert!(
        !forged.is_append_of(&before),
        "a rewrite that grows the file passed as an append",
    );
    Ok(())
}

#[test]
fn a_replayed_publication_key_is_refused_rather_than_applied_twice() -> TestResult {
    let fixture = Fixture::new("replayed-key")?;
    let gateway = gateway()?;
    let mission = MissionId::new(MISSION)?;
    let (evidence, canonical) = admit(&gateway, "replay", &serde_json::json!({"n": 1}))?;

    let first = gateway.publication_intent(&mission, Some("phase-1"), 1, &evidence, &canonical)?;
    let second = gateway.publication_intent(&mission, Some("phase-1"), 1, &evidence, &canonical)?;
    assert_eq!(
        first.idempotency_key(),
        second.idempotency_key(),
        "the key must be derived, not supplied, or replay detection is vacuous",
    );

    // Same key twice inside one transition: refused at intent assembly.
    let attached = gateway.attach(transition("replay-1", &mission)?, first)?;
    let replayed = gateway.attach(attached, second);
    assert!(
        replayed.is_err(),
        "a transition accepted the same publication key twice",
    );

    let mut store = open_fixture_runtime_store(&fixture.root)?;
    let once = gateway.publication_intent(&mission, Some("phase-1"), 1, &evidence, &canonical)?;
    store.append(&gateway.attach(transition("replay-1", &mission)?, once)?)?;

    // Replaying the *whole* transition is absorbed, not applied twice: this is
    // the crash-and-retry path, where the retry must be a no-op rather than a
    // second effect.
    let retry = gateway.publication_intent(&mission, Some("phase-1"), 1, &evidence, &canonical)?;
    store.append(&gateway.attach(transition("replay-1", &mission)?, retry)?)?;
    assert_eq!(
        store.publication_counts()?.pending,
        1,
        "a replayed transition enqueued its publication a second time",
    );

    // A *different* transition carrying the same publication key is the forgery
    // rather than the retry, and the store's UNIQUE index refuses it.
    let smuggled =
        gateway.publication_intent(&mission, Some("phase-1"), 1, &evidence, &canonical)?;
    let smuggle = store.append(&gateway.attach(transition("replay-2", &mission)?, smuggled)?);
    assert!(
        smuggle.is_err(),
        "a second transition re-enqueued an already-claimed publication key",
    );
    store.close()?;
    Ok(())
}

fn tamper(
    fixture: &Fixture,
    edit: impl FnOnce(&rusqlite::Connection) -> Result<(), Box<dyn std::error::Error>>,
) -> TestResult {
    let connection = rusqlite::Connection::open(fixture.path().join("runtime.db"))?;
    // The append-only triggers are the *first* line of defence and are proved
    // separately in Gate 2. Dropping them here is what lets this gate reach the
    // question it exists to answer: if an adversary gets past the triggers, do
    // the digests still catch the forgery?
    //
    // They are captured and reinstated afterwards because the store validates
    // its own schema on open: leaving them dropped would make the next open
    // fail as a corrupt database, and the gate would "pass" for the wrong
    // reason without ever re-reading the forged row.
    let bodies: Vec<String> = {
        let mut statement = connection.prepare(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger'
               AND name IN ('audit_chain_no_update', 'audit_chain_no_delete')
             ORDER BY name",
        )?;
        statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    assert_eq!(bodies.len(), 2, "both append-only triggers must exist");
    connection
        .execute_batch("DROP TRIGGER audit_chain_no_update; DROP TRIGGER audit_chain_no_delete;")?;
    edit(&connection)?;
    for body in bodies {
        connection.execute_batch(&body)?;
    }
    drop(connection);
    Ok(())
}

fn sha256_hex(text: &str) -> String {
    use sha2::Digest as _;
    let mut digest = sha2::Sha256::new();
    digest.update(text.as_bytes());
    hex(digest.finalize().as_slice())
}

// ===========================================================================
// 4. Outside-root process and network
// ===========================================================================

#[test]
fn the_b3_surface_spawns_nothing_and_connects_nowhere() -> TestResult {
    use std::process::{Command, Stdio};

    let fixture = Fixture::new("process-network")?;
    let key = project_key();
    seed_learnings(fixture.path())?;
    seed_memory(fixture.path(), &key)?;

    // A `PATH` that contains nothing but traps. Every executable a Go-era code
    // path might reach for records its own execution and exits non-zero, so a
    // spawn is a *file on disk*, not an absence someone has to notice.
    let trap_bin = fixture.parent.join("trap-bin");
    private_dir(&trap_bin)?;
    let marker = fixture.parent.join("trap-fired");
    for tool in [
        "sh",
        "bash",
        "sqlite3",
        "git",
        "go",
        "orchestrator",
        "claude",
        "curl",
        "python3",
        "env",
    ] {
        let script = trap_bin.join(tool);
        std::fs::write(
            &script,
            format!("#!/bin/sh\necho {tool} >> {}\nexit 97\n", marker.display()),
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))?;
        }
    }

    // A listener the helper is invited to connect to, through every proxy and
    // base-URL variable a provider client would honour.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address: SocketAddr = listener.local_addr()?;
    listener.set_nonblocking(true)?;
    let connections = Arc::new(AtomicUsize::new(0));
    let endpoint = format!("http://{address}");

    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "helper_entrypoint", "--nocapture"])
        .env(HELPER_MODE, "exercise-surface")
        .env(HELPER_ROOT, fixture.path())
        .env("PATH", &trap_bin)
        .env("HTTP_PROXY", &endpoint)
        .env("HTTPS_PROXY", &endpoint)
        .env("ALL_PROXY", &endpoint)
        .env("ANTHROPIC_BASE_URL", &endpoint)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    scrub_provider_environment(&mut command);
    let output = command.output()?;

    // Drain whatever the listener accepted before judging.
    for stream in listener.incoming() {
        match stream {
            Ok(_) => {
                connections.fetch_add(1, Ordering::Relaxed);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(error) => return Err(error.into()),
        }
    }

    assert!(
        output.status.success(),
        "the helper could not exercise the surface: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert!(
        !marker.exists(),
        "the B3 surface executed something: {:?}",
        std::fs::read_to_string(&marker).unwrap_or_default(),
    );
    assert_eq!(
        connections.load(Ordering::Relaxed),
        0,
        "the B3 surface opened an outbound connection",
    );
    Ok(())
}

#[test]
fn no_b3_module_names_a_process_or_socket_api() -> TestResult {
    // The runtime probe above proves nothing *did* spawn or connect on one
    // path through the surface. This proves nothing *can*, on any path, by
    // showing the capability is absent from the source.
    let forbidden = [
        "std::process",
        "Command::new",
        "std::net",
        "TcpStream",
        "TcpListener",
        "UdpSocket",
        "reqwest",
        "home_dir",
        "env::var",
    ];
    for module in b3_source_modules() {
        let (label, source) = read_source(&module)?;
        for needle in forbidden {
            assert!(
                !source.contains(needle),
                "{label} names {needle}; the B3 surface must reach neither the \
                 process table, the network, nor the ambient environment",
            );
        }
    }
    Ok(())
}

fn b3_source_modules() -> Vec<PathBuf> {
    let app = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let knowledge = Path::new(env!("CARGO_MANIFEST_DIR")).join("../orchestrator-knowledge/src");
    let mut modules: Vec<PathBuf> = [
        "knowledge_gateway.rs",
        "metrics_owner.rs",
        "metrics_query.rs",
        "audit_chain.rs",
        "capability.rs",
    ]
    .iter()
    .map(|name| app.join(name))
    .collect();
    for name in [
        "capability.rs",
        "envelope.rs",
        "error.rs",
        "evidence.rs",
        "identity.rs",
        "redaction.rs",
        "registry.rs",
    ] {
        modules.push(knowledge.join(name));
    }
    modules
}

fn read_source(path: &Path) -> TestResult<(String, String)> {
    let label = path
        .file_name()
        .map(std::ffi::OsStr::to_string_lossy)
        .unwrap_or_default()
        .into_owned();
    let source =
        std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok((label, source))
}

fn scrub_provider_environment(command: &mut std::process::Command) -> &mut std::process::Command {
    for name in SCRUBBED_PROVIDER_VARIABLES {
        command.env_remove(name);
    }
    command
}

// ===========================================================================
// 5. Secret-bearing values
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
fn a_secret_cannot_ride_an_additive_write_into_a_go_store() -> TestResult {
    // REGRESSION. Neither additive writer scanned its payload: a credential in
    // a `NewLearningRow` or a `MemoryEntry` was stored verbatim. B3-DESIGN §7
    // rule 2 puts the scan before durability precisely so the stored file never
    // has to be cleaned up afterwards.
    let fixture = Fixture::new("secret-additive")?;
    let key = project_key();
    seed_learnings(fixture.path())?;
    let directory = seed_memory(fixture.path(), &key)?;
    let grant = fixture.grant()?;
    let learning = GoLearningAppender::for_grant(&grant)?;
    let memory = GoMemoryAppender::for_grant(&grant)?;
    let reader = GoLearningAdapter::in_fixture(&fixture.root)?;
    let before = fixture.tree_digest()?;

    for (label, secret) in secret_corpus() {
        let mut row = new_row(&format!("secret-{label}"));
        row.content = format!("the provider replied {secret}");
        let refusal = learning.insert_new(&grant, &[row]);
        assert!(
            matches!(refusal, Err(GoAdapterError::SecretBearingPayload { .. })),
            "{label}: a credential entered learnings.db: {refusal:?}",
        );
        assert_no_leak(label, &secret, &refusal.err())?;

        let refusal = memory.append_new(&grant, &key, &[entry(&format!("noted {secret}"))]);
        assert!(
            matches!(refusal, Err(GoAdapterError::SecretBearingPayload { .. })),
            "{label}: a credential entered MEMORY_NEW.md: {refusal:?}",
        );
        assert_no_leak(label, &secret, &refusal.err())?;
    }

    // Positive control: the same shapes with clean content still land, so the
    // refusals are about content and not about the call being broken.
    learning.insert_new(&grant, &[new_row("clean-row")])?;
    memory.append_new(&grant, &key, &[entry("a clean note")])?;
    assert_eq!(reader.list(&list_query(8))?.rows.len(), 2);

    // Nothing but the two clean writes changed, and no stored byte carries a
    // secret.
    assert_ne!(
        fixture.tree_digest()?,
        before,
        "the controls must have written"
    );
    let stored = std::fs::read_to_string(directory.join("MEMORY_NEW.md"))?;
    for (_, secret) in secret_corpus() {
        for needle in secret.split_whitespace().filter(|part| part.len() >= 8) {
            assert!(!stored.contains(needle), "MEMORY_NEW.md carries {needle:?}");
        }
    }
    Ok(())
}

#[test]
fn a_secret_bearing_payload_is_refused_at_admission_and_never_becomes_durable() -> TestResult {
    let fixture = Fixture::new("secret-admission")?;
    open_fixture_runtime_store(&fixture.root)?.close()?;
    let gateway = gateway()?;

    for (label, secret) in secret_corpus() {
        let refusal = admit(&gateway, "secret", &serde_json::json!({"detail": secret}));
        let error = match refusal {
            Ok(_) => return Err(format!("{label} was admitted; it must be refused").into()),
            Err(error) => error,
        };
        assert!(
            matches!(error, KnowledgeError::SecretBearingPayload { .. }),
            "{label} was refused for the wrong reason: {error:?}",
        );
        assert_no_leak(label, &secret, &Some(error))?;
    }

    let store = open_fixture_runtime_store(&fixture.root)?;
    let counts = store.publication_counts()?;
    store.close()?;
    assert_eq!(
        (counts.pending, counts.delivered, counts.dead_letter),
        (0, 0, 0)
    );
    Ok(())
}

#[test]
fn the_redaction_wrapper_prints_a_placeholder_under_both_formatters() -> TestResult {
    // Rule 1 of §7 rests entirely on this type: `thiserror` renders `#[error]`
    // through `Display`, so a variant holding `Redacted<String>` is safe to log
    // with either `{}` or `{:?}`. If this ever stopped holding, every error
    // that carries context would start leaking it.
    for (label, secret) in secret_corpus() {
        let wrapped = Redacted::new(secret.clone());
        let rendered = format!("{wrapped}");
        let debugged = format!("{wrapped:?}");
        assert!(!rendered.contains(&secret), "{label}: Display leaked");
        assert!(!debugged.contains(&secret), "{label}: Debug leaked");
        assert!(rendered.contains("redacted"), "{label}: {rendered:?}");
        assert_eq!(
            wrapped.into_inner(),
            secret,
            "{label}: unwrapping must work"
        );
    }
    Ok(())
}

#[test]
fn the_scanner_names_the_family_and_the_replacement_keeps_the_context() -> TestResult {
    let redactor = Redactor::new();
    let kinds: BTreeMap<&str, SecretKind> = secret_corpus()
        .into_iter()
        .filter_map(|(label, secret)| match redactor.scan(&secret) {
            SecretVerdict::Bearing(kind) => Some((label, kind)),
            SecretVerdict::Clean => None,
        })
        .collect();
    assert_eq!(
        kinds.len(),
        secret_corpus().len(),
        "a denylist family stopped matching: {kinds:?}",
    );

    let secret = format!("sk-{}", "z".repeat(40));
    let redacted = redactor.redact(&format!("provider refused: {secret} was rejected"));
    assert!(!redacted.contains(&secret), "{redacted:?}");
    assert!(redacted.contains("[redacted:"), "{redacted:?}");
    assert!(redacted.contains("provider refused"), "{redacted:?}");
    assert!(redacted.contains("was rejected"), "{redacted:?}");

    // A clean string must survive untouched, or "redaction happened" carries no
    // information.
    let clean = "the mission completed after two retries";
    assert_eq!(redactor.redact(clean), clean);
    assert!(matches!(redactor.scan(clean), SecretVerdict::Clean));
    Ok(())
}

#[test]
fn an_egress_above_the_capability_ceiling_is_refused() -> TestResult {
    // The last of the three §7 rules. Admission and errors are covered above;
    // this is the read side, where a result classified above what the holder
    // may receive must not leave the gateway.
    let gateway = gateway()?;
    let (evidence, _) = admit(&gateway, "egress", &serde_json::json!({"n": 1}))?;
    gateway.admit_egress(&evidence, Sensitivity::Internal)?;
    let refused = gateway.admit_egress(&evidence, Sensitivity::Secret);
    assert!(
        matches!(refused, Err(KnowledgeError::SensitivityAboveCeiling { .. })),
        "a result above the ceiling escaped the gateway: {refused:?}",
    );
    Ok(())
}

/// Asserts an error prints no fragment of the secret under either formatter.
fn assert_no_leak(
    label: &str,
    secret: &str,
    error: &Option<impl std::fmt::Display + std::fmt::Debug>,
) -> TestResult {
    let Some(error) = error else {
        return Err(format!("{label}: expected an error to inspect").into());
    };
    let rendered = format!("{error}");
    let debugged = format!("{error:?}");
    for needle in secret.split_whitespace().filter(|part| part.len() >= 8) {
        assert!(
            !rendered.contains(needle),
            "{label}: Display leaked {needle:?} in {rendered:?}",
        );
        assert!(
            !debugged.contains(needle),
            "{label}: Debug leaked {needle:?} in {debugged:?}",
        );
    }
    Ok(())
}

// ===========================================================================
// 6. Hermetic fixture audit of every B3 gate
// ===========================================================================

/// Every gate file B3 adds, relative to this crate's manifest directory.
const B3_GATE_FILES: &[&str] = &[
    "tests/knowledge_plane_boundary_e2e.rs",
    "tests/metrics_audit_security_e2e.rs",
    "tests/sqlite_compat_matrix.rs",
    "tests/verifier_process_metrics_e2e.rs",
    "tests/authority_adversarial_e2e.rs",
    "../orchestrator-cli/tests/analytics_go_fixture_matrix.rs",
    "../orchestrator-cli/tests/learning_hooks_dry_run_matrix.rs",
];

/// The B3 gates permitted to read the operator's `HOME`, and why.
///
/// - `knowledge_plane_boundary_e2e.rs` reads it only to assert the *negative*:
///   that its fixture root is not inside the live home. Reading a value in
///   order to prove nothing was reached is the opposite of touching it.
/// - `learning_hooks_dry_run_matrix.rs` reads it to *locate* the accepted Go
///   binary that serves as its oracle. The binary it finds is executed with
///   `env_clear()` and a `HOME` pointing at a fixture, so no live data is
///   reachable from the child.
///
/// Pinning the set rather than banning the read is deliberate: a future gate
/// that starts reading `HOME` fails this audit and has to justify itself here.
const GATES_ALLOWED_TO_READ_HOME: &[&str] = &[
    "knowledge_plane_boundary_e2e.rs",
    "learning_hooks_dry_run_matrix.rs",
];

#[test]
fn no_b3_gate_reads_a_live_home_or_names_a_credential() -> TestResult {
    let mut readers_of_home = BTreeSet::new();
    for relative in B3_GATE_FILES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let (label, source) = read_source(&path)?;

        // No absolute path into a real user's tree, however it is spelled.
        //
        // The one exception is a `ProjectKey::encode` argument. That function
        // is a pure string transform that maps every `/` and `.` to `-`, so
        // such a literal is *data being destroyed*, never a path being opened —
        // `a_project_key_cannot_carry_a_separator_into_the_go_layout` proves
        // exactly that. Any other spelling is refused.
        // Assembled, not written, so the table does not match itself.
        let live_tree_spellings = [
            format!("/{}/", "Users"),
            format!("/{}/", "home"),
            format!("dirs::{}_dir", "home"),
            format!("{}::home_dir", "home"),
        ];
        for line in source.lines() {
            for needle in &live_tree_spellings {
                if !line.contains(needle.as_str()) {
                    continue;
                }
                assert!(
                    line.contains("ProjectKey::encode"),
                    "{label} names {needle} outside a project-key encoding: {line:?}",
                );
            }
        }

        // Provider credentials may be *scrubbed* by name and mentioned nowhere
        // else. A gate that reads one is a gate that could pass one on.
        for line in source.lines() {
            let names_credential = SCRUBBED_PROVIDER_VARIABLES
                .iter()
                .any(|variable| line.contains(variable));
            if !names_credential {
                continue;
            }
            assert!(
                line.trim_start().starts_with('"') || line.contains("env_remove"),
                "{label} uses a provider credential outside a scrub list: {line:?}",
            );
        }

        if source.contains("var_os(\"HOME\")") || source.contains("var(\"HOME\")") {
            readers_of_home.insert(label.clone());
        }

        // Any gate that spawns must scrub the credential environment it hands
        // the child.
        if source.contains("Command::new") {
            assert!(
                source.contains("env_clear") || source.contains("scrub_provider_environment"),
                "{label} spawns a subprocess without scrubbing provider credentials",
            );
        }
    }

    let expected: BTreeSet<String> = GATES_ALLOWED_TO_READ_HOME
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(
        readers_of_home, expected,
        "the set of B3 gates reading the operator's HOME changed",
    );
    Ok(())
}

#[test]
fn no_b3_gate_opens_a_socket_except_this_ones_trap_listener() -> TestResult {
    for relative in B3_GATE_FILES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
        let (label, source) = read_source(&path)?;
        // Needles are assembled rather than written, so this file does not
        // match its own audit table and quietly report itself.
        let connect = format!("Tcp{}::{}", "Stream", "connect");
        if label == "authority_adversarial_e2e.rs" {
            // This gate binds a loopback listener on purpose, and asserts it
            // accepted nothing. It must not be able to *connect* anywhere.
            assert!(!source.contains(&connect), "{label} connects outbound");
            continue;
        }
        for needle in [
            format!("std::{}", "net"),
            format!("Tcp{}", "Stream"),
            format!("Tcp{}", "Listener"),
            format!("Udp{}", "Socket"),
            "reqwest".to_owned(),
        ] {
            assert!(!source.contains(&needle), "{label} names {needle}");
        }
    }
    Ok(())
}

#[test]
fn every_fixture_this_gate_creates_is_a_private_directory_under_the_temp_root() -> TestResult {
    let temporary = std::fs::canonicalize(std::env::temp_dir())?;
    let fixture = Fixture::new("hermetic-self-check")?;
    assert!(
        fixture.path().starts_with(&temporary),
        "a fixture escaped the temp root: {}",
        fixture.path().display(),
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for directory in [&fixture.parent, &fixture.path().to_path_buf()] {
            let mode = std::fs::metadata(directory)?.permissions().mode() & 0o777;
            assert_eq!(
                mode,
                0o700,
                "{} is not private: {mode:o}",
                directory.display(),
            );
        }
    }
    // A fresh root starts empty: nothing is inherited from a previous run, so
    // no subcase can be reading state another one left behind.
    assert!(std::fs::read_dir(fixture.path())?.next().is_none());
    Ok(())
}

#[test]
fn the_helper_subprocess_sees_no_provider_credential() -> TestResult {
    // `scrub_provider_environment` is only useful if it is actually applied, so
    // this reads back the child's own view rather than trusting the builder.
    use std::process::{Command, Stdio};
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--exact", "environment_report_entrypoint", "--nocapture"])
        .env("NANIKA_B3_ADVERSARY_GATE_REPORT", "1")
        .stdout(Stdio::piped());
    // Plant a value for every scrubbed variable, so a scrub that silently does
    // nothing cannot pass by the variable happening to be unset here.
    for name in SCRUBBED_PROVIDER_VARIABLES {
        command.env(name, "planted-value");
    }
    scrub_provider_environment(&mut command);
    let output = command.output()?;
    let reported = String::from_utf8_lossy(&output.stdout);
    let mut leaked = Vec::new();
    for line in reported.lines() {
        if let Some(name) = line.strip_prefix("PRESENT ") {
            leaked.push(name.to_owned());
        }
    }
    assert!(
        leaked.is_empty(),
        "the helper inherited provider credentials: {leaked:?}",
    );
    assert!(
        reported.contains("REPORT-COMPLETE"),
        "the reporter did not run: {reported:?}",
    );
    Ok(())
}

/// Prints which scrubbed variables survived into this process.
#[test]
fn environment_report_entrypoint() {
    if std::env::var("NANIKA_B3_ADVERSARY_GATE_REPORT").is_err() {
        return;
    }
    let mut out = std::io::stdout();
    for name in SCRUBBED_PROVIDER_VARIABLES {
        if std::env::var_os(name).is_some() {
            let _ = writeln!(out, "PRESENT {name}");
        }
    }
    let _ = writeln!(out, "REPORT-COMPLETE");
    let _ = out.flush();
    std::process::exit(0);
}
