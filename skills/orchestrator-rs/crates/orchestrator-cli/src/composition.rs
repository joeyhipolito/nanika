//! The native CLI composition root (B5-DESIGN §1).
//!
//! One constructor, [`seal`], assembles every service one run needs. Seal 1 is
//! the read-only load that `--dry-run` and `--offline` have always used; seals
//! 2-13 need authority a shipped binary cannot present, so they are reached
//! through the [`FixtureEnrollment`] parameter and are compiled out entirely
//! without `test-support`. `run_system` passes `None`.

use orchestrator_app::{
    AppKnowledgeGateway, CancellationToken, DeniedGitMutationService, DirectoryProbe,
    EvidenceReconciler, GitEffectCapability, GitEffectError, GitEffectLedger, GitEffectService,
    HomeInputs, HomeSelection, PublicationDrain, ReadCapability, ResolvedRuntimeHome,
    RoutingDispatchError, RuntimeHomeResolver, RuntimeStoreError, UnenrolledMetricsQueryService,
    load_routing_map, publication_drain, routing_map_for_run,
};
use orchestrator_core::{MissionId, PersonaCatalog, PhaseId, RuntimePolicyFixture};
use orchestrator_exec::{AttemptOutcome, ExecutorRegistry, ServiceContractError};
use orchestrator_git::PrAdapter;
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};
use thiserror::Error;

use crate::run::{
    EnvInputs, GitRunError, GitRunPlan, GitRunReceipts, ResolvedPhase, ResolvedRun,
    RunExecutionError, RunFlags, RunResolutionContext,
};

#[cfg(not(any(test, feature = "test-support")))]
use std::marker::PhantomData;

// Seals 2-13 are no longer compiled out. B5-DESIGN §7's isolated-home door
// reaches them from a build with no `test-support`, which is the whole point of
// it: the campaign's claim is that the *shipped* binary enrolled. What still
// gates the fixture half is the three doors below and `FixtureEnrollment`
// itself, which stays uninhabited without the feature — so the fixture branch
// remains structurally unreachable in a shipped binary even though the seals it
// would have driven now exist there.
use orchestrator_app::{
    CanonicalEventLog, ClaimedPublication, DeliveryVerdict, DispatchDecisionRequest, DrainReport,
    FixtureArtifactEffectService, FixtureEvidenceVerifier, FixtureProcessAuthority,
    FixtureProcessService, FreshFixtureAuthority, IsolatedHomeGuards, MetricsOwner,
    PhaseMetricIntent, ProductionBoundary, RoutingDispatchOutcome, RuntimeStore, SupervisorLimits,
    TerminalMetricIntent, WorkspaceAuthority, WorkspaceSeed, decide_dispatch_route,
};
#[cfg(any(test, feature = "test-support"))]
use orchestrator_app::{
    IsolatedFixtureRoot, MetricsOwnerCapability, fixture_production_boundary,
    open_fixture_runtime_store,
};
use orchestrator_core::{
    CheckpointPhase, CheckpointPlan, CheckpointProjection, EventJsonMap, EventRecord,
    encode_current_event,
};
use orchestrator_daemon::DaemonClient;
use orchestrator_exec::{
    AttemptEvidence, Clock, DispatchRequest, EffectBudget, EffectReceipt, EffectRequest,
    EffectService, EffectServiceError, EffectServiceErrorKind, EventReceipt, EventSink,
    EventSinkError, EventSinkErrorKind, ExecutionContext, MechanicalTermination, PartialWork,
    PhaseExecutor, ProcessPurpose, ProcessRequest, RuntimeCaps, RuntimeDescriptor, RuntimeFamily,
    WatchdogDecision, WatchdogPolicy, WorkerEventDraft, WorkerIdentity,
};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Failures while assembling read-only run inputs from the host.
#[derive(Debug, Error)]
pub enum CompositionError {
    /// The process has no usable user-home input.
    #[error("HOME is unavailable")]
    MissingUserHome,
    /// Runtime-home precedence could not be resolved.
    #[error(transparent)]
    RuntimeHome(#[from] orchestrator_app::ApplicationError),
    /// The persona catalog could not be inspected.
    #[error("cannot inspect the persona catalog: {0}")]
    Personas(#[source] io::Error),
    /// One of B5-DESIGN §1.1's seals 2-12 refused the authority it was handed.
    ///
    /// The seal is named precisely enough to locate the failure — "attested
    /// helper" and "mission workspace" are separate names for separate steps
    /// of seal 5 — but the authority, the path, and the underlying error text
    /// are deliberately not retained, because every one of them describes a
    /// disposable fixture root the operator did not choose.
    #[error("the composition root could not seal the {0}")]
    Seal(&'static str),
    /// An enrollment named the live provider runtime family.
    ///
    /// There is no registration site for it anywhere in the tree, and this is
    /// the refusal that keeps it that way.
    #[error("the live provider runtime family is never enrolled")]
    LiveProviderRefused,
    /// The governed Git seam refused its capability or repository root.
    #[error(transparent)]
    GitEffect(#[from] GitEffectError),
    /// B5-DESIGN §7's isolated-home door refused. The message names the row.
    #[error("the isolated-home door refused: {0}")]
    IsolatedHome(String),
}

pub(crate) struct SystemRunContext {
    pub(crate) resolution: RunResolutionContext,
    pub(crate) warnings: Vec<String>,
    /// Seal 1's precedence rule.
    pub(crate) home: HomeSelection,
    /// Seal 1's resolved runtime-home root. Resolution grants no write access.
    pub(crate) home_path: PathBuf,
    /// Seal 1's resolution itself, retained so B5-DESIGN §7's door can consume
    /// it. `ResolvedRuntimeHome` is the type whose whole contract is that it
    /// grants nothing until an `authorize_*` arm accepts it, so carrying it
    /// here adds no authority — it is the same value seal 1 already produced.
    pub(crate) isolated_candidate: Option<ResolvedRuntimeHome>,
    /// The `HOME` seal 1 resolved against, so §7's D3 guards name the same
    /// user home the precedence rules did.
    pub(crate) user_home: PathBuf,
}

/// Loads only read-side configuration. It does not authorize or prepare a
/// runtime home and therefore cannot mutate live state.
pub(crate) fn load(flags: &RunFlags) -> Result<SystemRunContext, CompositionError> {
    let user_home = required_path("HOME")?;
    let inputs = HomeInputs {
        user_home: user_home.clone(),
        orchestrator_config_dir: optional_path("ORCHESTRATOR_CONFIG_DIR"),
        alluka_home: optional_path("ALLUKA_HOME"),
        via_home: optional_path("VIA_HOME"),
    };
    let home = RuntimeHomeResolver::resolve(&inputs, &FilesystemProbe)?;
    let (routing, warning) = routing_map_for_run(
        load_routing_map(&home, &FilesystemReader),
        flags.persistent.verbose,
    );
    let personas_dir = flags
        .persistent
        .personas_dir
        .clone()
        .or_else(|| optional_path("ORCHESTRATOR_PERSONAS_DIR"))
        .unwrap_or_else(|| user_home.join("nanika/personas"));
    let personas = load_persona_names(&personas_dir)?;
    let warnings = warning.into_iter().map(|warning| warning.message).collect();

    Ok(SystemRunContext {
        resolution: RunResolutionContext {
            environment: EnvInputs::from_environment(),
            user_home: Some(user_home.clone()),
            personas,
            routing,
            policy: RuntimePolicyFixture::default(),
            target: None,
        },
        warnings,
        home: home.selection(),
        home_path: home.path().to_path_buf(),
        isolated_candidate: Some(home),
        user_home,
    })
}

/// Resolves only the runtime-home root path, without loading persona/routing
/// state. Used by home-backed inspection commands (`status` and `events`) that
/// need a directory but not the full run-composition inputs `load` assembles.
pub(crate) fn resolve_home_path() -> Result<PathBuf, CompositionError> {
    if let Some(path) = explicit_home_path() {
        return Ok(path);
    }

    let user_home = required_path("HOME")?;
    let inputs = HomeInputs {
        user_home,
        orchestrator_config_dir: None,
        alluka_home: None,
        via_home: None,
    };
    let home = RuntimeHomeResolver::resolve(&inputs, &FilesystemProbe)?;
    Ok(home.path().to_path_buf())
}

// ---------------------------------------------------------------------------
// B4-DESIGN §1.5, §6.2 — governed Git composition
// ---------------------------------------------------------------------------

/// The effect service every worker's `ExecutionContext` receives.
///
/// It refuses every request, `EffectKind::GitMutation` included. Constructing
/// it performs no filesystem access and grants nothing; it exists so the
/// worker's authority to mutate a repository is `None` by construction rather
/// than by a policy check somewhere downstream.
///
/// # Errors
/// Returns [`ServiceContractError`] only if the fixed refusal message were
/// rejected by the effect contract, which it is not.
pub fn worker_effect_service() -> Result<DeniedGitMutationService, ServiceContractError> {
    DeniedGitMutationService::new()
}

/// Assembles the governed-Git run seam for one mission phase.
///
/// This is the composition root's only Git assembly point, and it is
/// **fixture-shaped by construction**: [`GitEffectCapability`] has no
/// constructor that accepts a filesystem path and no production mint at all,
/// and `GitEffectLedger::open` is compiled out of every production build, so a
/// caller that cannot present a disposable `IsolatedFixtureRoot` can obtain
/// neither and therefore cannot reach this function with a live repository.
///
/// [`seal`] is its only caller in `src/`, and it reaches it only through
/// `FixtureEnrollment::git`. A run the shipped binary performs is sealed with
/// `None`, so no Git service is assembled at all and the run is refused with
/// `CliError::ExecutionNotEnrolled` before any phase is dispatched.
///
/// # Errors
/// Returns [`GitEffectError`] when the capability cannot be verified or the
/// repository root is not inside it.
pub fn governed_git_run<'enrolment>(
    ledger: GitEffectLedger,
    capability: &'enrolment GitEffectCapability,
    adapter: &'enrolment dyn PrAdapter,
    mission: MissionId,
    phase: PhaseId,
    repo_root: PathBuf,
) -> Result<GitEffectService<'enrolment>, GitEffectError> {
    GitEffectService::new(ledger, capability, adapter, mission, phase, repo_root)
}

/// Returns the fail-closed metrics service until an owner-mediated protocol
/// or cooperative cross-language read lease is enrolled. Constructing this
/// service performs no runtime-home resolution or filesystem access.
pub(crate) const fn metrics_query_service() -> UnenrolledMetricsQueryService {
    UnenrolledMetricsQueryService
}

fn explicit_home_path() -> Option<PathBuf> {
    optional_path("ORCHESTRATOR_CONFIG_DIR")
        .or_else(|| optional_path("ALLUKA_HOME"))
        .or_else(|| optional_path("VIA_HOME").map(|path| path.join("orchestrator")))
}

fn required_path(name: &'static str) -> Result<PathBuf, CompositionError> {
    optional_path(name).ok_or(CompositionError::MissingUserHome)
}

fn optional_path(name: &'static str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn load_persona_names(directory: &Path) -> Result<PersonaCatalog, CompositionError> {
    let mut catalog = PersonaCatalog::default();
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(catalog),
        Err(error) => return Err(CompositionError::Personas(error)),
    };
    for entry in entries {
        let entry = entry.map_err(CompositionError::Personas)?;
        let file_type = entry.file_type().map_err(CompositionError::Personas)?;
        let name = entry.file_name();
        if file_type.is_file()
            && Path::new(&name)
                .extension()
                .is_some_and(|value| value == "md")
        {
            if let Some(stem) = Path::new(&name)
                .file_stem()
                .and_then(|value| value.to_str())
            {
                catalog.names.insert(stem.to_owned());
            }
            continue;
        }
        if file_type.is_dir() {
            let candidate = entry.path().join(&name).with_extension("md");
            if candidate.is_file() {
                if let Some(name) = name.to_str() {
                    catalog.names.insert(name.to_owned());
                }
            }
        }
    }
    Ok(catalog)
}

struct FilesystemProbe;

impl DirectoryProbe for FilesystemProbe {
    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }
}

struct FilesystemReader;

impl ReadCapability for FilesystemReader {
    fn read_relative(
        &self,
        home: &ResolvedRuntimeHome,
        relative: &Path,
    ) -> io::Result<Option<Vec<u8>>> {
        match fs::read(home.path().join(relative)) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

// ---------------------------------------------------------------------------
// B5-DESIGN §1 — the composition root
// ---------------------------------------------------------------------------

/// The runtime family the live provider would occupy.
///
/// `composition::seal` refuses to register an executor for it in any profile,
/// which is what keeps §2.1's right conjunct structural: a phase that resolves
/// to `claude` — every phase that names no runtime, and every phase whose
/// authored runtime is not in the Go-compatible supported set — finds no
/// registration and is [`ExecutionEnrollment::Unenrolled`].
pub const LIVE_PROVIDER_RUNTIME: &str = "claude";

/// Why the sealed root refused to run a phase.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnenrolledReason {
    /// `seal` was called with `None` — the shipped binary's only call.
    NoFixtureEnrollment,
    /// An enrollment exists but the phase's runtime resolves to no registered
    /// executor. This is `ExecutorRegistry::resolve` returning nothing, not a
    /// policy branch.
    NoExecutorForRuntime,
}

impl UnenrolledReason {
    /// A stable slug for reports and gate assertions.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoFixtureEnrollment => "no-fixture-enrollment",
            Self::NoExecutorForRuntime => "no-executor-for-runtime",
        }
    }
}

/// B5-DESIGN §2.1's resolution over the sealed home state and the sealed
/// registry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ExecutionEnrollment {
    /// The sealed registry resolves this runtime to an executor.
    Enrolled {
        /// The family the registry resolved directly.
        runtime: String,
    },
    /// Nothing runs. The runtime family that was refused is retained; no task
    /// text, path, or credential is.
    Unenrolled {
        /// The family that found no executor, or the family the phase asked
        /// for when no enrollment was supplied at all.
        runtime: String,
        /// Which conjunct of §2.1 failed.
        reason: UnenrolledReason,
    },
}

impl ExecutionEnrollment {
    /// Whether a phase with this resolution may be dispatched.
    #[must_use]
    pub const fn is_enrolled(&self) -> bool {
        matches!(self, Self::Enrolled { .. })
    }
}

/// Failures raised while driving a sealed run.
#[derive(Debug, Error)]
pub enum SealedRunError {
    /// The sealed root has no executor for a phase's runtime.
    #[error("phase {phase} resolves to runtime {runtime}, which is not enrolled")]
    NotEnrolled {
        /// The phase that could not be dispatched.
        phase: String,
        /// The runtime family that resolved to no executor.
        runtime: String,
    },
    /// The sealed root was built with `None`, so seals 2-13 do not exist.
    #[error("this run was sealed without a fixture enrollment")]
    Unenrolled,
    /// The resolved plan's `DEPENDS` edges cannot be linearised — a cycle, or
    /// an edge naming a phase the plan does not contain. Both are the same
    /// fault from here: there is no order in which every predecessor runs
    /// first, so nothing is dispatched.
    #[error("the resolved plan's dependencies cannot be ordered")]
    UnorderablePlan,
    /// The enrollment planned a Git effect for a phase this plan does not
    /// release, so the governed seam would never run. Failing here rather than
    /// silently dispatching every phase ungoverned is the difference between a
    /// mis-bound enrollment and a run that quietly skipped its Git effects.
    #[error("the enrolled Git effect binds phase {phase}, which this plan does not release")]
    GitPhaseNotReleased {
        /// The phase the sealed Git service is bound to.
        phase: String,
    },
    /// Dispatch itself failed.
    #[error(transparent)]
    Execution(#[from] RunExecutionError),
    /// A governed Git effect failed.
    #[error(transparent)]
    Git(#[from] GitRunError),
    /// The metrics owner refused a write.
    #[error("metrics owner refused a write: {0}")]
    Metrics(String),
    /// The sealed routing journal refused a dispatch decision.
    #[error(transparent)]
    Routing(#[from] RoutingDispatchError),
    /// The sealed runtime store refused a durable operation.
    #[error(transparent)]
    Store(#[from] RuntimeStoreError),
    /// The mission workspace refused a durable projection write.
    #[error("the mission workspace refused a projection write: {0}")]
    Workspace(String),
}

/// What one released phase produced.
///
/// An enum rather than an outcome plus an optional receipt bundle because
/// [`AttemptOutcome`] is not `Clone` and lives *inside* [`GitRunReceipts`] on
/// the governed branch — keeping both shapes in one value is what makes
/// [`Self::outcome`] total instead of fallible.
#[derive(Debug)]
pub enum SealedPhaseOutcome {
    /// `run::run_one_phase` — the phase planned no Git effect.
    Dispatched(Box<AttemptOutcome>),
    /// `run::run_one_phase_with_governed_git` — every receipt, in order.
    ///
    /// Boxed because the receipt bundle carries six `GitReceipt`s plus the
    /// attempt outcome; leaving it inline would make every `Dispatched` value
    /// pay for the governed branch's size.
    Governed(Box<GitRunReceipts>),
}

impl SealedPhaseOutcome {
    /// The terminal attempt outcome, whichever seam produced it.
    #[must_use]
    pub const fn outcome(&self) -> &AttemptOutcome {
        match self {
            Self::Dispatched(outcome) => outcome,
            Self::Governed(receipts) => &receipts.outcome,
        }
    }

    /// The governed Git receipts, when the phase planned an effect.
    #[must_use]
    pub const fn git(&self) -> Option<&GitRunReceipts> {
        match self {
            Self::Dispatched(_) => None,
            Self::Governed(receipts) => Some(receipts),
        }
    }
}

/// One phase's terminal result inside a sealed run.
#[derive(Debug)]
pub struct SealedPhaseRun {
    /// The phase that ran.
    pub phase: PhaseId,
    /// The runtime family the registry resolved.
    pub runtime: String,
    /// What the enforced dispatch seam produced.
    pub result: SealedPhaseOutcome,
}

/// Everything one sealed run produced, in phase order.
#[derive(Debug)]
pub struct SealedRunReport {
    /// The mission identity every row and receipt bound to.
    pub mission: MissionId,
    /// One entry per released phase, in dependency order.
    pub phases: Vec<SealedPhaseRun>,
}

/// The composition root's product: every seal of B5-DESIGN §1.1, plus the
/// executor registry and the enrollment resolution of §2.
///
/// **Field order is the reverse of the seal order and is load-bearing.** Rust
/// drops struct fields in declaration order, and seal 10's
/// [`MetricsOwner`] holds a kernel lease taken *under* seal 2's
/// [`ProductionBoundary`]. Declaring seal 2 before seal 10 would release the
/// boundary while the lease is still live. There is no type-level witness for
/// this, so `production_composition_e2e`'s case N4 fails if the fields are
/// reordered.
pub struct SealedRun<'enrolment> {
    // --- 13: shutdown -----------------------------------------------------
    cancellation: CancellationToken,
    // --- 12: verifier -----------------------------------------------------
    reconciler: EvidenceReconciler,
    verifier: Option<FixtureEvidenceVerifier>,
    artifact_effects: Option<FixtureArtifactEffectService>,
    // --- 11: daemon client ------------------------------------------------
    daemon: Option<DaemonClient>,
    // --- 10: metrics owner (its lease was taken under seal 2's boundary) ---
    metrics: Option<MetricsOwner>,
    // --- 9: knowledge -----------------------------------------------------
    drain: PublicationDrain,
    knowledge: Option<&'enrolment AppKnowledgeGateway>,
    // --- 8: governed Git --------------------------------------------------
    git: Option<GitEffectService<'enrolment>>,
    git_plan: Option<GitRunPlan>,
    /// The one phase seal 8's service is bound to. A governed run applies Git
    /// effects for this phase only; every other released phase goes through
    /// `run_one_phase`, because `GitEffectService` binds one mission/phase pair
    /// and replaying its slots under a second phase would journal another
    /// phase's work under this one's identity.
    git_phase: Option<PhaseId>,
    // --- 7: routing / usage multiplexer -----------------------------------
    //       The store *is* the journal; the routing map is seal 1's, and the
    //       store is handed out by scoped `&mut` borrow, never held as one.
    // --- 6: local fixture provider ----------------------------------------
    registry: ExecutorRegistry,
    provider: Option<FixtureProcessService>,
    // --- 5: process authority / installed helper --------------------------
    workspace: Option<WorkspaceAuthority>,
    worker_root: Option<PathBuf>,
    // --- 4: canonical event owner -----------------------------------------
    events: Option<CanonicalEventLog>,
    // --- 3: runtime store -------------------------------------------------
    store: Option<RuntimeStore>,
    // --- 2: writer authority ----------------------------------------------
    boundary: Option<Arc<ProductionBoundary>>,
    // --- B5-DESIGN §7: the isolated door's admitted authority -------------
    //     Declared after seal 2 so it drops *after* it: this value owns the
    //     admitted root's flock, and every seal above holds an `Arc` clone of
    //     the boundary that lock protects. It is retained rather than read —
    //     `#[expect]` says so, so a later reader does not "clean it up" and
    //     silently release the lock while the run is still writing.
    #[expect(dead_code, reason = "retained to hold the admitted root's lock")]
    isolated: Option<FreshFixtureAuthority>,
    // --- 1: home selection and read-only run inputs -----------------------
    home: HomeSelection,
    home_path: PathBuf,
    resolution: RunResolutionContext,
    warnings: Vec<String>,
    mission: Option<MissionId>,
    enrolled_runtime: Option<String>,
}

/// Seal 13, executed.
///
/// `Drop` runs before the fields' own drops, so this is where shutdown work
/// that must happen *while every seal is still alive* belongs. Two things do:
///
/// 1. Cancelling the token wakes every owned child the local fixture provider
///    still holds, so a run that ends early does not leave a process group
///    behind for the field drops to discover.
/// 2. `RuntimeStore::drop` releases its process-local writer lease only when
///    the store was cleanly closed, and retains it to process exit otherwise —
///    the right default for a store that died mid-write, and the wrong outcome
///    for a root that has finished with it. Closing here is what lets a second
///    `seal` over the same home succeed after the first one is gone, which is
///    the whole resume story (`production_composition_crash_e2e`).
///
/// The field declaration order still governs everything else: seal 10's
/// metrics lease is released before seal 2's boundary, and this method does
/// not touch either.
impl Drop for SealedRun<'_> {
    fn drop(&mut self) {
        let _first = self.cancellation.cancel();
        if let Some(store) = self.store.take() {
            // A close that fails leaves the lease retained, which is exactly
            // what a store that could not checkpoint should do. There is
            // nothing to report from a destructor and nothing safe to retry.
            let _retained = store.close();
        }
    }
}

impl fmt::Debug for SealedRun<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealedRun")
            .field("home", &self.home)
            .field("enrolled_runtime", &self.enrolled_runtime)
            .finish_non_exhaustive()
    }
}

impl SealedRun<'_> {
    /// The precedence rule that selected the runtime home (seal 1).
    #[must_use]
    pub const fn home(&self) -> &HomeSelection {
        &self.home
    }

    /// The resolved runtime-home root (seal 1).
    #[must_use]
    pub fn home_path(&self) -> &Path {
        &self.home_path
    }

    /// Read-only run inputs, the input to `run::resolve_with_context`.
    #[must_use]
    pub const fn resolution(&self) -> &RunResolutionContext {
        &self.resolution
    }

    /// Warnings seal 1 collected while loading the routing map.
    #[must_use]
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The sealed executor registry (seal 6).
    #[must_use]
    pub const fn registry(&self) -> &ExecutorRegistry {
        &self.registry
    }

    /// The shutdown token every owned child observes (seal 13).
    #[must_use]
    pub const fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    /// The evidence reconciler (seal 12).
    #[must_use]
    pub const fn reconciler(&self) -> &EvidenceReconciler {
        &self.reconciler
    }

    /// The loopback daemon client, when one could be opened (seal 11).
    #[must_use]
    pub const fn daemon(&self) -> Option<&DaemonClient> {
        self.daemon.as_ref()
    }

    /// The knowledge gateway this run was enrolled with (seal 9).
    #[must_use]
    pub const fn knowledge(&self) -> Option<&AppKnowledgeGateway> {
        self.knowledge
    }

    /// The mission identity every row and receipt binds to.
    #[must_use]
    pub const fn mission(&self) -> Option<&MissionId> {
        self.mission.as_ref()
    }

    /// Seal 9's durable half: the publication drain over the sealed store.
    #[must_use]
    pub const fn publication_drain(&self) -> &PublicationDrain {
        &self.drain
    }

    /// Seal 8's governed Git service, when the enrollment planned an effect.
    #[must_use]
    pub const fn git(&self) -> Option<&GitEffectService<'_>> {
        self.git.as_ref()
    }

    /// Seal 8's plan, when the enrollment planned an effect.
    #[must_use]
    pub const fn git_plan(&self) -> Option<&GitRunPlan> {
        self.git_plan.as_ref()
    }

    /// The one phase seal 8's Git service is bound to.
    #[must_use]
    pub const fn git_phase(&self) -> Option<&PhaseId> {
        self.git_phase.as_ref()
    }

    /// Seal 6: the root the local fixture provider spawns under.
    #[must_use]
    pub fn worker_root(&self) -> Option<&Path> {
        self.worker_root.as_deref()
    }

    /// Resolves B5-DESIGN §2.1's predicate for one runtime family.
    ///
    /// Both conjuncts are already structural: the left one is the sealed home
    /// state (seals 2-13 exist only under an admitted fixture authority), and
    /// the right one is [`ExecutorRegistry::resolve`]. Nothing here reads an
    /// environment variable or a flag.
    #[must_use]
    pub fn enrollment_for(&self, runtime: &str) -> ExecutionEnrollment {
        let requested = if runtime.is_empty() {
            LIVE_PROVIDER_RUNTIME
        } else {
            runtime
        };
        match self.enrolled_runtime.as_deref() {
            Some(enrolled) if enrolled == requested => ExecutionEnrollment::Enrolled {
                runtime: requested.to_owned(),
            },
            Some(_) => ExecutionEnrollment::Unenrolled {
                runtime: requested.to_owned(),
                reason: UnenrolledReason::NoExecutorForRuntime,
            },
            None => ExecutionEnrollment::Unenrolled {
                runtime: requested.to_owned(),
                reason: UnenrolledReason::NoFixtureEnrollment,
            },
        }
    }

    /// Resolves the enrollment for a whole plan: enrolled only when every
    /// released phase is.
    #[must_use]
    pub fn enrollment_for_plan(&self, resolved: &ResolvedRun) -> ExecutionEnrollment {
        if resolved.phases.is_empty() {
            return self.enrollment_for("");
        }
        for phase in &resolved.phases {
            let enrollment = self.enrollment_for(&phase.effective_runtime);
            if !enrollment.is_enrolled() {
                return enrollment;
            }
        }
        self.enrollment_for(&resolved.phases[0].effective_runtime)
    }
}

// ---------------------------------------------------------------------------
// §1.1 — the enrollment parameter
// ---------------------------------------------------------------------------

/// The authority seals 2-12 cannot mint for themselves.
///
/// In this profile the type is **uninhabited**: it has one private field whose
/// type has no variants, so no value of it can be constructed, `Option::Some`
/// of a reference to it can never be produced, and seals 2-13 are uncallable
/// rather than merely refused. That is B5-DESIGN §1.1's "the shipped binary is
/// unenrolled *structurally* rather than by a policy check".
#[cfg(not(any(test, feature = "test-support")))]
pub struct FixtureEnrollment<'enrolment> {
    never: NoFixtureDoor,
    lifetime: PhantomData<&'enrolment ()>,
}

/// Has no variants, so [`FixtureEnrollment`] has no values.
#[cfg(not(any(test, feature = "test-support")))]
enum NoFixtureDoor {}

/// The Git half of one enrollment. Its members are exactly
/// [`governed_git_run`]'s existing parameters, which is the precedent the
/// whole enrollment parameter follows.
#[cfg(any(test, feature = "test-support"))]
pub struct FixtureGitEnrollment<'enrolment> {
    /// Fixture-minted; [`GitEffectCapability`] has no production constructor.
    pub capability: &'enrolment GitEffectCapability,
    /// The offline pull-request adapter.
    pub adapter: &'enrolment dyn PrAdapter,
    /// The repository root, which must lie inside `capability`.
    pub repo_root: PathBuf,
    /// The parameters `run_one_phase_with_governed_git` needs beyond a phase.
    pub plan: GitRunPlan,
}

/// The exact artifact binding seal 12's verifier attests.
///
/// The bytes are pinned before the effect runs, so the verifier's attestation
/// is over a value the caller committed to rather than over whatever the
/// worker happened to write.
#[cfg(any(test, feature = "test-support"))]
pub struct FixtureEvidenceEnrollment<'enrolment> {
    /// One non-hidden ASCII filename component beneath the phase's artifact
    /// directory.
    pub artifact: &'enrolment str,
    /// The exact bytes the artifact must hold.
    pub expected: &'enrolment [u8],
}

/// One value carrying every authority seals 2-12 cannot mint for themselves
/// (B5-DESIGN §1.1).
///
/// It has **no production constructor**: the whole type is compiled out unless
/// `test-support` (or `cfg(test)`) is on, and every door its fields feed is
/// itself `#[cfg(any(test, feature = "test-support"))]` in `orchestrator-app`.
/// `seal` mints nothing; it consumes this and composes.
#[cfg(any(test, feature = "test-support"))]
pub struct FixtureEnrollment<'enrolment> {
    /// Seals 2, 3, 4, 9, 10, 12. An `identify`d handle on the same canonical
    /// path `authority` was admitted over. Seal 2 turns it into an
    /// `Arc<ProductionBoundary>` through `fixture_production_boundary`; seal 3
    /// opens the store through `open_fixture_runtime_store`. Both take an
    /// authority, never a path, so neither can be aimed at a live home.
    pub root: &'enrolment IsolatedFixtureRoot,
    /// Seals 4, 5, 6. `FreshFixtureAuthority::admit` over a `create_fresh`
    /// root, under a `FixtureAdmissionPolicy` built
    /// `with_expected_fixture_helper`.
    pub authority: &'enrolment FreshFixtureAuthority,
    /// Seal 5. The label the attested helper is installed under.
    pub helper_label: &'enrolment str,
    /// Seal 5. The exact bytes `install_fixture_executable` re-checks against
    /// the policy pin.
    pub helper_bytes: &'enrolment [u8],
    /// Seal 5. The argument vector the local fixture executor hands the
    /// installed helper.
    pub helper_arguments: &'enrolment [&'enrolment str],
    /// Seals 8, 10, 12. The identity every ledger row, metrics row and receipt
    /// binds to.
    pub mission: MissionId,
    /// Seals 8, 12. The phase identity artifacts and receipts bind to.
    pub phase: PhaseId,
    /// Seal 6. The runtime family the one local fixture executor is registered
    /// under.
    ///
    /// **Amendment to B5-DESIGN §1.1.** The design left this implicit as "the
    /// fixture runtime family", but `ResolvedPhase::effective_runtime` is
    /// clamped to the Go-compatible supported set by
    /// `orchestrator_core::resolve_runtime`, so an authored `RUNTIME:
    /// fixture-runtime` never survives resolution and no phase could ever
    /// match a family outside that set. The enrollment therefore names the
    /// family, and [`seal`] refuses [`LIVE_PROVIDER_RUNTIME`] outright — which
    /// is a stronger form of §2.1's "there is no `registry.register(
    /// claude_executor)` anywhere in the tree" than the design stated.
    pub runtime: &'enrolment str,
    /// Seal 8. `None` plans no Git effect.
    pub git: Option<FixtureGitEnrollment<'enrolment>>,
    /// Seal 9. The knowledge gateway, when the caller has one.
    ///
    /// **Amendment to B5-DESIGN §1.1.** `AppKnowledgeGateway::new` takes a
    /// `TypeRegistry` and a `KnowledgeCapability`, i.e. the namespace, the type
    /// manifests, and the operation grant — application policy B5 has no
    /// mandate to invent. The durable half of seal 9, `publication_drain()`,
    /// is unconditional and is always sealed; the gateway is a parameter for
    /// the same reason the Git capability is.
    pub knowledge: Option<&'enrolment AppKnowledgeGateway>,
    /// Seal 12. `None` seals the reconciler without an attested artifact.
    ///
    /// **Amendment to B5-DESIGN §1.1.** `FixtureArtifactEffectService::new`
    /// requires the exact expected bytes up front; a composition root cannot
    /// invent them.
    pub evidence: Option<FixtureEvidenceEnrollment<'enrolment>>,
}

// ---------------------------------------------------------------------------
// §1.1 — `seal`
// ---------------------------------------------------------------------------

/// **The** composition root.
///
/// Seal 1 ([`load`]) is unconditional: it resolves the home and the read-only
/// run inputs and authorizes nothing. Seals 2-13 require `Some`, and
/// `run_system` passes `None` — which is the entire production path. Because
/// [`FixtureEnrollment`] is uninhabited in a build without `test-support`,
/// seals 2-13 are not merely refused there; they are uncallable.
///
/// With `None` the returned [`SealedRun`] carries the seal-1 half and resolves
/// every runtime to [`ExecutionEnrollment::Unenrolled`] with
/// [`UnenrolledReason::NoFixtureEnrollment`]. That is not an error: `--dry-run`
/// and `--offline` consume exactly the seal-1 half and keep working unchanged.
/// Only the ordinary (non-dry, non-offline) path turns `Unenrolled` into
/// `CliError::ExecutionNotEnrolled`.
///
/// # Errors
/// Returns [`CompositionError`] when seal 1's home/persona/routing load fails,
/// when an enrollment names the live provider family, or when any of seals
/// 2-12 refuses the authority it was handed.
pub fn seal<'enrolment>(
    flags: &RunFlags,
    enrollment: Option<&'enrolment FixtureEnrollment<'enrolment>>,
) -> Result<SealedRun<'enrolment>, CompositionError> {
    // Seal 1 — home selection plus the read-only run inputs.
    let context = load(flags)?;
    match enrollment {
        None => seal_through_the_isolated_door(context),
        Some(enrollment) => seal_under_enrollment(context, enrollment),
    }
}

/// B5-DESIGN §7: the one door a shipped binary can enroll through.
///
/// It is tried only when no fixture enrollment was supplied — which is every
/// invocation of `run_system`. It refuses by default: without
/// `NANIKA_ISOLATED_HOME_ENROLL=1`, without a bundle manifest beside `argv[0]`,
/// or with any row of §7.3 unsatisfied, the returned run is the seal-1 half and
/// the ordinary path still exits `ExecutionNotEnrolled`. `run_not_enrolled`
/// therefore keeps passing unmodified: it sets neither the selector nor a
/// bundle.
///
/// A row that *refused* is an error rather than a silent `Unenrolled`, because
/// an operator who asked for the door and got a plan-only run instead would
/// have no way to tell which check said no.
fn seal_through_the_isolated_door<'enrolment>(
    context: SystemRunContext,
) -> Result<SealedRun<'enrolment>, CompositionError> {
    let Some(home) = context.isolated_candidate.clone() else {
        return Ok(SealedRun::unenrolled(context));
    };
    let guards = IsolatedHomeGuards {
        user_home: context.user_home.clone(),
        repository_checkout: fs::canonicalize(".").unwrap_or_else(|_| PathBuf::from(".")),
    };
    match crate::isolated_home::open(home, &guards) {
        Ok(None) => Ok(SealedRun::unenrolled(context)),
        Ok(Some(isolated)) => seal_under_isolated_home(context, isolated),
        Err(error) => Err(CompositionError::IsolatedHome(error.to_string())),
    }
}

/// Seals 2-13 over B5-DESIGN §7's isolated home.
///
/// Structurally the same sequence as [`seal_under_enrollment`], and
/// deliberately not shared with it: that one consumes authority a caller
/// minted, this one consumes authority the door minted from the environment,
/// and folding them would give the fixture branch a path to the door's mints or
/// the door a path to `FixtureEnrollment`'s. What they do share is every
/// invariant the seal *order* encodes — seal 2 before seal 10, the store as
/// seal 7's journal, the provider's token as seal 13.
///
/// Two seals are absent by design rather than by omission. Seal 8 plans no Git
/// effect: §7 widens the home, never the effects. Seal 9's gateway is `None`
/// for the same reason `FixtureEnrollment` takes it as a parameter — the
/// namespace and grant are application policy this root has no mandate to
/// invent; its durable half, the publication drain, is sealed unconditionally.
fn seal_under_isolated_home<'enrolment>(
    context: SystemRunContext,
    isolated: crate::isolated_home::IsolatedEnrollment,
) -> Result<SealedRun<'enrolment>, CompositionError> {
    let crate::isolated_home::IsolatedEnrollment {
        authority,
        home,
        helper_label,
        helper_bytes,
        helper_arguments,
        mission,
    } = isolated;

    // Seal 2 — the writer authority, from the door rather than from the
    // `test-support`-gated fixture mint.
    let boundary = home
        .boundary(&authority)
        .map_err(|_| CompositionError::Seal("writer authority"))?;

    // Seal 3 — the runtime store, which is also seal 7's routing journal.
    let store = home
        .open_runtime_store(Arc::clone(&boundary))
        .map_err(|_| CompositionError::Seal("runtime store"))?;

    // Seal 4 — the canonical event owner, leased for this mission.
    let events = authority
        .open_canonical_event_log(mission.clone())
        .map_err(|_| CompositionError::Seal("canonical event owner"))?;

    // Seal 5 — the attested helper and the workspace every spawn is confined
    // to. Install is the atomic direction and runs first; recovery is the
    // resume path, not a pre-test.
    let executable = match authority.install_fixture_executable(&helper_label, &helper_bytes) {
        Ok(executable) => executable,
        Err(_) => home
            .recover_helper(&authority, &helper_label, &helper_bytes)
            .map_err(|_| CompositionError::Seal("attested helper"))?,
    };
    let workspace = admit_workspace(&authority, &mission)?;
    let process_authority =
        FixtureProcessAuthority::new(executable, &workspace, SupervisorLimits::default())
            .map_err(|_| CompositionError::Seal("process authority"))?;

    // Seal 6 — the local provider and its one registered executor. The runtime
    // family is never `LIVE_PROVIDER_RUNTIME`; §7.3's D9 is that the door
    // widens the home, never the provider.
    let provider = FixtureProcessService::new(process_authority, CancellationToken::new())
        .map_err(|_| CompositionError::Seal("local fixture provider"))?;
    let worker_root = fs::canonicalize(home.path().join("workspaces").join(mission.as_str()))
        .map_err(|_| CompositionError::Seal("local fixture provider"))?;
    let mut registry = ExecutorRegistry::new();
    let _previous = registry
        .register(
            crate::isolated_home::ISOLATED_RUNTIME,
            Arc::new(LocalFixtureExecutor {
                runtime: crate::isolated_home::ISOLATED_RUNTIME.to_owned(),
                executable: helper_label,
                arguments: helper_arguments,
                worker_root: worker_root.clone(),
            }),
        )
        .map_err(|_| CompositionError::Seal("local fixture provider"))?;

    // Seal 10 — the metrics writer, under seal 2's boundary. 2 before 10 is
    // mandatory, and this mint takes a boundary, never a path.
    let metrics = MetricsOwner::assume(
        home.metrics_owner_capability(Arc::clone(&boundary))
            .map_err(|_| CompositionError::Seal("metrics owner"))?,
    )
    .map_err(|_| CompositionError::Seal("metrics owner"))?;

    // Seal 11 — the loopback daemon client over the enrolled root. Absent when
    // no daemon is running, which is the ordinary offline case.
    let daemon = DaemonClient::open(home.path()).ok();

    // Seal 13 — shutdown. The token is the provider's own.
    let cancellation = provider.cancellation_token();

    Ok(SealedRun {
        cancellation,
        reconciler: EvidenceReconciler::new(),
        verifier: None,
        artifact_effects: None,
        daemon,
        metrics: Some(metrics),
        drain: publication_drain(),
        knowledge: None,
        git: None,
        git_plan: None,
        git_phase: None,
        registry,
        provider: Some(provider),
        workspace: Some(workspace),
        worker_root: Some(worker_root),
        events: Some(events),
        store: Some(store),
        boundary: Some(boundary),
        isolated: Some(authority),
        home: context.home,
        home_path: home.path().to_path_buf(),
        resolution: context.resolution,
        warnings: context.warnings,
        mission: Some(mission),
        enrolled_runtime: Some(crate::isolated_home::ISOLATED_RUNTIME.to_owned()),
    })
}

impl SealedRun<'_> {
    /// The seal-1 half, with every later seal absent.
    fn unenrolled(context: SystemRunContext) -> Self {
        Self {
            cancellation: CancellationToken::new(),
            reconciler: EvidenceReconciler::new(),
            verifier: None,
            artifact_effects: None,
            daemon: None,
            metrics: None,
            drain: publication_drain(),
            knowledge: None,
            git: None,
            git_plan: None,
            git_phase: None,
            registry: ExecutorRegistry::new(),
            provider: None,
            workspace: None,
            worker_root: None,
            events: None,
            store: None,
            boundary: None,
            isolated: None,
            home: context.home,
            home_path: context.home_path,
            resolution: context.resolution,
            warnings: context.warnings,
            mission: None,
            enrolled_runtime: None,
        }
    }
}

/// Seals 2-13, under an enrollment that already holds every authority.
///
/// In a build without the fixture doors this is unreachable: its
/// `enrollment` argument is a reference to an uninhabited type, and the empty
/// match below is the compiler's own proof of that.
#[cfg(not(any(test, feature = "test-support")))]
fn seal_under_enrollment<'enrolment>(
    _context: SystemRunContext,
    enrollment: &'enrolment FixtureEnrollment<'enrolment>,
) -> Result<SealedRun<'enrolment>, CompositionError> {
    match enrollment.never {}
}

#[cfg(any(test, feature = "test-support"))]
fn seal_under_enrollment<'enrolment>(
    context: SystemRunContext,
    enrollment: &'enrolment FixtureEnrollment<'enrolment>,
) -> Result<SealedRun<'enrolment>, CompositionError> {
    // §2.1's structural guarantee, enforced here rather than asserted: the
    // live provider family has no registration site anywhere in the tree, and
    // this is the only `register` call in `src/`.
    if enrollment.runtime == LIVE_PROVIDER_RUNTIME {
        return Err(CompositionError::LiveProviderRefused);
    }

    // Seal 2 — writer authority. The live sibling
    // `ProductionWriterAuthority::acquire` is not called by B5.
    let boundary = fixture_production_boundary(enrollment.root)
        .map_err(|_| CompositionError::Seal("writer authority"))?;

    // Seal 3 — the runtime store. It is also seal 7's routing journal.
    let store = open_fixture_runtime_store(enrollment.root)
        .map_err(|_| CompositionError::Seal("runtime store"))?;

    // Seal 4 — the canonical event owner, leased for this mission.
    let events = enrollment
        .authority
        .open_canonical_event_log(enrollment.mission.clone())
        .map_err(|_| CompositionError::Seal("canonical event owner"))?;

    // Seal 5 — the attested helper and the workspace the process authority
    // confines every spawn to.
    // Install is the atomic direction and runs first; `recover` is the resume
    // path, not a pre-test. A second seal over a home a previous process left
    // behind re-admits the *same* installed bytes — reinstalling would change
    // the file identity the earlier authority pinned.
    let executable = match enrollment
        .authority
        .install_fixture_executable(enrollment.helper_label, enrollment.helper_bytes)
    {
        Ok(executable) => executable,
        Err(_) => enrollment
            .authority
            .recover_fixture_executable(enrollment.helper_label, enrollment.helper_bytes)
            .map_err(|_| CompositionError::Seal("attested helper"))?,
    };
    let workspace = admit_workspace(enrollment.authority, &enrollment.mission)?;
    let process_authority =
        FixtureProcessAuthority::new(executable, &workspace, SupervisorLimits::default())
            .map_err(|_| CompositionError::Seal("process authority"))?;

    // Seal 6 — the local fixture provider and the one registered executor.
    let provider = FixtureProcessService::new(process_authority, CancellationToken::new())
        .map_err(|_| CompositionError::Seal("local fixture provider"))?;
    let worker_root = fs::canonicalize(
        enrollment
            .root
            .path()
            .join("workspaces")
            .join(enrollment.mission.as_str()),
    )
    .map_err(|_| CompositionError::Seal("local fixture provider"))?;
    let mut registry = ExecutorRegistry::new();
    let _previous = registry
        .register(
            enrollment.runtime,
            Arc::new(LocalFixtureExecutor {
                runtime: enrollment.runtime.to_owned(),
                executable: enrollment.helper_label.to_owned(),
                arguments: enrollment
                    .helper_arguments
                    .iter()
                    .map(|value| (*value).to_owned())
                    .collect(),
                worker_root: worker_root.clone(),
            }),
        )
        .map_err(|_| CompositionError::Seal("local fixture provider"))?;

    // Seal 7 needs no construction: `RuntimeStore` *is* the
    // `RoutingDecisionJournal`, and the routing map is seal 1's. The store is
    // handed out by scoped `&mut` borrow (`route_dispatch`, `drain_publications`),
    // never retained as one, which is why this branch has no
    // ledger-during-dispatch hazard.

    // Seal 8 — the governed Git seam, through B4's existing assembly point.
    let (git, git_plan, git_phase) = match &enrollment.git {
        None => (None, None, None),
        Some(git) => {
            let ledger = GitEffectLedger::open(git.capability)?;
            let service = governed_git_run(
                ledger,
                git.capability,
                git.adapter,
                enrollment.mission.clone(),
                enrollment.phase.clone(),
                git.repo_root.clone(),
            )?;
            (
                Some(service),
                Some(git.plan.clone()),
                Some(enrollment.phase.clone()),
            )
        }
    };

    // Seal 9 — the knowledge plane: the durable publication drain always, the
    // gateway when the caller enrolled one.
    let drain = publication_drain();

    // Seal 10 — the metrics writer, under seal 2's boundary. 2 before 10 is
    // mandatory; `in_fixture_boundary` takes a boundary, never a path, and is
    // the same door seal 3 used.
    let metrics = MetricsOwner::assume(
        MetricsOwnerCapability::in_fixture_boundary(Arc::clone(&boundary))
            .map_err(|_| CompositionError::Seal("metrics owner"))?,
    )
    .map_err(|_| CompositionError::Seal("metrics owner"))?;

    // Seal 11 — the loopback daemon client. Absent when no daemon is running,
    // which is the ordinary offline case.
    //
    // **Amendment to B5-DESIGN §1.1.** The design's table says seal 11 takes
    // "seal 1's root path only". Under an enrollment seal 1's path is the
    // ambient runtime home while every other seal is the fixture root, so
    // taking it here would give the sealed run a client to a home it never
    // writes to. It takes the enrolled root instead, which keeps the whole
    // seal inside one home. `DaemonClient::open` reads two identity files and
    // a token and creates nothing, so an absent daemon is `None`, not a fault.
    let daemon = DaemonClient::open(enrollment.root.path()).ok();

    // Seal 12 — the independent evidence authority and the reconciler.
    let (artifact_effects, verifier) = match &enrollment.evidence {
        None => (None, None),
        Some(evidence) => {
            let authority = workspace
                .bind_fixture_artifact(&enrollment.phase, 1, evidence.artifact, evidence.expected)
                .map_err(|_| CompositionError::Seal("evidence verifier"))?;
            let (service, verifier) =
                FixtureArtifactEffectService::new(authority, evidence.expected, None)
                    .map_err(|_| CompositionError::Seal("evidence verifier"))?;
            (Some(service), Some(verifier))
        }
    };

    // Seal 13 — shutdown. The token is the provider's own, so cancelling the
    // sealed run cancels every owned child.
    let cancellation = provider.cancellation_token();

    Ok(SealedRun {
        cancellation,
        reconciler: EvidenceReconciler::new(),
        verifier,
        artifact_effects,
        daemon,
        metrics: Some(metrics),
        drain,
        knowledge: enrollment.knowledge,
        git,
        git_plan,
        git_phase,
        registry,
        provider: Some(provider),
        workspace: Some(workspace),
        worker_root: Some(worker_root),
        events: Some(events),
        store: Some(store),
        boundary: Some(boundary),
        isolated: None,
        home: context.home,
        home_path: context.home_path,
        resolution: context.resolution,
        warnings: context.warnings,
        mission: Some(enrollment.mission.clone()),
        enrolled_runtime: Some(enrollment.runtime.to_owned()),
    })
}

/// The Go-shaped plan one resolved run publishes into its checkpoint.
///
/// Only the fields Go's readers use are populated: `internal/cmd/status.go`
/// counts `Plan.Phases` and compares each phase's `Status`, and
/// `internal/core`'s decoder round-trips the rest. Every phase is written in
/// its pre-dispatch state, because this runs before the first dispatch.
fn authored_plan(mission: &str, resolved: &ResolvedRun, created_at: &str) -> CheckpointPlan {
    CheckpointPlan {
        id: mission.to_owned(),
        task: crate::run::first_line(&resolved.task).to_owned(),
        phases: resolved
            .phases
            .iter()
            .map(|phase| CheckpointPhase {
                id: phase.id.to_string(),
                name: phase.name.clone(),
                objective: phase.objective.clone(),
                persona: phase.persona.clone(),
                dependencies: phase.dependencies.iter().map(ToString::to_string).collect(),
                runtime: phase.effective_runtime.clone(),
                status: "pending".to_owned(),
                ..CheckpointPhase::default()
            })
            .collect(),
        execution_mode: match resolved.execution_mode {
            orchestrator_core::ExecutionMode::Sequential => "sequential",
            orchestrator_core::ExecutionMode::Parallel => "parallel",
        }
        .to_owned(),
        decomp_source: if resolved.mission_path.is_some() {
            "authored".to_owned()
        } else {
            "keyword".to_owned()
        },
        created_at: created_at.to_owned(),
        extra: std::collections::BTreeMap::new(),
    }
}

/// Creates the mission workspace, or adopts the one a previous process left.
///
/// `create_workspace` is the atomic direction, so it runs first and the open
/// is the recovery path — never a check-then-act pre-test.
///
/// It takes the authority and the mission rather than an enrollment, because
/// both enrollment shapes reach it: the fixture one of §1.1 and B5-DESIGN §7's
/// isolated one.
fn admit_workspace(
    authority: &FreshFixtureAuthority,
    mission: &MissionId,
) -> Result<WorkspaceAuthority, CompositionError> {
    let seed = WorkspaceSeed::new(
        format!("mission: {}\n", mission.as_str()).into_bytes(),
        &CheckpointProjection {
            workspace_id: mission.as_str().to_owned(),
            status: "pending".to_owned(),
            // CF-M4a-4. Go's `internal/cmd/status.go:63` dereferences
            // `cp.Plan` with no nil check, so an absent plan is not "the
            // optional field is unset" to the reader that matters — it is a
            // nil-pointer `SIGSEGV`. The workspace is admitted at seal time
            // and the plan is only resolved afterwards, so the seed carries
            // the empty placeholder and `SealedRun::execute` fills it with the
            // authored phases through `publish_authored_plan`. A crash in that
            // window therefore still leaves a workspace Go's `status` reads.
            plan: Some(CheckpointPlan::default()),
            ..CheckpointProjection::default()
        },
        b"{}".to_vec(),
    )
    .map_err(|_| CompositionError::Seal("mission workspace"))?;
    match authority.create_workspace(mission.clone(), seed) {
        Ok(workspace) => Ok(workspace),
        Err(_) => authority
            .open_workspace(mission.clone())
            .map_err(|_| CompositionError::Seal("mission workspace")),
    }
}

// ---------------------------------------------------------------------------
// §1.1 seal 6 — the one registered executor
// ---------------------------------------------------------------------------

/// Runs the attested fixture helper through the injected process authority.
///
/// It constructs no dispatch request and emits no event: `ResolvedExecutor::execute`
/// owns the whole lifecycle. It also never reads `request.worker_dir()` — the
/// root it spawns under is the sealed one, so a crafted `WORKDIR:` cannot aim
/// a spawn anywhere else.
struct LocalFixtureExecutor {
    runtime: String,
    executable: String,
    arguments: Vec<String>,
    worker_root: PathBuf,
}

impl PhaseExecutor for LocalFixtureExecutor {
    fn execute(
        &self,
        _request: DispatchRequest<'_>,
        context: &mut ExecutionContext<'_>,
    ) -> AttemptOutcome {
        let started = Instant::now();
        let mut process = match ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            &self.executable,
            &self.worker_root,
        ) {
            Ok(request) => request,
            Err(_) => return incomplete(MechanicalTermination::ContractViolation, started),
        };
        for argument in &self.arguments {
            process = match process.with_argument(argument) {
                Ok(request) => request,
                Err(_) => return incomplete(MechanicalTermination::ContractViolation, started),
            };
        }
        match context.run_process(&process) {
            Ok(receipt) if receipt.is_success() => AttemptOutcome::completed(
                "local fixture execution complete",
                AttemptEvidence::new(),
                started.elapsed(),
            )
            .unwrap_or_else(|_| incomplete(MechanicalTermination::ContractViolation, started)),
            Ok(_) => incomplete(MechanicalTermination::ProviderStreamEnded, started),
            Err(_) => incomplete(MechanicalTermination::ProviderStreamEnded, started),
        }
    }

    fn descriptor(&self) -> Option<RuntimeDescriptor> {
        Some(RuntimeDescriptor::new(
            RuntimeFamily::parse(self.runtime.as_str()).ok()?,
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

fn incomplete(termination: MechanicalTermination, started: Instant) -> AttemptOutcome {
    AttemptOutcome::incomplete(termination, None, PartialWork::empty(), started.elapsed())
}

// ---------------------------------------------------------------------------
// §1.1 seal 4 — the canonical event sink
// ---------------------------------------------------------------------------

/// Projects the enforced worker lifecycle onto the canonical event log.
///
/// The log validates the sequence, so the sink reads the next expected value
/// from the log itself rather than keeping a counter that could drift after a
/// crash.
struct CanonicalEventSink<'log> {
    log: &'log mut CanonicalEventLog,
    mission: String,
    error: EventSinkError,
}

impl EventSink for CanonicalEventSink<'_> {
    fn emit(&mut self, event: &WorkerEventDraft<'_>) -> Result<EventReceipt, EventSinkError> {
        let sequence = self.log.next_sequence().unwrap_or(1);
        let id = format!("evt_{sequence}");
        let timestamp = utc_rfc3339_now();
        let record = EventRecord {
            id: id.clone(),
            event_type: event.kind().as_go_str().to_owned(),
            timestamp: timestamp.clone(),
            sequence,
            mission_id: self.mission.clone(),
            phase_id: Some(event.identity().phase_id().to_owned()),
            worker_id: Some(event.identity().worker_id().to_owned()),
            data: Some(EventJsonMap::default()),
            extra: EventJsonMap::default(),
        };
        let encoded = encode_current_event(&record).map_err(|_| self.error.clone())?;
        self.log
            .append_current_json(&encoded)
            .map_err(|_| self.error.clone())?;
        EventReceipt::new(id, timestamp, sequence).map_err(|_| self.error.clone())
    }
}

// ---------------------------------------------------------------------------
// §1.1 — driving a sealed run
// ---------------------------------------------------------------------------

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct FixedWatchdog {
    stall_window: Duration,
}

impl WatchdogPolicy for FixedWatchdog {
    fn evaluate(&self, _now: Instant, last_activity: Instant) -> WatchdogDecision {
        WatchdogDecision::Continue {
            next_check: last_activity + self.stall_window,
        }
    }

    fn stall_window(&self) -> Duration {
        self.stall_window
    }
}

impl SealedRun<'_> {
    /// Drives every released phase in dependency order through the enforced
    /// dispatch seam, then records the mission's terminal metric.
    ///
    /// This is the only place `run::run_one_phase` and
    /// `run::run_one_phase_with_governed_git` are reached from the CLI.
    ///
    /// # Errors
    /// Returns [`SealedRunError`] when the run was sealed with `None`, when a
    /// phase's runtime is not enrolled, when the plan's `DEPENDS` edges do not
    /// form a DAG, or when dispatch, a governed Git effect, or the terminal
    /// metric write fails.
    pub fn execute(&mut self, resolved: &ResolvedRun) -> Result<SealedRunReport, SealedRunError> {
        let mission = self.mission.clone().ok_or(SealedRunError::Unenrolled)?;
        for phase in &resolved.phases {
            if let ExecutionEnrollment::Unenrolled { runtime, .. } =
                self.enrollment_for(&phase.effective_runtime)
            {
                return Err(SealedRunError::NotEnrolled {
                    phase: phase.id.to_string(),
                    runtime,
                });
            }
        }
        // `Option::filter` rather than a `let`-chain: let-chains are unstable on
        // the 1.85 MSRV floor the evidence lane builds against.
        if let Some(bound) = self
            .git_phase
            .as_ref()
            .filter(|bound| !resolved.phases.iter().any(|phase| &phase.id == *bound))
        {
            return Err(SealedRunError::GitPhaseNotReleased {
                phase: bound.to_string(),
            });
        }
        let order = dependency_order(&resolved.phases)?;
        let started_at = utc_rfc3339_now();
        // CF-M4a-4, the second half: the seeded placeholder becomes the
        // authored plan before the first phase is dispatched, so a workspace a
        // rolled-back Go binary lands on carries the phases it will count.
        self.workspace
            .as_ref()
            .ok_or(SealedRunError::Unenrolled)?
            .publish_authored_plan(&authored_plan(mission.as_str(), resolved, &started_at))
            .map_err(|error| SealedRunError::Workspace(error.to_string()))?;
        let mut report = SealedRunReport {
            mission: mission.clone(),
            phases: Vec::with_capacity(order.len()),
        };
        for index in order {
            let phase = &resolved.phases[index];
            report
                .phases
                .push(self.run_released_phase(mission.as_str(), resolved, phase)?);
        }
        let completed = report
            .phases
            .iter()
            .all(|run| run.result.outcome().is_completed());
        let metrics = self.metrics.as_ref().ok_or(SealedRunError::Unenrolled)?;
        metrics
            .record_terminal(&TerminalMetricIntent {
                mission: mission.as_str().to_owned(),
                domain: resolved.flags.persistent.domain.clone(),
                task: crate::run::first_line(&resolved.task).to_owned(),
                started_at,
                finished_at: utc_rfc3339_now(),
                duration_s: 0,
                status: if completed { "completed" } else { "failed" }.to_owned(),
                decomp_source: if resolved.mission_path.is_some() {
                    "authored".to_owned()
                } else {
                    "keyword".to_owned()
                },
            })
            .map_err(|error| SealedRunError::Metrics(error.to_string()))?;
        Ok(report)
    }

    fn run_released_phase(
        &mut self,
        mission_id: &str,
        resolved: &ResolvedRun,
        phase: &ResolvedPhase,
    ) -> Result<SealedPhaseRun, SealedRunError> {
        let Self {
            reconciler: _,
            verifier,
            artifact_effects,
            git,
            git_plan,
            git_phase,
            registry,
            provider,
            events,
            ..
        } = self;
        let provider = provider.as_ref().ok_or(SealedRunError::Unenrolled)?;
        let events = events.as_mut().ok_or(SealedRunError::Unenrolled)?;
        let clock = SystemClock;
        let watchdog = FixedWatchdog {
            stall_window: phase
                .stall_timeout
                .unwrap_or_else(|| Duration::from_secs(30)),
        };
        let denied = DeniedEffects::new().map_err(|_| SealedRunError::Unenrolled)?;
        let effects: &dyn EffectService = match artifact_effects.as_ref() {
            Some(service) => service,
            None => &denied,
        };
        let mut sink = CanonicalEventSink {
            mission: mission_id.to_owned(),
            error: EventSinkError::new(
                EventSinkErrorKind::Rejected,
                "the canonical event owner refused an append",
            )
            .map_err(|_| SealedRunError::Unenrolled)?,
            log: events,
        };
        let identity = WorkerIdentity::new(mission_id, phase.id.as_str(), WORKER_ID)
            .map_err(|_| SealedRunError::Unenrolled)?;
        let deadline = Instant::now() + watchdog.stall_window * 4;
        let mut context = ExecutionContext::new(
            provider, &clock, &watchdog, effects, &mut sink, identity, deadline,
        )
        .with_verbose(resolved.flags.persistent.verbose);
        if let Some(verifier) = verifier.as_ref() {
            context = context.with_evidence_verifier(verifier);
        }

        let governed = git_phase.as_ref().is_some_and(|bound| bound == &phase.id);
        match (git.as_mut(), git_plan.as_ref()) {
            (Some(service), Some(plan)) if governed => {
                let receipts = crate::run::run_one_phase_with_governed_git(
                    mission_id,
                    &resolved.flags.persistent.domain,
                    phase,
                    registry,
                    &mut context,
                    None,
                    crate::run::GovernedGit { service, plan },
                )?;
                Ok(SealedPhaseRun {
                    phase: phase.id.clone(),
                    runtime: phase.effective_runtime.clone(),
                    result: SealedPhaseOutcome::Governed(Box::new(receipts)),
                })
            }
            _ => {
                let outcome = crate::run::run_one_phase(
                    mission_id,
                    &resolved.flags.persistent.domain,
                    phase,
                    registry,
                    &mut context,
                    None,
                )?;
                Ok(SealedPhaseRun {
                    phase: phase.id.clone(),
                    runtime: phase.effective_runtime.clone(),
                    result: SealedPhaseOutcome::Dispatched(Box::new(outcome)),
                })
            }
        }
    }

    /// Seal 7: routes one dispatch with the sealed store as the journal.
    ///
    /// The store is borrowed for the call and released at its end — never
    /// retained as a long-lived `&mut` in a field.
    ///
    /// # Errors
    /// Returns [`SealedRunError::Unenrolled`] when no store was sealed, and
    /// [`SealedRunError::Routing`] when the boundary's own preconditions fail.
    /// Policy failures fail closed inside the boundary instead.
    pub fn route_dispatch(
        &mut self,
        request: &DispatchDecisionRequest,
    ) -> Result<RoutingDispatchOutcome, SealedRunError> {
        let store = self.store.as_mut().ok_or(SealedRunError::Unenrolled)?;
        Ok(decide_dispatch_route(request, store)?)
    }

    /// Seal 9: drains the durable publication queue with the sealed store.
    ///
    /// # Errors
    /// Returns [`SealedRunError::Unenrolled`] when no store was sealed and
    /// [`SealedRunError::Store`] when claiming or resolving fails.
    pub fn drain_publications<F>(
        &mut self,
        limit: usize,
        resolved_at_utc: &str,
        consumer: F,
    ) -> Result<DrainReport, SealedRunError>
    where
        F: FnMut(&ClaimedPublication) -> DeliveryVerdict,
    {
        let Self { drain, store, .. } = self;
        let store = store.as_mut().ok_or(SealedRunError::Unenrolled)?;
        Ok(drain.drain(store, limit, resolved_at_utc, consumer)?)
    }

    /// Seal 10: records one phase row through the sealed metrics writer.
    ///
    /// The reaping witness is the caller's, because only the observer of the
    /// phase's process group can produce one — that is the whole point of
    /// `PhaseMetricIntent::new`'s signature.
    ///
    /// # Errors
    /// Returns [`SealedRunError::Unenrolled`] when no metrics owner was sealed
    /// and [`SealedRunError::Metrics`] when the upsert fails.
    pub fn record_phase(&self, intent: &PhaseMetricIntent) -> Result<(), SealedRunError> {
        self.metrics
            .as_ref()
            .ok_or(SealedRunError::Unenrolled)?
            .record_phase(intent)
            .map_err(|error| SealedRunError::Metrics(error.to_string()))
    }

    /// Seal 10: the sealed metrics writer, for read-back assertions.
    #[must_use]
    pub const fn metrics(&self) -> Option<&MetricsOwner> {
        self.metrics.as_ref()
    }

    /// Seal 4: the canonical event owner.
    #[must_use]
    pub const fn events(&self) -> Option<&CanonicalEventLog> {
        self.events.as_ref()
    }

    /// Seal 2: the boundary every later seal descends from.
    #[must_use]
    pub const fn boundary(&self) -> Option<&Arc<ProductionBoundary>> {
        self.boundary.as_ref()
    }

    /// Seal 12: the independent evidence authority.
    #[must_use]
    pub const fn verifier(&self) -> Option<&FixtureEvidenceVerifier> {
        self.verifier.as_ref()
    }

    /// Seal 5: the workspace every spawn is confined to.
    #[must_use]
    pub const fn workspace(&self) -> Option<&WorkspaceAuthority> {
        self.workspace.as_ref()
    }
}

/// Orders released phases so every `DEPENDS` predecessor runs first.
///
/// Stable: among ready phases the authored order wins, so two runs of the same
/// plan dispatch in the same order.
fn dependency_order(phases: &[ResolvedPhase]) -> Result<Vec<usize>, SealedRunError> {
    let mut released: BTreeSet<&str> = BTreeSet::new();
    let mut placed = vec![false; phases.len()];
    let mut order = Vec::with_capacity(phases.len());
    while order.len() < phases.len() {
        let mut progressed = false;
        for (index, phase) in phases.iter().enumerate() {
            if placed[index] {
                continue;
            }
            // A self-reference is a phase the authored parser accepts (it
            // records a name before resolving its own `DEPENDS`), and it
            // constrains nothing.
            let ready = phase.dependencies.iter().all(|dependency| {
                dependency == &phase.id || released.contains(dependency.as_str())
            });
            if ready {
                placed[index] = true;
                released.insert(phase.id.as_str());
                order.push(index);
                progressed = true;
            }
        }
        if !progressed {
            return Err(SealedRunError::UnorderablePlan);
        }
    }
    Ok(order)
}

/// The persistent worker name every sealed phase runs under.
const WORKER_ID: &str = "worker-1";

/// An effect authority that admits nothing, used when no artifact effect is
/// enrolled. It is *not* the Git denial service — that one refuses a worker's
/// Git mutation specifically (`worker_effect_service`); this one refuses every
/// effect kind because no adapter is composed for any of them.
struct DeniedEffects(EffectServiceError);

impl DeniedEffects {
    fn new() -> Result<Self, ServiceContractError> {
        Ok(Self(EffectServiceError::new(
            EffectServiceErrorKind::Denied,
            "the sealed run composes no effect adapter",
        )?))
    }
}

impl EffectService for DeniedEffects {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        Err(self.0.clone())
    }
}

/// Current UTC time as RFC3339 with a `Z` offset, e.g. `2026-09-03T00:00:00Z`.
///
/// The calendar arithmetic is [`crate::cmd::time_fmt`]'s reviewed
/// `civil_from_days`, which the CLI already uses to render every timestamp it
/// prints; this only supplies the instant.
pub(crate) fn utc_date_stamp() -> String {
    utc_rfc3339_now()
        .split('T')
        .next()
        .unwrap_or("00000000")
        .replace('-', "")
}

fn utc_rfc3339_now() -> String {
    let epoch_secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    };
    crate::cmd::time_fmt::format_unix_seconds_rfc3339(epoch_secs)
}

#[cfg(test)]
mod tests {
    use super::load_persona_names;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(1);

    #[test]
    fn persona_catalog_accepts_flat_and_directory_conventions()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "orchestrator-cli-personas-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root)?;
        fs::write(root.join("architect.md"), "# Architect\n")?;
        fs::create_dir(root.join("reviewer"))?;
        fs::write(root.join("reviewer/reviewer.md"), "# Reviewer\n")?;
        fs::write(root.join("ignored.txt"), "ignored\n")?;

        let catalog = load_persona_names(&root)?;
        fs::remove_dir_all(&root)?;

        assert_eq!(
            catalog.names.into_iter().collect::<Vec<_>>(),
            ["architect", "reviewer"]
        );
        Ok(())
    }

    #[test]
    fn missing_persona_directory_is_an_empty_catalog() -> Result<(), Box<dyn std::error::Error>> {
        let missing = std::env::temp_dir().join(format!(
            "orchestrator-cli-missing-personas-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let catalog = load_persona_names(&missing)?;
        assert!(catalog.names.is_empty());
        Ok(())
    }
}
