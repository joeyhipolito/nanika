//! Governed Git effects: B4-DESIGN §§1, 4, 5.2.
//!
//! Every Git operation this orchestrator performs goes through one path:
//! typed intent → capability gate → durable plan row → execution →
//! receipt → resolution, with crash reconciliation that **re-observes the
//! repository** rather than trusting a stored claim.
//!
//! Three structural facts carry the design.
//!
//! **There is no ambient Git authority.** [`GitEffectCapability`] is minted
//! only from an [`IsolatedFixtureRoot`], is non-`Clone`, records the root's
//! canonical path and unix `(dev, ino)`, and re-verifies both before every
//! mutation. B4 ships no production mint at all; that is B5's composition root.
//! Workers receive [`DeniedGitMutationService`], which refuses every effect
//! request, so a worker cannot ask for a Git mutation that anything would
//! honour.
//!
//! **Git is the durable external state.** A git effect leaves its own evidence
//! in the repository — a ref, a commit, a worktree registration, a remote ref —
//! so the store's job is only to say *which effect was in flight and what it
//! planned to do*. [`GitEffectService::reconcile`] reads that plan back and
//! then asks the repository what actually happened.
//!
//! **A crash-interrupted git child never recorded an execution identity**, so
//! `EffectResolution::Succeeded` is unreachable for such a row. Every intent is
//! therefore idempotent at the repository level and a crashed effect is
//! reconciled by re-execution along the only reachable ladder,
//! `Executing → Uncertain → ObservedAbsent → Pending → re-claim`.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use orchestrator_core::{MissionId, PhaseId};
use orchestrator_exec::{
    AttemptOutcome, EffectBudget, EffectReceipt, EffectRequest, EffectService, EffectServiceError,
    EffectServiceErrorKind, ServiceContractError,
};
use orchestrator_git::{
    ClaimsDbError, CommitSha, GitChildIdentity, GitError, PrAdapter, PrAdapterError, PrReceipt,
    PrRequest, PushAck, RefreshedBase, RemotePolicy,
};
use serde_json::{Value, json};
use thiserror::Error;

use crate::capability::CapabilityError;
use crate::runtime_home::IsolatedFixtureRoot;
use crate::runtime_store::{
    EffectEvidence, EffectOperationSlot, EffectResolution, JournalIntent, OutboxEffect,
    OutboxEffectKind, OutboxIntent, OutboxState, PrivateProcessLedgerStore,
    ProcessExecutionIdentity, ProcessNotStartedEvidenceReason, RuntimeStoreError,
};

// ---------------------------------------------------------------------------
// §1.2 — operation slots and transition kinds, fixed at compile time
// ---------------------------------------------------------------------------

/// One slot per intent kind. `EffectOperationSlot::new` accepts only
/// `[A-Za-z0-9_-]`, and one intent per slot per logical attempt makes the
/// derived idempotency key exact, so a resumed process reconstructs it without
/// having stored it.
pub const SLOT_REFRESH_BASE: &str = "git-refresh-base";
pub const SLOT_CREATE_ISOLATION: &str = "git-create-isolation";
pub const SLOT_COMMIT: &str = "git-commit";
pub const SLOT_PUSH: &str = "git-push";
pub const SLOT_OPEN_PR: &str = "git-open-pr";
pub const SLOT_CLEANUP: &str = "git-cleanup";
pub const SLOT_ROLLBACK: &str = "git-rollback";

const KIND_REFRESH_BASE: &str = "git.refresh_base";
const KIND_CREATE_ISOLATION: &str = "git.create_isolation";
const KIND_COMMIT: &str = "git.commit";
const KIND_PUSH: &str = "git.push";
const KIND_OPEN_PR: &str = "git.open_pr";
const KIND_CLEANUP: &str = "git.cleanup";
const KIND_ROLLBACK: &str = "git.rollback";

/// Plan payloads are versioned so a later slice can widen them without a
/// resumed process misreading an older row.
const PLAN_PAYLOAD_VERSION: u64 = 1;

/// Bounded page size for the three durable reads this module uses. Well above
/// the seven rows one mission can produce, and below `MAX_QUERY_LIMIT`.
const EFFECT_PAGE: usize = 256;

/// Subdirectory of the capability root that linked worktrees are created under.
const WORKTREES_DIR: &str = "worktrees";
/// Advisory claim registry, kept inside the capability root so a fixture run
/// never touches the operator's `~/.alluka/learnings.db`.
const CLAIMS_DB_FILE: &str = "claims.db";

// ---------------------------------------------------------------------------
// §1.1 — the capability and its witnesses
// ---------------------------------------------------------------------------

/// Authority to perform governed Git effects inside one disposable root.
///
/// Modelled on [`crate::AdditiveFixtureGrant`]: minted only from an
/// [`IsolatedFixtureRoot`] that this process atomically created, carrying the
/// canonical path plus the root's unix `(dev, ino)`, re-verified before every
/// mutation. There is no constructor that accepts a filesystem path and no
/// production mint — B5 owns that.
///
/// Non-`Clone` and non-`Copy` on purpose: it is passed by reference into each
/// effect and never stored as ambient authority.
pub struct GitEffectCapability {
    root: PathBuf,
    remotes: BTreeSet<String>,
    /// Canonicalization cannot see one real directory swapped for another at
    /// the same path; the kernel's own `(dev, ino)` can.
    #[cfg(unix)]
    identity: crate::fs_util::FileIdentity,
}

impl GitEffectCapability {
    /// Mints Git-effect authority bounded to one disposable fixture root and
    /// one enrolled set of remote names.
    ///
    /// The remote set is part of the capability rather than of each `Push`
    /// intent, so "this run may only push to these remotes" is a property of
    /// the authority a caller holds, not of the argument it happens to pass.
    ///
    /// # Errors
    /// Returns [`CapabilityError::RootNotFreshlyCreated`] when the root was
    /// adopted rather than created, [`CapabilityError::Io`] when it cannot be
    /// re-opened or stat'd, and [`CapabilityError::IdentityChanged`] when
    /// canonicalizing it yields a different path than the authority reported.
    pub fn in_fixture(
        fixture: &IsolatedFixtureRoot,
        remotes: &[&str],
    ) -> Result<Self, CapabilityError> {
        if !fixture.freshly_created() {
            return Err(CapabilityError::RootNotFreshlyCreated);
        }
        let root = std::fs::canonicalize(fixture.path())?;
        if root != fixture.path() {
            return Err(CapabilityError::IdentityChanged);
        }
        #[cfg(unix)]
        let identity = crate::fs_util::path_identity(&root)?;
        Ok(Self {
            root,
            remotes: remotes.iter().map(|remote| (*remote).to_owned()).collect(),
            #[cfg(unix)]
            identity,
        })
    }

    /// Re-mints Git-effect authority over a fixture root that a **previous**
    /// process created and then died inside.
    ///
    /// [`Self::in_fixture`] demands a freshly created root so that no ambient
    /// directory can become Git authority. A crash gate needs precisely the
    /// complement of that on one path: the process that created the root is
    /// gone, and the durable state it left is the thing under test. Without
    /// this door a real cross-process crash over one Git fixture tree is not
    /// representable at all, and the crash has to be simulated by dropping the
    /// service — which runs `Drop`, closes the ledger and checkpoints the WAL,
    /// i.e. the three things a died process does not do.
    ///
    /// This mirrors `FreshFixtureAuthority::admit`/`recover` and inherits the
    /// same bound: [`IsolatedFixtureRoot`] has no production constructor, so
    /// nothing outside a fixture tree is reachable here. The two doors are
    /// disjoint — a fresh root is refused here and an adopted one is refused
    /// by `in_fixture` — so this is a complement, never a way around the
    /// freshness requirement.
    ///
    /// Fixture-gated exactly as [`GitEffectService::observe_barriers`] is: a
    /// production build compiles it out.
    ///
    /// # Errors
    /// Returns [`CapabilityError::RootFreshlyCreated`] when the root was
    /// created rather than adopted, [`CapabilityError::Io`] when it cannot be
    /// stat'd, and [`CapabilityError::IdentityChanged`] when canonicalizing it
    /// yields a different path than the authority reported.
    #[cfg(any(test, feature = "test-support"))]
    pub fn recover_fixture(
        fixture: &IsolatedFixtureRoot,
        remotes: &[&str],
    ) -> Result<Self, CapabilityError> {
        if fixture.freshly_created() {
            return Err(CapabilityError::RootFreshlyCreated);
        }
        let root = std::fs::canonicalize(fixture.path())?;
        if root != fixture.path() {
            return Err(CapabilityError::IdentityChanged);
        }
        #[cfg(unix)]
        let identity = crate::fs_util::path_identity(&root)?;
        Ok(Self {
            root,
            remotes: remotes.iter().map(|remote| (*remote).to_owned()).collect(),
            #[cfg(unix)]
            identity,
        })
    }

    /// Re-confirms the root has not been replaced since minting.
    ///
    /// # Errors
    /// Returns [`CapabilityError::IdentityChanged`] when the path now resolves
    /// elsewhere or the root is no longer the same directory object.
    pub(crate) fn verify(&self) -> Result<(), CapabilityError> {
        if std::fs::canonicalize(&self.root)? != self.root {
            return Err(CapabilityError::IdentityChanged);
        }
        #[cfg(unix)]
        if crate::fs_util::path_identity(&self.root)? != self.identity {
            return Err(CapabilityError::IdentityChanged);
        }
        Ok(())
    }

    /// The canonical root this capability bounds every mutation to.
    ///
    /// Crate-private: an out-of-crate holder can pass the capability into an
    /// effect but can never read the path back out of it.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    fn admits_remote(&self, remote: &str) -> bool {
        self.remotes.contains(remote)
    }
}

impl fmt::Debug for GitEffectCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitEffectCapability")
            .field("kind", &"git-effect-fixture-capability")
            .field("enrolled_remotes", &self.remotes.len())
            .finish()
    }
}

/// A cleanup destination that cannot name a path outside its capability root.
///
/// Minted only from a [`GitEffectCapability`] and only as a single safe path
/// component beneath that capability's root, so "cleanup cannot escape the
/// fixture root" is established before any filesystem work rather than checked
/// afterwards.
pub struct TrashRoot {
    path: PathBuf,
}

impl TrashRoot {
    /// Names one directory beneath the capability root.
    ///
    /// # Errors
    /// Returns [`GitEffectError::UnsafeComponent`] when `component` is not a
    /// single ordinary path component.
    pub fn under(
        capability: &GitEffectCapability,
        component: &str,
    ) -> Result<Self, GitEffectError> {
        if !safe_path_component(component) {
            return Err(GitEffectError::UnsafeComponent);
        }
        Ok(Self {
            path: capability.root().join(component),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }
}

impl fmt::Debug for TrashRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrashRoot")
            .field("path", &"[REDACTED]")
            .finish()
    }
}

/// Proof that one phase attempt reached a completed terminal outcome.
///
/// This is what makes "unverified work is not committed" a type fact rather
/// than a convention: [`GitIntent::Commit`] cannot be constructed without one,
/// and the only producer refuses an incomplete attempt.
///
/// **Deviation from B4-DESIGN §1.1.** The design writes the constructor as
/// `from_outcome(outcome: &AttemptOutcome)` and says the witness "carries the
/// phase id and attempt". `AttemptOutcome` carries neither — it is the
/// executor's return value, not an identified record — so the binding is
/// supplied by the caller that owns both. The service still checks the
/// witness's phase against its own, so a witness minted for one phase cannot
/// authorize a commit in another.
pub struct VerifiedWork {
    phase: PhaseId,
    attempt: u32,
}

impl VerifiedWork {
    /// Mints a witness, or `None` when the attempt did not complete.
    #[must_use]
    pub fn from_outcome(phase: &PhaseId, attempt: u32, outcome: &AttemptOutcome) -> Option<Self> {
        outcome.is_completed().then(|| Self {
            phase: phase.clone(),
            attempt,
        })
    }

    #[must_use]
    pub const fn phase(&self) -> &PhaseId {
        &self.phase
    }

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }
}

impl fmt::Debug for VerifiedWork {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedWork")
            .field("phase", &self.phase)
            .field("attempt", &self.attempt)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// §1.1 — intents, §1.3 — receipts
// ---------------------------------------------------------------------------

/// The seven governed Git operations. Nothing else is representable.
pub enum GitIntent {
    /// Fetch the configured base branch and record the exact commit it
    /// resolved to. Read-only against the remote.
    RefreshBase { remote: String, base_branch: String },
    /// Cut the isolation branch and its worktree. Consumes a [`RefreshedBase`]
    /// **by value**: there is no constructor that takes a base *branch name*,
    /// so a branch can never be cut from a ref another process can move.
    CreateIsolation {
        base: RefreshedBase,
        mission_id: MissionId,
        task: String,
    },
    /// Commit the worktree. Requires a [`VerifiedWork`] witness.
    Commit { message: String, work: VerifiedWork },
    /// Publish the isolation branch. Bounded by the capability's enrolled
    /// remote set.
    Push { remote: String, branch: String },
    /// Open a pull request through the enrolled adapter. No adapter means no
    /// pull request is representable.
    OpenPr {
        title: String,
        body: String,
        draft: bool,
    },
    /// Soft-delete the worktree into a confined trash root.
    Cleanup { trash: TrashRoot },
    /// Undo one partially applied effect. Requires the receipt being undone,
    /// so rolling back an effect that never produced one is not representable.
    Rollback { of: GitReceipt },
}

impl fmt::Debug for GitIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RefreshBase { .. } => "GitIntent::RefreshBase",
            Self::CreateIsolation { .. } => "GitIntent::CreateIsolation",
            Self::Commit { .. } => "GitIntent::Commit",
            Self::Push { .. } => "GitIntent::Push",
            Self::OpenPr { .. } => "GitIntent::OpenPr",
            Self::Cleanup { .. } => "GitIntent::Cleanup",
            Self::Rollback { .. } => "GitIntent::Rollback",
        })
    }
}

/// What a governed Git effect actually did.
///
/// `changed_files` and `claimed_files` are counts, not path lists: the payload
/// is bounded and paths are mission-sensitive. Every `Debug` rendering redacts
/// paths and messages, matching `OutboxEffect`'s own.
#[derive(Clone)]
pub enum GitReceipt {
    BaseRefreshed {
        remote: String,
        base_branch: String,
        base_sha: CommitSha,
        local_sha: Option<CommitSha>,
    },
    IsolationCreated {
        branch: String,
        base_sha: CommitSha,
        worktree: PathBuf,
        lock_pid: u32,
        claimed_files: usize,
    },
    Committed {
        commit_sha: CommitSha,
        parent_sha: CommitSha,
        changed_files: usize,
    },
    Pushed {
        remote: String,
        branch: String,
        ack: PushAck,
    },
    PrOpened {
        provider: String,
        receipt_id: String,
        url: String,
        head_sha: CommitSha,
    },
    CleanedUp {
        trash_entry: PathBuf,
    },
    RolledBack {
        slot: &'static str,
        action: RollbackAction,
    },
}

impl GitReceipt {
    /// The slot this receipt belongs to.
    #[must_use]
    pub const fn slot(&self) -> &'static str {
        match self {
            Self::BaseRefreshed { .. } => SLOT_REFRESH_BASE,
            Self::IsolationCreated { .. } => SLOT_CREATE_ISOLATION,
            Self::Committed { .. } => SLOT_COMMIT,
            Self::Pushed { .. } => SLOT_PUSH,
            Self::PrOpened { .. } => SLOT_OPEN_PR,
            Self::CleanedUp { .. } => SLOT_CLEANUP,
            Self::RolledBack { .. } => SLOT_ROLLBACK,
        }
    }
}

impl fmt::Debug for GitReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut record = formatter.debug_struct("GitReceipt");
        record.field("slot", &self.slot());
        match self {
            Self::BaseRefreshed {
                remote,
                base_branch,
                base_sha,
                local_sha,
            } => record
                .field("remote", remote)
                .field("base_branch", base_branch)
                .field("base_sha", base_sha)
                .field(
                    "local_base_is_stale",
                    &(local_sha.as_ref() != Some(base_sha)),
                ),
            Self::IsolationCreated {
                branch,
                base_sha,
                claimed_files,
                ..
            } => record
                .field("branch", branch)
                .field("base_sha", base_sha)
                .field("worktree", &"[REDACTED]")
                .field("claimed_files", claimed_files),
            Self::Committed {
                commit_sha,
                parent_sha,
                changed_files,
            } => record
                .field("commit_sha", commit_sha)
                .field("parent_sha", parent_sha)
                .field("changed_files", changed_files),
            Self::Pushed {
                remote,
                branch,
                ack,
            } => record
                .field("remote", remote)
                .field("branch", branch)
                .field("ack", ack),
            Self::PrOpened {
                provider,
                receipt_id,
                ..
            } => record
                .field("provider", provider)
                .field("receipt_id", receipt_id)
                .field("url", &"[REDACTED]"),
            Self::CleanedUp { .. } => record.field("trash_entry", &"[REDACTED]"),
            Self::RolledBack { action, .. } => record.field("action", action),
        }
        .finish()
    }
}

/// What a rollback actually undid (B4-DESIGN §5.2).
#[derive(Clone)]
pub enum RollbackAction {
    /// The worktree was renamed into trash and the branch deleted, having been
    /// confirmed still at its recorded base.
    WorktreeTrashedAndBranchDeleted { trash_entry: PathBuf },
    /// The receipt describes an effect that applies nothing to the working
    /// repository, so there is nothing to undo.
    NoEffectToUndo,
}

impl fmt::Debug for RollbackAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::WorktreeTrashedAndBranchDeleted { .. } => "worktree-trashed-and-branch-deleted",
            Self::NoEffectToUndo => "no-effect-to-undo",
        })
    }
}

// ---------------------------------------------------------------------------
// §4 — reconciliation outcomes
// ---------------------------------------------------------------------------

/// One reconciled crashed effect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Reconciliation {
    slot: String,
    disposition: ReconciliationDisposition,
}

impl Reconciliation {
    #[must_use]
    pub fn slot(&self) -> &str {
        &self.slot
    }

    #[must_use]
    pub const fn disposition(&self) -> ReconciliationDisposition {
        self.disposition
    }
}

/// How one crashed effect was resolved by re-observing the repository.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconciliationDisposition {
    /// The repository proves the effect landed. Resolved `Succeeded`; nothing
    /// is re-executed.
    AdoptedObservedSuccess,
    /// The repository proves the effect did not land. Resolved `ObservedAbsent`
    /// so the row returns to `Pending` for exactly one re-claim.
    ObservedAbsentAndRequeued,
    /// The repository proves the effect landed *differently* than planned.
    /// Resolved `Failed`; nothing is deleted, moved, or forced.
    FailedTerminally,
    /// The remote could not be observed at all. The row stays `Uncertain` and
    /// is retained; there is no path from here to a re-push without an
    /// intervening observation.
    RetainedUncertain,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Failures raised by the governed Git effect service.
#[derive(Debug, Error)]
pub enum GitEffectError {
    #[error("git effect capability: {0}")]
    Capability(#[from] CapabilityError),
    #[error("git effect store: {0}")]
    Store(#[from] RuntimeStoreError),
    /// Admission failed after the ledger was opened, and the ledger could not
    /// prove a clean close. Both failures are retained: reporting only the
    /// admission refusal would hide that writer handoff remains uncertain.
    #[error("git effect setup failed ({setup}); ledger close was uncertain ({close})")]
    SetupAndLedgerClose {
        setup: Box<GitEffectError>,
        close: RuntimeStoreError,
    },
    #[error("git: {0}")]
    Git(#[from] GitError),
    #[error("claim registry: {0}")]
    Claims(#[from] ClaimsDbError),
    #[error("pull-request adapter: {0}")]
    PrAdapter(#[from] PrAdapterError),
    /// The remote is not in the capability's enrolled set.
    #[error("remote is not enrolled for this capability")]
    RemoteNotEnrolled,
    /// A path component would escape its root.
    #[error("path component is not a single safe name")]
    UnsafeComponent,
    /// `Commit`, `Push`, `OpenPr`, and `Cleanup` all require an isolation the
    /// service created or adopted.
    #[error("no isolation has been created for this run")]
    MissingIsolation,
    /// A `VerifiedWork` witness minted for a different phase.
    #[error("verified-work witness belongs to another phase")]
    WitnessPhaseMismatch,
    /// The slot's durable row is `Executing` or `Uncertain`: it must be
    /// reconciled before it can be applied again.
    #[error("effect slot {slot} requires reconciliation before it can be applied")]
    RequiresReconciliation { slot: &'static str },
    /// The slot's durable row is `Failed`. `Failed → RetryFailed` needs a
    /// `RetryAuthorization` no caller in this batch can mint, and that is
    /// deliberate: `Failed` is reserved for refusals that must stop the run.
    #[error("effect slot {slot} failed terminally")]
    FailedTerminally { slot: &'static str },
    /// The repository shows the branch at a sha this run did not record. Never
    /// force-moved, never deleted.
    #[error("branch carries work this run did not record")]
    BranchCarriesUnknownWork,
    /// A commit landed but the worktree is still dirty: a partial stage that
    /// must not be papered over by `commit_all`'s "nothing to commit is
    /// success" behavior.
    #[error("commit landed but the worktree is not clean")]
    PartialCommitObserved,
    /// The remote's ref is at a sha this run did not push. Never forced.
    #[error("remote ref is at a sha this run did not publish")]
    RemoteRefDiverged,
    /// A rollback would delete a branch that has moved off its recorded base.
    #[error("rollback would discard work")]
    WouldDiscardWork,
    /// B4 never rewrites history and never retracts a published ref.
    #[error("this receipt is not rollbackable")]
    NotRollbackable,
    /// No supervised git child could be observed for this effect, so no
    /// execution identity can be recorded and the row cannot reach a terminal
    /// resolution.
    #[error("no supervised git child was observed for this effect")]
    ExecutionIdentityUnavailable,
    /// The durable plan row could not be located after it was journaled.
    #[error("the journaled plan row for slot {slot} could not be located")]
    PlanRowMissing { slot: &'static str },
    /// A stored plan payload is not the shape this version writes.
    #[error("the stored plan payload for slot {slot} is unreadable")]
    PlanPayloadUnreadable { slot: &'static str },
    /// A fixture observer cut the effect at a durable boundary. Unreachable
    /// unless [`GitEffectService::observe_barriers`] installed an observer that
    /// asked for it, which no production composition does.
    #[error("fixture observer cut slot {slot} at {point:?}")]
    FixtureCut {
        slot: &'static str,
        point: GitEffectBarrier,
    },
    /// Effect-service construction failed on a bounded contract input.
    #[error("effect service contract: {0}")]
    Contract(#[from] ServiceContractError),
}

// ---------------------------------------------------------------------------
// §1.5 — no ambient Git authority for workers
// ---------------------------------------------------------------------------

/// The `EffectService` every worker's `ExecutionContext` receives.
///
/// It refuses **every** request, `EffectKind::GitMutation` included. This is
/// what "no ambient Git authority" means operationally: a worker cannot request
/// a Git mutation because there is no service that would honour one. The
/// [`GitEffectService`] is held only by the CLI run path.
pub struct DeniedGitMutationService {
    denied: EffectServiceError,
}

impl DeniedGitMutationService {
    /// Builds the refusal once, so `execute` cannot fail for any reason other
    /// than the refusal itself.
    ///
    /// # Errors
    /// Returns [`ServiceContractError`] only if the fixed refusal message is
    /// rejected by the contract, which cannot happen for this constant.
    pub fn new() -> Result<Self, ServiceContractError> {
        Ok(Self {
            denied: EffectServiceError::new(
                EffectServiceErrorKind::Denied,
                "workers hold no git authority; git effects are the run path's",
            )?,
        })
    }
}

impl fmt::Debug for DeniedGitMutationService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeniedGitMutationService")
            .field("kind", &"denies-every-effect")
            .finish()
    }
}

impl EffectService for DeniedGitMutationService {
    fn execute(
        &self,
        _request: &EffectRequest,
        _budget: EffectBudget<'_>,
    ) -> Result<EffectReceipt, EffectServiceError> {
        Err(self.denied.clone())
    }
}

// ---------------------------------------------------------------------------
// The durable ledger a governed Git run journals into
// ---------------------------------------------------------------------------

/// Directory beneath the capability root that holds the Git effect ledger.
#[cfg(any(test, feature = "test-support"))]
const LEDGER_DIR: &str = "git-ledger";

/// The Rust-private journal and outbox a governed Git run writes to.
///
/// **Deviation from B4-DESIGN §1.4, and the reason for it.** The design routes
/// git effects through the public `RuntimeStore::append`, asserting that "no
/// compatibility projection is declared, so `append` writes `command_ack` in
/// the same transaction". That is not reachable: `RuntimeStore::append` targets
/// the Go-**compatibility** boundary, and `PreparedIntent::new` refuses a
/// compatibility transition whose `required_projections` is empty. A git fetch,
/// branch, or push projects nothing Go reads, so declaring a projection for one
/// and then recording a receipt for it would be a false attestation of a
/// Go-visible surface — and recording it *after* execution would force the
/// claim after execution too, which would leave a crashed effect `Pending`
/// rather than `Executing` and make the whole §4 reconciler unreachable.
///
/// The store already has the right home for this shape. A private process
/// ledger is "isolated Rust-private state", its transitions *require* a mission
/// and *forbid* projections, and `command_ack` is written in the append
/// transaction exactly as §1.4 needs. Everything else in §1.4 is unchanged:
/// the same `OutboxEffectKind::GitCommand` rows, the same `claim_exact` →
/// `record_execution_identity` → `resolve_effect` sequence, and the same
/// `recovery_effects` read driving §4.
///
/// The ledger lives in its own directory beneath the capability root, because a
/// database is one boundary kind for its entire lifetime and the runtime home's
/// own `runtime.db` is a compatibility store.
pub struct GitEffectLedger {
    store: PrivateProcessLedgerStore,
}

impl GitEffectLedger {
    /// Opens (or creates) the ledger bounded by one Git-effect capability.
    ///
    /// Fixture-gated, exactly like [`crate::open_fixture_runtime_store`]: the
    /// boundary this opens is built by a constructor that is compiled out of
    /// every production build, so B4 ships no way to journal a Git effect
    /// against a live runtime home. B5 owns the production door.
    ///
    /// # Errors
    /// Returns [`GitEffectError::Capability`] when the root cannot be verified
    /// and [`GitEffectError::Store`] when the ledger cannot be opened.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open(capability: &GitEffectCapability) -> Result<Self, GitEffectError> {
        capability.verify()?;
        let path = capability.root().join(LEDGER_DIR);
        create_private_dir(&path).map_err(|source| GitEffectError::Git(GitError::Io(source)))?;
        let canonical = std::fs::canonicalize(&path)
            .map_err(|source| GitEffectError::Git(GitError::Io(source)))?;
        let boundary = crate::runtime_home::ProductionBoundary::from_canonical_root(&canonical)
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let store = crate::runtime_store::RuntimeStore::open_private(
            crate::runtime_store::PrivateProcessLedgerBoundary::new(std::sync::Arc::new(boundary)),
            crate::runtime_store::StorageActorAuthority::new(),
        )?;
        Ok(Self { store })
    }
}

impl fmt::Debug for GitEffectLedger {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitEffectLedger")
            .field("kind", &"git-effect-private-ledger")
            .finish()
    }
}

/// Creates a mode-0700 directory, tolerating one that already exists.
#[cfg(any(test, feature = "test-support"))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Fixture-only durable boundaries (B4-DESIGN §4)
// ---------------------------------------------------------------------------

/// The exact durable boundaries a crash can fall between.
///
/// Modelled on this crate's existing `DurableProcessBarrierPoint`: an
/// infallible, actor-scoped observation that receives no storage, receipt, or
/// execution authority and cannot change what the service does. It exists so a
/// gate can cut a real process at each of B4-DESIGN §4's six points instead of
/// hand-planting a durable row, which would prove only that the reconciler
/// agrees with the test's idea of a crash.
///
/// The type is always compiled; the only thing that installs an observer,
/// [`GitEffectService::observe_barriers`], is fixture-gated, so a production
/// service holds `None` and every crossing is one null check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitEffectBarrier {
    /// The row is claimed and `Executing`. No git work has been attempted.
    ClaimedBeforeExecution,
    /// The isolation branch exists at the recorded base; its worktree does not
    /// yet (§4 C2).
    IsolationBranchBeforeWorktree,
    /// The worktree is registered and locked; the row is still `Executing`
    /// (§4 C3).
    IsolationWorktreeBeforeResolution,
    /// The effect's git work completed; the row is still `Executing`, and no
    /// execution identity has been recorded (§4 C4, C5, C6).
    ExecutedBeforeResolution,
    /// The row reached `Succeeded` and the next slot has not been journaled
    /// (§4 C1).
    SlotResolved,
}

/// What a fixture observer wants the service to do at a boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GitBarrierDecision {
    /// Carry on. This is what a production service always does, because it
    /// installs no observer.
    Continue,
    /// Stop here without resolving, leaving exactly the durable state a
    /// process that died at this boundary would leave.
    ///
    /// B4-DESIGN §6.2 specifies the crash cases as "drop the store without
    /// resolving, reopen, run `reconcile`", and this is the seam that produces
    /// that state. It is a cut in the *effect*, not in the process: a gate that
    /// wants to prove the difference asserts on the durable row afterwards —
    /// `Executing` with no execution identity is what a died-mid-effect row
    /// looks like and is unreachable from any graceful return.
    CutHere,
}

/// An observer of [`GitEffectBarrier`] crossings, installed only by a fixture.
pub type GitBarrierObserver =
    std::sync::Arc<dyn Fn(GitEffectBarrier, &str) -> GitBarrierDecision + Send + Sync>;

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// The isolation this run created or adopted.
#[derive(Clone)]
struct Isolation {
    branch: String,
    base_branch: String,
    worktree: PathBuf,
}

/// Governed Git effects for one mission phase.
///
/// Owns the runtime store for the run and borrows the capability and the
/// pull-request adapter, neither of which it may outlive.
pub struct GitEffectService<'enrolment> {
    /// The durable ledger, present for the whole life of the service.
    ///
    /// `RuntimeStore`'s own `Drop` deliberately **retains** the in-process
    /// writer lease when it is dropped without a clean close, so a service
    /// that never closed would make its root permanently unopenable for the
    /// rest of the process. Closing on drop is what lets a run reopen the same
    /// ledger after a cut, which is exactly what B4-DESIGN §6.2 asks a resume
    /// to do — and `close_in_place` lets [`Drop`] do it through the borrow it
    /// has, so the field needs no `Option` and no read needs an unwrap.
    store: PrivateProcessLedgerStore,
    capability: &'enrolment GitEffectCapability,
    adapter: &'enrolment dyn PrAdapter,
    mission: MissionId,
    phase: PhaseId,
    repo_root: PathBuf,
    logical_attempt: u32,
    isolation: Option<Isolation>,
    refreshed: Option<RefreshedBase>,
    barrier: Option<GitBarrierObserver>,
}

impl Drop for GitEffectService<'_> {
    /// Closes the ledger so the in-process writer lease is released.
    ///
    /// A failure here cannot be reported and must not abort a drop, so it is
    /// discarded: every durable write already committed before this point, and
    /// the only thing a failed close costs is the lease, which is exactly the
    /// state an unclosed drop would have left anyway.
    fn drop(&mut self) {
        let _ = self.store.close_in_place();
    }
}

impl fmt::Debug for GitEffectService<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitEffectService")
            .field("mission", &self.mission)
            .field("phase", &self.phase)
            .field("repo_root", &"[REDACTED]")
            .field("logical_attempt", &self.logical_attempt)
            .field("has_isolation", &self.isolation.is_some())
            .finish()
    }
}

/// Whether a slot's durable row is ready to be claimed, already terminal, or
/// mid-flight.
enum SlotEntry {
    /// The row was claimed and this call owns the attempt.
    Claimed { key: String, claim_attempt: u32 },
    /// The row already succeeded. The effect must not run again; the receipt
    /// is re-derived by observing the repository.
    AlreadySucceeded,
}

impl<'enrolment> GitEffectService<'enrolment> {
    /// Assembles the service for one mission phase.
    ///
    /// `repo_root` must live inside the capability's root; that is checked
    /// here rather than at each effect, so a service that exists at all is
    /// already confined.
    ///
    /// # Errors
    /// Returns [`GitEffectError::UnsafeComponent`] when `repo_root` is not
    /// under the capability root, and [`GitEffectError::Capability`] when the
    /// root cannot be verified. If admission fails and the already-open ledger
    /// cannot prove a clean close, returns
    /// [`GitEffectError::SetupAndLedgerClose`] carrying both failures.
    pub fn new(
        ledger: GitEffectLedger,
        capability: &'enrolment GitEffectCapability,
        adapter: &'enrolment dyn PrAdapter,
        mission: MissionId,
        phase: PhaseId,
        repo_root: PathBuf,
    ) -> Result<Self, GitEffectError> {
        // Take structural ownership before admission. `RuntimeStore::Drop`
        // intentionally retains an unclean writer lease, so every error below
        // must go through `close_after_setup_error` rather than dropping the
        // by-value ledger on a `?` return.
        let mut service = Self {
            store: ledger.store,
            capability,
            adapter,
            mission,
            phase,
            repo_root,
            logical_attempt: 1,
            isolation: None,
            refreshed: None,
            barrier: None,
        };

        match service.admit_repository() {
            Ok(canonical_repo) => {
                service.repo_root = canonical_repo;
                Ok(service)
            }
            Err(setup) => Err(service.close_after_setup_error(setup)),
        }
    }

    fn admit_repository(&self) -> Result<PathBuf, GitEffectError> {
        self.capability.verify()?;
        let canonical_root = std::fs::canonicalize(self.capability.root())
            .map_err(|source| GitEffectError::Git(GitError::Io(source)))?;
        let canonical_repo = std::fs::canonicalize(&self.repo_root)
            .map_err(|source| GitEffectError::Git(GitError::Io(source)))?;
        if !canonical_repo.starts_with(&canonical_root) {
            return Err(GitEffectError::UnsafeComponent);
        }
        Ok(canonical_repo)
    }

    fn close_after_setup_error(mut self, setup: GitEffectError) -> GitEffectError {
        match self.store.close_in_place() {
            Ok(()) => setup,
            Err(close) => GitEffectError::SetupAndLedgerClose {
                setup: Box::new(setup),
                close,
            },
        }
    }

    /// Takes the exact base the last governed `RefreshBase` resolved to.
    ///
    /// This is the **only** way a caller obtains a [`RefreshedBase`] from the
    /// service, and [`GitIntent::CreateIsolation`] consumes one by value, so an
    /// isolation branch can only ever be cut from a base a completed, durable
    /// refresh declared. It is taken rather than borrowed so a single refresh
    /// cannot authorize two isolations.
    #[must_use]
    pub fn take_refreshed_base(&mut self) -> Option<RefreshedBase> {
        self.refreshed.take()
    }

    const fn ledger(&self) -> &PrivateProcessLedgerStore {
        &self.store
    }

    const fn ledger_mut(&mut self) -> &mut PrivateProcessLedgerStore {
        &mut self.store
    }

    /// Installs a fixture-only observer of the durable boundaries in
    /// B4-DESIGN §4.
    ///
    /// The observer cannot change what the service does; it is called and its
    /// return value discarded. A gate uses it to hold a real process at an
    /// exact boundary so the parent can cut it abruptly.
    #[cfg(any(test, feature = "test-support"))]
    pub fn observe_barriers(&mut self, observer: GitBarrierObserver) {
        self.barrier = Some(observer);
    }

    fn barrier(&self, point: GitEffectBarrier, slot: &'static str) -> Result<(), GitEffectError> {
        match self.barrier.as_ref().map(|observer| observer(point, slot)) {
            Some(GitBarrierDecision::CutHere) => Err(GitEffectError::FixtureCut { slot, point }),
            None | Some(GitBarrierDecision::Continue) => Ok(()),
        }
    }

    /// The capability bounding every effect this service performs.
    ///
    /// Returned by reference so a caller can mint a [`TrashRoot`] under the
    /// same root without being handed the root path itself — `TrashRoot::under`
    /// is the only thing that can turn this borrow into a filesystem
    /// destination, and it accepts only a single safe component.
    #[must_use]
    pub const fn capability(&self) -> &GitEffectCapability {
        self.capability
    }

    /// The isolation branch this run created or adopted, if any.
    #[must_use]
    pub fn isolation_branch(&self) -> Option<&str> {
        self.isolation.as_ref().map(|state| state.branch.as_str())
    }

    /// The linked worktree this run created or adopted, if any.
    #[must_use]
    pub fn isolation_worktree(&self) -> Option<&Path> {
        self.isolation
            .as_ref()
            .map(|state| state.worktree.as_path())
    }

    /// Applies one governed Git intent.
    ///
    /// The whole sequence is: verify the capability, journal the plan, claim it
    /// exactly, execute idempotently, record the identity of the git child that
    /// ran, resolve. A slot whose row already succeeded is **not** executed
    /// again — its receipt is re-derived by observing the repository, which is
    /// how a resumed run reaches the same receipts with zero duplicates.
    ///
    /// # Errors
    /// Returns [`GitEffectError`] for a refused capability, a durable-store
    /// failure, a git failure, or an observation that proves the repository is
    /// in a state this run must not overwrite.
    pub fn apply(&mut self, intent: GitIntent) -> Result<GitReceipt, GitEffectError> {
        self.capability.verify()?;
        match intent {
            GitIntent::RefreshBase {
                remote,
                base_branch,
            } => self.apply_refresh_base(&remote, &base_branch),
            GitIntent::CreateIsolation {
                base,
                mission_id,
                task,
            } => self.apply_create_isolation(&base, &mission_id, &task),
            GitIntent::Commit { message, work } => self.apply_commit(&message, &work),
            GitIntent::Push { remote, branch } => self.apply_push(&remote, &branch),
            GitIntent::OpenPr { title, body, draft } => self.apply_open_pr(&title, &body, draft),
            GitIntent::Cleanup { trash } => self.apply_cleanup(&trash),
            GitIntent::Rollback { of } => self.apply_rollback(&of),
        }
    }

    // -- §2, C1 -------------------------------------------------------------

    fn apply_refresh_base(
        &mut self,
        remote: &str,
        base_branch: &str,
    ) -> Result<GitReceipt, GitEffectError> {
        if !self.capability.admits_remote(remote) {
            return Err(GitEffectError::RemoteNotEnrolled);
        }
        let payload = json!({
            "v": PLAN_PAYLOAD_VERSION,
            "remote": remote,
            "base_branch": base_branch,
        });
        match self.begin(SLOT_REFRESH_BASE, KIND_REFRESH_BASE, &payload)? {
            SlotEntry::AlreadySucceeded => {
                // C1: the refresh already happened. **Never re-fetch** — a
                // second fetch could observe an advanced remote and silently
                // move the base out from under a mission that already declared
                // one. The remote-tracking ref is the durable record of the
                // completed fetch, so the exact base is recovered by reading
                // it.
                let base = orchestrator_git::recorded_refreshed_base(
                    &self.repo_root,
                    remote,
                    base_branch,
                )?;
                let receipt = base_receipt(&base);
                self.refreshed = Some(base);
                Ok(receipt)
            }
            SlotEntry::Claimed { key, claim_attempt } => {
                orchestrator_git::forget_supervised_child_identity();
                let outcome = orchestrator_git::resolve_refreshed_base(
                    &self.repo_root,
                    remote,
                    base_branch,
                    RemotePolicy::LocalOnly,
                );
                let base = self.finish(&key, SLOT_REFRESH_BASE, claim_attempt, outcome)?;
                let receipt = base_receipt(&base);
                self.refreshed = Some(base);
                Ok(receipt)
            }
        }
    }

    // -- §2, C2/C3 ----------------------------------------------------------

    fn apply_create_isolation(
        &mut self,
        base: &RefreshedBase,
        mission_id: &MissionId,
        task: &str,
    ) -> Result<GitReceipt, GitEffectError> {
        let branch = orchestrator_git::branch_name(mission_id.as_str(), task);
        let worktree = self.worktree_path(mission_id, task);
        let payload = json!({
            "v": PLAN_PAYLOAD_VERSION,
            "branch": branch,
            "base_sha": base.base_sha().as_str(),
        });
        // The executor is idempotent, so the already-succeeded path re-runs
        // exactly the same adoption rather than a second, divergent one; the
        // only difference is that a succeeded row is never re-claimed.
        let claimed_files =
            match self.begin(SLOT_CREATE_ISOLATION, KIND_CREATE_ISOLATION, &payload)? {
                SlotEntry::Claimed { key, claim_attempt } => {
                    orchestrator_git::forget_supervised_child_identity();
                    let outcome =
                        self.establish_isolation(&branch, base.base_sha(), &worktree, base);
                    self.finish(&key, SLOT_CREATE_ISOLATION, claim_attempt, outcome)?
                }
                SlotEntry::AlreadySucceeded => self.adopt_isolation(&branch, &worktree)?,
            };
        self.isolation = Some(Isolation {
            branch: branch.clone(),
            base_branch: base.base_branch().to_owned(),
            worktree: worktree.clone(),
        });
        Ok(GitReceipt::IsolationCreated {
            branch,
            base_sha: base.base_sha().clone(),
            worktree,
            lock_pid: std::process::id(),
            claimed_files,
        })
    }

    /// The idempotent isolation executor, used by the first run and by every
    /// replay after a crash.
    ///
    /// Branch absent ⇒ create it at the exact base sha. Branch present at that
    /// sha ⇒ adopt. Branch present at a **different** sha ⇒ refuse: a branch
    /// carrying unknown work is never deleted or force-moved. Worktree
    /// registered ⇒ adopt. Directory present but unregistered ⇒ repair **once**
    /// and fail if that does not register it. Never a second worktree at a
    /// different path for the same branch.
    fn establish_isolation(
        &self,
        branch: &str,
        base_sha: &CommitSha,
        worktree: &Path,
        base: &RefreshedBase,
    ) -> Result<usize, GitEffectError> {
        match orchestrator_git::branch_sha(&self.repo_root, branch)? {
            None => {
                orchestrator_git::create_branch_from_refreshed_base(&self.repo_root, branch, base)?;
                // C2: the branch exists and its worktree does not.
                self.barrier(
                    GitEffectBarrier::IsolationBranchBeforeWorktree,
                    SLOT_CREATE_ISOLATION,
                )?;
            }
            Some(observed) if &observed == base_sha => {}
            Some(_) => return Err(GitEffectError::BranchCarriesUnknownWork),
        }
        let claimed = self.adopt_isolation(branch, worktree)?;
        // C3: the worktree is registered and locked, and the row is still
        // `Executing`.
        self.barrier(
            GitEffectBarrier::IsolationWorktreeBeforeResolution,
            SLOT_CREATE_ISOLATION,
        )?;
        Ok(claimed)
    }

    /// Registers, repairs, or creates the worktree for an existing branch, then
    /// takes the lock and the file claims.
    fn adopt_isolation(&self, branch: &str, worktree: &Path) -> Result<usize, GitEffectError> {
        let registered = orchestrator_git::worktree_is_registered(&self.repo_root, worktree)?;
        if !registered {
            if worktree.exists() {
                // C3: a directory that looks like a worktree but is not
                // registered. Repair once; a still-unregistered path is a
                // terminal refusal, never a second worktree elsewhere.
                orchestrator_git::repair_worktree(&self.repo_root, worktree)?;
                if !orchestrator_git::worktree_is_registered(&self.repo_root, worktree)? {
                    return Err(GitEffectError::BranchCarriesUnknownWork);
                }
            } else {
                if let Some(parent) = worktree.parent() {
                    std::fs::create_dir_all(parent).map_err(GitError::Io)?;
                }
                orchestrator_git::create_worktree(&self.repo_root, worktree, branch)?;
            }
        }
        // Adoption re-takes the lock with *this* process's PID and re-takes the
        // claims, so a resumed run owns what it adopted.
        orchestrator_git::write_lock(worktree, self.mission.as_str())?;
        self.take_claims(worktree)
    }

    fn take_claims(&self, worktree: &Path) -> Result<usize, GitEffectError> {
        let files = orchestrator_git::claim_changed_files(None, Some(worktree), None, None)?;
        let mut claims =
            orchestrator_git::open_claims_db(Some(&self.capability.root().join(CLAIMS_DB_FILE)))?;
        let repo_root = self.repo_root.to_string_lossy().into_owned();
        claims.update_file_claims_with_files(self.mission.as_str(), &repo_root, &files)?;
        claims.close()?;
        Ok(files.len())
    }

    // -- C4 -----------------------------------------------------------------

    fn apply_commit(
        &mut self,
        message: &str,
        work: &VerifiedWork,
    ) -> Result<GitReceipt, GitEffectError> {
        if work.phase() != &self.phase {
            return Err(GitEffectError::WitnessPhaseMismatch);
        }
        let isolation = self
            .isolation
            .clone()
            .ok_or(GitEffectError::MissingIsolation)?;
        let parent = head_commit(&isolation.worktree)?;
        let payload = json!({
            "v": PLAN_PAYLOAD_VERSION,
            "branch": isolation.branch,
            "parent_sha": parent.as_str(),
        });
        match self.begin(SLOT_COMMIT, KIND_COMMIT, &payload)? {
            SlotEntry::AlreadySucceeded => {
                let head = head_commit(&isolation.worktree)?;
                Ok(GitReceipt::Committed {
                    changed_files: self.count_changed(&parent, &head)?,
                    commit_sha: head,
                    parent_sha: parent,
                })
            }
            SlotEntry::Claimed { key, claim_attempt } => {
                orchestrator_git::forget_supervised_child_identity();
                let outcome = orchestrator_git::commit_all(&isolation.worktree, message);
                self.finish(&key, SLOT_COMMIT, claim_attempt, outcome)?;
                let head = head_commit(&isolation.worktree)?;
                Ok(GitReceipt::Committed {
                    changed_files: self.count_changed(&parent, &head)?,
                    commit_sha: head,
                    parent_sha: parent,
                })
            }
        }
    }

    fn count_changed(&self, parent: &CommitSha, head: &CommitSha) -> Result<usize, GitEffectError> {
        if parent == head {
            return Ok(0);
        }
        Ok(orchestrator_git::changed_files(&self.repo_root, parent.as_str(), head.as_str())?.len())
    }

    // -- §4.3, C5 -----------------------------------------------------------

    fn apply_push(&mut self, remote: &str, branch: &str) -> Result<GitReceipt, GitEffectError> {
        if !self.capability.admits_remote(remote) {
            return Err(GitEffectError::RemoteNotEnrolled);
        }
        let isolation = self
            .isolation
            .clone()
            .ok_or(GitEffectError::MissingIsolation)?;
        let head = head_commit(&isolation.worktree)?;
        let payload = json!({
            "v": PLAN_PAYLOAD_VERSION,
            "remote": remote,
            "branch": branch,
            "sha": head.as_str(),
        });
        match self.begin(SLOT_PUSH, KIND_PUSH, &payload)? {
            SlotEntry::AlreadySucceeded => Ok(GitReceipt::Pushed {
                remote: remote.to_owned(),
                branch: branch.to_owned(),
                ack: PushAck::Acknowledged { remote_sha: head },
            }),
            SlotEntry::Claimed { key, claim_attempt } => {
                orchestrator_git::forget_supervised_child_identity();
                let ack = orchestrator_git::push_with_acknowledgement(
                    &isolation.worktree,
                    remote,
                    branch,
                    &head,
                    RemotePolicy::LocalOnly,
                )?;
                self.barrier(GitEffectBarrier::ExecutedBeforeResolution, SLOT_PUSH)?;
                let identity = self.execution_identity(claim_attempt)?;
                self.ledger_mut()
                    .record_execution_identity(&key, &identity)?;
                let resolution = match &ack {
                    // §4.3.1: acknowledgement requires two facts — a zero exit
                    // **and** an `ls-remote` showing the exact sha. That is
                    // already what `push_with_acknowledgement` enforces.
                    PushAck::Acknowledged { .. } => {
                        EffectResolution::Succeeded(EffectEvidence::exit_observed_success())
                    }
                    // §4.3.2: a decided rejection. The acknowledgement
                    // classifier does not retain git's exit code, so the
                    // evidence carries the fact of a nonzero exit rather than
                    // a code it did not observe.
                    PushAck::Rejected => {
                        EffectResolution::Failed(EffectEvidence::exit_observed_failure(1)?)
                    }
                    // §4.3.3: signal, deadline, stall, cancellation, or an
                    // unobservable `ls-remote`. Nothing may be concluded.
                    PushAck::Ambiguous => {
                        EffectResolution::Uncertain(EffectEvidence::remote_state_unobservable())
                    }
                };
                self.ledger_mut()
                    .resolve_effect(&key, resolution, &now_utc())?;
                self.barrier(GitEffectBarrier::SlotResolved, SLOT_PUSH)?;
                Ok(GitReceipt::Pushed {
                    remote: remote.to_owned(),
                    branch: branch.to_owned(),
                    ack,
                })
            }
        }
    }

    // -- §3, C6 -------------------------------------------------------------

    fn apply_open_pr(
        &mut self,
        title: &str,
        body: &str,
        draft: bool,
    ) -> Result<GitReceipt, GitEffectError> {
        let isolation = self
            .isolation
            .clone()
            .ok_or(GitEffectError::MissingIsolation)?;
        let head = head_commit(&isolation.worktree)?;
        let request = PrRequest {
            repo_root: self.repo_root.clone(),
            base_branch: isolation.base_branch.clone(),
            head_branch: isolation.branch.clone(),
            head_sha: head.clone(),
            title: title.to_owned(),
            body: body.to_owned(),
            draft,
        };
        let receipt_id = self.adapter.receipt_id(&request)?;
        let payload = json!({
            "v": PLAN_PAYLOAD_VERSION,
            "provider": self.adapter.provider(),
            "receipt_id": receipt_id,
            "head_sha": head.as_str(),
        });
        match self.begin(SLOT_OPEN_PR, KIND_OPEN_PR, &payload)? {
            SlotEntry::AlreadySucceeded => {
                // The id is a pure function of the head, so no second pull
                // request is representable.
                let existing = self
                    .adapter
                    .lookup(&receipt_id)?
                    .ok_or(GitEffectError::PlanRowMissing { slot: SLOT_OPEN_PR })?;
                Ok(pr_receipt(&existing, head))
            }
            SlotEntry::Claimed { key, claim_attempt } => {
                orchestrator_git::forget_supervised_child_identity();
                let opened = self.adapter.open(&request);
                let opened = self.finish(&key, SLOT_OPEN_PR, claim_attempt, opened)?;
                Ok(pr_receipt(&opened, head))
            }
        }
    }

    // -- §5.1 ---------------------------------------------------------------

    fn apply_cleanup(&mut self, trash: &TrashRoot) -> Result<GitReceipt, GitEffectError> {
        let isolation = self
            .isolation
            .clone()
            .ok_or(GitEffectError::MissingIsolation)?;
        let payload = json!({
            "v": PLAN_PAYLOAD_VERSION,
            "branch": isolation.branch,
        });
        match self.begin(SLOT_CLEANUP, KIND_CLEANUP, &payload)? {
            SlotEntry::AlreadySucceeded => Ok(GitReceipt::CleanedUp {
                trash_entry: trash.path().to_path_buf(),
            }),
            SlotEntry::Claimed { key, claim_attempt } => {
                orchestrator_git::forget_supervised_child_identity();
                let outcome = self.trash_worktree(&isolation.worktree, trash);
                let trash_entry = self.finish(&key, SLOT_CLEANUP, claim_attempt, outcome)?;
                self.isolation = None;
                Ok(GitReceipt::CleanedUp { trash_entry })
            }
        }
    }

    /// Releases this run's own liveness lock before confining and renaming.
    ///
    /// The lock exists so *another* process does not remove a live worktree;
    /// leaving it in place would make a run unable to clean up after itself,
    /// because `is_locked` would see this process's own live PID.
    fn trash_worktree(&self, worktree: &Path, trash: &TrashRoot) -> Result<PathBuf, GitError> {
        orchestrator_git::remove_lock(worktree);
        orchestrator_git::remove_worktree_confined(worktree, trash.path(), self.capability.root())
    }

    // -- §5.2 ---------------------------------------------------------------

    fn apply_rollback(&mut self, of: &GitReceipt) -> Result<GitReceipt, GitEffectError> {
        // Rollbackability is decided **before** anything is journaled, so a
        // refused rollback never leaves a durable row behind.
        match of {
            GitReceipt::Committed { .. }
            | GitReceipt::Pushed { .. }
            | GitReceipt::PrOpened { .. }
            | GitReceipt::RolledBack { .. } => return Err(GitEffectError::NotRollbackable),
            GitReceipt::BaseRefreshed { .. }
            | GitReceipt::CleanedUp { .. }
            | GitReceipt::IsolationCreated { .. } => {}
        }
        let payload = json!({
            "v": PLAN_PAYLOAD_VERSION,
            "of_slot": of.slot(),
        });
        let SlotEntry::Claimed { key, claim_attempt } =
            self.begin(SLOT_ROLLBACK, KIND_ROLLBACK, &payload)?
        else {
            return Ok(GitReceipt::RolledBack {
                slot: of.slot(),
                action: RollbackAction::NoEffectToUndo,
            });
        };
        orchestrator_git::forget_supervised_child_identity();
        let outcome = self.undo(of);
        let action = self.finish(&key, SLOT_ROLLBACK, claim_attempt, outcome)?;
        Ok(GitReceipt::RolledBack {
            slot: of.slot(),
            action,
        })
    }

    fn undo(&mut self, of: &GitReceipt) -> Result<RollbackAction, GitEffectError> {
        match of {
            GitReceipt::IsolationCreated {
                branch,
                base_sha,
                worktree,
                ..
            } => {
                let trash = TrashRoot::under(self.capability, "trash")?;
                let trash_entry = self.trash_worktree(worktree, &trash)?;
                // `delete_branch_at` performs the sha check and the deletion in
                // one `update-ref -d <ref> <oldvalue>`, so a ref that moves
                // between observation and deletion is refused by git itself
                // rather than by a check-then-act here.
                if !orchestrator_git::delete_branch_at(&self.repo_root, branch, base_sha)? {
                    return Err(GitEffectError::WouldDiscardWork);
                }
                self.isolation = None;
                Ok(RollbackAction::WorktreeTrashedAndBranchDeleted { trash_entry })
            }
            // Refreshing a remote-tracking ref applies nothing to the working
            // repository, and a trash entry stands as the recovery artifact.
            // Both are observed rather than assumed, so the durable row still
            // carries the identity of a real supervised child.
            GitReceipt::BaseRefreshed { .. } | GitReceipt::CleanedUp { .. } => {
                let _observed = orchestrator_git::head_sha(&self.repo_root)?;
                Ok(RollbackAction::NoEffectToUndo)
            }
            GitReceipt::Committed { .. }
            | GitReceipt::Pushed { .. }
            | GitReceipt::PrOpened { .. }
            | GitReceipt::RolledBack { .. } => Err(GitEffectError::NotRollbackable),
        }
    }

    // -- §4 — reconciliation ------------------------------------------------

    /// Reconciles every crashed Git effect for this mission.
    ///
    /// Reads the only durable rows that can be mid-flight (`Executing` and
    /// `Uncertain`), asks the **repository** what actually happened, and
    /// resolves each row along the one reachable ladder. Reconciliation only
    /// ever observes: it never creates, deletes, force-moves, or re-pushes.
    /// A row it returns to `Pending` is re-executed idempotently by the next
    /// [`Self::apply`] for that slot.
    ///
    /// # Errors
    /// Returns [`GitEffectError`] when the store cannot be read or a resolution
    /// is refused.
    pub fn reconcile(&mut self) -> Result<Vec<Reconciliation>, GitEffectError> {
        self.capability.verify()?;
        let rows: Vec<OutboxEffect> = self
            .ledger()
            .recovery_effects(EFFECT_PAGE)?
            .into_iter()
            .filter(|effect| {
                effect.effect_kind() == OutboxEffectKind::GitCommand
                    && effect.mission_id() == &self.mission
            })
            .collect();
        let mut reconciled = Vec::with_capacity(rows.len());
        for row in rows {
            let slot = row.operation_slot().as_str().to_owned();
            let disposition = self.reconcile_one(&row)?;
            reconciled.push(Reconciliation { slot, disposition });
        }
        Ok(reconciled)
    }

    fn reconcile_one(
        &mut self,
        row: &OutboxEffect,
    ) -> Result<ReconciliationDisposition, GitEffectError> {
        match row.operation_slot().as_str() {
            SLOT_REFRESH_BASE => self.reconcile_refresh_base(row),
            SLOT_CREATE_ISOLATION => self.reconcile_create_isolation(row),
            SLOT_COMMIT => self.reconcile_commit(row),
            SLOT_PUSH => self.reconcile_push(row),
            SLOT_OPEN_PR => self.reconcile_open_pr(row),
            // Cleanup and rollback are both renames into a confined trash
            // directory. A crash mid-rename leaves either the original or the
            // entry, never both, and the executor is idempotent, so the row is
            // simply requeued for one re-claim.
            _ => self.requeue(row),
        }
    }

    /// C1. A crash *during* the fetch. The tracking ref is the durable record
    /// of a completed fetch: present ⇒ the fetch landed; absent ⇒ it did not.
    /// A row that already reached `Succeeded` is not in `recovery_effects` at
    /// all, so this never re-fetches a base that was already declared.
    ///
    /// Only an absent tracking ref is evidence that the fetch did not land, so
    /// only [`GitError::BaseRefMissing`] requeues. Every other failure — an
    /// unreadable repository, a refused remote, a git child that could not be
    /// run — says nothing about whether the fetch completed, and requeueing on
    /// it would authorize a second fetch that could move the base out from
    /// under a mission that already declared one. Those surface.
    fn reconcile_refresh_base(
        &mut self,
        row: &OutboxEffect,
    ) -> Result<ReconciliationDisposition, GitEffectError> {
        let remote = plan_string(row, "remote")?;
        let base_branch = plan_string(row, "base_branch")?;
        orchestrator_git::forget_supervised_child_identity();
        match orchestrator_git::recorded_refreshed_base(&self.repo_root, &remote, &base_branch) {
            Ok(_) => self.succeed(row),
            Err(GitError::BaseRefMissing { .. }) => self.requeue(row),
            Err(other) => Err(GitEffectError::Git(other)),
        }
    }

    /// C2 and C3. The branch and the worktree registration are the evidence.
    fn reconcile_create_isolation(
        &mut self,
        row: &OutboxEffect,
    ) -> Result<ReconciliationDisposition, GitEffectError> {
        let branch = plan_string(row, "branch")?;
        let base_sha = plan_commit(row, "base_sha")?;
        orchestrator_git::forget_supervised_child_identity();
        let observed = orchestrator_git::branch_sha(&self.repo_root, &branch)?;
        match observed {
            // C2: never delete or force-move a branch that carries unknown
            // work.
            Some(observed) if observed != base_sha => self.fail(row),
            Some(_) => {
                let worktree = self.worktree_path_for_branch(&branch);
                if orchestrator_git::worktree_is_registered(&self.repo_root, &worktree)?
                    && orchestrator_git::is_locked(&worktree)
                {
                    self.succeed(row)
                } else {
                    // C3: a registered-but-unlocked or absent worktree is
                    // re-established by the idempotent executor, which adopts
                    // rather than creating a second worktree.
                    self.requeue(row)
                }
            }
            None => self.requeue(row),
        }
    }

    /// C4. The head is the evidence; a dirty worktree behind a moved head is a
    /// partial stage and is the one case that must not be papered over.
    fn reconcile_commit(
        &mut self,
        row: &OutboxEffect,
    ) -> Result<ReconciliationDisposition, GitEffectError> {
        let branch = plan_string(row, "branch")?;
        let parent = plan_commit(row, "parent_sha")?;
        let worktree = self.worktree_path_for_branch(&branch);
        orchestrator_git::forget_supervised_child_identity();
        let head = orchestrator_git::branch_sha(&self.repo_root, &branch)?;
        let moved = head.is_some_and(|head| head != parent);
        if !moved {
            // The commit did not land. B4 persists no attempt outcome, so a
            // `VerifiedWork` witness cannot be re-minted after a crash and the
            // row is resolved `NotStarted` rather than committing unverified
            // work. The run's replay re-mints the witness from the re-executed
            // phase.
            self.ledger_mut().resolve_effect(
                row.idempotency_key(),
                EffectResolution::NotStarted(EffectEvidence::process_not_started(
                    ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn,
                )),
                &now_utc(),
            )?;
            return Ok(ReconciliationDisposition::FailedTerminally);
        }
        if orchestrator_git::has_uncommitted_changes(&worktree).unwrap_or(true) {
            return self.fail(row);
        }
        self.succeed(row)
    }

    /// C5. `ls-remote` is the evidence, and an unobservable remote keeps the
    /// row `Uncertain` rather than authorizing a blind re-push.
    fn reconcile_push(
        &mut self,
        row: &OutboxEffect,
    ) -> Result<ReconciliationDisposition, GitEffectError> {
        let remote = plan_string(row, "remote")?;
        let branch = plan_string(row, "branch")?;
        let pushed = plan_commit(row, "sha")?;
        orchestrator_git::forget_supervised_child_identity();
        match orchestrator_git::ls_remote_branch(
            &self.repo_root,
            &remote,
            &branch,
            RemotePolicy::LocalOnly,
        ) {
            Ok(Some(remote_sha)) if remote_sha == pushed => self.succeed(row),
            Ok(Some(_)) => self.fail(row),
            Ok(None) => self.requeue(row),
            // A remote the policy refuses is not an unobservable remote: the
            // observation was never attempted, so nothing about remote state
            // may be concluded and the refusal surfaces instead.
            Err(refusal @ GitError::RemoteNotLocal { .. }) => Err(GitEffectError::Git(refusal)),
            Err(_) => {
                if row.state() == OutboxState::Uncertain {
                    // Already uncertain and still unobservable: retain it. The
                    // ladder has no path from here to a re-push.
                    return Ok(ReconciliationDisposition::RetainedUncertain);
                }
                let identity = self.execution_identity(row.attempts())?;
                self.ledger_mut()
                    .record_execution_identity(row.idempotency_key(), &identity)?;
                self.ledger_mut().resolve_effect(
                    row.idempotency_key(),
                    EffectResolution::Uncertain(EffectEvidence::remote_state_unobservable()),
                    &now_utc(),
                )?;
                Ok(ReconciliationDisposition::RetainedUncertain)
            }
        }
    }

    /// C6. The adapter's ledger is the evidence, and the receipt id is a pure
    /// function of the head, so no second pull request is representable.
    fn reconcile_open_pr(
        &mut self,
        row: &OutboxEffect,
    ) -> Result<ReconciliationDisposition, GitEffectError> {
        let receipt_id = plan_string(row, "receipt_id")?;
        orchestrator_git::forget_supervised_child_identity();
        if self.adapter.lookup(&receipt_id)?.is_some() {
            // An identity is still required for `Succeeded`, and the adapter
            // lookup is a file read rather than a git child, so one read-only
            // observation is taken against the repository.
            let _observed = orchestrator_git::head_sha(&self.repo_root)?;
            return self.succeed(row);
        }
        self.requeue(row)
    }

    fn succeed(&mut self, row: &OutboxEffect) -> Result<ReconciliationDisposition, GitEffectError> {
        let identity = self.execution_identity(row.attempts())?;
        self.ledger_mut()
            .record_execution_identity(row.idempotency_key(), &identity)?;
        self.ledger_mut().resolve_effect(
            row.idempotency_key(),
            EffectResolution::Succeeded(EffectEvidence::exit_observed_success()),
            &now_utc(),
        )?;
        Ok(ReconciliationDisposition::AdoptedObservedSuccess)
    }

    fn fail(&mut self, row: &OutboxEffect) -> Result<ReconciliationDisposition, GitEffectError> {
        let identity = self.execution_identity(row.attempts())?;
        self.ledger_mut()
            .record_execution_identity(row.idempotency_key(), &identity)?;
        self.ledger_mut().resolve_effect(
            row.idempotency_key(),
            EffectResolution::Failed(EffectEvidence::exit_observed_failure(1)?),
            &now_utc(),
        )?;
        Ok(ReconciliationDisposition::FailedTerminally)
    }

    /// Drives one row back to `Pending` along the only reachable path.
    ///
    /// `Executing → Uncertain(exit_status_lost) → ObservedAbsent → Pending`.
    /// `Failed → RetryFailed` is not reachable — it needs a
    /// `RetryAuthorization` this batch cannot mint — so `Failed` stays reserved
    /// for refusals that must stop the run.
    fn requeue(&mut self, row: &OutboxEffect) -> Result<ReconciliationDisposition, GitEffectError> {
        let key = row.idempotency_key().to_owned();
        if row.state() == OutboxState::Executing {
            let identity = self.execution_identity(row.attempts())?;
            self.ledger_mut()
                .record_execution_identity(&key, &identity)?;
            self.ledger_mut().resolve_effect(
                &key,
                EffectResolution::Uncertain(EffectEvidence::exit_status_lost()),
                &now_utc(),
            )?;
        }
        self.ledger_mut().resolve_effect(
            &key,
            EffectResolution::ObservedAbsent(EffectEvidence::observed_absent()),
            &now_utc(),
        )?;
        Ok(ReconciliationDisposition::ObservedAbsentAndRequeued)
    }

    // -- §1.4 — durable plumbing --------------------------------------------

    /// Journals the plan when it is not already durable, then claims it.
    ///
    /// A crash between `append` and `claim_exact` leaves a `Pending` row that
    /// `recovery_effects` cannot see, so this checks for one before journaling:
    /// the sequence is idempotent by construction rather than by reconciliation.
    fn begin(
        &mut self,
        slot: &'static str,
        kind: &'static str,
        payload: &Value,
    ) -> Result<SlotEntry, GitEffectError> {
        let key = self.slot_key(slot, payload)?;
        match self.slot_state(&key)? {
            OutboxState::Succeeded => return Ok(SlotEntry::AlreadySucceeded),
            OutboxState::Failed => return Err(GitEffectError::FailedTerminally { slot }),
            OutboxState::Executing | OutboxState::Uncertain => {
                return Err(GitEffectError::RequiresReconciliation { slot });
            }
            OutboxState::Pending => {}
        }
        if !self.row_exists(&key)? {
            let transition = format!("{}-{slot}-{}", self.mission, self.logical_attempt);
            // No compatibility projection is declared, so `append` writes
            // `command_ack` in the same transaction and the row is immediately
            // claimable (B4-DESIGN Risk 2).
            let intent = JournalIntent::new(
                transition,
                Some(self.mission.clone()),
                kind,
                payload.clone(),
                now_utc(),
            )?
            .with_outbox(self.outbox_intent(slot, payload)?)?;
            let _commit = self.ledger_mut().append(&intent)?;
        }
        let claim = self.ledger_mut().claim_exact(&key, &now_utc())?;
        self.barrier(GitEffectBarrier::ClaimedBeforeExecution, slot)?;
        Ok(SlotEntry::Claimed {
            claim_attempt: claim.claim_attempt(),
            key,
        })
    }

    /// Records the identity of the git child that ran, then resolves the row.
    ///
    /// A failing execution is resolved `Failed` before the error is returned,
    /// so a refused effect never leaves an `Executing` row behind for a
    /// reconciler that has nothing to observe.
    fn finish<T, E: Into<GitEffectError>>(
        &mut self,
        key: &str,
        slot: &'static str,
        claim_attempt: u32,
        outcome: Result<T, E>,
    ) -> Result<T, GitEffectError> {
        let error: GitEffectError = match outcome {
            Ok(value) => {
                self.barrier(GitEffectBarrier::ExecutedBeforeResolution, slot)?;
                let identity = self.execution_identity(claim_attempt)?;
                self.ledger_mut()
                    .record_execution_identity(key, &identity)?;
                self.ledger_mut().resolve_effect(
                    key,
                    EffectResolution::Succeeded(EffectEvidence::exit_observed_success()),
                    &now_utc(),
                )?;
                self.barrier(GitEffectBarrier::SlotResolved, slot)?;
                return Ok(value);
            }
            Err(error) => error.into(),
        };
        // A fixture cut is not a failure: it stands for a process that stopped
        // existing at this boundary, so it must leave the row exactly as a
        // crash would — `Executing`, with no execution identity and no
        // observation — for the reconciler to find.
        if matches!(error, GitEffectError::FixtureCut { .. }) {
            return Err(error);
        }
        let identity = self.execution_identity(claim_attempt)?;
        self.ledger_mut()
            .record_execution_identity(key, &identity)?;
        self.ledger_mut().resolve_effect(
            key,
            EffectResolution::Failed(EffectEvidence::exit_observed_failure(1)?),
            &now_utc(),
        )?;
        Err(error)
    }

    /// The identity of the git child this effect's execution supervised.
    ///
    /// When the effect spawned none — an adapter that only reads a file, for
    /// example — one read-only observation is taken against the repository so
    /// the durable row carries a real supervised child rather than an identity
    /// that was never observed.
    fn execution_identity(&self, attempt: u32) -> Result<ProcessExecutionIdentity, GitEffectError> {
        let observed = match orchestrator_git::last_supervised_child_identity() {
            Some(identity) => identity,
            None => {
                let _probe = orchestrator_git::head_sha(&self.repo_root)?;
                orchestrator_git::last_supervised_child_identity()
                    .ok_or(GitEffectError::ExecutionIdentityUnavailable)?
            }
        };
        Ok(identity_from(&observed, attempt, &now_utc())?)
    }

    /// Derives the exact idempotency key for one slot without storing it.
    ///
    /// `OutboxIntent::for_mission` computes the key from
    /// `(mission, phase, GitCommand, slot, logical_attempt)`, and the intent is
    /// a pure value — building one performs no I/O — so a resumed process
    /// reconstructs the key it needs from identity it already holds.
    fn slot_key(&self, slot: &str, payload: &Value) -> Result<String, GitEffectError> {
        Ok(self
            .outbox_intent(slot, payload)?
            .idempotency_key()
            .to_owned())
    }

    fn outbox_intent(
        &self,
        slot: &str,
        payload: &Value,
    ) -> Result<OutboxIntent, RuntimeStoreError> {
        OutboxIntent::for_mission(
            self.mission.clone(),
            Some(self.phase.to_string()),
            OutboxEffectKind::GitCommand,
            EffectOperationSlot::new(slot)?,
            self.logical_attempt,
            payload.clone(),
        )
    }

    /// The durable state of one slot.
    ///
    /// A key with no row anywhere reads as `Pending`, which is exactly what
    /// [`Self::begin`] wants: journal it, then claim it.
    fn slot_state(&self, key: &str) -> Result<OutboxState, GitEffectError> {
        if let Some(effect) = self.find_effect(key)? {
            return Ok(effect.state());
        }
        Ok(self
            .ledger()
            .attempt_history(key, EFFECT_PAGE)?
            .last()
            .map_or(OutboxState::Pending, |observation| observation.state()))
    }

    fn row_exists(&self, key: &str) -> Result<bool, GitEffectError> {
        Ok(self.find_effect(key)?.is_some())
    }

    fn find_effect(&self, key: &str) -> Result<Option<OutboxEffect>, GitEffectError> {
        for effect in self
            .ledger()
            .pending_effects(EFFECT_PAGE)?
            .into_iter()
            .chain(self.ledger().recovery_effects(EFFECT_PAGE)?)
        {
            if effect.idempotency_key() == key {
                return Ok(Some(effect));
            }
        }
        Ok(None)
    }

    /// The linked worktree path for one mission/task, always under the
    /// capability root so cleanup is confined by construction.
    fn worktree_path(&self, mission_id: &MissionId, task: &str) -> PathBuf {
        self.capability.root().join(WORKTREES_DIR).join(format!(
            "{}-{}",
            mission_id.as_str(),
            orchestrator_git::slugify(task)
        ))
    }

    /// The same path recomputed from a branch name, for reconciliation, which
    /// reads the branch out of a plan payload rather than the task text.
    ///
    /// `branch_name` is `via/<mission>/<slug>`, and both components are already
    /// slug-safe, so the directory name is recovered without re-slugifying.
    fn worktree_path_for_branch(&self, branch: &str) -> PathBuf {
        let stem = branch
            .strip_prefix("via/")
            .map_or_else(|| branch.replace('/', "-"), |rest| rest.replace('/', "-"));
        self.capability.root().join(WORKTREES_DIR).join(stem)
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

fn base_receipt(base: &RefreshedBase) -> GitReceipt {
    GitReceipt::BaseRefreshed {
        remote: base.remote().to_owned(),
        base_branch: base.base_branch().to_owned(),
        base_sha: base.base_sha().clone(),
        local_sha: base.local_sha().cloned(),
    }
}

fn pr_receipt(receipt: &PrReceipt, head_sha: CommitSha) -> GitReceipt {
    GitReceipt::PrOpened {
        provider: receipt.provider.clone(),
        receipt_id: receipt.receipt_id.clone(),
        url: receipt.url.clone(),
        head_sha,
    }
}

fn head_commit(dir: &Path) -> Result<CommitSha, GitEffectError> {
    let raw = orchestrator_git::head_sha(dir)?;
    CommitSha::parse(&raw).ok_or(GitEffectError::PlanPayloadUnreadable { slot: SLOT_COMMIT })
}

fn identity_from(
    observed: &GitChildIdentity,
    attempt: u32,
    now: &str,
) -> Result<ProcessExecutionIdentity, RuntimeStoreError> {
    ProcessExecutionIdentity::new(
        attempt,
        observed.pid(),
        observed.process_group_id(),
        observed.start_identity(),
        now,
    )
}

fn plan_string(row: &OutboxEffect, field: &'static str) -> Result<String, GitEffectError> {
    row.payload()
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(GitEffectError::PlanPayloadUnreadable {
            slot: slot_label(row),
        })
}

fn plan_commit(row: &OutboxEffect, field: &'static str) -> Result<CommitSha, GitEffectError> {
    let raw = plan_string(row, field)?;
    CommitSha::parse(&raw).ok_or(GitEffectError::PlanPayloadUnreadable {
        slot: slot_label(row),
    })
}

/// Maps a row's slot back to one of the seven `'static` labels so an error can
/// name it without allocating or echoing durable text.
fn slot_label(row: &OutboxEffect) -> &'static str {
    match row.operation_slot().as_str() {
        SLOT_REFRESH_BASE => SLOT_REFRESH_BASE,
        SLOT_CREATE_ISOLATION => SLOT_CREATE_ISOLATION,
        SLOT_COMMIT => SLOT_COMMIT,
        SLOT_PUSH => SLOT_PUSH,
        SLOT_OPEN_PR => SLOT_OPEN_PR,
        SLOT_CLEANUP => SLOT_CLEANUP,
        _ => SLOT_ROLLBACK,
    }
}

/// A single ordinary path component: no separator, no traversal, no dot-name.
fn safe_path_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// Current UTC time as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Implemented here rather than borrowed from `orchestrator-git`'s private
/// `time_util` so this module owns its only clock dependency; the civil-from-
/// days arithmetic is Howard Hinnant's and is overflow-safe on `i64`.
fn now_utc() -> String {
    let epoch_secs = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        Err(earlier) => -i64::try_from(earlier.duration().as_secs()).unwrap_or(i64::MAX),
    };
    let days = epoch_secs.div_euclid(86_400);
    let seconds = epoch_secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds / 3600;
    let minute = (seconds % 3600) / 60;
    let second = seconds % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_position = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_position + 2) / 5 + 1;
    let month = if month_position < 10 {
        month_position + 3
    } else {
        month_position - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_utc_is_the_canonical_shape_the_store_validates() {
        let stamp = now_utc();
        assert_eq!(stamp.len(), 20, "{stamp}");
        assert!(stamp.ends_with('Z'), "{stamp}");
        assert_eq!(stamp.as_bytes().get(10), Some(&b'T'), "{stamp}");
    }

    #[test]
    fn civil_from_days_matches_known_epochs() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(1), (1970, 1, 2));
        assert_eq!(civil_from_days(10_957), (2000, 1, 1));
    }

    #[test]
    fn trash_components_cannot_climb_out_of_a_root() {
        assert!(safe_path_component("trash"));
        assert!(safe_path_component("trash-1"));
        assert!(!safe_path_component(""));
        assert!(!safe_path_component("."));
        assert!(!safe_path_component(".."));
        assert!(!safe_path_component("a/b"));
        assert!(!safe_path_component("../outside"));
    }

    #[test]
    fn every_slot_is_a_valid_operation_slot() {
        for slot in [
            SLOT_REFRESH_BASE,
            SLOT_CREATE_ISOLATION,
            SLOT_COMMIT,
            SLOT_PUSH,
            SLOT_OPEN_PR,
            SLOT_CLEANUP,
            SLOT_ROLLBACK,
        ] {
            assert!(
                EffectOperationSlot::new(slot).is_ok(),
                "slot {slot} is not a valid operation slot"
            );
        }
    }
}
