//! Capability-bounded local daemon transport.
//!
//! The service deliberately has no timer, polling thread, child runtime, or
//! JavaScript dependency. Idle workers block in kernel `accept`; connected
//! clients are bounded by fixed request, frame, and queue limits.
//!
//! Persistence is supplied by the application composition root through the
//! value-only [`CanonicalEventOwner`] port. The daemon never receives a
//! canonical-log, replay, or cursor path and never opens persistence itself.

#![cfg(unix)]

mod socket_path;
use socket_path::SocketPath;

use cap_primitives::fs::FollowSymlinks;
use cap_std::{
    ambient_authority,
    fs::{
        Dir, FileTypeExt as CapFileTypeExt, MetadataExt as CapMetadataExt,
        OpenOptions as CapOpenOptions, OpenOptionsExt as CapOpenOptionsExt,
        Permissions as CapPermissions, PermissionsExt as CapPermissionsExt,
    },
};
use orchestrator_core::{
    EventJsonMap, EventRecord, GO_EVENT_JSON_CONTENT_MAX_BYTES, decode_event_line,
    encode_current_event,
};
use orchestrator_process::{
    KernelProcessIdentity, RecordedProcessIdentityStatus, inspect_recorded_process_identity,
};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fd::AsFd,
    fs::{AtFlags, Mode, RenameFlags, chmodat, fchmod, fstat, renameat_with},
    process::{Pid, getpgid},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use signal_hook::{
    consts::signal::{SIGINT, SIGTERM},
    iterator::{Handle as SignalHandle, Signals},
};
use std::{
    collections::BTreeMap,
    fmt, fs,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream},
    os::unix::{
        fs::MetadataExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use thiserror::Error;

pub const MAX_FRAME_BYTES: usize = GO_EVENT_JSON_CONTENT_MAX_BYTES;
pub const MAX_PROJECTED_EVENT_BYTES: usize = GO_EVENT_JSON_CONTENT_MAX_BYTES;
pub const MAX_EVENT_TRANSPORT_BYTES: usize = MAX_FRAME_BYTES + MAX_PROJECTED_EVENT_BYTES;
pub const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
pub const MAX_HTTP_BODY_BYTES: usize = GO_EVENT_JSON_CONTENT_MAX_BYTES + 1;
pub const CLIENT_QUEUE_CAPACITY: usize = 16;
pub const MAX_CLIENT_QUEUED_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_GLOBAL_QUEUED_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_CONCURRENT_CLIENTS: usize = 12;
pub const MAX_REPLAY_PAGE_EVENTS: usize = 64;
pub const MAX_REPLAY_PAGE_BYTES: usize = GO_EVENT_JSON_CONTENT_MAX_BYTES;
pub const MAX_TOTAL_REPLAY_EVENTS: usize = 256;
pub const MAX_TOTAL_REPLAY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_MISSION_IDENTIFIER_BYTES: usize = 256;
// JSON can encode one identifier byte as six bytes (for example, `\u0000`).
const MAX_CANCELLATION_MESSAGE_BYTES: usize = 6 * MAX_MISSION_IDENTIFIER_BYTES + 32;

/// A deliberately conservative upper bound for daemon-owned transport data.
///
/// The bound includes four maximum event-sized allocations in every client
/// handler (ciphertext, plaintext, owner/live frame, and network projection),
/// every client's maximum replay, and the global queued-byte budget. Shared
/// `Arc` payloads make the usual case substantially smaller, but this
/// calculation does not rely on sharing: 48 MiB + 48 MiB + 8 MiB = 104 MiB.
pub const MAX_THEORETICAL_TRANSPORT_BYTES: usize = MAX_CONCURRENT_CLIENTS * MAX_FRAME_BYTES * 4
    + MAX_CONCURRENT_CLIENTS * MAX_TOTAL_REPLAY_BYTES
    + MAX_GLOBAL_QUEUED_BYTES;
const _: () = assert!(MAX_THEORETICAL_TRANSPORT_BYTES < 128 * 1024 * 1024);

const PID_FILE: &str = "daemon.pid";
const IDENTITY_FILE: &str = "daemon.identity.json";
const AUTH_FILE: &str = "daemon.auth";
const INGEST_SOCKET: &str = "daemon.sock";
const EVENTS_SOCKET: &str = "events.sock";
const PROTOCOL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("daemon root is not a private, stable directory")]
    UnsafeRoot,
    #[error("daemon entry {0} has an unsafe filesystem type")]
    UnsafeEntry(&'static str),
    #[error("daemon is already running")]
    AlreadyRunning,
    #[error("daemon configuration is unsupported: {0}")]
    UnsupportedConfiguration(&'static str),
    #[error("daemon protocol authentication failed")]
    Authentication,
    #[error("daemon subscription cursor {requested} exceeds durable high-water {high_water}")]
    CursorMismatch { requested: i64, high_water: i64 },
    #[error("daemon replay exceeds its bounded event or byte budget")]
    ReplayBudgetExceeded,
    #[error("daemon subscription was rejected by the canonical event owner")]
    EnrollmentRejected,
    #[error("daemon request or event frame is too large")]
    TooLarge,
    #[error("mission identifier is empty or invalid")]
    InvalidMissionIdentifier,
    #[error("daemon cancellation protocol is invalid")]
    CancellationProtocol,
    #[error("invalid event: {0}")]
    InvalidEvent(String),
    #[error(
        "event owner acknowledgement is indeterminate; retry the same event id (durable cursor {durable_cursor:?}): {reason}"
    )]
    EventOwnerRetrySameEventId {
        durable_cursor: Option<i64>,
        reason: &'static str,
    },
    #[error(
        "event owner permanently rejected ingress (durable cursor {durable_cursor:?}): {reason}"
    )]
    EventOwnerPermanentRejection {
        durable_cursor: Option<i64>,
        reason: &'static str,
    },
    #[error("event owner returned an invalid retry classification")]
    EventOwnerProtocol,
    #[error("daemon process identity is stale or ambiguous")]
    IdentityMismatch,
    #[error("daemon I/O failed during {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("daemon worker thread failed")]
    Thread,
}

fn io_error(operation: &'static str, source: io::Error) -> DaemonError {
    DaemonError::Io { operation, source }
}

#[derive(Clone)]
pub struct DaemonConfig {
    pub root: PathBuf,
    pub port: u16,
    pub api_key: Option<String>,
    pub cf_team: Option<String>,
    pub cf_aud: Option<String>,
    pub allowed_email: Option<String>,
}

impl fmt::Debug for DaemonConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DaemonConfig")
            .field("root", &self.root)
            .field("port", &self.port)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("cf_team", &self.cf_team)
            .field("cf_aud", &self.cf_aud.as_ref().map(|_| "[REDACTED]"))
            .field(
                "allowed_email",
                &self.allowed_email.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DurableCursor(i64);

impl DurableCursor {
    pub fn new(value: i64) -> Result<Self, CanonicalOwnerProtocolError> {
        (value > 0)
            .then_some(Self(value))
            .ok_or(CanonicalOwnerProtocolError)
    }

    #[must_use]
    pub const fn get(self) -> i64 {
        self.0
    }

    /// Empty-history sentinel. Persisted event cursors are always positive.
    #[must_use]
    pub const fn origin() -> Self {
        Self(0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Non-success variants are exercised by the cfg(test) owner.
pub enum RetryClassification {
    NoRetryRequired,
    RetrySameEventId,
    PermanentRejection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)] // Idempotent recovery is exercised by the cfg(test) owner.
pub enum CommitFreshness {
    New,
    AlreadyCommitted,
}

/// The canonical owner's durable cancellation decision for one mission.
///
/// A positive acknowledgement means only that the owner durably accepted the
/// cancellation request. It does not mean cleanup or mission termination has
/// completed.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MissionCancellationAcknowledgement {
    NewlyRequested,
    AlreadyRequested,
    UnknownMission,
    Unsupported,
    Rejected,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedEvent {
    pub cursor: DurableCursor,
    pub event_id: String,
    pub ingress_bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerCommit {
    pub event: OwnedEvent,
    pub freshness: CommitFreshness,
    pub retry: RetryClassification,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerCommitFailure {
    pub cursor: Option<DurableCursor>,
    pub retry: RetryClassification,
    pub reason: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnerReplayPage {
    pub high_water: DurableCursor,
    pub events: Vec<OwnedEvent>,
    pub has_more: bool,
}

#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("canonical event owner protocol value is invalid")]
pub struct CanonicalOwnerProtocolError;

/// Object-safe authority boundary. Transport receives only owned event values,
/// validated mission identifiers, and positive durable cursors: never a path,
/// handle, database, or mutation primitive. The application adapter remains
/// the persistence owner.
pub trait CanonicalEventOwner: Send {
    fn commit_exact(
        &mut self,
        event_id: &str,
        ingress_bytes: &[u8],
    ) -> Result<OwnerCommit, OwnerCommitFailure>;

    fn replay_page(
        &mut self,
        after: DurableCursor,
        max_events: usize,
        max_bytes: usize,
    ) -> Result<OwnerReplayPage, OwnerCommitFailure>;

    /// Requests durable cancellation for a named mission.
    ///
    /// Existing owners refuse this capability by default. Implementations own
    /// both the durable decision and any later cleanup or termination work.
    fn request_mission_cancellation(
        &mut self,
        _mission_id: &str,
    ) -> MissionCancellationAcknowledgement {
        MissionCancellationAcknowledgement::Unsupported
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct DaemonIdentity {
    pub pid: u32,
    pub process_group_id: u32,
    pub start_identity: String,
    pub port: u16,
}

#[derive(Clone)]
pub struct DaemonClient {
    root: Arc<RootCapability>,
    token: String,
    ingest_identity: FileIdentity,
    events_identity: FileIdentity,
}

pub struct DaemonSubscription {
    stream: UnixStream,
    channel: SecureChannel,
}

/// An opaque wake handle for interrupting one blocking subscription read.
///
/// This conveys no event-owner or persistence authority. It exists so a CLI
/// signal waiter can wake the healthy blocking path without a polling timeout.
pub struct DaemonSubscriptionCancel {
    stream: UnixStream,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonEvent {
    pub cursor: i64,
    pub ingress_bytes: Vec<u8>,
}

impl DaemonSubscription {
    pub fn set_read_timeout(
        &self,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), DaemonError> {
        self.stream
            .set_read_timeout(timeout)
            .map_err(|source| io_error("set subscription read timeout", source))
    }

    pub fn read_event(&mut self) -> Result<Option<DaemonEvent>, DaemonError> {
        let Some(frame) = self
            .channel
            .read_frame_blocking(&mut self.stream, MAX_FRAME_BYTES + 64)?
        else {
            return Ok(None);
        };
        let Some(separator) = frame.iter().position(|byte| *byte == b'\n') else {
            return Err(DaemonError::InvalidEvent(
                "invalid daemon event frame".into(),
            ));
        };
        let header = &frame[..separator];
        let line = &frame[separator + 1..];
        let cursor = std::str::from_utf8(header)
            .ok()
            .and_then(|value| value.strip_prefix("cursor:"))
            .map(str::trim)
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| DaemonError::InvalidEvent("invalid daemon cursor frame".into()))?;
        let ingress_bytes = line
            .strip_suffix(b"\n")
            .ok_or_else(|| DaemonError::InvalidEvent("unterminated daemon event frame".into()))?
            .to_vec();
        Ok(Some(DaemonEvent {
            cursor,
            ingress_bytes,
        }))
    }

    pub fn cancellation_handle(&self) -> Result<DaemonSubscriptionCancel, DaemonError> {
        self.stream
            .try_clone()
            .map(|stream| DaemonSubscriptionCancel { stream })
            .map_err(|source| io_error("clone subscription cancellation handle", source))
    }
}

impl DaemonSubscriptionCancel {
    pub fn cancel(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

impl DaemonClient {
    pub fn open(root: &Path) -> Result<Self, DaemonError> {
        let root = admit_existing_root(root)?;
        let token = read_private_text(&root, AUTH_FILE)?;
        let ingest_identity = socket_identity_at(&root, INGEST_SOCKET)?;
        let events_identity = socket_identity_at(&root, EVENTS_SOCKET)?;
        Ok(Self {
            root: Arc::new(root),
            token,
            ingest_identity,
            events_identity,
        })
    }

    fn connect_verified(
        &self,
        name: &'static str,
        expected: FileIdentity,
    ) -> Result<UnixStream, DaemonError> {
        verify_root_capability(&self.root)?;
        verify_socket_identity(&self.root, name, expected)?;
        let path = SocketPath::new(&self.root, name)?;
        let stream = UnixStream::connect(path.path())
            .map_err(|source| io_error("connect daemon socket", source))?;
        path.verify()?;
        // The ambient AF_UNIX API is path-based. Verify both retained directory
        // authority and the exact no-follow socket identity after connect,
        // before any credential or event bytes are written.
        verify_root_capability(&self.root)?;
        verify_socket_identity(&self.root, name, expected)?;
        Ok(stream)
    }

    fn authenticate_stream(
        &self,
        stream: &mut UnixStream,
        op: &'static str,
        cursor: i64,
        deadline: AbsoluteDeadline,
    ) -> Result<SecureChannel, DaemonError> {
        write_all_before(stream, b"hello\n", deadline)?;
        let challenge = read_bounded_line_before(stream, 160, deadline)?;
        let challenge = std::str::from_utf8(&challenge)
            .ok()
            .map(str::trim)
            .and_then(|value| value.strip_prefix("challenge:"))
            .ok_or(DaemonError::Authentication)?;
        let mut fields = challenge.split(':');
        let nonce = fields.next().filter(|value| valid_nonce(value));
        let proof = fields.next();
        if fields.next().is_some()
            || !nonce.zip(proof).is_some_and(|(nonce, proof)| {
                constant_time_eq(
                    proof.as_bytes(),
                    authentication_proof(&self.token, "server", nonce, "", 0).as_bytes(),
                )
            })
        {
            return Err(DaemonError::Authentication);
        }
        let nonce = nonce.ok_or(DaemonError::Authentication)?;
        let request = LocalRequest {
            auth: authentication_proof(&self.token, "client", nonce, op, cursor),
            op: op.to_owned(),
            cursor,
        };
        let mut encoded = serde_json::to_vec(&request).map_err(|error| {
            io_error(
                "encode authenticated daemon request",
                io::Error::new(io::ErrorKind::InvalidData, error),
            )
        })?;
        encoded.push(b'\n');
        write_all_before(stream, &encoded, deadline)?;
        Ok(SecureChannel::client(&self.token, nonce))
    }

    pub fn emit(&self, event_json: &[u8]) -> Result<(), DaemonError> {
        if event_json.len() > MAX_FRAME_BYTES {
            return Err(DaemonError::TooLarge);
        }
        let mut stream = self.connect_verified(INGEST_SOCKET, self.ingest_identity)?;
        stream
            .set_nonblocking(true)
            .map_err(|source| io_error("set ingest nonblocking", source))?;
        let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
        let mut channel = self.authenticate_stream(&mut stream, "emit", 0, deadline)?;
        channel.write_frame_before(&mut stream, event_json, deadline)?;
        let reply = channel
            .read_frame_before(&mut stream, 256, deadline)?
            .ok_or(DaemonError::Authentication)?;
        if reply == b"ok" {
            return Ok(());
        }
        let text = String::from_utf8_lossy(&reply);
        if let Some(value) = text.trim().strip_prefix("retry-same:") {
            let durable_cursor = value.parse::<i64>().ok().filter(|cursor| *cursor > 0);
            return Err(DaemonError::EventOwnerRetrySameEventId {
                durable_cursor,
                reason: "daemon reported an indeterminate owner acknowledgement",
            });
        }
        if let Some(value) = text.trim().strip_prefix("reject:") {
            let durable_cursor = value.parse::<i64>().ok().filter(|cursor| *cursor > 0);
            return Err(DaemonError::EventOwnerPermanentRejection {
                durable_cursor,
                reason: "daemon reported permanent owner rejection",
            });
        }
        if text.trim() == "too-large" {
            return Err(DaemonError::TooLarge);
        }
        Err(DaemonError::InvalidEvent(text.trim().to_owned()))
    }

    pub fn stop(&self) -> Result<bool, DaemonError> {
        let Some(identity) = read_identity(&self.root)? else {
            return Ok(false);
        };
        if !identity_matches(&identity)? {
            return Err(DaemonError::IdentityMismatch);
        }
        let mut stream = match self.connect_verified(INGEST_SOCKET, self.ingest_identity) {
            Ok(stream) => stream,
            Err(DaemonError::Io { source, .. })
                if matches!(
                    source.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Err(DaemonError::IdentityMismatch);
            }
            Err(error) => return Err(error),
        };
        stream
            .set_nonblocking(true)
            .map_err(|source| io_error("set daemon stop nonblocking", source))?;
        let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
        let mut channel = self.authenticate_stream(&mut stream, "stop", 0, deadline)?;
        let reply = channel
            .read_frame_before(&mut stream, 256, deadline)?
            .ok_or(DaemonError::Authentication)?;
        Ok(reply == b"stopping")
    }

    /// Requests cancellation through the canonical owner.
    ///
    /// `NewlyRequested` and `AlreadyRequested` mean the owner durably accepted
    /// the request; neither means mission cleanup or termination is complete.
    pub fn cancel_mission(
        &self,
        mission_id: &str,
    ) -> Result<MissionCancellationAcknowledgement, DaemonError> {
        validate_mission_identifier(mission_id)?;
        let mut stream = self.connect_verified(INGEST_SOCKET, self.ingest_identity)?;
        stream
            .set_nonblocking(true)
            .map_err(|source| io_error("set cancellation nonblocking", source))?;
        let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
        let mut channel = self.authenticate_stream(&mut stream, "cancel-mission", 0, deadline)?;
        let request = MissionCancellationRequest {
            mission_id: mission_id.to_owned(),
        };
        let encoded = serde_json::to_vec(&request).map_err(|error| {
            io_error(
                "encode cancellation request",
                io::Error::new(io::ErrorKind::InvalidData, error),
            )
        })?;
        channel.write_frame_before(&mut stream, &encoded, deadline)?;
        let reply = channel
            .read_frame_before(&mut stream, MAX_CANCELLATION_MESSAGE_BYTES, deadline)?
            .ok_or(DaemonError::Authentication)?;
        serde_json::from_slice(&reply).map_err(|_| DaemonError::CancellationProtocol)
    }

    pub fn identity(&self) -> Result<Option<DaemonIdentity>, DaemonError> {
        let identity = read_identity(&self.root)?;
        match identity {
            Some(value) if identity_matches(&value)? => Ok(Some(value)),
            Some(_) => Err(DaemonError::IdentityMismatch),
            None => Ok(None),
        }
    }

    pub fn subscribe(&self, cursor: i64) -> Result<DaemonSubscription, DaemonError> {
        let mut cursor = cursor;
        self.subscribe_with_cursor(&mut cursor)
    }

    pub fn subscribe_with_cursor(
        &self,
        cursor: &mut i64,
    ) -> Result<DaemonSubscription, DaemonError> {
        if *cursor < 0 {
            *cursor = self.prepare_subscription_cursor()?;
        }
        let mut stream = self.connect_verified(EVENTS_SOCKET, self.events_identity)?;
        stream
            .set_nonblocking(true)
            .map_err(|source| io_error("set subscribe nonblocking", source))?;
        let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
        let mut channel = self.authenticate_stream(&mut stream, "subscribe", *cursor, deadline)?;
        let acknowledgement = channel
            .read_frame_before(&mut stream, 64, deadline)?
            .ok_or(DaemonError::Authentication)?;
        let acknowledgement_text = String::from_utf8_lossy(&acknowledgement);
        if let Some(error) = subscription_protocol_error(&acknowledgement_text) {
            return Err(error);
        }
        let high_water = std::str::from_utf8(&acknowledgement)
            .ok()
            .and_then(|value| value.strip_prefix("ok:"))
            .map(str::trim)
            .and_then(|value| value.parse::<i64>().ok())
            .ok_or(DaemonError::Authentication)?;
        if high_water < *cursor {
            return Err(DaemonError::EnrollmentRejected);
        }
        stream
            .set_nonblocking(false)
            .map_err(|source| io_error("restore blocking subscription", source))?;
        Ok(DaemonSubscription { stream, channel })
    }

    /// Establishes an initial durable baseline without enrolling a live
    /// subscriber. The baseline is only returned after its acknowledgement is
    /// received, so losing this acknowledgement cannot strand an enrolled
    /// stream at an unrecorded cursor. A subsequent subscription replays every
    /// event committed after the acknowledged baseline.
    fn prepare_subscription_cursor(&self) -> Result<i64, DaemonError> {
        let mut stream = self.connect_verified(EVENTS_SOCKET, self.events_identity)?;
        stream
            .set_nonblocking(true)
            .map_err(|source| io_error("set prepare-subscribe nonblocking", source))?;
        let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
        let mut channel =
            self.authenticate_stream(&mut stream, "prepare-subscribe", 0, deadline)?;
        let acknowledgement = channel
            .read_frame_before(&mut stream, 64, deadline)?
            .ok_or(DaemonError::Authentication)?;
        let acknowledgement_text = String::from_utf8_lossy(&acknowledgement);
        if let Some(error) = subscription_protocol_error(&acknowledgement_text) {
            return Err(error);
        }
        acknowledgement_text
            .strip_prefix("high-water:")
            .map(str::trim)
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value >= 0)
            .ok_or(DaemonError::Authentication)
    }
}

fn subscription_protocol_error(value: &str) -> Option<DaemonError> {
    let value = value.trim();
    if let Some(values) = value.strip_prefix("error:cursor-mismatch:") {
        let mut values = values.split(':');
        let requested = values.next().and_then(|value| value.parse().ok());
        let high_water = values.next().and_then(|value| value.parse().ok());
        if let (Some(requested), Some(high_water)) = (requested, high_water) {
            return Some(DaemonError::CursorMismatch {
                requested,
                high_water,
            });
        }
    }
    match value {
        "error:replay-budget" => Some(DaemonError::ReplayBudgetExceeded),
        "error:enrollment-rejected" => Some(DaemonError::EnrollmentRejected),
        _ => None,
    }
}

/// Returns the verified live identity, `None` when no daemon metadata exists,
/// and fails closed when a PID has been reused or metadata is ambiguous.
pub fn daemon_status(root: &Path) -> Result<Option<DaemonIdentity>, DaemonError> {
    let root = match admit_existing_root(root) {
        Ok(root) => root,
        Err(DaemonError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    match read_identity(&root)? {
        Some(identity) if identity_matches(&identity)? => Ok(Some(identity)),
        Some(_) => Err(DaemonError::IdentityMismatch),
        None if root.path.join(PID_FILE).exists() => Err(DaemonError::IdentityMismatch),
        None => Ok(None),
    }
}

/// Requests authenticated graceful shutdown. Missing metadata is an
/// idempotent successful no-op; stale or reused process identity fails closed.
pub fn stop_daemon(root: &Path) -> Result<bool, DaemonError> {
    if daemon_status(root)?.is_none() {
        return Ok(false);
    }
    if !DaemonClient::open(root)?.stop()? {
        return Ok(false);
    }
    let deadline = Instant::now()
        .checked_add(PROTOCOL_TIMEOUT)
        .ok_or_else(deadline_elapsed)?;
    loop {
        match daemon_status(root) {
            Ok(None) => return Ok(true),
            Ok(Some(_)) => {
                let now = Instant::now();
                if now >= deadline {
                    break;
                }
                std::thread::park_timeout(
                    deadline
                        .saturating_duration_since(now)
                        .min(Duration::from_millis(20)),
                );
            }
            Err(DaemonError::Io { source, .. })
                if matches!(
                    source.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                return Ok(true);
            }
            Err(error) => return Err(error),
        }
    }
    Err(DaemonError::IdentityMismatch)
}

pub struct Daemon {
    root: RootCapability,
    address: SocketAddr,
    token: String,
    state: Arc<State>,
    threads: Vec<JoinHandle<()>>,
    artifacts: Vec<ManagedArtifact>,
    signal_handle: SignalHandle,
    signal_thread: Option<JoinHandle<()>>,
}

impl Daemon {
    pub fn start(
        config: DaemonConfig,
        owner: Box<dyn CanonicalEventOwner>,
    ) -> Result<Self, DaemonError> {
        validate_http_configuration(&config)?;
        Self::start_with_enrolled_owner(config, owner, StartupFault::None)
    }

    fn start_with_enrolled_owner(
        config: DaemonConfig,
        owner: Box<dyn CanonicalEventOwner>,
        startup_fault: StartupFault,
    ) -> Result<Self, DaemonError> {
        Self::start_with_recovery_mode(config, owner, startup_fault, false)
    }

    /// Starts an in-process service whose owner retains an exclusive runtime
    /// writer lease until this daemon and all its handlers have been dropped.
    ///
    /// The caller must dedicate this control root to that leased runtime.
    /// Recovery may discard a dead embedded owner's transport even when its
    /// former parent process group remains alive. It never signals that group
    /// or establishes cleanup of any application-owned child processes.
    pub fn start_embedded_with_exclusive_owner(
        config: DaemonConfig,
        owner: Box<dyn CanonicalEventOwner>,
    ) -> Result<Self, DaemonError> {
        validate_http_configuration(&config)?;
        Self::start_with_recovery_mode(config, owner, StartupFault::None, true)
    }

    fn start_with_recovery_mode(
        config: DaemonConfig,
        owner: Box<dyn CanonicalEventOwner>,
        startup_fault: StartupFault,
        exclusive_owner: bool,
    ) -> Result<Self, DaemonError> {
        #[cfg(not(test))]
        let _ = startup_fault;
        let root = admit_root(&config.root)?;
        reject_live_duplicate(&root, exclusive_owner)?;
        remove_stale_socket(&root, INGEST_SOCKET)?;
        remove_stale_socket(&root, EVENTS_SOCKET)?;
        let mut startup_cleanup = StartupCleanup {
            root: root.path.clone(),
            directory: root
                .directory
                .try_clone()
                .map_err(|source| io_error("retain startup cleanup root", source))?,
            root_identity: root.identity,
            artifacts: Vec::new(),
            armed: true,
        };
        let (ingest, ingest_identity) = bind_managed_socket(&root, INGEST_SOCKET)?;
        startup_cleanup.capture_expected(INGEST_SOCKET, ingest_identity);
        let (events, events_identity) = bind_managed_socket(&root, EVENTS_SOCKET)?;
        startup_cleanup.capture_expected(EVENTS_SOCKET, events_identity);

        let http = TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            config.port,
        ))
        .map_err(|source| io_error("bind loopback HTTP", source))?;
        let address = http
            .local_addr()
            .map_err(|source| io_error("inspect HTTP address", source))?;
        let token = match config.api_key.filter(|value| !value.is_empty()) {
            Some(value) => value,
            None => generate_token()?,
        };
        let http_mode = HttpMode::LocalKey(token.as_bytes().to_vec());
        let auth_identity = write_private_at(&root, AUTH_FILE, token.as_bytes())?;
        startup_cleanup.capture_expected(AUTH_FILE, auth_identity);
        let kernel_identity = current_kernel_identity(std::process::id())?;
        let identity = DaemonIdentity {
            pid: std::process::id(),
            process_group_id: kernel_identity.process_group_id(),
            start_identity: kernel_identity.process_start_identity().to_owned(),
            port: address.port(),
        };
        let pid_identity = write_private_at(&root, PID_FILE, identity.pid.to_string().as_bytes())?;
        startup_cleanup.capture_expected(PID_FILE, pid_identity);
        let identity_json = serde_json::to_vec(&identity).map_err(|error| {
            io_error(
                "encode daemon identity",
                io::Error::new(io::ErrorKind::InvalidData, error),
            )
        })?;
        let daemon_identity = write_private_at(&root, IDENTITY_FILE, &identity_json)?;
        startup_cleanup.capture_expected(IDENTITY_FILE, daemon_identity);

        let (ingest_cancel, ingest_cancel_waiter) = UnixStream::pair()
            .map_err(|source| io_error("create ingest listener cancellation", source))?;
        let (events_cancel, events_cancel_waiter) = UnixStream::pair()
            .map_err(|source| io_error("create events listener cancellation", source))?;
        let (http_cancel, http_cancel_waiter) = UnixStream::pair()
            .map_err(|source| io_error("create HTTP listener cancellation", source))?;

        let state = Arc::new(State {
            root_path: root.path.clone(),
            root_directory: root
                .directory
                .try_clone()
                .map_err(|source| io_error("retain daemon state root", source))?,
            root_identity: root.identity,
            token: token.clone(),
            http_mode,
            stopping: AtomicBool::new(false),
            stream_wakeups: AtomicUsize::new(0),
            owner: Mutex::new(owner),
            subscribers: Mutex::new(Vec::new()),
            global_queued_bytes: Arc::new(AtomicUsize::new(0)),
            listener_cancels: vec![ingest_cancel, events_cancel, http_cancel],
            clients: Mutex::new(ClientRegistry::default()),
        });
        let mut signals = Signals::new([SIGINT, SIGTERM])
            .map_err(|source| io_error("install daemon signal handlers", source))?;
        let signal_handle = signals.handle();
        let mut threads = Vec::with_capacity(3);
        match spawn_ingest(ingest, Arc::clone(&state), ingest_cancel_waiter) {
            Ok(handle) => threads.push(handle),
            Err(error) => return Err(error),
        }
        match spawn_events(events, Arc::clone(&state), events_cancel_waiter) {
            Ok(handle) => threads.push(handle),
            Err(error) => {
                abort_startup(&state, &root.path, address, &mut threads);
                return Err(error);
            }
        }
        match spawn_http(http, Arc::clone(&state), http_cancel_waiter) {
            Ok(handle) => threads.push(handle),
            Err(error) => {
                abort_startup(&state, &root.path, address, &mut threads);
                return Err(error);
            }
        }
        #[cfg(test)]
        if let StartupFault::PauseAndFailAfterListeners { entered, release } = startup_fault {
            let _ = entered.send(());
            let _ = release.recv();
            abort_startup(&state, &root.path, address, &mut threads);
            return Err(DaemonError::Thread);
        }
        let signal_state = Arc::clone(&state);
        let signal_root = root.path.clone();
        let signal_thread = match thread::Builder::new()
            .name("orchestrator-daemon-signals".into())
            .spawn(move || {
                if signals.forever().next().is_some() {
                    request_shutdown(&signal_state);
                    wake_all(&signal_root, address);
                }
            }) {
            Ok(handle) => handle,
            Err(source) => {
                abort_startup(&state, &root.path, address, &mut threads);
                return Err(io_error("spawn signal listener", source));
            }
        };
        let artifacts = startup_cleanup.artifacts.clone();
        startup_cleanup.armed = false;
        Ok(Self {
            root,
            address,
            token,
            state,
            threads,
            artifacts,
            signal_handle,
            signal_thread: Some(signal_thread),
        })
    }

    #[must_use]
    pub const fn address(&self) -> SocketAddr {
        self.address
    }

    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn wait(mut self) -> Result<(), DaemonError> {
        let mut failed = self.join_listeners().is_err();
        failed |= self.join_clients().is_err();
        self.signal_handle.close();
        if let Some(thread) = self.signal_thread.take() {
            failed |= thread.join().is_err();
        }
        self.cleanup();
        if failed {
            Err(DaemonError::Thread)
        } else {
            Ok(())
        }
    }

    pub fn shutdown(mut self) -> Result<(), DaemonError> {
        request_shutdown(&self.state);
        self.signal_handle.close();
        wake_all(&self.root.path, self.address);
        let mut failed = self.join_listeners().is_err();
        failed |= self.join_clients().is_err();
        if let Some(thread) = self.signal_thread.take() {
            failed |= thread.join().is_err();
        }
        self.cleanup();
        if failed {
            Err(DaemonError::Thread)
        } else {
            Ok(())
        }
    }

    fn cleanup(&self) {
        cleanup_managed_artifacts(
            &self.root.path,
            &self.root.directory,
            self.root.identity,
            &self.artifacts,
        );
    }

    fn join_listeners(&mut self) -> Result<(), DaemonError> {
        let mut failed = false;
        for thread in self.threads.drain(..) {
            failed |= thread.join().is_err();
        }
        if failed {
            Err(DaemonError::Thread)
        } else {
            Ok(())
        }
    }

    fn join_clients(&self) -> Result<(), DaemonError> {
        let failed = join_tracked_clients(&self.state);
        if failed {
            Err(DaemonError::Thread)
        } else {
            Ok(())
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        request_shutdown(&self.state);
        self.signal_handle.close();
        wake_all(&self.root.path, self.address);
        for thread in self.threads.drain(..) {
            let _ = thread.join();
        }
        let _ = self.join_clients();
        if let Some(thread) = self.signal_thread.take() {
            let _ = thread.join();
        }
        self.cleanup();
    }
}

struct RootCapability {
    path: PathBuf,
    directory: Dir,
    identity: (u64, u64),
}

struct StartupCleanup {
    root: PathBuf,
    directory: Dir,
    root_identity: (u64, u64),
    artifacts: Vec<ManagedArtifact>,
    armed: bool,
}

impl StartupCleanup {
    fn capture_expected(&mut self, name: &'static str, identity: FileIdentity) {
        self.artifacts.push(ManagedArtifact { name, identity });
    }
}

impl Drop for StartupCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        cleanup_managed_artifacts(
            &self.root,
            &self.directory,
            self.root_identity,
            &self.artifacts,
        );
    }
}

struct State {
    root_path: PathBuf,
    root_directory: Dir,
    root_identity: (u64, u64),
    token: String,
    http_mode: HttpMode,
    stopping: AtomicBool,
    stream_wakeups: AtomicUsize,
    owner: Mutex<Box<dyn CanonicalEventOwner>>,
    subscribers: Mutex<Vec<Subscriber>>,
    global_queued_bytes: Arc<AtomicUsize>,
    listener_cancels: Vec<UnixStream>,
    clients: Mutex<ClientRegistry>,
}

struct Subscriber {
    sender: SyncSender<QueuedEvent>,
    queued_bytes: Arc<AtomicUsize>,
    global_queued_bytes: Arc<AtomicUsize>,
}

struct QueuedEvent {
    event: OwnedEvent,
    network: Option<Arc<EventLine>>,
    queued_bytes: Arc<AtomicUsize>,
    global_queued_bytes: Arc<AtomicUsize>,
    byte_len: usize,
}

impl Drop for QueuedEvent {
    fn drop(&mut self) {
        self.queued_bytes.fetch_sub(self.byte_len, Ordering::AcqRel);
        self.global_queued_bytes
            .fetch_sub(self.byte_len, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn of_cap(metadata: &cap_std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    fn of_fd(fd: &impl AsFd) -> Result<Self, DaemonError> {
        let metadata = fstat(fd)
            .map_err(|source| io_error("capture daemon artifact identity", source.into()))?;
        Ok(Self {
            device: metadata.st_dev as u64,
            inode: metadata.st_ino,
        })
    }
}

#[derive(Clone)]
struct ManagedArtifact {
    name: &'static str,
    identity: FileIdentity,
}

fn cleanup_managed_artifacts(
    root: &Path,
    directory: &Dir,
    root_identity: (u64, u64),
    artifacts: &[ManagedArtifact],
) {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return;
    };
    let Ok(retained_metadata) = directory.dir_metadata() else {
        return;
    };
    if !metadata.is_dir()
        || (metadata.dev(), metadata.ino()) != root_identity
        || (retained_metadata.dev(), retained_metadata.ino()) != root_identity
    {
        return;
    }
    for artifact in artifacts {
        let quarantine = format!(
            ".daemon-cleanup-{}-{}-{}",
            std::process::id(),
            artifact.identity.device,
            artifact.identity.inode
        );
        if renameat_with(
            directory,
            artifact.name,
            directory,
            quarantine.as_str(),
            RenameFlags::NOREPLACE,
        )
        .is_err()
        {
            continue;
        }
        if directory
            .symlink_metadata(&quarantine)
            .is_ok_and(|metadata| FileIdentity::of_cap(&metadata) == artifact.identity)
        {
            let _ = directory.remove_file(&quarantine);
        } else {
            let _ = renameat_with(
                directory,
                quarantine.as_str(),
                directory,
                artifact.name,
                RenameFlags::NOREPLACE,
            );
        }
    }
}

enum StartupFault {
    None,
    #[cfg(test)]
    PauseAndFailAfterListeners {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    },
}

#[derive(Clone)]
struct EventLine {
    cursor: DurableCursor,
    event_type: String,
    bytes: Arc<[u8]>,
}

impl TryFrom<&OwnedEvent> for EventLine {
    type Error = DaemonError;

    fn try_from(event: &OwnedEvent) -> Result<Self, Self::Error> {
        let record = decode_event_line(&event.ingress_bytes)
            .map_err(|error| DaemonError::InvalidEvent(error.to_string()))?
            .record;
        let network_record = sanitize_event_for_network(&record);
        let event_type = validate_sse_field("event", &network_record.event_type)?;
        let id = event.cursor.0.to_string();
        let _ = validate_sse_field("id", &id)?;
        let bytes = encode_current_event(&network_record)
            .map_err(|error| DaemonError::InvalidEvent(error.to_string()))?;
        if bytes.len() > MAX_PROJECTED_EVENT_BYTES {
            return Err(DaemonError::TooLarge);
        }
        Ok(Self {
            cursor: event.cursor,
            event_type,
            bytes: Arc::from(bytes),
        })
    }
}

#[derive(Clone)]
enum HttpMode {
    LocalKey(Vec<u8>),
}

fn configured(value: &Option<String>) -> bool {
    value.as_ref().is_some_and(|value| !value.is_empty())
}

fn validate_http_configuration(config: &DaemonConfig) -> Result<(), DaemonError> {
    if configured(&config.cf_team)
        || configured(&config.cf_aud)
        || configured(&config.allowed_email)
    {
        return Err(DaemonError::UnsupportedConfiguration(
            "Cloudflare Access authentication is not enrolled in the Rust daemon",
        ));
    }
    Ok(())
}

enum ClientCancel {
    Unix(UnixStream),
    Tcp(TcpStream),
}

impl ClientCancel {
    fn cancel(&self) {
        use std::net::Shutdown;

        match self {
            Self::Unix(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
            Self::Tcp(stream) => {
                let _ = stream.shutdown(Shutdown::Both);
            }
        }
    }
}

#[derive(Default)]
struct ClientRegistry {
    admission_closed: bool,
    next_id: u64,
    active: BTreeMap<u64, ClientCancel>,
    handles: Vec<JoinHandle<()>>,
    thread_failed: bool,
}

struct ClientPermit {
    state: Arc<State>,
    id: u64,
}

impl Drop for ClientPermit {
    fn drop(&mut self) {
        lock(&self.state.clients).active.remove(&self.id);
    }
}

fn admit_client(state: &Arc<State>, cancel: ClientCancel) -> Option<ClientPermit> {
    let mut clients = lock(&state.clients);
    reap_finished_clients(&mut clients);
    if clients.admission_closed
        || state.stopping.load(Ordering::Acquire)
        || clients.active.len() == MAX_CONCURRENT_CLIENTS
    {
        cancel.cancel();
        return None;
    }
    let id = clients.next_id;
    clients.next_id = clients.next_id.wrapping_add(1);
    clients.active.insert(id, cancel);
    Some(ClientPermit {
        state: Arc::clone(state),
        id,
    })
}

fn reap_finished_clients(clients: &mut ClientRegistry) {
    let mut running = Vec::with_capacity(clients.handles.len());
    for handle in std::mem::take(&mut clients.handles) {
        if handle.is_finished() {
            clients.thread_failed |= handle.join().is_err();
        } else {
            running.push(handle);
        }
    }
    clients.handles = running;
}

fn track_client_handle(state: &State, handle: JoinHandle<()>) {
    lock(&state.clients).handles.push(handle);
}

fn join_tracked_clients(state: &State) -> bool {
    let (handles, mut failed) = {
        let mut clients = lock(&state.clients);
        (
            std::mem::take(&mut clients.handles),
            std::mem::take(&mut clients.thread_failed),
        )
    };
    for handle in handles {
        failed |= handle.join().is_err();
    }
    failed
}

fn spawn_unix_client(
    stream: UnixStream,
    state: &Arc<State>,
    name: &'static str,
    handler: fn(UnixStream, &State),
) {
    let Ok(cancel) = stream.try_clone() else {
        return;
    };
    let Some(permit) = admit_client(state, ClientCancel::Unix(cancel)) else {
        return;
    };
    let thread_state = Arc::clone(state);
    match thread::Builder::new().name(name.into()).spawn(move || {
        handler(stream, &thread_state);
        drop(permit);
    }) {
        Ok(handle) => track_client_handle(state, handle),
        Err(_) => {
            // `permit` is dropped by the failed spawn closure and removes the
            // cancellation handle from the registry.
        }
    }
}

fn spawn_tcp_client(
    stream: TcpStream,
    state: &Arc<State>,
    name: &'static str,
    handler: fn(TcpStream, &State),
) {
    let Ok(cancel) = stream.try_clone() else {
        return;
    };
    let Some(permit) = admit_client(state, ClientCancel::Tcp(cancel)) else {
        return;
    };
    let thread_state = Arc::clone(state);
    if let Ok(handle) = thread::Builder::new().name(name.into()).spawn(move || {
        handler(stream, &thread_state);
        drop(permit);
    }) {
        track_client_handle(state, handle);
    }
}

fn listener_ready(listener: &impl AsFd, cancel: &UnixStream) -> bool {
    let mut descriptors = [
        PollFd::new(listener, PollFlags::IN),
        PollFd::new(cancel, PollFlags::IN),
    ];
    match poll(&mut descriptors, None) {
        Ok(_) => {
            let cancelled = descriptors[1]
                .revents()
                .intersects(PollFlags::IN | PollFlags::HUP | PollFlags::ERR);
            !cancelled && descriptors[0].revents().intersects(PollFlags::IN)
        }
        Err(_) => false,
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LocalRequest {
    auth: String,
    #[serde(default)]
    op: String,
    #[serde(default)]
    cursor: i64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct MissionCancellationRequest {
    mission_id: String,
}

fn validate_mission_identifier(mission_id: &str) -> Result<(), DaemonError> {
    if mission_id.is_empty() {
        return Err(DaemonError::InvalidMissionIdentifier);
    }
    if mission_id.len() > MAX_MISSION_IDENTIFIER_BYTES {
        return Err(DaemonError::TooLarge);
    }
    Ok(())
}

fn valid_nonce(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn authentication_proof(token: &str, domain: &str, nonce: &str, op: &str, cursor: i64) -> String {
    let mut transcript = Vec::with_capacity(128);
    transcript.extend_from_slice(b"orchestrator-daemon-local-auth-v1\0");
    transcript.extend_from_slice(domain.as_bytes());
    transcript.push(0);
    transcript.extend_from_slice(nonce.as_bytes());
    transcript.push(0);
    transcript.extend_from_slice(op.as_bytes());
    transcript.push(0);
    transcript.extend_from_slice(&cursor.to_be_bytes());
    hmac_sha256(token.as_bytes(), &transcript)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    const BLOCK_BYTES: usize = 64;
    let mut normalized = [0u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        normalized[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        normalized[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36u8; BLOCK_BYTES];
    let mut outer_pad = [0x5cu8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= normalized[index];
        outer_pad[index] ^= normalized[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner);
    outer.finalize().into()
}

struct SecureChannel {
    key: [u8; 32],
    send_domain: &'static [u8],
    receive_domain: &'static [u8],
    send_counter: u64,
    receive_counter: u64,
}

impl SecureChannel {
    fn client(token: &str, nonce: &str) -> Self {
        Self::new(token, nonce, b"client-to-server", b"server-to-client")
    }

    fn server(token: &str, nonce: &str) -> Self {
        Self::new(token, nonce, b"server-to-client", b"client-to-server")
    }

    fn new(
        token: &str,
        nonce: &str,
        send_domain: &'static [u8],
        receive_domain: &'static [u8],
    ) -> Self {
        let mut transcript = Vec::with_capacity(96);
        transcript.extend_from_slice(b"orchestrator-daemon-session-v1\0");
        transcript.extend_from_slice(nonce.as_bytes());
        Self {
            key: hmac_sha256(token.as_bytes(), &transcript),
            send_domain,
            receive_domain,
            send_counter: 0,
            receive_counter: 0,
        }
    }

    fn write_frame_before(
        &mut self,
        stream: &mut UnixStream,
        plaintext: &[u8],
        deadline: AbsoluteDeadline,
    ) -> Result<(), DaemonError> {
        let (counter, ciphertext, tag) = self.seal(plaintext)?;
        let header = format!(
            "secure:{counter}:{}:{}\n",
            ciphertext.len(),
            hex_bytes(&tag)
        );
        write_all_before(stream, header.as_bytes(), deadline)?;
        write_all_before(stream, &ciphertext, deadline)
    }

    fn read_frame_before(
        &mut self,
        stream: &mut UnixStream,
        maximum: usize,
        deadline: AbsoluteDeadline,
    ) -> Result<Option<Vec<u8>>, DaemonError> {
        let header = read_bounded_line_before(stream, 160, deadline)?;
        if header.is_empty() {
            return Ok(None);
        }
        let (counter, length, tag) = parse_secure_header(&header, maximum)?;
        let mut ciphertext = vec![0u8; length];
        read_exact_before(stream, &mut ciphertext, deadline)?;
        self.open(counter, &ciphertext, &tag).map(Some)
    }

    fn read_frame_blocking(
        &mut self,
        stream: &mut UnixStream,
        maximum: usize,
    ) -> Result<Option<Vec<u8>>, DaemonError> {
        let header = read_bounded_line(stream, 160)?;
        if header.is_empty() {
            return Ok(None);
        }
        let (counter, length, tag) = parse_secure_header(&header, maximum)?;
        let mut ciphertext = vec![0u8; length];
        stream
            .read_exact(&mut ciphertext)
            .map_err(|source| io_error("read secure daemon frame", source))?;
        self.open(counter, &ciphertext, &tag).map(Some)
    }

    fn seal(&mut self, plaintext: &[u8]) -> Result<(u64, Vec<u8>, [u8; 32]), DaemonError> {
        let counter = self.send_counter;
        self.send_counter = self
            .send_counter
            .checked_add(1)
            .ok_or(DaemonError::Authentication)?;
        let ciphertext = crypt_frame(&self.key, self.send_domain, counter, plaintext);
        let tag = frame_tag(&self.key, self.send_domain, counter, &ciphertext);
        Ok((counter, ciphertext, tag))
    }

    fn open(
        &mut self,
        counter: u64,
        ciphertext: &[u8],
        tag: &[u8; 32],
    ) -> Result<Vec<u8>, DaemonError> {
        if counter != self.receive_counter {
            return Err(DaemonError::Authentication);
        }
        let expected = frame_tag(&self.key, self.receive_domain, counter, ciphertext);
        if !constant_time_eq(tag, &expected) {
            return Err(DaemonError::Authentication);
        }
        self.receive_counter = self
            .receive_counter
            .checked_add(1)
            .ok_or(DaemonError::Authentication)?;
        Ok(crypt_frame(
            &self.key,
            self.receive_domain,
            counter,
            ciphertext,
        ))
    }
}

fn crypt_frame(key: &[u8; 32], domain: &[u8], counter: u64, input: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(input.len());
    for (block_index, block) in input.chunks(32).enumerate() {
        let mut transcript = Vec::with_capacity(96);
        transcript.extend_from_slice(b"orchestrator-daemon-stream-v1\0");
        transcript.extend_from_slice(domain);
        transcript.extend_from_slice(&counter.to_be_bytes());
        transcript.extend_from_slice(&u64::try_from(block_index).unwrap_or(u64::MAX).to_be_bytes());
        let key_stream = hmac_sha256(key, &transcript);
        output.extend(
            block
                .iter()
                .zip(key_stream)
                .map(|(plain, mask)| plain ^ mask),
        );
    }
    output
}

fn frame_tag(key: &[u8; 32], domain: &[u8], counter: u64, ciphertext: &[u8]) -> [u8; 32] {
    let mut transcript = Vec::with_capacity(64 + ciphertext.len());
    transcript.extend_from_slice(b"orchestrator-daemon-frame-v1\0");
    transcript.extend_from_slice(domain);
    transcript.extend_from_slice(&counter.to_be_bytes());
    transcript.extend_from_slice(ciphertext);
    hmac_sha256(key, &transcript)
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn parse_secure_header(
    header: &[u8],
    maximum: usize,
) -> Result<(u64, usize, [u8; 32]), DaemonError> {
    let header = std::str::from_utf8(header)
        .ok()
        .map(str::trim)
        .and_then(|value| value.strip_prefix("secure:"))
        .ok_or(DaemonError::Authentication)?;
    let mut fields = header.split(':');
    let counter = fields
        .next()
        .and_then(|value| value.parse().ok())
        .ok_or(DaemonError::Authentication)?;
    let length = fields
        .next()
        .and_then(|value| value.parse().ok())
        .filter(|length| *length <= maximum)
        .ok_or(DaemonError::TooLarge)?;
    let encoded_tag = fields.next().ok_or(DaemonError::Authentication)?;
    if fields.next().is_some() || encoded_tag.len() != 64 {
        return Err(DaemonError::Authentication);
    }
    let mut tag = [0u8; 32];
    for (index, pair) in encoded_tag.as_bytes().chunks_exact(2).enumerate() {
        tag[index] = hex_nibble(pair[0])?
            .checked_mul(16)
            .and_then(|high| high.checked_add(hex_nibble(pair[1]).ok()?))
            .ok_or(DaemonError::Authentication)?;
    }
    Ok((counter, length, tag))
}

fn hex_nibble(byte: u8) -> Result<u8, DaemonError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(DaemonError::Authentication),
    }
}

fn authenticate_local_request(
    stream: &mut UnixStream,
    state: &State,
    deadline: AbsoluteDeadline,
) -> Result<(LocalRequest, SecureChannel), DaemonError> {
    let hello = read_bounded_line_before(stream, 16, deadline)?;
    if hello != b"hello\n" {
        return Err(DaemonError::Authentication);
    }
    let nonce = generate_token()?;
    let proof = authentication_proof(&state.token, "server", &nonce, "", 0);
    let challenge = format!("challenge:{nonce}:{proof}\n");
    write_all_before(stream, challenge.as_bytes(), deadline)?;
    let line = read_bounded_line_before(stream, 4096, deadline)?;
    let request =
        serde_json::from_slice::<LocalRequest>(&line).map_err(|_| DaemonError::Authentication)?;
    let expected =
        authentication_proof(&state.token, "client", &nonce, &request.op, request.cursor);
    if !constant_time_eq(request.auth.as_bytes(), expected.as_bytes()) {
        return Err(DaemonError::Authentication);
    }
    let channel = SecureChannel::server(&state.token, &nonce);
    Ok((request, channel))
}

fn spawn_ingest(
    listener: UnixListener,
    state: Arc<State>,
    cancel: UnixStream,
) -> Result<JoinHandle<()>, DaemonError> {
    thread::Builder::new()
        .name("orchestrator-daemon-ingest".into())
        .spawn(move || {
            while listener_ready(&listener, &cancel) {
                if let Ok((stream, _)) = listener.accept() {
                    spawn_unix_client(stream, &state, "orchestrator-daemon-client", handle_ingest);
                }
            }
        })
        .map_err(|source| io_error("spawn ingest listener", source))
}

fn handle_ingest(mut stream: UnixStream, state: &State) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let Ok(deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
        return;
    };
    let (request, mut channel) = match authenticate_local_request(&mut stream, state, deadline) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_protocol_reply(&mut stream, b"unauthorized\n");
            return;
        }
    };
    if request.op == "stop" {
        let _ = channel.write_frame_before(&mut stream, b"stopping", deadline);
        request_shutdown(state);
        let address =
            read_identity_from_parts(&state.root_path, &state.root_directory, state.root_identity)
                .ok()
                .flatten()
                .map_or(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), |v| {
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), v.port)
                });
        wake_all(&state.root_path, address);
        return;
    }
    if request.op == "cancel-mission" {
        handle_mission_cancellation(&mut stream, state, &mut channel, deadline);
        return;
    }
    if request.op != "emit" {
        let Ok(reply_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
            return;
        };
        let _ = channel.write_frame_before(&mut stream, b"invalid-operation", reply_deadline);
        return;
    }
    loop {
        let Ok(frame_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
            break;
        };
        let content = match channel.read_frame_before(&mut stream, MAX_FRAME_BYTES, frame_deadline)
        {
            Ok(Some(content)) => content,
            Ok(None) | Err(_) => break,
        };
        match ingest_event(state, &content) {
            Ok(()) => {
                let _ = channel.write_frame_before(&mut stream, b"ok", frame_deadline);
            }
            Err(error) => {
                let _ = write_ingest_error(&mut stream, &mut channel, &error);
                break;
            }
        }
    }
}

fn handle_mission_cancellation(
    stream: &mut UnixStream,
    state: &State,
    channel: &mut SecureChannel,
    deadline: AbsoluteDeadline,
) {
    let request = match channel.read_frame_before(stream, MAX_CANCELLATION_MESSAGE_BYTES, deadline)
    {
        Ok(Some(content)) => match serde_json::from_slice::<MissionCancellationRequest>(&content) {
            Ok(request) if validate_mission_identifier(&request.mission_id).is_ok() => request,
            Ok(_) | Err(_) => {
                write_cancellation_protocol_rejection(stream, channel, deadline);
                return;
            }
        },
        Ok(None) | Err(_) => {
            write_cancellation_protocol_rejection(stream, channel, deadline);
            return;
        }
    };
    let acknowledgement = lock(&state.owner).request_mission_cancellation(&request.mission_id);
    let Ok(encoded) = serde_json::to_vec(&acknowledgement) else {
        return;
    };
    let _ = channel.write_frame_before(stream, &encoded, deadline);
}

fn write_cancellation_protocol_rejection(
    stream: &mut UnixStream,
    channel: &mut SecureChannel,
    deadline: AbsoluteDeadline,
) {
    let _ = channel.write_frame_before(stream, b"invalid-request", deadline);
}

fn spawn_events(
    listener: UnixListener,
    state: Arc<State>,
    cancel: UnixStream,
) -> Result<JoinHandle<()>, DaemonError> {
    thread::Builder::new()
        .name("orchestrator-daemon-events".into())
        .spawn(move || {
            while listener_ready(&listener, &cancel) {
                if let Ok((stream, _)) = listener.accept() {
                    spawn_unix_client(
                        stream,
                        &state,
                        "orchestrator-events-client",
                        handle_events_client,
                    );
                }
            }
        })
        .map_err(|source| io_error("spawn events listener", source))
}

fn handle_events_client(mut stream: UnixStream, state: &State) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let Ok(deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
        return;
    };
    let (request, mut channel) = match authenticate_local_request(&mut stream, state, deadline) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_protocol_reply(&mut stream, b"unauthorized\n");
            return;
        }
    };
    if request.op == "prepare-subscribe" {
        let high_water = match subscription_high_water(state) {
            Ok(high_water) => high_water,
            Err(error) => {
                let _ = write_subscription_error(&mut stream, &mut channel, &error);
                return;
            }
        };
        let Ok(reply_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
            return;
        };
        let reply = format!("high-water:{}", high_water.0);
        let _ = channel.write_frame_before(&mut stream, reply.as_bytes(), reply_deadline);
        return;
    }
    let replay = match enroll_subscription(state, request.cursor) {
        Ok(replay) => replay,
        Err(error) => {
            let _ = write_subscription_error(&mut stream, &mut channel, &error);
            return;
        }
    };
    let Ok(ack_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
        return;
    };
    if channel
        .write_frame_before(
            &mut stream,
            format!("ok:{}", replay.high_water.0).as_bytes(),
            ack_deadline,
        )
        .is_err()
    {
        return;
    }
    let Ok(replay_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
        return;
    };
    for event in replay.events {
        if write_local_event(&mut stream, &mut channel, &event, replay_deadline).is_err() {
            return;
        }
    }
    while let Ok(queued) = replay.receiver.recv() {
        state.stream_wakeups.fetch_add(1, Ordering::Relaxed);
        let Ok(event_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
            return;
        };
        if write_local_event(&mut stream, &mut channel, &queued.event, event_deadline).is_err() {
            return;
        }
    }
}

fn spawn_http(
    listener: TcpListener,
    state: Arc<State>,
    cancel: UnixStream,
) -> Result<JoinHandle<()>, DaemonError> {
    thread::Builder::new()
        .name("orchestrator-daemon-http".into())
        .spawn(move || {
            while listener_ready(&listener, &cancel) {
                if let Ok((stream, _)) = listener.accept() {
                    spawn_tcp_client(stream, &state, "orchestrator-http-client", handle_http);
                }
            }
        })
        .map_err(|source| io_error("spawn HTTP listener", source))
}

fn handle_http(mut stream: TcpStream, state: &State) {
    if stream.set_nonblocking(true).is_err() {
        return;
    }
    let request = match AbsoluteDeadline::after(PROTOCOL_TIMEOUT)
        .and_then(|deadline| read_http_request(&mut stream, deadline))
    {
        Ok(value) => value,
        Err(DaemonError::TooLarge) => {
            let _ =
                http_reply_with_cors(&mut stream, 413, "text/plain", b"request too large\n", None);
            return;
        }
        Err(_) => {
            let _ = http_reply_with_cors(&mut stream, 400, "text/plain", b"bad request\n", None);
            return;
        }
    };
    if request
        .origin
        .as_deref()
        .is_some_and(|origin| !allowed_origin(origin))
    {
        let _ = http_reply_with_cors(
            &mut stream,
            403,
            "text/plain",
            b"cross-origin request rejected\n",
            None,
        );
        return;
    }
    let cors_origin = request.origin.as_deref();
    if request.method == "OPTIONS" {
        let _ = http_reply_with_cors(&mut stream, 204, "text/plain", b"", cors_origin);
        return;
    }
    if request.method == "GET" && request.path == "/api/health" {
        let _ = http_reply_with_cors(
            &mut stream,
            200,
            "application/json",
            b"{\"status\":\"ok\"}\n",
            cors_origin,
        );
        return;
    }
    if !request.authorized(&state.http_mode) {
        let _ = http_reply_with_cors(
            &mut stream,
            401,
            "application/json",
            b"{\"error\":\"unauthorized\"}\n",
            cors_origin,
        );
        return;
    }
    if request.method == "POST" && request.path == "/api/events" {
        let result = ingest_http_events(state, &request.body);
        let _ = match result {
            Ok(()) => http_reply_with_cors(
                &mut stream,
                202,
                "application/json",
                b"{\"accepted\":true}\n",
                cors_origin,
            ),
            Err(DaemonError::EventOwnerRetrySameEventId { .. }) => http_reply_with_cors(
                &mut stream,
                503,
                "application/json",
                b"{\"error\":\"event owner acknowledgement indeterminate\",\"retry\":\"same_event_id\"}\n",
                cors_origin,
            ),
            Err(
                DaemonError::InvalidEvent(_)
                | DaemonError::EventOwnerPermanentRejection { .. },
            ) => http_reply_with_cors(
                &mut stream,
                400,
                "application/json",
                b"{\"error\":\"invalid event\"}\n",
                cors_origin,
            ),
            Err(DaemonError::TooLarge) => http_reply_with_cors(
                &mut stream,
                413,
                "application/json",
                b"{\"error\":\"event_too_large\"}\n",
                cors_origin,
            ),
            Err(_) => http_reply_with_cors(
                &mut stream,
                500,
                "application/json",
                b"{\"error\":\"event owner unavailable\"}\n",
                cors_origin,
            ),
        };
        return;
    }
    if request.method == "GET" && request.path == "/api/events" {
        let replay = match enroll_subscription(state, request.cursor) {
            Ok(replay) => replay,
            Err(error) => {
                let (status, body) = subscription_http_error(&error);
                let _ = http_reply_with_cors(
                    &mut stream,
                    status,
                    "application/json",
                    body,
                    cors_origin,
                );
                return;
            }
        };
        let network_replay = match replay
            .events
            .iter()
            .map(EventLine::try_from)
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(events) => events,
            Err(_) => {
                let _ = http_reply_with_cors(
                    &mut stream,
                    422,
                    "application/json",
                    b"{\"error\":\"unsafe_event_projection\"}\n",
                    cors_origin,
                );
                return;
            }
        };
        if write_sse_headers(&mut stream, cors_origin).is_err() {
            return;
        }
        let Ok(replay_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
            return;
        };
        for line in network_replay {
            if write_sse(&mut stream, &line, replay_deadline).is_err() {
                return;
            }
        }
        while let Ok(queued) = replay.receiver.recv() {
            state.stream_wakeups.fetch_add(1, Ordering::Relaxed);
            let Some(line) = queued.network.as_deref() else {
                return;
            };
            let Ok(event_deadline) = AbsoluteDeadline::after(PROTOCOL_TIMEOUT) else {
                return;
            };
            if write_sse(&mut stream, line, event_deadline).is_err() {
                return;
            }
        }
        return;
    }
    let _ = http_reply_with_cors(
        &mut stream,
        404,
        "application/json",
        b"{\"error\":\"not found\"}\n",
        cors_origin,
    );
}

fn ingest_http_events(state: &State, body: &[u8]) -> Result<(), DaemonError> {
    let mut accepted = 0usize;
    for line in body.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if line.is_empty() {
            continue;
        }
        ingest_event(state, line)?;
        accepted += 1;
    }
    if accepted == 0 {
        return Err(DaemonError::InvalidEvent("empty event body".into()));
    }
    Ok(())
}

fn ingest_event(state: &State, bytes: &[u8]) -> Result<(), DaemonError> {
    if bytes.len() > MAX_FRAME_BYTES {
        return Err(DaemonError::TooLarge);
    }
    verify_root(state)?;
    let decoded =
        decode_event_line(bytes).map_err(|error| DaemonError::InvalidEvent(error.to_string()))?;
    validate_new_id(&decoded.record.id)?;
    let _ = validate_sse_field("event", &decoded.record.event_type)?;
    let _ = validate_sse_field("id", &decoded.record.sequence.to_string())?;
    let projected = EventLine::try_from(&OwnedEvent {
        cursor: DurableCursor(decoded.record.sequence),
        event_id: decoded.record.id.clone(),
        ingress_bytes: Arc::from(bytes),
    })?;
    if bytes.len().saturating_add(projected.bytes.len()) > MAX_EVENT_TRANSPORT_BYTES {
        return Err(DaemonError::TooLarge);
    }
    let mut owner = lock(&state.owner);
    let commit = match owner.commit_exact(&decoded.record.id, bytes) {
        Ok(commit) => commit,
        Err(failure) => {
            if failure.retry == RetryClassification::RetrySameEventId {
                if let Some(cursor) = failure.cursor {
                    let event = OwnedEvent {
                        cursor,
                        event_id: decoded.record.id,
                        ingress_bytes: Arc::from(bytes),
                    };
                    broadcast(&mut lock(&state.subscribers), &event);
                }
            }
            return Err(owner_failure(failure));
        }
    };
    if commit.event.cursor.0 <= 0
        || commit.event.event_id != decoded.record.id
        || commit.event.cursor.0 != decoded.record.sequence
        || commit.event.ingress_bytes.as_ref() != bytes
    {
        return Err(DaemonError::EventOwnerProtocol);
    }
    if commit.retry != RetryClassification::NoRetryRequired {
        return Err(DaemonError::EventOwnerProtocol);
    }
    if commit.freshness == CommitFreshness::New {
        broadcast(&mut lock(&state.subscribers), &commit.event);
    }
    Ok(())
}

/// Captures the owner high-water mark and installs the live sender in one
/// mutation-ordered handoff. Potentially blocking socket replay happens only
/// after both mutexes have been released.
struct SubscriptionReplay {
    high_water: DurableCursor,
    events: Vec<OwnedEvent>,
    receiver: Receiver<QueuedEvent>,
}

fn subscription_high_water(state: &State) -> Result<DurableCursor, DaemonError> {
    let mut owner = lock(&state.owner);
    let page = owner
        .replay_page(
            DurableCursor(i64::MAX),
            MAX_REPLAY_PAGE_EVENTS,
            MAX_REPLAY_PAGE_BYTES,
        )
        .map_err(owner_failure)?;
    if page.high_water.0 < 0 || !page.events.is_empty() || page.has_more {
        return Err(DaemonError::EnrollmentRejected);
    }
    Ok(page.high_water)
}

fn enroll_subscription(state: &State, cursor: i64) -> Result<SubscriptionReplay, DaemonError> {
    if cursor < 0 {
        return Err(DaemonError::CursorMismatch {
            requested: cursor,
            high_water: 0,
        });
    }
    let mut owner = lock(&state.owner);
    let mut after = DurableCursor(cursor);
    let mut expected_high_water = None;
    let mut events = Vec::new();
    let mut replay_bytes = 0usize;
    loop {
        let page = owner
            .replay_page(after, MAX_REPLAY_PAGE_EVENTS, MAX_REPLAY_PAGE_BYTES)
            .map_err(owner_failure)?;
        if page.events.len() > MAX_REPLAY_PAGE_EVENTS {
            return Err(DaemonError::EnrollmentRejected);
        }
        let mut page_bytes = 0usize;
        for event in &page.events {
            if event.ingress_bytes.len() > MAX_FRAME_BYTES {
                return Err(DaemonError::EnrollmentRejected);
            }
            page_bytes = page_bytes
                .checked_add(event.ingress_bytes.len())
                .ok_or(DaemonError::EnrollmentRejected)?;
        }
        if page_bytes > MAX_REPLAY_PAGE_BYTES {
            return Err(DaemonError::EnrollmentRejected);
        }
        let high_water = *expected_high_water.get_or_insert(page.high_water);
        if page.high_water != high_water {
            return Err(DaemonError::EnrollmentRejected);
        }
        if cursor > high_water.0 {
            return Err(DaemonError::CursorMismatch {
                requested: cursor,
                high_water: high_water.0,
            });
        }
        let mut prior = after.0;
        let page_was_empty = page.events.is_empty();
        for event in page.events {
            let projected_bytes = validate_owned_event(&event, prior, high_water)?;
            prior = event.cursor.0;
            after = event.cursor;
            if event.cursor.0 > cursor {
                replay_bytes = replay_bytes
                    .checked_add(event.ingress_bytes.len())
                    .and_then(|bytes| bytes.checked_add(projected_bytes))
                    .ok_or(DaemonError::ReplayBudgetExceeded)?;
                if events.len() == MAX_TOTAL_REPLAY_EVENTS || replay_bytes > MAX_TOTAL_REPLAY_BYTES
                {
                    return Err(DaemonError::ReplayBudgetExceeded);
                }
                events.push(event);
            }
        }
        if !page.has_more {
            if after != high_water {
                return Err(DaemonError::EnrollmentRejected);
            }
            break;
        }
        if prior == 0 || page_was_empty {
            return Err(DaemonError::EnrollmentRejected);
        }
    }
    let high_water = expected_high_water.unwrap_or_else(DurableCursor::origin);
    let (sender, receiver) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
    lock(&state.subscribers).push(Subscriber {
        sender,
        queued_bytes: Arc::new(AtomicUsize::new(0)),
        global_queued_bytes: Arc::clone(&state.global_queued_bytes),
    });
    Ok(SubscriptionReplay {
        high_water,
        events,
        receiver,
    })
}

fn validate_owned_event(
    event: &OwnedEvent,
    prior: i64,
    high_water: DurableCursor,
) -> Result<usize, DaemonError> {
    let decoded =
        decode_event_line(&event.ingress_bytes).map_err(|_| DaemonError::EnrollmentRejected)?;
    let canonical =
        encode_current_event(&decoded.record).map_err(|_| DaemonError::EnrollmentRejected)?;
    if event.cursor.0 != prior.saturating_add(1)
        || event.cursor > high_water
        || event.cursor.0 != decoded.record.sequence
        || event.event_id != decoded.record.id
        || event.ingress_bytes.as_ref() != canonical.as_slice()
    {
        return Err(DaemonError::EnrollmentRejected);
    }
    let projection = EventLine::try_from(event).map_err(|_| DaemonError::EnrollmentRejected)?;
    Ok(projection.bytes.len())
}

fn owner_failure(failure: OwnerCommitFailure) -> DaemonError {
    let durable_cursor = failure.cursor.map(|cursor| cursor.0);
    match failure.retry {
        RetryClassification::RetrySameEventId => DaemonError::EventOwnerRetrySameEventId {
            durable_cursor,
            reason: failure.reason,
        },
        RetryClassification::PermanentRejection => DaemonError::EventOwnerPermanentRejection {
            durable_cursor,
            reason: failure.reason,
        },
        RetryClassification::NoRetryRequired => DaemonError::EventOwnerProtocol,
    }
}

fn broadcast(subscribers: &mut Vec<Subscriber>, event: &OwnedEvent) {
    // Encode and redact once for every live subscriber. The canonical owner
    // bytes and network projection are immutable shared allocations.
    let network = EventLine::try_from(event).ok().map(Arc::new);
    subscribers.retain(|subscriber| {
        let byte_len = event.ingress_bytes.len().saturating_add(
            network
                .as_ref()
                .map_or(0, |projection| projection.bytes.len()),
        );
        if !try_reserve_bytes(&subscriber.queued_bytes, byte_len, MAX_CLIENT_QUEUED_BYTES) {
            return false;
        }
        if !try_reserve_bytes(
            &subscriber.global_queued_bytes,
            byte_len,
            MAX_GLOBAL_QUEUED_BYTES,
        ) {
            subscriber
                .queued_bytes
                .fetch_sub(byte_len, Ordering::AcqRel);
            return false;
        }
        let queued = QueuedEvent {
            event: event.clone(),
            network: network.clone(),
            queued_bytes: Arc::clone(&subscriber.queued_bytes),
            global_queued_bytes: Arc::clone(&subscriber.global_queued_bytes),
            byte_len,
        };
        match subscriber.sender.try_send(queued) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        }
    });
}

fn try_reserve_bytes(counter: &AtomicUsize, amount: usize, maximum: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(amount).filter(|next| *next <= maximum) else {
            return false;
        };
        match counter.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

fn write_local_event(
    stream: &mut UnixStream,
    channel: &mut SecureChannel,
    event: &OwnedEvent,
    deadline: AbsoluteDeadline,
) -> Result<(), DaemonError> {
    let mut frame = format!("cursor:{}\n", event.cursor.0).into_bytes();
    frame.extend_from_slice(&event.ingress_bytes);
    frame.push(b'\n');
    channel.write_frame_before(stream, &frame, deadline)
}

fn write_protocol_reply(stream: &mut UnixStream, bytes: &[u8]) -> Result<(), DaemonError> {
    let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
    write_all_before(stream, bytes, deadline)
}

fn write_ingest_error(
    stream: &mut UnixStream,
    channel: &mut SecureChannel,
    error: &DaemonError,
) -> Result<(), DaemonError> {
    let reply = match error {
        DaemonError::EventOwnerRetrySameEventId { durable_cursor, .. } => {
            format!("retry-same:{}", durable_cursor.unwrap_or(0))
        }
        DaemonError::EventOwnerPermanentRejection { durable_cursor, .. } => {
            format!("reject:{}", durable_cursor.unwrap_or(0))
        }
        DaemonError::TooLarge => "too-large".to_owned(),
        _ => format!("error: {error}"),
    };
    let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
    channel.write_frame_before(stream, reply.as_bytes(), deadline)
}

fn write_subscription_error(
    stream: &mut UnixStream,
    channel: &mut SecureChannel,
    error: &DaemonError,
) -> Result<(), DaemonError> {
    let reply = match error {
        DaemonError::CursorMismatch {
            requested,
            high_water,
        } => format!("error:cursor-mismatch:{requested}:{high_water}"),
        DaemonError::ReplayBudgetExceeded => "error:replay-budget".to_owned(),
        _ => "error:enrollment-rejected".to_owned(),
    };
    let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
    channel.write_frame_before(stream, reply.as_bytes(), deadline)
}

fn subscription_http_error(error: &DaemonError) -> (u16, &'static [u8]) {
    match error {
        DaemonError::CursorMismatch { .. } => (
            409,
            b"{\"error\":\"cursor_mismatch\",\"retry\":\"reset_cursor\"}\n",
        ),
        DaemonError::ReplayBudgetExceeded => (
            413,
            b"{\"error\":\"replay_budget_exceeded\",\"retry\":\"advance_cursor\"}\n",
        ),
        _ => (503, b"{\"error\":\"subscription_enrollment_failed\"}\n"),
    }
}

struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    origin: Option<String>,
    cursor: i64,
    body: Vec<u8>,
}

impl HttpRequest {
    fn authorized(&self, mode: &HttpMode) -> bool {
        match mode {
            HttpMode::LocalKey(key) => self
                .headers
                .get("authorization")
                .and_then(|value| value.strip_prefix("Bearer "))
                .is_some_and(|value| constant_time_eq(value.as_bytes(), key)),
        }
    }
}

fn read_http_request(
    stream: &mut TcpStream,
    deadline: AbsoluteDeadline,
) -> Result<HttpRequest, DaemonError> {
    let mut total = 0usize;
    let request_line = read_http_line(stream, &mut total, deadline)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts
        .next()
        .unwrap_or_default()
        .split('?')
        .next()
        .unwrap_or_default()
        .to_owned();
    if method.is_empty() || path.is_empty() {
        return Err(DaemonError::InvalidEvent("bad HTTP request".into()));
    }
    let mut headers = BTreeMap::new();
    loop {
        let line = read_http_line(stream, &mut total, deadline)?;
        if line == "\r\n" || line == "\n" {
            break;
        }
        let Some((name, value)) = line.trim_end().split_once(':') else {
            return Err(DaemonError::InvalidEvent("bad HTTP header".into()));
        };
        headers.insert(name.to_ascii_lowercase(), value.trim().to_owned());
    }
    let length = headers.get("content-length").map_or(Ok(0usize), |value| {
        value.parse::<usize>().map_err(|_| DaemonError::TooLarge)
    })?;
    if length > MAX_HTTP_BODY_BYTES {
        return Err(DaemonError::TooLarge);
    }
    let mut body = vec![0; length];
    read_exact_before(stream, &mut body, deadline)?;
    let cursor = headers
        .get("last-event-id")
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let origin = headers.get("origin").cloned();
    Ok(HttpRequest {
        method,
        path,
        headers,
        origin,
        cursor,
        body,
    })
}

fn read_http_line(
    reader: &mut TcpStream,
    total: &mut usize,
    deadline: AbsoluteDeadline,
) -> Result<String, DaemonError> {
    let remaining = MAX_HTTP_HEADER_BYTES.saturating_sub(*total);
    let bytes = read_bounded_line_before(reader, remaining, deadline)?;
    let count = bytes.len();
    *total = total.saturating_add(count);
    if count == 0 || *total > MAX_HTTP_HEADER_BYTES {
        return Err(DaemonError::TooLarge);
    }
    String::from_utf8(bytes).map_err(|error| DaemonError::InvalidEvent(error.to_string()))
}

fn write_sse(
    stream: &mut TcpStream,
    event: &EventLine,
    deadline: AbsoluteDeadline,
) -> Result<(), DaemonError> {
    write_all_before(
        stream,
        format!(
            "id: {}\nevent: {}\ndata: ",
            event.cursor.0, event.event_type
        )
        .as_bytes(),
        deadline,
    )?;
    write_all_before(stream, &event.bytes, deadline)?;
    write_all_before(stream, b"\n\n", deadline)
}

fn sanitize_event_for_network(record: &EventRecord) -> EventRecord {
    let data = record.data.as_ref().and_then(|data| {
        let values = data
            .iter()
            .map(|(key, value)| {
                let value = if is_sensitive_key(key) {
                    Value::String("[REDACTED]".into())
                } else if key == "task" {
                    match value {
                        Value::String(text) => Value::String(sanitize_string_safe(text)),
                        other => sanitize_json_value(other),
                    }
                } else {
                    sanitize_json_value(value)
                };
                (key.clone(), value)
            })
            .collect::<BTreeMap<_, _>>();
        (!values.is_empty()).then(|| EventJsonMap::from(values))
    });
    EventRecord {
        id: record.id.clone(),
        event_type: sanitize_string(&record.event_type),
        timestamp: sanitize_string(&record.timestamp),
        sequence: record.sequence,
        mission_id: sanitize_string(&record.mission_id),
        phase_id: record.phase_id.as_deref().map(sanitize_string),
        worker_id: record.worker_id.as_deref().map(sanitize_string),
        data,
        // Network projection is an explicit allowlist. Unknown forensic
        // envelope fields remain owner bytes and never cross the SSE boundary.
        extra: EventJsonMap::default(),
    }
}

fn is_sensitive_key(key: &str) -> bool {
    if key == "dir" || key == "error" {
        return true;
    }
    let lower = key.to_lowercase();
    [
        "password",
        "secret",
        "token",
        "api_key",
        "apikey",
        "credential",
        "auth",
    ]
    .iter()
    .any(|pattern| lower.contains(pattern))
}

fn sanitize_json_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(sanitize_string(text)),
        Value::Array(values) => Value::Array(values.iter().map(sanitize_json_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        if is_sensitive_key(key) {
                            Value::String("[REDACTED]".into())
                        } else {
                            sanitize_json_value(value)
                        },
                    )
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

fn validate_sse_field(name: &'static str, value: &str) -> Result<String, DaemonError> {
    if value.is_empty()
        || value.len() > 256
        || value
            .chars()
            .any(|character| character.is_control() || matches!(character, '\r' | '\n'))
    {
        return Err(DaemonError::InvalidEvent(format!(
            "invalid SSE {name} field"
        )));
    }
    Ok(value.to_owned())
}

fn sanitize_string_safe(value: &str) -> String {
    redact_bearer_basic(&redact_pem_headers(&redact_api_keys(value)))
}

fn sanitize_string(value: &str) -> String {
    let sanitized = sanitize_string_safe(value);
    if !sanitized.contains("[REDACTED]") && is_high_entropy(&sanitized) {
        return "[REDACTED]".into();
    }
    if sanitized
        .split_whitespace()
        .any(|token| !token.contains("[REDACTED]") && is_high_entropy(token))
    {
        return "[REDACTED]".into();
    }
    sanitized
}

fn redact_api_keys(value: &str) -> String {
    const PREFIXES: [&[u8]; 7] = [
        b"sk-ant-", b"ghp_", b"gho_", b"glpat-", b"AKIA", b"xoxb-", b"xoxp-",
    ];
    redact_ascii(value, |bytes, start| {
        let prefix = PREFIXES
            .iter()
            .find(|prefix| bytes[start..].starts_with(prefix))?;
        let mut end = start + prefix.len();
        let body_start = end;
        while bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            end += 1;
        }
        (end > body_start).then_some(end)
    })
}

fn redact_pem_headers(value: &str) -> String {
    redact_ascii(value, |bytes, start| {
        bytes[start..].starts_with(b"-----BEGIN").then(|| {
            bytes[start..]
                .iter()
                .position(|byte| matches!(byte, b'\r' | b'\n'))
                .map_or(bytes.len(), |offset| start + offset)
        })
    })
}

fn redact_bearer_basic(value: &str) -> String {
    redact_ascii(value, |bytes, start| {
        let scheme_length = [b"Bearer".as_slice(), b"Basic".as_slice()]
            .iter()
            .find(|scheme| {
                bytes.len().saturating_sub(start) >= scheme.len()
                    && bytes[start..start + scheme.len()].eq_ignore_ascii_case(scheme)
            })?
            .len();
        let mut token_start = start + scheme_length;
        while bytes
            .get(token_start)
            .is_some_and(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\x0c' | b'\r'))
        {
            token_start += 1;
        }
        if token_start == start + scheme_length {
            return None;
        }
        let mut end = token_start;
        while bytes.get(end).is_some_and(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=' | b'.' | b'_' | b'-')
        }) {
            end += 1;
        }
        (end.saturating_sub(token_start) >= 8).then_some(end)
    })
}

fn redact_ascii(value: &str, matcher: impl Fn(&[u8], usize) -> Option<usize>) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut copied = 0usize;
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if let Some(end) = matcher(bytes, cursor) {
            output.push_str(&value[copied..cursor]);
            output.push_str("[REDACTED]");
            cursor = end;
            copied = end;
        } else {
            cursor += 1;
            while cursor < bytes.len() && !value.is_char_boundary(cursor) {
                cursor += 1;
            }
        }
    }
    output.push_str(&value[copied..]);
    output
}

fn is_high_entropy(value: &str) -> bool {
    if value.len() <= 20 {
        return false;
    }
    let mut frequencies = BTreeMap::<char, usize>::new();
    let mut count = 0usize;
    for character in value.chars() {
        *frequencies.entry(character).or_default() += 1;
        count += 1;
    }
    let count = count as f64;
    let entropy = frequencies.values().fold(0.0, |entropy, frequency| {
        let probability = *frequency as f64 / count;
        entropy - probability * probability.log2()
    });
    entropy > 4.5
}

fn allowed_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://localhost")
        .or_else(|| origin.strip_prefix("http://127.0.0.1"))
    else {
        return false;
    };
    rest.is_empty()
        || rest
            .strip_prefix(':')
            .is_some_and(|port| !port.is_empty() && port.bytes().all(|byte| byte.is_ascii_digit()))
}

fn cors_headers(origin: Option<&str>) -> String {
    let allow_origin = origin
        .filter(|value| allowed_origin(value))
        .unwrap_or("http://localhost");
    format!(
        "Access-Control-Allow-Origin: {allow_origin}\r\nAccess-Control-Allow-Methods: GET, POST, OPTIONS\r\nAccess-Control-Allow-Headers: Authorization, Content-Type, Cf-Access-Jwt-Assertion, Last-Event-ID, Cache-Control\r\nAccess-Control-Allow-Credentials: true\r\n"
    )
}

fn write_sse_headers(stream: &mut TcpStream, origin: Option<&str>) -> Result<(), DaemonError> {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\nX-Accel-Buffering: no\r\n{}\r\n",
        cors_headers(origin)
    );
    let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
    write_all_before(stream, response.as_bytes(), deadline)
}

fn http_reply_with_cors(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    origin: Option<&str>,
) -> Result<(), DaemonError> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        500 => "Internal Server Error",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
        body.len(),
        cors_headers(origin)
    );
    let deadline = AbsoluteDeadline::after(PROTOCOL_TIMEOUT)?;
    write_all_before(stream, response.as_bytes(), deadline)?;
    write_all_before(stream, body, deadline)
}

fn admit_root(path: &Path) -> Result<RootCapability, DaemonError> {
    if !path.is_absolute() {
        return Err(DaemonError::UnsafeRoot);
    }
    fs::create_dir_all(path).map_err(|source| io_error("create daemon root", source))?;
    let root = admit_existing_root_unsealed(path)?;
    fchmod(&root.directory, Mode::from_raw_mode(0o700))
        .map_err(|source| io_error("set daemon root permissions", source.into()))?;
    Ok(root)
}

fn admit_existing_root(path: &Path) -> Result<RootCapability, DaemonError> {
    let root = admit_existing_root_unsealed(path)?;
    let metadata = root
        .directory
        .dir_metadata()
        .map_err(|source| io_error("inspect retained daemon root", source))?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(DaemonError::UnsafeRoot);
    }
    Ok(root)
}

fn admit_existing_root_unsealed(path: &Path) -> Result<RootCapability, DaemonError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| io_error("inspect daemon root", source))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(DaemonError::UnsafeRoot);
    }
    let canonical =
        fs::canonicalize(path).map_err(|source| io_error("canonicalize daemon root", source))?;
    let dir = Dir::open_ambient_dir(&canonical, ambient_authority())
        .map_err(|source| io_error("retain daemon root", source))?;
    let retained = dir
        .dir_metadata()
        .map_err(|source| io_error("inspect retained daemon root", source))?;
    if (retained.dev(), retained.ino()) != (metadata.dev(), metadata.ino()) {
        return Err(DaemonError::UnsafeRoot);
    }
    Ok(RootCapability {
        path: canonical,
        directory: dir,
        identity: (metadata.dev(), metadata.ino()),
    })
}

fn verify_root(state: &State) -> Result<(), DaemonError> {
    let metadata = fs::symlink_metadata(&state.root_path)
        .map_err(|source| io_error("reverify daemon root", source))?;
    let retained = state
        .root_directory
        .dir_metadata()
        .map_err(|source| io_error("reverify retained daemon root", source))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || (metadata.dev(), metadata.ino()) != state.root_identity
        || (retained.dev(), retained.ino()) != state.root_identity
    {
        return Err(DaemonError::UnsafeRoot);
    }
    Ok(())
}

fn verify_root_capability(root: &RootCapability) -> Result<(), DaemonError> {
    let metadata = fs::symlink_metadata(&root.path)
        .map_err(|source| io_error("reverify daemon root", source))?;
    let retained = root
        .directory
        .dir_metadata()
        .map_err(|source| io_error("reverify retained daemon root", source))?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || (metadata.dev(), metadata.ino()) != root.identity
        || (retained.dev(), retained.ino()) != root.identity
    {
        return Err(DaemonError::UnsafeRoot);
    }
    Ok(())
}

fn reject_live_duplicate(root: &RootCapability, exclusive_owner: bool) -> Result<(), DaemonError> {
    if let Some(identity) = read_identity(root)? {
        match identity_matches(&identity) {
            Ok(true) => return Err(DaemonError::AlreadyRunning),
            Ok(false) => {}
            Err(DaemonError::IdentityMismatch) if exclusive_owner => {
                // identity_matches uses the two-observation inspector: this
                // error means the recorded leader is absent while its numeric
                // group is present. A leased embedded owner owns only itself.
                // Refuse live/uncertain transport before touching any metadata.
                let sockets = [
                    (INGEST_SOCKET, probe_stale_socket(root, INGEST_SOCKET)?),
                    (EVENTS_SOCKET, probe_stale_socket(root, EVENTS_SOCKET)?),
                ];
                for (name, expected) in sockets {
                    match (root.directory.symlink_metadata(name), expected) {
                        (Ok(metadata), Some(expected))
                            if metadata.file_type().is_socket()
                                && FileIdentity::of_cap(&metadata) == expected => {}
                        (Err(error), None) if error.kind() == io::ErrorKind::NotFound => {}
                        _ => return Err(DaemonError::UnsafeEntry(name)),
                    }
                }
            }
            Err(error) => return Err(error),
        }
    } else if root.directory.symlink_metadata(PID_FILE).is_ok() {
        let pid_text = read_private_text(root, PID_FILE)?;
        let pid = pid_text
            .parse::<u32>()
            .map_err(|_| DaemonError::IdentityMismatch)?;
        if current_kernel_identity(pid).is_ok() {
            // A live legacy daemon has no start-identity sidecar. It is
            // ambiguous and must never be treated as stale or signalled.
            return Err(DaemonError::AlreadyRunning);
        }
    }
    for name in [PID_FILE, IDENTITY_FILE, AUTH_FILE] {
        remove_stale_regular(root, name)?;
    }
    Ok(())
}

fn probe_stale_socket(
    root: &RootCapability,
    name: &'static str,
) -> Result<Option<FileIdentity>, DaemonError> {
    match root.directory.symlink_metadata(name) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            let path = SocketPath::new(root, name)?;
            let connection = UnixStream::connect(path.path());
            path.verify()?;
            match connection {
                Ok(_) => Err(DaemonError::AlreadyRunning),
                Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                    Ok(Some(FileIdentity::of_cap(&metadata)))
                }
                Err(source) => Err(io_error("probe stale daemon socket", source)),
            }
        }
        Ok(_) => Err(DaemonError::UnsafeEntry(name)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error("inspect stale socket", source)),
    }
}

fn remove_stale_socket(root: &RootCapability, name: &'static str) -> Result<(), DaemonError> {
    if let Some(expected) = probe_stale_socket(root, name)? {
        remove_identity_checked(root, name, expected)?;
    }
    Ok(())
}

fn verify_socket_identity(
    root: &RootCapability,
    name: &'static str,
    expected: FileIdentity,
) -> Result<(), DaemonError> {
    let metadata = root
        .directory
        .symlink_metadata(name)
        .map_err(|source| io_error("inspect daemon socket identity", source))?;
    if !metadata.file_type().is_socket() || FileIdentity::of_cap(&metadata) != expected {
        return Err(DaemonError::UnsafeEntry(name));
    }
    Ok(())
}

fn socket_identity_at(
    root: &RootCapability,
    name: &'static str,
) -> Result<FileIdentity, DaemonError> {
    verify_root_capability(root)?;
    let metadata = root
        .directory
        .symlink_metadata(name)
        .map_err(|source| io_error("inspect daemon socket authority", source))?;
    if !metadata.file_type().is_socket() {
        return Err(DaemonError::UnsafeEntry(name));
    }
    Ok(FileIdentity::of_cap(&metadata))
}

fn bind_managed_socket(
    root: &RootCapability,
    name: &'static str,
) -> Result<(UnixListener, FileIdentity), DaemonError> {
    verify_root_capability(root)?;
    let mut random = [0u8; 5];
    getrandom::fill(&mut random).map_err(|error| {
        io_error(
            "generate temporary socket name",
            io::Error::other(error.to_string()),
        )
    })?;
    let temporary = format!(
        ".s{}",
        random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    );
    let path = SocketPath::new(root, &temporary)?;
    let listener = UnixListener::bind(path.path())
        .map_err(|source| io_error("bind private temporary daemon socket", source))?;
    path.verify()?;
    let before = root
        .directory
        .symlink_metadata(&temporary)
        .map_err(|source| io_error("capture private temporary socket", source))?;
    if !before.file_type().is_socket() {
        return Err(DaemonError::UnsafeEntry(name));
    }
    let expected = FileIdentity::of_cap(&before);
    let publication = (|| {
        verify_root_capability(root)?;
        chmodat(
            &root.directory,
            temporary.as_str(),
            Mode::from_raw_mode(0o600),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|source| {
            io_error(
                "set unpublished daemon socket permissions without following",
                source.into(),
            )
        })?;
        let private = root
            .directory
            .symlink_metadata(&temporary)
            .map_err(|source| io_error("verify unpublished daemon socket", source))?;
        if !private.file_type().is_socket()
            || FileIdentity::of_cap(&private) != expected
            || private.permissions().mode() & 0o777 != 0o600
        {
            return Err(DaemonError::UnsafeEntry(name));
        }
        renameat_with(
            &root.directory,
            temporary.as_str(),
            &root.directory,
            name,
            RenameFlags::NOREPLACE,
        )
        .map_err(|source| io_error("publish verified daemon socket", source.into()))?;
        verify_socket_identity(root, name, expected)
    })();
    if let Err(error) = publication {
        cleanup_dynamic_expected(root, &temporary, expected);
        if root
            .directory
            .symlink_metadata(name)
            .is_ok_and(|metadata| FileIdentity::of_cap(&metadata) == expected)
        {
            let _ = remove_identity_checked(root, name, expected);
        }
        return Err(error);
    }
    Ok((listener, expected))
}

fn cleanup_dynamic_expected(root: &RootCapability, name: &str, expected: FileIdentity) {
    let quarantine = format!(
        ".daemon-temp-remove-{}-{}-{}",
        std::process::id(),
        expected.device,
        expected.inode
    );
    if renameat_with(
        &root.directory,
        name,
        &root.directory,
        quarantine.as_str(),
        RenameFlags::NOREPLACE,
    )
    .is_err()
    {
        return;
    }
    if root
        .directory
        .symlink_metadata(&quarantine)
        .is_ok_and(|metadata| FileIdentity::of_cap(&metadata) == expected)
    {
        let _ = root.directory.remove_file(&quarantine);
    } else {
        let _ = renameat_with(
            &root.directory,
            quarantine.as_str(),
            &root.directory,
            name,
            RenameFlags::NOREPLACE,
        );
    }
}

fn remove_stale_regular(root: &RootCapability, name: &'static str) -> Result<(), DaemonError> {
    match root.directory.symlink_metadata(name) {
        Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => {
            remove_identity_checked(root, name, FileIdentity::of_cap(&metadata))
        }
        Ok(_) => Err(DaemonError::UnsafeEntry(name)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("inspect stale daemon file", source)),
    }
}

fn remove_identity_checked(
    root: &RootCapability,
    name: &'static str,
    expected: FileIdentity,
) -> Result<(), DaemonError> {
    let quarantine = format!(
        ".daemon-remove-{}-{}-{}",
        std::process::id(),
        expected.device,
        expected.inode
    );
    renameat_with(
        &root.directory,
        name,
        &root.directory,
        quarantine.as_str(),
        RenameFlags::NOREPLACE,
    )
    .map_err(|source| io_error("quarantine stale daemon entry", source.into()))?;
    let metadata = root
        .directory
        .symlink_metadata(&quarantine)
        .map_err(|source| io_error("verify quarantined daemon entry", source))?;
    if FileIdentity::of_cap(&metadata) != expected {
        let _ = renameat_with(
            &root.directory,
            quarantine.as_str(),
            &root.directory,
            name,
            RenameFlags::NOREPLACE,
        );
        return Err(DaemonError::UnsafeEntry(name));
    }
    root.directory
        .remove_file(&quarantine)
        .map_err(|source| io_error("remove verified stale daemon entry", source))
}

fn read_identity(root: &RootCapability) -> Result<Option<DaemonIdentity>, DaemonError> {
    read_identity_from_parts(&root.path, &root.directory, root.identity)
}

fn read_identity_from_parts(
    root_path: &Path,
    directory: &Dir,
    root_identity: (u64, u64),
) -> Result<Option<DaemonIdentity>, DaemonError> {
    let retained = directory
        .dir_metadata()
        .map_err(|source| io_error("inspect retained daemon root", source))?;
    if (retained.dev(), retained.ino()) != root_identity {
        return Err(DaemonError::UnsafeRoot);
    }
    match directory.symlink_metadata(IDENTITY_FILE) {
        Ok(metadata) if metadata.is_file() && metadata.nlink() == 1 => {}
        Ok(_) => return Err(DaemonError::UnsafeEntry(IDENTITY_FILE)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error("inspect daemon identity", source)),
    }
    let bytes = read_private_bytes(directory, IDENTITY_FILE, 4096)?;
    let identity: DaemonIdentity = serde_json::from_slice(&bytes).map_err(|error| {
        io_error(
            "decode daemon identity",
            io::Error::new(io::ErrorKind::InvalidData, error),
        )
    })?;
    let pid_text = read_private_text_from_parts(root_path, directory, root_identity, PID_FILE)?;
    let pid = pid_text
        .parse::<u32>()
        .map_err(|_| DaemonError::IdentityMismatch)?;
    if pid != identity.pid {
        return Err(DaemonError::IdentityMismatch);
    }
    Ok(Some(identity))
}

fn identity_matches(identity: &DaemonIdentity) -> Result<bool, DaemonError> {
    let status = inspect_recorded_process_identity(
        identity.pid,
        identity.process_group_id,
        &identity.start_identity,
    )
    .map_err(|error| {
        io_error(
            "inspect process start identity",
            io::Error::other(error.to_string()),
        )
    })?;
    match status {
        RecordedProcessIdentityStatus::ExactLive => Ok(true),
        RecordedProcessIdentityStatus::ExactGroupAbsent(_) => Ok(false),
        RecordedProcessIdentityStatus::LeaderAbsentGroupPresent => {
            Err(DaemonError::IdentityMismatch)
        }
    }
}

fn current_kernel_identity(pid: u32) -> Result<KernelProcessIdentity, DaemonError> {
    let raw_pid = i32::try_from(pid)
        .ok()
        .and_then(Pid::from_raw)
        .ok_or(DaemonError::IdentityMismatch)?;
    let process_group_id = getpgid(Some(raw_pid))
        .map_err(|error| io_error("inspect process group", io::Error::from(error)))?
        .as_raw_nonzero()
        .get() as u32;
    KernelProcessIdentity::observe(pid, process_group_id).map_err(|error| {
        io_error(
            "inspect process start identity",
            io::Error::other(error.to_string()),
        )
    })
}

fn request_shutdown(state: &State) {
    state.stopping.store(true, Ordering::Release);
    for cancel in &state.listener_cancels {
        let _ = cancel.shutdown(std::net::Shutdown::Both);
    }
    lock(&state.subscribers).clear();
    let mut clients = lock(&state.clients);
    clients.admission_closed = true;
    for client in clients.active.values() {
        client.cancel();
    }
}

fn abort_startup(
    state: &State,
    root: &Path,
    address: SocketAddr,
    threads: &mut Vec<JoinHandle<()>>,
) {
    request_shutdown(state);
    wake_all(root, address);
    for handle in threads.drain(..) {
        let _ = handle.join();
    }
    let _ = join_tracked_clients(state);
}

fn wake_all(root: &Path, address: SocketAddr) {
    if let Ok(root) = admit_existing_root(root) {
        for name in [INGEST_SOCKET, EVENTS_SOCKET] {
            if let Ok(path) = SocketPath::new(&root, name) {
                let _ = UnixStream::connect(path.path());
            }
        }
    }
    if address.port() != 0 {
        let _ = TcpStream::connect(address);
    }
}

trait ReadDeadline: Read + AsFd {}

impl ReadDeadline for UnixStream {}

impl ReadDeadline for TcpStream {}

#[derive(Clone, Copy)]
struct AbsoluteDeadline(Instant);

impl AbsoluteDeadline {
    fn after(duration: Duration) -> Result<Self, DaemonError> {
        Instant::now()
            .checked_add(duration)
            .map(Self)
            .ok_or_else(deadline_elapsed)
    }

    fn remaining(self) -> Result<Duration, DaemonError> {
        self.0
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .map(|remaining| {
                Duration::from_millis(
                    u64::try_from(remaining.as_millis())
                        .unwrap_or(u64::MAX)
                        .max(1),
                )
            })
            .ok_or_else(deadline_elapsed)
    }
}

fn deadline_elapsed() -> DaemonError {
    io_error(
        "meet absolute protocol deadline",
        io::Error::new(
            io::ErrorKind::TimedOut,
            "absolute protocol deadline elapsed",
        ),
    )
}

fn read_bounded_line_before(
    reader: &mut impl ReadDeadline,
    limit: usize,
    deadline: AbsoluteDeadline,
) -> Result<Vec<u8>, DaemonError> {
    let mut output = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        wait_readable(reader, deadline)?;
        match reader.read(&mut byte) {
            Ok(0) if output.is_empty() => return Ok(output),
            Ok(0) => {
                return Err(DaemonError::InvalidEvent(
                    "unterminated protocol frame".into(),
                ));
            }
            Ok(_) => {
                if output.len() == limit {
                    return Err(DaemonError::TooLarge);
                }
                output.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(output);
                }
            }
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => continue,
            Err(source) => return Err(io_error("read bounded frame", source)),
        }
    }
}

fn read_exact_before(
    reader: &mut impl ReadDeadline,
    output: &mut [u8],
    deadline: AbsoluteDeadline,
) -> Result<(), DaemonError> {
    let mut offset = 0usize;
    while offset < output.len() {
        wait_readable(reader, deadline)?;
        match reader.read(&mut output[offset..]) {
            Ok(0) => {
                return Err(DaemonError::InvalidEvent(
                    "unterminated protocol body".into(),
                ));
            }
            Ok(count) => offset += count,
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => continue,
            Err(source) => return Err(io_error("read HTTP body", source)),
        }
    }
    Ok(())
}

fn wait_readable(reader: &impl AsFd, deadline: AbsoluteDeadline) -> Result<(), DaemonError> {
    wait_for_socket(reader, deadline, PollFlags::IN)
}

fn write_all_before(
    writer: &mut (impl Write + AsFd),
    bytes: &[u8],
    deadline: AbsoluteDeadline,
) -> Result<(), DaemonError> {
    let mut offset = 0usize;
    while offset < bytes.len() {
        wait_for_socket(writer, deadline, PollFlags::OUT)?;
        match writer.write(&bytes[offset..]) {
            Ok(0) => {
                return Err(io_error(
                    "write protocol frame",
                    io::Error::new(io::ErrorKind::WriteZero, "protocol socket wrote zero bytes"),
                ));
            }
            Ok(count) => offset += count,
            Err(source) if source.kind() == io::ErrorKind::WouldBlock => continue,
            Err(source) => return Err(io_error("write protocol frame", source)),
        }
    }
    Ok(())
}

fn wait_for_socket(
    socket: &impl AsFd,
    deadline: AbsoluteDeadline,
    interest: PollFlags,
) -> Result<(), DaemonError> {
    let remaining = deadline.remaining()?;
    let seconds = i64::try_from(remaining.as_secs()).unwrap_or(i64::MAX);
    let timeout = Timespec {
        tv_sec: seconds,
        tv_nsec: i64::from(remaining.subsec_nanos()),
    };
    let mut descriptors = [PollFd::new(socket, interest)];
    let ready = poll(&mut descriptors, Some(&timeout))
        .map_err(|source| io_error("wait for protocol input", source.into()))?;
    if ready == 0 {
        return Err(deadline_elapsed());
    }
    let events = descriptors[0].revents();
    if events.intersects(PollFlags::ERR | PollFlags::NVAL) {
        return Err(io_error(
            "wait for protocol input",
            io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "protocol socket poll failed",
            ),
        ));
    }
    Ok(())
}

fn read_bounded_line(reader: &mut impl Read, limit: usize) -> Result<Vec<u8>, DaemonError> {
    let mut output = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) if output.is_empty() => return Ok(output),
            Ok(0) => {
                return Err(DaemonError::InvalidEvent(
                    "unterminated protocol frame".into(),
                ));
            }
            Ok(_) => {
                if output.len() == limit {
                    return Err(DaemonError::TooLarge);
                }
                output.push(byte[0]);
                if byte[0] == b'\n' {
                    return Ok(output);
                }
            }
            Err(source) => return Err(io_error("read bounded frame", source)),
        }
    }
}

fn validate_new_id(value: &str) -> Result<(), DaemonError> {
    let Some(suffix) = value.strip_prefix("evt_") else {
        return Err(DaemonError::InvalidEvent("invalid event id".into()));
    };
    let random_hex = suffix.len() == 16
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase());
    // Go's entropy-failure fallback is `evt_` plus a positive UnixNano value.
    let decimal_entropy_fallback = !suffix.is_empty()
        && suffix.bytes().all(|byte| byte.is_ascii_digit())
        && !suffix.starts_with('0')
        && suffix.parse::<i64>().is_ok_and(|value| value > 0);
    if !random_hex && !decimal_entropy_fallback {
        return Err(DaemonError::InvalidEvent("invalid event id".into()));
    }
    Ok(())
}

fn generate_token() -> Result<String, DaemonError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| io_error("generate daemon token", io::Error::other(error.to_string())))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn write_private_at(
    root: &RootCapability,
    name: &'static str,
    bytes: &[u8],
) -> Result<FileIdentity, DaemonError> {
    let mut options = CapOpenOptions::new();
    options.create(true).write(true).mode(0o600);
    options._cap_fs_ext_follow(FollowSymlinks::No);
    let mut file = root
        .directory
        .open_with(name, &options)
        .map_err(|source| io_error("write private daemon file", source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect private daemon file", source))?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(DaemonError::UnsafeEntry(name));
    }
    file.set_permissions(CapPermissions::from_mode(0o600))
        .map_err(|source| io_error("set private file permissions", source))?;
    file.set_len(0)
        .map_err(|source| io_error("truncate private daemon file", source))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|source| io_error("sync private daemon file", source))?;
    let identity = FileIdentity::of_fd(&file)?;
    let path_metadata = root
        .directory
        .symlink_metadata(name)
        .map_err(|source| io_error("verify private daemon file identity", source))?;
    if !path_metadata.is_file()
        || path_metadata.nlink() != 1
        || FileIdentity::of_cap(&path_metadata) != identity
    {
        return Err(DaemonError::UnsafeEntry(name));
    }
    Ok(identity)
}

#[cfg(test)]
fn write_private(root: &Path, name: &'static str, bytes: &[u8]) -> Result<(), DaemonError> {
    let root = admit_existing_root(root)?;
    write_private_at(&root, name, bytes).map(|_| ())
}

fn read_private_text(root: &RootCapability, name: &'static str) -> Result<String, DaemonError> {
    read_private_text_from_parts(&root.path, &root.directory, root.identity, name)
}

fn read_private_text_from_parts(
    _root_path: &Path,
    directory: &Dir,
    root_identity: (u64, u64),
    name: &'static str,
) -> Result<String, DaemonError> {
    let retained = directory
        .dir_metadata()
        .map_err(|source| io_error("inspect retained daemon root", source))?;
    if (retained.dev(), retained.ino()) != root_identity {
        return Err(DaemonError::UnsafeRoot);
    }
    let bytes = read_private_bytes(directory, name, 4096)?;
    String::from_utf8(bytes).map_err(|error| {
        io_error(
            "decode private daemon file",
            io::Error::new(io::ErrorKind::InvalidData, error),
        )
    })
}

fn read_private_bytes(
    directory: &Dir,
    name: &'static str,
    maximum: usize,
) -> Result<Vec<u8>, DaemonError> {
    let mut options = CapOpenOptions::new();
    options.read(true);
    options._cap_fs_ext_follow(FollowSymlinks::No);
    let mut file = directory
        .open_with(name, &options)
        .map_err(|source| io_error("read private daemon file", source))?;
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect private daemon file", source))?;
    if !metadata.is_file() || metadata.permissions().mode() & 0o077 != 0 || metadata.nlink() != 1 {
        return Err(DaemonError::UnsafeEntry(name));
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| io_error("read private daemon file", source))?;
    if bytes.len() > maximum {
        return Err(DaemonError::TooLarge);
    }
    Ok(bytes)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod fixture_owner {
    //! Crate-internal proof owner for daemon unit tests.
    //!
    //! This owner is bounded, durable, and restartable so transport behavior
    //! can be proved without weakening the production enrollment boundary. It
    //! is not a production event-owner enrollment or a supported data format.

    use super::{
        CanonicalEventOwner, CommitFreshness, DurableCursor, OwnedEvent, OwnerCommit,
        OwnerCommitFailure, OwnerReplayPage, RetryClassification, decode_event_line,
        validate_new_id,
    };
    use serde::{Deserialize, Serialize};
    use std::{
        collections::BTreeSet,
        fs::{self, File, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
        path::{Path, PathBuf},
        sync::Arc,
    };

    const STATE_FILE: &str = "fixture-event-owner.json";
    const STAGED_STATE_FILE: &str = ".fixture-event-owner.staged";
    const MAX_OWNER_ENTRIES: usize = 8;
    const MAX_OWNER_BYTES: usize = 64 * 1024;

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum CrashCut {
        BeforeDurableCommit,
        AfterDurableCommitBeforeAcknowledgement,
    }

    #[derive(Clone, Debug, Deserialize, Serialize)]
    struct StoredEvent {
        cursor: i64,
        event_id: String,
        ingress_bytes: Vec<u8>,
    }

    #[derive(Clone, Debug, Default, Deserialize, Serialize)]
    struct StoredState {
        events: Vec<StoredEvent>,
    }

    pub(super) struct FixtureEventOwner {
        directory: PathBuf,
        state: StoredState,
        crash_cut: Option<CrashCut>,
    }

    impl FixtureEventOwner {
        pub(super) fn open(directory: &Path) -> Result<Self, OwnerCommitFailure> {
            let metadata = fs::symlink_metadata(directory).map_err(|_| {
                failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture directory is unavailable",
                )
            })?;
            if !metadata.is_dir() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture directory is not private",
                ));
            }
            let path = directory.join(STATE_FILE);
            let state = match File::open(path) {
                Ok(file) => {
                    let mut bytes = Vec::new();
                    file.take((MAX_OWNER_BYTES + 1) as u64)
                        .read_to_end(&mut bytes)
                        .map_err(|_| {
                            failure(
                                None,
                                RetryClassification::PermanentRejection,
                                "fixture owner state cannot be read",
                            )
                        })?;
                    if bytes.len() > MAX_OWNER_BYTES {
                        return Err(failure(
                            None,
                            RetryClassification::PermanentRejection,
                            "fixture owner state exceeds its byte bound",
                        ));
                    }
                    serde_json::from_slice(&bytes).map_err(|_| {
                        failure(
                            None,
                            RetryClassification::PermanentRejection,
                            "fixture owner state is corrupt",
                        )
                    })?
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    StoredState::default()
                }
                Err(_) => {
                    return Err(failure(
                        None,
                        RetryClassification::PermanentRejection,
                        "fixture owner state cannot be read",
                    ));
                }
            };
            validate_state(&state)?;
            Ok(Self {
                directory: directory.to_owned(),
                state,
                crash_cut: None,
            })
        }

        pub(super) fn inject_crash_cut_once(&mut self, cut: CrashCut) {
            self.crash_cut = Some(cut);
        }

        fn persist(&self, cursor: DurableCursor) -> Result<(), OwnerCommitFailure> {
            let bytes = serde_json::to_vec(&self.state).map_err(|_| {
                failure(
                    None,
                    RetryClassification::RetrySameEventId,
                    "fixture owner state cannot be encoded",
                )
            })?;
            if bytes.len() > MAX_OWNER_BYTES {
                return Err(failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner state exceeds its byte bound",
                ));
            }
            let staged = self.directory.join(STAGED_STATE_FILE);
            let target = self.directory.join(STATE_FILE);
            let _ = fs::remove_file(&staged);
            let mut options = OpenOptions::new();
            options.create_new(true).write(true).mode(0o600);
            let mut file = options.open(&staged).map_err(|_| {
                failure(
                    None,
                    RetryClassification::RetrySameEventId,
                    "fixture owner cannot stage durable state",
                )
            })?;
            file.write_all(&bytes)
                .and_then(|()| file.sync_all())
                .map_err(|_| {
                    failure(
                        None,
                        RetryClassification::RetrySameEventId,
                        "fixture owner cannot sync durable state",
                    )
                })?;
            fs::rename(&staged, &target).map_err(|_| {
                failure(
                    None,
                    RetryClassification::RetrySameEventId,
                    "fixture owner cannot publish durable state",
                )
            })?;
            File::open(&self.directory)
                .and_then(|directory| directory.sync_all())
                .map_err(|_| {
                    failure(
                        Some(cursor),
                        RetryClassification::RetrySameEventId,
                        "fixture owner committed state but cannot sync its directory",
                    )
                })
        }
    }

    impl CanonicalEventOwner for FixtureEventOwner {
        fn commit_exact(
            &mut self,
            event_id: &str,
            ingress_bytes: &[u8],
        ) -> Result<OwnerCommit, OwnerCommitFailure> {
            if ingress_bytes.len() > MAX_OWNER_BYTES {
                return Err(failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner ingress exceeds its byte bound",
                ));
            }
            validate_new_id(event_id).map_err(|_| {
                failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner rejected event id",
                )
            })?;
            let decoded = decode_event_line(ingress_bytes).map_err(|_| {
                failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner rejected ingress bytes",
                )
            })?;
            if decoded.record.id != event_id {
                return Err(failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "event id does not match ingress bytes",
                ));
            }
            if let Some(existing) = self
                .state
                .events
                .iter()
                .find(|existing| existing.event_id == event_id)
            {
                if existing.ingress_bytes != ingress_bytes {
                    return Err(failure(
                        Some(DurableCursor(existing.cursor)),
                        RetryClassification::PermanentRejection,
                        "event id was reused with different ingress bytes",
                    ));
                }
                return Ok(OwnerCommit {
                    event: owned(existing),
                    freshness: CommitFreshness::AlreadyCommitted,
                    retry: RetryClassification::NoRetryRequired,
                });
            }
            if self.state.events.len() == MAX_OWNER_ENTRIES {
                return Err(failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner entry bound reached",
                ));
            }
            let cursor = self
                .state
                .events
                .last()
                .map_or(Some(1), |event| event.cursor.checked_add(1))
                .ok_or_else(|| {
                    failure(
                        None,
                        RetryClassification::PermanentRejection,
                        "fixture owner cursor exhausted",
                    )
                })?;
            if decoded.record.sequence != cursor {
                return Err(failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "event sequence does not match durable cursor",
                ));
            }
            let crash_cut = self.crash_cut.take();
            if crash_cut == Some(CrashCut::BeforeDurableCommit) {
                return Err(failure(
                    None,
                    RetryClassification::RetrySameEventId,
                    "injected crash before durable commit",
                ));
            }
            self.state.events.push(StoredEvent {
                cursor,
                event_id: event_id.to_owned(),
                ingress_bytes: ingress_bytes.to_vec(),
            });
            if let Err(error) = self.persist(DurableCursor(cursor)) {
                if error.cursor.is_none() {
                    self.state.events.pop();
                }
                return Err(error);
            }
            if crash_cut == Some(CrashCut::AfterDurableCommitBeforeAcknowledgement) {
                return Err(failure(
                    Some(DurableCursor(cursor)),
                    RetryClassification::RetrySameEventId,
                    "injected crash after durable commit",
                ));
            }
            let event = self.state.events.last().ok_or_else(|| {
                failure(
                    None,
                    RetryClassification::RetrySameEventId,
                    "fixture owner lost committed state",
                )
            })?;
            Ok(OwnerCommit {
                event: owned(event),
                freshness: CommitFreshness::New,
                retry: RetryClassification::NoRetryRequired,
            })
        }

        fn replay_page(
            &mut self,
            after: DurableCursor,
            max_events: usize,
            max_bytes: usize,
        ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
            validate_state(&self.state)?;
            let high_water =
                DurableCursor(self.state.events.last().map_or(0, |event| event.cursor));
            let mut bytes = 0usize;
            let mut events = Vec::new();
            let mut remaining = false;
            for event in self
                .state
                .events
                .iter()
                .filter(|event| event.cursor > after.0)
            {
                let next_bytes = bytes.saturating_add(event.ingress_bytes.len());
                if events.len() == max_events || (!events.is_empty() && next_bytes > max_bytes) {
                    remaining = true;
                    break;
                }
                if next_bytes > max_bytes {
                    return Err(failure(
                        Some(DurableCursor(event.cursor)),
                        RetryClassification::PermanentRejection,
                        "fixture replay event exceeds page byte budget",
                    ));
                }
                bytes = next_bytes;
                events.push(owned(event));
            }
            Ok(OwnerReplayPage {
                high_water,
                events,
                has_more: remaining,
            })
        }
    }

    fn validate_state(state: &StoredState) -> Result<(), OwnerCommitFailure> {
        if state.events.len() > MAX_OWNER_ENTRIES {
            return Err(failure(
                None,
                RetryClassification::PermanentRejection,
                "fixture owner state exceeds its entry bound",
            ));
        }
        let mut ids = BTreeSet::new();
        for (index, event) in state.events.iter().enumerate() {
            let expected = i64::try_from(index + 1).map_err(|_| {
                failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner cursor is invalid",
                )
            })?;
            let decoded = decode_event_line(&event.ingress_bytes).map_err(|_| {
                failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner state contains invalid ingress",
                )
            })?;
            if event.cursor != expected
                || decoded.record.sequence != event.cursor
                || decoded.record.id != event.event_id
                || !ids.insert(&event.event_id)
            {
                return Err(failure(
                    None,
                    RetryClassification::PermanentRejection,
                    "fixture owner state is divergent",
                ));
            }
        }
        Ok(())
    }

    fn owned(event: &StoredEvent) -> OwnedEvent {
        OwnedEvent {
            cursor: DurableCursor(event.cursor),
            event_id: event.event_id.clone(),
            ingress_bytes: Arc::from(event.ingress_bytes.clone()),
        }
    }

    fn failure(
        cursor: Option<DurableCursor>,
        retry: RetryClassification,
        reason: &'static str,
    ) -> OwnerCommitFailure {
        OwnerCommitFailure {
            cursor,
            retry,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AUTH_FILE, CLIENT_QUEUE_CAPACITY, CanonicalEventOwner, ClientCancel, CommitFreshness,
        Daemon, DaemonClient, DaemonConfig, DaemonSubscription, DurableCursor, EVENTS_SOCKET,
        IDENTITY_FILE, INGEST_SOCKET, MAX_CONCURRENT_CLIENTS, MAX_HTTP_BODY_BYTES,
        MissionCancellationAcknowledgement, OwnedEvent, OwnerCommit, OwnerCommitFailure,
        OwnerReplayPage, PID_FILE, RetryClassification, StartupFault, admit_client, broadcast,
        constant_time_eq, current_kernel_identity,
        fixture_owner::{CrashCut, FixtureEventOwner},
        lock, sanitize_event_for_network, validate_http_configuration, validate_new_id,
        write_private,
    };
    use orchestrator_core::{EventJsonMap, EventRecord, encode_current_event};
    use serde_json::json;
    use std::{
        collections::BTreeMap,
        fs,
        io::{Read, Write},
        net::TcpStream,
        os::unix::fs::{PermissionsExt, symlink},
        os::unix::net::{UnixListener, UnixStream},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    static NEXT: AtomicU64 = AtomicU64::new(1);

    struct DisposableDirectory(PathBuf);

    impl DisposableDirectory {
        fn new(label: &str) -> Result<Self, std::io::Error> {
            let path = std::env::temp_dir().join(format!(
                "od-{label}-{:x}-{:x}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for DisposableDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn event(id: &str, sequence: i64) -> Result<Vec<u8>, orchestrator_core::EventError> {
        encode_current_event(&EventRecord {
            id: id.to_owned(),
            event_type: "worker.output".into(),
            timestamp: "2026-07-22T00:00:00Z".into(),
            sequence,
            mission_id: "fixture-mission".into(),
            phase_id: None,
            worker_id: None,
            data: Some(EventJsonMap::default()),
            extra: EventJsonMap::default(),
        })
    }

    fn config(root: &Path, api_key: Option<&str>) -> DaemonConfig {
        DaemonConfig {
            root: root.to_owned(),
            port: 0,
            api_key: api_key.map(str::to_owned),
            cf_team: None,
            cf_aud: None,
            allowed_email: None,
        }
    }

    fn start_fixture_daemon(
        config: DaemonConfig,
        owner: Box<dyn CanonicalEventOwner>,
    ) -> Result<Daemon, Box<dyn std::error::Error>> {
        Ok(Daemon::start(config, owner)?)
    }

    fn send_cancellation_payload(
        client: &DaemonClient,
        payload: &[u8],
    ) -> Result<MissionCancellationAcknowledgement, super::DaemonError> {
        let mut stream = client.connect_verified(INGEST_SOCKET, client.ingest_identity)?;
        stream
            .set_nonblocking(true)
            .map_err(|source| super::io_error("set test cancellation nonblocking", source))?;
        let deadline = super::AbsoluteDeadline::after(Duration::from_secs(2))?;
        let mut channel = client.authenticate_stream(&mut stream, "cancel-mission", 0, deadline)?;
        channel.write_frame_before(&mut stream, payload, deadline)?;
        let reply = channel
            .read_frame_before(&mut stream, super::MAX_CANCELLATION_MESSAGE_BYTES, deadline)?
            .ok_or(super::DaemonError::Authentication)?;
        serde_json::from_slice(&reply).map_err(|_| super::DaemonError::CancellationProtocol)
    }

    struct CommitGateOwner {
        inner: FixtureEventOwner,
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
        calls: Arc<AtomicUsize>,
    }

    /// Transport-only owner fake. Production durable cancellation remains an
    /// application adapter responsibility.
    struct CancellationOwner {
        inner: FixtureEventOwner,
        mission_id: String,
        requested: bool,
        calls: Arc<AtomicUsize>,
    }

    impl CanonicalEventOwner for CancellationOwner {
        fn commit_exact(
            &mut self,
            event_id: &str,
            ingress_bytes: &[u8],
        ) -> Result<OwnerCommit, OwnerCommitFailure> {
            self.inner.commit_exact(event_id, ingress_bytes)
        }

        fn replay_page(
            &mut self,
            after: DurableCursor,
            max_events: usize,
            max_bytes: usize,
        ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
            self.inner.replay_page(after, max_events, max_bytes)
        }

        fn request_mission_cancellation(
            &mut self,
            mission_id: &str,
        ) -> MissionCancellationAcknowledgement {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if mission_id != self.mission_id {
                return MissionCancellationAcknowledgement::UnknownMission;
            }
            if self.requested {
                MissionCancellationAcknowledgement::AlreadyRequested
            } else {
                self.requested = true;
                MissionCancellationAcknowledgement::NewlyRequested
            }
        }
    }

    impl CanonicalEventOwner for CommitGateOwner {
        fn commit_exact(
            &mut self,
            event_id: &str,
            ingress_bytes: &[u8],
        ) -> Result<OwnerCommit, OwnerCommitFailure> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
                let _ = self.release.recv();
            }
            self.inner.commit_exact(event_id, ingress_bytes)
        }

        fn replay_page(
            &mut self,
            after: DurableCursor,
            max_events: usize,
            max_bytes: usize,
        ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
            self.inner.replay_page(after, max_events, max_bytes)
        }
    }

    struct SnapshotGateOwner {
        inner: FixtureEventOwner,
        entered: Option<mpsc::Sender<()>>,
        release: mpsc::Receiver<()>,
    }

    struct StaticOwner {
        events: Vec<OwnedEvent>,
        high_water: DurableCursor,
        fabricated_has_more: Option<bool>,
    }

    struct UnboundedPageOwner {
        events: Vec<OwnedEvent>,
        high_water: DurableCursor,
    }

    impl CanonicalEventOwner for UnboundedPageOwner {
        fn commit_exact(
            &mut self,
            _event_id: &str,
            _ingress_bytes: &[u8],
        ) -> Result<OwnerCommit, OwnerCommitFailure> {
            Err(OwnerCommitFailure {
                cursor: None,
                retry: RetryClassification::PermanentRejection,
                reason: "unbounded owner is read only",
            })
        }

        fn replay_page(
            &mut self,
            after: DurableCursor,
            _max_events: usize,
            _max_bytes: usize,
        ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
            Ok(OwnerReplayPage {
                high_water: self.high_water,
                events: self
                    .events
                    .iter()
                    .filter(|event| event.cursor > after)
                    .cloned()
                    .collect(),
                has_more: false,
            })
        }
    }

    impl CanonicalEventOwner for StaticOwner {
        fn commit_exact(
            &mut self,
            _event_id: &str,
            _ingress_bytes: &[u8],
        ) -> Result<OwnerCommit, OwnerCommitFailure> {
            Err(OwnerCommitFailure {
                cursor: None,
                retry: RetryClassification::PermanentRejection,
                reason: "static owner is read only",
            })
        }

        fn replay_page(
            &mut self,
            after: DurableCursor,
            max_events: usize,
            max_bytes: usize,
        ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
            let mut bytes = 0usize;
            let mut events = Vec::new();
            let mut has_more = false;
            for event in self.events.iter().filter(|event| event.cursor > after) {
                let next = bytes.saturating_add(event.ingress_bytes.len());
                if events.len() == max_events || (!events.is_empty() && next > max_bytes) {
                    has_more = true;
                    break;
                }
                bytes = next;
                events.push(event.clone());
            }
            Ok(OwnerReplayPage {
                high_water: self.high_water,
                events,
                has_more: self.fabricated_has_more.unwrap_or(has_more),
            })
        }
    }

    impl CanonicalEventOwner for SnapshotGateOwner {
        fn commit_exact(
            &mut self,
            event_id: &str,
            ingress_bytes: &[u8],
        ) -> Result<OwnerCommit, OwnerCommitFailure> {
            self.inner.commit_exact(event_id, ingress_bytes)
        }

        fn replay_page(
            &mut self,
            after: DurableCursor,
            max_events: usize,
            max_bytes: usize,
        ) -> Result<OwnerReplayPage, OwnerCommitFailure> {
            if let Some(entered) = self.entered.take() {
                let _ = entered.send(());
                let _ = self.release.recv();
            }
            self.inner.replay_page(after, max_events, max_bytes)
        }
    }

    fn request_until(
        address: std::net::SocketAddr,
        request: &[u8],
        marker: &[u8],
    ) -> std::io::Result<Vec<u8>> {
        let mut stream = TcpStream::connect(address)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(request)?;
        let mut response = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    response.extend_from_slice(&chunk[..count]);
                    if response
                        .windows(marker.len())
                        .any(|window| window == marker)
                    {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(response)
    }

    fn assert_cors(response: &[u8], origin: &str) {
        let text = String::from_utf8_lossy(response);
        assert!(text.contains(&format!("Access-Control-Allow-Origin: {origin}\r\n")));
        assert!(text.contains("Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n"));
        assert!(text.contains("Access-Control-Allow-Headers: Authorization, Content-Type, Cf-Access-Jwt-Assertion, Last-Event-ID, Cache-Control\r\n"));
        assert!(text.contains("Access-Control-Allow-Credentials: true\r\n"));
    }

    #[test]
    fn authentication_comparison_and_ids_are_strict() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"other"));
        assert!(validate_new_id("evt_0123456789abcdef").is_ok());
        assert!(validate_new_id("evt_1784678400123456789").is_ok());
        assert!(validate_new_id("evt_0123456789ABCDEF").is_err());
        assert!(validate_new_id("evt_01784678400123456789").is_err());
        let debug = format!(
            "{:?}",
            DaemonConfig {
                root: PathBuf::from("/private/root"),
                port: 1,
                api_key: Some("api-secret".into()),
                cf_team: Some("team.example".into()),
                cf_aud: Some("aud-secret".into()),
                allowed_email: Some("owner-secret@example.com".into()),
            }
        );
        for forbidden in ["api-secret", "aud-secret", "owner-secret@example.com"] {
            assert!(!debug.contains(forbidden));
        }
    }

    #[test]
    fn unauthenticated_socket_never_receives_the_bearer_token() -> TestResult {
        let directory = DisposableDirectory::new("socket-auth-proof")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("bearer-must-never-cross-the-socket")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let (mut attempted, mut replacement) = UnixStream::pair()?;
        attempted.set_nonblocking(true)?;
        let replacement_thread = thread::spawn(move || -> std::io::Result<Vec<u8>> {
            let mut received = [0u8; 64];
            let count = replacement.read(&mut received)?;
            replacement.write_all(
                b"challenge:0000000000000000000000000000000000000000000000000000000000000000:invalid\n",
            )?;
            Ok(received[..count].to_vec())
        });
        assert!(matches!(
            client.authenticate_stream(
                &mut attempted,
                "emit",
                0,
                super::AbsoluteDeadline::after(Duration::from_secs(2))?
            ),
            Err(super::DaemonError::Authentication)
        ));
        let received = replacement_thread
            .join()
            .map_err(|_| "replacement socket thread panicked")??;
        assert_eq!(received, b"hello\n");
        assert!(!String::from_utf8_lossy(&received).contains(&client.token));
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn long_root_supports_authenticated_cancel_live_probe_and_restart() -> TestResult {
        let directory = DisposableDirectory::new("long-control")?;
        let root = directory
            .path()
            .join("long-root-".repeat(20))
            .join("control");
        fs::create_dir_all(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let calls = Arc::new(AtomicUsize::new(0));
        let make_owner = || -> Result<Box<dyn CanonicalEventOwner>, Box<dyn std::error::Error>> {
            Ok(Box::new(CancellationOwner {
                inner: FixtureEventOwner::open(&root).map_err(super::owner_failure)?,
                mission_id: "long-mission".into(),
                requested: false,
                calls: Arc::clone(&calls),
            }))
        };
        let daemon = start_fixture_daemon(config(&root, None), make_owner()?)?;
        let retained = super::admit_existing_root(&root)?;
        assert!(matches!(
            super::remove_stale_socket(&retained, INGEST_SOCKET),
            Err(super::DaemonError::AlreadyRunning)
        ));
        let alias = super::SocketPath::new(&retained, INGEST_SOCKET)?;
        let alias_root = alias
            .path()
            .parent()
            .and_then(Path::parent)
            .ok_or("alias parent")?
            .to_owned();
        assert!(alias.path().as_os_str().len() < 100);
        drop(alias);
        assert!(!alias_root.exists());
        let client = DaemonClient::open(&root)?;
        assert_eq!(
            client.cancel_mission("long-mission")?,
            MissionCancellationAcknowledgement::NewlyRequested
        );
        daemon.shutdown()?;
        // Exercise stale socket removal through the short path as well.
        let stale_path = super::SocketPath::new(&retained, INGEST_SOCKET)?;
        drop(UnixListener::bind(stale_path.path())?);
        drop(stale_path);
        let restarted = start_fixture_daemon(config(&root, None), make_owner()?)?;
        assert_eq!(
            DaemonClient::open(&root)?.cancel_mission("long-mission")?,
            MissionCancellationAcknowledgement::NewlyRequested
        );
        restarted.shutdown()?;
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        Ok(())
    }

    #[test]
    fn short_socket_alias_replacement_is_preserved_and_refused() -> TestResult {
        let directory = DisposableDirectory::new("alias-replacement")?;
        let root = directory.path().join("long-root-".repeat(20));
        fs::create_dir_all(&root)?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let retained = super::admit_existing_root(&root)?;
        let alias = super::SocketPath::new(&retained, INGEST_SOCKET)?;
        let link = alias.path().parent().ok_or("alias link")?.to_owned();
        let alias_root = link.parent().ok_or("alias root")?.to_owned();
        // Retain the original link so its inode cannot be immediately reused.
        fs::rename(&link, alias_root.join("original"))?;
        symlink(directory.path(), &link)?;
        assert!(alias.verify().is_err());
        drop(alias);
        assert!(fs::symlink_metadata(&link)?.file_type().is_symlink());
        assert!(root.is_dir());
        fs::remove_file(link)?;
        fs::remove_file(alias_root.join("original"))?;
        fs::remove_dir(alias_root)?;
        Ok(())
    }

    #[test]
    fn cancellation_forwards_to_owner_without_stopping_or_ingesting() -> TestResult {
        let directory = DisposableDirectory::new("cancel-forward")?;
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CancellationOwner {
            inner: FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?,
            mission_id: "mission-a".into(),
            requested: false,
            calls: Arc::clone(&calls),
        };
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let client = DaemonClient::open(directory.path())?;

        let acknowledgement = client.cancel_mission("mission-a")?;

        assert_eq!(
            acknowledgement,
            MissionCancellationAcknowledgement::NewlyRequested
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(client.identity()?.is_some());
        assert_eq!(client.prepare_subscription_cursor()?, 0);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn cancellation_accepts_maximum_identifier_after_json_escaping() -> TestResult {
        let directory = DisposableDirectory::new("cancel-escaped-bound")?;
        let mission_id = "\0".repeat(super::MAX_MISSION_IDENTIFIER_BYTES);
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CancellationOwner {
            inner: FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?,
            mission_id: mission_id.clone(),
            requested: false,
            calls: Arc::clone(&calls),
        };
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let acknowledgement = DaemonClient::open(directory.path())?.cancel_mission(&mission_id)?;
        assert_eq!(
            acknowledgement,
            MissionCancellationAcknowledgement::NewlyRequested
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn unknown_authenticated_operation_does_not_enter_event_ingestion() -> TestResult {
        let directory = DisposableDirectory::new("unknown-op")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let mut stream = client.connect_verified(INGEST_SOCKET, client.ingest_identity)?;
        stream.set_nonblocking(true)?;
        let deadline = super::AbsoluteDeadline::after(Duration::from_secs(2))?;
        let mut channel = client.authenticate_stream(&mut stream, "unknown", 0, deadline)?;
        let reply = channel.read_frame_before(&mut stream, 64, deadline)?;
        assert_eq!(reply.as_deref(), Some(b"invalid-operation".as_slice()));
        assert_eq!(client.prepare_subscription_cursor()?, 0);
        assert!(client.identity()?.is_some());
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn cancellation_repeats_are_forwarded_for_owner_idempotency() -> TestResult {
        let directory = DisposableDirectory::new("cancel-repeat")?;
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CancellationOwner {
            inner: FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?,
            mission_id: "mission-a".into(),
            requested: false,
            calls: Arc::clone(&calls),
        };
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let client = DaemonClient::open(directory.path())?;

        let first = client.cancel_mission("mission-a")?;
        let repeated = client.cancel_mission("mission-a")?;

        assert_eq!(first, MissionCancellationAcknowledgement::NewlyRequested);
        assert_eq!(
            repeated,
            MissionCancellationAcknowledgement::AlreadyRequested
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn cancellation_refuses_foreign_and_unsupported_missions() -> TestResult {
        let foreign_directory = DisposableDirectory::new("cancel-foreign")?;
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CancellationOwner {
            inner: FixtureEventOwner::open(foreign_directory.path())
                .map_err(super::owner_failure)?,
            mission_id: "mission-a".into(),
            requested: false,
            calls: Arc::clone(&calls),
        };
        let foreign_daemon = start_fixture_daemon(
            config(foreign_directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let foreign = DaemonClient::open(foreign_directory.path())?.cancel_mission("mission-b")?;
        assert_eq!(foreign, MissionCancellationAcknowledgement::UnknownMission);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        foreign_daemon.shutdown()?;

        let unsupported_directory = DisposableDirectory::new("cancel-unsupported")?;
        let unsupported_daemon = start_fixture_daemon(
            config(unsupported_directory.path(), Some("fixture-secret")),
            Box::new(
                FixtureEventOwner::open(unsupported_directory.path())
                    .map_err(super::owner_failure)?,
            ),
        )?;
        let unsupported =
            DaemonClient::open(unsupported_directory.path())?.cancel_mission("mission-a")?;
        assert_eq!(unsupported, MissionCancellationAcknowledgement::Unsupported);
        unsupported_daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn cancellation_rejects_malformed_and_oversized_authenticated_requests() -> TestResult {
        let directory = DisposableDirectory::new("cancel-invalid")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;

        let malformed =
            send_cancellation_payload(&client, br#"{"mission_id":"mission-a","unknown":true}"#);
        let oversized = send_cancellation_payload(
            &client,
            &vec![b'x'; super::MAX_CANCELLATION_MESSAGE_BYTES + 1],
        );

        assert!(matches!(
            malformed,
            Err(super::DaemonError::CancellationProtocol)
        ));
        assert!(matches!(
            oversized,
            Err(super::DaemonError::CancellationProtocol)
        ));
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn unauthenticated_cancellation_is_denied_before_owner_access() -> TestResult {
        let directory = DisposableDirectory::new("cancel-unauthenticated")?;
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CancellationOwner {
            inner: FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?,
            mission_id: "mission-a".into(),
            requested: false,
            calls: Arc::clone(&calls),
        };
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let mut stream = UnixStream::connect(directory.path().join(INGEST_SOCKET))?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.write_all(b"hello\n")?;
        let mut challenge = [0u8; 256];
        let _ = stream.read(&mut challenge)?;
        stream.write_all(b"{\"auth\":\"invalid\",\"op\":\"cancel-mission\",\"cursor\":0}\n")?;
        let mut reply = [0u8; 32];
        let count = stream.read(&mut reply)?;

        assert_eq!(&reply[..count], b"unauthorized\n");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn relayed_secure_frames_hide_reject_tampering_and_replay() -> TestResult {
        let nonce = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let mut client = super::SecureChannel::client("fixture-session-key", nonce);
        let mut server = super::SecureChannel::server("fixture-session-key", nonce);
        let secret = b"top-level-secret-and-event-payload";
        let (counter, ciphertext, tag) = client.seal(secret)?;
        assert_ne!(ciphertext, secret);
        assert!(
            !ciphertext
                .windows(secret.len())
                .any(|value| value == secret)
        );
        assert_eq!(server.open(counter, &ciphertext, &tag)?, secret);
        assert!(matches!(
            server.open(counter, &ciphertext, &tag),
            Err(super::DaemonError::Authentication)
        ));

        let (counter, mut ciphertext, tag) = client.seal(secret)?;
        ciphertext[0] ^= 1;
        assert!(matches!(
            server.open(counter, &ciphertext, &tag),
            Err(super::DaemonError::Authentication)
        ));
        Ok(())
    }

    #[test]
    fn authenticated_stop_has_a_bounded_nonresponding_peer_deadline() -> TestResult {
        let directory = DisposableDirectory::new("stop-deadline")?;
        write_private(directory.path(), AUTH_FILE, b"fixture-secret")?;
        let observed = current_kernel_identity(std::process::id())?;
        let identity = super::DaemonIdentity {
            pid: observed.pid(),
            process_group_id: observed.process_group_id(),
            start_identity: observed.process_start_identity().to_owned(),
            port: 0,
        };
        write_private(
            directory.path(),
            IDENTITY_FILE,
            &serde_json::to_vec(&identity)?,
        )?;
        write_private(
            directory.path(),
            PID_FILE,
            identity.pid.to_string().as_bytes(),
        )?;
        let listener = UnixListener::bind(directory.path().join(INGEST_SOCKET))?;
        let _events_listener = UnixListener::bind(directory.path().join(EVENTS_SOCKET))?;
        let (accepted_tx, accepted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let peer = thread::spawn(move || {
            if let Ok((_stream, _)) = listener.accept() {
                let _ = accepted_tx.send(());
                let _ = release_rx.recv();
            }
        });
        let client = DaemonClient::open(directory.path())?;
        let (result_tx, result_rx) = mpsc::channel();
        let stop = thread::spawn(move || {
            let _ = result_tx.send(client.stop());
        });
        accepted_rx.recv_timeout(Duration::from_secs(1))?;
        let result = result_rx.recv_timeout(Duration::from_secs(3))?;
        assert!(matches!(
            result,
            Err(super::DaemonError::Io { source, .. })
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                )
        ));
        release_tx.send(())?;
        stop.join().map_err(|_| "stop client thread panicked")?;
        peer.join().map_err(|_| "stop peer thread panicked")?;
        Ok(())
    }

    #[test]
    fn unsupported_remote_start_has_zero_side_effects() -> TestResult {
        let parent = DisposableDirectory::new("production-refusal")?;
        let root = parent.path().join("must-not-exist");
        let owner = FixtureEventOwner::open(parent.path()).map_err(super::owner_failure)?;
        let result = Daemon::start(
            DaemonConfig {
                root: root.clone(),
                port: 0,
                api_key: Some("key".into()),
                cf_team: Some("team.example".into()),
                cf_aud: Some("aud".into()),
                allowed_email: Some("owner@example.com".into()),
            },
            Box::new(owner),
        );
        assert!(matches!(
            result,
            Err(super::DaemonError::UnsupportedConfiguration(_))
        ));
        assert!(!root.exists());
        Ok(())
    }

    #[test]
    fn fixture_owner_is_exact_idempotent_bounded_and_restartable() -> TestResult {
        let directory = DisposableDirectory::new("owner")?;
        let first = event("evt_0000000000000001", 1)?;
        let fallback = event("evt_1784678400123456789", 2)?;
        let mut owner = FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?;
        let committed = owner
            .commit_exact("evt_0000000000000001", &first)
            .map_err(super::owner_failure)?;
        assert_eq!(committed.event.cursor, DurableCursor(1));
        assert_eq!(committed.event.ingress_bytes.as_ref(), first.as_slice());
        let repeated = owner
            .commit_exact("evt_0000000000000001", &committed.event.ingress_bytes)
            .map_err(super::owner_failure)?;
        assert_eq!(repeated.event.cursor, DurableCursor(1));
        assert_eq!(repeated.freshness, CommitFreshness::AlreadyCommitted);
        let decimal = owner
            .commit_exact("evt_1784678400123456789", &fallback)
            .map_err(super::owner_failure)?;
        assert_eq!(decimal.event.cursor, DurableCursor(2));
        drop(owner);

        let mut recovered =
            FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?;
        let snapshot = recovered
            .replay_page(DurableCursor::origin(), usize::MAX, usize::MAX)
            .map_err(super::owner_failure)?;
        assert_eq!(snapshot.high_water, DurableCursor(2));
        assert_eq!(snapshot.events.len(), 2);
        let replay = recovered
            .replay_page(DurableCursor(1), usize::MAX, usize::MAX)
            .map_err(super::owner_failure)?;
        assert_eq!(
            replay.events.into_iter().collect::<Vec<_>>(),
            vec![decimal.event]
        );
        let conflicting = event("evt_0000000000000001", 42)?;
        let Err(rejection) = recovered.commit_exact("evt_0000000000000001", &conflicting) else {
            return Err(
                std::io::Error::other("different bytes reused an existing event id").into(),
            );
        };
        assert_eq!(rejection.cursor, Some(DurableCursor(1)));
        assert_eq!(rejection.retry, RetryClassification::PermanentRejection);

        for sequence in 3..=8 {
            let id = format!("evt_{sequence:016x}");
            let bytes = event(&id, sequence)?;
            recovered
                .commit_exact(&id, &bytes)
                .map_err(super::owner_failure)?;
        }
        let ninth = event("evt_0000000000000009", 9)?;
        let Err(full) = recovered.commit_exact("evt_0000000000000009", &ninth) else {
            return Err(std::io::Error::other("owner exceeded its durable entry bound").into());
        };
        assert_eq!(full.retry, RetryClassification::PermanentRejection);
        Ok(())
    }

    #[test]
    fn fixture_owner_classifies_both_crash_cuts_and_post_commit_retry() -> TestResult {
        let directory = DisposableDirectory::new("crash-cuts")?;
        let first = event("evt_0000000000000001", 1)?;
        let second = event("evt_0000000000000002", 2)?;
        let mut owner = FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?;

        owner.inject_crash_cut_once(CrashCut::BeforeDurableCommit);
        let Err(before) = owner.commit_exact("evt_0000000000000001", &first) else {
            return Err(std::io::Error::other("pre-commit cut returned success").into());
        };
        assert_eq!(before.cursor, None);
        assert_eq!(before.retry, RetryClassification::RetrySameEventId);
        assert!(
            owner
                .replay_page(DurableCursor::origin(), usize::MAX, usize::MAX)
                .map_err(super::owner_failure)?
                .events
                .is_empty()
        );
        assert_eq!(
            owner
                .commit_exact("evt_0000000000000001", &first)
                .map_err(super::owner_failure)?
                .event
                .cursor,
            DurableCursor(1)
        );

        owner.inject_crash_cut_once(CrashCut::AfterDurableCommitBeforeAcknowledgement);
        let Err(after) = owner.commit_exact("evt_0000000000000002", &second) else {
            return Err(std::io::Error::other("post-commit cut returned success").into());
        };
        assert_eq!(after.cursor, Some(DurableCursor(2)));
        assert_eq!(after.retry, RetryClassification::RetrySameEventId);
        drop(owner);

        let mut recovered =
            FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?;
        let retry = recovered
            .commit_exact("evt_0000000000000002", &second)
            .map_err(super::owner_failure)?;
        assert_eq!(retry.event.cursor, DurableCursor(2));
        assert_eq!(retry.freshness, CommitFreshness::AlreadyCommitted);
        assert_eq!(retry.event.ingress_bytes.as_ref(), second.as_slice());
        Ok(())
    }

    #[test]
    fn fixture_daemon_replays_exact_ingress_across_restart() -> TestResult {
        let directory = DisposableDirectory::new("transport")?;
        let first = event("evt_0000000000000001", 1)?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let mut subscription = client.subscribe(0)?;
        subscription.set_read_timeout(Some(Duration::from_secs(2)))?;
        client.emit(&first)?;
        let live = subscription
            .read_event()?
            .ok_or_else(|| std::io::Error::other("live event stream closed"))?;
        assert_eq!(live.cursor, 1);
        assert_eq!(live.ingress_bytes, first);
        drop(subscription);
        daemon.shutdown()?;

        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret-2")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let mut subscription = client.subscribe(0)?;
        subscription.set_read_timeout(Some(Duration::from_secs(2)))?;
        let replay = subscription
            .read_event()?
            .ok_or_else(|| std::io::Error::other("replay event stream closed"))?;
        assert_eq!(replay.cursor, 1);
        assert_eq!(replay.ingress_bytes, first);
        let second = event("evt_0000000000000002", 2)?;
        client.emit(&second)?;
        let live = subscription
            .read_event()?
            .ok_or_else(|| std::io::Error::other("second live event stream closed"))?;
        assert_eq!(live.cursor, 2);
        assert_eq!(live.ingress_bytes, second);
        assert!(!directory.path().join("events").exists());
        assert!(!directory.path().join("daemon-replay.jsonl").exists());
        drop(subscription);
        daemon.shutdown()?;

        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret-3")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let mut resumed = DaemonClient::open(directory.path())?.subscribe(1)?;
        resumed.set_read_timeout(Some(Duration::from_secs(2)))?;
        let replay = resumed
            .read_event()?
            .ok_or_else(|| std::io::Error::other("cursor replay event stream closed"))?;
        assert_eq!(replay.cursor, 2);
        assert_eq!(replay.ingress_bytes, second);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn fixture_daemon_delivers_post_commit_retry_once_with_owner_cursor() -> TestResult {
        let directory = DisposableDirectory::new("post-commit-transport")?;
        let first = event("evt_0000000000000001", 1)?;
        let mut owner = FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?;
        owner.inject_crash_cut_once(CrashCut::AfterDurableCommitBeforeAcknowledgement);
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let mut subscription = client.subscribe(0)?;
        subscription.set_read_timeout(Some(Duration::from_secs(2)))?;

        let Err(error) = client.emit(&first) else {
            return Err(std::io::Error::other("post-commit cut returned success").into());
        };
        assert!(matches!(
            error,
            super::DaemonError::EventOwnerRetrySameEventId {
                durable_cursor: Some(1),
                ..
            }
        ));
        let delivered = subscription
            .read_event()?
            .ok_or_else(|| std::io::Error::other("post-commit event stream closed"))?;
        assert_eq!(delivered.cursor, 1);
        assert_eq!(delivered.ingress_bytes, first);

        client.emit(&first)?;
        subscription.set_read_timeout(Some(Duration::from_millis(100)))?;
        let retry_delivery = subscription.read_event();
        assert!(matches!(
            retry_delivery,
            Err(super::DaemonError::Io { source, .. })
                if matches!(
                    source.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                )
        ));
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn go_compatible_network_sanitization_preserves_owner_bytes() -> TestResult {
        let data = json!({
            "password": "plain-password",
            "nested": {
                "token": "plain nested value",
                "api": "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz123456",
                "deeper": [{"pem": "-----BEGIN PRIVATE KEY-----\nbody"}]
            },
            "api": "key=sk-ant-api03-abcdef1234567890XYZ",
            "bearer": "Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9",
            "pem": "-----BEGIN RSA PRIVATE KEY-----",
            "entropy": "aB3!dEf7gHiJkLmNoPqRsTuVwXyZ0@#$",
            "task": "abcdefghijklmnopqrstuvwxyz"
        });
        let data = data
            .as_object()
            .ok_or("fixture data is not an object")?
            .clone()
            .into_iter()
            .collect::<BTreeMap<_, _>>();
        let record = EventRecord {
            id: "evt_0000000000000001".into(),
            event_type: "worker.output".into(),
            timestamp: "2026-07-22T00:00:00Z".into(),
            sequence: 1,
            mission_id: "fixture-mission".into(),
            phase_id: None,
            worker_id: None,
            data: Some(EventJsonMap::from(data)),
            extra: EventJsonMap::default(),
        };
        let forensic = encode_current_event(&record)?;
        let sanitized = sanitize_event_for_network(&record);
        let sanitized_data = sanitized.data.as_ref().ok_or("sanitized data missing")?;
        assert_eq!(sanitized_data.get("password"), Some(&json!("[REDACTED]")));
        assert_eq!(sanitized_data.get("api"), Some(&json!("key=[REDACTED]")));
        assert_eq!(
            sanitized_data.get("bearer"),
            Some(&json!("Authorization: [REDACTED]"))
        );
        assert_eq!(sanitized_data.get("pem"), Some(&json!("[REDACTED]")));
        assert_eq!(sanitized_data.get("entropy"), Some(&json!("[REDACTED]")));
        assert_eq!(
            sanitized_data.get("task"),
            Some(&json!("abcdefghijklmnopqrstuvwxyz"))
        );
        assert_eq!(
            sanitized_data.get("nested"),
            Some(&json!({
                "token": "[REDACTED]",
                "api": "[REDACTED]",
                "deeper": [{"pem": "[REDACTED]\nbody"}]
            }))
        );

        let directory = DisposableDirectory::new("sanitized-replay")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("http-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        client.emit(&forensic)?;
        let mut replay = client.subscribe(0)?;
        replay.set_read_timeout(Some(Duration::from_secs(2)))?;
        assert_eq!(
            replay
                .read_event()?
                .ok_or("owner replay ended")?
                .ingress_bytes,
            forensic
        );

        let response = request_until(
            daemon.address(),
            b"GET /api/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer http-secret\r\nLast-Event-ID: 0\r\n\r\n",
            b"\n\n",
        )?;
        let response_text = String::from_utf8(response)?;
        assert!(response_text.contains("event: worker.output\n"));
        assert!(response_text.contains("[REDACTED]"));
        for forbidden in [
            "plain-password",
            "sk-ant-api03",
            "ghp_AbCd",
            "eyJhbGci",
            "-----BEGIN",
            "aB3!dEf7",
        ] {
            assert!(!response_text.contains(forbidden), "SSE leaked {forbidden}");
        }
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn unknown_top_level_secrets_are_dropped_and_sse_controls_never_commit() -> TestResult {
        let mut extra = BTreeMap::new();
        extra.insert(
            "top_level_secret".to_owned(),
            json!("TOP-LEVEL-SECRET-MUST-NOT-ESCAPE"),
        );
        extra.insert("event".to_owned(), json!("fake\nid: injected"));
        let nested = json!({
            "visible": "ok",
            "nested": {"credential": "NESTED-SECRET-MUST-NOT-ESCAPE"}
        })
        .as_object()
        .ok_or("nested fixture is not an object")?
        .clone()
        .into_iter()
        .collect::<BTreeMap<_, _>>();
        let record = EventRecord {
            id: "evt_0000000000000001".into(),
            event_type: "worker.output".into(),
            timestamp: "2026-07-22T00:00:00Z".into(),
            sequence: 1,
            mission_id: "fixture-mission".into(),
            phase_id: None,
            worker_id: Some("Bearer known-field-secret-1234567890".into()),
            data: Some(EventJsonMap::from(nested)),
            extra: EventJsonMap::from(extra),
        };
        let forensic = encode_current_event(&record)?;
        let directory = DisposableDirectory::new("unknown-secret")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("http-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        client.emit(&forensic)?;
        let response = request_until(
            daemon.address(),
            b"GET /api/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer http-secret\r\nLast-Event-ID: 0\r\n\r\n",
            b"\n\n",
        )?;
        let text = String::from_utf8(response)?;
        assert!(text.contains("\"visible\":\"ok\""));
        assert!(text.contains("\"credential\":\"[REDACTED]\""));
        for forbidden in [
            "top_level_secret",
            "TOP-LEVEL-SECRET-MUST-NOT-ESCAPE",
            "NESTED-SECRET-MUST-NOT-ESCAPE",
            "known-field-secret-1234567890",
            "fake\\nid: injected",
        ] {
            assert!(!text.contains(forbidden), "SSE leaked {forbidden}");
        }

        let injected = encode_current_event(&EventRecord {
            id: "evt_0000000000000002".into(),
            event_type: "worker.output\nid: attacker".into(),
            timestamp: "2026-07-22T00:00:00Z".into(),
            sequence: 2,
            mission_id: "fixture-mission".into(),
            phase_id: None,
            worker_id: None,
            data: None,
            extra: EventJsonMap::default(),
        })?;
        assert!(matches!(
            client.emit(&injected),
            Err(super::DaemonError::InvalidEvent(_))
        ));
        let mut replay = client.subscribe(1)?;
        replay.set_read_timeout(Some(Duration::from_millis(100)))?;
        assert!(replay.read_event().is_err(), "injected event was committed");
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn cursor_owner_and_replay_budget_failures_are_typed_before_enrollment() -> TestResult {
        let directory = DisposableDirectory::new("cursor-mismatch")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("http-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        assert!(matches!(
            client.subscribe(1),
            Err(super::DaemonError::CursorMismatch {
                requested: 1,
                high_water: 0
            })
        ));
        let response = request_until(
            daemon.address(),
            b"GET /api/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer http-secret\r\nLast-Event-ID: 1\r\n\r\n",
            b"\r\n\r\n",
        )?;
        assert!(response.starts_with(b"HTTP/1.1 409 Conflict\r\n"));
        daemon.shutdown()?;

        let fabricated_directory = DisposableDirectory::new("owner-fabrication")?;
        let fabricated = OwnedEvent {
            cursor: DurableCursor(2),
            event_id: "evt_0000000000000002".into(),
            ingress_bytes: Arc::from(event("evt_0000000000000002", 2)?),
        };
        let daemon = start_fixture_daemon(
            config(fabricated_directory.path(), Some("http-secret")),
            Box::new(StaticOwner {
                events: vec![fabricated],
                high_water: DurableCursor(2),
                fabricated_has_more: Some(false),
            }),
        )?;
        assert!(matches!(
            DaemonClient::open(fabricated_directory.path())?.subscribe(0),
            Err(super::DaemonError::EnrollmentRejected)
        ));
        let response = request_until(
            daemon.address(),
            b"GET /api/events HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer http-secret\r\nLast-Event-ID: 0\r\n\r\n",
            b"subscription_enrollment_failed",
        )?;
        assert!(response.starts_with(b"HTTP/1.1 503 Service Unavailable\r\n"));
        assert!(String::from_utf8_lossy(&response).contains("subscription_enrollment_failed"));
        daemon.shutdown()?;

        let page_directory = DisposableDirectory::new("owner-page-limit")?;
        let mut page_events = Vec::new();
        for sequence in 1..=super::MAX_REPLAY_PAGE_EVENTS + 1 {
            let sequence = i64::try_from(sequence)?;
            let id = format!("evt_{sequence:016x}");
            page_events.push(OwnedEvent {
                cursor: DurableCursor(sequence),
                event_id: id.clone(),
                ingress_bytes: Arc::from(event(&id, sequence)?),
            });
        }
        let high_water = page_events.last().ok_or("page events missing")?.cursor;
        let daemon = start_fixture_daemon(
            config(page_directory.path(), None),
            Box::new(UnboundedPageOwner {
                events: page_events,
                high_water,
            }),
        )?;
        assert!(matches!(
            DaemonClient::open(page_directory.path())?.subscribe(0),
            Err(super::DaemonError::EnrollmentRejected)
        ));
        daemon.shutdown()?;

        let page_bytes_directory = DisposableDirectory::new("owner-page-byte-limit")?;
        let mut large_page = Vec::new();
        for sequence in 1..=2 {
            let id = format!("evt_{sequence:016x}");
            let mut data = BTreeMap::new();
            data.insert(
                "task".to_owned(),
                json!("x".repeat(super::MAX_REPLAY_PAGE_BYTES / 2)),
            );
            let ingress = encode_current_event(&EventRecord {
                id: id.clone(),
                event_type: "worker.output".into(),
                timestamp: "2026-07-22T00:00:00Z".into(),
                sequence,
                mission_id: "fixture-mission".into(),
                phase_id: None,
                worker_id: None,
                data: Some(EventJsonMap::from(data)),
                extra: EventJsonMap::default(),
            })?;
            large_page.push(OwnedEvent {
                cursor: DurableCursor(sequence),
                event_id: id,
                ingress_bytes: Arc::from(ingress),
            });
        }
        assert!(
            large_page
                .iter()
                .map(|event| event.ingress_bytes.len())
                .sum::<usize>()
                > super::MAX_REPLAY_PAGE_BYTES
        );
        let daemon = start_fixture_daemon(
            config(page_bytes_directory.path(), None),
            Box::new(UnboundedPageOwner {
                events: large_page,
                high_water: DurableCursor(2),
            }),
        )?;
        assert!(matches!(
            DaemonClient::open(page_bytes_directory.path())?.subscribe(0),
            Err(super::DaemonError::EnrollmentRejected)
        ));
        daemon.shutdown()?;

        let budget_directory = DisposableDirectory::new("replay-budget")?;
        let mut events = Vec::new();
        for sequence in 1..=super::MAX_TOTAL_REPLAY_EVENTS + 1 {
            let sequence = i64::try_from(sequence)?;
            let id = format!("evt_{sequence:016x}");
            events.push(OwnedEvent {
                cursor: DurableCursor(sequence),
                event_id: id.clone(),
                ingress_bytes: Arc::from(event(&id, sequence)?),
            });
        }
        let high_water = events.last().ok_or("budget events missing")?.cursor;
        let daemon = start_fixture_daemon(
            config(budget_directory.path(), None),
            Box::new(StaticOwner {
                events,
                high_water,
                fabricated_has_more: None,
            }),
        )?;
        assert!(matches!(
            DaemonClient::open(budget_directory.path())?.subscribe(0),
            Err(super::DaemonError::ReplayBudgetExceeded)
        ));
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn fixture_http_is_always_authenticated_and_remote_modes_refuse_before_startup() -> TestResult {
        let generated_key_directory = DisposableDirectory::new("http-generated-key")?;
        let daemon = start_fixture_daemon(
            config(generated_key_directory.path(), None),
            Box::new(
                FixtureEventOwner::open(generated_key_directory.path())
                    .map_err(super::owner_failure)?,
            ),
        )?;
        assert!(daemon.address().ip().is_loopback());
        assert!(
            request_until(
                daemon.address(),
                b"GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
                b"\r\n\r\n"
            )?
            .starts_with(b"HTTP/1.1 401")
        );
        let generated_request = format!(
            "GET /missing HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\n\r\n",
            daemon.token()
        );
        assert!(
            request_until(daemon.address(), generated_request.as_bytes(), b"\r\n\r\n")?
                .starts_with(b"HTTP/1.1 404")
        );
        daemon.shutdown()?;

        let key_directory = DisposableDirectory::new("http-local-key")?;
        validate_http_configuration(&config(key_directory.path(), Some("http-secret")))?;
        let daemon = start_fixture_daemon(
            config(key_directory.path(), Some("http-secret")),
            Box::new(FixtureEventOwner::open(key_directory.path()).map_err(super::owner_failure)?),
        )?;
        assert!(
            request_until(
                daemon.address(),
                b"GET /missing HTTP/1.1\r\nHost: localhost\r\n\r\n",
                b"\r\n\r\n"
            )?
            .starts_with(b"HTTP/1.1 401")
        );
        assert!(
            request_until(daemon.address(), b"GET /missing HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer http-secret\r\n\r\n", b"\r\n\r\n")?
                .starts_with(b"HTTP/1.1 404")
        );
        daemon.shutdown()?;

        for (label, cf_team, cf_aud, allowed_email, api_key) in [
            ("partial-team", Some("team.example"), None, None, None),
            ("partial-aud", None, Some("aud"), None, Some("key")),
            ("partial-email", None, None, Some("owner@example.com"), None),
            (
                "full-remote",
                Some("team.example"),
                Some("aud"),
                Some("owner@example.com"),
                Some("key"),
            ),
        ] {
            let directory = DisposableDirectory::new(label)?;
            let mut remote = config(directory.path(), api_key);
            remote.cf_team = cf_team.map(str::to_owned);
            remote.cf_aud = cf_aud.map(str::to_owned);
            remote.allowed_email = allowed_email.map(str::to_owned);
            let owner = FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?;
            assert!(matches!(
                Daemon::start(remote, Box::new(owner)),
                Err(super::DaemonError::UnsupportedConfiguration(_))
            ));
            for name in ["daemon.sock", "events.sock", "daemon.pid", "daemon.auth"] {
                assert!(!directory.path().join(name).exists());
            }
        }
        Ok(())
    }

    #[test]
    fn fixture_cors_preflight_is_unauthenticated_exact_and_origin_closed() -> TestResult {
        let directory = DisposableDirectory::new("http-cors")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("http-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        for origin in [
            "http://localhost",
            "http://localhost:3000",
            "http://127.0.0.1:7331",
        ] {
            let request = format!(
                "OPTIONS /api/events HTTP/1.1\r\nHost: localhost\r\nOrigin: {origin}\r\n\r\n"
            );
            let response = request_until(daemon.address(), request.as_bytes(), b"\r\n\r\n")?;
            assert!(response.starts_with(b"HTTP/1.1 204 No Content\r\n"));
            assert_cors(&response, origin);
            assert!(!String::from_utf8_lossy(&response).contains("unauthorized"));
        }

        let default_origin = request_until(
            daemon.address(),
            b"OPTIONS /api/events HTTP/1.1\r\nHost: localhost\r\n\r\n",
            b"\r\n\r\n",
        )?;
        assert_cors(&default_origin, "http://localhost");

        let attacker = request_until(
            daemon.address(),
            b"OPTIONS /api/events HTTP/1.1\r\nHost: localhost\r\nOrigin: https://attacker.invalid\r\n\r\n",
            b"\r\n\r\n",
        )?;
        assert!(attacker.starts_with(b"HTTP/1.1 403 Forbidden\r\n"));
        assert!(
            !String::from_utf8_lossy(&attacker)
                .contains("Access-Control-Allow-Origin: https://attacker.invalid")
        );
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn shutdown_waits_for_in_flight_owner_and_refuses_all_later_requests() -> TestResult {
        let directory = DisposableDirectory::new("shutdown-in-flight")?;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CommitGateOwner {
            inner: FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?,
            entered: Some(entered_tx),
            release: release_rx,
            calls: Arc::clone(&calls),
        };
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let event_bytes = event("evt_0000000000000001", 1)?;
        let emit_client = client.clone();
        let emitter = thread::spawn(move || emit_client.emit(&event_bytes));
        entered_rx.recv_timeout(Duration::from_secs(2))?;

        let (stopped_tx, stopped_rx) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            let result = daemon.shutdown();
            let _ = stopped_tx.send(result);
        });
        assert!(matches!(
            stopped_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        release_tx.send(())?;
        stopped_rx.recv_timeout(Duration::from_secs(2))??;
        shutdown.join().map_err(|_| "shutdown thread panicked")?;
        let _ = emitter.join().map_err(|_| "emitter thread panicked")?;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(client.emit(&event("evt_0000000000000002", 2)?).is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for name in [
            "daemon.sock",
            "events.sock",
            "daemon.pid",
            "daemon.identity.json",
            "daemon.auth",
        ] {
            assert!(!directory.path().join(name).exists(), "retained {name}");
        }
        Ok(())
    }

    #[test]
    fn startup_failure_joins_in_flight_client_before_verified_cleanup() -> TestResult {
        let directory = DisposableDirectory::new("startup-in-flight")?;
        let (owner_entered_tx, owner_entered_rx) = mpsc::channel();
        let (owner_release_tx, owner_release_rx) = mpsc::channel();
        let calls = Arc::new(AtomicUsize::new(0));
        let owner = CommitGateOwner {
            inner: FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?,
            entered: Some(owner_entered_tx),
            release: owner_release_rx,
            calls: Arc::clone(&calls),
        };
        let (listeners_entered_tx, listeners_entered_rx) = mpsc::channel();
        let (startup_release_tx, startup_release_rx) = mpsc::channel();
        let (startup_done_tx, startup_done_rx) = mpsc::channel();
        let root = directory.path().to_owned();
        let startup = thread::spawn(move || {
            let result = Daemon::start_with_enrolled_owner(
                config(&root, Some("fixture-secret")),
                Box::new(owner),
                StartupFault::PauseAndFailAfterListeners {
                    entered: listeners_entered_tx,
                    release: startup_release_rx,
                },
            );
            let _ = startup_done_tx.send(matches!(result, Err(super::DaemonError::Thread)));
        });
        listeners_entered_rx.recv_timeout(Duration::from_secs(2))?;

        let client = DaemonClient::open(directory.path())?;
        fs::rename(
            directory.path().join("daemon.auth"),
            directory.path().join("original-daemon.auth"),
        )?;
        write_private(directory.path(), "daemon.auth", b"replacement")?;
        let bytes = event("evt_0000000000000001", 1)?;
        let emitter = thread::spawn(move || client.emit(&bytes));
        owner_entered_rx.recv_timeout(Duration::from_secs(2))?;
        startup_release_tx.send(())?;
        assert!(matches!(
            startup_done_rx.recv_timeout(Duration::from_millis(50)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        owner_release_tx.send(())?;
        assert!(startup_done_rx.recv_timeout(Duration::from_secs(2))?);
        startup.join().map_err(|_| "startup thread panicked")?;
        let _ = emitter.join().map_err(|_| "emitter thread panicked")?;

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        for name in [
            "daemon.sock",
            "events.sock",
            "daemon.pid",
            "daemon.identity.json",
        ] {
            assert!(!directory.path().join(name).exists(), "retained {name}");
        }
        assert_eq!(
            fs::read(directory.path().join("daemon.auth"))?,
            b"replacement"
        );
        Ok(())
    }

    #[test]
    fn slow_consumer_is_evicted_at_the_exact_queue_bound() -> TestResult {
        let (sender, receiver) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let global_queued_bytes = Arc::new(AtomicUsize::new(0));
        let sample = OwnedEvent {
            cursor: DurableCursor(1),
            event_id: "evt_0000000000000001".into(),
            ingress_bytes: Arc::from(event("evt_0000000000000001", 1)?),
        };
        for _ in 0..CLIENT_QUEUE_CAPACITY {
            let mut subscribers = vec![super::Subscriber {
                sender: sender.clone(),
                queued_bytes: Arc::clone(&queued_bytes),
                global_queued_bytes: Arc::clone(&global_queued_bytes),
            }];
            broadcast(&mut subscribers, &sample);
            assert_eq!(subscribers.len(), 1);
        }
        let mut subscribers = vec![super::Subscriber {
            sender,
            queued_bytes: Arc::clone(&queued_bytes),
            global_queued_bytes: Arc::clone(&global_queued_bytes),
        }];
        broadcast(&mut subscribers, &sample);
        assert!(subscribers.is_empty());
        let queued = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(queued.len(), CLIENT_QUEUE_CAPACITY);
        assert!(
            queued
                .iter()
                .all(|queued| { Arc::ptr_eq(&queued.event.ingress_bytes, &sample.ingress_bytes) })
        );
        drop(queued);
        assert_eq!(queued_bytes.load(Ordering::Acquire), 0);
        assert_eq!(global_queued_bytes.load(Ordering::Acquire), 0);

        let (first_sender, first_receiver) = mpsc::sync_channel(1);
        let (second_sender, second_receiver) = mpsc::sync_channel(1);
        let first_bytes = Arc::new(AtomicUsize::new(0));
        let second_bytes = Arc::new(AtomicUsize::new(0));
        let shared_global = Arc::new(AtomicUsize::new(0));
        let mut subscribers = vec![
            super::Subscriber {
                sender: first_sender,
                queued_bytes: first_bytes,
                global_queued_bytes: Arc::clone(&shared_global),
            },
            super::Subscriber {
                sender: second_sender,
                queued_bytes: second_bytes,
                global_queued_bytes: shared_global,
            },
        ];
        broadcast(&mut subscribers, &sample);
        let first = first_receiver.recv()?;
        let second = second_receiver.recv()?;
        assert!(Arc::ptr_eq(
            first.network.as_ref().ok_or("first projection missing")?,
            second.network.as_ref().ok_or("second projection missing")?
        ));

        let large = OwnedEvent {
            cursor: DurableCursor(1),
            event_id: "evt_0000000000000001".into(),
            ingress_bytes: Arc::from(vec![b'x'; super::MAX_FRAME_BYTES]),
        };
        let (sender, receiver) = mpsc::sync_channel(CLIENT_QUEUE_CAPACITY);
        let client_bytes = Arc::new(AtomicUsize::new(0));
        let global_bytes = Arc::new(AtomicUsize::new(0));
        let mut client = vec![super::Subscriber {
            sender,
            queued_bytes: Arc::clone(&client_bytes),
            global_queued_bytes: Arc::clone(&global_bytes),
        }];
        broadcast(&mut client, &large);
        assert_eq!(client.len(), 1);
        broadcast(&mut client, &large);
        assert_eq!(client.len(), 1);
        broadcast(&mut client, &large);
        assert!(client.is_empty());
        assert_eq!(
            client_bytes.load(Ordering::Acquire),
            large.ingress_bytes.len() * 2
        );
        drop(receiver);
        assert_eq!(client_bytes.load(Ordering::Acquire), 0);

        let global_bytes = Arc::new(AtomicUsize::new(0));
        let mut receivers = Vec::new();
        let mut global_clients = Vec::new();
        for _ in 0..9 {
            let (sender, receiver) = mpsc::sync_channel(1);
            receivers.push(receiver);
            global_clients.push(super::Subscriber {
                sender,
                queued_bytes: Arc::new(AtomicUsize::new(0)),
                global_queued_bytes: Arc::clone(&global_bytes),
            });
        }
        broadcast(&mut global_clients, &large);
        assert_eq!(global_clients.len(), 8);
        assert_eq!(
            global_bytes.load(Ordering::Acquire),
            large.ingress_bytes.len() * 8
        );
        assert!(
            super::MAX_GLOBAL_QUEUED_BYTES - global_bytes.load(Ordering::Acquire)
                < large.ingress_bytes.len()
        );
        drop(receivers);
        assert_eq!(global_bytes.load(Ordering::Acquire), 0);
        Ok(())
    }

    #[test]
    fn client_admission_is_bounded_and_closes_after_shutdown_begins() -> TestResult {
        let directory = DisposableDirectory::new("capacity")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let mut peers = Vec::new();
        let mut permits = Vec::new();
        for _ in 0..MAX_CONCURRENT_CLIENTS {
            let (cancel, peer) = UnixStream::pair()?;
            permits.push(
                admit_client(&daemon.state, ClientCancel::Unix(cancel))
                    .ok_or("capacity rejected an admitted client")?,
            );
            peers.push(peer);
        }
        let (cancel, _peer) = UnixStream::pair()?;
        assert!(admit_client(&daemon.state, ClientCancel::Unix(cancel)).is_none());
        super::request_shutdown(&daemon.state);
        drop(permits);
        let (cancel, _peer) = UnixStream::pair()?;
        assert!(admit_client(&daemon.state, ClientCancel::Unix(cancel)).is_none());
        drop(peers);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn server_rejects_declared_oversize_without_reading_or_mutating_owner() -> TestResult {
        let directory = DisposableDirectory::new("server-oversize")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), None),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let request = format!(
            "POST /api/events HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\n\r\n",
            MAX_HTTP_BODY_BYTES + 1
        );
        let response = request_until(daemon.address(), request.as_bytes(), b"\r\n\r\n")?;
        assert!(response.starts_with(b"HTTP/1.1 413 Payload Too Large\r\n"));
        let mut data = BTreeMap::new();
        data.insert("items".to_owned(), json!(vec![json!({"auth": 0}); 60_000]));
        let expanding = encode_current_event(&EventRecord {
            id: "evt_0000000000000001".into(),
            event_type: "worker.output".into(),
            timestamp: "2026-07-22T00:00:00Z".into(),
            sequence: 1,
            mission_id: "fixture-mission".into(),
            phase_id: None,
            worker_id: None,
            data: Some(EventJsonMap::from(data)),
            extra: EventJsonMap::default(),
        })?;
        assert!(expanding.len() < super::MAX_FRAME_BYTES);
        assert!(matches!(
            DaemonClient::open(directory.path())?.emit(&expanding),
            Err(super::DaemonError::TooLarge)
        ));
        daemon.shutdown()?;
        let mut owner = FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?;
        assert!(
            owner
                .replay_page(DurableCursor::origin(), usize::MAX, usize::MAX)
                .map_err(super::owner_failure)?
                .events
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn embedded_recovery_refuses_live_transport_before_metadata_removal() -> TestResult {
        let directory = DisposableDirectory::new("embedded-live")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), None),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let root = super::admit_existing_root(directory.path())?;
        let original = fs::read(directory.path().join(IDENTITY_FILE))?;
        let mut identity = super::read_identity(&root)?.ok_or("daemon identity")?;
        let mut start = identity.start_identity.into_bytes();
        let last = start.last_mut().ok_or("start identity")?;
        *last = if *last == b'0' { b'1' } else { b'0' };
        identity.start_identity = String::from_utf8(start)?;
        fs::write(
            directory.path().join(IDENTITY_FILE),
            serde_json::to_vec(&identity)?,
        )?;
        let before = [PID_FILE, IDENTITY_FILE, AUTH_FILE]
            .map(|name| fs::read(directory.path().join(name)))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        assert!(matches!(
            super::reject_live_duplicate(&root, true),
            Err(super::DaemonError::AlreadyRunning)
        ));
        let after = [PID_FILE, IDENTITY_FILE, AUTH_FILE]
            .map(|name| fs::read(directory.path().join(name)))
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(before, after);
        fs::write(directory.path().join(IDENTITY_FILE), original)?;
        assert_eq!(
            DaemonClient::open(directory.path())?.cancel_mission("mission-a")?,
            MissionCancellationAcknowledgement::Unsupported
        );
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn ambiguous_recorded_leader_with_present_group_fails_closed() -> TestResult {
        let directory = DisposableDirectory::new("ambiguous-pid")?;
        let observed = current_kernel_identity(std::process::id())?;
        let mut mismatched = observed.process_start_identity().as_bytes().to_vec();
        let last = mismatched.last_mut().ok_or("empty process identity")?;
        *last = if *last == b'0' { b'1' } else { b'0' };
        let identity = super::DaemonIdentity {
            pid: observed.pid(),
            process_group_id: observed.process_group_id(),
            start_identity: String::from_utf8(mismatched)?,
            port: 1,
        };
        write_private(
            directory.path(),
            "daemon.pid",
            identity.pid.to_string().as_bytes(),
        )?;
        write_private(
            directory.path(),
            "daemon.identity.json",
            &serde_json::to_vec(&identity)?,
        )?;
        assert!(matches!(
            super::daemon_status(directory.path()),
            Err(super::DaemonError::IdentityMismatch)
        ));
        Ok(())
    }

    #[test]
    fn attach_during_commit_handoff_delivers_exactly_once_without_loss() -> TestResult {
        let directory = DisposableDirectory::new("attach-race")?;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let owner = SnapshotGateOwner {
            inner: FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?,
            entered: Some(entered_tx),
            release: release_rx,
        };
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(owner),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let attach_client = client.clone();
        let attach = thread::spawn(move || attach_client.subscribe(0));
        entered_rx.recv_timeout(Duration::from_secs(2))?;
        let bytes = event("evt_0000000000000001", 1)?;
        let emit_client = client.clone();
        let emit_bytes = bytes.clone();
        let emitter = thread::spawn(move || emit_client.emit(&emit_bytes));
        release_tx.send(())?;
        let mut subscription = attach.join().map_err(|_| "attach thread panicked")??;
        emitter.join().map_err(|_| "emit thread panicked")??;
        subscription.set_read_timeout(Some(Duration::from_secs(2)))?;
        let delivered = subscription.read_event()?.ok_or("event was lost")?;
        assert_eq!(delivered.cursor, 1);
        assert_eq!(delivered.ingress_bytes, bytes);
        subscription.set_read_timeout(Some(Duration::from_millis(50)))?;
        assert!(
            subscription.read_event().is_err(),
            "event was delivered twice"
        );
        drop(subscription);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn lost_initial_enrollment_ack_replays_from_acknowledged_baseline() -> TestResult {
        let directory = DisposableDirectory::new("lost-initial-ack")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let client = DaemonClient::open(directory.path())?;
        let mut cursor = client.prepare_subscription_cursor()?;
        assert_eq!(cursor, 0);

        // Enroll a stream and deliberately discard its acknowledgement while
        // retaining only the separately acknowledged durable baseline.
        let mut lost = client.connect_verified(EVENTS_SOCKET, client.events_identity)?;
        lost.set_nonblocking(true)?;
        client.authenticate_stream(
            &mut lost,
            "subscribe",
            cursor,
            super::AbsoluteDeadline::after(Duration::from_secs(2))?,
        )?;
        let deadline = Instant::now() + Duration::from_secs(2);
        while lock(&daemon.state.subscribers).is_empty() {
            if Instant::now() >= deadline {
                return Err("subscription was not enrolled".into());
            }
            thread::yield_now();
        }

        let bytes = event("evt_0000000000000001", 1)?;
        client.emit(&bytes)?;
        drop(lost);

        let mut recovered = client.subscribe_with_cursor(&mut cursor)?;
        assert_eq!(cursor, 0);
        recovered.set_read_timeout(Some(Duration::from_secs(2)))?;
        let replay = recovered.read_event()?.ok_or("lost-ack replay ended")?;
        assert_eq!(replay.cursor, 1);
        assert_eq!(replay.ingress_bytes, bytes);
        daemon.shutdown()?;
        Ok(())
    }

    #[test]
    fn idle_stream_has_zero_periodic_wakeups_and_shutdown_cancels_it() -> TestResult {
        let directory = DisposableDirectory::new("idle-wakeups")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        let subscription = DaemonClient::open(directory.path())?.subscribe(-1)?;
        assert_eq!(daemon.state.stream_wakeups.load(Ordering::Relaxed), 0);
        daemon.shutdown()?;
        drop(subscription);
        Ok(())
    }

    #[test]
    fn subscription_cancel_wakes_a_blocking_reader_without_a_timeout() -> TestResult {
        let (stream, peer) = UnixStream::pair()?;
        let subscription = DaemonSubscription {
            stream,
            channel: super::SecureChannel::client(
                "fixture",
                "0000000000000000000000000000000000000000000000000000000000000000",
            ),
        };
        let cancel = subscription.cancellation_handle()?;
        let (done_tx, done_rx) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut subscription = subscription;
            let _ = done_tx.send(subscription.read_event());
        });

        cancel.cancel();
        let result = done_rx.recv_timeout(Duration::from_secs(2))?;
        assert!(matches!(
            result,
            Ok(None) | Err(super::DaemonError::Io { .. })
        ));
        reader.join().map_err(|_| "subscription reader panicked")?;
        drop(peer);
        Ok(())
    }

    #[test]
    fn shutdown_preserves_replaced_metadata_with_the_wrong_identity() -> TestResult {
        let directory = DisposableDirectory::new("metadata-identity")?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        fs::rename(
            directory.path().join("daemon.auth"),
            directory.path().join("original-daemon.auth"),
        )?;
        write_private(directory.path(), "daemon.auth", b"replacement")?;
        daemon.shutdown()?;
        assert_eq!(
            fs::read(directory.path().join("daemon.auth"))?,
            b"replacement"
        );
        Ok(())
    }

    #[test]
    fn symlink_replacements_are_never_chmodded_or_unlinked() -> TestResult {
        let directory = DisposableDirectory::new("symlink-replacement")?;
        let outside = directory.path().join("outside-secret");
        fs::write(&outside, b"outside-unchanged")?;
        fs::set_permissions(&outside, fs::Permissions::from_mode(0o644))?;
        let daemon = start_fixture_daemon(
            config(directory.path(), Some("fixture-secret")),
            Box::new(FixtureEventOwner::open(directory.path()).map_err(super::owner_failure)?),
        )?;
        fs::rename(
            directory.path().join(AUTH_FILE),
            directory.path().join("original-daemon.auth"),
        )?;
        symlink(&outside, directory.path().join(AUTH_FILE))?;
        let outside_socket = directory.path().join("outside-live-socket-target");
        fs::write(&outside_socket, b"outside-live-socket-unchanged")?;
        fs::rename(
            directory.path().join(INGEST_SOCKET),
            directory.path().join("original-daemon.sock"),
        )?;
        symlink(&outside_socket, directory.path().join(INGEST_SOCKET))?;
        daemon.shutdown()?;
        assert!(
            fs::symlink_metadata(directory.path().join(AUTH_FILE))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&outside)?, b"outside-unchanged");
        assert_eq!(fs::metadata(&outside)?.permissions().mode() & 0o777, 0o644);
        assert!(
            fs::symlink_metadata(directory.path().join(INGEST_SOCKET))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&outside_socket)?, b"outside-live-socket-unchanged");

        let stale = DisposableDirectory::new("stale-socket-symlink")?;
        let outside = stale.path().join("outside-socket-target");
        fs::write(&outside, b"outside-socket-unchanged")?;
        symlink(&outside, stale.path().join(INGEST_SOCKET))?;
        let owner = FixtureEventOwner::open(stale.path()).map_err(super::owner_failure)?;
        assert!(matches!(
            Daemon::start(config(stale.path(), None), Box::new(owner)),
            Err(super::DaemonError::UnsafeEntry(INGEST_SOCKET))
        ));
        assert!(
            fs::symlink_metadata(stale.path().join(INGEST_SOCKET))?
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&outside)?, b"outside-socket-unchanged");
        Ok(())
    }
}
