//! Single-owner SQLite journal and side-effect outbox for the Rust runtime.
//!
//! `runtime.db` is Rust-private source-of-truth state. Go-compatible checkpoints,
//! event JSONL, sidecars, and existing databases remain projections; this module
//! never opens or rewrites them. Construction requires a retained production
//! boundary, so the connection cannot outlive the whole-home writer lease.

use crate::{
    ProductionBoundary,
    capability::CapabilityRoot,
    fs_util::{
        FileIdentity, create_private_file, identity, link_count, mode, open_file_nofollow, sync_dir,
    },
};
use orchestrator_core::{
    EventId, EventJsonMap, EventRecord, GO_EVENT_JSON_CONTENT_MAX_BYTES, MissionId, PhaseId,
    VerificationAction, VerificationClass, VerificationDecision, VerificationMode,
    VerificationOutcome, WorkerId, decide_verification, decode_event_line, encode_current_event,
};
use orchestrator_exec::{
    MechanicalTermination, ProcessExitStatus, ProcessPurpose, ProcessRequest,
    ProcessRequestFingerprint, RuntimeFamily, SessionHandle,
};
use orchestrator_process::{
    ExactProcessGroupAbsence, ProcessRequestBinding, ProcessStartGateRequest, ProcessStartedReceipt,
};
use rusqlite::{
    Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior, params,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::continuation::ContinuationDecisionSummary;
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};
use thiserror::Error;

const DATABASE_FILE: &str = "runtime.db";
const DATABASE_WAL_FILE: &str = "runtime.db-wal";
const DATABASE_SHM_FILE: &str = "runtime.db-shm";
const LEGACY_BOUNDARY_SEAL_FILE: &str = "runtime.boundary";
const COMPATIBILITY_BOUNDARY_SEAL_FILE: &str = "runtime.boundary-v1.compatibility";
const PRIVATE_PROCESS_LEDGER_BOUNDARY_SEAL_FILE: &str =
    "runtime.boundary-v1.private-process-ledger";
const LEGACY_RECORD_SCHEMA_VERSION: i64 = 1;
const RECORD_SCHEMA_VERSION: i64 = 2;
const LEGACY_DATABASE_SCHEMA_VERSION: i64 = 1;
const INTERMEDIATE_DATABASE_SCHEMA_VERSION: i64 = 2;
const PROCESS_DATABASE_SCHEMA_VERSION: i64 = 3;
const DATABASE_SCHEMA_VERSION: i64 = 7;
/// Schema v4 introduced the store boundary-kind column; retained as the named
/// migration source when an existing v4 store is upgraded to v5.
const BOUNDARY_DATABASE_SCHEMA_VERSION: i64 = 4;
/// Schema v5 introduced the private reasoning projection; retained as the named
/// migration source when an existing v5 store is upgraded to v6.
const REASONING_DATABASE_SCHEMA_VERSION: i64 = 5;
/// Schema v6 introduced the durable knowledge-publication queue; retained as
/// the named migration source when an existing v6 store is upgraded to v7.
///
/// B3-DESIGN §5 pins both the publication queue and the audit chain to "the
/// schema bump", written when B3 was one step. The queue landed first, so the
/// chain takes the next version rather than mutating v6's shape underneath a
/// store that already reports itself as v6.
const KNOWLEDGE_DATABASE_SCHEMA_VERSION: i64 = 6;
const LEGACY_SCHEMA_DIGEST: &str =
    "f7e0bcde7721b930132e4f9b75dbaa69140ce02e6fd10e0311b37a22a38b3580";
const NATIVE_SCHEMA_DIGEST: &str =
    "b861b097e0e694d5fb711717614150c866e5bcf64be974d62352e925341e23ac";
const MIGRATED_SCHEMA_DIGEST: &str =
    "fce8ea0625bd134df237c96d52114088399fcf378cd887cc306fa4764769272e";
const NATIVE_V3_SCHEMA_DIGEST: &str =
    "6668a4dda36fcc6576a985b9767529b72cc1ef480ed8fadfc257cba4104dfa26";
const NATIVE_V2_MIGRATED_V3_SCHEMA_DIGEST: &str =
    "4c13673320fb77c00812c324eb6b43bca838d96149136e34d3d7629ca2abf88e";
const LEGACY_V1_MIGRATED_V3_SCHEMA_DIGEST: &str =
    "3e5b40a5e272668c92f544edd824cb51f5795151e14f131d4986052acfae3b48";
const NATIVE_V4_SCHEMA_DIGEST: &str =
    "e5eadb6a90188e7c5a98011b3cd2b66d5f0bbe5be71670bf29f53bc4cebe453f";
const NATIVE_V3_MIGRATED_V4_SCHEMA_DIGEST: &str =
    "715d631fbf376abe840d251e5d6cf75d549c30de0ddb83233e8da44b76df3ff6";
const NATIVE_V2_MIGRATED_V4_SCHEMA_DIGEST: &str =
    "acc74797e9fb9f4ff08ca0b33107305d00673f25ba96301c1a73a4712e0a1580";
const LEGACY_V1_MIGRATED_V4_SCHEMA_DIGEST: &str =
    "44d62decef67f1366c6fb1e8b5511b636296d8c09d523527f3f78551190f4545";
const NATIVE_V5_SCHEMA_DIGEST: &str =
    "72ebfbd9cd620f77f0521c8507161c37872c5191ef443f30c1d4b096837ef583";
const NATIVE_V3_MIGRATED_V5_SCHEMA_DIGEST: &str =
    "665d5dd1183135def9dcf4f04cc89088233c08cad02de094d94bdb25bd3451cd";
const NATIVE_V2_MIGRATED_V5_SCHEMA_DIGEST: &str =
    "5b30450b7eb91ee4a37d872b7ce8f1a5a146dfef3e1bbb22cff4cd8832b54f4d";
const LEGACY_V1_MIGRATED_V5_SCHEMA_DIGEST: &str =
    "abad62b6f8a25c8ca599a2ad12e0d9c7516a8c5ee992b82c7dea833f707ee08d";
const NATIVE_V6_SCHEMA_DIGEST: &str =
    "e833908204eca61ccd92a0274e54b0422c46e9173e4040d84a137c2d26540ed1";
const NATIVE_V3_MIGRATED_V6_SCHEMA_DIGEST: &str =
    "490727e70011993f688d74637b55fd800893e8e32c3a1c32634ce5308f46a29c";
const NATIVE_V2_MIGRATED_V6_SCHEMA_DIGEST: &str =
    "5059da12e22e8b1c5aaec0a51a2ba17e9b211f6eb86e20eefae1306835d048b3";
const LEGACY_V1_MIGRATED_V6_SCHEMA_DIGEST: &str =
    "c757933fbe93efca1c51fc1d595404ecac787a76844c73f3a883e280d3e29f2a";
const NATIVE_V7_SCHEMA_DIGEST: &str =
    "4a7dbc1cf9be7bfefd9c8826eadacd8d410a176a1c74c9316fb38fe0e0a881aa";
const NATIVE_V3_MIGRATED_V7_SCHEMA_DIGEST: &str =
    "2da515bc26a49fbb208ad4f9ba77a932e6ea4c04296a85730f63d9c04e89301f";
const NATIVE_V2_MIGRATED_V7_SCHEMA_DIGEST: &str =
    "046f4a38f74ffe98550d93e610c344403571bf6e14b3f465ad829ed5766abc64";
const LEGACY_V1_MIGRATED_V7_SCHEMA_DIGEST: &str =
    "7de3774806f43cb9af758cf9692e36df3457e48b9b494a8346bfb8b0bba8fef9";
/// Per-transition bound on knowledge publications, mirroring
/// [`MAX_OUTBOX_PER_TRANSITION`] for the outbox.
const MAX_PUBLICATIONS_PER_TRANSITION: usize = 128;
/// Bound on one audit-chain read page. Verification walks the chain in pages of
/// at most this size, so no query is unbounded regardless of chain length.
pub(crate) const MAX_AUDIT_CHAIN_PAGE: usize = 1_024;
const GENESIS_CHECKSUM: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const MAX_IDENTIFIER_BYTES: usize = 256;
const MAX_KIND_BYTES: usize = 128;
const MAX_JSON_BYTES: usize = (1024 * 1024) - 1;
const MAX_EVIDENCE_BYTES: usize = 1_024;
const MAX_LEGACY_EVIDENCE_BYTES: usize = 64 * 1024;
const MAX_LEGACY_EVIDENCE_FIELDS: usize = 32;
const MAX_LEGACY_EVIDENCE_VALUE_BYTES: usize = 1_024;
const MAX_OUTBOX_PER_TRANSITION: usize = 128;
const MAX_QUERY_LIMIT: usize = 1_000;
const PROCESS_INTENT_SCHEMA_VERSION: u8 = 1;
const MAX_PROCESS_INTENT_BYTES: usize = 192;
const EVENT_PROJECTION_RECIPE_SCHEMA_VERSION: u8 = 1;
const EVENT_PROJECTION_RECIPE_EXTRA_KEY: &str = "__event_projection_recipe";
const PRIVATE_PROCESS_LEDGER_RECORD_SCHEMA_VERSION: u8 = 1;
const PRIVATE_PROCESS_LEDGER_RECORD_EXTRA_KEY: &str = "__private_process_ledger_record";
const RESERVED_EXTRA_KEYS: &[&str] = &[
    EVENT_PROJECTION_RECIPE_EXTRA_KEY,
    PRIVATE_PROCESS_LEDGER_RECORD_EXTRA_KEY,
];
/// Envelope version for [`TerminalDecisionRecord`] (Cell 2F). Distinct from
/// both `RECORD_SCHEMA_VERSION` (the journal row format) and
/// `EVENT_PROJECTION_RECIPE_SCHEMA_VERSION` (an unrelated sealed envelope);
/// this one versions only the terminal-decision payload shape.
const TERMINAL_DECISION_SCHEMA_VERSION: u8 = 1;
/// Journal `transition_kind` tag for a durable terminal-decision row. Never
/// Go-visible: this transition requires no compatibility projection and
/// self-acknowledges on append.
const TERMINAL_DECISION_TRANSITION_KIND: &str = "cell1.terminal_decision";
const TERMINAL_DECISION_TRANSITION_ID_V2_DOMAIN: &[u8] =
    b"nanika:runtime-store:terminal-decision-transition-id:v2";
pub(crate) const PRIVATE_PROCESS_CLAIM_TRANSITION_KIND: &str = "cell3.process_claim";
const PROCESS_ATTEMPT_CLAIMED_TRANSITION_KIND: &str = "cell3.process_attempt_claimed";
const PROCESS_SPAWN_PERMITTED_TRANSITION_KIND: &str = "cell3.process_spawn_permitted";
const PROCESS_STARTED_OBSERVED_TRANSITION_KIND: &str = "cell3.process_started_observed";
const PROCESS_RECOVERY_MARKER_SCHEMA_VERSION: u8 = 1;
const PROCESS_RECOVERY_TRANSITION_ID_DOMAIN: &[u8] =
    b"nanika:runtime-store:process-recovery-transition-id:v1";
/// Journal `transition_kind` tags for the Rust-private reasoning record family.
/// Never Go-visible: reasoning transitions live only in a private-process-ledger
/// store, require a mission, and declare no compatibility projection.
const REASONING_INTENT_TRANSITION_KIND: &str = "reasoning.intent_recorded";
const REASONING_REVISION_TRANSITION_KIND: &str = "reasoning.revision_applied";
const REASONING_ATTEMPT_TRANSITION_KIND: &str = "reasoning.attempt_recorded";
/// Journal `transition_kind` tags for the Rust-private continuation family.
/// Never Go-visible: continuation transitions live only in a private-process-
/// ledger store, require a mission, and declare no compatibility projection.
/// The handle record carries a provider `session_id` and is therefore never
/// surfaced off-store; the decision record is provider-neutral and carries only
/// closed ids, tags, and a capsule digest.
const CONTINUATION_HANDLE_TRANSITION_KIND: &str = "continuation.handle_recorded";
const CONTINUATION_DECISION_TRANSITION_KIND: &str = "continuation.decision_selected";
/// Envelope version for the continuation handle/decision payload shapes.
/// Independent of the store/journal version: continuation adds new journal
/// *kinds* only, no DDL, so the v5 schema digest is unchanged.
const CONTINUATION_RECORD_SCHEMA_VERSION: u8 = 1;
/// Additive v5 Rust-private reasoning-record projection tables. Every column is
/// typed and bounded; there is deliberately no column for raw provider output
/// or chain-of-thought, so such text is unrepresentable rather than filtered.
/// Each table is append-only (reject_update / reject_delete) and references
/// `journal(sequence)` for causal ordering. The DDL is shared verbatim between
/// native creation and the v4->v5 migration so a fresh and a migrated store are
/// byte-identical.
/// Additive, append-only publication queue introduced in schema v6
/// (B3-DESIGN §2).
///
/// This is deliberately **not** a new `OutboxEffectKind` variant. That enum is
/// "finite, process-backed effect families": every variant is claimed,
/// executed, and reconciled against a real OS process with exact receipt
/// evidence. A knowledge publication has no process, no PID, and no exit
/// status, so overloading the enum would force `ClaimedOutboxEffect`,
/// `EffectEvidence`, and `ProcessUncertaintyEvidence` to carry meaningless
/// variants and would weaken R0's exact-effect recovery invariants. The queue
/// is therefore its own table, committed inside the *same* journal transaction
/// as the transition it derives from — so a publication exists if and only if
/// the execution fact it describes was committed.
const KNOWLEDGE_PUBLICATION_SCHEMA_DDL: &str = r"CREATE TABLE knowledge_publication (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    idempotency_key TEXT NOT NULL UNIQUE,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    mission_id TEXT NOT NULL,
    phase_id TEXT,
    logical_attempt INTEGER NOT NULL CHECK (logical_attempt > 0),
    namespace TEXT NOT NULL,
    type_name TEXT NOT NULL,
    primitive_kind TEXT NOT NULL CHECK (
      primitive_kind IN ('record','event','blob','edge')
    ),
    schema_version INTEGER NOT NULL CHECK (schema_version > 0),
    registry_generation INTEGER NOT NULL CHECK (registry_generation > 0),
    sensitivity TEXT NOT NULL CHECK (sensitivity IN ('public','internal','secret')),
    identity TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending','delivered','dead_letter')),
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    enqueued_at_utc TEXT NOT NULL,
    resolved_at_utc TEXT,
    CHECK (
      (state = 'pending' AND resolved_at_utc IS NULL)
      OR (state != 'pending' AND resolved_at_utc IS NOT NULL)
    )
);
CREATE INDEX knowledge_publication_delivery_order
    ON knowledge_publication(state, sequence);
CREATE TRIGGER knowledge_publication_reject_immutable_update
    BEFORE UPDATE OF idempotency_key, journal_sequence, mission_id, phase_id,
                     logical_attempt, namespace, type_name, primitive_kind,
                     schema_version, registry_generation, sensitivity, identity,
                     payload_json, payload_digest, enqueued_at_utc
    ON knowledge_publication
    BEGIN SELECT RAISE(ABORT, 'knowledge publication identity is immutable'); END;
CREATE TRIGGER knowledge_publication_reject_reopen
    BEFORE UPDATE OF state ON knowledge_publication
    WHEN old.state != 'pending'
    BEGIN SELECT RAISE(ABORT, 'resolved knowledge publications are terminal'); END;
CREATE TRIGGER knowledge_publication_reject_delete
    BEFORE DELETE ON knowledge_publication
    BEGIN SELECT RAISE(ABORT, 'knowledge publications are retained'); END;
";

/// B3-DESIGN §5 — the immutable audit chain.
///
/// Storage is a table in `runtime.db`, not a new database: Knowledge-Plane
/// Addendum §5.1 forbids standing up another canonical silo before K0.5, and
/// the chain is ledger-shaped with no product semantics.
///
/// Immutability is enforced twice. In Rust, [`crate::AuditChain`] exposes no
/// update and no delete. In SQLite the two triggers below refuse a direct
/// `UPDATE` or `DELETE` even from inside the owning process, so a caller
/// holding the connection still cannot rewrite history quietly.
///
/// `audit_chain_head` is the persisted head claim. Truncation is exactly the
/// fault the row digests cannot catch on their own — deleting a tail leaves a
/// shorter but internally consistent chain — so the head is recorded
/// separately and may only advance.
const AUDIT_CHAIN_SCHEMA_DDL: &str = r"CREATE TABLE audit_chain (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    prev_digest TEXT NOT NULL,
    namespace TEXT NOT NULL,
    type_name TEXT NOT NULL,
    identity TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    payload_digest TEXT NOT NULL,
    recorded_at_utc TEXT NOT NULL,
    link_digest TEXT NOT NULL UNIQUE
);
CREATE TABLE audit_chain_head (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    sequence INTEGER NOT NULL CHECK (sequence >= 0),
    link_digest TEXT NOT NULL
);
CREATE TRIGGER audit_chain_no_update
    BEFORE UPDATE ON audit_chain
    BEGIN SELECT RAISE(ABORT, 'audit_chain is append-only'); END;
CREATE TRIGGER audit_chain_no_delete
    BEFORE DELETE ON audit_chain
    BEGIN SELECT RAISE(ABORT, 'audit_chain is append-only'); END;
CREATE TRIGGER audit_chain_head_advances_only
    BEFORE UPDATE ON audit_chain_head
    WHEN new.sequence <= old.sequence
    BEGIN SELECT RAISE(ABORT, 'audit chain head advances only'); END;
CREATE TRIGGER audit_chain_head_no_delete
    BEFORE DELETE ON audit_chain_head
    BEGIN SELECT RAISE(ABORT, 'audit chain head is retained'); END;
";

const REASONING_SCHEMA_DDL: &str = r"CREATE TABLE reasoning_record (
    mission_id TEXT PRIMARY KEY,
    intent TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    created_at_utc TEXT NOT NULL
);
CREATE TRIGGER reasoning_record_reject_update
    BEFORE UPDATE ON reasoning_record
    BEGIN SELECT RAISE(ABORT, 'reasoning records are immutable'); END;
CREATE TRIGGER reasoning_record_reject_delete
    BEFORE DELETE ON reasoning_record
    BEGIN SELECT RAISE(ABORT, 'reasoning records are retained'); END;
CREATE TABLE reasoning_assumption (
    mission_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    statement TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, ordinal)
);
CREATE TRIGGER reasoning_assumption_reject_update
    BEFORE UPDATE ON reasoning_assumption
    BEGIN SELECT RAISE(ABORT, 'reasoning assumptions are immutable'); END;
CREATE TRIGGER reasoning_assumption_reject_delete
    BEFORE DELETE ON reasoning_assumption
    BEGIN SELECT RAISE(ABORT, 'reasoning assumptions are retained'); END;
CREATE TABLE reasoning_criterion (
    mission_id TEXT NOT NULL,
    phase_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    criterion TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, phase_id, ordinal)
);
CREATE TRIGGER reasoning_criterion_reject_update
    BEFORE UPDATE ON reasoning_criterion
    BEGIN SELECT RAISE(ABORT, 'reasoning criteria are immutable'); END;
CREATE TRIGGER reasoning_criterion_reject_delete
    BEFORE DELETE ON reasoning_criterion
    BEGIN SELECT RAISE(ABORT, 'reasoning criteria are retained'); END;
CREATE TABLE reasoning_evidence (
    mission_id TEXT NOT NULL,
    phase_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    kind TEXT NOT NULL,
    digest TEXT NOT NULL,
    summary TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, phase_id, ordinal)
);
CREATE TRIGGER reasoning_evidence_reject_update
    BEFORE UPDATE ON reasoning_evidence
    BEGIN SELECT RAISE(ABORT, 'reasoning evidence is immutable'); END;
CREATE TRIGGER reasoning_evidence_reject_delete
    BEFORE DELETE ON reasoning_evidence
    BEGIN SELECT RAISE(ABORT, 'reasoning evidence is retained'); END;
CREATE TABLE reasoning_assignment (
    mission_id TEXT NOT NULL,
    phase_id TEXT NOT NULL,
    persona TEXT NOT NULL,
    role TEXT NOT NULL,
    model_tier TEXT NOT NULL,
    runtime TEXT NOT NULL,
    selection_method TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, phase_id)
);
CREATE TRIGGER reasoning_assignment_reject_update
    BEFORE UPDATE ON reasoning_assignment
    BEGIN SELECT RAISE(ABORT, 'reasoning assignments are immutable'); END;
CREATE TRIGGER reasoning_assignment_reject_delete
    BEFORE DELETE ON reasoning_assignment
    BEGIN SELECT RAISE(ABORT, 'reasoning assignments are retained'); END;
CREATE TABLE reasoning_handoff (
    mission_id TEXT NOT NULL,
    from_phase TEXT NOT NULL,
    to_phase TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    summary TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, from_phase, to_phase, ordinal)
);
CREATE TRIGGER reasoning_handoff_reject_update
    BEFORE UPDATE ON reasoning_handoff
    BEGIN SELECT RAISE(ABORT, 'reasoning handoffs are immutable'); END;
CREATE TRIGGER reasoning_handoff_reject_delete
    BEFORE DELETE ON reasoning_handoff
    BEGIN SELECT RAISE(ABORT, 'reasoning handoffs are retained'); END;
CREATE TABLE reasoning_review (
    mission_id TEXT NOT NULL,
    phase_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal >= 0),
    verdict TEXT NOT NULL,
    reviewer_persona TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, phase_id, ordinal)
);
CREATE TRIGGER reasoning_review_reject_update
    BEFORE UPDATE ON reasoning_review
    BEGIN SELECT RAISE(ABORT, 'reasoning reviews are immutable'); END;
CREATE TRIGGER reasoning_review_reject_delete
    BEFORE DELETE ON reasoning_review
    BEGIN SELECT RAISE(ABORT, 'reasoning reviews are retained'); END;
CREATE TABLE reasoning_attempt (
    mission_id TEXT NOT NULL,
    phase_id TEXT NOT NULL,
    attempt INTEGER NOT NULL CHECK (attempt > 0),
    strategy_fingerprint TEXT NOT NULL,
    outcome TEXT NOT NULL,
    failure_kind TEXT,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, phase_id, attempt)
);
CREATE TRIGGER reasoning_attempt_reject_update
    BEFORE UPDATE ON reasoning_attempt
    BEGIN SELECT RAISE(ABORT, 'reasoning attempts are immutable'); END;
CREATE TRIGGER reasoning_attempt_reject_delete
    BEFORE DELETE ON reasoning_attempt
    BEGIN SELECT RAISE(ABORT, 'reasoning attempts are retained'); END;
CREATE TABLE reasoning_revision (
    mission_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    reason TEXT NOT NULL,
    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
    PRIMARY KEY (mission_id, revision)
);
CREATE TRIGGER reasoning_revision_reject_update
    BEFORE UPDATE ON reasoning_revision
    BEGIN SELECT RAISE(ABORT, 'reasoning revisions are immutable'); END;
CREATE TRIGGER reasoning_revision_reject_delete
    BEFORE DELETE ON reasoning_revision
    BEGIN SELECT RAISE(ABORT, 'reasoning revisions are retained'); END;";
const MAX_RECOVERY_SNAPSHOT_BYTES: usize = 8 * 1024 * 1024;
/// Dedicated ceiling for [`ProjectionRecoveryBounds::new`]'s `max_records`,
/// independent of [`MAX_QUERY_LIMIT`] (the outbox pagination cap). A
/// mission's full event-log journal can legitimately exceed 1,000 rows;
/// routing recovery through the same cap as a page size would make such a
/// mission permanently un-snapshottable. Cell 2D may still need pagination
/// for extreme missions — this bound stays strict and contractual, never a
/// silent truncation.
const MAX_RECOVERY_SNAPSHOT_RECORDS: usize = 10_000;

static STORE_LEASES: OnceLock<Mutex<BTreeSet<FileIdentity>>> = OnceLock::new();

/// One immutable transition and all effects made eligible by that transition.
pub struct JournalIntent {
    transition_id: String,
    mission_id: Option<MissionId>,
    kind: String,
    payload: Value,
    committed_at_utc: String,
    extra: BTreeMap<String, Value>,
    required_projections: BTreeSet<CompatibilityProjection>,
    outbox: Vec<OutboxIntent>,
    publications: Vec<PublicationIntent>,
}

impl JournalIntent {
    /// Validates a transition before it reaches the storage transaction.
    pub fn new(
        transition_id: impl Into<String>,
        mission_id: Option<MissionId>,
        kind: impl Into<String>,
        payload: Value,
        committed_at_utc: impl Into<String>,
    ) -> Result<Self, RuntimeStoreError> {
        let transition_id = transition_id.into();
        let kind = kind.into();
        let committed_at_utc = committed_at_utc.into();
        validate_atom("transition ID", &transition_id, MAX_IDENTIFIER_BYTES)?;
        validate_atom("transition kind", &kind, MAX_KIND_BYTES)?;
        validate_timestamp(&committed_at_utc)?;
        canonical_json(&payload, "journal payload")?;
        Ok(Self {
            transition_id,
            mission_id,
            kind,
            payload,
            committed_at_utc,
            extra: BTreeMap::new(),
            required_projections: BTreeSet::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        })
    }

    /// Declares one compatibility projection that must be durably proven
    /// before this transition can be acknowledged or release an effect.
    #[must_use]
    pub fn with_required_projection(mut self, projection: CompatibilityProjection) -> Self {
        self.required_projections.insert(projection);
        self
    }

    /// Carries additive private record fields without teaching Go about them.
    ///
    /// Keys reserved for runtime-sealed material (currently the event
    /// projection recipe) are rejected here so a caller can never forge or
    /// collide with identity the storage actor seals at append time.
    pub fn with_extra(mut self, extra: BTreeMap<String, Value>) -> Result<Self, RuntimeStoreError> {
        if extra
            .keys()
            .any(|key| RESERVED_EXTRA_KEYS.contains(&key.as_str()))
        {
            return Err(RuntimeStoreError::InvalidIntent(
                "record extras cannot use a runtime-sealed reserved key",
            ));
        }
        canonical_json(
            &Value::Object(extra.clone().into_iter().collect()),
            "record extras",
        )?;
        self.extra = extra;
        Ok(self)
    }

    /// Returns the transition identifier this intent will commit under.
    #[cfg(test)]
    pub(crate) fn transition_id(&self) -> &str {
        &self.transition_id
    }

    /// Returns the transition kind.
    #[cfg(any(test, feature = "test-support"))]
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// Returns the validated payload.
    ///
    /// Gated the same way as the rest of this crate's test-only surface
    /// ([`JournalCommit::for_fixture`]): the public [`fmt::Debug`] rendering
    /// still redacts it in every build, so a production intent never leaks its
    /// payload through this accessor — only a test double implementing
    /// [`crate::routing::RoutingDecisionJournal`] under `test-support` can read
    /// back what it was asked to persist.
    #[cfg(any(test, feature = "test-support"))]
    pub const fn payload(&self) -> &Value {
        &self.payload
    }

    /// Adds one effect intent to the same atomic journal transaction.
    pub fn with_outbox(mut self, effect: OutboxIntent) -> Result<Self, RuntimeStoreError> {
        if self.outbox.len() >= MAX_OUTBOX_PER_TRANSITION {
            return Err(RuntimeStoreError::InvalidIntent(
                "transition exceeds the outbox effect-count limit",
            ));
        }
        if self
            .outbox
            .iter()
            .any(|existing| existing.idempotency_key == effect.idempotency_key)
        {
            return Err(RuntimeStoreError::InvalidIntent(
                "transition repeats an outbox idempotency key",
            ));
        }
        if self.mission_id.as_ref() != Some(&effect.mission_id) {
            return Err(RuntimeStoreError::InvalidIntent(
                "outbox effect mission does not match its authorizing transition",
            ));
        }
        self.outbox.push(effect);
        Ok(self)
    }

    /// Adds one knowledge publication to the same atomic journal transaction.
    ///
    /// This mirrors [`Self::with_outbox`] deliberately: the same
    /// per-transition count bound, the same duplicate-idempotency-key
    /// rejection, and the same mission-identity binding between the queued
    /// item and its authorizing transition. Because the row lands inside the
    /// journal transaction, a publication exists **if and only if** the
    /// execution fact it describes was committed.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::InvalidIntent`] when the transition exceeds
    /// [`MAX_PUBLICATIONS_PER_TRANSITION`], repeats an idempotency key, or
    /// carries a publication for a different mission.
    pub fn with_publication(
        mut self,
        publication: PublicationIntent,
    ) -> Result<Self, RuntimeStoreError> {
        if self.publications.len() >= MAX_PUBLICATIONS_PER_TRANSITION {
            return Err(RuntimeStoreError::InvalidIntent(
                "transition exceeds the knowledge publication count limit",
            ));
        }
        if self
            .publications
            .iter()
            .any(|existing| existing.idempotency_key == publication.idempotency_key)
        {
            return Err(RuntimeStoreError::InvalidIntent(
                "transition repeats a knowledge publication idempotency key",
            ));
        }
        if self.mission_id.as_ref() != Some(&publication.mission_id) {
            return Err(RuntimeStoreError::InvalidIntent(
                "knowledge publication mission does not match its authorizing transition",
            ));
        }
        self.publications.push(publication);
        Ok(self)
    }
}

impl fmt::Debug for JournalIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("JournalIntent")
            .field("has_mission", &self.mission_id.is_some())
            .field("kind", &self.kind)
            .field("payload", &"[REDACTED]")
            .field("extra_fields", &self.extra.len())
            .field("required_projections", &self.required_projections)
            .field("outbox_effects", &self.outbox.len())
            .field("publications", &self.publications.len())
            .finish()
    }
}

/// Go-visible projection families that can gate one Rust transition.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CompatibilityProjection {
    Checkpoint,
    EventLog,
    Workspace,
    Sidecars,
    Metrics,
}

impl CompatibilityProjection {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Checkpoint => "checkpoint",
            Self::EventLog => "event_log",
            Self::Workspace => "workspace",
            Self::Sidecars => "sidecars",
            Self::Metrics => "metrics",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "checkpoint" => Ok(Self::Checkpoint),
            "event_log" => Ok(Self::EventLog),
            "workspace" => Ok(Self::Workspace),
            "sidecars" => Ok(Self::Sidecars),
            "metrics" => Ok(Self::Metrics),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }
}

/// Sealed, versioned event-projection identity bound in checksummed journal
/// material at append time, before any public projection is attempted.
///
/// Once a transition requires [`CompatibilityProjection::EventLog`], the
/// storage actor allocates this identity transactionally and embeds it in
/// the transition's own `extra_json` (already part of the journal's
/// checksum chain), so a reopen after a crash can prove the intended event
/// ID and mission-scoped public sequence without trusting anything a
/// projector published to the filesystem. Receipts attest a projection of
/// this recipe; they never choose one.
#[derive(Clone, Eq, PartialEq)]
pub struct EventProjectionRecipe {
    version: u8,
    event_id: String,
    public_sequence: i64,
}

impl fmt::Debug for EventProjectionRecipe {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EventProjectionRecipe")
            .field("version", &self.version)
            .field("public_sequence", &self.public_sequence)
            .field("event_id", &"[REDACTED]")
            .finish()
    }
}

impl EventProjectionRecipe {
    /// Returns the sealed `evt_`-prefixed identifier.
    #[must_use]
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    /// Returns the sealed mission-scoped public sequence (always positive).
    #[must_use]
    pub const fn public_sequence(&self) -> i64 {
        self.public_sequence
    }

    /// Returns the recipe schema version this identity was sealed under.
    #[must_use]
    pub const fn version(&self) -> u8 {
        self.version
    }

    fn to_value(&self) -> Value {
        serde_json::json!({
            "version": self.version,
            "event_id": self.event_id,
            "public_sequence": self.public_sequence,
        })
    }

    /// Parses a recipe from stored `extra_json` material. Any shape other
    /// than the exact current-version envelope is rejected rather than
    /// guessed at.
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 3 {
            return None;
        }
        let version = object.get("version")?.as_u64()?;
        if version != u64::from(EVENT_PROJECTION_RECIPE_SCHEMA_VERSION) {
            return None;
        }
        let event_id = object.get("event_id")?.as_str()?;
        if !event_id.starts_with("evt_") || EventId::new(event_id).is_err() {
            return None;
        }
        let public_sequence = object.get("public_sequence")?.as_i64()?;
        if public_sequence <= 0 {
            return None;
        }
        Some(Self {
            version: EVENT_PROJECTION_RECIPE_SCHEMA_VERSION,
            event_id: event_id.to_owned(),
            public_sequence,
        })
    }
}

/// Parses a sealed [`EventProjectionRecipe`] out of one transition's stored
/// `extra_json`, tolerating its absence (legacy pre-recipe transitions).
/// Returns `None` for anything malformed; callers that require a recipe to
/// exist convert that into a typed fail-closed error themselves.
fn parse_sealed_recipe(extra_json: &str) -> Option<EventProjectionRecipe> {
    let parsed: Value = serde_json::from_str(extra_json).ok()?;
    let Value::Object(object) = parsed else {
        return None;
    };
    EventProjectionRecipe::from_value(object.get(EVENT_PROJECTION_RECIPE_EXTRA_KEY)?)
}

/// Seals a freshly allocated recipe into one transition's canonical
/// `extra_json`, returning the updated canonical string. The reserved key
/// is only ever written here; [`JournalIntent::with_extra`] rejects it from
/// caller-supplied material.
fn seal_event_projection_recipe(
    extra_json: &str,
    recipe: &EventProjectionRecipe,
) -> Result<String, RuntimeStoreError> {
    let parsed: Value =
        serde_json::from_str(extra_json).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    let Value::Object(mut object) = parsed else {
        return Err(RuntimeStoreError::CorruptDatabase);
    };
    if object
        .insert(
            EVENT_PROJECTION_RECIPE_EXTRA_KEY.to_owned(),
            recipe.to_value(),
        )
        .is_some()
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    canonical_json(&Value::Object(object), "record extras with sealed recipe")
}

/// Returns one transition's canonical `extra_json` with the sealed event
/// recipe (if any) removed, for comparing caller-supplied extras across an
/// exact retry without the server-sealed identity affecting the match.
fn extra_json_without_recipe(extra_json: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(extra_json).ok()?;
    let Value::Object(mut object) = parsed else {
        return None;
    };
    object.remove(EVENT_PROJECTION_RECIPE_EXTRA_KEY);
    canonical_json(
        &Value::Object(object),
        "record extras without sealed recipe",
    )
    .ok()
}

fn private_process_ledger_record_marker() -> Value {
    serde_json::json!({
        "schema": PRIVATE_PROCESS_LEDGER_RECORD_SCHEMA_VERSION
    })
}

fn is_known_private_transition_kind(kind: &str) -> bool {
    matches!(
        kind,
        TERMINAL_DECISION_TRANSITION_KIND
            | PRIVATE_PROCESS_CLAIM_TRANSITION_KIND
            | PROCESS_ATTEMPT_CLAIMED_TRANSITION_KIND
            | PROCESS_SPAWN_PERMITTED_TRANSITION_KIND
            | PROCESS_STARTED_OBSERVED_TRANSITION_KIND
            | REASONING_INTENT_TRANSITION_KIND
            | REASONING_REVISION_TRANSITION_KIND
            | REASONING_ATTEMPT_TRANSITION_KIND
            | CONTINUATION_HANDLE_TRANSITION_KIND
            | CONTINUATION_DECISION_TRANSITION_KIND
    )
}

// --- Cell 2F: durable terminal-decision persistence -------------------------
//
// Cell 1's fixture engine (`fixture_sequential_run.rs`) never writes to this
// journal for its own event/checkpoint projections — those stay workspace-
// local files, on purpose (see the Cell 2 architecture note: one event
// writer per composition). The one thing Cell 1 explicitly cannot make
// durable on its own is the *terminal/verification decision* it first
// selects for a worker attempt, because that decision has nowhere durable to
// live once selected. This section gives it exactly one such place: a
// dedicated, versioned journal row (no new table — the existing checksummed
// `journal` table already gives every row tamper-evidence, exact-retry
// idempotency, and crash durability for free) keyed by a deterministic
// transition ID derived from the mission/phase/worker/attempt binding it
// proves. A conflicting write under the same binding is rejected by the
// journal's existing exact-retry check, not by new code here.

/// Typed mirror of one [`MechanicalTermination`] as closed JSON, never a
/// human-authored string. `None` (absence of this case entirely) means the
/// attempt completed cleanly.
fn mechanical_termination_to_value(termination: MechanicalTermination) -> Value {
    match termination {
        MechanicalTermination::Cancelled => serde_json::json!({"kind": "cancelled"}),
        MechanicalTermination::HardDeadlineExceeded => {
            serde_json::json!({"kind": "hard_deadline_exceeded"})
        }
        MechanicalTermination::WatchdogStalled => serde_json::json!({"kind": "watchdog_stalled"}),
        MechanicalTermination::ProcessExited(status) => match status.as_code() {
            Some(code) => {
                serde_json::json!({"kind": "process_exited", "exit": "code", "value": code})
            }
            None => match status.as_signal() {
                Some(signal) => {
                    serde_json::json!({"kind": "process_exited", "exit": "signal", "value": signal})
                }
                None => serde_json::json!({"kind": "process_exited", "exit": "unknown"}),
            },
        },
        MechanicalTermination::ProviderStreamEnded => {
            serde_json::json!({"kind": "provider_stream_ended"})
        }
        MechanicalTermination::SupervisorFailure => {
            serde_json::json!({"kind": "supervisor_failure"})
        }
        MechanicalTermination::EventDeliveryFailure => {
            serde_json::json!({"kind": "event_delivery_failure"})
        }
        MechanicalTermination::ContractViolation => {
            serde_json::json!({"kind": "contract_violation"})
        }
    }
}

/// Parses a typed [`MechanicalTermination`] from closed JSON. Any shape
/// other than one exact known tag (including an unexpected extra field) is
/// rejected rather than guessed at.
fn mechanical_termination_from_value(value: &Value) -> Option<MechanicalTermination> {
    let object = value.as_object()?;
    match object.get("kind")?.as_str()? {
        "cancelled" if object.len() == 1 => Some(MechanicalTermination::Cancelled),
        "hard_deadline_exceeded" if object.len() == 1 => {
            Some(MechanicalTermination::HardDeadlineExceeded)
        }
        "watchdog_stalled" if object.len() == 1 => Some(MechanicalTermination::WatchdogStalled),
        "process_exited" if object.len() == 3 => {
            let exit = object.get("exit")?.as_str()?;
            let raw = object.get("value")?.as_i64()?;
            let code = i32::try_from(raw).ok()?;
            match exit {
                "code" => ProcessExitStatus::code(code).ok(),
                "signal" => ProcessExitStatus::signal(code).ok(),
                _ => None,
            }
            .map(MechanicalTermination::ProcessExited)
        }
        "provider_stream_ended" if object.len() == 1 => {
            Some(MechanicalTermination::ProviderStreamEnded)
        }
        "supervisor_failure" if object.len() == 1 => Some(MechanicalTermination::SupervisorFailure),
        "event_delivery_failure" if object.len() == 1 => {
            Some(MechanicalTermination::EventDeliveryFailure)
        }
        "contract_violation" if object.len() == 1 => Some(MechanicalTermination::ContractViolation),
        _ => None,
    }
}

const fn verification_class_str(class: VerificationClass) -> &'static str {
    match class {
        VerificationClass::Pass => "pass",
        VerificationClass::Fail => "fail",
        VerificationClass::Skip => "skip",
        VerificationClass::NoTests => "no_tests",
        VerificationClass::Timeout => "timeout",
        VerificationClass::InfrastructureError => "infrastructure_error",
    }
}

fn verification_class_from_str(value: &str) -> Option<VerificationClass> {
    Some(match value {
        "pass" => VerificationClass::Pass,
        "fail" => VerificationClass::Fail,
        "skip" => VerificationClass::Skip,
        "no_tests" => VerificationClass::NoTests,
        "timeout" => VerificationClass::Timeout,
        "infrastructure_error" => VerificationClass::InfrastructureError,
        _ => return None,
    })
}

fn verification_outcome_to_value(outcome: VerificationOutcome) -> Value {
    match outcome {
        VerificationOutcome::Cancelled => serde_json::json!({"kind": "cancelled"}),
        VerificationOutcome::Classified(class) => {
            serde_json::json!({"kind": "classified", "class": verification_class_str(class)})
        }
    }
}

fn verification_outcome_from_value(value: &Value) -> Option<VerificationOutcome> {
    let object = value.as_object()?;
    match object.get("kind")?.as_str()? {
        "cancelled" if object.len() == 1 => Some(VerificationOutcome::Cancelled),
        "classified" if object.len() == 2 => {
            verification_class_from_str(object.get("class")?.as_str()?)
                .map(VerificationOutcome::Classified)
        }
        _ => None,
    }
}

const fn verification_action_str(action: VerificationAction) -> &'static str {
    match action {
        VerificationAction::Continue => "continue",
        VerificationAction::Block => "block",
        VerificationAction::Cancelled => "cancelled",
    }
}

fn verification_action_from_str(value: &str) -> Option<VerificationAction> {
    Some(match value {
        "continue" => VerificationAction::Continue,
        "block" => VerificationAction::Block,
        "cancelled" => VerificationAction::Cancelled,
        _ => return None,
    })
}

/// Recomputes the [`VerificationMode`] that [`decide_verification`] would
/// need to reproduce a stored `(action, warning)` pair exactly.
///
/// Both modes reproduce an identical decision when the outcome is
/// `Cancelled` or a classified `Pass` (mode never affects those cases), so
/// the choice only matters for a classified non-pass outcome: `action ==
/// Block` uniquely implies `Block` was used, and `warning == true` uniquely
/// implies `Warn` was used. This lets storage keep only closed, typed
/// fields — never a `VerificationMode` — while still reconstructing a
/// byte-identical [`VerificationDecision`] through the same production
/// `decide_verification` logic that could have created the original.
const fn canonical_verification_mode(
    action: VerificationAction,
    warning: bool,
) -> VerificationMode {
    match (action, warning) {
        (VerificationAction::Block, _) => VerificationMode::Block,
        (_, true) => VerificationMode::Warn,
        _ => VerificationMode::Block,
    }
}

fn verification_decision_to_value(decision: VerificationDecision) -> Value {
    serde_json::json!({
        "outcome": verification_outcome_to_value(decision.outcome()),
        "action": verification_action_str(decision.action()),
        "gate_passed": decision.gate_passed(),
        "warning": decision.warning(),
    })
}

/// Parses a verification decision and cross-checks it against the same
/// [`decide_verification`] production logic that could have created it. Any
/// stored `(action, gate_passed, warning)` combination that function would
/// never produce for the stored outcome is corruption, not a guess.
fn verification_decision_from_value(value: &Value) -> Option<VerificationDecision> {
    let object = value.as_object()?;
    if object.len() != 4 {
        return None;
    }
    let outcome = verification_outcome_from_value(object.get("outcome")?)?;
    let action = verification_action_from_str(object.get("action")?.as_str()?)?;
    let gate_passed = object.get("gate_passed")?.as_bool()?;
    let warning = object.get("warning")?.as_bool()?;
    let mode = canonical_verification_mode(action, warning);
    let reconstructed = decide_verification(outcome, mode);
    (reconstructed.action() == action
        && reconstructed.gate_passed() == gate_passed
        && reconstructed.warning() == warning)
        .then_some(reconstructed)
}

/// Deterministic idempotency key binding one durable terminal decision to
/// its exact mission/phase/worker/attempt.
///
/// Existing Cell-2F rows retain their exact v1 text when the whole candidate
/// is in the legacy accepted domain: bounded ASCII with colon-free phase and
/// worker identifiers. New identifiers containing a colon or non-ASCII text,
/// plus any overlong v1 candidate, use a bounded ASCII v2 SHA-256 identifier.
/// The v2 digest is domain-separated and length-frames the exact UTF-8 mission,
/// phase, and worker bytes plus the big-endian attempt, so no separator or
/// Unicode-normalization assumption carries identity.
fn terminal_decision_transition_id(
    mission_id: &MissionId,
    phase_id: &str,
    worker_id: &str,
    attempt: u32,
) -> String {
    let legacy = format!(
        "cell2f-terminal-decision:{}:{phase_id}:{worker_id}:{attempt}",
        mission_id.as_str(),
    );
    if !phase_id.contains(':')
        && worker_id.is_ascii()
        && !worker_id.contains(':')
        && legacy.is_ascii()
        && legacy.len() <= MAX_IDENTIFIER_BYTES
    {
        return legacy;
    }

    let mut digest = Sha256::new();
    terminal_decision_digest_frame(&mut digest, TERMINAL_DECISION_TRANSITION_ID_V2_DOMAIN);
    terminal_decision_digest_frame(&mut digest, mission_id.as_str().as_bytes());
    terminal_decision_digest_frame(&mut digest, phase_id.as_bytes());
    terminal_decision_digest_frame(&mut digest, worker_id.as_bytes());
    terminal_decision_digest_frame(&mut digest, &attempt.to_be_bytes());
    format!(
        "cell2f-terminal-decision-v2:{}",
        hex_digest(digest.finalize().as_slice())
    )
}

fn terminal_decision_digest_frame(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

/// Closed, versioned, mission/phase/worker/attempt-bound record of Cell 1's
/// first-selected terminal decision (Cell 2F): the typed observed mechanical
/// termination (or `None` for a clean completion) and the structured
/// verification decision used to reach the terminal plan.
///
/// Persisted the instant the fixture engine first selects them, before any
/// lifecycle mutation, so a fresh runner reopening a still-running phase
/// with a durable worker terminal can resume the exact original decision
/// instead of refusing with `AmbiguousDurableTerminalDecision` or guessing
/// from a new caller's argument. Every field is a closed enum, a bounded
/// identifier, or a bool — never human provider output or chain-of-thought.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct TerminalDecisionRecord {
    version: u8,
    mission_id: MissionId,
    phase_id: String,
    worker_id: String,
    attempt: u32,
    observed_termination: Option<MechanicalTermination>,
    verification: VerificationDecision,
}

impl fmt::Debug for TerminalDecisionRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TerminalDecisionRecord")
            .field("version", &self.version)
            .field("phase_id", &self.phase_id)
            .field("attempt", &self.attempt)
            .field("observed_termination", &self.observed_termination)
            .field("verification_gate_passed", &self.verification.gate_passed())
            .finish_non_exhaustive()
    }
}

impl TerminalDecisionRecord {
    /// Validates and constructs one closed terminal-decision record.
    pub(crate) fn new(
        mission_id: &MissionId,
        phase_id: &str,
        worker_id: &str,
        attempt: u32,
        observed_termination: Option<MechanicalTermination>,
        verification: VerificationDecision,
    ) -> Result<Self, RuntimeStoreError> {
        validate_atom("terminal decision phase ID", phase_id, MAX_IDENTIFIER_BYTES)?;
        validate_terminal_decision_worker_id(worker_id)?;
        if attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "terminal decision attempt must be positive",
            ));
        }
        // The storage layer must not itself durably persist a Warn-shaped
        // decision (`gate_passed == false` alongside `action ==
        // VerificationAction::Continue`): the runner's own
        // `validate_terminal_verification` already refuses to reach this
        // constructor with that shape, but making the invariant local here
        // means a future or alternate caller cannot silently persist one.
        if !verification.gate_passed() && verification.action() == VerificationAction::Continue {
            return Err(RuntimeStoreError::InvalidIntent(
                "terminal decision verification must not be Warn-shaped \
                 (gate_passed=false with action=Continue is never a valid terminal decision)",
            ));
        }
        Ok(Self {
            version: TERMINAL_DECISION_SCHEMA_VERSION,
            mission_id: mission_id.clone(),
            phase_id: phase_id.to_owned(),
            worker_id: worker_id.to_owned(),
            attempt,
            observed_termination,
            verification,
        })
    }

    /// Returns the typed mechanical termination this decision observed, or
    /// `None` for a clean completion.
    #[must_use]
    pub(crate) const fn observed_termination(&self) -> Option<MechanicalTermination> {
        self.observed_termination
    }

    /// Returns the structured verification decision this record selected.
    #[must_use]
    pub(crate) const fn verification(&self) -> VerificationDecision {
        self.verification
    }

    fn to_value(&self) -> Value {
        serde_json::json!({
            "version": self.version,
            "mission_id": self.mission_id.as_str(),
            "phase_id": self.phase_id,
            "worker_id": self.worker_id,
            "attempt": self.attempt,
            "observed_termination": self
                .observed_termination
                .map_or(Value::Null, mechanical_termination_to_value),
            "verification": verification_decision_to_value(self.verification),
        })
    }

    /// Parses a record from stored `payload_json`. Any shape other than the
    /// exact current-version envelope — including an unsupported version,
    /// a wrong-typed field, or an internally inconsistent verification
    /// sub-object — is rejected rather than guessed at.
    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 7 {
            return None;
        }
        let version = object.get("version")?.as_u64()?;
        if version != u64::from(TERMINAL_DECISION_SCHEMA_VERSION) {
            return None;
        }
        let mission_id = MissionId::new(object.get("mission_id")?.as_str()?).ok()?;
        let phase_id = object.get("phase_id")?.as_str()?.to_owned();
        let worker_id = object.get("worker_id")?.as_str()?.to_owned();
        WorkerId::new(worker_id.clone()).ok()?;
        let attempt = u32::try_from(object.get("attempt")?.as_u64()?).ok()?;
        if attempt == 0 {
            return None;
        }
        let observed_termination = match object.get("observed_termination")? {
            Value::Null => None,
            other => Some(mechanical_termination_from_value(other)?),
        };
        let verification = verification_decision_from_value(object.get("verification")?)?;
        Some(Self {
            version: TERMINAL_DECISION_SCHEMA_VERSION,
            mission_id,
            phase_id,
            worker_id,
            attempt,
            observed_termination,
            verification,
        })
    }
}

/// Builds the private-ledger transition id for a continuation record.
///
/// Bound to the exact `(mission, phase, attempt)` triple and the record family
/// tag, so the handle and decision for one attempt occupy distinct rows and an
/// exact retry of either is idempotent while a divergent rewrite conflicts.
fn continuation_transition_id(
    family_tag: &str,
    mission_id: &MissionId,
    phase_id: &str,
    attempt: u32,
) -> String {
    format!(
        "{}:continuation:{family_tag}:{phase_id}:{attempt}",
        mission_id.as_str()
    )
}

/// Which arm of the continuation decision was selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ContinuationDecisionKind {
    /// Resume the same provider session under its family-bound handle.
    ResumeSameProvider,
    /// Start a fresh session from a provider-neutral capsule.
    FreshFromCapsule,
}

impl ContinuationDecisionKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ResumeSameProvider => "resume_same_provider",
            Self::FreshFromCapsule => "fresh_from_capsule",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "resume_same_provider" => Some(Self::ResumeSameProvider),
            "fresh_from_capsule" => Some(Self::FreshFromCapsule),
            _ => None,
        }
    }
}

/// Closed, versioned, mission/phase/attempt-bound record of a same-provider
/// continuation handle.
///
/// Carries the provider `session_id`, so it is private-ledger-only and never
/// surfaced off-store or copied into a provider-neutral capsule. Every field is
/// a closed id, a bounded label, or an integer — never provider output.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct ContinuationHandleRecord {
    version: u8,
    mission_id: MissionId,
    phase_id: String,
    attempt: u32,
    runtime_family: String,
    session_id: String,
}

impl fmt::Debug for ContinuationHandleRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ContinuationHandleRecord")
            .field("version", &self.version)
            .field("phase_id", &self.phase_id)
            .field("attempt", &self.attempt)
            .field("runtime_family", &self.runtime_family)
            .field("session_id", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl ContinuationHandleRecord {
    /// Builds a handle record from an executor-contract [`SessionHandle`].
    pub(crate) fn from_handle(
        mission_id: &MissionId,
        phase_id: &str,
        attempt: u32,
        handle: &SessionHandle,
    ) -> Result<Self, RuntimeStoreError> {
        validate_atom("continuation phase ID", phase_id, MAX_IDENTIFIER_BYTES)?;
        if attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "continuation attempt must be positive",
            ));
        }
        Ok(Self {
            version: CONTINUATION_RECORD_SCHEMA_VERSION,
            mission_id: mission_id.clone(),
            phase_id: phase_id.to_owned(),
            attempt,
            runtime_family: handle.runtime_family().as_str().to_owned(),
            session_id: handle.expose_session_id().to_owned(),
        })
    }

    /// Reconstructs the opaque, family-bound [`SessionHandle`].
    pub(crate) fn to_session_handle(&self) -> Result<SessionHandle, RuntimeStoreError> {
        let family = RuntimeFamily::parse(self.runtime_family.clone())
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        SessionHandle::new(family, self.session_id.clone())
            .map_err(|_| RuntimeStoreError::CorruptDatabase)
    }

    fn to_value(&self) -> Value {
        serde_json::json!({
            "version": self.version,
            "mission_id": self.mission_id.as_str(),
            "phase_id": self.phase_id,
            "attempt": self.attempt,
            "runtime_family": self.runtime_family,
            "session_id": self.session_id,
        })
    }

    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 6 {
            return None;
        }
        let version = object.get("version")?.as_u64()?;
        if version != u64::from(CONTINUATION_RECORD_SCHEMA_VERSION) {
            return None;
        }
        let mission_id = MissionId::new(object.get("mission_id")?.as_str()?).ok()?;
        let phase_id = object.get("phase_id")?.as_str()?.to_owned();
        let attempt = u32::try_from(object.get("attempt")?.as_u64()?).ok()?;
        if attempt == 0 {
            return None;
        }
        let runtime_family = object.get("runtime_family")?.as_str()?.to_owned();
        let session_id = object.get("session_id")?.as_str()?.to_owned();
        Some(Self {
            version: CONTINUATION_RECORD_SCHEMA_VERSION,
            mission_id,
            phase_id,
            attempt,
            runtime_family,
            session_id,
        })
    }
}

/// Closed, versioned, mission/phase/attempt-bound record of the selected
/// continuation decision.
///
/// Provider-neutral: it carries only the chosen runtime family, a decision-kind
/// tag, whether a same-provider handle was present, and — for a fresh session —
/// the capsule's SHA-256 digest. There is deliberately no `session_id` and no
/// free-form field, so a decision row can never leak a provider id or raw
/// provider output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ContinuationDecisionRecord {
    version: u8,
    mission_id: MissionId,
    phase_id: String,
    attempt: u32,
    revision: u32,
    kind: ContinuationDecisionKind,
    chosen_runtime: String,
    capsule_digest: Option<String>,
    handle_present: bool,
}

impl ContinuationDecisionRecord {
    pub(crate) fn new(
        mission_id: &MissionId,
        phase_id: &str,
        attempt: u32,
        revision: u32,
        kind: ContinuationDecisionKind,
        chosen_runtime: &str,
        capsule_digest: Option<String>,
    ) -> Result<Self, RuntimeStoreError> {
        validate_atom("continuation phase ID", phase_id, MAX_IDENTIFIER_BYTES)?;
        if attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "continuation attempt must be positive",
            ));
        }
        if revision == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "continuation revision must be positive",
            ));
        }
        validate_atom("continuation runtime", chosen_runtime, MAX_KIND_BYTES)?;
        // Disjointness invariant, enforced fail-closed at the storage boundary:
        // a resume decision presents a handle and no capsule; a fresh decision
        // presents a capsule digest and no handle.
        let (handle_present, capsule_digest) = match kind {
            ContinuationDecisionKind::ResumeSameProvider => {
                if capsule_digest.is_some() {
                    return Err(RuntimeStoreError::InvalidIntent(
                        "a resume continuation decision must not carry a capsule digest",
                    ));
                }
                (true, None)
            }
            ContinuationDecisionKind::FreshFromCapsule => {
                let digest = capsule_digest.ok_or(RuntimeStoreError::InvalidIntent(
                    "a fresh continuation decision must carry a capsule digest",
                ))?;
                if !valid_checksum(&digest) {
                    return Err(RuntimeStoreError::InvalidIntent(
                        "continuation capsule digest is not a SHA-256 hex digest",
                    ));
                }
                (false, Some(digest))
            }
        };
        Ok(Self {
            version: CONTINUATION_RECORD_SCHEMA_VERSION,
            mission_id: mission_id.clone(),
            phase_id: phase_id.to_owned(),
            attempt,
            revision,
            kind,
            chosen_runtime: chosen_runtime.to_owned(),
            capsule_digest,
            handle_present,
        })
    }

    /// A provider-neutral, replay-stable view of this decision.
    pub(crate) fn summary(&self) -> ContinuationDecisionSummary {
        ContinuationDecisionSummary {
            attempt: self.attempt,
            revision: self.revision,
            resumed_same_provider: matches!(
                self.kind,
                ContinuationDecisionKind::ResumeSameProvider
            ),
            chosen_runtime: self.chosen_runtime.clone(),
            capsule_digest: self.capsule_digest.clone(),
        }
    }

    fn to_value(&self) -> Value {
        serde_json::json!({
            "version": self.version,
            "mission_id": self.mission_id.as_str(),
            "phase_id": self.phase_id,
            "attempt": self.attempt,
            "revision": self.revision,
            "kind": self.kind.as_str(),
            "chosen_runtime": self.chosen_runtime,
            "capsule_digest": self.capsule_digest.clone().map_or(Value::Null, Value::String),
            "handle_present": self.handle_present,
        })
    }

    fn from_value(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 9 {
            return None;
        }
        let version = object.get("version")?.as_u64()?;
        if version != u64::from(CONTINUATION_RECORD_SCHEMA_VERSION) {
            return None;
        }
        let mission_id = MissionId::new(object.get("mission_id")?.as_str()?).ok()?;
        let phase_id = object.get("phase_id")?.as_str()?.to_owned();
        let attempt = u32::try_from(object.get("attempt")?.as_u64()?).ok()?;
        if attempt == 0 {
            return None;
        }
        let revision = u32::try_from(object.get("revision")?.as_u64()?).ok()?;
        if revision == 0 {
            return None;
        }
        let kind = ContinuationDecisionKind::from_str(object.get("kind")?.as_str()?)?;
        let chosen_runtime = object.get("chosen_runtime")?.as_str()?.to_owned();
        let capsule_digest = match object.get("capsule_digest")? {
            Value::Null => None,
            other => Some(other.as_str()?.to_owned()),
        };
        let handle_present = object.get("handle_present")?.as_bool()?;
        // Re-enforce the disjointness invariant on read, so a hand-forged or
        // corrupted row is rejected rather than trusted.
        match kind {
            ContinuationDecisionKind::ResumeSameProvider => {
                if !handle_present || capsule_digest.is_some() {
                    return None;
                }
            }
            ContinuationDecisionKind::FreshFromCapsule => {
                if handle_present || capsule_digest.as_deref().is_none_or(|d| !valid_checksum(d)) {
                    return None;
                }
            }
        }
        Some(Self {
            version: CONTINUATION_RECORD_SCHEMA_VERSION,
            mission_id,
            phase_id,
            attempt,
            revision,
            kind,
            chosen_runtime,
            capsule_digest,
            handle_present,
        })
    }
}

/// Durable proof that one required compatibility projection was applied.
#[derive(Clone, Eq, PartialEq)]
pub struct ProjectionReceipt {
    record_schema_version: i64,
    journal_sequence: i64,
    projection: CompatibilityProjection,
    mission_id: Option<String>,
    public_sequence: Option<i64>,
    event_id: Option<String>,
    event_jsonl: Option<Vec<u8>>,
    event_sha256: Option<String>,
    applied_at_utc: String,
}

impl fmt::Debug for ProjectionReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectionReceipt")
            .field("record_schema_version", &self.record_schema_version)
            .field("journal_sequence", &self.journal_sequence)
            .field("projection", &self.projection)
            .field("public_sequence", &self.public_sequence)
            .field("has_event_mapping", &self.event_jsonl.is_some())
            .field("event_jsonl", &"[REDACTED]")
            .finish()
    }
}

impl ProjectionReceipt {
    pub(crate) fn compatibility(
        journal_sequence: i64,
        projection: CompatibilityProjection,
        applied_at_utc: impl Into<String>,
    ) -> Result<Self, RuntimeStoreError> {
        if journal_sequence <= 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "projection journal sequence must be positive",
            ));
        }
        let applied_at_utc = applied_at_utc.into();
        validate_timestamp(&applied_at_utc)?;
        if projection == CompatibilityProjection::EventLog {
            return Err(RuntimeStoreError::InvalidIntent(
                "event-log projections require exact canonical JSONL bytes",
            ));
        }
        Ok(Self {
            record_schema_version: RECORD_SCHEMA_VERSION,
            journal_sequence,
            projection,
            mission_id: None,
            public_sequence: None,
            event_id: None,
            event_jsonl: None,
            event_sha256: None,
            applied_at_utc,
        })
    }

    pub(crate) fn event_log(
        journal_sequence: i64,
        event_jsonl: Vec<u8>,
        applied_at_utc: impl Into<String>,
    ) -> Result<Self, RuntimeStoreError> {
        if journal_sequence <= 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "projection journal sequence must be positive",
            ));
        }
        let applied_at_utc = applied_at_utc.into();
        validate_timestamp(&applied_at_utc)?;
        let mapping =
            canonical_event_mapping(&event_jsonl).ok_or(RuntimeStoreError::InvalidIntent(
                "event-log projection bytes are not one canonical Go JSONL event",
            ))?;
        Ok(Self {
            record_schema_version: RECORD_SCHEMA_VERSION,
            journal_sequence,
            projection: CompatibilityProjection::EventLog,
            mission_id: Some(mapping.record.mission_id.clone()),
            public_sequence: Some(mapping.public_sequence),
            event_id: Some(mapping.event_id),
            event_sha256: Some(mapping.sha256),
            event_jsonl: Some(event_jsonl),
            applied_at_utc,
        })
    }

    fn from_stored(stored: Self) -> Result<Self, RuntimeStoreError> {
        let Self {
            record_schema_version,
            journal_sequence,
            projection,
            mission_id,
            public_sequence,
            event_id,
            event_jsonl,
            event_sha256,
            applied_at_utc,
        } = stored;
        validate_timestamp(&applied_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if journal_sequence <= 0 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        if !matches!(
            record_schema_version,
            LEGACY_RECORD_SCHEMA_VERSION | RECORD_SCHEMA_VERSION
        ) {
            return Err(RuntimeStoreError::UnsupportedSchema {
                found: record_schema_version,
                expected: RECORD_SCHEMA_VERSION,
            });
        }
        if projection == CompatibilityProjection::EventLog {
            let mission_id = mission_id
                .as_deref()
                .ok_or(RuntimeStoreError::CorruptDatabase)?;
            MissionId::new(mission_id.to_owned())
                .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
            if public_sequence.is_none_or(|value| value <= 0)
                || event_id
                    .as_deref()
                    .is_none_or(|value| !value.starts_with("evt_") || EventId::new(value).is_err())
            {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            if record_schema_version == RECORD_SCHEMA_VERSION {
                let bytes = event_jsonl
                    .as_deref()
                    .ok_or(RuntimeStoreError::CorruptDatabase)?;
                let mapping =
                    canonical_event_mapping(bytes).ok_or(RuntimeStoreError::CorruptDatabase)?;
                if mission_id != mapping.record.mission_id
                    || public_sequence != Some(mapping.public_sequence)
                    || event_id.as_deref() != Some(mapping.event_id.as_str())
                    || event_sha256.as_deref() != Some(mapping.sha256.as_str())
                {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
            } else if event_jsonl.is_some() || event_sha256.is_some() {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        } else if mission_id.is_some()
            || public_sequence.is_some()
            || event_id.is_some()
            || event_jsonl.is_some()
            || event_sha256.is_some()
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(Self {
            record_schema_version,
            journal_sequence,
            projection,
            mission_id,
            public_sequence,
            event_id,
            event_jsonl,
            event_sha256,
            applied_at_utc,
        })
    }
}

struct CanonicalEventMapping {
    public_sequence: i64,
    event_id: String,
    sha256: String,
    record: EventRecord,
}

fn canonical_event_mapping(bytes: &[u8]) -> Option<CanonicalEventMapping> {
    let content = bytes.strip_suffix(b"\n")?;
    if content.is_empty()
        || content.len() > GO_EVENT_JSON_CONTENT_MAX_BYTES
        || content.contains(&b'\n')
        || content.ends_with(b"\r")
    {
        return None;
    }
    let decoded = decode_event_line(content).ok()?;
    if decoded.record.sequence <= 0
        || !decoded.record.id.starts_with("evt_")
        || EventId::new(&decoded.record.id).is_err()
        || encode_current_event(&decoded.record).ok()?.as_slice() != content
    {
        return None;
    }
    Some(CanonicalEventMapping {
        public_sequence: decoded.record.sequence,
        event_id: decoded.record.id.clone(),
        sha256: hex_digest(Sha256::digest(bytes).as_slice()),
        record: decoded.record,
    })
}

#[derive(Debug, Eq, PartialEq)]
struct ExpectedEventProjection {
    phase_id: Option<String>,
    worker_id: Option<String>,
    data: Option<EventJsonMap>,
    extra: EventJsonMap,
}

impl ExpectedEventProjection {
    fn from_journal_payload(payload_json: &str) -> Result<Self, RuntimeStoreError> {
        let payload = validate_stored_json(payload_json, "journal event projection")?;
        let Value::Object(mut object) = payload else {
            return Err(RuntimeStoreError::InvalidIntent(
                "event-producing journal payload must be an object",
            ));
        };
        if object.keys().any(|field| {
            matches!(
                field.as_str(),
                "id" | "type" | "timestamp" | "sequence" | "mission_id"
            )
        }) {
            return Err(RuntimeStoreError::InvalidIntent(
                "journal event payload cannot override generated envelope fields",
            ));
        }
        let phase_id = take_optional_event_string(&mut object, "phase_id")?;
        let worker_id = take_optional_event_string(&mut object, "worker_id")?;
        let data = match object.remove("data") {
            None | Some(Value::Null) => None,
            Some(Value::Object(value)) if value.is_empty() => None,
            Some(Value::Object(value)) => Some(value.into_iter().collect()),
            Some(_) => {
                return Err(RuntimeStoreError::InvalidIntent(
                    "journal event data must be an object when present",
                ));
            }
        };
        Ok(Self {
            phase_id,
            worker_id,
            data,
            extra: object.into_iter().collect(),
        })
    }

    fn matches(&self, record: &EventRecord) -> bool {
        self.phase_id == record.phase_id
            && self.worker_id == record.worker_id
            && self.data == record.data
            && self.extra == record.extra
    }
}

fn take_optional_event_string(
    object: &mut serde_json::Map<String, Value>,
    field: &'static str,
) -> Result<Option<String>, RuntimeStoreError> {
    match object.remove(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() => {
            validate_atom(field, &value, MAX_IDENTIFIER_BYTES)?;
            Ok(Some(value))
        }
        Some(_) => Err(RuntimeStoreError::InvalidIntent(
            "journal event identifiers must be nonempty strings when present",
        )),
    }
}

/// Finite, process-backed effect families supported by the first runtime slice.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub enum OutboxEffectKind {
    ProviderProcess,
    GitCommand,
    PluginProcess,
}

/// Stable logical slot distinguishing same-kind effects in one phase attempt.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct EffectOperationSlot(String);

impl EffectOperationSlot {
    pub fn new(value: impl Into<String>) -> Result<Self, RuntimeStoreError> {
        let value = value.into();
        validate_atom("effect operation slot", &value, MAX_IDENTIFIER_BYTES)?;
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(RuntimeStoreError::InvalidIntent(
                "effect operation slot must use stable ASCII identifier characters",
            ));
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl OutboxEffectKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ProviderProcess => "provider_process",
            Self::GitCommand => "git_command",
            Self::PluginProcess => "plugin_process",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "provider_process" => Ok(Self::ProviderProcess),
            "git_command" => Ok(Self::GitCommand),
            "plugin_process" => Ok(Self::PluginProcess),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }
}

/// Versioned, bounded sidecar that binds a process effect to exact request
/// semantics without persisting executable, path, argument, environment, or
/// stdin material a second time.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProcessIntentEnvelope {
    schema_version: u8,
    request_sha256: String,
}

impl ProcessIntentEnvelope {
    fn new(fingerprint: ProcessRequestFingerprint) -> Self {
        Self {
            schema_version: PROCESS_INTENT_SCHEMA_VERSION,
            request_sha256: hex_digest(fingerprint.as_bytes()),
        }
    }

    fn canonical_json(&self) -> Result<String, RuntimeStoreError> {
        let value = serde_json::to_string(self)
            .map_err(|source| RuntimeStoreError::operation("encode process intent", source))?;
        if value.len() > MAX_PROCESS_INTENT_BYTES {
            return Err(RuntimeStoreError::InvalidIntent(
                "process intent exceeds its fixed storage bound",
            ));
        }
        Ok(value)
    }

    fn from_stored(value: &str) -> Result<Self, RuntimeStoreError> {
        if value.len() > MAX_PROCESS_INTENT_BYTES {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let intent: Self =
            serde_json::from_str(value).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if intent.schema_version != PROCESS_INTENT_SCHEMA_VERSION
            || !valid_checksum(&intent.request_sha256)
            || intent
                .canonical_json()
                .map_err(|_| RuntimeStoreError::CorruptDatabase)?
                != value
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(intent)
    }
}

impl fmt::Debug for ProcessIntentEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessIntentEnvelope")
            .field("schema_version", &self.schema_version)
            .field("request_sha256", &"[REDACTED]")
            .finish()
    }
}

/// Immutable side-effect input inserted with its authorizing transition.
///
/// Callers cannot supply an idempotency key. The key is derived from the
/// mission, optional phase, finite effect kind, typed operation slot, and
/// logical attempt so retries in a resumed process reconstruct exactly the
/// same external-effect identity without colliding with sibling operations.
#[derive(Clone)]
pub struct OutboxIntent {
    idempotency_key: String,
    mission_id: MissionId,
    phase_id: Option<String>,
    effect_kind: OutboxEffectKind,
    operation_slot: EffectOperationSlot,
    logical_attempt: u32,
    payload: Value,
    process_intent: Option<ProcessIntentEnvelope>,
}

impl OutboxIntent {
    /// Constructor for effect rows whose external effect is not a spawn-gated
    /// provider process. Supervised **provider** processes must use
    /// [`Self::for_process`] so release authorization is request-bound; the
    /// Git lane (B4-DESIGN §1.4) has no launcher gate and uses this
    /// constructor. Crate-private: no out-of-crate caller can forge an
    /// untyped effect row.
    pub(crate) fn for_mission(
        mission_id: MissionId,
        phase_id: Option<String>,
        effect_kind: OutboxEffectKind,
        operation_slot: EffectOperationSlot,
        logical_attempt: u32,
        payload: Value,
    ) -> Result<Self, RuntimeStoreError> {
        if let Some(value) = phase_id.as_deref() {
            validate_atom("outbox phase ID", value, MAX_IDENTIFIER_BYTES)?;
        }
        if logical_attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "outbox logical attempt must be positive",
            ));
        }
        canonical_json(&payload, "outbox payload")?;
        let idempotency_key = effect_idempotency_key(
            &mission_id,
            phase_id.as_deref(),
            effect_kind,
            &operation_slot,
            logical_attempt,
        );
        Ok(Self {
            idempotency_key,
            mission_id,
            phase_id,
            effect_kind,
            operation_slot,
            logical_attempt,
            payload,
            process_intent: None,
        })
    }

    /// Creates a process effect whose durable identity includes the exact
    /// request fingerprint. `ProcessRequest` deliberately remains independent
    /// of mission/outbox identity; this constructor binds both domains and
    /// retains the exact key later used by the one-shot process service.
    #[cfg(unix)]
    pub fn for_process(
        mission_id: MissionId,
        phase_id: Option<String>,
        effect_kind: OutboxEffectKind,
        operation_slot: EffectOperationSlot,
        logical_attempt: u32,
        payload: Value,
        request: &ProcessRequest,
    ) -> Result<Self, RuntimeStoreError> {
        validate_process_effect_purpose(effect_kind, request.purpose())?;
        if let Some(value) = phase_id.as_deref() {
            validate_atom("outbox phase ID", value, MAX_IDENTIFIER_BYTES)?;
        }
        if logical_attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "outbox logical attempt must be positive",
            ));
        }
        canonical_json(&payload, "outbox payload")?;
        let fingerprint = request.fingerprint();
        let process_intent = ProcessIntentEnvelope::new(fingerprint);
        process_intent.canonical_json()?;
        let idempotency_key = process_effect_idempotency_key(
            &mission_id,
            phase_id.as_deref(),
            effect_kind,
            &operation_slot,
            logical_attempt,
            &process_intent.request_sha256,
        );
        Ok(Self {
            idempotency_key,
            mission_id,
            phase_id,
            effect_kind,
            operation_slot,
            logical_attempt,
            payload,
            process_intent: Some(process_intent),
        })
    }

    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// Binds this typed intent to the exact request accepted by the one-shot
    /// durable process service. The returned value is intentionally private to
    /// this crate and non-cloneable: handlers cannot reuse an outbox key as
    /// ambient process authority.
    #[cfg(unix)]
    pub(crate) fn bind_process(
        &self,
        request: &ProcessRequest,
    ) -> Result<ProcessEffectBinding, RuntimeStoreError> {
        validate_process_effect_purpose(self.effect_kind, request.purpose())?;
        let fingerprint = request.fingerprint();
        let expected = ProcessIntentEnvelope::new(fingerprint);
        if self
            .process_intent
            .as_ref()
            .is_none_or(|intent| intent.request_sha256 != expected.request_sha256)
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        Ok(ProcessEffectBinding {
            idempotency_key: self.idempotency_key.clone(),
            effect_kind: self.effect_kind,
            fingerprint,
        })
    }
}

/// Crate-private, one-shot binding between durable identity and exact process
/// semantics. It deliberately carries no executable path or launch authority.
#[cfg(unix)]
pub(crate) struct ProcessEffectBinding {
    idempotency_key: String,
    effect_kind: OutboxEffectKind,
    fingerprint: ProcessRequestFingerprint,
}

#[cfg(unix)]
impl ProcessEffectBinding {
    pub(crate) fn matches(&self, request: &ProcessRequest) -> bool {
        validate_process_effect_purpose(self.effect_kind, request.purpose()).is_ok()
            && self.fingerprint == request.fingerprint()
    }

    #[expect(
        dead_code,
        reason = "staged for durable recovery reconciliation and CLI composition"
    )]
    pub(crate) fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }
}

/// Opaque snapshot proving one exact typed process effect was pending before
/// this storage actor attempted to claim it. The prior attempt counter and all
/// immutable outbox identity fields prevent an older executing attempt from
/// being mistaken for this call's ambiguous commit.
#[cfg(unix)]
pub(crate) struct PendingProcessClaim {
    prior: OutboxEffect,
    fingerprint: ProcessRequestFingerprint,
}

/// Non-cloneable exact typed process attempt produced only by a prepared
/// request-bound claim.
pub(crate) struct ClaimedProcessAttempt {
    claimed: ClaimedOutboxEffect,
    fingerprint: ProcessRequestFingerprint,
}

impl ClaimedProcessAttempt {
    pub(crate) const fn claimed(&self) -> &ClaimedOutboxEffect {
        &self.claimed
    }

    pub(crate) fn idempotency_key(&self) -> &str {
        self.claimed.idempotency_key()
    }

    pub(crate) const fn claim_attempt(&self) -> u32 {
        self.claimed.claim_attempt()
    }

    pub(crate) const fn fingerprint(&self) -> ProcessRequestFingerprint {
        self.fingerprint
    }

    pub(crate) fn matches_request(&self, request: &ProcessRequest) -> bool {
        self.fingerprint == request.fingerprint()
    }

    pub(crate) fn into_terminal(self) -> ProcessTerminalClaim {
        ProcessTerminalClaim { attempt: self }
    }

    pub(crate) fn outcome_binding(&self) -> ProcessAttemptBinding {
        ProcessAttemptBinding {
            idempotency_key: self.claimed.idempotency_key.clone(),
            journal_sequence: self.claimed.journal_sequence,
            mission_id: self.claimed.mission_id.clone(),
            phase_id: self.claimed.phase_id.clone(),
            effect_kind: self.claimed.effect_kind,
            operation_slot: self.claimed.operation_slot.clone(),
            logical_attempt: self.claimed.logical_attempt,
            claim_attempt: self.claimed.claim_attempt,
            payload: self.claimed.payload.clone(),
            fingerprint: self.fingerprint,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessRecoveryMarkerStage {
    AttemptClaimed,
    SpawnPermitted,
    StartedObserved,
}

impl ProcessRecoveryMarkerStage {
    const fn transition_kind(self) -> &'static str {
        match self {
            Self::AttemptClaimed => PROCESS_ATTEMPT_CLAIMED_TRANSITION_KIND,
            Self::SpawnPermitted => PROCESS_SPAWN_PERMITTED_TRANSITION_KIND,
            Self::StartedObserved => PROCESS_STARTED_OBSERVED_TRANSITION_KIND,
        }
    }

    const fn record_tag(self) -> &'static str {
        match self {
            Self::AttemptClaimed => "attempt_claimed",
            Self::SpawnPermitted => "spawn_permitted",
            Self::StartedObserved => "started_observed",
        }
    }

    fn from_transition_kind(kind: &str) -> Option<Self> {
        match kind {
            PROCESS_ATTEMPT_CLAIMED_TRANSITION_KIND => Some(Self::AttemptClaimed),
            PROCESS_SPAWN_PERMITTED_TRANSITION_KIND => Some(Self::SpawnPermitted),
            PROCESS_STARTED_OBSERVED_TRANSITION_KIND => Some(Self::StartedObserved),
            _ => None,
        }
    }
}

/// Immutable recovery binding for one exact process attempt stage.
///
/// It stores digests rather than process payloads. All three stages bind every
/// immutable outbox field. The claim marker is inserted atomically with the
/// claim, the spawn permit follows runtime initialization, and StartedObserved
/// additionally binds the exact release authorization and kernel identity.
struct ProcessRecoveryMarker {
    stage: ProcessRecoveryMarkerStage,
    idempotency_key: String,
    journal_sequence: i64,
    mission_id: MissionId,
    phase_id: Option<String>,
    effect_kind: OutboxEffectKind,
    operation_slot: EffectOperationSlot,
    logical_attempt: u32,
    claim_attempt: u32,
    payload_sha256: String,
    request_sha256: String,
    marked_at_utc: String,
    started_identity: Option<ProcessStartedIdentityBinding>,
}

struct ProcessStartedIdentityBinding {
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
    release_authorized_at_utc: String,
}

impl ProcessRecoveryMarker {
    fn from_claimed(
        claimed: &ClaimedProcessAttempt,
        stage: ProcessRecoveryMarkerStage,
        marked_at_utc: &str,
    ) -> Result<Self, RuntimeStoreError> {
        validate_timestamp(marked_at_utc)?;
        let payload_json = canonical_json(&claimed.claimed.payload, "claimed process payload")?;
        Ok(Self {
            stage,
            idempotency_key: claimed.claimed.idempotency_key.clone(),
            journal_sequence: claimed.claimed.journal_sequence,
            mission_id: claimed.claimed.mission_id.clone(),
            phase_id: claimed.claimed.phase_id.clone(),
            effect_kind: claimed.claimed.effect_kind,
            operation_slot: claimed.claimed.operation_slot.clone(),
            logical_attempt: claimed.claimed.logical_attempt,
            claim_attempt: claimed.claimed.claim_attempt,
            payload_sha256: hex_digest(Sha256::digest(payload_json.as_bytes()).as_slice()),
            request_sha256: hex_digest(claimed.fingerprint.as_bytes()),
            marked_at_utc: marked_at_utc.to_owned(),
            started_identity: None,
        })
    }

    fn from_started(
        binding: &BoundStartedProcess<'_>,
        release_authorized_at_utc: &str,
        marked_at_utc: &str,
    ) -> Result<Self, RuntimeStoreError> {
        validate_timestamp(release_authorized_at_utc)?;
        validate_timestamp(marked_at_utc)?;
        let payload_json = canonical_json(&binding.attempt.payload, "started process payload")?;
        Ok(Self {
            stage: ProcessRecoveryMarkerStage::StartedObserved,
            idempotency_key: binding.attempt.idempotency_key.clone(),
            journal_sequence: binding.attempt.journal_sequence,
            mission_id: binding.attempt.mission_id.clone(),
            phase_id: binding.attempt.phase_id.clone(),
            effect_kind: binding.attempt.effect_kind,
            operation_slot: binding.attempt.operation_slot.clone(),
            logical_attempt: binding.attempt.logical_attempt,
            claim_attempt: binding.attempt.claim_attempt,
            payload_sha256: hex_digest(Sha256::digest(payload_json.as_bytes()).as_slice()),
            request_sha256: hex_digest(binding.attempt.fingerprint.as_bytes()),
            marked_at_utc: marked_at_utc.to_owned(),
            started_identity: Some(ProcessStartedIdentityBinding {
                pid: binding.authorized.pid,
                process_group_id: binding.authorized.process_group_id,
                process_start_identity: binding.authorized.process_start_identity.clone(),
                release_authorized_at_utc: release_authorized_at_utc.to_owned(),
            }),
        })
    }

    fn from_value(
        stage: ProcessRecoveryMarkerStage,
        value: &Value,
    ) -> Result<Self, RuntimeStoreError> {
        let expected_fields = if stage == ProcessRecoveryMarkerStage::StartedObserved {
            17
        } else {
            13
        };
        let object = value
            .as_object()
            .filter(|object| object.len() == expected_fields)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let version = object
            .get("version")
            .and_then(Value::as_u64)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        if version != u64::from(PROCESS_RECOVERY_MARKER_SCHEMA_VERSION)
            || object.get("record").and_then(Value::as_str) != Some(stage.record_tag())
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let idempotency_key = object
            .get("idempotency_key")
            .and_then(Value::as_str)
            .ok_or(RuntimeStoreError::CorruptDatabase)?
            .to_owned();
        validate_atom(
            "process recovery idempotency key",
            &idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let journal_sequence = object
            .get("journal_sequence")
            .and_then(Value::as_i64)
            .filter(|sequence| *sequence > 0)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let mission_id = MissionId::new(
            object
                .get("mission_id")
                .and_then(Value::as_str)
                .ok_or(RuntimeStoreError::CorruptDatabase)?,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let phase_id = match object
            .get("phase_id")
            .ok_or(RuntimeStoreError::CorruptDatabase)?
        {
            Value::Null => None,
            Value::String(value) => {
                validate_atom("process recovery phase ID", value, MAX_IDENTIFIER_BYTES)
                    .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
                Some(value.clone())
            }
            _ => return Err(RuntimeStoreError::CorruptDatabase),
        };
        let effect_kind = OutboxEffectKind::parse(
            object
                .get("effect_kind")
                .and_then(Value::as_str)
                .ok_or(RuntimeStoreError::CorruptDatabase)?,
        )?;
        let operation_slot = EffectOperationSlot::new(
            object
                .get("operation_slot")
                .and_then(Value::as_str)
                .ok_or(RuntimeStoreError::CorruptDatabase)?,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let logical_attempt = object
            .get("logical_attempt")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|attempt| *attempt > 0)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let claim_attempt = object
            .get("claim_attempt")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|attempt| *attempt > 0)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let payload_sha256 = object
            .get("payload_sha256")
            .and_then(Value::as_str)
            .filter(|value| valid_checksum(value))
            .ok_or(RuntimeStoreError::CorruptDatabase)?
            .to_owned();
        let request_sha256 = object
            .get("request_sha256")
            .and_then(Value::as_str)
            .filter(|value| valid_checksum(value))
            .ok_or(RuntimeStoreError::CorruptDatabase)?
            .to_owned();
        let marked_at_utc = object
            .get("marked_at_utc")
            .and_then(Value::as_str)
            .ok_or(RuntimeStoreError::CorruptDatabase)?
            .to_owned();
        validate_timestamp(&marked_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let started_identity = if stage == ProcessRecoveryMarkerStage::StartedObserved {
            let pid = object
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or(RuntimeStoreError::CorruptDatabase)?;
            let process_group_id = object
                .get("process_group_id")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .filter(|value| *value > 0)
                .ok_or(RuntimeStoreError::CorruptDatabase)?;
            let process_start_identity = object
                .get("process_start_identity")
                .and_then(Value::as_str)
                .ok_or(RuntimeStoreError::CorruptDatabase)?
                .to_owned();
            validate_atom(
                "started process identity",
                &process_start_identity,
                MAX_IDENTIFIER_BYTES,
            )
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
            let release_authorized_at_utc = object
                .get("release_authorized_at_utc")
                .and_then(Value::as_str)
                .ok_or(RuntimeStoreError::CorruptDatabase)?
                .to_owned();
            validate_timestamp(&release_authorized_at_utc)
                .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
            Some(ProcessStartedIdentityBinding {
                pid,
                process_group_id,
                process_start_identity,
                release_authorized_at_utc,
            })
        } else {
            None
        };
        Ok(Self {
            stage,
            idempotency_key,
            journal_sequence,
            mission_id,
            phase_id,
            effect_kind,
            operation_slot,
            logical_attempt,
            claim_attempt,
            payload_sha256,
            request_sha256,
            marked_at_utc,
            started_identity,
        })
    }

    fn transition_id(&self) -> String {
        process_recovery_transition_id(self.stage, &self.idempotency_key, self.claim_attempt)
    }

    fn to_value(&self) -> Value {
        let mut value = serde_json::json!({
            "version": PROCESS_RECOVERY_MARKER_SCHEMA_VERSION,
            "record": self.stage.record_tag(),
            "idempotency_key": self.idempotency_key,
            "journal_sequence": self.journal_sequence,
            "mission_id": self.mission_id.as_str(),
            "phase_id": self.phase_id,
            "effect_kind": self.effect_kind.as_str(),
            "operation_slot": self.operation_slot.as_str(),
            "logical_attempt": self.logical_attempt,
            "claim_attempt": self.claim_attempt,
            "payload_sha256": self.payload_sha256,
            "request_sha256": self.request_sha256,
            "marked_at_utc": self.marked_at_utc,
        });
        if let Some(identity) = &self.started_identity {
            if let Some(object) = value.as_object_mut() {
                object.insert("pid".to_owned(), Value::from(identity.pid));
                object.insert(
                    "process_group_id".to_owned(),
                    Value::from(identity.process_group_id),
                );
                object.insert(
                    "process_start_identity".to_owned(),
                    Value::from(identity.process_start_identity.clone()),
                );
                object.insert(
                    "release_authorized_at_utc".to_owned(),
                    Value::from(identity.release_authorized_at_utc.clone()),
                );
            }
        }
        value
    }

    fn intent(&self) -> Result<JournalIntent, RuntimeStoreError> {
        JournalIntent::new(
            self.transition_id(),
            Some(self.mission_id.clone()),
            self.stage.transition_kind(),
            self.to_value(),
            self.marked_at_utc.clone(),
        )
    }

    fn matches_effect(
        &self,
        effect: &OutboxEffect,
        request_sha256: &str,
    ) -> Result<bool, RuntimeStoreError> {
        let payload_json = canonical_json(&effect.payload, "recovered process payload")?;
        Ok(self.idempotency_key == effect.idempotency_key
            && self.journal_sequence == effect.journal_sequence
            && self.mission_id == effect.mission_id
            && self.phase_id == effect.phase_id
            && self.effect_kind == effect.effect_kind
            && self.operation_slot == effect.operation_slot
            && self.logical_attempt == effect.logical_attempt
            && self.claim_attempt == effect.attempts
            && self.payload_sha256
                == hex_digest(Sha256::digest(payload_json.as_bytes()).as_slice())
            && self.request_sha256 == request_sha256)
    }
}

fn process_recovery_transition_id(
    stage: ProcessRecoveryMarkerStage,
    idempotency_key: &str,
    attempt: u32,
) -> String {
    let mut digest = Sha256::new();
    digest_field(&mut digest, PROCESS_RECOVERY_TRANSITION_ID_DOMAIN);
    digest_field(&mut digest, stage.transition_kind().as_bytes());
    digest_field(&mut digest, idempotency_key.as_bytes());
    digest_field(&mut digest, &attempt.to_be_bytes());
    format!(
        "process_recovery_{}",
        hex_digest(digest.finalize().as_slice())
    )
}

fn insert_private_recovery_marker(
    transaction: &Transaction<'_>,
    marker: &ProcessRecoveryMarker,
) -> Result<(), RuntimeStoreError> {
    let intent = marker.intent()?;
    let prepared = PreparedIntent::new(&intent, StoreBoundaryKind::PrivateProcessLedger)?;
    if let Some(existing) = load_existing_transition(transaction, &intent.transition_id)? {
        return if existing.matches(&prepared) {
            Ok(())
        } else {
            Err(RuntimeStoreError::TransitionConflict)
        };
    }
    let (sequence, previous_checksum) = next_journal_position(transaction)?;
    let checksum = transition_checksum(
        RECORD_SCHEMA_VERSION,
        sequence,
        &previous_checksum,
        &prepared,
    );
    transaction
        .execute(
            "INSERT INTO journal (
                sequence, transition_id, mission_id, record_schema_version,
                transition_kind, payload_json, committed_at_utc, extra_json,
                outbox_fingerprint, projection_fingerprint, previous_checksum, checksum
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                sequence,
                intent.transition_id,
                intent.mission_id.as_ref().map(MissionId::as_str),
                RECORD_SCHEMA_VERSION,
                intent.kind,
                prepared.payload_json,
                intent.committed_at_utc,
                prepared.extra_json,
                prepared.outbox_fingerprint,
                prepared.projection_fingerprint,
                previous_checksum,
                checksum,
            ],
        )
        .map_err(map_journal_insert_error)?;
    transaction
        .execute(
            "INSERT INTO command_ack (journal_sequence, acknowledged_at_utc)
             VALUES (?1, ?2)",
            params![sequence, marker.marked_at_utc],
        )
        .map_err(|source| {
            RuntimeStoreError::operation("acknowledge process recovery marker", source)
        })?;
    Ok(())
}

struct StoredProcessRecoveryMarkerRow {
    sequence: i64,
    mission_id: Option<String>,
    kind: String,
    payload_json: String,
    committed_at_utc: String,
    acknowledgement: Option<i64>,
}

fn load_process_recovery_marker(
    connection: &Connection,
    effect: &OutboxEffect,
    request_sha256: &str,
    stage: ProcessRecoveryMarkerStage,
) -> Result<Option<ProcessRecoveryMarker>, RuntimeStoreError> {
    let transition_id =
        process_recovery_transition_id(stage, &effect.idempotency_key, effect.attempts);
    let row: Option<StoredProcessRecoveryMarkerRow> = connection
        .query_row(
            "SELECT journal.sequence, journal.mission_id, journal.transition_kind,
                    journal.payload_json, journal.committed_at_utc,
                    command_ack.journal_sequence
             FROM journal
             LEFT JOIN command_ack ON command_ack.journal_sequence = journal.sequence
             WHERE journal.transition_id = ?1",
            [&transition_id],
            |row| {
                Ok(StoredProcessRecoveryMarkerRow {
                    sequence: row.get(0)?,
                    mission_id: row.get(1)?,
                    kind: row.get(2)?,
                    payload_json: row.get(3)?,
                    committed_at_utc: row.get(4)?,
                    acknowledgement: row.get(5)?,
                })
            },
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load process recovery marker", source))?;
    let Some(row) = row else {
        return Ok(None);
    };
    if row.kind != stage.transition_kind()
        || row.mission_id.as_deref() != Some(effect.mission_id.as_str())
        || row.acknowledgement != Some(row.sequence)
        || row.sequence <= effect.journal_sequence
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let value = validate_stored_json(&row.payload_json, "process recovery marker")?;
    let marker = ProcessRecoveryMarker::from_value(stage, &value)?;
    if marker.transition_id() != transition_id
        || marker.marked_at_utc != row.committed_at_utc
        || !marker.matches_effect(effect, request_sha256)?
        || canonical_json(&marker.to_value(), "process recovery marker")? != row.payload_json
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    match stage {
        ProcessRecoveryMarkerStage::AttemptClaimed => {
            let claimed_at_utc: String = connection
                .query_row(
                    "SELECT claimed_at_utc FROM outbox_attempt_claim
                     WHERE idempotency_key = ?1 AND attempt = ?2",
                    params![effect.idempotency_key, effect.attempts],
                    |row| row.get(0),
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("load process recovery claim", source)
                })?;
            if claimed_at_utc != marker.marked_at_utc {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        }
        ProcessRecoveryMarkerStage::SpawnPermitted => {
            let claim = load_process_recovery_marker(
                connection,
                effect,
                request_sha256,
                ProcessRecoveryMarkerStage::AttemptClaimed,
            )?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
            let claim_sequence = connection
                .query_row(
                    "SELECT sequence FROM journal WHERE transition_id = ?1",
                    [claim.transition_id()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("load process recovery claim sequence", source)
                })?;
            if row.sequence <= claim_sequence {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        }
        ProcessRecoveryMarkerStage::StartedObserved => {
            let permit = load_process_recovery_marker(
                connection,
                effect,
                request_sha256,
                ProcessRecoveryMarkerStage::SpawnPermitted,
            )?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
            let permit_sequence = connection
                .query_row(
                    "SELECT sequence FROM journal WHERE transition_id = ?1",
                    [permit.transition_id()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("load process spawn permit sequence", source)
                })?;
            let identity = marker
                .started_identity
                .as_ref()
                .ok_or(RuntimeStoreError::CorruptDatabase)?;
            let recorded =
                load_execution_identity(connection, &effect.idempotency_key, effect.attempts)?
                    .ok_or(RuntimeStoreError::CorruptDatabase)?;
            let authorization = load_process_release_authorization(
                connection,
                &effect.idempotency_key,
                effect.attempts,
            )?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
            if row.sequence <= permit_sequence
                || identity.pid != recorded.pid
                || identity.process_group_id != recorded.process_group_id
                || identity.process_start_identity != recorded.process_start_identity
                || identity.pid != authorization.pid
                || identity.process_group_id != authorization.process_group_id
                || identity.process_start_identity != authorization.process_start_identity
                || identity.release_authorized_at_utc != authorization.authorized_at_utc
            {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        }
    }
    Ok(Some(marker))
}

/// Immutable exact binding copied into a mapped outcome without copying its
/// one-shot terminal authority.
pub(crate) struct ProcessAttemptBinding {
    idempotency_key: String,
    journal_sequence: i64,
    mission_id: MissionId,
    phase_id: Option<String>,
    effect_kind: OutboxEffectKind,
    operation_slot: EffectOperationSlot,
    logical_attempt: u32,
    claim_attempt: u32,
    payload: Value,
    fingerprint: ProcessRequestFingerprint,
}

impl ProcessAttemptBinding {
    pub(crate) fn matches(&self, attempt: &ClaimedProcessAttempt) -> bool {
        self.idempotency_key == attempt.claimed.idempotency_key
            && self.journal_sequence == attempt.claimed.journal_sequence
            && self.mission_id == attempt.claimed.mission_id
            && self.phase_id == attempt.claimed.phase_id
            && self.effect_kind == attempt.claimed.effect_kind
            && self.operation_slot == attempt.claimed.operation_slot
            && self.logical_attempt == attempt.claimed.logical_attempt
            && self.claim_attempt == attempt.claimed.claim_attempt
            && self.payload == attempt.claimed.payload
            && self.fingerprint == attempt.fingerprint
    }

    #[expect(
        dead_code,
        reason = "staged for durable recovery reconciliation and CLI composition"
    )]
    pub(crate) const fn claim_attempt(&self) -> u32 {
        self.claim_attempt
    }

    #[expect(
        dead_code,
        reason = "staged for durable recovery reconciliation and CLI composition"
    )]
    pub(crate) const fn fingerprint(&self) -> ProcessRequestFingerprint {
        self.fingerprint
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        fingerprint: ProcessRequestFingerprint,
        claim_attempt: u32,
    ) -> Result<Self, RuntimeStoreError> {
        let mission_id = MissionId::new("process-mapper-test")
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let operation_slot = EffectOperationSlot::new("process-mapper-test")?;
        Ok(Self {
            idempotency_key: "process-mapper-test-key".to_owned(),
            journal_sequence: 1,
            mission_id,
            phase_id: Some("process-mapper-test-phase".to_owned()),
            effect_kind: OutboxEffectKind::ProviderProcess,
            operation_slot,
            logical_attempt: 1,
            claim_attempt,
            payload: serde_json::json!({"test":true}),
            fingerprint,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExactProcessClaimError {
    ProvenNotCommitted,
    Indeterminate,
}

/// Restart-only authority reconstructed from one exact durable process attempt.
///
/// It carries no executable, start-gate permit, retry authorization, or ambient
/// process-control capability. Cleanup authority can only be obtained by
/// consuming a classification that retained an exact blocked/released identity.
pub(crate) struct RecoveredProcessAttempt {
    kind: RecoveredProcessAttemptKind,
}

enum RecoveredProcessAttemptKind {
    ClaimedBeforeSpawn {
        terminal: ProcessTerminalClaim,
    },
    SpawnUnobserved,
    BlockedLauncher {
        terminal: ProcessTerminalClaim,
        identity: ProcessExecutionIdentity,
    },
    AuthorizedWithoutStarted {
        terminal: ProcessTerminalClaim,
        identity: ProcessExecutionIdentity,
    },
    StartedExecuting {
        terminal: ProcessTerminalClaim,
        identity: ProcessExecutionIdentity,
    },
    UnreleasedUncertain {
        identity: ProcessExecutionIdentity,
    },
    AuthorizedUncertain {
        identity: ProcessExecutionIdentity,
    },
    StartedUncertain {
        identity: ProcessExecutionIdentity,
    },
    Unresolved,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RecoveredProcessAttemptClassification {
    ClaimedBeforeSpawn,
    SpawnUnobserved,
    BlockedLauncher,
    AuthorizedWithoutStarted,
    StartedExecuting,
    UnreleasedUncertain,
    AuthorizedUncertain,
    StartedUncertain,
    Unresolved,
}

enum RecoveredCleanupResolution {
    NotStarted(ProcessTerminalClaim),
    ProcessUncertain(ProcessTerminalClaim),
    CleanupOnly,
}

/// Non-cloneable cleanup capability reconstructed from one consumed recovered
/// attempt. Its raw identity is visible only inside this crate and is never a
/// general PID/PGID signalling API.
pub(crate) struct RecoveredProcessCleanup {
    identity: ProcessExecutionIdentity,
    resolution: RecoveredCleanupResolution,
}

impl RecoveredProcessAttempt {
    pub(crate) const fn classification(&self) -> RecoveredProcessAttemptClassification {
        match self.kind {
            RecoveredProcessAttemptKind::ClaimedBeforeSpawn { .. } => {
                RecoveredProcessAttemptClassification::ClaimedBeforeSpawn
            }
            RecoveredProcessAttemptKind::SpawnUnobserved => {
                RecoveredProcessAttemptClassification::SpawnUnobserved
            }
            RecoveredProcessAttemptKind::BlockedLauncher { .. } => {
                RecoveredProcessAttemptClassification::BlockedLauncher
            }
            RecoveredProcessAttemptKind::AuthorizedWithoutStarted { .. } => {
                RecoveredProcessAttemptClassification::AuthorizedWithoutStarted
            }
            RecoveredProcessAttemptKind::StartedExecuting { .. } => {
                RecoveredProcessAttemptClassification::StartedExecuting
            }
            RecoveredProcessAttemptKind::UnreleasedUncertain { .. } => {
                RecoveredProcessAttemptClassification::UnreleasedUncertain
            }
            RecoveredProcessAttemptKind::AuthorizedUncertain { .. } => {
                RecoveredProcessAttemptClassification::AuthorizedUncertain
            }
            RecoveredProcessAttemptKind::StartedUncertain { .. } => {
                RecoveredProcessAttemptClassification::StartedUncertain
            }
            RecoveredProcessAttemptKind::Unresolved => {
                RecoveredProcessAttemptClassification::Unresolved
            }
        }
    }

    fn into_before_spawn_terminal(self) -> Result<ProcessTerminalClaim, RuntimeStoreError> {
        match self.kind {
            RecoveredProcessAttemptKind::ClaimedBeforeSpawn { terminal } => Ok(terminal),
            _ => Err(RuntimeStoreError::InvalidOutboxTransition),
        }
    }

    pub(crate) fn into_cleanup(self) -> Result<RecoveredProcessCleanup, RuntimeStoreError> {
        let (identity, resolution) = match self.kind {
            RecoveredProcessAttemptKind::BlockedLauncher { terminal, identity } => {
                (identity, RecoveredCleanupResolution::NotStarted(terminal))
            }
            RecoveredProcessAttemptKind::AuthorizedWithoutStarted { terminal, identity }
            | RecoveredProcessAttemptKind::StartedExecuting { terminal, identity } => (
                identity,
                RecoveredCleanupResolution::ProcessUncertain(terminal),
            ),
            RecoveredProcessAttemptKind::UnreleasedUncertain { identity } => {
                (identity, RecoveredCleanupResolution::CleanupOnly)
            }
            RecoveredProcessAttemptKind::AuthorizedUncertain { identity }
            | RecoveredProcessAttemptKind::StartedUncertain { identity } => {
                (identity, RecoveredCleanupResolution::CleanupOnly)
            }
            _ => return Err(RuntimeStoreError::InvalidOutboxTransition),
        };
        Ok(RecoveredProcessCleanup {
            identity,
            resolution,
        })
    }
}

impl RecoveredProcessCleanup {
    pub(crate) const fn pid(&self) -> u32 {
        self.identity.pid()
    }

    pub(crate) const fn process_group_id(&self) -> u32 {
        self.identity.process_group_id()
    }

    pub(crate) fn process_start_identity(&self) -> &str {
        self.identity.process_start_identity()
    }

    fn into_group_absent_resolution(
        self,
        absence: &ExactProcessGroupAbsence,
    ) -> Result<RecoveredCleanupResolution, RuntimeStoreError> {
        if absence.pid() != self.identity.pid()
            || absence.process_group_id() != self.identity.process_group_id()
            || absence.process_start_identity() != self.identity.process_start_identity()
        {
            return Err(RuntimeStoreError::ProcessIdentityMismatch);
        }
        Ok(self.resolution)
    }
}

impl fmt::Debug for OutboxIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboxIntent")
            .field("effect_kind", &self.effect_kind)
            .field("operation_slot", &self.operation_slot)
            .field("has_phase", &self.phase_id.is_some())
            .field("logical_attempt", &self.logical_attempt)
            .field("has_process_intent", &self.process_intent.is_some())
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

/// Durable lifecycle state of one external effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutboxState {
    Pending,
    Executing,
    Succeeded,
    Failed,
    Uncertain,
}

impl OutboxState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Executing => "executing",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Uncertain => "uncertain",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "executing" => Ok(Self::Executing),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "uncertain" => Ok(Self::Uncertain),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }
}

/// Finite observation classifications accepted by the durable effect history.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EffectEvidenceCode {
    ExitObservedSuccess,
    ExitObservedFailure,
    ProcessNotStarted,
    ProcessFailed,
    ProcessUncertain,
    ProcessStateUnobservable,
    ExitStatusLost,
    RemoteStateUnobservable,
    ObservedAbsent,
    PolicyAuthorizedRetry,
    OperatorAuthorizedRetry,
    LegacyV1Succeeded,
    LegacyV1Failed,
    LegacyV1Uncertain,
    LegacyV1Pending,
}

impl EffectEvidenceCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::ExitObservedSuccess => "exit_observed_success",
            Self::ExitObservedFailure => "exit_observed_failure",
            Self::ProcessNotStarted => "process_not_started",
            Self::ProcessFailed => "process_failed",
            Self::ProcessUncertain => "process_uncertain",
            Self::ProcessStateUnobservable => "process_state_unobservable",
            Self::ExitStatusLost => "exit_status_lost",
            Self::RemoteStateUnobservable => "remote_state_unobservable",
            Self::ObservedAbsent => "observed_absent",
            Self::PolicyAuthorizedRetry => "policy_authorized_retry",
            Self::OperatorAuthorizedRetry => "operator_authorized_retry",
            Self::LegacyV1Succeeded => "legacy_v1_succeeded",
            Self::LegacyV1Failed => "legacy_v1_failed",
            Self::LegacyV1Uncertain => "legacy_v1_uncertain",
            Self::LegacyV1Pending => "legacy_v1_pending",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "exit_observed_success" => Ok(Self::ExitObservedSuccess),
            "exit_observed_failure" => Ok(Self::ExitObservedFailure),
            "process_not_started" => Ok(Self::ProcessNotStarted),
            "process_failed" => Ok(Self::ProcessFailed),
            "process_uncertain" => Ok(Self::ProcessUncertain),
            "process_state_unobservable" => Ok(Self::ProcessStateUnobservable),
            "exit_status_lost" => Ok(Self::ExitStatusLost),
            "remote_state_unobservable" => Ok(Self::RemoteStateUnobservable),
            "observed_absent" => Ok(Self::ObservedAbsent),
            "policy_authorized_retry" => Ok(Self::PolicyAuthorizedRetry),
            "operator_authorized_retry" => Ok(Self::OperatorAuthorizedRetry),
            "legacy_v1_succeeded" => Ok(Self::LegacyV1Succeeded),
            "legacy_v1_failed" => Ok(Self::LegacyV1Failed),
            "legacy_v1_uncertain" => Ok(Self::LegacyV1Uncertain),
            "legacy_v1_pending" => Ok(Self::LegacyV1Pending),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }
}

/// Bounded reason that a supervised target was proven never to have executed.
///
/// These values deliberately mirror the process layer without storing free-form
/// launcher diagnostics in the durable evidence ledger.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessNotStartedEvidenceReason {
    Cancelled,
    Deadline,
    SpawnFailed,
    GateRejected,
    GateIndeterminate,
    GateProtocol,
    RecoveredBeforeSpawn,
}

/// Bounded reason that a released process attempt did not succeed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StartedProcessFailureEvidence {
    /// The direct child exited because of a positive operating-system signal.
    Signaled(i32),
    /// The hard process deadline elapsed.
    Deadline,
    /// The output-inactivity deadline elapsed.
    Stalled,
    /// Cancellation won before a successful exit.
    Cancelled,
    /// Output exceeded the configured retention limit.
    OutputLimit,
    /// A finite process, pipe, thread, or group-control operation failed after
    /// process ownership was otherwise resolved.
    InfrastructureFailure,
}

impl StartedProcessFailureEvidence {
    const fn valid(self) -> bool {
        !matches!(self, Self::Signaled(signal) if signal <= 0)
    }
}

/// Bounded reason that process execution or launcher ownership is inconclusive.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProcessUncertaintyEvidence {
    /// Release-grant delivery failed where the target may have received it.
    ReleaseDelivery,
    /// Pre-release cleanup could not prove the blocked launcher absent.
    CleanupIncomplete(ProcessNotStartedEvidenceReason),
    /// A production process report appeared without a gate lifecycle.
    UngatedExecution,
    /// A started process report could not prove all owned resources absent.
    StartedOwnershipUnresolved,
    /// The supervisor failed to return a structurally proven process outcome.
    SupervisorOutcomeLost,
    /// Durable result persistence may have committed, but commit status is unknown.
    CommitIndeterminate,
}

/// Bounded evidence with no caller-defined keys or free-form values.
#[derive(Clone, Eq, PartialEq, Serialize)]
pub struct EffectEvidence {
    code: EffectEvidenceCode,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    authorization_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_not_started_reason: Option<ProcessNotStartedEvidenceReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_process_failure: Option<StartedProcessFailureEvidence>,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_uncertainty: Option<ProcessUncertaintyEvidence>,
}

impl EffectEvidence {
    #[must_use]
    pub const fn exit_observed_success() -> Self {
        Self::with_exit(EffectEvidenceCode::ExitObservedSuccess, 0)
    }

    pub fn exit_observed_failure(exit_code: i32) -> Result<Self, RuntimeStoreError> {
        if exit_code == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "failed exit evidence requires a nonzero exit code",
            ));
        }
        Ok(Self::with_exit(
            EffectEvidenceCode::ExitObservedFailure,
            exit_code,
        ))
    }

    #[must_use]
    pub const fn process_not_started(reason: ProcessNotStartedEvidenceReason) -> Self {
        Self {
            code: EffectEvidenceCode::ProcessNotStarted,
            exit_code: None,
            authorization_sha256: None,
            process_not_started_reason: Some(reason),
            started_process_failure: None,
            process_uncertainty: None,
        }
    }

    /// Creates failure evidence for a process whose target may have executed.
    /// Unresolved ownership must instead use
    /// [`ProcessUncertaintyEvidence::StartedOwnershipUnresolved`].
    pub fn process_failed(
        failure: StartedProcessFailureEvidence,
    ) -> Result<Self, RuntimeStoreError> {
        if !failure.valid() {
            return Err(RuntimeStoreError::InvalidIntent(
                "signal failure evidence requires a positive signal number",
            ));
        }
        Ok(Self {
            code: EffectEvidenceCode::ProcessFailed,
            exit_code: None,
            authorization_sha256: None,
            process_not_started_reason: None,
            started_process_failure: Some(failure),
            process_uncertainty: None,
        })
    }

    /// Creates finite uncertainty evidence for process execution or ownership.
    #[must_use]
    pub const fn process_uncertain(uncertainty: ProcessUncertaintyEvidence) -> Self {
        Self {
            code: EffectEvidenceCode::ProcessUncertain,
            exit_code: None,
            authorization_sha256: None,
            process_not_started_reason: None,
            started_process_failure: None,
            process_uncertainty: Some(uncertainty),
        }
    }

    #[must_use]
    pub const fn process_state_unobservable() -> Self {
        Self::without_fields(EffectEvidenceCode::ProcessStateUnobservable)
    }

    #[must_use]
    pub const fn exit_status_lost() -> Self {
        Self::without_fields(EffectEvidenceCode::ExitStatusLost)
    }

    #[must_use]
    pub const fn remote_state_unobservable() -> Self {
        Self::without_fields(EffectEvidenceCode::RemoteStateUnobservable)
    }

    #[must_use]
    pub const fn observed_absent() -> Self {
        Self::without_fields(EffectEvidenceCode::ObservedAbsent)
    }

    const fn without_fields(code: EffectEvidenceCode) -> Self {
        Self {
            code,
            exit_code: None,
            authorization_sha256: None,
            process_not_started_reason: None,
            started_process_failure: None,
            process_uncertainty: None,
        }
    }

    const fn with_exit(code: EffectEvidenceCode, exit_code: i32) -> Self {
        Self {
            code,
            exit_code: Some(exit_code),
            authorization_sha256: None,
            process_not_started_reason: None,
            started_process_failure: None,
            process_uncertainty: None,
        }
    }

    fn authorized_retry(code: EffectEvidenceCode, authorization_sha256: String) -> Self {
        Self {
            code,
            exit_code: None,
            authorization_sha256: Some(authorization_sha256),
            process_not_started_reason: None,
            started_process_failure: None,
            process_uncertainty: None,
        }
    }

    fn migrated_v1(state: OutboxState, original_json: &str) -> Result<Self, RuntimeStoreError> {
        let _evidence = parse_legacy_v1_evidence(original_json)?;
        let code = match state {
            OutboxState::Succeeded => EffectEvidenceCode::LegacyV1Succeeded,
            OutboxState::Failed => EffectEvidenceCode::LegacyV1Failed,
            OutboxState::Uncertain => EffectEvidenceCode::LegacyV1Uncertain,
            OutboxState::Pending => EffectEvidenceCode::LegacyV1Pending,
            OutboxState::Executing => return Err(RuntimeStoreError::CorruptDatabase),
        };
        Ok(Self {
            code,
            exit_code: None,
            authorization_sha256: Some(hex_digest(Sha256::digest(original_json).as_slice())),
            process_not_started_reason: None,
            started_process_failure: None,
            process_uncertainty: None,
        })
    }

    #[must_use]
    pub const fn code(&self) -> EffectEvidenceCode {
        self.code
    }

    /// Returns the observed exit code, when this evidence carries one
    /// (`exit_observed_success`/`exit_observed_failure` only). Cell 3's
    /// hermetic provider uses this to reconstruct an exact
    /// `MechanicalTermination::ProcessExited` after a crash, without
    /// re-deriving anything from provider output.
    #[must_use]
    pub(crate) const fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    #[must_use]
    pub const fn process_not_started_reason(&self) -> Option<ProcessNotStartedEvidenceReason> {
        self.process_not_started_reason
    }

    /// Returns the finite started-process failure classification, when present.
    #[must_use]
    pub const fn started_process_failure(&self) -> Option<StartedProcessFailureEvidence> {
        self.started_process_failure
    }

    /// Returns the finite process uncertainty classification, when present.
    #[must_use]
    pub const fn process_uncertainty(&self) -> Option<ProcessUncertaintyEvidence> {
        self.process_uncertainty
    }

    fn canonical_json(&self) -> Result<String, RuntimeStoreError> {
        serde_json::to_string(self)
            .map_err(|source| RuntimeStoreError::operation("encode effect evidence", source))
    }

    fn from_stored_json(value: &str) -> Result<Self, RuntimeStoreError> {
        if value.len() > MAX_EVIDENCE_BYTES {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let stored: StoredEffectEvidence =
            serde_json::from_str(value).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let result = Self {
            code: stored.code,
            exit_code: stored.exit_code,
            authorization_sha256: stored.authorization_sha256,
            process_not_started_reason: stored.process_not_started_reason,
            started_process_failure: stored.started_process_failure,
            process_uncertainty: stored.process_uncertainty,
        };
        if !result.valid_shape() || result.canonical_json()? != value {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(result)
    }

    fn valid_shape(&self) -> bool {
        match self.code {
            EffectEvidenceCode::ExitObservedSuccess => {
                self.exit_code == Some(0)
                    && self.authorization_sha256.is_none()
                    && self.process_not_started_reason.is_none()
                    && self.started_process_failure.is_none()
                    && self.process_uncertainty.is_none()
            }
            EffectEvidenceCode::ExitObservedFailure => {
                self.exit_code.is_some_and(|value| value != 0)
                    && self.authorization_sha256.is_none()
                    && self.process_not_started_reason.is_none()
                    && self.started_process_failure.is_none()
                    && self.process_uncertainty.is_none()
            }
            EffectEvidenceCode::ProcessNotStarted => {
                self.exit_code.is_none()
                    && self.authorization_sha256.is_none()
                    && self.process_not_started_reason.is_some()
                    && self.started_process_failure.is_none()
                    && self.process_uncertainty.is_none()
            }
            EffectEvidenceCode::ProcessFailed => {
                self.exit_code.is_none()
                    && self.authorization_sha256.is_none()
                    && self.process_not_started_reason.is_none()
                    && self
                        .started_process_failure
                        .is_some_and(StartedProcessFailureEvidence::valid)
                    && self.process_uncertainty.is_none()
            }
            EffectEvidenceCode::ProcessUncertain => {
                self.exit_code.is_none()
                    && self.authorization_sha256.is_none()
                    && self.process_not_started_reason.is_none()
                    && self.started_process_failure.is_none()
                    && self.process_uncertainty.is_some()
            }
            EffectEvidenceCode::PolicyAuthorizedRetry
            | EffectEvidenceCode::OperatorAuthorizedRetry
            | EffectEvidenceCode::LegacyV1Succeeded
            | EffectEvidenceCode::LegacyV1Failed
            | EffectEvidenceCode::LegacyV1Uncertain
            | EffectEvidenceCode::LegacyV1Pending => {
                self.exit_code.is_none()
                    && self
                        .authorization_sha256
                        .as_deref()
                        .is_some_and(valid_checksum)
                    && self.process_not_started_reason.is_none()
                    && self.started_process_failure.is_none()
                    && self.process_uncertainty.is_none()
            }
            _ => {
                self.exit_code.is_none()
                    && self.authorization_sha256.is_none()
                    && self.process_not_started_reason.is_none()
                    && self.started_process_failure.is_none()
                    && self.process_uncertainty.is_none()
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredEffectEvidence {
    code: EffectEvidenceCode,
    exit_code: Option<i32>,
    authorization_sha256: Option<String>,
    process_not_started_reason: Option<ProcessNotStartedEvidenceReason>,
    started_process_failure: Option<StartedProcessFailureEvidence>,
    process_uncertainty: Option<ProcessUncertaintyEvidence>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LegacyV1EffectEvidence {
    code: String,
    metadata: BTreeMap<String, String>,
}

fn parse_legacy_v1_evidence(value: &str) -> Result<LegacyV1EffectEvidence, RuntimeStoreError> {
    if value.len() > MAX_LEGACY_EVIDENCE_BYTES {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let evidence: LegacyV1EffectEvidence =
        serde_json::from_str(value).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    validate_atom("legacy evidence code", &evidence.code, MAX_KIND_BYTES)
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    if evidence.metadata.len() > MAX_LEGACY_EVIDENCE_FIELDS {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    for (key, item) in &evidence.metadata {
        validate_atom("legacy evidence key", key, MAX_KIND_BYTES)
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if sensitive_metadata_key(key)
            || item.len() > MAX_LEGACY_EVIDENCE_VALUE_BYTES
            || item.chars().any(char::is_control)
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    if serde_json::to_string(&evidence)
        .map_err(|source| RuntimeStoreError::operation("encode legacy effect evidence", source))?
        != value
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(evidence)
}

struct LegacyRetryEvidence {
    authority: RetryAuthority,
    source_state: OutboxState,
    action: RetryAction,
    decision_id: String,
    decided_at_utc: String,
}

fn bind_legacy_retry_evidence(
    evidence: &LegacyV1EffectEvidence,
    source_state: OutboxState,
) -> Result<Option<LegacyRetryEvidence>, RuntimeStoreError> {
    let authority = match evidence.code.as_str() {
        "policy_authorized_retry" => RetryAuthority::Policy,
        "operator_authorized_retry" => RetryAuthority::Operator,
        _ => return Ok(None),
    };
    if evidence.metadata.len() != 2 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let decision_id = evidence
        .metadata
        .get("decision_id")
        .cloned()
        .ok_or(RuntimeStoreError::CorruptDatabase)?;
    let decided_at_utc = evidence
        .metadata
        .get("decided_at_utc")
        .cloned()
        .ok_or(RuntimeStoreError::CorruptDatabase)?;
    validate_atom(
        "legacy retry decision ID",
        &decision_id,
        MAX_IDENTIFIER_BYTES,
    )
    .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    validate_timestamp(&decided_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    let action = match (authority, source_state) {
        (RetryAuthority::Policy, OutboxState::Failed)
        | (RetryAuthority::Operator, OutboxState::Failed) => RetryAction::RetryFailed,
        (RetryAuthority::Operator, OutboxState::Uncertain) => RetryAction::OperatorAuthorizedRetry,
        _ => return Err(RuntimeStoreError::CorruptDatabase),
    };
    Ok(Some(LegacyRetryEvidence {
        authority,
        source_state,
        action,
        decision_id,
        decided_at_utc,
    }))
}

impl fmt::Debug for EffectEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EffectEvidence")
            .field("code", &self.code)
            .field("has_exit_code", &self.exit_code.is_some())
            .field(
                "has_authorization_digest",
                &self.authorization_sha256.is_some(),
            )
            .finish()
    }
}

/// Exact operating-system identity recorded immediately after process spawn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessExecutionIdentity {
    attempt: u32,
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
    recorded_at_utc: String,
}

impl ProcessExecutionIdentity {
    pub fn new(
        attempt: u32,
        pid: u32,
        process_group_id: u32,
        process_start_identity: impl Into<String>,
        recorded_at_utc: impl Into<String>,
    ) -> Result<Self, RuntimeStoreError> {
        if attempt == 0 || pid == 0 || process_group_id == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "process identity requires positive attempt, PID, and process-group ID",
            ));
        }
        let process_start_identity = process_start_identity.into();
        let recorded_at_utc = recorded_at_utc.into();
        validate_atom(
            "process start identity",
            &process_start_identity,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_timestamp(&recorded_at_utc)?;
        Ok(Self {
            attempt,
            pid,
            process_group_id,
            process_start_identity,
            recorded_at_utc,
        })
    }

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn pid(&self) -> u32 {
        self.pid
    }

    #[must_use]
    pub const fn process_group_id(&self) -> u32 {
        self.process_group_id
    }

    #[must_use]
    pub fn process_start_identity(&self) -> &str {
        &self.process_start_identity
    }
}

/// Opaque proof that a retry was authorized by policy or an operator.
#[derive(Clone, Eq, PartialEq)]
pub struct RetryAuthorization {
    authority: RetryAuthority,
    idempotency_key: String,
    attempt: u32,
    source_state: OutboxState,
    action: RetryAction,
    target_state: OutboxState,
    policy_version: String,
    decision_id: String,
    decided_at_utc: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetryAuthority {
    Policy,
    Operator,
}

impl RetryAuthority {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Policy => "policy",
            Self::Operator => "operator",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "policy" => Ok(Self::Policy),
            "operator" => Ok(Self::Operator),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RetryAction {
    RetryFailed,
    OperatorAuthorizedRetry,
}

/// Private capability retained by the mission actor that evaluates retry
/// policy. Keeping both the type and constructor private prevents sibling
/// modules from manufacturing an authorization from caller-selected fields.
struct RetryIssuanceAuthority {
    _private: (),
}

impl RetryIssuanceAuthority {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "retry issuance is enrolled with the later mission actor composition"
        )
    )]
    const fn new() -> Self {
        Self { _private: () }
    }
}

impl RetryAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RetryFailed => "retry_failed",
            Self::OperatorAuthorizedRetry => "operator_authorized_retry",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "retry_failed" => Ok(Self::RetryFailed),
            "operator_authorized_retry" => Ok(Self::OperatorAuthorizedRetry),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }
}

impl RetryAuthorization {
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "retry authorization is minted by the later mission actor composition"
        )
    )]
    fn policy(
        _authority: &RetryIssuanceAuthority,
        idempotency_key: impl Into<String>,
        attempt: u32,
        policy_version: impl Into<String>,
        decision_id: impl Into<String>,
        decided_at_utc: impl Into<String>,
    ) -> Result<Self, RuntimeStoreError> {
        Self::new(
            RetryAuthority::Policy,
            idempotency_key,
            attempt,
            OutboxState::Failed,
            RetryAction::RetryFailed,
            OutboxState::Pending,
            policy_version,
            decision_id,
            decided_at_utc,
        )
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "retry authorization is minted by the later mission actor composition"
        )
    )]
    fn operator(
        _authority: &RetryIssuanceAuthority,
        idempotency_key: impl Into<String>,
        attempt: u32,
        policy_version: impl Into<String>,
        decision_id: impl Into<String>,
        decided_at_utc: impl Into<String>,
    ) -> Result<Self, RuntimeStoreError> {
        Self::new(
            RetryAuthority::Operator,
            idempotency_key,
            attempt,
            OutboxState::Uncertain,
            RetryAction::OperatorAuthorizedRetry,
            OutboxState::Pending,
            policy_version,
            decision_id,
            decided_at_utc,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "all authorization bindings are explicit"
    )]
    fn new(
        authority: RetryAuthority,
        idempotency_key: impl Into<String>,
        attempt: u32,
        source_state: OutboxState,
        action: RetryAction,
        target_state: OutboxState,
        policy_version: impl Into<String>,
        decision_id: impl Into<String>,
        decided_at_utc: impl Into<String>,
    ) -> Result<Self, RuntimeStoreError> {
        let idempotency_key = idempotency_key.into();
        let policy_version = policy_version.into();
        let decision_id = decision_id.into();
        let decided_at_utc = decided_at_utc.into();
        validate_atom(
            "retry effect idempotency key",
            &idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        if attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "retry authorization attempt must be positive",
            ));
        }
        validate_atom("retry policy version", &policy_version, MAX_KIND_BYTES)?;
        validate_atom("retry decision ID", &decision_id, MAX_IDENTIFIER_BYTES)?;
        validate_timestamp(&decided_at_utc)?;
        let valid_shape = matches!(
            (authority, source_state, action, target_state),
            (
                RetryAuthority::Policy,
                OutboxState::Failed,
                RetryAction::RetryFailed,
                OutboxState::Pending
            ) | (
                RetryAuthority::Operator,
                OutboxState::Uncertain,
                RetryAction::OperatorAuthorizedRetry,
                OutboxState::Pending
            )
        );
        if !valid_shape {
            return Err(RuntimeStoreError::InvalidIntent(
                "retry authorization has an invalid authority or state binding",
            ));
        }
        Ok(Self {
            authority,
            idempotency_key,
            attempt,
            source_state,
            action,
            target_state,
            policy_version,
            decision_id,
            decided_at_utc,
        })
    }

    fn evidence(&self) -> EffectEvidence {
        EffectEvidence::authorized_retry(
            match self.authority {
                RetryAuthority::Policy => EffectEvidenceCode::PolicyAuthorizedRetry,
                RetryAuthority::Operator => EffectEvidenceCode::OperatorAuthorizedRetry,
            },
            self.fingerprint(),
        )
    }

    fn fingerprint(&self) -> String {
        let mut digest = Sha256::new();
        digest_field(&mut digest, b"nanika-retry-authorization/v1");
        digest_field(&mut digest, self.authority.as_str().as_bytes());
        digest_field(&mut digest, self.idempotency_key.as_bytes());
        digest_field(&mut digest, &self.attempt.to_be_bytes());
        digest_field(&mut digest, self.source_state.as_str().as_bytes());
        digest_field(&mut digest, self.action.as_str().as_bytes());
        digest_field(&mut digest, self.target_state.as_str().as_bytes());
        digest_field(&mut digest, self.policy_version.as_bytes());
        digest_field(&mut digest, self.decision_id.as_bytes());
        digest_field(&mut digest, self.decided_at_utc.as_bytes());
        hex_digest(digest.finalize().as_slice())
    }

    fn matches_effect(
        &self,
        idempotency_key: &str,
        attempt: u32,
        source_state: OutboxState,
        action: RetryAction,
        target_state: OutboxState,
    ) -> bool {
        self.idempotency_key == idempotency_key
            && self.attempt == attempt
            && self.source_state == source_state
            && self.action == action
            && self.target_state == target_state
    }
}

impl fmt::Debug for RetryAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetryAuthorization")
            .field("authority", &self.authority)
            .field("attempt", &self.attempt)
            .field("source_state", &self.source_state)
            .field("action", &self.action)
            .field("target_state", &self.target_state)
            .field("idempotency_key", &"[REDACTED]")
            .field("policy_version", &"[REDACTED]")
            .field("decision_id", &"[REDACTED]")
            .finish()
    }
}

/// Reconciliation decision after an effect was claimed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EffectResolution {
    Succeeded(EffectEvidence),
    Failed(EffectEvidence),
    ProcessFailed(EffectEvidence),
    Uncertain(EffectEvidence),
    ProcessUncertain(EffectEvidence),
    NotStarted(EffectEvidence),
    ObservedAbsent(EffectEvidence),
    RetryFailed(RetryAuthorization),
    OperatorAuthorizedRetry(RetryAuthorization),
}

impl EffectResolution {
    const fn state(&self) -> OutboxState {
        match self {
            Self::Succeeded(_) => OutboxState::Succeeded,
            Self::Failed(_) | Self::ProcessFailed(_) | Self::NotStarted(_) => OutboxState::Failed,
            Self::Uncertain(_) | Self::ProcessUncertain(_) => OutboxState::Uncertain,
            Self::ObservedAbsent(_) | Self::RetryFailed(_) | Self::OperatorAuthorizedRetry(_) => {
                OutboxState::Pending
            }
        }
    }

    fn evidence(&self) -> EffectEvidence {
        match self {
            Self::Succeeded(value)
            | Self::Failed(value)
            | Self::ProcessFailed(value)
            | Self::Uncertain(value)
            | Self::ProcessUncertain(value)
            | Self::NotStarted(value)
            | Self::ObservedAbsent(value) => value.clone(),
            Self::RetryFailed(value) | Self::OperatorAuthorizedRetry(value) => value.evidence(),
        }
    }

    const fn retry_binding(&self) -> Option<(&RetryAuthorization, RetryAction)> {
        match self {
            Self::RetryFailed(value) => Some((value, RetryAction::RetryFailed)),
            Self::OperatorAuthorizedRetry(value) => {
                Some((value, RetryAction::OperatorAuthorizedRetry))
            }
            _ => None,
        }
    }

    const fn evidence_matches_resolution(&self) -> bool {
        matches!(
            self,
            Self::Succeeded(EffectEvidence {
                code: EffectEvidenceCode::ExitObservedSuccess,
                ..
            }) | Self::Failed(EffectEvidence {
                code: EffectEvidenceCode::ExitObservedFailure,
                ..
            }) | Self::ProcessFailed(EffectEvidence {
                code: EffectEvidenceCode::ProcessFailed,
                ..
            }) | Self::Uncertain(EffectEvidence {
                code: EffectEvidenceCode::ProcessStateUnobservable
                    | EffectEvidenceCode::ExitStatusLost
                    | EffectEvidenceCode::RemoteStateUnobservable,
                ..
            }) | Self::ProcessUncertain(EffectEvidence {
                code: EffectEvidenceCode::ProcessUncertain,
                ..
            }) | Self::NotStarted(EffectEvidence {
                code: EffectEvidenceCode::ProcessNotStarted,
                ..
            }) | Self::ObservedAbsent(EffectEvidence {
                code: EffectEvidenceCode::ObservedAbsent,
                ..
            }) | Self::RetryFailed(_)
                | Self::OperatorAuthorizedRetry(_)
        )
    }

    const fn requires_execution_identity(&self) -> bool {
        matches!(
            self,
            Self::Succeeded(_) | Self::Failed(_) | Self::ProcessFailed(_) | Self::Uncertain(_)
        )
    }
}

/// One effect reconstructed from durable state. Payloads are intentionally not
/// included in `Debug`, because they may contain mission context.
#[derive(Clone)]
pub struct OutboxEffect {
    idempotency_key: String,
    journal_sequence: i64,
    mission_id: MissionId,
    phase_id: Option<String>,
    effect_kind: OutboxEffectKind,
    operation_slot: EffectOperationSlot,
    logical_attempt: u32,
    payload: Value,
    state: OutboxState,
    attempts: u32,
    execution_identity: Option<ProcessExecutionIdentity>,
    updated_at_utc: String,
}

impl OutboxEffect {
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    #[must_use]
    pub const fn journal_sequence(&self) -> i64 {
        self.journal_sequence
    }

    #[must_use]
    pub const fn effect_kind(&self) -> OutboxEffectKind {
        self.effect_kind
    }

    #[must_use]
    pub fn operation_slot(&self) -> &EffectOperationSlot {
        &self.operation_slot
    }

    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    #[must_use]
    pub fn phase_id(&self) -> Option<&str> {
        self.phase_id.as_deref()
    }

    #[must_use]
    pub const fn logical_attempt(&self) -> u32 {
        self.logical_attempt
    }

    #[must_use]
    pub const fn state(&self) -> OutboxState {
        self.state
    }

    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }

    #[must_use]
    pub fn payload(&self) -> &Value {
        &self.payload
    }

    #[must_use]
    pub fn execution_identity(&self) -> Option<&ProcessExecutionIdentity> {
        self.execution_identity.as_ref()
    }

    #[must_use]
    pub fn updated_at_utc(&self) -> &str {
        &self.updated_at_utc
    }
}

impl fmt::Debug for OutboxEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OutboxEffect")
            .field("journal_sequence", &self.journal_sequence)
            .field("effect_kind", &self.effect_kind)
            .field("operation_slot", &self.operation_slot)
            .field("logical_attempt", &self.logical_attempt)
            .field("state", &self.state)
            .field("attempts", &self.attempts)
            .field("payload", &"[REDACTED]")
            .field("has_execution_identity", &self.execution_identity.is_some())
            .finish()
    }
}

/// Opaque proof that the storage actor durably claimed one exact outbox effect
/// attempt. This value carries no executable, filesystem, or spawn authority.
#[must_use = "a claimed outbox effect must be reconciled before it is discarded"]
pub struct ClaimedOutboxEffect {
    idempotency_key: String,
    journal_sequence: i64,
    mission_id: MissionId,
    phase_id: Option<String>,
    effect_kind: OutboxEffectKind,
    operation_slot: EffectOperationSlot,
    logical_attempt: u32,
    claim_attempt: u32,
    payload: Value,
}

// ---------------------------------------------------------------------------
// Knowledge publication queue (B3-DESIGN §2)
// ---------------------------------------------------------------------------

/// Publication counts by state, used by recovery assertions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PublicationCounts {
    /// Committed but not yet delivered.
    pub pending: u64,
    /// Delivered to a consumer.
    pub delivered: u64,
    /// Terminally failed and retained.
    pub dead_letter: u64,
}

/// Where one queued publication is in its lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationState {
    /// Enqueued and not yet delivered.
    Pending,
    /// Delivered to its consumer.
    Delivered,
    /// Delivery failed terminally. **Never** treated as success.
    DeadLetter,
}

impl PublicationState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::DeadLetter => "dead_letter",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "pending" => Ok(Self::Pending),
            "delivered" => Ok(Self::Delivered),
            "dead_letter" => Ok(Self::DeadLetter),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }
}

/// How a claimed publication finished.
///
/// There is deliberately no `Default` and no implicit success: a caller must
/// state the outcome, so "failure never advances a source checkpoint as
/// successful" (Addendum §K5) holds by construction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PublicationOutcome {
    /// The consumer accepted the publication.
    Delivered,
    /// Delivery failed terminally; the row is retained as a dead letter.
    DeadLetter,
}

impl PublicationOutcome {
    const fn state(self) -> PublicationState {
        match self {
            Self::Delivered => PublicationState::Delivered,
            Self::DeadLetter => PublicationState::DeadLetter,
        }
    }
}

/// One validated publication awaiting commit alongside its transition.
///
/// The fields are private and the constructor is the only way in, so a caller
/// cannot enqueue an unvalidated or unclassified row. Construction is driven
/// from the knowledge crate's typed `PublicationEvidence`, which is itself only
/// obtainable from a live `TypeRegistry::admit`.
pub struct PublicationIntent {
    idempotency_key: String,
    mission_id: MissionId,
    phase_id: Option<String>,
    logical_attempt: u32,
    namespace: String,
    type_name: String,
    primitive_kind: String,
    schema_version: u32,
    registry_generation: u64,
    sensitivity: String,
    identity: String,
    payload_json: String,
    payload_digest: String,
}

impl PublicationIntent {
    /// Validates one publication before it reaches the storage transaction.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::InvalidIntent`] for a zero logical attempt,
    /// an over-long identifier, or a payload that is not canonical JSON within
    /// [`MAX_JSON_BYTES`].
    pub fn new(
        mission_id: &MissionId,
        phase_id: Option<&str>,
        logical_attempt: u32,
        evidence: &orchestrator_knowledge::PublicationEvidence,
        payload_json: &str,
    ) -> Result<Self, RuntimeStoreError> {
        if logical_attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "knowledge publication logical attempt must be positive",
            ));
        }
        if let Some(value) = phase_id {
            validate_atom("publication phase ID", value, MAX_IDENTIFIER_BYTES)?;
        }
        validate_atom(
            "publication identity",
            evidence.identity(),
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_atom(
            "publication payload digest",
            evidence.payload_digest(),
            MAX_IDENTIFIER_BYTES,
        )?;
        if payload_json.is_empty() || payload_json.len() > MAX_JSON_BYTES {
            return Err(RuntimeStoreError::InvalidIntent(
                "knowledge publication payload is empty or exceeds the JSON bound",
            ));
        }
        // Re-canonicalize rather than trust the caller's bytes: the digest the
        // drain re-derives must be over exactly what is stored.
        let parsed: Value = serde_json::from_str(payload_json).map_err(|_| {
            RuntimeStoreError::InvalidIntent("knowledge publication payload is not JSON")
        })?;
        let payload_json = canonical_json(&parsed, "knowledge publication payload")?;
        let mut hasher = Sha256::new();
        hasher.update(payload_json.as_bytes());
        let payload_digest = hex_digest(hasher.finalize().as_slice());
        // A body-carrying primitive must enqueue exactly the bytes that were
        // admitted: the registry scanned *those* bytes for secrets, so a caller
        // must not be able to swap in different content afterwards. Body-less
        // primitives (blob, edge) digest their identity instead, and their
        // payload is derived metadata, so no equality holds there.
        let carries_body = matches!(
            evidence.primitive(),
            orchestrator_knowledge::PrimitiveKind::Record
                | orchestrator_knowledge::PrimitiveKind::Event
        );
        if carries_body && payload_digest != evidence.payload_digest() {
            return Err(RuntimeStoreError::InvalidIntent(
                "knowledge publication payload does not match the admitted envelope",
            ));
        }
        Ok(Self {
            idempotency_key: evidence.idempotency_key(
                mission_id.as_str(),
                phase_id,
                logical_attempt,
            ),
            mission_id: mission_id.clone(),
            phase_id: phase_id.map(ToOwned::to_owned),
            logical_attempt,
            namespace: evidence.namespace().as_str().to_owned(),
            type_name: evidence.type_name().as_str().to_owned(),
            primitive_kind: evidence.primitive().as_str().to_owned(),
            schema_version: evidence.schema_version().get(),
            registry_generation: evidence.generation().get(),
            sensitivity: evidence.sensitivity().as_str().to_owned(),
            identity: evidence.identity().to_owned(),
            payload_digest,
            payload_json,
        })
    }

    /// The durable idempotency key this publication will commit under.
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }
}

/// Bodies are never rendered: `Debug` must not leak a queued payload.
impl fmt::Debug for PublicationIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PublicationIntent")
            .field("namespace", &self.namespace)
            .field("type_name", &self.type_name)
            .field("primitive_kind", &self.primitive_kind)
            .field("sensitivity", &self.sensitivity)
            .field("payload", &"[REDACTED]")
            .field("payload_digest", &self.payload_digest)
            .finish()
    }
}

/// Non-`Clone` authority over exactly one claimed publication.
///
/// A handler cannot duplicate this token, so it cannot replay one idempotency
/// key as if it were two deliveries. It is consumed by
/// [`RuntimeStore::resolve_publication`].
pub struct ClaimedPublication {
    sequence: i64,
    idempotency_key: String,
    journal_sequence: i64,
    mission_id: MissionId,
    phase_id: Option<String>,
    logical_attempt: u32,
    namespace: String,
    type_name: String,
    primitive_kind: String,
    schema_version: u32,
    registry_generation: u64,
    sensitivity: String,
    identity: String,
    payload_json: String,
    payload_digest: String,
    claim_attempt: u32,
}

impl ClaimedPublication {
    /// Monotonic queue position; delivery order within a mission/phase.
    #[must_use]
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    /// The durable idempotency key. A consumer upserts on this.
    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    /// The journal transition that authorized this publication.
    #[must_use]
    pub const fn journal_sequence(&self) -> i64 {
        self.journal_sequence
    }

    /// The authorizing mission.
    #[must_use]
    pub const fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    /// The authorizing phase, when the transition had one.
    #[must_use]
    pub fn phase_id(&self) -> Option<&str> {
        self.phase_id.as_deref()
    }

    /// The logical attempt of the authorizing phase.
    #[must_use]
    pub const fn logical_attempt(&self) -> u32 {
        self.logical_attempt
    }

    /// Registered namespace of the published envelope.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Registered type name of the published envelope.
    #[must_use]
    pub fn type_name(&self) -> &str {
        &self.type_name
    }

    /// Primitive of the published envelope.
    #[must_use]
    pub fn primitive_kind(&self) -> &str {
        &self.primitive_kind
    }

    /// Schema version of the published envelope.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Registry generation the envelope was admitted under. The drain re-derives
    /// its envelope from this rather than assuming the current generation.
    #[must_use]
    pub const fn registry_generation(&self) -> u64 {
        self.registry_generation
    }

    /// Classification of the published envelope.
    #[must_use]
    pub fn sensitivity(&self) -> &str {
        &self.sensitivity
    }

    /// Identity text of the published envelope.
    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The stored canonical payload.
    #[must_use]
    pub fn payload_json(&self) -> &str {
        &self.payload_json
    }

    /// The digest recorded at enqueue time.
    #[must_use]
    pub fn payload_digest(&self) -> &str {
        &self.payload_digest
    }

    /// How many times this row has been claimed, including this claim.
    #[must_use]
    pub const fn claim_attempt(&self) -> u32 {
        self.claim_attempt
    }

    /// Recomputes the digest over the stored payload and compares it to the
    /// digest recorded at enqueue time.
    ///
    /// A row whose `payload_json` was altered in `runtime.db` therefore cannot
    /// be delivered as if it were the admitted one.
    #[must_use]
    pub fn payload_matches_digest(&self) -> bool {
        let mut digest = Sha256::new();
        digest.update(self.payload_json.as_bytes());
        hex_digest(digest.finalize().as_slice()) == self.payload_digest
    }
}

/// Bodies are never rendered: `Debug` must not leak a claimed payload.
impl fmt::Debug for ClaimedPublication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimedPublication")
            .field("sequence", &self.sequence)
            .field("namespace", &self.namespace)
            .field("type_name", &self.type_name)
            .field("payload", &"[REDACTED]")
            .field("payload_digest", &self.payload_digest)
            .field("claim_attempt", &self.claim_attempt)
            .finish()
    }
}

/// Consuming terminal authority for one exact typed process claim.
///
/// It is crate-private and non-cloneable so the same claimed attempt cannot be
/// bound to two different process receipts.
pub(crate) struct ProcessTerminalClaim {
    attempt: ClaimedProcessAttempt,
}

impl ClaimedOutboxEffect {
    fn from_claimed(effect: OutboxEffect) -> Result<Self, RuntimeStoreError> {
        if effect.state != OutboxState::Executing
            || effect.attempts == 0
            || effect.execution_identity.is_some()
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(Self {
            idempotency_key: effect.idempotency_key,
            journal_sequence: effect.journal_sequence,
            mission_id: effect.mission_id,
            phase_id: effect.phase_id,
            effect_kind: effect.effect_kind,
            operation_slot: effect.operation_slot,
            logical_attempt: effect.logical_attempt,
            claim_attempt: effect.attempts,
            payload: effect.payload,
        })
    }

    fn from_recovered_process(effect: OutboxEffect) -> Result<Self, RuntimeStoreError> {
        if !matches!(
            effect.state,
            OutboxState::Executing | OutboxState::Uncertain
        ) || effect.attempts == 0
        {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        Ok(Self {
            idempotency_key: effect.idempotency_key,
            journal_sequence: effect.journal_sequence,
            mission_id: effect.mission_id,
            phase_id: effect.phase_id,
            effect_kind: effect.effect_kind,
            operation_slot: effect.operation_slot,
            logical_attempt: effect.logical_attempt,
            claim_attempt: effect.attempts,
            payload: effect.payload,
        })
    }

    #[must_use]
    pub fn idempotency_key(&self) -> &str {
        &self.idempotency_key
    }

    #[must_use]
    pub const fn journal_sequence(&self) -> i64 {
        self.journal_sequence
    }

    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    #[must_use]
    pub fn phase_id(&self) -> Option<&str> {
        self.phase_id.as_deref()
    }

    #[must_use]
    pub const fn effect_kind(&self) -> OutboxEffectKind {
        self.effect_kind
    }

    #[must_use]
    pub fn operation_slot(&self) -> &EffectOperationSlot {
        &self.operation_slot
    }

    #[must_use]
    pub const fn logical_attempt(&self) -> u32 {
        self.logical_attempt
    }

    #[must_use]
    pub const fn claim_attempt(&self) -> u32 {
        self.claim_attempt
    }

    #[must_use]
    pub fn payload(&self) -> &Value {
        &self.payload
    }
}

impl ProcessTerminalClaim {
    pub(crate) const fn claimed(&self) -> &ClaimedOutboxEffect {
        self.attempt.claimed()
    }

    #[expect(
        dead_code,
        reason = "claim-attempt accessor staged for durable recovery reconciliation"
    )]
    pub(crate) const fn claim_attempt(&self) -> u32 {
        self.attempt.claim_attempt()
    }

    pub(crate) const fn fingerprint(&self) -> ProcessRequestFingerprint {
        self.attempt.fingerprint()
    }

    pub(crate) fn matches_binding(&self, binding: &ProcessAttemptBinding) -> bool {
        binding.matches(&self.attempt)
    }
}

impl fmt::Debug for ProcessTerminalClaim {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessTerminalClaim")
            .field("claim_attempt", &self.attempt.claim_attempt())
            .field("identity", &"[REDACTED]")
            .finish()
    }
}

impl fmt::Debug for ClaimedOutboxEffect {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaimedOutboxEffect")
            .field("idempotency_key", &"[REDACTED]")
            .field("journal_sequence", &self.journal_sequence)
            .field("effect_kind", &self.effect_kind)
            .field("operation_slot", &self.operation_slot)
            .field("logical_attempt", &self.logical_attempt)
            .field("claim_attempt", &self.claim_attempt)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}

/// Opaque proof that one exact claimed attempt, process request, and observed
/// kernel identity were committed durably before the start gate was released.
///
/// The receipt is intentionally non-Clone and exposes no path, executable,
/// process identifier, outbox key, or spawn authority. A later
/// `DurableProcessService` may consume it only inside this crate's composition
/// root together with the still-blocked one-shot gate request.
///
/// ```compile_fail
/// use orchestrator_app::ProcessReleaseAuthorization;
///
/// fn require_clone<T: Clone>() {}
/// require_clone::<ProcessReleaseAuthorization>();
/// ```
///
/// ```compile_fail
/// use orchestrator_app::ProcessReleaseAuthorization;
///
/// // Only an atomic RuntimeStore transaction can mint the receipt.
/// let _forged = ProcessReleaseAuthorization {};
/// ```
#[must_use = "a process release authorization must be consumed by its one-shot gate"]
pub struct ProcessReleaseAuthorization {
    _idempotency_key: String,
    claim_attempt: u32,
    request_fingerprint: ProcessRequestFingerprint,
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
}

/// Opaque identity of the exact launcher whose durable authorization was
/// accepted through its one-shot start gate. Cancellation, deadline, and
/// authenticated grant delivery still arbitrate after this point.
#[must_use = "an authorized launcher identity must be reconciled with the supervised outcome"]
pub(crate) struct AuthorizedLauncherIdentity {
    idempotency_key: String,
    claim_attempt: u32,
    request_fingerprint: ProcessRequestFingerprint,
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
}

struct BoundStartedProcess<'a> {
    attempt: &'a ProcessAttemptBinding,
    authorized: &'a AuthorizedLauncherIdentity,
}

impl AuthorizedLauncherIdentity {
    fn from_authorization(authorization: &ProcessReleaseAuthorization) -> Self {
        Self {
            idempotency_key: authorization._idempotency_key.clone(),
            claim_attempt: authorization.claim_attempt,
            request_fingerprint: authorization.request_fingerprint,
            pid: authorization.pid,
            process_group_id: authorization.process_group_id,
            process_start_identity: authorization.process_start_identity.clone(),
        }
    }

    pub(crate) fn matches(
        &self,
        pid: u32,
        process_group_id: u32,
        process_start_identity: &str,
    ) -> bool {
        self.pid == pid
            && self.process_group_id == process_group_id
            && self.process_start_identity == process_start_identity
    }

    pub(crate) fn matches_attempt_binding(&self, binding: &ProcessAttemptBinding) -> bool {
        self.idempotency_key == binding.idempotency_key
            && self.claim_attempt == binding.claim_attempt
            && self.request_fingerprint == binding.fingerprint
    }

    fn bind_started<'a>(
        &'a self,
        attempt: &'a ProcessAttemptBinding,
        receipt: &ProcessStartedReceipt,
    ) -> Result<BoundStartedProcess<'a>, RuntimeStoreError> {
        if !self.matches_attempt_binding(attempt)
            || !receipt.matches_request(attempt.fingerprint.as_bytes())
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        let observed = receipt.identity();
        if !self.matches(
            observed.pid(),
            observed.process_group_id(),
            observed.process_start_identity(),
        ) {
            return Err(RuntimeStoreError::ProcessIdentityMismatch);
        }
        Ok(BoundStartedProcess {
            attempt,
            authorized: self,
        })
    }

    #[cfg(test)]
    pub(crate) fn bind_for_test(&mut self, binding: &ProcessAttemptBinding) {
        self.idempotency_key.clone_from(&binding.idempotency_key);
        self.claim_attempt = binding.claim_attempt;
        self.request_fingerprint = binding.fingerprint;
    }

    #[cfg(test)]
    pub(crate) fn from_kernel_identity(
        identity: &orchestrator_process::KernelProcessIdentity,
        request_fingerprint: ProcessRequestFingerprint,
    ) -> Self {
        Self {
            idempotency_key: "process-mapper-test-key".to_owned(),
            claim_attempt: 1,
            request_fingerprint,
            pid: identity.pid(),
            process_group_id: identity.process_group_id(),
            process_start_identity: identity.process_start_identity().to_owned(),
        }
    }

    #[cfg(test)]
    pub(crate) fn from_test_parts(
        pid: u32,
        process_group_id: u32,
        process_start_identity: &str,
        request_fingerprint: ProcessRequestFingerprint,
    ) -> Self {
        Self {
            idempotency_key: "process-mapper-test-key".to_owned(),
            claim_attempt: 1,
            request_fingerprint,
            pid,
            process_group_id,
            process_start_identity: process_start_identity.to_owned(),
        }
    }
}

impl fmt::Debug for AuthorizedLauncherIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthorizedLauncherIdentity")
            .field("identity", &"[REDACTED]")
            .finish()
    }
}

impl ProcessReleaseAuthorization {
    /// Consumes the durable authorization and the only blocked-launcher reply
    /// capability together. `ProcessStartGateRequest::release_authorized`
    /// remains public in the lower process crate; this crate-private wrapper is
    /// the app composition invariant and additionally rejects a swapped gate.
    #[cfg(unix)]
    pub(crate) fn release(
        self,
        request: ProcessStartGateRequest,
    ) -> Result<AuthorizedLauncherIdentity, ProcessAuthorizationReleaseError> {
        let observed = ObservedGateBinding::from_request(&request);
        let matches = self.matches_observed(&observed);
        if !matches {
            drop(self);
            let _ = request.reject();
            return Err(ProcessAuthorizationReleaseError::BindingMismatch);
        }
        let authorized_identity = AuthorizedLauncherIdentity::from_authorization(&self);
        drop(self);
        match request.release_authorized() {
            Ok(()) => Ok(authorized_identity),
            Err(_) => Err(ProcessAuthorizationReleaseError::GateClosed(
                authorized_identity,
            )),
        }
    }

    fn matches_observed(&self, observed: &ObservedGateBinding<'_>) -> bool {
        observed
            .request_binding
            .matches_bytes(self.request_fingerprint.as_bytes())
            && observed.pid == self.pid
            && observed.process_group_id == self.process_group_id
            && observed.process_start_identity == self.process_start_identity
    }
}

#[derive(Debug)]
pub(crate) enum ProcessAuthorizationReleaseError {
    BindingMismatch,
    GateClosed(AuthorizedLauncherIdentity),
}

impl fmt::Debug for ProcessReleaseAuthorization {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessReleaseAuthorization")
            .field("claim_attempt", &self.claim_attempt)
            .field("idempotency_key", &"[REDACTED]")
            .field("request_fingerprint", &"[REDACTED]")
            .field("process_identity", &"[REDACTED]")
            .finish()
    }
}

struct ObservedGateBinding<'a> {
    request_binding: &'a ProcessRequestBinding,
    pid: u32,
    process_group_id: u32,
    process_start_identity: &'a str,
}

impl<'a> ObservedGateBinding<'a> {
    fn from_request(request: &'a ProcessStartGateRequest) -> Self {
        let identity = request.identity();
        Self {
            request_binding: request.request_binding(),
            pid: identity.pid(),
            process_group_id: identity.process_group_id(),
            process_start_identity: identity.process_start_identity(),
        }
    }
}

/// One immutable observation from the append-only attempt history.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectObservation {
    sequence: i64,
    attempt: u32,
    state: OutboxState,
    evidence: EffectEvidence,
    observed_at_utc: String,
}

impl EffectObservation {
    #[must_use]
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    #[must_use]
    pub const fn attempt(&self) -> u32 {
        self.attempt
    }

    #[must_use]
    pub const fn state(&self) -> OutboxState {
        self.state
    }

    #[must_use]
    pub fn evidence(&self) -> &EffectEvidence {
        &self.evidence
    }
}

/// One transactionally consistent view of an exact retained outbox effect and
/// the newest observation for its current attempt.
pub(crate) struct ExactOutboxSnapshot {
    effect: OutboxEffect,
    current_observation: Option<EffectObservation>,
}

impl ExactOutboxSnapshot {
    pub(crate) const fn effect(&self) -> &OutboxEffect {
        &self.effect
    }

    pub(crate) const fn current_observation(&self) -> Option<&EffectObservation> {
        self.current_observation.as_ref()
    }

    /// Returns the commit time bound to this snapshot's exact current
    /// observation.
    ///
    /// Process resolution writes the immutable observation and advances the
    /// outbox row with the same timestamp in one transaction. Requiring both
    /// copies to agree keeps a later compatibility projection from accepting
    /// either a stale observation or a separately mutated outbox clock.
    pub(crate) fn current_observation_committed_at_utc(
        &self,
    ) -> Result<Option<&str>, RuntimeStoreError> {
        let Some(observation) = self.current_observation.as_ref() else {
            return Ok(None);
        };
        if observation.attempt != self.effect.attempts
            || observation.state != self.effect.state
            || observation.observed_at_utc != self.effect.updated_at_utc
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(Some(&observation.observed_at_utc))
    }
}

/// Durable journal commit. This is deliberately not a command acknowledgement:
/// required compatibility projections may still be pending.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JournalCommit {
    sequence: i64,
    checksum: String,
    duplicate: bool,
    event_projection_recipe: Option<EventProjectionRecipe>,
}

impl JournalCommit {
    /// Builds a receipt for an in-memory fixture journal. Test-only: production
    /// receipts are minted by the storage actor inside the append transaction.
    #[cfg(any(test, feature = "test-support"))]
    pub const fn for_fixture(sequence: i64) -> Self {
        Self {
            sequence,
            checksum: String::new(),
            duplicate: false,
            event_projection_recipe: None,
        }
    }

    #[must_use]
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    #[must_use]
    pub fn checksum(&self) -> &str {
        &self.checksum
    }

    #[must_use]
    pub const fn duplicate(&self) -> bool {
        self.duplicate
    }

    /// Returns the sealed event-projection identity when this transition
    /// required [`CompatibilityProjection::EventLog`]. A projector must
    /// derive its published bytes from this recipe rather than choosing its
    /// own ID or sequence.
    #[must_use]
    pub fn event_projection_recipe(&self) -> Option<&EventProjectionRecipe> {
        self.event_projection_recipe.as_ref()
    }
}

/// Proof that all declared compatibility projections for one transition are
/// durable. Only this state makes the transition's outbox effects claimable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAcknowledgement {
    journal_sequence: i64,
    acknowledged_at_utc: String,
}

impl CommandAcknowledgement {
    #[must_use]
    pub const fn journal_sequence(&self) -> i64 {
        self.journal_sequence
    }

    #[must_use]
    pub fn acknowledged_at_utc(&self) -> &str {
        &self.acknowledged_at_utc
    }
}

/// Fail-closed storage errors. Payloads, database paths, and SQL parameters are
/// deliberately absent from every display message.
#[derive(Debug, Error)]
pub enum RuntimeStoreError {
    #[error("runtime store already has an in-process owner")]
    WriterLeased,
    #[error("runtime store intent is invalid: {0}")]
    InvalidIntent(&'static str),
    #[error("runtime store schema {found} is unsupported; expected {expected}")]
    UnsupportedSchema { found: i64, expected: i64 },
    #[error("runtime store boundary kind does not match the requested typed opener")]
    BoundaryKindMismatch,
    #[error("runtime store database is corrupt or violates its journal invariants")]
    CorruptDatabase,
    #[error("runtime transition ID was reused with different content")]
    TransitionConflict,
    #[error("runtime outbox idempotency key already belongs to another transition")]
    OutboxConflict,
    /// A publication key was reused, or a claim lost its race to another
    /// resolver. Carries no payload — see B3-DESIGN §7 rule 1.
    #[error("runtime knowledge publication conflicts with durable queue state")]
    PublicationConflict,
    #[error("runtime projection receipt conflicts with durable projection state")]
    ProjectionConflict,
    #[error("runtime transition did not require the supplied compatibility projection")]
    ProjectionNotRequired,
    #[error("runtime outbox state transition is invalid")]
    InvalidOutboxTransition,
    #[error("runtime expected outbox effect is not claimable")]
    ExpectedEffectNotClaimable,
    #[error("runtime expected outbox effect is not next in deterministic delivery order")]
    ExpectedEffectOutOfOrder,
    #[error("runtime typed process effect requires an exact capability-bearing claim")]
    TypedProcessRequiresExactClaim,
    #[error("runtime retry authorization was already consumed or does not match this effect")]
    RetryAuthorizationRejected,
    #[error("runtime process execution identity conflicts with durable attempt state")]
    ExecutionIdentityConflict,
    #[error("runtime process effect has no typed durable request intent")]
    ProcessIntentRequired,
    #[error("runtime process request does not match its durable intent")]
    ProcessRequestMismatch,
    #[error("runtime process start gate identity does not match the supplied execution identity")]
    ProcessIdentityMismatch,
    #[error("runtime process release authorization already exists or conflicts with durable state")]
    ProcessReleaseAuthorizationRejected,
    #[error("runtime store could not complete a clean checkpoint and handoff")]
    CloseIncomplete,
    #[error("runtime store entry has an unsafe filesystem shape")]
    UnsafeEntry,
    #[error("runtime projection recovery snapshot exceeds its configured bounds")]
    RecoveryBoundsExceeded,
    #[error("runtime store operation failed during {operation}")]
    Operation {
        operation: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error(transparent)]
    Capability(#[from] crate::CapabilityError),
}

impl RuntimeStoreError {
    fn operation(
        operation: &'static str,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self::Operation {
            operation,
            source: Box::new(source),
        }
    }
}

/// The storage-actor-owned connection to `<config>/runtime.db`.
///
/// The type is intentionally not `Clone` or `Sync`. One actor owns it, and the
/// retained [`ProductionBoundary`] keeps the cross-language writer lease alive.
pub struct StorageActorAuthority {
    _private: (),
}

impl StorageActorAuthority {
    /// Minting remains inside `orchestrator-app`; the hermetic compatibility
    /// projector (Cell 2D, `hermetic_projector.rs`) is that mission/storage
    /// actor and never hands the raw store to handlers or executors.
    pub(crate) const fn new() -> Self {
        Self { _private: () }
    }
}

impl fmt::Debug for StorageActorAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("StorageActorAuthority")
    }
}

/// Store-wide semantic boundary persisted in schema v4.
///
/// A database is either Go-compatibility projection authority or isolated
/// Rust-private process-ledger state for its entire lifetime. The value is
/// checked before schema configuration or migration so the wrong opener
/// cannot silently adopt an existing store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StoreBoundaryKind {
    Compatibility,
    PrivateProcessLedger,
}

impl StoreBoundaryKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Compatibility => "compatibility",
            Self::PrivateProcessLedger => "private_process_ledger",
        }
    }

    fn parse(value: &str) -> Result<Self, RuntimeStoreError> {
        match value {
            "compatibility" => Ok(Self::Compatibility),
            "private_process_ledger" => Ok(Self::PrivateProcessLedger),
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }

    const fn boundary_seal_file(self) -> &'static str {
        match self {
            Self::Compatibility => COMPATIBILITY_BOUNDARY_SEAL_FILE,
            Self::PrivateProcessLedger => PRIVATE_PROCESS_LEDGER_BOUNDARY_SEAL_FILE,
        }
    }
}

/// Capability proving that a production root is intentionally dedicated to
/// isolated process-ledger canary state.
///
/// This ledger is not a second canonical mission truth. It retains only the
/// private execution claim, observation, recovery, and terminal-decision
/// evidence needed by the staged Rust canary.
#[derive(Clone)]
pub(crate) struct PrivateProcessLedgerBoundary {
    boundary: Arc<ProductionBoundary>,
}

impl PrivateProcessLedgerBoundary {
    pub(crate) fn new(boundary: Arc<ProductionBoundary>) -> Self {
        Self { boundary }
    }

    pub(crate) fn production_boundary(&self) -> &Arc<ProductionBoundary> {
        &self.boundary
    }
}

impl fmt::Debug for PrivateProcessLedgerBoundary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateProcessLedgerBoundary")
            .field("kind", &"isolated-private-process-ledger")
            .finish()
    }
}

pub struct RuntimeStore {
    _actor_authority: StorageActorAuthority,
    boundary_kind: StoreBoundaryKind,
    boundary: Arc<ProductionBoundary>,
    root_identity: FileIdentity,
    boundary_seal_identity: FileIdentity,
    database_identity: FileIdentity,
    wal_identity: Option<FileIdentity>,
    shm_identity: Option<FileIdentity>,
    connection: Option<Connection>,
    cleanly_closed: bool,
    #[cfg(test)]
    process_resolution_fault: ProcessResolutionFault,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct SidecarIdentities {
    wal: Option<FileIdentity>,
    shm: Option<FileIdentity>,
}

impl SidecarIdentities {
    const fn any(self) -> bool {
        self.wal.is_some() || self.shm.is_some()
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ProcessResolutionFault {
    #[default]
    None,
    AfterCommit,
    BeforeRetry,
}

impl fmt::Debug for RuntimeStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeStore")
            .field("kind", &self.boundary_kind)
            .finish()
    }
}

/// Typed owner for a schema-v4 private process ledger.
///
/// Deliberately does not implement `Deref`: compatibility projection writes
/// and projection-recovery snapshots are unavailable from this capability.
pub(crate) struct PrivateProcessLedgerStore {
    inner: RuntimeStore,
}

impl fmt::Debug for PrivateProcessLedgerStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateProcessLedgerStore")
            .field("kind", &"isolated-private-process-ledger")
            .finish()
    }
}

impl Drop for RuntimeStore {
    fn drop(&mut self) {
        drop(self.connection.take());
        if self.cleanly_closed {
            lock_unpoisoned(store_leases()).remove(&self.root_identity);
        } else {
            // Dropping without a successful checkpoint must not silently hand
            // writer authority to another in-process actor. Retain one boundary
            // reference and the process-local lease until process exit.
            std::mem::forget(Arc::clone(&self.boundary));
        }
    }
}

/// Maximum byte length of any single bounded reasoning text field.
const MAX_REASONING_TEXT_BYTES: usize = 4096;

/// Rejects an oversized reasoning field or one carrying control text, while
/// tolerating ordinary UTF-8 record text. Reasoning fields are plan-derived
/// typed values, never raw provider output.
fn validate_reasoning_text(value: &str) -> Result<(), RuntimeStoreError> {
    if value.len() > MAX_REASONING_TEXT_BYTES
        || value
            .chars()
            .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(RuntimeStoreError::InvalidIntent(
            "a reasoning field is oversized or contains control text",
        ));
    }
    Ok(())
}

/// One phase-scoped success criterion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningCriterionRow {
    /// Phase the criterion belongs to.
    pub phase_id: PhaseId,
    /// Bounded, plan-derived criterion text.
    pub criterion: String,
}

/// One phase-scoped evidence reference (digest + bounded summary, never raw output).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningEvidenceRow {
    /// Phase the evidence belongs to.
    pub phase_id: PhaseId,
    /// Evidence kind tag.
    pub kind: String,
    /// Artifact digest (SHA-256 hex), mirroring the store's fingerprint idiom.
    pub digest: String,
    /// Bounded evidence summary.
    pub summary: String,
}

/// One phase persona/runtime/model assignment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningAssignmentRow {
    /// Phase the assignment applies to.
    pub phase_id: PhaseId,
    /// Resolved persona.
    pub persona: String,
    /// Resolved role.
    pub role: String,
    /// Resolved model tier.
    pub model_tier: String,
    /// Resolved runtime family.
    pub runtime: String,
    /// How the persona was selected.
    pub selection_method: String,
}

/// One inter-phase handoff summary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningHandoffRow {
    /// Producing phase.
    pub from_phase: PhaseId,
    /// Consuming phase.
    pub to_phase: PhaseId,
    /// Bounded handoff summary.
    pub summary: String,
}

/// One review node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningReviewRow {
    /// Phase under review.
    pub phase_id: PhaseId,
    /// Review verdict tag.
    pub verdict: String,
    /// Reviewer persona.
    pub reviewer_persona: String,
}

/// The mission-admission reasoning intent and its full plan-derived detail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningIntent {
    /// Mission the record belongs to.
    pub mission_id: MissionId,
    /// Caller-supplied RFC3339-Z commit timestamp.
    pub committed_at_utc: String,
    /// Bounded, plan-derived mission intent.
    pub intent: String,
    /// Assumptions, in declared order.
    pub assumptions: Vec<String>,
    /// Success criteria per phase.
    pub criteria: Vec<ReasoningCriterionRow>,
    /// Evidence references per phase.
    pub evidence: Vec<ReasoningEvidenceRow>,
    /// Persona/runtime assignments per phase.
    pub assignments: Vec<ReasoningAssignmentRow>,
    /// Inter-phase handoffs.
    pub handoffs: Vec<ReasoningHandoffRow>,
    /// Review nodes.
    pub reviews: Vec<ReasoningReviewRow>,
}

/// A DAG/plan revision reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningRevision {
    /// Mission the revision belongs to.
    pub mission_id: MissionId,
    /// Caller-supplied RFC3339-Z commit timestamp.
    pub committed_at_utc: String,
    /// One-based revision ordinal.
    pub revision: u32,
    /// Bounded revision reason.
    pub reason: String,
}

/// A per-phase attempt record, mapped from a terminal outcome with all raw
/// provider output deliberately dropped.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReasoningAttempt {
    /// Mission the attempt belongs to.
    pub mission_id: MissionId,
    /// Caller-supplied RFC3339-Z commit timestamp.
    pub committed_at_utc: String,
    /// Attempted phase.
    pub phase_id: PhaseId,
    /// One-based attempt ordinal.
    pub attempt: u32,
    /// SHA-256 hex fingerprint of the strategy tried.
    pub strategy_fingerprint: String,
    /// Terminal outcome tag (for example `completed` / `incomplete`).
    pub outcome: String,
    /// Failure kind tag when the attempt did not complete.
    pub failure_kind: Option<String>,
}

/// A typed, bounded reasoning write for the private-process-ledger store.
///
/// Every variant is composed solely of validated, typed fields. There is no
/// free-form `String` blob, `serde_json::Value` payload, or column that can
/// carry raw provider output or chain-of-thought, so such text is structurally
/// unrepresentable rather than filtered after the fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReasoningWrite {
    /// Mission-admission intent and plan detail.
    Intent(ReasoningIntent),
    /// A plan/DAG revision.
    Revision(ReasoningRevision),
    /// A per-phase attempt outcome.
    Attempt(ReasoningAttempt),
}

impl ReasoningWrite {
    fn mission_id(&self) -> &MissionId {
        match self {
            Self::Intent(record) => &record.mission_id,
            Self::Revision(record) => &record.mission_id,
            Self::Attempt(record) => &record.mission_id,
        }
    }

    fn committed_at_utc(&self) -> &str {
        match self {
            Self::Intent(record) => &record.committed_at_utc,
            Self::Revision(record) => &record.committed_at_utc,
            Self::Attempt(record) => &record.committed_at_utc,
        }
    }

    fn transition_kind(&self) -> &'static str {
        match self {
            Self::Intent(_) => REASONING_INTENT_TRANSITION_KIND,
            Self::Revision(_) => REASONING_REVISION_TRANSITION_KIND,
            Self::Attempt(_) => REASONING_ATTEMPT_TRANSITION_KIND,
        }
    }

    fn transition_id(&self) -> String {
        match self {
            Self::Intent(record) => {
                format!("{}:reasoning:intent", record.mission_id.as_str())
            }
            Self::Revision(record) => format!(
                "{}:reasoning:revision:{}",
                record.mission_id.as_str(),
                record.revision
            ),
            Self::Attempt(record) => format!(
                "{}:reasoning:attempt:{}:{}",
                record.mission_id.as_str(),
                record.phase_id.as_str(),
                record.attempt
            ),
        }
    }

    /// Canonical, bounded, typed journal payload. Carries only declared record
    /// fields; there is no path for raw provider output to enter it.
    fn payload(&self) -> Value {
        match self {
            // The projection tables hold the per-phase detail; the canonical
            // journal payload carries only the typed intent scalar so it stays
            // bounded and free of any provider text.
            Self::Intent(record) => serde_json::json!({
                "record": "intent",
                "intent": record.intent,
            }),
            Self::Revision(record) => serde_json::json!({
                "record": "revision",
                "revision": record.revision,
                "reason": record.reason,
            }),
            Self::Attempt(record) => serde_json::json!({
                "record": "attempt",
                "phase": record.phase_id.as_str(),
                "attempt": record.attempt,
                "strategy_fingerprint": record.strategy_fingerprint,
                "outcome": record.outcome,
                "failure_kind": record.failure_kind,
            }),
        }
    }

    fn validate(&self) -> Result<(), RuntimeStoreError> {
        match self {
            Self::Intent(record) => {
                validate_reasoning_text(&record.intent)?;
                for assumption in &record.assumptions {
                    validate_reasoning_text(assumption)?;
                }
                for criterion in &record.criteria {
                    validate_reasoning_text(&criterion.criterion)?;
                }
                for evidence in &record.evidence {
                    validate_reasoning_text(&evidence.kind)?;
                    validate_reasoning_text(&evidence.digest)?;
                    validate_reasoning_text(&evidence.summary)?;
                }
                for assignment in &record.assignments {
                    validate_reasoning_text(&assignment.persona)?;
                    validate_reasoning_text(&assignment.role)?;
                    validate_reasoning_text(&assignment.model_tier)?;
                    validate_reasoning_text(&assignment.runtime)?;
                    validate_reasoning_text(&assignment.selection_method)?;
                }
                for handoff in &record.handoffs {
                    validate_reasoning_text(&handoff.summary)?;
                }
                for review in &record.reviews {
                    validate_reasoning_text(&review.verdict)?;
                    validate_reasoning_text(&review.reviewer_persona)?;
                }
            }
            Self::Revision(record) => validate_reasoning_text(&record.reason)?,
            Self::Attempt(record) => {
                validate_reasoning_text(&record.strategy_fingerprint)?;
                validate_reasoning_text(&record.outcome)?;
                if let Some(failure_kind) = &record.failure_kind {
                    validate_reasoning_text(failure_kind)?;
                }
            }
        }
        Ok(())
    }

    fn insert_rows(
        &self,
        transaction: &Transaction<'_>,
        sequence: i64,
    ) -> Result<(), RuntimeStoreError> {
        let mission = self.mission_id().as_str();
        let at = self.committed_at_utc();
        let op =
            |source: rusqlite::Error| RuntimeStoreError::operation("insert reasoning row", source);
        match self {
            Self::Intent(record) => {
                transaction
                    .execute(
                        "INSERT INTO reasoning_record
                            (mission_id, intent, journal_sequence, created_at_utc)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![mission, record.intent, sequence, at],
                    )
                    .map_err(op)?;
                for (ordinal, assumption) in record.assumptions.iter().enumerate() {
                    transaction
                        .execute(
                            "INSERT INTO reasoning_assumption
                                (mission_id, ordinal, statement, journal_sequence)
                             VALUES (?1, ?2, ?3, ?4)",
                            params![mission, ordinal as i64, assumption, sequence],
                        )
                        .map_err(op)?;
                }
                for (ordinal, criterion) in record.criteria.iter().enumerate() {
                    transaction
                        .execute(
                            "INSERT INTO reasoning_criterion
                                (mission_id, phase_id, ordinal, criterion, journal_sequence)
                             VALUES (?1, ?2, ?3, ?4, ?5)",
                            params![
                                mission,
                                criterion.phase_id.as_str(),
                                ordinal as i64,
                                criterion.criterion,
                                sequence
                            ],
                        )
                        .map_err(op)?;
                }
                for (ordinal, evidence) in record.evidence.iter().enumerate() {
                    transaction
                        .execute(
                            "INSERT INTO reasoning_evidence
                                (mission_id, phase_id, ordinal, kind, digest, summary, journal_sequence)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                mission,
                                evidence.phase_id.as_str(),
                                ordinal as i64,
                                evidence.kind,
                                evidence.digest,
                                evidence.summary,
                                sequence
                            ],
                        )
                        .map_err(op)?;
                }
                for assignment in &record.assignments {
                    transaction
                        .execute(
                            "INSERT INTO reasoning_assignment
                                (mission_id, phase_id, persona, role, model_tier, runtime, selection_method, journal_sequence)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            params![
                                mission,
                                assignment.phase_id.as_str(),
                                assignment.persona,
                                assignment.role,
                                assignment.model_tier,
                                assignment.runtime,
                                assignment.selection_method,
                                sequence
                            ],
                        )
                        .map_err(op)?;
                }
                for (ordinal, handoff) in record.handoffs.iter().enumerate() {
                    transaction
                        .execute(
                            "INSERT INTO reasoning_handoff
                                (mission_id, from_phase, to_phase, ordinal, summary, journal_sequence)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![
                                mission,
                                handoff.from_phase.as_str(),
                                handoff.to_phase.as_str(),
                                ordinal as i64,
                                handoff.summary,
                                sequence
                            ],
                        )
                        .map_err(op)?;
                }
                for (ordinal, review) in record.reviews.iter().enumerate() {
                    transaction
                        .execute(
                            "INSERT INTO reasoning_review
                                (mission_id, phase_id, ordinal, verdict, reviewer_persona, journal_sequence)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                            params![
                                mission,
                                review.phase_id.as_str(),
                                ordinal as i64,
                                review.verdict,
                                review.reviewer_persona,
                                sequence
                            ],
                        )
                        .map_err(op)?;
                }
            }
            Self::Revision(record) => {
                transaction
                    .execute(
                        "INSERT INTO reasoning_revision
                            (mission_id, revision, reason, journal_sequence)
                         VALUES (?1, ?2, ?3, ?4)",
                        params![mission, i64::from(record.revision), record.reason, sequence],
                    )
                    .map_err(op)?;
            }
            Self::Attempt(record) => {
                transaction
                    .execute(
                        "INSERT INTO reasoning_attempt
                            (mission_id, phase_id, attempt, strategy_fingerprint, outcome, failure_kind, journal_sequence)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        params![
                            mission,
                            record.phase_id.as_str(),
                            i64::from(record.attempt),
                            record.strategy_fingerprint,
                            record.outcome,
                            record.failure_kind,
                            sequence
                        ],
                    )
                    .map_err(op)?;
            }
        }
        Ok(())
    }
}

/// Row counts of each reasoning projection table for one mission.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReasoningCounts {
    /// Rows in `reasoning_record`.
    pub records: u64,
    /// Rows in `reasoning_assumption`.
    pub assumptions: u64,
    /// Rows in `reasoning_criterion`.
    pub criteria: u64,
    /// Rows in `reasoning_evidence`.
    pub evidence: u64,
    /// Rows in `reasoning_assignment`.
    pub assignments: u64,
    /// Rows in `reasoning_handoff`.
    pub handoffs: u64,
    /// Rows in `reasoning_review`.
    pub reviews: u64,
    /// Rows in `reasoning_attempt`.
    pub attempts: u64,
    /// Rows in `reasoning_revision`.
    pub revisions: u64,
}

impl ReasoningCounts {
    /// True when no reasoning row exists for the mission.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.records == 0
            && self.assumptions == 0
            && self.criteria == 0
            && self.evidence == 0
            && self.assignments == 0
            && self.handoffs == 0
            && self.reviews == 0
            && self.attempts == 0
            && self.revisions == 0
    }
}

impl RuntimeStore {
    /// Opens or initializes a Go-compatibility runtime database under an
    /// enrolled, writer-owned production home.
    pub fn open(
        boundary: Arc<ProductionBoundary>,
        actor_authority: StorageActorAuthority,
    ) -> Result<Self, RuntimeStoreError> {
        Self::open_for_kind(boundary, actor_authority, StoreBoundaryKind::Compatibility)
    }

    /// Opens or initializes an isolated Rust-private process ledger.
    ///
    /// Legacy schema versions are never adopted through this entry point.
    /// Existing schema-v4 compatibility stores fail before connection
    /// configuration or migration.
    pub(crate) fn open_private(
        boundary: PrivateProcessLedgerBoundary,
        actor_authority: StorageActorAuthority,
    ) -> Result<PrivateProcessLedgerStore, RuntimeStoreError> {
        Self::open_for_kind(
            Arc::clone(boundary.production_boundary()),
            actor_authority,
            StoreBoundaryKind::PrivateProcessLedger,
        )
        .map(|inner| PrivateProcessLedgerStore { inner })
    }

    fn open_for_kind(
        boundary: Arc<ProductionBoundary>,
        actor_authority: StorageActorAuthority,
        boundary_kind: StoreBoundaryKind,
    ) -> Result<Self, RuntimeStoreError> {
        boundary.verify()?;
        let root_metadata = boundary
            .directory()
            .dir_metadata()
            .map_err(|source| RuntimeStoreError::operation("inspect runtime root", source))?;
        let root_identity = identity(&root_metadata);
        {
            let mut leases = lock_unpoisoned(store_leases());
            if !leases.insert(root_identity) {
                return Err(RuntimeStoreError::WriterLeased);
            }
        }

        let result = Self::open_retained(boundary, root_identity, actor_authority, boundary_kind);
        if result.is_err() {
            lock_unpoisoned(store_leases()).remove(&root_identity);
        }
        result
    }

    fn open_retained(
        boundary: Arc<ProductionBoundary>,
        root_identity: FileIdentity,
        actor_authority: StorageActorAuthority,
        boundary_kind: StoreBoundaryKind,
    ) -> Result<Self, RuntimeStoreError> {
        let initial_database = inspect_optional_database_file(&boundary)?;
        let database_is_nonempty = initial_database
            .as_ref()
            .is_some_and(|(_, length)| *length > 0);
        let mut initial_seal_identity = inspect_boundary_seal(&boundary, boundary_kind)?;
        let mut sidecar_identities = inspect_optional_sidecars(&boundary)?;
        let needs_unsealed_compatibility_preflight = if initial_seal_identity.is_some() {
            false
        } else if database_is_nonempty {
            if boundary_kind == StoreBoundaryKind::PrivateProcessLedger {
                return Err(RuntimeStoreError::BoundaryKindMismatch);
            }
            true
        } else {
            if sidecar_identities.any() {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            initial_seal_identity = Some(create_boundary_seal(&boundary, boundary_kind)?);
            false
        };
        boundary.verify()?;
        ensure_database_file(&boundary)?;
        let database_identity = verify_private_file(boundary.directory(), DATABASE_FILE)?;
        if initial_database
            .as_ref()
            .is_some_and(|(initial_identity, _)| *initial_identity != database_identity)
        {
            return Err(RuntimeStoreError::UnsafeEntry);
        }
        if let Some(seal_identity) = initial_seal_identity {
            verify_boundary_seal(&boundary, seal_identity, boundary_kind)?;
        }
        let database_path = boundary.canonical_path().join(DATABASE_FILE);
        if needs_unsealed_compatibility_preflight {
            let preflight_flags = OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW;
            let preflight = Connection::open_with_flags(&database_path, preflight_flags)
                .map_err(|source| RuntimeStoreError::operation("open runtime database", source))?;
            let preflight_result =
                preflight_requested_boundary(&preflight, StoreBoundaryKind::Compatibility);
            drop(preflight);
            sidecar_identities = recapture_optional_sidecars(&boundary, sidecar_identities)?;
            preflight_result?;
            boundary.verify()?;
            if verify_private_file(boundary.directory(), DATABASE_FILE)? != database_identity {
                return Err(RuntimeStoreError::UnsafeEntry);
            }
        }
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(database_path, flags)
            .map_err(|source| RuntimeStoreError::operation("open runtime database", source))?;
        sidecar_identities = recapture_optional_sidecars(&boundary, sidecar_identities)?;
        boundary.verify()?;
        if verify_private_file(boundary.directory(), DATABASE_FILE)? != database_identity {
            return Err(RuntimeStoreError::UnsafeEntry);
        }
        let configure_result = configure_connection(&connection);
        sidecar_identities = recapture_optional_sidecars(&boundary, sidecar_identities)?;
        configure_result?;
        let validation_result = initialize_or_validate_schema(&mut connection, boundary_kind)
            .and_then(|()| validate_database(&connection, boundary_kind));
        sidecar_identities = recapture_optional_sidecars(&boundary, sidecar_identities)?;
        validation_result?;
        let boundary_seal_identity = match initial_seal_identity {
            Some(seal_identity) => {
                verify_boundary_seal(&boundary, seal_identity, boundary_kind)?;
                seal_identity
            }
            None => create_boundary_seal(&boundary, StoreBoundaryKind::Compatibility)?,
        };
        boundary.verify()?;
        if verify_private_file(boundary.directory(), DATABASE_FILE)? != database_identity {
            return Err(RuntimeStoreError::UnsafeEntry);
        }
        verify_boundary_seal(&boundary, boundary_seal_identity, boundary_kind)?;
        sidecar_identities = recapture_optional_sidecars(&boundary, sidecar_identities)?;
        Ok(Self {
            _actor_authority: actor_authority,
            boundary_kind,
            boundary,
            root_identity,
            boundary_seal_identity,
            database_identity,
            wal_identity: sidecar_identities.wal,
            shm_identity: sidecar_identities.shm,
            connection: Some(connection),
            cleanly_closed: false,
            #[cfg(test)]
            process_resolution_fault: ProcessResolutionFault::None,
        })
    }

    /// Injects a lost acknowledgement after one exact process resolution has
    /// committed, followed by one failed retry. The caller must therefore use
    /// the independent exact readback to prove the durable outcome.
    #[cfg(test)]
    pub(crate) fn inject_process_resolution_acknowledgement_loss(&mut self) {
        assert_eq!(
            self.process_resolution_fault,
            ProcessResolutionFault::None,
            "a process resolution fault is already armed"
        );
        self.process_resolution_fault = ProcessResolutionFault::AfterCommit;
    }

    /// Checkpoints WAL, closes SQLite fallibly, fsyncs the database and its
    /// parent directory, and only then releases writer authority.
    pub fn close(mut self) -> Result<(), RuntimeStoreError> {
        self.close_in_place()
    }

    /// [`close`](Self::close), performed through a mutable borrow.
    ///
    /// An owner whose [`Drop`] must release the writer lease cannot move the
    /// store out of `&mut self`, so without this it would have to hold the
    /// store in an `Option` and unwrap it on every read. Closing twice is
    /// harmless and reported: the second call finds no connection and returns
    /// [`RuntimeStoreError::CloseIncomplete`] without touching the lease.
    pub(crate) fn close_in_place(&mut self) -> Result<(), RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self
            .connection
            .take()
            .ok_or(RuntimeStoreError::CloseIncomplete)?;
        let checkpoint: Result<(i64, i64, i64), rusqlite::Error> =
            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            });
        let (busy, log_frames, checkpointed_frames) = checkpoint.map_err(|source| {
            RuntimeStoreError::operation("checkpoint runtime database", source)
        })?;
        if busy != 0 || checkpointed_frames < log_frames {
            drop(connection);
            return Err(RuntimeStoreError::CloseIncomplete);
        }
        if let Err((_connection, source)) = connection.close() {
            return Err(RuntimeStoreError::operation(
                "close runtime database",
                source,
            ));
        }
        self.boundary.verify()?;
        if verify_private_file(self.boundary.directory(), DATABASE_FILE)? != self.database_identity
        {
            return Err(RuntimeStoreError::UnsafeEntry);
        }
        verify_boundary_seal(
            &self.boundary,
            self.boundary_seal_identity,
            self.boundary_kind,
        )?;
        let database = open_file_nofollow(self.boundary.directory(), Path::new(DATABASE_FILE))
            .map_err(|source| RuntimeStoreError::operation("sync runtime database", source))?;
        database
            .sync_all()
            .map_err(|source| RuntimeStoreError::operation("sync runtime database", source))?;
        sync_dir(self.boundary.directory()).map_err(|source| {
            RuntimeStoreError::operation("sync runtime database directory", source)
        })?;
        self.cleanly_closed = true;
        lock_unpoisoned(store_leases()).remove(&self.root_identity);
        Ok(())
    }

    /// Atomically appends one journal record and all of its initially-pending
    /// outbox effects. Exact retries return the original receipt.
    pub fn append(&mut self, intent: &JournalIntent) -> Result<JournalCommit, RuntimeStoreError> {
        self.append_for_boundary(intent, StoreBoundaryKind::Compatibility)
    }

    /// Atomically appends one Rust-private reasoning transition and its typed
    /// projection rows. Enforced structurally: the only input is a typed
    /// [`ReasoningWrite`] with no free-form or raw-text field, so provider
    /// chain-of-thought is unrepresentable. Only valid on a private-process-
    /// ledger store; a compatibility store rejects the boundary.
    pub(crate) fn record_reasoning(
        &mut self,
        write: &ReasoningWrite,
    ) -> Result<JournalCommit, RuntimeStoreError> {
        if self.boundary_kind != StoreBoundaryKind::PrivateProcessLedger {
            return Err(RuntimeStoreError::BoundaryKindMismatch);
        }
        write.validate()?;
        let intent = JournalIntent::new(
            write.transition_id(),
            Some(write.mission_id().clone()),
            write.transition_kind(),
            write.payload(),
            write.committed_at_utc(),
        )?;
        self.verify_retained()?;
        let prepared = PreparedIntent::new(&intent, StoreBoundaryKind::PrivateProcessLedger)?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin reasoning transaction", source)
            })?;
        verify_schema_version(&transaction)?;
        if load_existing_transition(&transaction, &intent.transition_id)?.is_some() {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close reasoning retry transaction", source)
            })?;
            return Err(RuntimeStoreError::TransitionConflict);
        }
        let (sequence, previous_checksum) = next_journal_position(&transaction)?;
        let checksum = transition_checksum(
            RECORD_SCHEMA_VERSION,
            sequence,
            &previous_checksum,
            &prepared,
        );
        transaction
            .execute(
                "INSERT INTO journal (
                    sequence, transition_id, mission_id, record_schema_version,
                    transition_kind, payload_json, committed_at_utc, extra_json,
                    outbox_fingerprint, projection_fingerprint, previous_checksum, checksum
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    sequence,
                    intent.transition_id,
                    intent.mission_id.as_ref().map(MissionId::as_str),
                    RECORD_SCHEMA_VERSION,
                    intent.kind,
                    prepared.payload_json,
                    intent.committed_at_utc,
                    prepared.extra_json,
                    prepared.outbox_fingerprint,
                    prepared.projection_fingerprint,
                    previous_checksum,
                    checksum,
                ],
            )
            .map_err(map_journal_insert_error)?;
        transaction
            .execute(
                "INSERT INTO command_ack (journal_sequence, acknowledged_at_utc)
                 VALUES (?1, ?2)",
                params![sequence, intent.committed_at_utc],
            )
            .map_err(|source| {
                RuntimeStoreError::operation("acknowledge reasoning transition", source)
            })?;
        write.insert_rows(&transaction, sequence)?;
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit reasoning transaction", source)
        })?;
        self.verify_retained()?;
        Ok(JournalCommit {
            sequence,
            checksum,
            duplicate: false,
            event_projection_recipe: None,
        })
    }

    /// Counts reasoning projection rows for one mission.
    pub(crate) fn reasoning_counts(
        &self,
        mission_id: &MissionId,
    ) -> Result<ReasoningCounts, RuntimeStoreError> {
        let connection = self.connection()?;
        let count = |table: &'static str| -> Result<u64, RuntimeStoreError> {
            let value: i64 = connection
                .query_row(
                    &format!("SELECT count(*) FROM {table} WHERE mission_id = ?1"),
                    params![mission_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(|source| RuntimeStoreError::operation("count reasoning rows", source))?;
            u64::try_from(value).map_err(|_| RuntimeStoreError::CorruptDatabase)
        };
        Ok(ReasoningCounts {
            records: count("reasoning_record")?,
            assumptions: count("reasoning_assumption")?,
            criteria: count("reasoning_criterion")?,
            evidence: count("reasoning_evidence")?,
            assignments: count("reasoning_assignment")?,
            handoffs: count("reasoning_handoff")?,
            reviews: count("reasoning_review")?,
            attempts: count("reasoning_attempt")?,
            revisions: count("reasoning_revision")?,
        })
    }

    /// Every `(table, column)` pair across all reasoning projection tables, for
    /// asserting no chain-of-thought / raw-output column exists.
    pub(crate) fn reasoning_column_inventory(
        &self,
    ) -> Result<Vec<(String, String)>, RuntimeStoreError> {
        let connection = self.connection()?;
        let mut tables_statement = connection
            .prepare(
                "SELECT name FROM sqlite_schema
                 WHERE type = 'table' AND name LIKE 'reasoning\\_%' ESCAPE '\\'
                 ORDER BY name",
            )
            .map_err(|source| RuntimeStoreError::operation("list reasoning tables", source))?;
        let tables: Vec<String> = tables_statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|source| RuntimeStoreError::operation("read reasoning tables", source))?
            .collect::<Result<_, _>>()
            .map_err(|source| RuntimeStoreError::operation("decode reasoning tables", source))?;
        let mut inventory = Vec::new();
        for table in tables {
            let mut columns_statement = connection
                .prepare(&format!("PRAGMA table_info({table})"))
                .map_err(|source| {
                    RuntimeStoreError::operation("inspect reasoning table", source)
                })?;
            let columns = columns_statement
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|source| RuntimeStoreError::operation("read reasoning columns", source))?;
            for column in columns {
                let column = column.map_err(|source| {
                    RuntimeStoreError::operation("decode reasoning column", source)
                })?;
                inventory.push((table.clone(), column));
            }
        }
        Ok(inventory)
    }

    /// Every stored text cell for a mission across the reasoning projection
    /// tables and their journal payloads, for asserting that no raw provider
    /// output leaked into persisted reasoning state.
    pub(crate) fn reasoning_text_cells(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        let connection = self.connection()?;
        let mut cells = Vec::new();
        let mut push_query = |sql: &str| -> Result<(), RuntimeStoreError> {
            let mut statement = connection.prepare(sql).map_err(|source| {
                RuntimeStoreError::operation("prepare reasoning text scan", source)
            })?;
            let rows = statement
                .query_map(params![mission_id.as_str()], |row| row.get::<_, String>(0))
                .map_err(|source| RuntimeStoreError::operation("scan reasoning text", source))?;
            for row in rows {
                cells.push(row.map_err(|source| {
                    RuntimeStoreError::operation("decode reasoning text", source)
                })?);
            }
            Ok(())
        };
        push_query("SELECT intent FROM reasoning_record WHERE mission_id = ?1")?;
        push_query("SELECT statement FROM reasoning_assumption WHERE mission_id = ?1")?;
        push_query("SELECT criterion FROM reasoning_criterion WHERE mission_id = ?1")?;
        push_query(
            "SELECT kind || ' ' || digest || ' ' || summary FROM reasoning_evidence WHERE mission_id = ?1",
        )?;
        push_query(
            "SELECT persona || ' ' || role || ' ' || model_tier || ' ' || runtime || ' ' || selection_method FROM reasoning_assignment WHERE mission_id = ?1",
        )?;
        push_query("SELECT summary FROM reasoning_handoff WHERE mission_id = ?1")?;
        push_query(
            "SELECT verdict || ' ' || reviewer_persona FROM reasoning_review WHERE mission_id = ?1",
        )?;
        push_query(
            "SELECT strategy_fingerprint || ' ' || outcome || ' ' || coalesce(failure_kind, '') FROM reasoning_attempt WHERE mission_id = ?1",
        )?;
        push_query("SELECT reason FROM reasoning_revision WHERE mission_id = ?1")?;
        push_query(
            "SELECT payload_json FROM journal
             WHERE mission_id = ?1 AND transition_kind LIKE 'reasoning.%'",
        )?;
        Ok(cells)
    }

    fn append_private(
        &mut self,
        intent: &JournalIntent,
    ) -> Result<JournalCommit, RuntimeStoreError> {
        self.append_for_boundary(intent, StoreBoundaryKind::PrivateProcessLedger)
    }

    fn append_for_boundary(
        &mut self,
        intent: &JournalIntent,
        boundary_kind: StoreBoundaryKind,
    ) -> Result<JournalCommit, RuntimeStoreError> {
        if self.boundary_kind != boundary_kind {
            return Err(RuntimeStoreError::BoundaryKindMismatch);
        }
        self.verify_retained()?;
        let mut prepared = PreparedIntent::new(intent, boundary_kind)?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| RuntimeStoreError::operation("begin journal transaction", source))?;
        verify_schema_version(&transaction)?;

        if let Some(existing) = load_existing_transition(&transaction, &intent.transition_id)? {
            let matches = existing.matches(&prepared);
            let commit = if matches {
                let event_projection_recipe = if intent
                    .required_projections
                    .contains(&CompatibilityProjection::EventLog)
                {
                    Some(
                        parse_sealed_recipe(&existing.extra_json)
                            .ok_or(RuntimeStoreError::CorruptDatabase)?,
                    )
                } else {
                    None
                };
                Some(JournalCommit {
                    sequence: existing.sequence,
                    checksum: existing.checksum,
                    duplicate: true,
                    event_projection_recipe,
                })
            } else {
                None
            };
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close retry transaction", source)
            })?;
            return commit.ok_or(RuntimeStoreError::TransitionConflict);
        }

        let (sequence, previous_checksum) = next_journal_position(&transaction)?;
        // Bind exact event-projection identity before this row is ever
        // inserted, sealing it into the same `extra_json` bytes that are
        // already part of the transition checksum chain below.
        let event_projection_recipe = if prepared
            .required_projections
            .contains(&CompatibilityProjection::EventLog)
        {
            let mission_id = prepared
                .mission_id
                .as_deref()
                .ok_or(RuntimeStoreError::CorruptDatabase)?;
            let recipe = allocate_event_projection_recipe(&transaction, mission_id)?;
            prepared.extra_json = seal_event_projection_recipe(&prepared.extra_json, &recipe)?;
            Some(recipe)
        } else {
            None
        };
        let checksum = transition_checksum(
            RECORD_SCHEMA_VERSION,
            sequence,
            &previous_checksum,
            &prepared,
        );
        transaction
            .execute(
                "INSERT INTO journal (
                    sequence, transition_id, mission_id, record_schema_version,
                    transition_kind, payload_json, committed_at_utc, extra_json,
                    outbox_fingerprint, projection_fingerprint, previous_checksum, checksum
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    sequence,
                    intent.transition_id,
                    intent.mission_id.as_ref().map(MissionId::as_str),
                    RECORD_SCHEMA_VERSION,
                    intent.kind,
                    prepared.payload_json,
                    intent.committed_at_utc,
                    prepared.extra_json,
                    prepared.outbox_fingerprint,
                    prepared.projection_fingerprint,
                    previous_checksum,
                    checksum,
                ],
            )
            .map_err(map_journal_insert_error)?;
        for effect in &prepared.outbox {
            transaction
                .execute(
                    "INSERT INTO outbox (
                        idempotency_key, journal_sequence, mission_id, phase_id,
                        effect_kind, operation_slot, logical_attempt, payload_json,
                        state, attempts, updated_at_utc
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'pending', 0, ?9)",
                    params![
                        effect.idempotency_key,
                        sequence,
                        effect.mission_id,
                        effect.phase_id,
                        effect.effect_kind,
                        effect.operation_slot,
                        effect.logical_attempt,
                        effect.payload_json,
                        intent.committed_at_utc
                    ],
                )
                .map_err(map_outbox_insert_error)?;
            if let Some(process_intent_json) = effect.process_intent_json.as_deref() {
                transaction
                    .execute(
                        "INSERT INTO outbox_process_intent (idempotency_key, intent_json)
                         VALUES (?1, ?2)",
                        params![effect.idempotency_key, process_intent_json],
                    )
                    .map_err(|source| {
                        RuntimeStoreError::operation("insert process outbox intent", source)
                    })?;
            }
        }
        for publication in &prepared.publications {
            transaction
                .execute(
                    "INSERT INTO knowledge_publication (
                        idempotency_key, journal_sequence, mission_id, phase_id,
                        logical_attempt, namespace, type_name, primitive_kind,
                        schema_version, registry_generation, sensitivity, identity,
                        payload_json, payload_digest, state, attempts, enqueued_at_utc
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                               'pending', 0, ?15)",
                    params![
                        publication.idempotency_key,
                        sequence,
                        publication.mission_id,
                        publication.phase_id,
                        publication.logical_attempt,
                        publication.namespace,
                        publication.type_name,
                        publication.primitive_kind,
                        publication.schema_version,
                        publication.registry_generation,
                        publication.sensitivity,
                        publication.identity,
                        publication.payload_json,
                        publication.payload_digest,
                        intent.committed_at_utc,
                    ],
                )
                .map_err(map_publication_insert_error)?;
        }
        for projection in &prepared.required_projections {
            transaction
                .execute(
                    "INSERT INTO projection_requirement (journal_sequence, projection)
                     VALUES (?1, ?2)",
                    params![sequence, projection.as_str()],
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("insert projection requirement", source)
                })?;
        }
        if prepared.required_projections.is_empty() {
            transaction
                .execute(
                    "INSERT INTO command_ack (journal_sequence, acknowledged_at_utc)
                     VALUES (?1, ?2)",
                    params![sequence, intent.committed_at_utc],
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("acknowledge private transition", source)
                })?;
        }
        if boundary_kind == StoreBoundaryKind::Compatibility {
            advance_existing_projection_cursors(&transaction, &intent.committed_at_utc)?;
        }
        transaction
            .commit()
            .map_err(|source| RuntimeStoreError::operation("commit journal transaction", source))?;
        self.verify_retained()?;
        Ok(JournalCommit {
            sequence,
            checksum,
            duplicate: false,
            event_projection_recipe,
        })
    }

    /// Durably persists Cell 1's first-selected terminal decision exactly
    /// once per mission/phase/worker/attempt binding (Cell 2F).
    ///
    /// This rides the ordinary journal append path: an exact retry of the
    /// identical record is idempotent (the journal's own exact-retry check),
    /// while a retry under the same binding with *different* content is
    /// rejected with [`RuntimeStoreError::TransitionConflict`] — a pre-crash
    /// BLOCK/CANCEL decision can never be silently promoted to PASS by a
    /// later caller. The transition requires no compatibility projection
    /// (it is private Rust-only state, never Go-visible) and therefore
    /// self-acknowledges on append.
    fn record_terminal_decision_private(
        &mut self,
        record: &TerminalDecisionRecord,
    ) -> Result<(), RuntimeStoreError> {
        let transition_id = terminal_decision_transition_id(
            &record.mission_id,
            &record.phase_id,
            &record.worker_id,
            record.attempt,
        );
        // This journal kind carries no Go-visible projection and is never
        // ordered against real wall-clock events, so a stable synthetic
        // RFC3339 timestamp (mirroring the fixture engine's own
        // `2000-01-01T00:00:00.<n>Z` clock convention) keeps exact retries
        // byte-identical without depending on a live clock.
        //
        // The 19-digit fractional-second width is deliberately wider than
        // standard RFC3339 allows; it passes only because this codebase's
        // `is_rfc3339` is permissive about fractional width. This value is
        // never Go-visible and never parsed by anything outside this
        // module, so the non-standard width is harmless.
        let committed_at_utc = format!("2000-01-01T00:00:00.{:019}Z", record.attempt);
        let intent = JournalIntent::new(
            transition_id,
            Some(record.mission_id.clone()),
            TERMINAL_DECISION_TRANSITION_KIND,
            record.to_value(),
            committed_at_utc,
        )?;
        self.append_private(&intent)?;
        Ok(())
    }

    /// Looks up the durable terminal decision for one exact
    /// mission/phase/worker/attempt binding, when one was ever persisted
    /// through the typed private-ledger terminal-decision API (Cell 2F).
    ///
    /// `Ok(None)` means no decision was ever durably selected for this exact
    /// binding — including every pre-Cell-2F history, which the caller must
    /// keep treating as fail-closed. Any row found under this binding's
    /// transition ID that does not match the expected kind, mission column,
    /// or the binding embedded in its own payload is reported as
    /// [`RuntimeStoreError::CorruptDatabase`] rather than trusted.
    fn terminal_decision_private(
        &self,
        mission_id: &MissionId,
        phase_id: &str,
        worker_id: &str,
        attempt: u32,
    ) -> Result<Option<TerminalDecisionRecord>, RuntimeStoreError> {
        validate_terminal_decision_worker_id(worker_id)?;
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let transition_id =
            terminal_decision_transition_id(mission_id, phase_id, worker_id, attempt);
        let row: Option<(Option<String>, String, String)> = connection
            .query_row(
                "SELECT mission_id, transition_kind, payload_json FROM journal
                 WHERE transition_id = ?1",
                params![transition_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|source| RuntimeStoreError::operation("look up terminal decision", source))?;
        let Some((row_mission_id, kind, payload_json)) = row else {
            return Ok(None);
        };
        if kind != TERMINAL_DECISION_TRANSITION_KIND
            || row_mission_id.as_deref() != Some(mission_id.as_str())
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let payload = validate_stored_json(&payload_json, "terminal decision payload")?;
        let record = TerminalDecisionRecord::from_value(&payload)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        if record.mission_id.as_str() != mission_id.as_str()
            || record.phase_id != phase_id
            || record.worker_id != worker_id
            || record.attempt != attempt
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(Some(record))
    }

    /// Durably records a same-provider continuation handle for one exact
    /// mission/phase/attempt binding.
    ///
    /// Rides the ordinary private-append path: an exact retry is idempotent and
    /// a divergent rewrite under the same binding conflicts. The synthetic,
    /// attempt-derived timestamp keeps exact retries byte-identical without a
    /// live clock, mirroring the terminal-decision family.
    fn record_continuation_handle_private(
        &mut self,
        record: &ContinuationHandleRecord,
    ) -> Result<(), RuntimeStoreError> {
        let transition_id = continuation_transition_id(
            "handle",
            &record.mission_id,
            &record.phase_id,
            record.attempt,
        );
        let committed_at_utc = format!("2000-01-01T00:00:00.{:019}Z", record.attempt);
        let intent = JournalIntent::new(
            transition_id,
            Some(record.mission_id.clone()),
            CONTINUATION_HANDLE_TRANSITION_KIND,
            record.to_value(),
            committed_at_utc,
        )?;
        self.append_private(&intent)?;
        Ok(())
    }

    /// Looks up the durable continuation handle for one exact
    /// mission/phase/attempt binding, if one was ever recorded.
    fn continuation_handle_private(
        &self,
        mission_id: &MissionId,
        phase_id: &str,
        attempt: u32,
    ) -> Result<Option<ContinuationHandleRecord>, RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let transition_id = continuation_transition_id("handle", mission_id, phase_id, attempt);
        let row: Option<(Option<String>, String, String)> = connection
            .query_row(
                "SELECT mission_id, transition_kind, payload_json FROM journal
                 WHERE transition_id = ?1",
                params![transition_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|source| {
                RuntimeStoreError::operation("look up continuation handle", source)
            })?;
        let Some((row_mission_id, kind, payload_json)) = row else {
            return Ok(None);
        };
        if kind != CONTINUATION_HANDLE_TRANSITION_KIND
            || row_mission_id.as_deref() != Some(mission_id.as_str())
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let payload = validate_stored_json(&payload_json, "continuation handle payload")?;
        let record = ContinuationHandleRecord::from_value(&payload)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        if record.mission_id.as_str() != mission_id.as_str()
            || record.phase_id != phase_id
            || record.attempt != attempt
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(Some(record))
    }

    /// Durably records the selected continuation decision for one exact
    /// mission/phase/attempt binding. Exact retries are idempotent; a divergent
    /// rewrite conflicts, so an accepted decision can never be silently changed
    /// on resume.
    fn record_continuation_decision_private(
        &mut self,
        record: &ContinuationDecisionRecord,
    ) -> Result<(), RuntimeStoreError> {
        let transition_id = continuation_transition_id(
            "decision",
            &record.mission_id,
            &record.phase_id,
            record.attempt,
        );
        let committed_at_utc = format!("2000-01-01T00:00:00.{:019}Z", record.attempt);
        let intent = JournalIntent::new(
            transition_id,
            Some(record.mission_id.clone()),
            CONTINUATION_DECISION_TRANSITION_KIND,
            record.to_value(),
            committed_at_utc,
        )?;
        self.append_private(&intent)?;
        Ok(())
    }

    /// Every continuation decision recorded for a mission, in ascending attempt
    /// order. Used for deterministic offline replay of the resume-vs-fresh path.
    fn continuation_decisions_private(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<ContinuationDecisionRecord>, RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let mut statement = connection
            .prepare(
                "SELECT payload_json FROM journal
                 WHERE mission_id = ?1 AND transition_kind = ?2
                 ORDER BY sequence",
            )
            .map_err(|source| {
                RuntimeStoreError::operation("prepare continuation decisions", source)
            })?;
        let rows = statement
            .query_map(
                params![mission_id.as_str(), CONTINUATION_DECISION_TRANSITION_KIND],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("read continuation decisions", source)
            })?;
        let mut decisions = Vec::new();
        for row in rows {
            let payload_json = row.map_err(|source| {
                RuntimeStoreError::operation("decode continuation decision", source)
            })?;
            let payload = validate_stored_json(&payload_json, "continuation decision payload")?;
            let record = ContinuationDecisionRecord::from_value(&payload)
                .ok_or(RuntimeStoreError::CorruptDatabase)?;
            if record.mission_id.as_str() != mission_id.as_str() {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            decisions.push(record);
        }
        Ok(decisions)
    }

    /// Every persisted continuation decision payload for a mission, as raw text,
    /// for asserting that no raw provider output or provider `session_id` leaked
    /// into a provider-neutral decision row.
    fn continuation_capsule_text_cells(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT payload_json FROM journal
                 WHERE mission_id = ?1 AND transition_kind = ?2
                 ORDER BY sequence",
            )
            .map_err(|source| {
                RuntimeStoreError::operation("prepare continuation text scan", source)
            })?;
        let rows = statement
            .query_map(
                params![mission_id.as_str(), CONTINUATION_DECISION_TRANSITION_KIND],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| RuntimeStoreError::operation("scan continuation text", source))?;
        let mut cells = Vec::new();
        for row in rows {
            cells.push(row.map_err(|source| {
                RuntimeStoreError::operation("decode continuation text", source)
            })?);
        }
        Ok(cells)
    }

    /// The sorted set of distinct top-level payload keys across a mission's
    /// continuation decision rows, for asserting no free-form / secret-bearing
    /// key (`session_id`, `raw`, `context`, `transcript`, ...) exists.
    fn continuation_decision_key_inventory(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT payload_json FROM journal
                 WHERE mission_id = ?1 AND transition_kind = ?2",
            )
            .map_err(|source| {
                RuntimeStoreError::operation("prepare continuation key inventory", source)
            })?;
        let rows = statement
            .query_map(
                params![mission_id.as_str(), CONTINUATION_DECISION_TRANSITION_KIND],
                |row| row.get::<_, String>(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("read continuation key inventory", source)
            })?;
        let mut keys = BTreeSet::new();
        for row in rows {
            let payload_json = row.map_err(|source| {
                RuntimeStoreError::operation("decode continuation key inventory", source)
            })?;
            let payload = validate_stored_json(&payload_json, "continuation decision payload")?;
            let object = payload
                .as_object()
                .ok_or(RuntimeStoreError::CorruptDatabase)?;
            for key in object.keys() {
                keys.insert(key.clone());
            }
        }
        Ok(keys.into_iter().collect())
    }

    /// Every recorded strategy fingerprint for a phase, in ascending attempt
    /// order. These are the prior strategies a continuation retry must not
    /// repeat.
    fn reasoning_attempt_fingerprints_private(
        &self,
        mission_id: &MissionId,
        phase_id: &str,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT strategy_fingerprint FROM reasoning_attempt
                 WHERE mission_id = ?1 AND phase_id = ?2
                 ORDER BY attempt",
            )
            .map_err(|source| {
                RuntimeStoreError::operation("prepare attempt fingerprints", source)
            })?;
        let rows = statement
            .query_map(params![mission_id.as_str(), phase_id], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|source| RuntimeStoreError::operation("read attempt fingerprints", source))?;
        let mut fingerprints = Vec::new();
        for row in rows {
            fingerprints.push(row.map_err(|source| {
                RuntimeStoreError::operation("decode attempt fingerprint", source)
            })?);
        }
        Ok(fingerprints)
    }

    /// Records one verified compatibility projection and returns a command
    /// acknowledgement only when every projection required by that transition
    /// is durable. Exact receipt retries are idempotent.
    pub fn record_projection(
        &mut self,
        receipt: &ProjectionReceipt,
    ) -> Result<Option<CommandAcknowledgement>, RuntimeStoreError> {
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin projection receipt transaction", source)
            })?;
        verify_schema_version(&transaction)?;
        let required: bool = transaction
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM projection_requirement
                    WHERE journal_sequence = ?1 AND projection = ?2
                 )",
                params![receipt.journal_sequence, receipt.projection.as_str()],
                |row| row.get(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("inspect projection requirement", source)
            })?;
        if !required {
            return Err(RuntimeStoreError::ProjectionNotRequired);
        }
        if let Some(existing) =
            load_projection_receipt(&transaction, receipt.journal_sequence, receipt.projection)?
        {
            let acknowledgement = load_command_ack(&transaction, receipt.journal_sequence)?;
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close projection receipt retry", source)
            })?;
            return (existing == *receipt)
                .then_some(acknowledgement)
                .ok_or(RuntimeStoreError::ProjectionConflict);
        }
        if receipt.projection == CompatibilityProjection::EventLog {
            validate_event_projection_binding(&transaction, receipt)?;
        }
        transaction
            .execute(
                "INSERT INTO projection_receipt (
                    record_schema_version, journal_sequence, projection, mission_id,
                    public_sequence, event_id, event_jsonl, event_sha256, applied_at_utc
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    receipt.record_schema_version,
                    receipt.journal_sequence,
                    receipt.projection.as_str(),
                    receipt.mission_id,
                    receipt.public_sequence,
                    receipt.event_id,
                    receipt.event_jsonl,
                    receipt.event_sha256,
                    receipt.applied_at_utc,
                ],
            )
            .map_err(|source| {
                if is_constraint_violation(&source) {
                    RuntimeStoreError::ProjectionConflict
                } else {
                    RuntimeStoreError::operation("insert projection receipt", source)
                }
            })?;
        advance_projection_cursor(&transaction, receipt.projection, &receipt.applied_at_utc)?;
        let remaining: i64 = transaction
            .query_row(
                "SELECT count(*)
                 FROM projection_requirement AS requirement
                 LEFT JOIN projection_receipt AS receipt
                   ON receipt.journal_sequence = requirement.journal_sequence
                  AND receipt.projection = requirement.projection
                 WHERE requirement.journal_sequence = ?1
                   AND receipt.journal_sequence IS NULL",
                [receipt.journal_sequence],
                |row| row.get(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("count pending compatibility projections", source)
            })?;
        if remaining == 0 {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO command_ack (journal_sequence, acknowledged_at_utc)
                     VALUES (?1, ?2)",
                    params![receipt.journal_sequence, receipt.applied_at_utc],
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("acknowledge projected transition", source)
                })?;
        }
        let acknowledgement = load_command_ack(&transaction, receipt.journal_sequence)?;
        transaction
            .commit()
            .map_err(|source| RuntimeStoreError::operation("commit projection receipt", source))?;
        self.verify_retained()?;
        Ok(acknowledgement)
    }

    /// Returns a bounded, single-transaction, crate-private snapshot of one
    /// mission's journal-first projection state for projector recovery.
    ///
    /// The snapshot carries only typed/canonical data: no SQLite connection,
    /// filesystem path, or write capability escapes this call. Exceeding
    /// either bound in `bounds` is a typed [`RuntimeStoreError::RecoveryBoundsExceeded`],
    /// never a silent truncation.
    pub(crate) fn projection_recovery_snapshot(
        &mut self,
        mission_id: &MissionId,
        bounds: &ProjectionRecoveryBounds,
    ) -> Result<ProjectionRecoverySnapshot, RuntimeStoreError> {
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|source| {
                RuntimeStoreError::operation("begin projection recovery snapshot", source)
            })?;
        verify_schema_version(&transaction)?;
        let snapshot = build_projection_recovery_snapshot(&transaction, mission_id, bounds);
        transaction.rollback().map_err(|source| {
            RuntimeStoreError::operation("close projection recovery snapshot", source)
        })?;
        self.verify_retained()?;
        snapshot
    }

    /// Reads pending effects in deterministic journal/key order without
    /// changing their eligibility.
    pub fn pending_effects(&self, limit: usize) -> Result<Vec<OutboxEffect>, RuntimeStoreError> {
        self.effects_in_states(&[OutboxState::Pending], limit)
    }

    /// Reads effects requiring crash reconciliation. `executing` means the
    /// process died after claim; `uncertain` means observation could not prove
    /// whether the external effect happened.
    pub fn recovery_effects(&self, limit: usize) -> Result<Vec<OutboxEffect>, RuntimeStoreError> {
        self.effects_in_states(&[OutboxState::Executing, OutboxState::Uncertain], limit)
    }

    /// Reads one exact effect and the newest observation for its current
    /// attempt in a single transaction.
    ///
    /// Unlike bounded queue/history reads, this cannot substitute a sibling
    /// key, return an observation from an older attempt, or expose a torn
    /// state/observation pair.
    pub(crate) fn exact_outbox_snapshot(
        &mut self,
        idempotency_key: &str,
    ) -> Result<Option<ExactOutboxSnapshot>, RuntimeStoreError> {
        validate_atom(
            "outbox idempotency key",
            idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|source| {
                RuntimeStoreError::operation("begin exact outbox snapshot", source)
            })?;
        verify_schema_version(&transaction)?;
        let snapshot = load_exact_outbox_snapshot(&transaction, idempotency_key);
        transaction.rollback().map_err(|source| {
            RuntimeStoreError::operation("close exact outbox snapshot", source)
        })?;
        self.verify_retained()?;
        snapshot
    }

    /// Reads the sole effect occupying one exact logical process slot,
    /// independent of the request-bound idempotency key.
    fn exact_logical_outbox_snapshot(
        &mut self,
        mission_id: &MissionId,
        phase_id: Option<&str>,
        effect_kind: OutboxEffectKind,
        operation_slot: &EffectOperationSlot,
        logical_attempt: u32,
    ) -> Result<Option<ExactOutboxSnapshot>, RuntimeStoreError> {
        if let Some(phase_id) = phase_id {
            validate_atom("outbox phase ID", phase_id, MAX_IDENTIFIER_BYTES)?;
        }
        let _validated_slot = EffectOperationSlot::new(operation_slot.as_str())?;
        if logical_attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "outbox logical attempt must be positive",
            ));
        }
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(|source| {
                RuntimeStoreError::operation("begin exact logical outbox snapshot", source)
            })?;
        verify_schema_version(&transaction)?;
        let snapshot = (|| {
            let keys = {
                let mut statement = transaction
                    .prepare(
                        "SELECT idempotency_key
                         FROM outbox
                         WHERE mission_id = ?1
                           AND phase_id IS ?2
                           AND effect_kind = ?3
                           AND operation_slot = ?4
                           AND logical_attempt = ?5
                         ORDER BY journal_sequence, idempotency_key
                         LIMIT 2",
                    )
                    .map_err(|source| {
                        RuntimeStoreError::operation(
                            "prepare exact logical outbox snapshot",
                            source,
                        )
                    })?;
                let rows = statement
                    .query_map(
                        params![
                            mission_id.as_str(),
                            phase_id,
                            effect_kind.as_str(),
                            operation_slot.as_str(),
                            i64::from(logical_attempt),
                        ],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(|source| {
                        RuntimeStoreError::operation("query exact logical outbox snapshot", source)
                    })?;
                rows.collect::<Result<Vec<_>, _>>().map_err(|source| {
                    RuntimeStoreError::operation("read exact logical outbox snapshot", source)
                })?
            };
            match keys.as_slice() {
                [] => Ok(None),
                [idempotency_key] => load_exact_outbox_snapshot(&transaction, idempotency_key),
                _ => Err(RuntimeStoreError::CorruptDatabase),
            }
        })();
        transaction.rollback().map_err(|source| {
            RuntimeStoreError::operation("close exact logical outbox snapshot", source)
        })?;
        self.verify_retained()?;
        snapshot
    }

    /// Claims the first pending legacy effect and durably increments its
    /// attempt count before the caller may perform external I/O.
    ///
    /// Typed process effects are never claimed through this compatibility
    /// surface: they require [`Self::claim_exact`] so the caller receives the
    /// non-forgeable [`ClaimedOutboxEffect`] needed for atomic release
    /// authorization. A typed effect at the head of the deterministic queue
    /// fails without mutation and is never skipped.
    pub fn claim_next(
        &mut self,
        updated_at_utc: &str,
    ) -> Result<Option<OutboxEffect>, RuntimeStoreError> {
        validate_timestamp(updated_at_utc)?;
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| RuntimeStoreError::operation("begin outbox claim", source))?;
        verify_schema_version(&transaction)?;
        let key = next_claimable_effect_key(&transaction)?;
        let Some(key) = key else {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close empty outbox claim", source)
            })?;
            return Ok(None);
        };
        if load_process_intent(&transaction, &key)?.is_some() {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close typed outbox compatibility claim", source)
            })?;
            return Err(RuntimeStoreError::TypedProcessRequiresExactClaim);
        }
        let effect = claim_pending_effect(&transaction, &key, updated_at_utc)?;
        transaction
            .commit()
            .map_err(|source| RuntimeStoreError::operation("commit outbox claim", source))?;
        self.verify_retained()?;
        Ok(Some(effect))
    }

    /// Claims only the supplied effect when it is the first eligible effect in
    /// deterministic journal/key order. Any missing, unacknowledged, or
    /// non-pending key is rejected without substituting another queued effect.
    pub fn claim_exact(
        &mut self,
        expected_idempotency_key: &str,
        updated_at_utc: &str,
    ) -> Result<ClaimedOutboxEffect, RuntimeStoreError> {
        validate_atom(
            "outbox idempotency key",
            expected_idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_timestamp(updated_at_utc)?;
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| RuntimeStoreError::operation("begin exact outbox claim", source))?;
        verify_schema_version(&transaction)?;

        let expected = load_effect(&transaction, expected_idempotency_key)?
            .ok_or(RuntimeStoreError::ExpectedEffectNotClaimable)?;
        if expected.state != OutboxState::Pending
            || load_command_ack(&transaction, expected.journal_sequence)?.is_none()
        {
            return Err(RuntimeStoreError::ExpectedEffectNotClaimable);
        }
        let next =
            next_claimable_effect_key(&transaction)?.ok_or(RuntimeStoreError::CorruptDatabase)?;
        if next != expected_idempotency_key {
            return Err(RuntimeStoreError::ExpectedEffectOutOfOrder);
        }

        let effect = claim_pending_effect(&transaction, expected_idempotency_key, updated_at_utc)?;
        let claimed = ClaimedOutboxEffect::from_claimed(effect)?;
        transaction
            .commit()
            .map_err(|source| RuntimeStoreError::operation("commit exact outbox claim", source))?;
        self.verify_retained()?;
        Ok(claimed)
    }

    /// Captures the exact pending state consumed by
    /// [`Self::claim_prepared_process`].
    #[cfg(unix)]
    pub(crate) fn prepare_exact_process_claim(
        &self,
        binding: &ProcessEffectBinding,
        request: &ProcessRequest,
    ) -> Result<PendingProcessClaim, RuntimeStoreError> {
        if !binding.matches(request) {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        validate_atom(
            "outbox idempotency key",
            &binding.idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let effect = load_effect(connection, &binding.idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let expected_binding = ProcessIntentEnvelope::new(binding.fingerprint);
        let expected_request = ProcessIntentEnvelope::new(request.fingerprint());
        let process_intent = load_process_intent(connection, &binding.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        if effect.idempotency_key != binding.idempotency_key
            || effect.effect_kind != binding.effect_kind
            || validate_process_effect_purpose(effect.effect_kind, request.purpose()).is_err()
            || process_intent.request_sha256 != expected_binding.request_sha256
            || process_intent.request_sha256 != expected_request.request_sha256
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        if effect.state != OutboxState::Pending || effect.execution_identity.is_some() {
            return Err(RuntimeStoreError::ExpectedEffectNotClaimable);
        }
        Ok(PendingProcessClaim {
            prior: effect,
            fingerprint: binding.fingerprint,
        })
    }

    /// Claims a prepared process effect and reconciles any returned error
    /// against the exact pre-claim snapshot. It never resumes an older
    /// executing attempt and never emits a second release capability.
    #[cfg(unix)]
    pub(crate) fn claim_prepared_process(
        &mut self,
        prepared: PendingProcessClaim,
        updated_at_utc: &str,
    ) -> Result<ClaimedProcessAttempt, ExactProcessClaimError> {
        if self.boundary_kind != StoreBoundaryKind::PrivateProcessLedger {
            return self.claim_prepared_process_without_recovery_protocol(prepared, updated_at_utc);
        }
        match self.claim_prepared_process_with_marker(&prepared, updated_at_utc) {
            Ok(claimed) => Ok(claimed),
            Err(_) => match self.reconcile_prepared_process_claim(&prepared) {
                Ok(Some(claimed)) => Ok(ClaimedProcessAttempt {
                    claimed,
                    fingerprint: prepared.fingerprint,
                }),
                Ok(None) => Err(ExactProcessClaimError::ProvenNotCommitted),
                Err(_) => Err(ExactProcessClaimError::Indeterminate),
            },
        }
    }

    #[cfg(unix)]
    fn claim_prepared_process_without_recovery_protocol(
        &mut self,
        prepared: PendingProcessClaim,
        updated_at_utc: &str,
    ) -> Result<ClaimedProcessAttempt, ExactProcessClaimError> {
        let fingerprint = prepared.fingerprint;
        match self.claim_exact(&prepared.prior.idempotency_key, updated_at_utc) {
            Ok(claimed)
                if claimed.claim_attempt == prepared.prior.attempts.saturating_add(1)
                    && claimed_process_matches_snapshot(&claimed, &prepared.prior) =>
            {
                Ok(ClaimedProcessAttempt {
                    claimed,
                    fingerprint,
                })
            }
            Ok(_) => Err(ExactProcessClaimError::Indeterminate),
            Err(_) => match self.reconcile_prepared_process_claim_without_protocol(&prepared) {
                Ok(Some(claimed)) => Ok(ClaimedProcessAttempt {
                    claimed,
                    fingerprint,
                }),
                Ok(None) => Err(ExactProcessClaimError::ProvenNotCommitted),
                Err(_) => Err(ExactProcessClaimError::Indeterminate),
            },
        }
    }

    #[cfg(unix)]
    fn claim_prepared_process_with_marker(
        &mut self,
        prepared: &PendingProcessClaim,
        updated_at_utc: &str,
    ) -> Result<ClaimedProcessAttempt, RuntimeStoreError> {
        validate_timestamp(updated_at_utc)?;
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin prepared process claim", source)
            })?;
        verify_schema_version(&transaction)?;
        let expected = load_effect(&transaction, &prepared.prior.idempotency_key)?
            .ok_or(RuntimeStoreError::ExpectedEffectNotClaimable)?;
        let process_intent = load_process_intent(&transaction, &prepared.prior.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        let expected_request = hex_digest(prepared.fingerprint.as_bytes());
        if expected.state != OutboxState::Pending
            || expected.execution_identity.is_some()
            || load_command_ack(&transaction, expected.journal_sequence)?.is_none()
            || !process_effect_immutable_fields_match(&expected, &prepared.prior)
            || process_intent.request_sha256 != expected_request
        {
            return Err(RuntimeStoreError::ExpectedEffectNotClaimable);
        }
        let next =
            next_claimable_effect_key(&transaction)?.ok_or(RuntimeStoreError::CorruptDatabase)?;
        if next != prepared.prior.idempotency_key {
            return Err(RuntimeStoreError::ExpectedEffectOutOfOrder);
        }
        let effect = claim_pending_effect(
            &transaction,
            &prepared.prior.idempotency_key,
            updated_at_utc,
        )?;
        let claimed = ClaimedOutboxEffect::from_claimed(effect)?;
        if claimed.claim_attempt != prepared.prior.attempts.saturating_add(1)
            || !claimed_process_matches_snapshot(&claimed, &prepared.prior)
        {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        let claimed = ClaimedProcessAttempt {
            claimed,
            fingerprint: prepared.fingerprint,
        };
        let marker = ProcessRecoveryMarker::from_claimed(
            &claimed,
            ProcessRecoveryMarkerStage::AttemptClaimed,
            updated_at_utc,
        )?;
        insert_private_recovery_marker(&transaction, &marker)?;
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit prepared process claim", source)
        })?;
        self.verify_retained()?;
        Ok(claimed)
    }

    #[cfg(unix)]
    fn reconcile_prepared_process_claim(
        &self,
        prepared: &PendingProcessClaim,
    ) -> Result<Option<ClaimedOutboxEffect>, RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let current = load_effect(connection, &prepared.prior.idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let process_intent = load_process_intent(connection, &prepared.prior.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        let expected = ProcessIntentEnvelope::new(prepared.fingerprint);
        if process_intent.request_sha256 != expected.request_sha256
            || !process_effect_immutable_fields_match(&current, &prepared.prior)
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        if current.state == OutboxState::Pending
            && current.attempts == prepared.prior.attempts
            && current.execution_identity.is_none()
        {
            return Ok(None);
        }
        if current.state == OutboxState::Executing
            && current.attempts == prepared.prior.attempts.saturating_add(1)
            && current.execution_identity.is_none()
        {
            let request_sha256 = hex_digest(prepared.fingerprint.as_bytes());
            if load_process_recovery_marker(
                connection,
                &current,
                &request_sha256,
                ProcessRecoveryMarkerStage::AttemptClaimed,
            )?
            .is_some()
            {
                return ClaimedOutboxEffect::from_claimed(current).map(Some);
            }
        }
        Err(RuntimeStoreError::InvalidOutboxTransition)
    }

    #[cfg(unix)]
    fn reconcile_prepared_process_claim_without_protocol(
        &self,
        prepared: &PendingProcessClaim,
    ) -> Result<Option<ClaimedOutboxEffect>, RuntimeStoreError> {
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let current = load_effect(connection, &prepared.prior.idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let process_intent = load_process_intent(connection, &prepared.prior.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        let expected = ProcessIntentEnvelope::new(prepared.fingerprint);
        if process_intent.request_sha256 != expected.request_sha256
            || !process_effect_immutable_fields_match(&current, &prepared.prior)
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        if current.state == OutboxState::Pending
            && current.attempts == prepared.prior.attempts
            && current.execution_identity.is_none()
        {
            return Ok(None);
        }
        if current.state == OutboxState::Executing
            && current.attempts == prepared.prior.attempts.saturating_add(1)
            && current.execution_identity.is_none()
        {
            return ClaimedOutboxEffect::from_claimed(current).map(Some);
        }
        Err(RuntimeStoreError::InvalidOutboxTransition)
    }

    /// Persists the immutable post-initialization permit for one exact claim.
    ///
    /// This does not spawn or release anything. The caller must complete all
    /// specification and runtime initialization first, then invoke this method
    /// immediately before creating the gated launcher. An exact retry observes
    /// the already-retained marker; a divergent binding fails closed.
    #[cfg(unix)]
    pub(crate) fn record_process_spawn_permit(
        &mut self,
        claimed: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        permitted_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        validate_timestamp(permitted_at_utc)?;
        if !claimed.matches_request(request) {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        self.verify_retained()?;
        let request_sha256 = hex_digest(request.fingerprint().as_bytes());
        {
            let connection = self.connection()?;
            verify_schema_version(connection)?;
            let durable = load_effect(connection, claimed.idempotency_key())?
                .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
            let intent = load_process_intent(connection, claimed.idempotency_key())?
                .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
            if durable.state != OutboxState::Executing
                || durable.attempts != claimed.claim_attempt()
                || durable.execution_identity.is_some()
                || !claimed_process_matches_effect(claimed.claimed(), &durable)
                || intent.request_sha256 != request_sha256
                || process_release_authorization_exists(
                    connection,
                    claimed.idempotency_key(),
                    claimed.claim_attempt(),
                )?
            {
                return Err(RuntimeStoreError::InvalidOutboxTransition);
            }
            load_process_recovery_marker(
                connection,
                &durable,
                &request_sha256,
                ProcessRecoveryMarkerStage::AttemptClaimed,
            )?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
            if load_process_recovery_marker(
                connection,
                &durable,
                &request_sha256,
                ProcessRecoveryMarkerStage::SpawnPermitted,
            )?
            .is_some()
            {
                return Ok(());
            }
        }
        let marker = ProcessRecoveryMarker::from_claimed(
            claimed,
            ProcessRecoveryMarkerStage::SpawnPermitted,
            permitted_at_utc,
        )?;
        self.append_private(&marker.intent()?)?;
        Ok(())
    }

    /// Reconstructs restart-only authority for one exact typed process.
    ///
    /// Only an attempt whose claim-protocol marker was committed atomically
    /// with the claim can be classified as safely pre-spawn. Legacy active
    /// attempts without that marker remain unresolved. A spawn permit without
    /// an identity carries no cleanup or terminal authority; a blocked or
    /// released identity can mint cleanup authority only by consuming the
    /// returned attempt.
    #[cfg(unix)]
    pub(crate) fn recover_exact_process_attempt(
        &self,
        binding: &ProcessEffectBinding,
        request: &ProcessRequest,
    ) -> Result<RecoveredProcessAttempt, RuntimeStoreError> {
        if !binding.matches(request) {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        validate_atom(
            "outbox idempotency key",
            &binding.idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let snapshot = load_exact_outbox_snapshot(connection, &binding.idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let effect = snapshot.effect;
        let request_intent = ProcessIntentEnvelope::new(request.fingerprint());
        let binding_intent = ProcessIntentEnvelope::new(binding.fingerprint);
        let durable_intent = load_process_intent(connection, &binding.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        if effect.idempotency_key != binding.idempotency_key
            || effect.effect_kind != binding.effect_kind
            || validate_process_effect_purpose(effect.effect_kind, request.purpose()).is_err()
            || durable_intent.request_sha256 != request_intent.request_sha256
            || durable_intent.request_sha256 != binding_intent.request_sha256
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        if !matches!(
            effect.state,
            OutboxState::Executing | OutboxState::Uncertain
        ) || effect.attempts == 0
        {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        let authorized = process_release_authorization_exists(
            connection,
            &effect.idempotency_key,
            effect.attempts,
        )?;
        let request_sha256 = request_intent.request_sha256;
        let claim_marker = load_process_recovery_marker(
            connection,
            &effect,
            &request_sha256,
            ProcessRecoveryMarkerStage::AttemptClaimed,
        )?;
        let spawn_permit = load_process_recovery_marker(
            connection,
            &effect,
            &request_sha256,
            ProcessRecoveryMarkerStage::SpawnPermitted,
        )?;
        let started_observed = load_process_recovery_marker(
            connection,
            &effect,
            &request_sha256,
            ProcessRecoveryMarkerStage::StartedObserved,
        )?;
        let state = effect.state;
        let identity = effect.execution_identity.clone();
        match (
            state,
            identity,
            authorized,
            claim_marker.is_some(),
            spawn_permit.is_some(),
            started_observed.is_some(),
        ) {
            (OutboxState::Executing, None, false, true, false, false) => {
                let claimed = ClaimedOutboxEffect::from_recovered_process(effect)?;
                Ok(RecoveredProcessAttempt {
                    kind: RecoveredProcessAttemptKind::ClaimedBeforeSpawn {
                        terminal: ClaimedProcessAttempt {
                            claimed,
                            fingerprint: binding.fingerprint,
                        }
                        .into_terminal(),
                    },
                })
            }
            (OutboxState::Executing, None, false, true, true, false) => {
                Ok(RecoveredProcessAttempt {
                    kind: RecoveredProcessAttemptKind::SpawnUnobserved,
                })
            }
            (OutboxState::Executing, Some(identity), false, true, true, false) => {
                if identity.attempt() != effect.attempts {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                let claimed = ClaimedOutboxEffect::from_recovered_process(effect)?;
                Ok(RecoveredProcessAttempt {
                    kind: RecoveredProcessAttemptKind::BlockedLauncher {
                        terminal: ClaimedProcessAttempt {
                            claimed,
                            fingerprint: binding.fingerprint,
                        }
                        .into_terminal(),
                        identity,
                    },
                })
            }
            (OutboxState::Uncertain, Some(identity), false, true, true, false) => {
                if identity.attempt() != effect.attempts {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                Ok(RecoveredProcessAttempt {
                    kind: RecoveredProcessAttemptKind::UnreleasedUncertain { identity },
                })
            }
            (
                OutboxState::Executing | OutboxState::Uncertain,
                Some(identity),
                true,
                claim_protocol,
                spawn_permitted,
                started,
            ) => {
                if identity.attempt() != effect.attempts {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                if claim_protocol != spawn_permitted {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                if started && (!claim_protocol || !spawn_permitted) {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                let kind = if state == OutboxState::Executing {
                    let claimed = ClaimedOutboxEffect::from_recovered_process(effect)?;
                    let terminal = ClaimedProcessAttempt {
                        claimed,
                        fingerprint: binding.fingerprint,
                    }
                    .into_terminal();
                    if started {
                        RecoveredProcessAttemptKind::StartedExecuting { terminal, identity }
                    } else {
                        RecoveredProcessAttemptKind::AuthorizedWithoutStarted { terminal, identity }
                    }
                } else if started {
                    RecoveredProcessAttemptKind::StartedUncertain { identity }
                } else {
                    RecoveredProcessAttemptKind::AuthorizedUncertain { identity }
                };
                Ok(RecoveredProcessAttempt { kind })
            }
            (OutboxState::Executing | OutboxState::Uncertain, None, false, false, false, false)
            | (OutboxState::Uncertain, None, false, true, false, false)
            | (OutboxState::Uncertain, None, false, true, true, false) => {
                Ok(RecoveredProcessAttempt {
                    kind: RecoveredProcessAttemptKind::Unresolved,
                })
            }
            _ => Err(RuntimeStoreError::CorruptDatabase),
        }
    }

    /// Returns the recorded launcher identity and whether the durable started
    /// receipt confirmed target release for one exact request-bound effect.
    #[cfg(unix)]
    pub(crate) fn process_metric_identity(
        &self,
        binding: &ProcessEffectBinding,
        request: &ProcessRequest,
    ) -> Result<(Option<ProcessExecutionIdentity>, bool), RuntimeStoreError> {
        if !binding.matches(request) {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let snapshot = load_exact_outbox_snapshot(connection, &binding.idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let effect = snapshot.effect;
        let request_sha256 = ProcessIntentEnvelope::new(request.fingerprint()).request_sha256;
        let durable_intent = load_process_intent(connection, &binding.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        if effect.idempotency_key != binding.idempotency_key
            || effect.effect_kind != binding.effect_kind
            || durable_intent.request_sha256 != request_sha256
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        let started = load_process_recovery_marker(
            connection,
            &effect,
            &request_sha256,
            ProcessRecoveryMarkerStage::StartedObserved,
        )?
        .is_some();
        Ok((effect.execution_identity, started))
    }

    #[cfg(unix)]
    fn resolve_recovered_before_spawn(
        &mut self,
        recovered: RecoveredProcessAttempt,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        let terminal = recovered.into_before_spawn_terminal()?;
        let resolution = EffectResolution::NotStarted(EffectEvidence::process_not_started(
            ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn,
        ));
        self.resolve_recovered_terminal(&terminal, &resolution, observed_at_utc)
    }

    #[cfg(unix)]
    fn resolve_recovered_process_group_absent(
        &mut self,
        cleanup: RecoveredProcessCleanup,
        absence: &ExactProcessGroupAbsence,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        let (terminal, resolution) = match cleanup.into_group_absent_resolution(absence)? {
            RecoveredCleanupResolution::NotStarted(terminal) => (
                terminal,
                EffectResolution::NotStarted(EffectEvidence::process_not_started(
                    ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn,
                )),
            ),
            RecoveredCleanupResolution::ProcessUncertain(terminal) => (
                terminal,
                EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                    ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                )),
            ),
            RecoveredCleanupResolution::CleanupOnly => return Ok(()),
        };
        self.resolve_recovered_terminal(&terminal, &resolution, observed_at_utc)
    }

    fn resolve_recovered_terminal(
        &mut self,
        terminal: &ProcessTerminalClaim,
        resolution: &EffectResolution,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        if self
            .resolve_claimed_process(terminal, resolution.clone(), observed_at_utc)
            .is_ok()
            || self
                .resolve_claimed_process(terminal, resolution.clone(), observed_at_utc)
                .is_ok()
            || self.claimed_process_resolution_is_durable(terminal, resolution, observed_at_utc)?
        {
            Ok(())
        } else {
            Err(RuntimeStoreError::InvalidOutboxTransition)
        }
    }

    /// Records the exact OS identity of a claimed process. Exact retries are
    /// idempotent; conflicting identities fail closed.
    pub fn record_execution_identity(
        &mut self,
        idempotency_key: &str,
        identity: &ProcessExecutionIdentity,
    ) -> Result<OutboxEffect, RuntimeStoreError> {
        validate_atom(
            "outbox idempotency key",
            idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin process identity record", source)
            })?;
        verify_schema_version(&transaction)?;
        let effect = load_effect(&transaction, idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        if effect.state != OutboxState::Executing || effect.attempts != identity.attempt {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        if load_process_intent(&transaction, idempotency_key)?.is_some() {
            return Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected);
        }
        if let Some(existing) =
            load_execution_identity(&transaction, idempotency_key, identity.attempt)?
        {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close process identity retry", source)
            })?;
            return if existing == *identity {
                Ok(effect)
            } else {
                Err(RuntimeStoreError::ExecutionIdentityConflict)
            };
        }
        transaction
            .execute(
                "INSERT INTO outbox_execution_identity (
                    idempotency_key, attempt, pid, process_group_id,
                    process_start_identity, recorded_at_utc
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    idempotency_key,
                    identity.attempt,
                    identity.pid,
                    identity.process_group_id,
                    identity.process_start_identity,
                    identity.recorded_at_utc,
                ],
            )
            .map_err(|source| {
                RuntimeStoreError::operation("insert process execution identity", source)
            })?;
        let effect = load_effect(&transaction, idempotency_key)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit process execution identity", source)
        })?;
        self.verify_retained()?;
        Ok(effect)
    }

    /// Persists the exact still-blocked launcher identity before release.
    ///
    /// The post-initialization spawn permit must already be durable. Exact
    /// retries of the same gate identity are idempotent; a different identity,
    /// missing permit, or existing release authorization fails closed.
    #[cfg(unix)]
    pub(crate) fn record_blocked_process_identity(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        gate_request: &ProcessStartGateRequest,
        identity: &ProcessExecutionIdentity,
    ) -> Result<(), RuntimeStoreError> {
        let observed_gate = ObservedGateBinding::from_request(gate_request);
        self.record_blocked_process_identity_bound(
            attempt.claimed(),
            request,
            &observed_gate,
            identity,
        )
    }

    #[cfg(unix)]
    fn record_blocked_process_identity_bound(
        &mut self,
        claimed: &ClaimedOutboxEffect,
        request: &ProcessRequest,
        observed_gate: &ObservedGateBinding<'_>,
        identity: &ProcessExecutionIdentity,
    ) -> Result<(), RuntimeStoreError> {
        validate_process_effect_purpose(claimed.effect_kind, request.purpose())
            .map_err(|_| RuntimeStoreError::ProcessRequestMismatch)?;
        let fingerprint = request.fingerprint();
        let request_sha256 = hex_digest(fingerprint.as_bytes());
        if !observed_gate
            .request_binding
            .matches_bytes(fingerprint.as_bytes())
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        if identity.attempt != claimed.claim_attempt
            || identity.pid != observed_gate.pid
            || identity.process_group_id != observed_gate.process_group_id
            || identity.process_start_identity != observed_gate.process_start_identity
        {
            return Err(RuntimeStoreError::ProcessIdentityMismatch);
        }
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin blocked process identity record", source)
            })?;
        verify_schema_version(&transaction)?;
        let durable = load_effect(&transaction, &claimed.idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let process_intent = load_process_intent(&transaction, &claimed.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        if durable.state != OutboxState::Executing
            || durable.attempts != claimed.claim_attempt
            || !claimed_process_matches_effect(claimed, &durable)
            || process_intent.request_sha256 != request_sha256
            || process_release_authorization_exists(
                &transaction,
                &claimed.idempotency_key,
                claimed.claim_attempt,
            )?
        {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        load_process_recovery_marker(
            &transaction,
            &durable,
            &request_sha256,
            ProcessRecoveryMarkerStage::SpawnPermitted,
        )?
        .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        if let Some(existing) = load_execution_identity(
            &transaction,
            &claimed.idempotency_key,
            claimed.claim_attempt,
        )? {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close blocked identity retry", source)
            })?;
            return if existing == *identity {
                Ok(())
            } else {
                Err(RuntimeStoreError::ExecutionIdentityConflict)
            };
        }
        transaction
            .execute(
                "INSERT INTO outbox_execution_identity (
                    idempotency_key, attempt, pid, process_group_id,
                    process_start_identity, recorded_at_utc
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    claimed.idempotency_key,
                    identity.attempt,
                    identity.pid,
                    identity.process_group_id,
                    identity.process_start_identity,
                    identity.recorded_at_utc,
                ],
            )
            .map_err(|source| {
                if is_constraint_violation(&source) {
                    RuntimeStoreError::ExecutionIdentityConflict
                } else {
                    RuntimeStoreError::operation("insert blocked process identity", source)
                }
            })?;
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit blocked process identity", source)
        })?;
        self.verify_retained()?;
        Ok(())
    }

    /// Binds one exact claimed, permitted, and identity-recorded blocked
    /// launcher to an immutable durable release marker. A prior authorization
    /// rejects the operation, including an exact duplicate: recovery can
    /// inspect durable state but can never mint a second release permit.
    #[cfg(unix)]
    pub(crate) fn authorize_process_release(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        gate_request: &ProcessStartGateRequest,
        identity: &ProcessExecutionIdentity,
        authorized_at_utc: &str,
    ) -> Result<ProcessReleaseAuthorization, RuntimeStoreError> {
        let observed_gate = ObservedGateBinding::from_request(gate_request);
        self.authorize_process_release_bound(
            attempt.claimed(),
            request,
            &observed_gate,
            identity,
            authorized_at_utc,
            false,
        )
    }

    #[cfg(all(test, unix))]
    pub(crate) fn authorize_process_release_for_test(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        identity: &ProcessExecutionIdentity,
        authorized_at_utc: &str,
    ) -> Result<ProcessReleaseAuthorization, RuntimeStoreError> {
        let request_binding = ProcessRequestBinding::from_bytes(request.fingerprint().into_bytes());
        let observed_gate = ObservedGateBinding {
            request_binding: &request_binding,
            pid: identity.pid(),
            process_group_id: identity.process_group_id(),
            process_start_identity: identity.process_start_identity(),
        };
        self.record_process_spawn_permit(attempt, request, authorized_at_utc)?;
        self.record_blocked_process_identity_bound(
            attempt.claimed(),
            request,
            &observed_gate,
            identity,
        )?;
        self.authorize_process_release_bound(
            attempt.claimed(),
            request,
            &observed_gate,
            identity,
            authorized_at_utc,
            false,
        )
    }

    #[cfg(unix)]
    fn authorize_process_release_bound(
        &mut self,
        claimed: &ClaimedOutboxEffect,
        request: &ProcessRequest,
        observed_gate: &ObservedGateBinding<'_>,
        identity: &ProcessExecutionIdentity,
        authorized_at_utc: &str,
        inject_failure_after_identity: bool,
    ) -> Result<ProcessReleaseAuthorization, RuntimeStoreError> {
        validate_timestamp(authorized_at_utc)?;
        validate_process_effect_purpose(claimed.effect_kind, request.purpose())
            .map_err(|_| RuntimeStoreError::ProcessRequestMismatch)?;
        let fingerprint = request.fingerprint();
        let request_sha256 = hex_digest(fingerprint.as_bytes());
        if !observed_gate
            .request_binding
            .matches_bytes(fingerprint.as_bytes())
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        if identity.attempt != claimed.claim_attempt
            || identity.pid != observed_gate.pid
            || identity.process_group_id != observed_gate.process_group_id
            || identity.process_start_identity != observed_gate.process_start_identity
        {
            return Err(RuntimeStoreError::ProcessIdentityMismatch);
        }

        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin process release authorization", source)
            })?;
        verify_schema_version(&transaction)?;

        let durable = load_effect(&transaction, &claimed.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessReleaseAuthorizationRejected)?;
        if durable.state != OutboxState::Executing
            || durable.attempts != claimed.claim_attempt
            || durable.journal_sequence != claimed.journal_sequence
            || durable.mission_id != claimed.mission_id
            || durable.phase_id != claimed.phase_id
            || durable.effect_kind != claimed.effect_kind
            || durable.operation_slot != claimed.operation_slot
            || durable.logical_attempt != claimed.logical_attempt
            || durable.payload != claimed.payload
        {
            return Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected);
        }
        let process_intent = load_process_intent(&transaction, &claimed.idempotency_key)?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        if process_intent.request_sha256 != request_sha256 {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        load_process_recovery_marker(
            &transaction,
            &durable,
            &request_sha256,
            ProcessRecoveryMarkerStage::SpawnPermitted,
        )?
        .ok_or(RuntimeStoreError::ProcessReleaseAuthorizationRejected)?;
        let recorded = load_execution_identity(
            &transaction,
            &claimed.idempotency_key,
            claimed.claim_attempt,
        )?
        .ok_or(RuntimeStoreError::ProcessReleaseAuthorizationRejected)?;
        if recorded != *identity
            || process_release_authorization_exists(
                &transaction,
                &claimed.idempotency_key,
                claimed.claim_attempt,
            )?
        {
            return Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected);
        }
        if inject_failure_after_identity {
            return Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected);
        }
        let payload_json = canonical_json(&claimed.payload, "claimed process payload")?;
        let payload_sha256 = hex_digest(Sha256::digest(payload_json.as_bytes()).as_slice());
        transaction
            .execute(
                "INSERT INTO outbox_process_release_authorization (
                    idempotency_key, attempt, journal_sequence, mission_id, phase_id,
                    effect_kind, operation_slot, logical_attempt, payload_sha256,
                    request_sha256, pid, process_group_id, process_start_identity,
                    authorized_at_utc
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
                 )",
                params![
                    claimed.idempotency_key,
                    claimed.claim_attempt,
                    claimed.journal_sequence,
                    claimed.mission_id.as_str(),
                    claimed.phase_id,
                    claimed.effect_kind.as_str(),
                    claimed.operation_slot.as_str(),
                    claimed.logical_attempt,
                    payload_sha256,
                    request_sha256,
                    identity.pid,
                    identity.process_group_id,
                    identity.process_start_identity,
                    authorized_at_utc,
                ],
            )
            .map_err(|source| {
                if is_constraint_violation(&source) {
                    RuntimeStoreError::ProcessReleaseAuthorizationRejected
                } else {
                    RuntimeStoreError::operation("insert process release authorization", source)
                }
            })?;
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit process release authorization", source)
        })?;
        self.verify_retained()?;
        Ok(ProcessReleaseAuthorization {
            _idempotency_key: claimed.idempotency_key.clone(),
            claim_attempt: claimed.claim_attempt,
            request_fingerprint: fingerprint,
            pid: identity.pid,
            process_group_id: identity.process_group_id,
            process_start_identity: identity.process_start_identity.clone(),
        })
    }

    /// Persists the single post-grant observation for one exact authorized attempt.
    ///
    /// The lower receipt is non-forgeable and the app binding additionally
    /// requires the exact claimed attempt, request fingerprint, immutable
    /// release authorization, and kernel identity. An exact retry is
    /// idempotent; any divergent binding fails closed.
    #[cfg(unix)]
    pub(crate) fn record_process_started_observed(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        authorized: &AuthorizedLauncherIdentity,
        receipt: &ProcessStartedReceipt,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        validate_timestamp(observed_at_utc)?;
        if !attempt.matches_request(request) {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        let attempt_binding = attempt.outcome_binding();
        let bound = authorized.bind_started(&attempt_binding, receipt)?;
        self.record_process_started_observed_bound(attempt, request, &bound, observed_at_utc)
    }

    #[cfg(all(test, unix))]
    fn record_process_started_observed_for_test(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        authorization: &ProcessReleaseAuthorization,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        let attempt_binding = attempt.outcome_binding();
        let authorized = AuthorizedLauncherIdentity::from_authorization(authorization);
        let bound = BoundStartedProcess {
            attempt: &attempt_binding,
            authorized: &authorized,
        };
        self.record_process_started_observed_bound(attempt, request, &bound, observed_at_utc)
    }

    #[cfg(unix)]
    fn record_process_started_observed_bound(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        bound: &BoundStartedProcess<'_>,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        validate_timestamp(observed_at_utc)?;
        if !attempt.matches_request(request)
            || !bound.attempt.matches(attempt)
            || !bound.authorized.matches_attempt_binding(bound.attempt)
        {
            return Err(RuntimeStoreError::ProcessRequestMismatch);
        }
        let authorized = bound.authorized;
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin started process observation", source)
            })?;
        verify_schema_version(&transaction)?;
        let durable = load_effect(&transaction, attempt.idempotency_key())?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let intent = load_process_intent(&transaction, attempt.idempotency_key())?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        let request_sha256 = hex_digest(request.fingerprint().as_bytes());
        let identity = load_execution_identity(
            &transaction,
            attempt.idempotency_key(),
            attempt.claim_attempt(),
        )?
        .ok_or(RuntimeStoreError::ProcessIdentityMismatch)?;
        let authorization = load_process_release_authorization(
            &transaction,
            attempt.idempotency_key(),
            attempt.claim_attempt(),
        )?
        .ok_or(RuntimeStoreError::ProcessReleaseAuthorizationRejected)?;
        let payload_json = canonical_json(&durable.payload, "started process payload")?;
        let payload_sha256 = hex_digest(Sha256::digest(payload_json.as_bytes()).as_slice());
        if durable.state != OutboxState::Executing
            || durable.attempts != attempt.claim_attempt()
            || !claimed_process_matches_effect(attempt.claimed(), &durable)
            || intent.request_sha256 != request_sha256
            || identity.pid != authorized.pid
            || identity.process_group_id != authorized.process_group_id
            || identity.process_start_identity != authorized.process_start_identity
            || authorization.journal_sequence != durable.journal_sequence
            || authorization.mission_id != durable.mission_id.as_str()
            || authorization.phase_id.as_deref() != durable.phase_id.as_deref()
            || authorization.effect_kind != durable.effect_kind.as_str()
            || authorization.operation_slot != durable.operation_slot.as_str()
            || authorization.logical_attempt != durable.logical_attempt
            || authorization.payload_sha256 != payload_sha256
            || authorization.request_sha256 != request_sha256
            || authorization.pid != authorized.pid
            || authorization.process_group_id != authorized.process_group_id
            || authorization.process_start_identity != authorized.process_start_identity
        {
            return Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected);
        }
        load_process_recovery_marker(
            &transaction,
            &durable,
            &request_sha256,
            ProcessRecoveryMarkerStage::SpawnPermitted,
        )?
        .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        if load_process_recovery_marker(
            &transaction,
            &durable,
            &request_sha256,
            ProcessRecoveryMarkerStage::StartedObserved,
        )?
        .is_some()
        {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close started process retry", source)
            })?;
            return Ok(());
        }
        let marker = ProcessRecoveryMarker::from_started(
            bound,
            &authorization.authorized_at_utc,
            observed_at_utc,
        )?;
        insert_private_recovery_marker(&transaction, &marker)?;
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit started process observation", source)
        })?;
        self.verify_retained()?;
        Ok(())
    }

    /// Read-only recovery probe. It reports durable authorization state but
    /// deliberately cannot recreate a release receipt or gate capability.
    pub fn process_release_was_authorized(
        &self,
        idempotency_key: &str,
        attempt: u32,
    ) -> Result<bool, RuntimeStoreError> {
        validate_atom(
            "outbox idempotency key",
            idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        if attempt == 0 {
            return Err(RuntimeStoreError::InvalidIntent(
                "process release attempt must be positive",
            ));
        }
        self.verify_retained()?;
        process_release_authorization_exists(self.connection()?, idempotency_key, attempt)
    }

    /// Durably resolves or explicitly requeues a claimed/recovered effect.
    /// Every decision appends immutable evidence; uncertain effects never
    /// become pending without observed-absence proof or operator authority.
    /// Claims up to `limit` pending publications in `sequence` order.
    ///
    /// Ordering is per `(mission_id, phase_id)` by `sequence`; there is no
    /// cross-mission total order. `limit` is explicit and bounded by
    /// [`MAX_QUERY_LIMIT`], so this queue can never be drained as an
    /// unbounded scan.
    ///
    /// Restart recovery is exactly this call: a row committed before a crash is
    /// still `pending`, so it is re-claimed and re-delivered. The consumer's
    /// upsert key makes redelivery a no-op.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::InvalidIntent`] for a zero or over-large
    /// limit, and [`RuntimeStoreError::CorruptDatabase`] for an unreadable row.
    pub fn claim_pending_publications(
        &mut self,
        _actor: &StorageActorAuthority,
        limit: usize,
    ) -> Result<Vec<ClaimedPublication>, RuntimeStoreError> {
        if limit == 0 || limit > MAX_QUERY_LIMIT {
            return Err(RuntimeStoreError::InvalidIntent(
                "knowledge publication claim limit must be within the query bound",
            ));
        }
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin publication claim transaction", source)
            })?;
        verify_schema_version(&transaction)?;
        let claimed = {
            let mut statement = transaction
                .prepare(
                    "SELECT sequence, idempotency_key, journal_sequence, mission_id, phase_id,
                            logical_attempt, namespace, type_name, primitive_kind, schema_version,
                            registry_generation, sensitivity, identity, payload_json,
                            payload_digest, attempts
                     FROM knowledge_publication
                     WHERE state = 'pending'
                     ORDER BY sequence
                     LIMIT ?1",
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("prepare publication claim", source)
                })?;
            let rows = statement
                .query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, i64>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                        row.get::<_, String>(11)?,
                        row.get::<_, String>(12)?,
                        row.get::<_, String>(13)?,
                        row.get::<_, String>(14)?,
                        row.get::<_, i64>(15)?,
                    ))
                })
                .map_err(|source| {
                    RuntimeStoreError::operation("query pending publications", source)
                })?;
            let mut collected = Vec::new();
            for row in rows {
                let row = row.map_err(|source| {
                    RuntimeStoreError::operation("decode pending publication", source)
                })?;
                collected.push(ClaimedPublication {
                    sequence: row.0,
                    idempotency_key: row.1,
                    journal_sequence: row.2,
                    mission_id: MissionId::new(row.3)
                        .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                    phase_id: row.4,
                    logical_attempt: u32::try_from(row.5)
                        .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                    namespace: row.6,
                    type_name: row.7,
                    primitive_kind: row.8,
                    schema_version: u32::try_from(row.9)
                        .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                    registry_generation: u64::try_from(row.10)
                        .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                    sensitivity: row.11,
                    identity: row.12,
                    payload_json: row.13,
                    payload_digest: row.14,
                    claim_attempt: u32::try_from(row.15)
                        .map_err(|_| RuntimeStoreError::CorruptDatabase)?
                        .saturating_add(1),
                });
            }
            collected
        };
        for publication in &claimed {
            transaction
                .execute(
                    "UPDATE knowledge_publication SET attempts = ?2
                     WHERE sequence = ?1 AND state = 'pending'",
                    params![publication.sequence, publication.claim_attempt],
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("record publication claim", source)
                })?;
        }
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit publication claim transaction", source)
        })?;
        self.verify_retained()?;
        Ok(claimed)
    }

    /// Resolves exactly one claimed publication.
    ///
    /// The claim token is consumed, so one key cannot be resolved twice from
    /// the same claim. `outcome` is mandatory and has no success default: a
    /// failed delivery becomes a retained `dead_letter` row and **never**
    /// advances anything as successful.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::PublicationConflict`] when the row is no
    /// longer pending or was claimed by a newer attempt.
    pub fn resolve_publication(
        &mut self,
        _actor: &StorageActorAuthority,
        claimed: ClaimedPublication,
        outcome: PublicationOutcome,
        resolved_at_utc: &str,
    ) -> Result<PublicationState, RuntimeStoreError> {
        validate_timestamp(resolved_at_utc)?;
        self.verify_retained()?;
        let state = outcome.state();
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin publication resolve transaction", source)
            })?;
        verify_schema_version(&transaction)?;
        let updated = transaction
            .execute(
                "UPDATE knowledge_publication
                 SET state = ?2, resolved_at_utc = ?3
                 WHERE sequence = ?1 AND state = 'pending' AND attempts = ?4",
                params![
                    claimed.sequence,
                    state.as_str(),
                    resolved_at_utc,
                    claimed.claim_attempt,
                ],
            )
            .map_err(|source| {
                RuntimeStoreError::operation("resolve knowledge publication", source)
            })?;
        if updated != 1 {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close publication resolve transaction", source)
            })?;
            return Err(RuntimeStoreError::PublicationConflict);
        }
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit publication resolve transaction", source)
        })?;
        self.verify_retained()?;
        Ok(state)
    }

    /// Reports the state of one publication by idempotency key.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::CorruptDatabase`] for an unparseable state.
    pub fn publication_state(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<PublicationState>, RuntimeStoreError> {
        let connection = self.connection()?;
        let state: Option<String> = connection
            .query_row(
                "SELECT state FROM knowledge_publication WHERE idempotency_key = ?1",
                params![idempotency_key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| {
                RuntimeStoreError::operation("read knowledge publication state", source)
            })?;
        state
            .map(|value| PublicationState::parse(&value))
            .transpose()
    }

    /// Counts publications in each state, for recovery assertions.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::Operation`] when the count query fails.
    pub fn publication_counts(&self) -> Result<PublicationCounts, RuntimeStoreError> {
        let connection = self.connection()?;
        let count = |state: PublicationState| -> Result<u64, RuntimeStoreError> {
            let value: i64 = connection
                .query_row(
                    "SELECT count(*) FROM knowledge_publication WHERE state = ?1",
                    params![state.as_str()],
                    |row| row.get(0),
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("count knowledge publications", source)
                })?;
            u64::try_from(value).map_err(|_| RuntimeStoreError::CorruptDatabase)
        };
        Ok(PublicationCounts {
            pending: count(PublicationState::Pending)?,
            delivered: count(PublicationState::Delivered)?,
            dead_letter: count(PublicationState::DeadLetter)?,
        })
    }

    /// Appends one audit-chain entry and advances the persisted head claim in
    /// the same transaction (B3-DESIGN §5).
    ///
    /// The two writes are atomic on purpose: a head claim that outran its rows,
    /// or rows that outran the claim, would each read as a fault to
    /// [`crate::AuditChain::verify`], so neither may exist even briefly.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::InvalidIntent`] for an over-long field or a
    /// non-RFC3339 timestamp, and [`RuntimeStoreError::CorruptDatabase`] when
    /// the head row is missing or the assigned sequence is not the successor of
    /// the previous head.
    pub(crate) fn append_audit_chain(
        &mut self,
        _actor: &StorageActorAuthority,
        append: &crate::audit_chain::AuditChainAppend<'_>,
    ) -> Result<u64, RuntimeStoreError> {
        validate_timestamp(append.recorded_at_utc)?;
        validate_atom(
            "audit chain namespace",
            append.namespace,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_atom(
            "audit chain type name",
            append.type_name,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_atom(
            "audit chain identity",
            append.identity,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_atom(
            "audit chain previous digest",
            append.prev_digest,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_atom(
            "audit chain payload digest",
            append.payload_digest,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_atom(
            "audit chain link digest",
            append.link_digest,
            MAX_IDENTIFIER_BYTES,
        )?;
        if append.payload_json.is_empty() || append.payload_json.len() > MAX_JSON_BYTES {
            return Err(RuntimeStoreError::InvalidIntent(
                "audit chain payload is empty or exceeds the JSON bound",
            ));
        }
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin audit chain append transaction", source)
            })?;
        verify_schema_version(&transaction)?;
        let head: i64 = transaction
            .query_row(
                "SELECT sequence FROM audit_chain_head WHERE singleton = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| RuntimeStoreError::operation("read audit chain head", source))?
            .unwrap_or(0);
        transaction
            .execute(
                "INSERT INTO audit_chain(
                    prev_digest, namespace, type_name, identity,
                    payload_json, payload_digest, recorded_at_utc, link_digest
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    append.prev_digest,
                    append.namespace,
                    append.type_name,
                    append.identity,
                    append.payload_json,
                    append.payload_digest,
                    append.recorded_at_utc,
                    append.link_digest,
                ],
            )
            .map_err(|source| RuntimeStoreError::operation("append audit chain entry", source))?;
        let sequence = transaction.last_insert_rowid();
        // AUTOINCREMENT never reuses a sequence, so a gap here means rows were
        // removed behind the append-only trigger. Refuse rather than extend a
        // chain whose head claim would then be a lie.
        if sequence != head.saturating_add(1) {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close audit chain append transaction", source)
            })?;
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let updated = transaction
            .execute(
                "UPDATE audit_chain_head SET sequence = ?1, link_digest = ?2 WHERE singleton = 1",
                params![sequence, append.link_digest],
            )
            .map_err(|source| RuntimeStoreError::operation("advance audit chain head", source))?;
        // The first append creates the head row. `singleton INTEGER PRIMARY KEY
        // CHECK (singleton = 1)` makes a second row unrepresentable, so this
        // cannot race into two heads.
        let updated = if updated == 0 {
            transaction
                .execute(
                    "INSERT INTO audit_chain_head(singleton, sequence, link_digest)
                     VALUES (1, ?1, ?2)",
                    params![sequence, append.link_digest],
                )
                .map_err(|source| RuntimeStoreError::operation("open audit chain head", source))?
        } else {
            updated
        };
        if updated != 1 {
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close audit chain append transaction", source)
            })?;
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        transaction.commit().map_err(|source| {
            RuntimeStoreError::operation("commit audit chain append transaction", source)
        })?;
        self.verify_retained()?;
        u64::try_from(sequence).map_err(|_| RuntimeStoreError::CorruptDatabase)
    }

    /// Reads the persisted head claim as `(sequence, link_digest)`.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::CorruptDatabase`] when the singleton head
    /// row is absent or negative.
    pub(crate) fn audit_chain_head(&self) -> Result<(u64, String), RuntimeStoreError> {
        let connection = self.connection()?;
        let head: Option<(i64, String)> = connection
            .query_row(
                "SELECT sequence, link_digest FROM audit_chain_head WHERE singleton = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|source| RuntimeStoreError::operation("read audit chain head", source))?;
        let Some((sequence, digest)) = head else {
            return Ok((0, crate::audit_chain::GENESIS_DIGEST_HEX.to_owned()));
        };
        Ok((
            u64::try_from(sequence).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            digest,
        ))
    }

    /// Reads at most `limit` entries with `sequence >= from_sequence`.
    ///
    /// Bounded by [`MAX_AUDIT_CHAIN_PAGE`] so a verification pass over a long
    /// chain streams in pages instead of materializing the whole table.
    ///
    /// # Errors
    /// Returns [`RuntimeStoreError::InvalidIntent`] for a zero or over-large
    /// page, and [`RuntimeStoreError::CorruptDatabase`] for an unreadable row.
    pub(crate) fn audit_chain_rows(
        &self,
        from_sequence: u64,
        limit: usize,
    ) -> Result<Vec<crate::audit_chain::StoredAuditRow>, RuntimeStoreError> {
        if limit == 0 || limit > MAX_AUDIT_CHAIN_PAGE {
            return Err(RuntimeStoreError::InvalidIntent(
                "audit chain page is zero or exceeds the page bound",
            ));
        }
        let from = i64::try_from(from_sequence).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let limit = i64::try_from(limit).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let connection = self.connection()?;
        let mut statement = connection
            .prepare(
                "SELECT sequence, prev_digest, namespace, type_name, identity,
                        payload_json, payload_digest, recorded_at_utc, link_digest
                 FROM audit_chain WHERE sequence >= ?1 ORDER BY sequence LIMIT ?2",
            )
            .map_err(|source| RuntimeStoreError::operation("prepare audit chain scan", source))?;
        let rows = statement
            .query_map(params![from, limit], |row| {
                Ok(crate::audit_chain::StoredAuditRow {
                    sequence: row.get::<_, i64>(0)?,
                    prev_digest: row.get(1)?,
                    namespace: row.get(2)?,
                    type_name: row.get(3)?,
                    identity: row.get(4)?,
                    payload_json: row.get(5)?,
                    payload_digest: row.get(6)?,
                    recorded_at_utc: row.get(7)?,
                    link_digest: row.get(8)?,
                })
            })
            .map_err(|source| RuntimeStoreError::operation("query audit chain scan", source))?;
        let mut collected = Vec::new();
        for row in rows {
            collected.push(
                row.map_err(|source| RuntimeStoreError::operation("decode audit chain", source))?,
            );
        }
        Ok(collected)
    }

    pub fn resolve_effect(
        &mut self,
        idempotency_key: &str,
        resolution: EffectResolution,
        updated_at_utc: &str,
    ) -> Result<OutboxEffect, RuntimeStoreError> {
        self.resolve_effect_inner(idempotency_key, None, resolution, updated_at_utc)
    }

    /// Resolves only the exact process attempt represented by `claimed`.
    /// A stale claim can neither terminate nor confirm a newer retry attempt.
    #[cfg(unix)]
    pub(crate) fn resolve_claimed_process(
        &mut self,
        terminal: &ProcessTerminalClaim,
        resolution: EffectResolution,
        updated_at_utc: &str,
    ) -> Result<OutboxEffect, RuntimeStoreError> {
        let claimed = terminal.claimed();
        self.resolve_effect_inner(
            claimed.idempotency_key(),
            Some((claimed, terminal.fingerprint())),
            resolution,
            updated_at_utc,
        )
    }

    fn resolve_effect_inner(
        &mut self,
        idempotency_key: &str,
        expected_process_claim: Option<(&ClaimedOutboxEffect, ProcessRequestFingerprint)>,
        resolution: EffectResolution,
        updated_at_utc: &str,
    ) -> Result<OutboxEffect, RuntimeStoreError> {
        validate_atom(
            "outbox idempotency key",
            idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_timestamp(updated_at_utc)?;
        if !resolution.evidence_matches_resolution() {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        #[cfg(test)]
        if expected_process_claim.is_some()
            && self.process_resolution_fault == ProcessResolutionFault::BeforeRetry
        {
            self.process_resolution_fault = ProcessResolutionFault::None;
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        let evidence = resolution.evidence();
        let evidence_json = evidence.canonical_json()?;
        self.verify_retained()?;
        let transaction = self
            .connection_mut()?
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| RuntimeStoreError::operation("begin outbox resolution", source))?;
        verify_schema_version(&transaction)?;
        let current = load_effect(&transaction, idempotency_key)?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let process_intent = load_process_intent(&transaction, idempotency_key)?;
        let has_process_intent = process_intent.is_some();
        if expected_process_claim.is_none() && has_process_intent {
            return Err(RuntimeStoreError::ProcessIntentRequired);
        }
        if expected_process_claim.is_some() && !has_process_intent {
            return Err(RuntimeStoreError::ProcessIntentRequired);
        }
        if expected_process_claim.is_some_and(|(claimed, fingerprint)| {
            current.attempts != claimed.claim_attempt
                || !claimed_process_matches_effect(claimed, &current)
                || process_intent.as_ref().is_none_or(|intent| {
                    intent.request_sha256 != ProcessIntentEnvelope::new(fingerprint).request_sha256
                })
        }) {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        let target = resolution.state();
        if let Some((authorization, action)) = resolution.retry_binding() {
            if !authorization.matches_effect(
                idempotency_key,
                current.attempts,
                current.state,
                action,
                target,
            ) || retry_authorization_consumed(&transaction, &authorization.decision_id)?
            {
                return Err(RuntimeStoreError::RetryAuthorizationRejected);
            }
        }
        if current.state == target {
            let same_observation =
                load_latest_observation(&transaction, idempotency_key, current.attempts)?
                    .is_some_and(|observation| {
                        observation.state == target
                            && observation.evidence == evidence
                            && observation.observed_at_utc == updated_at_utc
                    });
            transaction.rollback().map_err(|source| {
                RuntimeStoreError::operation("close repeated resolution", source)
            })?;
            return same_observation
                .then_some(current)
                .ok_or(RuntimeStoreError::InvalidOutboxTransition);
        }
        if !valid_outbox_transition(current.state, &resolution) {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        if resolution.requires_execution_identity() && current.execution_identity.is_none() {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        if matches!(
            &resolution,
            EffectResolution::ObservedAbsent(value)
                if value.code != EffectEvidenceCode::ObservedAbsent
        ) {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        let observation_sequence: i64 = transaction
            .query_row(
                "SELECT coalesce(max(observation_sequence), 0) + 1
                 FROM outbox_attempt_observation",
                [],
                |row| row.get(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("allocate outbox observation sequence", source)
            })?;
        transaction
            .execute(
                "INSERT INTO outbox_attempt_observation (
                    observation_sequence, idempotency_key, attempt, evidence_schema_version, observed_state,
                    evidence_code, evidence_json, observed_at_utc
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    observation_sequence,
                    idempotency_key,
                    current.attempts,
                    RECORD_SCHEMA_VERSION,
                    target.as_str(),
                    evidence.code.as_str(),
                    evidence_json,
                    updated_at_utc,
                ],
            )
            .map_err(|source| RuntimeStoreError::operation("append outbox observation", source))?;
        if let Some((authorization, _)) = resolution.retry_binding() {
            consume_retry_authorization(
                &transaction,
                authorization,
                observation_sequence,
                updated_at_utc,
            )?;
        }
        let changed = transaction
            .execute(
                "UPDATE outbox SET state = ?1, updated_at_utc = ?2
                 WHERE idempotency_key = ?3 AND state = ?4",
                params![
                    target.as_str(),
                    updated_at_utc,
                    idempotency_key,
                    current.state.as_str(),
                ],
            )
            .map_err(|source| RuntimeStoreError::operation("resolve outbox effect", source))?;
        if changed != 1 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let effect = load_effect(&transaction, idempotency_key)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        transaction
            .commit()
            .map_err(|source| RuntimeStoreError::operation("commit outbox resolution", source))?;
        #[cfg(test)]
        if expected_process_claim.is_some()
            && self.process_resolution_fault == ProcessResolutionFault::AfterCommit
        {
            self.process_resolution_fault = ProcessResolutionFault::BeforeRetry;
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        self.verify_retained()?;
        Ok(effect)
    }

    /// Read-only exact confirmation used after a resolution call returned an
    /// error at a boundary where SQLite may nevertheless have committed.
    pub(crate) fn claimed_process_resolution_is_durable(
        &self,
        terminal: &ProcessTerminalClaim,
        resolution: &EffectResolution,
        observed_at_utc: &str,
    ) -> Result<bool, RuntimeStoreError> {
        let claimed = terminal.claimed();
        validate_atom(
            "outbox idempotency key",
            claimed.idempotency_key(),
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_timestamp(observed_at_utc)?;
        if !resolution.evidence_matches_resolution() {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let current = load_effect(connection, claimed.idempotency_key())?
            .ok_or(RuntimeStoreError::InvalidOutboxTransition)?;
        let process_intent = load_process_intent(connection, claimed.idempotency_key())?
            .ok_or(RuntimeStoreError::ProcessIntentRequired)?;
        if current.attempts != claimed.claim_attempt
            || !claimed_process_matches_effect(claimed, &current)
            || process_intent.request_sha256
                != ProcessIntentEnvelope::new(terminal.fingerprint()).request_sha256
            || current.state != resolution.state()
        {
            return Ok(false);
        }
        let evidence = resolution.evidence();
        Ok(
            load_latest_observation(connection, claimed.idempotency_key(), claimed.claim_attempt)?
                .is_some_and(|observation| {
                    observation.state == current.state
                        && observation.evidence == evidence
                        && observation.observed_at_utc == observed_at_utc
                }),
        )
    }

    /// Reads immutable resolution and retry evidence in append order.
    pub fn attempt_history(
        &self,
        idempotency_key: &str,
        limit: usize,
    ) -> Result<Vec<EffectObservation>, RuntimeStoreError> {
        validate_atom(
            "outbox idempotency key",
            idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        validate_query_limit(limit)?;
        self.verify_retained()?;
        let mut statement = self
            .connection()?
            .prepare(
                "SELECT observation_sequence, attempt, evidence_schema_version, observed_state,
                        evidence_code, evidence_json, observed_at_utc
                 FROM outbox_attempt_observation
                 WHERE idempotency_key = ?1
                 ORDER BY observation_sequence LIMIT ?2",
            )
            .map_err(|source| RuntimeStoreError::operation("prepare attempt history", source))?;
        let rows = statement
            .query_map(params![idempotency_key, limit as i64], observation_from_row)
            .map_err(|source| RuntimeStoreError::operation("query attempt history", source))?;
        let mut history = Vec::new();
        for row in rows {
            history.push(row.map_err(|source| {
                RuntimeStoreError::operation("decode attempt history", source)
            })??);
        }
        Ok(history)
    }

    #[cfg(unix)]
    fn recorded_process_identities(
        &self,
        limit: usize,
    ) -> Result<Vec<ProcessExecutionIdentity>, RuntimeStoreError> {
        validate_query_limit(limit)?;
        self.verify_retained()?;
        let mut statement = self
            .connection()?
            .prepare(
                "SELECT attempt, pid, process_group_id, process_start_identity, recorded_at_utc
                 FROM outbox_execution_identity
                 ORDER BY idempotency_key, attempt LIMIT ?1",
            )
            .map_err(|source| {
                RuntimeStoreError::operation("prepare recorded process identities", source)
            })?;
        let rows = statement
            .query_map([limit as i64], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|source| {
                RuntimeStoreError::operation("query recorded process identities", source)
            })?;
        let mut identities = Vec::new();
        for row in rows {
            let (attempt, pid, process_group_id, start, recorded_at) = row.map_err(|source| {
                RuntimeStoreError::operation("decode recorded process identity", source)
            })?;
            identities.push(ProcessExecutionIdentity::new(
                u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                u32::try_from(pid).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                u32::try_from(process_group_id).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                start,
                recorded_at,
            )?);
        }
        Ok(identities)
    }

    fn effects_in_states(
        &self,
        states: &[OutboxState],
        limit: usize,
    ) -> Result<Vec<OutboxEffect>, RuntimeStoreError> {
        validate_query_limit(limit)?;
        self.verify_retained()?;
        let connection = self.connection()?;
        verify_schema_version(connection)?;
        let mut effects = Vec::new();
        for state in states {
            let remaining = limit.saturating_sub(effects.len());
            if remaining == 0 {
                break;
            }
            let mut statement = connection
                .prepare(
                    "SELECT outbox.idempotency_key, outbox.journal_sequence,
                            outbox.mission_id, outbox.phase_id, outbox.effect_kind,
                            outbox.operation_slot, outbox.logical_attempt,
                            outbox.payload_json, outbox.state,
                            outbox.attempts, outbox.updated_at_utc,
                            identity.attempt, identity.pid, identity.process_group_id,
                            identity.process_start_identity, identity.recorded_at_utc,
                            journal.record_schema_version, process_intent.intent_json
                     FROM outbox
                     JOIN journal ON journal.sequence = outbox.journal_sequence
                     LEFT JOIN outbox_execution_identity AS identity
                       ON identity.idempotency_key = outbox.idempotency_key
                      AND identity.attempt = outbox.attempts
                     LEFT JOIN outbox_process_intent AS process_intent
                       ON process_intent.idempotency_key = outbox.idempotency_key
                     WHERE outbox.state = ?1
                     ORDER BY outbox.journal_sequence, outbox.idempotency_key LIMIT ?2",
                )
                .map_err(|source| RuntimeStoreError::operation("prepare outbox query", source))?;
            let rows = statement
                .query_map(params![state.as_str(), remaining as i64], effect_from_row)
                .map_err(|source| RuntimeStoreError::operation("query outbox effects", source))?;
            for row in rows {
                effects.push(row.map_err(|source| {
                    RuntimeStoreError::operation("decode outbox effect", source)
                })??);
            }
        }
        effects.sort_by(|left, right| {
            (left.journal_sequence, left.idempotency_key.as_str())
                .cmp(&(right.journal_sequence, right.idempotency_key.as_str()))
        });
        effects.truncate(limit);
        Ok(effects)
    }

    fn connection(&self) -> Result<&Connection, RuntimeStoreError> {
        self.connection
            .as_ref()
            .ok_or(RuntimeStoreError::CorruptDatabase)
    }

    fn connection_mut(&mut self) -> Result<&mut Connection, RuntimeStoreError> {
        self.connection
            .as_mut()
            .ok_or(RuntimeStoreError::CorruptDatabase)
    }

    fn verify_retained(&self) -> Result<(), RuntimeStoreError> {
        self.boundary.verify()?;
        verify_boundary_seal(
            &self.boundary,
            self.boundary_seal_identity,
            self.boundary_kind,
        )?;
        if verify_private_file(self.boundary.directory(), DATABASE_FILE)? != self.database_identity
        {
            return Err(RuntimeStoreError::UnsafeEntry);
        }
        if verify_optional_sidecar(self.boundary.directory(), DATABASE_WAL_FILE)?
            != self.wal_identity
            || verify_optional_sidecar(self.boundary.directory(), DATABASE_SHM_FILE)?
                != self.shm_identity
        {
            return Err(RuntimeStoreError::UnsafeEntry);
        }
        Ok(())
    }
}

impl PrivateProcessLedgerStore {
    pub(crate) fn close(self) -> Result<(), RuntimeStoreError> {
        self.inner.close()
    }

    /// [`close`](Self::close) through a mutable borrow, for an owner that
    /// closes the ledger from its own [`Drop`].
    pub(crate) fn close_in_place(&mut self) -> Result<(), RuntimeStoreError> {
        self.inner.close_in_place()
    }

    /// Appends one mission-scoped private record. The store injects its sealed
    /// marker before exact-retry comparison and checksum construction.
    pub(crate) fn append(
        &mut self,
        intent: &JournalIntent,
    ) -> Result<JournalCommit, RuntimeStoreError> {
        self.inner.append_private(intent)
    }

    pub(crate) fn runtime_store_mut(&mut self) -> &mut RuntimeStore {
        &mut self.inner
    }

    pub(crate) fn transition_payload(
        &self,
        transition_id: &str,
    ) -> Result<Option<Value>, RuntimeStoreError> {
        self.inner.verify_retained()?;
        let payload: Option<String> = self
            .inner
            .connection()?
            .query_row(
                "SELECT payload_json FROM journal WHERE transition_id = ?1",
                [transition_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| {
                RuntimeStoreError::operation("read private transition payload", source)
            })?;
        payload
            .map(|value| {
                serde_json::from_str(&value).map_err(|_| RuntimeStoreError::CorruptDatabase)
            })
            .transpose()
    }

    pub(crate) fn transition_committed_at_utc(
        &self,
        transition_id: &str,
    ) -> Result<Option<String>, RuntimeStoreError> {
        self.inner.verify_retained()?;
        let committed_at_utc = self
            .inner
            .connection()?
            .query_row(
                "SELECT committed_at_utc FROM journal WHERE transition_id = ?1",
                [transition_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|source| {
                RuntimeStoreError::operation("read existing transition timestamp", source)
            })?;
        if let Some(committed_at_utc) = committed_at_utc.as_deref() {
            validate_timestamp(committed_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        }
        Ok(committed_at_utc)
    }

    /// Records one typed reasoning write against this private ledger.
    pub(crate) fn record_reasoning(
        &mut self,
        write: &ReasoningWrite,
    ) -> Result<JournalCommit, RuntimeStoreError> {
        self.inner.record_reasoning(write)
    }

    /// Counts reasoning projection rows for one mission.
    pub(crate) fn reasoning_counts(
        &self,
        mission_id: &MissionId,
    ) -> Result<ReasoningCounts, RuntimeStoreError> {
        self.inner.reasoning_counts(mission_id)
    }

    /// Lists every `(table, column)` across the reasoning projection tables.
    pub(crate) fn reasoning_column_inventory(
        &self,
    ) -> Result<Vec<(String, String)>, RuntimeStoreError> {
        self.inner.reasoning_column_inventory()
    }

    /// Every stored reasoning text cell for a mission.
    pub(crate) fn reasoning_text_cells(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        self.inner.reasoning_text_cells(mission_id)
    }

    pub(crate) fn pending_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<OutboxEffect>, RuntimeStoreError> {
        self.inner.pending_effects(limit)
    }

    pub(crate) fn recovery_effects(
        &self,
        limit: usize,
    ) -> Result<Vec<OutboxEffect>, RuntimeStoreError> {
        self.inner.recovery_effects(limit)
    }

    // --- Untyped effect lifecycle (B4-DESIGN §1.4) -------------------------
    //
    // Pure delegations to the same-named methods on the wrapped store. They
    // exist because a governed Git effect is Rust-private state with no
    // Go-visible projection, which is precisely what this ledger is for: the
    // compatibility boundary refuses a transition that declares no projection,
    // and a git fetch, branch, or push projects nothing Go reads.

    pub(crate) fn claim_exact(
        &mut self,
        expected_idempotency_key: &str,
        updated_at_utc: &str,
    ) -> Result<ClaimedOutboxEffect, RuntimeStoreError> {
        self.inner
            .claim_exact(expected_idempotency_key, updated_at_utc)
    }

    pub(crate) fn record_execution_identity(
        &mut self,
        idempotency_key: &str,
        identity: &ProcessExecutionIdentity,
    ) -> Result<OutboxEffect, RuntimeStoreError> {
        self.inner
            .record_execution_identity(idempotency_key, identity)
    }

    pub(crate) fn resolve_effect(
        &mut self,
        idempotency_key: &str,
        resolution: EffectResolution,
        updated_at_utc: &str,
    ) -> Result<OutboxEffect, RuntimeStoreError> {
        self.inner
            .resolve_effect(idempotency_key, resolution, updated_at_utc)
    }

    // `attempt_history` is already forwarded below, beside the typed process
    // lifecycle; the Git lane reads the same durable observations.

    pub(crate) fn exact_outbox_snapshot(
        &mut self,
        idempotency_key: &str,
    ) -> Result<Option<ExactOutboxSnapshot>, RuntimeStoreError> {
        self.inner.exact_outbox_snapshot(idempotency_key)
    }

    #[cfg(unix)]
    pub(crate) fn process_metric_identity(
        &self,
        binding: &ProcessEffectBinding,
        request: &ProcessRequest,
    ) -> Result<(Option<ProcessExecutionIdentity>, bool), RuntimeStoreError> {
        self.inner.process_metric_identity(binding, request)
    }

    pub(crate) fn exact_logical_outbox_snapshot(
        &mut self,
        mission_id: &MissionId,
        phase_id: Option<&str>,
        effect_kind: OutboxEffectKind,
        operation_slot: &EffectOperationSlot,
        logical_attempt: u32,
    ) -> Result<Option<ExactOutboxSnapshot>, RuntimeStoreError> {
        self.inner.exact_logical_outbox_snapshot(
            mission_id,
            phase_id,
            effect_kind,
            operation_slot,
            logical_attempt,
        )
    }

    #[cfg(unix)]
    pub(crate) fn prepare_exact_process_claim(
        &self,
        binding: &ProcessEffectBinding,
        request: &ProcessRequest,
    ) -> Result<PendingProcessClaim, RuntimeStoreError> {
        self.inner.prepare_exact_process_claim(binding, request)
    }

    #[cfg(unix)]
    pub(crate) fn claim_prepared_process(
        &mut self,
        prepared: PendingProcessClaim,
        updated_at_utc: &str,
    ) -> Result<ClaimedProcessAttempt, ExactProcessClaimError> {
        self.inner.claim_prepared_process(prepared, updated_at_utc)
    }

    #[cfg(unix)]
    pub(crate) fn recover_exact_process_attempt(
        &self,
        binding: &ProcessEffectBinding,
        request: &ProcessRequest,
    ) -> Result<RecoveredProcessAttempt, RuntimeStoreError> {
        self.inner.recover_exact_process_attempt(binding, request)
    }

    #[cfg(unix)]
    pub(crate) fn record_process_spawn_permit(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        permitted_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        self.inner
            .record_process_spawn_permit(attempt, request, permitted_at_utc)
    }

    #[cfg(unix)]
    pub(crate) fn record_blocked_process_identity(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        gate_request: &ProcessStartGateRequest,
        identity: &ProcessExecutionIdentity,
    ) -> Result<(), RuntimeStoreError> {
        self.inner
            .record_blocked_process_identity(attempt, request, gate_request, identity)
    }

    #[cfg(unix)]
    pub(crate) fn resolve_recovered_before_spawn(
        &mut self,
        recovered: RecoveredProcessAttempt,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        self.inner
            .resolve_recovered_before_spawn(recovered, observed_at_utc)
    }

    #[cfg(unix)]
    pub(crate) fn resolve_recovered_process_group_absent(
        &mut self,
        cleanup: RecoveredProcessCleanup,
        absence: &ExactProcessGroupAbsence,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        self.inner
            .resolve_recovered_process_group_absent(cleanup, absence, observed_at_utc)
    }

    #[cfg(unix)]
    pub(crate) fn authorize_process_release(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        gate_request: &ProcessStartGateRequest,
        identity: &ProcessExecutionIdentity,
        authorized_at_utc: &str,
    ) -> Result<ProcessReleaseAuthorization, RuntimeStoreError> {
        self.inner.authorize_process_release(
            attempt,
            request,
            gate_request,
            identity,
            authorized_at_utc,
        )
    }

    #[cfg(unix)]
    pub(crate) fn record_process_started_observed(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        authorized: &AuthorizedLauncherIdentity,
        receipt: &ProcessStartedReceipt,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        self.inner.record_process_started_observed(
            attempt,
            request,
            authorized,
            receipt,
            observed_at_utc,
        )
    }

    #[cfg(all(test, unix))]
    pub(crate) fn record_process_started_observed_for_test(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        authorization: &ProcessReleaseAuthorization,
        observed_at_utc: &str,
    ) -> Result<(), RuntimeStoreError> {
        self.inner.record_process_started_observed_for_test(
            attempt,
            request,
            authorization,
            observed_at_utc,
        )
    }

    #[cfg(all(test, unix))]
    pub(crate) fn authorize_process_release_for_test(
        &mut self,
        attempt: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        identity: &ProcessExecutionIdentity,
        authorized_at_utc: &str,
    ) -> Result<ProcessReleaseAuthorization, RuntimeStoreError> {
        self.inner
            .authorize_process_release_for_test(attempt, request, identity, authorized_at_utc)
    }

    pub(crate) fn process_release_was_authorized(
        &self,
        idempotency_key: &str,
        attempt: u32,
    ) -> Result<bool, RuntimeStoreError> {
        self.inner
            .process_release_was_authorized(idempotency_key, attempt)
    }

    #[cfg(unix)]
    pub(crate) fn resolve_claimed_process(
        &mut self,
        terminal: &ProcessTerminalClaim,
        resolution: EffectResolution,
        updated_at_utc: &str,
    ) -> Result<OutboxEffect, RuntimeStoreError> {
        self.inner
            .resolve_claimed_process(terminal, resolution, updated_at_utc)
    }

    pub(crate) fn claimed_process_resolution_is_durable(
        &self,
        terminal: &ProcessTerminalClaim,
        resolution: &EffectResolution,
        observed_at_utc: &str,
    ) -> Result<bool, RuntimeStoreError> {
        self.inner
            .claimed_process_resolution_is_durable(terminal, resolution, observed_at_utc)
    }

    pub(crate) fn attempt_history(
        &self,
        idempotency_key: &str,
        limit: usize,
    ) -> Result<Vec<EffectObservation>, RuntimeStoreError> {
        self.inner.attempt_history(idempotency_key, limit)
    }

    #[cfg(unix)]
    pub(crate) fn recorded_process_identities(
        &self,
        limit: usize,
    ) -> Result<Vec<ProcessExecutionIdentity>, RuntimeStoreError> {
        self.inner.recorded_process_identities(limit)
    }

    pub(crate) fn record_terminal_decision(
        &mut self,
        record: &TerminalDecisionRecord,
    ) -> Result<(), RuntimeStoreError> {
        self.inner.record_terminal_decision_private(record)
    }

    pub(crate) fn terminal_decision(
        &self,
        mission_id: &MissionId,
        phase_id: &str,
        worker_id: &str,
        attempt: u32,
    ) -> Result<Option<TerminalDecisionRecord>, RuntimeStoreError> {
        self.inner
            .terminal_decision_private(mission_id, phase_id, worker_id, attempt)
    }

    pub(crate) fn record_continuation_handle(
        &mut self,
        record: &ContinuationHandleRecord,
    ) -> Result<(), RuntimeStoreError> {
        self.inner.record_continuation_handle_private(record)
    }

    pub(crate) fn continuation_handle(
        &self,
        mission_id: &MissionId,
        phase_id: &str,
        attempt: u32,
    ) -> Result<Option<ContinuationHandleRecord>, RuntimeStoreError> {
        self.inner
            .continuation_handle_private(mission_id, phase_id, attempt)
    }

    pub(crate) fn record_continuation_decision(
        &mut self,
        record: &ContinuationDecisionRecord,
    ) -> Result<(), RuntimeStoreError> {
        self.inner.record_continuation_decision_private(record)
    }

    pub(crate) fn continuation_decisions(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<ContinuationDecisionRecord>, RuntimeStoreError> {
        self.inner.continuation_decisions_private(mission_id)
    }

    pub(crate) fn continuation_capsule_text_cells(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        self.inner.continuation_capsule_text_cells(mission_id)
    }

    pub(crate) fn continuation_decision_key_inventory(
        &self,
        mission_id: &MissionId,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        self.inner.continuation_decision_key_inventory(mission_id)
    }

    pub(crate) fn reasoning_attempt_fingerprints(
        &self,
        mission_id: &MissionId,
        phase_id: &str,
    ) -> Result<Vec<String>, RuntimeStoreError> {
        self.inner
            .reasoning_attempt_fingerprints_private(mission_id, phase_id)
    }

    #[cfg(test)]
    pub(crate) fn inject_process_resolution_acknowledgement_loss(&mut self) {
        self.inner.inject_process_resolution_acknowledgement_loss();
    }

    #[cfg(test)]
    fn connection(&self) -> Result<&Connection, RuntimeStoreError> {
        self.inner.connection()
    }
}

struct PreparedIntent {
    transition_id: String,
    mission_id: Option<String>,
    kind: String,
    payload_json: String,
    committed_at_utc: String,
    extra_json: String,
    outbox_fingerprint: String,
    projection_fingerprint: String,
    required_projections: Vec<CompatibilityProjection>,
    outbox: Vec<PreparedOutbox>,
    publications: Vec<PreparedPublication>,
}

/// One publication row, already validated and canonicalized.
struct PreparedPublication {
    idempotency_key: String,
    mission_id: String,
    phase_id: Option<String>,
    logical_attempt: u32,
    namespace: String,
    type_name: String,
    primitive_kind: String,
    schema_version: u32,
    registry_generation: u64,
    sensitivity: String,
    identity: String,
    payload_json: String,
    payload_digest: String,
}

impl PreparedIntent {
    fn new(
        intent: &JournalIntent,
        boundary_kind: StoreBoundaryKind,
    ) -> Result<Self, RuntimeStoreError> {
        match boundary_kind {
            StoreBoundaryKind::Compatibility if is_known_private_transition_kind(&intent.kind) => {
                return Err(RuntimeStoreError::InvalidIntent(
                    "private transition kinds are forbidden in compatibility stores",
                ));
            }
            StoreBoundaryKind::Compatibility if intent.required_projections.is_empty() => {
                return Err(RuntimeStoreError::InvalidIntent(
                    "compatibility transitions require at least one real projection",
                ));
            }
            StoreBoundaryKind::PrivateProcessLedger
                if intent.mission_id.is_none() || !intent.required_projections.is_empty() =>
            {
                return Err(RuntimeStoreError::InvalidIntent(
                    "private process-ledger transitions require a mission and forbid projections",
                ));
            }
            _ => {}
        }
        let payload_json = canonical_json(&intent.payload, "journal payload")?;
        if intent
            .required_projections
            .contains(&CompatibilityProjection::EventLog)
        {
            if intent.mission_id.is_none() {
                return Err(RuntimeStoreError::InvalidIntent(
                    "event-producing transitions require a mission",
                ));
            }
            ExpectedEventProjection::from_journal_payload(&payload_json)?;
        }
        let mut extra = intent.extra.clone();
        if boundary_kind == StoreBoundaryKind::PrivateProcessLedger {
            extra.insert(
                PRIVATE_PROCESS_LEDGER_RECORD_EXTRA_KEY.to_owned(),
                private_process_ledger_record_marker(),
            );
        }
        let extra_json =
            canonical_json(&Value::Object(extra.into_iter().collect()), "record extras")?;
        let mut outbox = intent
            .outbox
            .iter()
            .map(|effect| {
                Ok(PreparedOutbox {
                    idempotency_key: effect.idempotency_key.clone(),
                    mission_id: effect.mission_id.as_str().to_owned(),
                    phase_id: effect.phase_id.clone(),
                    effect_kind: effect.effect_kind.as_str().to_owned(),
                    operation_slot: effect.operation_slot.as_str().to_owned(),
                    logical_attempt: effect.logical_attempt,
                    payload_json: canonical_json(&effect.payload, "outbox payload")?,
                    process_request_sha256: effect
                        .process_intent
                        .as_ref()
                        .map(|intent| intent.request_sha256.clone()),
                    process_intent_json: effect
                        .process_intent
                        .as_ref()
                        .map(ProcessIntentEnvelope::canonical_json)
                        .transpose()?,
                })
            })
            .collect::<Result<Vec<_>, RuntimeStoreError>>()?;
        outbox.sort_by(|left, right| left.idempotency_key.cmp(&right.idempotency_key));
        let outbox_fingerprint = outbox_fingerprint(&outbox)?;
        let required_projections = intent
            .required_projections
            .iter()
            .copied()
            .collect::<Vec<_>>();
        let projection_fingerprint = projection_fingerprint(&required_projections);
        Ok(Self {
            transition_id: intent.transition_id.clone(),
            mission_id: intent
                .mission_id
                .as_ref()
                .map(|value| value.as_str().to_owned()),
            kind: intent.kind.clone(),
            payload_json,
            committed_at_utc: intent.committed_at_utc.clone(),
            extra_json,
            outbox_fingerprint,
            projection_fingerprint,
            required_projections,
            outbox,
            publications: intent
                .publications
                .iter()
                .map(|publication| PreparedPublication {
                    idempotency_key: publication.idempotency_key.clone(),
                    mission_id: publication.mission_id.as_str().to_owned(),
                    phase_id: publication.phase_id.clone(),
                    logical_attempt: publication.logical_attempt,
                    namespace: publication.namespace.clone(),
                    type_name: publication.type_name.clone(),
                    primitive_kind: publication.primitive_kind.clone(),
                    schema_version: publication.schema_version,
                    registry_generation: publication.registry_generation,
                    sensitivity: publication.sensitivity.clone(),
                    identity: publication.identity.clone(),
                    payload_json: publication.payload_json.clone(),
                    payload_digest: publication.payload_digest.clone(),
                })
                .collect(),
        })
    }
}

#[derive(Serialize)]
struct PreparedOutbox {
    idempotency_key: String,
    mission_id: String,
    phase_id: Option<String>,
    effect_kind: String,
    operation_slot: String,
    logical_attempt: u32,
    payload_json: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    process_request_sha256: Option<String>,
    #[serde(skip)]
    process_intent_json: Option<String>,
}

#[derive(Serialize)]
struct LegacyPreparedOutbox<'a> {
    idempotency_key: &'a str,
    mission_id: &'a str,
    phase_id: Option<&'a str>,
    effect_kind: &'a str,
    logical_attempt: u32,
    payload_json: &'a str,
}

struct ExistingTransition {
    sequence: i64,
    checksum: String,
    mission_id: Option<String>,
    kind: String,
    payload_json: String,
    committed_at_utc: String,
    extra_json: String,
    outbox_fingerprint: String,
    projection_fingerprint: String,
}

impl ExistingTransition {
    fn matches(&self, prepared: &PreparedIntent) -> bool {
        // `prepared.extra_json` is the caller-supplied material before any
        // recipe is sealed; strip a previously-sealed recipe from the
        // stored side before comparing so a real retry is never rejected
        // because the server, not the caller, added that key.
        self.mission_id == prepared.mission_id
            && self.kind == prepared.kind
            && self.payload_json == prepared.payload_json
            && self.committed_at_utc == prepared.committed_at_utc
            && extra_json_without_recipe(&self.extra_json).as_deref()
                == Some(prepared.extra_json.as_str())
            && self.outbox_fingerprint == prepared.outbox_fingerprint
            && self.projection_fingerprint == prepared.projection_fingerprint
    }
}

fn store_leases() -> &'static Mutex<BTreeSet<FileIdentity>> {
    STORE_LEASES.get_or_init(|| Mutex::new(BTreeSet::new()))
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn inspect_optional_database_file(
    boundary: &ProductionBoundary,
) -> Result<Option<(FileIdentity, u64)>, RuntimeStoreError> {
    let file = match open_file_nofollow(boundary.directory(), Path::new(DATABASE_FILE)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(RuntimeStoreError::operation(
                "inspect runtime database entry",
                source,
            ));
        }
    };
    let metadata = file.metadata().map_err(|source| {
        RuntimeStoreError::operation("inspect runtime database metadata", source)
    })?;
    if !metadata.is_file() || mode(&metadata) != 0o600 || link_count(&metadata) != 1 {
        return Err(RuntimeStoreError::UnsafeEntry);
    }
    Ok(Some((identity(&metadata), metadata.len())))
}

fn inspect_boundary_seal(
    boundary: &ProductionBoundary,
    expected: StoreBoundaryKind,
) -> Result<Option<FileIdentity>, RuntimeStoreError> {
    if inspect_optional_boundary_marker(boundary, LEGACY_BOUNDARY_SEAL_FILE)?.is_some() {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let compatibility =
        inspect_optional_boundary_marker(boundary, COMPATIBILITY_BOUNDARY_SEAL_FILE)?;
    let private =
        inspect_optional_boundary_marker(boundary, PRIVATE_PROCESS_LEDGER_BOUNDARY_SEAL_FILE)?;
    match (compatibility, private) {
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(RuntimeStoreError::CorruptDatabase),
        (Some(identity), None) if expected == StoreBoundaryKind::Compatibility => {
            Ok(Some(identity))
        }
        (None, Some(identity)) if expected == StoreBoundaryKind::PrivateProcessLedger => {
            Ok(Some(identity))
        }
        (Some(_), None) | (None, Some(_)) => Err(RuntimeStoreError::BoundaryKindMismatch),
    }
}

fn inspect_optional_boundary_marker(
    boundary: &ProductionBoundary,
    name: &str,
) -> Result<Option<FileIdentity>, RuntimeStoreError> {
    let file = match open_file_nofollow(boundary.directory(), Path::new(name)) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RuntimeStoreError::UnsafeEntry),
    };
    let metadata_before = file.metadata().map_err(|source| {
        RuntimeStoreError::operation("inspect runtime boundary marker metadata", source)
    })?;
    if !metadata_before.is_file()
        || mode(&metadata_before) != 0o600
        || link_count(&metadata_before) != 1
    {
        return Err(RuntimeStoreError::UnsafeEntry);
    }
    if metadata_before.len() != 0 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let metadata_after = file.metadata().map_err(|source| {
        RuntimeStoreError::operation("reinspect runtime boundary marker metadata", source)
    })?;
    let marker_identity = identity(&metadata_before);
    if identity(&metadata_after) != marker_identity
        || metadata_after.len() != 0
        || mode(&metadata_after) != 0o600
        || link_count(&metadata_after) != 1
    {
        return Err(RuntimeStoreError::UnsafeEntry);
    }
    Ok(Some(marker_identity))
}

fn create_boundary_seal(
    boundary: &ProductionBoundary,
    expected: StoreBoundaryKind,
) -> Result<FileIdentity, RuntimeStoreError> {
    match create_private_file(
        boundary.directory(),
        Path::new(expected.boundary_seal_file()),
        b"",
        false,
    ) {
        Ok(()) => sync_dir(boundary.directory()).map_err(|source| {
            RuntimeStoreError::operation("sync runtime boundary seal directory", source)
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(RuntimeStoreError::operation(
                "create runtime boundary seal",
                source,
            ));
        }
    }
    let Some(seal_identity) = inspect_boundary_seal(boundary, expected)? else {
        return Err(RuntimeStoreError::UnsafeEntry);
    };
    Ok(seal_identity)
}

fn verify_boundary_seal(
    boundary: &ProductionBoundary,
    expected_identity: FileIdentity,
    expected_kind: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    let Some(found_identity) = inspect_boundary_seal(boundary, expected_kind)? else {
        return Err(RuntimeStoreError::UnsafeEntry);
    };
    if found_identity != expected_identity {
        return Err(RuntimeStoreError::UnsafeEntry);
    }
    Ok(())
}

fn ensure_database_file(boundary: &ProductionBoundary) -> Result<(), RuntimeStoreError> {
    match create_private_file(boundary.directory(), Path::new(DATABASE_FILE), b"", false) {
        Ok(()) => sync_dir(boundary.directory()).map_err(|source| {
            RuntimeStoreError::operation("sync runtime database directory", source)
        }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(RuntimeStoreError::operation(
            "create runtime database",
            source,
        )),
    }
}

fn verify_private_file(
    directory: &cap_std::fs::Dir,
    name: &str,
) -> Result<FileIdentity, RuntimeStoreError> {
    let file = open_file_nofollow(directory, Path::new(name))
        .map_err(|source| RuntimeStoreError::operation("inspect runtime database entry", source))?;
    let metadata = file.metadata().map_err(|source| {
        RuntimeStoreError::operation("inspect runtime database metadata", source)
    })?;
    if !metadata.is_file() || mode(&metadata) != 0o600 || link_count(&metadata) != 1 {
        return Err(RuntimeStoreError::UnsafeEntry);
    }
    Ok(identity(&metadata))
}

fn verify_optional_sidecar(
    directory: &cap_std::fs::Dir,
    name: &str,
) -> Result<Option<FileIdentity>, RuntimeStoreError> {
    match open_file_nofollow(directory, Path::new(name)) {
        Ok(file) => {
            let metadata = file.metadata().map_err(|source| {
                RuntimeStoreError::operation("inspect runtime database sidecar", source)
            })?;
            if !metadata.is_file() || mode(&metadata) != 0o600 || link_count(&metadata) != 1 {
                return Err(RuntimeStoreError::UnsafeEntry);
            }
            Ok(Some(identity(&metadata)))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(RuntimeStoreError::UnsafeEntry),
    }
}

fn inspect_optional_sidecars(
    boundary: &ProductionBoundary,
) -> Result<SidecarIdentities, RuntimeStoreError> {
    Ok(SidecarIdentities {
        wal: verify_optional_sidecar(boundary.directory(), DATABASE_WAL_FILE)?,
        shm: verify_optional_sidecar(boundary.directory(), DATABASE_SHM_FILE)?,
    })
}

fn recapture_optional_sidecars(
    boundary: &ProductionBoundary,
    _previous: SidecarIdentities,
) -> Result<SidecarIdentities, RuntimeStoreError> {
    let current = inspect_optional_sidecars(boundary)?;
    // SQLite may legitimately create, remove, or replace WAL/SHM files while
    // opening and recovering a connection. Retain each observation to prove
    // the prior boundary was safe, but require the new shape to pass the same
    // no-link, private-mode checks instead of requiring inode equality.
    Ok(current)
}

fn configure_connection(connection: &Connection) -> Result<(), RuntimeStoreError> {
    connection
        .busy_timeout(std::time::Duration::from_millis(5_000))
        .map_err(|source| {
            RuntimeStoreError::operation("configure database busy timeout", source)
        })?;
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("inspect database journal mode", source))?;
    if !journal_mode.eq_ignore_ascii_case("wal") {
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .map_err(|source| RuntimeStoreError::operation("enable database WAL", source))?;
    }
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| RuntimeStoreError::operation("enable database foreign keys", source))?;
    connection
        .pragma_update(None, "synchronous", "FULL")
        .map_err(|source| RuntimeStoreError::operation("enable full database sync", source))?;
    connection
        .pragma_update(None, "trusted_schema", "OFF")
        .map_err(|source| {
            RuntimeStoreError::operation("disable trusted database schema", source)
        })?;
    let journal_mode: String = connection
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("verify database WAL", source))?;
    let foreign_keys: i64 = connection
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("verify database foreign keys", source))?;
    let busy_timeout: i64 = connection
        .pragma_query_value(None, "busy_timeout", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("verify database busy timeout", source))?;
    let synchronous: i64 = connection
        .pragma_query_value(None, "synchronous", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("verify database sync mode", source))?;
    let trusted_schema: i64 = connection
        .pragma_query_value(None, "trusted_schema", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("verify trusted schema mode", source))?;
    let sqlite_version: String = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("inspect SQLite version", source))?;
    if !journal_mode.eq_ignore_ascii_case("wal")
        || foreign_keys != 1
        || busy_timeout != 5_000
        || synchronous != 2
        || trusted_schema != 0
        || !sqlite_version_at_least(&sqlite_version, (3, 35, 0))
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

fn sqlite_version_at_least(value: &str, minimum: (u32, u32, u32)) -> bool {
    let mut parts = value.split('.');
    let parsed = (
        parts.next().and_then(|part| part.parse::<u32>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
        parts.next().and_then(|part| part.parse::<u32>().ok()),
    );
    matches!(parsed, (Some(major), Some(minor), Some(patch)) if (major, minor, patch) >= minimum && parts.next().is_none())
}

/// Validates an explicitly authorized unsealed compatibility database before
/// its read-write migration/recovery path.
///
/// This is never used for sealed cross-open decisions or private legacy
/// rejection: normal SQLite read-only opens can create or mutate `-shm` while
/// observing a crash WAL. The atomic, name-encoded boundary marker handles
/// those decisions before any SQLite connection. Only a nonempty, unsealed
/// database requested through the compatibility opener reaches this
/// recovery-only check.
fn preflight_requested_boundary(
    connection: &Connection,
    requested: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    let table_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("preflight runtime schema", source))?;
    if table_count == 0 {
        return Ok(());
    }
    let runtime_schema_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'runtime_schema'",
            [],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("preflight runtime schema", source))?;
    if runtime_schema_count != 1 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let (version, user_version) = read_schema_versions(connection)?;
    if version != user_version {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    if version > DATABASE_SCHEMA_VERSION {
        return Err(RuntimeStoreError::UnsupportedSchema {
            found: version,
            expected: DATABASE_SCHEMA_VERSION,
        });
    }
    if version <= PROCESS_DATABASE_SCHEMA_VERSION {
        return if requested == StoreBoundaryKind::Compatibility {
            preflight_legacy_compatibility_rows(connection)?;
            Ok(())
        } else {
            Err(RuntimeStoreError::BoundaryKindMismatch)
        };
    }
    if version != DATABASE_SCHEMA_VERSION {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let stored: String = connection
        .query_row(
            "SELECT boundary_kind FROM runtime_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    if StoreBoundaryKind::parse(&stored)? != requested {
        return Err(RuntimeStoreError::BoundaryKindMismatch);
    }
    Ok(())
}

fn initialize_or_validate_schema(
    connection: &mut Connection,
    boundary_kind: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    let table_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("inspect runtime schema", source))?;
    if table_count == 0 {
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|source| {
                RuntimeStoreError::operation("begin runtime schema creation", source)
            })?;
        transaction
            .execute_batch(
                "CREATE TABLE runtime_schema (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    version INTEGER NOT NULL CHECK (version > 0),
                    boundary_kind TEXT NOT NULL CHECK (
                      boundary_kind IN ('compatibility','private_process_ledger')
                    )
                 );
                 CREATE TRIGGER runtime_schema_reject_boundary_kind_update
                    BEFORE UPDATE OF boundary_kind ON runtime_schema
                    BEGIN SELECT RAISE(ABORT, 'runtime store boundary kind is immutable'); END;
                 CREATE TRIGGER runtime_schema_reject_delete
                    BEFORE DELETE ON runtime_schema
                    BEGIN SELECT RAISE(ABORT, 'runtime schema singleton is retained'); END;
                 CREATE TRIGGER runtime_schema_reject_insert
                    BEFORE INSERT ON runtime_schema
                    WHEN EXISTS (SELECT 1 FROM runtime_schema)
                    BEGIN SELECT RAISE(ABORT, 'runtime schema singleton already exists'); END;

                 CREATE TABLE journal (
                    sequence INTEGER PRIMARY KEY CHECK (sequence > 0),
                    transition_id TEXT NOT NULL UNIQUE,
                    mission_id TEXT,
                    record_schema_version INTEGER NOT NULL CHECK (record_schema_version > 0),
                    transition_kind TEXT NOT NULL,
                    payload_json TEXT NOT NULL,
                    committed_at_utc TEXT NOT NULL,
                    extra_json TEXT NOT NULL,
                    outbox_fingerprint TEXT NOT NULL,
                    projection_fingerprint TEXT NOT NULL,
                    previous_checksum TEXT NOT NULL,
                    checksum TEXT NOT NULL UNIQUE
                 );
                 CREATE TRIGGER journal_reject_update
                    BEFORE UPDATE ON journal
                    BEGIN SELECT RAISE(ABORT, 'journal records are immutable'); END;
                 CREATE TRIGGER journal_reject_delete
                    BEFORE DELETE ON journal
                    BEGIN SELECT RAISE(ABORT, 'journal records are immutable'); END;

                 CREATE TABLE outbox (
                    idempotency_key TEXT PRIMARY KEY,
                    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
                    mission_id TEXT NOT NULL,
                    phase_id TEXT,
                    effect_kind TEXT NOT NULL CHECK (effect_kind IN ('provider_process','git_command','plugin_process')),
                    operation_slot TEXT NOT NULL,
                    logical_attempt INTEGER NOT NULL CHECK (logical_attempt > 0),
                    payload_json TEXT NOT NULL,
                    state TEXT NOT NULL CHECK (state IN ('pending','executing','succeeded','failed','uncertain')),
                    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
                    updated_at_utc TEXT NOT NULL
                 );
                 CREATE INDEX outbox_delivery_order
                    ON outbox(state, journal_sequence, idempotency_key);
                 CREATE TRIGGER outbox_reject_immutable_update
                    BEFORE UPDATE OF idempotency_key, journal_sequence, mission_id, phase_id,
                                     effect_kind, operation_slot, logical_attempt, payload_json
                    ON outbox
                    BEGIN SELECT RAISE(ABORT, 'outbox identity and intent are immutable'); END;
                 CREATE TRIGGER outbox_reject_operation_slot_update
                    BEFORE UPDATE OF operation_slot ON outbox
                    BEGIN SELECT RAISE(ABORT, 'outbox operation slots are immutable'); END;
                 CREATE TRIGGER outbox_reject_delete
                    BEFORE DELETE ON outbox
                    BEGIN SELECT RAISE(ABORT, 'outbox records are retained'); END;

                 CREATE TABLE outbox_attempt_claim (
                    idempotency_key TEXT NOT NULL REFERENCES outbox(idempotency_key) ON DELETE RESTRICT,
                    attempt INTEGER NOT NULL CHECK (attempt > 0),
                    claimed_at_utc TEXT NOT NULL,
                    PRIMARY KEY (idempotency_key, attempt)
                 );
                 CREATE TRIGGER outbox_attempt_claim_reject_update
                    BEFORE UPDATE ON outbox_attempt_claim
                    BEGIN SELECT RAISE(ABORT, 'outbox attempt claims are immutable'); END;
                 CREATE TRIGGER outbox_attempt_claim_reject_delete
                    BEFORE DELETE ON outbox_attempt_claim
                    BEGIN SELECT RAISE(ABORT, 'outbox attempt claims are retained'); END;

                 CREATE TABLE outbox_execution_identity (
                    idempotency_key TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    pid INTEGER NOT NULL CHECK (pid > 0),
                    process_group_id INTEGER NOT NULL CHECK (process_group_id > 0),
                    process_start_identity TEXT NOT NULL,
                    recorded_at_utc TEXT NOT NULL,
                    PRIMARY KEY (idempotency_key, attempt),
                    FOREIGN KEY (idempotency_key, attempt)
                      REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                      ON DELETE RESTRICT
                 );
                 CREATE TRIGGER outbox_execution_identity_reject_update
                    BEFORE UPDATE ON outbox_execution_identity
                    BEGIN SELECT RAISE(ABORT, 'process execution identities are immutable'); END;
                 CREATE TRIGGER outbox_execution_identity_reject_delete
                    BEFORE DELETE ON outbox_execution_identity
                    BEGIN SELECT RAISE(ABORT, 'process execution identities are retained'); END;

                 CREATE TABLE outbox_process_intent (
                    idempotency_key TEXT PRIMARY KEY
                      REFERENCES outbox(idempotency_key) ON DELETE RESTRICT,
                    intent_json TEXT NOT NULL CHECK (
                      length(intent_json) > 0 AND length(intent_json) <= 192
                    )
                 );
                 CREATE TRIGGER outbox_process_intent_reject_update
                    BEFORE UPDATE ON outbox_process_intent
                    BEGIN SELECT RAISE(ABORT, 'process intents are immutable'); END;
                 CREATE TRIGGER outbox_process_intent_reject_delete
                    BEFORE DELETE ON outbox_process_intent
                    BEGIN SELECT RAISE(ABORT, 'process intents are retained'); END;

                 CREATE TABLE outbox_process_release_authorization (
                    idempotency_key TEXT NOT NULL,
                    attempt INTEGER NOT NULL CHECK (attempt > 0),
                    journal_sequence INTEGER NOT NULL CHECK (journal_sequence > 0),
                    mission_id TEXT NOT NULL,
                    phase_id TEXT,
                    effect_kind TEXT NOT NULL CHECK (
                      effect_kind IN ('provider_process','git_command','plugin_process')
                    ),
                    operation_slot TEXT NOT NULL,
                    logical_attempt INTEGER NOT NULL CHECK (logical_attempt > 0),
                    payload_sha256 TEXT NOT NULL,
                    request_sha256 TEXT NOT NULL,
                    pid INTEGER NOT NULL CHECK (pid > 0),
                    process_group_id INTEGER NOT NULL CHECK (process_group_id > 0),
                    process_start_identity TEXT NOT NULL,
                    authorized_at_utc TEXT NOT NULL,
                    PRIMARY KEY (idempotency_key, attempt),
                    FOREIGN KEY (idempotency_key, attempt)
                      REFERENCES outbox_execution_identity(idempotency_key, attempt)
                      ON DELETE RESTRICT,
                    FOREIGN KEY (idempotency_key)
                      REFERENCES outbox_process_intent(idempotency_key)
                      ON DELETE RESTRICT
                 );
                 CREATE TRIGGER outbox_process_release_authorization_reject_update
                    BEFORE UPDATE ON outbox_process_release_authorization
                    BEGIN SELECT RAISE(ABORT, 'process release authorizations are immutable'); END;
                 CREATE TRIGGER outbox_process_release_authorization_reject_delete
                    BEFORE DELETE ON outbox_process_release_authorization
                    BEGIN SELECT RAISE(ABORT, 'process release authorizations are retained'); END;

                 CREATE TABLE outbox_attempt_observation (
                    observation_sequence INTEGER PRIMARY KEY CHECK (observation_sequence > 0),
                    idempotency_key TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    evidence_schema_version INTEGER NOT NULL CHECK (evidence_schema_version IN (1, 2)),
                    observed_state TEXT NOT NULL CHECK (observed_state IN ('pending','succeeded','failed','uncertain')),
                    evidence_code TEXT NOT NULL,
                    evidence_json TEXT NOT NULL,
                    observed_at_utc TEXT NOT NULL,
                    FOREIGN KEY (idempotency_key, attempt)
                      REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                      ON DELETE RESTRICT
                 );
                 CREATE INDEX outbox_observation_history
                    ON outbox_attempt_observation(idempotency_key, attempt, observation_sequence);
                 CREATE TRIGGER outbox_attempt_observation_reject_update
                    BEFORE UPDATE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are immutable'); END;
                 CREATE TRIGGER outbox_attempt_observation_reject_delete
                    BEFORE DELETE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are retained'); END;

                 CREATE TABLE legacy_retry_authorization_consumption (
                    observation_sequence INTEGER PRIMARY KEY CHECK (observation_sequence > 0)
                      REFERENCES outbox_attempt_observation(observation_sequence)
                      ON DELETE RESTRICT,
                    record_schema_version INTEGER NOT NULL CHECK (record_schema_version = 1),
                    decision_id TEXT NOT NULL,
                    idempotency_key TEXT NOT NULL,
                    attempt INTEGER NOT NULL CHECK (attempt > 0),
                    source_state TEXT NOT NULL CHECK (source_state IN ('failed','uncertain')),
                    action TEXT NOT NULL CHECK (action IN ('retry_failed','operator_authorized_retry')),
                    target_state TEXT NOT NULL CHECK (target_state = 'pending'),
                    authority TEXT NOT NULL CHECK (authority IN ('policy','operator')),
                    decided_at_utc TEXT NOT NULL,
                    consumed_at_utc TEXT NOT NULL,
                    CHECK (
                      (authority = 'policy' AND source_state = 'failed' AND action = 'retry_failed')
                      OR
                      (authority = 'operator' AND source_state = 'failed' AND action = 'retry_failed')
                      OR
                      (authority = 'operator' AND source_state = 'uncertain'
                        AND action = 'operator_authorized_retry')
                    ),
                    FOREIGN KEY (idempotency_key, attempt)
                      REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                      ON DELETE RESTRICT
                 );
                 CREATE INDEX legacy_retry_authorization_decision_lookup
                    ON legacy_retry_authorization_consumption(decision_id);
                 CREATE TRIGGER legacy_retry_authorization_consumption_reject_update
                    BEFORE UPDATE ON legacy_retry_authorization_consumption
                    BEGIN SELECT RAISE(ABORT, 'legacy retry authorizations are immutable once consumed'); END;
                 CREATE TRIGGER legacy_retry_authorization_consumption_reject_delete
                    BEFORE DELETE ON legacy_retry_authorization_consumption
                    BEGIN SELECT RAISE(ABORT, 'legacy retry authorization consumption is retained'); END;

                 CREATE TABLE retry_authorization_consumption (
                    decision_id TEXT PRIMARY KEY,
                    idempotency_key TEXT NOT NULL,
                    attempt INTEGER NOT NULL,
                    source_state TEXT NOT NULL CHECK (source_state IN ('failed','uncertain')),
                    action TEXT NOT NULL CHECK (action IN ('retry_failed','operator_authorized_retry')),
                    target_state TEXT NOT NULL CHECK (target_state = 'pending'),
                    authority TEXT NOT NULL CHECK (authority IN ('policy','operator')),
                    policy_version TEXT NOT NULL,
                    decided_at_utc TEXT NOT NULL,
                    consumed_at_utc TEXT NOT NULL,
                    observation_sequence INTEGER NOT NULL UNIQUE
                      REFERENCES outbox_attempt_observation(observation_sequence)
                      ON DELETE RESTRICT,
                    FOREIGN KEY (idempotency_key, attempt)
                      REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                      ON DELETE RESTRICT
                 );
                 CREATE TRIGGER retry_authorization_consumption_reject_reused_decision
                    BEFORE INSERT ON retry_authorization_consumption
                    WHEN EXISTS (
                      SELECT 1 FROM legacy_retry_authorization_consumption
                      WHERE decision_id = NEW.decision_id
                    )
                    BEGIN SELECT RAISE(ABORT, 'retry authorization decision is already consumed'); END;
                 CREATE TRIGGER retry_authorization_consumption_reject_update
                    BEFORE UPDATE ON retry_authorization_consumption
                    BEGIN SELECT RAISE(ABORT, 'retry authorizations are immutable once consumed'); END;
                 CREATE TRIGGER retry_authorization_consumption_reject_delete
                    BEFORE DELETE ON retry_authorization_consumption
                    BEGIN SELECT RAISE(ABORT, 'retry authorization consumption is retained'); END;

                 CREATE TABLE projection_requirement (
                    journal_sequence INTEGER NOT NULL REFERENCES journal(sequence) ON DELETE RESTRICT,
                    projection TEXT NOT NULL CHECK (projection IN ('checkpoint','event_log','workspace','sidecars','metrics')),
                    PRIMARY KEY (journal_sequence, projection)
                 );
                 CREATE TRIGGER projection_requirement_reject_update
                    BEFORE UPDATE ON projection_requirement
                    BEGIN SELECT RAISE(ABORT, 'projection requirements are immutable'); END;
                 CREATE TRIGGER projection_requirement_reject_delete
                    BEFORE DELETE ON projection_requirement
                    BEGIN SELECT RAISE(ABORT, 'projection requirements are retained'); END;

                 CREATE TABLE projection_receipt (
                    record_schema_version INTEGER NOT NULL CHECK (record_schema_version IN (1, 2)),
                    journal_sequence INTEGER NOT NULL,
                    projection TEXT NOT NULL,
                    mission_id TEXT,
                    public_sequence INTEGER CHECK (public_sequence IS NULL OR public_sequence > 0),
                    event_id TEXT,
                    event_jsonl BLOB CHECK (
                      event_jsonl IS NULL OR (length(event_jsonl) > 1 AND length(event_jsonl) <= 1048576)
                    ),
                    event_sha256 TEXT,
                    applied_at_utc TEXT NOT NULL,
                    PRIMARY KEY (journal_sequence, projection),
                    FOREIGN KEY (journal_sequence, projection)
                      REFERENCES projection_requirement(journal_sequence, projection)
                      ON DELETE RESTRICT,
                    CHECK (
                      (projection = 'event_log'
                        AND mission_id IS NOT NULL
                        AND public_sequence IS NOT NULL
                        AND event_id IS NOT NULL
                        AND ((record_schema_version = 1
                              AND event_jsonl IS NULL
                              AND event_sha256 IS NULL)
                             OR
                             (record_schema_version = 2
                              AND event_jsonl IS NOT NULL
                              AND event_sha256 IS NOT NULL)))
                      OR
                      (projection != 'event_log'
                        AND mission_id IS NULL
                        AND public_sequence IS NULL
                        AND event_id IS NULL
                        AND event_jsonl IS NULL
                        AND event_sha256 IS NULL)
                    )
                 );
                 CREATE UNIQUE INDEX projection_event_sequence_v2
                    ON projection_receipt(mission_id, public_sequence)
                    WHERE projection = 'event_log' AND record_schema_version = 2;
                 CREATE TRIGGER projection_receipt_reject_update
                    BEFORE UPDATE ON projection_receipt
                    BEGIN SELECT RAISE(ABORT, 'projection receipts are immutable'); END;
                 CREATE TRIGGER projection_receipt_reject_delete
                    BEFORE DELETE ON projection_receipt
                    BEGIN SELECT RAISE(ABORT, 'projection receipts are retained'); END;

                 CREATE TABLE command_ack (
                    journal_sequence INTEGER PRIMARY KEY REFERENCES journal(sequence) ON DELETE RESTRICT,
                    acknowledged_at_utc TEXT NOT NULL
                 );
                 CREATE TRIGGER command_ack_reject_update
                    BEFORE UPDATE ON command_ack
                    BEGIN SELECT RAISE(ABORT, 'command acknowledgements are immutable'); END;
                 CREATE TRIGGER command_ack_reject_delete
                    BEFORE DELETE ON command_ack
                    BEGIN SELECT RAISE(ABORT, 'command acknowledgements are retained'); END;

                 CREATE TABLE projection_cursor (
                    projection TEXT PRIMARY KEY,
                    journal_sequence INTEGER NOT NULL CHECK (journal_sequence >= 0),
                    mission_id TEXT,
                    public_sequence INTEGER CHECK (public_sequence IS NULL OR public_sequence > 0),
                    event_id TEXT,
                    event_sha256 TEXT,
                    updated_at_utc TEXT NOT NULL
                 );
                 PRAGMA user_version = 7;",
            )
            .map_err(|source| RuntimeStoreError::operation("create runtime schema", source))?;
        transaction
            .execute_batch(REASONING_SCHEMA_DDL)
            .map_err(|source| RuntimeStoreError::operation("create reasoning schema", source))?;
        transaction
            .execute_batch(KNOWLEDGE_PUBLICATION_SCHEMA_DDL)
            .map_err(|source| {
                RuntimeStoreError::operation("create knowledge publication schema", source)
            })?;
        create_audit_chain_schema(&transaction)?;
        transaction
            .execute(
                "INSERT INTO runtime_schema(singleton, version, boundary_kind)
                 VALUES (1, ?1, ?2)",
                params![DATABASE_SCHEMA_VERSION, boundary_kind.as_str()],
            )
            .map_err(|source| {
                RuntimeStoreError::operation("bind runtime schema boundary", source)
            })?;
        transaction
            .commit()
            .map_err(|source| RuntimeStoreError::operation("commit runtime schema", source))?;
    } else {
        let (version, user_version) = read_schema_versions(connection)?;
        match (version, user_version) {
            (LEGACY_DATABASE_SCHEMA_VERSION, LEGACY_DATABASE_SCHEMA_VERSION) => {
                if boundary_kind != StoreBoundaryKind::Compatibility {
                    return Err(RuntimeStoreError::BoundaryKindMismatch);
                }
                verify_legacy_schema(connection)?;
                preflight_legacy_compatibility_rows(connection)?;
                migrate_v1_to_v2(connection)?;
                verify_v2_schema_for_migration(connection)?;
                migrate_v2_to_v3(connection)?;
                verify_v3_schema_for_migration(connection)?;
                migrate_v3_to_v4(connection)?;
                verify_v4_schema_for_migration(connection)?;
                migrate_v4_to_v5(connection)?;
                verify_v5_schema_for_migration(connection)?;
                migrate_v5_to_v6(connection, boundary_kind)?;
                verify_v6_schema_for_migration(connection)?;
                migrate_v6_to_v7(connection, boundary_kind)?;
            }
            (INTERMEDIATE_DATABASE_SCHEMA_VERSION, INTERMEDIATE_DATABASE_SCHEMA_VERSION) => {
                if boundary_kind != StoreBoundaryKind::Compatibility {
                    return Err(RuntimeStoreError::BoundaryKindMismatch);
                }
                verify_v2_schema_for_migration(connection)?;
                preflight_legacy_compatibility_rows(connection)?;
                migrate_v2_to_v3(connection)?;
                verify_v3_schema_for_migration(connection)?;
                migrate_v3_to_v4(connection)?;
                verify_v4_schema_for_migration(connection)?;
                migrate_v4_to_v5(connection)?;
                verify_v5_schema_for_migration(connection)?;
                migrate_v5_to_v6(connection, boundary_kind)?;
                verify_v6_schema_for_migration(connection)?;
                migrate_v6_to_v7(connection, boundary_kind)?;
            }
            (PROCESS_DATABASE_SCHEMA_VERSION, PROCESS_DATABASE_SCHEMA_VERSION) => {
                if boundary_kind != StoreBoundaryKind::Compatibility {
                    return Err(RuntimeStoreError::BoundaryKindMismatch);
                }
                verify_v3_schema_for_migration(connection)?;
                preflight_legacy_compatibility_rows(connection)?;
                migrate_v3_to_v4(connection)?;
                verify_v4_schema_for_migration(connection)?;
                migrate_v4_to_v5(connection)?;
                verify_v5_schema_for_migration(connection)?;
                migrate_v5_to_v6(connection, boundary_kind)?;
                verify_v6_schema_for_migration(connection)?;
                migrate_v6_to_v7(connection, boundary_kind)?;
            }
            (BOUNDARY_DATABASE_SCHEMA_VERSION, BOUNDARY_DATABASE_SCHEMA_VERSION) => {
                verify_v4_schema_for_migration(connection)?;
                migrate_v4_to_v5(connection)?;
                verify_v5_schema_for_migration(connection)?;
                migrate_v5_to_v6(connection, boundary_kind)?;
                verify_v6_schema_for_migration(connection)?;
                migrate_v6_to_v7(connection, boundary_kind)?;
            }
            (REASONING_DATABASE_SCHEMA_VERSION, REASONING_DATABASE_SCHEMA_VERSION) => {
                verify_v5_schema_for_migration(connection)?;
                migrate_v5_to_v6(connection, boundary_kind)?;
                verify_v6_schema_for_migration(connection)?;
                migrate_v6_to_v7(connection, boundary_kind)?;
            }
            (KNOWLEDGE_DATABASE_SCHEMA_VERSION, KNOWLEDGE_DATABASE_SCHEMA_VERSION) => {
                verify_v6_schema_for_migration(connection)?;
                migrate_v6_to_v7(connection, boundary_kind)?;
            }
            (DATABASE_SCHEMA_VERSION, DATABASE_SCHEMA_VERSION) => {}
            (found, user) if found > DATABASE_SCHEMA_VERSION || user > DATABASE_SCHEMA_VERSION => {
                return Err(RuntimeStoreError::UnsupportedSchema {
                    found: found.max(user),
                    expected: DATABASE_SCHEMA_VERSION,
                });
            }
            _ => return Err(RuntimeStoreError::CorruptDatabase),
        }
    }
    verify_schema_version(connection)?;
    verify_store_boundary_kind(connection, boundary_kind)
}

fn read_schema_versions(connection: &Connection) -> Result<(i64, i64), RuntimeStoreError> {
    let version = connection
        .query_row(
            "SELECT version FROM runtime_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("read runtime schema version", source))?;
    let user_version = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("read database user version", source))?;
    Ok((version, user_version))
}

fn verify_legacy_schema(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM runtime_schema", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("validate legacy schema row", source))?;
    if rows != 1 || !legacy_schema_digest_is_approved(&schema_digest(connection)?) {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let quick_check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("check legacy runtime database", source))?;
    let foreign_key_violation: Option<i64> = connection
        .query_row(
            "SELECT rowid FROM pragma_foreign_key_check LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("check legacy foreign keys", source))?;
    if quick_check != "ok" || foreign_key_violation.is_some() {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

fn legacy_schema_digest_is_approved(digest: &str) -> bool {
    digest == LEGACY_SCHEMA_DIGEST
}

/// Proves that a boundary-less v1-v3 database contains compatibility rows
/// only before any schema migration can begin.
///
/// A zero-requirement row or a known Rust-private transition kind is never
/// inferred or adopted as compatibility. The enrolled whole-home writer
/// authority makes this read-only query a stable snapshot; no write
/// transaction is needed before deciding whether migration is authorized.
fn preflight_legacy_compatibility_rows(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let incompatible_row: Option<i64> = connection
        .query_row(
            "SELECT journal.sequence
             FROM journal
             WHERE journal.transition_kind IN ('cell3.process_claim', 'cell1.terminal_decision')
                OR NOT EXISTS (
                  SELECT 1 FROM projection_requirement
                  WHERE projection_requirement.journal_sequence = journal.sequence
                )
             ORDER BY journal.sequence LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| {
            RuntimeStoreError::operation("preflight legacy compatibility rows", source)
        })?;
    if incompatible_row.is_some() {
        return Err(RuntimeStoreError::BoundaryKindMismatch);
    }
    Ok(())
}

fn backfill_legacy_retry_authorization_consumption(
    transaction: &Transaction<'_>,
) -> Result<(), RuntimeStoreError> {
    let mut statement = transaction
        .prepare(
            "SELECT observation_sequence, idempotency_key, attempt, evidence_schema_version,
                    observed_state, evidence_code, evidence_json, observed_at_utc
             FROM outbox_attempt_observation ORDER BY observation_sequence",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare legacy retry backfill", source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query legacy retry backfill", source))?;
    let mut prior_by_attempt = BTreeMap::<(String, u32), OutboxState>::new();
    for row in rows {
        let (
            observation_sequence,
            idempotency_key,
            attempt,
            evidence_schema_version,
            observed_state,
            evidence_code,
            evidence_json,
            observed_at_utc,
        ) = row.map_err(|source| {
            RuntimeStoreError::operation("decode legacy retry backfill", source)
        })?;
        let attempt = u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if observation_sequence <= 0
            || attempt == 0
            || evidence_schema_version != LEGACY_RECORD_SCHEMA_VERSION
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        validate_atom(
            "legacy retry idempotency key",
            &idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        validate_timestamp(&observed_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let observed_state = OutboxState::parse(&observed_state)?;
        let prior = prior_by_attempt
            .get(&(idempotency_key.clone(), attempt))
            .copied()
            .unwrap_or(OutboxState::Executing);
        let evidence = parse_legacy_v1_evidence(&evidence_json)?;
        if evidence.code != evidence_code {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        if let Some(retry) = bind_legacy_retry_evidence(&evidence, prior)? {
            if observed_state != OutboxState::Pending {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            transaction
                .execute(
                    "INSERT INTO legacy_retry_authorization_consumption (
                        observation_sequence, record_schema_version, decision_id,
                        idempotency_key, attempt, source_state, action, target_state,
                        authority, decided_at_utc, consumed_at_utc
                     ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7, ?8, ?9)",
                    params![
                        observation_sequence,
                        retry.decision_id,
                        idempotency_key,
                        attempt,
                        retry.source_state.as_str(),
                        retry.action.as_str(),
                        retry.authority.as_str(),
                        retry.decided_at_utc,
                        observed_at_utc,
                    ],
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("backfill legacy retry authorization", source)
                })?;
        }
        prior_by_attempt.insert((idempotency_key, attempt), observed_state);
    }
    Ok(())
}

fn migrate_v1_to_v2(connection: &mut Connection) -> Result<(), RuntimeStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| RuntimeStoreError::operation("begin runtime schema migration", source))?;
    transaction
        .execute_batch(
            "ALTER TABLE outbox ADD COLUMN operation_slot TEXT;
             UPDATE outbox SET operation_slot = 'legacy-v1' WHERE operation_slot IS NULL;
             CREATE TRIGGER outbox_reject_operation_slot_update
                BEFORE UPDATE OF operation_slot ON outbox
                BEGIN SELECT RAISE(ABORT, 'outbox operation slots are immutable'); END;

             ALTER TABLE outbox_attempt_observation
                ADD COLUMN evidence_schema_version INTEGER NOT NULL DEFAULT 1
                CHECK (evidence_schema_version IN (1, 2));

             CREATE TABLE legacy_retry_authorization_consumption (
                observation_sequence INTEGER PRIMARY KEY CHECK (observation_sequence > 0)
                  REFERENCES outbox_attempt_observation(observation_sequence)
                  ON DELETE RESTRICT,
                record_schema_version INTEGER NOT NULL CHECK (record_schema_version = 1),
                decision_id TEXT NOT NULL,
                idempotency_key TEXT NOT NULL,
                attempt INTEGER NOT NULL CHECK (attempt > 0),
                source_state TEXT NOT NULL CHECK (source_state IN ('failed','uncertain')),
                action TEXT NOT NULL CHECK (action IN ('retry_failed','operator_authorized_retry')),
                target_state TEXT NOT NULL CHECK (target_state = 'pending'),
                authority TEXT NOT NULL CHECK (authority IN ('policy','operator')),
                decided_at_utc TEXT NOT NULL,
                consumed_at_utc TEXT NOT NULL,
                CHECK (
                  (authority = 'policy' AND source_state = 'failed' AND action = 'retry_failed')
                  OR
                  (authority = 'operator' AND source_state = 'failed' AND action = 'retry_failed')
                  OR
                  (authority = 'operator' AND source_state = 'uncertain'
                    AND action = 'operator_authorized_retry')
                ),
                FOREIGN KEY (idempotency_key, attempt)
                  REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                  ON DELETE RESTRICT
             );
             CREATE INDEX legacy_retry_authorization_decision_lookup
                ON legacy_retry_authorization_consumption(decision_id);
             CREATE TRIGGER legacy_retry_authorization_consumption_reject_update
                BEFORE UPDATE ON legacy_retry_authorization_consumption
                BEGIN SELECT RAISE(ABORT, 'legacy retry authorizations are immutable once consumed'); END;
             CREATE TRIGGER legacy_retry_authorization_consumption_reject_delete
                BEFORE DELETE ON legacy_retry_authorization_consumption
                BEGIN SELECT RAISE(ABORT, 'legacy retry authorization consumption is retained'); END;

             CREATE TABLE retry_authorization_consumption (
                decision_id TEXT PRIMARY KEY,
                idempotency_key TEXT NOT NULL,
                attempt INTEGER NOT NULL,
                source_state TEXT NOT NULL CHECK (source_state IN ('failed','uncertain')),
                action TEXT NOT NULL CHECK (action IN ('retry_failed','operator_authorized_retry')),
                target_state TEXT NOT NULL CHECK (target_state = 'pending'),
                authority TEXT NOT NULL CHECK (authority IN ('policy','operator')),
                policy_version TEXT NOT NULL,
                decided_at_utc TEXT NOT NULL,
                consumed_at_utc TEXT NOT NULL,
                observation_sequence INTEGER NOT NULL UNIQUE
                  REFERENCES outbox_attempt_observation(observation_sequence)
                  ON DELETE RESTRICT,
                FOREIGN KEY (idempotency_key, attempt)
                  REFERENCES outbox_attempt_claim(idempotency_key, attempt)
                  ON DELETE RESTRICT
             );
             CREATE TRIGGER retry_authorization_consumption_reject_reused_decision
                BEFORE INSERT ON retry_authorization_consumption
                WHEN EXISTS (
                  SELECT 1 FROM legacy_retry_authorization_consumption
                  WHERE decision_id = NEW.decision_id
                )
                BEGIN SELECT RAISE(ABORT, 'retry authorization decision is already consumed'); END;
             CREATE TRIGGER retry_authorization_consumption_reject_update
                BEFORE UPDATE ON retry_authorization_consumption
                BEGIN SELECT RAISE(ABORT, 'retry authorizations are immutable once consumed'); END;
             CREATE TRIGGER retry_authorization_consumption_reject_delete
                BEFORE DELETE ON retry_authorization_consumption
                BEGIN SELECT RAISE(ABORT, 'retry authorization consumption is retained'); END;

             DROP TRIGGER projection_receipt_reject_update;
             DROP TRIGGER projection_receipt_reject_delete;
             ALTER TABLE projection_receipt
                ADD COLUMN record_schema_version INTEGER NOT NULL DEFAULT 1
                CHECK (record_schema_version IN (1, 2));
             ALTER TABLE projection_receipt ADD COLUMN mission_id TEXT;
             ALTER TABLE projection_receipt ADD COLUMN event_jsonl BLOB;
             ALTER TABLE projection_receipt ADD COLUMN event_sha256 TEXT;
             UPDATE projection_receipt
                SET mission_id = (
                    SELECT journal.mission_id FROM journal
                    WHERE journal.sequence = projection_receipt.journal_sequence
                )
                WHERE projection = 'event_log';
             CREATE UNIQUE INDEX projection_event_sequence_v2
                ON projection_receipt(mission_id, public_sequence)
                WHERE projection = 'event_log' AND record_schema_version = 2;
             CREATE TRIGGER projection_receipt_reject_update
                BEFORE UPDATE ON projection_receipt
                BEGIN SELECT RAISE(ABORT, 'projection receipts are immutable'); END;
             CREATE TRIGGER projection_receipt_reject_delete
                BEFORE DELETE ON projection_receipt
                BEGIN SELECT RAISE(ABORT, 'projection receipts are retained'); END;

             ALTER TABLE projection_cursor ADD COLUMN mission_id TEXT;
             ALTER TABLE projection_cursor ADD COLUMN event_sha256 TEXT;
             UPDATE projection_cursor
                SET mission_id = (
                    SELECT journal.mission_id
                    FROM projection_receipt
                    JOIN journal ON journal.sequence = projection_receipt.journal_sequence
                    WHERE projection_receipt.projection = 'event_log'
                      AND projection_receipt.journal_sequence <= projection_cursor.journal_sequence
                    ORDER BY projection_receipt.journal_sequence DESC LIMIT 1
                )
                WHERE projection = 'event_log' AND public_sequence IS NOT NULL;",
        )
        .map_err(|source| RuntimeStoreError::operation("migrate runtime schema", source))?;
    backfill_legacy_retry_authorization_consumption(&transaction)?;
    maybe_crash_during_migration("after_backfill");
    transaction
        .execute_batch(
            "UPDATE runtime_schema SET version = 2 WHERE singleton = 1;
             PRAGMA user_version = 2;",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("version migrated runtime schema", source)
        })?;
    verify_v2_schema_for_migration(&transaction)?;
    maybe_crash_during_migration("after_validation");
    transaction.commit().map_err(|source| {
        RuntimeStoreError::operation("commit runtime schema migration", source)
    })?;
    Ok(())
}

fn verify_v2_schema_for_migration(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let (version, user_version) = read_schema_versions(connection)?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM runtime_schema", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("validate v2 schema row", source))?;
    let digest = schema_digest(connection)?;
    if version != INTERMEDIATE_DATABASE_SCHEMA_VERSION
        || user_version != INTERMEDIATE_DATABASE_SCHEMA_VERSION
        || rows != 1
        || (digest != NATIVE_SCHEMA_DIGEST && digest != MIGRATED_SCHEMA_DIGEST)
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let quick_check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("check v2 runtime database", source))?;
    let foreign_key_violation: Option<i64> = connection
        .query_row(
            "SELECT rowid FROM pragma_foreign_key_check LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("check v2 foreign keys", source))?;
    if quick_check != "ok" || foreign_key_violation.is_some() {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

fn migrate_v2_to_v3(connection: &mut Connection) -> Result<(), RuntimeStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| RuntimeStoreError::operation("begin v3 schema migration", source))?;
    transaction
        .execute_batch(
            "CREATE TABLE outbox_process_intent (
                idempotency_key TEXT PRIMARY KEY
                  REFERENCES outbox(idempotency_key) ON DELETE RESTRICT,
                intent_json TEXT NOT NULL CHECK (
                  length(intent_json) > 0 AND length(intent_json) <= 192
                )
             );
             CREATE TRIGGER outbox_process_intent_reject_update
                BEFORE UPDATE ON outbox_process_intent
                BEGIN SELECT RAISE(ABORT, 'process intents are immutable'); END;
             CREATE TRIGGER outbox_process_intent_reject_delete
                BEFORE DELETE ON outbox_process_intent
                BEGIN SELECT RAISE(ABORT, 'process intents are retained'); END;

             CREATE TABLE outbox_process_release_authorization (
                idempotency_key TEXT NOT NULL,
                attempt INTEGER NOT NULL CHECK (attempt > 0),
                journal_sequence INTEGER NOT NULL CHECK (journal_sequence > 0),
                mission_id TEXT NOT NULL,
                phase_id TEXT,
                effect_kind TEXT NOT NULL CHECK (
                  effect_kind IN ('provider_process','git_command','plugin_process')
                ),
                operation_slot TEXT NOT NULL,
                logical_attempt INTEGER NOT NULL CHECK (logical_attempt > 0),
                payload_sha256 TEXT NOT NULL,
                request_sha256 TEXT NOT NULL,
                pid INTEGER NOT NULL CHECK (pid > 0),
                process_group_id INTEGER NOT NULL CHECK (process_group_id > 0),
                process_start_identity TEXT NOT NULL,
                authorized_at_utc TEXT NOT NULL,
                PRIMARY KEY (idempotency_key, attempt),
                FOREIGN KEY (idempotency_key, attempt)
                  REFERENCES outbox_execution_identity(idempotency_key, attempt)
                  ON DELETE RESTRICT,
                FOREIGN KEY (idempotency_key)
                  REFERENCES outbox_process_intent(idempotency_key)
                  ON DELETE RESTRICT
             );
             CREATE TRIGGER outbox_process_release_authorization_reject_update
                BEFORE UPDATE ON outbox_process_release_authorization
                BEGIN SELECT RAISE(ABORT, 'process release authorizations are immutable'); END;
             CREATE TRIGGER outbox_process_release_authorization_reject_delete
                BEFORE DELETE ON outbox_process_release_authorization
                BEGIN SELECT RAISE(ABORT, 'process release authorizations are retained'); END;

             UPDATE runtime_schema SET version = 3 WHERE singleton = 1;
             PRAGMA user_version = 3;",
        )
        .map_err(|source| RuntimeStoreError::operation("migrate runtime schema to v3", source))?;
    maybe_crash_during_v3_migration("after_schema");
    validate_v3_database(&transaction)?;
    maybe_crash_during_v3_migration("after_validation");
    transaction
        .commit()
        .map_err(|source| RuntimeStoreError::operation("commit v3 schema migration", source))?;
    Ok(())
}

fn verify_v3_schema_for_migration(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let (version, user_version) = read_schema_versions(connection)?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM runtime_schema", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("validate v3 schema row", source))?;
    let digest = schema_digest(connection)?;
    if version != PROCESS_DATABASE_SCHEMA_VERSION
        || user_version != PROCESS_DATABASE_SCHEMA_VERSION
        || rows != 1
        || (digest != NATIVE_V3_SCHEMA_DIGEST
            && digest != NATIVE_V2_MIGRATED_V3_SCHEMA_DIGEST
            && digest != LEGACY_V1_MIGRATED_V3_SCHEMA_DIGEST)
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    verify_required_schema_objects(connection, false)
}

fn validate_v3_database(connection: &Connection) -> Result<(), RuntimeStoreError> {
    verify_v3_schema_for_migration(connection)?;
    validate_integrity(connection)?;
    validate_checksum_chain(connection)
}

fn migrate_v3_to_v4(connection: &mut Connection) -> Result<(), RuntimeStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| RuntimeStoreError::operation("begin v4 schema migration", source))?;
    transaction
        .execute_batch(
            "ALTER TABLE runtime_schema
                ADD COLUMN boundary_kind TEXT NOT NULL DEFAULT 'compatibility'
                CHECK (boundary_kind IN ('compatibility','private_process_ledger'));
             CREATE TRIGGER runtime_schema_reject_boundary_kind_update
                BEFORE UPDATE OF boundary_kind ON runtime_schema
                BEGIN SELECT RAISE(ABORT, 'runtime store boundary kind is immutable'); END;
             CREATE TRIGGER runtime_schema_reject_delete
                BEFORE DELETE ON runtime_schema
                BEGIN SELECT RAISE(ABORT, 'runtime schema singleton is retained'); END;
             CREATE TRIGGER runtime_schema_reject_insert
                BEFORE INSERT ON runtime_schema
                WHEN EXISTS (SELECT 1 FROM runtime_schema)
                BEGIN SELECT RAISE(ABORT, 'runtime schema singleton already exists'); END;
             UPDATE runtime_schema SET version = 4 WHERE singleton = 1;
             PRAGMA user_version = 4;",
        )
        .map_err(|source| RuntimeStoreError::operation("migrate runtime schema to v4", source))?;
    maybe_crash_during_v4_migration("after_schema");
    validate_v4_database(&transaction)?;
    maybe_crash_during_v4_migration("after_validation");
    transaction
        .commit()
        .map_err(|source| RuntimeStoreError::operation("commit v4 schema migration", source))?;
    Ok(())
}

fn verify_v4_schema_for_migration(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let (version, user_version) = read_schema_versions(connection)?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM runtime_schema", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("validate v4 schema row", source))?;
    let digest = schema_digest(connection)?;
    if version != BOUNDARY_DATABASE_SCHEMA_VERSION
        || user_version != BOUNDARY_DATABASE_SCHEMA_VERSION
        || rows != 1
        || (digest != NATIVE_V4_SCHEMA_DIGEST
            && digest != NATIVE_V3_MIGRATED_V4_SCHEMA_DIGEST
            && digest != NATIVE_V2_MIGRATED_V4_SCHEMA_DIGEST
            && digest != LEGACY_V1_MIGRATED_V4_SCHEMA_DIGEST)
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    verify_required_schema_objects(connection, true)
}

fn validate_v4_database(connection: &Connection) -> Result<(), RuntimeStoreError> {
    verify_v4_schema_for_migration(connection)?;
    validate_integrity(connection)?;
    validate_checksum_chain(connection)
}

// Unlike the earlier migrations, `migrate_v4_to_v5` carries no
// `maybe_crash_during_*` injection points: it only appends new tables (no
// journal-byte rewrite), and the single Immediate transaction rolls the whole
// additive DDL back atomically on any crash, so there is no partial-migration
// state to resume.
fn migrate_v4_to_v5(connection: &mut Connection) -> Result<(), RuntimeStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| RuntimeStoreError::operation("begin v5 schema migration", source))?;
    transaction
        .execute_batch(REASONING_SCHEMA_DDL)
        .map_err(|source| RuntimeStoreError::operation("create reasoning schema", source))?;
    transaction
        .execute_batch(
            "UPDATE runtime_schema SET version = 5 WHERE singleton = 1;
             PRAGMA user_version = 5;",
        )
        .map_err(|source| RuntimeStoreError::operation("migrate runtime schema to v5", source))?;
    validate_integrity(&transaction)?;
    validate_checksum_chain(&transaction)?;
    transaction
        .commit()
        .map_err(|source| RuntimeStoreError::operation("commit v5 schema migration", source))?;
    Ok(())
}

fn verify_v5_schema_for_migration(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let (version, user_version) = read_schema_versions(connection)?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM runtime_schema", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("validate v5 schema row", source))?;
    let digest = schema_digest(connection)?;
    if version != REASONING_DATABASE_SCHEMA_VERSION
        || user_version != REASONING_DATABASE_SCHEMA_VERSION
        || rows != 1
        || (digest != NATIVE_V5_SCHEMA_DIGEST
            && digest != NATIVE_V3_MIGRATED_V5_SCHEMA_DIGEST
            && digest != NATIVE_V2_MIGRATED_V5_SCHEMA_DIGEST
            && digest != LEGACY_V1_MIGRATED_V5_SCHEMA_DIGEST)
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    verify_required_schema_objects(connection, true)
}

// Like `migrate_v4_to_v5`, this migration carries no `maybe_crash_during_*`
// injection point: it only appends the append-only `knowledge_publication`
// queue (no journal-byte rewrite), and the single Immediate transaction rolls
// the whole additive DDL back atomically on any crash, so there is no
// partial-migration state to resume.
fn migrate_v5_to_v6(
    connection: &mut Connection,
    boundary_kind: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| RuntimeStoreError::operation("begin v6 schema migration", source))?;
    transaction
        .execute_batch(KNOWLEDGE_PUBLICATION_SCHEMA_DDL)
        .map_err(|source| {
            RuntimeStoreError::operation("create knowledge publication schema", source)
        })?;
    transaction
        .execute_batch(
            "UPDATE runtime_schema SET version = 6 WHERE singleton = 1;
             PRAGMA user_version = 6;",
        )
        .map_err(|source| RuntimeStoreError::operation("migrate runtime schema to v6", source))?;
    // v6 is no longer the terminal version, so validate what is true *at v6*
    // rather than calling `validate_database`, whose `verify_schema_version`
    // asserts the final `DATABASE_SCHEMA_VERSION`. This mirrors
    // `migrate_v4_to_v5`, which has always been an intermediate step.
    verify_store_boundary_kind(&transaction, boundary_kind)?;
    validate_integrity(&transaction)?;
    validate_checksum_chain(&transaction)?;
    transaction
        .commit()
        .map_err(|source| RuntimeStoreError::operation("commit v6 schema migration", source))?;
    Ok(())
}

/// Objects introduced by schema v6. Checked from [`verify_v6_schema_for_migration`]
/// and [`verify_schema_objects`] — the v3/v4/v5 migration verifications run
/// before these exist, so they stay out of the shared `REQUIRED` list.
const KNOWLEDGE_PUBLICATION_REQUIRED: &[(&str, &str)] = &[
    ("table", "knowledge_publication"),
    ("index", "knowledge_publication_delivery_order"),
    ("trigger", "knowledge_publication_reject_immutable_update"),
    ("trigger", "knowledge_publication_reject_reopen"),
    ("trigger", "knowledge_publication_reject_delete"),
];

/// Objects introduced by schema v7 (B3-DESIGN §5).
const AUDIT_CHAIN_REQUIRED: &[(&str, &str)] = &[
    ("table", "audit_chain"),
    ("table", "audit_chain_head"),
    ("trigger", "audit_chain_no_update"),
    ("trigger", "audit_chain_no_delete"),
    ("trigger", "audit_chain_head_advances_only"),
    ("trigger", "audit_chain_head_no_delete"),
];

/// Creates the audit-chain tables.
///
/// The head row is deliberately *not* seeded: a fresh store has no audit
/// history, and every other table in `runtime.db` starts empty, an invariant
/// the hermetic-provider pre-admission proof asserts table by table. An absent
/// head row reads as genesis, and the first append creates it.
fn create_audit_chain_schema(connection: &Connection) -> Result<(), RuntimeStoreError> {
    connection
        .execute_batch(AUDIT_CHAIN_SCHEMA_DDL)
        .map_err(|source| RuntimeStoreError::operation("create audit chain schema", source))
}

fn verify_v6_schema_for_migration(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let (version, user_version) = read_schema_versions(connection)?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM runtime_schema", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("validate v6 schema row", source))?;
    let digest = schema_digest(connection)?;
    if version != KNOWLEDGE_DATABASE_SCHEMA_VERSION
        || user_version != KNOWLEDGE_DATABASE_SCHEMA_VERSION
        || rows != 1
        || (digest != NATIVE_V6_SCHEMA_DIGEST
            && digest != NATIVE_V3_MIGRATED_V6_SCHEMA_DIGEST
            && digest != NATIVE_V2_MIGRATED_V6_SCHEMA_DIGEST
            && digest != LEGACY_V1_MIGRATED_V6_SCHEMA_DIGEST)
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    verify_required_schema_objects(connection, true)?;
    require_schema_objects(connection, KNOWLEDGE_PUBLICATION_REQUIRED)
}

// Like `migrate_v5_to_v6`, this migration carries no `maybe_crash_during_*`
// injection point: it appends the append-only `audit_chain` tables and one
// genesis head row, rewrites no journal byte, and the single Immediate
// transaction rolls the whole additive DDL back atomically on any crash, so
// there is no partial-migration state to resume.
fn migrate_v6_to_v7(
    connection: &mut Connection,
    boundary_kind: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|source| RuntimeStoreError::operation("begin v7 schema migration", source))?;
    create_audit_chain_schema(&transaction)?;
    transaction
        .execute_batch(
            "UPDATE runtime_schema SET version = 7 WHERE singleton = 1;
             PRAGMA user_version = 7;",
        )
        .map_err(|source| RuntimeStoreError::operation("migrate runtime schema to v7", source))?;
    validate_database(&transaction, boundary_kind)?;
    transaction
        .commit()
        .map_err(|source| RuntimeStoreError::operation("commit v7 schema migration", source))?;
    Ok(())
}

#[cfg(test)]
fn maybe_crash_during_v4_migration(point: &str) {
    match (
        std::env::var("NANIKA_RUNTIME_STORE_V4_MIGRATION_CRASH").as_deref(),
        point,
    ) {
        (Ok("after_schema"), "after_schema") => std::process::exit(98),
        (Ok("after_validation"), "after_validation") => std::process::exit(99),
        _ => {}
    }
}

#[cfg(not(test))]
const fn maybe_crash_during_v4_migration(_point: &str) {}

#[cfg(test)]
fn maybe_crash_during_v3_migration(point: &str) {
    match (
        std::env::var("NANIKA_RUNTIME_STORE_V3_MIGRATION_CRASH").as_deref(),
        point,
    ) {
        (Ok("after_schema"), "after_schema") => std::process::exit(96),
        (Ok("after_validation"), "after_validation") => std::process::exit(97),
        _ => {}
    }
}

#[cfg(not(test))]
const fn maybe_crash_during_v3_migration(_point: &str) {}

#[cfg(test)]
fn maybe_crash_during_migration(point: &str) {
    match (
        std::env::var("NANIKA_RUNTIME_STORE_MIGRATION_CRASH").as_deref(),
        point,
    ) {
        (Ok("after_backfill"), "after_backfill") => std::process::exit(94),
        (Ok("after_validation"), "after_validation") => std::process::exit(95),
        _ => {}
    }
}

#[cfg(not(test))]
const fn maybe_crash_during_migration(_point: &str) {}

fn verify_schema_version(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let version: i64 = connection
        .query_row(
            "SELECT version FROM runtime_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("read runtime schema version", source))?;
    let rows: i64 = connection
        .query_row("SELECT count(*) FROM runtime_schema", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("validate runtime schema row", source))?;
    let user_version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("read database user version", source))?;
    if version != DATABASE_SCHEMA_VERSION || user_version != DATABASE_SCHEMA_VERSION {
        return Err(RuntimeStoreError::UnsupportedSchema {
            found: version.max(user_version),
            expected: DATABASE_SCHEMA_VERSION,
        });
    }
    if rows != 1 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    verify_schema_objects(connection)
}

fn verify_schema_objects(connection: &Connection) -> Result<(), RuntimeStoreError> {
    verify_required_schema_objects(connection, true)?;
    require_schema_objects(connection, KNOWLEDGE_PUBLICATION_REQUIRED)?;
    require_schema_objects(connection, AUDIT_CHAIN_REQUIRED)?;
    let digest = schema_digest(connection)?;
    if digest != NATIVE_V7_SCHEMA_DIGEST
        && digest != NATIVE_V3_MIGRATED_V7_SCHEMA_DIGEST
        && digest != NATIVE_V2_MIGRATED_V7_SCHEMA_DIGEST
        && digest != LEGACY_V1_MIGRATED_V7_SCHEMA_DIGEST
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

/// Asserts each named object exists exactly once in `sqlite_schema`.
fn require_schema_objects(
    connection: &Connection,
    objects: &[(&str, &str)],
) -> Result<(), RuntimeStoreError> {
    for (kind, name) in objects {
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE type = ?1 AND name = ?2",
                params![kind, name],
                |row| row.get(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("validate runtime schema objects", source)
            })?;
        if count != 1 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    Ok(())
}

fn verify_required_schema_objects(
    connection: &Connection,
    require_boundary_trigger: bool,
) -> Result<(), RuntimeStoreError> {
    const REQUIRED: &[(&str, &str)] = &[
        ("table", "runtime_schema"),
        ("table", "journal"),
        ("table", "outbox"),
        ("table", "outbox_attempt_claim"),
        ("table", "outbox_execution_identity"),
        ("table", "outbox_process_intent"),
        ("table", "outbox_process_release_authorization"),
        ("table", "outbox_attempt_observation"),
        ("table", "legacy_retry_authorization_consumption"),
        ("table", "retry_authorization_consumption"),
        ("table", "projection_requirement"),
        ("table", "projection_receipt"),
        ("table", "command_ack"),
        ("table", "projection_cursor"),
        ("index", "outbox_delivery_order"),
        ("index", "outbox_observation_history"),
        ("index", "projection_event_sequence_v2"),
        ("index", "legacy_retry_authorization_decision_lookup"),
        ("trigger", "journal_reject_update"),
        ("trigger", "journal_reject_delete"),
        ("trigger", "outbox_reject_immutable_update"),
        ("trigger", "outbox_reject_operation_slot_update"),
        ("trigger", "outbox_reject_delete"),
        ("trigger", "outbox_attempt_claim_reject_update"),
        ("trigger", "outbox_attempt_claim_reject_delete"),
        ("trigger", "outbox_execution_identity_reject_update"),
        ("trigger", "outbox_execution_identity_reject_delete"),
        ("trigger", "outbox_process_intent_reject_update"),
        ("trigger", "outbox_process_intent_reject_delete"),
        (
            "trigger",
            "outbox_process_release_authorization_reject_update",
        ),
        (
            "trigger",
            "outbox_process_release_authorization_reject_delete",
        ),
        ("trigger", "outbox_attempt_observation_reject_update"),
        ("trigger", "outbox_attempt_observation_reject_delete"),
        (
            "trigger",
            "legacy_retry_authorization_consumption_reject_update",
        ),
        (
            "trigger",
            "legacy_retry_authorization_consumption_reject_delete",
        ),
        ("trigger", "retry_authorization_consumption_reject_update"),
        ("trigger", "retry_authorization_consumption_reject_delete"),
        (
            "trigger",
            "retry_authorization_consumption_reject_reused_decision",
        ),
        ("trigger", "projection_requirement_reject_update"),
        ("trigger", "projection_requirement_reject_delete"),
        ("trigger", "projection_receipt_reject_update"),
        ("trigger", "projection_receipt_reject_delete"),
        ("trigger", "command_ack_reject_update"),
        ("trigger", "command_ack_reject_delete"),
    ];
    require_schema_objects(connection, REQUIRED)?;
    if require_boundary_trigger {
        for name in [
            "runtime_schema_reject_boundary_kind_update",
            "runtime_schema_reject_delete",
            "runtime_schema_reject_insert",
        ] {
            let count: i64 = connection
                .query_row(
                    "SELECT count(*) FROM sqlite_schema
                     WHERE type = 'trigger' AND name = ?1",
                    [name],
                    |row| row.get(0),
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("validate runtime boundary trigger", source)
                })?;
            if count != 1 {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        }
    }
    Ok(())
}

fn schema_digest(connection: &Connection) -> Result<String, RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT type, name, tbl_name, coalesce(sql, '')
             FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare schema digest", source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query schema digest", source))?;
    let mut digest = Sha256::new();
    for row in rows {
        let (kind, name, table, sql) =
            row.map_err(|source| RuntimeStoreError::operation("decode schema digest", source))?;
        digest_field(&mut digest, kind.as_bytes());
        digest_field(&mut digest, name.as_bytes());
        digest_field(&mut digest, table.as_bytes());
        digest_field(&mut digest, sql.as_bytes());
    }
    Ok(hex_digest(digest.finalize().as_slice()))
}

fn validate_database(
    connection: &Connection,
    boundary_kind: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    verify_schema_version(connection)?;
    verify_store_boundary_kind(connection, boundary_kind)?;
    validate_integrity(connection)?;
    validate_checksum_chain(connection)?;
    validate_store_boundary_state(connection, boundary_kind)
}

fn validate_integrity(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let quick_check: String = connection
        .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))
        .map_err(|source| RuntimeStoreError::operation("check runtime database", source))?;
    if quick_check != "ok" {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let foreign_key_violation: Option<i64> = connection
        .query_row(
            "SELECT rowid FROM pragma_foreign_key_check LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("check runtime foreign keys", source))?;
    if foreign_key_violation.is_some() {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

fn verify_store_boundary_kind(
    connection: &Connection,
    expected: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    let stored: String = connection
        .query_row(
            "SELECT boundary_kind FROM runtime_schema WHERE singleton = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    if StoreBoundaryKind::parse(&stored)? != expected {
        return Err(RuntimeStoreError::BoundaryKindMismatch);
    }
    Ok(())
}

fn validate_store_boundary_state(
    connection: &Connection,
    boundary_kind: StoreBoundaryKind,
) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT sequence, transition_id, mission_id, transition_kind, payload_json, extra_json
             FROM journal ORDER BY sequence",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare runtime boundary validation", source)
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|source| {
            RuntimeStoreError::operation("query runtime boundary validation", source)
        })?;
    for row in rows {
        let (sequence, transition_id, mission_id, transition_kind, payload_json, extra_json) = row
            .map_err(|source| {
                RuntimeStoreError::operation("decode runtime boundary row", source)
            })?;
        let requirement_count: i64 = connection
            .query_row(
                "SELECT count(*) FROM projection_requirement
                 WHERE journal_sequence = ?1",
                [sequence],
                |row| row.get(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("count runtime boundary requirements", source)
            })?;
        let extras = validate_stored_json(&extra_json, "record extras")?;
        let extras = extras
            .as_object()
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let private_marker = extras.get(PRIVATE_PROCESS_LEDGER_RECORD_EXTRA_KEY);
        match boundary_kind {
            StoreBoundaryKind::Compatibility => {
                if requirement_count == 0
                    || private_marker.is_some()
                    || is_known_private_transition_kind(&transition_kind)
                {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
            }
            StoreBoundaryKind::PrivateProcessLedger => {
                if mission_id.is_none()
                    || requirement_count != 0
                    || extras.contains_key(EVENT_PROJECTION_RECIPE_EXTRA_KEY)
                    || private_marker != Some(&private_process_ledger_record_marker())
                    || load_command_ack(connection, sequence)?.is_none()
                {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                if transition_kind == TERMINAL_DECISION_TRANSITION_KIND {
                    let payload = validate_stored_json(&payload_json, "terminal decision payload")?;
                    let decision = TerminalDecisionRecord::from_value(&payload)
                        .ok_or(RuntimeStoreError::CorruptDatabase)?;
                    let expected_transition_id = terminal_decision_transition_id(
                        &decision.mission_id,
                        &decision.phase_id,
                        &decision.worker_id,
                        decision.attempt,
                    );
                    if mission_id.as_deref() != Some(decision.mission_id.as_str())
                        || transition_id != expected_transition_id
                    {
                        return Err(RuntimeStoreError::CorruptDatabase);
                    }
                }
            }
        }
    }
    if boundary_kind == StoreBoundaryKind::PrivateProcessLedger {
        for table in [
            "projection_requirement",
            "projection_receipt",
            "projection_cursor",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .map_err(|source| {
                    RuntimeStoreError::operation("validate empty private projection state", source)
                })?;
            if count != 0 {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        }
    }
    Ok(())
}

fn validate_checksum_chain(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT sequence, transition_id, mission_id, record_schema_version,
                    transition_kind, payload_json, committed_at_utc, extra_json,
                    outbox_fingerprint, projection_fingerprint, previous_checksum, checksum
             FROM journal ORDER BY sequence",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare journal validation", source))?;
    let mut rows = statement
        .query([])
        .map_err(|source| RuntimeStoreError::operation("scan journal", source))?;
    let mut expected_sequence = 1_i64;
    let mut previous = GENESIS_CHECKSUM.to_owned();
    while let Some(row) = rows
        .next()
        .map_err(|source| RuntimeStoreError::operation("read journal record", source))?
    {
        let sequence: i64 = row
            .get(0)
            .map_err(|source| RuntimeStoreError::operation("decode journal sequence", source))?;
        let record_version: i64 = row
            .get(3)
            .map_err(|source| RuntimeStoreError::operation("decode record version", source))?;
        if sequence != expected_sequence {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        if !matches!(
            record_version,
            LEGACY_RECORD_SCHEMA_VERSION | RECORD_SCHEMA_VERSION
        ) {
            return Err(RuntimeStoreError::UnsupportedSchema {
                found: record_version,
                expected: RECORD_SCHEMA_VERSION,
            });
        }
        let transition_id: String = row
            .get(1)
            .map_err(|source| RuntimeStoreError::operation("decode transition ID", source))?;
        let mission_id: Option<String> = row
            .get(2)
            .map_err(|source| RuntimeStoreError::operation("decode mission ID", source))?;
        let kind: String = row
            .get(4)
            .map_err(|source| RuntimeStoreError::operation("decode transition kind", source))?;
        let payload_json: String = row
            .get(5)
            .map_err(|source| RuntimeStoreError::operation("decode journal payload", source))?;
        let committed_at_utc: String = row
            .get(6)
            .map_err(|source| RuntimeStoreError::operation("decode commit timestamp", source))?;
        let extra_json: String = row
            .get(7)
            .map_err(|source| RuntimeStoreError::operation("decode record extras", source))?;
        let outbox_fingerprint: String = row
            .get(8)
            .map_err(|source| RuntimeStoreError::operation("decode outbox fingerprint", source))?;
        let projection_fingerprint: String = row.get(9).map_err(|source| {
            RuntimeStoreError::operation("decode projection fingerprint", source)
        })?;
        let stored_previous: String = row
            .get(10)
            .map_err(|source| RuntimeStoreError::operation("decode previous checksum", source))?;
        let checksum: String = row
            .get(11)
            .map_err(|source| RuntimeStoreError::operation("decode journal checksum", source))?;
        validate_atom("transition ID", &transition_id, MAX_IDENTIFIER_BYTES)?;
        validate_atom("transition kind", &kind, MAX_KIND_BYTES)?;
        if let Some(value) = &mission_id {
            MissionId::new(value.clone()).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        }
        validate_timestamp(&committed_at_utc)?;
        validate_stored_json(&payload_json, "journal payload")?;
        let extras = validate_stored_json(&extra_json, "record extras")?;
        if !extras.is_object()
            || !valid_checksum(&outbox_fingerprint)
            || !valid_checksum(&projection_fingerprint)
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        if stored_previous != previous || !valid_checksum(&checksum) {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let prepared = PreparedIntent {
            transition_id,
            mission_id,
            kind,
            payload_json,
            committed_at_utc,
            extra_json,
            outbox_fingerprint,
            projection_fingerprint,
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
            // Publications are not part of the transition checksum: they are
            // derived from the transition, not part of its identity, so a
            // checksum-chain replay reconstructs them as empty.
        };
        if transition_checksum(record_version, sequence, &previous, &prepared) != checksum {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        previous = checksum;
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
    }
    validate_outbox_fingerprints(connection)?;
    validate_outbox_attempt_state(connection)?;
    validate_process_authorization_state(connection)?;
    validate_projection_state(connection)?;
    validate_event_projection_recipes(connection)
}

/// Proves, across every mission, that sealed event-projection recipes form a
/// gapless per-mission public-sequence counter with globally distinct event
/// IDs, and that any receipt already recorded against a recipe-bearing
/// transition attests that exact identity. Runs at open/reopen so tampering
/// with a recipe (or with the receipt that attests it) fails closed instead
/// of silently reproducing a wrong frontier on the next append.
fn validate_event_projection_recipes(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT journal.mission_id, journal.record_schema_version, journal.extra_json,
                    receipt.public_sequence, receipt.event_id
             FROM projection_requirement AS requirement
             JOIN journal ON journal.sequence = requirement.journal_sequence
             LEFT JOIN projection_receipt AS receipt
               ON receipt.journal_sequence = requirement.journal_sequence
              AND receipt.projection = 'event_log'
             WHERE requirement.projection = 'event_log'
             ORDER BY journal.mission_id, journal.sequence",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare event recipe validation", source)
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query event recipe rows", source))?;

    let mut expected_next: BTreeMap<String, i64> = BTreeMap::new();
    let mut seen_event_ids: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        let (mission_id, record_schema_version, extra_json, receipt_sequence, receipt_event_id) =
            row.map_err(|source| RuntimeStoreError::operation("decode event recipe row", source))?;
        let mission_id = mission_id.ok_or(RuntimeStoreError::CorruptDatabase)?;
        if record_schema_version != RECORD_SCHEMA_VERSION {
            // Legacy transitions predate sealed recipes; only their
            // receipted frontier (if any) participates in the mission
            // counter, matching `allocate_event_projection_recipe`.
            if let Some(receipted) = receipt_sequence {
                let expected = expected_next.entry(mission_id).or_insert(1);
                if receipted != *expected {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                *expected = receipted
                    .checked_add(1)
                    .ok_or(RuntimeStoreError::CorruptDatabase)?;
                // Legacy receipted event IDs never went through
                // `allocate_event_projection_recipe`'s collision guard, but
                // they still occupy the same global event-ID namespace a
                // sealed recipe's ID is checked against. Without inserting
                // them here, a tampered (or buggy) sealed recipe could claim
                // a legacy ID and pass this loop undetected.
                if let Some(event_id) = &receipt_event_id {
                    if !seen_event_ids.insert(event_id.clone()) {
                        return Err(RuntimeStoreError::CorruptDatabase);
                    }
                }
            }
            continue;
        }
        let recipe = parse_sealed_recipe(&extra_json).ok_or(RuntimeStoreError::CorruptDatabase)?;
        let expected = expected_next.entry(mission_id).or_insert(1);
        if recipe.public_sequence != *expected {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        if let Some(receipted) = receipt_sequence {
            if receipted != recipe.public_sequence
                || receipt_event_id.as_deref() != Some(recipe.event_id.as_str())
            {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        }
        if !seen_event_ids.insert(recipe.event_id.clone()) {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        *expected = recipe
            .public_sequence
            .checked_add(1)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
    }
    Ok(())
}

fn validate_process_authorization_state(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let mut unbound = connection
        .prepare(
            "SELECT identity.idempotency_key, identity.attempt
             FROM outbox_execution_identity AS identity
             JOIN outbox_process_intent AS intent
               ON intent.idempotency_key = identity.idempotency_key
             LEFT JOIN outbox_process_release_authorization AS authorization
               ON authorization.idempotency_key = identity.idempotency_key
              AND authorization.attempt = identity.attempt
             WHERE authorization.idempotency_key IS NULL
             ORDER BY identity.idempotency_key, identity.attempt",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare blocked process identity validation", source)
        })?;
    let rows = unbound
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(|source| {
            RuntimeStoreError::operation("query blocked process identities", source)
        })?;
    for row in rows {
        let (key, attempt) = row.map_err(|source| {
            RuntimeStoreError::operation("decode blocked process identity", source)
        })?;
        let attempt = u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let mut effect =
            load_effect(connection, &key)?.ok_or(RuntimeStoreError::CorruptDatabase)?;
        effect.attempts = attempt;
        let intent =
            load_process_intent(connection, &key)?.ok_or(RuntimeStoreError::CorruptDatabase)?;
        load_process_recovery_marker(
            connection,
            &effect,
            &intent.request_sha256,
            ProcessRecoveryMarkerStage::SpawnPermitted,
        )?
        .ok_or(RuntimeStoreError::CorruptDatabase)?;
    }

    let mut statement = connection
        .prepare(
            "SELECT idempotency_key, attempt, journal_sequence, mission_id, phase_id,
                    effect_kind, operation_slot, logical_attempt, payload_sha256,
                    request_sha256, pid, process_group_id, process_start_identity,
                    authorized_at_utc
             FROM outbox_process_release_authorization
             ORDER BY idempotency_key, attempt",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare process authorization validation", source)
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
                row.get::<_, String>(12)?,
                row.get::<_, String>(13)?,
            ))
        })
        .map_err(|source| {
            RuntimeStoreError::operation("query process authorization validation", source)
        })?;
    for row in rows {
        let (
            key,
            attempt,
            journal_sequence,
            mission_id,
            phase_id,
            effect_kind,
            operation_slot,
            logical_attempt,
            payload_sha256,
            request_sha256,
            pid,
            process_group_id,
            process_start_identity,
            authorized_at_utc,
        ) = row.map_err(|source| {
            RuntimeStoreError::operation("decode process authorization", source)
        })?;
        validate_timestamp(&authorized_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if !valid_checksum(&payload_sha256) || !valid_checksum(&request_sha256) {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let attempt = u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let logical_attempt =
            u32::try_from(logical_attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let effect = load_effect(connection, &key)?.ok_or(RuntimeStoreError::CorruptDatabase)?;
        let intent =
            load_process_intent(connection, &key)?.ok_or(RuntimeStoreError::CorruptDatabase)?;
        let identity = load_execution_identity(connection, &key, attempt)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let payload_json = canonical_json(&effect.payload, "authorized process payload")?;
        if attempt == 0
            || journal_sequence != effect.journal_sequence
            || mission_id != effect.mission_id.as_str()
            || phase_id.as_deref() != effect.phase_id.as_deref()
            || effect_kind != effect.effect_kind.as_str()
            || operation_slot != effect.operation_slot.as_str()
            || logical_attempt != effect.logical_attempt
            || payload_sha256 != hex_digest(Sha256::digest(payload_json.as_bytes()).as_slice())
            || request_sha256 != intent.request_sha256
            || identity.pid != u32::try_from(pid).map_err(|_| RuntimeStoreError::CorruptDatabase)?
            || identity.process_group_id
                != u32::try_from(process_group_id)
                    .map_err(|_| RuntimeStoreError::CorruptDatabase)?
            || identity.process_start_identity != process_start_identity
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    validate_process_recovery_markers(connection)
}

fn validate_process_recovery_markers(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT transition_id, mission_id, transition_kind, payload_json
             FROM journal
             WHERE transition_kind IN (?1, ?2, ?3)
             ORDER BY sequence",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare process recovery marker validation", source)
        })?;
    let rows = statement
        .query_map(
            params![
                PROCESS_ATTEMPT_CLAIMED_TRANSITION_KIND,
                PROCESS_SPAWN_PERMITTED_TRANSITION_KIND,
                PROCESS_STARTED_OBSERVED_TRANSITION_KIND,
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            },
        )
        .map_err(|source| RuntimeStoreError::operation("query process recovery markers", source))?;
    for row in rows {
        let (transition_id, mission_id, kind, payload_json) = row.map_err(|source| {
            RuntimeStoreError::operation("decode process recovery marker", source)
        })?;
        let stage = ProcessRecoveryMarkerStage::from_transition_kind(&kind)
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        let value = validate_stored_json(&payload_json, "process recovery marker")?;
        let marker = ProcessRecoveryMarker::from_value(stage, &value)?;
        let mut effect = load_effect(connection, &marker.idempotency_key)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        if marker.claim_attempt > effect.attempts
            || transition_id != marker.transition_id()
            || mission_id.as_deref() != Some(marker.mission_id.as_str())
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        effect.attempts = marker.claim_attempt;
        let intent = load_process_intent(connection, &marker.idempotency_key)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        load_process_recovery_marker(connection, &effect, &intent.request_sha256, stage)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
    }
    Ok(())
}

fn validate_outbox_attempt_state(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let mut identities = connection
        .prepare(
            "SELECT idempotency_key, attempt, pid, process_group_id,
                    process_start_identity, recorded_at_utc
             FROM outbox_execution_identity ORDER BY idempotency_key, attempt",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare identity validation", source))?;
    let identity_rows = identities
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query identity validation", source))?;
    for identity in identity_rows {
        let (key, attempt, pid, process_group_id, start, recorded_at) = identity
            .map_err(|source| RuntimeStoreError::operation("decode process identity", source))?;
        validate_atom("outbox idempotency key", &key, MAX_IDENTIFIER_BYTES)
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        ProcessExecutionIdentity::new(
            u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            u32::try_from(pid).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            u32::try_from(process_group_id).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            start,
            recorded_at,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    }

    let mut statement = connection
        .prepare(
            "SELECT idempotency_key, state, attempts, updated_at_utc
             FROM outbox ORDER BY idempotency_key",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare attempt validation", source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query attempt validation", source))?;
    for row in rows {
        let (key, state, attempts, updated_at_utc) =
            row.map_err(|source| RuntimeStoreError::operation("decode attempt summary", source))?;
        validate_timestamp(&updated_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let state = OutboxState::parse(&state)?;
        let attempts = u32::try_from(attempts).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let (claim_count, minimum, maximum): (i64, Option<i64>, Option<i64>) = connection
            .query_row(
                "SELECT count(*), min(attempt), max(attempt)
                 FROM outbox_attempt_claim WHERE idempotency_key = ?1",
                [&key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(|source| RuntimeStoreError::operation("validate attempt claims", source))?;
        if claim_count != i64::from(attempts)
            || (attempts == 0 && (minimum.is_some() || maximum.is_some()))
            || (attempts > 0 && (minimum != Some(1) || maximum != Some(i64::from(attempts))))
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let mut claims = connection
            .prepare(
                "SELECT attempt, claimed_at_utc FROM outbox_attempt_claim
                 WHERE idempotency_key = ?1 ORDER BY attempt",
            )
            .map_err(|source| RuntimeStoreError::operation("prepare attempt claims", source))?;
        let claim_rows = claims
            .query_map([&key], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| RuntimeStoreError::operation("query attempt claims", source))?;
        for claim in claim_rows {
            let (attempt, claimed_at) = claim
                .map_err(|source| RuntimeStoreError::operation("decode attempt claim", source))?;
            if attempt <= 0 {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            validate_timestamp(&claimed_at).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        }

        let mut prior_by_attempt = BTreeMap::<u32, OutboxState>::new();
        let mut observations = connection
            .prepare(
                "SELECT observation_sequence, attempt, evidence_schema_version, observed_state,
                        evidence_code, evidence_json, observed_at_utc
                 FROM outbox_attempt_observation
                 WHERE idempotency_key = ?1 ORDER BY observation_sequence",
            )
            .map_err(|source| {
                RuntimeStoreError::operation("prepare observation validation", source)
            })?;
        let observation_rows = observations
            .query_map([&key], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|source| RuntimeStoreError::operation("query observations", source))?;
        for observation in observation_rows {
            let (
                sequence,
                attempt,
                evidence_schema_version,
                observed_state,
                evidence_code,
                evidence_json,
                observed_at,
            ) = observation
                .map_err(|source| RuntimeStoreError::operation("decode observation", source))?;
            let attempt = u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
            if sequence <= 0 || attempt == 0 || attempt > attempts {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            validate_timestamp(&observed_at).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
            let observed_state = OutboxState::parse(&observed_state)?;
            let prior = prior_by_attempt
                .get(&attempt)
                .copied()
                .unwrap_or(OutboxState::Executing);
            let (evidence, legacy_code) = match evidence_schema_version {
                LEGACY_RECORD_SCHEMA_VERSION => {
                    let legacy = parse_legacy_v1_evidence(&evidence_json)?;
                    if legacy.code != evidence_code {
                        return Err(RuntimeStoreError::CorruptDatabase);
                    }
                    (
                        EffectEvidence::migrated_v1(observed_state, &evidence_json)?,
                        Some(legacy.code),
                    )
                }
                RECORD_SCHEMA_VERSION => {
                    let evidence = EffectEvidence::from_stored_json(&evidence_json)?;
                    if EffectEvidenceCode::parse(&evidence_code)? != evidence.code
                        || !evidence_code_matches_state(observed_state, evidence.code)
                    {
                        return Err(RuntimeStoreError::CorruptDatabase);
                    }
                    (evidence, None)
                }
                found => {
                    return Err(RuntimeStoreError::UnsupportedSchema {
                        found,
                        expected: RECORD_SCHEMA_VERSION,
                    });
                }
            };
            if !valid_observation_transition(
                prior,
                observed_state,
                evidence.code,
                legacy_code.as_deref(),
            ) || (observation_requires_execution_identity(observed_state, evidence.code)
                && load_execution_identity(connection, &key, attempt)?.is_none())
            {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            prior_by_attempt.insert(attempt, observed_state);
        }

        if attempts == 0 {
            if state != OutboxState::Pending || !prior_by_attempt.is_empty() {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        } else {
            let latest = prior_by_attempt.get(&attempts).copied();
            if (state == OutboxState::Executing && latest.is_some())
                || (state != OutboxState::Executing && latest != Some(state))
            {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
        }
    }
    validate_retry_authorization_consumption(connection)
}

const fn evidence_code_matches_state(state: OutboxState, code: EffectEvidenceCode) -> bool {
    matches!(
        (state, code),
        (
            OutboxState::Succeeded,
            EffectEvidenceCode::ExitObservedSuccess
        ) | (
            OutboxState::Failed,
            EffectEvidenceCode::ExitObservedFailure
                | EffectEvidenceCode::ProcessNotStarted
                | EffectEvidenceCode::ProcessFailed
        ) | (
            OutboxState::Uncertain,
            EffectEvidenceCode::ProcessStateUnobservable
                | EffectEvidenceCode::ExitStatusLost
                | EffectEvidenceCode::RemoteStateUnobservable
                | EffectEvidenceCode::ProcessUncertain
        ) | (
            OutboxState::Pending,
            EffectEvidenceCode::ObservedAbsent
                | EffectEvidenceCode::PolicyAuthorizedRetry
                | EffectEvidenceCode::OperatorAuthorizedRetry
        )
    )
}

fn valid_observation_transition(
    prior: OutboxState,
    observed: OutboxState,
    code: EffectEvidenceCode,
    legacy_code: Option<&str>,
) -> bool {
    if let Some(legacy_code) = legacy_code {
        return matches!(
            (prior, observed, code, legacy_code),
            (
                OutboxState::Executing | OutboxState::Uncertain,
                OutboxState::Succeeded,
                EffectEvidenceCode::LegacyV1Succeeded,
                _
            ) | (
                OutboxState::Executing | OutboxState::Uncertain,
                OutboxState::Failed,
                EffectEvidenceCode::LegacyV1Failed,
                _
            ) | (
                OutboxState::Executing,
                OutboxState::Uncertain,
                EffectEvidenceCode::LegacyV1Uncertain,
                _
            ) | (
                OutboxState::Uncertain,
                OutboxState::Pending,
                EffectEvidenceCode::LegacyV1Pending,
                "observed_absent" | "operator_authorized_retry"
            ) | (
                OutboxState::Failed,
                OutboxState::Pending,
                EffectEvidenceCode::LegacyV1Pending,
                "policy_authorized_retry" | "operator_authorized_retry"
            )
        );
    }
    matches!(
        (prior, observed, code),
        (
            OutboxState::Executing | OutboxState::Uncertain,
            OutboxState::Succeeded,
            EffectEvidenceCode::ExitObservedSuccess
        ) | (
            OutboxState::Executing | OutboxState::Uncertain,
            OutboxState::Failed,
            EffectEvidenceCode::ExitObservedFailure | EffectEvidenceCode::ProcessFailed
        ) | (
            OutboxState::Executing,
            OutboxState::Failed,
            EffectEvidenceCode::ProcessNotStarted
        ) | (
            OutboxState::Executing,
            OutboxState::Uncertain,
            EffectEvidenceCode::ProcessStateUnobservable
                | EffectEvidenceCode::ExitStatusLost
                | EffectEvidenceCode::RemoteStateUnobservable
                | EffectEvidenceCode::ProcessUncertain
        ) | (
            OutboxState::Uncertain,
            OutboxState::Pending,
            EffectEvidenceCode::ObservedAbsent | EffectEvidenceCode::OperatorAuthorizedRetry
        ) | (
            OutboxState::Failed,
            OutboxState::Pending,
            EffectEvidenceCode::PolicyAuthorizedRetry
        )
    )
}

const fn observation_requires_execution_identity(
    state: OutboxState,
    code: EffectEvidenceCode,
) -> bool {
    matches!(
        state,
        OutboxState::Succeeded | OutboxState::Failed | OutboxState::Uncertain
    ) && !matches!(
        (state, code),
        (OutboxState::Failed, EffectEvidenceCode::ProcessNotStarted)
            | (OutboxState::Uncertain, EffectEvidenceCode::ProcessUncertain)
    )
}

fn validate_retry_authorization_consumption(
    connection: &Connection,
) -> Result<(), RuntimeStoreError> {
    validate_legacy_retry_authorization_consumption(connection)?;
    validate_current_retry_authorization_consumption(connection)?;
    let cross_schema_collision: i64 = connection
        .query_row(
            "SELECT count(*) FROM retry_authorization_consumption AS current
             JOIN legacy_retry_authorization_consumption AS legacy
               ON legacy.decision_id = current.decision_id",
            [],
            |row| row.get(0),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("validate retry authorization namespaces", source)
        })?;
    if cross_schema_collision != 0 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

fn validate_legacy_retry_authorization_consumption(
    connection: &Connection,
) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT observation_sequence, record_schema_version, decision_id,
                    idempotency_key, attempt, source_state, action, target_state,
                    authority, decided_at_utc, consumed_at_utc
             FROM legacy_retry_authorization_consumption ORDER BY observation_sequence",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare legacy retry validation", source)
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, String>(10)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query legacy retry validation", source))?;
    for row in rows {
        let (
            observation_sequence,
            record_schema_version,
            decision_id,
            idempotency_key,
            attempt,
            source_state,
            action,
            target_state,
            authority,
            decided_at_utc,
            consumed_at_utc,
        ) = row.map_err(|source| {
            RuntimeStoreError::operation("decode legacy retry consumption", source)
        })?;
        let attempt = u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let source_state = OutboxState::parse(&source_state)?;
        let target_state = OutboxState::parse(&target_state)?;
        let action = RetryAction::parse(&action)?;
        let authority = RetryAuthority::parse(&authority)?;
        if observation_sequence <= 0
            || record_schema_version != LEGACY_RECORD_SCHEMA_VERSION
            || attempt == 0
            || target_state != OutboxState::Pending
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        validate_atom(
            "legacy retry idempotency key",
            &idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        validate_timestamp(&consumed_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let observation: (String, i64, i64, String, String, String, String) = connection
            .query_row(
                "SELECT idempotency_key, attempt, evidence_schema_version, observed_state,
                        evidence_code, evidence_json, observed_at_utc
                 FROM outbox_attempt_observation WHERE observation_sequence = ?1",
                [observation_sequence],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .map_err(|source| {
                RuntimeStoreError::operation("load legacy retry observation", source)
            })?;
        let prior_state = load_prior_observation_state(
            connection,
            &idempotency_key,
            attempt,
            observation_sequence,
        )?;
        let evidence = parse_legacy_v1_evidence(&observation.5)?;
        if evidence.code != observation.4 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let bound = bind_legacy_retry_evidence(&evidence, prior_state)?
            .ok_or(RuntimeStoreError::CorruptDatabase)?;
        if observation.0 != idempotency_key
            || u32::try_from(observation.1).ok() != Some(attempt)
            || observation.2 != LEGACY_RECORD_SCHEMA_VERSION
            || OutboxState::parse(&observation.3)? != target_state
            || observation.6 != consumed_at_utc
            || source_state != prior_state
            || bound.authority != authority
            || bound.source_state != source_state
            || bound.action != action
            || bound.decision_id != decision_id
            || bound.decided_at_utc != decided_at_utc
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    let missing_consumption: i64 = connection
        .query_row(
            "SELECT count(*) FROM outbox_attempt_observation AS observation
             LEFT JOIN legacy_retry_authorization_consumption AS consumption
               ON consumption.observation_sequence = observation.observation_sequence
             WHERE observation.evidence_schema_version = 1
               AND observation.evidence_code IN (
                 'policy_authorized_retry','operator_authorized_retry'
               )
               AND consumption.observation_sequence IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("validate legacy retry evidence consumption", source)
        })?;
    if missing_consumption != 0 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

fn validate_current_retry_authorization_consumption(
    connection: &Connection,
) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT decision_id, idempotency_key, attempt, source_state, action,
                    target_state, authority, policy_version, decided_at_utc,
                    consumed_at_utc, observation_sequence
             FROM retry_authorization_consumption ORDER BY observation_sequence",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare retry authorization validation", source)
        })?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
                row.get::<_, String>(9)?,
                row.get::<_, i64>(10)?,
            ))
        })
        .map_err(|source| {
            RuntimeStoreError::operation("query retry authorization validation", source)
        })?;
    for row in rows {
        let (
            decision_id,
            idempotency_key,
            attempt,
            source_state,
            action,
            target_state,
            authority,
            policy_version,
            decided_at_utc,
            consumed_at_utc,
            observation_sequence,
        ) = row.map_err(|source| {
            RuntimeStoreError::operation("decode retry authorization consumption", source)
        })?;
        let attempt = u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let source_state = OutboxState::parse(&source_state)?;
        let target_state = OutboxState::parse(&target_state)?;
        let action = RetryAction::parse(&action)?;
        let authority = RetryAuthority::parse(&authority)?;
        let authorization = RetryAuthorization::new(
            authority,
            idempotency_key.clone(),
            attempt,
            source_state,
            action,
            target_state,
            policy_version,
            decision_id,
            decided_at_utc,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        validate_timestamp(&consumed_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if observation_sequence <= 0 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let observation: (String, i64, i64, String, String, String, String) = connection
            .query_row(
                "SELECT idempotency_key, attempt, evidence_schema_version, observed_state,
                        evidence_code, evidence_json, observed_at_utc
                 FROM outbox_attempt_observation WHERE observation_sequence = ?1",
                [observation_sequence],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .map_err(|source| {
                RuntimeStoreError::operation("load consumed retry observation", source)
            })?;
        let prior_state = load_prior_observation_state(
            connection,
            &idempotency_key,
            attempt,
            observation_sequence,
        )?;
        let evidence = EffectEvidence::from_stored_json(&observation.5)?;
        if observation.0 != idempotency_key
            || u32::try_from(observation.1).ok() != Some(attempt)
            || observation.2 != RECORD_SCHEMA_VERSION
            || OutboxState::parse(&observation.3)? != target_state
            || EffectEvidenceCode::parse(&observation.4)? != evidence.code
            || prior_state != source_state
            || observation.6 != consumed_at_utc
            || evidence != authorization.evidence()
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    let missing_consumption: i64 = connection
        .query_row(
            "SELECT count(*) FROM outbox_attempt_observation AS observation
             LEFT JOIN retry_authorization_consumption AS consumption
               ON consumption.observation_sequence = observation.observation_sequence
             WHERE observation.evidence_schema_version = 2
               AND observation.evidence_code IN (
                 'policy_authorized_retry','operator_authorized_retry'
               )
               AND consumption.observation_sequence IS NULL",
            [],
            |row| row.get(0),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("validate retry evidence consumption", source)
        })?;
    if missing_consumption != 0 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

fn load_prior_observation_state(
    connection: &Connection,
    idempotency_key: &str,
    attempt: u32,
    observation_sequence: i64,
) -> Result<OutboxState, RuntimeStoreError> {
    let prior = connection
        .query_row(
            "SELECT observed_state FROM outbox_attempt_observation
             WHERE idempotency_key = ?1 AND attempt = ?2
               AND observation_sequence < ?3
             ORDER BY observation_sequence DESC LIMIT 1",
            params![idempotency_key, attempt, observation_sequence],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load retry source observation", source))?;
    prior
        .as_deref()
        .map(OutboxState::parse)
        .transpose()
        .map(|state| state.unwrap_or(OutboxState::Executing))
}

fn retry_authorization_consumed(
    connection: &Connection,
    decision_id: &str,
) -> Result<bool, RuntimeStoreError> {
    connection
        .query_row(
            "SELECT EXISTS(
               SELECT 1 FROM retry_authorization_consumption WHERE decision_id = ?1
               UNION ALL
               SELECT 1 FROM legacy_retry_authorization_consumption WHERE decision_id = ?1
             )",
            [decision_id],
            |row| row.get(0),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("inspect retry authorization consumption", source)
        })
}

fn consume_retry_authorization(
    transaction: &Transaction<'_>,
    authorization: &RetryAuthorization,
    observation_sequence: i64,
    consumed_at_utc: &str,
) -> Result<(), RuntimeStoreError> {
    transaction
        .execute(
            "INSERT INTO retry_authorization_consumption (
                decision_id, idempotency_key, attempt, source_state, action,
                target_state, authority, policy_version, decided_at_utc,
                consumed_at_utc, observation_sequence
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                authorization.decision_id,
                authorization.idempotency_key,
                authorization.attempt,
                authorization.source_state.as_str(),
                authorization.action.as_str(),
                authorization.target_state.as_str(),
                authorization.authority.as_str(),
                authorization.policy_version,
                authorization.decided_at_utc,
                consumed_at_utc,
                observation_sequence,
            ],
        )
        .map_err(|source| {
            if is_constraint_violation(&source) {
                RuntimeStoreError::RetryAuthorizationRejected
            } else {
                RuntimeStoreError::operation("consume retry authorization", source)
            }
        })?;
    Ok(())
}

fn validate_outbox_fingerprints(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT sequence, record_schema_version, outbox_fingerprint
             FROM journal ORDER BY sequence",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare outbox validation", source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query outbox fingerprints", source))?;
    for row in rows {
        let (sequence, record_schema_version, expected) = row
            .map_err(|source| RuntimeStoreError::operation("decode outbox fingerprint", source))?;
        let outbox = load_prepared_outbox(connection, sequence)?;
        if outbox_fingerprint_for_version(record_schema_version, &outbox)? != expected {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    Ok(())
}

fn validate_projection_state(connection: &Connection) -> Result<(), RuntimeStoreError> {
    let mut statement = connection
        .prepare("SELECT sequence, projection_fingerprint FROM journal ORDER BY sequence")
        .map_err(|source| RuntimeStoreError::operation("prepare projection validation", source))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|source| RuntimeStoreError::operation("query projection fingerprints", source))?;
    for row in rows {
        let (sequence, expected) = row.map_err(|source| {
            RuntimeStoreError::operation("decode projection fingerprint", source)
        })?;
        let projections = load_required_projections(connection, sequence)?;
        if projection_fingerprint(&projections) != expected {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let pending: i64 = connection
            .query_row(
                "SELECT count(*)
                 FROM projection_requirement AS requirement
                 LEFT JOIN projection_receipt AS receipt
                   ON receipt.journal_sequence = requirement.journal_sequence
                  AND receipt.projection = requirement.projection
                 WHERE requirement.journal_sequence = ?1
                   AND receipt.journal_sequence IS NULL",
                [sequence],
                |row| row.get(0),
            )
            .map_err(|source| {
                RuntimeStoreError::operation("validate pending projections", source)
            })?;
        let acknowledgement = load_command_ack(connection, sequence)?;
        if (pending == 0) != acknowledgement.is_some() {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }

    let mut receipts = connection
        .prepare(
            "SELECT record_schema_version, journal_sequence, projection, mission_id,
                    public_sequence, event_id, event_jsonl, event_sha256, applied_at_utc
             FROM projection_receipt ORDER BY journal_sequence, projection",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare projection receipt validation", source)
        })?;
    let rows = receipts
        .query_map([], projection_receipt_from_row)
        .map_err(|source| RuntimeStoreError::operation("query projection receipts", source))?;
    for row in rows {
        let receipt = row.map_err(|source| {
            RuntimeStoreError::operation("decode projection receipt row", source)
        })??;
        if receipt.projection == CompatibilityProjection::EventLog {
            if receipt.record_schema_version == RECORD_SCHEMA_VERSION {
                validate_event_projection_binding(connection, &receipt)
                    .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
            } else {
                validate_legacy_event_projection_binding(connection, &receipt)?;
                let mission_id = receipt
                    .mission_id
                    .as_deref()
                    .ok_or(RuntimeStoreError::CorruptDatabase)?;
                if next_event_public_sequence(connection, mission_id, receipt.journal_sequence)?
                    != receipt.public_sequence
                {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
            }
        }
    }

    let mut cursors = connection
        .prepare(
            "SELECT projection, journal_sequence, mission_id, public_sequence, event_id,
                    event_sha256, updated_at_utc
             FROM projection_cursor ORDER BY projection",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare cursor validation", source))?;
    let rows = cursors
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query projection cursors", source))?;
    let mut cursor_projections = BTreeSet::new();
    for row in rows {
        let (
            projection,
            sequence,
            mission_id,
            public_sequence,
            event_id,
            event_sha256,
            updated_at_utc,
        ) =
            row.map_err(|source| RuntimeStoreError::operation("decode projection cursor", source))?;
        let projection = CompatibilityProjection::parse(&projection)?;
        if !cursor_projections.insert(projection) {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        validate_timestamp(&updated_at_utc)?;
        let expected = projection_cursor_target(connection, projection)?;
        if sequence != expected.journal_sequence
            || mission_id != expected.mission_id
            || public_sequence != expected.public_sequence
            || event_id != expected.event_id
            || event_sha256 != expected.event_sha256
        {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    let mut applied = connection
        .prepare("SELECT DISTINCT projection FROM projection_receipt ORDER BY projection")
        .map_err(|source| {
            RuntimeStoreError::operation("prepare applied projection query", source)
        })?;
    let rows = applied
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|source| RuntimeStoreError::operation("query applied projection kinds", source))?;
    for row in rows {
        let projection = row.map_err(|source| {
            RuntimeStoreError::operation("decode applied projection kind", source)
        })?;
        if !cursor_projections.contains(&CompatibilityProjection::parse(&projection)?) {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    }
    Ok(())
}

/// Strict record-count and aggregate-byte bounds guarding
/// [`RuntimeStore::projection_recovery_snapshot`]. Exceeding either bound is
/// a typed error, never a truncated result.
pub(crate) struct ProjectionRecoveryBounds {
    max_records: usize,
    max_total_bytes: usize,
}

impl ProjectionRecoveryBounds {
    /// `max_records` is validated against the dedicated
    /// [`MAX_RECOVERY_SNAPSHOT_RECORDS`] ceiling, not [`MAX_QUERY_LIMIT`] —
    /// the two caps guard unrelated things (a full mission journal replay
    /// vs. one page of outbox rows) and must be free to diverge.
    pub(crate) fn new(
        max_records: usize,
        max_total_bytes: usize,
    ) -> Result<Self, RuntimeStoreError> {
        if max_records == 0 || max_records > MAX_RECOVERY_SNAPSHOT_RECORDS {
            return Err(RuntimeStoreError::InvalidIntent(
                "projection recovery record bound is outside its supported range",
            ));
        }
        if max_total_bytes == 0 || max_total_bytes > MAX_RECOVERY_SNAPSHOT_BYTES {
            return Err(RuntimeStoreError::InvalidIntent(
                "projection recovery byte bound is outside its supported range",
            ));
        }
        Ok(Self {
            max_records,
            max_total_bytes,
        })
    }
}

/// One present compatibility-projection receipt surfaced for recovery, bounded
/// and typed rather than the raw stored row.
pub(crate) struct ProjectionRecoveryReceipt {
    projection: CompatibilityProjection,
    mission_id: Option<String>,
    public_sequence: Option<i64>,
    event_id: Option<String>,
    event_sha256: Option<String>,
    applied_at_utc: String,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "staged for the hermetic projector composition (Cell 2D); exercised directly by this file's tests"
    )
)]
impl ProjectionRecoveryReceipt {
    fn from_receipt(receipt: &ProjectionReceipt) -> Self {
        Self {
            projection: receipt.projection,
            mission_id: receipt.mission_id.clone(),
            public_sequence: receipt.public_sequence,
            event_id: receipt.event_id.clone(),
            event_sha256: receipt.event_sha256.clone(),
            applied_at_utc: receipt.applied_at_utc.clone(),
        }
    }

    fn approximate_bytes(&self) -> usize {
        self.mission_id.as_deref().map_or(0, str::len)
            + self.event_id.as_deref().map_or(0, str::len)
            + self.event_sha256.as_deref().map_or(0, str::len)
            + self.applied_at_utc.len()
    }

    pub(crate) const fn projection(&self) -> CompatibilityProjection {
        self.projection
    }

    pub(crate) fn public_sequence(&self) -> Option<i64> {
        self.public_sequence
    }

    pub(crate) fn event_id(&self) -> Option<&str> {
        self.event_id.as_deref()
    }

    pub(crate) fn event_sha256(&self) -> Option<&str> {
        self.event_sha256.as_deref()
    }

    pub(crate) fn applied_at_utc(&self) -> &str {
        &self.applied_at_utc
    }
}

/// One immutable, bounded, typed journal record surfaced for projector
/// recovery. Carries no SQLite connection, filesystem path, or write
/// capability: a projector can only read it.
pub(crate) struct ProjectionRecoveryRecord {
    journal_sequence: i64,
    transition_id: String,
    transition_kind: String,
    payload_json: String,
    committed_at_utc: String,
    extra_json: String,
    required_projections: Vec<CompatibilityProjection>,
    present_receipts: Vec<ProjectionRecoveryReceipt>,
    missing_projections: Vec<CompatibilityProjection>,
    acknowledgement: Option<CommandAcknowledgement>,
    event_projection_recipe: Option<EventProjectionRecipe>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "staged for the hermetic projector composition (Cell 2D); exercised directly by this file's tests"
    )
)]
impl ProjectionRecoveryRecord {
    pub(crate) const fn journal_sequence(&self) -> i64 {
        self.journal_sequence
    }

    pub(crate) fn transition_id(&self) -> &str {
        &self.transition_id
    }

    pub(crate) fn transition_kind(&self) -> &str {
        &self.transition_kind
    }

    pub(crate) fn payload_json(&self) -> &str {
        &self.payload_json
    }

    pub(crate) fn committed_at_utc(&self) -> &str {
        &self.committed_at_utc
    }

    pub(crate) fn extra_json(&self) -> &str {
        &self.extra_json
    }

    pub(crate) fn required_projections(&self) -> &[CompatibilityProjection] {
        &self.required_projections
    }

    pub(crate) fn present_receipts(&self) -> &[ProjectionRecoveryReceipt] {
        &self.present_receipts
    }

    pub(crate) fn missing_projections(&self) -> &[CompatibilityProjection] {
        &self.missing_projections
    }

    pub(crate) fn acknowledgement(&self) -> Option<&CommandAcknowledgement> {
        self.acknowledgement.as_ref()
    }

    pub(crate) fn event_projection_recipe(&self) -> Option<&EventProjectionRecipe> {
        self.event_projection_recipe.as_ref()
    }
}

impl fmt::Debug for ProjectionRecoveryRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectionRecoveryRecord")
            .field("journal_sequence", &self.journal_sequence)
            .field("transition_id_bytes", &self.transition_id.len())
            .field("transition_kind", &self.transition_kind)
            .field("payload_json", &"[REDACTED]")
            .field("extra_json", &"[REDACTED]")
            .field("required_projections", &self.required_projections)
            .field("present_receipt_count", &self.present_receipts.len())
            .field("missing_projections", &self.missing_projections)
            .field("has_acknowledgement", &self.acknowledgement.is_some())
            .field("event_projection_recipe", &self.event_projection_recipe)
            .finish()
    }
}

/// Derived per-mission event frontier: the next public sequence a projector
/// must use for this mission's next event, and the last identity this store
/// has sealed, if any.
#[derive(Clone, Eq, PartialEq)]
pub(crate) struct MissionEventFrontier {
    next_public_sequence: i64,
    last_bound_identity: Option<EventProjectionRecipe>,
}

impl fmt::Debug for MissionEventFrontier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MissionEventFrontier")
            .field("next_public_sequence", &self.next_public_sequence)
            .field("last_bound_identity", &self.last_bound_identity)
            .finish()
    }
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "staged for the hermetic projector composition (Cell 2D); exercised directly by this file's tests"
    )
)]
impl MissionEventFrontier {
    pub(crate) const fn next_public_sequence(&self) -> i64 {
        self.next_public_sequence
    }

    pub(crate) fn last_bound_identity(&self) -> Option<&EventProjectionRecipe> {
        self.last_bound_identity.as_ref()
    }
}

/// Bounded, single-transaction snapshot of one mission's journal-first
/// projection state, returned by
/// [`RuntimeStore::projection_recovery_snapshot`].
pub(crate) struct ProjectionRecoverySnapshot {
    mission_id: MissionId,
    records: Vec<ProjectionRecoveryRecord>,
    frontier: MissionEventFrontier,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "staged for the hermetic projector composition (Cell 2D); exercised directly by this file's tests"
    )
)]
impl ProjectionRecoverySnapshot {
    pub(crate) fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    pub(crate) fn records(&self) -> &[ProjectionRecoveryRecord] {
        &self.records
    }

    pub(crate) fn frontier(&self) -> &MissionEventFrontier {
        &self.frontier
    }
}

impl fmt::Debug for ProjectionRecoverySnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProjectionRecoverySnapshot")
            .field("mission_id", &self.mission_id.as_str())
            .field("record_count", &self.records.len())
            .field("records", &"[REDACTED]")
            .field("frontier", &self.frontier)
            .finish()
    }
}

/// Tri-state per-mission event frontier accumulated while scanning
/// [`build_projection_recovery_snapshot`]'s ordered rows. Distinguishes "no
/// event has ever been required" from "the last required event is a legacy
/// (pre-recipe) transition still awaiting its receipt" — the latter is
/// exactly the historical ambiguity this cell removes, so it fails closed
/// instead of collapsing into the same state as "no events yet".
enum EventFrontierState {
    NotStarted,
    Known(i64),
    AmbiguousLegacyPending,
}

fn build_projection_recovery_snapshot(
    connection: &Connection,
    mission_id: &MissionId,
    bounds: &ProjectionRecoveryBounds,
) -> Result<ProjectionRecoverySnapshot, RuntimeStoreError> {
    let mission = mission_id.as_str();
    let total: i64 = connection
        .query_row(
            "SELECT count(*) FROM journal WHERE mission_id = ?1",
            [mission],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("count mission recovery records", source))?;
    let total = usize::try_from(total).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    if total > bounds.max_records {
        return Err(RuntimeStoreError::RecoveryBoundsExceeded);
    }

    let mut statement = connection
        .prepare(
            "SELECT sequence, transition_id, transition_kind, payload_json, committed_at_utc,
                    extra_json, record_schema_version
             FROM journal WHERE mission_id = ?1 ORDER BY sequence",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare mission recovery scan", source))?;
    let rows = statement
        .query_map([mission], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(|source| RuntimeStoreError::operation("query mission recovery rows", source))?;

    let mut records = Vec::with_capacity(total);
    let mut total_bytes: usize = 0;
    let mut frontier_state = EventFrontierState::NotStarted;
    let mut last_bound_identity: Option<EventProjectionRecipe> = None;

    for row in rows {
        let (
            sequence,
            transition_id,
            transition_kind,
            payload_json,
            committed_at_utc,
            extra_json,
            record_schema_version,
        ) = row.map_err(|source| {
            RuntimeStoreError::operation("decode mission recovery row", source)
        })?;

        total_bytes = total_bytes
            .saturating_add(transition_id.len())
            .saturating_add(transition_kind.len())
            .saturating_add(payload_json.len())
            .saturating_add(committed_at_utc.len())
            .saturating_add(extra_json.len());
        if total_bytes > bounds.max_total_bytes {
            return Err(RuntimeStoreError::RecoveryBoundsExceeded);
        }

        let required_projections = load_required_projections(connection, sequence)?;
        let mut present_receipts = Vec::with_capacity(required_projections.len());
        let mut missing_projections = Vec::new();
        let mut event_log_public_sequence: Option<i64> = None;
        for projection in &required_projections {
            match load_projection_receipt(connection, sequence, *projection)? {
                Some(receipt) => {
                    if *projection == CompatibilityProjection::EventLog {
                        event_log_public_sequence = receipt.public_sequence;
                    }
                    let summary = ProjectionRecoveryReceipt::from_receipt(&receipt);
                    total_bytes = total_bytes.saturating_add(summary.approximate_bytes());
                    if total_bytes > bounds.max_total_bytes {
                        return Err(RuntimeStoreError::RecoveryBoundsExceeded);
                    }
                    present_receipts.push(summary);
                }
                None => missing_projections.push(*projection),
            }
        }

        let acknowledgement = load_command_ack(connection, sequence)?;

        let event_projection_recipe =
            if required_projections.contains(&CompatibilityProjection::EventLog) {
                if record_schema_version == RECORD_SCHEMA_VERSION {
                    let recipe = parse_sealed_recipe(&extra_json)
                        .ok_or(RuntimeStoreError::CorruptDatabase)?;
                    frontier_state = EventFrontierState::Known(recipe.public_sequence);
                    last_bound_identity = Some(recipe.clone());
                    Some(recipe)
                } else {
                    frontier_state = event_log_public_sequence
                        .map_or(EventFrontierState::AmbiguousLegacyPending, |sequence| {
                            EventFrontierState::Known(sequence)
                        });
                    last_bound_identity = None;
                    None
                }
            } else {
                None
            };

        records.push(ProjectionRecoveryRecord {
            journal_sequence: sequence,
            transition_id,
            transition_kind,
            payload_json,
            committed_at_utc,
            extra_json,
            required_projections,
            present_receipts,
            missing_projections,
            acknowledgement,
            event_projection_recipe,
        });
    }

    let next_public_sequence = match frontier_state {
        EventFrontierState::NotStarted => 1,
        EventFrontierState::Known(value) => value
            .checked_add(1)
            .ok_or(RuntimeStoreError::CorruptDatabase)?,
        EventFrontierState::AmbiguousLegacyPending => {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
    };

    Ok(ProjectionRecoverySnapshot {
        mission_id: mission_id.clone(),
        records,
        frontier: MissionEventFrontier {
            next_public_sequence,
            last_bound_identity,
        },
    })
}

fn load_required_projections(
    connection: &Connection,
    sequence: i64,
) -> Result<Vec<CompatibilityProjection>, RuntimeStoreError> {
    let mut statement = connection
        .prepare(
            "SELECT projection FROM projection_requirement
             WHERE journal_sequence = ?1 ORDER BY projection",
        )
        .map_err(|source| {
            RuntimeStoreError::operation("prepare projection requirement query", source)
        })?;
    let rows = statement
        .query_map([sequence], |row| row.get::<_, String>(0))
        .map_err(|source| RuntimeStoreError::operation("query projection requirements", source))?;
    let mut projections = Vec::new();
    for row in rows {
        let projection = row.map_err(|source| {
            RuntimeStoreError::operation("decode projection requirement", source)
        })?;
        projections.push(CompatibilityProjection::parse(&projection)?);
    }
    projections.sort();
    Ok(projections)
}

fn load_projection_receipt(
    connection: &Connection,
    sequence: i64,
    projection: CompatibilityProjection,
) -> Result<Option<ProjectionReceipt>, RuntimeStoreError> {
    connection
        .query_row(
            "SELECT record_schema_version, journal_sequence, projection, mission_id,
                    public_sequence, event_id, event_jsonl, event_sha256, applied_at_utc
             FROM projection_receipt
             WHERE journal_sequence = ?1 AND projection = ?2",
            params![sequence, projection.as_str()],
            projection_receipt_from_row,
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load projection receipt", source))?
        .transpose()
}

fn projection_receipt_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<ProjectionReceipt, RuntimeStoreError>> {
    let record_schema_version: i64 = row.get(0)?;
    let sequence: i64 = row.get(1)?;
    let projection: String = row.get(2)?;
    let mission_id: Option<String> = row.get(3)?;
    let public_sequence: Option<i64> = row.get(4)?;
    let event_id: Option<String> = row.get(5)?;
    let event_jsonl: Option<Vec<u8>> = row.get(6)?;
    let event_sha256: Option<String> = row.get(7)?;
    let applied_at_utc: String = row.get(8)?;
    Ok(ProjectionReceipt::from_stored(ProjectionReceipt {
        record_schema_version,
        journal_sequence: sequence,
        projection: match CompatibilityProjection::parse(&projection) {
            Ok(projection) => projection,
            Err(error) => return Ok(Err(error)),
        },
        mission_id,
        public_sequence,
        event_id,
        event_jsonl,
        event_sha256,
        applied_at_utc,
    }))
}

fn load_command_ack(
    connection: &Connection,
    sequence: i64,
) -> Result<Option<CommandAcknowledgement>, RuntimeStoreError> {
    let row: Option<(i64, String)> = connection
        .query_row(
            "SELECT journal_sequence, acknowledged_at_utc
             FROM command_ack WHERE journal_sequence = ?1",
            [sequence],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load command acknowledgement", source))?;
    row.map(|(journal_sequence, acknowledged_at_utc)| {
        validate_timestamp(&acknowledged_at_utc)?;
        if journal_sequence <= 0 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        Ok(CommandAcknowledgement {
            journal_sequence,
            acknowledged_at_utc,
        })
    })
    .transpose()
}

fn advance_projection_cursor(
    transaction: &Transaction<'_>,
    projection: CompatibilityProjection,
    updated_at_utc: &str,
) -> Result<(), RuntimeStoreError> {
    let target = projection_cursor_target(transaction, projection)?;
    transaction
        .execute(
            "INSERT INTO projection_cursor (
                projection, journal_sequence, mission_id, public_sequence, event_id,
                event_sha256, updated_at_utc
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(projection) DO UPDATE SET
                journal_sequence = excluded.journal_sequence,
                mission_id = excluded.mission_id,
                public_sequence = excluded.public_sequence,
                event_id = excluded.event_id,
                event_sha256 = excluded.event_sha256,
                updated_at_utc = excluded.updated_at_utc",
            params![
                projection.as_str(),
                target.journal_sequence,
                target.mission_id,
                target.public_sequence,
                target.event_id,
                target.event_sha256,
                updated_at_utc,
            ],
        )
        .map_err(|source| RuntimeStoreError::operation("advance projection cursor", source))?;
    Ok(())
}

fn advance_existing_projection_cursors(
    transaction: &Transaction<'_>,
    updated_at_utc: &str,
) -> Result<(), RuntimeStoreError> {
    let projections = {
        let mut statement = transaction
            .prepare("SELECT projection FROM projection_cursor ORDER BY projection")
            .map_err(|source| {
                RuntimeStoreError::operation("prepare existing projection cursors", source)
            })?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|source| {
                RuntimeStoreError::operation("query existing projection cursors", source)
            })?;
        let mut projections = Vec::new();
        for row in rows {
            let projection = row.map_err(|source| {
                RuntimeStoreError::operation("decode existing projection cursor", source)
            })?;
            projections.push(CompatibilityProjection::parse(&projection)?);
        }
        projections
    };
    for projection in projections {
        advance_projection_cursor(transaction, projection, updated_at_utc)?;
    }
    Ok(())
}

fn validate_event_projection_binding(
    transaction: &Connection,
    receipt: &ProjectionReceipt,
) -> Result<(), RuntimeStoreError> {
    let event_jsonl = receipt
        .event_jsonl
        .as_deref()
        .ok_or(RuntimeStoreError::ProjectionConflict)?;
    let mapping =
        canonical_event_mapping(event_jsonl).ok_or(RuntimeStoreError::ProjectionConflict)?;
    let transition: (i64, Option<String>, String, String, String, String) = transaction
        .query_row(
            "SELECT record_schema_version, mission_id, transition_kind,
                    committed_at_utc, payload_json, extra_json
             FROM journal WHERE sequence = ?1",
            [receipt.journal_sequence],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .map_err(|source| {
            RuntimeStoreError::operation("load event projection transition", source)
        })?;
    if transition.0 != RECORD_SCHEMA_VERSION
        || receipt.record_schema_version != RECORD_SCHEMA_VERSION
        || receipt.mission_id.as_deref() != Some(mapping.record.mission_id.as_str())
        || transition.1.as_deref() != Some(mapping.record.mission_id.as_str())
        || transition.2 != mapping.record.event_type
        || transition.3 != mapping.record.timestamp
        || !ExpectedEventProjection::from_journal_payload(&transition.4)
            .map_err(|_| RuntimeStoreError::ProjectionConflict)?
            .matches(&mapping.record)
    {
        return Err(RuntimeStoreError::ProjectionConflict);
    }
    // The receipt attests a projection; it must match the identity the
    // storage actor sealed at append time, not merely be internally
    // consistent. This is journal truth, not a caller assertion.
    let recipe = parse_sealed_recipe(&transition.5).ok_or(RuntimeStoreError::ProjectionConflict)?;
    if recipe.event_id != mapping.event_id || recipe.public_sequence != mapping.public_sequence {
        return Err(RuntimeStoreError::ProjectionConflict);
    }
    // On the live `record_projection` path this is now near-unreachable:
    // recipes pre-seal identity at append time, so two receipts can no
    // longer be minted against different event IDs for the same recipe.
    // It stays load-bearing as defense-in-depth for reopen-time history
    // audits of a tampered database (e.g. a receipt row edited directly to
    // claim another transition's identity) — do not remove it as dead code.
    let duplicate_identity: bool = transaction
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM projection_receipt
                WHERE projection = 'event_log'
                  AND journal_sequence != ?1
                  AND (event_id = ?2 OR event_sha256 = ?3)
             )",
            params![receipt.journal_sequence, &mapping.event_id, &mapping.sha256],
            |row| row.get(0),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("inspect event projection identity", source)
        })?;
    if duplicate_identity {
        return Err(RuntimeStoreError::ProjectionConflict);
    }
    Ok(())
}

fn next_event_public_sequence(
    connection: &Connection,
    mission_id: &str,
    journal_sequence: i64,
) -> Result<Option<i64>, RuntimeStoreError> {
    let prior: Option<Option<i64>> = connection
        .query_row(
            "SELECT receipt.public_sequence
             FROM projection_requirement AS requirement
             JOIN journal ON journal.sequence = requirement.journal_sequence
             LEFT JOIN projection_receipt AS receipt
               ON receipt.journal_sequence = requirement.journal_sequence
              AND receipt.projection = 'event_log'
             WHERE requirement.projection = 'event_log'
               AND journal.mission_id = ?1
               AND requirement.journal_sequence < ?2
             ORDER BY requirement.journal_sequence DESC LIMIT 1",
            params![mission_id, journal_sequence],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load event mission frontier", source))?;
    Ok(match prior {
        None => Some(1),
        Some(None) => None,
        Some(Some(public_sequence)) => public_sequence.checked_add(1),
    })
}

/// Allocates and seals one mission-scoped event-projection identity,
/// transactionally, before the owning journal row is inserted.
///
/// The frontier is derived from the single most recent `event_log`-required
/// journal row for this mission, whether it is still pending or already
/// receipted — closing the gap where a prior sealed-but-unreceipted
/// transition previously left the next sequence unprovable. A missing or
/// malformed recipe on that prior row is a database invariant violation,
/// never a guess.
///
/// Deliberately `fn` (private) rather than an associated constructor on
/// [`EventProjectionRecipe`]: this is the *only* place in the crate allowed
/// to mint a recipe, called from exactly one site inside [`RuntimeStore::append`].
/// A second admission path that allocated sequences elsewhere — even one
/// that looked read-only — would race this one for the same mission
/// frontier and could silently defeat the deterministic-ID collision
/// guarantees `validate_event_projection_recipes` relies on at reopen. If a
/// future caller needs a recipe, it must go through `append`, not a new
/// constructor here.
fn allocate_event_projection_recipe(
    transaction: &Transaction<'_>,
    mission_id: &str,
) -> Result<EventProjectionRecipe, RuntimeStoreError> {
    let previous: Option<(i64, String, Option<i64>)> = transaction
        .query_row(
            "SELECT journal.record_schema_version, journal.extra_json, receipt.public_sequence
             FROM projection_requirement AS requirement
             JOIN journal ON journal.sequence = requirement.journal_sequence
             LEFT JOIN projection_receipt AS receipt
               ON receipt.journal_sequence = requirement.journal_sequence
              AND receipt.projection = 'event_log'
             WHERE requirement.projection = 'event_log'
               AND journal.mission_id = ?1
             ORDER BY requirement.journal_sequence DESC LIMIT 1",
            [mission_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|source| {
            RuntimeStoreError::operation("load event projection recipe frontier", source)
        })?;
    let next_public_sequence = match previous {
        None => 1,
        Some((record_schema_version, extra_json, receipted_sequence))
            if record_schema_version == RECORD_SCHEMA_VERSION =>
        {
            let recipe =
                parse_sealed_recipe(&extra_json).ok_or(RuntimeStoreError::CorruptDatabase)?;
            if receipted_sequence.is_some_and(|sequence| sequence != recipe.public_sequence) {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            recipe
                .public_sequence
                .checked_add(1)
                .ok_or(RuntimeStoreError::CorruptDatabase)?
        }
        Some((_legacy_record_schema_version, _extra_json, receipted_sequence)) => {
            // Legacy transitions predate sealed recipes. An outstanding
            // legacy transition with no receipt yet is exactly the
            // ambiguity this cell removes going forward: fail closed
            // instead of guessing its eventual public sequence.
            receipted_sequence
                .ok_or(RuntimeStoreError::CorruptDatabase)?
                .checked_add(1)
                .ok_or(RuntimeStoreError::CorruptDatabase)?
        }
    };
    Ok(EventProjectionRecipe {
        version: EVENT_PROJECTION_RECIPE_SCHEMA_VERSION,
        event_id: derive_event_projection_id(mission_id, next_public_sequence)?,
        public_sequence: next_public_sequence,
    })
}

/// Builds the domain-separation tag digested into every derived event
/// projection ID, parameterized on the recipe schema version rather than a
/// bare literal — a future bump to [`EVENT_PROJECTION_RECIPE_SCHEMA_VERSION`]
/// therefore changes the digested tag automatically instead of silently
/// deriving identical IDs under a stale literal.
fn event_projection_domain_tag(version: u8) -> String {
    format!("nanika.runtime_store.event_projection_recipe.v{version}")
}

/// Derives one `evt_`-prefixed identifier deterministically from the sealed
/// mission/public-sequence pair rather than drawing new process-wide
/// randomness: the recipe algorithm is versioned and this crate's dependency
/// set has no approved randomness source, so a deterministic derivation
/// keeps allocation reproducible and dependency-free without weakening
/// uniqueness (SHA-256 over a distinct mission/sequence pair per event).
fn derive_event_projection_id(
    mission_id: &str,
    public_sequence: i64,
) -> Result<String, RuntimeStoreError> {
    derive_event_projection_id_with_domain_tag(
        &event_projection_domain_tag(EVENT_PROJECTION_RECIPE_SCHEMA_VERSION),
        mission_id,
        public_sequence,
    )
}

/// Core digest for [`derive_event_projection_id`], taking the domain tag as
/// a parameter so tests can prove the derivation is version-sensitive
/// without mutating the real schema-version constant.
fn derive_event_projection_id_with_domain_tag(
    domain_tag: &str,
    mission_id: &str,
    public_sequence: i64,
) -> Result<String, RuntimeStoreError> {
    let mut digest = Sha256::new();
    digest_field(&mut digest, domain_tag.as_bytes());
    digest_field(&mut digest, mission_id.as_bytes());
    digest_field(&mut digest, &public_sequence.to_be_bytes());
    let candidate = format!("evt_{}", &hex_digest(digest.finalize().as_slice())[..16]);
    EventId::new(&candidate).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    Ok(candidate)
}

fn validate_legacy_event_projection_binding(
    connection: &Connection,
    receipt: &ProjectionReceipt,
) -> Result<(), RuntimeStoreError> {
    if receipt.record_schema_version != LEGACY_RECORD_SCHEMA_VERSION
        || receipt.event_jsonl.is_some()
        || receipt.event_sha256.is_some()
    {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let transition: (i64, Option<String>) = connection
        .query_row(
            "SELECT record_schema_version, mission_id FROM journal WHERE sequence = ?1",
            [receipt.journal_sequence],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("load legacy event projection transition", source)
        })?;
    if transition.0 != LEGACY_RECORD_SCHEMA_VERSION || transition.1 != receipt.mission_id {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(())
}

struct ProjectionCursorTarget {
    journal_sequence: i64,
    mission_id: Option<String>,
    public_sequence: Option<i64>,
    event_id: Option<String>,
    event_sha256: Option<String>,
}

fn projection_cursor_target(
    connection: &Connection,
    projection: CompatibilityProjection,
) -> Result<ProjectionCursorTarget, RuntimeStoreError> {
    let maximum: i64 = connection
        .query_row(
            "SELECT coalesce(max(sequence), 0) FROM journal",
            [],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("read journal maximum", source))?;
    let first_pending: Option<i64> = connection
        .query_row(
            "SELECT min(requirement.journal_sequence)
             FROM projection_requirement AS requirement
             LEFT JOIN projection_receipt AS receipt
               ON receipt.journal_sequence = requirement.journal_sequence
              AND receipt.projection = requirement.projection
             WHERE requirement.projection = ?1
               AND receipt.journal_sequence IS NULL",
            [projection.as_str()],
            |row| row.get(0),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("find pending projection frontier", source)
        })?;
    let sequence = first_pending.map_or(maximum, |pending| pending.saturating_sub(1));
    if projection != CompatibilityProjection::EventLog {
        return Ok(ProjectionCursorTarget {
            journal_sequence: sequence,
            mission_id: None,
            public_sequence: None,
            event_id: None,
            event_sha256: None,
        });
    }
    let mapping: Option<(String, i64, String, Option<String>)> = connection
        .query_row(
            "SELECT mission_id, public_sequence, event_id, event_sha256 FROM projection_receipt
             WHERE projection = 'event_log' AND journal_sequence <= ?1
             ORDER BY journal_sequence DESC LIMIT 1",
            [sequence],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("read event projection frontier", source))?;
    let (mission_id, public_sequence, event_id, event_sha256) = match mapping {
        Some((mission_id, public_sequence, event_id, event_sha256)) => (
            Some(mission_id),
            Some(public_sequence),
            Some(event_id),
            event_sha256,
        ),
        None => (None, None, None, None),
    };
    Ok(ProjectionCursorTarget {
        journal_sequence: sequence,
        mission_id,
        public_sequence,
        event_id,
        event_sha256,
    })
}

fn load_existing_transition(
    transaction: &Transaction<'_>,
    transition_id: &str,
) -> Result<Option<ExistingTransition>, RuntimeStoreError> {
    transaction
        .query_row(
            "SELECT sequence, checksum, mission_id, transition_kind, payload_json,
                    committed_at_utc, extra_json, outbox_fingerprint, projection_fingerprint
             FROM journal WHERE transition_id = ?1",
            [transition_id],
            |row| {
                Ok(ExistingTransition {
                    sequence: row.get(0)?,
                    checksum: row.get(1)?,
                    mission_id: row.get(2)?,
                    kind: row.get(3)?,
                    payload_json: row.get(4)?,
                    committed_at_utc: row.get(5)?,
                    extra_json: row.get(6)?,
                    outbox_fingerprint: row.get(7)?,
                    projection_fingerprint: row.get(8)?,
                })
            },
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("look up transition retry", source))
}

fn next_journal_position(
    transaction: &Transaction<'_>,
) -> Result<(i64, String), RuntimeStoreError> {
    let tail: Option<(i64, String)> = transaction
        .query_row(
            "SELECT sequence, checksum FROM journal ORDER BY sequence DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("read journal tail", source))?;
    match tail {
        Some((sequence, checksum)) => Ok((
            sequence
                .checked_add(1)
                .ok_or(RuntimeStoreError::CorruptDatabase)?,
            checksum,
        )),
        None => Ok((1, GENESIS_CHECKSUM.to_owned())),
    }
}

fn transition_checksum(
    record_schema_version: i64,
    sequence: i64,
    previous: &str,
    intent: &PreparedIntent,
) -> String {
    let mut digest = Sha256::new();
    digest_field(&mut digest, &record_schema_version.to_be_bytes());
    digest_field(&mut digest, &sequence.to_be_bytes());
    digest_field(&mut digest, previous.as_bytes());
    digest_field(&mut digest, intent.transition_id.as_bytes());
    match &intent.mission_id {
        Some(value) => {
            digest_field(&mut digest, &[1]);
            digest_field(&mut digest, value.as_bytes());
        }
        None => digest_field(&mut digest, &[0]),
    }
    digest_field(&mut digest, intent.kind.as_bytes());
    digest_field(&mut digest, intent.payload_json.as_bytes());
    digest_field(&mut digest, intent.committed_at_utc.as_bytes());
    digest_field(&mut digest, intent.extra_json.as_bytes());
    digest_field(&mut digest, intent.outbox_fingerprint.as_bytes());
    digest_field(&mut digest, intent.projection_fingerprint.as_bytes());
    hex_digest(digest.finalize().as_slice())
}

fn digest_field(digest: &mut Sha256, bytes: &[u8]) {
    digest.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_be_bytes());
    digest.update(bytes);
}

fn outbox_fingerprint(outbox: &[PreparedOutbox]) -> Result<String, RuntimeStoreError> {
    let bytes = serde_json::to_vec(outbox)
        .map_err(|source| RuntimeStoreError::operation("encode outbox fingerprint", source))?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(RuntimeStoreError::InvalidIntent(
            "combined outbox metadata exceeds the JSON limit",
        ));
    }
    Ok(hex_digest(Sha256::digest(bytes).as_slice()))
}

fn outbox_fingerprint_for_version(
    record_schema_version: i64,
    outbox: &[PreparedOutbox],
) -> Result<String, RuntimeStoreError> {
    if record_schema_version == RECORD_SCHEMA_VERSION {
        return outbox_fingerprint(outbox);
    }
    if record_schema_version != LEGACY_RECORD_SCHEMA_VERSION {
        return Err(RuntimeStoreError::UnsupportedSchema {
            found: record_schema_version,
            expected: RECORD_SCHEMA_VERSION,
        });
    }
    let legacy = outbox
        .iter()
        .map(|effect| LegacyPreparedOutbox {
            idempotency_key: &effect.idempotency_key,
            mission_id: &effect.mission_id,
            phase_id: effect.phase_id.as_deref(),
            effect_kind: &effect.effect_kind,
            logical_attempt: effect.logical_attempt,
            payload_json: &effect.payload_json,
        })
        .collect::<Vec<_>>();
    let bytes = serde_json::to_vec(&legacy).map_err(|source| {
        RuntimeStoreError::operation("encode legacy outbox fingerprint", source)
    })?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(hex_digest(Sha256::digest(bytes).as_slice()))
}

fn effect_idempotency_key(
    mission_id: &MissionId,
    phase_id: Option<&str>,
    effect_kind: OutboxEffectKind,
    operation_slot: &EffectOperationSlot,
    logical_attempt: u32,
) -> String {
    let mut digest = Sha256::new();
    digest_field(&mut digest, b"nanika-effect-id/v2");
    digest_field(&mut digest, mission_id.as_str().as_bytes());
    match phase_id {
        Some(value) => {
            digest_field(&mut digest, &[1]);
            digest_field(&mut digest, value.as_bytes());
        }
        None => digest_field(&mut digest, &[0]),
    }
    digest_field(&mut digest, effect_kind.as_str().as_bytes());
    digest_field(&mut digest, operation_slot.as_str().as_bytes());
    digest_field(&mut digest, &logical_attempt.to_be_bytes());
    format!("effect_{}", hex_digest(digest.finalize().as_slice()))
}

fn process_effect_idempotency_key(
    mission_id: &MissionId,
    phase_id: Option<&str>,
    effect_kind: OutboxEffectKind,
    operation_slot: &EffectOperationSlot,
    logical_attempt: u32,
    request_sha256: &str,
) -> String {
    let mut digest = Sha256::new();
    digest_field(&mut digest, b"nanika-process-effect-id/v1");
    digest_field(&mut digest, mission_id.as_str().as_bytes());
    match phase_id {
        Some(value) => {
            digest_field(&mut digest, &[1]);
            digest_field(&mut digest, value.as_bytes());
        }
        None => digest_field(&mut digest, &[0]),
    }
    digest_field(&mut digest, effect_kind.as_str().as_bytes());
    digest_field(&mut digest, operation_slot.as_str().as_bytes());
    digest_field(&mut digest, &logical_attempt.to_be_bytes());
    digest_field(&mut digest, request_sha256.as_bytes());
    format!("effect_{}", hex_digest(digest.finalize().as_slice()))
}

fn validate_process_effect_purpose(
    effect_kind: OutboxEffectKind,
    purpose: ProcessPurpose,
) -> Result<(), RuntimeStoreError> {
    let matches = matches!(
        (effect_kind, purpose),
        (
            OutboxEffectKind::ProviderProcess,
            ProcessPurpose::ProviderWorker
        ) | (
            OutboxEffectKind::ProviderProcess,
            ProcessPurpose::Verification
        ) | (OutboxEffectKind::GitCommand, ProcessPurpose::Git)
            | (OutboxEffectKind::PluginProcess, ProcessPurpose::Plugin)
    );
    if matches {
        Ok(())
    } else {
        Err(RuntimeStoreError::InvalidIntent(
            "process request purpose does not match its finite outbox effect kind",
        ))
    }
}

#[cfg(unix)]
fn process_effect_immutable_fields_match(left: &OutboxEffect, right: &OutboxEffect) -> bool {
    left.idempotency_key == right.idempotency_key
        && left.journal_sequence == right.journal_sequence
        && left.mission_id == right.mission_id
        && left.phase_id == right.phase_id
        && left.effect_kind == right.effect_kind
        && left.operation_slot == right.operation_slot
        && left.logical_attempt == right.logical_attempt
        && left.payload == right.payload
}

#[cfg(unix)]
fn claimed_process_matches_snapshot(claimed: &ClaimedOutboxEffect, prior: &OutboxEffect) -> bool {
    claimed.idempotency_key == prior.idempotency_key
        && claimed.journal_sequence == prior.journal_sequence
        && claimed.mission_id == prior.mission_id
        && claimed.phase_id == prior.phase_id
        && claimed.effect_kind == prior.effect_kind
        && claimed.operation_slot == prior.operation_slot
        && claimed.logical_attempt == prior.logical_attempt
        && claimed.payload == prior.payload
}

fn claimed_process_matches_effect(claimed: &ClaimedOutboxEffect, current: &OutboxEffect) -> bool {
    claimed.idempotency_key == current.idempotency_key
        && claimed.journal_sequence == current.journal_sequence
        && claimed.mission_id == current.mission_id
        && claimed.phase_id == current.phase_id
        && claimed.effect_kind == current.effect_kind
        && claimed.operation_slot == current.operation_slot
        && claimed.logical_attempt == current.logical_attempt
        && claimed.payload == current.payload
}

fn legacy_effect_idempotency_key(
    mission_id: &MissionId,
    phase_id: Option<&str>,
    effect_kind: OutboxEffectKind,
    logical_attempt: u32,
) -> String {
    let mut digest = Sha256::new();
    digest_field(&mut digest, mission_id.as_str().as_bytes());
    match phase_id {
        Some(value) => {
            digest_field(&mut digest, &[1]);
            digest_field(&mut digest, value.as_bytes());
        }
        None => digest_field(&mut digest, &[0]),
    }
    digest_field(&mut digest, effect_kind.as_str().as_bytes());
    digest_field(&mut digest, &logical_attempt.to_be_bytes());
    format!("effect_{}", hex_digest(digest.finalize().as_slice()))
}

fn projection_fingerprint(projections: &[CompatibilityProjection]) -> String {
    let mut digest = Sha256::new();
    for projection in projections {
        digest_field(&mut digest, projection.as_str().as_bytes());
    }
    hex_digest(digest.finalize().as_slice())
}

pub(crate) fn hex_digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn valid_checksum(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn load_prepared_outbox(
    connection: &Connection,
    sequence: i64,
) -> Result<Vec<PreparedOutbox>, RuntimeStoreError> {
    let record_schema_version: i64 = connection
        .query_row(
            "SELECT record_schema_version FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )
        .map_err(|source| RuntimeStoreError::operation("read outbox record version", source))?;
    let mut statement = connection
        .prepare(
            "SELECT outbox.idempotency_key, outbox.mission_id, outbox.phase_id,
                    outbox.effect_kind, outbox.operation_slot, outbox.logical_attempt,
                    outbox.payload_json, process_intent.intent_json
             FROM outbox
             LEFT JOIN outbox_process_intent AS process_intent
               ON process_intent.idempotency_key = outbox.idempotency_key
             WHERE outbox.journal_sequence = ?1 ORDER BY outbox.idempotency_key",
        )
        .map_err(|source| RuntimeStoreError::operation("prepare immutable outbox query", source))?;
    let rows = statement
        .query_map([sequence], |row| {
            Ok(PreparedOutbox {
                idempotency_key: row.get(0)?,
                mission_id: row.get(1)?,
                phase_id: row.get(2)?,
                effect_kind: row.get(3)?,
                operation_slot: row.get(4)?,
                logical_attempt: row.get(5)?,
                payload_json: row.get(6)?,
                process_request_sha256: None,
                process_intent_json: row.get(7)?,
            })
        })
        .map_err(|source| RuntimeStoreError::operation("query immutable outbox rows", source))?;
    let mut outbox = Vec::new();
    for row in rows {
        let mut effect = row.map_err(|source| {
            RuntimeStoreError::operation("decode immutable outbox row", source)
        })?;
        validate_atom(
            "outbox idempotency key",
            &effect.idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        let mission_id = MissionId::new(effect.mission_id.clone())
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if let Some(value) = effect.phase_id.as_deref() {
            validate_atom("outbox phase ID", value, MAX_IDENTIFIER_BYTES)
                .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        }
        let effect_kind = OutboxEffectKind::parse(&effect.effect_kind)?;
        let operation_slot = EffectOperationSlot::new(effect.operation_slot.clone())
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let process_intent = effect
            .process_intent_json
            .as_deref()
            .map(ProcessIntentEnvelope::from_stored)
            .transpose()?;
        effect.process_request_sha256 = process_intent
            .as_ref()
            .map(|intent| intent.request_sha256.clone());
        let expected_key = match (record_schema_version, process_intent.as_ref()) {
            (LEGACY_RECORD_SCHEMA_VERSION, None) => legacy_effect_idempotency_key(
                &mission_id,
                effect.phase_id.as_deref(),
                effect_kind,
                effect.logical_attempt,
            ),
            (RECORD_SCHEMA_VERSION, None) => effect_idempotency_key(
                &mission_id,
                effect.phase_id.as_deref(),
                effect_kind,
                &operation_slot,
                effect.logical_attempt,
            ),
            (RECORD_SCHEMA_VERSION, Some(process_intent)) => process_effect_idempotency_key(
                &mission_id,
                effect.phase_id.as_deref(),
                effect_kind,
                &operation_slot,
                effect.logical_attempt,
                &process_intent.request_sha256,
            ),
            (LEGACY_RECORD_SCHEMA_VERSION, Some(_)) => {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            (found, _) => {
                return Err(RuntimeStoreError::UnsupportedSchema {
                    found,
                    expected: RECORD_SCHEMA_VERSION,
                });
            }
        };
        if effect.logical_attempt == 0 || expected_key != effect.idempotency_key {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        validate_stored_json(&effect.payload_json, "outbox payload")?;
        outbox.push(effect);
    }
    Ok(outbox)
}

fn load_effect(
    connection: &Connection,
    idempotency_key: &str,
) -> Result<Option<OutboxEffect>, RuntimeStoreError> {
    connection
        .query_row(
            "SELECT outbox.idempotency_key, outbox.journal_sequence,
                    outbox.mission_id, outbox.phase_id, outbox.effect_kind,
                    outbox.operation_slot, outbox.logical_attempt, outbox.payload_json,
                    outbox.state, outbox.attempts, outbox.updated_at_utc,
                    identity.attempt, identity.pid, identity.process_group_id,
                    identity.process_start_identity, identity.recorded_at_utc,
                    journal.record_schema_version, process_intent.intent_json
             FROM outbox
             JOIN journal ON journal.sequence = outbox.journal_sequence
             LEFT JOIN outbox_execution_identity AS identity
               ON identity.idempotency_key = outbox.idempotency_key
              AND identity.attempt = outbox.attempts
             LEFT JOIN outbox_process_intent AS process_intent
               ON process_intent.idempotency_key = outbox.idempotency_key
             WHERE outbox.idempotency_key = ?1",
            [idempotency_key],
            effect_from_row,
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load outbox effect", source))?
        .transpose()
}

fn load_exact_outbox_snapshot(
    connection: &Connection,
    idempotency_key: &str,
) -> Result<Option<ExactOutboxSnapshot>, RuntimeStoreError> {
    let Some(effect) = load_effect(connection, idempotency_key)? else {
        return Ok(None);
    };
    let current_observation = if effect.attempts == 0 {
        None
    } else {
        load_latest_observation(connection, idempotency_key, effect.attempts)?
    };
    let coherent = match (effect.state, effect.attempts, current_observation.as_ref()) {
        (OutboxState::Pending, 0, None) => true,
        (OutboxState::Executing, attempt, None) => attempt > 0,
        (
            OutboxState::Pending
            | OutboxState::Succeeded
            | OutboxState::Failed
            | OutboxState::Uncertain,
            attempt,
            Some(observation),
        ) => attempt > 0 && observation.attempt == attempt && observation.state == effect.state,
        _ => false,
    };
    if !coherent {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(Some(ExactOutboxSnapshot {
        effect,
        current_observation,
    }))
}

fn next_claimable_effect_key(connection: &Connection) -> Result<Option<String>, RuntimeStoreError> {
    connection
        .query_row(
            "SELECT outbox.idempotency_key FROM outbox
             INNER JOIN command_ack
               ON command_ack.journal_sequence = outbox.journal_sequence
             WHERE outbox.state = 'pending'
             ORDER BY outbox.journal_sequence, outbox.idempotency_key LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("select pending outbox effect", source))
}

fn claim_pending_effect(
    transaction: &Transaction<'_>,
    idempotency_key: &str,
    updated_at_utc: &str,
) -> Result<OutboxEffect, RuntimeStoreError> {
    let changed = transaction
        .execute(
            "UPDATE outbox
             SET state = 'executing', attempts = attempts + 1, updated_at_utc = ?1
             WHERE idempotency_key = ?2 AND state = 'pending'",
            params![updated_at_utc, idempotency_key],
        )
        .map_err(|source| RuntimeStoreError::operation("claim pending outbox effect", source))?;
    if changed != 1 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let claim_inserted = transaction
        .execute(
            "INSERT INTO outbox_attempt_claim (idempotency_key, attempt, claimed_at_utc)
             SELECT idempotency_key, attempts, ?1 FROM outbox
             WHERE idempotency_key = ?2 AND state = 'executing'",
            params![updated_at_utc, idempotency_key],
        )
        .map_err(|source| RuntimeStoreError::operation("record outbox attempt claim", source))?;
    if claim_inserted != 1 {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    load_effect(transaction, idempotency_key)?.ok_or(RuntimeStoreError::CorruptDatabase)
}

fn load_execution_identity(
    connection: &Connection,
    idempotency_key: &str,
    attempt: u32,
) -> Result<Option<ProcessExecutionIdentity>, RuntimeStoreError> {
    let row: Option<(i64, i64, i64, String, String)> = connection
        .query_row(
            "SELECT attempt, pid, process_group_id, process_start_identity, recorded_at_utc
             FROM outbox_execution_identity
             WHERE idempotency_key = ?1 AND attempt = ?2",
            params![idempotency_key, attempt],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load process identity", source))?;
    row.map(|(attempt, pid, process_group_id, start, recorded_at)| {
        ProcessExecutionIdentity::new(
            u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            u32::try_from(pid).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            u32::try_from(process_group_id).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            start,
            recorded_at,
        )
        .map_err(|_| RuntimeStoreError::CorruptDatabase)
    })
    .transpose()
}

fn load_process_intent(
    connection: &Connection,
    idempotency_key: &str,
) -> Result<Option<ProcessIntentEnvelope>, RuntimeStoreError> {
    let value = connection
        .query_row(
            "SELECT intent_json FROM outbox_process_intent WHERE idempotency_key = ?1",
            [idempotency_key],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load process intent", source))?;
    value
        .as_deref()
        .map(ProcessIntentEnvelope::from_stored)
        .transpose()
}

fn process_release_authorization_exists(
    connection: &Connection,
    idempotency_key: &str,
    attempt: u32,
) -> Result<bool, RuntimeStoreError> {
    let count: i64 = connection
        .query_row(
            "SELECT count(*) FROM outbox_process_release_authorization
             WHERE idempotency_key = ?1 AND attempt = ?2",
            params![idempotency_key, attempt],
            |row| row.get(0),
        )
        .map_err(|source| {
            RuntimeStoreError::operation("inspect process release authorization", source)
        })?;
    match count {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(RuntimeStoreError::CorruptDatabase),
    }
}

struct StoredProcessReleaseAuthorization {
    journal_sequence: i64,
    mission_id: String,
    phase_id: Option<String>,
    effect_kind: String,
    operation_slot: String,
    logical_attempt: u32,
    payload_sha256: String,
    request_sha256: String,
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
    authorized_at_utc: String,
}

struct StoredProcessReleaseAuthorizationRow {
    journal_sequence: i64,
    mission_id: String,
    phase_id: Option<String>,
    effect_kind: String,
    operation_slot: String,
    logical_attempt: i64,
    payload_sha256: String,
    request_sha256: String,
    pid: i64,
    process_group_id: i64,
    process_start_identity: String,
    authorized_at_utc: String,
}

fn load_process_release_authorization(
    connection: &Connection,
    idempotency_key: &str,
    attempt: u32,
) -> Result<Option<StoredProcessReleaseAuthorization>, RuntimeStoreError> {
    let row: Option<StoredProcessReleaseAuthorizationRow> = connection
        .query_row(
            "SELECT journal_sequence, mission_id, phase_id, effect_kind,
                    operation_slot, logical_attempt, payload_sha256,
                    request_sha256, pid, process_group_id,
                    process_start_identity, authorized_at_utc
             FROM outbox_process_release_authorization
             WHERE idempotency_key = ?1 AND attempt = ?2",
            params![idempotency_key, attempt],
            |row| {
                Ok(StoredProcessReleaseAuthorizationRow {
                    journal_sequence: row.get(0)?,
                    mission_id: row.get(1)?,
                    phase_id: row.get(2)?,
                    effect_kind: row.get(3)?,
                    operation_slot: row.get(4)?,
                    logical_attempt: row.get(5)?,
                    payload_sha256: row.get(6)?,
                    request_sha256: row.get(7)?,
                    pid: row.get(8)?,
                    process_group_id: row.get(9)?,
                    process_start_identity: row.get(10)?,
                    authorized_at_utc: row.get(11)?,
                })
            },
        )
        .optional()
        .map_err(|source| {
            RuntimeStoreError::operation("load process release authorization", source)
        })?;
    row.map(|row| {
        Ok(StoredProcessReleaseAuthorization {
            journal_sequence: row.journal_sequence,
            mission_id: row.mission_id,
            phase_id: row.phase_id,
            effect_kind: row.effect_kind,
            operation_slot: row.operation_slot,
            logical_attempt: u32::try_from(row.logical_attempt)
                .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            payload_sha256: row.payload_sha256,
            request_sha256: row.request_sha256,
            pid: u32::try_from(row.pid).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            process_group_id: u32::try_from(row.process_group_id)
                .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            process_start_identity: row.process_start_identity,
            authorized_at_utc: row.authorized_at_utc,
        })
    })
    .transpose()
}

fn load_latest_observation(
    connection: &Connection,
    idempotency_key: &str,
    attempt: u32,
) -> Result<Option<EffectObservation>, RuntimeStoreError> {
    connection
        .query_row(
            "SELECT observation_sequence, attempt, evidence_schema_version, observed_state,
                    evidence_code, evidence_json, observed_at_utc
             FROM outbox_attempt_observation
             WHERE idempotency_key = ?1 AND attempt = ?2
             ORDER BY observation_sequence DESC LIMIT 1",
            params![idempotency_key, attempt],
            observation_from_row,
        )
        .optional()
        .map_err(|source| RuntimeStoreError::operation("load latest outbox observation", source))?
        .transpose()
}

fn observation_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<EffectObservation, RuntimeStoreError>> {
    let sequence: i64 = row.get(0)?;
    let attempt: i64 = row.get(1)?;
    let evidence_schema_version: i64 = row.get(2)?;
    let state: String = row.get(3)?;
    let evidence_code: String = row.get(4)?;
    let evidence_json: String = row.get(5)?;
    let observed_at_utc: String = row.get(6)?;
    Ok((|| {
        if sequence <= 0 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let attempt = u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if attempt == 0 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        validate_timestamp(&observed_at_utc).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let state = OutboxState::parse(&state)?;
        let evidence = match evidence_schema_version {
            LEGACY_RECORD_SCHEMA_VERSION => {
                let legacy = parse_legacy_v1_evidence(&evidence_json)?;
                if legacy.code != evidence_code {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                EffectEvidence::migrated_v1(state, &evidence_json)?
            }
            RECORD_SCHEMA_VERSION => {
                let evidence = EffectEvidence::from_stored_json(&evidence_json)?;
                if evidence.code.as_str() != evidence_code {
                    return Err(RuntimeStoreError::CorruptDatabase);
                }
                evidence
            }
            found => {
                return Err(RuntimeStoreError::UnsupportedSchema {
                    found,
                    expected: RECORD_SCHEMA_VERSION,
                });
            }
        };
        Ok(EffectObservation {
            sequence,
            attempt,
            state,
            evidence,
            observed_at_utc,
        })
    })())
}

fn effect_from_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<OutboxEffect, RuntimeStoreError>> {
    let idempotency_key: String = row.get(0)?;
    let journal_sequence: i64 = row.get(1)?;
    let mission_id: String = row.get(2)?;
    let phase_id: Option<String> = row.get(3)?;
    let effect_kind: String = row.get(4)?;
    let operation_slot: String = row.get(5)?;
    let logical_attempt: i64 = row.get(6)?;
    let payload_json: String = row.get(7)?;
    let state_text: String = row.get(8)?;
    let attempts: i64 = row.get(9)?;
    let updated_at_utc: String = row.get(10)?;
    let identity_attempt: Option<i64> = row.get(11)?;
    let identity_pid: Option<i64> = row.get(12)?;
    let identity_process_group_id: Option<i64> = row.get(13)?;
    let identity_process_start: Option<String> = row.get(14)?;
    let identity_recorded_at: Option<String> = row.get(15)?;
    let record_schema_version: i64 = row.get(16)?;
    let process_intent_json: Option<String> = row.get(17)?;
    Ok((|| {
        validate_atom(
            "outbox idempotency key",
            &idempotency_key,
            MAX_IDENTIFIER_BYTES,
        )?;
        let mission_id =
            MissionId::new(mission_id).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if let Some(value) = phase_id.as_deref() {
            validate_atom("outbox phase ID", value, MAX_IDENTIFIER_BYTES)
                .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        }
        let effect_kind = OutboxEffectKind::parse(&effect_kind)?;
        let operation_slot = EffectOperationSlot::new(operation_slot)
            .map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let logical_attempt =
            u32::try_from(logical_attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let process_intent = process_intent_json
            .as_deref()
            .map(ProcessIntentEnvelope::from_stored)
            .transpose()?;
        let expected_key = match (record_schema_version, process_intent.as_ref()) {
            (LEGACY_RECORD_SCHEMA_VERSION, None) => legacy_effect_idempotency_key(
                &mission_id,
                phase_id.as_deref(),
                effect_kind,
                logical_attempt,
            ),
            (RECORD_SCHEMA_VERSION, None) => effect_idempotency_key(
                &mission_id,
                phase_id.as_deref(),
                effect_kind,
                &operation_slot,
                logical_attempt,
            ),
            (RECORD_SCHEMA_VERSION, Some(process_intent)) => process_effect_idempotency_key(
                &mission_id,
                phase_id.as_deref(),
                effect_kind,
                &operation_slot,
                logical_attempt,
                &process_intent.request_sha256,
            ),
            (LEGACY_RECORD_SCHEMA_VERSION, Some(_)) => {
                return Err(RuntimeStoreError::CorruptDatabase);
            }
            (found, _) => {
                return Err(RuntimeStoreError::UnsupportedSchema {
                    found,
                    expected: RECORD_SCHEMA_VERSION,
                });
            }
        };
        if logical_attempt == 0 || expected_key != idempotency_key {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let payload = validate_stored_json(&payload_json, "outbox payload")?;
        validate_timestamp(&updated_at_utc)?;
        let attempts = u32::try_from(attempts).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        if journal_sequence <= 0 {
            return Err(RuntimeStoreError::CorruptDatabase);
        }
        let execution_identity = match (
            identity_attempt,
            identity_pid,
            identity_process_group_id,
            identity_process_start,
            identity_recorded_at,
        ) {
            (None, None, None, None, None) => None,
            (Some(attempt), Some(pid), Some(process_group_id), Some(start), Some(recorded_at)) => {
                Some(
                    ProcessExecutionIdentity::new(
                        u32::try_from(attempt).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                        u32::try_from(pid).map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                        u32::try_from(process_group_id)
                            .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                        start,
                        recorded_at,
                    )
                    .map_err(|_| RuntimeStoreError::CorruptDatabase)?,
                )
            }
            _ => return Err(RuntimeStoreError::CorruptDatabase),
        };
        Ok(OutboxEffect {
            idempotency_key,
            journal_sequence,
            mission_id,
            phase_id,
            effect_kind,
            operation_slot,
            logical_attempt,
            payload,
            state: OutboxState::parse(&state_text)?,
            attempts,
            execution_identity,
            updated_at_utc,
        })
    })())
}

fn valid_outbox_transition(current: OutboxState, resolution: &EffectResolution) -> bool {
    matches!(
        (current, resolution),
        (
            OutboxState::Executing,
            EffectResolution::Succeeded(_)
                | EffectResolution::Failed(_)
                | EffectResolution::ProcessFailed(_)
                | EffectResolution::Uncertain(_)
                | EffectResolution::ProcessUncertain(_)
                | EffectResolution::NotStarted(_)
        ) | (
            OutboxState::Uncertain,
            EffectResolution::Succeeded(_)
                | EffectResolution::Failed(_)
                | EffectResolution::ProcessFailed(_)
                | EffectResolution::ObservedAbsent(_)
                | EffectResolution::OperatorAuthorizedRetry(_)
        ) | (OutboxState::Failed, EffectResolution::RetryFailed(_))
    )
}

fn map_journal_insert_error(source: rusqlite::Error) -> RuntimeStoreError {
    if is_constraint_violation(&source) {
        RuntimeStoreError::TransitionConflict
    } else {
        RuntimeStoreError::operation("insert journal record", source)
    }
}

fn map_publication_insert_error(source: rusqlite::Error) -> RuntimeStoreError {
    if is_constraint_violation(&source) {
        RuntimeStoreError::PublicationConflict
    } else {
        RuntimeStoreError::operation("insert knowledge publication", source)
    }
}

fn map_outbox_insert_error(source: rusqlite::Error) -> RuntimeStoreError {
    if is_constraint_violation(&source) {
        RuntimeStoreError::OutboxConflict
    } else {
        RuntimeStoreError::operation("insert outbox effect", source)
    }
}

fn is_constraint_violation(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation
    )
}

fn validate_atom(
    _label: &'static str,
    value: &str,
    maximum: usize,
) -> Result<(), RuntimeStoreError> {
    if value.is_empty()
        || value.len() > maximum
        || !value.is_ascii()
        || value.chars().any(char::is_control)
    {
        return Err(RuntimeStoreError::InvalidIntent(
            "an identifier or kind is empty, oversized, or contains control text",
        ));
    }
    Ok(())
}

fn validate_terminal_decision_worker_id(worker_id: &str) -> Result<(), RuntimeStoreError> {
    WorkerId::new(worker_id.to_owned())
        .map(|_| ())
        .map_err(|_| {
            RuntimeStoreError::InvalidIntent(
                "terminal decision worker ID is not a safe worker path stem",
            )
        })
}

fn sensitive_metadata_key(value: &str) -> bool {
    let lowered = value.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "credential",
        "password",
        "secret",
        "token",
        "api_key",
        "private_key",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn validate_timestamp(value: &str) -> Result<(), RuntimeStoreError> {
    if !value.ends_with('Z') {
        return Err(RuntimeStoreError::InvalidIntent(
            "timestamp must use canonical UTC with a Z suffix",
        ));
    }
    let probe = EventRecord {
        id: "runtime-probe".to_owned(),
        event_type: "runtime.probe".to_owned(),
        timestamp: value.to_owned(),
        sequence: 1,
        mission_id: "runtime-probe".to_owned(),
        phase_id: None,
        worker_id: None,
        data: None,
        extra: EventJsonMap::default(),
    };
    encode_current_event(&probe)
        .map(|_| ())
        .map_err(|_| RuntimeStoreError::InvalidIntent("timestamp is not valid RFC3339"))
}

fn canonical_json(value: &Value, _label: &'static str) -> Result<String, RuntimeStoreError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|source| RuntimeStoreError::operation("encode bounded JSON", source))?;
    if bytes.len() > MAX_JSON_BYTES {
        return Err(RuntimeStoreError::InvalidIntent(
            "JSON value exceeds the one-MiB compatibility limit",
        ));
    }
    String::from_utf8(bytes)
        .map_err(|source| RuntimeStoreError::operation("validate encoded JSON", source))
}

fn validate_stored_json(value: &str, label: &'static str) -> Result<Value, RuntimeStoreError> {
    if value.len() > MAX_JSON_BYTES {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    let parsed: Value =
        serde_json::from_str(value).map_err(|_| RuntimeStoreError::CorruptDatabase)?;
    if canonical_json(&parsed, label)? != value {
        return Err(RuntimeStoreError::CorruptDatabase);
    }
    Ok(parsed)
}

fn validate_query_limit(limit: usize) -> Result<(), RuntimeStoreError> {
    if (1..=MAX_QUERY_LIMIT).contains(&limit) {
        Ok(())
    } else {
        Err(RuntimeStoreError::InvalidIntent(
            "outbox query limit is outside its supported range",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::MAX_WORKER_ID_BYTES;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(1);
    const CRASH_HELPER_HOME: &str = "NANIKA_RUNTIME_STORE_CRASH_HELPER_HOME";
    const BOUNDARY_WAL_CRASH_HELPER_HOME: &str =
        "NANIKA_RUNTIME_STORE_BOUNDARY_WAL_CRASH_HELPER_HOME";
    const MIGRATION_CRASH_HELPER_HOME: &str = "NANIKA_RUNTIME_STORE_MIGRATION_CRASH_HELPER_HOME";
    const LEGACY_V1_SCHEMA_SQL: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/runtime-store-v1.sql"
    ));

    struct TestHome {
        path: PathBuf,
    }

    impl TestHome {
        fn new() -> Result<Self, Box<dyn std::error::Error>> {
            let path = std::env::temp_dir().join(format!(
                "orchestrator-runtime-store-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            }
            let path = fs::canonicalize(path)?;
            Ok(Self { path })
        }

        fn boundary(&self) -> Result<Arc<ProductionBoundary>, Box<dyn std::error::Error>> {
            Ok(Arc::new(ProductionBoundary::from_canonical_root(
                &self.path,
            )?))
        }

        fn database(&self) -> PathBuf {
            self.path.join(DATABASE_FILE)
        }
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn timestamp(step: u8) -> String {
        format!("2026-07-16T00:00:{step:02}Z")
    }

    fn optional_file_bytes(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
        match fs::read(path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(source),
        }
    }

    #[cfg(unix)]
    fn write_private_test_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        fs::write(path, bytes)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }

    fn remove_optional_test_file(path: &Path) -> std::io::Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(source),
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct RuntimeFileSnapshot {
        database: Option<Vec<u8>>,
        wal: Option<Vec<u8>>,
        shm: Option<Vec<u8>>,
        compatibility_boundary_seal: Option<Vec<u8>>,
        private_boundary_seal: Option<Vec<u8>>,
        legacy_boundary_seal: Option<Vec<u8>>,
    }

    fn runtime_file_snapshot(home: &TestHome) -> std::io::Result<RuntimeFileSnapshot> {
        Ok(RuntimeFileSnapshot {
            database: optional_file_bytes(&home.database())?,
            wal: optional_file_bytes(&home.path.join(DATABASE_WAL_FILE))?,
            shm: optional_file_bytes(&home.path.join(DATABASE_SHM_FILE))?,
            compatibility_boundary_seal: optional_file_bytes(
                &home.path.join(COMPATIBILITY_BOUNDARY_SEAL_FILE),
            )?,
            private_boundary_seal: optional_file_bytes(
                &home.path.join(PRIVATE_PROCESS_LEDGER_BOUNDARY_SEAL_FILE),
            )?,
            legacy_boundary_seal: optional_file_bytes(&home.path.join(LEGACY_BOUNDARY_SEAL_FILE))?,
        })
    }

    fn bytes_contain(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty()
            && haystack
                .windows(needle.len())
                .any(|window| window == needle)
    }

    fn read_only_schema_digest(path: &Path) -> Result<String, RuntimeStoreError> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let connection = Connection::open_with_flags(path, flags)
            .map_err(|source| RuntimeStoreError::operation("open test schema read-only", source))?;
        schema_digest(&connection)
    }

    fn assert_runtime_schema_singleton_is_immutable(
        connection: &Connection,
        expected_boundary_kind: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let opposite_boundary_kind =
            if expected_boundary_kind == StoreBoundaryKind::Compatibility.as_str() {
                StoreBoundaryKind::PrivateProcessLedger.as_str()
            } else {
                StoreBoundaryKind::Compatibility.as_str()
            };
        assert!(
            connection
                .execute(
                    "UPDATE runtime_schema SET boundary_kind = ?1 WHERE singleton = 1",
                    [opposite_boundary_kind],
                )
                .is_err()
        );
        assert!(
            connection
                .execute("DELETE FROM runtime_schema WHERE singleton = 1", [])
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT INTO runtime_schema(singleton, version, boundary_kind)
                     VALUES (1, ?1, ?2)",
                    params![DATABASE_SCHEMA_VERSION, expected_boundary_kind],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "INSERT OR REPLACE INTO runtime_schema(singleton, version, boundary_kind)
                     VALUES (1, ?1, ?2)",
                    params![DATABASE_SCHEMA_VERSION, expected_boundary_kind],
                )
                .is_err()
        );
        let retained: (i64, i64, String) = connection.query_row(
            "SELECT count(*), version, boundary_kind FROM runtime_schema",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            retained,
            (
                1,
                DATABASE_SCHEMA_VERSION,
                expected_boundary_kind.to_owned()
            )
        );
        Ok(())
    }

    fn intent(id: &str, step: u8) -> Result<JournalIntent, RuntimeStoreError> {
        Ok(JournalIntent::new(
            id,
            Some(MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?),
            "phase.started",
            serde_json::json!({"phase_id":"phase-1","step":step}),
            timestamp(step),
        )?
        .with_required_projection(CompatibilityProjection::Checkpoint))
    }

    fn private_intent(id: &str, step: u8) -> Result<JournalIntent, RuntimeStoreError> {
        JournalIntent::new(
            id,
            Some(MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?),
            "private.process.ledger",
            serde_json::json!({"phase_id":"phase-1","step":step}),
            timestamp(step),
        )
    }

    fn intent_for_projection(
        id: &str,
        step: u8,
        projection: CompatibilityProjection,
    ) -> Result<JournalIntent, RuntimeStoreError> {
        intent_for_mission_projection(id, "mission-1", step, projection)
    }

    fn intent_for_mission_projection(
        id: &str,
        mission_id: &str,
        step: u8,
        projection: CompatibilityProjection,
    ) -> Result<JournalIntent, RuntimeStoreError> {
        Ok(JournalIntent::new(
            id,
            Some(MissionId::new(mission_id).map_err(|_| RuntimeStoreError::CorruptDatabase)?),
            "phase.started",
            serde_json::json!({"phase_id":"phase-1","step":step}),
            timestamp(step),
        )?
        .with_required_projection(projection))
    }

    fn effect(
        kind: OutboxEffectKind,
        logical_attempt: u32,
        payload: Value,
    ) -> Result<OutboxIntent, RuntimeStoreError> {
        effect_in_slot(
            kind,
            &format!("{}-{logical_attempt}", kind.as_str()),
            logical_attempt,
            payload,
        )
    }

    fn effect_in_slot(
        kind: OutboxEffectKind,
        operation_slot: &str,
        logical_attempt: u32,
        payload: Value,
    ) -> Result<OutboxIntent, RuntimeStoreError> {
        OutboxIntent::for_mission(
            MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            Some("phase-1".to_owned()),
            kind,
            EffectOperationSlot::new(operation_slot)?,
            logical_attempt,
            payload,
        )
    }

    #[cfg(unix)]
    fn typed_process_request(
        purpose: ProcessPurpose,
    ) -> Result<ProcessRequest, orchestrator_exec::ServiceContractError> {
        ProcessRequest::new(purpose, "enrolled-worker", "/fixture/worker")?
            .with_argument("--model")?
            .with_argument("fixture-model")?
            .with_environment("FIXTURE_TOKEN", "sensitive-value")?
            .with_stdin(b"bounded-input".to_vec())?
            .with_max_output_bytes(32_768)
    }

    #[cfg(unix)]
    fn typed_process_effect(
        request: &ProcessRequest,
        operation_slot: &str,
        logical_attempt: u32,
    ) -> Result<OutboxIntent, RuntimeStoreError> {
        OutboxIntent::for_process(
            MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?,
            Some("phase-1".to_owned()),
            OutboxEffectKind::ProviderProcess,
            EffectOperationSlot::new(operation_slot)?,
            logical_attempt,
            serde_json::json!({"provider":"fixture"}),
            request,
        )
    }

    #[cfg(unix)]
    fn observed_gate<'a>(
        binding: &'a ProcessRequestBinding,
        identity: &ProcessExecutionIdentity,
    ) -> ObservedGateBinding<'a> {
        ObservedGateBinding {
            request_binding: binding,
            pid: identity.pid,
            process_group_id: identity.process_group_id,
            process_start_identity: "fixture:start:1",
        }
    }

    #[cfg(unix)]
    fn append_typed_process_pending(
        store: &mut RuntimeStore,
        transition_id: &str,
        request: &ProcessRequest,
        step: u8,
    ) -> Result<(String, ProcessEffectBinding), RuntimeStoreError> {
        let effect = typed_process_effect(request, transition_id, 1)?;
        let key = effect.idempotency_key().to_owned();
        let binding = effect.bind_process(request)?;
        let commit = store.append(&intent(transition_id, step)?.with_outbox(effect)?)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(step.saturating_add(1)),
        )?)?;
        Ok((key, binding))
    }

    #[cfg(unix)]
    fn append_typed_process_pending_private(
        store: &mut PrivateProcessLedgerStore,
        transition_id: &str,
        request: &ProcessRequest,
        step: u8,
    ) -> Result<(String, ProcessEffectBinding), RuntimeStoreError> {
        let effect = typed_process_effect(request, transition_id, 1)?;
        let key = effect.idempotency_key().to_owned();
        let binding = effect.bind_process(request)?;
        let intent = JournalIntent::new(
            transition_id,
            Some(MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?),
            "private.process.claim",
            serde_json::json!({"phase_id":"phase-1","step":step}),
            timestamp(step),
        )?
        .with_outbox(effect)?;
        store.append(&intent)?;
        Ok((key, binding))
    }

    #[cfg(unix)]
    fn append_and_claim_typed_process_private(
        store: &mut PrivateProcessLedgerStore,
        transition_id: &str,
        request: &ProcessRequest,
        step: u8,
    ) -> Result<(ClaimedProcessAttempt, ProcessEffectBinding), RuntimeStoreError> {
        let (_, binding) =
            append_typed_process_pending_private(store, transition_id, request, step)?;
        let prepared = store.prepare_exact_process_claim(&binding, request)?;
        let claimed = store
            .claim_prepared_process(prepared, &timestamp(step.saturating_add(1)))
            .map_err(|_| RuntimeStoreError::InvalidOutboxTransition)?;
        Ok((claimed, binding))
    }

    #[cfg(unix)]
    fn prepare_blocked_identity_for_test(
        store: &mut PrivateProcessLedgerStore,
        claimed: &ClaimedProcessAttempt,
        request: &ProcessRequest,
        gate: &ObservedGateBinding<'_>,
        identity: &ProcessExecutionIdentity,
        step: u8,
    ) -> Result<(), RuntimeStoreError> {
        store.record_process_spawn_permit(claimed, request, &timestamp(step))?;
        store.inner.record_blocked_process_identity_bound(
            claimed.claimed(),
            request,
            gate,
            identity,
        )
    }

    fn evidence(code: EffectEvidenceCode) -> Result<EffectEvidence, RuntimeStoreError> {
        match code {
            EffectEvidenceCode::ExitObservedSuccess => Ok(EffectEvidence::exit_observed_success()),
            EffectEvidenceCode::ExitObservedFailure => EffectEvidence::exit_observed_failure(1),
            EffectEvidenceCode::ProcessNotStarted => Ok(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            EffectEvidenceCode::ProcessFailed => {
                EffectEvidence::process_failed(StartedProcessFailureEvidence::InfrastructureFailure)
            }
            EffectEvidenceCode::ProcessUncertain => Ok(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
            )),
            EffectEvidenceCode::ProcessStateUnobservable => {
                Ok(EffectEvidence::process_state_unobservable())
            }
            EffectEvidenceCode::ExitStatusLost => Ok(EffectEvidence::exit_status_lost()),
            EffectEvidenceCode::RemoteStateUnobservable => {
                Ok(EffectEvidence::remote_state_unobservable())
            }
            EffectEvidenceCode::ObservedAbsent => Ok(EffectEvidence::observed_absent()),
            _ => Err(RuntimeStoreError::InvalidIntent(
                "retry evidence is minted only from an authorization",
            )),
        }
    }

    fn policy_authorization(
        idempotency_key: impl Into<String>,
        attempt: u32,
        policy_version: impl Into<String>,
        decision_id: impl Into<String>,
        decided_at_utc: impl Into<String>,
    ) -> Result<RetryAuthorization, RuntimeStoreError> {
        RetryAuthorization::policy(
            &RetryIssuanceAuthority::new(),
            idempotency_key,
            attempt,
            policy_version,
            decision_id,
            decided_at_utc,
        )
    }

    fn operator_authorization(
        idempotency_key: impl Into<String>,
        attempt: u32,
        policy_version: impl Into<String>,
        decision_id: impl Into<String>,
        decided_at_utc: impl Into<String>,
    ) -> Result<RetryAuthorization, RuntimeStoreError> {
        RetryAuthorization::operator(
            &RetryIssuanceAuthority::new(),
            idempotency_key,
            attempt,
            policy_version,
            decision_id,
            decided_at_utc,
        )
    }

    fn event_jsonl(sequence: i64, event_id: &str, step: u8) -> Result<Vec<u8>, RuntimeStoreError> {
        event_jsonl_for("mission-1", sequence, event_id, step)
    }

    fn event_jsonl_for(
        mission_id: &str,
        sequence: i64,
        event_id: &str,
        step: u8,
    ) -> Result<Vec<u8>, RuntimeStoreError> {
        let mut bytes = encode_current_event(&EventRecord {
            id: event_id.to_owned(),
            event_type: "phase.started".to_owned(),
            timestamp: timestamp(step),
            sequence,
            mission_id: mission_id.to_owned(),
            phase_id: Some("phase-1".to_owned()),
            worker_id: None,
            data: None,
            extra: EventJsonMap::from_iter([("step".to_owned(), Value::from(step))]),
        })
        .map_err(|_| RuntimeStoreError::InvalidIntent("test event is invalid"))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Directly tampers with one journal row's `extra_json`, recomputing a
    /// matching checksum so the generic checksum-chain check alone cannot
    /// catch the forgery — only whatever invariant the caller is targeting
    /// can. Mirrors the manual forge sequence used by
    /// `tampered_event_projection_sequence_gap_or_duplicate_is_rejected_by_recipe_validation`,
    /// factored out for reuse by the recipe forgery-matrix tests.
    fn forge_journal_extra_json(
        home: &TestHome,
        sequence: i64,
        forged_extra: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let raw = Connection::open(home.database())?;
        let reject_update_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_update'",
            [],
            |row| row.get(0),
        )?;
        let reject_delete_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_delete'",
            [],
            |row| row.get(0),
        )?;
        raw.execute_batch(
            "DROP TRIGGER journal_reject_update;
             DROP TRIGGER journal_reject_delete;",
        )?;
        let transition_id: String = raw.query_row(
            "SELECT transition_id FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let mission_id: Option<String> = raw.query_row(
            "SELECT mission_id FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let kind: String = raw.query_row(
            "SELECT transition_kind FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let payload_json: String = raw.query_row(
            "SELECT payload_json FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let committed_at_utc: String = raw.query_row(
            "SELECT committed_at_utc FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let outbox_fingerprint: String = raw.query_row(
            "SELECT outbox_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let projection_fingerprint: String = raw.query_row(
            "SELECT projection_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let previous_checksum: String = raw.query_row(
            "SELECT previous_checksum FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let prepared = PreparedIntent {
            transition_id,
            mission_id,
            kind,
            payload_json,
            committed_at_utc,
            extra_json: forged_extra.to_owned(),
            outbox_fingerprint,
            projection_fingerprint,
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        };
        let forged_checksum = transition_checksum(
            RECORD_SCHEMA_VERSION,
            sequence,
            &previous_checksum,
            &prepared,
        );
        raw.execute(
            "UPDATE journal SET extra_json = ?1, checksum = ?2 WHERE sequence = ?3",
            params![forged_extra, forged_checksum, sequence],
        )?;
        raw.execute_batch(&format!("{reject_update_sql};\n{reject_delete_sql};"))?;
        drop(raw);
        Ok(())
    }

    /// Rebinds only a journal row's transition ID and/or kind while retaining
    /// every other byte, recomputing the checksum, and restoring the exact
    /// immutable-row triggers. This isolates transition-identity validation
    /// from generic checksum and schema-object checks.
    fn forge_journal_transition_identity(
        home: &TestHome,
        sequence: i64,
        forged_transition_id: Option<&str>,
        forged_transition_kind: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let raw = Connection::open(home.database())?;
        let reject_update_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_update'",
            [],
            |row| row.get(0),
        )?;
        let reject_delete_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_delete'",
            [],
            |row| row.get(0),
        )?;
        raw.execute_batch(
            "DROP TRIGGER journal_reject_update;
             DROP TRIGGER journal_reject_delete;",
        )?;
        let original_transition_id: String = raw.query_row(
            "SELECT transition_id FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let mission_id: Option<String> = raw.query_row(
            "SELECT mission_id FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let original_transition_kind: String = raw.query_row(
            "SELECT transition_kind FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let payload_json: String = raw.query_row(
            "SELECT payload_json FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let committed_at_utc: String = raw.query_row(
            "SELECT committed_at_utc FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let extra_json: String = raw.query_row(
            "SELECT extra_json FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let outbox_fingerprint: String = raw.query_row(
            "SELECT outbox_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let projection_fingerprint: String = raw.query_row(
            "SELECT projection_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let previous_checksum: String = raw.query_row(
            "SELECT previous_checksum FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let transition_id = forged_transition_id
            .unwrap_or(&original_transition_id)
            .to_owned();
        let transition_kind = forged_transition_kind
            .unwrap_or(&original_transition_kind)
            .to_owned();
        let prepared = PreparedIntent {
            transition_id: transition_id.clone(),
            mission_id,
            kind: transition_kind.clone(),
            payload_json,
            committed_at_utc,
            extra_json,
            outbox_fingerprint,
            projection_fingerprint,
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        };
        let forged_checksum = transition_checksum(
            RECORD_SCHEMA_VERSION,
            sequence,
            &previous_checksum,
            &prepared,
        );
        raw.execute(
            "UPDATE journal
             SET transition_id = ?1, transition_kind = ?2, checksum = ?3
             WHERE sequence = ?4",
            params![transition_id, transition_kind, forged_checksum, sequence],
        )?;
        raw.execute_batch(&format!("{reject_update_sql};\n{reject_delete_sql};"))?;
        drop(raw);
        Ok(())
    }

    fn event_record_jsonl(record: &EventRecord) -> Result<Vec<u8>, RuntimeStoreError> {
        let mut bytes = encode_current_event(record)
            .map_err(|_| RuntimeStoreError::InvalidIntent("test event is invalid"))?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    fn projection_receipt(
        commit: &JournalCommit,
        projection: CompatibilityProjection,
        step: u8,
    ) -> Result<ProjectionReceipt, RuntimeStoreError> {
        if projection == CompatibilityProjection::EventLog {
            let recipe =
                commit
                    .event_projection_recipe()
                    .ok_or(RuntimeStoreError::InvalidIntent(
                        "test commit has no sealed event recipe",
                    ))?;
            ProjectionReceipt::event_log(
                commit.sequence(),
                event_jsonl(recipe.public_sequence(), recipe.event_id(), step)?,
                timestamp(step.saturating_add(1)),
            )
        } else {
            ProjectionReceipt::compatibility(
                commit.sequence(),
                projection,
                timestamp(step.saturating_add(1)),
            )
        }
    }

    fn open_store(boundary: Arc<ProductionBoundary>) -> Result<RuntimeStore, RuntimeStoreError> {
        RuntimeStore::open(boundary, StorageActorAuthority::new())
    }

    fn open_private_store(
        boundary: Arc<ProductionBoundary>,
    ) -> Result<PrivateProcessLedgerStore, RuntimeStoreError> {
        RuntimeStore::open_private(
            PrivateProcessLedgerBoundary::new(boundary),
            StorageActorAuthority::new(),
        )
    }

    fn create_unsealed_v3_fake_sidecars_private_row(
        home: &TestHome,
        transition_kind: &str,
    ) -> Result<(String, String), Box<dyn std::error::Error>> {
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(&intent_for_projection(
            "legacy-fake-sidecars-private-row",
            1,
            CompatibilityProjection::Sidecars,
        )?)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Sidecars,
            timestamp(2),
        )?)?;
        store.close()?;

        forge_journal_transition_identity(home, 1, None, Some(transition_kind))?;
        let connection = Connection::open(home.database())?;
        connection.execute_batch(
            "DROP TABLE audit_chain; DROP TABLE audit_chain_head; DROP TABLE knowledge_publication; DROP TABLE reasoning_revision; DROP TABLE reasoning_attempt; DROP TABLE reasoning_review; DROP TABLE reasoning_handoff; DROP TABLE reasoning_assignment; DROP TABLE reasoning_evidence; DROP TABLE reasoning_criterion; DROP TABLE reasoning_assumption; DROP TABLE reasoning_record; DROP TRIGGER runtime_schema_reject_boundary_kind_update;
                 DROP TRIGGER runtime_schema_reject_delete;
                 DROP TRIGGER runtime_schema_reject_insert;
                 ALTER TABLE runtime_schema RENAME TO runtime_schema_v4;
                 CREATE TABLE runtime_schema (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    version INTEGER NOT NULL CHECK (version > 0)
                 );
                 INSERT INTO runtime_schema(singleton, version) VALUES (1, 3);
                 DROP TABLE runtime_schema_v4;
                 PRAGMA user_version = 3;",
        )?;
        assert_eq!(schema_digest(&connection)?, NATIVE_V3_SCHEMA_DIGEST);
        let retained: (String, String) = connection.query_row(
            "SELECT transition_kind, checksum FROM journal WHERE sequence = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let (busy, log_frames, checkpointed_frames): (i64, i64, i64) =
            connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?;
        assert_eq!(busy, 0);
        assert!(checkpointed_frames >= log_frames);
        drop(connection);

        fs::remove_file(home.path.join(COMPATIBILITY_BOUNDARY_SEAL_FILE))?;
        sync_dir(boundary.directory())?;
        assert_eq!(
            optional_file_bytes(&home.path.join(COMPATIBILITY_BOUNDARY_SEAL_FILE))?,
            None
        );
        Ok(retained)
    }

    fn legacy_v1_evidence(
        code: &str,
        metadata: BTreeMap<String, String>,
    ) -> Result<String, Box<dyn std::error::Error>> {
        Ok(serde_json::to_string(&LegacyV1EffectEvidence {
            code: code.to_owned(),
            metadata,
        })?)
    }

    fn create_populated_v1_database(
        home: &TestHome,
    ) -> Result<(String, String), Box<dyn std::error::Error>> {
        create_populated_v1_database_with_retry_evidence(home, false)
    }

    fn create_populated_v1_database_with_retry_evidence(
        home: &TestHome,
        malformed_late_retry: bool,
    ) -> Result<(String, String), Box<dyn std::error::Error>> {
        let connection = Connection::open(home.database())?;
        connection.execute_batch(LEGACY_V1_SCHEMA_SQL)?;
        let mission_id = MissionId::new("mission-1")?;
        let pending_key = legacy_effect_idempotency_key(
            &mission_id,
            Some("phase-1"),
            OutboxEffectKind::PluginProcess,
            1,
        );
        let succeeded_key = legacy_effect_idempotency_key(
            &mission_id,
            Some("phase-1"),
            OutboxEffectKind::ProviderProcess,
            1,
        );
        let mut prepared_outbox = vec![
            PreparedOutbox {
                idempotency_key: pending_key.clone(),
                mission_id: mission_id.as_str().to_owned(),
                phase_id: Some("phase-1".to_owned()),
                effect_kind: OutboxEffectKind::PluginProcess.as_str().to_owned(),
                operation_slot: "legacy-v1".to_owned(),
                logical_attempt: 1,
                payload_json: r#"{"workspace":"mission-1"}"#.to_owned(),
                process_request_sha256: None,
                process_intent_json: None,
            },
            PreparedOutbox {
                idempotency_key: succeeded_key.clone(),
                mission_id: mission_id.as_str().to_owned(),
                phase_id: Some("phase-1".to_owned()),
                effect_kind: OutboxEffectKind::ProviderProcess.as_str().to_owned(),
                operation_slot: "legacy-v1".to_owned(),
                logical_attempt: 1,
                payload_json: r#"{"provider":"claude"}"#.to_owned(),
                process_request_sha256: None,
                process_intent_json: None,
            },
        ];
        prepared_outbox.sort_by(|left, right| left.idempotency_key.cmp(&right.idempotency_key));
        let required_projections = vec![
            CompatibilityProjection::Checkpoint,
            CompatibilityProjection::EventLog,
        ];
        let prepared = PreparedIntent {
            transition_id: "legacy-transition-1".to_owned(),
            mission_id: Some(mission_id.as_str().to_owned()),
            kind: "phase.started".to_owned(),
            payload_json: r#"{"phase_id":"phase-1","step":1}"#.to_owned(),
            committed_at_utc: timestamp(1),
            extra_json: "{}".to_owned(),
            outbox_fingerprint: outbox_fingerprint_for_version(
                LEGACY_RECORD_SCHEMA_VERSION,
                &prepared_outbox,
            )?,
            projection_fingerprint: projection_fingerprint(&required_projections),
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        };
        let checksum =
            transition_checksum(LEGACY_RECORD_SCHEMA_VERSION, 1, GENESIS_CHECKSUM, &prepared);
        connection.execute(
            "INSERT INTO journal (
                sequence, transition_id, mission_id, record_schema_version,
                transition_kind, payload_json, committed_at_utc, extra_json,
                outbox_fingerprint, projection_fingerprint, previous_checksum, checksum
             ) VALUES (1, ?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                prepared.transition_id,
                prepared.mission_id,
                prepared.kind,
                prepared.payload_json,
                prepared.committed_at_utc,
                prepared.extra_json,
                prepared.outbox_fingerprint,
                prepared.projection_fingerprint,
                GENESIS_CHECKSUM,
                checksum,
            ],
        )?;
        for effect in &prepared_outbox {
            let (state, attempts) = if effect.idempotency_key == succeeded_key {
                ("succeeded", 4_i64)
            } else {
                ("pending", 0_i64)
            };
            connection.execute(
                "INSERT INTO outbox (
                    idempotency_key, journal_sequence, mission_id, phase_id,
                    effect_kind, logical_attempt, payload_json, state, attempts, updated_at_utc
                 ) VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    effect.idempotency_key,
                    effect.mission_id,
                    effect.phase_id,
                    effect.effect_kind,
                    effect.logical_attempt,
                    effect.payload_json,
                    state,
                    attempts,
                    if effect.idempotency_key == succeeded_key {
                        timestamp(9)
                    } else {
                        timestamp(2)
                    },
                ],
            )?;
        }
        for attempt in 1_i64..=4 {
            let step = u8::try_from(attempt * 2)?;
            connection.execute(
                "INSERT INTO outbox_attempt_claim(idempotency_key, attempt, claimed_at_utc)
                 VALUES (?1, ?2, ?3)",
                params![succeeded_key, attempt, timestamp(step)],
            )?;
            connection.execute(
                "INSERT INTO outbox_execution_identity(
                    idempotency_key, attempt, pid, process_group_id,
                    process_start_identity, recorded_at_utc
                 ) VALUES (?1, ?2, ?3, ?3, ?4, ?5)",
                params![
                    succeeded_key,
                    attempt,
                    40_i64 + attempt,
                    format!("legacy-boot:{attempt}"),
                    timestamp(step),
                ],
            )?;
        }
        let custom_failure = legacy_v1_evidence(
            "custom_failure",
            BTreeMap::from([("result".to_owned(), "bounded".to_owned())]),
        )?;
        let malformed_operator_retry = legacy_v1_evidence(
            "operator_authorized_retry",
            BTreeMap::from([(
                "decision_id".to_owned(),
                "legacy-operator-uncertain".to_owned(),
            )]),
        )?;
        let policy_retry = legacy_v1_evidence(
            "policy_authorized_retry",
            BTreeMap::from([
                ("decided_at_utc".to_owned(), timestamp(3)),
                (
                    "decision_id".to_owned(),
                    "legacy-reused-decision".to_owned(),
                ),
            ]),
        )?;
        let operator_retry_from_failed = legacy_v1_evidence(
            "operator_authorized_retry",
            BTreeMap::from([
                ("decided_at_utc".to_owned(), timestamp(5)),
                (
                    "decision_id".to_owned(),
                    "legacy-reused-decision".to_owned(),
                ),
            ]),
        )?;
        let custom_uncertain = legacy_v1_evidence(
            "custom_uncertain",
            BTreeMap::from([("result".to_owned(), "bounded".to_owned())]),
        )?;
        let operator_retry_from_uncertain = legacy_v1_evidence(
            "operator_authorized_retry",
            BTreeMap::from([
                ("decided_at_utc".to_owned(), timestamp(7)),
                (
                    "decision_id".to_owned(),
                    "legacy-operator-uncertain".to_owned(),
                ),
            ]),
        )?;
        let custom_success = legacy_v1_evidence(
            "custom_success",
            BTreeMap::from([("result".to_owned(), "bounded".to_owned())]),
        )?;
        let operator_retry_from_uncertain = if malformed_late_retry {
            malformed_operator_retry
        } else {
            operator_retry_from_uncertain
        };
        let observations = [
            (
                1_i64,
                1_i64,
                "failed",
                "custom_failure",
                custom_failure,
                3_u8,
            ),
            (2, 1, "pending", "policy_authorized_retry", policy_retry, 4),
            (
                3,
                2,
                "failed",
                "custom_failure",
                legacy_v1_evidence(
                    "custom_failure",
                    BTreeMap::from([("result".to_owned(), "bounded".to_owned())]),
                )?,
                5,
            ),
            (
                4,
                2,
                "pending",
                "operator_authorized_retry",
                operator_retry_from_failed,
                6,
            ),
            (5, 3, "uncertain", "custom_uncertain", custom_uncertain, 7),
            (
                6,
                3,
                "pending",
                "operator_authorized_retry",
                operator_retry_from_uncertain,
                8,
            ),
            (7, 4, "succeeded", "custom_success", custom_success, 9),
        ];
        for (sequence, attempt, state, code, evidence, step) in observations {
            connection.execute(
                "INSERT INTO outbox_attempt_observation(
                    observation_sequence, idempotency_key, attempt, observed_state,
                    evidence_code, evidence_json, observed_at_utc
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    sequence,
                    succeeded_key,
                    attempt,
                    state,
                    code,
                    evidence,
                    timestamp(step),
                ],
            )?;
        }
        for projection in &required_projections {
            connection.execute(
                "INSERT INTO projection_requirement(journal_sequence, projection)
                 VALUES (1, ?1)",
                [projection.as_str()],
            )?;
        }
        connection.execute(
            "INSERT INTO projection_receipt(
                journal_sequence, projection, public_sequence, event_id, applied_at_utc
             ) VALUES (1, 'event_log', 1, 'evt_legacy00000001', ?1)",
            [timestamp(3)],
        )?;
        connection.execute(
            "INSERT INTO projection_receipt(
                journal_sequence, projection, public_sequence, event_id, applied_at_utc
             ) VALUES (1, 'checkpoint', NULL, NULL, ?1)",
            [timestamp(3)],
        )?;
        connection.execute(
            "INSERT INTO command_ack(journal_sequence, acknowledged_at_utc) VALUES (1, ?1)",
            [timestamp(3)],
        )?;
        connection.execute(
            "INSERT INTO projection_cursor(
                projection, journal_sequence, public_sequence, event_id, updated_at_utc
             ) VALUES ('event_log', 1, 1, 'evt_legacy00000001', ?1)",
            [timestamp(3)],
        )?;
        connection.execute(
            "INSERT INTO projection_cursor(
                projection, journal_sequence, public_sequence, event_id, updated_at_utc
             ) VALUES ('checkpoint', 1, NULL, NULL, ?1)",
            [timestamp(3)],
        )?;
        drop(connection);
        #[cfg(unix)]
        fs::set_permissions(home.database(), fs::Permissions::from_mode(0o600))?;
        Ok((pending_key, succeeded_key))
    }

    fn append_acknowledge_claim(
        store: &mut RuntimeStore,
        transition_id: &str,
        effect: OutboxIntent,
    ) -> Result<(String, OutboxEffect), Box<dyn std::error::Error>> {
        let key = effect.idempotency_key().to_owned();
        let commit = store.append(&intent(transition_id, 1)?.with_outbox(effect)?)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;
        let claimed = store.claim_next(&timestamp(3))?.ok_or("missing claim")?;
        Ok((key, claimed))
    }

    fn append_and_acknowledge(
        store: &mut RuntimeStore,
        transition_id: &str,
        effect: OutboxIntent,
    ) -> Result<(String, i64), Box<dyn std::error::Error>> {
        let key = effect.idempotency_key().to_owned();
        let commit = store.append(&intent(transition_id, 1)?.with_outbox(effect)?)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;
        Ok((key, commit.sequence()))
    }

    #[test]
    fn append_is_atomic_idempotent_and_recovers_pending_effects()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let transition = intent("transition-1", 1)?.with_outbox(effect(
            OutboxEffectKind::PluginProcess,
            1,
            serde_json::json!({"workspace":"mission-1"}),
        )?)?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let first = store.append(&transition)?;
        assert_eq!(first.sequence(), 1);
        assert!(!first.duplicate());
        let retry = store.append(&transition)?;
        assert_eq!(retry.sequence(), 1);
        assert!(retry.duplicate());
        assert_eq!(store.pending_effects(10)?.len(), 1);
        store.close()?;

        let reopened = open_store(boundary)?;
        let pending = reopened.pending_effects(10)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].state(), OutboxState::Pending);
        assert_eq!(pending[0].attempts(), 0);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn schema_v4_runtime_boundary_row_rejects_delete_insert_and_replace()
    -> Result<(), Box<dyn std::error::Error>> {
        let compatibility_home = TestHome::new()?;
        let compatibility_store = open_store(compatibility_home.boundary()?)?;
        assert_runtime_schema_singleton_is_immutable(
            compatibility_store.connection()?,
            StoreBoundaryKind::Compatibility.as_str(),
        )?;
        compatibility_store.close()?;

        let private_home = TestHome::new()?;
        let private_store = open_private_store(private_home.boundary()?)?;
        assert_runtime_schema_singleton_is_immutable(
            private_store.connection()?,
            StoreBoundaryKind::PrivateProcessLedger.as_str(),
        )?;
        private_store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn ambiguous_nonzero_and_legacy_boundary_markers_fail_before_sqlite()
    -> Result<(), Box<dyn std::error::Error>> {
        for case in ["both", "nonzero", "legacy"] {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            open_store(Arc::clone(&boundary))?.close()?;
            let schema_before = read_only_schema_digest(&home.database())?;
            let compatibility_marker = home.path.join(COMPATIBILITY_BOUNDARY_SEAL_FILE);
            let private_marker = home.path.join(PRIVATE_PROCESS_LEDGER_BOUNDARY_SEAL_FILE);
            let legacy_marker = home.path.join(LEGACY_BOUNDARY_SEAL_FILE);

            match case {
                "both" => write_private_test_file(&private_marker, b"")?,
                "nonzero" => write_private_test_file(&compatibility_marker, b"partial")?,
                "legacy" => {
                    fs::remove_file(&compatibility_marker)?;
                    write_private_test_file(
                        &legacy_marker,
                        b"nanika-runtime-boundary-v1\nkind=compatibility\n",
                    )?;
                }
                _ => unreachable!(),
            }

            let files_before = runtime_file_snapshot(&home)?;
            assert!(matches!(
                open_store(boundary),
                Err(RuntimeStoreError::CorruptDatabase)
            ));
            assert_eq!(
                runtime_file_snapshot(&home)?,
                files_before,
                "marker case {case} changed a runtime file before rejection"
            );
            assert_eq!(
                read_only_schema_digest(&home.database())?,
                schema_before,
                "marker case {case} changed the schema"
            );
        }
        Ok(())
    }

    #[test]
    fn compatibility_append_rejects_known_private_kinds_without_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        for (index, transition_kind) in [
            PRIVATE_PROCESS_CLAIM_TRANSITION_KIND,
            TERMINAL_DECISION_TRANSITION_KIND,
        ]
        .into_iter()
        .enumerate()
        {
            let intent = JournalIntent::new(
                format!("forbidden-private-kind-{index}"),
                Some(MissionId::new("mission-1")?),
                transition_kind,
                serde_json::json!({"phase_id":"phase-1"}),
                timestamp(u8::try_from(index + 1)?),
            )?
            .with_required_projection(CompatibilityProjection::Sidecars);
            assert!(matches!(
                store.append(&intent),
                Err(RuntimeStoreError::InvalidIntent(_))
            ));
            let retained_rows: i64 = store.connection()?.query_row(
                "SELECT
                   (SELECT count(*) FROM journal)
                   + (SELECT count(*) FROM outbox)
                   + (SELECT count(*) FROM projection_requirement)
                   + (SELECT count(*) FROM projection_receipt)
                   + (SELECT count(*) FROM command_ack)",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(retained_rows, 0, "{transition_kind}");
        }
        store.close()?;
        Ok(())
    }

    #[test]
    fn schema_v4_typed_openers_reject_cross_open_without_mutating_store()
    -> Result<(), Box<dyn std::error::Error>> {
        let compatibility_home = TestHome::new()?;
        let compatibility_boundary = compatibility_home.boundary()?;
        open_store(Arc::clone(&compatibility_boundary))?.close()?;
        let compatibility_database = compatibility_home.database();
        let compatibility_wal = compatibility_home.path.join(DATABASE_WAL_FILE);
        let compatibility_shm = compatibility_home.path.join(DATABASE_SHM_FILE);
        let compatibility_seal = compatibility_home
            .path
            .join(COMPATIBILITY_BOUNDARY_SEAL_FILE);
        let compatibility_schema_before = read_only_schema_digest(&compatibility_database)?;
        let compatibility_database_before = fs::read(&compatibility_database)?;
        let compatibility_wal_before = optional_file_bytes(&compatibility_wal)?;
        let compatibility_shm_before = optional_file_bytes(&compatibility_shm)?;
        let compatibility_seal_before = optional_file_bytes(&compatibility_seal)?;
        assert_eq!(compatibility_seal_before, Some(Vec::new()));
        assert_eq!(
            optional_file_bytes(
                &compatibility_home
                    .path
                    .join(PRIVATE_PROCESS_LEDGER_BOUNDARY_SEAL_FILE)
            )?,
            None
        );
        assert_eq!(
            optional_file_bytes(&compatibility_home.path.join(LEGACY_BOUNDARY_SEAL_FILE))?,
            None
        );

        assert!(matches!(
            open_private_store(Arc::clone(&compatibility_boundary)),
            Err(RuntimeStoreError::BoundaryKindMismatch)
        ));
        assert_eq!(
            fs::read(&compatibility_database)?,
            compatibility_database_before
        );
        assert_eq!(
            optional_file_bytes(&compatibility_wal)?,
            compatibility_wal_before
        );
        assert_eq!(
            optional_file_bytes(&compatibility_shm)?,
            compatibility_shm_before
        );
        assert_eq!(
            optional_file_bytes(&compatibility_seal)?,
            compatibility_seal_before
        );
        assert_eq!(
            read_only_schema_digest(&compatibility_database)?,
            compatibility_schema_before
        );
        open_store(compatibility_boundary)?.close()?;

        let private_home = TestHome::new()?;
        let private_boundary = private_home.boundary()?;
        open_private_store(Arc::clone(&private_boundary))?.close()?;
        let private_database = private_home.database();
        let private_wal = private_home.path.join(DATABASE_WAL_FILE);
        let private_shm = private_home.path.join(DATABASE_SHM_FILE);
        let private_seal = private_home
            .path
            .join(PRIVATE_PROCESS_LEDGER_BOUNDARY_SEAL_FILE);
        let private_schema_before = read_only_schema_digest(&private_database)?;
        let private_database_before = fs::read(&private_database)?;
        let private_wal_before = optional_file_bytes(&private_wal)?;
        let private_shm_before = optional_file_bytes(&private_shm)?;
        let private_seal_before = optional_file_bytes(&private_seal)?;
        assert_eq!(private_seal_before, Some(Vec::new()));
        assert_eq!(
            optional_file_bytes(&private_home.path.join(COMPATIBILITY_BOUNDARY_SEAL_FILE))?,
            None
        );
        assert_eq!(
            optional_file_bytes(&private_home.path.join(LEGACY_BOUNDARY_SEAL_FILE))?,
            None
        );

        assert!(matches!(
            open_store(Arc::clone(&private_boundary)),
            Err(RuntimeStoreError::BoundaryKindMismatch)
        ));
        assert_eq!(fs::read(&private_database)?, private_database_before);
        assert_eq!(optional_file_bytes(&private_wal)?, private_wal_before);
        assert_eq!(optional_file_bytes(&private_shm)?, private_shm_before);
        assert_eq!(optional_file_bytes(&private_seal)?, private_seal_before);
        assert_eq!(
            read_only_schema_digest(&private_database)?,
            private_schema_before
        );
        open_private_store(private_boundary)?.close()?;
        Ok(())
    }

    #[test]
    fn private_append_reopen_seals_marker_self_acks_and_remains_idempotent()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let intent = private_intent("private-ledger-row", 1)?;
        let mut store = open_private_store(Arc::clone(&boundary))?;

        let first = store.append(&intent)?;
        assert_eq!(first.sequence(), 1);
        assert!(!first.duplicate());

        let before_reopen: (Option<String>, String, i64, i64, i64, i64, i64) =
            store.connection()?.query_row(
                "SELECT mission_id, extra_json,
                        (SELECT count(*) FROM projection_requirement),
                        (SELECT count(*) FROM projection_receipt),
                        (SELECT count(*) FROM projection_cursor),
                        (SELECT count(*) FROM command_ack WHERE journal_sequence = journal.sequence),
                        (SELECT count(*) FROM journal)
                 FROM journal WHERE sequence = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )?;
        let expected = (
            Some("mission-1".to_owned()),
            r#"{"__private_process_ledger_record":{"schema":1}}"#.to_owned(),
            0,
            0,
            0,
            1,
            1,
        );
        assert_eq!(before_reopen, expected);
        store.close()?;

        let mut reopened = open_private_store(boundary)?;
        let retry = reopened.append(&intent)?;
        assert_eq!(retry.sequence(), 1);
        assert!(retry.duplicate());
        let after_reopen: (Option<String>, String, i64, i64, i64, i64, i64) =
            reopened.connection()?.query_row(
                "SELECT mission_id, extra_json,
                        (SELECT count(*) FROM projection_requirement),
                        (SELECT count(*) FROM projection_receipt),
                        (SELECT count(*) FROM projection_cursor),
                        (SELECT count(*) FROM command_ack WHERE journal_sequence = journal.sequence),
                        (SELECT count(*) FROM journal)
                 FROM journal WHERE sequence = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )?;
        assert_eq!(after_reopen, expected);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn private_marker_forgery_fails_semantic_reopen_with_valid_checksum_and_schema()
    -> Result<(), Box<dyn std::error::Error>> {
        for (label, forged_extra) in [
            ("missing", serde_json::json!({})),
            (
                "wrong-version",
                serde_json::json!({"__private_process_ledger_record":{"schema":2}}),
            ),
            (
                "wrong-shape",
                serde_json::json!({"__private_process_ledger_record":true}),
            ),
        ] {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_private_store(Arc::clone(&boundary))?;
            store.append(&private_intent("private-marker-forgery", 1)?)?;
            store.close()?;

            let forged_extra_json =
                canonical_json(&forged_extra, "test forged private marker extras")?;
            forge_journal_extra_json(&home, 1, &forged_extra_json)?;

            let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
                | OpenFlags::SQLITE_OPEN_FULL_MUTEX
                | OpenFlags::SQLITE_OPEN_NOFOLLOW;
            let raw = Connection::open_with_flags(home.database(), flags)?;
            assert_eq!(schema_digest(&raw)?, NATIVE_V7_SCHEMA_DIGEST, "{label}");
            validate_checksum_chain(&raw)?;
            drop(raw);

            assert!(
                matches!(
                    open_private_store(boundary),
                    Err(RuntimeStoreError::CorruptDatabase)
                ),
                "{label}"
            );
        }
        Ok(())
    }

    #[test]
    fn populated_v1_migration_preserves_data_and_is_idempotent()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let (pending_key, succeeded_key) = create_populated_v1_database(&home)?;
        let connection = Connection::open(home.database())?;
        assert_eq!(schema_digest(&connection)?, LEGACY_SCHEMA_DIGEST);
        let journal_bytes_before: (i64, String, String, String, String) = connection.query_row(
            "SELECT record_schema_version, payload_json, extra_json,
                    outbox_fingerprint, checksum
             FROM journal WHERE sequence = 1",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        drop(connection);
        let boundary = home.boundary()?;
        let mut migrated = open_store(Arc::clone(&boundary))?;
        let pending = migrated.pending_effects(10)?;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].idempotency_key(), pending_key);
        assert_eq!(pending[0].operation_slot().as_str(), "legacy-v1");
        let history = migrated.attempt_history(&succeeded_key, 10)?;
        assert_eq!(history.len(), 7);
        assert_eq!(history[0].state(), OutboxState::Failed);
        assert_eq!(history[1].state(), OutboxState::Pending);
        assert_eq!(history[2].state(), OutboxState::Failed);
        assert_eq!(history[3].state(), OutboxState::Pending);
        assert_eq!(history[4].state(), OutboxState::Uncertain);
        assert_eq!(history[5].state(), OutboxState::Pending);
        assert_eq!(history[6].state(), OutboxState::Succeeded);
        assert_eq!(
            history[6].evidence().code(),
            EffectEvidenceCode::LegacyV1Succeeded
        );
        let legacy_consumption_count: i64 = migrated.connection()?.query_row(
            "SELECT count(*) FROM legacy_retry_authorization_consumption",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(legacy_consumption_count, 3);
        let journal_bytes_after: (i64, String, String, String, String) =
            migrated.connection()?.query_row(
                "SELECT record_schema_version, payload_json, extra_json,
                        outbox_fingerprint, checksum
                 FROM journal WHERE sequence = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )?;
        assert_eq!(journal_bytes_after, journal_bytes_before);
        let reused_decision_count: i64 = migrated.connection()?.query_row(
            "SELECT count(*) FROM legacy_retry_authorization_consumption
             WHERE decision_id = 'legacy-reused-decision'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(reused_decision_count, 2);
        let policy_version_column_count: i64 = migrated.connection()?.query_row(
            "SELECT count(*) FROM pragma_table_info('legacy_retry_authorization_consumption')
             WHERE name = 'policy_version'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(policy_version_column_count, 0);

        let claimed = migrated
            .claim_next(&timestamp(10))?
            .ok_or("missing legacy effect")?;
        assert_eq!(claimed.idempotency_key(), pending_key);
        migrated.record_execution_identity(
            &pending_key,
            &ProcessExecutionIdentity::new(1, 91, 91, "legacy-reuse-test:1", timestamp(10))?,
        )?;
        migrated.resolve_effect(
            &pending_key,
            EffectResolution::Failed(EffectEvidence::exit_observed_failure(1)?),
            &timestamp(11),
        )?;
        for legacy_decision in ["legacy-reused-decision", "legacy-operator-uncertain"] {
            assert!(matches!(
                migrated.resolve_effect(
                    &pending_key,
                    EffectResolution::RetryFailed(policy_authorization(
                        pending_key.clone(),
                        1,
                        "policy-v2",
                        legacy_decision,
                        timestamp(12),
                    )?),
                    &timestamp(13),
                ),
                Err(RuntimeStoreError::RetryAuthorizationRejected)
            ));
        }
        migrated.resolve_effect(
            &pending_key,
            EffectResolution::RetryFailed(policy_authorization(
                pending_key.clone(),
                1,
                "policy-v2",
                "fresh-post-migration-decision",
                timestamp(12),
            )?),
            &timestamp(13),
        )?;
        let appended = migrated.append(&intent_for_projection(
            "post-migration-transition",
            14,
            CompatibilityProjection::EventLog,
        )?)?;
        assert_eq!(appended.sequence(), 2);
        let appended_recipe = appended
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        // The legacy row's already-receipted public sequence (1) is the only
        // proof of that transition's frontier; the new transition continues
        // from it exactly as `allocate_event_projection_recipe` requires.
        assert_eq!(appended_recipe.public_sequence(), 2);
        migrated.record_projection(&ProjectionReceipt::event_log(
            appended.sequence(),
            event_jsonl(
                appended_recipe.public_sequence(),
                appended_recipe.event_id(),
                14,
            )?,
            timestamp(15),
        )?)?;
        migrated.close()?;

        let reopened = open_store(boundary)?;
        assert_eq!(
            reopened.pending_effects(10)?[0].idempotency_key(),
            pending_key
        );
        assert_eq!(
            read_schema_versions(reopened.connection()?)?,
            (DATABASE_SCHEMA_VERSION, DATABASE_SCHEMA_VERSION)
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn migration_crash_rolls_back_then_reopens_cleanly() -> Result<(), Box<dyn std::error::Error>> {
        for (crash_point, exit_code) in [("after_backfill", 94), ("after_validation", 95)] {
            let home = TestHome::new()?;
            let (pending_key, _) = create_populated_v1_database(&home)?;
            let before_crash = Connection::open(home.database())?;
            let original_evidence: String = before_crash.query_row(
                "SELECT evidence_json FROM outbox_attempt_observation
                 WHERE observation_sequence = 2",
                [],
                |row| row.get(0),
            )?;
            drop(before_crash);
            let output = Command::new(std::env::current_exe()?)
                .arg("--exact")
                .arg("runtime_store::tests::runtime_store_migration_crash_helper")
                .arg("--nocapture")
                .env(MIGRATION_CRASH_HELPER_HOME, &home.path)
                .env("NANIKA_RUNTIME_STORE_MIGRATION_CRASH", crash_point)
                .output()?;
            assert_eq!(output.status.code(), Some(exit_code));
            let after_crash = Connection::open(home.database())?;
            assert_eq!(
                read_schema_versions(&after_crash)?,
                (
                    LEGACY_DATABASE_SCHEMA_VERSION,
                    LEGACY_DATABASE_SCHEMA_VERSION
                )
            );
            assert_eq!(schema_digest(&after_crash)?, LEGACY_SCHEMA_DIGEST);
            let migrated_table_count: i64 = after_crash.query_row(
                "SELECT count(*) FROM sqlite_schema
                 WHERE type = 'table' AND name = 'legacy_retry_authorization_consumption'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(migrated_table_count, 0);
            let migrated_column_count: i64 = after_crash.query_row(
                "SELECT count(*) FROM pragma_table_info('outbox') WHERE name = 'operation_slot'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(migrated_column_count, 0);
            let retained_evidence: String = after_crash.query_row(
                "SELECT evidence_json FROM outbox_attempt_observation
                 WHERE observation_sequence = 2",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(retained_evidence, original_evidence);
            drop(after_crash);

            let migrated = open_store(home.boundary()?)?;
            assert_eq!(
                migrated.pending_effects(10)?[0].idempotency_key(),
                pending_key
            );
            migrated.close()?;
        }
        Ok(())
    }

    #[test]
    fn v3_to_v4_migration_crash_rolls_back_without_rewriting_journal_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        for (crash_point, exit_code) in [("after_schema", 98), ("after_validation", 99)] {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_store(Arc::clone(&boundary))?;
            let effect = effect(
                OutboxEffectKind::PluginProcess,
                1,
                serde_json::json!({"plugin":"legacy-v3-crash"}),
            )?;
            let key = effect.idempotency_key().to_owned();
            store.append(&intent("v3-crash-row", 1)?.with_outbox(effect)?)?;
            store.close()?;

            let connection = Connection::open(home.database())?;
            let journal_before: (String, String, String) = connection.query_row(
                "SELECT payload_json, outbox_fingerprint, checksum FROM journal WHERE sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            connection.execute_batch(
                "DROP TABLE audit_chain; DROP TABLE audit_chain_head; DROP TABLE knowledge_publication; DROP TABLE reasoning_revision; DROP TABLE reasoning_attempt; DROP TABLE reasoning_review; DROP TABLE reasoning_handoff; DROP TABLE reasoning_assignment; DROP TABLE reasoning_evidence; DROP TABLE reasoning_criterion; DROP TABLE reasoning_assumption; DROP TABLE reasoning_record; DROP TRIGGER runtime_schema_reject_boundary_kind_update;
                 DROP TRIGGER runtime_schema_reject_delete;
                 DROP TRIGGER runtime_schema_reject_insert;
                 ALTER TABLE runtime_schema RENAME TO runtime_schema_v4;
                 CREATE TABLE runtime_schema (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    version INTEGER NOT NULL CHECK (version > 0)
                 );
                 INSERT INTO runtime_schema(singleton, version) VALUES (1, 3);
                 DROP TABLE runtime_schema_v4;
                 PRAGMA user_version = 3;",
            )?;
            assert_eq!(schema_digest(&connection)?, NATIVE_V3_SCHEMA_DIGEST);
            drop(connection);

            let output = Command::new(std::env::current_exe()?)
                .arg("--exact")
                .arg("runtime_store::tests::runtime_store_migration_crash_helper")
                .arg("--nocapture")
                .env(MIGRATION_CRASH_HELPER_HOME, &home.path)
                .env("NANIKA_RUNTIME_STORE_V4_MIGRATION_CRASH", crash_point)
                .output()?;
            assert_eq!(output.status.code(), Some(exit_code));

            let after_crash = Connection::open(home.database())?;
            assert_eq!(
                read_schema_versions(&after_crash)?,
                (
                    PROCESS_DATABASE_SCHEMA_VERSION,
                    PROCESS_DATABASE_SCHEMA_VERSION
                )
            );
            assert_eq!(schema_digest(&after_crash)?, NATIVE_V3_SCHEMA_DIGEST);
            let boundary_columns: i64 = after_crash.query_row(
                "SELECT count(*) FROM sqlite_schema
                 JOIN pragma_table_info('runtime_schema')
                 WHERE sqlite_schema.name = 'runtime_schema'
                   AND pragma_table_info.name = 'boundary_kind'",
                [],
                |row| row.get(0),
            )?;
            assert_eq!(boundary_columns, 0);
            let journal_after: (String, String, String) = after_crash.query_row(
                "SELECT payload_json, outbox_fingerprint, checksum FROM journal WHERE sequence = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(journal_after, journal_before);
            drop(after_crash);

            let reopened = open_store(boundary)?;
            assert_eq!(reopened.pending_effects(10)?[0].idempotency_key(), key);
            assert_eq!(
                read_schema_versions(reopened.connection()?)?,
                (DATABASE_SCHEMA_VERSION, DATABASE_SCHEMA_VERSION)
            );
            reopened.close()?;
        }
        Ok(())
    }

    #[test]
    fn unsealed_v3_fake_sidecars_private_kinds_are_never_adopted()
    -> Result<(), Box<dyn std::error::Error>> {
        for transition_kind in [
            PRIVATE_PROCESS_CLAIM_TRANSITION_KIND,
            TERMINAL_DECISION_TRANSITION_KIND,
        ] {
            for private_opener in [false, true] {
                let home = TestHome::new()?;
                let retained_before =
                    create_unsealed_v3_fake_sidecars_private_row(&home, transition_kind)?;
                let files_before = runtime_file_snapshot(&home)?;
                assert_eq!(files_before.compatibility_boundary_seal, None);
                assert_eq!(files_before.private_boundary_seal, None);
                assert_eq!(files_before.legacy_boundary_seal, None);

                let boundary = home.boundary()?;
                let rejected = if private_opener {
                    matches!(
                        open_private_store(boundary),
                        Err(RuntimeStoreError::BoundaryKindMismatch)
                    )
                } else {
                    matches!(
                        open_store(boundary),
                        Err(RuntimeStoreError::BoundaryKindMismatch)
                    )
                };
                assert!(rejected, "{transition_kind} private={private_opener}");

                let files_after = runtime_file_snapshot(&home)?;
                if private_opener {
                    assert_eq!(
                        files_after, files_before,
                        "{transition_kind} private opener touched a store file"
                    );
                } else {
                    assert_eq!(
                        files_after.database, files_before.database,
                        "{transition_kind}"
                    );
                    assert_eq!(files_after.wal, files_before.wal, "{transition_kind}");
                    assert_eq!(
                        files_after.compatibility_boundary_seal,
                        files_before.compatibility_boundary_seal,
                        "{transition_kind}"
                    );
                    assert_eq!(
                        files_after.private_boundary_seal, files_before.private_boundary_seal,
                        "{transition_kind}"
                    );
                    assert_eq!(
                        files_after.legacy_boundary_seal, files_before.legacy_boundary_seal,
                        "{transition_kind}"
                    );
                }

                let connection = Connection::open(home.database())?;
                assert_eq!(schema_digest(&connection)?, NATIVE_V3_SCHEMA_DIGEST);
                assert_eq!(
                    read_schema_versions(&connection)?,
                    (
                        PROCESS_DATABASE_SCHEMA_VERSION,
                        PROCESS_DATABASE_SCHEMA_VERSION
                    )
                );
                let boundary_columns: i64 = connection.query_row(
                    "SELECT count(*) FROM pragma_table_info('runtime_schema')
                     WHERE name = 'boundary_kind'",
                    [],
                    |row| row.get(0),
                )?;
                assert_eq!(boundary_columns, 0);
                let retained_after: (String, String) = connection.query_row(
                    "SELECT transition_kind, checksum FROM journal WHERE sequence = 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                assert_eq!(retained_after, retained_before);
                drop(connection);
            }
        }
        Ok(())
    }

    #[test]
    fn corrupt_late_v1_retry_rolls_back_exactly_and_can_be_repaired()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        create_populated_v1_database_with_retry_evidence(&home, true)?;
        let connection = Connection::open(home.database())?;
        let corrupt_evidence: String = connection.query_row(
            "SELECT evidence_json FROM outbox_attempt_observation
             WHERE observation_sequence = 6",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(schema_digest(&connection)?, LEGACY_SCHEMA_DIGEST);
        drop(connection);

        assert!(matches!(
            open_store(home.boundary()?),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        let after_failure = Connection::open(home.database())?;
        assert_eq!(
            read_schema_versions(&after_failure)?,
            (
                LEGACY_DATABASE_SCHEMA_VERSION,
                LEGACY_DATABASE_SCHEMA_VERSION
            )
        );
        assert_eq!(schema_digest(&after_failure)?, LEGACY_SCHEMA_DIGEST);
        let retained_evidence: String = after_failure.query_row(
            "SELECT evidence_json FROM outbox_attempt_observation
             WHERE observation_sequence = 6",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(retained_evidence, corrupt_evidence);
        let migrated_table_count: i64 = after_failure.query_row(
            "SELECT count(*) FROM sqlite_schema
             WHERE type = 'table' AND name = 'legacy_retry_authorization_consumption'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(migrated_table_count, 0);
        let migrated_column_count: i64 = after_failure.query_row(
            "SELECT count(*) FROM pragma_table_info('outbox') WHERE name = 'operation_slot'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(migrated_column_count, 0);

        let repaired_evidence = legacy_v1_evidence(
            "operator_authorized_retry",
            BTreeMap::from([
                ("decided_at_utc".to_owned(), timestamp(7)),
                (
                    "decision_id".to_owned(),
                    "legacy-operator-uncertain".to_owned(),
                ),
            ]),
        )?;
        after_failure.execute_batch(
            "DROP TRIGGER outbox_attempt_observation_reject_update;
             CREATE TEMP TABLE repaired_legacy_evidence(value TEXT NOT NULL);",
        )?;
        after_failure.execute(
            "INSERT INTO repaired_legacy_evidence(value) VALUES (?1)",
            [&repaired_evidence],
        )?;
        after_failure.execute_batch(
            "UPDATE outbox_attempt_observation
                SET evidence_json = (SELECT value FROM repaired_legacy_evidence)
                WHERE observation_sequence = 6;
             DROP TABLE repaired_legacy_evidence;
             CREATE TRIGGER outbox_attempt_observation_reject_update
                    BEFORE UPDATE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are immutable'); END;",
        )?;
        assert_eq!(schema_digest(&after_failure)?, LEGACY_SCHEMA_DIGEST);
        drop(after_failure);

        let migrated = open_store(home.boundary()?)?;
        let legacy_rows: i64 = migrated.connection()?.query_row(
            "SELECT count(*) FROM legacy_retry_authorization_consumption",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(legacy_rows, 3);
        migrated.close()?;

        let reopened = open_store(home.boundary()?)?;
        let legacy_rows: i64 = reopened.connection()?.query_row(
            "SELECT count(*) FROM legacy_retry_authorization_consumption",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(legacy_rows, 3);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn runtime_store_migration_crash_helper() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os(MIGRATION_CRASH_HELPER_HOME) else {
            return Ok(());
        };
        let path = fs::canonicalize(path)?;
        let boundary = Arc::new(ProductionBoundary::from_canonical_root(&path)?);
        let _store = open_store(boundary)?;
        Err("migration crash point did not terminate the helper".into())
    }

    #[test]
    fn exact_claim_mints_attempt_bound_redacted_capability()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let (key, sequence) = append_and_acknowledge(
            &mut store,
            "transition-exact-capability",
            effect_in_slot(
                OutboxEffectKind::ProviderProcess,
                "primary-reasoner",
                7,
                serde_json::json!({"secret":"do-not-print"}),
            )?,
        )?;

        let claimed = store.claim_exact(&key, &timestamp(3))?;

        assert_eq!(claimed.idempotency_key(), key);
        assert_eq!(claimed.journal_sequence(), sequence);
        assert_eq!(claimed.mission_id().as_str(), "mission-1");
        assert_eq!(claimed.phase_id(), Some("phase-1"));
        assert_eq!(claimed.effect_kind(), OutboxEffectKind::ProviderProcess);
        assert_eq!(claimed.operation_slot().as_str(), "primary-reasoner");
        assert_eq!(claimed.logical_attempt(), 7);
        assert_eq!(claimed.claim_attempt(), 1);
        assert_eq!(
            claimed.payload(),
            &serde_json::json!({"secret":"do-not-print"})
        );
        let debug = format!("{claimed:?}");
        assert!(debug.contains("ClaimedOutboxEffect"));
        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&key));
        assert!(!debug.contains("do-not-print"));
        store.close()?;
        Ok(())
    }

    #[test]
    fn exact_claim_refuses_queue_substitution_without_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let first = effect_in_slot(
            OutboxEffectKind::ProviderProcess,
            "reasoner-a",
            1,
            serde_json::json!({"model":"a"}),
        )?;
        let second = effect_in_slot(
            OutboxEffectKind::ProviderProcess,
            "reasoner-b",
            1,
            serde_json::json!({"model":"b"}),
        )?;
        let commit = store.append(
            &intent("transition-exact-order", 1)?
                .with_outbox(first)?
                .with_outbox(second)?,
        )?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;
        let before = store.pending_effects(10)?;
        let expected_first = before[0].idempotency_key().to_owned();
        let wrong_expected = before[1].idempotency_key().to_owned();

        assert!(matches!(
            store.claim_exact(&wrong_expected, &timestamp(3)),
            Err(RuntimeStoreError::ExpectedEffectOutOfOrder)
        ));
        let unchanged = store.pending_effects(10)?;
        assert_eq!(
            unchanged
                .iter()
                .map(|effect| (effect.idempotency_key(), effect.attempts()))
                .collect::<Vec<_>>(),
            vec![(expected_first.as_str(), 0), (wrong_expected.as_str(), 0)]
        );
        let missing_key = format!("effect_{}", "0".repeat(64));
        assert!(matches!(
            store.claim_exact(&missing_key, &timestamp(4)),
            Err(RuntimeStoreError::ExpectedEffectNotClaimable)
        ));
        assert!(
            store
                .pending_effects(10)?
                .iter()
                .all(|effect| effect.attempts() == 0)
        );
        let claimed = store.claim_exact(&expected_first, &timestamp(4))?;
        assert_eq!(claimed.idempotency_key(), expected_first);
        assert_eq!(
            store.pending_effects(10)?[0].idempotency_key(),
            wrong_expected
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn exact_claim_rejects_missing_and_unacknowledged_keys_without_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let effect = effect_in_slot(
            OutboxEffectKind::PluginProcess,
            "unacknowledged-plugin",
            1,
            serde_json::json!({"plugin":"fixture"}),
        )?;
        let key = effect.idempotency_key().to_owned();
        store.append(&intent("transition-unacknowledged", 1)?.with_outbox(effect)?)?;
        let missing_key = format!("effect_{}", "0".repeat(64));

        assert!(matches!(
            store.claim_exact(&missing_key, &timestamp(2)),
            Err(RuntimeStoreError::ExpectedEffectNotClaimable)
        ));
        assert!(matches!(
            store.claim_exact(&key, &timestamp(2)),
            Err(RuntimeStoreError::ExpectedEffectNotClaimable)
        ));
        let pending = store.pending_effects(10)?;
        assert_eq!(pending[0].state(), OutboxState::Pending);
        assert_eq!(pending[0].attempts(), 0);
        store.close()?;
        Ok(())
    }

    #[test]
    fn exact_claim_rejects_every_non_pending_state_without_mutation()
    -> Result<(), Box<dyn std::error::Error>> {
        for state in [
            OutboxState::Executing,
            OutboxState::Succeeded,
            OutboxState::Failed,
            OutboxState::Uncertain,
        ] {
            let home = TestHome::new()?;
            let mut store = open_store(home.boundary()?)?;
            let (key, _) = append_and_acknowledge(
                &mut store,
                &format!("transition-invalid-state-{state:?}"),
                effect_in_slot(
                    OutboxEffectKind::ProviderProcess,
                    &format!("invalid-state-{}", state.as_str()),
                    1,
                    serde_json::json!({"state":state.as_str()}),
                )?,
            )?;
            let claimed = store.claim_exact(&key, &timestamp(3))?;
            match state {
                OutboxState::Executing => {}
                OutboxState::Succeeded => {
                    store.record_execution_identity(
                        &key,
                        &ProcessExecutionIdentity::new(
                            claimed.claim_attempt(),
                            201,
                            201,
                            "boot-exact:201",
                            timestamp(3),
                        )?,
                    )?;
                    store.resolve_effect(
                        &key,
                        EffectResolution::Succeeded(EffectEvidence::exit_observed_success()),
                        &timestamp(4),
                    )?;
                }
                OutboxState::Failed => {
                    store.resolve_effect(
                        &key,
                        EffectResolution::NotStarted(EffectEvidence::process_not_started(
                            ProcessNotStartedEvidenceReason::SpawnFailed,
                        )),
                        &timestamp(4),
                    )?;
                }
                OutboxState::Uncertain => {
                    store.resolve_effect(
                        &key,
                        EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                            ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
                        )),
                        &timestamp(4),
                    )?;
                }
                OutboxState::Pending => unreachable!("pending is the only claimable state"),
            }
            let before = load_effect(store.connection()?, &key)?
                .ok_or("missing effect before rejected exact claim")?;

            assert!(matches!(
                store.claim_exact(&key, &timestamp(5)),
                Err(RuntimeStoreError::ExpectedEffectNotClaimable)
            ));
            let after = load_effect(store.connection()?, &key)?
                .ok_or("missing effect after rejected exact claim")?;
            assert_eq!(after.state(), before.state());
            assert_eq!(after.attempts(), before.attempts());
            assert_eq!(after.updated_at_utc(), before.updated_at_utc());
            store.close()?;
        }
        Ok(())
    }

    #[test]
    fn exact_claim_duplicate_remains_executing_across_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (key, _) = append_and_acknowledge(
            &mut store,
            "transition-exact-reopen",
            effect_in_slot(
                OutboxEffectKind::GitCommand,
                "git-status",
                1,
                serde_json::json!({"command":"status"}),
            )?,
        )?;
        let first = store.claim_exact(&key, &timestamp(3))?;
        assert_eq!(first.claim_attempt(), 1);
        assert!(matches!(
            store.claim_exact(&key, &timestamp(4)),
            Err(RuntimeStoreError::ExpectedEffectNotClaimable)
        ));
        store.close()?;

        let mut reopened = open_store(boundary)?;
        assert!(matches!(
            reopened.claim_exact(&key, &timestamp(5)),
            Err(RuntimeStoreError::ExpectedEffectNotClaimable)
        ));
        let recovery = reopened.recovery_effects(10)?;
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].idempotency_key(), key);
        assert_eq!(recovery[0].state(), OutboxState::Executing);
        assert_eq!(recovery[0].attempts(), 1);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn exact_claim_rolls_back_when_attempt_claim_insert_fails()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let (key, _) = append_and_acknowledge(
            &mut store,
            "transition-exact-rollback",
            effect_in_slot(
                OutboxEffectKind::PluginProcess,
                "rollback-probe",
                1,
                serde_json::json!({"plugin":"probe"}),
            )?,
        )?;
        store.connection()?.execute_batch(
            "CREATE TEMP TRIGGER exact_claim_injected_conflict
             BEFORE INSERT ON main.outbox_attempt_claim
             BEGIN SELECT RAISE(ABORT, 'injected exact claim conflict'); END;",
        )?;

        assert!(matches!(
            store.claim_exact(&key, &timestamp(3)),
            Err(RuntimeStoreError::Operation {
                operation: "record outbox attempt claim",
                ..
            })
        ));
        let rolled_back = load_effect(store.connection()?, &key)?
            .ok_or("missing effect after exact claim rollback")?;
        assert_eq!(rolled_back.state(), OutboxState::Pending);
        assert_eq!(rolled_back.attempts(), 0);
        let claim_rows: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM outbox_attempt_claim WHERE idempotency_key = ?1",
            [&key],
            |row| row.get(0),
        )?;
        assert_eq!(claim_rows, 0);
        store
            .connection()?
            .execute_batch("DROP TRIGGER temp.exact_claim_injected_conflict;")?;
        assert_eq!(store.claim_exact(&key, &timestamp(4))?.claim_attempt(), 1);
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn prepared_process_claim_proves_rollback_without_resurrecting_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_store(home.boundary()?)?;
        let (key, binding) =
            append_typed_process_pending(&mut store, "prepared-rollback", &request, 1)?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        store.connection()?.execute_batch(
            "CREATE TEMP TRIGGER prepared_claim_injected_conflict
             BEFORE INSERT ON main.outbox_attempt_claim
             BEGIN SELECT RAISE(ABORT, 'injected prepared claim conflict'); END;",
        )?;

        assert!(matches!(
            store.claim_prepared_process(prepared, &timestamp(3)),
            Err(ExactProcessClaimError::ProvenNotCommitted)
        ));
        let durable = load_effect(store.connection()?, &key)?.ok_or("missing process effect")?;
        assert_eq!(durable.state(), OutboxState::Pending);
        assert_eq!(durable.attempts(), 0);
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn private_process_claim_and_recovery_marker_commit_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(home.boundary()?)?;
        let (key, binding) =
            append_typed_process_pending_private(&mut store, "claim-marker-atomic", &request, 1)?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        store.connection()?.execute_batch(
            "CREATE TEMP TRIGGER process_claim_marker_injected_failure
             BEFORE INSERT ON main.journal
             WHEN NEW.transition_kind = 'cell3.process_attempt_claimed'
             BEGIN SELECT RAISE(ABORT, 'injected process claim marker failure'); END;",
        )?;

        assert!(matches!(
            store.claim_prepared_process(prepared, &timestamp(3)),
            Err(ExactProcessClaimError::ProvenNotCommitted)
        ));
        let rolled_back = store
            .exact_outbox_snapshot(&key)?
            .ok_or("rolled-back process effect is missing")?;
        assert_eq!(rolled_back.effect().state(), OutboxState::Pending);
        assert_eq!(rolled_back.effect().attempts(), 0);
        let claim_count: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM outbox_attempt_claim WHERE idempotency_key = ?1",
            [&key],
            |row| row.get(0),
        )?;
        let marker_count: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM journal
             WHERE transition_kind = 'cell3.process_attempt_claimed'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(claim_count, 0);
        assert_eq!(marker_count, 0);

        store
            .connection()?
            .execute_batch("DROP TRIGGER temp.process_claim_marker_injected_failure;")?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        assert_eq!(
            store
                .claim_prepared_process(prepared, &timestamp(4))
                .map_err(|_| "prepared process claim retry failed")?
                .claim_attempt(),
            1
        );
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovered_process_attempt_before_spawn_resolves_not_started_exactly_once()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        let (key, binding) = append_typed_process_pending_private(
            &mut store,
            "recover-never-authorized",
            &request,
            1,
        )?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let claimed = store
            .claim_prepared_process(prepared, &timestamp(3))
            .map_err(|_| "prepared process claim failed")?;

        let recovered = store.recover_exact_process_attempt(&binding, &request)?;

        assert!(matches!(
            recovered.kind,
            RecoveredProcessAttemptKind::ClaimedBeforeSpawn { .. }
        ));
        store.resolve_recovered_before_spawn(recovered, &timestamp(4))?;
        let resolved = store
            .exact_outbox_snapshot(&key)?
            .ok_or("recovered process effect is missing")?;
        assert_eq!(resolved.effect().state(), OutboxState::Failed);
        assert_eq!(resolved.effect().attempts(), claimed.claim_attempt());
        assert_eq!(
            resolved
                .current_observation()
                .and_then(|observation| observation.evidence().process_not_started_reason()),
            Some(ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn)
        );
        let history = store.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        store.close()?;

        let reopened = open_private_store(boundary)?;
        assert_eq!(reopened.attempt_history(&key, 10)?, history);
        assert!(matches!(
            reopened.recover_exact_process_attempt(&binding, &request),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        reopened.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovered_spawn_permit_without_identity_stays_unresolved_and_idempotent()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        let (claimed, binding) =
            append_and_claim_typed_process_private(&mut store, "recover-permitted", &request, 1)?;
        store.record_process_spawn_permit(&claimed, &request, &timestamp(3))?;
        store.record_process_spawn_permit(&claimed, &request, &timestamp(4))?;

        let recovered = store.recover_exact_process_attempt(&binding, &request)?;
        assert_eq!(
            recovered.classification(),
            RecoveredProcessAttemptClassification::SpawnUnobserved
        );
        let snapshot = store
            .exact_outbox_snapshot(claimed.idempotency_key())?
            .ok_or("permitted process effect is missing")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Executing);
        assert!(snapshot.effect().execution_identity().is_none());
        assert!(snapshot.current_observation().is_none());
        let marker_count: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM journal
             WHERE transition_kind IN (?1, ?2)",
            params![
                PROCESS_ATTEMPT_CLAIMED_TRANSITION_KIND,
                PROCESS_SPAWN_PERMITTED_TRANSITION_KIND
            ],
            |row| row.get(0),
        )?;
        assert_eq!(marker_count, 2);
        store.close()?;

        let reopened = open_private_store(boundary)?;
        assert_eq!(
            reopened
                .recover_exact_process_attempt(&binding, &request)?
                .classification(),
            RecoveredProcessAttemptClassification::SpawnUnobserved
        );
        reopened.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovered_blocked_launcher_retains_exact_cleanup_identity_without_release()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(home.boundary()?)?;
        let (claimed, binding) =
            append_and_claim_typed_process_private(&mut store, "recover-blocked", &request, 1)?;
        let request_binding = ProcessRequestBinding::from_bytes(request.fingerprint().into_bytes());
        let identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_050,
            1_050,
            "fixture:start:1",
            timestamp(4),
        )?;
        let gate = observed_gate(&request_binding, &identity);
        prepare_blocked_identity_for_test(&mut store, &claimed, &request, &gate, &identity, 3)?;

        let recovered = store.recover_exact_process_attempt(&binding, &request)?;
        assert_eq!(
            recovered.classification(),
            RecoveredProcessAttemptClassification::BlockedLauncher
        );
        let cleanup = recovered.into_cleanup()?;
        assert_eq!(cleanup.pid(), identity.pid());
        assert_eq!(cleanup.process_group_id(), identity.process_group_id());
        assert_eq!(
            cleanup.process_start_identity(),
            identity.process_start_identity()
        );
        assert!(
            !store.process_release_was_authorized(
                claimed.idempotency_key(),
                claimed.claim_attempt()
            )?
        );
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovered_unreleased_uncertainty_is_cleanup_only() -> Result<(), Box<dyn std::error::Error>>
    {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(home.boundary()?)?;
        let (claimed, binding) =
            append_and_claim_typed_process_private(&mut store, "recover-unreleased", &request, 1)?;
        let request_binding = ProcessRequestBinding::from_bytes(request.fingerprint().into_bytes());
        let identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_049,
            1_049,
            "fixture:start:1",
            timestamp(4),
        )?;
        let gate = observed_gate(&request_binding, &identity);
        prepare_blocked_identity_for_test(&mut store, &claimed, &request, &gate, &identity, 3)?;
        let key = claimed.idempotency_key().to_owned();
        let attempt = claimed.claim_attempt();
        store.resolve_claimed_process(
            &claimed.into_terminal(),
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::SupervisorOutcomeLost,
            )),
            &timestamp(5),
        )?;

        let recovered = store.recover_exact_process_attempt(&binding, &request)?;
        assert_eq!(
            recovered.classification(),
            RecoveredProcessAttemptClassification::UnreleasedUncertain
        );
        let cleanup = recovered.into_cleanup()?;
        assert_eq!(cleanup.pid(), identity.pid());
        assert_eq!(cleanup.process_group_id(), identity.process_group_id());
        assert!(!store.process_release_was_authorized(&key, attempt)?);
        let snapshot = store
            .exact_outbox_snapshot(&key)?
            .ok_or("unreleased uncertain process is missing")?;
        assert_eq!(snapshot.effect().state(), OutboxState::Uncertain);
        assert_eq!(store.attempt_history(&key, 10)?.len(), 1);
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn legacy_markerless_active_process_remains_unresolved()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_store(home.boundary()?)?;
        let (_, binding) = append_typed_process_pending(&mut store, "legacy-active", &request, 1)?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let _claimed = store
            .claim_prepared_process(prepared, &timestamp(3))
            .map_err(|_| "legacy prepared process claim failed")?;

        assert_eq!(
            store
                .recover_exact_process_attempt(&binding, &request)?
                .classification(),
            RecoveredProcessAttemptClassification::Unresolved
        );
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovered_released_process_is_bound_to_exact_request_and_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(home.boundary()?)?;
        let (claimed, binding) =
            append_and_claim_typed_process_private(&mut store, "recover-released", &request, 1)?;
        let identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_051,
            1_051,
            "fixture:start:1",
            timestamp(4),
        )?;
        let _authorization = store.authorize_process_release_for_test(
            &claimed,
            &request,
            &identity,
            &timestamp(5),
        )?;

        let recovered = store.recover_exact_process_attempt(&binding, &request)?;

        let RecoveredProcessAttemptKind::AuthorizedWithoutStarted {
            terminal,
            identity: recovered_identity,
        } = recovered.kind
        else {
            return Err("released process was not reconstructed".into());
        };
        assert_eq!(recovered_identity, identity);
        assert_eq!(terminal.claimed().claim_attempt(), claimed.claim_attempt());

        let changed_request = typed_process_request(ProcessPurpose::ProviderWorker)?
            .with_argument("different-request")?;
        assert!(matches!(
            store.recover_exact_process_attempt(&binding, &changed_request),
            Err(RuntimeStoreError::ProcessRequestMismatch)
        ));
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn started_observed_is_ordered_idempotent_and_recovers_as_started()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        let (claimed, binding) =
            append_and_claim_typed_process_private(&mut store, "started-observed", &request, 1)?;
        let identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_052,
            1_052,
            "fixture:start:2",
            timestamp(4),
        )?;
        let mut authorization = store.authorize_process_release_for_test(
            &claimed,
            &request,
            &identity,
            &timestamp(5),
        )?;

        store.record_process_started_observed_for_test(
            &claimed,
            &request,
            &authorization,
            &timestamp(6),
        )?;
        store.record_process_started_observed_for_test(
            &claimed,
            &request,
            &authorization,
            &timestamp(7),
        )?;
        assert_eq!(
            store
                .recover_exact_process_attempt(&binding, &request)?
                .classification(),
            RecoveredProcessAttemptClassification::StartedExecuting
        );

        let claim_sequence: i64 = store.connection()?.query_row(
            "SELECT sequence FROM journal WHERE transition_kind = ?1",
            [PROCESS_ATTEMPT_CLAIMED_TRANSITION_KIND],
            |row| row.get(0),
        )?;
        let permit_sequence: i64 = store.connection()?.query_row(
            "SELECT sequence FROM journal WHERE transition_kind = ?1",
            [PROCESS_SPAWN_PERMITTED_TRANSITION_KIND],
            |row| row.get(0),
        )?;
        let started_sequence: i64 = store.connection()?.query_row(
            "SELECT sequence FROM journal WHERE transition_kind = ?1",
            [PROCESS_STARTED_OBSERVED_TRANSITION_KIND],
            |row| row.get(0),
        )?;
        let started_count: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM journal WHERE transition_kind = ?1",
            [PROCESS_STARTED_OBSERVED_TRANSITION_KIND],
            |row| row.get(0),
        )?;
        assert!(claim_sequence < permit_sequence);
        assert!(permit_sequence < started_sequence);
        assert_eq!(started_count, 1);

        authorization.pid = authorization.pid.saturating_add(1);
        assert!(matches!(
            store.record_process_started_observed_for_test(
                &claimed,
                &request,
                &authorization,
                &timestamp(8),
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        store.close()?;

        let reopened = open_private_store(boundary)?;
        assert_eq!(
            reopened
                .recover_exact_process_attempt(&binding, &request)?
                .classification(),
            RecoveredProcessAttemptClassification::StartedExecuting
        );
        reopened.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn started_observed_identity_tampering_fails_reopen_with_schema_intact()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        let (claimed, _) =
            append_and_claim_typed_process_private(&mut store, "tamper-started", &request, 1)?;
        let identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_053,
            1_053,
            "fixture:start:3",
            timestamp(4),
        )?;
        let authorization = store.authorize_process_release_for_test(
            &claimed,
            &request,
            &identity,
            &timestamp(5),
        )?;
        store.record_process_started_observed_for_test(
            &claimed,
            &request,
            &authorization,
            &timestamp(6),
        )?;
        let started_sequence: i64 = store.connection()?.query_row(
            "SELECT sequence FROM journal WHERE transition_kind = ?1",
            [PROCESS_STARTED_OBSERVED_TRANSITION_KIND],
            |row| row.get(0),
        )?;
        let payload_json: String = store.connection()?.query_row(
            "SELECT payload_json FROM journal WHERE sequence = ?1",
            [started_sequence],
            |row| row.get(0),
        )?;
        store.close()?;

        let mut forged: Value = serde_json::from_str(&payload_json)?;
        let object = forged
            .as_object_mut()
            .ok_or("started marker payload is not an object")?;
        object.insert("pid".to_owned(), Value::from(1_054_u32));
        let forged_json = canonical_json(&forged, "forged started process marker")?;
        forge_journal_payload_json(&home, started_sequence, &forged_json)?;
        let raw = Connection::open(home.database())?;
        assert_eq!(schema_digest(&raw)?, NATIVE_V7_SCHEMA_DIGEST);
        drop(raw);

        assert!(matches!(
            open_private_store(boundary),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn legacy_markerless_authorized_identity_recovers_as_c5_without_release_authority()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        let (claimed, binding) =
            append_and_claim_typed_process_private(&mut store, "legacy-authorized", &request, 1)?;
        let identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_055,
            1_055,
            "fixture:start:legacy",
            timestamp(4),
        )?;
        let _authorization = store.authorize_process_release_for_test(
            &claimed,
            &request,
            &identity,
            &timestamp(5),
        )?;
        store.close()?;

        let raw = Connection::open(home.database())?;
        let journal_delete_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'journal_reject_delete'",
            [],
            |row| row.get(0),
        )?;
        let ack_delete_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema
             WHERE type = 'trigger' AND name = 'command_ack_reject_delete'",
            [],
            |row| row.get(0),
        )?;
        raw.execute_batch(
            "DROP TRIGGER journal_reject_delete;
             DROP TRIGGER command_ack_reject_delete;
             DELETE FROM command_ack
              WHERE journal_sequence IN (
                SELECT sequence FROM journal
                 WHERE transition_kind IN (
                    'cell3.process_attempt_claimed',
                    'cell3.process_spawn_permitted'
                 )
              );
             DELETE FROM journal
              WHERE transition_kind IN (
                'cell3.process_attempt_claimed',
                'cell3.process_spawn_permitted'
              );",
        )?;
        raw.execute_batch(&format!("{journal_delete_sql};\n{ack_delete_sql};"))?;
        assert_eq!(schema_digest(&raw)?, NATIVE_V7_SCHEMA_DIGEST);
        drop(raw);

        let reopened = open_private_store(boundary)?;
        let recovered = reopened.recover_exact_process_attempt(&binding, &request)?;
        assert_eq!(
            recovered.classification(),
            RecoveredProcessAttemptClassification::AuthorizedWithoutStarted
        );
        assert!(matches!(
            recovered.kind,
            RecoveredProcessAttemptKind::AuthorizedWithoutStarted { .. }
        ));
        reopened.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recovered_identityless_uncertainty_remains_unresolved()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_store(home.boundary()?)?;
        let (key, binding) =
            append_typed_process_pending(&mut store, "recover-unresolved", &request, 1)?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let claimed = store
            .claim_prepared_process(prepared, &timestamp(3))
            .map_err(|_| "prepared process claim failed")?;
        store.resolve_claimed_process(
            &claimed.into_terminal(),
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::CommitIndeterminate,
            )),
            &timestamp(4),
        )?;

        let recovered = store.recover_exact_process_attempt(&binding, &request)?;

        assert!(matches!(
            recovered.kind,
            RecoveredProcessAttemptKind::Unresolved
        ));
        let unchanged = store
            .exact_outbox_snapshot(&key)?
            .ok_or("uncertain process effect is missing")?;
        assert_eq!(unchanged.effect().state(), OutboxState::Uncertain);
        assert_eq!(
            unchanged
                .current_observation()
                .map(EffectObservation::attempt),
            Some(1)
        );
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn typed_process_resolution_rejects_ambient_key_and_accepts_exact_terminal_capability()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_store(home.boundary()?)?;
        let (key, binding) =
            append_typed_process_pending(&mut store, "typed-terminal-capability", &request, 1)?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let claimed = store
            .claim_prepared_process(prepared, &timestamp(3))
            .map_err(|_| "prepared process claim failed")?;
        let observation_count = |store: &RuntimeStore| -> Result<i64, RuntimeStoreError> {
            store
                .connection()?
                .query_row(
                    "SELECT count(*) FROM outbox_attempt_observation WHERE idempotency_key = ?1",
                    [&key],
                    |row| row.get(0),
                )
                .map_err(|source| {
                    RuntimeStoreError::operation("count process observations", source)
                })
        };
        assert_eq!(observation_count(&store)?, 0);
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::NotStarted(EffectEvidence::process_not_started(
                    ProcessNotStartedEvidenceReason::SpawnFailed,
                )),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::ProcessIntentRequired)
        ));
        let unchanged = load_effect(store.connection()?, &key)?
            .ok_or("missing typed process after ambient resolution rejection")?;
        assert_eq!(unchanged.state(), OutboxState::Executing);
        assert_eq!(unchanged.attempts(), claimed.claim_attempt());
        assert_eq!(observation_count(&store)?, 0);
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                    ProcessUncertaintyEvidence::CommitIndeterminate,
                )),
                &timestamp(5),
            ),
            Err(RuntimeStoreError::ProcessIntentRequired)
        ));
        let unchanged = load_effect(store.connection()?, &key)?
            .ok_or("missing typed process after ambient uncertainty rejection")?;
        assert_eq!(unchanged.state(), OutboxState::Executing);
        assert_eq!(unchanged.attempts(), claimed.claim_attempt());
        assert_eq!(observation_count(&store)?, 0);

        let terminal = claimed.into_terminal();
        store.resolve_claimed_process(
            &terminal,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(6),
        )?;
        assert_eq!(observation_count(&store)?, 1);
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn prepared_process_claim_supports_exact_pending_retry_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_store(home.boundary()?)?;
        let (key, binding) =
            append_typed_process_pending(&mut store, "prepared-retry", &request, 1)?;
        let first_prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let first = store
            .claim_prepared_process(first_prepared, &timestamp(3))
            .map_err(|_| "first prepared process claim failed")?;
        let first_attempt = first.claim_attempt();
        let first = first.into_terminal();
        store.resolve_claimed_process(
            &first,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(4),
        )?;
        store.resolve_claimed_process(
            &first,
            EffectResolution::RetryFailed(policy_authorization(
                &key,
                first_attempt,
                "prepared-retry-v1",
                "prepared-retry-decision",
                timestamp(5),
            )?),
            &timestamp(5),
        )?;

        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let second = store
            .claim_prepared_process(prepared, &timestamp(6))
            .map_err(|_| "prepared retry claim failed")?;
        assert_eq!(second.claim_attempt(), 2);
        let second = second.into_terminal();
        store.resolve_claimed_process(
            &second,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(7),
        )?;
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn exact_outbox_snapshot_never_carries_an_older_attempt_observation()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_store(home.boundary()?)?;
        let (key, binding) =
            append_typed_process_pending(&mut store, "snapshot-current-attempt", &request, 1)?;

        let first_prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let first = store
            .claim_prepared_process(first_prepared, &timestamp(3))
            .map_err(|_| "first prepared process claim failed")?;
        let first_attempt = first.claim_attempt();
        let first = first.into_terminal();
        store.resolve_claimed_process(
            &first,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(4),
        )?;
        store.resolve_claimed_process(
            &first,
            EffectResolution::RetryFailed(policy_authorization(
                &key,
                first_attempt,
                "snapshot-retry-v1",
                "snapshot-retry-decision",
                timestamp(5),
            )?),
            &timestamp(5),
        )?;

        let older_history = store.attempt_history(&key, 10)?;
        assert_eq!(
            older_history
                .iter()
                .map(EffectObservation::attempt)
                .collect::<Vec<_>>(),
            [1, 1]
        );
        assert_eq!(
            older_history
                .iter()
                .map(EffectObservation::state)
                .collect::<Vec<_>>(),
            [OutboxState::Failed, OutboxState::Pending]
        );

        let second_prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let second = store
            .claim_prepared_process(second_prepared, &timestamp(6))
            .map_err(|_| "second prepared process claim failed")?;
        assert_eq!(second.claim_attempt(), 2);
        let executing = store
            .exact_outbox_snapshot(&key)?
            .ok_or("current exact process effect is missing")?;
        assert_eq!(executing.effect().state(), OutboxState::Executing);
        assert_eq!(executing.effect().attempts(), 2);
        assert!(
            executing.current_observation().is_none(),
            "attempt-1 Pending observation leaked into attempt-2 Executing snapshot"
        );

        store.resolve_claimed_process(
            &second.into_terminal(),
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::Deadline,
            )),
            &timestamp(7),
        )?;
        let terminal = store
            .exact_outbox_snapshot(&key)?
            .ok_or("terminal exact process effect is missing")?;
        let current = terminal
            .current_observation()
            .ok_or("attempt-2 terminal observation is missing")?;
        assert_eq!(current.attempt(), 2);
        assert_eq!(current.state(), OutboxState::Failed);
        assert_eq!(
            current.evidence().process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::Deadline)
        );
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn exact_logical_outbox_snapshot_finds_request_bound_process_row()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mission_id = MissionId::new("mission-1")?;
        let operation_slot = EffectOperationSlot::new("logical-process-slot")?;
        let retained_request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let retained_effect = typed_process_effect(&retained_request, operation_slot.as_str(), 7)?;
        let retained_key = retained_effect.idempotency_key().to_owned();
        let mut store = open_private_store(home.boundary()?)?;
        store.append(
            &JournalIntent::new(
                "logical-process-slot-retained",
                Some(mission_id.clone()),
                "private.process.claim",
                serde_json::json!({"phase_id":"phase-1"}),
                timestamp(1),
            )?
            .with_outbox(retained_effect)?,
        )?;

        let candidate_request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-worker",
            "/fixture/different-worker",
        )?
        .with_max_output_bytes(32_768)?;
        let candidate_effect =
            typed_process_effect(&candidate_request, operation_slot.as_str(), 7)?;
        let candidate_key = candidate_effect.idempotency_key().to_owned();
        assert_ne!(candidate_key, retained_key);
        assert!(store.exact_outbox_snapshot(&candidate_key)?.is_none());

        let retained = store
            .exact_logical_outbox_snapshot(
                &mission_id,
                Some("phase-1"),
                OutboxEffectKind::ProviderProcess,
                &operation_slot,
                7,
            )?
            .ok_or("logical process slot did not retain its request-bound row")?;
        assert_eq!(retained.effect().idempotency_key(), retained_key.as_str());
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn exact_logical_outbox_snapshot_rejects_duplicate_slot_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mission_id = MissionId::new("mission-1")?;
        let operation_slot = EffectOperationSlot::new("duplicate-logical-slot")?;
        let first_request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let second_request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-worker",
            "/fixture/duplicate-worker",
        )?
        .with_max_output_bytes(32_768)?;
        let mut store = open_private_store(home.boundary()?)?;
        for (transition_id, request, step) in [
            ("duplicate-logical-slot-first", &first_request, 1),
            ("duplicate-logical-slot-second", &second_request, 2),
        ] {
            let effect = typed_process_effect(request, operation_slot.as_str(), 3)?;
            store.append(
                &JournalIntent::new(
                    transition_id,
                    Some(mission_id.clone()),
                    "private.process.claim",
                    serde_json::json!({"phase_id":"phase-1","step":step}),
                    timestamp(step),
                )?
                .with_outbox(effect)?,
            )?;
        }

        assert!(matches!(
            store.exact_logical_outbox_snapshot(
                &mission_id,
                Some("phase-1"),
                OutboxEffectKind::ProviderProcess,
                &operation_slot,
                3,
            ),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn stale_process_outcome_cannot_resolve_or_release_a_newer_retry_receipt()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(home.boundary()?)?;
        let (key, binding) =
            append_typed_process_pending_private(&mut store, "stale-terminal-plan", &request, 1)?;
        let first_prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let first = store
            .claim_prepared_process(first_prepared, &timestamp(3))
            .map_err(|_| "first process claim failed")?;
        let first_attempt = first.claim_attempt();
        let stale_plan = crate::process_receipt::map_authorized_process_outcome(
            orchestrator_process::AuthorizedProcessOutcome::NotStarted(
                orchestrator_process::ProcessNotStartedReceipt {
                    reason: orchestrator_process::ProcessNotStartedReason::SpawnFailed,
                    launcher_spawned: false,
                    launcher_identity: None,
                    elapsed: std::time::Duration::ZERO,
                    cancellation_observed: false,
                    deadline_observed: false,
                },
            ),
            &request,
            None,
            &first,
        )?;
        let first = first.into_terminal();
        store.resolve_claimed_process(
            &first,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(4),
        )?;
        store.resolve_claimed_process(
            &first,
            EffectResolution::RetryFailed(policy_authorization(
                &key,
                first_attempt,
                "stale-terminal-plan-v1",
                "stale-terminal-plan-decision",
                timestamp(5),
            )?),
            &timestamp(5),
        )?;
        let stale_plan = stale_plan.bind_terminal(first)?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let second = store
            .claim_prepared_process(prepared, &timestamp(6))
            .map_err(|_| "second process claim failed")?;

        assert!(stale_plan.commit(&mut store, &timestamp(7)).is_err());
        let still_executing =
            load_effect(store.connection()?, &key)?.ok_or("missing second process attempt")?;
        assert_eq!(still_executing.state(), OutboxState::Executing);
        assert_eq!(still_executing.attempts(), second.claim_attempt());
        let second = second.into_terminal();
        store.resolve_claimed_process(
            &second,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(8),
        )?;
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn mapped_process_outcome_rejects_same_attempt_cross_effect_substitution()
    -> Result<(), Box<dyn std::error::Error>> {
        for identical_request in [false, true] {
            let home = TestHome::new()?;
            let request_a = typed_process_request(ProcessPurpose::ProviderWorker)?;
            let request_b = if identical_request {
                typed_process_request(ProcessPurpose::ProviderWorker)?
            } else {
                typed_process_request(ProcessPurpose::ProviderWorker)?
                    .with_argument("different-provider-request")?
            };
            let mut store = open_store(home.boundary()?)?;
            let (key_a, binding_a) =
                append_typed_process_pending(&mut store, "substitution-a", &request_a, 1)?;
            let (key_b, binding_b) =
                append_typed_process_pending(&mut store, "substitution-b", &request_b, 3)?;
            let prepared_a = store.prepare_exact_process_claim(&binding_a, &request_a)?;
            let prepared_b = store.prepare_exact_process_claim(&binding_b, &request_b)?;
            let claimed_a = store
                .claim_prepared_process(prepared_a, &timestamp(5))
                .map_err(|_| "first substitution claim failed")?;
            let claimed_b = store
                .claim_prepared_process(prepared_b, &timestamp(6))
                .map_err(|_| "second substitution claim failed")?;
            assert_eq!(claimed_a.claim_attempt(), claimed_b.claim_attempt());
            let claimed_b_attempt = claimed_b.claim_attempt();
            let mapped_a = crate::process_receipt::map_authorized_process_outcome(
                orchestrator_process::AuthorizedProcessOutcome::NotStarted(
                    orchestrator_process::ProcessNotStartedReceipt {
                        reason: orchestrator_process::ProcessNotStartedReason::SpawnFailed,
                        launcher_spawned: false,
                        launcher_identity: None,
                        elapsed: std::time::Duration::ZERO,
                        cancellation_observed: false,
                        deadline_observed: false,
                    },
                ),
                &request_a,
                None,
                &claimed_a,
            )?;
            assert!(mapped_a.bind(claimed_b).is_err());
            let unchanged_b = load_effect(store.connection()?, &key_b)?
                .ok_or("missing second substitution effect")?;
            assert_eq!(unchanged_b.state(), OutboxState::Executing);
            assert_eq!(unchanged_b.attempts(), claimed_b_attempt);
            let observations_b: i64 = store.connection()?.query_row(
                "SELECT count(*) FROM outbox_attempt_observation WHERE idempotency_key = ?1",
                [&key_b],
                |row| row.get(0),
            )?;
            assert_eq!(observations_b, 0);

            let terminal_a = claimed_a.into_terminal();
            store.resolve_claimed_process(
                &terminal_a,
                EffectResolution::NotStarted(EffectEvidence::process_not_started(
                    ProcessNotStartedEvidenceReason::SpawnFailed,
                )),
                &timestamp(7),
            )?;
            let resolved_a = load_effect(store.connection()?, &key_a)?
                .ok_or("missing first substitution effect")?;
            assert_eq!(resolved_a.state(), OutboxState::Failed);
            store.close()?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn prepared_process_claim_refuses_an_older_executing_attempt()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_store(home.boundary()?)?;
        let (_key, binding) =
            append_typed_process_pending(&mut store, "prepared-old-attempt", &request, 1)?;
        let prepared = store.prepare_exact_process_claim(&binding, &request)?;
        let claimed = store
            .claim_prepared_process(prepared, &timestamp(3))
            .map_err(|_| "prepared process claim failed")?;
        assert!(matches!(
            store.prepare_exact_process_claim(&binding, &request),
            Err(RuntimeStoreError::ExpectedEffectNotClaimable)
        ));
        let claimed = claimed.into_terminal();
        store.resolve_claimed_process(
            &claimed,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(4),
        )?;
        store.close()?;
        Ok(())
    }

    #[test]
    fn exact_claim_remains_compatible_with_recovery_and_typed_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (key, _) = append_and_acknowledge(
            &mut store,
            "transition-exact-recovery",
            effect_in_slot(
                OutboxEffectKind::ProviderProcess,
                "recoverable-reasoner",
                1,
                serde_json::json!({"provider":"fixture"}),
            )?,
        )?;
        let first = store.claim_exact(&key, &timestamp(3))?;
        let identity = ProcessExecutionIdentity::new(
            first.claim_attempt(),
            301,
            301,
            "boot-exact:301",
            timestamp(3),
        )?;
        store.record_execution_identity(first.idempotency_key(), &identity)?;
        store.resolve_effect(
            first.idempotency_key(),
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::ReleaseDelivery,
            )),
            &timestamp(4),
        )?;
        store.close()?;

        let mut reopened = open_store(Arc::clone(&boundary))?;
        let recovery = reopened.recovery_effects(10)?;
        assert_eq!(recovery[0].state(), OutboxState::Uncertain);
        assert_eq!(recovery[0].execution_identity(), Some(&identity));
        reopened.resolve_effect(
            &key,
            EffectResolution::ProcessFailed(EffectEvidence::process_failed(
                StartedProcessFailureEvidence::Stalled,
            )?),
            &timestamp(5),
        )?;
        reopened.resolve_effect(
            &key,
            EffectResolution::RetryFailed(policy_authorization(
                &key,
                1,
                "exact-retry-policy-v1",
                "exact-retry-decision-1",
                timestamp(6),
            )?),
            &timestamp(6),
        )?;
        reopened.close()?;

        let mut retry_store = open_store(boundary)?;
        let durable_retry =
            load_effect(retry_store.connection()?, &key)?.ok_or("missing reopened exact retry")?;
        assert_eq!(durable_retry.state(), OutboxState::Pending);
        assert_eq!(durable_retry.attempts(), 1);
        let retry = retry_store.claim_exact(&key, &timestamp(7))?;
        assert_eq!(retry.claim_attempt(), 2);
        retry_store.resolve_effect(
            retry.idempotency_key(),
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::Deadline,
            )),
            &timestamp(8),
        )?;
        assert_eq!(
            retry_store
                .attempt_history(&key, 10)?
                .iter()
                .map(EffectObservation::state)
                .collect::<Vec<_>>(),
            vec![
                OutboxState::Uncertain,
                OutboxState::Failed,
                OutboxState::Pending,
                OutboxState::Failed,
            ]
        );
        retry_store.close()?;
        Ok(())
    }

    #[test]
    fn claim_and_reconcile_survive_reopen() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let effect = effect(
            OutboxEffectKind::ProviderProcess,
            1,
            serde_json::json!({"executable_id":"claude"}),
        )?;
        let key = effect.idempotency_key().to_owned();
        let transition = intent("transition-1", 1)?.with_outbox(effect)?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(&transition)?;
        assert!(store.claim_next(&timestamp(2))?.is_none());
        let acknowledgement = store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;
        assert_eq!(
            acknowledgement
                .as_ref()
                .map(CommandAcknowledgement::journal_sequence),
            Some(commit.sequence())
        );
        let claimed = store.claim_next(&timestamp(3))?.ok_or("missing claim")?;
        assert_eq!(claimed.state(), OutboxState::Executing);
        assert_eq!(claimed.attempts(), 1);
        let identity = ProcessExecutionIdentity::new(1, 42, 42, "boot-42:100", timestamp(3))?;
        store.record_execution_identity(&key, &identity)?;
        store.close()?;

        let mut reopened = open_store(boundary)?;
        let recovery = reopened.recovery_effects(10)?;
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].execution_identity(), Some(&identity));
        let uncertain = reopened.resolve_effect(
            &key,
            EffectResolution::Uncertain(evidence(EffectEvidenceCode::ProcessStateUnobservable)?),
            &timestamp(4),
        )?;
        assert_eq!(uncertain.state(), OutboxState::Uncertain);
        let succeeded = reopened.resolve_effect(
            &key,
            EffectResolution::Succeeded(evidence(EffectEvidenceCode::ExitObservedSuccess)?),
            &timestamp(5),
        )?;
        assert_eq!(succeeded.state(), OutboxState::Succeeded);
        assert!(reopened.recovery_effects(10)?.is_empty());
        assert_eq!(reopened.attempt_history(&key, 10)?.len(), 2);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn committed_wal_recovers_after_process_exit_without_destructors()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_store::tests::runtime_store_crash_helper")
            .arg("--nocapture")
            .env(CRASH_HELPER_HOME, &home.path)
            .output()?;
        assert_eq!(output.status.code(), Some(93));

        let store = open_store(home.boundary()?)?;
        let pending = store.pending_effects(10)?;
        assert_eq!(pending.len(), 1);
        let expected = effect(
            OutboxEffectKind::PluginProcess,
            1,
            serde_json::json!({"workspace":"mission-1"}),
        )?;
        assert_eq!(pending[0].idempotency_key(), expected.idempotency_key());
        store.close()?;
        Ok(())
    }

    #[test]
    fn runtime_store_crash_helper() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os(CRASH_HELPER_HOME) else {
            return Ok(());
        };
        let path = fs::canonicalize(path)?;
        let boundary = Arc::new(ProductionBoundary::from_canonical_root(&path)?);
        let transition = intent("crash-transition-1", 1)?.with_outbox(effect(
            OutboxEffectKind::PluginProcess,
            1,
            serde_json::json!({"workspace":"mission-1"}),
        )?)?;
        let mut store = open_store(boundary)?;
        store.append(&transition)?;
        std::process::exit(93);
    }

    #[test]
    fn sealed_private_crash_wal_wrong_opener_is_byte_inert_and_correct_opener_recovers()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_store::tests::boundary_seal_wal_crash_helper")
            .arg("--nocapture")
            .env(BOUNDARY_WAL_CRASH_HELPER_HOME, &home.path)
            .output()?;
        assert_eq!(output.status.code(), Some(100));

        let before = runtime_file_snapshot(&home)?;
        assert_eq!(before.compatibility_boundary_seal, None);
        assert_eq!(before.private_boundary_seal, Some(Vec::new()));
        assert_eq!(before.legacy_boundary_seal, None);
        let database_bytes = before.database.as_deref().ok_or("missing crash database")?;
        let wal_bytes = before.wal.as_deref().ok_or("missing committed crash WAL")?;
        assert!(!bytes_contain(database_bytes, b"boundary-wal-row"));
        assert!(bytes_contain(wal_bytes, b"boundary-wal-row"));

        let boundary = home.boundary()?;
        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::BoundaryKindMismatch)
        ));
        assert_eq!(runtime_file_snapshot(&home)?, before);

        let recovered = open_private_store(boundary)?;
        let retained: (i64, String, String) = recovered.connection()?.query_row(
            "SELECT runtime_schema.version, runtime_schema.boundary_kind, journal.transition_id
             FROM runtime_schema JOIN journal ON journal.sequence = 1
             WHERE runtime_schema.singleton = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        assert_eq!(
            retained,
            (
                DATABASE_SCHEMA_VERSION,
                StoreBoundaryKind::PrivateProcessLedger.as_str().to_owned(),
                "boundary-wal-row".to_owned()
            )
        );
        recovered.close()?;
        Ok(())
    }

    #[test]
    fn boundary_seal_wal_crash_helper() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os(BOUNDARY_WAL_CRASH_HELPER_HOME) else {
            return Ok(());
        };
        let path = fs::canonicalize(path)?;
        let boundary = Arc::new(ProductionBoundary::from_canonical_root(&path)?);
        let mut store = open_private_store(boundary)?;
        store.append(&private_intent("boundary-wal-row", 1)?)?;
        std::process::exit(100);
    }

    #[test]
    fn all_projection_receipts_gate_acknowledgement_and_effect_release()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let transition = intent("transition-1", 1)?
            .with_required_projection(CompatibilityProjection::EventLog)
            .with_outbox(effect(
                OutboxEffectKind::GitCommand,
                1,
                serde_json::json!({"logical_attempt":1}),
            )?)?;
        let commit = store.append(&transition)?;
        assert!(store.claim_next(&timestamp(2))?.is_none());
        let recipe = commit
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?;
        let event_receipt = ProjectionReceipt::event_log(
            commit.sequence(),
            event_jsonl(recipe.public_sequence(), recipe.event_id(), 1)?,
            timestamp(2),
        )?;
        assert!(store.record_projection(&event_receipt)?.is_none());
        assert!(store.claim_next(&timestamp(3))?.is_none());
        assert!(store.record_projection(&event_receipt)?.is_none());
        let acknowledgement = store
            .record_projection(&ProjectionReceipt::compatibility(
                commit.sequence(),
                CompatibilityProjection::Checkpoint,
                timestamp(3),
            )?)?
            .ok_or("missing command acknowledgement")?;
        assert_eq!(acknowledgement.journal_sequence(), commit.sequence());
        assert!(store.claim_next(&timestamp(4))?.is_some());
        store.close()?;
        open_store(home.boundary()?)?.close()?;
        Ok(())
    }

    #[test]
    fn later_unrelated_append_advances_every_existing_projection_cursor()
    -> Result<(), Box<dyn std::error::Error>> {
        for projection in [
            CompatibilityProjection::Checkpoint,
            CompatibilityProjection::EventLog,
            CompatibilityProjection::Workspace,
            CompatibilityProjection::Sidecars,
            CompatibilityProjection::Metrics,
        ]
        .into_iter()
        {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_store(Arc::clone(&boundary))?;
            let first = store.append(&intent_for_projection("transition-1", 1, projection)?)?;
            store.record_projection(&projection_receipt(&first, projection, 1)?)?;
            let unrelated = if projection == CompatibilityProjection::Workspace {
                CompatibilityProjection::Checkpoint
            } else {
                CompatibilityProjection::Workspace
            };
            let second = store.append(&intent_for_projection("transition-2", 2, unrelated)?)?;
            let cursor: (i64, Option<i64>, Option<String>) = store.connection()?.query_row(
                "SELECT journal_sequence, public_sequence, event_sha256
                 FROM projection_cursor WHERE projection = ?1",
                [projection.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
            assert_eq!(cursor.0, second.sequence(), "projection {projection:?}");
            if projection == CompatibilityProjection::EventLog {
                assert_eq!(cursor.1, Some(1));
                assert!(cursor.2.as_deref().is_some_and(valid_checksum));
            } else {
                assert_eq!(cursor.1, None);
                assert_eq!(cursor.2, None);
            }
            store.close()?;
            let reopened = open_store(boundary)?;
            let reopened_sequence: i64 = reopened.connection()?.query_row(
                "SELECT journal_sequence FROM projection_cursor WHERE projection = ?1",
                [projection.as_str()],
                |row| row.get(0),
            )?;
            assert_eq!(reopened_sequence, second.sequence());
            reopened.close()?;
        }
        Ok(())
    }

    #[test]
    fn event_mapping_binds_exact_canonical_bytes_via_sealed_recipe()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let first = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        let second = store.append(&intent_for_projection(
            "transition-2",
            2,
            CompatibilityProjection::EventLog,
        )?)?;
        let third = store.append(&intent_for_projection(
            "transition-3",
            3,
            CompatibilityProjection::EventLog,
        )?)?;
        let first_recipe = first
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let second_recipe = second
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let third_recipe = third
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        assert_eq!(first_recipe.public_sequence(), 1);
        assert_eq!(second_recipe.public_sequence(), 2);
        assert_eq!(third_recipe.public_sequence(), 3);

        // Wrong sequence: bytes carry another row's sequence.
        assert!(matches!(
            store.record_projection(&ProjectionReceipt::event_log(
                first.sequence(),
                event_jsonl(second_recipe.public_sequence(), first_recipe.event_id(), 1)?,
                timestamp(4),
            )?),
            Err(RuntimeStoreError::ProjectionConflict)
        ));
        // Wrong identity: correct sequence, forged event ID.
        assert!(matches!(
            store.record_projection(&ProjectionReceipt::event_log(
                first.sequence(),
                event_jsonl(first_recipe.public_sequence(), "evt_forged_identity", 1)?,
                timestamp(4),
            )?),
            Err(RuntimeStoreError::ProjectionConflict)
        ));

        let first_bytes = event_jsonl(first_recipe.public_sequence(), first_recipe.event_id(), 1)?;
        let first_receipt =
            ProjectionReceipt::event_log(first.sequence(), first_bytes.clone(), timestamp(4))?;
        assert!(!format!("{first_receipt:?}").contains("mission-1"));

        // Recording out of journal order now succeeds: identity was already
        // sealed at append time, so a receipt no longer needs journal order
        // to resolve an otherwise-ambiguous "next" sequence.
        store.record_projection(&ProjectionReceipt::event_log(
            third.sequence(),
            event_jsonl(third_recipe.public_sequence(), third_recipe.event_id(), 3)?,
            timestamp(4),
        )?)?;
        store.record_projection(&first_receipt)?;
        store.record_projection(&ProjectionReceipt::event_log(
            second.sequence(),
            event_jsonl(second_recipe.public_sequence(), second_recipe.event_id(), 2)?,
            timestamp(4),
        )?)?;

        let stored: (Vec<u8>, String) = store.connection()?.query_row(
            "SELECT event_jsonl, event_sha256 FROM projection_receipt
             WHERE journal_sequence = ?1 AND projection = 'event_log'",
            [first.sequence()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        assert_eq!(stored.0, first_bytes);
        assert_eq!(stored.1, hex_digest(Sha256::digest(&stored.0).as_slice()));
        store.close()?;
        open_store(boundary)?.close()?;
        Ok(())
    }

    #[test]
    fn event_mapping_rejects_wrong_sequence_wrong_identity_and_noncanonical_bytes()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let first = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        let second = store.append(&intent_for_projection(
            "transition-2",
            2,
            CompatibilityProjection::EventLog,
        )?)?;
        let first_recipe = first
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let second_recipe = second
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        store.record_projection(&ProjectionReceipt::event_log(
            first.sequence(),
            event_jsonl(first_recipe.public_sequence(), first_recipe.event_id(), 1)?,
            timestamp(3),
        )?)?;
        // Wrong sequence for second's own sealed recipe (reuses first's).
        assert!(matches!(
            store.record_projection(&ProjectionReceipt::event_log(
                second.sequence(),
                event_jsonl(first_recipe.public_sequence(), second_recipe.event_id(), 2)?,
                timestamp(3),
            )?),
            Err(RuntimeStoreError::ProjectionConflict)
        ));
        // Wrong identity for second's own sealed recipe (reuses first's).
        assert!(matches!(
            store.record_projection(&ProjectionReceipt::event_log(
                second.sequence(),
                event_jsonl(second_recipe.public_sequence(), first_recipe.event_id(), 2)?,
                timestamp(3),
            )?),
            Err(RuntimeStoreError::ProjectionConflict)
        ));
        // Correct identity, wrong committed timestamp.
        assert!(matches!(
            store.record_projection(&ProjectionReceipt::event_log(
                second.sequence(),
                event_jsonl(second_recipe.public_sequence(), second_recipe.event_id(), 1)?,
                timestamp(3),
            )?),
            Err(RuntimeStoreError::ProjectionConflict)
        ));
        let mut noncanonical =
            event_jsonl(second_recipe.public_sequence(), second_recipe.event_id(), 2)?;
        noncanonical.pop();
        assert!(
            ProjectionReceipt::event_log(second.sequence(), noncanonical, timestamp(3)).is_err()
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn event_sequences_are_mission_scoped_across_interleaving_and_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let mission_a_1 = store.append(&intent_for_mission_projection(
            "mission-a-transition-1",
            "mission-a",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        let mission_b_1 = store.append(&intent_for_mission_projection(
            "mission-b-transition-1",
            "mission-b",
            2,
            CompatibilityProjection::EventLog,
        )?)?;
        let mission_a_2 = store.append(&intent_for_mission_projection(
            "mission-a-transition-2",
            "mission-a",
            3,
            CompatibilityProjection::EventLog,
        )?)?;
        let mission_b_2 = store.append(&intent_for_mission_projection(
            "mission-b-transition-2",
            "mission-b",
            4,
            CompatibilityProjection::EventLog,
        )?)?;

        let mission_a_1_recipe = mission_a_1
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let mission_b_1_recipe = mission_b_1
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let mission_a_2_recipe = mission_a_2
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let mission_b_2_recipe = mission_b_2
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        // Each mission's own recipe starts its public sequence back at 1:
        // interleaved appends do not share one global event counter.
        assert_eq!(mission_a_1_recipe.public_sequence(), 1);
        assert_eq!(mission_b_1_recipe.public_sequence(), 1);
        assert_eq!(mission_a_2_recipe.public_sequence(), 2);
        assert_eq!(mission_b_2_recipe.public_sequence(), 2);

        store.record_projection(&ProjectionReceipt::event_log(
            mission_a_1.sequence(),
            event_jsonl_for(
                "mission-a",
                mission_a_1_recipe.public_sequence(),
                mission_a_1_recipe.event_id(),
                1,
            )?,
            timestamp(5),
        )?)?;
        store.record_projection(&ProjectionReceipt::event_log(
            mission_b_1.sequence(),
            event_jsonl_for(
                "mission-b",
                mission_b_1_recipe.public_sequence(),
                mission_b_1_recipe.event_id(),
                2,
            )?,
            timestamp(5),
        )?)?;
        // Reusing mission-a's already-receipted sequence/identity against
        // mission-a's second row fails: that row's own sealed recipe demands
        // sequence 2, not 1.
        assert!(matches!(
            store.record_projection(&ProjectionReceipt::event_log(
                mission_a_2.sequence(),
                event_jsonl_for(
                    "mission-a",
                    mission_a_1_recipe.public_sequence(),
                    mission_a_1_recipe.event_id(),
                    3,
                )?,
                timestamp(5),
            )?),
            Err(RuntimeStoreError::ProjectionConflict)
        ));
        store.record_projection(&ProjectionReceipt::event_log(
            mission_a_2.sequence(),
            event_jsonl_for(
                "mission-a",
                mission_a_2_recipe.public_sequence(),
                mission_a_2_recipe.event_id(),
                3,
            )?,
            timestamp(5),
        )?)?;
        store.record_projection(&ProjectionReceipt::event_log(
            mission_b_2.sequence(),
            event_jsonl_for(
                "mission-b",
                mission_b_2_recipe.public_sequence(),
                mission_b_2_recipe.event_id(),
                4,
            )?,
            timestamp(5),
        )?)?;
        let sequence_ones: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM projection_receipt
             WHERE projection = 'event_log' AND public_sequence = 1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(sequence_ones, 2);
        store.close()?;

        let mut reopened = open_store(boundary)?;
        let mission_a_3 = reopened.append(&intent_for_mission_projection(
            "mission-a-transition-3",
            "mission-a",
            6,
            CompatibilityProjection::EventLog,
        )?)?;
        let mission_a_3_recipe = mission_a_3
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        assert_eq!(mission_a_3_recipe.public_sequence(), 3);
        reopened.record_projection(&ProjectionReceipt::event_log(
            mission_a_3.sequence(),
            event_jsonl_for(
                "mission-a",
                mission_a_3_recipe.public_sequence(),
                mission_a_3_recipe.event_id(),
                6,
            )?,
            timestamp(7),
        )?)?;
        reopened.close()?;
        open_store(home.boundary()?)?.close()?;
        Ok(())
    }

    #[test]
    fn event_mapping_rejects_reserved_data_and_forward_field_substitution()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(
            &JournalIntent::new(
                "full-envelope-transition",
                Some(MissionId::new("mission-full")?),
                "phase.started",
                serde_json::json!({
                    "phase_id":"phase-full",
                    "worker_id":"worker-full",
                    "data":{"answer":42},
                    "future_envelope":{"mode":"kept"}
                }),
                timestamp(1),
            )?
            .with_required_projection(CompatibilityProjection::EventLog),
        )?;
        let recipe = commit
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let valid = EventRecord {
            id: recipe.event_id().to_owned(),
            event_type: "phase.started".to_owned(),
            timestamp: timestamp(1),
            sequence: recipe.public_sequence(),
            mission_id: "mission-full".to_owned(),
            phase_id: Some("phase-full".to_owned()),
            worker_id: Some("worker-full".to_owned()),
            data: Some(EventJsonMap::from_iter([(
                "answer".to_owned(),
                Value::from(42),
            )])),
            extra: EventJsonMap::from_iter([(
                "future_envelope".to_owned(),
                serde_json::json!({"mode":"kept"}),
            )]),
        };
        let mut substituted = Vec::new();
        let mut wrong_phase = valid.clone();
        wrong_phase.phase_id = Some("phase-other".to_owned());
        substituted.push(wrong_phase);
        let mut wrong_worker = valid.clone();
        wrong_worker.worker_id = Some("worker-other".to_owned());
        substituted.push(wrong_worker);
        let mut wrong_data = valid.clone();
        wrong_data.data = Some(EventJsonMap::from_iter([(
            "answer".to_owned(),
            Value::from(43),
        )]));
        substituted.push(wrong_data);
        let mut wrong_forward = valid.clone();
        wrong_forward.extra.insert(
            "future_envelope".to_owned(),
            serde_json::json!({"mode":"substituted"}),
        );
        substituted.push(wrong_forward);

        for record in substituted {
            assert!(matches!(
                store.record_projection(&ProjectionReceipt::event_log(
                    commit.sequence(),
                    event_record_jsonl(&record)?,
                    timestamp(2),
                )?),
                Err(RuntimeStoreError::ProjectionConflict)
            ));
        }
        store.record_projection(&ProjectionReceipt::event_log(
            commit.sequence(),
            event_record_jsonl(&valid)?,
            timestamp(2),
        )?)?;
        store.close()?;
        open_store(boundary)?.close()?;
        Ok(())
    }

    #[test]
    fn empty_event_data_normalizes_to_go_omitempty_before_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(
            &JournalIntent::new(
                "empty-data-transition",
                Some(MissionId::new("mission-empty-data")?),
                "phase.started",
                serde_json::json!({"phase_id":"phase-empty","data":{}}),
                timestamp(1),
            )?
            .with_required_projection(CompatibilityProjection::EventLog),
        )?;
        let recipe = commit
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let event = EventRecord {
            id: recipe.event_id().to_owned(),
            event_type: "phase.started".to_owned(),
            timestamp: timestamp(1),
            sequence: recipe.public_sequence(),
            mission_id: "mission-empty-data".to_owned(),
            phase_id: Some("phase-empty".to_owned()),
            worker_id: None,
            data: None,
            extra: EventJsonMap::default(),
        };
        store.record_projection(&ProjectionReceipt::event_log(
            commit.sequence(),
            event_record_jsonl(&event)?,
            timestamp(2),
        )?)?;
        store.close()?;
        open_store(boundary)?.close()?;
        Ok(())
    }

    #[test]
    fn reserved_event_envelope_extras_fail_before_any_row() -> Result<(), Box<dyn std::error::Error>>
    {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        for (index, (field, value)) in [
            ("id", Value::from("evt_caller_selected")),
            ("type", Value::from("mission.completed")),
            ("timestamp", Value::from(timestamp(9))),
            ("sequence", Value::from(9)),
            ("mission_id", Value::from("mission-other")),
        ]
        .into_iter()
        .enumerate()
        {
            let mut payload =
                serde_json::Map::from_iter([("phase_id".to_owned(), Value::from("phase-1"))]);
            payload.insert(field.to_owned(), value);
            let invalid = JournalIntent::new(
                format!("reserved-envelope-{index}"),
                Some(MissionId::new("mission-reserved")?),
                "phase.started",
                Value::Object(payload),
                timestamp(1),
            )?
            .with_required_projection(CompatibilityProjection::EventLog);
            assert!(matches!(
                store.append(&invalid),
                Err(RuntimeStoreError::InvalidIntent(_))
            ));
            let counts: (i64, i64) = store.connection()?.query_row(
                "SELECT (SELECT count(*) FROM journal),
                        (SELECT count(*) FROM projection_requirement)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(counts, (0, 0));
        }
        store.close()?;
        Ok(())
    }

    #[test]
    fn reserved_extra_key_is_rejected_by_with_extra() -> Result<(), Box<dyn std::error::Error>> {
        let attempt = intent("transition-1", 1)?.with_extra(BTreeMap::from([(
            "__event_projection_recipe".to_owned(),
            serde_json::json!({"version": 1, "event_id": "evt_forged00000000", "public_sequence": 1}),
        )]));
        assert!(matches!(attempt, Err(RuntimeStoreError::InvalidIntent(_))));
        // An unrelated key is still accepted, proving the rejection is
        // specific to the reserved name and not a blanket ban on extras.
        intent("transition-2", 1)?.with_extra(BTreeMap::from([(
            "caller_context".to_owned(),
            Value::from("kept"),
        )]))?;
        Ok(())
    }

    #[test]
    fn caller_extras_survive_sealed_event_recipe_injection()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let transition =
            intent_for_projection("transition-1", 1, CompatibilityProjection::EventLog)?
                .with_extra(BTreeMap::from([(
                    "caller_context".to_owned(),
                    Value::from("kept"),
                )]))?;
        let commit = store.append(&transition)?;
        let recipe = commit
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?;
        let stored_extra_json: String = store.connection()?.query_row(
            "SELECT extra_json FROM journal WHERE sequence = ?1",
            [commit.sequence()],
            |row| row.get(0),
        )?;
        let extra: Value = serde_json::from_str(&stored_extra_json)?;
        assert_eq!(extra["caller_context"], Value::from("kept"));
        assert_eq!(
            extra["__event_projection_recipe"]["event_id"],
            Value::from(recipe.event_id())
        );
        assert_eq!(
            extra["__event_projection_recipe"]["public_sequence"],
            Value::from(recipe.public_sequence())
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn event_projection_recipe_allocation_is_idempotent_across_exact_retries()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let first = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        let retry = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        assert!(!first.duplicate());
        assert!(retry.duplicate());
        assert_eq!(first.checksum(), retry.checksum());
        let first_recipe = first
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?;
        let retry_recipe = retry
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?;
        assert_eq!(first_recipe, retry_recipe);

        // A genuinely new transition afterward continues the mission
        // frontier from 2: the exact retry above must not have double
        // allocated a sequence.
        let second = store.append(&intent_for_projection(
            "transition-2",
            2,
            CompatibilityProjection::EventLog,
        )?)?;
        let second_recipe = second
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?;
        assert_eq!(second_recipe.public_sequence(), 2);
        store.close()?;
        Ok(())
    }

    #[test]
    fn projection_recovery_snapshot_reports_recipes_receipts_and_frontier_after_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        const SECRET_TRANSITION_ID: &str = "transition-secret-sentinel-never-log";
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let mission_id =
            MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        let first = store.append(&intent_for_projection(
            SECRET_TRANSITION_ID,
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        let first_recipe = first
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        let first_event_bytes =
            event_jsonl(first_recipe.public_sequence(), first_recipe.event_id(), 1)?;
        let first_event_sha256 = hex_digest(Sha256::digest(&first_event_bytes).as_slice());
        store.record_projection(&ProjectionReceipt::event_log(
            first.sequence(),
            first_event_bytes,
            timestamp(2),
        )?)?;
        let second = store.append(&intent_for_projection(
            "transition-2",
            3,
            CompatibilityProjection::EventLog,
        )?)?;
        let second_recipe = second
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        // `second` stays pending (no receipt) across the reopen boundary.
        store.close()?;

        let mut reopened = open_store(boundary)?;
        let bounds = ProjectionRecoveryBounds::new(100, 1024 * 1024)?;
        let snapshot = reopened.projection_recovery_snapshot(&mission_id, &bounds)?;
        assert_eq!(snapshot.mission_id(), &mission_id);
        assert_eq!(snapshot.records().len(), 2);

        let first_record = &snapshot.records()[0];
        assert_eq!(first_record.journal_sequence(), first.sequence());
        assert_eq!(first_record.transition_id(), SECRET_TRANSITION_ID);
        let first_debug = format!("{first_record:?}");
        assert!(!first_debug.contains(SECRET_TRANSITION_ID));
        assert!(first_debug.contains("transition_id_bytes"));
        assert_eq!(first_record.transition_kind(), "phase.started");
        assert_eq!(
            first_record.payload_json(),
            canonical_json(&serde_json::json!({"phase_id":"phase-1","step":1}), "test")?
        );
        assert_eq!(first_record.committed_at_utc(), timestamp(1));
        assert_eq!(
            parse_sealed_recipe(first_record.extra_json()),
            Some(first_recipe.clone())
        );
        assert_eq!(
            first_record.required_projections().to_vec(),
            vec![CompatibilityProjection::EventLog]
        );
        assert_eq!(first_record.event_projection_recipe(), Some(&first_recipe));
        assert!(first_record.missing_projections().is_empty());
        assert_eq!(first_record.present_receipts().len(), 1);
        assert_eq!(
            first_record.present_receipts()[0].projection(),
            CompatibilityProjection::EventLog
        );
        assert_eq!(
            first_record.present_receipts()[0].public_sequence(),
            Some(first_recipe.public_sequence())
        );
        assert_eq!(
            first_record.present_receipts()[0].event_id(),
            Some(first_recipe.event_id())
        );
        assert_eq!(
            first_record.present_receipts()[0].event_sha256(),
            Some(first_event_sha256.as_str())
        );
        assert_eq!(
            first_record.present_receipts()[0].applied_at_utc(),
            timestamp(2)
        );
        assert!(first_record.acknowledgement().is_some());

        let second_record = &snapshot.records()[1];
        assert_eq!(second_record.journal_sequence(), second.sequence());
        assert_eq!(second_record.transition_id(), "transition-2");
        assert_eq!(
            second_record.event_projection_recipe(),
            Some(&second_recipe)
        );
        assert_eq!(
            second_record.missing_projections().to_vec(),
            vec![CompatibilityProjection::EventLog]
        );
        assert!(second_record.present_receipts().is_empty());
        assert!(second_record.acknowledgement().is_none());

        assert_eq!(
            snapshot.frontier().next_public_sequence(),
            second_recipe.public_sequence() + 1
        );
        assert_eq!(
            snapshot.frontier().last_bound_identity(),
            Some(&second_recipe)
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn projection_recovery_snapshot_enforces_record_and_byte_bounds()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let mission_id =
            MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        store.append(&intent("transition-1", 1)?)?;
        store.append(&intent("transition-2", 2)?)?;
        store.append(&intent("transition-3", 3)?)?;

        let tight_count_bounds = ProjectionRecoveryBounds::new(2, 1024 * 1024)?;
        assert!(matches!(
            store.projection_recovery_snapshot(&mission_id, &tight_count_bounds),
            Err(RuntimeStoreError::RecoveryBoundsExceeded)
        ));

        let tight_byte_bounds = ProjectionRecoveryBounds::new(100, 8)?;
        assert!(matches!(
            store.projection_recovery_snapshot(&mission_id, &tight_byte_bounds),
            Err(RuntimeStoreError::RecoveryBoundsExceeded)
        ));

        let generous_bounds = ProjectionRecoveryBounds::new(100, 1024 * 1024)?;
        let snapshot = store.projection_recovery_snapshot(&mission_id, &generous_bounds)?;
        assert_eq!(snapshot.records().len(), 3);
        store.close()?;
        Ok(())
    }

    #[test]
    fn tampering_with_sealed_event_recipe_bytes_fails_reopen_with_schema_intact()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        let recipe = commit
            .event_projection_recipe()
            .ok_or("missing sealed event recipe")?
            .clone();
        store.close()?;

        // Mutate the recipe bytes without recomputing the row's checksum:
        // the generic checksum chain alone must already fail this closed,
        // proving the recipe truly lives inside checksummed material.
        let raw = Connection::open(home.database())?;
        raw.execute_batch(
            "DROP TRIGGER journal_reject_update;
             DROP TRIGGER journal_reject_delete;",
        )?;
        let tampered_extra = format!(
            r#"{{"__event_projection_recipe":{{"event_id":"{}","public_sequence":9,"version":1}}}}"#,
            recipe.event_id()
        );
        raw.execute(
            "UPDATE journal SET extra_json = ?1 WHERE sequence = ?2",
            params![tampered_extra, commit.sequence()],
        )?;
        drop(raw);

        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn tampered_event_projection_sequence_gap_or_duplicate_is_rejected_by_recipe_validation()
    -> Result<(), Box<dyn std::error::Error>> {
        for forged_sequence in [1_i64, 3_i64] {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_store(Arc::clone(&boundary))?;
            let first = store.append(&intent_for_projection(
                "transition-1",
                1,
                CompatibilityProjection::EventLog,
            )?)?;
            let second = store.append(&intent_for_projection(
                "transition-2",
                2,
                CompatibilityProjection::EventLog,
            )?)?;
            let first_recipe = first
                .event_projection_recipe()
                .ok_or("missing sealed event recipe")?
                .clone();
            assert_eq!(first_recipe.public_sequence(), 1);
            store.close()?;

            let raw = Connection::open(home.database())?;
            raw.execute_batch(
                "DROP TRIGGER journal_reject_update;
                 DROP TRIGGER journal_reject_delete;",
            )?;
            let transition_id: String = raw.query_row(
                "SELECT transition_id FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;
            let mission_id: Option<String> = raw.query_row(
                "SELECT mission_id FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;
            let kind: String = raw.query_row(
                "SELECT transition_kind FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;
            let payload_json: String = raw.query_row(
                "SELECT payload_json FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;
            let committed_at_utc: String = raw.query_row(
                "SELECT committed_at_utc FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;
            let outbox_fingerprint: String = raw.query_row(
                "SELECT outbox_fingerprint FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;
            let projection_fingerprint: String = raw.query_row(
                "SELECT projection_fingerprint FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;
            let previous_checksum: String = raw.query_row(
                "SELECT previous_checksum FROM journal WHERE sequence = ?1",
                [second.sequence()],
                |row| row.get(0),
            )?;

            // Forge second's sealed recipe to collide with (or skip past)
            // first's mission-scoped sequence, recomputing a matching
            // checksum so the generic chain check alone cannot catch it —
            // only per-mission recipe validation can.
            let forged_extra = format!(
                r#"{{"__event_projection_recipe":{{"event_id":"evt_forged_collision000","public_sequence":{forged_sequence},"version":1}}}}"#
            );
            let prepared = PreparedIntent {
                transition_id,
                mission_id,
                kind,
                payload_json,
                committed_at_utc,
                extra_json: forged_extra.clone(),
                outbox_fingerprint,
                projection_fingerprint,
                required_projections: Vec::new(),
                outbox: Vec::new(),
                publications: Vec::new(),
            };
            let forged_checksum = transition_checksum(
                RECORD_SCHEMA_VERSION,
                second.sequence(),
                &previous_checksum,
                &prepared,
            );
            raw.execute(
                "UPDATE journal SET extra_json = ?1, checksum = ?2 WHERE sequence = ?3",
                params![forged_extra, forged_checksum, second.sequence()],
            )?;
            drop(raw);

            assert!(
                matches!(
                    open_store(Arc::clone(&boundary)),
                    Err(RuntimeStoreError::CorruptDatabase)
                ),
                "forged sequence {forged_sequence} was not rejected"
            );
        }
        Ok(())
    }

    #[test]
    fn tampered_event_projection_recipe_unknown_version_is_rejected_by_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        store.close()?;

        forge_journal_extra_json(
            &home,
            commit.sequence(),
            r#"{"__event_projection_recipe":{"event_id":"evt_deadbeefdeadbeef","public_sequence":1,"version":99}}"#,
        )?;

        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn tampered_event_projection_recipe_invalid_event_id_is_rejected_by_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        store.close()?;

        // "evt_" prefixed but containing a byte outside the identifier
        // charset (only ASCII letters, digits, '-' and '_' are allowed).
        forge_journal_extra_json(
            &home,
            commit.sequence(),
            r#"{"__event_projection_recipe":{"event_id":"evt_bad!chars","public_sequence":1,"version":1}}"#,
        )?;

        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn tampered_event_projection_recipe_wrong_json_type_is_rejected_by_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        store.close()?;

        // `public_sequence` forged as a JSON string instead of a number.
        forge_journal_extra_json(
            &home,
            commit.sequence(),
            r#"{"__event_projection_recipe":{"event_id":"evt_deadbeefdeadbeef","public_sequence":"1","version":1}}"#,
        )?;

        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn tampered_event_projection_recipe_missing_key_is_rejected_by_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(&intent_for_projection(
            "transition-1",
            1,
            CompatibilityProjection::EventLog,
        )?)?;
        store.close()?;

        // The reserved recipe key is entirely absent on a schema-v2 row
        // that requires the event_log projection.
        forge_journal_extra_json(&home, commit.sequence(), "{}")?;

        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn tampered_legacy_and_sealed_recipe_event_id_collision_is_rejected_by_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        // Legacy (record_schema_version = 1) receipted event IDs never went
        // through `allocate_event_projection_recipe`'s collision guard, but
        // they occupy the same global event-ID namespace. Builds one
        // legacy row with a receipted ID and one schema-v2 row whose sealed
        // recipe forges the *same* ID, entirely by hand (both rows predate
        // the running store, so `append` can't be used for either). Only
        // inserting legacy receipted IDs into `seen_event_ids` catches this.
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        open_store(Arc::clone(&boundary))?.close()?;

        let connection = Connection::open(home.database())?;
        let legacy_event_id = "evt_legacyduplicateid00";

        let legacy_required = vec![CompatibilityProjection::EventLog];
        let legacy_prepared = PreparedIntent {
            transition_id: "legacy-transition-1".to_owned(),
            mission_id: Some("mission-1".to_owned()),
            kind: "phase.started".to_owned(),
            payload_json: r#"{"phase_id":"phase-1","step":1}"#.to_owned(),
            committed_at_utc: timestamp(1),
            extra_json: "{}".to_owned(),
            outbox_fingerprint: outbox_fingerprint_for_version(LEGACY_RECORD_SCHEMA_VERSION, &[])?,
            projection_fingerprint: projection_fingerprint(&legacy_required),
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        };
        let legacy_checksum = transition_checksum(
            LEGACY_RECORD_SCHEMA_VERSION,
            1,
            GENESIS_CHECKSUM,
            &legacy_prepared,
        );
        connection.execute(
            "INSERT INTO journal (
                sequence, transition_id, mission_id, record_schema_version,
                transition_kind, payload_json, committed_at_utc, extra_json,
                outbox_fingerprint, projection_fingerprint, previous_checksum, checksum
             ) VALUES (1, ?1, ?2, 1, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                legacy_prepared.transition_id,
                legacy_prepared.mission_id,
                legacy_prepared.kind,
                legacy_prepared.payload_json,
                legacy_prepared.committed_at_utc,
                legacy_prepared.extra_json,
                legacy_prepared.outbox_fingerprint,
                legacy_prepared.projection_fingerprint,
                GENESIS_CHECKSUM,
                legacy_checksum,
            ],
        )?;
        connection.execute(
            "INSERT INTO projection_requirement (journal_sequence, projection)
             VALUES (1, 'event_log')",
            [],
        )?;
        connection.execute(
            "INSERT INTO projection_receipt (
                record_schema_version, journal_sequence, projection, mission_id,
                public_sequence, event_id, event_jsonl, event_sha256, applied_at_utc
             ) VALUES (1, 1, 'event_log', 'mission-1', 1, ?1, NULL, NULL, ?2)",
            params![legacy_event_id, timestamp(2)],
        )?;
        connection.execute(
            "INSERT INTO command_ack (journal_sequence, acknowledged_at_utc) VALUES (1, ?1)",
            params![timestamp(2)],
        )?;

        let forged_extra = format!(
            r#"{{"__event_projection_recipe":{{"event_id":"{legacy_event_id}","public_sequence":2,"version":1}}}}"#
        );
        let v2_required = vec![CompatibilityProjection::EventLog];
        let v2_prepared = PreparedIntent {
            transition_id: "transition-2".to_owned(),
            mission_id: Some("mission-1".to_owned()),
            kind: "phase.started".to_owned(),
            payload_json: r#"{"phase_id":"phase-1","step":2}"#.to_owned(),
            committed_at_utc: timestamp(3),
            extra_json: forged_extra.clone(),
            outbox_fingerprint: outbox_fingerprint_for_version(RECORD_SCHEMA_VERSION, &[])?,
            projection_fingerprint: projection_fingerprint(&v2_required),
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        };
        let v2_checksum =
            transition_checksum(RECORD_SCHEMA_VERSION, 2, &legacy_checksum, &v2_prepared);
        connection.execute(
            "INSERT INTO journal (
                sequence, transition_id, mission_id, record_schema_version,
                transition_kind, payload_json, committed_at_utc, extra_json,
                outbox_fingerprint, projection_fingerprint, previous_checksum, checksum
             ) VALUES (2, ?1, ?2, 2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                v2_prepared.transition_id,
                v2_prepared.mission_id,
                v2_prepared.kind,
                v2_prepared.payload_json,
                v2_prepared.committed_at_utc,
                v2_prepared.extra_json,
                v2_prepared.outbox_fingerprint,
                v2_prepared.projection_fingerprint,
                legacy_checksum,
                v2_checksum,
            ],
        )?;
        connection.execute(
            "INSERT INTO projection_requirement (journal_sequence, projection)
             VALUES (2, 'event_log')",
            [],
        )?;
        drop(connection);

        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn projection_recovery_snapshot_allows_exact_byte_bound_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let mission_id =
            MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        store.append(&intent("transition-1", 1)?)?;

        let generous_bounds = ProjectionRecoveryBounds::new(10, 1024 * 1024)?;
        let probe = store.projection_recovery_snapshot(&mission_id, &generous_bounds)?;
        let record = &probe.records()[0];
        let exact_bytes = record.transition_id().len()
            + record.transition_kind().len()
            + record.payload_json().len()
            + record.committed_at_utc().len()
            + record.extra_json().len();

        // Exactly at the byte ceiling passes: the comparison in
        // `build_projection_recovery_snapshot` is strict `>`, not `>=`.
        let exact_bounds = ProjectionRecoveryBounds::new(10, exact_bytes)?;
        let snapshot = store.projection_recovery_snapshot(&mission_id, &exact_bounds)?;
        assert_eq!(snapshot.records().len(), 1);

        // One byte under the row's exact size aborts.
        let one_short_bounds = ProjectionRecoveryBounds::new(10, exact_bytes - 1)?;
        assert!(matches!(
            store.projection_recovery_snapshot(&mission_id, &one_short_bounds),
            Err(RuntimeStoreError::RecoveryBoundsExceeded)
        ));
        store.close()?;
        Ok(())
    }

    #[test]
    fn projection_recovery_snapshot_rejects_single_row_exceeding_byte_bound()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let mission_id =
            MissionId::new("mission-1").map_err(|_| RuntimeStoreError::CorruptDatabase)?;
        store.append(&intent("transition-1", 1)?)?;

        let tiny_bounds = ProjectionRecoveryBounds::new(10, 1)?;
        assert!(matches!(
            store.projection_recovery_snapshot(&mission_id, &tiny_bounds),
            Err(RuntimeStoreError::RecoveryBoundsExceeded)
        ));
        store.close()?;
        Ok(())
    }

    #[test]
    fn projection_recovery_bounds_rejects_zero_records_or_bytes() {
        assert!(matches!(
            ProjectionRecoveryBounds::new(0, 1024),
            Err(RuntimeStoreError::InvalidIntent(_))
        ));
        assert!(matches!(
            ProjectionRecoveryBounds::new(10, 0),
            Err(RuntimeStoreError::InvalidIntent(_))
        ));
    }

    #[test]
    fn event_projection_domain_tag_change_alters_derived_id()
    -> Result<(), Box<dyn std::error::Error>> {
        // Proves the derivation is version-sensitive via the tag-building
        // helper itself, without mutating the real
        // `EVENT_PROJECTION_RECIPE_SCHEMA_VERSION` constant.
        let tag_v1 = event_projection_domain_tag(1);
        let tag_v2 = event_projection_domain_tag(2);
        assert_ne!(tag_v1, tag_v2);
        let id_v1 = derive_event_projection_id_with_domain_tag(&tag_v1, "mission-1", 1)?;
        let id_v2 = derive_event_projection_id_with_domain_tag(&tag_v2, "mission-1", 1)?;
        assert_ne!(id_v1, id_v2);
        Ok(())
    }

    #[test]
    fn operation_slot_prevents_same_kind_effect_identity_collisions()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(EffectOperationSlot::new("not stable").is_err());
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let primary = effect_in_slot(
            OutboxEffectKind::ProviderProcess,
            "primary-reasoner",
            1,
            serde_json::json!({"provider":"claude"}),
        )?;
        let replay = effect_in_slot(
            OutboxEffectKind::ProviderProcess,
            "primary-reasoner",
            1,
            serde_json::json!({"provider":"claude"}),
        )?;
        let reviewer = effect_in_slot(
            OutboxEffectKind::ProviderProcess,
            "parallel-reviewer",
            1,
            serde_json::json!({"provider":"claude"}),
        )?;
        assert_eq!(primary.idempotency_key(), replay.idempotency_key());
        assert_ne!(primary.idempotency_key(), reviewer.idempotency_key());
        let mut store = open_store(Arc::clone(&boundary))?;
        let commit = store.append(
            &intent("transition-1", 1)?
                .with_outbox(primary)?
                .with_outbox(reviewer)?,
        )?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;
        let pending = store.pending_effects(10)?;
        assert_eq!(pending.len(), 2);
        assert_eq!(
            pending
                .iter()
                .map(|effect| effect.operation_slot().as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["parallel-reviewer", "primary-reasoner"])
        );
        store.close()?;
        let reopened = open_store(boundary)?;
        assert_eq!(reopened.pending_effects(10)?.len(), 2);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn conflicting_transition_and_outbox_key_roll_back() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(boundary)?;
        let first = intent("transition-1", 1)?.with_outbox(effect(
            OutboxEffectKind::PluginProcess,
            1,
            serde_json::json!({"event":"one"}),
        )?)?;
        store.append(&first)?;
        let changed = JournalIntent::new(
            "transition-1",
            Some(MissionId::new("mission-1")?),
            "phase.completed",
            serde_json::json!({"changed":true}),
            timestamp(1),
        )?
        .with_required_projection(CompatibilityProjection::Checkpoint);
        assert!(matches!(
            store.append(&changed),
            Err(RuntimeStoreError::TransitionConflict)
        ));
        let second = intent("transition-2", 2)?.with_outbox(effect(
            OutboxEffectKind::PluginProcess,
            1,
            serde_json::json!({"event":"two"}),
        )?)?;
        assert!(matches!(
            store.append(&second),
            Err(RuntimeStoreError::OutboxConflict)
        ));
        let third = store.append(&intent("transition-3", 3)?)?;
        assert_eq!(third.sequence(), 2);
        store.close()?;
        Ok(())
    }

    #[test]
    fn process_identity_is_required_idempotent_and_durable()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (key, claimed) = append_acknowledge_claim(
            &mut store,
            "transition-1",
            effect(
                OutboxEffectKind::ProviderProcess,
                1,
                serde_json::json!({"provider":"claude"}),
            )?,
        )?;
        assert_eq!(claimed.attempts(), 1);
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::Succeeded(evidence(EffectEvidenceCode::ExitObservedSuccess,)?),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        let identity = ProcessExecutionIdentity::new(1, 101, 101, "boot-a:9001", timestamp(4))?;
        assert_eq!(
            store
                .record_execution_identity(&key, &identity)?
                .execution_identity(),
            Some(&identity)
        );
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::Succeeded(EffectEvidence::process_state_unobservable()),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        store.record_execution_identity(&key, &identity)?;
        let conflict = ProcessExecutionIdentity::new(1, 102, 102, "boot-a:9002", timestamp(4))?;
        assert!(matches!(
            store.record_execution_identity(&key, &conflict),
            Err(RuntimeStoreError::ExecutionIdentityConflict)
        ));
        store.close()?;

        let reopened = open_store(boundary)?;
        let recovery = reopened.recovery_effects(10)?;
        assert_eq!(recovery.len(), 1);
        assert_eq!(recovery[0].execution_identity(), Some(&identity));
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn process_specific_resolutions_bind_identity_state_and_retry_history()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (identified_key, _) = append_acknowledge_claim(
            &mut store,
            "transition-process-uncertain-identified",
            effect_in_slot(
                OutboxEffectKind::ProviderProcess,
                "process-uncertain-identified",
                1,
                serde_json::json!({"provider":"claude"}),
            )?,
        )?;
        let first_identity =
            ProcessExecutionIdentity::new(1, 501, 501, "boot-process:1", timestamp(3))?;
        store.record_execution_identity(&identified_key, &first_identity)?;
        let process_failure =
            EffectEvidence::process_failed(StartedProcessFailureEvidence::Stalled)?;
        let process_uncertainty =
            EffectEvidence::process_uncertain(ProcessUncertaintyEvidence::ReleaseDelivery);
        assert!(matches!(
            store.resolve_effect(
                &identified_key,
                EffectResolution::Failed(process_failure.clone()),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        assert!(matches!(
            store.resolve_effect(
                &identified_key,
                EffectResolution::ProcessFailed(EffectEvidence::exit_observed_failure(1)?),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        assert!(matches!(
            store.resolve_effect(
                &identified_key,
                EffectResolution::Uncertain(process_uncertainty.clone()),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        assert!(matches!(
            store.resolve_effect(
                &identified_key,
                EffectResolution::ProcessUncertain(EffectEvidence::exit_status_lost()),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        store.resolve_effect(
            &identified_key,
            EffectResolution::ProcessUncertain(process_uncertainty),
            &timestamp(4),
        )?;

        let (unidentified_key, claimed) = append_acknowledge_claim(
            &mut store,
            "transition-process-uncertain-unidentified",
            effect_in_slot(
                OutboxEffectKind::ProviderProcess,
                "process-uncertain-unidentified",
                1,
                serde_json::json!({"provider":"codex"}),
            )?,
        )?;
        assert!(claimed.execution_identity().is_none());
        assert!(matches!(
            store.resolve_effect(
                &unidentified_key,
                EffectResolution::ProcessFailed(EffectEvidence::process_failed(
                    StartedProcessFailureEvidence::Cancelled,
                )?),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        assert!(matches!(
            store.resolve_effect(
                &unidentified_key,
                EffectResolution::Uncertain(EffectEvidence::exit_status_lost()),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        store.resolve_effect(
            &unidentified_key,
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::UngatedExecution,
            )),
            &timestamp(4),
        )?;
        store.close()?;

        let mut reopened = open_store(Arc::clone(&boundary))?;
        let recovery = reopened.recovery_effects(10)?;
        let identified = recovery
            .iter()
            .find(|effect| effect.idempotency_key() == identified_key)
            .ok_or("missing identified uncertainty")?;
        let unidentified = recovery
            .iter()
            .find(|effect| effect.idempotency_key() == unidentified_key)
            .ok_or("missing unidentified uncertainty")?;
        assert_eq!(identified.execution_identity(), Some(&first_identity));
        assert!(unidentified.execution_identity().is_none());
        assert_eq!(
            reopened.attempt_history(&identified_key, 10)?[0]
                .evidence()
                .process_uncertainty(),
            Some(ProcessUncertaintyEvidence::ReleaseDelivery),
        );
        assert_eq!(
            reopened.attempt_history(&unidentified_key, 10)?[0]
                .evidence()
                .process_uncertainty(),
            Some(ProcessUncertaintyEvidence::UngatedExecution),
        );
        assert!(matches!(
            reopened.resolve_effect(
                &identified_key,
                EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                    ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
                )),
                &timestamp(5),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        assert!(matches!(
            reopened.resolve_effect(
                &unidentified_key,
                EffectResolution::ProcessFailed(EffectEvidence::process_failed(
                    StartedProcessFailureEvidence::InfrastructureFailure,
                )?),
                &timestamp(5),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        reopened.resolve_effect(
            &identified_key,
            EffectResolution::ProcessFailed(process_failure),
            &timestamp(5),
        )?;
        assert!(matches!(
            reopened.resolve_effect(
                &identified_key,
                EffectResolution::ProcessFailed(EffectEvidence::process_failed(
                    StartedProcessFailureEvidence::Deadline,
                )?),
                &timestamp(6),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        reopened.resolve_effect(
            &identified_key,
            EffectResolution::RetryFailed(policy_authorization(
                &identified_key,
                1,
                "retry-policy-v1",
                "retry-process-failure-1",
                timestamp(6),
            )?),
            &timestamp(6),
        )?;
        let second_identified_attempt = reopened
            .claim_next(&timestamp(7))?
            .ok_or("missing process failure retry")?;
        assert_eq!(second_identified_attempt.idempotency_key(), identified_key);
        let second_identity =
            ProcessExecutionIdentity::new(2, 502, 502, "boot-process:2", timestamp(7))?;
        reopened.record_execution_identity(&identified_key, &second_identity)?;
        reopened.resolve_effect(
            &identified_key,
            EffectResolution::ProcessFailed(EffectEvidence::process_failed(
                StartedProcessFailureEvidence::OutputLimit,
            )?),
            &timestamp(8),
        )?;

        reopened.resolve_effect(
            &unidentified_key,
            EffectResolution::ObservedAbsent(EffectEvidence::observed_absent()),
            &timestamp(9),
        )?;
        let second_unidentified_attempt = reopened
            .claim_next(&timestamp(10))?
            .ok_or("missing observed-absent retry")?;
        assert_eq!(
            second_unidentified_attempt.idempotency_key(),
            unidentified_key
        );
        reopened.resolve_effect(
            &unidentified_key,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::Deadline,
            )),
            &timestamp(11),
        )?;
        reopened.close()?;

        let verified = open_store(boundary)?;
        assert_eq!(
            verified
                .attempt_history(&identified_key, 10)?
                .iter()
                .map(EffectObservation::state)
                .collect::<Vec<_>>(),
            vec![
                OutboxState::Uncertain,
                OutboxState::Failed,
                OutboxState::Pending,
                OutboxState::Failed,
            ],
        );
        assert_eq!(
            verified
                .attempt_history(&unidentified_key, 10)?
                .iter()
                .map(EffectObservation::state)
                .collect::<Vec<_>>(),
            vec![
                OutboxState::Uncertain,
                OutboxState::Pending,
                OutboxState::Failed,
            ],
        );
        verified.close()?;
        Ok(())
    }

    #[test]
    fn process_not_started_evidence_is_finite_canonical_and_state_bound()
    -> Result<(), Box<dyn std::error::Error>> {
        let cases = [
            (ProcessNotStartedEvidenceReason::Cancelled, "cancelled"),
            (ProcessNotStartedEvidenceReason::Deadline, "deadline"),
            (ProcessNotStartedEvidenceReason::SpawnFailed, "spawn_failed"),
            (
                ProcessNotStartedEvidenceReason::GateRejected,
                "gate_rejected",
            ),
            (
                ProcessNotStartedEvidenceReason::GateIndeterminate,
                "gate_indeterminate",
            ),
            (
                ProcessNotStartedEvidenceReason::GateProtocol,
                "gate_protocol",
            ),
            (
                ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn,
                "recovered_before_spawn",
            ),
        ];
        for (reason, wire_reason) in cases {
            let evidence = EffectEvidence::process_not_started(reason);
            let canonical = format!(
                r#"{{"code":"process_not_started","process_not_started_reason":"{wire_reason}"}}"#,
            );
            assert_eq!(evidence.canonical_json()?, canonical);
            assert_eq!(EffectEvidence::from_stored_json(&canonical)?, evidence);
            assert_eq!(evidence.process_not_started_reason(), Some(reason));
        }

        assert!(evidence_code_matches_state(
            OutboxState::Failed,
            EffectEvidenceCode::ProcessNotStarted,
        ));
        for state in [
            OutboxState::Pending,
            OutboxState::Executing,
            OutboxState::Succeeded,
            OutboxState::Uncertain,
        ] {
            assert!(!evidence_code_matches_state(
                state,
                EffectEvidenceCode::ProcessNotStarted,
            ));
        }
        for malformed in [
            r#"{"code":"process_not_started"}"#,
            r#"{"code":"process_not_started","process_not_started_reason":"launcher said no"}"#,
            r#"{"code":"process_not_started","process_not_started_reason":"cancelled","detail":"freeform"}"#,
            r#"{"code":"exit_observed_success","exit_code":0,"process_not_started_reason":"cancelled"}"#,
        ] {
            assert!(EffectEvidence::from_stored_json(malformed).is_err());
        }
        Ok(())
    }

    #[test]
    fn started_process_outcome_evidence_is_finite_canonical_and_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let failures = [
            (
                StartedProcessFailureEvidence::Signaled(9),
                r#"{"code":"process_failed","started_process_failure":{"signaled":9}}"#,
            ),
            (
                StartedProcessFailureEvidence::Deadline,
                r#"{"code":"process_failed","started_process_failure":"deadline"}"#,
            ),
            (
                StartedProcessFailureEvidence::Stalled,
                r#"{"code":"process_failed","started_process_failure":"stalled"}"#,
            ),
            (
                StartedProcessFailureEvidence::Cancelled,
                r#"{"code":"process_failed","started_process_failure":"cancelled"}"#,
            ),
            (
                StartedProcessFailureEvidence::OutputLimit,
                r#"{"code":"process_failed","started_process_failure":"output_limit"}"#,
            ),
            (
                StartedProcessFailureEvidence::InfrastructureFailure,
                r#"{"code":"process_failed","started_process_failure":"infrastructure_failure"}"#,
            ),
        ];
        for (failure, canonical) in failures {
            let evidence = EffectEvidence::process_failed(failure)?;
            assert_eq!(evidence.canonical_json()?, canonical);
            assert_eq!(EffectEvidence::from_stored_json(canonical)?, evidence);
            assert_eq!(evidence.started_process_failure(), Some(failure));
            assert!(canonical.len() <= MAX_EVIDENCE_BYTES);
        }

        let not_started_reasons = [
            (ProcessNotStartedEvidenceReason::Cancelled, "cancelled"),
            (ProcessNotStartedEvidenceReason::Deadline, "deadline"),
            (ProcessNotStartedEvidenceReason::SpawnFailed, "spawn_failed"),
            (
                ProcessNotStartedEvidenceReason::GateRejected,
                "gate_rejected",
            ),
            (
                ProcessNotStartedEvidenceReason::GateIndeterminate,
                "gate_indeterminate",
            ),
            (
                ProcessNotStartedEvidenceReason::GateProtocol,
                "gate_protocol",
            ),
            (
                ProcessNotStartedEvidenceReason::RecoveredBeforeSpawn,
                "recovered_before_spawn",
            ),
        ];
        for (reason, wire_reason) in not_started_reasons {
            let uncertainty = ProcessUncertaintyEvidence::CleanupIncomplete(reason);
            let evidence = EffectEvidence::process_uncertain(uncertainty);
            let canonical = format!(
                r#"{{"code":"process_uncertain","process_uncertainty":{{"cleanup_incomplete":"{wire_reason}"}}}}"#,
            );
            assert_eq!(evidence.canonical_json()?, canonical);
            assert_eq!(EffectEvidence::from_stored_json(&canonical)?, evidence);
            assert_eq!(evidence.process_uncertainty(), Some(uncertainty));
            assert!(canonical.len() <= MAX_EVIDENCE_BYTES);
        }
        for (uncertainty, canonical) in [
            (
                ProcessUncertaintyEvidence::ReleaseDelivery,
                r#"{"code":"process_uncertain","process_uncertainty":"release_delivery"}"#,
            ),
            (
                ProcessUncertaintyEvidence::UngatedExecution,
                r#"{"code":"process_uncertain","process_uncertainty":"ungated_execution"}"#,
            ),
            (
                ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
                r#"{"code":"process_uncertain","process_uncertainty":"started_ownership_unresolved"}"#,
            ),
            (
                ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                r#"{"code":"process_uncertain","process_uncertainty":"supervisor_outcome_lost"}"#,
            ),
            (
                ProcessUncertaintyEvidence::CommitIndeterminate,
                r#"{"code":"process_uncertain","process_uncertainty":"commit_indeterminate"}"#,
            ),
        ] {
            let evidence = EffectEvidence::process_uncertain(uncertainty);
            assert_eq!(evidence.canonical_json()?, canonical);
            assert_eq!(EffectEvidence::from_stored_json(canonical)?, evidence);
            assert_eq!(evidence.process_uncertainty(), Some(uncertainty));
            assert!(canonical.len() <= MAX_EVIDENCE_BYTES);
        }

        assert!(
            EffectEvidence::process_failed(StartedProcessFailureEvidence::Signaled(0)).is_err()
        );
        assert!(
            EffectEvidence::process_failed(StartedProcessFailureEvidence::Signaled(-9)).is_err()
        );
        for malformed in [
            r#"{"code":"process_failed"}"#,
            r#"{"code":"process_failed","started_process_failure":{"signaled":0}}"#,
            r#"{"code":"process_failed","started_process_failure":{"signaled":9,"detail":"freeform"}}"#,
            r#"{"code":"process_failed","started_process_failure":"launcher crashed"}"#,
            r#"{"code":"process_failed","process_uncertainty":"release_delivery"}"#,
            r#"{"code":"process_uncertain"}"#,
            r#"{"code":"process_uncertain","process_uncertainty":{"cleanup_incomplete":"launcher crashed"}}"#,
            r#"{"code":"process_uncertain","process_uncertainty":{"cleanup_incomplete":"deadline","detail":"freeform"}}"#,
            r#"{"code":"process_uncertain","process_uncertainty":"release_delivery","detail":"freeform"}"#,
            r#"{"code":"process_uncertain","process_uncertainty":"SupervisorOutcomeLost"}"#,
            r#"{"code":"process_uncertain","started_process_failure":"deadline"}"#,
            r#"{"process_uncertainty":"release_delivery","code":"process_uncertain"}"#,
        ] {
            assert!(EffectEvidence::from_stored_json(malformed).is_err());
        }
        let oversized = format!(
            r#"{{"code":"process_uncertain","detail":"{}"}}"#,
            "x".repeat(MAX_EVIDENCE_BYTES),
        );
        assert!(oversized.len() > MAX_EVIDENCE_BYTES);
        assert!(EffectEvidence::from_stored_json(&oversized).is_err());
        assert!(evidence_code_matches_state(
            OutboxState::Failed,
            EffectEvidenceCode::ProcessFailed,
        ));
        assert!(evidence_code_matches_state(
            OutboxState::Uncertain,
            EffectEvidenceCode::ProcessUncertain,
        ));
        for wrong_state in [
            OutboxState::Pending,
            OutboxState::Executing,
            OutboxState::Succeeded,
            OutboxState::Uncertain,
        ] {
            assert!(!evidence_code_matches_state(
                wrong_state,
                EffectEvidenceCode::ProcessFailed,
            ));
        }
        for wrong_state in [
            OutboxState::Pending,
            OutboxState::Executing,
            OutboxState::Succeeded,
            OutboxState::Failed,
        ] {
            assert!(!evidence_code_matches_state(
                wrong_state,
                EffectEvidenceCode::ProcessUncertain,
            ));
        }
        Ok(())
    }

    #[test]
    fn existing_v1_and_v2_evidence_bytes_are_unchanged() -> Result<(), Box<dyn std::error::Error>> {
        assert_eq!(
            EffectEvidence::exit_observed_success().canonical_json()?,
            r#"{"code":"exit_observed_success","exit_code":0}"#,
        );
        assert_eq!(
            EffectEvidence::exit_observed_failure(7)?.canonical_json()?,
            r#"{"code":"exit_observed_failure","exit_code":7}"#,
        );
        assert_eq!(
            EffectEvidence::process_not_started(ProcessNotStartedEvidenceReason::GateRejected)
                .canonical_json()?,
            r#"{"code":"process_not_started","process_not_started_reason":"gate_rejected"}"#,
        );
        assert_eq!(
            EffectEvidence::exit_status_lost().canonical_json()?,
            r#"{"code":"exit_status_lost"}"#,
        );
        assert_eq!(
            EffectEvidence::process_uncertain(ProcessUncertaintyEvidence::ReleaseDelivery)
                .canonical_json()?,
            r#"{"code":"process_uncertain","process_uncertainty":"release_delivery"}"#,
        );
        assert_eq!(
            legacy_v1_evidence(
                "custom_failure",
                BTreeMap::from([("result".to_owned(), "bounded".to_owned())]),
            )?,
            r#"{"code":"custom_failure","metadata":{"result":"bounded"}}"#,
        );
        Ok(())
    }

    #[test]
    fn proven_not_started_without_identity_is_durable_and_requires_authorized_retry()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (key, claimed) = append_acknowledge_claim(
            &mut store,
            "transition-not-started",
            effect(
                OutboxEffectKind::ProviderProcess,
                1,
                serde_json::json!({"provider":"claude"}),
            )?,
        )?;
        assert!(claimed.execution_identity().is_none());
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::NotStarted(EffectEvidence::exit_status_lost()),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::Failed(EffectEvidence::process_not_started(
                    ProcessNotStartedEvidenceReason::SpawnFailed,
                )),
                &timestamp(4),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));

        let failed = store.resolve_effect(
            &key,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::SpawnFailed,
            )),
            &timestamp(4),
        )?;
        assert_eq!(failed.state(), OutboxState::Failed);
        assert!(failed.execution_identity().is_none());
        assert!(store.claim_next(&timestamp(5))?.is_none());
        store.close()?;

        let mut reopened = open_store(Arc::clone(&boundary))?;
        let history = reopened.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].state(), OutboxState::Failed);
        assert_eq!(
            history[0].evidence().process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::SpawnFailed),
        );
        assert!(reopened.pending_effects(10)?.is_empty());
        let pending = reopened.resolve_effect(
            &key,
            EffectResolution::RetryFailed(policy_authorization(
                &key,
                1,
                "retry-policy-v1",
                "retry-not-started-1",
                timestamp(5),
            )?),
            &timestamp(5),
        )?;
        assert_eq!(pending.state(), OutboxState::Pending);
        let second = reopened
            .claim_next(&timestamp(6))?
            .ok_or("missing authorized retry claim")?;
        assert_eq!(second.attempts(), 2);
        assert!(second.execution_identity().is_none());
        reopened.resolve_effect(
            &key,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::Deadline,
            )),
            &timestamp(7),
        )?;
        reopened.close()?;

        let verified = open_store(boundary)?;
        let history = verified.attempt_history(&key, 10)?;
        assert_eq!(
            history
                .iter()
                .map(EffectObservation::state)
                .collect::<Vec<_>>(),
            vec![
                OutboxState::Failed,
                OutboxState::Pending,
                OutboxState::Failed,
            ],
        );
        assert_eq!(
            history[2].evidence().process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::Deadline),
        );
        verified.close()?;
        Ok(())
    }

    #[test]
    fn proven_not_started_may_retain_a_blocked_launcher_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (key, _) = append_acknowledge_claim(
            &mut store,
            "transition-gate-rejected",
            effect(
                OutboxEffectKind::ProviderProcess,
                1,
                serde_json::json!({"provider":"claude"}),
            )?,
        )?;
        let identity = ProcessExecutionIdentity::new(1, 301, 301, "boot-gate:1", timestamp(3))?;
        store.record_execution_identity(&key, &identity)?;
        let failed = store.resolve_effect(
            &key,
            EffectResolution::NotStarted(EffectEvidence::process_not_started(
                ProcessNotStartedEvidenceReason::GateRejected,
            )),
            &timestamp(4),
        )?;
        assert_eq!(failed.execution_identity(), Some(&identity));
        store.close()?;

        let reopened = open_store(boundary)?;
        assert_eq!(
            load_execution_identity(reopened.connection()?, &key, 1)?,
            Some(identity),
        );
        assert_eq!(
            reopened.attempt_history(&key, 10)?[0]
                .evidence()
                .process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::GateRejected),
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn malformed_process_not_started_evidence_fails_reopen_validation()
    -> Result<(), Box<dyn std::error::Error>> {
        for (index, malformed) in [
            r#"{"code":"process_not_started"}"#,
            r#"{"code":"process_not_started","process_not_started_reason":"launcher said no"}"#,
            r#"{"code":"process_not_started","process_not_started_reason":"cancelled","detail":"freeform"}"#,
        ]
        .into_iter()
        .enumerate()
        {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_store(Arc::clone(&boundary))?;
            let (key, _) = append_acknowledge_claim(
                &mut store,
                &format!("transition-tamper-{index}"),
                effect(
                    OutboxEffectKind::ProviderProcess,
                    1,
                    serde_json::json!({"provider":"claude"}),
                )?,
            )?;
            store.resolve_effect(
                &key,
                EffectResolution::NotStarted(EffectEvidence::process_not_started(
                    ProcessNotStartedEvidenceReason::Cancelled,
                )),
                &timestamp(4),
            )?;
            store.close()?;

            let connection = Connection::open(home.database())?;
            connection.execute_batch("DROP TRIGGER outbox_attempt_observation_reject_update;")?;
            connection.execute(
                "UPDATE outbox_attempt_observation SET evidence_json = ?1",
                [malformed],
            )?;
            connection.execute_batch(
                "CREATE TRIGGER outbox_attempt_observation_reject_update
                    BEFORE UPDATE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are immutable'); END;",
            )?;
            assert_eq!(schema_digest(&connection)?, NATIVE_V7_SCHEMA_DIGEST);
            drop(connection);
            assert!(matches!(
                open_store(boundary),
                Err(RuntimeStoreError::CorruptDatabase)
            ));
        }
        Ok(())
    }

    #[test]
    fn process_not_started_evidence_cannot_be_forged_after_uncertainty()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (key, _) = append_acknowledge_claim(
            &mut store,
            "transition-uncertain-tamper",
            effect(
                OutboxEffectKind::ProviderProcess,
                1,
                serde_json::json!({"provider":"claude"}),
            )?,
        )?;
        store.record_execution_identity(
            &key,
            &ProcessExecutionIdentity::new(1, 401, 401, "boot-uncertain:1", timestamp(3))?,
        )?;
        store.resolve_effect(
            &key,
            EffectResolution::Uncertain(EffectEvidence::exit_status_lost()),
            &timestamp(4),
        )?;
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::NotStarted(EffectEvidence::process_not_started(
                    ProcessNotStartedEvidenceReason::GateProtocol,
                )),
                &timestamp(5),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        store.resolve_effect(
            &key,
            EffectResolution::Failed(EffectEvidence::exit_observed_failure(1)?),
            &timestamp(5),
        )?;
        store.close()?;

        let forged =
            EffectEvidence::process_not_started(ProcessNotStartedEvidenceReason::GateProtocol)
                .canonical_json()?;
        let connection = Connection::open(home.database())?;
        connection.execute_batch("DROP TRIGGER outbox_attempt_observation_reject_update;")?;
        connection.execute(
            "UPDATE outbox_attempt_observation
             SET evidence_code = 'process_not_started', evidence_json = ?1
             WHERE observation_sequence = 2",
            [&forged],
        )?;
        connection.execute_batch(
            "CREATE TRIGGER outbox_attempt_observation_reject_update
                    BEFORE UPDATE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are immutable'); END;",
        )?;
        assert_eq!(schema_digest(&connection)?, NATIVE_V7_SCHEMA_DIGEST);
        drop(connection);
        assert!(matches!(
            open_store(boundary),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn process_outcome_source_state_tampering_fails_reopen_with_schema_intact()
    -> Result<(), Box<dyn std::error::Error>> {
        for replace_second_with_uncertainty in [false, true] {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_store(Arc::clone(&boundary))?;
            let (key, _) = append_acknowledge_claim(
                &mut store,
                if replace_second_with_uncertainty {
                    "transition-source-tamper-uncertain"
                } else {
                    "transition-source-tamper-failed"
                },
                effect_in_slot(
                    OutboxEffectKind::ProviderProcess,
                    if replace_second_with_uncertainty {
                        "source-tamper-uncertain"
                    } else {
                        "source-tamper-failed"
                    },
                    1,
                    serde_json::json!({"provider":"claude"}),
                )?,
            )?;
            store.record_execution_identity(
                &key,
                &ProcessExecutionIdentity::new(1, 601, 601, "boot-tamper:1", timestamp(3))?,
            )?;
            store.resolve_effect(
                &key,
                EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                    ProcessUncertaintyEvidence::ReleaseDelivery,
                )),
                &timestamp(4),
            )?;
            store.resolve_effect(
                &key,
                EffectResolution::ProcessFailed(EffectEvidence::process_failed(
                    StartedProcessFailureEvidence::Deadline,
                )?),
                &timestamp(5),
            )?;
            store.close()?;

            let connection = Connection::open(home.database())?;
            connection.execute_batch("DROP TRIGGER outbox_attempt_observation_reject_update;")?;
            if replace_second_with_uncertainty {
                let forged = EffectEvidence::process_uncertain(
                    ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
                )
                .canonical_json()?;
                connection.execute(
                    "UPDATE outbox_attempt_observation
                     SET observed_state = 'uncertain', evidence_code = 'process_uncertain',
                         evidence_json = ?1
                     WHERE observation_sequence = 2",
                    [&forged],
                )?;
                connection.execute("UPDATE outbox SET state = 'uncertain'", [])?;
            } else {
                let forged = EffectEvidence::process_failed(
                    StartedProcessFailureEvidence::InfrastructureFailure,
                )?
                .canonical_json()?;
                connection.execute(
                    "UPDATE outbox_attempt_observation
                     SET observed_state = 'failed', evidence_code = 'process_failed',
                         evidence_json = ?1
                     WHERE observation_sequence = 1",
                    [&forged],
                )?;
            }
            connection.execute_batch(
                "CREATE TRIGGER outbox_attempt_observation_reject_update
                    BEFORE UPDATE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are immutable'); END;",
            )?;
            assert_eq!(schema_digest(&connection)?, NATIVE_V7_SCHEMA_DIGEST);
            drop(connection);
            assert!(matches!(
                open_store(boundary),
                Err(RuntimeStoreError::CorruptDatabase)
            ));
        }
        Ok(())
    }

    #[test]
    fn malformed_process_outcome_rows_fail_reopen_with_schema_intact()
    -> Result<(), Box<dyn std::error::Error>> {
        let canonical_process_failure =
            EffectEvidence::process_failed(StartedProcessFailureEvidence::Deadline)?
                .canonical_json()?;
        let canonical_process_uncertainty = EffectEvidence::process_uncertain(
            ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
        )
        .canonical_json()?;
        for (index, (observed_state, evidence_code, malformed)) in [
            (
                "uncertain",
                "process_failed",
                canonical_process_failure.as_str(),
            ),
            (
                "failed",
                "process_uncertain",
                canonical_process_uncertainty.as_str(),
            ),
            ("failed", "process_failed", r#"{"code":"process_failed"}"#),
            (
                "failed",
                "process_failed",
                r#"{"code":"process_failed","started_process_failure":"deadline","detail":"freeform"}"#,
            ),
            (
                "failed",
                "process_failed",
                r#"{"code":"process_failed","started_process_failure":"launcher crashed"}"#,
            ),
            (
                "failed",
                "process_failed",
                r#"{"code":"process_failed","started_process_failure":{"signaled":0}}"#,
            ),
            (
                "failed",
                "process_failed",
                r#"{"code":"process_failed","process_uncertainty":"release_delivery"}"#,
            ),
            (
                "failed",
                "process_failed",
                r#"{"started_process_failure":"deadline","code":"process_failed"}"#,
            ),
            (
                "uncertain",
                "process_uncertain",
                r#"{"code":"process_uncertain"}"#,
            ),
            (
                "uncertain",
                "process_uncertain",
                r#"{"code":"process_uncertain","process_uncertainty":"cleanup_incomplete"}"#,
            ),
            (
                "uncertain",
                "process_uncertain",
                r#"{"code":"process_uncertain","process_uncertainty":{"cleanup_incomplete":"launcher crashed"}}"#,
            ),
            (
                "uncertain",
                "process_uncertain",
                r#"{"code":"process_uncertain","process_uncertainty":"release_delivery","detail":"freeform"}"#,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_store(Arc::clone(&boundary))?;
            let (key, _) = append_acknowledge_claim(
                &mut store,
                &format!("transition-process-row-tamper-{index}"),
                effect_in_slot(
                    OutboxEffectKind::ProviderProcess,
                    &format!("process-row-tamper-{index}"),
                    1,
                    serde_json::json!({"provider":"claude"}),
                )?,
            )?;
            store.record_execution_identity(
                &key,
                &ProcessExecutionIdentity::new(1, 701, 701, "boot-row-tamper:1", timestamp(3))?,
            )?;
            store.resolve_effect(
                &key,
                EffectResolution::ProcessFailed(EffectEvidence::process_failed(
                    StartedProcessFailureEvidence::Deadline,
                )?),
                &timestamp(4),
            )?;
            store.close()?;

            let connection = Connection::open(home.database())?;
            connection.execute_batch("DROP TRIGGER outbox_attempt_observation_reject_update;")?;
            connection.execute(
                "UPDATE outbox_attempt_observation
                 SET observed_state = ?1, evidence_code = ?2, evidence_json = ?3",
                params![observed_state, evidence_code, malformed],
            )?;
            connection.execute(
                "UPDATE outbox SET state = ?1",
                [observed_state],
            )?;
            connection.execute_batch(
                "CREATE TRIGGER outbox_attempt_observation_reject_update
                    BEFORE UPDATE ON outbox_attempt_observation
                    BEGIN SELECT RAISE(ABORT, 'outbox observations are immutable'); END;",
            )?;
            assert_eq!(schema_digest(&connection)?, NATIVE_V7_SCHEMA_DIGEST);
            drop(connection);
            assert!(matches!(
                open_store(boundary),
                Err(RuntimeStoreError::CorruptDatabase)
            ));
        }
        Ok(())
    }

    #[test]
    fn uncertain_retry_requires_evidence_and_history_survives_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let (key, _) = append_acknowledge_claim(
            &mut store,
            "transition-1",
            effect(
                OutboxEffectKind::GitCommand,
                1,
                serde_json::json!({"operation":"commit"}),
            )?,
        )?;
        store.record_execution_identity(
            &key,
            &ProcessExecutionIdentity::new(1, 201, 201, "boot-b:1", timestamp(3))?,
        )?;
        store.resolve_effect(
            &key,
            EffectResolution::Uncertain(evidence(EffectEvidenceCode::ExitStatusLost)?),
            &timestamp(4),
        )?;
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::OperatorAuthorizedRetry(policy_authorization(
                    &key,
                    1,
                    "retry-policy-v1",
                    "policy-incorrect-for-uncertain",
                    timestamp(5),
                )?),
                &timestamp(5),
            ),
            Err(RuntimeStoreError::RetryAuthorizationRejected)
        ));
        assert!(matches!(
            store.resolve_effect(
                &key,
                EffectResolution::ObservedAbsent(evidence(EffectEvidenceCode::ExitStatusLost)?),
                &timestamp(5),
            ),
            Err(RuntimeStoreError::InvalidOutboxTransition)
        ));
        let pending = store.resolve_effect(
            &key,
            EffectResolution::ObservedAbsent(evidence(EffectEvidenceCode::ObservedAbsent)?),
            &timestamp(5),
        )?;
        assert_eq!(pending.state(), OutboxState::Pending);
        let second = store
            .claim_next(&timestamp(6))?
            .ok_or("missing retry claim")?;
        assert_eq!(second.attempts(), 2);
        store.record_execution_identity(
            &key,
            &ProcessExecutionIdentity::new(2, 202, 202, "boot-b:2", timestamp(6))?,
        )?;
        store.resolve_effect(
            &key,
            EffectResolution::Failed(evidence(EffectEvidenceCode::ExitObservedFailure)?),
            &timestamp(7),
        )?;
        store.resolve_effect(
            &key,
            EffectResolution::RetryFailed(policy_authorization(
                &key,
                2,
                "retry-policy-v1",
                "retry-policy-1",
                timestamp(8),
            )?),
            &timestamp(8),
        )?;
        let third = store
            .claim_next(&timestamp(9))?
            .ok_or("missing operator-retry claim")?;
        assert_eq!(third.attempts(), 3);
        store.record_execution_identity(
            &key,
            &ProcessExecutionIdentity::new(3, 203, 203, "boot-b:3", timestamp(9))?,
        )?;
        store.resolve_effect(
            &key,
            EffectResolution::Uncertain(evidence(EffectEvidenceCode::RemoteStateUnobservable)?),
            &timestamp(10),
        )?;
        store.resolve_effect(
            &key,
            EffectResolution::OperatorAuthorizedRetry(operator_authorization(
                &key,
                3,
                "operator-policy-v1",
                "operator-decision-1",
                timestamp(11),
            )?),
            &timestamp(11),
        )?;
        let history = store.attempt_history(&key, 10)?;
        assert_eq!(history.len(), 6);
        assert_eq!(
            history
                .iter()
                .map(EffectObservation::state)
                .collect::<Vec<_>>(),
            vec![
                OutboxState::Uncertain,
                OutboxState::Pending,
                OutboxState::Failed,
                OutboxState::Pending,
                OutboxState::Uncertain,
                OutboxState::Pending,
            ]
        );
        store.close()?;

        let reopened = open_store(boundary)?;
        assert_eq!(reopened.attempt_history(&key, 10)?, history);
        assert_eq!(reopened.pending_effects(10)?[0].attempts(), 3);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn retry_authorization_is_exact_single_use_and_cross_effect_safe()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let transition = intent("transition-1", 1)?
            .with_outbox(effect_in_slot(
                OutboxEffectKind::GitCommand,
                "commit-primary",
                1,
                serde_json::json!({"operation":"commit"}),
            )?)?
            .with_outbox(effect_in_slot(
                OutboxEffectKind::GitCommand,
                "commit-review",
                1,
                serde_json::json!({"operation":"commit"}),
            )?)?;
        let commit = store.append(&transition)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;
        let mut failed_keys = Vec::new();
        for (offset, pid) in [(3_u8, 501_u32), (4_u8, 502_u32)] {
            let claimed = store
                .claim_next(&timestamp(offset))?
                .ok_or("missing effect claim")?;
            let key = claimed.idempotency_key().to_owned();
            store.record_execution_identity(
                &key,
                &ProcessExecutionIdentity::new(
                    1,
                    pid,
                    pid,
                    format!("boot-retry:{pid}"),
                    timestamp(offset),
                )?,
            )?;
            store.resolve_effect(
                &key,
                EffectResolution::Failed(EffectEvidence::exit_observed_failure(1)?),
                &timestamp(offset.saturating_add(1)),
            )?;
            failed_keys.push(key);
        }
        let first_key = &failed_keys[0];
        let second_key = &failed_keys[1];
        let first_authorization = policy_authorization(
            first_key,
            1,
            "retry-policy-v7",
            "retry-decision-single-use",
            timestamp(7),
        )?;
        assert!(matches!(
            store.resolve_effect(
                second_key,
                EffectResolution::RetryFailed(first_authorization.clone()),
                &timestamp(7),
            ),
            Err(RuntimeStoreError::RetryAuthorizationRejected)
        ));
        assert!(matches!(
            store.resolve_effect(
                first_key,
                EffectResolution::RetryFailed(policy_authorization(
                    first_key,
                    2,
                    "retry-policy-v7",
                    "retry-decision-wrong-attempt",
                    timestamp(7),
                )?),
                &timestamp(7),
            ),
            Err(RuntimeStoreError::RetryAuthorizationRejected)
        ));
        store.resolve_effect(
            first_key,
            EffectResolution::RetryFailed(first_authorization.clone()),
            &timestamp(7),
        )?;
        assert!(matches!(
            store.resolve_effect(
                first_key,
                EffectResolution::RetryFailed(first_authorization),
                &timestamp(7),
            ),
            Err(RuntimeStoreError::RetryAuthorizationRejected)
        ));
        assert!(matches!(
            store.resolve_effect(
                second_key,
                EffectResolution::RetryFailed(policy_authorization(
                    second_key,
                    1,
                    "retry-policy-v7",
                    "retry-decision-single-use",
                    timestamp(7),
                )?),
                &timestamp(7),
            ),
            Err(RuntimeStoreError::RetryAuthorizationRejected)
        ));
        store.resolve_effect(
            second_key,
            EffectResolution::RetryFailed(policy_authorization(
                second_key,
                1,
                "retry-policy-v7",
                "retry-decision-second",
                timestamp(8),
            )?),
            &timestamp(8),
        )?;
        let consumed: i64 = store.connection()?.query_row(
            "SELECT count(*) FROM retry_authorization_consumption",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(consumed, 2);
        store.close()?;
        let reopened = open_store(boundary)?;
        assert_eq!(reopened.pending_effects(10)?.len(), 2);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn typed_evidence_rejects_free_form_fields_and_redacts_authority_details()
    -> Result<(), Box<dyn std::error::Error>> {
        assert!(EffectEvidence::exit_observed_failure(0).is_err());
        assert!(
            EffectEvidence::from_stored_json(
                r#"{"code":"exit_status_lost","metadata":{"token":"secret"}}"#,
            )
            .is_err()
        );
        assert!(EffectEvidence::from_stored_json(
            r#"{"authorization_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","code":"observed_absent"}"#,
        )
        .is_err());
        let marker = "secret-token-must-not-appear";
        let authorization = policy_authorization(
            "effect_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            1,
            marker,
            marker,
            timestamp(1),
        )?;
        let authorization_debug = format!("{authorization:?}");
        let resolution_debug =
            format!("{:?}", EffectResolution::RetryFailed(authorization.clone()));
        let evidence_json = authorization.evidence().canonical_json()?;
        assert!(!authorization_debug.contains(marker));
        assert!(!resolution_debug.contains(marker));
        assert!(!evidence_json.contains(marker));
        assert_eq!(
            authorization.evidence().code(),
            EffectEvidenceCode::PolicyAuthorizedRetry
        );
        Ok(())
    }

    #[test]
    fn connection_contract_and_exact_schema_body_are_verified()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let store = open_store(Arc::clone(&boundary))?;
        let connection = store.connection()?;
        assert_eq!(
            connection.pragma_query_value::<String, _>(None, "journal_mode", |row| row.get(0))?,
            "wal"
        );
        assert_eq!(
            connection.pragma_query_value::<i64, _>(None, "foreign_keys", |row| row.get(0))?,
            1
        );
        assert_eq!(
            connection.pragma_query_value::<i64, _>(None, "synchronous", |row| row.get(0))?,
            2
        );
        assert_eq!(
            connection.pragma_query_value::<i64, _>(None, "trusted_schema", |row| row.get(0))?,
            0
        );
        assert_eq!(
            connection.pragma_query_value::<i64, _>(None, "busy_timeout", |row| row.get(0))?,
            5_000
        );
        store.close()?;

        let connection = Connection::open(home.database())?;
        connection.execute_batch(
            "DROP TRIGGER outbox_reject_delete;
             CREATE TRIGGER outbox_reject_delete
             BEFORE DELETE ON outbox BEGIN SELECT 1; END;",
        )?;
        drop(connection);
        assert!(matches!(
            open_store(boundary),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn database_identity_replacement_fails_closed() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let store = open_store(Arc::clone(&boundary))?;
        let original = home.path.join("runtime.db.original");
        fs::rename(home.database(), &original)?;
        fs::copy(&original, home.database())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(home.database(), fs::Permissions::from_mode(0o600))?;
        }
        assert!(matches!(
            store.pending_effects(10),
            Err(RuntimeStoreError::UnsafeEntry)
        ));
        drop(store);
        assert!(matches!(
            open_store(boundary),
            Err(RuntimeStoreError::WriterLeased)
        ));
        Ok(())
    }

    #[test]
    fn wal_and_shared_memory_link_mutation_fail_closed() -> Result<(), Box<dyn std::error::Error>> {
        for sidecar in [DATABASE_WAL_FILE, DATABASE_SHM_FILE] {
            let home = TestHome::new()?;
            let store = open_store(home.boundary()?)?;
            let sidecar_path = home.path.join(sidecar);
            assert!(sidecar_path.is_file(), "missing SQLite sidecar {sidecar}");
            fs::hard_link(&sidecar_path, home.path.join(format!("{sidecar}.link")))?;
            assert!(matches!(
                store.pending_effects(10),
                Err(RuntimeStoreError::UnsafeEntry)
            ));
            drop(store);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_preexisting_wal_and_shm_fail_before_sqlite_touches_store()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

        #[derive(Clone, Copy, Debug)]
        enum UnsafeShape {
            HardLink,
            WrongMode,
            Symlink,
        }

        for sealed in [true, false] {
            for sidecar in [DATABASE_WAL_FILE, DATABASE_SHM_FILE] {
                for shape in [
                    UnsafeShape::HardLink,
                    UnsafeShape::WrongMode,
                    UnsafeShape::Symlink,
                ] {
                    let home = TestHome::new()?;
                    let boundary = home.boundary()?;
                    open_store(Arc::clone(&boundary))?.close()?;
                    for name in [DATABASE_WAL_FILE, DATABASE_SHM_FILE] {
                        remove_optional_test_file(&home.path.join(name))?;
                    }
                    if !sealed {
                        fs::remove_file(home.path.join(COMPATIBILITY_BOUNDARY_SEAL_FILE))?;
                    }

                    let sidecar_path = home.path.join(sidecar);
                    let companion = home.path.join(format!("{sidecar}.unsafe-companion"));
                    match shape {
                        UnsafeShape::HardLink => {
                            write_private_test_file(&sidecar_path, b"unsafe-sidecar-sentinel")?;
                            fs::hard_link(&sidecar_path, &companion)?;
                        }
                        UnsafeShape::WrongMode => {
                            write_private_test_file(&sidecar_path, b"unsafe-sidecar-sentinel")?;
                            fs::set_permissions(&sidecar_path, fs::Permissions::from_mode(0o640))?;
                        }
                        UnsafeShape::Symlink => {
                            write_private_test_file(&companion, b"unsafe-sidecar-sentinel")?;
                            symlink(&companion, &sidecar_path)?;
                        }
                    }
                    let files_before = runtime_file_snapshot(&home)?;

                    assert!(
                        matches!(open_store(boundary), Err(RuntimeStoreError::UnsafeEntry)),
                        "sealed={sealed} {sidecar} {shape:?}"
                    );
                    assert_eq!(
                        runtime_file_snapshot(&home)?,
                        files_before,
                        "sealed={sealed} {sidecar} {shape:?} was touched before rejection"
                    );
                    let metadata = fs::symlink_metadata(&sidecar_path)?;
                    match shape {
                        UnsafeShape::HardLink => assert_eq!(metadata.nlink(), 2),
                        UnsafeShape::WrongMode => {
                            assert_eq!(metadata.permissions().mode() & 0o777, 0o640)
                        }
                        UnsafeShape::Symlink => assert!(metadata.file_type().is_symlink()),
                    }

                    remove_optional_test_file(&sidecar_path)?;
                    remove_optional_test_file(&companion)?;
                }
            }
        }
        Ok(())
    }

    #[test]
    fn busy_checkpoint_blocks_writer_handoff() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        store.append(&intent("transition-1", 1)?)?;
        let observer = Connection::open(home.database())?;
        observer.execute_batch("BEGIN; SELECT count(*) FROM journal;")?;
        store.append(&intent("transition-2", 2)?)?;
        assert!(matches!(
            store.close(),
            Err(RuntimeStoreError::CloseIncomplete)
        ));
        assert!(matches!(
            open_store(boundary),
            Err(RuntimeStoreError::WriterLeased)
        ));
        observer.execute_batch("ROLLBACK")?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn compatibility_claim_refuses_typed_head_without_mutation_or_reordering()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mut store = open_store(home.boundary()?)?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let typed = typed_process_effect(&request, "typed-head", 1)?;
        let typed_key = typed.idempotency_key().to_owned();
        let typed_commit = store.append(&intent("typed-head", 1)?.with_outbox(typed)?)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            typed_commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;

        let legacy = effect_in_slot(
            OutboxEffectKind::PluginProcess,
            "legacy-behind",
            1,
            serde_json::json!({"legacy":true}),
        )?;
        let legacy_key = legacy.idempotency_key().to_owned();
        let legacy_commit = store.append(&intent("legacy-behind", 3)?.with_outbox(legacy)?)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            legacy_commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(4),
        )?)?;

        assert!(matches!(
            store.claim_next(&timestamp(5)),
            Err(RuntimeStoreError::TypedProcessRequiresExactClaim)
        ));
        let pending = store.pending_effects(10)?;
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].idempotency_key(), typed_key);
        assert_eq!(pending[0].state(), OutboxState::Pending);
        assert_eq!(pending[0].attempts(), 0);
        assert_eq!(pending[1].idempotency_key(), legacy_key);
        assert_eq!(pending[1].state(), OutboxState::Pending);
        assert_eq!(pending[1].attempts(), 0);

        let typed_claim = store.claim_exact(&typed_key, &timestamp(6))?;
        assert_eq!(typed_claim.claim_attempt(), 1);
        let legacy_claim = store
            .claim_next(&timestamp(7))?
            .ok_or("missing legacy claim")?;
        assert_eq!(legacy_claim.idempotency_key(), legacy_key);
        assert_eq!(legacy_claim.attempts(), 1);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn typed_process_identity_and_release_authorization_commit_once_and_survive_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        let (claimed, _) =
            append_and_claim_typed_process_private(&mut store, "typed-release", &request, 1)?;
        let binding = ProcessRequestBinding::from_bytes(request.fingerprint().into_bytes());
        let identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_001,
            1_001,
            "fixture:start:1",
            timestamp(4),
        )?;
        let gate = observed_gate(&binding, &identity);
        prepare_blocked_identity_for_test(&mut store, &claimed, &request, &gate, &identity, 5)?;

        let authorization = store.inner.authorize_process_release_bound(
            claimed.claimed(),
            &request,
            &gate,
            &identity,
            &timestamp(5),
            false,
        )?;
        let rendered = format!("{authorization:?}");
        assert!(rendered.contains("[REDACTED]"));
        for secret in [
            "enrolled-worker",
            "/fixture/worker",
            "fixture-model",
            "sensitive-value",
            "bounded-input",
            "effect_",
            "1001",
        ] {
            assert!(!rendered.contains(secret));
        }
        assert!(
            store.process_release_was_authorized(
                claimed.idempotency_key(),
                claimed.claim_attempt()
            )?
        );
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed.claimed(),
                &request,
                &gate,
                &identity,
                &timestamp(6),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        let conflicting_identity = ProcessExecutionIdentity::new(
            claimed.claim_attempt(),
            1_007,
            1_007,
            "fixture:start:1",
            timestamp(6),
        )?;
        let conflicting_gate = observed_gate(&binding, &conflicting_identity);
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed.claimed(),
                &request,
                &conflicting_gate,
                &conflicting_identity,
                &timestamp(6),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        store.close()?;

        let mut reopened = open_private_store(boundary)?;
        assert!(
            reopened.process_release_was_authorized(
                claimed.idempotency_key(),
                claimed.claim_attempt()
            )?
        );
        assert!(matches!(
            reopened.inner.authorize_process_release_bound(
                claimed.claimed(),
                &request,
                &gate,
                &identity,
                &timestamp(7),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        reopened.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_effect_key_binds_request_and_logical_effect_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let same_request_other_slot = typed_process_effect(&request, "slot-b", 1)?;
        let same_request_first_slot = typed_process_effect(&request, "slot-a", 1)?;
        let changed_request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-worker",
            "/fixture/worker",
        )?
        .with_argument("--model")?
        .with_argument("different-model")?
        .with_environment("FIXTURE_TOKEN", "sensitive-value")?
        .with_stdin(b"bounded-input".to_vec())?
        .with_max_output_bytes(32_768)?;
        let changed_request_same_slot = typed_process_effect(&changed_request, "slot-a", 1)?;

        assert_ne!(
            same_request_first_slot.idempotency_key(),
            same_request_other_slot.idempotency_key()
        );
        assert_ne!(
            same_request_first_slot.idempotency_key(),
            changed_request_same_slot.idempotency_key()
        );
        for purpose in [
            ProcessPurpose::Git,
            ProcessPurpose::Plugin,
            ProcessPurpose::Tool,
        ] {
            let mismatched = typed_process_request(purpose)?;
            assert!(matches!(
                typed_process_effect(&mismatched, "wrong-purpose", 1),
                Err(RuntimeStoreError::InvalidIntent(_))
            ));
        }
        Ok(())
    }

    #[test]
    fn stored_process_intent_is_canonical_versioned_strict_and_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let canonical = ProcessIntentEnvelope {
            schema_version: PROCESS_INTENT_SCHEMA_VERSION,
            request_sha256: "a".repeat(64),
        }
        .canonical_json()?;
        assert_eq!(
            ProcessIntentEnvelope::from_stored(&canonical)?.canonical_json()?,
            canonical
        );
        for rejected in [
            r#"{"schema_version":1,"request_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","unknown":true}"#.to_owned(),
            r#"{"request_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","schema_version":1}"#.to_owned(),
            r#"{"schema_version":2,"request_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#.to_owned(),
            r#"{"schema_version":1,"request_sha256":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#.to_owned(),
            r#"{"schema_version":1,"request_sha256":"short"}"#.to_owned(),
            "{".to_owned(),
            "x".repeat(MAX_PROCESS_INTENT_BYTES + 1),
        ] {
            assert!(ProcessIntentEnvelope::from_stored(&rejected).is_err());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn legacy_arbitrary_process_rows_are_recoverable_but_never_release_authorizable()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let legacy_effect = effect_in_slot(
            OutboxEffectKind::ProviderProcess,
            "legacy-arbitrary",
            1,
            serde_json::json!({"provider":"legacy"}),
        )?;
        let key = legacy_effect.idempotency_key().to_owned();
        let mut store = open_store(home.boundary()?)?;
        let commit = store.append(&intent("legacy-process-row", 1)?.with_outbox(legacy_effect)?)?;
        store.record_projection(&ProjectionReceipt::compatibility(
            commit.sequence(),
            CompatibilityProjection::Checkpoint,
            timestamp(2),
        )?)?;
        let claimed = store.claim_exact(&key, &timestamp(3))?;
        let binding = ProcessRequestBinding::from_bytes(request.fingerprint().into_bytes());
        let identity =
            ProcessExecutionIdentity::new(1, 1_002, 1_002, "fixture:start:1", timestamp(4))?;
        let gate = observed_gate(&binding, &identity);
        assert!(matches!(
            store.authorize_process_release_bound(
                &claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessIntentRequired)
        ));
        assert_eq!(store.recovery_effects(10)?.len(), 1);
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn release_authorization_rejects_gate_request_identity_and_claim_substitution_atomically()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(home.boundary()?)?;
        let (mut claimed_attempt, _) =
            append_and_claim_typed_process_private(&mut store, "substitution", &request, 1)?;
        let binding = ProcessRequestBinding::from_bytes(request.fingerprint().into_bytes());
        let wrong_binding = ProcessRequestBinding::from_bytes([0x5a; 32]);
        let identity =
            ProcessExecutionIdentity::new(1, 1_003, 1_003, "fixture:start:1", timestamp(4))?;
        let gate = observed_gate(&binding, &identity);
        let wrong_gate = observed_gate(&wrong_binding, &identity);
        prepare_blocked_identity_for_test(
            &mut store,
            &claimed_attempt,
            &request,
            &gate,
            &identity,
            5,
        )?;
        let claimed = &mut claimed_attempt.claimed;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &wrong_gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessRequestMismatch)
        ));

        let changed_request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "enrolled-worker",
            "/fixture/worker",
        )?
        .with_argument("--model")?
        .with_argument("substituted-model")?
        .with_environment("FIXTURE_TOKEN", "sensitive-value")?
        .with_stdin(b"bounded-input".to_vec())?
        .with_max_output_bytes(32_768)?;
        let changed_binding =
            ProcessRequestBinding::from_bytes(changed_request.fingerprint().into_bytes());
        let changed_gate = observed_gate(&changed_binding, &identity);
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &changed_request,
                &changed_gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessRequestMismatch)
        ));

        let wrong_identity =
            ProcessExecutionIdentity::new(1, 1_004, 1_003, "fixture:start:1", timestamp(4))?;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &wrong_identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessIdentityMismatch)
        ));

        let original_sequence = claimed.journal_sequence;
        claimed.journal_sequence = original_sequence + 1;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        claimed.journal_sequence = original_sequence;

        let original_mission = claimed.mission_id.clone();
        claimed.mission_id = MissionId::new("mission-substituted")?;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        claimed.mission_id = original_mission;

        let original_phase = claimed.phase_id.clone();
        claimed.phase_id = Some("phase-substituted".to_owned());
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        claimed.phase_id = original_phase;

        let original_kind = claimed.effect_kind;
        claimed.effect_kind = OutboxEffectKind::GitCommand;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessRequestMismatch)
        ));
        claimed.effect_kind = original_kind;

        let original_slot = claimed.operation_slot.clone();
        claimed.operation_slot = EffectOperationSlot::new("slot-substituted")?;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        claimed.operation_slot = original_slot;

        let original_logical_attempt = claimed.logical_attempt;
        claimed.logical_attempt = original_logical_attempt + 1;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        claimed.logical_attempt = original_logical_attempt;

        let original_claim_attempt = claimed.claim_attempt;
        claimed.claim_attempt = original_claim_attempt + 1;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessIdentityMismatch)
        ));
        claimed.claim_attempt = original_claim_attempt;

        let original_payload = claimed.payload.clone();
        claimed.payload = serde_json::json!({"provider":"substituted"});
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        claimed.payload = original_payload;

        let original_key = claimed.idempotency_key.clone();
        claimed.idempotency_key =
            "effect_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_owned();
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed,
                &request,
                &gate,
                &identity,
                &timestamp(5),
                false,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        claimed.idempotency_key = original_key;

        assert!(
            !store.process_release_was_authorized(
                claimed.idempotency_key(),
                claimed.claim_attempt()
            )?
        );
        assert!(
            load_execution_identity(
                store.connection()?,
                claimed.idempotency_key(),
                claimed.claim_attempt()
            )?
            .is_some()
        );
        let _authorization = store.inner.authorize_process_release_bound(
            claimed,
            &request,
            &gate,
            &identity,
            &timestamp(6),
            false,
        )?;
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn release_authorizations_reject_two_gate_capability_swaps()
    -> Result<(), Box<dyn std::error::Error>> {
        let request_a = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let request_b = typed_process_request(ProcessPurpose::ProviderWorker)?
            .with_argument("different-request")?;
        let binding_a = ProcessRequestBinding::from_bytes(request_a.fingerprint().into_bytes());
        let binding_b = ProcessRequestBinding::from_bytes(request_b.fingerprint().into_bytes());
        let gate_a = ObservedGateBinding {
            request_binding: &binding_a,
            pid: 801,
            process_group_id: 801,
            process_start_identity: "fixture:start:a",
        };
        let gate_b = ObservedGateBinding {
            request_binding: &binding_b,
            pid: 802,
            process_group_id: 802,
            process_start_identity: "fixture:start:b",
        };
        let authorization_a = ProcessReleaseAuthorization {
            _idempotency_key: "effect-a".to_owned(),
            claim_attempt: 1,
            request_fingerprint: request_a.fingerprint(),
            pid: gate_a.pid,
            process_group_id: gate_a.process_group_id,
            process_start_identity: gate_a.process_start_identity.to_owned(),
        };
        let authorization_b = ProcessReleaseAuthorization {
            _idempotency_key: "effect-b".to_owned(),
            claim_attempt: 1,
            request_fingerprint: request_b.fingerprint(),
            pid: gate_b.pid,
            process_group_id: gate_b.process_group_id,
            process_start_identity: gate_b.process_start_identity.to_owned(),
        };

        assert!(authorization_a.matches_observed(&gate_a));
        assert!(authorization_b.matches_observed(&gate_b));
        assert!(!authorization_a.matches_observed(&gate_b));
        assert!(!authorization_b.matches_observed(&gate_a));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn release_authorization_failure_retains_blocked_identity_without_authorizing()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
        let mut store = open_private_store(home.boundary()?)?;
        let (claimed, _) =
            append_and_claim_typed_process_private(&mut store, "rollback-release", &request, 1)?;
        let binding = ProcessRequestBinding::from_bytes(request.fingerprint().into_bytes());
        let identity =
            ProcessExecutionIdentity::new(1, 1_005, 1_005, "fixture:start:1", timestamp(4))?;
        let gate = observed_gate(&binding, &identity);
        prepare_blocked_identity_for_test(&mut store, &claimed, &request, &gate, &identity, 5)?;
        assert!(matches!(
            store.inner.authorize_process_release_bound(
                claimed.claimed(),
                &request,
                &gate,
                &identity,
                &timestamp(6),
                true,
            ),
            Err(RuntimeStoreError::ProcessReleaseAuthorizationRejected)
        ));
        assert!(
            load_execution_identity(
                store.connection()?,
                claimed.idempotency_key(),
                claimed.claim_attempt()
            )?
            .is_some()
        );
        assert!(
            !store.process_release_was_authorized(
                claimed.idempotency_key(),
                claimed.claim_attempt()
            )?
        );
        let _authorization = store.inner.authorize_process_release_bound(
            claimed.claimed(),
            &request,
            &gate,
            &identity,
            &timestamp(7),
            false,
        )?;
        store.close()?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn process_intent_and_release_marker_tampering_fail_reopen_with_schema_intact()
    -> Result<(), Box<dyn std::error::Error>> {
        for tamper_authorization in [false, true] {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let request = typed_process_request(ProcessPurpose::ProviderWorker)?;
            let mut store = open_private_store(Arc::clone(&boundary))?;
            let (claimed, _) =
                append_and_claim_typed_process_private(&mut store, "tamper-release", &request, 1)?;
            let identity =
                ProcessExecutionIdentity::new(1, 1_006, 1_006, "fixture:start:1", timestamp(4))?;
            let _authorization = store.authorize_process_release_for_test(
                &claimed,
                &request,
                &identity,
                &timestamp(5),
            )?;
            store.close()?;

            let connection = Connection::open(home.database())?;
            if tamper_authorization {
                let trigger_sql: String = connection.query_row(
                    "SELECT sql FROM sqlite_schema
                     WHERE type = 'trigger'
                       AND name = 'outbox_process_release_authorization_reject_update'",
                    [],
                    |row| row.get(0),
                )?;
                connection.execute_batch(
                    "DROP TRIGGER outbox_process_release_authorization_reject_update;
                     UPDATE outbox_process_release_authorization
                        SET payload_sha256 = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';",
                )?;
                connection.execute_batch(&trigger_sql)?;
            } else {
                let request_sha256: String = connection.query_row(
                    "SELECT request_sha256 FROM outbox_process_release_authorization",
                    [],
                    |row| row.get(0),
                )?;
                let trigger_sql: String = connection.query_row(
                    "SELECT sql FROM sqlite_schema
                     WHERE type = 'trigger'
                       AND name = 'outbox_process_intent_reject_update'",
                    [],
                    |row| row.get(0),
                )?;
                connection.execute_batch("DROP TRIGGER outbox_process_intent_reject_update;")?;
                connection.execute(
                    "UPDATE outbox_process_intent SET intent_json = ?1",
                    [format!(
                        r#"{{"schema_version":1,"request_sha256":"{request_sha256}","unknown":true}}"#
                    )],
                )?;
                connection.execute_batch(&trigger_sql)?;
            }
            assert_eq!(schema_digest(&connection)?, NATIVE_V7_SCHEMA_DIGEST);
            drop(connection);
            assert!(matches!(
                open_private_store(boundary),
                Err(RuntimeStoreError::CorruptDatabase)
            ));
        }
        Ok(())
    }

    #[test]
    fn native_v2_database_migrates_additively_without_rewriting_existing_rows()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        let effect = effect(
            OutboxEffectKind::PluginProcess,
            1,
            serde_json::json!({"plugin":"legacy-v2"}),
        )?;
        let key = effect.idempotency_key().to_owned();
        store.append(&intent("native-v2-row", 1)?.with_outbox(effect)?)?;
        store.close()?;

        let connection = Connection::open(home.database())?;
        connection.execute_batch(
            "DROP TABLE audit_chain; DROP TABLE audit_chain_head; DROP TABLE knowledge_publication; DROP TABLE reasoning_revision; DROP TABLE reasoning_attempt; DROP TABLE reasoning_review; DROP TABLE reasoning_handoff; DROP TABLE reasoning_assignment; DROP TABLE reasoning_evidence; DROP TABLE reasoning_criterion; DROP TABLE reasoning_assumption; DROP TABLE reasoning_record; DROP TRIGGER runtime_schema_reject_boundary_kind_update;
                 DROP TRIGGER runtime_schema_reject_delete;
                 DROP TRIGGER runtime_schema_reject_insert;
                 ALTER TABLE runtime_schema RENAME TO runtime_schema_v4;
                 CREATE TABLE runtime_schema (
                    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                    version INTEGER NOT NULL CHECK (version > 0)
                 );
                 INSERT INTO runtime_schema(singleton, version) VALUES (1, 3);
                 DROP TABLE runtime_schema_v4;
                 PRAGMA user_version = 3;",
        )?;
        assert_eq!(schema_digest(&connection)?, NATIVE_V3_SCHEMA_DIGEST);
        connection.execute_batch(
            "DROP TABLE outbox_process_release_authorization;
             DROP TABLE outbox_process_intent;
             UPDATE runtime_schema SET version = 2 WHERE singleton = 1;
             PRAGMA user_version = 2;",
        )?;
        assert_eq!(schema_digest(&connection)?, NATIVE_SCHEMA_DIGEST);
        drop(connection);

        let reopened = open_store(boundary)?;
        assert_eq!(reopened.pending_effects(10)?[0].idempotency_key(), key);
        assert_eq!(
            read_schema_versions(reopened.connection()?)?,
            (DATABASE_SCHEMA_VERSION, DATABASE_SCHEMA_VERSION)
        );
        assert_eq!(
            schema_digest(reopened.connection()?)?,
            NATIVE_V2_MIGRATED_V7_SCHEMA_DIGEST
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn checksum_tampering_and_future_schema_fail_closed() -> Result<(), Box<dyn std::error::Error>>
    {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_store(Arc::clone(&boundary))?;
        store.append(&intent("transition-1", 1)?)?;
        store.close()?;
        let connection = Connection::open(home.database())?;
        assert!(
            connection
                .execute("DELETE FROM journal WHERE sequence = 1", [])
                .is_err()
        );
        connection.execute_batch(
            "DROP TRIGGER journal_reject_update;
             DROP TRIGGER journal_reject_delete;",
        )?;
        connection.execute(
            "UPDATE journal SET payload_json = '{\"tampered\":true}' WHERE sequence = 1",
            [],
        )?;
        drop(connection);
        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::CorruptDatabase)
        ));

        let second_home = TestHome::new()?;
        let second_boundary = second_home.boundary()?;
        open_store(Arc::clone(&second_boundary))?.close()?;
        let connection = Connection::open(second_home.database())?;
        connection.execute(
            "UPDATE runtime_schema SET version = ?1",
            [DATABASE_SCHEMA_VERSION + 1],
        )?;
        drop(connection);
        assert!(matches!(
            open_store(second_boundary),
            Err(RuntimeStoreError::UnsupportedSchema { .. })
        ));
        Ok(())
    }

    #[test]
    fn store_is_single_owner_and_database_is_private() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let store = open_store(Arc::clone(&boundary))?;
        assert!(matches!(
            open_store(Arc::clone(&boundary)),
            Err(RuntimeStoreError::WriterLeased)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(home.database())?.permissions().mode() & 0o777,
                0o600
            );
        }
        store.close()?;
        open_store(boundary)?.close()?;
        Ok(())
    }

    #[test]
    fn source_has_no_ambient_spawn_or_raw_path_constructor() {
        let source = include_str!("runtime_store.rs");
        let production = source.split("#[cfg(test)]").next().unwrap_or(source);
        assert!(!production.contains(&["std::process", "::Command"].concat()));
        assert!(!production.contains(&["std::env", "::var"].concat()));
        assert!(!production.contains(&["pub fn open", "(path"].concat()));
        assert!(!production.contains(&["unsafe", " {"].concat()));
    }

    // --- Cell 2F: durable terminal-decision persistence --------------------

    fn td_pass() -> VerificationDecision {
        decide_verification(
            VerificationOutcome::Classified(VerificationClass::Pass),
            VerificationMode::Block,
        )
    }

    fn td_block() -> VerificationDecision {
        decide_verification(
            VerificationOutcome::Classified(VerificationClass::Fail),
            VerificationMode::Block,
        )
    }

    fn td_mission() -> Result<MissionId, Box<dyn std::error::Error>> {
        Ok(MissionId::new("terminal-decision-mission")?)
    }

    fn td_record(
        verification: VerificationDecision,
        termination: Option<MechanicalTermination>,
    ) -> Result<TerminalDecisionRecord, Box<dyn std::error::Error>> {
        Ok(TerminalDecisionRecord::new(
            &td_mission()?,
            "phase-1",
            "worker-1",
            1,
            termination,
            verification,
        )?)
    }

    fn cont_mission() -> Result<MissionId, Box<dyn std::error::Error>> {
        Ok(MissionId::new("continuation-mission")?)
    }

    fn claude_handle(session: &str) -> Result<SessionHandle, Box<dyn std::error::Error>> {
        Ok(SessionHandle::new(
            RuntimeFamily::parse("claude")?,
            session,
        )?)
    }

    fn fresh_decision(
        mission: &MissionId,
        attempt: u32,
        digest: &str,
    ) -> Result<ContinuationDecisionRecord, Box<dyn std::error::Error>> {
        Ok(ContinuationDecisionRecord::new(
            mission,
            "phase-1",
            attempt,
            1,
            ContinuationDecisionKind::FreshFromCapsule,
            "codex",
            Some(digest.to_owned()),
        )?)
    }

    #[test]
    fn continuation_handle_round_trips_across_close_and_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mission = cont_mission()?;
        let handle = claude_handle("provider-session-abc")?;
        let record = ContinuationHandleRecord::from_handle(&mission, "phase-1", 2, &handle)?;

        let mut store = open_private_store(Arc::clone(&boundary))?;
        store.record_continuation_handle(&record)?;
        store.close()?;

        let reopened = open_private_store(boundary)?;
        let found = reopened
            .continuation_handle(&mission, "phase-1", 2)?
            .ok_or("persisted continuation handle did not survive reopen")?;
        let rebuilt = found.to_session_handle()?;
        assert_eq!(rebuilt.runtime_family().as_str(), "claude");
        assert_eq!(rebuilt.expose_session_id(), "provider-session-abc");
        assert_eq!(reopened.continuation_handle(&mission, "phase-1", 3)?, None);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn continuation_decision_exact_retry_is_idempotent_across_crash()
    -> Result<(), Box<dyn std::error::Error>> {
        // The store-backed half of "an interrupted resume repeats no accepted
        // effect": a durable decision consulted on resume returns the existing
        // row, never a second one. The dedup key is the journal transition_id
        // bound to (mission, phase, attempt).
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mission = cont_mission()?;
        let digest = "a".repeat(64);
        let record = fresh_decision(&mission, 1, &digest)?;

        let mut store = open_private_store(Arc::clone(&boundary))?;
        store.record_continuation_decision(&record)?;
        store.close()?;

        // Crash-injected resume: reopen the durable store and replay the same
        // decision. The exact-retry path is idempotent — no second effect.
        let mut reopened = open_private_store(boundary)?;
        reopened.record_continuation_decision(&record)?;
        let row_count: i64 = reopened.connection()?.query_row(
            "SELECT COUNT(*) FROM journal WHERE transition_kind = ?1",
            params![CONTINUATION_DECISION_TRANSITION_KIND],
            |row| row.get(0),
        )?;
        assert_eq!(row_count, 1, "resume must not re-emit the decision effect");
        let decisions = reopened.continuation_decisions(&mission)?;
        assert_eq!(decisions.len(), 1);
        let summary = decisions[0].summary();
        assert_eq!(summary.attempt, 1);
        assert!(!summary.resumed_same_provider);
        assert_eq!(summary.chosen_runtime, "codex");
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn continuation_decision_divergent_rewrite_under_same_binding_conflicts()
    -> Result<(), Box<dyn std::error::Error>> {
        // An accepted decision can never be silently changed on resume: a
        // divergent payload under the same (mission, phase, attempt) binding is
        // a conflict, not an overwrite.
        let home = TestHome::new()?;
        let mission = cont_mission()?;
        let mut store = open_private_store(home.boundary()?)?;
        store.record_continuation_decision(&fresh_decision(&mission, 1, &"a".repeat(64))?)?;
        // Same binding (attempt 1), different capsule digest.
        let conflict =
            store.record_continuation_decision(&fresh_decision(&mission, 1, &"b".repeat(64))?);
        assert!(matches!(
            conflict,
            Err(RuntimeStoreError::TransitionConflict)
        ));
        // A resume decision under the same binding also conflicts with the fresh
        // decision already durable there.
        let resume = ContinuationDecisionRecord::new(
            &mission,
            "phase-1",
            1,
            1,
            ContinuationDecisionKind::ResumeSameProvider,
            "claude",
            None,
        )?;
        assert!(matches!(
            store.record_continuation_decision(&resume),
            Err(RuntimeStoreError::TransitionConflict)
        ));
        store.close()?;
        Ok(())
    }

    #[test]
    fn continuation_decision_replay_is_byte_identical() -> Result<(), Box<dyn std::error::Error>> {
        // Determinism: the same decision over the same inputs persists a
        // byte-identical payload, so an offline replay reproduces it exactly.
        let home = TestHome::new()?;
        let mission = cont_mission()?;
        let digest = "c".repeat(64);
        let mut store = open_private_store(home.boundary()?)?;
        store.record_continuation_decision(&fresh_decision(&mission, 1, &digest)?)?;
        let first = store.continuation_capsule_text_cells(&mission)?;
        // Replay the identical decision; the exact-retry path keeps one row.
        store.record_continuation_decision(&fresh_decision(&mission, 1, &digest)?)?;
        let second = store.continuation_capsule_text_cells(&mission)?;
        assert_eq!(first, second);
        assert_eq!(first.len(), 1);
        store.close()?;
        Ok(())
    }

    #[test]
    fn continuation_decision_key_inventory_has_no_freeform_or_secret_key()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mission = cont_mission()?;
        let mut store = open_private_store(home.boundary()?)?;
        store.record_continuation_decision(&fresh_decision(&mission, 1, &"d".repeat(64))?)?;
        let keys = store.continuation_decision_key_inventory(&mission)?;
        for forbidden in [
            "session_id",
            "raw",
            "context",
            "transcript",
            "notes",
            "summary",
            "cot",
            "chain_of_thought",
        ] {
            assert!(
                !keys.iter().any(|key| key == forbidden),
                "continuation decision payload exposes a {forbidden} key"
            );
        }
        // The decision payload text also never carries a provider session id.
        for cell in store.continuation_capsule_text_cells(&mission)? {
            assert!(!cell.contains("session_id"));
        }
        store.close()?;
        Ok(())
    }

    #[test]
    fn continuation_writes_leave_v5_schema_digest_invariant()
    -> Result<(), Box<dyn std::error::Error>> {
        // Additive-proof: continuation adds new journal *kinds* only, never a
        // table, index, or trigger — so a store carrying continuation rows has a
        // byte-identical v5 schema and needs no migration. If a future change
        // added a continuation projection table, this digest would move and the
        // proof would fail loudly.
        let home = TestHome::new()?;
        let mission = cont_mission()?;
        let handle = claude_handle("session-xyz")?;
        let mut store = open_private_store(home.boundary()?)?;
        store.record_continuation_handle(&ContinuationHandleRecord::from_handle(
            &mission, "phase-1", 1, &handle,
        )?)?;
        store.record_continuation_decision(&fresh_decision(&mission, 2, &"e".repeat(64))?)?;
        assert_eq!(
            schema_digest(store.connection()?)?,
            NATIVE_V7_SCHEMA_DIGEST,
            "continuation persistence must not alter the v5 schema"
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn terminal_decision_record_rejects_warn_shaped_verification()
    -> Result<(), Box<dyn std::error::Error>> {
        // gate_passed == false with action == Continue is the Warn shape:
        // the runner's own `validate_terminal_verification` already refuses
        // to reach this constructor with it, but the storage layer must
        // reject it independently rather than trust that every caller does.
        let warn_shaped = decide_verification(
            VerificationOutcome::Classified(VerificationClass::Fail),
            VerificationMode::Warn,
        );
        assert!(!warn_shaped.gate_passed());
        assert_eq!(warn_shaped.action(), VerificationAction::Continue);
        assert!(matches!(
            TerminalDecisionRecord::new(
                &td_mission()?,
                "phase-1",
                "worker-1",
                1,
                None,
                warn_shaped
            ),
            Err(RuntimeStoreError::InvalidIntent(_))
        ));
        Ok(())
    }

    #[test]
    fn terminal_decision_record_accepts_colon_spaces_and_unicode_worker_id()
    -> Result<(), Box<dyn std::error::Error>> {
        let worker = "data science:cafe\u{301}-phase-1";
        let record =
            TerminalDecisionRecord::new(&td_mission()?, "phase-1", worker, 1, None, td_pass())?;
        assert_eq!(
            record.to_value().get("worker_id").and_then(Value::as_str),
            Some(worker)
        );
        Ok(())
    }

    /// Directly tampers with one journal row's `payload_json`, recomputing a
    /// matching checksum so the generic checksum-chain check alone cannot
    /// catch the forgery — only whatever invariant the caller is targeting
    /// can. Mirrors [`forge_journal_extra_json`], targeting `payload_json`
    /// instead since that is where [`TerminalDecisionRecord`] content lives.
    ///
    /// Unlike [`forge_journal_extra_json`] (whose callers only assert that
    /// `RuntimeStore::open` itself rejects the forgery), this restores the
    /// exact original `journal_reject_update`/`journal_reject_delete`
    /// trigger definitions it must drop to perform the update, so `open`'s
    /// generic required-object and schema-digest checks still pass and a
    /// forged-but-openable database can reach [`RuntimeStore::terminal_decision`]'s
    /// own semantic validation instead of being rejected earlier for an
    /// unrelated, coarser reason.
    fn forge_journal_payload_json(
        home: &TestHome,
        sequence: i64,
        forged_payload: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let raw = Connection::open(home.database())?;
        let reject_update_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_update'",
            [],
            |row| row.get(0),
        )?;
        let reject_delete_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_delete'",
            [],
            |row| row.get(0),
        )?;
        raw.execute_batch(
            "DROP TRIGGER journal_reject_update;
             DROP TRIGGER journal_reject_delete;",
        )?;
        let transition_id: String = raw.query_row(
            "SELECT transition_id FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let mission_id: Option<String> = raw.query_row(
            "SELECT mission_id FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let kind: String = raw.query_row(
            "SELECT transition_kind FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let committed_at_utc: String = raw.query_row(
            "SELECT committed_at_utc FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let extra_json: String = raw.query_row(
            "SELECT extra_json FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let outbox_fingerprint: String = raw.query_row(
            "SELECT outbox_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let projection_fingerprint: String = raw.query_row(
            "SELECT projection_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let previous_checksum: String = raw.query_row(
            "SELECT previous_checksum FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let prepared = PreparedIntent {
            transition_id,
            mission_id,
            kind,
            payload_json: forged_payload.to_owned(),
            committed_at_utc,
            extra_json,
            outbox_fingerprint,
            projection_fingerprint,
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        };
        let forged_checksum = transition_checksum(
            RECORD_SCHEMA_VERSION,
            sequence,
            &previous_checksum,
            &prepared,
        );
        raw.execute(
            "UPDATE journal SET payload_json = ?1, checksum = ?2 WHERE sequence = ?3",
            params![forged_payload, forged_checksum, sequence],
        )?;
        raw.execute_batch(&format!("{reject_update_sql};\n{reject_delete_sql};"))?;
        drop(raw);
        Ok(())
    }

    /// Directly tampers with one journal row's normally-unbound `mission_id`
    /// column, recomputing a matching checksum the same way
    /// [`forge_journal_payload_json`] does (drop the anti-tamper triggers,
    /// rebuild a [`PreparedIntent`] from the row's own untouched fields plus
    /// the forged `mission_id`, recompute the checksum, restore the
    /// triggers), so the generic checksum-chain check alone cannot catch
    /// this forgery — only [`RuntimeStore::terminal_decision`]'s own
    /// `row_mission_id.is_some()` semantic check can.
    fn forge_journal_mission_id(
        home: &TestHome,
        sequence: i64,
        forged_mission_id: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let raw = Connection::open(home.database())?;
        let reject_update_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_update'",
            [],
            |row| row.get(0),
        )?;
        let reject_delete_sql: String = raw.query_row(
            "SELECT sql FROM sqlite_schema WHERE type = 'trigger' AND name = 'journal_reject_delete'",
            [],
            |row| row.get(0),
        )?;
        raw.execute_batch(
            "DROP TRIGGER journal_reject_update;
             DROP TRIGGER journal_reject_delete;",
        )?;
        let transition_id: String = raw.query_row(
            "SELECT transition_id FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let kind: String = raw.query_row(
            "SELECT transition_kind FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let payload_json: String = raw.query_row(
            "SELECT payload_json FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let committed_at_utc: String = raw.query_row(
            "SELECT committed_at_utc FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let extra_json: String = raw.query_row(
            "SELECT extra_json FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let outbox_fingerprint: String = raw.query_row(
            "SELECT outbox_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let projection_fingerprint: String = raw.query_row(
            "SELECT projection_fingerprint FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let previous_checksum: String = raw.query_row(
            "SELECT previous_checksum FROM journal WHERE sequence = ?1",
            [sequence],
            |row| row.get(0),
        )?;
        let prepared = PreparedIntent {
            transition_id,
            mission_id: Some(forged_mission_id.to_owned()),
            kind,
            payload_json,
            committed_at_utc,
            extra_json,
            outbox_fingerprint,
            projection_fingerprint,
            required_projections: Vec::new(),
            outbox: Vec::new(),
            publications: Vec::new(),
        };
        let forged_checksum = transition_checksum(
            RECORD_SCHEMA_VERSION,
            sequence,
            &previous_checksum,
            &prepared,
        );
        raw.execute(
            "UPDATE journal SET mission_id = ?1, checksum = ?2 WHERE sequence = ?3",
            params![forged_mission_id, forged_checksum, sequence],
        )?;
        raw.execute_batch(&format!("{reject_update_sql};\n{reject_delete_sql};"))?;
        drop(raw);
        Ok(())
    }

    #[test]
    fn terminal_decision_round_trips_across_close_and_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mission = td_mission()?;
        let record = td_record(td_pass(), None)?;

        let mut store = open_private_store(Arc::clone(&boundary))?;
        store.record_terminal_decision(&record)?;
        store.close()?;

        let reopened = open_private_store(boundary)?;
        let found = reopened
            .terminal_decision(&mission, "phase-1", "worker-1", 1)?
            .ok_or("persisted terminal decision did not survive reopen")?;
        assert_eq!(found, record);
        assert_eq!(found.verification(), td_pass());
        assert_eq!(found.observed_termination(), None);
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn terminal_decision_legacy_ascii_transition_id_is_unchanged()
    -> Result<(), Box<dyn std::error::Error>> {
        let mission = td_mission()?;
        assert_eq!(
            terminal_decision_transition_id(&mission, "phase-1", "worker-1", 7),
            "cell2f-terminal-decision:terminal-decision-mission:phase-1:worker-1:7"
        );
        assert_eq!(
            terminal_decision_transition_id(&mission, "phase-1", "data science-phase-1", 7),
            "cell2f-terminal-decision:terminal-decision-mission:phase-1:data science-phase-1:7"
        );
        Ok(())
    }

    #[test]
    fn terminal_decision_hashed_transition_id_is_stable_and_distinguishes_fields()
    -> Result<(), Box<dyn std::error::Error>> {
        let mission = td_mission()?;
        let baseline = terminal_decision_transition_id(&mission, "phase-1", "worker:with-colon", 7);
        assert_eq!(
            baseline,
            "cell2f-terminal-decision-v2:\
             bf65717dfaa9f7d28f44c92ef671e4936bb8a0459943077b8286464d72600e36"
        );
        assert!(baseline.is_ascii());
        assert!(baseline.len() <= MAX_IDENTIFIER_BYTES);
        assert_ne!(
            baseline,
            terminal_decision_transition_id(&mission, "phase-1", "worker:with-colon", 8)
        );
        assert_ne!(
            baseline,
            terminal_decision_transition_id(&mission, "phase-2", "worker:with-colon", 7)
        );
        assert!(
            terminal_decision_transition_id(&mission, "phase:1", "worker-1", 7)
                .starts_with("cell2f-terminal-decision-v2:")
        );
        assert_ne!(
            baseline,
            terminal_decision_transition_id(&mission, "phase-1", "worker:with:colon", 7)
        );
        let different_mission = MissionId::new("terminal-decision-mission-other")?;
        assert_ne!(
            baseline,
            terminal_decision_transition_id(&different_mission, "phase-1", "worker:with-colon", 7,)
        );
        Ok(())
    }

    #[test]
    fn terminal_decision_max_length_worker_id_persists_across_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mission = td_mission()?;
        let worker = "w".repeat(MAX_WORKER_ID_BYTES);
        let record = TerminalDecisionRecord::new(&mission, "phase-1", &worker, 1, None, td_pass())?;

        let mut store = open_private_store(Arc::clone(&boundary))?;
        store.record_terminal_decision(&record)?;
        store.close()?;

        let reopened = open_private_store(boundary)?;
        assert_eq!(
            reopened.terminal_decision(&mission, "phase-1", &worker, 1)?,
            Some(record)
        );
        reopened.close()?;
        Ok(())
    }

    #[test]
    fn terminal_decision_absent_binding_returns_none() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let store = open_private_store(home.boundary()?)?;
        assert_eq!(
            store.terminal_decision(&td_mission()?, "phase-1", "worker-1", 1)?,
            None
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn terminal_decision_exact_retry_is_idempotent() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mission = td_mission()?;
        let record = td_record(td_block(), None)?;
        let mut store = open_private_store(home.boundary()?)?;
        store.record_terminal_decision(&record)?;
        // An exact retry of the identical binding and content is idempotent,
        // not a conflict.
        store.record_terminal_decision(&record)?;
        let row_count: i64 = store.connection()?.query_row(
            "SELECT COUNT(*) FROM journal WHERE transition_kind = ?1",
            params![TERMINAL_DECISION_TRANSITION_KIND],
            |row| row.get(0),
        )?;
        assert_eq!(row_count, 1);
        assert_eq!(
            store.terminal_decision(&mission, "phase-1", "worker-1", 1)?,
            Some(record)
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn terminal_decision_conflicting_retry_under_same_binding_is_rejected()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let mission = td_mission()?;
        let original = td_record(td_block(), None)?;
        let mut store = open_private_store(home.boundary()?)?;
        store.record_terminal_decision(&original)?;

        // A later caller attempting to persist a DIFFERENT decision (PASS)
        // under the exact same mission/phase/worker/attempt binding must
        // never silently override the durable BLOCK decision.
        let overriding = td_record(td_pass(), None)?;
        assert!(matches!(
            store.record_terminal_decision(&overriding),
            Err(RuntimeStoreError::TransitionConflict)
        ));

        // The original decision remains exactly what a fresh lookup returns.
        assert_eq!(
            store.terminal_decision(&mission, "phase-1", "worker-1", 1)?,
            Some(original)
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn terminal_decision_persists_across_process_crash() -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let output = Command::new(std::env::current_exe()?)
            .arg("--exact")
            .arg("runtime_store::tests::terminal_decision_crash_helper")
            .arg("--nocapture")
            .env(CRASH_HELPER_HOME, &home.path)
            .output()?;
        assert_eq!(output.status.code(), Some(93));

        let store = open_private_store(home.boundary()?)?;
        let found = store
            .terminal_decision(&td_mission()?, "phase-1", "worker-1", 1)?
            .ok_or("terminal decision did not survive an ungraceful process exit")?;
        assert_eq!(found.verification(), td_block());
        assert_eq!(
            found.observed_termination(),
            Some(MechanicalTermination::ContractViolation)
        );
        store.close()?;
        Ok(())
    }

    #[test]
    fn terminal_decision_crash_helper() -> Result<(), Box<dyn std::error::Error>> {
        let Some(path) = std::env::var_os(CRASH_HELPER_HOME) else {
            return Ok(());
        };
        let path = fs::canonicalize(path)?;
        let boundary = Arc::new(ProductionBoundary::from_canonical_root(&path)?);
        let record = td_record(td_block(), Some(MechanicalTermination::ContractViolation))?;
        let mut store = open_private_store(boundary)?;
        store.record_terminal_decision(&record)?;
        std::process::exit(93);
    }

    #[test]
    fn terminal_decision_tamper_checksum_is_caught_generically_on_reopen()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        store.record_terminal_decision(&td_record(td_pass(), None)?)?;
        store.close()?;

        let raw = Connection::open(home.database())?;
        raw.execute_batch(
            "DROP TRIGGER journal_reject_update;
             DROP TRIGGER journal_reject_delete;",
        )?;
        raw.execute(
            "UPDATE journal SET checksum = 'deadbeef' || substr(checksum, 9)
             WHERE transition_kind = ?1",
            params![TERMINAL_DECISION_TRANSITION_KIND],
        )?;
        drop(raw);

        assert!(matches!(
            open_private_store(boundary),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn terminal_decision_forgery_matrix_is_rejected_without_relying_on_checksum_alone()
    -> Result<(), Box<dyn std::error::Error>> {
        let valid = td_record(td_pass(), None)?.to_value();

        let mut unknown_version = valid.clone();
        unknown_version["version"] = Value::from(99_u64);

        let mut wrong_type_attempt = valid.clone();
        wrong_type_attempt["attempt"] = Value::from("one");

        let mut binding_mismatch_phase = valid.clone();
        binding_mismatch_phase["phase_id"] = Value::from("phase-2");

        let mut binding_mismatch_worker = valid.clone();
        binding_mismatch_worker["worker_id"] = Value::from("a-different-worker");

        let mut binding_mismatch_attempt = valid.clone();
        binding_mismatch_attempt["attempt"] = Value::from(2_u64);

        let mut inconsistent_verification = valid.clone();
        inconsistent_verification["verification"] = serde_json::json!({
            "outcome": {"kind": "classified", "class": "fail"},
            "action": "block",
            "gate_passed": false,
            // `warning` can never be `true` alongside `action: "block"` —
            // `decide_verification` never produces that combination.
            "warning": true,
        });

        let mut unknown_termination_kind = valid.clone();
        unknown_termination_kind["observed_termination"] =
            serde_json::json!({"kind": "not_a_real_kind"});

        for (label, forged) in [
            ("unknown_version", unknown_version),
            ("wrong_type_attempt", wrong_type_attempt),
            ("binding_mismatch_phase", binding_mismatch_phase),
            ("binding_mismatch_worker", binding_mismatch_worker),
            ("binding_mismatch_attempt", binding_mismatch_attempt),
            ("inconsistent_verification", inconsistent_verification),
            ("unknown_termination_kind", unknown_termination_kind),
        ] {
            let home = TestHome::new()?;
            let boundary = home.boundary()?;
            let mut store = open_private_store(Arc::clone(&boundary))?;
            store.record_terminal_decision(&td_record(td_pass(), None)?)?;
            store.close()?;

            let forged_json = canonical_json(&forged, "test forged terminal decision")?;
            forge_journal_payload_json(&home, 1, &forged_json)?;

            assert!(
                matches!(
                    open_private_store(boundary),
                    Err(RuntimeStoreError::CorruptDatabase)
                ),
                "{label}"
            );
        }

        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        store.record_terminal_decision(&td_record(td_pass(), None)?)?;
        store.close()?;
        let forged_transition_id =
            terminal_decision_transition_id(&td_mission()?, "phase-1", "worker-1", 2);
        forge_journal_transition_identity(&home, 1, Some(&forged_transition_id), None)?;
        let raw = Connection::open(home.database())?;
        validate_checksum_chain(&raw)?;
        drop(raw);
        assert!(matches!(
            open_private_store(boundary),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn terminal_decision_forged_row_mission_id_is_rejected_as_corrupt()
    -> Result<(), Box<dyn std::error::Error>> {
        let home = TestHome::new()?;
        let boundary = home.boundary()?;
        let mut store = open_private_store(Arc::clone(&boundary))?;
        store.record_terminal_decision(&td_record(td_pass(), None)?)?;
        store.close()?;

        // The private row binds the payload mission in the journal column.
        // Forging that column to another valid mission while recomputing the
        // checksum must still fail the semantic binding check.
        forge_journal_mission_id(&home, 1, "a-different-mission")?;

        assert!(matches!(
            open_private_store(boundary),
            Err(RuntimeStoreError::CorruptDatabase)
        ));
        Ok(())
    }

    #[test]
    fn terminal_decision_mechanical_termination_round_trips_every_variant()
    -> Result<(), Box<dyn std::error::Error>> {
        let terminations = [
            None,
            Some(MechanicalTermination::Cancelled),
            Some(MechanicalTermination::HardDeadlineExceeded),
            Some(MechanicalTermination::WatchdogStalled),
            Some(MechanicalTermination::ProcessExited(
                ProcessExitStatus::code(7)?,
            )),
            Some(MechanicalTermination::ProcessExited(
                ProcessExitStatus::signal(9)?,
            )),
            Some(MechanicalTermination::ProviderStreamEnded),
            Some(MechanicalTermination::SupervisorFailure),
            Some(MechanicalTermination::EventDeliveryFailure),
            Some(MechanicalTermination::ContractViolation),
        ];
        for (index, termination) in terminations.into_iter().enumerate() {
            let home = TestHome::new()?;
            let mission = td_mission()?;
            let record = td_record(td_block(), termination)?;
            let mut store = open_private_store(home.boundary()?)?;
            store.record_terminal_decision(&record)?;
            let found = store
                .terminal_decision(&mission, "phase-1", "worker-1", 1)?
                .ok_or_else(|| format!("case {index} did not round-trip"))?;
            assert_eq!(found.observed_termination(), termination, "case {index}");
            store.close()?;
        }
        Ok(())
    }

    #[test]
    fn terminal_decision_verification_round_trips_every_outcome_and_mode()
    -> Result<(), Box<dyn std::error::Error>> {
        // Every case here must produce a *valid* (non-Warn-shaped) decision:
        // `TerminalDecisionRecord::new` now rejects `gate_passed=false` with
        // `action=Continue` outright (see
        // `terminal_decision_record_rejects_warn_shaped_verification`), so a
        // non-Pass outcome is only exercised here under `Block` mode, never
        // `Warn`. `Warn` mode itself still round-trips for the two outcomes
        // where it never produces that shape: `Pass` (always gate-passes)
        // and `Cancelled` (always `action=Cancelled`, never `Continue`).
        let cases = [
            (
                VerificationOutcome::Classified(VerificationClass::Pass),
                VerificationMode::Block,
            ),
            (
                VerificationOutcome::Classified(VerificationClass::Pass),
                VerificationMode::Warn,
            ),
            (
                VerificationOutcome::Classified(VerificationClass::Fail),
                VerificationMode::Block,
            ),
            (
                VerificationOutcome::Classified(VerificationClass::Skip),
                VerificationMode::Block,
            ),
            (
                VerificationOutcome::Classified(VerificationClass::NoTests),
                VerificationMode::Block,
            ),
            (
                VerificationOutcome::Classified(VerificationClass::Timeout),
                VerificationMode::Block,
            ),
            (
                VerificationOutcome::Classified(VerificationClass::InfrastructureError),
                VerificationMode::Block,
            ),
            (VerificationOutcome::Cancelled, VerificationMode::Block),
            (VerificationOutcome::Cancelled, VerificationMode::Warn),
        ];
        for (index, (outcome, mode)) in cases.into_iter().enumerate() {
            let home = TestHome::new()?;
            let mission = td_mission()?;
            let decision = decide_verification(outcome, mode);
            let record = td_record(decision, None)?;
            let mut store = open_private_store(home.boundary()?)?;
            store.record_terminal_decision(&record)?;
            let found = store
                .terminal_decision(&mission, "phase-1", "worker-1", 1)?
                .ok_or_else(|| format!("case {index} did not round-trip"))?;
            assert_eq!(found.verification(), decision, "case {index}");
            store.close()?;
        }
        Ok(())
    }
}
