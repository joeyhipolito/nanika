//! Fixture-bounded backend for the canonical Go event-log projection.
//!
//! This backend owns only `<fixture-root>/events/<mission-id>.jsonl`. The
//! existing workspace-local fixture projector remains a separate test surface;
//! neither backend mirrors or dual-writes the other.

use crate::{
    FreshFixtureAuthority,
    capability::{CapabilityError, SharedCapabilityRoot},
    fs_util::{
        FileIdentity, create_dir_private, create_private_file, identity, link_count, mode,
        open_dir_path_nofollow, open_file_nofollow, sync_dir,
    },
    runtime_home::ProductionBoundary,
};
use cap_primitives::fs::FollowSymlinks;
use cap_std::fs::{Dir, OpenOptions};
use orchestrator_core::{
    DecodedEvent, GO_EVENT_JSON_CONTENT_MAX_BYTES, MissionId, decode_event_line,
    encode_current_event, scan_event_log,
};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use std::{
    collections::BTreeSet,
    fmt,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};
use thiserror::Error;

const EVENTS_DIRECTORY: &str = "events";
const MAX_FIXTURE_EVENT_LOG_BYTES: usize = 8 * 1024 * 1024;
type LeaseKey = (FileIdentity, String);
static CANONICAL_EVENT_LOG_LEASES: OnceLock<Mutex<BTreeSet<LeaseKey>>> = OnceLock::new();

/// Deterministic one-shot failures for the canonical event sink.
///
/// This seam attaches to any [`CanonicalEventLog`], however it was
/// constructed — [`FreshFixtureAuthority::open_canonical_event_log`] or
/// [`CanonicalEventLog::open_production`] alike; the mutation paths it
/// interrupts are shared by both. `BeforeWrite`, `AfterPartialWrite`,
/// `AfterSyncBeforeAcknowledge`, and `ReplacePathAfterSync` interrupt the
/// incremental-append path used by [`CanonicalEventLog::append_current_json`].
/// `CrashBeforeRename` and `CrashAfterRenameBeforeVerify` interrupt the
/// atomic whole-file-replace path shared by `ReplacePathAfterSync` and
/// [`CanonicalEventLog::publish_exact_next_line`]. It exists solely for
/// crash-boundary integration tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalEventLogFault {
    /// Fail after validation but before any event byte is written.
    BeforeWrite,
    /// Write a strict prefix of the line, then lose acknowledgement.
    AfterPartialWrite {
        /// Requested prefix length; it is clamped to a strict non-empty prefix.
        bytes: usize,
    },
    /// Sync the complete line, then lose acknowledgement.
    AfterSyncBeforeAcknowledge,
    /// Atomically replace the path after syncing the retained file.
    ReplacePathAfterSync,
    /// Fail a whole-file replacement right after the private staged
    /// temporary file is written and fsynced, before it is renamed over the
    /// retained path. The retained path is untouched by this crash: a fresh
    /// open sees the exact prior bytes, and the same call retried against a
    /// fresh handle publishes normally.
    CrashBeforeRename,
    /// Fail a whole-file replacement right after the rename (and directory
    /// fsync) committed the new bytes, before the caller's own
    /// receipt-equivalent verify/readback runs. The retained path already
    /// holds the exact target bytes when this fires: a fresh open observes
    /// them directly, and replaying the same call against that fresh handle
    /// is an idempotent crossed-crash-boundary success.
    CrashAfterRenameBeforeVerify,
}

/// Failures from the isolated canonical event-log backend.
#[derive(Debug, Error)]
pub enum CanonicalEventLogError {
    /// The admitted fixture root was replaced or became unavailable.
    #[error(transparent)]
    Capability(#[from] CapabilityError),
    /// Another in-process writer already owns this mission log.
    #[error("canonical fixture event log already has an active writer")]
    WriterLeased,
    /// A capability-bounded filesystem operation failed before an append.
    #[error("canonical fixture event-log operation failed during {operation}: {source}")]
    Io {
        /// Stable operation name that does not expose a caller path.
        operation: &'static str,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The canonical events directory or log file has an unsafe filesystem shape.
    #[error("canonical fixture event-log entry has the wrong type, mode, or link topology")]
    InvalidKnownEntry,
    /// The retained file no longer contains the exact admitted byte prefix.
    #[error("canonical fixture event log no longer matches its admitted history")]
    AuthoritativePrefixChanged,
    /// The fixture log exceeds its explicit in-memory recovery bound.
    #[error("canonical fixture event log exceeds its eight-MiB recovery bound")]
    EventLogTooLarge,
    /// A record is malformed, oversized, or otherwise not one event envelope.
    #[error("canonical fixture event record is invalid")]
    InvalidEvent,
    /// An event belongs to a different mission than the log capability.
    #[error("canonical fixture event mission does not match its log")]
    MissionMismatch,
    /// A newly authored event ID does not use the committed generator shape.
    #[error("new canonical fixture event ID is not Go-generator-compatible")]
    InvalidEventId,
    /// Public event sequences must be positive.
    #[error("canonical fixture event sequence must be positive")]
    InvalidSequence,
    /// Existing history reuses one event ID.
    #[error("canonical fixture event history contains a duplicate event ID")]
    DuplicateEventId,
    /// Existing history assigns one public sequence to more than one record.
    #[error("canonical fixture event history contains a duplicate sequence")]
    DuplicateSequence,
    /// No sequence remains after `i64::MAX`.
    #[error("canonical fixture event sequence is exhausted")]
    SequenceExhausted,
    /// An append did not continue after the greatest valid sequence.
    #[error("canonical fixture event sequence {found} does not continue at {expected}")]
    SequenceMismatch {
        /// Required next public sequence.
        expected: i64,
        /// Sequence supplied by the event.
        found: i64,
    },
    /// The append may have crossed the durable boundary; reopening is required.
    #[error("canonical fixture event append may have committed during {operation}: {source}")]
    AppendIndeterminate {
        /// Stable failed operation name.
        operation: &'static str,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The append was synced, but the retained path identity no longer verifies.
    #[error("canonical fixture event append committed before its path identity changed")]
    AppendIdentityIndeterminate,
    /// A prior append was indeterminate and this handle may no longer mutate.
    #[error("canonical fixture event log requires reopen recovery after an indeterminate append")]
    RecoveryRequired,
}

/// Exclusive fixture capability for one canonical Go-compatible mission log.
///
/// Construction is available only through
/// [`FreshFixtureAuthority::open_canonical_event_log`]. Appends require the
/// exact next public sequence, write one canonical-codec JSON line, and sync
/// the file before acknowledging success. Existing valid history retains its
/// authoritative source bytes unchanged.
pub struct CanonicalEventLog {
    boundary: SharedCapabilityRoot,
    lease_key: LeaseKey,
    mission_id: MissionId,
    directory: Dir,
    directory_identity: FileIdentity,
    file_name: String,
    file: cap_std::fs::File,
    file_identity: FileIdentity,
    log_bytes: usize,
    authoritative_prefix: Vec<u8>,
    events: Vec<DecodedEvent>,
    event_ids: BTreeSet<String>,
    sequences: BTreeSet<i64>,
    next_sequence: Option<i64>,
    recovery_required: bool,
    fixture_fault: Option<CanonicalEventLogFault>,
}

impl fmt::Debug for CanonicalEventLog {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CanonicalEventLog")
            .field("kind", &"exclusive-fixture-canonical-event-log")
            .field("event_count", &self.events.len())
            .field("next_sequence", &self.next_sequence)
            .finish()
    }
}

impl Drop for CanonicalEventLog {
    fn drop(&mut self) {
        lock_unpoisoned(canonical_event_log_leases()).remove(&self.lease_key);
    }
}

impl FreshFixtureAuthority {
    /// Opens or creates the canonical log for one validated mission.
    ///
    /// The returned writer is leased exclusively within this admitted fixture
    /// root. Existing malformed/truncated, cross-mission, hard-linked, or
    /// identity-ambiguous history is rejected before any append is admitted.
    pub fn open_canonical_event_log(
        &self,
        mission_id: MissionId,
    ) -> Result<CanonicalEventLog, CanonicalEventLogError> {
        self.boundary.verify()?;
        let root_identity =
            identity(&self.boundary.directory().dir_metadata().map_err(|source| {
                CanonicalEventLogError::Io {
                    operation: "inspect canonical fixture root",
                    source,
                }
            })?);
        let lease_key = (root_identity, mission_id.as_str().to_owned());
        {
            let mut leases = lock_unpoisoned(canonical_event_log_leases());
            if !leases.insert(lease_key.clone()) {
                return Err(CanonicalEventLogError::WriterLeased);
            }
        }

        match CanonicalEventLog::open(Arc::clone(&self.boundary), lease_key.clone(), mission_id) {
            Ok(log) => Ok(log),
            Err(error) => {
                lock_unpoisoned(canonical_event_log_leases()).remove(&lease_key);
                Err(error)
            }
        }
    }
}

impl CanonicalEventLog {
    /// Opens or creates the canonical log for one validated mission beneath
    /// an enrolled production boundary — the same authorized root a
    /// production workspace projection writer uses.
    ///
    /// Uses the same in-process exclusivity discipline as
    /// [`FreshFixtureAuthority::open_canonical_event_log`]: only one
    /// [`CanonicalEventLog`] may be open for a given (root, mission) pair at
    /// a time. Existing malformed/truncated, cross-mission, hard-linked, or
    /// identity-ambiguous history is rejected before any append is admitted,
    /// exactly as it is for the fixture-bounded constructor.
    pub fn open_production(
        boundary: Arc<ProductionBoundary>,
        mission_id: MissionId,
    ) -> Result<Self, CanonicalEventLogError> {
        let shared: SharedCapabilityRoot = boundary;
        shared.verify()?;
        let root_identity = identity(&shared.directory().dir_metadata().map_err(|source| {
            CanonicalEventLogError::Io {
                operation: "inspect canonical production root",
                source,
            }
        })?);
        let lease_key = (root_identity, mission_id.as_str().to_owned());
        {
            let mut leases = lock_unpoisoned(canonical_event_log_leases());
            if !leases.insert(lease_key.clone()) {
                return Err(CanonicalEventLogError::WriterLeased);
            }
        }
        match Self::open(shared, lease_key.clone(), mission_id) {
            Ok(log) => Ok(log),
            Err(error) => {
                lock_unpoisoned(canonical_event_log_leases()).remove(&lease_key);
                Err(error)
            }
        }
    }

    fn open(
        boundary: SharedCapabilityRoot,
        lease_key: LeaseKey,
        mission_id: MissionId,
    ) -> Result<Self, CanonicalEventLogError> {
        boundary.verify()?;
        let directory = ensure_private_events_directory(&boundary)?;
        let directory_identity =
            identity(
                &directory
                    .dir_metadata()
                    .map_err(|source| CanonicalEventLogError::Io {
                        operation: "inspect canonical events directory",
                        source,
                    })?,
            );
        let file_name = format!("{}.jsonl", mission_id.as_str());
        let file_path = Path::new(&file_name);
        match create_private_file(&directory, file_path, b"", false) {
            Ok(()) => sync_dir(&directory).map_err(|source| CanonicalEventLogError::Io {
                operation: "sync canonical events directory",
                source,
            })?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(CanonicalEventLogError::Io {
                    operation: "create canonical event log",
                    source,
                });
            }
        }
        let mut file = open_read_append_nofollow(&directory, file_path).map_err(|source| {
            CanonicalEventLogError::Io {
                operation: "open canonical event log for retained read and append",
                source,
            }
        })?;
        let file_metadata = file
            .metadata()
            .map_err(|source| CanonicalEventLogError::Io {
                operation: "inspect retained canonical event log",
                source,
            })?;
        if !file_metadata.is_file()
            || mode(&file_metadata) != 0o600
            || link_count(&file_metadata) != 1
        {
            return Err(CanonicalEventLogError::InvalidKnownEntry);
        }
        let file_identity = identity(&file_metadata);
        let bytes = match read_retained_bounded(&mut file, MAX_FIXTURE_EVENT_LOG_BYTES) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Err(CanonicalEventLogError::EventLogTooLarge);
            }
            Err(source) => {
                return Err(CanonicalEventLogError::Io {
                    operation: "read canonical event history",
                    source,
                });
            }
        };
        let (events, event_ids, sequences, next_sequence) = validate_history(&mission_id, &bytes)?;
        let log_bytes = bytes.len();
        let mut log = Self {
            boundary,
            lease_key,
            mission_id,
            directory,
            directory_identity,
            file_name,
            file,
            file_identity,
            log_bytes,
            authoritative_prefix: bytes,
            events,
            event_ids,
            sequences,
            next_sequence,
            recovery_required: false,
            fixture_fault: None,
        };
        log.verify_authoritative_prefix()?;
        Ok(log)
    }

    /// Returns decoded history in exact on-disk order. Each event retains its
    /// authoritative source line and its original terminator when one exists.
    #[must_use]
    pub fn events(&self) -> &[DecodedEvent] {
        &self.events
    }

    /// Returns the required next public sequence, or `None` after exhaustion.
    #[must_use]
    pub const fn next_sequence(&self) -> Option<i64> {
        self.next_sequence
    }

    /// Stops the next fixture append once at a deterministic durability boundary.
    pub fn inject_fixture_fault_once(&mut self, fault: CanonicalEventLogFault) {
        self.fixture_fault = Some(fault);
    }

    /// Appends one canonical current event JSON object and durably syncs it.
    ///
    /// The input must exactly match [`orchestrator_core::encode_current_event`]
    /// for its decoded record. Existing history is never normalized, but new
    /// writes cannot introduce pretty, padded, CRLF, or reordered JSON. A valid
    /// final EOF record without LF is retained on open; the next append writes
    /// its missing separator together with the new record and syncs both before
    /// acknowledgement. Unknown event types and fields remain accepted through
    /// the current codec.
    pub fn append_current_json(&mut self, event_json: &[u8]) -> Result<(), CanonicalEventLogError> {
        if self.recovery_required {
            return Err(CanonicalEventLogError::RecoveryRequired);
        }
        self.verify_authoritative_prefix()?;
        if event_json.is_empty()
            || event_json.len() > GO_EVENT_JSON_CONTENT_MAX_BYTES
            || event_json.iter().any(|byte| matches!(byte, b'\n' | b'\r'))
        {
            return Err(CanonicalEventLogError::InvalidEvent);
        }
        let separator_required =
            !self.authoritative_prefix.is_empty() && !self.authoritative_prefix.ends_with(b"\n");
        let separator_bytes = if separator_required { 1 } else { 0 };
        if separator_required && self.events.is_empty() {
            return Err(CanonicalEventLogError::AuthoritativePrefixChanged);
        }
        if self
            .log_bytes
            .checked_add(event_json.len())
            .and_then(|length| length.checked_add(1 + separator_bytes))
            .is_none_or(|length| length > MAX_FIXTURE_EVENT_LOG_BYTES)
        {
            return Err(CanonicalEventLogError::EventLogTooLarge);
        }
        reject_duplicate_object_keys(event_json)
            .map_err(|_| CanonicalEventLogError::InvalidEvent)?;
        let mut event =
            decode_event_line(event_json).map_err(|_| CanonicalEventLogError::InvalidEvent)?;
        if encode_current_event(&event.record)
            .map_err(|_| CanonicalEventLogError::InvalidEvent)?
            .as_slice()
            != event_json
        {
            return Err(CanonicalEventLogError::InvalidEvent);
        }
        self.validate_append(&event)?;

        let event_offset = separator_bytes;
        let mut appended = Vec::with_capacity(event_json.len().saturating_add(1 + separator_bytes));
        if separator_required {
            appended.push(b'\n');
        }
        appended.extend_from_slice(event_json);
        appended.push(b'\n');
        let committed_length = self.log_bytes + appended.len();
        let fault = self.fixture_fault.take();
        if fault == Some(CanonicalEventLogFault::BeforeWrite) {
            return Err(CanonicalEventLogError::Io {
                operation: "append canonical event before durable write",
                source: injected_fault("fixture pre-write failure"),
            });
        }
        if let Some(CanonicalEventLogFault::AfterPartialWrite { bytes }) = fault {
            let strict_prefix = bytes.max(1).min(appended.len().saturating_sub(1));
            if let Err(source) = self.file.write_all(&appended[..strict_prefix]) {
                self.recovery_required = true;
                return Err(CanonicalEventLogError::AppendIndeterminate {
                    operation: "partially append canonical event",
                    source,
                });
            }
            self.recovery_required = true;
            return Err(CanonicalEventLogError::AppendIndeterminate {
                operation: "partially append canonical event",
                source: injected_fault("fixture partial-write acknowledgement loss"),
            });
        }
        if let Err(source) = self.file.write_all(&appended) {
            self.recovery_required = true;
            return Err(CanonicalEventLogError::AppendIndeterminate {
                operation: "append canonical event",
                source,
            });
        }
        if let Err(source) = self.file.sync_all() {
            self.recovery_required = true;
            return Err(CanonicalEventLogError::AppendIndeterminate {
                operation: "sync canonical event",
                source,
            });
        }
        if fault == Some(CanonicalEventLogFault::AfterSyncBeforeAcknowledge) {
            self.recovery_required = true;
            return Err(CanonicalEventLogError::AppendIndeterminate {
                operation: "acknowledge synced canonical event",
                source: injected_fault("fixture post-sync acknowledgement loss"),
            });
        }
        if fault == Some(CanonicalEventLogFault::ReplacePathAfterSync) {
            if let Err(source) = self.replace_path_after_sync(&appended) {
                self.recovery_required = true;
                return Err(CanonicalEventLogError::AppendIndeterminate {
                    operation: "replace canonical event path after sync",
                    source,
                });
            }
        }
        if self
            .verify_committed_append(&appended, committed_length)
            .is_err()
        {
            self.recovery_required = true;
            return Err(CanonicalEventLogError::AppendIdentityIndeterminate);
        }

        let event_line = appended.split_off(event_offset);
        if separator_required {
            let Some(previous) = self.events.last_mut() else {
                self.recovery_required = true;
                return Err(CanonicalEventLogError::AppendIdentityIndeterminate);
            };
            previous.raw_line.push(b'\n');
        }
        self.authoritative_prefix.extend_from_slice(&appended);
        self.log_bytes += appended.len();
        event.raw_line = event_line;
        self.event_ids.insert(event.record.id.clone());
        self.sequences.insert(event.record.sequence);
        self.next_sequence = event.record.sequence.checked_add(1);
        self.log_bytes += event.raw_line.len();
        self.authoritative_prefix.extend_from_slice(&event.raw_line);
        self.events.push(event);
        Ok(())
    }

    fn validate_append(&self, event: &DecodedEvent) -> Result<(), CanonicalEventLogError> {
        let record = &event.record;
        if record.mission_id != self.mission_id.as_str() {
            return Err(CanonicalEventLogError::MissionMismatch);
        }
        validate_new_event_id(&record.id)?;
        if record.sequence <= 0 {
            return Err(CanonicalEventLogError::InvalidSequence);
        }
        if self.event_ids.contains(&record.id) {
            return Err(CanonicalEventLogError::DuplicateEventId);
        }
        if self.sequences.contains(&record.sequence) {
            return Err(CanonicalEventLogError::DuplicateSequence);
        }
        let expected = self
            .next_sequence
            .ok_or(CanonicalEventLogError::SequenceExhausted)?;
        if record.sequence != expected {
            return Err(CanonicalEventLogError::SequenceMismatch {
                expected,
                found: record.sequence,
            });
        }
        Ok(())
    }

    fn verify_mapping(&self, expected_length: usize) -> Result<(), CanonicalEventLogError> {
        self.boundary.verify()?;
        let current_directory =
            open_dir_path_nofollow(self.boundary.directory(), Path::new(EVENTS_DIRECTORY))
                .map_err(|source| CanonicalEventLogError::Io {
                    operation: "reopen canonical events directory",
                    source,
                })?;
        let current_directory_metadata =
            current_directory
                .dir_metadata()
                .map_err(|source| CanonicalEventLogError::Io {
                    operation: "inspect reopened canonical events directory",
                    source,
                })?;
        let held_directory_metadata =
            self.directory
                .dir_metadata()
                .map_err(|source| CanonicalEventLogError::Io {
                    operation: "inspect held canonical events directory",
                    source,
                })?;
        if !current_directory_metadata.is_dir()
            || !held_directory_metadata.is_dir()
            || mode(&current_directory_metadata) != 0o700
            || mode(&held_directory_metadata) != 0o700
            || identity(&current_directory_metadata) != self.directory_identity
            || identity(&held_directory_metadata) != self.directory_identity
        {
            return Err(CanonicalEventLogError::InvalidKnownEntry);
        }
        let current_file = open_file_nofollow(&current_directory, Path::new(&self.file_name))
            .map_err(|source| CanonicalEventLogError::Io {
                operation: "reopen canonical event log",
                source,
            })?;
        let current_file_metadata =
            current_file
                .metadata()
                .map_err(|source| CanonicalEventLogError::Io {
                    operation: "inspect reopened canonical event log",
                    source,
                })?;
        let held_file_metadata =
            self.file
                .metadata()
                .map_err(|source| CanonicalEventLogError::Io {
                    operation: "inspect held canonical event log",
                    source,
                })?;
        let expected_length = u64::try_from(expected_length)
            .map_err(|_| CanonicalEventLogError::InvalidKnownEntry)?;
        if !current_file_metadata.is_file()
            || !held_file_metadata.is_file()
            || mode(&current_file_metadata) != 0o600
            || mode(&held_file_metadata) != 0o600
            || link_count(&current_file_metadata) != 1
            || link_count(&held_file_metadata) != 1
            || identity(&current_file_metadata) != self.file_identity
            || identity(&held_file_metadata) != self.file_identity
            || current_file_metadata.len() != expected_length
            || held_file_metadata.len() != expected_length
        {
            return Err(CanonicalEventLogError::InvalidKnownEntry);
        }
        Ok(())
    }

    fn verify_authoritative_prefix(&mut self) -> Result<(), CanonicalEventLogError> {
        self.verify_mapping(self.authoritative_prefix.len())?;
        let current = read_retained_bounded(&mut self.file, MAX_FIXTURE_EVENT_LOG_BYTES).map_err(
            |source| CanonicalEventLogError::Io {
                operation: "verify retained canonical event prefix",
                source,
            },
        )?;
        if current != self.authoritative_prefix {
            return Err(CanonicalEventLogError::AuthoritativePrefixChanged);
        }
        self.verify_mapping(self.authoritative_prefix.len())
    }

    fn verify_committed_append(
        &mut self,
        line: &[u8],
        committed_length: usize,
    ) -> Result<(), CanonicalEventLogError> {
        self.verify_mapping(committed_length)?;
        let current = read_retained_bounded(&mut self.file, MAX_FIXTURE_EVENT_LOG_BYTES).map_err(
            |source| CanonicalEventLogError::Io {
                operation: "verify committed canonical event append",
                source,
            },
        )?;
        if current.len() != committed_length
            || !current.starts_with(&self.authoritative_prefix)
            || current.get(self.authoritative_prefix.len()..) != Some(line)
        {
            return Err(CanonicalEventLogError::AuthoritativePrefixChanged);
        }
        self.verify_mapping(committed_length)
    }

    fn replace_path_after_sync(&self, line: &[u8]) -> std::io::Result<()> {
        let mut replacement = Vec::with_capacity(self.authoritative_prefix.len() + line.len());
        replacement.extend_from_slice(&self.authoritative_prefix);
        replacement.extend_from_slice(line);
        self.whole_file_replace(&replacement, "fixture-replacement")
    }

    /// Atomically replaces the whole mission log file with `bytes`: stage a
    /// private temporary file, `fsync` it, rename it over the retained path,
    /// then `fsync` the containing directory. `suffix` distinguishes the
    /// staging name per call site; it carries no other meaning.
    fn whole_file_replace(&self, bytes: &[u8], suffix: &str) -> std::io::Result<()> {
        self.whole_file_replace_with_fault(bytes, suffix, None)
    }

    /// Same as [`Self::whole_file_replace`], but honors
    /// [`CanonicalEventLogFault::CrashBeforeRename`] by returning an injected
    /// failure right after the temporary file is staged and fsynced, leaving
    /// the retained path untouched and the stray staged temporary in place —
    /// exactly what a real crash between those two syscalls would leave
    /// behind.
    fn whole_file_replace_with_fault(
        &self,
        bytes: &[u8],
        suffix: &str,
        fault: Option<CanonicalEventLogFault>,
    ) -> std::io::Result<()> {
        let temporary_name = format!(".{}.{suffix}", self.file_name);
        let temporary = Path::new(&temporary_name);
        match self.directory.remove_file(temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        if let Err(error) = create_private_file(&self.directory, temporary, bytes, false) {
            let _ = self.directory.remove_file(temporary);
            return Err(error);
        }
        if fault == Some(CanonicalEventLogFault::CrashBeforeRename) {
            return Err(injected_fault(
                "whole-file replacement pre-rename crash injection",
            ));
        }
        match self
            .directory
            .rename(temporary, &self.directory, Path::new(&self.file_name))
        {
            Ok(()) => sync_dir(&self.directory),
            Err(error) => {
                let _ = self.directory.remove_file(temporary);
                Err(error)
            }
        }
    }

    /// Reopens the retained read/append handle after a whole-file
    /// replacement swapped the directory entry to a new inode. The
    /// previously retained handle no longer names the published file, so
    /// every subsequent `verify_mapping`/readback must run against this
    /// freshly reopened one.
    fn reopen_retained_handle(&mut self) -> Result<(), CanonicalEventLogError> {
        let file_path = Path::new(&self.file_name);
        let file = open_read_append_nofollow(&self.directory, file_path).map_err(|source| {
            CanonicalEventLogError::Io {
                operation: "reopen canonical event log after whole-file replacement",
                source,
            }
        })?;
        let metadata = file
            .metadata()
            .map_err(|source| CanonicalEventLogError::Io {
                operation: "inspect reopened canonical event log",
                source,
            })?;
        if !metadata.is_file() || mode(&metadata) != 0o600 || link_count(&metadata) != 1 {
            return Err(CanonicalEventLogError::InvalidKnownEntry);
        }
        self.file = file;
        self.file_identity = identity(&metadata);
        Ok(())
    }

    /// Publishes one further canonical event line via atomic whole-file
    /// replacement, sourced from caller-supplied exact bytes rather than
    /// this handle's own incremental append history.
    ///
    /// Prefer this entry point for the hermetic production canary, where the
    /// caller derives the exact expected prior bytes and exact next line
    /// independently from journal truth on every call, including after a
    /// restart.
    ///
    /// `expected_prior` must equal this log's exact retained bytes.
    /// `next_line` is the exact bytes to append after them: normally one
    /// canonical JSON object followed by its own trailing newline, plus one
    /// leading newline first when `expected_prior` is non-empty and does not
    /// already end with one (repairing a torn tail exactly once as part of
    /// this call, never silently normalized on its own). If the retained
    /// bytes already equal `expected_prior` followed by `next_line`, this is
    /// a crossed crash boundary and succeeds without writing again
    /// (idempotent). Any other retained content — truncated, extra,
    /// reordered, linked, identity-swapped, or otherwise divergent — is
    /// rejected without ever being overwritten.
    pub fn publish_exact_next_line(
        &mut self,
        expected_prior: &[u8],
        next_line: &[u8],
    ) -> Result<VerifiedEventProjection, CanonicalEventLogError> {
        if self.recovery_required {
            return Err(CanonicalEventLogError::RecoveryRequired);
        }
        self.verify_authoritative_prefix()?;
        if next_line.is_empty() {
            return Err(CanonicalEventLogError::InvalidEvent);
        }
        let separator_required = !expected_prior.is_empty() && !expected_prior.ends_with(b"\n");
        let content = if separator_required {
            next_line
                .strip_prefix(b"\n")
                .ok_or(CanonicalEventLogError::InvalidEvent)?
        } else {
            next_line
        };
        let content = content
            .strip_suffix(b"\n")
            .ok_or(CanonicalEventLogError::InvalidEvent)?;
        if content.is_empty()
            || content.len() > GO_EVENT_JSON_CONTENT_MAX_BYTES
            || content.iter().any(|byte| matches!(byte, b'\n' | b'\r'))
        {
            return Err(CanonicalEventLogError::InvalidEvent);
        }
        reject_duplicate_object_keys(content).map_err(|_| CanonicalEventLogError::InvalidEvent)?;
        let mut event =
            decode_event_line(content).map_err(|_| CanonicalEventLogError::InvalidEvent)?;
        if encode_current_event(&event.record)
            .map_err(|_| CanonicalEventLogError::InvalidEvent)?
            .as_slice()
            != content
        {
            return Err(CanonicalEventLogError::InvalidEvent);
        }
        let event_id = event.record.id.clone();
        let event_sequence = event.record.sequence;

        let mut target = expected_prior.to_vec();
        target.extend_from_slice(next_line);
        if target.len() > MAX_FIXTURE_EVENT_LOG_BYTES {
            return Err(CanonicalEventLogError::EventLogTooLarge);
        }

        let already_published = self.authoritative_prefix == target;
        let fault = self.fixture_fault.take();
        if already_published {
            // Crossed crash boundary: a previous call's whole-file
            // replacement committed but its acknowledgement was lost. The
            // in-memory bookkeeping already reflects this event from the
            // history this handle loaded at open time, so it is left
            // untouched below; only the final readback re-verifies it.
        } else {
            if self.authoritative_prefix != expected_prior {
                return Err(CanonicalEventLogError::AuthoritativePrefixChanged);
            }
            self.validate_append(&event)?;
            if let Err(source) =
                self.whole_file_replace_with_fault(&target, "production-replacement", fault)
            {
                self.recovery_required = true;
                return Err(CanonicalEventLogError::AppendIndeterminate {
                    operation: "publish canonical event via whole-file replacement",
                    source,
                });
            }
            if fault == Some(CanonicalEventLogFault::CrashAfterRenameBeforeVerify) {
                // The rename (and directory fsync) above already committed
                // `target` to the retained path — this failure only loses
                // this handle's own acknowledgement of that fact. A fresh
                // open of the same path observes `target` directly and
                // treats a replay as the crossed-crash-boundary case above.
                self.recovery_required = true;
                return Err(CanonicalEventLogError::AppendIndeterminate {
                    operation: "acknowledge published whole-file canonical event replacement",
                    source: injected_fault(
                        "whole-file replacement post-rename acknowledgement loss",
                    ),
                });
            }
            if self.reopen_retained_handle().is_err() {
                self.recovery_required = true;
                return Err(CanonicalEventLogError::AppendIdentityIndeterminate);
            }
        }

        if self.verify_mapping(target.len()).is_err() {
            self.recovery_required = true;
            return Err(CanonicalEventLogError::AppendIdentityIndeterminate);
        }
        let readback = read_retained_bounded(&mut self.file, MAX_FIXTURE_EVENT_LOG_BYTES).map_err(
            |source| CanonicalEventLogError::Io {
                operation: "verify committed whole-file canonical event replacement",
                source,
            },
        )?;
        if readback != target {
            self.recovery_required = true;
            return Err(CanonicalEventLogError::AppendIdentityIndeterminate);
        }
        self.authoritative_prefix = target;
        self.log_bytes = self.authoritative_prefix.len();

        if !already_published {
            if separator_required {
                let Some(previous) = self.events.last_mut() else {
                    self.recovery_required = true;
                    return Err(CanonicalEventLogError::AppendIdentityIndeterminate);
                };
                previous.raw_line.push(b'\n');
            }
            let mut raw_line = content.to_vec();
            raw_line.push(b'\n');
            event.raw_line = raw_line;
            self.event_ids.insert(event_id.clone());
            self.sequences.insert(event_sequence);
            self.next_sequence = event_sequence.checked_add(1);
            self.events.push(event);
        }

        Ok(VerifiedEventProjection::new(
            self.mission_id.clone(),
            event_id,
            event_sequence,
            content.to_vec(),
        ))
    }
}

/// Opaque proof that a canonical event log now durably holds one exact
/// published line, verified through the retained no-follow capability after
/// either publishing it via whole-file replacement or observing a
/// crossed-crash-boundary replay.
///
/// Only [`CanonicalEventLog::publish_exact_next_line`] can construct one.
/// Its fields are private and it has no public constructor, so external code
/// cannot fabricate this value by assertion — it is the value from which an
/// event-log compatibility receipt may be minted:
///
/// Doctest honesty note: `VerifiedEventProjection` is re-exported from
/// `orchestrator_app`'s `lib.rs` as of Cell 2D's root integration (landed at
/// `d9a3c962`), so the `use` below resolves. The `compile_fail` failure is
/// therefore genuinely **E0451** (private field), not an unresolved import.
/// `rustdoc` only checks that a `compile_fail` block fails to compile at
/// all; it does not verify the annotated error code, so this passes either
/// way without lying about which failure it caught. If this ever regresses
/// to failing with E0432 instead (e.g. because the export was accidentally
/// removed), this doctest must be re-verified by hand; do not remove the
/// export without restoring an equivalent seal proof.
///
/// ```compile_fail,E0451
/// use orchestrator_app::VerifiedEventProjection;
/// use orchestrator_core::MissionId;
///
/// fn cannot_forge(mission_id: MissionId) -> VerifiedEventProjection {
///     VerifiedEventProjection {
///         mission_id,
///         event_id: String::new(),
///         sequence: 1,
///         line_bytes: Vec::new(),
///     }
/// }
/// ```
pub struct VerifiedEventProjection {
    mission_id: MissionId,
    event_id: String,
    sequence: i64,
    line_bytes: Vec<u8>,
}

impl fmt::Debug for VerifiedEventProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedEventProjection")
            .field("kind", &"opaque-verified-event-projection")
            .field("event_id", &self.event_id)
            .field("sequence", &self.sequence)
            .field("line_bytes", &self.line_bytes.len())
            .finish()
    }
}

impl VerifiedEventProjection {
    fn new(mission_id: MissionId, event_id: String, sequence: i64, line_bytes: Vec<u8>) -> Self {
        Self {
            mission_id,
            event_id,
            sequence,
            line_bytes,
        }
    }

    /// Returns the mission this verified event belongs to.
    #[must_use]
    pub fn mission_id(&self) -> &MissionId {
        &self.mission_id
    }

    /// Returns the published event's Go-generator-compatible ID.
    #[must_use]
    pub fn event_id(&self) -> &str {
        &self.event_id
    }

    /// Returns the published event's mission-scoped public sequence.
    #[must_use]
    pub const fn sequence(&self) -> i64 {
        self.sequence
    }

    /// Returns the exact published canonical JSON line, without its
    /// terminating newline.
    #[must_use]
    pub fn line_bytes(&self) -> &[u8] {
        &self.line_bytes
    }
}

fn ensure_private_events_directory(
    boundary: &SharedCapabilityRoot,
) -> Result<Dir, CanonicalEventLogError> {
    let name = Path::new(EVENTS_DIRECTORY);
    match create_dir_private(boundary.directory(), name) {
        Ok(()) => sync_dir(boundary.directory()).map_err(|source| CanonicalEventLogError::Io {
            operation: "sync fixture root after events directory creation",
            source,
        })?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(CanonicalEventLogError::Io {
                operation: "create canonical events directory",
                source,
            });
        }
    }
    let directory = open_dir_path_nofollow(boundary.directory(), name).map_err(|source| {
        CanonicalEventLogError::Io {
            operation: "open canonical events directory",
            source,
        }
    })?;
    let metadata = directory
        .dir_metadata()
        .map_err(|source| CanonicalEventLogError::Io {
            operation: "inspect canonical events directory mode",
            source,
        })?;
    if !metadata.is_dir() || mode(&metadata) != 0o700 {
        return Err(CanonicalEventLogError::InvalidKnownEntry);
    }
    Ok(directory)
}

fn open_read_append_nofollow(directory: &Dir, path: &Path) -> std::io::Result<cap_std::fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).append(true);
    options._cap_fs_ext_follow(FollowSymlinks::No);
    directory.open_with(path, &options)
}

fn read_retained_bounded(file: &mut cap_std::fs::File, limit: usize) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)?;
    file.seek(SeekFrom::End(0))?;
    if bytes.len() > limit {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file exceeds the canonical event-log recovery bound",
        ));
    }
    Ok(bytes)
}

fn injected_fault(detail: &'static str) -> std::io::Error {
    std::io::Error::other(detail)
}

type ValidatedHistory = (
    Vec<DecodedEvent>,
    BTreeSet<String>,
    BTreeSet<i64>,
    Option<i64>,
);

fn validate_history(
    mission_id: &MissionId,
    bytes: &[u8],
) -> Result<ValidatedHistory, CanonicalEventLogError> {
    for raw_line in bytes.split_inclusive(|byte| *byte == b'\n') {
        let content = raw_line.strip_suffix(b"\n").unwrap_or(raw_line);
        let content = content.strip_suffix(b"\r").unwrap_or(content);
        reject_duplicate_object_keys(content).map_err(|_| CanonicalEventLogError::InvalidEvent)?;
    }
    let scan = scan_event_log(bytes);
    if !scan.diagnostics.is_empty() {
        return Err(CanonicalEventLogError::InvalidEvent);
    }
    let mut event_ids = BTreeSet::new();
    let mut sequences = BTreeSet::new();
    for event in &scan.events {
        if event.record.mission_id != mission_id.as_str() {
            return Err(CanonicalEventLogError::MissionMismatch);
        }
        if event.record.sequence <= 0 {
            return Err(CanonicalEventLogError::InvalidSequence);
        }
        if !event_ids.insert(event.record.id.clone()) {
            return Err(CanonicalEventLogError::DuplicateEventId);
        }
        if !sequences.insert(event.record.sequence) {
            return Err(CanonicalEventLogError::DuplicateSequence);
        }
    }
    Ok((scan.events, event_ids, sequences, scan.next_sequence))
}

fn validate_new_event_id(value: &str) -> Result<(), CanonicalEventLogError> {
    let Some(suffix) = value.strip_prefix("evt_") else {
        return Err(CanonicalEventLogError::InvalidEventId);
    };
    let random_shape = suffix.len() == 16
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'));
    let fallback_shape = suffix
        .parse::<i64>()
        .is_ok_and(|parsed| parsed.to_string() == suffix);
    if random_shape || fallback_shape {
        Ok(())
    } else {
        Err(CanonicalEventLogError::InvalidEventId)
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn canonical_event_log_leases() -> &'static Mutex<BTreeSet<LeaseKey>> {
    CANONICAL_EVENT_LOG_LEASES.get_or_init(|| Mutex::new(BTreeSet::new()))
}

/// Recursively validates JSON without changing the source representation.
#[derive(Clone, Copy)]
struct UniqueJson;

impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueJsonVisitor)
    }
}

struct UniqueJsonVisitor;

impl<'de> Visitor<'de> for UniqueJsonVisitor {
    type Value = UniqueJson;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueJson)
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        UniqueJson::deserialize(deserializer)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element::<UniqueJson>()?.is_some() {}
        Ok(UniqueJson)
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(de::Error::custom("duplicate JSON object key"));
            }
            map.next_value::<UniqueJson>()?;
        }
        Ok(UniqueJson)
    }
}

fn reject_duplicate_object_keys(bytes: &[u8]) -> Result<(), serde_json::Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    UniqueJson::deserialize(&mut deserializer)?;
    deserializer.end()
}

/// Dedicated coverage for the production canonical-event publisher —
/// [`CanonicalEventLog::open_production`] and
/// [`CanonicalEventLog::publish_exact_next_line`] — mirroring the fixture
/// append-path integration coverage in `tests/canonical_event_log.rs`, but
/// exercising the atomic whole-file-replace path a hermetic production
/// canary uses instead.
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::{
        fixture_authority::FixtureAdmissionPolicy, fixture_authority::FreshFixtureAuthority,
        runtime_home::IsolatedFixtureRoot,
    };
    use std::{
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    static CASE: AtomicU64 = AtomicU64::new(1);

    fn private_dir(path: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }

    fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(path, bytes)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
    }

    fn admit_fixture(label: &str) -> TestResult<(PathBuf, FreshFixtureAuthority)> {
        let number = CASE.fetch_add(1, Ordering::Relaxed);
        let canonical_temp = std::fs::canonicalize(std::env::temp_dir())?;
        let parent = canonical_temp.join(format!(
            "orchestrator-rs-canonical-event-log-production-{}-{number}-{label}",
            std::process::id()
        ));
        private_dir(&parent)?;
        let isolated = IsolatedFixtureRoot::create_fresh(&parent)?;
        let checkout = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))?;
        let policy =
            FixtureAdmissionPolicy::new(parent.join("live-user"), checkout, &canonical_temp);
        let authority = FreshFixtureAuthority::admit(isolated, &policy)?;
        Ok((parent, authority))
    }

    fn production_boundary(
        authority: &FreshFixtureAuthority,
    ) -> TestResult<Arc<ProductionBoundary>> {
        Ok(Arc::new(ProductionBoundary::from_canonical_root(
            authority.boundary.canonical_path(),
        )?))
    }

    fn events_path(authority: &FreshFixtureAuthority, mission: &MissionId) -> PathBuf {
        authority
            .boundary
            .canonical_path()
            .join(EVENTS_DIRECTORY)
            .join(format!("{}.jsonl", mission.as_str()))
    }

    fn event(id: &str, event_type: &str, sequence: i64, mission_id: &str) -> Vec<u8> {
        format!(
            r#"{{"id":"{id}","type":"{event_type}","timestamp":"2026-07-15T00:00:00Z","sequence":{sequence},"mission_id":"{mission_id}"}}"#
        )
        .into_bytes()
    }

    // (a) Happy path: open production, publish one exact next line, prove the
    // byte readback is exactly prefix+line, and confirm a
    // `VerifiedEventProjection` is minted with the right identity.
    #[test]
    fn open_production_publishes_one_exact_next_line_and_mints_projection() -> TestResult {
        let (parent, authority) = admit_fixture("happy-path")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-open-production-happy")?;
        let mut log = CanonicalEventLog::open_production(Arc::clone(&boundary), mission.clone())?;
        assert!(log.events().is_empty());

        let first = event(
            "evt_0000000000000101",
            "mission.started",
            1,
            mission.as_str(),
        );
        let next_line = [first.as_slice(), b"\n"].concat();
        let projection = log.publish_exact_next_line(b"", &next_line)?;

        assert_eq!(projection.mission_id(), &mission);
        assert_eq!(projection.event_id(), "evt_0000000000000101");
        assert_eq!(projection.sequence(), 1);
        assert_eq!(projection.line_bytes(), first.as_slice());
        assert_eq!(std::fs::read(events_path(&authority, &mission))?, next_line);
        assert_eq!(log.events().len(), 1);
        assert_eq!(log.next_sequence(), Some(2));

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    // (b) Same-session idempotent retry: a second call with identical
    // (expected_prior, next_line) is an already-published success, the file
    // is unchanged, and bookkeeping is not duplicated — a subsequent publish
    // of the NEXT event still computes the right prefix.
    #[test]
    fn publish_exact_next_line_same_session_idempotent_retry_then_advances() -> TestResult {
        let (parent, authority) = admit_fixture("idempotent-retry")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-idempotent-retry")?;
        let mut log = CanonicalEventLog::open_production(Arc::clone(&boundary), mission.clone())?;
        let path = events_path(&authority, &mission);

        let first = event(
            "evt_0000000000000102",
            "mission.started",
            1,
            mission.as_str(),
        );
        let first_line = [first.as_slice(), b"\n"].concat();
        log.publish_exact_next_line(b"", &first_line)?;
        let after_first = std::fs::read(&path)?;
        assert_eq!(after_first, first_line);

        let replay = log.publish_exact_next_line(b"", &first_line)?;
        assert_eq!(replay.event_id(), "evt_0000000000000102");
        assert_eq!(std::fs::read(&path)?, after_first);
        assert_eq!(log.events().len(), 1);
        assert_eq!(log.next_sequence(), Some(2));

        let second = event("evt_0000000000000103", "phase.started", 2, mission.as_str());
        let second_line = [second.as_slice(), b"\n"].concat();
        log.publish_exact_next_line(&after_first, &second_line)?;
        assert_eq!(
            std::fs::read(&path)?,
            [after_first.as_slice(), second_line.as_slice()].concat()
        );
        assert_eq!(log.events().len(), 2);
        assert_eq!(log.next_sequence(), Some(3));

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    // (c) Crossed crash boundary: a fresh `open_production` on a file already
    // equal to `expected_prior + next_line` is an idempotent success without
    // a further write.
    #[test]
    fn open_production_crossed_crash_boundary_replay_is_idempotent_without_write() -> TestResult {
        let (parent, authority) = admit_fixture("crossed-boundary")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-crossed-boundary")?;
        drop(CanonicalEventLog::open_production(
            Arc::clone(&boundary),
            mission.clone(),
        )?);
        let path = events_path(&authority, &mission);
        let first = event(
            "evt_0000000000000104",
            "mission.started",
            1,
            mission.as_str(),
        );
        let target = [first.as_slice(), b"\n"].concat();
        write_private(&path, &target)?;

        let mut log = CanonicalEventLog::open_production(Arc::clone(&boundary), mission)?;
        assert_eq!(log.events().len(), 1);
        let projection = log.publish_exact_next_line(b"", &target)?;
        assert_eq!(projection.event_id(), "evt_0000000000000104");
        assert_eq!(std::fs::read(&path)?, target);
        assert_eq!(log.next_sequence(), Some(2));

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    // (d) Divergence matrix: a wrong caller-supplied `expected_prior`, extra
    // trailing garbage appended out-of-band, and a same-length replaced/
    // reordered record are each rejected with a typed error and never
    // written.
    #[test]
    fn publish_exact_next_line_rejects_every_divergence_shape_without_writing() -> TestResult {
        let (parent, authority) = admit_fixture("divergence-matrix")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-divergence-matrix")?;
        let mut log = CanonicalEventLog::open_production(Arc::clone(&boundary), mission.clone())?;
        let path = events_path(&authority, &mission);
        let first = event(
            "evt_0000000000000105",
            "mission.started",
            1,
            mission.as_str(),
        );
        let first_line = [first.as_slice(), b"\n"].concat();
        log.publish_exact_next_line(b"", &first_line)?;
        let committed = std::fs::read(&path)?;
        assert_eq!(committed, first_line);

        let second = event("evt_0000000000000106", "phase.started", 2, mission.as_str());
        let second_line = [second.as_slice(), b"\n"].concat();

        // Truncated: the caller's own `expected_prior` no longer matches this
        // handle's retained bytes, even though the file on disk is fine.
        let mut truncated_prior = committed[..committed.len().saturating_sub(6)].to_vec();
        truncated_prior.push(b'\n');
        assert_ne!(truncated_prior, committed);
        assert!(matches!(
            log.publish_exact_next_line(&truncated_prior, &second_line),
            Err(CanonicalEventLogError::AuthoritativePrefixChanged)
        ));
        assert_eq!(std::fs::read(&path)?, committed);

        // Extra trailing garbage appended to the file out-of-band: the file
        // no longer has the exact length this handle's retained mapping
        // expects, so the shared shape/identity/length check inside
        // `verify_authoritative_prefix` rejects it before any byte
        // comparison — a distinct typed error from the pure content
        // divergence covered above and below, still without ever writing.
        let mut with_garbage = committed.clone();
        with_garbage.extend_from_slice(b"garbage-not-a-record\n");
        write_private(&path, &with_garbage)?;
        assert!(matches!(
            log.publish_exact_next_line(&committed, &second_line),
            Err(CanonicalEventLogError::InvalidKnownEntry)
        ));
        assert_eq!(std::fs::read(&path)?, with_garbage);
        write_private(&path, &committed)?;

        // Reordered/replaced line: the on-disk record is swapped for a
        // same-length different one at the same offset (mutate one interior
        // byte, preserving length so only content — not shape — diverges).
        let marker = b"mission.started";
        let marker_start = committed
            .windows(marker.len())
            .position(|window| window == marker)
            .ok_or("event type marker is missing")?;
        let mut replaced_line = committed.clone();
        replaced_line[marker_start + marker.len() - 1] = b'D';
        assert_eq!(replaced_line.len(), committed.len());
        assert_ne!(replaced_line, committed);
        write_private(&path, &replaced_line)?;
        assert!(matches!(
            log.publish_exact_next_line(&committed, &second_line),
            Err(CanonicalEventLogError::AuthoritativePrefixChanged)
        ));
        assert_eq!(std::fs::read(&path)?, replaced_line);

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    // (e) Shape attacks through the production boundary: a pre-existing hard
    // link (nlink > 1) and a wrong mode are each rejected at open time.
    #[test]
    fn open_production_rejects_hard_linked_events_file() -> TestResult {
        let (parent, authority) = admit_fixture("hard-link")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-open-production-hardlink")?;
        drop(CanonicalEventLog::open_production(
            Arc::clone(&boundary),
            mission.clone(),
        )?);
        let path = events_path(&authority, &mission);
        let external = parent.join("external-hardlink.jsonl");
        std::fs::hard_link(&path, &external)?;

        assert!(matches!(
            CanonicalEventLog::open_production(Arc::clone(&boundary), mission),
            Err(CanonicalEventLogError::InvalidKnownEntry)
        ));
        assert!(std::fs::read(&external)?.is_empty());

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn open_production_rejects_wrong_mode_events_file() -> TestResult {
        let (parent, authority) = admit_fixture("wrong-mode")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-open-production-wrong-mode")?;
        drop(CanonicalEventLog::open_production(
            Arc::clone(&boundary),
            mission.clone(),
        )?);
        let path = events_path(&authority, &mission);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;

        assert!(matches!(
            CanonicalEventLog::open_production(Arc::clone(&boundary), mission),
            Err(CanonicalEventLogError::InvalidKnownEntry)
        ));

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    // (f) Crash injection on the whole-file-replace path, via the new
    // production-path fault variants (the existing fixture-append fault
    // hooks — `BeforeWrite`/`AfterPartialWrite`/`AfterSyncBeforeAcknowledge`/
    // `ReplacePathAfterSync` — only interrupt `append_current_json` and
    // cannot reach `publish_exact_next_line`'s whole-file-replace call).
    #[test]
    fn publish_exact_next_line_crash_before_rename_reopen_sees_prior_and_retry_succeeds()
    -> TestResult {
        let (parent, authority) = admit_fixture("crash-before-rename")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-crash-before-rename")?;
        let mut log = CanonicalEventLog::open_production(Arc::clone(&boundary), mission.clone())?;
        log.inject_fixture_fault_once(CanonicalEventLogFault::CrashBeforeRename);

        let first = event(
            "evt_0000000000000107",
            "mission.started",
            1,
            mission.as_str(),
        );
        let first_line = [first.as_slice(), b"\n"].concat();
        let path = events_path(&authority, &mission);
        assert!(matches!(
            log.publish_exact_next_line(b"", &first_line),
            Err(CanonicalEventLogError::AppendIndeterminate { .. })
        ));
        // The crash landed before the rename: the retained path is still
        // exactly the prior (empty) bytes.
        assert!(std::fs::read(&path)?.is_empty());
        assert!(matches!(
            log.publish_exact_next_line(b"", &first_line),
            Err(CanonicalEventLogError::RecoveryRequired)
        ));
        drop(log);

        let mut reopened = CanonicalEventLog::open_production(Arc::clone(&boundary), mission)?;
        assert!(reopened.events().is_empty());
        let projection = reopened.publish_exact_next_line(b"", &first_line)?;
        assert_eq!(projection.event_id(), "evt_0000000000000107");
        assert_eq!(std::fs::read(path)?, first_line);

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    #[test]
    fn publish_exact_next_line_crash_after_rename_before_verify_reopen_sees_target_and_replay_is_idempotent()
    -> TestResult {
        let (parent, authority) = admit_fixture("crash-after-rename")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-crash-after-rename")?;
        let mut log = CanonicalEventLog::open_production(Arc::clone(&boundary), mission.clone())?;
        log.inject_fixture_fault_once(CanonicalEventLogFault::CrashAfterRenameBeforeVerify);

        let first = event(
            "evt_0000000000000108",
            "mission.started",
            1,
            mission.as_str(),
        );
        let first_line = [first.as_slice(), b"\n"].concat();
        let path = events_path(&authority, &mission);
        assert!(matches!(
            log.publish_exact_next_line(b"", &first_line),
            Err(CanonicalEventLogError::AppendIndeterminate { .. })
        ));
        // The rename (and directory fsync) already committed the target
        // bytes before this handle's own acknowledgement was lost.
        assert_eq!(std::fs::read(&path)?, first_line);
        assert!(matches!(
            log.publish_exact_next_line(b"", &first_line),
            Err(CanonicalEventLogError::RecoveryRequired)
        ));
        drop(log);

        let mut reopened = CanonicalEventLog::open_production(Arc::clone(&boundary), mission)?;
        assert_eq!(reopened.events().len(), 1);
        let replay = reopened.publish_exact_next_line(b"", &first_line)?;
        assert_eq!(replay.event_id(), "evt_0000000000000108");
        assert_eq!(std::fs::read(path)?, first_line);

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }

    // (g) Separator repair: `expected_prior` retains an unterminated final
    // record (no trailing LF); the published line is correctly
    // separator-prefixed and the result is byte-exact.
    #[test]
    fn publish_exact_next_line_repairs_missing_separator_on_unterminated_prior() -> TestResult {
        let (parent, authority) = admit_fixture("separator-repair")?;
        let boundary = production_boundary(&authority)?;
        let mission = MissionId::new("mission-separator-repair")?;
        drop(CanonicalEventLog::open_production(
            Arc::clone(&boundary),
            mission.clone(),
        )?);
        let path = events_path(&authority, &mission);
        let first = event(
            "evt_0000000000000109",
            "mission.started",
            1,
            mission.as_str(),
        );
        // No trailing LF: an unterminated final record.
        write_private(&path, &first)?;

        let mut log = CanonicalEventLog::open_production(Arc::clone(&boundary), mission.clone())?;
        assert_eq!(log.events().len(), 1);
        assert_eq!(log.events()[0].raw_line, first);

        let second = event("evt_0000000000000110", "phase.started", 2, mission.as_str());
        // `expected_prior` is the unterminated record; `next_line` must carry
        // the leading separator that repairs the torn tail as part of this
        // call.
        let next_line = [b"\n".as_slice(), second.as_slice(), b"\n"].concat();
        let projection = log.publish_exact_next_line(&first, &next_line)?;
        assert_eq!(projection.event_id(), "evt_0000000000000110");
        assert_eq!(
            std::fs::read(&path)?,
            [first.as_slice(), b"\n", second.as_slice(), b"\n"].concat()
        );
        assert_eq!(log.events()[0].raw_line, [first.as_slice(), b"\n"].concat());
        assert_eq!(
            log.events()[1].raw_line,
            [second.as_slice(), b"\n"].concat()
        );

        let _ = std::fs::remove_dir_all(&parent);
        Ok(())
    }
}
