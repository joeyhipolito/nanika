//! Process-wide supervision for bounded, cancellable command execution.
//!
//! A supervised command starts in a fresh process group. Child exit, pipe
//! completion, and cancellation wake the controller through one
//! condition-variable-backed event queue; the controller does not poll a fixed
//! timer. Every return carries bounded partial output and an explicit cleanup
//! receipt. If synchronous cleanup cannot prove ownership closed, a reaper
//! retains all handles and closes new admission until ownership is resolved.
//!
//! Process-group supervision is containment for cooperative descendants, not a
//! security sandbox: a descendant that deliberately creates a new session or
//! process group, or changes to credentials the supervisor cannot signal, can
//! escape it. Ambient launches retain detect-after-spawn path checks. Linux
//! fixture authority launches use direct FD paths; Linux production execution
//! remains fail-closed until it has the same pre-exec durable gate. The macOS
//! production authority uses a private sealed broker and either a sealed canary
//! target or a pinned original external target. It gives the broker the retained
//! CWD as descriptor 0. After an authenticated durable grant, the broker
//! performs `fchdir` and `exec`s the target in place so PID, process group, and
//! kernel start identity remain unchanged.
//! Bounded control files carry argv, allowlisted environment, and optional
//! stdin without exposing payloads in process arguments. Final macOS target
//! execution remains path-based. Persistent callers must hold their declared
//! external executable namespace read-only for the authority lifetime; the
//! constructor accepts ordinary 0755 files but does not create a writer lease.

mod broker;
mod gate;
mod output;
mod process_identity;

#[doc(hidden)]
pub use broker::process_broker_main;
pub use output::{
    ProcessOutputChunk, ProcessOutputReceiver, ProcessOutputSender, ProcessOutputStream,
    output_channel,
};
pub use process_identity::{
    ExactProcessGroupAbsence, KernelProcessIdentity, ProcessIdentityError,
    RecordedProcessIdentityStatus, inspect_recorded_process_identity,
};

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs::{File, Metadata};
use std::io::{Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Child, ExitStatus, Stdio};
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "linux"))]
use rustix::process::test_kill_process_group;
use rustix::process::{Pid, Signal, getpgid, kill_process_group};
#[cfg(not(any(
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
use rustix::process::{WaitId, WaitIdOptions, waitid};
use sha2::{Digest, Sha256};
use thiserror::Error;

const MAX_ARGUMENTS: usize = 128;
const MAX_ARGUMENT_BYTES: usize = 8 * 1024;
const MAX_ARGUMENT_TOTAL: usize = 64 * 1024;
const MAX_ENVIRONMENT_ENTRIES: usize = 64;
const MAX_ENVIRONMENT_TOTAL: usize = 64 * 1024;
const MAX_STDIN_BYTES: usize = 16 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const MAX_FIXTURE_EXECUTABLE_BYTES: usize = 64 * 1024 * 1024;
const EXECUTABLE_STREAM_CHUNK: usize = 64 * 1024;
/// Maximum exact length accepted from a streamed executable attestation.
///
/// Admission is linear in this value. External macOS admission reads and
/// hashes the retained source for its pre-copy proof, streamed copy, and
/// post-copy proof, then reads and hashes the sealed clone once, always with a
/// fixed 64 KiB buffer. The 256 MiB ceiling admits the reviewed
/// 242,445,680-byte Claude Code image while bounding CPU work and temporary
/// disk consumption.
pub const MAX_ATTESTED_EXECUTABLE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_MAX_OUTPUT: usize = 256 * 1024;
const DEFAULT_TERM_GRACE: Duration = Duration::from_secs(10);
const DEFAULT_CLEANUP_GRACE: Duration = Duration::from_secs(2);
const MAX_RUN_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
const READER_CHUNK: usize = 8 * 1024;
const GLOBAL_MAX_OWNED_GROUPS: usize = 32;
const REAPER_POLL: Duration = Duration::from_millis(25);
#[cfg(target_os = "macos")]
const GATE_ROOT_RECORD_NAME: &str = "gate-root";
#[cfg(target_os = "macos")]
const GATE_ROOT_RECORD_MAGIC: &[u8; 8] = b"NANGRT01";
#[cfg(target_os = "macos")]
const GATE_ROOT_RECORD_FIXED_BYTES: usize = 8 + (12 * 8) + 2;
#[cfg(target_os = "macos")]
const MAX_GATE_ROOT_RECORD_BYTES: usize = GATE_ROOT_RECORD_FIXED_BYTES + 255;

/// A cloneable, wakeable cancellation source shared with a supervised process.
///
/// Cancellation is monotonic. The first call to [`Self::cancel`] records the
/// transition time and wakes every registered process controller. Later calls
/// are no-ops.
#[derive(Clone, Default)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

#[derive(Default)]
struct CancellationState {
    state: Mutex<CancellationData>,
}

#[derive(Default)]
struct CancellationData {
    cancelled_at: Option<Instant>,
    next_registration: u64,
    observers: Vec<CancellationObserver>,
}

struct CancellationObserver {
    id: u64,
    queue: Weak<EventQueue>,
}

struct CancellationRegistration {
    token: Weak<CancellationState>,
    id: u64,
}

impl CancellationToken {
    /// Creates a token in the active state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancels the token and wakes registered process controllers.
    ///
    /// Returns `true` only for the call that performs the state transition.
    pub fn cancel(&self) -> bool {
        let now = Instant::now();
        let queues = {
            let mut state = lock_unpoisoned(&self.inner.state);
            if state.cancelled_at.is_some() {
                return false;
            }
            state.cancelled_at = Some(now);
            let mut queues = Vec::with_capacity(state.observers.len());
            state.observers.retain(|observer| {
                if let Some(queue) = observer.queue.upgrade() {
                    queues.push(queue);
                    true
                } else {
                    false
                }
            });
            queues
        };
        for queue in queues {
            queue.push(Event::Cancelled(now));
        }
        true
    }

    /// Returns whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled_at().is_some()
    }

    fn cancelled_at(&self) -> Option<Instant> {
        lock_unpoisoned(&self.inner.state).cancelled_at
    }

    fn register(&self, queue: &Arc<EventQueue>) -> CancellationRegistration {
        let (id, cancelled_at) = {
            let mut state = lock_unpoisoned(&self.inner.state);
            let id = state.next_registration;
            state.next_registration = state.next_registration.saturating_add(1);
            let cancelled_at = state.cancelled_at;
            if cancelled_at.is_none() {
                state.observers.push(CancellationObserver {
                    id,
                    queue: Arc::downgrade(queue),
                });
            }
            (id, cancelled_at)
        };
        if let Some(cancelled_at) = cancelled_at {
            queue.push(Event::Cancelled(cancelled_at));
        }
        CancellationRegistration {
            token: Arc::downgrade(&self.inner),
            id,
        }
    }
}

impl fmt::Debug for CancellationToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl Drop for CancellationRegistration {
    fn drop(&mut self) {
        if let Some(token) = self.token.upgrade() {
            lock_unpoisoned(&token.state)
                .observers
                .retain(|observer| observer.id != self.id);
        }
    }
}

/// Bounded argv, deadlines, environment, cancellation, and I/O for one command.
///
/// This type deliberately has no revealing `Debug` implementation. The child
/// environment is clear by default; callers must explicitly add values with
/// [`Self::with_env`] or allow individual inherited values with
/// [`Self::with_inherited_env`]. Relative paths and bare executable names are
/// resolved before spawn and then checked for replacement, but this is
/// detect-after-spawn verification rather than an authority boundary.
pub struct ProcessSpec {
    argv: Vec<OsString>,
    hard_deadline: Duration,
    hard_deadline_at: Option<Instant>,
    term_grace: Duration,
    cleanup_grace: Duration,
    stall_timeout: Option<Duration>,
    max_output_bytes: usize,
    environment: Vec<(OsString, OsString)>,
    inherit_environment: Vec<OsString>,
    env_remove: Vec<OsString>,
    stdin: Option<Arc<[u8]>>,
    cancellation: Option<CancellationToken>,
    output_sender: Option<ProcessOutputSender>,
    #[cfg(test)]
    fail_reap_once: bool,
    #[cfg(test)]
    reaper_reap_gate: Option<Arc<Mutex<bool>>>,
    #[cfg(test)]
    post_adoption_barrier: Option<Arc<std::sync::Barrier>>,
    #[cfg(test)]
    pre_spawn_barriers: Option<(Arc<std::sync::Barrier>, Arc<std::sync::Barrier>)>,
    #[cfg(test)]
    controller_iterations: Option<Arc<Mutex<usize>>>,
}

impl ProcessSpec {
    /// Creates a bounded process specification.
    ///
    /// The first argv entry names the executable. It is resolved to a canonical
    /// path before spawn and checked again after spawn. That check detects many
    /// replacements but cannot prevent a mutable path from racing `exec`; callers
    /// requiring an authority boundary must supply immutable, externally
    /// controlled executable and working-directory paths. The hard deadline is
    /// limited to 24 hours so monotonic deadline arithmetic cannot overflow.
    pub fn new(argv: Vec<OsString>, hard_deadline: Duration) -> Result<Self, ProcessError> {
        validate_argv(&argv, hard_deadline)?;
        Ok(Self {
            argv,
            hard_deadline,
            hard_deadline_at: None,
            term_grace: DEFAULT_TERM_GRACE,
            cleanup_grace: DEFAULT_CLEANUP_GRACE,
            stall_timeout: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT,
            environment: vec![
                (OsString::from("LANG"), OsString::from("C")),
                (OsString::from("LC_ALL"), OsString::from("C")),
            ],
            inherit_environment: Vec::new(),
            env_remove: Vec::new(),
            stdin: None,
            cancellation: None,
            output_sender: None,
            #[cfg(test)]
            fail_reap_once: false,
            #[cfg(test)]
            reaper_reap_gate: None,
            #[cfg(test)]
            post_adoption_barrier: None,
            #[cfg(test)]
            pre_spawn_barriers: None,
            #[cfg(test)]
            controller_iterations: None,
        })
    }

    /// Caps the relative runtime with an already-established monotonic deadline.
    ///
    /// Adapters translating an upstream absolute budget should set this so
    /// validation and command construction cannot reset or extend that budget.
    #[must_use]
    pub fn with_hard_deadline_at(mut self, deadline: Instant) -> Self {
        self.hard_deadline_at = Some(deadline);
        self
    }

    /// Grace between `SIGTERM` and `SIGKILL`.
    #[must_use]
    pub fn with_term_grace(mut self, grace: Duration) -> Self {
        self.term_grace = grace;
        self
    }

    /// Deadline for proving direct-child reap, pipe completion, and group absence
    /// after escalation.
    #[must_use]
    pub fn with_cleanup_grace(mut self, grace: Duration) -> Self {
        self.cleanup_grace = grace;
        self
    }

    /// Sets a bounded inactivity timeout for stdout/stderr production.
    ///
    /// The timer starts after child adoption. Every successful non-empty read
    /// from stdout or stderr resets it, including bytes drained beyond the
    /// retained-output cap. Zero and values above 24 hours are rejected by
    /// [`run`]. Stdin progress and process liveness are not output activity.
    #[must_use]
    pub fn with_stall_timeout(mut self, timeout: Duration) -> Self {
        self.stall_timeout = Some(timeout);
        self
    }

    /// Per-stream retained byte cap. Additional bytes are drained and counted.
    /// A zero cap retains no output and counts every drained byte as discarded.
    #[must_use]
    pub fn with_max_output_bytes(mut self, cap: usize) -> Self {
        self.max_output_bytes = cap;
        self
    }

    /// Adds one explicit environment value to the otherwise-cleared child.
    /// Repeated explicit keys resolve deterministically to the last value.
    #[must_use]
    pub fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.environment.push((key.into(), value.into()));
        self
    }

    /// Allows one named variable to be copied from the parent environment.
    /// An inherited key never overrides an explicit value of the same name.
    #[must_use]
    pub fn with_inherited_env(mut self, key: impl Into<OsString>) -> Self {
        self.inherit_environment.push(key.into());
        self
    }

    /// Explicitly denies a variable, overriding both explicit and inherited
    /// entries. Retained for compatibility with the Go Git-safe environment.
    #[must_use]
    pub fn with_env_remove(mut self, key: impl Into<OsString>) -> Self {
        self.env_remove.push(key.into());
        self
    }

    /// Supplies bounded stdin. The byte limit is validated by [`run`].
    #[must_use]
    pub fn with_stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(Arc::from(bytes));
        self
    }

    /// Wires a wakeable cancellation token into this process.
    #[must_use]
    pub fn with_cancellation(mut self, token: CancellationToken) -> Self {
        self.cancellation = Some(token);
        self
    }

    /// Enables bounded best-effort delivery of stdout and stderr chunks.
    #[must_use]
    pub fn with_output_sender(mut self, sender: ProcessOutputSender) -> Self {
        self.output_sender = Some(sender);
        self
    }
}

/// Failures that prevent a process receipt from being produced.
#[derive(Debug, Error)]
pub enum ProcessError {
    /// The specification violated a fixed bound.
    #[error("invalid process specification")]
    InvalidSpec,
    /// Registry setup, capability verification, or spawning failed.
    #[error("supervised process spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
}

/// Public classification for a production launch that produced no outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthorizedProcessErrorClassification {
    /// The launch failed before the operating system returned a child.
    ProvenNotStarted,
    /// Supervision could not produce a conclusive post-spawn outcome.
    OutcomeLost,
}

/// Opaque failure to produce a classified outcome for an admitted launch.
///
/// [`AuthorizedProcessErrorClassification::ProvenNotStarted`] is issued only
/// by operations that complete before `Command::spawn` returns a child.
/// [`AuthorizedProcessErrorClassification::OutcomeLost`] is the conservative
/// classification for any error after that boundary, where a caller must not
/// infer that the target did not execute. The underlying operating-system
/// error remains private because it may contain admitted paths.
#[derive(Error)]
#[error("authorized process did not produce a process outcome")]
pub struct AuthorizedProcessError {
    classification: AuthorizedProcessErrorClassification,
    cause: ProcessError,
}

impl AuthorizedProcessError {
    fn proven_not_started(cause: ProcessError) -> Self {
        Self {
            classification: AuthorizedProcessErrorClassification::ProvenNotStarted,
            cause,
        }
    }

    fn outcome_lost(cause: ProcessError) -> Self {
        Self {
            classification: AuthorizedProcessErrorClassification::OutcomeLost,
            cause,
        }
    }

    /// Returns whether the failed operation was structurally pre- or post-spawn.
    #[must_use]
    pub const fn classification(&self) -> AuthorizedProcessErrorClassification {
        self.classification
    }
}

impl fmt::Debug for AuthorizedProcessError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let _ = &self.cause;
        formatter
            .debug_struct("AuthorizedProcessError")
            .field("classification", &self.classification)
            .field("cause", &"REDACTED")
            .finish()
    }
}

/// Opaque binding to the exact durable process request awaiting release.
///
/// The bytes are supplied by the request-fingerprint owner. This type is a
/// correlation value, not release authority: only a controller-created
/// [`ProcessStartGateRequest`] carries the one-shot reply capability.
pub struct ProcessRequestBinding([u8; 32]);

impl ProcessRequestBinding {
    /// Wraps the canonical 32-byte process-request fingerprint.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// Compares this binding with a recomputed process-request fingerprint.
    #[must_use]
    pub fn matches_bytes(&self, candidate: &[u8; 32]) -> bool {
        self.0 == *candidate
    }

    fn duplicate_for_started_receipt(&self) -> Self {
        Self(self.0)
    }
}

impl fmt::Debug for ProcessRequestBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcessRequestBinding(REDACTED)")
    }
}

/// One-shot, bounded request endpoint for a durable process-start authority.
///
/// Create a pair with [`ProcessStartGate::channel`], binding it to the exact
/// process-request fingerprint, move the authority endpoint to the storage
/// actor, and pass this endpoint to [`ProcessSupervisor::run_authorized`]. The
/// process controller only performs a non-blocking send; durable work never
/// runs on the supervision thread.
pub struct ProcessStartGate {
    sender: SyncSender<ProcessStartGateRequest>,
    started_sender: SyncSender<ProcessStartedObservation>,
    request_binding: Option<ProcessRequestBinding>,
    submitted: bool,
    started_submitted: bool,
}

impl ProcessStartGate {
    /// Creates a request-bound, one-shot channel between supervision and a durable actor.
    #[must_use]
    pub fn channel(request_binding: ProcessRequestBinding) -> (Self, ProcessStartGateAuthority) {
        let (sender, receiver) = sync_channel(1);
        let (started_sender, started_receiver) = sync_channel(1);
        (
            Self {
                sender,
                started_sender,
                request_binding: Some(request_binding),
                submitted: false,
                started_submitted: false,
            },
            ProcessStartGateAuthority {
                receiver,
                started_receiver,
            },
        )
    }

    fn submit(
        &mut self,
        identity: KernelProcessIdentity,
        reply: ProcessStartGateReply,
    ) -> Result<ProcessRequestBinding, ()> {
        if self.submitted {
            return Err(());
        }
        self.submitted = true;
        let request_binding = self.request_binding.take().ok_or(())?;
        let started_binding = request_binding.duplicate_for_started_receipt();
        let request = ProcessStartGateRequest {
            identity,
            request_binding,
            reply: Some(reply),
        };
        self.sender.try_send(request).map_err(|error| match error {
            TrySendError::Full(_) | TrySendError::Disconnected(_) => (),
        })?;
        Ok(started_binding)
    }

    fn submit_started(
        &mut self,
        receipt: ProcessStartedReceipt,
        reply: ProcessStartedObservationReply,
    ) -> Result<(), ()> {
        if !self.submitted || self.started_submitted {
            return Err(());
        }
        self.started_submitted = true;
        self.started_sender
            .try_send(ProcessStartedObservation {
                receipt,
                reply: Some(reply),
            })
            .map_err(|error| match error {
                TrySendError::Full(_) | TrySendError::Disconnected(_) => (),
            })
    }
}

impl fmt::Debug for ProcessStartGate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessStartGate")
            .field("submitted", &self.submitted)
            .finish_non_exhaustive()
    }
}

/// Receiving capability held by the durable storage actor.
///
/// This endpoint is deliberately not cloneable. Receiving a request is the
/// only way for another crate to obtain the capability that can authorize an
/// exact, currently blocked kernel identity.
///
/// ```compile_fail
/// use orchestrator_process::ProcessStartGateAuthority;
///
/// fn receive_out_of_order(authority: ProcessStartGateAuthority) {
///     let _ = authority.receive_started();
/// }
/// ```
pub struct ProcessStartGateAuthority {
    receiver: Receiver<ProcessStartGateRequest>,
    started_receiver: Receiver<ProcessStartedObservation>,
}

impl ProcessStartGateAuthority {
    /// Consumes the initial authority and blocks until its bounded request arrives.
    ///
    /// # Errors
    ///
    /// Returns [`ProcessStartGateClosed`] if supervision closed before issuing
    /// a request. The paired second-stage authority exists only after this
    /// receive succeeds, preventing a caller from waiting for the started
    /// receipt before it decides the initial request.
    pub fn receive(
        self,
    ) -> Result<(ProcessStartGateRequest, ProcessStartedReceiptAuthority), ProcessStartGateClosed>
    {
        let request = self
            .receiver
            .recv()
            .map_err(|_| ProcessStartGateClosed(()))?;
        Ok((
            request,
            ProcessStartedReceiptAuthority {
                receiver: self.started_receiver,
            },
        ))
    }
}

/// Second-stage authority available only after the initial request is received.
///
/// ```compile_fail
/// use orchestrator_process::ProcessStartedReceiptAuthority;
///
/// fn receive_twice(authority: ProcessStartedReceiptAuthority) {
///     let _ = authority.receive_started();
///     let _ = authority.receive_started();
/// }
/// ```
pub struct ProcessStartedReceiptAuthority {
    receiver: Receiver<ProcessStartedObservation>,
}

impl ProcessStartedReceiptAuthority {
    /// Blocks until the lower supervisor proves the authenticated grant and
    /// exact blocked-launcher identity.
    ///
    /// The returned observation must be acknowledged only after its exact
    /// durable started marker commits.
    pub fn receive_started(self) -> Result<ProcessStartedObservation, ProcessStartGateClosed> {
        self.receiver.recv().map_err(|_| ProcessStartGateClosed(()))
    }
}

impl fmt::Debug for ProcessStartGateAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessStartGateAuthority")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for ProcessStartedReceiptAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessStartedReceiptAuthority")
            .finish_non_exhaustive()
    }
}

/// The process controller closed a one-shot gate operation.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("process start gate is closed")]
pub struct ProcessStartGateClosed(());

/// Durable authorization request for one exact, blocked launcher identity.
///
/// Dropping an unanswered request reports an indeterminate transaction and
/// wakes supervision. The request cannot be constructed outside this crate.
///
/// ```compile_fail
/// use orchestrator_process::ProcessStartGateRequest;
///
/// // Only the process controller can bind a reply capability to an observed
/// // kernel identity; downstream composition cannot forge one.
/// let _forged = ProcessStartGateRequest {};
/// ```
///
/// ```compile_fail
/// use orchestrator_process::ProcessStartGateRequest;
///
/// // The one-shot release capability cannot be duplicated downstream.
/// fn require_clone<T: Clone>() {}
/// require_clone::<ProcessStartGateRequest>();
/// ```
pub struct ProcessStartGateRequest {
    identity: KernelProcessIdentity,
    request_binding: ProcessRequestBinding,
    reply: Option<ProcessStartGateReply>,
}

/// Non-forgeable proof that the exact blocked launcher acknowledged its grant.
///
/// This receipt is minted only after the lower process layer re-observes the
/// same PID, process group, and kernel start identity after the authenticated
/// grant acknowledgement. It is intentionally non-Clone and has no public
/// constructor.
///
/// ```compile_fail
/// use orchestrator_process::ProcessStartedReceipt;
///
/// fn require_clone<T: Clone>() {}
/// require_clone::<ProcessStartedReceipt>();
/// ```
///
/// ```compile_fail
/// use orchestrator_process::ProcessStartedReceipt;
///
/// let _forged = ProcessStartedReceipt {};
/// ```
pub struct ProcessStartedReceipt {
    request_binding: ProcessRequestBinding,
    identity: KernelProcessIdentity,
}

impl ProcessStartedReceipt {
    /// Returns whether this receipt belongs to the exact request fingerprint.
    #[must_use]
    pub fn matches_request(&self, candidate: &[u8; 32]) -> bool {
        self.request_binding.matches_bytes(candidate)
    }

    /// Borrows the exact post-grant kernel identity.
    #[must_use]
    pub fn identity(&self) -> &KernelProcessIdentity {
        &self.identity
    }
}

impl fmt::Debug for ProcessStartedReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ProcessStartedReceipt(REDACTED)")
    }
}

/// One-shot delivery of a post-grant receipt to durable composition.
///
/// Dropping the observation without acknowledging its durable marker reports
/// an indeterminate start observation to supervision.
pub struct ProcessStartedObservation {
    receipt: ProcessStartedReceipt,
    reply: Option<ProcessStartedObservationReply>,
}

impl ProcessStartedObservation {
    /// Borrows the lower-layer receipt for exact durable binding.
    #[must_use]
    pub fn receipt(&self) -> &ProcessStartedReceipt {
        &self.receipt
    }

    /// Acknowledges that the exact immutable started marker committed.
    pub fn persisted(mut self) -> Result<(), ProcessStartGateClosed> {
        self.reply
            .take()
            .ok_or(ProcessStartGateClosed(()))?
            .decide(ProcessStartedObservationDecision::Persisted)
    }

    /// Reports that durable started-marker commit could not be established.
    pub fn indeterminate(mut self) -> Result<(), ProcessStartGateClosed> {
        self.reply
            .take()
            .ok_or(ProcessStartGateClosed(()))?
            .decide(ProcessStartedObservationDecision::Indeterminate)
    }
}

impl Drop for ProcessStartedObservation {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.decide(ProcessStartedObservationDecision::Indeterminate);
        }
    }
}

impl fmt::Debug for ProcessStartedObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessStartedObservation")
            .field("receipt", &"REDACTED")
            .field("reply_open", &self.reply.is_some())
            .finish()
    }
}

impl ProcessStartGateRequest {
    /// Borrows the kernel identity that must be durably recorded.
    #[must_use]
    pub fn identity(&self) -> &KernelProcessIdentity {
        &self.identity
    }

    /// Borrows the binding that must match the durable process request.
    #[must_use]
    pub fn request_binding(&self) -> &ProcessRequestBinding {
        &self.request_binding
    }

    /// Reports that identity and release authorization committed durably.
    ///
    /// This consumes the only reply capability for the request. A successful
    /// reply authorizes supervision to race cancellation/deadline at its single
    /// grant linearization point; it does not itself release the launcher.
    pub fn release_authorized(mut self) -> Result<(), ProcessStartGateClosed> {
        self.reply
            .take()
            .ok_or(ProcessStartGateClosed(()))?
            .decide(ProcessStartGateActorDecision::ReleaseAuthorized)
    }

    /// Rejects the observed identity without a durable release marker.
    pub fn reject(mut self) -> Result<(), ProcessStartGateClosed> {
        self.reply
            .take()
            .ok_or(ProcessStartGateClosed(()))?
            .decide(ProcessStartGateActorDecision::Rejected)
    }

    /// Reports that durable commit status cannot be established.
    pub fn indeterminate(mut self) -> Result<(), ProcessStartGateClosed> {
        self.reply
            .take()
            .ok_or(ProcessStartGateClosed(()))?
            .decide(ProcessStartGateActorDecision::Indeterminate)
    }
}

impl Drop for ProcessStartGateRequest {
    fn drop(&mut self) {
        if let Some(reply) = self.reply.take() {
            let _ = reply.decide(ProcessStartGateActorDecision::Indeterminate);
        }
    }
}

impl fmt::Debug for ProcessStartGateRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessStartGateRequest")
            .field("identity", &"REDACTED")
            .field("request_binding", &"REDACTED")
            .field("reply_open", &self.reply.is_some())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessStartGateActorDecision {
    ReleaseAuthorized,
    Rejected,
    Indeterminate,
}

struct ProcessStartGateReply {
    arbitration: Arc<ProcessStartGateArbitration>,
}

impl ProcessStartGateReply {
    fn decide(self, decision: ProcessStartGateActorDecision) -> Result<(), ProcessStartGateClosed> {
        self.arbitration.actor_decide(decision)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessStartedObservationDecision {
    Persisted,
    Indeterminate,
}

struct ProcessStartedObservationReply {
    arbitration: Arc<ProcessStartedObservationArbitration>,
}

impl ProcessStartedObservationReply {
    fn decide(
        self,
        decision: ProcessStartedObservationDecision,
    ) -> Result<(), ProcessStartGateClosed> {
        self.arbitration.actor_decide(decision)
    }
}

struct ProcessReleasePermit {
    arbitration: Arc<ProcessStartGateArbitration>,
    hard_deadline: Instant,
}

struct ProcessFinalStartPermit {
    arbitration: Arc<ProcessStartedObservationArbitration>,
    hard_deadline: Instant,
}

/// Why an admitted target was proven not to have executed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessNotStartedReason {
    /// Cancellation won before the private launcher received its release grant.
    Cancelled,
    /// The hard deadline expired before the private launcher received its release grant.
    Deadline,
    /// The operating system could not spawn the private launcher.
    SpawnFailed,
    /// Durable composition explicitly rejected the observed launcher identity.
    GateRejected,
    /// Durable composition could not prove whether its authorization transaction committed.
    GateIndeterminate,
    /// The authenticated launcher protocol failed before a release grant was delivered.
    GateProtocol,
}

/// Proof that the target did not execute and all launcher ownership was closed.
pub struct ProcessNotStartedReceipt {
    /// Typed reason the target was not released.
    pub reason: ProcessNotStartedReason,
    /// Whether a blocked private launcher had been spawned.
    pub launcher_spawned: bool,
    /// Stable launcher identity when one was observed before release was denied.
    pub launcher_identity: Option<KernelProcessIdentity>,
    /// Time through synchronous cleanup and proof of absence.
    pub elapsed: Duration,
    /// Whether cancellation was observed before return.
    pub cancellation_observed: bool,
    /// Whether the hard deadline was observed before return.
    pub deadline_observed: bool,
}

impl fmt::Debug for ProcessNotStartedReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessNotStartedReceipt")
            .field("reason", &self.reason)
            .field("launcher_spawned", &self.launcher_spawned)
            .field(
                "launcher_identity",
                &self.launcher_identity.as_ref().map(|_| "REDACTED"),
            )
            .field("elapsed", &self.elapsed)
            .field("cancellation_observed", &self.cancellation_observed)
            .field("deadline_observed", &self.deadline_observed)
            .finish()
    }
}

/// Why target execution or launcher cleanup could not be classified conclusively.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessUncertainReason {
    /// Grant delivery failed at the boundary where the target may have received it.
    ReleaseDelivery,
    /// A pre-release failure occurred, but synchronous launcher cleanup was not proven.
    CleanupIncomplete(ProcessNotStartedReason),
    /// An internal composition invariant allowed a production report without a gate lifecycle.
    UngatedExecution,
}

/// Fail-closed receipt retaining ownership evidence for an inconclusive launch.
#[derive(Debug)]
pub struct ProcessUncertainReceipt {
    /// Typed source of uncertainty.
    pub reason: ProcessUncertainReason,
    /// Stable launcher identity when it was observed before uncertainty arose.
    pub launcher_identity: Option<KernelProcessIdentity>,
    /// Bounded launcher/target supervision report, including unresolved ownership.
    pub report: ProcessReport,
}

/// Production outcome that distinguishes target execution from a blocked launcher.
#[derive(Debug)]
pub enum AuthorizedProcessOutcome {
    /// The target provably never executed and launcher cleanup completed synchronously.
    NotStarted(ProcessNotStartedReceipt),
    /// The release grant was delivered, so target execution may have begun.
    Started(ProcessReport),
    /// Release delivery or ownership cleanup could not be classified conclusively.
    Uncertain(ProcessUncertainReceipt),
}

impl AuthorizedProcessOutcome {
    /// Borrows the report only when a release grant was delivered.
    #[must_use]
    pub fn started_report(&self) -> Option<&ProcessReport> {
        match self {
            Self::Started(report) => Some(report),
            Self::NotStarted(_) | Self::Uncertain(_) => None,
        }
    }

    /// Consumes the outcome and returns the report only after a delivered release grant.
    #[must_use]
    pub fn into_started_report(self) -> Option<ProcessReport> {
        match self {
            Self::Started(report) => Some(report),
            Self::NotStarted(_) | Self::Uncertain(_) => None,
        }
    }
}

/// How a supervised process ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessTermination {
    /// Exited with this status code.
    Exited(i32),
    /// Terminated by this signal number.
    Signaled(i32),
    /// The hard deadline preceded direct-child exit.
    Timeout,
    /// The output-inactivity deadline preceded direct-child exit.
    Stalled,
    /// Cancellation preceded both the hard deadline and direct-child exit.
    Cancelled,
    /// Compatibility variant retained for older callers.
    ///
    /// New reports keep output truncation orthogonal to process termination and
    /// expose it through [`ProcessReport::truncated`] and discarded-byte counts.
    OutputLimit,
    /// A process, pipe, thread, or group-control operation failed.
    InfrastructureError,
    /// Cleanup could not prove all owned resources absent before its deadline.
    UnresolvedOwnership,
}

impl ProcessTermination {
    /// Returns `true` only for a clean exit status. Callers should prefer
    /// [`ProcessReport::is_success`], which also checks cleanup and truncation.
    #[must_use]
    pub fn is_success(self) -> bool {
        matches!(self, Self::Exited(0))
    }
}

/// Non-sensitive infrastructure failures retained in a process receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessInfrastructureFailure {
    /// Waiting for the direct child failed.
    DirectChildWait,
    /// Reading stdout failed or its reader panicked.
    StdoutRead,
    /// Reading stderr failed or its reader panicked.
    StderrRead,
    /// Writing stdin failed or its writer panicked.
    StdinWrite,
    /// Process-group identity, signalling, or absence testing failed.
    GroupControl,
    /// Synchronous cleanup did not complete within its bound.
    CleanupIncomplete,
    /// The two-phase launcher gate failed or appeared outside its protocol state.
    LaunchGate,
    /// Production launcher setup failed after the blocked launcher was spawned.
    LaunchSetup,
}

/// Receipt returned by [`run`].
pub struct ProcessReport {
    /// Typed termination.
    pub termination: ProcessTermination,
    /// Bounded stdout retained at return time.
    pub stdout: Vec<u8>,
    /// Bounded stderr retained at return time.
    pub stderr: Vec<u8>,
    /// Whether either stream discarded bytes.
    pub truncated: bool,
    /// Bytes drained but not retained from stdout.
    pub stdout_discarded_bytes: u64,
    /// Bytes drained but not retained from stderr.
    pub stderr_discarded_bytes: u64,
    /// Wall-clock duration including cleanup.
    pub elapsed: Duration,
    /// Whether the process was spawned.
    pub spawned: bool,
    /// Spawned direct-child PID when available.
    pub pid: Option<u32>,
    /// Dedicated process-group ID when available.
    pub pgid: Option<u32>,
    /// Stable production-launcher identity after a release grant was delivered.
    pub kernel_identity: Option<KernelProcessIdentity>,
    /// Whether cancellation was observed before return.
    pub cancellation_observed: bool,
    /// Whether the hard deadline preceded direct-child exit.
    pub deadline_observed: bool,
    /// Whether an output-inactivity deadline was observed before return.
    pub stall_observed: bool,
    /// Whether `SIGTERM` was sent to the owned group.
    pub term_sent: bool,
    /// Whether cleanup escalated to `SIGKILL`.
    pub kill_sent: bool,
    /// Compatibility alias for `kill_sent`.
    pub escalated_to_kill: bool,
    /// Whether the direct child was successfully reaped.
    pub direct_child_reaped: bool,
    /// Whether the platform cleanup check found no remaining controllable group
    /// member before the direct leader was reaped.
    pub group_absent: bool,
    /// Whether both readers, the optional writer, direct-child reap, and group
    /// absence completed before return.
    pub cleanup_complete: bool,
    /// Typed, non-sensitive infrastructure failures.
    pub infrastructure_failures: Vec<ProcessInfrastructureFailure>,
}

impl fmt::Debug for ProcessReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessReport")
            .field("termination", &self.termination)
            .field("stdout_bytes", &self.stdout.len())
            .field("stderr_bytes", &self.stderr.len())
            .field("stdout_discarded_bytes", &self.stdout_discarded_bytes)
            .field("stderr_discarded_bytes", &self.stderr_discarded_bytes)
            .field("elapsed", &self.elapsed)
            .field("spawned", &self.spawned)
            .field("pid", &self.pid)
            .field("pgid", &self.pgid)
            .field("kernel_identity_observed", &self.kernel_identity.is_some())
            .field("cancellation_observed", &self.cancellation_observed)
            .field("deadline_observed", &self.deadline_observed)
            .field("stall_observed", &self.stall_observed)
            .field("term_sent", &self.term_sent)
            .field("kill_sent", &self.kill_sent)
            .field("direct_child_reaped", &self.direct_child_reaped)
            .field("group_absent", &self.group_absent)
            .field("cleanup_complete", &self.cleanup_complete)
            .field("infrastructure_failures", &self.infrastructure_failures)
            .finish()
    }
}

impl ProcessReport {
    /// Returns `true` only for a complete, untruncated, failure-free exit 0.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.termination.is_success()
            && self.cleanup_complete
            && !self.truncated
            && self.infrastructure_failures.is_empty()
    }

    /// stdout immediately followed by stderr.
    #[must_use]
    pub fn combined(&self) -> Vec<u8> {
        let mut combined = self.stdout.clone();
        combined.extend_from_slice(&self.stderr);
        combined
    }
}

/// Kind of private process-owned directory beneath an enrolled root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProcessOwnedDirectoryKind {
    /// A private clone of the admitted target executable.
    SealedExecutable,
    /// A private clone of the macOS FD-CWD broker executable.
    SealedBroker,
    /// A bounded broker request, transition status, and optional staged stdin.
    BrokerRequest,
    /// The durable parent that owns all per-run broker request directories.
    BrokerStaging,
}

/// Exact ownership metadata that root-lock recovery may persist and reconcile.
///
/// The name is a single relative component beneath the retained authority root.
/// Device and inode identities bind both that root and the owned directory.
/// No executable path, argv, environment value, or stdin content is exposed.
#[derive(Clone)]
pub struct ProcessOwnedDirectory {
    kind: ProcessOwnedDirectoryKind,
    name: OsString,
    root_identity: FileIdentity,
    directory_identity: FileIdentity,
}

impl ProcessOwnedDirectory {
    /// Encodes this redacted record for a root-lock ownership journal.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut encoded = Vec::with_capacity(45 + self.name.as_bytes().len());
        encoded.extend_from_slice(b"NANOWN01");
        encoded.push(match self.kind {
            ProcessOwnedDirectoryKind::SealedExecutable => 1,
            ProcessOwnedDirectoryKind::SealedBroker => 2,
            ProcessOwnedDirectoryKind::BrokerRequest => 3,
            ProcessOwnedDirectoryKind::BrokerStaging => 4,
        });
        for value in [
            self.root_identity.device,
            self.root_identity.inode,
            self.directory_identity.device,
            self.directory_identity.inode,
        ] {
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        let length = u32::try_from(self.name.as_bytes().len()).unwrap_or(u32::MAX);
        encoded.extend_from_slice(&length.to_be_bytes());
        encoded.extend_from_slice(self.name.as_bytes());
        encoded
    }

    /// Reconstructs and validates one redacted journal record.
    pub fn decode(encoded: &[u8]) -> Result<Self, ProcessError> {
        const HEADER: usize = 8 + 1 + (4 * 8) + 4;
        if encoded.len() < HEADER || encoded.get(..8) != Some(b"NANOWN01") {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "invalid process ownership record",
            )));
        }
        let kind = match encoded[8] {
            1 => ProcessOwnedDirectoryKind::SealedExecutable,
            2 => ProcessOwnedDirectoryKind::SealedBroker,
            3 => ProcessOwnedDirectoryKind::BrokerRequest,
            4 => ProcessOwnedDirectoryKind::BrokerStaging,
            _ => {
                return Err(ProcessError::Spawn(std::io::Error::other(
                    "invalid process ownership record",
                )));
            }
        };
        let mut offset = 9usize;
        let mut next_u64 = || -> Result<u64, ProcessError> {
            let end = offset.saturating_add(8);
            let bytes: [u8; 8] = encoded
                .get(offset..end)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| {
                    ProcessError::Spawn(std::io::Error::other("invalid process ownership record"))
                })?;
            offset = end;
            Ok(u64::from_be_bytes(bytes))
        };
        let root_identity = FileIdentity {
            device: next_u64()?,
            inode: next_u64()?,
        };
        let directory_identity = FileIdentity {
            device: next_u64()?,
            inode: next_u64()?,
        };
        let length_end = offset.saturating_add(4);
        let length_bytes: [u8; 4] = encoded
            .get(offset..length_end)
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| {
                ProcessError::Spawn(std::io::Error::other("invalid process ownership record"))
            })?;
        let length = usize::try_from(u32::from_be_bytes(length_bytes)).map_err(|_| {
            ProcessError::Spawn(std::io::Error::other("invalid process ownership record"))
        })?;
        let name = encoded
            .get(length_end..)
            .filter(|name| name.len() == length && length <= 255)
            .ok_or_else(|| {
                ProcessError::Spawn(std::io::Error::other("invalid process ownership record"))
            })?;
        if classify_owned_directory_name(name).map_err(ProcessError::Spawn)? != Some(kind) {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "invalid process ownership record",
            )));
        }
        Ok(Self {
            kind,
            name: OsString::from_vec(name.to_vec()),
            root_identity,
            directory_identity,
        })
    }

    /// Returns the expected directory layout.
    #[must_use]
    pub fn kind(&self) -> ProcessOwnedDirectoryKind {
        self.kind
    }

    /// Returns the exact single-component name beneath the enrolled root.
    #[must_use]
    pub fn name(&self) -> &OsStr {
        &self.name
    }

    /// Returns the retained root's `(device, inode)` identity.
    #[must_use]
    pub fn root_identity(&self) -> (u64, u64) {
        (self.root_identity.device, self.root_identity.inode)
    }

    /// Returns the owned directory's `(device, inode)` identity.
    #[must_use]
    pub fn directory_identity(&self) -> (u64, u64) {
        (
            self.directory_identity.device,
            self.directory_identity.inode,
        )
    }
}

impl fmt::Debug for ProcessOwnedDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessOwnedDirectory")
            .field("kind", &self.kind)
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// Exact digest and byte length pinned for one retained executable file.
///
/// This value lets canary and persistent authorities admit an executable larger
/// than the legacy 64 MiB byte-backed fixture limit without retaining the
/// executable in memory. The digest is SHA-256 over exactly [`Self::length`]
/// bytes. Admission accepts lengths from one byte through
/// [`MAX_ATTESTED_EXECUTABLE_BYTES`], inclusive.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct ExecutableFileAttestation {
    length: u64,
    sha256: [u8; 32],
}

impl ExecutableFileAttestation {
    /// Creates an attestation from an independently obtained exact length and
    /// SHA-256 digest.
    #[must_use]
    pub const fn new(length: u64, sha256: [u8; 32]) -> Self {
        Self { length, sha256 }
    }

    /// Returns the exact admitted byte length.
    #[must_use]
    pub const fn length(self) -> u64 {
        self.length
    }

    /// Returns the exact admitted SHA-256 digest.
    #[must_use]
    pub const fn sha256(self) -> [u8; 32] {
        self.sha256
    }
}

impl fmt::Debug for ExecutableFileAttestation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecutableFileAttestation")
            .field("length", &self.length)
            .field("sha256", &"[32-byte digest]")
            .finish()
    }
}

fn validate_attested_executable_length(
    attestation: ExecutableFileAttestation,
) -> std::io::Result<()> {
    if attestation.length == 0 || attestation.length > MAX_ATTESTED_EXECUTABLE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "attested executable length is outside the reviewed admission bound",
        ));
    }
    Ok(())
}

fn external_canary_root_metadata_is_valid(metadata: &Metadata, expected_owner: u32) -> bool {
    metadata.is_dir()
        && metadata.permissions().mode() & 0o777 == 0o700
        && metadata.uid() == expected_owner
}

fn pinned_external_metadata_is_valid(metadata: &Metadata, expected_owner: u32) -> bool {
    let mode = metadata.permissions().mode();
    metadata.is_file()
        && (metadata.uid() == 0 || metadata.uid() == expected_owner)
        && mode & 0o7000 == 0
        && mode & 0o100 != 0
        && mode & 0o022 == 0
}

fn canonical_external_mapping(
    path: &Path,
    expected_identity: FileIdentity,
) -> std::io::Result<bool> {
    if !path.is_absolute() || std::fs::canonicalize(path)? != path {
        return Ok(false);
    }
    let mapped = File::from(
        rustix::fs::open(
            path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    Ok(FileIdentity::of(&mapped.metadata()?) == expected_identity)
}

enum RetainedExecutableProof {
    ExactImage(Arc<[u8]>),
    Attested {
        attestation: ExecutableFileAttestation,
        admitted_metadata: ExecutableMetadataSnapshot,
    },
}

enum RetainedExecutableNamespace {
    RootRelative(PathBuf),
    External,
    CanonicalExternal(PathBuf),
}

/// Fixture-only open-file launch authority for one exact executable and CWD.
///
/// Both relative names are resolved beneath the retained root without
/// following symlinks and must identify the supplied open files. Linux launches
/// through `/proc/self/fd`, so a namespace replacement cannot select a new
/// executable or CWD. macOS launches a private, read-only clone of the admitted
/// executable and resolves the held CWD with `F_GETPATH`, with root-bound
/// identity checks immediately before and after spawn.
///
/// On macOS this closes substitution between application admission and kernel
/// launch, but it is not a sandbox against a hostile process running as the
/// same user: safe `std::process::Command` exposes neither FD-based `chdir` nor
/// an atomic namespace guard, so a same-UID rename in the final CWD
/// check-to-spawn window may start before the post-spawn check fails closed.
/// Consequently, this type and [`ProcessSupervisor::run_fixture_authorized`]
/// can only express hermetic fixture verification. Production enrollment must
/// use [`ProductionProcessLaunchAuthority`], whose private broker receives the
/// retained CWD descriptor and performs `fchdir` at the final boundary.
///
/// This type intentionally exposes neither paths nor file descriptors through
/// `Debug`.
pub struct FixtureProcessLaunchAuthority {
    root: File,
    root_identity: FileIdentity,
    executable: File,
    executable_identity: FileIdentity,
    executable_proof: RetainedExecutableProof,
    executable_namespace: RetainedExecutableNamespace,
    cwd: File,
    cwd_identity: FileIdentity,
    cwd_relative: PathBuf,
    #[cfg(target_os = "macos")]
    sealed_executable: Option<SealedExecutable>,
}

impl fmt::Debug for FixtureProcessLaunchAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureProcessLaunchAuthority")
            .field("kind", &"fixture-only-fd-bound-process-launch")
            .finish()
    }
}

impl FixtureProcessLaunchAuthority {
    /// Binds retained fixture files to exact nofollow-relative names beneath `root`.
    ///
    /// `admitted_executable_image` must exactly match the bytes readable from
    /// `executable`. This proof prevents a caller from authorizing one open-file
    /// identity while selecting unrelated bytes for the macOS sealed clone.
    pub fn new(
        root: File,
        executable: File,
        executable_relative: PathBuf,
        cwd: File,
        cwd_relative: PathBuf,
        admitted_executable_image: &[u8],
    ) -> Result<Self, ProcessError> {
        validate_authority_relative(&executable_relative).map_err(ProcessError::Spawn)?;
        validate_authority_relative(&cwd_relative).map_err(ProcessError::Spawn)?;
        let root_metadata = root.metadata().map_err(ProcessError::Spawn)?;
        let executable_metadata = executable.metadata().map_err(ProcessError::Spawn)?;
        let cwd_metadata = cwd.metadata().map_err(ProcessError::Spawn)?;
        if !root_metadata.is_dir()
            || !cwd_metadata.is_dir()
            || !executable_metadata.is_file()
            || executable_metadata.permissions().mode() & 0o111 == 0
            || admitted_executable_image.is_empty()
            || admitted_executable_image.len() > MAX_FIXTURE_EXECUTABLE_BYTES
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "launch authority files have invalid types or modes",
            )));
        }
        verify_executable_image(
            &executable,
            FileIdentity::of(&executable_metadata),
            admitted_executable_image,
        )
        .map_err(ProcessError::Spawn)?;
        #[cfg(target_os = "macos")]
        let sealed_executable = SealedExecutable::create(
            &root,
            admitted_executable_image,
            ProcessOwnedDirectoryKind::SealedExecutable,
        )
        .map_err(ProcessError::Spawn)?;
        #[cfg(not(target_os = "macos"))]
        let _ = admitted_executable_image;
        let authority = Self {
            root_identity: FileIdentity::of(&root_metadata),
            executable_identity: FileIdentity::of(&executable_metadata),
            executable_proof: RetainedExecutableProof::ExactImage(Arc::from(
                admitted_executable_image,
            )),
            cwd_identity: FileIdentity::of(&cwd_metadata),
            root,
            executable,
            executable_namespace: RetainedExecutableNamespace::RootRelative(executable_relative),
            cwd,
            cwd_relative,
            #[cfg(target_os = "macos")]
            sealed_executable: Some(sealed_executable),
        };
        authority.verify().map_err(ProcessError::Spawn)?;
        Ok(authority)
    }

    fn new_attested_disposable_canary(
        root: File,
        executable: File,
        executable_relative: PathBuf,
        cwd: File,
        cwd_relative: PathBuf,
        attestation: ExecutableFileAttestation,
    ) -> Result<Self, ProcessError> {
        validate_attested_executable_length(attestation).map_err(ProcessError::Spawn)?;
        validate_authority_relative(&executable_relative).map_err(ProcessError::Spawn)?;
        validate_authority_relative(&cwd_relative).map_err(ProcessError::Spawn)?;
        let root_metadata = root.metadata().map_err(ProcessError::Spawn)?;
        let executable_metadata = executable.metadata().map_err(ProcessError::Spawn)?;
        let cwd_metadata = cwd.metadata().map_err(ProcessError::Spawn)?;
        if !root_metadata.is_dir()
            || !cwd_metadata.is_dir()
            || !executable_metadata.is_file()
            || executable_metadata.permissions().mode() & 0o111 == 0
            || executable_metadata.len() != attestation.length
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "attested launch authority files have invalid types, modes, or lengths",
            )));
        }
        let executable_identity = FileIdentity::of(&executable_metadata);
        #[cfg(target_os = "macos")]
        let (sealed_executable, admitted_metadata) = SealedExecutable::create_from_attested_file(
            &root,
            &executable,
            executable_identity,
            attestation,
            ProcessOwnedDirectoryKind::SealedExecutable,
        )
        .map_err(ProcessError::Spawn)?;
        #[cfg(not(target_os = "macos"))]
        let admitted_metadata = verify_executable_file_attestation(
            &executable,
            executable_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        )
        .map_err(ProcessError::Spawn)?;
        let authority = Self {
            root_identity: FileIdentity::of(&root_metadata),
            executable_identity,
            executable_proof: RetainedExecutableProof::Attested {
                attestation,
                admitted_metadata,
            },
            cwd_identity: FileIdentity::of(&cwd_metadata),
            root,
            executable,
            executable_namespace: RetainedExecutableNamespace::RootRelative(executable_relative),
            cwd,
            cwd_relative,
            #[cfg(target_os = "macos")]
            sealed_executable: Some(sealed_executable),
        };
        authority.verify().map_err(ProcessError::Spawn)?;
        Ok(authority)
    }

    fn new_external_attested_disposable_canary(
        root: File,
        executable: File,
        cwd: File,
        cwd_relative: PathBuf,
        attestation: ExecutableFileAttestation,
    ) -> Result<Self, ProcessError> {
        validate_attested_executable_length(attestation).map_err(ProcessError::Spawn)?;
        validate_authority_relative(&cwd_relative).map_err(ProcessError::Spawn)?;
        let root_metadata = root.metadata().map_err(ProcessError::Spawn)?;
        let executable_metadata = executable.metadata().map_err(ProcessError::Spawn)?;
        let cwd_metadata = cwd.metadata().map_err(ProcessError::Spawn)?;
        if !external_canary_root_metadata_is_valid(
            &root_metadata,
            rustix::process::geteuid().as_raw(),
        ) || !cwd_metadata.is_dir()
            || !executable_metadata.is_file()
            || executable_metadata.permissions().mode() & 0o111 == 0
            || executable_metadata.len() != attestation.length
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "external attested launch authority has invalid types, modes, or lengths",
            )));
        }
        #[cfg(target_os = "macos")]
        if !scan_recovery_root(&root, &[])
            .map_err(ProcessError::Spawn)?
            .is_empty()
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "external attested canary root is not fresh",
            )));
        }
        let mapped_cwd =
            open_authority_path(&root, &cwd_relative, true).map_err(ProcessError::Spawn)?;
        if FileIdentity::of(&mapped_cwd.metadata().map_err(ProcessError::Spawn)?)
            != FileIdentity::of(&cwd_metadata)
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "external attested launch CWD escapes its retained capability root",
            )));
        }
        let executable_identity = FileIdentity::of(&executable_metadata);
        let admitted_metadata = verify_executable_file_attestation(
            &executable,
            executable_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        )
        .map_err(ProcessError::Spawn)?;
        #[cfg(target_os = "macos")]
        let (sealed_executable, verified_metadata) =
            seal_external_attested_executable_with_observers(
                &root,
                &executable,
                executable_identity,
                attestation,
                admitted_metadata,
                |_, _| Ok(()),
                || Ok(()),
            )
            .map_err(ProcessError::Spawn)?;
        #[cfg(not(target_os = "macos"))]
        let verified_metadata = verify_executable_file_attestation(
            &executable,
            executable_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        )
        .map_err(ProcessError::Spawn)?;
        if verified_metadata != admitted_metadata {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "external attested executable changed after its sealed admission",
            )));
        }
        let authority = Self {
            root_identity: FileIdentity::of(&root_metadata),
            executable_identity,
            executable_proof: RetainedExecutableProof::Attested {
                attestation,
                admitted_metadata,
            },
            cwd_identity: FileIdentity::of(&cwd_metadata),
            root,
            executable,
            executable_namespace: RetainedExecutableNamespace::External,
            cwd,
            cwd_relative,
            #[cfg(target_os = "macos")]
            sealed_executable: Some(sealed_executable),
        };
        authority.verify().map_err(ProcessError::Spawn)?;
        Ok(authority)
    }

    #[cfg(target_os = "macos")]
    fn new_persistent_external(
        root: File,
        executable: File,
        canonical_executable_path: PathBuf,
        cwd: File,
        cwd_relative: PathBuf,
        attestation: ExecutableFileAttestation,
    ) -> Result<Self, ProcessError> {
        validate_attested_executable_length(attestation).map_err(ProcessError::Spawn)?;
        validate_authority_relative(&cwd_relative).map_err(ProcessError::Spawn)?;
        let root_metadata = root.metadata().map_err(ProcessError::Spawn)?;
        let executable_metadata = executable.metadata().map_err(ProcessError::Spawn)?;
        let cwd_metadata = cwd.metadata().map_err(ProcessError::Spawn)?;
        let expected_owner = rustix::process::geteuid().as_raw();
        if !external_canary_root_metadata_is_valid(&root_metadata, expected_owner)
            || !cwd_metadata.is_dir()
            || !pinned_external_metadata_is_valid(&executable_metadata, expected_owner)
            || executable_metadata.len() != attestation.length
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "persistent launch authority has invalid root, CWD, target, owner, mode, or length",
            )));
        }
        if !scan_recovery_root(&root, &[])
            .map_err(ProcessError::Spawn)?
            .is_empty()
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "persistent launch root contains unreconciled process-owned state",
            )));
        }
        let mapped_cwd =
            open_authority_path(&root, &cwd_relative, true).map_err(ProcessError::Spawn)?;
        if FileIdentity::of(&mapped_cwd.metadata().map_err(ProcessError::Spawn)?)
            != FileIdentity::of(&cwd_metadata)
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "persistent launch CWD escapes its retained capability root",
            )));
        }
        let executable_identity = FileIdentity::of(&executable_metadata);
        if !canonical_external_mapping(&canonical_executable_path, executable_identity)
            .map_err(ProcessError::Spawn)?
        {
            return Err(ProcessError::Spawn(std::io::Error::other(
                "persistent launch target is not its admitted canonical namespace mapping",
            )));
        }
        let admitted_metadata = verify_executable_file_attestation(
            &executable,
            executable_identity,
            attestation,
            ExecutableFileMode::PinnedExternal,
        )
        .map_err(ProcessError::Spawn)?;
        let authority = Self {
            root_identity: FileIdentity::of(&root_metadata),
            executable_identity,
            executable_proof: RetainedExecutableProof::Attested {
                attestation,
                admitted_metadata,
            },
            executable_namespace: RetainedExecutableNamespace::CanonicalExternal(
                canonical_executable_path,
            ),
            cwd_identity: FileIdentity::of(&cwd_metadata),
            root,
            executable,
            cwd,
            cwd_relative,
            sealed_executable: None,
        };
        authority.verify().map_err(ProcessError::Spawn)?;
        Ok(authority)
    }

    fn verify(&self) -> std::io::Result<()> {
        self.verify_retained()?;
        let mapped_cwd = open_authority_path(&self.root, &self.cwd_relative, true)?;
        let executable_mapping_is_valid = match &self.executable_namespace {
            RetainedExecutableNamespace::RootRelative(relative) => {
                let mapped_executable = open_authority_path(&self.root, relative, false)?;
                FileIdentity::of(&mapped_executable.metadata()?) == self.executable_identity
            }
            RetainedExecutableNamespace::External => true,
            RetainedExecutableNamespace::CanonicalExternal(path) => {
                canonical_external_mapping(path, self.executable_identity)?
            }
        };
        if !executable_mapping_is_valid
            || FileIdentity::of(&mapped_cwd.metadata()?) != self.cwd_identity
        {
            return Err(std::io::Error::other(
                "process launch name no longer maps to its admitted identity",
            ));
        }
        Ok(())
    }

    fn verify_retained(&self) -> std::io::Result<()> {
        let root_metadata = self.root.metadata()?;
        let executable_metadata = self.executable.metadata()?;
        let cwd_metadata = self.cwd.metadata()?;
        let root_mode_is_valid = match &self.executable_namespace {
            RetainedExecutableNamespace::RootRelative(_) => true,
            RetainedExecutableNamespace::External
            | RetainedExecutableNamespace::CanonicalExternal(_) => {
                external_canary_root_metadata_is_valid(
                    &root_metadata,
                    rustix::process::geteuid().as_raw(),
                )
            }
        };
        if !root_metadata.is_dir()
            || FileIdentity::of(&root_metadata) != self.root_identity
            || !root_mode_is_valid
            || !executable_metadata.is_file()
            || executable_metadata.permissions().mode() & 0o111 == 0
            || FileIdentity::of(&executable_metadata) != self.executable_identity
            || !cwd_metadata.is_dir()
            || FileIdentity::of(&cwd_metadata) != self.cwd_identity
        {
            return Err(std::io::Error::other(
                "retained process launch identity changed",
            ));
        }
        match &self.executable_proof {
            RetainedExecutableProof::ExactImage(image) => {
                verify_executable_image(&self.executable, self.executable_identity, image)?;
            }
            RetainedExecutableProof::Attested {
                attestation,
                admitted_metadata,
            } => verify_admitted_executable_metadata(
                &self.executable,
                self.executable_identity,
                *attestation,
                *admitted_metadata,
                ExecutableFileMode::RetainedSource,
            )?,
        }
        #[cfg(target_os = "macos")]
        if let Some(sealed_executable) = &self.sealed_executable {
            sealed_executable.verify(&self.root)?;
        }
        Ok(())
    }

    fn paths(&self) -> std::io::Result<(PathBuf, PathBuf)> {
        self.verify()?;
        #[cfg(target_os = "linux")]
        {
            Ok((
                PathBuf::from(format!("/proc/self/fd/{}", self.executable.as_raw_fd())),
                PathBuf::from(format!("/proc/self/fd/{}", self.cwd.as_raw_fd())),
            ))
        }
        #[cfg(target_os = "macos")]
        {
            let executable = self
                .sealed_executable
                .as_ref()
                .ok_or_else(|| std::io::Error::other("fixture target is not sealed"))?
                .path()?;
            let cwd = path_from_fd(&self.cwd)?;
            self.verify()?;
            Ok((executable, cwd))
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "FD-bound process launch is supported only on Linux and macOS",
        ))
    }
}

/// Production launch authority for one exact executable and working directory.
///
/// Linux can construct and verify this authority, but production execution is
/// intentionally rejected until a pre-exec durable launcher is enrolled there.
/// On macOS, the authority additionally admits and seals this package's private
/// broker. Each run passes the retained CWD as broker descriptor 0; the broker
/// waits for a durable authenticated grant, performs a safe `fchdir`, and
/// `exec`s the sealed target in place. Target argv, environment, and stdin
/// travel only through private, bounded control files.
///
/// The final target spawn remains path-based on macOS because stable safe Rust
/// exposes no FD-exec primitive there. Canary targets name mode-0500 sealed
/// clones. Persistent targets name their admitted original canonical paths so
/// runtime-relative assets remain available; their mapping, identity, owner,
/// mode, length, and digest are checked at admission and again in the broker
/// immediately before `exec`.
///
/// The macOS broker begins as the process-group leader and preserves that exact
/// kernel identity across `exec`; no nested target process or broker epilogue
/// exists after release.
pub struct ProductionProcessLaunchAuthority {
    inner: FixtureProcessLaunchAuthority,
    #[cfg(target_os = "macos")]
    target_policy: ProductionTargetPolicy,
    #[cfg(target_os = "macos")]
    broker_proof: RetainedBrokerProof,
    #[cfg(target_os = "macos")]
    sealed_broker: SealedExecutable,
    #[cfg(target_os = "macos")]
    broker_staging: BrokerStagingRoot,
    #[cfg(target_os = "macos")]
    short_gate_parent: ShortGateParent,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Eq, PartialEq)]
enum ProductionTargetPolicy {
    SealedCanary,
    PinnedExternal,
}

#[cfg(target_os = "macos")]
enum RetainedBrokerProof {
    DiscoveredExactImage {
        source: File,
        source_path: PathBuf,
        source_identity: FileIdentity,
        admitted_image: Arc<[u8]>,
    },
}

#[cfg(target_os = "macos")]
impl RetainedBrokerProof {
    fn verify(&self) -> std::io::Result<()> {
        match self {
            Self::DiscoveredExactImage {
                source,
                source_path,
                source_identity,
                admitted_image,
            } => {
                verify_executable_image(source, *source_identity, admitted_image)?;
                let mapped = File::from(
                    rustix::fs::open(
                        source_path,
                        rustix::fs::OFlags::RDONLY
                            | rustix::fs::OFlags::NOFOLLOW
                            | rustix::fs::OFlags::CLOEXEC,
                        rustix::fs::Mode::empty(),
                    )
                    .map_err(std::io::Error::from)?,
                );
                if FileIdentity::of(&mapped.metadata()?) != *source_identity {
                    return Err(std::io::Error::other(
                        "process broker source identity changed",
                    ));
                }
                Ok(())
            }
        }
    }
}

impl fmt::Debug for ProductionProcessLaunchAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProductionProcessLaunchAuthority")
            .field("kind", &"production-fd-bound-process-launch")
            .finish()
    }
}

impl ProductionProcessLaunchAuthority {
    /// Enrolls one original canonical executable for persistent macOS launches.
    ///
    /// `root` must be an exclusively writer-owned mode-0700 private directory,
    /// and `cwd_relative` must map the retained `cwd` beneath it. Before calling,
    /// the application must reconcile every recorded process group to exact
    /// absence, acquire the exclusive root-writer lease, and call
    /// [`recover_orphaned_process_directories`] with the prior journal records.
    /// The root must then contain no recognized process-owned residue.
    ///
    /// `canonical_executable_path` must be the absolute canonical namespace path
    /// for `executable`. The target must be owned by the current user, have owner
    /// execute permission, have no set-id bits, and not be group/world writable;
    /// ordinary owner-writable mode 0755 is supported. Its exact identity, full
    /// mode, owner/group, timestamps, length, and SHA-256 digest are pinned.
    ///
    /// `persist_owned_directories` must durably replace the application's exact
    /// ownership journal while it still holds the exclusive root-writer lease.
    /// No launch authority is returned, and therefore no child can start, until
    /// that callback succeeds. A callback error removes newly created owned
    /// directories and returns a construction failure. The callback is an I/O
    /// operation, not a caller-supplied durability boolean.
    ///
    /// Linux and other platforms return `Unsupported` before invoking the
    /// callback because they do not have the enrolled pre-exec gate.
    pub fn new<F>(
        root: File,
        executable: File,
        canonical_executable_path: PathBuf,
        cwd: File,
        cwd_relative: PathBuf,
        attestation: ExecutableFileAttestation,
        persist_owned_directories: F,
    ) -> Result<Self, ProcessError>
    where
        F: FnOnce(&[ProcessOwnedDirectory]) -> std::io::Result<()>,
    {
        #[cfg(target_os = "macos")]
        {
            let inner = FixtureProcessLaunchAuthority::new_persistent_external(
                root,
                executable,
                canonical_executable_path,
                cwd,
                cwd_relative,
                attestation,
            )?;
            let authority = Self::finish_macos(inner, ProductionTargetPolicy::PinnedExternal)?;
            let records = authority.owned_directories();
            persist_owned_directories(&records).map_err(ProcessError::Spawn)?;
            authority.verify().map_err(ProcessError::Spawn)?;
            Ok(authority)
        }
        #[cfg(not(target_os = "macos"))]
        {
            drop((
                root,
                executable,
                canonical_executable_path,
                cwd,
                cwd_relative,
                attestation,
                persist_owned_directories,
            ));
            Err(ProcessError::Spawn(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "persistent production launch requires the macOS pre-exec gate",
            )))
        }
    }

    /// Constructs the exact-image launcher for a disposable, private canary.
    ///
    /// This constructor does not enroll production. The caller must use a fresh
    /// private home, run one sequential canary, and discard all state afterward.
    /// A parent crash may leave a bounded private gate directory in
    /// `/private/tmp`, so persistent or concurrent use is rejected by policy.
    pub fn new_disposable_canary(
        root: File,
        executable: File,
        executable_relative: PathBuf,
        cwd: File,
        cwd_relative: PathBuf,
        admitted_executable_image: &[u8],
    ) -> Result<Self, ProcessError> {
        let inner = FixtureProcessLaunchAuthority::new(
            root,
            executable,
            executable_relative,
            cwd,
            cwd_relative,
            admitted_executable_image,
        )?;
        Self::finish_disposable_canary(inner)
    }

    /// Constructs a digest-pinned disposable canary without retaining its image.
    ///
    /// The retained executable descriptor must match `attestation` exactly. On
    /// macOS its bytes are copied into the private sealed namespace with a
    /// fixed-size buffer; both source and destination identities, lengths, and
    /// SHA-256 digests are checked before this returns. The target is always
    /// launched from that sealed absolute path, never through ambient `PATH`.
    /// Lengths outside `1..=`[`MAX_ATTESTED_EXECUTABLE_BYTES`] are rejected
    /// before any sealed directory is created or any byte is copied or hashed.
    ///
    /// This is still canary-only: the caller must use a fresh private home, run
    /// one sequential canary, and discard all state afterward. Persistent
    /// production enrollment remains fail-closed through [`Self::new`].
    pub fn new_disposable_canary_from_attested_file(
        root: File,
        executable: File,
        executable_relative: PathBuf,
        cwd: File,
        cwd_relative: PathBuf,
        attestation: ExecutableFileAttestation,
    ) -> Result<Self, ProcessError> {
        let inner = FixtureProcessLaunchAuthority::new_attested_disposable_canary(
            root,
            executable,
            executable_relative,
            cwd,
            cwd_relative,
            attestation,
        )?;
        Self::finish_disposable_canary(inner)
    }

    /// Constructs a digest-pinned disposable canary from an externally opened file.
    ///
    /// The caller opens `executable` before invoking this API; no source path is
    /// accepted, retained, or resolved. The source may therefore live outside
    /// the fresh private capability root without first being copied beneath it.
    /// `cwd_relative` is still resolved component-by-component beneath `root`
    /// without following symlinks and must identify the retained `cwd`.
    ///
    /// The capability root must be a private mode-0700 directory. The source is
    /// checked against `attestation` before and after admission with fixed-size
    /// buffers. On macOS the admitted target is a mode-0500 sealed clone beneath
    /// `root`; on Linux the verified source descriptor remains the authority's
    /// retained executable proof. Linux production execution still fails closed
    /// until its durable pre-exec gate is implemented; constructing this value
    /// does not enable a Linux canary launch. Lengths outside
    /// `1..=`[`MAX_ATTESTED_EXECUTABLE_BYTES`] are rejected before owned state is
    /// created.
    ///
    /// This is canary-only: the caller must create a fresh root, run one
    /// sequential canary, and discard the root afterward. Persistent production
    /// enrollment remains fail-closed through [`Self::new`].
    pub fn new_disposable_canary_from_external_attested_file(
        root: File,
        executable: File,
        cwd: File,
        cwd_relative: PathBuf,
        attestation: ExecutableFileAttestation,
    ) -> Result<Self, ProcessError> {
        let inner = FixtureProcessLaunchAuthority::new_external_attested_disposable_canary(
            root,
            executable,
            cwd,
            cwd_relative,
            attestation,
        )?;
        Self::finish_disposable_canary(inner)
    }

    fn finish_disposable_canary(
        inner: FixtureProcessLaunchAuthority,
    ) -> Result<Self, ProcessError> {
        #[cfg(target_os = "macos")]
        {
            Self::finish_macos(inner, ProductionTargetPolicy::SealedCanary)
        }
        #[cfg(not(target_os = "macos"))]
        {
            let authority = Self { inner };
            authority.verify().map_err(ProcessError::Spawn)?;
            Ok(authority)
        }
    }

    #[cfg(target_os = "macos")]
    fn finish_macos(
        inner: FixtureProcessLaunchAuthority,
        target_policy: ProductionTargetPolicy,
    ) -> Result<Self, ProcessError> {
        let short_gate_parent = ShortGateParent::open_system().map_err(ProcessError::Spawn)?;
        let (broker_source, broker_source_path, broker_source_identity, broker_image) =
            discover_process_broker().map_err(ProcessError::Spawn)?;
        let sealed_broker = SealedExecutable::create(
            &inner.root,
            &broker_image,
            ProcessOwnedDirectoryKind::SealedBroker,
        )
        .map_err(ProcessError::Spawn)?;
        let broker_staging = BrokerStagingRoot::create(&inner.root, inner.root_identity)
            .map_err(ProcessError::Spawn)?;
        let authority = Self {
            inner,
            target_policy,
            broker_proof: RetainedBrokerProof::DiscoveredExactImage {
                source: broker_source,
                source_path: broker_source_path,
                source_identity: broker_source_identity,
                admitted_image: Arc::from(broker_image),
            },
            sealed_broker,
            broker_staging,
            short_gate_parent,
        };
        authority.verify().map_err(ProcessError::Spawn)?;
        Ok(authority)
    }

    /// Returns exact, non-sensitive directory records for root-lock recovery.
    #[must_use]
    pub fn owned_directories(&self) -> Vec<ProcessOwnedDirectory> {
        #[cfg(target_os = "macos")]
        {
            let mut records = Vec::with_capacity(3);
            if let Some(sealed_executable) = &self.inner.sealed_executable {
                records.push(sealed_executable.ownership(self.inner.root_identity));
            }
            records.push(self.sealed_broker.ownership(self.inner.root_identity));
            records.push(self.broker_staging.ownership());
            records
        }
        #[cfg(not(target_os = "macos"))]
        {
            Vec::new()
        }
    }

    fn verify(&self) -> std::io::Result<()> {
        self.inner.verify()?;
        #[cfg(target_os = "macos")]
        {
            match self.target_policy {
                ProductionTargetPolicy::SealedCanary if self.inner.sealed_executable.is_none() => {
                    return Err(std::io::Error::other(
                        "sealed canary target policy has no sealed executable",
                    ));
                }
                ProductionTargetPolicy::PinnedExternal
                    if self.inner.sealed_executable.is_some()
                        || !matches!(
                            &self.inner.executable_namespace,
                            RetainedExecutableNamespace::CanonicalExternal(_)
                        ) =>
                {
                    return Err(std::io::Error::other(
                        "pinned external target policy has an invalid authority",
                    ));
                }
                ProductionTargetPolicy::SealedCanary | ProductionTargetPolicy::PinnedExternal => {}
            }
            self.broker_proof.verify()?;
            self.sealed_broker.verify(&self.inner.root)?;
            self.broker_staging
                .verify_for_launch(self.inner.root_identity, &self.short_gate_parent)?;
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    fn verify_after_spawn(&self) -> std::io::Result<()> {
        self.inner.verify_retained()?;
        self.broker_proof.verify()?;
        self.sealed_broker.verify(&self.inner.root).and_then(|()| {
            self.broker_staging
                .verify_for_launch(self.inner.root_identity, &self.short_gate_parent)
        })
    }
}

/// Reconciles exact private process directories proven to belong to a crashed owner.
///
/// The caller must hold the enrolled root's exclusive root lock, after exact
/// process recovery has proved every broker group absent, and must call this
/// before constructing any live process authority. A broker status file is not
/// an absence proof: invoking this while a broker is live would violate the
/// root-lock composition and may remove its private channel. `owned` must be
/// the exact records durably persisted while the previous owner held that lock;
/// this function never infers ownership from a filename or layout. Nested gate
/// roots are admitted only by the closed ownership records inside those exact
/// directories. An exact gate already absent is treated as reconciled. Any
/// malformed or unrecorded process namespace, symlink, wrong identity, writable
/// finalized directory, unknown entry, or invalid file mode fails closed. The
/// returned count is the number of top-level enrolled-root records removed.
pub fn recover_orphaned_process_directories(
    root: &File,
    owned: &[ProcessOwnedDirectory],
) -> Result<usize, ProcessError> {
    let root_metadata = root.metadata().map_err(ProcessError::Spawn)?;
    if !root_metadata.is_dir() {
        return Err(ProcessError::Spawn(std::io::Error::other(
            "process recovery root is not a directory",
        )));
    }
    #[cfg(target_os = "macos")]
    {
        let gate_parent = ShortGateParent::open_system().map_err(ProcessError::Spawn)?;
        recover_macos_owned_directories(root, FileIdentity::of(&root_metadata), owned, &gate_parent)
            .map_err(ProcessError::Spawn)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = owned;
        Ok(0)
    }
}

#[cfg(target_os = "macos")]
fn recover_macos_owned_directories(
    root: &File,
    root_identity: FileIdentity,
    owned: &[ProcessOwnedDirectory],
    gate_parent: &ShortGateParent,
) -> std::io::Result<usize> {
    gate_parent.verify()?;
    let mut names = Vec::with_capacity(owned.len());
    for record in owned {
        if record.root_identity != root_identity
            || classify_owned_directory_name(record.name.as_bytes())? != Some(record.kind)
            || names.iter().any(|name: &OsString| name == &record.name)
        {
            return Err(std::io::Error::other(
                "invalid or duplicate process ownership record",
            ));
        }
        names.push(record.name.clone());
    }

    let present = scan_recovery_root(root, owned)?;
    let mut validated = Vec::with_capacity(present.len());
    for record in owned {
        if present.iter().any(|name| name == &record.name) {
            validated.push(validate_owned_directory(
                root,
                root_identity,
                record,
                gate_parent,
            )?);
        }
    }
    let recovered = validated.len();
    for directory in validated {
        remove_validated_owned_directory(root, directory, gate_parent)?;
    }
    if FileIdentity::of(&root.metadata()?) != root_identity {
        return Err(std::io::Error::other(
            "process recovery root identity changed",
        ));
    }
    if !scan_recovery_root(root, &[])?.is_empty() {
        return Err(std::io::Error::other(
            "process recovery left recognized namespace residue",
        ));
    }
    root.sync_all()?;
    Ok(recovered)
}

#[cfg(target_os = "macos")]
fn scan_recovery_root(
    root: &File,
    owned: &[ProcessOwnedDirectory],
) -> std::io::Result<Vec<OsString>> {
    let mut present = Vec::new();
    for raw_name in owned_directory_entry_names(root)? {
        let Some(kind) = classify_owned_directory_name(&raw_name)? else {
            continue;
        };
        let name = OsString::from_vec(raw_name);
        if !owned
            .iter()
            .any(|record| record.kind == kind && record.name == name)
        {
            return Err(std::io::Error::other(
                "unrecorded process-owned directory residue",
            ));
        }
        present.push(name);
    }
    Ok(present)
}

fn classify_owned_directory_name(
    name: &[u8],
) -> std::io::Result<Option<ProcessOwnedDirectoryKind>> {
    for (prefix, kind) in [
        (
            b".orchestrator-launch-".as_slice(),
            ProcessOwnedDirectoryKind::SealedExecutable,
        ),
        (
            b".orchestrator-broker-".as_slice(),
            ProcessOwnedDirectoryKind::SealedBroker,
        ),
        (
            b".orchestrator-request-".as_slice(),
            ProcessOwnedDirectoryKind::BrokerRequest,
        ),
        (
            b".orchestrator-staging-".as_slice(),
            ProcessOwnedDirectoryKind::BrokerStaging,
        ),
    ] {
        let Some(suffix) = name.strip_prefix(prefix) else {
            continue;
        };
        let mut fields = suffix.split(|byte| *byte == b'-');
        let pid = fields.next().unwrap_or_default();
        let nonce = fields.next().unwrap_or_default();
        if pid.is_empty()
            || nonce.is_empty()
            || fields.next().is_some()
            || !pid.iter().all(u8::is_ascii_digit)
            || !nonce.iter().all(u8::is_ascii_digit)
        {
            return Err(std::io::Error::other(
                "malformed process-owned directory name",
            ));
        }
        return Ok(Some(kind));
    }
    Ok(None)
}

#[cfg(target_os = "macos")]
struct ValidatedOwnedDirectory {
    directory: File,
    name: OsString,
    entries: Vec<Vec<u8>>,
    broker_requests: Vec<ValidatedBrokerRequest>,
}

#[cfg(target_os = "macos")]
struct ValidatedBrokerRequest {
    directory: File,
    name: Vec<u8>,
    entries: Vec<Vec<u8>>,
    gate_root: Option<ValidatedShortGateRoot>,
}

#[cfg(target_os = "macos")]
struct ValidatedShortGateRoot {
    directory: File,
    name: String,
    socket_identity: Option<FileIdentity>,
}

#[cfg(target_os = "macos")]
fn validate_owned_directory(
    root: &File,
    enrolled_root_identity: FileIdentity,
    record: &ProcessOwnedDirectory,
    gate_parent: &ShortGateParent,
) -> std::io::Result<ValidatedOwnedDirectory> {
    let name_path = Path::new(&record.name);
    let directory = File::from(
        rustix::fs::openat(
            root,
            name_path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let directory_metadata = directory.metadata()?;
    let directory_identity = FileIdentity::of(&directory_metadata);
    let mode = directory_metadata.permissions().mode() & 0o777;
    let valid_mode = match record.kind {
        ProcessOwnedDirectoryKind::SealedExecutable
        | ProcessOwnedDirectoryKind::SealedBroker
        | ProcessOwnedDirectoryKind::BrokerRequest => mode == 0o500,
        ProcessOwnedDirectoryKind::BrokerStaging => mode == 0o700,
    };
    if !directory_metadata.is_dir()
        || directory_identity != record.directory_identity
        || !valid_mode
    {
        return Err(std::io::Error::other(
            "process-owned directory has an invalid mode",
        ));
    }

    if record.kind == ProcessOwnedDirectoryKind::BrokerStaging {
        let mut broker_requests = Vec::new();
        for name in owned_directory_entry_names(&directory)? {
            if classify_owned_directory_name(&name)?
                != Some(ProcessOwnedDirectoryKind::BrokerRequest)
            {
                return Err(std::io::Error::other(
                    "broker staging parent contains an unknown entry",
                ));
            }
            broker_requests.push(validate_broker_request_directory(
                &directory,
                directory_identity,
                enrolled_root_identity,
                gate_parent,
                name,
            )?);
        }
        return Ok(ValidatedOwnedDirectory {
            directory,
            name: record.name.clone(),
            entries: Vec::new(),
            broker_requests,
        });
    }

    let allowed: &[&str] = match record.kind {
        ProcessOwnedDirectoryKind::SealedExecutable | ProcessOwnedDirectoryKind::SealedBroker => {
            &["executable"]
        }
        ProcessOwnedDirectoryKind::BrokerRequest => &["request", "status", "stdin"],
        ProcessOwnedDirectoryKind::BrokerStaging => &[],
    };
    let actual = owned_directory_entry_names(&directory)?;
    if actual.iter().any(|name| {
        !allowed
            .iter()
            .any(|allowed| name.as_slice() == allowed.as_bytes())
    }) {
        return Err(std::io::Error::other(
            "process-owned directory contains unknown entries",
        ));
    }
    let finalized_complete = match record.kind {
        ProcessOwnedDirectoryKind::SealedExecutable | ProcessOwnedDirectoryKind::SealedBroker => {
            actual.iter().any(|name| name.as_slice() == b"executable")
        }
        ProcessOwnedDirectoryKind::BrokerRequest => {
            actual.iter().any(|name| name.as_slice() == b"request")
                && actual.iter().any(|name| name.as_slice() == b"status")
        }
        ProcessOwnedDirectoryKind::BrokerStaging => true,
    };
    if !finalized_complete {
        return Err(std::io::Error::other(
            "finalized process-owned directory is incomplete",
        ));
    }
    for entry_name in &actual {
        let file = File::from(
            rustix::fs::openat(
                &directory,
                Path::new(OsStr::from_bytes(entry_name)),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let metadata = file.metadata()?;
        let expected_mode = match entry_name.as_slice() {
            b"executable" => 0o500,
            b"status" => 0o600,
            _ => 0o400,
        };
        if !metadata.is_file() || metadata.permissions().mode() & 0o777 != expected_mode {
            return Err(std::io::Error::other(
                "process-owned file has an invalid type or mode",
            ));
        }
        if entry_name == b"status" {
            let _status = broker::read_status(&file, FileIdentity::of(&metadata))?;
        }
    }

    let remapped = File::from(
        rustix::fs::openat(
            root,
            name_path,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if FileIdentity::of(&remapped.metadata()?) != directory_identity {
        return Err(std::io::Error::other(
            "process-owned directory identity changed during recovery",
        ));
    }

    Ok(ValidatedOwnedDirectory {
        directory,
        name: record.name.clone(),
        entries: actual,
        broker_requests: Vec::new(),
    })
}

#[cfg(target_os = "macos")]
fn validate_broker_request_directory(
    parent: &File,
    staging_identity: FileIdentity,
    enrolled_root_identity: FileIdentity,
    gate_parent: &ShortGateParent,
    name: Vec<u8>,
) -> std::io::Result<ValidatedBrokerRequest> {
    let directory = File::from(
        rustix::fs::openat(
            parent,
            Path::new(OsStr::from_bytes(&name)),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let metadata = directory.metadata()?;
    let identity = FileIdentity::of(&metadata);
    let mode = metadata.permissions().mode() & 0o777;
    if !metadata.is_dir() || (mode != 0o500 && mode != 0o700) {
        return Err(std::io::Error::other(
            "broker request directory has an invalid type or mode",
        ));
    }
    let entries = owned_directory_entry_names(&directory)?;
    if entries.iter().any(|entry| {
        entry != GATE_ROOT_RECORD_NAME.as_bytes()
            && entry != b"request"
            && entry != b"status"
            && entry != b"stdin"
    }) || (mode == 0o500
        && (!entries.iter().any(|entry| entry == b"request")
            || !entries.iter().any(|entry| entry == b"status")
            || !entries
                .iter()
                .any(|entry| entry == GATE_ROOT_RECORD_NAME.as_bytes())))
    {
        return Err(std::io::Error::other(
            "broker request directory has an invalid layout",
        ));
    }
    let mut gate_root = None;
    for entry in &entries {
        if entry == GATE_ROOT_RECORD_NAME.as_bytes() {
            let ownership = read_gate_root_ownership(
                &directory,
                enrolled_root_identity,
                staging_identity,
                identity,
                gate_parent,
            )?;
            gate_root = validate_short_gate_root(gate_parent, &ownership)?;
            continue;
        }
        let file = File::from(
            rustix::fs::openat(
                &directory,
                Path::new(OsStr::from_bytes(entry)),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let metadata = file.metadata()?;
        let expected_mode = if entry == b"status" { 0o600 } else { 0o400 };
        if !metadata.is_file() || metadata.permissions().mode() & 0o777 != expected_mode {
            return Err(std::io::Error::other(
                "broker request file has an invalid type or mode",
            ));
        }
        if mode == 0o500 && entry == b"status" {
            let _status = broker::read_status(&file, FileIdentity::of(&metadata))?;
        }
    }
    let remapped = File::from(
        rustix::fs::openat(
            parent,
            Path::new(OsStr::from_bytes(&name)),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if FileIdentity::of(&remapped.metadata()?) != identity {
        return Err(std::io::Error::other(
            "broker request identity changed during recovery validation",
        ));
    }
    Ok(ValidatedBrokerRequest {
        directory,
        name,
        entries,
        gate_root,
    })
}

#[cfg(target_os = "macos")]
fn read_gate_root_ownership(
    request: &File,
    enrolled_root_identity: FileIdentity,
    staging_identity: FileIdentity,
    request_identity: FileIdentity,
    gate_parent: &ShortGateParent,
) -> std::io::Result<GateRootOwnership> {
    use std::os::unix::fs::FileExt;

    let open_record = || {
        rustix::fs::openat(
            request,
            Path::new(GATE_ROOT_RECORD_NAME),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map(File::from)
        .map_err(std::io::Error::from)
    };
    let file = open_record()?;
    let before = file.metadata()?;
    let length = usize::try_from(before.len())
        .map_err(|_| std::io::Error::other("launcher gate ownership record is too large"))?;
    if !before.is_file()
        || before.permissions().mode() & 0o777 != 0o400
        || before.uid() != rustix::process::geteuid().as_raw()
        || before.nlink() != 1
        || !(GATE_ROOT_RECORD_FIXED_BYTES..=MAX_GATE_ROOT_RECORD_BYTES).contains(&length)
    {
        return Err(std::io::Error::other(
            "launcher gate ownership record has an invalid type, mode, or length",
        ));
    }
    let identity = FileIdentity::of(&before);
    let mut encoded = vec![0_u8; length];
    let mut offset = 0usize;
    while offset < encoded.len() {
        let read = file.read_at(&mut encoded[offset..], offset as u64)?;
        if read == 0 {
            return Err(std::io::Error::other(
                "launcher gate ownership record is truncated",
            ));
        }
        offset = offset.saturating_add(read);
    }
    let after = file.metadata()?;
    let remapped = open_record()?;
    if FileIdentity::of(&after) != identity
        || after.len() != before.len()
        || after.permissions().mode() & 0o777 != 0o400
        || FileIdentity::of(&remapped.metadata()?) != identity
    {
        return Err(std::io::Error::other(
            "launcher gate ownership record identity changed",
        ));
    }
    let ownership = GateRootOwnership::decode(&encoded)?;
    if ownership.enrolled_root_identity != enrolled_root_identity
        || ownership.staging_identity != staging_identity
        || ownership.request_identity != request_identity
        || ownership.parent_identity != gate_parent.identity
    {
        return Err(std::io::Error::other(
            "launcher gate ownership record identity mismatch",
        ));
    }
    Ok(ownership)
}

#[cfg(target_os = "macos")]
fn validate_short_gate_root(
    parent: &ShortGateParent,
    ownership: &GateRootOwnership,
) -> std::io::Result<Option<ValidatedShortGateRoot>> {
    parent.verify()?;
    let directory = match rustix::fs::openat(
        &parent.directory,
        Path::new(&ownership.name),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::DIRECTORY
            | rustix::fs::OFlags::NOFOLLOW
            | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    ) {
        Ok(directory) => File::from(directory),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(std::io::Error::from(error)),
    };
    let metadata = directory.metadata()?;
    if !metadata.is_dir()
        || FileIdentity::of(&metadata) != ownership.directory_identity
        || metadata.uid() != rustix::process::geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(std::io::Error::other(
            "launcher gate root identity changed during recovery",
        ));
    }
    let entries = owned_directory_entry_names(&directory)?;
    if entries.len() > 1 || entries.first().is_some_and(|entry| entry != b"gate") {
        return Err(std::io::Error::other(
            "launcher gate root contains an unknown entry",
        ));
    }
    let socket_identity = if entries.is_empty() {
        None
    } else {
        let stat = rustix::fs::statat(
            &directory,
            Path::new("gate"),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(std::io::Error::from)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Socket
            || stat.st_mode & 0o777 != 0o600
            || stat.st_uid != rustix::process::geteuid().as_raw()
            || FileIdentity::of_stat(&stat)? != ownership.socket_identity
        {
            return Err(std::io::Error::other(
                "launcher gate socket identity changed during recovery",
            ));
        }
        Some(ownership.socket_identity)
    };
    let remapped = open_owned_directory(&parent.directory, &ownership.name)?;
    if FileIdentity::of(&remapped.metadata()?) != ownership.directory_identity {
        return Err(std::io::Error::other(
            "launcher gate root identity changed during recovery",
        ));
    }
    Ok(Some(ValidatedShortGateRoot {
        directory,
        name: ownership.name.clone(),
        socket_identity,
    }))
}

#[cfg(target_os = "macos")]
fn remove_validated_owned_directory(
    root: &File,
    validated: ValidatedOwnedDirectory,
    gate_parent: &ShortGateParent,
) -> std::io::Result<()> {
    let remapped = File::from(
        rustix::fs::openat(
            root,
            Path::new(&validated.name),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if FileIdentity::of(&remapped.metadata()?) != FileIdentity::of(&validated.directory.metadata()?)
    {
        return Err(std::io::Error::other(
            "process-owned directory identity changed before removal",
        ));
    }
    for broker_request in validated.broker_requests {
        remove_validated_broker_request(&validated.directory, broker_request, gate_parent)?;
    }
    rustix::fs::fchmod(&validated.directory, rustix::fs::Mode::from_raw_mode(0o700))
        .map_err(std::io::Error::from)?;
    for entry_name in validated.entries {
        rustix::fs::unlinkat(
            &validated.directory,
            Path::new(OsStr::from_bytes(&entry_name)),
            rustix::fs::AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
    }
    rustix::fs::unlinkat(
        root,
        Path::new(&validated.name),
        rustix::fs::AtFlags::REMOVEDIR,
    )
    .map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_validated_broker_request(
    parent: &File,
    validated: ValidatedBrokerRequest,
    gate_parent: &ShortGateParent,
) -> std::io::Result<()> {
    let remapped = File::from(
        rustix::fs::openat(
            parent,
            Path::new(OsStr::from_bytes(&validated.name)),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    if FileIdentity::of(&remapped.metadata()?) != FileIdentity::of(&validated.directory.metadata()?)
    {
        return Err(std::io::Error::other(
            "broker request identity changed before recovery removal",
        ));
    }
    if let Some(gate_root) = validated.gate_root {
        remove_validated_short_gate_root(gate_parent, gate_root)?;
    }
    rustix::fs::fchmod(&validated.directory, rustix::fs::Mode::from_raw_mode(0o700))
        .map_err(std::io::Error::from)?;
    for entry in validated.entries {
        rustix::fs::unlinkat(
            &validated.directory,
            Path::new(OsStr::from_bytes(&entry)),
            rustix::fs::AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
    }
    rustix::fs::unlinkat(
        parent,
        Path::new(OsStr::from_bytes(&validated.name)),
        rustix::fs::AtFlags::REMOVEDIR,
    )
    .map_err(std::io::Error::from)?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_validated_short_gate_root(
    parent: &ShortGateParent,
    validated: ValidatedShortGateRoot,
) -> std::io::Result<()> {
    parent.verify()?;
    let remapped = open_owned_directory(&parent.directory, &validated.name)?;
    let retained_metadata = validated.directory.metadata()?;
    if FileIdentity::of(&remapped.metadata()?) != FileIdentity::of(&retained_metadata)
        || retained_metadata.uid() != rustix::process::geteuid().as_raw()
        || retained_metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(std::io::Error::other(
            "launcher gate root identity changed before recovery removal",
        ));
    }
    if let Some(socket_identity) = validated.socket_identity {
        let stat = rustix::fs::statat(
            &validated.directory,
            Path::new("gate"),
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(std::io::Error::from)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Socket
            || stat.st_mode & 0o777 != 0o600
            || stat.st_uid != rustix::process::geteuid().as_raw()
            || FileIdentity::of_stat(&stat)? != socket_identity
        {
            return Err(std::io::Error::other(
                "launcher gate socket identity changed before recovery removal",
            ));
        }
        rustix::fs::unlinkat(
            &validated.directory,
            Path::new("gate"),
            rustix::fs::AtFlags::empty(),
        )
        .map_err(std::io::Error::from)?;
        validated.directory.sync_all()?;
    }
    remove_owned_directory_mapping(&parent.directory, &validated.directory, &validated.name)
}

#[cfg(target_os = "macos")]
fn owned_directory_entry_names(directory: &File) -> std::io::Result<Vec<Vec<u8>>> {
    let mut entries = rustix::fs::Dir::read_from(directory)
        .map_err(std::io::Error::from)?
        .filter_map(|entry| match entry {
            Ok(entry) => {
                let name = entry.file_name().to_bytes();
                if name == b"." || name == b".." {
                    None
                } else {
                    Some(Ok(name.to_vec()))
                }
            }
            Err(error) => Some(Err(std::io::Error::from(error))),
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_unstable();
    Ok(entries)
}

#[cfg(target_os = "macos")]
fn discover_process_broker() -> std::io::Result<(File, PathBuf, FileIdentity, Vec<u8>)> {
    let current = std::env::current_exe()?;
    let parent = current
        .parent()
        .ok_or_else(|| std::io::Error::other("current executable has no parent"))?;
    let mut candidates = vec![parent.join("orchestrator-process-broker")];
    if parent.file_name() == Some(OsStr::new("deps")) {
        if let Some(target_directory) = parent.parent() {
            candidates.push(target_directory.join("orchestrator-process-broker"));
        }
    }

    for candidate in candidates {
        let canonical = match std::fs::canonicalize(&candidate) {
            Ok(path) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let file = File::from(
            rustix::fs::open(
                &canonical,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let metadata = file.metadata()?;
        let length = usize::try_from(metadata.len())
            .map_err(|_| std::io::Error::other("process broker image is too large"))?;
        if !metadata.is_file()
            || metadata.permissions().mode() & 0o111 == 0
            || length == 0
            || length > MAX_FIXTURE_EXECUTABLE_BYTES
        {
            return Err(std::io::Error::other(
                "process broker has invalid type, mode, or length",
            ));
        }
        let identity = FileIdentity::of(&metadata);
        let mut image = Vec::with_capacity(length);
        file.try_clone()?.read_to_end(&mut image)?;
        verify_executable_image(&file, identity, &image)?;
        return Ok((file, canonical, identity, image));
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "orchestrator process broker was not found next to the current executable",
    ))
}

/// A process supervisor backed by an ownership registry and blocking reaper.
///
/// [`Self::new`] creates an isolated registry and reaper for fixture-scoped
/// work. [`Self::process_wide`] shares the lazy production registry and its one
/// reaper across every handle, so unresolved ownership closes admission across
/// independently composed services. Each active run still owns its waiter,
/// two output-reader threads, and optional stdin-writer thread.
pub struct ProcessSupervisor {
    registry: Arc<Registry>,
}

impl ProcessSupervisor {
    /// Creates a supervisor that admits at most `maximum` concurrently owned
    /// process groups.
    pub fn new(maximum: usize) -> Result<Self, ProcessError> {
        if maximum == 0 {
            return Err(ProcessError::InvalidSpec);
        }
        Ok(Self {
            registry: Registry::new(maximum).map_err(ProcessError::Spawn)?,
        })
    }

    /// Uses the process-wide production ownership registry.
    ///
    /// Every handle shares one lazy reaper and one unresolved-ownership
    /// admission gate. Fixture-isolated callers should continue to use
    /// [`Self::new`].
    pub fn process_wide() -> Result<Self, ProcessError> {
        Ok(Self {
            registry: Arc::clone(global_registry()?),
        })
    }

    /// Runs a command under this supervisor's ownership registry.
    pub fn run(&self, spec: &ProcessSpec, cwd: &Path) -> Result<ProcessReport, ProcessError> {
        run_inner(spec, cwd, &self.registry)
    }

    /// Runs a hermetic fixture command using retained executable, CWD, and root identities.
    pub fn run_fixture_authorized(
        &self,
        spec: &ProcessSpec,
        launch: &FixtureProcessLaunchAuthority,
    ) -> Result<ProcessReport, ProcessError> {
        run_inner_fixture_authorized(spec, launch, &self.registry)
    }

    /// Runs an admitted command using retained executable, CWD, and root identities.
    ///
    /// On macOS this inherits the exact-image canary limitations documented on
    /// [`ProductionProcessLaunchAuthority`]; it is not broad command enrollment.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizedProcessError`] with a structural pre/post-spawn
    /// classification when no [`AuthorizedProcessOutcome`] can be produced.
    pub fn run_authorized(
        &self,
        spec: &ProcessSpec,
        launch: &ProductionProcessLaunchAuthority,
        start_gate: &mut ProcessStartGate,
    ) -> Result<AuthorizedProcessOutcome, AuthorizedProcessError> {
        run_inner_production_authorized(spec, launch, start_gate, &self.registry)
    }

    /// Returns whether active or unresolved ownership remains.
    #[must_use]
    pub fn has_owned_processes(&self) -> bool {
        let state = self.registry.state();
        state.active != 0 || state.unresolved != 0
    }

    /// Shuts down this isolated supervisor when it is idle and uniquely owned.
    ///
    /// This consumes the handle so admission cannot race with shutdown. It
    /// succeeds only when the registry has no active or unresolved ownership
    /// and this is its sole strong reference. Process-wide supervisors
    /// therefore fail closed because the global registry retains a reference.
    /// On refusal, the returned supervisor remains usable and admission stays
    /// open.
    ///
    /// On success, the unresolved-work channel is closed and the exact reaper
    /// thread retained by this registry is joined before returning. A panic in
    /// that already-terminated idle reaper still establishes thread
    /// quiescence; it does not constitute or claim process cleanup.
    ///
    /// # Errors
    ///
    /// Returns the unchanged supervisor when the registry is shared or owns
    /// active or unresolved work.
    pub fn try_shutdown_idle(self) -> Result<(), Self> {
        let has_owned_processes = {
            let state = self.registry.state();
            state.active != 0 || state.unresolved != 0
        };
        if has_owned_processes {
            return Err(self);
        }

        let mut registry = match Arc::try_unwrap(self.registry) {
            Ok(registry) => registry,
            Err(registry) => return Err(Self { registry }),
        };
        drop(registry.unresolved_sender.take());
        if let Some(reaper) = registry.reaper.take() {
            let _ = reaper.join();
        }
        Ok(())
    }
}

/// Returns process-wide ownership state without initializing the registry.
///
/// A status query before the first process-wide execution remains allocation-
/// and thread-free.
#[must_use]
pub fn process_wide_has_owned_processes() -> bool {
    GLOBAL_REGISTRY
        .get()
        .and_then(|result| result.as_ref().ok())
        .is_some_and(|registry| {
            let state = registry.state();
            state.active != 0 || state.unresolved != 0
        })
}

impl fmt::Debug for ProcessSupervisor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let state = self.registry.state();
        formatter
            .debug_struct("ProcessSupervisor")
            .field("active", &state.active)
            .field("unresolved", &state.unresolved)
            .finish()
    }
}

/// Runs `spec` in `cwd` under the process-wide supervisor.
///
/// The earliest terminal observation wins. Ties are ordered cancellation, hard
/// deadline, output stall, then direct-child exit. `waitid` supplies no kernel
/// exit timestamp, so cancellation or a deadline concurrent with exit is
/// ordered against the supervisor's observation time rather than the
/// unknowable exact exit instant. All observations remain recorded in the
/// receipt.
pub fn run(spec: &ProcessSpec, cwd: &Path) -> Result<ProcessReport, ProcessError> {
    let registry = global_registry()?;
    run_inner(spec, cwd, registry)
}

fn run_inner(
    spec: &ProcessSpec,
    cwd: &Path,
    registry: &Arc<Registry>,
) -> Result<ProcessReport, ProcessError> {
    run_inner_with_launch(spec, RequestedLaunch::Ambient(cwd), None, registry)
        .map(|completion| completion.report)
        .map_err(RunFailure::into_process_error)
}

fn run_inner_fixture_authorized(
    spec: &ProcessSpec,
    launch: &FixtureProcessLaunchAuthority,
    registry: &Arc<Registry>,
) -> Result<ProcessReport, ProcessError> {
    run_inner_with_launch(
        spec,
        RequestedLaunch::FixtureAuthorized(launch),
        None,
        registry,
    )
    .map(|completion| completion.report)
    .map_err(RunFailure::into_process_error)
}

fn run_inner_production_authorized(
    spec: &ProcessSpec,
    launch: &ProductionProcessLaunchAuthority,
    start_gate: &mut ProcessStartGate,
    registry: &Arc<Registry>,
) -> Result<AuthorizedProcessOutcome, AuthorizedProcessError> {
    let mut completion = run_inner_with_launch(
        spec,
        RequestedLaunch::ProductionAuthorized(launch),
        Some(start_gate),
        registry,
    )
    .map_err(RunFailure::into_authorized_error)?;
    match completion.gate_lifecycle {
        GateLifecycle::Released(identity) => {
            completion.report.kernel_identity = Some(identity);
            Ok(AuthorizedProcessOutcome::Started(completion.report))
        }
        GateLifecycle::NotReleased { reason, identity } => {
            if completion.report.cleanup_complete {
                Ok(AuthorizedProcessOutcome::NotStarted(
                    ProcessNotStartedReceipt {
                        reason,
                        launcher_spawned: completion.report.spawned,
                        launcher_identity: identity,
                        elapsed: completion.report.elapsed,
                        cancellation_observed: completion.report.cancellation_observed,
                        deadline_observed: completion.report.deadline_observed,
                    },
                ))
            } else {
                Ok(AuthorizedProcessOutcome::Uncertain(
                    ProcessUncertainReceipt {
                        reason: ProcessUncertainReason::CleanupIncomplete(reason),
                        launcher_identity: identity,
                        report: completion.report,
                    },
                ))
            }
        }
        GateLifecycle::ReleaseUncertain(identity) => Ok(AuthorizedProcessOutcome::Uncertain(
            ProcessUncertainReceipt {
                reason: ProcessUncertainReason::ReleaseDelivery,
                launcher_identity: Some(identity),
                report: completion.report,
            },
        )),
        GateLifecycle::Ungated => Ok(AuthorizedProcessOutcome::Uncertain(
            ProcessUncertainReceipt {
                reason: ProcessUncertainReason::UngatedExecution,
                launcher_identity: None,
                report: completion.report,
            },
        )),
    }
}

struct RunCompletion {
    report: ProcessReport,
    gate_lifecycle: GateLifecycle,
}

enum RunFailure {
    ProvenNotStarted(ProcessError),
    OutcomeLost(ProcessError),
}

impl RunFailure {
    fn proven_not_started(cause: ProcessError) -> Self {
        Self::ProvenNotStarted(cause)
    }

    fn outcome_lost(cause: ProcessError) -> Self {
        Self::OutcomeLost(cause)
    }

    fn into_process_error(self) -> ProcessError {
        match self {
            Self::ProvenNotStarted(cause) | Self::OutcomeLost(cause) => cause,
        }
    }

    fn into_authorized_error(self) -> AuthorizedProcessError {
        match self {
            Self::ProvenNotStarted(cause) => AuthorizedProcessError::proven_not_started(cause),
            Self::OutcomeLost(cause) => AuthorizedProcessError::outcome_lost(cause),
        }
    }
}

enum GateLifecycle {
    Ungated,
    NotReleased {
        reason: ProcessNotStartedReason,
        identity: Option<KernelProcessIdentity>,
    },
    Released(KernelProcessIdentity),
    ReleaseUncertain(KernelProcessIdentity),
}

fn run_inner_with_launch(
    spec: &ProcessSpec,
    requested_launch: RequestedLaunch<'_>,
    mut start_gate: Option<&mut ProcessStartGate>,
    registry: &Arc<Registry>,
) -> Result<RunCompletion, RunFailure> {
    validate_spec(spec).map_err(RunFailure::proven_not_started)?;
    let started = Instant::now();
    let relative_hard_deadline = started
        .checked_add(spec.hard_deadline)
        .ok_or_else(|| RunFailure::proven_not_started(ProcessError::InvalidSpec))?;
    let hard_deadline = spec
        .hard_deadline_at
        .map_or(relative_hard_deadline, |deadline| {
            deadline.min(relative_hard_deadline)
        });

    if let Some(termination) = pre_spawn_termination(spec, hard_deadline) {
        let gate_lifecycle = if start_gate.is_some() {
            GateLifecycle::NotReleased {
                reason: not_started_reason(termination),
                identity: None,
            }
        } else {
            GateLifecycle::Ungated
        };
        return Ok(RunCompletion {
            report: not_spawned_report(started, termination),
            gate_lifecycle,
        });
    }

    let environment = child_environment(spec).map_err(RunFailure::proven_not_started)?;
    let launch = LaunchBinding::prepare(requested_launch, spec, &environment)
        .map_err(|cause| RunFailure::proven_not_started(ProcessError::Spawn(cause)))?;
    if launch.requires_start_gate() != start_gate.is_some() {
        return Err(RunFailure::proven_not_started(ProcessError::Spawn(
            std::io::Error::other("process launch durable-gate composition mismatch"),
        )));
    }
    let staged_stdin = launch.stages_stdin();
    let lease = RegistryLease::reserve(registry).map_err(RunFailure::proven_not_started)?;
    let queue = Arc::new(EventQueue::default());
    let cancellation_registration = spec
        .cancellation
        .as_ref()
        .map(|token| token.register(&queue));

    let (adopt_tx, adopt_rx) = channel::<WaitTarget>();
    let (reap_tx, reap_rx) = channel::<()>();
    let waiter_queue = Arc::clone(&queue);
    let waiter_lease = lease.clone();
    let waiter = thread::Builder::new()
        .name("orchestrator-process-wait".to_owned())
        .spawn(move || {
            if let Ok(mut target) = adopt_rx.recv() {
                match observe_leader_exit(target.pid) {
                    Ok(()) => {
                        waiter_queue.push(Event::LeaderObserved(Instant::now()));
                        if reap_rx.recv().is_ok() {
                            match reap_shared_child(&target.child, &mut target.fail_reap_once) {
                                Ok(status) => waiter_queue.push(Event::LeaderReaped(status)),
                                Err(()) => waiter_queue.push(Event::LeaderReapFailed),
                            }
                        }
                    }
                    Err(_) => waiter_queue.push(Event::LeaderObservationFailed),
                }
            }
            drop(waiter_lease);
        })
        .map_err(|cause| RunFailure::proven_not_started(ProcessError::Spawn(cause)))?;

    let mut command = match launch.command(spec, &environment) {
        Ok(command) => command,
        Err(cause) => {
            return Err(pre_spawn_command_failure(cause, adopt_tx, reap_tx, waiter));
        }
    };
    #[cfg(test)]
    if let Some((ready, resume)) = &spec.pre_spawn_barriers {
        ready.wait();
        resume.wait();
    }

    if let Some(termination) = pre_spawn_termination(spec, hard_deadline) {
        close_unadopted_waiter(adopt_tx, reap_tx, waiter);
        let gate_lifecycle = if start_gate.is_some() {
            GateLifecycle::NotReleased {
                reason: not_started_reason(termination),
                identity: None,
            }
        } else {
            GateLifecycle::Ungated
        };
        return Ok(RunCompletion {
            report: not_spawned_report(started, termination),
            gate_lifecycle,
        });
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(source) => {
            close_unadopted_waiter(adopt_tx, reap_tx, waiter);
            if start_gate.is_some() {
                return Ok(RunCompletion {
                    report: not_spawned_report(started, ProcessTermination::InfrastructureError),
                    gate_lifecycle: GateLifecycle::NotReleased {
                        reason: ProcessNotStartedReason::SpawnFailed,
                        identity: None,
                    },
                });
            }
            return Err(RunFailure::proven_not_started(ProcessError::Spawn(source)));
        }
    };
    let raw_child_pid = child.id();
    let Some(pid) = pid_from_raw(raw_child_pid) else {
        let _ = child.kill();
        let direct_child_reaped = child.wait().is_ok();
        close_unadopted_waiter(adopt_tx, reap_tx, waiter);
        if start_gate.is_some() {
            if !direct_child_reaped {
                std::mem::forget(child);
            }
            drop(lease);
            drop(cancellation_registration);
            // No supported process identifier exists with which either this
            // thread or the reaper could prove group absence. Keep admission
            // permanently fail-closed and return explicit uncertainty.
            std::mem::forget(registry.begin_unresolved());
            return Ok(RunCompletion {
                report: ProcessReport {
                    termination: ProcessTermination::UnresolvedOwnership,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    truncated: false,
                    stdout_discarded_bytes: 0,
                    stderr_discarded_bytes: 0,
                    elapsed: started.elapsed(),
                    spawned: true,
                    pid: Some(raw_child_pid),
                    pgid: None,
                    kernel_identity: None,
                    cancellation_observed: false,
                    deadline_observed: false,
                    stall_observed: false,
                    term_sent: false,
                    kill_sent: true,
                    escalated_to_kill: true,
                    direct_child_reaped,
                    group_absent: false,
                    cleanup_complete: false,
                    infrastructure_failures: vec![
                        ProcessInfrastructureFailure::LaunchSetup,
                        ProcessInfrastructureFailure::CleanupIncomplete,
                    ],
                },
                gate_lifecycle: GateLifecycle::NotReleased {
                    reason: ProcessNotStartedReason::GateProtocol,
                    identity: None,
                },
            });
        }
        return Err(RunFailure::outcome_lost(ProcessError::Spawn(
            std::io::Error::other("child PID exceeded the supported range"),
        )));
    };
    let waiter_control = WaiterControl {
        waiter,
        reap_sender: reap_tx,
        #[cfg(test)]
        reaper_reap_gate: spec.reaper_reap_gate.clone(),
    };
    let mut spawn_guard = SpawnGuard::new(
        child,
        pid,
        Arc::clone(registry),
        lease,
        waiter_control,
        cancellation_registration,
    );
    let stdout_capture = Arc::new(Mutex::new(CaptureState::new(spec.max_output_bytes)));
    let stderr_capture = Arc::new(Mutex::new(CaptureState::new(spec.max_output_bytes)));
    let production_gated = start_gate.is_some();
    macro_rules! setup_try {
        ($operation:expr) => {
            match $operation {
                Ok(value) => value,
                Err(source) => {
                    return post_spawn_setup_failure(
                        spawn_guard,
                        adopt_tx,
                        started,
                        &stdout_capture,
                        &stderr_capture,
                        production_gated,
                        source,
                    );
                }
            }
        };
    }

    setup_try!(spawn_guard.verify_process_group());
    setup_try!(launch.verify());
    let gate_waiter = setup_try!(launch.start_gate_waiter(pid, Arc::clone(&queue)));

    let stdout = setup_try!(spawn_guard.take_stdout());
    let stdout_queue = Arc::clone(&queue);
    let stdout_state = Arc::clone(&stdout_capture);
    let stdout_output = spec.output_sender.clone();
    let stdout_reader = setup_try!(
        thread::Builder::new()
            .name("orchestrator-process-stdout".to_owned())
            .spawn(move || {
                read_stream(
                    stdout,
                    stdout_state,
                    OutputStream::Stdout,
                    stdout_queue,
                    stdout_output,
                )
            })
    );
    spawn_guard.stdout_reader = Some(stdout_reader);

    let stderr = setup_try!(spawn_guard.take_stderr());
    let stderr_queue = Arc::clone(&queue);
    let stderr_state = Arc::clone(&stderr_capture);
    let stderr_output = spec.output_sender.clone();
    let stderr_reader = setup_try!(
        thread::Builder::new()
            .name("orchestrator-process-stderr".to_owned())
            .spawn(move || {
                read_stream(
                    stderr,
                    stderr_state,
                    OutputStream::Stderr,
                    stderr_queue,
                    stderr_output,
                )
            })
    );
    spawn_guard.stderr_reader = Some(stderr_reader);

    if !staged_stdin {
        if let Some(input) = &spec.stdin {
            let stdin = setup_try!(spawn_guard.take_stdin());
            let writer_queue = Arc::clone(&queue);
            let input = Arc::clone(input);
            let writer = setup_try!(
                thread::Builder::new()
                    .name("orchestrator-process-stdin".to_owned())
                    .spawn(move || {
                        let mut stdin = stdin;
                        let result = stdin.write_all(&input);
                        drop(stdin);
                        writer_queue.push(Event::WriterDone(result));
                    })
            );
            spawn_guard.stdin_writer = Some(writer);
        }
    }

    let child = Arc::new(Mutex::new(Some(setup_try!(spawn_guard.take_child()))));
    spawn_guard.adopted_child = Some(Arc::clone(&child));
    if adopt_tx
        .send(WaitTarget {
            pid,
            child,
            fail_reap_once: force_reap_failure(spec),
        })
        .is_err()
    {
        return post_spawn_setup_failure(
            spawn_guard,
            adopt_tx,
            started,
            &stdout_capture,
            &stderr_capture,
            production_gated,
            std::io::Error::other("process waiter stopped before adopting child"),
        );
    }
    let mut guard = spawn_guard.into_run_guard();
    let gate_lifecycle = if let Some(gate_waiter) = gate_waiter {
        match start_gate.as_deref_mut() {
            Some(start_gate) => match await_and_grant_start_gate(
                &queue,
                hard_deadline,
                pid,
                start_gate,
                spec.cancellation.as_ref(),
                gate_waiter,
            ) {
                GateAwaitOutcome::Released(identity) => GateLifecycle::Released(identity),
                GateAwaitOutcome::NotReleased { reason, identity } => {
                    GateLifecycle::NotReleased { reason, identity }
                }
                GateAwaitOutcome::ReleaseUncertain(identity) => {
                    GateLifecycle::ReleaseUncertain(identity)
                }
            },
            None => {
                let _ = gate_waiter.abort();
                GateLifecycle::NotReleased {
                    reason: ProcessNotStartedReason::GateProtocol,
                    identity: None,
                }
            }
        }
    } else if start_gate.is_some() {
        GateLifecycle::NotReleased {
            reason: ProcessNotStartedReason::GateProtocol,
            identity: None,
        }
    } else {
        GateLifecycle::Ungated
    };
    if matches!(
        &gate_lifecycle,
        GateLifecycle::Released(_) | GateLifecycle::Ungated
    ) {
        queue.arm_stall(spec.stall_timeout, Instant::now());
    }
    #[cfg(test)]
    if let Some(barrier) = &spec.post_adoption_barrier {
        barrier.wait();
    }
    let mut state = RunState::new(spec.stdin.is_none() || staged_stdin);
    match &gate_lifecycle {
        GateLifecycle::NotReleased {
            reason: ProcessNotStartedReason::Cancelled | ProcessNotStartedReason::Deadline,
            ..
        } => {}
        GateLifecycle::NotReleased { .. } | GateLifecycle::ReleaseUncertain(_) => {
            state.record_failure(ProcessInfrastructureFailure::LaunchGate);
        }
        GateLifecycle::Released(_) | GateLifecycle::Ungated => {}
    }

    loop {
        #[cfg(test)]
        if let Some(iterations) = &spec.controller_iterations {
            let mut iterations = lock_unpoisoned(iterations);
            *iterations = iterations.saturating_add(1);
        }
        while let Some(event) = queue.try_pop() {
            state.apply(event);
        }
        state.observe_token(spec.cancellation.as_ref());
        let stall = queue.observe_stall(Instant::now());
        state.observe_stall(stall);
        state.refresh_group_absence(pid);
        state.maybe_start_shutdown(hard_deadline, pid, spec);

        if state.group_absent {
            guard.mark_group_absent();
        }
        if state.group_absent && state.leader_observed_at.is_some() && !state.reap_requested {
            state.reap_requested = true;
            if !guard.request_reap() {
                state.reap_failed = true;
                state.record_failure(ProcessInfrastructureFailure::DirectChildWait);
            }
        }

        if state.ready_to_finish() {
            break;
        }

        if state.group_absent && (state.leader_observation_failed || state.reap_failed) {
            state.record_failure(ProcessInfrastructureFailure::CleanupIncomplete);
            break;
        }

        if state.cleanup_expired() {
            state.record_failure(ProcessInfrastructureFailure::CleanupIncomplete);
            break;
        }

        if let Some(event) = queue.pop_until(state.next_wake(hard_deadline, stall.deadline)) {
            state.apply(event);
        }
    }

    let synchronous_cleanup = state.ready_to_finish();
    if synchronous_cleanup {
        guard.join_finished(&mut state);
    } else {
        guard.handoff(state.group_absent);
    }
    state.observe_token(spec.cancellation.as_ref());

    let stdout = snapshot_capture(&stdout_capture);
    let stderr = snapshot_capture(&stderr_capture);
    // Cleanup completeness is an ownership fact, not an alias for overall
    // success. Reader, protocol, or other typed infrastructure failures remain
    // in `infrastructure_failures` and make `is_success` false without erasing
    // proof that the child, group, pipes, and threads were synchronously closed.
    let cleanup_complete = synchronous_cleanup;
    let termination = launch.termination_after_cleanup(
        state.termination(hard_deadline, cleanup_complete),
        cleanup_complete,
    );
    let cancellation_observed = state.cancellation_at.is_some();
    let deadline_observed = state
        .leader_observed_at
        .is_none_or(|observed| observed >= hard_deadline)
        && Instant::now() >= hard_deadline;

    Ok(RunCompletion {
        report: ProcessReport {
            termination,
            stdout: stdout.retained,
            stderr: stderr.retained,
            truncated: stdout.discarded_bytes != 0 || stderr.discarded_bytes != 0,
            stdout_discarded_bytes: stdout.discarded_bytes,
            stderr_discarded_bytes: stderr.discarded_bytes,
            elapsed: started.elapsed(),
            spawned: true,
            pid: Some(pid.as_raw_pid() as u32),
            pgid: Some(pid.as_raw_pid() as u32),
            kernel_identity: None,
            cancellation_observed,
            deadline_observed,
            stall_observed: state.stall_observed_at.is_some(),
            term_sent: state.term_sent,
            kill_sent: state.kill_sent,
            escalated_to_kill: state.kill_sent,
            direct_child_reaped: state.leader_reaped,
            group_absent: state.group_absent,
            cleanup_complete,
            infrastructure_failures: state.failures,
        },
        gate_lifecycle,
    })
}

fn pre_spawn_command_failure(
    cause: std::io::Error,
    adopt_sender: Sender<WaitTarget>,
    reap_sender: Sender<()>,
    waiter: JoinHandle<()>,
) -> RunFailure {
    close_unadopted_waiter(adopt_sender, reap_sender, waiter);
    RunFailure::proven_not_started(ProcessError::Spawn(cause))
}

fn close_unadopted_waiter(
    adopt_sender: Sender<WaitTarget>,
    reap_sender: Sender<()>,
    waiter: JoinHandle<()>,
) {
    drop(adopt_sender);
    drop(reap_sender);
    let _ = waiter.join();
}

fn not_started_reason(termination: ProcessTermination) -> ProcessNotStartedReason {
    match termination {
        ProcessTermination::Cancelled => ProcessNotStartedReason::Cancelled,
        ProcessTermination::Timeout => ProcessNotStartedReason::Deadline,
        ProcessTermination::Exited(_)
        | ProcessTermination::Signaled(_)
        | ProcessTermination::Stalled
        | ProcessTermination::OutputLimit
        | ProcessTermination::InfrastructureError
        | ProcessTermination::UnresolvedOwnership => ProcessNotStartedReason::GateProtocol,
    }
}

fn post_spawn_setup_failure(
    spawn_guard: SpawnGuard,
    adopt_sender: Sender<WaitTarget>,
    started: Instant,
    stdout_capture: &Arc<Mutex<CaptureState>>,
    stderr_capture: &Arc<Mutex<CaptureState>>,
    production_gated: bool,
    source: std::io::Error,
) -> Result<RunCompletion, RunFailure> {
    drop(adopt_sender);
    if !production_gated {
        return Err(RunFailure::outcome_lost(ProcessError::Spawn(source)));
    }
    let report =
        spawn_guard.finish_pre_release_setup_failure(started, stdout_capture, stderr_capture);
    Ok(RunCompletion {
        report,
        gate_lifecycle: GateLifecycle::NotReleased {
            reason: ProcessNotStartedReason::GateProtocol,
            identity: None,
        },
    })
}

fn pre_spawn_termination(spec: &ProcessSpec, hard_deadline: Instant) -> Option<ProcessTermination> {
    let cancellation_at = spec
        .cancellation
        .as_ref()
        .and_then(CancellationToken::cancelled_at);
    if cancellation_at.is_some_and(|cancelled| cancelled <= hard_deadline) {
        return Some(ProcessTermination::Cancelled);
    }
    if Instant::now() >= hard_deadline {
        return Some(ProcessTermination::Timeout);
    }
    cancellation_at.map(|_| ProcessTermination::Cancelled)
}

fn not_spawned_report(started: Instant, termination: ProcessTermination) -> ProcessReport {
    let cancellation_observed = termination == ProcessTermination::Cancelled;
    let deadline_observed = termination == ProcessTermination::Timeout;
    ProcessReport {
        termination,
        stdout: Vec::new(),
        stderr: Vec::new(),
        truncated: false,
        stdout_discarded_bytes: 0,
        stderr_discarded_bytes: 0,
        elapsed: started.elapsed(),
        spawned: false,
        pid: None,
        pgid: None,
        kernel_identity: None,
        cancellation_observed,
        deadline_observed,
        stall_observed: false,
        term_sent: false,
        kill_sent: false,
        escalated_to_kill: false,
        direct_child_reaped: false,
        group_absent: true,
        cleanup_complete: true,
        infrastructure_failures: Vec::new(),
    }
}

fn validate_argv(argv: &[OsString], hard_deadline: Duration) -> Result<(), ProcessError> {
    if argv.is_empty()
        || argv.len() > MAX_ARGUMENTS
        || hard_deadline.is_zero()
        || hard_deadline > MAX_RUN_TIMEOUT
    {
        return Err(ProcessError::InvalidSpec);
    }
    let mut total = 0usize;
    for argument in argv {
        let bytes = argument.as_bytes();
        if bytes.contains(&0) || bytes.len() > MAX_ARGUMENT_BYTES {
            return Err(ProcessError::InvalidSpec);
        }
        total = total.saturating_add(bytes.len());
        if total > MAX_ARGUMENT_TOTAL {
            return Err(ProcessError::InvalidSpec);
        }
    }
    Ok(())
}

fn validate_spec(spec: &ProcessSpec) -> Result<(), ProcessError> {
    validate_argv(&spec.argv, spec.hard_deadline)?;
    if spec.cleanup_grace.is_zero()
        || spec.term_grace > MAX_RUN_TIMEOUT
        || spec.cleanup_grace > MAX_RUN_TIMEOUT
        || spec
            .stall_timeout
            .is_some_and(|timeout| timeout.is_zero() || timeout > MAX_RUN_TIMEOUT)
        || spec.max_output_bytes > MAX_OUTPUT_BYTES
        || spec
            .stdin
            .as_ref()
            .is_some_and(|stdin| stdin.len() > MAX_STDIN_BYTES)
    {
        return Err(ProcessError::InvalidSpec);
    }
    validate_environment(spec)
}

fn validate_environment(spec: &ProcessSpec) -> Result<(), ProcessError> {
    let count = spec
        .environment
        .len()
        .saturating_add(spec.inherit_environment.len())
        .saturating_add(spec.env_remove.len());
    if count > MAX_ENVIRONMENT_ENTRIES {
        return Err(ProcessError::InvalidSpec);
    }
    let mut total = 0usize;
    for (key, value) in &spec.environment {
        validate_environment_entry(key, value)?;
        total = total
            .saturating_add(key.as_bytes().len())
            .saturating_add(value.as_bytes().len());
    }
    for key in &spec.inherit_environment {
        if !valid_environment_key(key) {
            return Err(ProcessError::InvalidSpec);
        }
        total = total.saturating_add(key.as_bytes().len());
    }
    for key in &spec.env_remove {
        if !valid_environment_key(key) {
            return Err(ProcessError::InvalidSpec);
        }
        total = total.saturating_add(key.as_bytes().len());
    }
    if total > MAX_ENVIRONMENT_TOTAL {
        return Err(ProcessError::InvalidSpec);
    }
    Ok(())
}

fn validate_environment_entry(key: &OsStr, value: &OsStr) -> Result<(), ProcessError> {
    if !valid_environment_key(key) || value.as_bytes().contains(&0) {
        return Err(ProcessError::InvalidSpec);
    }
    Ok(())
}

fn valid_environment_key(key: &OsStr) -> bool {
    let bytes = key.as_bytes();
    !bytes.is_empty() && !bytes.contains(&0) && !bytes.contains(&b'=')
}

fn child_environment(spec: &ProcessSpec) -> Result<Vec<(OsString, OsString)>, ProcessError> {
    let mut environment = Vec::with_capacity(spec.environment.len());
    for (key, value) in &spec.environment {
        if spec.env_remove.iter().any(|removed| removed == key) {
            continue;
        }
        if let Some((_, captured)) = environment.iter_mut().find(|(captured, _)| captured == key) {
            *captured = value.clone();
        } else {
            environment.push((key.clone(), value.clone()));
        }
    }
    let mut inherited_seen = Vec::with_capacity(spec.inherit_environment.len());
    for key in &spec.inherit_environment {
        if spec.env_remove.iter().any(|removed| removed == key)
            || environment.iter().any(|(captured, _)| captured == key)
            || inherited_seen.iter().any(|captured| captured == key)
        {
            continue;
        }
        inherited_seen.push(key.clone());
        if let Some(value) = std::env::var_os(key) {
            environment.push((key.clone(), value));
        }
    }
    validate_captured_environment(&environment)?;
    Ok(environment)
}

fn validate_captured_environment(environment: &[(OsString, OsString)]) -> Result<(), ProcessError> {
    if environment.len() > MAX_ENVIRONMENT_ENTRIES {
        return Err(ProcessError::InvalidSpec);
    }
    let mut total = 0usize;
    for (key, value) in environment {
        validate_environment_entry(key, value)?;
        total = total
            .saturating_add(key.as_bytes().len())
            .saturating_add(value.as_bytes().len());
        if total > MAX_ENVIRONMENT_TOTAL {
            return Err(ProcessError::InvalidSpec);
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl FileIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
        }
    }

    #[cfg(target_os = "macos")]
    fn of_stat(stat: &rustix::fs::Stat) -> std::io::Result<Self> {
        Ok(Self {
            device: u64::try_from(stat.st_dev)
                .map_err(|_| std::io::Error::other("negative file device identity"))?,
            inode: stat.st_ino,
        })
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
struct ExecutableMetadataSnapshot {
    identity: FileIdentity,
    length: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

impl ExecutableMetadataSnapshot {
    fn of(metadata: &Metadata) -> Self {
        Self {
            identity: FileIdentity::of(metadata),
            length: metadata.len(),
            mode: metadata.mode(),
            uid: metadata.uid(),
            gid: metadata.gid(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

#[derive(Clone, Copy)]
enum ExecutableFileMode {
    RetainedSource,
    SealedClone,
    PinnedExternal,
}

enum RequestedLaunch<'a> {
    Ambient(&'a Path),
    FixtureAuthorized(&'a FixtureProcessLaunchAuthority),
    ProductionAuthorized(&'a ProductionProcessLaunchAuthority),
}

enum LaunchBinding<'a> {
    Ambient {
        cwd: PathBinding,
        executable: ExecutableBinding,
    },
    FixtureAuthorized {
        authority: &'a FixtureProcessLaunchAuthority,
        executable_path: PathBuf,
        cwd_path: PathBuf,
    },
    #[cfg(target_os = "macos")]
    ProductionBroker {
        authority: &'a ProductionProcessLaunchAuthority,
        stage: Box<BrokerStage>,
    },
}

impl<'a> LaunchBinding<'a> {
    fn prepare(
        requested: RequestedLaunch<'a>,
        spec: &ProcessSpec,
        _environment: &[(OsString, OsString)],
    ) -> std::io::Result<Self> {
        match requested {
            RequestedLaunch::Ambient(cwd) => {
                let cwd = PathBinding::directory(cwd)?;
                let executable = ExecutableBinding::resolve(&spec.argv[0], &cwd.path)?;
                Ok(Self::Ambient { cwd, executable })
            }
            RequestedLaunch::FixtureAuthorized(authority) => {
                let (executable_path, cwd_path) = authority.paths()?;
                Ok(Self::FixtureAuthorized {
                    authority,
                    executable_path,
                    cwd_path,
                })
            }
            RequestedLaunch::ProductionAuthorized(authority) => {
                authority.verify()?;
                #[cfg(target_os = "macos")]
                {
                    let stage = Box::new(BrokerStage::create(authority, spec, _environment)?);
                    Ok(Self::ProductionBroker { authority, stage })
                }
                #[cfg(not(target_os = "macos"))]
                {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::Unsupported,
                        "production launch requires the cross-platform durable pre-exec gate",
                    ))
                }
            }
        }
    }

    fn command(
        &self,
        spec: &ProcessSpec,
        environment: &[(OsString, OsString)],
    ) -> std::io::Result<std::process::Command> {
        match self {
            Self::Ambient { cwd, executable } => Ok(direct_command(
                &executable.path,
                &cwd.path,
                spec,
                environment,
            )),
            Self::FixtureAuthorized {
                executable_path,
                cwd_path,
                ..
            } => Ok(direct_command(executable_path, cwd_path, spec, environment)),
            #[cfg(target_os = "macos")]
            Self::ProductionBroker { authority, stage } => stage.command(authority),
        }
    }

    fn stages_stdin(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            matches!(self, Self::ProductionBroker { .. })
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn requires_start_gate(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            matches!(self, Self::ProductionBroker { .. })
        }
        #[cfg(not(target_os = "macos"))]
        {
            false
        }
    }

    fn start_gate_waiter(
        &self,
        pid: Pid,
        queue: Arc<EventQueue>,
    ) -> std::io::Result<Option<gate::GateWaiter>> {
        #[cfg(target_os = "macos")]
        if let Self::ProductionBroker { stage, .. } = self {
            let expected_pid = u32::try_from(pid.as_raw_pid())
                .map_err(|_| std::io::Error::other("launcher PID exceeded its wire type"))?;
            return stage
                .gate
                .spawn_waiter(expected_pid, move |result| match result {
                    Ok(connection) => queue.push(Event::GateReady(connection)),
                    Err(_) => queue.push(Event::GateFailed),
                })
                .map(Some);
        }
        let _ = (pid, queue);
        Ok(None)
    }

    fn verify(&self) -> std::io::Result<()> {
        match self {
            Self::Ambient { cwd, executable } => cwd.verify().and_then(|()| executable.verify()),
            Self::FixtureAuthorized { authority, .. } => authority.verify(),
            #[cfg(target_os = "macos")]
            Self::ProductionBroker { authority, stage } => {
                authority.verify_after_spawn().and_then(|()| stage.verify())
            }
        }
    }

    fn termination_after_cleanup(
        &self,
        termination: ProcessTermination,
        cleanup_complete: bool,
    ) -> ProcessTermination {
        #[cfg(target_os = "macos")]
        if let Self::ProductionBroker { stage, .. } = self {
            if !cleanup_complete {
                return termination;
            }
            return match termination {
                ProcessTermination::Cancelled
                | ProcessTermination::Timeout
                | ProcessTermination::Stalled
                | ProcessTermination::UnresolvedOwnership => termination,
                ProcessTermination::Exited(_)
                | ProcessTermination::Signaled(_)
                | ProcessTermination::OutputLimit
                | ProcessTermination::InfrastructureError => match stage.status_after_cleanup() {
                    // The gated launcher execs in place, so Running means the
                    // direct wait status already belongs to the target.
                    Ok(broker::BrokerStatus::Running) => termination,
                    // Retain compatibility with request directories created by
                    // the pre-gate nested-target broker.
                    Ok(broker::BrokerStatus::TargetExited(code)) => {
                        ProcessTermination::Exited(code)
                    }
                    Ok(broker::BrokerStatus::TargetSignaled(signal)) => {
                        ProcessTermination::Signaled(signal)
                    }
                    Ok(broker::BrokerStatus::Pending | broker::BrokerStatus::Failed) | Err(_) => {
                        ProcessTermination::InfrastructureError
                    }
                },
            };
        }
        let _ = cleanup_complete;
        termination
    }
}

fn direct_command(
    executable_path: &Path,
    cwd_path: &Path,
    spec: &ProcessSpec,
    environment: &[(OsString, OsString)],
) -> std::process::Command {
    let mut command = std::process::Command::new(executable_path);
    command.args(&spec.argv[1..]);
    command.current_dir(cwd_path);
    command.env_clear();
    for (key, value) in environment {
        command.env(key, value);
    }
    command.process_group(0);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.stdin(if spec.stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    command
}

struct PathBinding {
    path: PathBuf,
    identity: FileIdentity,
}

impl PathBinding {
    fn directory(path: &Path) -> std::io::Result<Self> {
        let path = std::fs::canonicalize(path)?;
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_dir() {
            return Err(std::io::Error::other("process CWD is not a directory"));
        }
        Ok(Self {
            path,
            identity: FileIdentity::of(&metadata),
        })
    }

    fn verify(&self) -> std::io::Result<()> {
        let metadata = std::fs::metadata(&self.path)?;
        if metadata.is_dir() && FileIdentity::of(&metadata) == self.identity {
            Ok(())
        } else {
            Err(std::io::Error::other("process CWD identity changed"))
        }
    }
}

struct ExecutableBinding {
    path: PathBuf,
    identity: FileIdentity,
}

impl ExecutableBinding {
    fn resolve(program: &OsStr, cwd: &Path) -> std::io::Result<Self> {
        let path = resolve_executable(program, cwd)?;
        let metadata = std::fs::metadata(&path)?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "resolved process executable is not executable",
            ));
        }
        Ok(Self {
            path,
            identity: FileIdentity::of(&metadata),
        })
    }

    fn verify(&self) -> std::io::Result<()> {
        let metadata = std::fs::metadata(&self.path)?;
        if metadata.is_file()
            && metadata.permissions().mode() & 0o111 != 0
            && FileIdentity::of(&metadata) == self.identity
        {
            Ok(())
        } else {
            Err(std::io::Error::other("process executable identity changed"))
        }
    }
}

fn validate_authority_relative(path: &Path) -> std::io::Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "launch authority path is not an exact non-empty relative path",
        ));
    }
    Ok(())
}

fn open_authority_path(
    root: &File,
    path: &Path,
    final_is_directory: bool,
) -> std::io::Result<File> {
    validate_authority_relative(path)?;
    let mut current = root.try_clone()?;
    let count = path.components().count();
    for (index, component) in path.components().enumerate() {
        let std::path::Component::Normal(name) = component else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "launch authority path contains a non-normal component",
            ));
        };
        let is_directory = index + 1 < count || final_is_directory;
        let mut flags =
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NOFOLLOW;
        if is_directory {
            flags |= rustix::fs::OFlags::DIRECTORY;
        }
        current = File::from(
            rustix::fs::openat(&current, Path::new(name), flags, rustix::fs::Mode::empty())
                .map_err(std::io::Error::from)?,
        );
    }
    Ok(current)
}

fn verify_executable_image(
    executable: &File,
    admitted_identity: FileIdentity,
    admitted_image: &[u8],
) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;

    let admitted_len = u64::try_from(admitted_image.len())
        .map_err(|_| std::io::Error::other("admitted executable image is too large"))?;
    let before = executable.metadata()?;
    if !before.is_file()
        || FileIdentity::of(&before) != admitted_identity
        || before.permissions().mode() & 0o111 == 0
        || before.len() != admitted_len
    {
        return Err(std::io::Error::other(
            "admitted executable image does not match its retained file",
        ));
    }

    let mut offset = 0usize;
    let mut buffer = [0u8; READER_CHUNK];
    while offset < admitted_image.len() {
        let chunk_len = (admitted_image.len() - offset).min(buffer.len());
        let read = executable.read_at(&mut buffer[..chunk_len], offset as u64)?;
        if read == 0 || buffer[..read] != admitted_image[offset..offset.saturating_add(read)] {
            return Err(std::io::Error::other(
                "admitted executable image does not match its retained file",
            ));
        }
        offset = offset.saturating_add(read);
    }

    let after = executable.metadata()?;
    if !after.is_file()
        || FileIdentity::of(&after) != admitted_identity
        || after.permissions().mode() & 0o111 == 0
        || after.len() != admitted_len
    {
        return Err(std::io::Error::other(
            "admitted executable image changed during verification",
        ));
    }
    Ok(())
}

fn executable_metadata_snapshot(
    executable: &File,
    admitted_identity: FileIdentity,
    attestation: ExecutableFileAttestation,
    required_mode: ExecutableFileMode,
) -> std::io::Result<ExecutableMetadataSnapshot> {
    let metadata = executable.metadata()?;
    let mode = metadata.permissions().mode() & 0o777;
    let mode_is_valid = match required_mode {
        ExecutableFileMode::RetainedSource => mode & 0o111 != 0,
        ExecutableFileMode::SealedClone => mode == 0o500,
        ExecutableFileMode::PinnedExternal => {
            pinned_external_metadata_is_valid(&metadata, rustix::process::geteuid().as_raw())
        }
    };
    if !metadata.is_file()
        || FileIdentity::of(&metadata) != admitted_identity
        || metadata.len() != attestation.length
        || !mode_is_valid
    {
        return Err(std::io::Error::other(
            "attested executable identity, length, or mode does not match",
        ));
    }
    Ok(ExecutableMetadataSnapshot::of(&metadata))
}

fn verify_admitted_executable_metadata(
    executable: &File,
    admitted_identity: FileIdentity,
    attestation: ExecutableFileAttestation,
    admitted_metadata: ExecutableMetadataSnapshot,
    required_mode: ExecutableFileMode,
) -> std::io::Result<()> {
    if executable_metadata_snapshot(executable, admitted_identity, attestation, required_mode)?
        != admitted_metadata
    {
        return Err(std::io::Error::other(
            "attested executable metadata changed after admission",
        ));
    }
    Ok(())
}

fn verify_executable_file_attestation(
    executable: &File,
    admitted_identity: FileIdentity,
    attestation: ExecutableFileAttestation,
    required_mode: ExecutableFileMode,
) -> std::io::Result<ExecutableMetadataSnapshot> {
    use std::os::unix::fs::FileExt;

    let before =
        executable_metadata_snapshot(executable, admitted_identity, attestation, required_mode)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; EXECUTABLE_STREAM_CHUNK].into_boxed_slice();
    let buffer_length = u64::try_from(buffer.len())
        .map_err(|_| std::io::Error::other("attested executable buffer is too large"))?;
    let mut offset = 0_u64;
    while offset < attestation.length {
        let remaining = attestation.length.saturating_sub(offset);
        let chunk_length = usize::try_from(remaining.min(buffer_length))
            .map_err(|_| std::io::Error::other("attested executable chunk is too large"))?;
        let read = executable.read_at(&mut buffer[..chunk_length], offset)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "attested executable ended before its declared length",
            ));
        }
        hasher.update(&buffer[..read]);
        let read = u64::try_from(read)
            .map_err(|_| std::io::Error::other("attested executable read is too large"))?;
        offset = offset
            .checked_add(read)
            .ok_or_else(|| std::io::Error::other("attested executable offset overflow"))?;
    }
    let actual_digest: [u8; 32] = hasher.finalize().into();
    if actual_digest != attestation.sha256 {
        return Err(std::io::Error::other(
            "attested executable SHA-256 does not match",
        ));
    }
    let after =
        executable_metadata_snapshot(executable, admitted_identity, attestation, required_mode)?;
    if after != before {
        return Err(std::io::Error::other(
            "attested executable changed while its digest was verified",
        ));
    }
    Ok(after)
}

#[cfg(target_os = "macos")]
struct BrokerStagingRoot {
    root: File,
    root_identity: FileIdentity,
    directory: File,
    directory_name: String,
    directory_identity: FileIdentity,
    request_registry: Arc<BrokerRequestRegistry>,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Eq, PartialEq)]
struct OwnedBrokerRequest {
    directory_name: String,
    directory_identity: FileIdentity,
}

#[cfg(target_os = "macos")]
#[derive(Default)]
struct BrokerRequestRegistry {
    owned: Mutex<Vec<OwnedBrokerRequest>>,
}

#[cfg(target_os = "macos")]
impl BrokerRequestRegistry {
    fn register(&self, ownership: OwnedBrokerRequest) {
        lock_unpoisoned(&self.owned).push(ownership);
    }

    fn unregister(&self, directory_name: &str, directory_identity: FileIdentity) {
        lock_unpoisoned(&self.owned).retain(|registered| {
            registered.directory_name != directory_name
                || registered.directory_identity != directory_identity
        });
    }

    fn snapshot(&self) -> Vec<OwnedBrokerRequest> {
        lock_unpoisoned(&self.owned).clone()
    }
}

#[cfg(target_os = "macos")]
struct PendingBrokerStagingRoot<'a> {
    root: &'a File,
    directory: &'a File,
    directory_name: &'a str,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl Drop for PendingBrokerStagingRoot<'_> {
    fn drop(&mut self) {
        if self.armed {
            cleanup_empty_owned_directory(self.root, self.directory, self.directory_name);
        }
    }
}

#[cfg(target_os = "macos")]
impl BrokerStagingRoot {
    fn create(root: &File, root_identity: FileIdentity) -> std::io::Result<Self> {
        Self::create_with_observer(root, root_identity, || Ok(()))
    }

    fn create_with_observer<F>(
        root: &File,
        root_identity: FileIdentity,
        after_open: F,
    ) -> std::io::Result<Self>
    where
        F: FnOnce() -> std::io::Result<()>,
    {
        let directory_name = reserve_owned_directory(root, ".orchestrator-staging")?;
        let directory = match open_owned_directory(root, &directory_name) {
            Ok(directory) => directory,
            Err(error) => {
                let _ = rustix::fs::unlinkat(
                    root,
                    Path::new(&directory_name),
                    rustix::fs::AtFlags::REMOVEDIR,
                );
                return Err(error);
            }
        };
        let mut pending = PendingBrokerStagingRoot {
            root,
            directory: &directory,
            directory_name: &directory_name,
            armed: true,
        };
        after_open()?;
        root.sync_all()?;
        let retained_root = root.try_clone()?;
        let directory_identity = FileIdentity::of(&directory.metadata()?);
        pending.armed = false;
        drop(pending);
        let staging = Self {
            root: retained_root,
            root_identity,
            directory_identity,
            directory,
            directory_name,
            request_registry: Arc::new(BrokerRequestRegistry::default()),
        };
        staging.verify()?;
        Ok(staging)
    }

    fn verify(&self) -> std::io::Result<()> {
        if FileIdentity::of(&self.root.metadata()?) != self.root_identity {
            return Err(std::io::Error::other(
                "broker staging root identity changed",
            ));
        }
        let mapped = open_owned_directory(&self.root, &self.directory_name)?;
        let metadata = self.directory.metadata()?;
        if FileIdentity::of(&metadata) != self.directory_identity
            || FileIdentity::of(&mapped.metadata()?) != self.directory_identity
            || metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(std::io::Error::other(
                "broker staging parent identity changed",
            ));
        }
        for name in owned_directory_entry_names(&self.directory)? {
            if classify_owned_directory_name(&name)?
                != Some(ProcessOwnedDirectoryKind::BrokerRequest)
            {
                return Err(std::io::Error::other(
                    "broker staging parent contains an unknown entry",
                ));
            }
        }
        Ok(())
    }

    fn verify_for_launch(
        &self,
        enrolled_root_identity: FileIdentity,
        gate_parent: &ShortGateParent,
    ) -> std::io::Result<()> {
        self.verify()?;
        for name in owned_directory_entry_names(&self.directory)? {
            let _validated = validate_broker_request_directory(
                &self.directory,
                self.directory_identity,
                enrolled_root_identity,
                gate_parent,
                name,
            )?;
        }
        Ok(())
    }

    fn ownership(&self) -> ProcessOwnedDirectory {
        ProcessOwnedDirectory {
            kind: ProcessOwnedDirectoryKind::BrokerStaging,
            name: OsString::from(&self.directory_name),
            root_identity: self.root_identity,
            directory_identity: self.directory_identity,
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for BrokerStagingRoot {
    fn drop(&mut self) {
        if verify_owned_directory_mapping(&self.root, &self.directory, &self.directory_name)
            .is_err()
        {
            return;
        }
        for ownership in self.request_registry.snapshot() {
            let Ok(directory) =
                open_owned_directory(&self.directory, ownership.directory_name.as_str())
            else {
                continue;
            };
            let Ok(metadata) = directory.metadata() else {
                continue;
            };
            if FileIdentity::of(&metadata) != ownership.directory_identity {
                continue;
            }
            if cleanup_broker_stage(
                &self.directory,
                &directory,
                ownership.directory_name.as_str(),
            ) {
                self.request_registry.unregister(
                    ownership.directory_name.as_str(),
                    ownership.directory_identity,
                );
            }
        }
        let _ = remove_owned_directory_mapping(&self.root, &self.directory, &self.directory_name);
    }
}

#[cfg(target_os = "macos")]
struct BrokerStage {
    root: File,
    root_identity: FileIdentity,
    directory: File,
    directory_name: String,
    directory_identity: FileIdentity,
    request_registry: Arc<BrokerRequestRegistry>,
    request: StagedControlFile,
    status: StagedStatusFile,
    stdin: Option<StagedControlFile>,
    gate: gate::ParentGate,
    gate_root: ShortGateRoot,
    gate_ownership: StagedControlFile,
}

#[cfg(target_os = "macos")]
struct ShortGateParent {
    directory: File,
    identity: FileIdentity,
    path: PathBuf,
    allow_private_owner: bool,
}

#[cfg(target_os = "macos")]
impl ShortGateParent {
    fn open_system() -> std::io::Result<Self> {
        Self::open(Path::new("/private/tmp"), false)
    }

    #[cfg(test)]
    fn open_private(path: &Path) -> std::io::Result<Self> {
        let physical = std::fs::canonicalize(path)?;
        Self::open(&physical, true)
    }

    fn open(path: &Path, allow_private_owner: bool) -> std::io::Result<Self> {
        if !path.is_absolute() {
            return Err(std::io::Error::other(
                "launcher gate parent path is not absolute",
            ));
        }
        let directory = File::from(
            rustix::fs::open(
                path,
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let metadata = directory.metadata()?;
        let owner = rustix::process::geteuid().as_raw();
        let mode = metadata.permissions().mode() & 0o7777;
        let system_parent = metadata.uid() == 0 && mode & 0o1777 == 0o1777;
        let private_parent =
            allow_private_owner && metadata.uid() == owner && mode & 0o777 == 0o700;
        if !metadata.is_dir() || (!system_parent && !private_parent) {
            return Err(std::io::Error::other(
                "launcher gate parent has unsafe ownership or mode",
            ));
        }
        let retained_path = path_from_fd(&directory)?;
        if retained_path != path {
            return Err(std::io::Error::other(
                "launcher gate parent path is not canonical",
            ));
        }
        Ok(Self {
            identity: FileIdentity::of(&metadata),
            path: retained_path,
            directory,
            allow_private_owner,
        })
    }

    fn verify(&self) -> std::io::Result<()> {
        let metadata = self.directory.metadata()?;
        let owner = rustix::process::geteuid().as_raw();
        let mode = metadata.permissions().mode() & 0o7777;
        let system_parent = metadata.uid() == 0 && mode & 0o1777 == 0o1777;
        let private_parent =
            self.allow_private_owner && metadata.uid() == owner && mode & 0o777 == 0o700;
        if !metadata.is_dir()
            || FileIdentity::of(&metadata) != self.identity
            || (!system_parent && !private_parent)
        {
            return Err(std::io::Error::other(
                "launcher gate parent identity changed",
            ));
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
struct ShortGateRoot {
    parent: File,
    parent_identity: FileIdentity,
    directory: File,
    directory_identity: FileIdentity,
    name: String,
    path: PathBuf,
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy, Eq, PartialEq)]
enum ShortGateRootCreationPhase {
    Opened,
    Finalized,
}

#[cfg(target_os = "macos")]
struct PendingShortGateRoot<'a> {
    parent: &'a File,
    directory: &'a File,
    name: &'a str,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl Drop for PendingShortGateRoot<'_> {
    fn drop(&mut self) {
        if self.armed {
            cleanup_short_gate_root(self.parent, self.directory, self.name);
        }
    }
}

#[cfg(target_os = "macos")]
impl ShortGateRoot {
    fn create(parent: &ShortGateParent) -> std::io::Result<Self> {
        Self::create_with_parent_observer(parent, |_, _, _, _| Ok(()))
    }

    fn create_with_parent_observer<F>(
        parent: &ShortGateParent,
        mut observer: F,
    ) -> std::io::Result<Self>
    where
        F: FnMut(ShortGateRootCreationPhase, &File, &File, &str) -> std::io::Result<()>,
    {
        parent.verify()?;
        let parent_directory = parent.directory.try_clone()?;

        for _ in 0..32 {
            let mut nonce = [0_u8; 8];
            getrandom::fill(&mut nonce).map_err(|error| {
                std::io::Error::other(format!("launcher gate root randomness: {error}"))
            })?;
            let name = format!(
                ".nanika-gate-{}-{}",
                std::process::id(),
                u64::from_be_bytes(nonce)
            );
            match rustix::fs::mkdirat(
                &parent_directory,
                Path::new(&name),
                rustix::fs::Mode::from_raw_mode(0o700),
            ) {
                Ok(()) => {}
                Err(rustix::io::Errno::EXIST) => continue,
                Err(error) => return Err(std::io::Error::from(error)),
            }
            let directory = match open_owned_directory(&parent_directory, &name) {
                Ok(directory) => directory,
                Err(error) => {
                    let _ = rustix::fs::unlinkat(
                        &parent_directory,
                        Path::new(&name),
                        rustix::fs::AtFlags::REMOVEDIR,
                    );
                    return Err(error);
                }
            };
            let mut pending = PendingShortGateRoot {
                parent: &parent_directory,
                directory: &directory,
                name: &name,
                armed: true,
            };
            observer(
                ShortGateRootCreationPhase::Opened,
                &parent_directory,
                &directory,
                &name,
            )?;
            rustix::fs::fchmod(&directory, rustix::fs::Mode::from_raw_mode(0o700))
                .map_err(std::io::Error::from)?;
            let directory_identity = FileIdentity::of(&directory.metadata()?);
            observer(
                ShortGateRootCreationPhase::Finalized,
                &parent_directory,
                &directory,
                &name,
            )?;
            pending.armed = false;
            drop(pending);
            let root = Self {
                parent_identity: parent.identity,
                directory_identity,
                path: parent.path.join(&name),
                parent: parent_directory,
                directory,
                name,
            };
            root.verify()?;
            return Ok(root);
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "cannot reserve private launcher gate root",
        ))
    }

    fn gate_path(&self) -> PathBuf {
        self.path.join("gate")
    }

    fn verify(&self) -> std::io::Result<()> {
        let parent_metadata = self.parent.metadata()?;
        let directory_metadata = self.directory.metadata()?;
        let mapped = open_owned_directory(&self.parent, &self.name)?;
        let parent_mode = parent_metadata.permissions().mode() & 0o7777;
        let safe_parent = (parent_metadata.uid() == 0 && parent_mode & 0o1777 == 0o1777)
            || (parent_metadata.uid() == rustix::process::geteuid().as_raw()
                && parent_mode & 0o777 == 0o700);
        if !parent_metadata.is_dir()
            || FileIdentity::of(&parent_metadata) != self.parent_identity
            || !safe_parent
            || !directory_metadata.is_dir()
            || FileIdentity::of(&directory_metadata) != self.directory_identity
            || FileIdentity::of(&mapped.metadata()?) != self.directory_identity
            || directory_metadata.uid() != rustix::process::geteuid().as_raw()
            || directory_metadata.permissions().mode() & 0o777 != 0o700
        {
            return Err(std::io::Error::other("launcher gate root identity changed"));
        }
        Ok(())
    }
}

#[cfg(target_os = "macos")]
impl Drop for ShortGateRoot {
    fn drop(&mut self) {
        cleanup_short_gate_root(&self.parent, &self.directory, &self.name);
    }
}

#[cfg(target_os = "macos")]
impl fmt::Debug for ShortGateRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ShortGateRoot")
            .field("kind", &"private-short-launcher-gate-root")
            .finish_non_exhaustive()
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone, Eq, PartialEq)]
struct GateRootOwnership {
    enrolled_root_identity: FileIdentity,
    staging_identity: FileIdentity,
    request_identity: FileIdentity,
    parent_identity: FileIdentity,
    directory_identity: FileIdentity,
    socket_identity: FileIdentity,
    name: String,
}

#[cfg(target_os = "macos")]
impl GateRootOwnership {
    fn new(
        enrolled_root_identity: FileIdentity,
        staging_identity: FileIdentity,
        request_identity: FileIdentity,
        gate_root: &ShortGateRoot,
        socket_identity: FileIdentity,
    ) -> Self {
        Self {
            enrolled_root_identity,
            staging_identity,
            request_identity,
            parent_identity: gate_root.parent_identity,
            directory_identity: gate_root.directory_identity,
            socket_identity,
            name: gate_root.name.clone(),
        }
    }

    fn encode(&self) -> std::io::Result<Vec<u8>> {
        validate_gate_root_name(self.name.as_bytes())?;
        let name_length = u16::try_from(self.name.len())
            .map_err(|_| std::io::Error::other("launcher gate ownership record is too large"))?;
        let mut encoded = Vec::with_capacity(GATE_ROOT_RECORD_FIXED_BYTES + self.name.len());
        encoded.extend_from_slice(GATE_ROOT_RECORD_MAGIC);
        for identity in [
            self.enrolled_root_identity,
            self.staging_identity,
            self.request_identity,
            self.parent_identity,
            self.directory_identity,
            self.socket_identity,
        ] {
            encoded.extend_from_slice(&identity.device.to_be_bytes());
            encoded.extend_from_slice(&identity.inode.to_be_bytes());
        }
        encoded.extend_from_slice(&name_length.to_be_bytes());
        encoded.extend_from_slice(self.name.as_bytes());
        Ok(encoded)
    }

    fn decode(encoded: &[u8]) -> std::io::Result<Self> {
        if encoded.len() < GATE_ROOT_RECORD_FIXED_BYTES
            || encoded.len() > MAX_GATE_ROOT_RECORD_BYTES
            || encoded.get(..GATE_ROOT_RECORD_MAGIC.len()) != Some(GATE_ROOT_RECORD_MAGIC)
        {
            return Err(std::io::Error::other(
                "invalid launcher gate ownership record",
            ));
        }
        let mut offset = GATE_ROOT_RECORD_MAGIC.len();
        let mut next_identity = || -> std::io::Result<FileIdentity> {
            let device_end = offset.saturating_add(8);
            let device = encoded
                .get(offset..device_end)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u64::from_be_bytes)
                .ok_or_else(|| std::io::Error::other("invalid launcher gate ownership record"))?;
            offset = device_end;
            let inode_end = offset.saturating_add(8);
            let inode = encoded
                .get(offset..inode_end)
                .and_then(|bytes| bytes.try_into().ok())
                .map(u64::from_be_bytes)
                .ok_or_else(|| std::io::Error::other("invalid launcher gate ownership record"))?;
            offset = inode_end;
            Ok(FileIdentity { device, inode })
        };
        let enrolled_root_identity = next_identity()?;
        let staging_identity = next_identity()?;
        let request_identity = next_identity()?;
        let parent_identity = next_identity()?;
        let directory_identity = next_identity()?;
        let socket_identity = next_identity()?;
        let length_end = offset.saturating_add(2);
        let name_length = encoded
            .get(offset..length_end)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u16::from_be_bytes)
            .map(usize::from)
            .ok_or_else(|| std::io::Error::other("invalid launcher gate ownership record"))?;
        let name = encoded
            .get(length_end..)
            .filter(|name| name.len() == name_length && name_length <= 255)
            .ok_or_else(|| std::io::Error::other("invalid launcher gate ownership record"))?;
        validate_gate_root_name(name)?;
        let name = std::str::from_utf8(name)
            .map_err(|_| std::io::Error::other("invalid launcher gate ownership record"))?
            .to_owned();
        Ok(Self {
            enrolled_root_identity,
            staging_identity,
            request_identity,
            parent_identity,
            directory_identity,
            socket_identity,
            name,
        })
    }
}

#[cfg(target_os = "macos")]
fn validate_gate_root_name(name: &[u8]) -> std::io::Result<()> {
    let Some(suffix) = name.strip_prefix(b".nanika-gate-") else {
        return Err(std::io::Error::other(
            "invalid launcher gate ownership record",
        ));
    };
    let mut fields = suffix.split(|byte| *byte == b'-');
    let pid = fields.next().unwrap_or_default();
    let nonce = fields.next().unwrap_or_default();
    if pid.is_empty()
        || nonce.is_empty()
        || fields.next().is_some()
        || !pid.iter().all(u8::is_ascii_digit)
        || !nonce.iter().all(u8::is_ascii_digit)
    {
        return Err(std::io::Error::other(
            "invalid launcher gate ownership record",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
struct StagedControlFile {
    file: File,
    name: &'static str,
    path: PathBuf,
    identity: FileIdentity,
    image: Arc<[u8]>,
}

#[cfg(target_os = "macos")]
struct StagedStatusFile {
    file: File,
    path: PathBuf,
    identity: FileIdentity,
}

#[cfg(target_os = "macos")]
impl StagedControlFile {
    fn length(&self) -> std::io::Result<u64> {
        u64::try_from(self.image.len())
            .map_err(|_| std::io::Error::other("staged control file is too large"))
    }

    fn verify(&self, directory: &File) -> std::io::Result<()> {
        let mapped = File::from(
            rustix::fs::openat(
                directory,
                Path::new(self.name),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        if FileIdentity::of(&mapped.metadata()?) != self.identity {
            return Err(std::io::Error::other(
                "staged broker control identity changed",
            ));
        }
        verify_private_file_image(&self.file, self.identity, &self.image)
    }
}

#[cfg(target_os = "macos")]
impl StagedStatusFile {
    fn verify(&self, directory: &File) -> std::io::Result<broker::BrokerStatus> {
        let mapped = File::from(
            rustix::fs::openat(
                directory,
                Path::new("status"),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        if FileIdentity::of(&mapped.metadata()?) != self.identity {
            return Err(std::io::Error::other(
                "staged broker status identity changed",
            ));
        }
        let _mapped_status = broker::read_status(&mapped, self.identity)?;
        broker::read_status(&self.file, self.identity)
    }
}

#[cfg(target_os = "macos")]
struct PendingBrokerStage<'a> {
    root: &'a File,
    directory: &'a File,
    directory_name: &'a str,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl Drop for PendingBrokerStage<'_> {
    fn drop(&mut self) {
        if self.armed {
            cleanup_broker_stage(self.root, self.directory, self.directory_name);
        }
    }
}

#[cfg(target_os = "macos")]
impl BrokerStage {
    fn create(
        authority: &ProductionProcessLaunchAuthority,
        spec: &ProcessSpec,
        environment: &[(OsString, OsString)],
    ) -> std::io::Result<Self> {
        Self::create_with_observers(authority, spec, environment, |_, _| Ok(()), |_, _| Ok(()))
    }

    #[cfg(test)]
    fn create_with_observer<F>(
        authority: &ProductionProcessLaunchAuthority,
        spec: &ProcessSpec,
        environment: &[(OsString, OsString)],
        after_finalize: F,
    ) -> std::io::Result<Self>
    where
        F: FnOnce(&File, &str) -> std::io::Result<()>,
    {
        Self::create_with_observers(authority, spec, environment, |_, _| Ok(()), after_finalize)
    }

    fn create_with_observers<G, F>(
        authority: &ProductionProcessLaunchAuthority,
        spec: &ProcessSpec,
        environment: &[(OsString, OsString)],
        after_gate_bind: G,
        after_finalize: F,
    ) -> std::io::Result<Self>
    where
        G: FnOnce(&ShortGateRoot, &gate::ParentGate) -> std::io::Result<()>,
        F: FnOnce(&File, &str) -> std::io::Result<()>,
    {
        authority.verify()?;
        let (executable_path, executable_identity, executable_length, target_policy) =
            match authority.target_policy {
                ProductionTargetPolicy::SealedCanary => {
                    let sealed =
                        authority.inner.sealed_executable.as_ref().ok_or_else(|| {
                            std::io::Error::other("sealed canary target is absent")
                        })?;
                    (
                        sealed.path()?,
                        sealed.file_identity,
                        sealed.file.metadata()?.len(),
                        broker::BrokerTargetPolicy::SealedClone,
                    )
                }
                ProductionTargetPolicy::PinnedExternal => {
                    let RetainedExecutableNamespace::CanonicalExternal(path) =
                        &authority.inner.executable_namespace
                    else {
                        return Err(std::io::Error::other(
                            "pinned external target path is absent",
                        ));
                    };
                    let RetainedExecutableProof::Attested {
                        attestation,
                        admitted_metadata,
                    } = &authority.inner.executable_proof
                    else {
                        return Err(std::io::Error::other(
                            "pinned external target proof is absent",
                        ));
                    };
                    (
                        path.clone(),
                        authority.inner.executable_identity,
                        attestation.length,
                        broker::BrokerTargetPolicy::PinnedExternal {
                            attestation: *attestation,
                            admitted_metadata: *admitted_metadata,
                        },
                    )
                }
            };

        let directory_name =
            reserve_owned_directory(&authority.broker_staging.directory, ".orchestrator-request")?;
        let directory =
            match open_owned_directory(&authority.broker_staging.directory, &directory_name) {
                Ok(directory) => directory,
                Err(error) => {
                    let _ = rustix::fs::unlinkat(
                        &authority.broker_staging.directory,
                        Path::new(&directory_name),
                        rustix::fs::AtFlags::REMOVEDIR,
                    );
                    return Err(error);
                }
            };
        let mut pending = PendingBrokerStage {
            root: &authority.broker_staging.directory,
            directory: &directory,
            directory_name: &directory_name,
            armed: true,
        };
        let directory_identity = FileIdentity::of(&directory.metadata()?);
        let gate_root = ShortGateRoot::create(&authority.short_gate_parent)?;
        let gate = gate::ParentGate::bind(gate_root.gate_path())?;
        after_gate_bind(&gate_root, &gate)?;
        gate_root.directory.sync_all()?;
        gate_root.parent.sync_all()?;
        let gate_ownership_image = GateRootOwnership::new(
            authority.inner.root_identity,
            authority.broker_staging.directory_identity,
            directory_identity,
            &gate_root,
            gate.control().identity(),
        )
        .encode()?;
        let gate_ownership =
            stage_control_file(&directory, GATE_ROOT_RECORD_NAME, &gate_ownership_image)?;
        directory.sync_all()?;
        authority.broker_staging.directory.sync_all()?;
        let request_image = broker::encode_request(&broker::BrokerRequest {
            gate_challenge: gate.control().challenge(),
            cwd_identity: authority.inner.cwd_identity,
            executable_path: &executable_path,
            executable_identity,
            executable_length,
            target_policy,
            arguments: &spec.argv[1..],
            environment,
        })?;
        let status = stage_status_file(&directory)?;
        let request = stage_control_file(&directory, "request", &request_image)?;
        let stdin = spec
            .stdin
            .as_ref()
            .map(|input| stage_control_file(&directory, "stdin", input))
            .transpose()?;
        directory.sync_all()?;
        rustix::fs::fchmod(&directory, rustix::fs::Mode::from_raw_mode(0o500))
            .map_err(std::io::Error::from)?;
        authority.broker_staging.directory.sync_all()?;
        after_finalize(&directory, &directory_name)?;
        let root = authority.broker_staging.directory.try_clone()?;
        let ownership = OwnedBrokerRequest {
            directory_name: directory_name.clone(),
            directory_identity,
        };
        authority
            .broker_staging
            .request_registry
            .register(ownership);
        pending.armed = false;
        drop(pending);

        let stage = Self {
            root,
            root_identity: authority.broker_staging.directory_identity,
            directory_identity,
            directory,
            directory_name,
            request_registry: Arc::clone(&authority.broker_staging.request_registry),
            request,
            status,
            stdin,
            gate,
            gate_root,
            gate_ownership,
        };
        stage.verify()?;
        Ok(stage)
    }

    fn command(
        &self,
        authority: &ProductionProcessLaunchAuthority,
    ) -> std::io::Result<std::process::Command> {
        authority.verify()?;
        self.verify()?;
        let mut command = std::process::Command::new(authority.sealed_broker.path()?);
        command.current_dir(Path::new("/"));
        command.env_clear();
        add_control_environment(
            &mut command,
            &self.request,
            broker::REQUEST_PATH_ENV,
            broker::REQUEST_DEVICE_ENV,
            broker::REQUEST_INODE_ENV,
            broker::REQUEST_LENGTH_ENV,
        )?;
        command.env(broker::STATUS_PATH_ENV, &self.status.path);
        command.env(
            broker::STATUS_DEVICE_ENV,
            self.status.identity.device.to_string(),
        );
        command.env(
            broker::STATUS_INODE_ENV,
            self.status.identity.inode.to_string(),
        );
        command.env(broker::GATE_PATH_ENV, self.gate.control().path());
        command.env(
            broker::GATE_DEVICE_ENV,
            self.gate.control().identity().device.to_string(),
        );
        command.env(
            broker::GATE_INODE_ENV,
            self.gate.control().identity().inode.to_string(),
        );
        if let Some(stdin) = &self.stdin {
            add_control_environment(
                &mut command,
                stdin,
                broker::STDIN_PATH_ENV,
                broker::STDIN_DEVICE_ENV,
                broker::STDIN_INODE_ENV,
                broker::STDIN_LENGTH_ENV,
            )?;
        }
        command.process_group(0);
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.stdin(Stdio::from(authority.inner.cwd.try_clone()?));
        Ok(command)
    }

    fn verify(&self) -> std::io::Result<()> {
        let root_metadata = self.root.metadata()?;
        if !root_metadata.is_dir() || FileIdentity::of(&root_metadata) != self.root_identity {
            return Err(std::io::Error::other(
                "broker staging root identity changed",
            ));
        }
        let mapped_directory = open_owned_directory(&self.root, &self.directory_name)?;
        let directory_metadata = self.directory.metadata()?;
        if FileIdentity::of(&directory_metadata) != self.directory_identity
            || FileIdentity::of(&mapped_directory.metadata()?) != self.directory_identity
            || directory_metadata.permissions().mode() & 0o777 != 0o500
        {
            return Err(std::io::Error::other(
                "broker staging directory identity changed",
            ));
        }
        self.request.verify(&self.directory)?;
        let _status = self.status.verify(&self.directory)?;
        self.gate_root.verify()?;
        self.gate.verify()?;
        self.gate_ownership.verify(&self.directory)?;
        if let Some(stdin) = &self.stdin {
            stdin.verify(&self.directory)?;
        }
        verify_owned_directory_entries(
            &self.directory,
            if self.stdin.is_some() {
                &["gate-root", "request", "status", "stdin"]
            } else {
                &["gate-root", "request", "status"]
            },
        )
    }

    fn status_after_cleanup(&self) -> std::io::Result<broker::BrokerStatus> {
        self.verify()?;
        self.status.verify(&self.directory)
    }
}

#[cfg(target_os = "macos")]
impl Drop for BrokerStage {
    fn drop(&mut self) {
        if cleanup_broker_stage(&self.root, &self.directory, &self.directory_name) {
            self.request_registry
                .unregister(&self.directory_name, self.directory_identity);
        }
    }
}

#[cfg(target_os = "macos")]
fn add_control_environment(
    command: &mut std::process::Command,
    control: &StagedControlFile,
    path_key: &str,
    device_key: &str,
    inode_key: &str,
    length_key: &str,
) -> std::io::Result<()> {
    command.env(path_key, &control.path);
    command.env(device_key, control.identity.device.to_string());
    command.env(inode_key, control.identity.inode.to_string());
    command.env(length_key, control.length()?.to_string());
    Ok(())
}

#[cfg(target_os = "macos")]
fn reserve_owned_directory(root: &File, prefix: &str) -> std::io::Result<String> {
    (0..32)
        .find_map(|_| {
            let nonce = LAUNCH_SEAL_NONCE.fetch_add(1, Ordering::Relaxed);
            let candidate = format!("{prefix}-{}-{nonce}", std::process::id());
            match rustix::fs::mkdirat(
                root,
                Path::new(&candidate),
                rustix::fs::Mode::from_raw_mode(0o700),
            ) {
                Ok(()) => Some(Ok(candidate)),
                Err(rustix::io::Errno::EXIST) => None,
                Err(error) => Some(Err(std::io::Error::from(error))),
            }
        })
        .transpose()?
        .ok_or_else(|| std::io::Error::other("cannot reserve process-owned directory"))
}

#[cfg(target_os = "macos")]
fn open_owned_directory(root: &File, name: &str) -> std::io::Result<File> {
    Ok(File::from(
        rustix::fs::openat(
            root,
            Path::new(name),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    ))
}

#[cfg(target_os = "macos")]
fn verify_owned_directory_mapping(
    root: &File,
    directory: &File,
    directory_name: &str,
) -> std::io::Result<()> {
    let mapped = open_owned_directory(root, directory_name)?;
    if FileIdentity::of(&mapped.metadata()?) != FileIdentity::of(&directory.metadata()?) {
        return Err(std::io::Error::other(
            "process-owned directory mapping identity changed",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_owned_directory_mapping(
    root: &File,
    directory: &File,
    directory_name: &str,
) -> std::io::Result<()> {
    verify_owned_directory_mapping(root, directory, directory_name)?;
    rustix::fs::unlinkat(
        root,
        Path::new(directory_name),
        rustix::fs::AtFlags::REMOVEDIR,
    )
    .map_err(std::io::Error::from)?;
    root.sync_all()
}

#[cfg(target_os = "macos")]
fn cleanup_empty_owned_directory(root: &File, directory: &File, directory_name: &str) {
    let _ = remove_owned_directory_mapping(root, directory, directory_name);
}

#[cfg(target_os = "macos")]
fn stage_status_file(directory: &File) -> std::io::Result<StagedStatusFile> {
    let image = broker::pending_status_image();
    let mut destination = File::from(
        rustix::fs::openat(
            directory,
            Path::new("status"),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        )
        .map_err(std::io::Error::from)?,
    );
    destination.write_all(&image)?;
    destination.sync_all()?;
    rustix::fs::fchmod(&destination, rustix::fs::Mode::from_raw_mode(0o600))
        .map_err(std::io::Error::from)?;
    drop(destination);
    let file = File::from(
        rustix::fs::openat(
            directory,
            Path::new("status"),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let status = StagedStatusFile {
        path: path_from_fd(&file)?,
        identity: FileIdentity::of(&file.metadata()?),
        file,
    };
    if status.verify(directory)? != broker::BrokerStatus::Pending {
        return Err(std::io::Error::other(
            "staged broker status did not begin pending",
        ));
    }
    Ok(status)
}

#[cfg(target_os = "macos")]
fn stage_control_file(
    directory: &File,
    name: &'static str,
    image: &[u8],
) -> std::io::Result<StagedControlFile> {
    let mut destination = File::from(
        rustix::fs::openat(
            directory,
            Path::new(name),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o400),
        )
        .map_err(std::io::Error::from)?,
    );
    destination.write_all(image)?;
    destination.sync_all()?;
    rustix::fs::fchmod(&destination, rustix::fs::Mode::from_raw_mode(0o400))
        .map_err(std::io::Error::from)?;
    drop(destination);
    let file = File::from(
        rustix::fs::openat(
            directory,
            Path::new(name),
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(std::io::Error::from)?,
    );
    let staged = StagedControlFile {
        path: path_from_fd(&file)?,
        identity: FileIdentity::of(&file.metadata()?),
        file,
        name,
        image: Arc::from(image),
    };
    staged.verify(directory)?;
    Ok(staged)
}

#[cfg(target_os = "macos")]
fn verify_private_file_image(
    file: &File,
    identity: FileIdentity,
    image: &[u8],
) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;

    let length = u64::try_from(image.len())
        .map_err(|_| std::io::Error::other("private file image is too large"))?;
    let before = file.metadata()?;
    if !before.is_file()
        || FileIdentity::of(&before) != identity
        || before.len() != length
        || before.permissions().mode() & 0o777 != 0o400
    {
        return Err(std::io::Error::other("private file identity changed"));
    }
    let mut offset = 0usize;
    let mut buffer = [0u8; READER_CHUNK];
    while offset < image.len() {
        let chunk_length = (image.len() - offset).min(buffer.len());
        let read = file.read_at(&mut buffer[..chunk_length], offset as u64)?;
        if read == 0 || buffer[..read] != image[offset..offset.saturating_add(read)] {
            return Err(std::io::Error::other("private file image changed"));
        }
        offset = offset.saturating_add(read);
    }
    let after = file.metadata()?;
    if FileIdentity::of(&after) != identity
        || after.len() != length
        || after.permissions().mode() & 0o777 != 0o400
    {
        return Err(std::io::Error::other(
            "private file changed during verification",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_owned_directory_entries(directory: &File, expected: &[&str]) -> std::io::Result<()> {
    let mut actual = rustix::fs::Dir::read_from(directory)
        .map_err(std::io::Error::from)?
        .filter_map(|entry| match entry {
            Ok(entry) => {
                let name = entry.file_name().to_bytes();
                if name == b"." || name == b".." {
                    None
                } else {
                    Some(Ok(name.to_vec()))
                }
            }
            Err(error) => Some(Err(std::io::Error::from(error))),
        })
        .collect::<std::io::Result<Vec<_>>>()?;
    actual.sort_unstable();
    let mut expected = expected
        .iter()
        .map(|name| name.as_bytes().to_vec())
        .collect::<Vec<_>>();
    expected.sort_unstable();
    if actual != expected {
        return Err(std::io::Error::other(
            "process-owned directory contains unknown entries",
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn cleanup_short_gate_root(parent: &File, directory: &File, name: &str) {
    if verify_owned_directory_mapping(parent, directory, name).is_err() {
        return;
    }
    let _ = rustix::fs::fchmod(directory, rustix::fs::Mode::from_raw_mode(0o700));
    let _ = rustix::fs::unlinkat(directory, Path::new("gate"), rustix::fs::AtFlags::empty());
    let _ = remove_owned_directory_mapping(parent, directory, name);
}

#[cfg(target_os = "macos")]
fn cleanup_broker_stage(root: &File, directory: &File, directory_name: &str) -> bool {
    if verify_owned_directory_mapping(root, directory, directory_name).is_err() {
        return false;
    }
    let _ = rustix::fs::fchmod(directory, rustix::fs::Mode::from_raw_mode(0o700));
    for name in [GATE_ROOT_RECORD_NAME, "request", "status", "stdin"] {
        let _ = rustix::fs::unlinkat(directory, Path::new(name), rustix::fs::AtFlags::empty());
    }
    remove_owned_directory_mapping(root, directory, directory_name).is_ok()
}

#[cfg(target_os = "macos")]
static LAUNCH_SEAL_NONCE: AtomicU64 = AtomicU64::new(1);

#[cfg(target_os = "macos")]
struct SealedExecutable {
    root: File,
    directory: File,
    directory_name: String,
    kind: ProcessOwnedDirectoryKind,
    directory_identity: FileIdentity,
    file: File,
    file_identity: FileIdentity,
    attestation: Option<ExecutableFileAttestation>,
    admitted_metadata: Option<ExecutableMetadataSnapshot>,
}

#[cfg(target_os = "macos")]
struct PendingSealedDirectory<'a> {
    root: &'a File,
    directory: &'a File,
    directory_name: &'a str,
    armed: bool,
}

#[cfg(target_os = "macos")]
impl Drop for PendingSealedDirectory<'_> {
    fn drop(&mut self) {
        if self.armed {
            cleanup_sealed_directory(self.root, self.directory, self.directory_name);
        }
    }
}

#[cfg(target_os = "macos")]
fn stream_attested_executable<F>(
    source: &File,
    source_identity: FileIdentity,
    attestation: ExecutableFileAttestation,
    destination: &mut File,
    observer: &mut F,
) -> std::io::Result<ExecutableMetadataSnapshot>
where
    F: FnMut(u64, usize) -> std::io::Result<()>,
{
    use std::os::unix::fs::FileExt;

    let before = executable_metadata_snapshot(
        source,
        source_identity,
        attestation,
        ExecutableFileMode::RetainedSource,
    )?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; EXECUTABLE_STREAM_CHUNK].into_boxed_slice();
    let buffer_length = u64::try_from(buffer.len())
        .map_err(|_| std::io::Error::other("streaming executable buffer is too large"))?;
    let mut offset = 0_u64;
    while offset < attestation.length {
        let remaining = attestation.length.saturating_sub(offset);
        let chunk_length = usize::try_from(remaining.min(buffer_length))
            .map_err(|_| std::io::Error::other("streaming executable chunk is too large"))?;
        let read = source.read_at(&mut buffer[..chunk_length], offset)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "attested executable ended during sealed copy",
            ));
        }
        destination.write_all(&buffer[..read])?;
        hasher.update(&buffer[..read]);
        let read_length = read;
        let read = u64::try_from(read)
            .map_err(|_| std::io::Error::other("streaming executable read is too large"))?;
        offset = offset
            .checked_add(read)
            .ok_or_else(|| std::io::Error::other("streaming executable offset overflow"))?;
        observer(offset, read_length)?;
    }
    let actual_digest: [u8; 32] = hasher.finalize().into();
    if actual_digest != attestation.sha256 {
        return Err(std::io::Error::other(
            "attested executable SHA-256 changed during sealed copy",
        ));
    }
    let after = executable_metadata_snapshot(
        source,
        source_identity,
        attestation,
        ExecutableFileMode::RetainedSource,
    )?;
    if after != before {
        return Err(std::io::Error::other(
            "attested executable metadata changed during sealed copy",
        ));
    }
    Ok(after)
}

#[cfg(target_os = "macos")]
fn seal_external_attested_executable_with_observers<F, G>(
    root: &File,
    source: &File,
    source_identity: FileIdentity,
    attestation: ExecutableFileAttestation,
    pre_copy_metadata: ExecutableMetadataSnapshot,
    stream_observer: F,
    post_copy_observer: G,
) -> std::io::Result<(SealedExecutable, ExecutableMetadataSnapshot)>
where
    F: FnMut(u64, usize) -> std::io::Result<()>,
    G: FnOnce() -> std::io::Result<()>,
{
    let (sealed, streamed_metadata) = SealedExecutable::create_from_attested_file_with_observer(
        root,
        source,
        source_identity,
        attestation,
        ProcessOwnedDirectoryKind::SealedExecutable,
        stream_observer,
    )?;
    if streamed_metadata != pre_copy_metadata {
        return Err(std::io::Error::other(
            "external attested executable changed after its pre-copy proof",
        ));
    }
    post_copy_observer()?;
    let post_copy_metadata = verify_executable_file_attestation(
        source,
        source_identity,
        attestation,
        ExecutableFileMode::RetainedSource,
    )?;
    if post_copy_metadata != pre_copy_metadata {
        return Err(std::io::Error::other(
            "external attested executable changed after its sealed admission",
        ));
    }
    Ok((sealed, post_copy_metadata))
}

#[cfg(target_os = "macos")]
impl SealedExecutable {
    fn create(
        root: &File,
        executable_image: &[u8],
        kind: ProcessOwnedDirectoryKind,
    ) -> std::io::Result<Self> {
        let (sealed, source_metadata) = Self::create_with_populator(root, kind, |destination| {
            destination.write_all(executable_image)?;
            Ok(None)
        })?;
        if source_metadata.is_some() {
            return Err(std::io::Error::other(
                "byte-backed executable unexpectedly retained source metadata",
            ));
        }
        Ok(sealed)
    }

    fn create_from_attested_file(
        root: &File,
        source: &File,
        source_identity: FileIdentity,
        attestation: ExecutableFileAttestation,
        kind: ProcessOwnedDirectoryKind,
    ) -> std::io::Result<(Self, ExecutableMetadataSnapshot)> {
        Self::create_from_attested_file_with_observer(
            root,
            source,
            source_identity,
            attestation,
            kind,
            |_, _| Ok(()),
        )
    }

    fn create_from_attested_file_with_observer<F>(
        root: &File,
        source: &File,
        source_identity: FileIdentity,
        attestation: ExecutableFileAttestation,
        kind: ProcessOwnedDirectoryKind,
        mut observer: F,
    ) -> std::io::Result<(Self, ExecutableMetadataSnapshot)>
    where
        F: FnMut(u64, usize) -> std::io::Result<()>,
    {
        validate_attested_executable_length(attestation)?;
        let (sealed, source_metadata) = Self::create_with_populator(root, kind, |destination| {
            let source_metadata = stream_attested_executable(
                source,
                source_identity,
                attestation,
                destination,
                &mut observer,
            )?;
            Ok(Some((attestation, source_metadata)))
        })?;
        let Some(source_metadata) = source_metadata else {
            return Err(std::io::Error::other(
                "attested executable did not retain source metadata",
            ));
        };
        Ok((sealed, source_metadata))
    }

    fn create_with_populator<F>(
        root: &File,
        kind: ProcessOwnedDirectoryKind,
        populate: F,
    ) -> std::io::Result<(Self, Option<ExecutableMetadataSnapshot>)>
    where
        F: FnOnce(
            &mut File,
        ) -> std::io::Result<
            Option<(ExecutableFileAttestation, ExecutableMetadataSnapshot)>,
        >,
    {
        let prefix = match kind {
            ProcessOwnedDirectoryKind::SealedExecutable => ".orchestrator-launch",
            ProcessOwnedDirectoryKind::SealedBroker => ".orchestrator-broker",
            ProcessOwnedDirectoryKind::BrokerRequest | ProcessOwnedDirectoryKind::BrokerStaging => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "broker request layout cannot seal an executable",
                ));
            }
        };
        let directory_name = (0..32)
            .find_map(|_| {
                let nonce = LAUNCH_SEAL_NONCE.fetch_add(1, Ordering::Relaxed);
                let candidate = format!("{prefix}-{}-{nonce}", std::process::id());
                match rustix::fs::mkdirat(
                    root,
                    Path::new(&candidate),
                    rustix::fs::Mode::from_raw_mode(0o700),
                ) {
                    Ok(()) => Some(Ok(candidate)),
                    Err(rustix::io::Errno::EXIST) => None,
                    Err(error) => Some(Err(std::io::Error::from(error))),
                }
            })
            .transpose()?
            .ok_or_else(|| std::io::Error::other("cannot reserve sealed launch directory"))?;
        let directory = match rustix::fs::openat(
            root,
            Path::new(&directory_name),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::DIRECTORY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        ) {
            Ok(fd) => File::from(fd),
            Err(error) => {
                let _ = rustix::fs::unlinkat(
                    root,
                    Path::new(&directory_name),
                    rustix::fs::AtFlags::REMOVEDIR,
                );
                return Err(std::io::Error::from(error));
            }
        };
        let mut pending = PendingSealedDirectory {
            root,
            directory: &directory,
            directory_name: &directory_name,
            armed: true,
        };
        let destination = rustix::fs::openat(
            &directory,
            Path::new("executable"),
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::from_raw_mode(0o600),
        );
        let mut destination = match destination {
            Ok(fd) => File::from(fd),
            Err(error) => return Err(std::io::Error::from(error)),
        };
        let attested_source = populate(&mut destination)?;
        destination.sync_all()?;
        rustix::fs::fchmod(&destination, rustix::fs::Mode::from_raw_mode(0o500))
            .map_err(std::io::Error::from)?;
        destination.sync_all()?;
        drop(destination);
        let file = File::from(
            rustix::fs::openat(
                &directory,
                Path::new("executable"),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let file_identity = FileIdentity::of(&file.metadata()?);
        let (attestation, admitted_metadata, source_metadata) =
            if let Some((attestation, source_metadata)) = attested_source {
                let admitted_metadata = verify_executable_file_attestation(
                    &file,
                    file_identity,
                    attestation,
                    ExecutableFileMode::SealedClone,
                )?;
                (
                    Some(attestation),
                    Some(admitted_metadata),
                    Some(source_metadata),
                )
            } else {
                (None, None, None)
            };
        directory.sync_all()?;
        rustix::fs::fchmod(&directory, rustix::fs::Mode::from_raw_mode(0o500))
            .map_err(std::io::Error::from)?;
        directory.sync_all()?;
        root.sync_all()?;
        let retained_root = root.try_clone()?;
        let directory_identity = FileIdentity::of(&directory.metadata()?);
        pending.armed = false;
        drop(pending);
        let result = Self {
            root: retained_root,
            directory_identity,
            file_identity,
            directory,
            directory_name,
            kind,
            file,
            attestation,
            admitted_metadata,
        };
        result.verify(root)?;
        Ok((result, source_metadata))
    }

    fn verify(&self, root: &File) -> std::io::Result<()> {
        let mapped_directory = File::from(
            rustix::fs::openat(
                root,
                Path::new(&self.directory_name),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let mapped_file = File::from(
            rustix::fs::openat(
                &mapped_directory,
                Path::new("executable"),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(std::io::Error::from)?,
        );
        let directory_metadata = self.directory.metadata()?;
        let file_metadata = self.file.metadata()?;
        if FileIdentity::of(&directory_metadata) != self.directory_identity
            || FileIdentity::of(&mapped_directory.metadata()?) != self.directory_identity
            || directory_metadata.permissions().mode() & 0o777 != 0o500
            || FileIdentity::of(&file_metadata) != self.file_identity
            || FileIdentity::of(&mapped_file.metadata()?) != self.file_identity
            || !file_metadata.is_file()
            || file_metadata.permissions().mode() & 0o777 != 0o500
        {
            return Err(std::io::Error::other(
                "sealed macOS executable identity changed",
            ));
        }
        match (self.attestation, self.admitted_metadata) {
            (Some(attestation), Some(admitted_metadata)) => {
                verify_admitted_executable_metadata(
                    &self.file,
                    self.file_identity,
                    attestation,
                    admitted_metadata,
                    ExecutableFileMode::SealedClone,
                )?;
            }
            (None, None) => {}
            _ => {
                return Err(std::io::Error::other(
                    "sealed executable has incomplete attestation metadata",
                ));
            }
        }
        Ok(())
    }

    fn path(&self) -> std::io::Result<PathBuf> {
        path_from_fd(&self.file)
    }

    fn ownership(&self, root_identity: FileIdentity) -> ProcessOwnedDirectory {
        ProcessOwnedDirectory {
            kind: self.kind,
            name: OsString::from(&self.directory_name),
            root_identity,
            directory_identity: self.directory_identity,
        }
    }
}

#[cfg(target_os = "macos")]
impl Drop for SealedExecutable {
    fn drop(&mut self) {
        cleanup_sealed_directory(&self.root, &self.directory, &self.directory_name);
    }
}

#[cfg(target_os = "macos")]
fn cleanup_sealed_directory(root: &File, directory: &File, directory_name: &str) {
    if verify_owned_directory_mapping(root, directory, directory_name).is_err() {
        return;
    }
    let _ = rustix::fs::fchmod(directory, rustix::fs::Mode::from_raw_mode(0o700));
    let _ = rustix::fs::unlinkat(
        directory,
        Path::new("executable"),
        rustix::fs::AtFlags::empty(),
    );
    let _ = remove_owned_directory_mapping(root, directory, directory_name);
}

#[cfg(target_os = "macos")]
fn path_from_fd(file: &File) -> std::io::Result<PathBuf> {
    let bytes = rustix::fs::getpath(file)
        .map_err(std::io::Error::from)?
        .into_bytes();
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn resolve_executable(program: &OsStr, cwd: &Path) -> std::io::Result<PathBuf> {
    let program_path = Path::new(program);
    if program_path.components().count() > 1 || program_path.is_absolute() {
        let candidate = if program_path.is_absolute() {
            program_path.to_path_buf()
        } else {
            cwd.join(program_path)
        };
        return std::fs::canonicalize(candidate);
    }
    let search_path = std::env::var_os("PATH").ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "PATH is unavailable while resolving executable",
        )
    })?;
    for directory in std::env::split_paths(&search_path) {
        let candidate = directory.join(program_path);
        if let Ok(path) = std::fs::canonicalize(candidate) {
            let metadata = std::fs::metadata(&path)?;
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return Ok(path);
            }
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "process executable was not found",
    ))
}

fn pid_from_raw(raw: u32) -> Option<Pid> {
    i32::try_from(raw).ok().and_then(Pid::from_raw)
}

#[cfg(target_os = "linux")]
fn group_exists(pid: Pid, leader_observed: bool) -> Result<bool, ()> {
    let group = pid.as_raw_pid();
    let entries = std::fs::read_dir("/proc").map_err(|_| ())?;
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        let Some(raw_pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        let stat = match std::fs::read(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        let Some((state, process_group)) = parse_linux_process_stat(&stat) else {
            if entry.path().exists() {
                return Err(());
            }
            continue;
        };
        let retained_leader_zombie = leader_observed && raw_pid == group && state == b'Z';
        if process_group == group && !retained_leader_zombie {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
fn parse_linux_process_stat(stat: &[u8]) -> Option<(u8, i32)> {
    let command_end = stat.iter().rposition(|byte| *byte == b')')?;
    let fields = stat.get(command_end + 1..)?;
    let mut fields = fields
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let state = *fields.next()?.first()?;
    let _parent = fields.next()?;
    let process_group = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
    Some((state, process_group))
}

#[cfg(target_os = "macos")]
fn group_exists(pid: Pid, leader_observed: bool) -> Result<bool, ()> {
    match test_kill_process_group(pid) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(rustix::io::Errno::PERM)
            if leader_observed && matches!(getpgid(Some(pid)), Err(rustix::io::Errno::SRCH)) =>
        {
            // Darwin reports EPERM for a group containing only its retained
            // zombie leader. All supervised descendants begin with the same
            // credentials; a descendant that changes credentials is outside
            // this process-group containment contract.
            Ok(false)
        }
        Err(rustix::io::Errno::PERM) => Ok(true),
        Err(_) => Err(()),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn group_exists(pid: Pid, _leader_observed: bool) -> Result<bool, ()> {
    match test_kill_process_group(pid) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(_) => Err(()),
    }
}

fn signal_group(pid: Pid, signal: Signal) -> Result<bool, ()> {
    match kill_process_group(pid, signal) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::SRCH) => Ok(false),
        Err(_) => Err(()),
    }
}

#[derive(Default)]
struct EventQueue {
    state: Mutex<EventQueueState>,
    wake: Condvar,
}

#[derive(Default)]
struct EventQueueState {
    events: VecDeque<Event>,
    stall: StallTracker,
}

impl EventQueueState {
    fn pop(&mut self) -> Option<Event> {
        self.events.pop_front()
    }

    fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

#[derive(Clone, Copy, Default)]
struct StallSnapshot {
    deadline: Option<Instant>,
    observed_at: Option<Instant>,
    overflowed: bool,
}

#[derive(Default)]
struct StallTracker {
    timeout: Option<Duration>,
    last_activity_at: Option<Instant>,
    deadline: Option<Instant>,
    observed_at: Option<Instant>,
    overflowed: bool,
}

impl StallTracker {
    fn arm(&mut self, timeout: Option<Duration>, at: Instant) {
        self.timeout = timeout;
        self.last_activity_at = timeout.map(|_| at);
        self.deadline = timeout.and_then(|timeout| at.checked_add(timeout));
        self.observed_at = None;
        self.overflowed = timeout.is_some() && self.deadline.is_none();
    }

    fn record_activity(&mut self, at: Instant) {
        let (Some(timeout), Some(deadline), Some(last_activity_at)) =
            (self.timeout, self.deadline, self.last_activity_at)
        else {
            return;
        };
        if self.observed_at.is_some() || at <= last_activity_at {
            return;
        }
        if at >= deadline {
            self.observed_at = Some(deadline);
            return;
        }
        self.last_activity_at = Some(at);
        self.deadline = at.checked_add(timeout);
        self.overflowed |= self.deadline.is_none();
    }

    fn observe(&mut self, now: Instant) {
        if self.observed_at.is_none() && self.deadline.is_some_and(|deadline| now >= deadline) {
            self.observed_at = self.deadline;
        }
    }

    fn snapshot(&self) -> StallSnapshot {
        StallSnapshot {
            deadline: self.deadline,
            observed_at: self.observed_at,
            overflowed: self.overflowed,
        }
    }
}

impl EventQueue {
    fn push(&self, event: Event) {
        lock_unpoisoned(&self.state).events.push_back(event);
        self.wake.notify_one();
    }

    fn restore_front(&self, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        let mut state = lock_unpoisoned(&self.state);
        for event in events.into_iter().rev() {
            state.events.push_front(event);
        }
        drop(state);
        self.wake.notify_one();
    }

    fn push_output_activity(&self, at: Instant) {
        let mut state = lock_unpoisoned(&self.state);
        state.stall.record_activity(at);
        drop(state);
        self.wake.notify_one();
    }

    fn arm_stall(&self, timeout: Option<Duration>, at: Instant) {
        lock_unpoisoned(&self.state).stall.arm(timeout, at);
        self.wake.notify_one();
    }

    fn observe_stall(&self, now: Instant) -> StallSnapshot {
        let mut state = lock_unpoisoned(&self.state);
        state.stall.observe(now);
        state.stall.snapshot()
    }

    fn try_pop(&self) -> Option<Event> {
        lock_unpoisoned(&self.state).pop()
    }

    fn pop_until(&self, deadline: Instant) -> Option<Event> {
        let mut state = lock_unpoisoned(&self.state);
        loop {
            if let Some(event) = state.pop() {
                return Some(event);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let duration = deadline.saturating_duration_since(now);
            let waited = self.wake.wait_timeout(state, duration);
            let (next_state, timed_out) = match waited {
                Ok(result) => result,
                Err(poisoned) => poisoned.into_inner(),
            };
            state = next_state;
            if timed_out.timed_out() && state.is_empty() {
                return None;
            }
        }
    }
}

enum Event {
    Cancelled(Instant),
    LeaderObserved(Instant),
    LeaderObservationFailed,
    LeaderReaped(ExitStatus),
    LeaderReapFailed,
    ReaderDone {
        stream: OutputStream,
        io_error: bool,
    },
    WriterDone(std::io::Result<()>),
    GateReady(gate::ParentGateConnection),
    GateFailed,
    GateActorReply,
    StartedActorReply,
}

struct ProcessStartGateArbitration {
    state: Mutex<ProcessStartGateArbitrationState>,
    queue: Weak<EventQueue>,
    cancellation: Option<CancellationToken>,
    hard_deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessStartGateArbitrationState {
    Pending,
    ReleaseAuthorized,
    Closed(ProcessNotStartedReason),
    GrantWriting,
    Finished,
}

enum BeginGrant {
    Permit(ProcessReleasePermit),
    Closed(ProcessNotStartedReason),
}

impl ProcessStartGateArbitration {
    fn new(
        queue: &Arc<EventQueue>,
        cancellation: Option<&CancellationToken>,
        hard_deadline: Instant,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ProcessStartGateArbitrationState::Pending),
            queue: Arc::downgrade(queue),
            cancellation: cancellation.cloned(),
            hard_deadline,
        })
    }

    fn actor_decide(
        &self,
        decision: ProcessStartGateActorDecision,
    ) -> Result<(), ProcessStartGateClosed> {
        let queue = self.queue.upgrade().ok_or(ProcessStartGateClosed(()))?;
        {
            let mut state = lock_unpoisoned(&self.state);
            if *state != ProcessStartGateArbitrationState::Pending {
                return Err(ProcessStartGateClosed(()));
            }
            if self
                .cancellation
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                *state =
                    ProcessStartGateArbitrationState::Closed(ProcessNotStartedReason::Cancelled);
                drop(state);
                queue.push(Event::GateActorReply);
                return Err(ProcessStartGateClosed(()));
            }
            if Instant::now() >= self.hard_deadline {
                *state =
                    ProcessStartGateArbitrationState::Closed(ProcessNotStartedReason::Deadline);
                drop(state);
                queue.push(Event::GateActorReply);
                return Err(ProcessStartGateClosed(()));
            }
            *state = match decision {
                ProcessStartGateActorDecision::ReleaseAuthorized => {
                    ProcessStartGateArbitrationState::ReleaseAuthorized
                }
                ProcessStartGateActorDecision::Rejected => {
                    ProcessStartGateArbitrationState::Closed(ProcessNotStartedReason::GateRejected)
                }
                ProcessStartGateActorDecision::Indeterminate => {
                    ProcessStartGateArbitrationState::Closed(
                        ProcessNotStartedReason::GateIndeterminate,
                    )
                }
            };
        }
        queue.push(Event::GateActorReply);
        Ok(())
    }

    fn cancel(&self, reason: ProcessNotStartedReason) -> ProcessNotStartedReason {
        let mut state = lock_unpoisoned(&self.state);
        match *state {
            ProcessStartGateArbitrationState::Pending
            | ProcessStartGateArbitrationState::ReleaseAuthorized => {
                *state = ProcessStartGateArbitrationState::Closed(reason);
                reason
            }
            ProcessStartGateArbitrationState::Closed(winner) => winner,
            ProcessStartGateArbitrationState::GrantWriting
            | ProcessStartGateArbitrationState::Finished => reason,
        }
    }

    fn begin_grant(
        self: &Arc<Self>,
        cancellation: Option<&CancellationToken>,
        hard_deadline: Instant,
    ) -> BeginGrant {
        let now = Instant::now();
        let cancellation_won = cancellation
            .and_then(CancellationToken::cancelled_at)
            .is_some_and(|cancelled| cancelled <= now);
        let mut state = lock_unpoisoned(&self.state);
        match *state {
            ProcessStartGateArbitrationState::ReleaseAuthorized if cancellation_won => {
                *state =
                    ProcessStartGateArbitrationState::Closed(ProcessNotStartedReason::Cancelled);
                BeginGrant::Closed(ProcessNotStartedReason::Cancelled)
            }
            ProcessStartGateArbitrationState::ReleaseAuthorized if now >= hard_deadline => {
                *state =
                    ProcessStartGateArbitrationState::Closed(ProcessNotStartedReason::Deadline);
                BeginGrant::Closed(ProcessNotStartedReason::Deadline)
            }
            ProcessStartGateArbitrationState::ReleaseAuthorized => {
                *state = ProcessStartGateArbitrationState::GrantWriting;
                BeginGrant::Permit(ProcessReleasePermit {
                    arbitration: Arc::clone(self),
                    hard_deadline,
                })
            }
            ProcessStartGateArbitrationState::Closed(reason) => BeginGrant::Closed(reason),
            ProcessStartGateArbitrationState::Pending
            | ProcessStartGateArbitrationState::GrantWriting
            | ProcessStartGateArbitrationState::Finished => {
                *state =
                    ProcessStartGateArbitrationState::Closed(ProcessNotStartedReason::GateProtocol);
                BeginGrant::Closed(ProcessNotStartedReason::GateProtocol)
            }
        }
    }

    fn snapshot(&self) -> ProcessStartGateArbitrationState {
        *lock_unpoisoned(&self.state)
    }

    fn finish_grant(&self) {
        let mut state = lock_unpoisoned(&self.state);
        if *state == ProcessStartGateArbitrationState::GrantWriting {
            *state = ProcessStartGateArbitrationState::Finished;
        }
    }
}

struct ProcessStartedObservationArbitration {
    state: Mutex<ProcessStartedObservationState>,
    queue: Weak<EventQueue>,
    cancellation: Option<CancellationToken>,
    hard_deadline: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProcessStartedObservationState {
    Pending,
    Persisted,
    Indeterminate,
    Starting,
    Finished,
}

impl ProcessStartedObservationArbitration {
    fn new(
        queue: &Arc<EventQueue>,
        cancellation: Option<&CancellationToken>,
        hard_deadline: Instant,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(ProcessStartedObservationState::Pending),
            queue: Arc::downgrade(queue),
            cancellation: cancellation.cloned(),
            hard_deadline,
        })
    }

    fn actor_decide(
        &self,
        decision: ProcessStartedObservationDecision,
    ) -> Result<(), ProcessStartGateClosed> {
        let queue = self.queue.upgrade().ok_or(ProcessStartGateClosed(()))?;
        let cancellation = self
            .cancellation
            .as_ref()
            .map(|token| lock_unpoisoned(&token.inner.state));
        {
            let mut state = lock_unpoisoned(&self.state);
            if *state != ProcessStartedObservationState::Pending {
                return Err(ProcessStartGateClosed(()));
            }
            if cancellation
                .as_ref()
                .is_some_and(|state| state.cancelled_at.is_some())
                || Instant::now() >= self.hard_deadline
            {
                *state = ProcessStartedObservationState::Indeterminate;
                drop(state);
                drop(cancellation);
                queue.push(Event::StartedActorReply);
                return Err(ProcessStartGateClosed(()));
            }
            *state = match decision {
                ProcessStartedObservationDecision::Persisted => {
                    ProcessStartedObservationState::Persisted
                }
                ProcessStartedObservationDecision::Indeterminate => {
                    ProcessStartedObservationState::Indeterminate
                }
            };
        }
        drop(cancellation);
        queue.push(Event::StartedActorReply);
        Ok(())
    }

    fn cancel(&self) {
        let mut state = lock_unpoisoned(&self.state);
        if matches!(
            *state,
            ProcessStartedObservationState::Pending | ProcessStartedObservationState::Persisted
        ) {
            *state = ProcessStartedObservationState::Indeterminate;
        }
    }

    fn begin_start(self: &Arc<Self>) -> Option<ProcessFinalStartPermit> {
        self.begin_start_at(Instant::now())
    }

    fn begin_start_at(self: &Arc<Self>, now: Instant) -> Option<ProcessFinalStartPermit> {
        let cancellation = self
            .cancellation
            .as_ref()
            .map(|token| lock_unpoisoned(&token.inner.state));
        let mut state = lock_unpoisoned(&self.state);
        let cancellation_won = cancellation
            .as_ref()
            .is_some_and(|state| state.cancelled_at.is_some());
        if *state != ProcessStartedObservationState::Persisted
            || cancellation_won
            || now >= self.hard_deadline
        {
            if matches!(
                *state,
                ProcessStartedObservationState::Pending | ProcessStartedObservationState::Persisted
            ) {
                *state = ProcessStartedObservationState::Indeterminate;
            }
            return None;
        }
        *state = ProcessStartedObservationState::Starting;
        Some(ProcessFinalStartPermit {
            arbitration: Arc::clone(self),
            hard_deadline: self.hard_deadline,
        })
    }

    fn finish_start(&self) {
        let mut state = lock_unpoisoned(&self.state);
        if *state == ProcessStartedObservationState::Starting {
            *state = ProcessStartedObservationState::Finished;
        }
    }
}

enum GateAwaitOutcome {
    Released(KernelProcessIdentity),
    NotReleased {
        reason: ProcessNotStartedReason,
        identity: Option<KernelProcessIdentity>,
    },
    ReleaseUncertain(KernelProcessIdentity),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum GrantDeliveryOutcome {
    Released,
    NotReleased(ProcessNotStartedReason),
    ReleaseUncertain,
}

fn classify_grant_delivery(delivery: gate::GrantDelivery) -> GrantDeliveryOutcome {
    match delivery {
        gate::GrantDelivery::Delivered => GrantDeliveryOutcome::Released,
        gate::GrantDelivery::NotDeliveredDeadline
        | gate::GrantDelivery::NotDeliveredIncompleteGrantDeadline => {
            GrantDeliveryOutcome::NotReleased(ProcessNotStartedReason::Deadline)
        }
        gate::GrantDelivery::NotDeliveredSocketIdentity
        | gate::GrantDelivery::NotDeliveredTimeoutConfiguration
        | gate::GrantDelivery::NotDeliveredIncompleteGrant => {
            GrantDeliveryOutcome::NotReleased(ProcessNotStartedReason::GateProtocol)
        }
        gate::GrantDelivery::AmbiguousAcknowledgementDeadline
        | gate::GrantDelivery::AmbiguousAcknowledgementReadConfiguration
        | gate::GrantDelivery::AmbiguousAcknowledgementRead
        | gate::GrantDelivery::AmbiguousAcknowledgementMismatch => {
            GrantDeliveryOutcome::ReleaseUncertain
        }
    }
}

fn classify_start_delivery(delivery: gate::GrantDelivery) -> GrantDeliveryOutcome {
    match delivery {
        gate::GrantDelivery::Delivered => GrantDeliveryOutcome::Released,
        gate::GrantDelivery::NotDeliveredSocketIdentity
        | gate::GrantDelivery::NotDeliveredDeadline
        | gate::GrantDelivery::NotDeliveredTimeoutConfiguration
        | gate::GrantDelivery::NotDeliveredIncompleteGrant
        | gate::GrantDelivery::NotDeliveredIncompleteGrantDeadline
        | gate::GrantDelivery::AmbiguousAcknowledgementDeadline
        | gate::GrantDelivery::AmbiguousAcknowledgementReadConfiguration
        | gate::GrantDelivery::AmbiguousAcknowledgementRead
        | gate::GrantDelivery::AmbiguousAcknowledgementMismatch => {
            GrantDeliveryOutcome::ReleaseUncertain
        }
    }
}

fn await_and_grant_start_gate(
    queue: &Arc<EventQueue>,
    hard_deadline: Instant,
    pid: Pid,
    start_gate: &mut ProcessStartGate,
    cancellation: Option<&CancellationToken>,
    gate_waiter: gate::GateWaiter,
) -> GateAwaitOutcome {
    let mut connection = {
        let Some(event) = queue.pop_until(hard_deadline) else {
            let reason = if gate_waiter.abort().is_ok() {
                ProcessNotStartedReason::Deadline
            } else {
                ProcessNotStartedReason::GateProtocol
            };
            return GateAwaitOutcome::NotReleased {
                reason,
                identity: None,
            };
        };
        match event {
            Event::GateReady(connection) => {
                if gate_waiter.finish().is_err() {
                    drop(connection);
                    return GateAwaitOutcome::NotReleased {
                        reason: ProcessNotStartedReason::GateProtocol,
                        identity: None,
                    };
                }
                connection
            }
            Event::GateFailed => {
                let _ = gate_waiter.finish();
                return GateAwaitOutcome::NotReleased {
                    reason: ProcessNotStartedReason::GateProtocol,
                    identity: None,
                };
            }
            event @ (Event::Cancelled(_)
            | Event::LeaderObserved(_)
            | Event::LeaderObservationFailed
            | Event::LeaderReaped(_)
            | Event::LeaderReapFailed
            | Event::ReaderDone { .. }
            | Event::WriterDone(_)
            | Event::GateActorReply
            | Event::StartedActorReply) => {
                let cancellation_won = matches!(&event, Event::Cancelled(_));
                queue.push(event);
                let reason = if gate_waiter.abort().is_err() {
                    ProcessNotStartedReason::GateProtocol
                } else if cancellation_won {
                    ProcessNotStartedReason::Cancelled
                } else {
                    ProcessNotStartedReason::GateProtocol
                };
                return GateAwaitOutcome::NotReleased {
                    reason,
                    identity: None,
                };
            }
        }
    };

    let Ok(raw_pid) = u32::try_from(pid.as_raw_pid()) else {
        drop(connection);
        return GateAwaitOutcome::NotReleased {
            reason: ProcessNotStartedReason::GateProtocol,
            identity: None,
        };
    };
    let Ok(identity) = KernelProcessIdentity::observe(raw_pid, raw_pid) else {
        drop(connection);
        return GateAwaitOutcome::NotReleased {
            reason: ProcessNotStartedReason::GateProtocol,
            identity: None,
        };
    };
    let arbitration = ProcessStartGateArbitration::new(queue, cancellation, hard_deadline);
    let started_binding = match start_gate.submit(
        identity.clone(),
        ProcessStartGateReply {
            arbitration: Arc::clone(&arbitration),
        },
    ) {
        Ok(binding) => binding,
        Err(()) => {
            arbitration.cancel(ProcessNotStartedReason::GateProtocol);
            drop(connection);
            return GateAwaitOutcome::NotReleased {
                reason: ProcessNotStartedReason::GateProtocol,
                identity: Some(identity),
            };
        }
    };

    let Some(event) = queue.pop_until(hard_deadline) else {
        let reason = arbitration.cancel(ProcessNotStartedReason::Deadline);
        drop(connection);
        return GateAwaitOutcome::NotReleased {
            reason,
            identity: Some(identity),
        };
    };
    match event {
        Event::GateActorReply => match arbitration.snapshot() {
            ProcessStartGateArbitrationState::ReleaseAuthorized => {
                let Ok(confirmed) = KernelProcessIdentity::observe(raw_pid, raw_pid) else {
                    let reason = arbitration.cancel(ProcessNotStartedReason::GateProtocol);
                    drop(connection);
                    return GateAwaitOutcome::NotReleased {
                        reason,
                        identity: Some(identity),
                    };
                };
                if confirmed.pid() != identity.pid()
                    || confirmed.process_group_id() != identity.process_group_id()
                    || confirmed.process_start_identity() != identity.process_start_identity()
                {
                    let reason = arbitration.cancel(ProcessNotStartedReason::GateProtocol);
                    drop(connection);
                    return GateAwaitOutcome::NotReleased {
                        reason,
                        identity: Some(identity),
                    };
                }
                match arbitration.begin_grant(cancellation, hard_deadline) {
                    BeginGrant::Closed(reason) => {
                        drop(connection);
                        GateAwaitOutcome::NotReleased {
                            reason,
                            identity: Some(identity),
                        }
                    }
                    BeginGrant::Permit(permit) => {
                        match classify_grant_delivery(grant_start_gate(&mut connection, permit)) {
                            GrantDeliveryOutcome::Released => {}
                            GrantDeliveryOutcome::NotReleased(reason) => {
                                return GateAwaitOutcome::NotReleased {
                                    reason,
                                    identity: Some(identity),
                                };
                            }
                            GrantDeliveryOutcome::ReleaseUncertain => {
                                return GateAwaitOutcome::ReleaseUncertain(identity);
                            }
                        }
                        let Ok(confirmed) = KernelProcessIdentity::observe(raw_pid, raw_pid) else {
                            return GateAwaitOutcome::ReleaseUncertain(identity);
                        };
                        if !kernel_identities_match(&confirmed, &identity) {
                            return GateAwaitOutcome::ReleaseUncertain(identity);
                        }
                        let receipt = ProcessStartedReceipt {
                            request_binding: started_binding,
                            identity: confirmed,
                        };
                        let Some(start_permit) = await_started_observation(
                            queue,
                            hard_deadline,
                            start_gate,
                            receipt,
                            cancellation,
                        ) else {
                            return GateAwaitOutcome::ReleaseUncertain(identity);
                        };
                        match classify_start_delivery(start_process_gate(connection, start_permit))
                        {
                            GrantDeliveryOutcome::Released => GateAwaitOutcome::Released(identity),
                            GrantDeliveryOutcome::NotReleased(_)
                            | GrantDeliveryOutcome::ReleaseUncertain => {
                                GateAwaitOutcome::ReleaseUncertain(identity)
                            }
                        }
                    }
                }
            }
            ProcessStartGateArbitrationState::Closed(reason) => {
                drop(connection);
                GateAwaitOutcome::NotReleased {
                    reason,
                    identity: Some(identity),
                }
            }
            ProcessStartGateArbitrationState::Pending
            | ProcessStartGateArbitrationState::GrantWriting
            | ProcessStartGateArbitrationState::Finished => {
                let reason = arbitration.cancel(ProcessNotStartedReason::GateProtocol);
                drop(connection);
                GateAwaitOutcome::NotReleased {
                    reason,
                    identity: Some(identity),
                }
            }
        },
        Event::Cancelled(_) => {
            let reason = arbitration.cancel(ProcessNotStartedReason::Cancelled);
            drop(connection);
            GateAwaitOutcome::NotReleased {
                reason,
                identity: Some(identity),
            }
        }
        event @ (Event::LeaderObserved(_)
        | Event::LeaderObservationFailed
        | Event::LeaderReaped(_)
        | Event::LeaderReapFailed
        | Event::ReaderDone { .. }
        | Event::WriterDone(_)
        | Event::StartedActorReply) => {
            queue.push(event);
            let reason = arbitration.cancel(ProcessNotStartedReason::GateProtocol);
            drop(connection);
            GateAwaitOutcome::NotReleased {
                reason,
                identity: Some(identity),
            }
        }
        Event::GateReady(_) | Event::GateFailed => {
            let reason = arbitration.cancel(ProcessNotStartedReason::GateProtocol);
            drop(connection);
            GateAwaitOutcome::NotReleased {
                reason,
                identity: Some(identity),
            }
        }
    }
}

fn grant_start_gate(
    connection: &mut gate::ParentGateConnection,
    permit: ProcessReleasePermit,
) -> gate::GrantDelivery {
    let delivery = connection.grant(permit.hard_deadline);
    permit.arbitration.finish_grant();
    delivery
}

fn start_process_gate(
    connection: gate::ParentGateConnection,
    permit: ProcessFinalStartPermit,
) -> gate::GrantDelivery {
    let delivery = connection.start(permit.hard_deadline);
    permit.arbitration.finish_start();
    delivery
}

fn await_started_observation(
    queue: &Arc<EventQueue>,
    hard_deadline: Instant,
    start_gate: &mut ProcessStartGate,
    receipt: ProcessStartedReceipt,
    cancellation: Option<&CancellationToken>,
) -> Option<ProcessFinalStartPermit> {
    let arbitration = ProcessStartedObservationArbitration::new(queue, cancellation, hard_deadline);
    if start_gate
        .submit_started(
            receipt,
            ProcessStartedObservationReply {
                arbitration: Arc::clone(&arbitration),
            },
        )
        .is_err()
    {
        arbitration.cancel();
        return None;
    }

    let mut deferred = Vec::new();
    let permit = loop {
        let Some(event) = queue.pop_until(hard_deadline) else {
            arbitration.cancel();
            break None;
        };
        if matches!(event, Event::StartedActorReply) {
            break arbitration.begin_start();
        }
        deferred.push(event);
    };
    queue.restore_front(deferred);
    permit
}

fn kernel_identities_match(left: &KernelProcessIdentity, right: &KernelProcessIdentity) -> bool {
    left.pid() == right.pid()
        && left.process_group_id() == right.process_group_id()
        && left.process_start_identity() == right.process_start_identity()
}

type SharedChild = Arc<Mutex<Option<Child>>>;

struct WaitTarget {
    pid: Pid,
    child: SharedChild,
    fail_reap_once: bool,
}

#[cfg(not(any(
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
fn observe_leader_exit(pid: Pid) -> std::io::Result<()> {
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => continue,
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(std::io::Error::from(error)),
        }
    }
}

#[cfg(not(any(
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
)))]
fn try_observe_leader_exit(pid: Pid) -> std::io::Result<bool> {
    loop {
        match waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOWAIT | WaitIdOptions::NOHANG,
        ) {
            Ok(Some(_)) => return Ok(true),
            Ok(None) => return Ok(false),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(std::io::Error::from(error)),
        }
    }
}

#[cfg(any(
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
))]
fn observe_leader_exit(_pid: Pid) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unreaped child observation is unsupported on this platform",
    ))
}

#[cfg(any(
    target_os = "horizon",
    target_os = "openbsd",
    target_os = "redox",
    target_os = "wasi"
))]
fn try_observe_leader_exit(_pid: Pid) -> std::io::Result<bool> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "unreaped child observation is unsupported on this platform",
    ))
}

fn reap_shared_child(child: &SharedChild, fail_once: &mut bool) -> Result<ExitStatus, ()> {
    let mut child = lock_unpoisoned(child);
    let Some(owned_child) = child.as_mut() else {
        return Err(());
    };
    if *fail_once {
        *fail_once = false;
        return Err(());
    }
    loop {
        match owned_child.wait() {
            Ok(status) => {
                *child = None;
                return Ok(status);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(()),
        }
    }
}

#[cfg(test)]
fn force_reap_failure(spec: &ProcessSpec) -> bool {
    spec.fail_reap_once
}

#[cfg(not(test))]
fn force_reap_failure(_spec: &ProcessSpec) -> bool {
    false
}

#[derive(Clone, Copy)]
enum OutputStream {
    Stdout,
    Stderr,
}

struct CaptureState {
    retained: Vec<u8>,
    discarded_bytes: u64,
    cap: usize,
}

impl CaptureState {
    fn new(cap: usize) -> Self {
        Self {
            retained: Vec::with_capacity(cap.min(READER_CHUNK)),
            discarded_bytes: 0,
            cap,
        }
    }

    fn append(&mut self, bytes: &[u8]) {
        let remaining = self.cap.saturating_sub(self.retained.len());
        let retained = remaining.min(bytes.len());
        self.retained.extend_from_slice(&bytes[..retained]);
        self.discarded_bytes = self
            .discarded_bytes
            .saturating_add((bytes.len() - retained) as u64);
    }
}

struct CaptureSnapshot {
    retained: Vec<u8>,
    discarded_bytes: u64,
}

fn snapshot_capture(capture: &Mutex<CaptureState>) -> CaptureSnapshot {
    let capture = lock_unpoisoned(capture);
    CaptureSnapshot {
        retained: capture.retained.clone(),
        discarded_bytes: capture.discarded_bytes,
    }
}

fn read_stream(
    mut stream: impl Read,
    capture: Arc<Mutex<CaptureState>>,
    output_stream: OutputStream,
    queue: Arc<EventQueue>,
    output: Option<ProcessOutputSender>,
) {
    let mut buffer = [0u8; READER_CHUNK];
    let io_error = loop {
        match stream.read(&mut buffer) {
            Ok(0) => break false,
            Ok(read) => {
                lock_unpoisoned(&capture).append(&buffer[..read]);
                if let Some(output) = &output {
                    output.try_send(ProcessOutputChunk {
                        stream: match output_stream {
                            OutputStream::Stdout => ProcessOutputStream::Stdout,
                            OutputStream::Stderr => ProcessOutputStream::Stderr,
                        },
                        bytes: buffer[..read].to_vec(),
                    });
                }
                queue.push_output_activity(Instant::now());
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break true,
        }
    };
    queue.push(Event::ReaderDone {
        stream: output_stream,
        io_error,
    });
}

struct RunState {
    leader_observed_at: Option<Instant>,
    leader_status: Option<ExitStatus>,
    leader_reaped: bool,
    leader_observation_failed: bool,
    reap_failed: bool,
    reap_requested: bool,
    cancellation_at: Option<Instant>,
    stall_observed_at: Option<Instant>,
    stdout_done: bool,
    stderr_done: bool,
    stdin_done: bool,
    group_absent: bool,
    term_sent: bool,
    kill_sent: bool,
    shutdown_started: bool,
    escalation_started: bool,
    term_deadline: Option<Instant>,
    cleanup_deadline: Option<Instant>,
    failures: Vec<ProcessInfrastructureFailure>,
}

impl RunState {
    fn new(stdin_done: bool) -> Self {
        Self {
            leader_observed_at: None,
            leader_status: None,
            leader_reaped: false,
            leader_observation_failed: false,
            reap_failed: false,
            reap_requested: false,
            cancellation_at: None,
            stall_observed_at: None,
            stdout_done: false,
            stderr_done: false,
            stdin_done,
            group_absent: false,
            term_sent: false,
            kill_sent: false,
            shutdown_started: false,
            escalation_started: false,
            term_deadline: None,
            cleanup_deadline: None,
            failures: Vec::new(),
        }
    }

    fn apply(&mut self, event: Event) {
        match event {
            Event::Cancelled(at) => {
                self.cancellation_at = Some(self.cancellation_at.map_or(at, |seen| seen.min(at)));
            }
            Event::LeaderObserved(at) => {
                self.leader_observed_at = Some(
                    self.leader_observed_at
                        .map_or(at, |observed| observed.min(at)),
                );
            }
            Event::LeaderObservationFailed => {
                self.leader_observation_failed = true;
                self.record_failure(ProcessInfrastructureFailure::DirectChildWait);
            }
            Event::LeaderReaped(status) => {
                self.leader_status = Some(status);
                self.leader_reaped = true;
            }
            Event::LeaderReapFailed => {
                self.reap_failed = true;
                self.record_failure(ProcessInfrastructureFailure::DirectChildWait);
            }
            Event::ReaderDone { stream, io_error } => {
                match stream {
                    OutputStream::Stdout => self.stdout_done = true,
                    OutputStream::Stderr => self.stderr_done = true,
                }
                if io_error {
                    self.record_failure(match stream {
                        OutputStream::Stdout => ProcessInfrastructureFailure::StdoutRead,
                        OutputStream::Stderr => ProcessInfrastructureFailure::StderrRead,
                    });
                }
            }
            Event::WriterDone(result) => {
                self.stdin_done = true;
                if result.is_err() {
                    self.record_failure(ProcessInfrastructureFailure::StdinWrite);
                }
            }
            // The gate owner has already classified these events before the
            // general supervision loop begins. A connection can be enqueued
            // concurrently with cancellation while the gate worker is being
            // joined; dropping it here remains fail-closed and must not turn a
            // proven pre-release cancellation into an infrastructure failure.
            Event::GateReady(_)
            | Event::GateFailed
            | Event::GateActorReply
            | Event::StartedActorReply => {}
        }
    }

    fn observe_token(&mut self, token: Option<&CancellationToken>) {
        if let Some(cancelled_at) = token.and_then(CancellationToken::cancelled_at) {
            self.cancellation_at = Some(
                self.cancellation_at
                    .map_or(cancelled_at, |seen| seen.min(cancelled_at)),
            );
        }
    }

    fn record_failure(&mut self, failure: ProcessInfrastructureFailure) {
        if !self.failures.contains(&failure) {
            self.failures.push(failure);
        }
    }

    fn observe_stall(&mut self, stall: StallSnapshot) {
        if stall.overflowed {
            self.record_failure(ProcessInfrastructureFailure::GroupControl);
        }
        if let Some(observed_at) = stall.observed_at {
            if self
                .leader_observed_at
                .is_none_or(|leader_at| observed_at <= leader_at)
            {
                self.stall_observed_at = Some(
                    self.stall_observed_at
                        .map_or(observed_at, |seen| seen.min(observed_at)),
                );
            }
        }
    }

    fn refresh_group_absence(&mut self, pid: Pid) {
        let identity_is_pinned = !self.leader_reaped;
        let group_may_have_changed = self.leader_observed_at.is_some()
            || self.leader_observation_failed
            || self.shutdown_started;
        if self.group_absent || !identity_is_pinned || !group_may_have_changed {
            return;
        }
        match group_exists(pid, self.leader_observed_at.is_some()) {
            Ok(true) => {}
            Ok(false) => self.group_absent = true,
            Err(()) => self.record_failure(ProcessInfrastructureFailure::GroupControl),
        }
    }

    fn maybe_start_shutdown(&mut self, hard_deadline: Instant, pid: Pid, spec: &ProcessSpec) {
        let now = Instant::now();
        let shutdown_triggered = self.cancellation_at.is_some()
            || self.stall_observed_at.is_some()
            || now >= hard_deadline
            || !self.failures.is_empty();
        if shutdown_triggered
            && self.leader_observed_at.is_none()
            && matches!(try_observe_leader_exit(pid), Ok(true))
        {
            self.leader_observed_at = Some(now);
            self.refresh_group_absence(pid);
        }
        let leader_at = self.leader_observed_at;
        let cancelled_before_exit = self
            .cancellation_at
            .is_some_and(|cancelled| leader_at.is_none_or(|exited| cancelled <= exited));
        let deadline_before_exit = leader_at.is_none_or(|exited| hard_deadline <= exited);
        let stalled_before_exit = self
            .stall_observed_at
            .is_some_and(|stalled| leader_at.is_none_or(|exited| stalled <= exited));
        let infrastructure_failure = !self.failures.is_empty();
        let descendants_remain = self.leader_observed_at.is_some() && !self.group_absent;
        let io_complete = self.stdout_done && self.stderr_done && self.stdin_done;
        let must_close = infrastructure_failure
            || cancelled_before_exit
            || (now >= hard_deadline && deadline_before_exit)
            || stalled_before_exit
            || descendants_remain;

        if must_close && !self.shutdown_started {
            self.shutdown_started = true;
            if self.group_absent {
                self.cleanup_deadline = now.checked_add(spec.cleanup_grace);
            } else {
                match signal_group(pid, Signal::TERM) {
                    Ok(true) => self.term_sent = true,
                    Ok(false) => self.group_absent = true,
                    Err(()) => self.record_failure(ProcessInfrastructureFailure::GroupControl),
                }
                self.term_deadline = now.checked_add(spec.term_grace);
                if self.term_deadline.is_none() {
                    self.record_failure(ProcessInfrastructureFailure::GroupControl);
                }
            }
        }

        if self.leader_observed_at.is_some()
            && self.group_absent
            && !io_complete
            && !self.shutdown_started
            && self.cleanup_deadline.is_none()
        {
            self.cleanup_deadline = now.checked_add(spec.cleanup_grace);
            if self.cleanup_deadline.is_none() {
                self.record_failure(ProcessInfrastructureFailure::GroupControl);
            }
        }

        if self.shutdown_started
            && !self.escalation_started
            && !self.group_absent
            && self.term_deadline.is_none_or(|deadline| now >= deadline)
        {
            self.escalation_started = true;
            match signal_group(pid, Signal::KILL) {
                Ok(true) => self.kill_sent = true,
                Ok(false) => self.group_absent = true,
                Err(()) => self.record_failure(ProcessInfrastructureFailure::GroupControl),
            }
            self.cleanup_deadline = now.checked_add(spec.cleanup_grace);
            if self.cleanup_deadline.is_none() {
                self.record_failure(ProcessInfrastructureFailure::GroupControl);
            }
        }
    }

    fn ready_to_finish(&self) -> bool {
        self.leader_reaped
            && self.stdout_done
            && self.stderr_done
            && self.stdin_done
            && self.group_absent
    }

    fn cleanup_expired(&self) -> bool {
        self.cleanup_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }

    fn next_wake(&self, hard_deadline: Instant, stall_deadline: Option<Instant>) -> Instant {
        if let Some(deadline) = self.cleanup_deadline {
            return deadline;
        }
        if let Some(deadline) = self.term_deadline {
            return deadline;
        }
        if self.stall_observed_at.is_none() && self.leader_observed_at.is_none() {
            return stall_deadline.map_or(hard_deadline, |deadline| deadline.min(hard_deadline));
        }
        hard_deadline
    }

    fn termination(&self, hard_deadline: Instant, cleanup_complete: bool) -> ProcessTermination {
        if self
            .failures
            .iter()
            .any(|failure| *failure != ProcessInfrastructureFailure::CleanupIncomplete)
        {
            return ProcessTermination::InfrastructureError;
        }
        if !cleanup_complete {
            return ProcessTermination::UnresolvedOwnership;
        }
        let Some(leader_at) = self.leader_observed_at else {
            return ProcessTermination::UnresolvedOwnership;
        };
        let Some(status) = &self.leader_status else {
            return ProcessTermination::UnresolvedOwnership;
        };
        let cancellation_won = self.cancellation_at.is_some_and(|cancelled| {
            cancelled <= hard_deadline
                && self
                    .stall_observed_at
                    .is_none_or(|stalled| cancelled <= stalled)
                && cancelled <= leader_at
        });
        if cancellation_won {
            return ProcessTermination::Cancelled;
        }
        if hard_deadline <= leader_at
            && self
                .stall_observed_at
                .is_none_or(|stalled| hard_deadline <= stalled)
        {
            return ProcessTermination::Timeout;
        }
        if self
            .stall_observed_at
            .is_some_and(|stalled| stalled <= leader_at)
        {
            return ProcessTermination::Stalled;
        }
        classify(status)
    }
}

fn classify(status: &ExitStatus) -> ProcessTermination {
    if let Some(code) = status.code() {
        ProcessTermination::Exited(code)
    } else if let Some(signal) = status.signal() {
        ProcessTermination::Signaled(signal)
    } else {
        ProcessTermination::InfrastructureError
    }
}

#[derive(Default)]
struct RegistryState {
    active: usize,
    unresolved: usize,
}

struct Registry {
    ownership: Mutex<RegistryState>,
    maximum: usize,
    unresolved_sender: Option<Sender<UnresolvedEntry>>,
    reaper: Option<JoinHandle<()>>,
}

#[cfg(test)]
struct ReaperExitGate {
    reached: Sender<()>,
    release: Receiver<()>,
}

impl Registry {
    fn new(maximum: usize) -> std::io::Result<Arc<Self>> {
        Self::new_inner(maximum, None)
    }

    #[cfg(test)]
    fn new_with_reaper_exit_gate(
        maximum: usize,
        reaper_exit_gate: ReaperExitGate,
    ) -> std::io::Result<Arc<Self>> {
        Self::new_inner(maximum, Some(reaper_exit_gate))
    }

    fn new_inner(
        maximum: usize,
        #[cfg(test)] reaper_exit_gate: Option<ReaperExitGate>,
        #[cfg(not(test))] _reaper_exit_gate: Option<()>,
    ) -> std::io::Result<Arc<Self>> {
        let (sender, receiver) = channel();
        let reaper = thread::Builder::new()
            .name("orchestrator-process-reaper".to_owned())
            .spawn(move || {
                run_reaper(receiver);
                #[cfg(test)]
                if let Some(gate) = reaper_exit_gate {
                    let _ = gate.reached.send(());
                    let _ = gate.release.recv();
                }
            })?;
        Ok(Arc::new(Self {
            ownership: Mutex::new(RegistryState::default()),
            maximum,
            unresolved_sender: Some(sender),
            reaper: Some(reaper),
        }))
    }

    fn state(&self) -> MutexGuard<'_, RegistryState> {
        lock_unpoisoned(&self.ownership)
    }

    fn begin_unresolved(self: &Arc<Self>) -> UnresolvedLease {
        lock_unpoisoned(&self.ownership).unresolved += 1;
        UnresolvedLease {
            registry: Arc::clone(self),
        }
    }

    fn handoff(self: &Arc<Self>, mut entry: UnresolvedEntry) {
        entry.unresolved = Some(self.begin_unresolved());
        let Some(sender) = self.unresolved_sender.as_ref() else {
            // Shutdown has closed admission. This path is only reachable after
            // a defensive misuse: retain ownership forever rather than claim
            // that work which no reaper accepted was resolved.
            std::mem::forget(entry);
            return;
        };
        if let Err(error) = sender.send(entry) {
            // The process-wide reaper has no fallible work after startup. If it
            // is nevertheless gone, leaking the entry preserves closed
            // admission and ownership instead of falsely claiming cleanup.
            std::mem::forget(error.0);
        }
    }
}

struct LeaseInner {
    registry: Arc<Registry>,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let mut state = lock_unpoisoned(&self.registry.ownership);
        debug_assert!(state.active != 0, "registry active ownership underflow");
        state.active = state.active.saturating_sub(1);
    }
}

#[derive(Clone)]
struct RegistryLease {
    _inner: Arc<LeaseInner>,
}

impl RegistryLease {
    fn reserve(registry: &Arc<Registry>) -> Result<Self, ProcessError> {
        let mut state = lock_unpoisoned(&registry.ownership);
        if state.unresolved != 0 {
            return Err(ProcessError::Spawn(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "process admission is closed by unresolved ownership",
            )));
        }
        if state.active >= registry.maximum {
            return Err(ProcessError::Spawn(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "owned process-group capacity reached",
            )));
        }
        state.active += 1;
        drop(state);
        Ok(Self {
            _inner: Arc::new(LeaseInner {
                registry: Arc::clone(registry),
            }),
        })
    }
}

struct UnresolvedLease {
    registry: Arc<Registry>,
}

impl Drop for UnresolvedLease {
    fn drop(&mut self) {
        let mut state = lock_unpoisoned(&self.registry.ownership);
        debug_assert!(
            state.unresolved != 0,
            "registry unresolved ownership underflow"
        );
        state.unresolved = state.unresolved.saturating_sub(1);
    }
}

struct UnresolvedEntry {
    pid: Pid,
    leader_observed: bool,
    group_absent: bool,
    child: Option<SharedChild>,
    reap_sender: Option<Sender<()>>,
    waiter: Option<JoinHandle<()>>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    stdin_writer: Option<JoinHandle<()>>,
    _lease: Option<RegistryLease>,
    unresolved: Option<UnresolvedLease>,
    #[cfg(test)]
    reaper_reap_gate: Option<Arc<Mutex<bool>>>,
}

impl UnresolvedEntry {
    fn poll(&mut self) -> bool {
        if !self.leader_observed {
            self.leader_observed = matches!(try_observe_leader_exit(self.pid), Ok(true));
        }
        if !self.group_absent {
            match group_exists(self.pid, self.leader_observed) {
                Ok(true) => {
                    let _ = signal_group(self.pid, Signal::KILL);
                }
                Ok(false) => self.group_absent = true,
                Err(()) => {}
            }
        }

        if self.group_absent {
            if let Some(sender) = self.reap_sender.take() {
                let _ = sender.send(());
            }
            join_if_finished(&mut self.waiter);
            #[cfg(test)]
            let reaper_reap_is_held = self
                .reaper_reap_gate
                .as_ref()
                .is_some_and(|gate| *lock_unpoisoned(gate));
            #[cfg(not(test))]
            let reaper_reap_is_held = false;
            if !reaper_reap_is_held
                && self.waiter.is_none()
                && self
                    .child
                    .as_ref()
                    .is_some_and(retry_reap_after_group_absence)
            {
                self.child.take();
            }
        }
        join_if_finished(&mut self.stdout_reader);
        join_if_finished(&mut self.stderr_reader);
        join_if_finished(&mut self.stdin_writer);
        self.child.is_none()
            && self.waiter.is_none()
            && self.stdout_reader.is_none()
            && self.stderr_reader.is_none()
            && self.stdin_writer.is_none()
            && self.reap_sender.is_none()
            && self.group_absent
    }
}

fn retry_reap_after_group_absence(child: &SharedChild) -> bool {
    let mut child = lock_unpoisoned(child);
    let Some(owned_child) = child.as_mut() else {
        return true;
    };
    loop {
        match owned_child.try_wait() {
            Ok(Some(_)) => {
                *child = None;
                return true;
            }
            Ok(None) => return false,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
}

fn run_reaper(receiver: Receiver<UnresolvedEntry>) {
    let mut entries = Vec::new();
    loop {
        if entries.is_empty() {
            match receiver.recv() {
                Ok(entry) => entries.push(entry),
                Err(_) => return,
            }
        }
        while let Ok(entry) = receiver.try_recv() {
            entries.push(entry);
        }
        let mut index = 0;
        while index < entries.len() {
            if entries[index].poll() {
                entries.swap_remove(index);
            } else {
                index += 1;
            }
        }
        if !entries.is_empty() {
            thread::sleep(REAPER_POLL);
        }
    }
}

fn join_if_finished(handle: &mut Option<JoinHandle<()>>) {
    if handle.as_ref().is_some_and(JoinHandle::is_finished) {
        if let Some(handle) = handle.take() {
            let _ = handle.join();
        }
    }
}

static GLOBAL_REGISTRY: OnceLock<Result<Arc<Registry>, RegistryInitError>> = OnceLock::new();

#[derive(Clone)]
struct RegistryInitError {
    kind: std::io::ErrorKind,
    message: String,
}

fn global_registry() -> Result<&'static Arc<Registry>, ProcessError> {
    match GLOBAL_REGISTRY.get_or_init(|| {
        Registry::new(GLOBAL_MAX_OWNED_GROUPS).map_err(|error| RegistryInitError {
            kind: error.kind(),
            message: error.to_string(),
        })
    }) {
        Ok(registry) => Ok(registry),
        Err(error) => Err(ProcessError::Spawn(std::io::Error::new(
            error.kind,
            error.message.clone(),
        ))),
    }
}

struct SpawnGuard {
    child: Option<Child>,
    adopted_child: Option<SharedChild>,
    pid: Pid,
    registry: Arc<Registry>,
    lease: Option<RegistryLease>,
    waiter: Option<JoinHandle<()>>,
    reap_sender: Option<Sender<()>>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    stdin_writer: Option<JoinHandle<()>>,
    cancellation_registration: Option<CancellationRegistration>,
    #[cfg(test)]
    reaper_reap_gate: Option<Arc<Mutex<bool>>>,
    disarmed: bool,
}

struct WaiterControl {
    waiter: JoinHandle<()>,
    reap_sender: Sender<()>,
    #[cfg(test)]
    reaper_reap_gate: Option<Arc<Mutex<bool>>>,
}

impl SpawnGuard {
    fn new(
        child: Child,
        pid: Pid,
        registry: Arc<Registry>,
        lease: RegistryLease,
        waiter_control: WaiterControl,
        cancellation_registration: Option<CancellationRegistration>,
    ) -> Self {
        Self {
            child: Some(child),
            adopted_child: None,
            pid,
            registry,
            lease: Some(lease),
            waiter: Some(waiter_control.waiter),
            reap_sender: Some(waiter_control.reap_sender),
            stdout_reader: None,
            stderr_reader: None,
            stdin_writer: None,
            cancellation_registration,
            #[cfg(test)]
            reaper_reap_gate: waiter_control.reaper_reap_gate,
            disarmed: false,
        }
    }

    fn take_child(&mut self) -> std::io::Result<Child> {
        self.child
            .take()
            .ok_or_else(|| std::io::Error::other("spawn guard lost direct child"))
    }

    fn verify_process_group(&self) -> std::io::Result<()> {
        match getpgid(Some(self.pid)) {
            Ok(group) if group == self.pid => Ok(()),
            Ok(_) => Err(std::io::Error::other(
                "child did not enter its dedicated process group",
            )),
            Err(rustix::io::Errno::SRCH) => {
                // On Darwin an exited, unreaped group leader is no longer
                // visible to getpgid. Retaining Child still pins its numeric
                // identity, so the reaper can prove group absence before reap.
                Ok(())
            }
            Err(error) => Err(std::io::Error::from(error)),
        }
    }

    fn take_stdout(&mut self) -> std::io::Result<std::process::ChildStdout> {
        self.child
            .as_mut()
            .and_then(|child| child.stdout.take())
            .ok_or_else(|| std::io::Error::other("spawned child has no stdout pipe"))
    }

    fn take_stderr(&mut self) -> std::io::Result<std::process::ChildStderr> {
        self.child
            .as_mut()
            .and_then(|child| child.stderr.take())
            .ok_or_else(|| std::io::Error::other("spawned child has no stderr pipe"))
    }

    fn take_stdin(&mut self) -> std::io::Result<std::process::ChildStdin> {
        self.child
            .as_mut()
            .and_then(|child| child.stdin.take())
            .ok_or_else(|| std::io::Error::other("spawned child has no stdin pipe"))
    }

    fn into_run_guard(mut self) -> RunGuard {
        self.disarmed = true;
        RunGuard {
            pid: self.pid,
            registry: Arc::clone(&self.registry),
            lease: self.lease.take(),
            child: self.adopted_child.take(),
            waiter: self.waiter.take(),
            reap_sender: self.reap_sender.take(),
            stdout_reader: self.stdout_reader.take(),
            stderr_reader: self.stderr_reader.take(),
            stdin_writer: self.stdin_writer.take(),
            cancellation_registration: self.cancellation_registration.take(),
            #[cfg(test)]
            reaper_reap_gate: self.reaper_reap_gate.take(),
            group_absent: false,
            disarmed: false,
        }
    }

    fn finish_pre_release_setup_failure(
        mut self,
        started: Instant,
        stdout_capture: &Arc<Mutex<CaptureState>>,
        stderr_capture: &Arc<Mutex<CaptureState>>,
    ) -> ProcessReport {
        self.cancellation_registration.take();
        let (kill_sent, mut group_control_failed) = match signal_group(self.pid, Signal::KILL) {
            Ok(sent) => (sent, false),
            Err(()) => (false, true),
        };

        if let Some(child) = self.child.as_mut() {
            child.stdin.take();
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        self.child.take();
                        break;
                    }
                    Ok(None) => break,
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                }
            }
        } else if self.adopted_child.is_some() {
            let _ = self.reap_sender.take().map(|sender| sender.send(()));
        }

        join_if_finished(&mut self.waiter);
        join_if_finished(&mut self.stdout_reader);
        join_if_finished(&mut self.stderr_reader);
        join_if_finished(&mut self.stdin_writer);
        let direct_child_reaped = self.child.is_none()
            && self
                .adopted_child
                .as_ref()
                .is_none_or(|child| lock_unpoisoned(child).is_none());
        let group_absent = match group_exists(self.pid, direct_child_reaped) {
            Ok(exists) => !exists,
            Err(()) => {
                group_control_failed = true;
                false
            }
        };
        let cleanup_complete = direct_child_reaped
            && group_absent
            && self.waiter.is_none()
            && self.stdout_reader.is_none()
            && self.stderr_reader.is_none()
            && self.stdin_writer.is_none();

        let stdout = snapshot_capture(stdout_capture);
        let stderr = snapshot_capture(stderr_capture);
        let mut infrastructure_failures = vec![ProcessInfrastructureFailure::LaunchSetup];
        if group_control_failed {
            infrastructure_failures.push(ProcessInfrastructureFailure::GroupControl);
        }
        if !cleanup_complete {
            infrastructure_failures.push(ProcessInfrastructureFailure::CleanupIncomplete);
        }
        let report = ProcessReport {
            termination: if cleanup_complete {
                ProcessTermination::InfrastructureError
            } else {
                ProcessTermination::UnresolvedOwnership
            },
            stdout: stdout.retained,
            stderr: stderr.retained,
            truncated: stdout.discarded_bytes != 0 || stderr.discarded_bytes != 0,
            stdout_discarded_bytes: stdout.discarded_bytes,
            stderr_discarded_bytes: stderr.discarded_bytes,
            elapsed: started.elapsed(),
            spawned: true,
            pid: u32::try_from(self.pid.as_raw_pid()).ok(),
            pgid: u32::try_from(self.pid.as_raw_pid()).ok(),
            kernel_identity: None,
            cancellation_observed: false,
            deadline_observed: false,
            stall_observed: false,
            term_sent: false,
            kill_sent,
            escalated_to_kill: kill_sent,
            direct_child_reaped,
            group_absent,
            cleanup_complete,
            infrastructure_failures,
        };

        if cleanup_complete {
            self.child.take();
            self.adopted_child.take();
            self.reap_sender.take();
            self.lease.take();
            self.disarmed = true;
        } else {
            let entry = self.unresolved_entry();
            self.registry.handoff(entry);
            self.disarmed = true;
        }
        report
    }

    fn unresolved_entry(&mut self) -> UnresolvedEntry {
        let child = self.adopted_child.take().or_else(|| {
            self.child
                .take()
                .map(|child| Arc::new(Mutex::new(Some(child))))
        });
        UnresolvedEntry {
            pid: self.pid,
            leader_observed: false,
            group_absent: false,
            child,
            reap_sender: self.reap_sender.take(),
            waiter: self.waiter.take(),
            stdout_reader: self.stdout_reader.take(),
            stderr_reader: self.stderr_reader.take(),
            stdin_writer: self.stdin_writer.take(),
            _lease: self.lease.take(),
            unresolved: None,
            #[cfg(test)]
            reaper_reap_gate: self.reaper_reap_gate.take(),
        }
    }
}

impl Drop for SpawnGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        self.cancellation_registration.take();
        let _ = signal_group(self.pid, Signal::KILL);
        let entry = self.unresolved_entry();
        self.registry.handoff(entry);
    }
}

struct RunGuard {
    pid: Pid,
    registry: Arc<Registry>,
    lease: Option<RegistryLease>,
    child: Option<SharedChild>,
    waiter: Option<JoinHandle<()>>,
    reap_sender: Option<Sender<()>>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    stdin_writer: Option<JoinHandle<()>>,
    cancellation_registration: Option<CancellationRegistration>,
    #[cfg(test)]
    reaper_reap_gate: Option<Arc<Mutex<bool>>>,
    group_absent: bool,
    disarmed: bool,
}

impl RunGuard {
    fn mark_group_absent(&mut self) {
        self.group_absent = true;
    }

    fn join_finished(&mut self, state: &mut RunState) {
        join_checked(
            &mut self.waiter,
            ProcessInfrastructureFailure::DirectChildWait,
            state,
        );
        join_checked(
            &mut self.stdout_reader,
            ProcessInfrastructureFailure::StdoutRead,
            state,
        );
        join_checked(
            &mut self.stderr_reader,
            ProcessInfrastructureFailure::StderrRead,
            state,
        );
        join_checked(
            &mut self.stdin_writer,
            ProcessInfrastructureFailure::StdinWrite,
            state,
        );
        self.cancellation_registration.take();
        self.reap_sender.take();
        self.child.take();
        self.lease.take();
        self.disarmed = true;
    }

    fn request_reap(&mut self) -> bool {
        self.group_absent = true;
        self.reap_sender
            .take()
            .is_some_and(|sender| sender.send(()).is_ok())
    }

    fn handoff(&mut self, group_absent: bool) {
        self.cancellation_registration.take();
        self.group_absent |= group_absent;
        if !self.group_absent {
            let _ = signal_group(self.pid, Signal::KILL);
        }
        let entry = UnresolvedEntry {
            pid: self.pid,
            leader_observed: false,
            group_absent: self.group_absent,
            child: self.child.take(),
            reap_sender: self.reap_sender.take(),
            waiter: self.waiter.take(),
            stdout_reader: self.stdout_reader.take(),
            stderr_reader: self.stderr_reader.take(),
            stdin_writer: self.stdin_writer.take(),
            _lease: self.lease.take(),
            unresolved: None,
            #[cfg(test)]
            reaper_reap_gate: self.reaper_reap_gate.take(),
        };
        self.registry.handoff(entry);
        self.disarmed = true;
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        if !self.disarmed {
            self.handoff(self.group_absent);
        }
    }
}

fn join_checked(
    handle: &mut Option<JoinHandle<()>>,
    failure: ProcessInfrastructureFailure,
    state: &mut RunState,
) {
    if let Some(handle) = handle.take() {
        if handle.join().is_err() {
            state.record_failure(failure);
        }
    }
}

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn grant_delivery_classification_preserves_public_deadline_and_uncertainty() {
        let cases = [
            (
                gate::GrantDelivery::Delivered,
                GrantDeliveryOutcome::Released,
            ),
            (
                gate::GrantDelivery::NotDeliveredDeadline,
                GrantDeliveryOutcome::NotReleased(ProcessNotStartedReason::Deadline),
            ),
            (
                gate::GrantDelivery::NotDeliveredIncompleteGrantDeadline,
                GrantDeliveryOutcome::NotReleased(ProcessNotStartedReason::Deadline),
            ),
            (
                gate::GrantDelivery::NotDeliveredSocketIdentity,
                GrantDeliveryOutcome::NotReleased(ProcessNotStartedReason::GateProtocol),
            ),
            (
                gate::GrantDelivery::NotDeliveredTimeoutConfiguration,
                GrantDeliveryOutcome::NotReleased(ProcessNotStartedReason::GateProtocol),
            ),
            (
                gate::GrantDelivery::NotDeliveredIncompleteGrant,
                GrantDeliveryOutcome::NotReleased(ProcessNotStartedReason::GateProtocol),
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementDeadline,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementReadConfiguration,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementRead,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementMismatch,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
        ];

        for (delivery, expected) in cases {
            assert_eq!(classify_grant_delivery(delivery), expected);
        }
    }

    #[test]
    fn final_start_delivery_is_never_classified_as_proven_not_started() {
        let cases = [
            (
                gate::GrantDelivery::Delivered,
                GrantDeliveryOutcome::Released,
            ),
            (
                gate::GrantDelivery::NotDeliveredSocketIdentity,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::NotDeliveredDeadline,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::NotDeliveredTimeoutConfiguration,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::NotDeliveredIncompleteGrant,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::NotDeliveredIncompleteGrantDeadline,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementDeadline,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementReadConfiguration,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementRead,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
            (
                gate::GrantDelivery::AmbiguousAcknowledgementMismatch,
                GrantDeliveryOutcome::ReleaseUncertain,
            ),
        ];

        for (delivery, expected) in cases {
            assert_eq!(classify_start_delivery(delivery), expected);
        }
    }

    #[test]
    fn cancellation_before_started_ack_prevents_final_start_permit() {
        let queue = Arc::new(EventQueue::default());
        let cancellation = CancellationToken::new();
        let arbitration = ProcessStartedObservationArbitration::new(
            &queue,
            Some(&cancellation),
            Instant::now() + Duration::from_secs(1),
        );
        assert!(cancellation.cancel());

        assert!(
            arbitration
                .actor_decide(ProcessStartedObservationDecision::Persisted)
                .is_err()
        );
        assert!(arbitration.begin_start().is_none());
    }

    #[test]
    fn deadline_before_started_ack_prevents_final_start_permit() {
        let queue = Arc::new(EventQueue::default());
        let arbitration = ProcessStartedObservationArbitration::new(&queue, None, Instant::now());

        assert!(
            arbitration
                .actor_decide(ProcessStartedObservationDecision::Persisted)
                .is_err()
        );
        assert!(arbitration.begin_start().is_none());
    }

    #[test]
    fn cancellation_after_started_ack_but_before_begin_start_prevents_final_permit() {
        let queue = Arc::new(EventQueue::default());
        let cancellation = CancellationToken::new();
        let arbitration = ProcessStartedObservationArbitration::new(
            &queue,
            Some(&cancellation),
            Instant::now() + Duration::from_secs(1),
        );
        assert!(
            arbitration
                .actor_decide(ProcessStartedObservationDecision::Persisted)
                .is_ok()
        );

        assert!(cancellation.cancel());
        assert!(arbitration.begin_start().is_none());
    }

    #[test]
    fn deadline_after_started_ack_but_before_begin_start_prevents_final_permit() {
        let queue = Arc::new(EventQueue::default());
        let acknowledged_at = Instant::now();
        let hard_deadline = acknowledged_at + Duration::from_secs(1);
        let arbitration = ProcessStartedObservationArbitration::new(&queue, None, hard_deadline);
        assert!(
            arbitration
                .actor_decide(ProcessStartedObservationDecision::Persisted)
                .is_ok()
        );

        assert!(arbitration.begin_start_at(hard_deadline).is_none());
    }

    #[test]
    fn persisted_started_ack_mints_exactly_one_final_start_permit()
    -> Result<(), Box<dyn std::error::Error>> {
        let queue = Arc::new(EventQueue::default());
        let cancellation = CancellationToken::new();
        let arbitration = ProcessStartedObservationArbitration::new(
            &queue,
            Some(&cancellation),
            Instant::now() + Duration::from_secs(1),
        );
        arbitration
            .actor_decide(ProcessStartedObservationDecision::Persisted)
            .map_err(|_| std::io::Error::other("fresh durable acknowledgement was rejected"))?;

        let permit = arbitration
            .begin_start()
            .ok_or_else(|| std::io::Error::other("durable acknowledgement minted no permit"))?;
        assert!(arbitration.begin_start().is_none());
        assert!(cancellation.cancel());
        permit.arbitration.finish_start();
        Ok(())
    }

    #[test]
    fn dropped_started_observation_fails_closed_without_final_start_permit()
    -> Result<(), Box<dyn std::error::Error>> {
        let queue = Arc::new(EventQueue::default());
        let arbitration = ProcessStartedObservationArbitration::new(
            &queue,
            None,
            Instant::now() + Duration::from_secs(1),
        );
        let pid = std::process::id();
        let process_group_id = u32::try_from(getpgid(None)?.as_raw_pid())?;
        let receipt = ProcessStartedReceipt {
            request_binding: ProcessRequestBinding::from_bytes([0x3c; 32]),
            identity: KernelProcessIdentity::observe(pid, process_group_id)?,
        };
        drop(ProcessStartedObservation {
            receipt,
            reply: Some(ProcessStartedObservationReply {
                arbitration: Arc::clone(&arbitration),
            }),
        });

        assert!(arbitration.begin_start().is_none());
        Ok(())
    }

    #[test]
    fn process_request_binding_matches_the_exact_fingerprint() {
        let bytes = [0xa5; 32];
        let binding = ProcessRequestBinding::from_bytes(bytes);

        assert!(binding.matches_bytes(&bytes));
    }

    #[test]
    fn process_request_binding_rejects_a_fingerprint_substitution() {
        let binding = ProcessRequestBinding::from_bytes([0xa5; 32]);

        assert!(!binding.matches_bytes(&[0x5a; 32]));
    }

    #[test]
    fn process_request_binding_debug_output_is_redacted() {
        let binding = ProcessRequestBinding::from_bytes([0xa5; 32]);

        assert_eq!(format!("{binding:?}"), "ProcessRequestBinding(REDACTED)");
    }

    #[test]
    fn authorized_pre_spawn_failure_retains_only_its_public_classification() {
        let failure = RunFailure::proven_not_started(ProcessError::Spawn(std::io::Error::other(
            "/private/admitted/pre-spawn",
        )))
        .into_authorized_error();

        assert_eq!(
            failure.classification(),
            AuthorizedProcessErrorClassification::ProvenNotStarted
        );
        assert!(std::error::Error::source(&failure).is_none());
        assert_eq!(
            failure.to_string(),
            "authorized process did not produce a process outcome"
        );
        assert!(!format!("{failure:?}").contains("/private/admitted/pre-spawn"));
    }

    #[test]
    fn pre_spawn_command_failure_joins_the_waiter_and_releases_ownership()
    -> Result<(), Box<dyn std::error::Error>> {
        let supervisor = ProcessSupervisor::new(1)?;
        let lease = RegistryLease::reserve(&supervisor.registry)?;
        let waiter_lease = lease.clone();
        let (adopt_sender, adopt_receiver) = channel::<WaitTarget>();
        let (reap_sender, _reap_receiver) = channel::<()>();
        let waiter_blocked = Arc::new(std::sync::Barrier::new(2));
        let waiter_release = Arc::new(std::sync::Barrier::new(2));
        let blocked = Arc::clone(&waiter_blocked);
        let release = Arc::clone(&waiter_release);
        let waiter = thread::spawn(move || {
            assert!(adopt_receiver.recv().is_err());
            blocked.wait();
            release.wait();
            drop(waiter_lease);
        });
        let failure = thread::spawn(move || {
            let failure = pre_spawn_command_failure(
                std::io::Error::other("/private/admitted/command"),
                adopt_sender,
                reap_sender,
                waiter,
            )
            .into_authorized_error();
            drop(lease);
            failure
        });

        waiter_blocked.wait();
        assert!(!failure.is_finished());
        assert!(supervisor.has_owned_processes());
        waiter_release.wait();
        let failure = failure
            .join()
            .map_err(|_| std::io::Error::other("pre-spawn cleanup panicked"))?;

        assert_eq!(
            failure.classification(),
            AuthorizedProcessErrorClassification::ProvenNotStarted
        );
        assert!(std::error::Error::source(&failure).is_none());
        assert!(!format!("{failure:?}").contains("/private/admitted/command"));
        assert!(!supervisor.has_owned_processes());
        Ok(())
    }

    #[test]
    fn post_spawn_failure_maps_conservatively_to_redacted_outcome_loss() {
        let failure = RunFailure::outcome_lost(ProcessError::Spawn(std::io::Error::other(
            "/private/admitted/post-spawn",
        )))
        .into_authorized_error();

        assert_eq!(
            failure.classification(),
            AuthorizedProcessErrorClassification::OutcomeLost
        );
        assert!(std::error::Error::source(&failure).is_none());
        assert!(!format!("{failure:?}").contains("/private/admitted/post-spawn"));
    }

    fn started(outcome: AuthorizedProcessOutcome) -> Result<ProcessReport, ProcessError> {
        outcome.into_started_report().ok_or_else(|| {
            ProcessError::Spawn(std::io::Error::other("production target was not released"))
        })
    }

    fn test_request_binding() -> ProcessRequestBinding {
        ProcessRequestBinding::from_bytes([0x5a; 32])
    }

    fn run_with_durable_gate(
        supervisor: &ProcessSupervisor,
        spec: &ProcessSpec,
        authority: &ProductionProcessLaunchAuthority,
    ) -> Result<AuthorizedProcessOutcome, Box<dyn std::error::Error>> {
        let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
        let actor = thread::spawn(move || {
            let Ok((request, started_authority)) = gate_authority.receive() else {
                return Ok(());
            };
            request.release_authorized()?;
            let started = started_authority.receive_started()?;
            if !started.receipt().matches_request(&[0x5a; 32]) {
                return Err(ProcessStartGateClosed(()));
            }
            started.persisted()
        });
        let outcome = supervisor.run_authorized(spec, authority, &mut gate)?;
        drop(gate);
        actor
            .join()
            .map_err(|_| ProcessError::Spawn(std::io::Error::other("gate actor panicked")))?
            .map_err(|source| {
                ProcessError::Spawn(std::io::Error::new(std::io::ErrorKind::BrokenPipe, source))
            })?;
        Ok(outcome)
    }

    static AUTHORITY_CASE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

    struct AuthorityCase(PathBuf);

    impl Drop for AuthorityCase {
        fn drop(&mut self) {
            if let Ok(path) = std::fs::read(self.0.join("gate-parent-path")) {
                let _ = std::fs::remove_dir_all(PathBuf::from(OsString::from_vec(path)));
            }
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(target_os = "macos")]
    struct GateRootAliasCleanup {
        original: PathBuf,
        retained: PathBuf,
    }

    #[cfg(target_os = "macos")]
    impl Drop for GateRootAliasCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.original);
            let _ = std::fs::remove_dir_all(&self.retained);
        }
    }

    fn authority_case(label: &str) -> std::io::Result<AuthorityCase> {
        let nonce = AUTHORITY_CASE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "orchestrator-process-authority-{}-{nonce}-{label}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("root/bin"))?;
        std::fs::create_dir_all(path.join("root/workspaces/run"))?;
        std::fs::create_dir(path.join("outside"))?;
        Ok(AuthorityCase(path))
    }

    #[cfg(target_os = "macos")]
    const GATE_CRASH_CASE_ENV: &str = "NANIKA_TEST_GATE_CRASH_CASE";
    #[cfg(target_os = "macos")]
    const GATE_CRASH_MODE_ENV: &str = "NANIKA_TEST_GATE_CRASH_MODE";

    #[cfg(target_os = "macos")]
    fn prepare_private_gate_parent(case: &AuthorityCase) -> std::io::Result<PathBuf> {
        let nonce = AUTHORITY_CASE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = PathBuf::from(format!(
            "/private/tmp/nanika-gp-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        write_synced(
            &case.0.join("gate-parent-path"),
            path.as_os_str().as_bytes(),
        )?;
        Ok(path)
    }

    #[cfg(target_os = "macos")]
    fn install_synthetic_gate_authority(
        case: &AuthorityCase,
    ) -> Result<ProductionProcessLaunchAuthority, ProcessError> {
        let mut authority = installed_production_authority(case)?;
        let path = PathBuf::from(OsString::from_vec(
            std::fs::read(case.0.join("gate-parent-path")).map_err(ProcessError::Spawn)?,
        ));
        authority.short_gate_parent =
            ShortGateParent::open_private(&path).map_err(ProcessError::Spawn)?;
        Ok(authority)
    }

    #[cfg(target_os = "macos")]
    fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
        std::fs::write(path, bytes)?;
        File::open(path)?.sync_all()
    }

    #[cfg(target_os = "macos")]
    fn write_owned_record_journal(
        case: &AuthorityCase,
        records: &[ProcessOwnedDirectory],
    ) -> std::io::Result<()> {
        for (index, record) in records.iter().enumerate() {
            write_synced(&case.0.join(format!("owned-{index}")), &record.encode())?;
        }
        File::open(&case.0)?.sync_all()
    }

    #[cfg(target_os = "macos")]
    fn read_owned_record_journal(
        case: &AuthorityCase,
    ) -> Result<Vec<ProcessOwnedDirectory>, ProcessError> {
        (0..3)
            .map(|index| {
                std::fs::read(case.0.join(format!("owned-{index}")))
                    .map_err(ProcessError::Spawn)
                    .and_then(|encoded| ProcessOwnedDirectory::decode(&encoded))
            })
            .collect()
    }

    #[cfg(target_os = "macos")]
    fn recover_synthetic_gate_case(
        case: &AuthorityCase,
        records: &[ProcessOwnedDirectory],
    ) -> Result<usize, ProcessError> {
        let root = File::open(case.0.join("root")).map_err(ProcessError::Spawn)?;
        let root_identity = FileIdentity::of(&root.metadata().map_err(ProcessError::Spawn)?);
        let gate_parent_path = PathBuf::from(OsString::from_vec(
            std::fs::read(case.0.join("gate-parent-path")).map_err(ProcessError::Spawn)?,
        ));
        let gate_parent =
            ShortGateParent::open_private(&gate_parent_path).map_err(ProcessError::Spawn)?;
        recover_macos_owned_directories(&root, root_identity, records, &gate_parent)
            .map_err(ProcessError::Spawn)
    }

    #[cfg(target_os = "macos")]
    fn spawn_gate_crash_helper(case: &AuthorityCase, mode: &str) -> std::io::Result<()> {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "tests::short_gate_root_crash_subprocess_helper",
                "--nocapture",
            ])
            .env(GATE_CRASH_CASE_ENV, &case.0)
            .env(GATE_CRASH_MODE_ENV, mode)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::other(
                "short gate crash helper did not exit successfully",
            ))
        }
    }

    #[cfg(target_os = "macos")]
    fn create_synthetic_broker_stage(
        case: &AuthorityCase,
    ) -> Result<(ProductionProcessLaunchAuthority, BrokerStage), ProcessError> {
        let authority = install_synthetic_gate_authority(case)?;
        let spec = ProcessSpec::new(
            vec![OsString::from("admitted-helper")],
            Duration::from_secs(5),
        )?;
        let stage = BrokerStage::create(&authority, &spec, &[]).map_err(ProcessError::Spawn)?;
        Ok((authority, stage))
    }

    #[cfg(target_os = "macos")]
    fn preserve_crashed_gate_stage(
        authority: ProductionProcessLaunchAuthority,
        stage: BrokerStage,
    ) -> Vec<ProcessOwnedDirectory> {
        let records = authority.owned_directories();
        std::mem::forget(stage);
        std::mem::forget(authority);
        records
    }

    #[cfg(target_os = "macos")]
    fn authority_root_has_launch_clone(case: &AuthorityCase) -> std::io::Result<bool> {
        for entry in std::fs::read_dir(case.0.join("root"))? {
            if entry?
                .file_name()
                .to_string_lossy()
                .starts_with(".orchestrator-launch-")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn system_executable(candidates: &[&str]) -> std::io::Result<PathBuf> {
        candidates
            .iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "fixture binary"))
    }

    fn installed_authority(
        case: &AuthorityCase,
    ) -> Result<FixtureProcessLaunchAuthority, ProcessError> {
        let source =
            system_executable(&["/usr/bin/true", "/bin/true"]).map_err(ProcessError::Spawn)?;
        let image = std::fs::read(&source).map_err(ProcessError::Spawn)?;
        let executable_path = case.0.join("root/bin/helper");
        std::fs::copy(&source, &executable_path).map_err(ProcessError::Spawn)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))
            .map_err(ProcessError::Spawn)?;
        FixtureProcessLaunchAuthority::new(
            File::open(case.0.join("root")).map_err(ProcessError::Spawn)?,
            File::open(&executable_path).map_err(ProcessError::Spawn)?,
            PathBuf::from("bin/helper"),
            File::open(case.0.join("root/workspaces/run")).map_err(ProcessError::Spawn)?,
            PathBuf::from("workspaces/run"),
            &image,
        )
    }

    #[cfg(target_os = "macos")]
    fn installed_production_authority(
        case: &AuthorityCase,
    ) -> Result<ProductionProcessLaunchAuthority, ProcessError> {
        let image = b"#!/bin/sh\nexec /bin/sh \"$@\"\n";
        let executable_path = case.0.join("root/bin/helper");
        std::fs::write(&executable_path, image).map_err(ProcessError::Spawn)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))
            .map_err(ProcessError::Spawn)?;
        ProductionProcessLaunchAuthority::new_disposable_canary(
            File::open(case.0.join("root")).map_err(ProcessError::Spawn)?,
            File::open(&executable_path).map_err(ProcessError::Spawn)?,
            PathBuf::from("bin/helper"),
            File::open(case.0.join("root/workspaces/run")).map_err(ProcessError::Spawn)?,
            PathBuf::from("workspaces/run"),
            image,
        )
    }

    #[test]
    fn fixture_launch_authority_rejects_mismatched_executable_image()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("image-mismatch")?;
        let admitted_source = system_executable(&["/usr/bin/true", "/bin/true"])?;
        let unadmitted_source = system_executable(&["/usr/bin/touch", "/bin/touch"])?;
        let unadmitted_image = std::fs::read(&unadmitted_source)?;
        let executable_path = case.0.join("root/bin/helper");
        std::fs::copy(&admitted_source, &executable_path)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;

        let result = FixtureProcessLaunchAuthority::new(
            File::open(case.0.join("root"))?,
            File::open(&executable_path)?,
            PathBuf::from("bin/helper"),
            File::open(case.0.join("root/workspaces/run"))?,
            PathBuf::from("workspaces/run"),
            &unadmitted_image,
        );

        assert!(matches!(result, Err(ProcessError::Spawn(_))));
        #[cfg(target_os = "macos")]
        assert!(std::fs::read_dir(case.0.join("root"))?.all(|entry| {
            entry
                .map(|entry| {
                    !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".orchestrator-launch-")
                })
                .unwrap_or(false)
        }));
        Ok(())
    }

    #[test]
    fn executable_attestation_preserves_a_242445680_byte_pin_without_an_image() {
        let attestation = ExecutableFileAttestation::new(242_445_680, [7_u8; 32]);

        assert_eq!(attestation.length(), 242_445_680);
        assert_eq!(attestation.sha256(), [7_u8; 32]);
        assert!(validate_attested_executable_length(attestation).is_ok());
        assert!(
            validate_attested_executable_length(ExecutableFileAttestation::new(
                MAX_ATTESTED_EXECUTABLE_BYTES,
                [8_u8; 32],
            ))
            .is_ok()
        );
        for rejected_length in [0, MAX_ATTESTED_EXECUTABLE_BYTES + 1] {
            let result = validate_attested_executable_length(ExecutableFileAttestation::new(
                rejected_length,
                [9_u8; 32],
            ));
            assert!(matches!(
                result,
                Err(ref error) if error.kind() == std::io::ErrorKind::InvalidInput
            ));
        }
    }

    #[test]
    fn external_canary_root_metadata_rejects_a_different_effective_owner()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("external-root-owner")?;
        std::fs::set_permissions(case.0.join("root"), std::fs::Permissions::from_mode(0o700))?;
        let metadata = std::fs::metadata(case.0.join("root"))?;
        let different_owner = if metadata.uid() == u32::MAX {
            0
        } else {
            metadata.uid() + 1
        };

        assert!(!external_canary_root_metadata_is_valid(
            &metadata,
            different_owner
        ));
        Ok(())
    }

    #[test]
    fn retained_attestation_rejects_a_different_open_file_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("attested-source-identity")?;
        let image = b"#!/bin/sh\nexit 0\n";
        let executable_path = case.0.join("root/bin/helper");
        let different_path = case.0.join("root/bin/different");
        std::fs::write(&executable_path, image)?;
        std::fs::write(&different_path, image)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;
        std::fs::set_permissions(&different_path, std::fs::Permissions::from_mode(0o700))?;
        let source = File::open(executable_path)?;
        let different_identity = FileIdentity::of(&File::open(different_path)?.metadata()?);
        let digest: [u8; 32] = Sha256::digest(image).into();
        let attestation = ExecutableFileAttestation::new(u64::try_from(image.len())?, digest);

        let result = verify_executable_file_attestation(
            &source,
            different_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        );

        assert!(result.is_err());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn streamed_sealed_clone_never_exceeds_the_fixed_copy_buffer()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("stream-copy-bound")?;
        let image = vec![b'a'; (EXECUTABLE_STREAM_CHUNK * 3) + 17];
        let executable_path = case.0.join("root/bin/helper");
        std::fs::write(&executable_path, &image)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;
        let source = File::open(&executable_path)?;
        let source_identity = FileIdentity::of(&source.metadata()?);
        let digest: [u8; 32] = Sha256::digest(&image).into();
        let attestation = ExecutableFileAttestation::new(u64::try_from(image.len())?, digest);
        let root = File::open(case.0.join("root"))?;
        let mut largest_chunk = 0_usize;

        let (sealed, _) = SealedExecutable::create_from_attested_file_with_observer(
            &root,
            &source,
            source_identity,
            attestation,
            ProcessOwnedDirectoryKind::SealedExecutable,
            |_, chunk_length| {
                largest_chunk = largest_chunk.max(chunk_length);
                Ok(())
            },
        )?;

        assert_eq!(largest_chunk, EXECUTABLE_STREAM_CHUNK);
        sealed.verify(&root)?;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn broker_staging_failure_after_open_removes_the_reserved_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("broker-staging-open-failure")?;
        let root = File::open(case.0.join("root"))?;
        let root_identity = FileIdentity::of(&root.metadata()?);

        let result = BrokerStagingRoot::create_with_observer(&root, root_identity, || {
            Err(std::io::Error::other(
                "injected failure after broker staging open",
            ))
        });

        assert!(matches!(result, Err(ref error) if error.to_string().contains("injected failure")));
        assert!(std::fs::read_dir(case.0.join("root"))?.all(|entry| {
            entry
                .map(|entry| {
                    !entry
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".orchestrator-staging-")
                })
                .unwrap_or(false)
        }));
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn sealed_executable_cleanup_preserves_a_replacement_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("sealed-cleanup-replacement")?;
        let root_path = case.0.join("root");
        let root = File::open(&root_path)?;
        let sealed = SealedExecutable::create(
            &root,
            b"#!/bin/sh\nexit 0\n",
            ProcessOwnedDirectoryKind::SealedExecutable,
        )?;
        let original = root_path.join(&sealed.directory_name);
        let retained = root_path.join("renamed-sealed-owned-directory");
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700))?;
        std::fs::rename(&original, &retained)?;
        std::fs::set_permissions(&retained, std::fs::Permissions::from_mode(0o500))?;
        std::fs::create_dir(&original)?;

        drop(sealed);

        assert!(original.is_dir());
        assert!(retained.join("executable").is_file());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn broker_staging_cleanup_preserves_a_replacement_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("staging-cleanup-replacement")?;
        let root_path = case.0.join("root");
        let root = File::open(&root_path)?;
        let staging = BrokerStagingRoot::create(&root, FileIdentity::of(&root.metadata()?))?;
        let original = root_path.join(&staging.directory_name);
        let retained = root_path.join("renamed-staging-owned-directory");
        std::fs::rename(&original, &retained)?;
        std::fs::create_dir(&original)?;

        drop(staging);

        assert!(original.is_dir());
        assert!(retained.is_dir());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn broker_stage_failure_after_finalize_removes_the_request_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("broker-stage-finalize-failure")?;
        let authority = installed_production_authority(&case)?;
        let spec = ProcessSpec::new(
            vec![OsString::from("admitted-helper")],
            Duration::from_secs(1),
        )?;
        let staging_path = path_from_fd(&authority.broker_staging.directory)?;
        let mut request_name = None;

        let result = BrokerStage::create_with_observer(&authority, &spec, &[], |_, name| {
            request_name = Some(name.to_owned());
            Err(std::io::Error::other(
                "injected failure after broker request finalize",
            ))
        });

        let request_name = request_name
            .ok_or_else(|| std::io::Error::other("broker request observer did not run"))?;
        assert!(matches!(result, Err(ref error) if error.to_string().contains("injected failure")));
        assert!(!staging_path.join(request_name).exists());
        assert!(
            authority
                .broker_staging
                .request_registry
                .snapshot()
                .is_empty()
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn staging_drop_preserves_a_registered_request_replacement()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("registered-request-replacement")?;
        let authority = installed_production_authority(&case)?;
        let spec = ProcessSpec::new(
            vec![OsString::from("admitted-helper")],
            Duration::from_secs(1),
        )?;
        let stage = BrokerStage::create(&authority, &spec, &[])?;
        let staging_path = path_from_fd(&authority.broker_staging.directory)?;
        let original = staging_path.join(&stage.directory_name);
        let retained = staging_path.join("renamed-owned-broker-request");
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700))?;
        std::fs::rename(&original, &retained)?;
        std::fs::set_permissions(&retained, std::fs::Permissions::from_mode(0o500))?;
        std::fs::create_dir(&original)?;

        drop(stage);
        assert!(original.is_dir());
        drop(authority);

        assert!(original.is_dir());
        assert!(retained.join("request").is_file());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn short_gate_root_failure_after_open_removes_the_reserved_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("short-gate-open-failure")?;
        let parent_path = prepare_private_gate_parent(&case)?;
        let parent = ShortGateParent::open_private(&parent_path)?;
        let mut reserved_name = None;

        let result = ShortGateRoot::create_with_parent_observer(&parent, |phase, _, _, name| {
            if phase == ShortGateRootCreationPhase::Opened {
                reserved_name = Some(name.to_owned());
                return Err(std::io::Error::other(
                    "injected failure after short gate root open",
                ));
            }
            Ok(())
        });

        let reserved_name = reserved_name
            .ok_or_else(|| std::io::Error::other("short gate open observer did not run"))?;
        assert!(matches!(result, Err(ref error) if error.to_string().contains("injected failure")));
        assert!(!parent_path.join(reserved_name).exists());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn short_gate_root_failure_after_finalize_removes_the_reserved_directory()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("short-gate-finalize-failure")?;
        let parent_path = prepare_private_gate_parent(&case)?;
        let parent = ShortGateParent::open_private(&parent_path)?;
        let mut reserved_name = None;

        let result = ShortGateRoot::create_with_parent_observer(&parent, |phase, _, _, name| {
            if phase == ShortGateRootCreationPhase::Finalized {
                reserved_name = Some(name.to_owned());
                return Err(std::io::Error::other(
                    "injected failure after short gate root finalize",
                ));
            }
            Ok(())
        });

        let reserved_name = reserved_name
            .ok_or_else(|| std::io::Error::other("short gate finalize observer did not run"))?;
        assert!(matches!(result, Err(ref error) if error.to_string().contains("injected failure")));
        assert!(!parent_path.join(reserved_name).exists());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn short_gate_root_cleanup_preserves_a_replacement_alias()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("short-gate-replacement")?;
        let parent_path = prepare_private_gate_parent(&case)?;
        let parent = ShortGateParent::open_private(&parent_path)?;
        let mut aliases = None;

        let result = ShortGateRoot::create_with_parent_observer(&parent, |phase, _, _, name| {
            if phase != ShortGateRootCreationPhase::Finalized {
                return Ok(());
            }
            let original = parent_path.join(name);
            let retained = parent_path.join(format!("{name}-retained"));
            std::fs::rename(&original, &retained)?;
            std::fs::create_dir(&original)?;
            aliases = Some((original, retained));
            Err(std::io::Error::other(
                "injected failure after short gate root replacement",
            ))
        });

        let (original, retained) = aliases
            .ok_or_else(|| std::io::Error::other("short gate replacement observer did not run"))?;
        let cleanup = GateRootAliasCleanup {
            original: original.clone(),
            retained: retained.clone(),
        };
        assert!(matches!(result, Err(ref error) if error.to_string().contains("injected failure")));
        assert!(original.is_dir());
        assert!(retained.is_dir());
        drop(cleanup);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn short_gate_root_crash_subprocess_helper() -> Result<(), Box<dyn std::error::Error>> {
        let Some(case_path) = std::env::var_os(GATE_CRASH_CASE_ENV) else {
            return Ok(());
        };
        let mode = std::env::var(GATE_CRASH_MODE_ENV)?;
        let case = AuthorityCase(PathBuf::from(case_path));
        let authority = install_synthetic_gate_authority(&case)?;
        let marker = case.0.join("target-started");
        let spec = ProcessSpec::new(
            vec![
                OsString::from("admitted-helper"),
                OsString::from("-c"),
                OsString::from("printf started > \"$1\""),
                OsString::from("sh"),
                marker.into_os_string(),
            ],
            Duration::from_secs(5),
        )?;

        match mode.as_str() {
            "recorded" => {
                let stage = BrokerStage::create(&authority, &spec, &[])?;
                write_owned_record_journal(&case, &authority.owned_directories())?;
                write_synced(
                    &case.0.join("gate-path"),
                    stage.gate_root.path.as_os_str().as_bytes(),
                )?;
                File::open(&case.0)?.sync_all()?;
                std::process::exit(0);
            }
            "before-record" => {
                let _stage = BrokerStage::create_with_observers(
                    &authority,
                    &spec,
                    &[],
                    |gate_root, _| {
                        write_synced(
                            &case.0.join("gate-path"),
                            gate_root.path.as_os_str().as_bytes(),
                        )?;
                        File::open(&case.0)?.sync_all()?;
                        std::process::exit(0);
                    },
                    |_, _| Ok(()),
                )?;
                unreachable!();
            }
            _ => Err(std::io::Error::other("unknown gate crash helper mode").into()),
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn recorded_gate_root_recovers_after_subprocess_owner_death()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::FileTypeExt;

        let case = authority_case("recorded-gate-owner-death")?;
        let _parent = prepare_private_gate_parent(&case)?;
        spawn_gate_crash_helper(&case, "recorded")?;
        let records = read_owned_record_journal(&case)?;
        let gate_path = PathBuf::from(OsString::from_vec(std::fs::read(case.0.join("gate-path"))?));
        assert!(
            std::fs::symlink_metadata(gate_path.join("gate"))?
                .file_type()
                .is_socket()
        );

        let first = recover_synthetic_gate_case(&case, &records)?;
        let second = recover_synthetic_gate_case(&case, &records)?;

        assert_eq!((first, second), (3, 0));
        assert!(!gate_path.exists());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn crash_before_gate_record_cannot_start_the_target() -> Result<(), Box<dyn std::error::Error>>
    {
        let case = authority_case("gate-before-record-crash")?;
        let _parent = prepare_private_gate_parent(&case)?;

        spawn_gate_crash_helper(&case, "before-record")?;

        let gate_path = PathBuf::from(OsString::from_vec(std::fs::read(case.0.join("gate-path"))?));
        assert!(gate_path.is_dir());
        assert!(!case.0.join("target-started").exists());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gate_recovery_preserves_a_replacement_directory_inode()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::FileTypeExt;

        let case = authority_case("gate-recovery-replacement")?;
        let _parent = prepare_private_gate_parent(&case)?;
        let (authority, stage) = create_synthetic_broker_stage(&case)?;
        let original = stage.gate_root.path.clone();
        let retained = original
            .parent()
            .ok_or_else(|| std::io::Error::other("gate root has no parent"))?
            .join("retained-owned-gate");
        std::fs::rename(&original, &retained)?;
        std::fs::create_dir(&original)?;
        std::fs::set_permissions(&original, std::fs::Permissions::from_mode(0o700))?;
        let records = preserve_crashed_gate_stage(authority, stage);

        let result = recover_synthetic_gate_case(&case, &records);

        assert!(result.is_err());
        assert!(original.is_dir());
        assert!(
            std::fs::symlink_metadata(retained.join("gate"))?
                .file_type()
                .is_socket()
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gate_recovery_preserves_unknown_gate_entries() -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("gate-recovery-unknown-entry")?;
        let _parent = prepare_private_gate_parent(&case)?;
        let (authority, stage) = create_synthetic_broker_stage(&case)?;
        let unknown = stage.gate_root.path.join("unknown");
        std::fs::write(&unknown, b"preserve")?;
        let records = preserve_crashed_gate_stage(authority, stage);

        let result = recover_synthetic_gate_case(&case, &records);

        assert!(result.is_err());
        assert_eq!(std::fs::read(unknown)?, b"preserve");
        assert!(
            records
                .iter()
                .all(|record| case.0.join("root").join(record.name()).is_dir())
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gate_recovery_refuses_malformed_and_oversized_records()
    -> Result<(), Box<dyn std::error::Error>> {
        for (label, image) in [
            ("malformed", b"not-a-gate-record".to_vec()),
            ("oversized", vec![0_u8; MAX_GATE_ROOT_RECORD_BYTES + 1]),
        ] {
            let case = authority_case(&format!("gate-record-{label}"))?;
            let _parent = prepare_private_gate_parent(&case)?;
            let (authority, stage) = create_synthetic_broker_stage(&case)?;
            let record_path = path_from_fd(&stage.directory)?.join(GATE_ROOT_RECORD_NAME);
            std::fs::set_permissions(
                path_from_fd(&stage.directory)?,
                std::fs::Permissions::from_mode(0o700),
            )?;
            std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o600))?;
            write_synced(&record_path, &image)?;
            std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o400))?;
            std::fs::set_permissions(
                path_from_fd(&stage.directory)?,
                std::fs::Permissions::from_mode(0o500),
            )?;
            let gate_path = stage.gate_root.path.clone();
            let records = preserve_crashed_gate_stage(authority, stage);

            let result = recover_synthetic_gate_case(&case, &records);

            assert!(result.is_err(), "accepted {label} gate record");
            assert!(gate_path.join("gate").exists());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gate_recovery_refuses_wrong_record_mode_and_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        for label in ["mode", "identity"] {
            let case = authority_case(&format!("gate-record-wrong-{label}"))?;
            let _parent = prepare_private_gate_parent(&case)?;
            let (authority, stage) = create_synthetic_broker_stage(&case)?;
            let request_path = path_from_fd(&stage.directory)?;
            let record_path = request_path.join(GATE_ROOT_RECORD_NAME);
            if label == "mode" {
                std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o600))?;
            } else {
                let mut ownership = GateRootOwnership::decode(&stage.gate_ownership.image)?;
                ownership.directory_identity.inode =
                    ownership.directory_identity.inode.saturating_add(1);
                std::fs::set_permissions(&request_path, std::fs::Permissions::from_mode(0o700))?;
                std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o600))?;
                write_synced(&record_path, &ownership.encode()?)?;
                std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o400))?;
                std::fs::set_permissions(&request_path, std::fs::Permissions::from_mode(0o500))?;
            }
            let gate_path = stage.gate_root.path.clone();
            let records = preserve_crashed_gate_stage(authority, stage);

            let result = recover_synthetic_gate_case(&case, &records);

            assert!(result.is_err(), "accepted wrong gate record {label}");
            assert!(gate_path.join("gate").exists());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn gate_recovery_refuses_symlink_and_fifo_records_without_blocking()
    -> Result<(), Box<dyn std::error::Error>> {
        for label in ["symlink", "fifo"] {
            let case = authority_case(&format!("gate-record-{label}"))?;
            let _parent = prepare_private_gate_parent(&case)?;
            let (authority, stage) = create_synthetic_broker_stage(&case)?;
            let request_path = path_from_fd(&stage.directory)?;
            let record_path = request_path.join(GATE_ROOT_RECORD_NAME);
            std::fs::set_permissions(&request_path, std::fs::Permissions::from_mode(0o700))?;
            std::fs::remove_file(&record_path)?;
            if label == "symlink" {
                std::os::unix::fs::symlink("request", &record_path)?;
            } else {
                let status = std::process::Command::new("/usr/bin/mkfifo")
                    .arg(&record_path)
                    .status()?;
                if !status.success() {
                    return Err(std::io::Error::other("mkfifo test helper failed").into());
                }
                std::fs::set_permissions(&record_path, std::fs::Permissions::from_mode(0o400))?;
            }
            std::fs::set_permissions(&request_path, std::fs::Permissions::from_mode(0o500))?;
            let gate_path = stage.gate_root.path.clone();
            let records = preserve_crashed_gate_stage(authority, stage);
            let started = Instant::now();

            let result = recover_synthetic_gate_case(&case, &records);

            assert!(result.is_err(), "accepted nonregular gate record {label}");
            assert!(started.elapsed() < Duration::from_secs(1));
            assert!(gate_path.join("gate").exists());
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn successful_canary_removes_gate_record_and_actual_socket_root()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("successful-canary-gate-cleanup")?;
        let gate_parent = prepare_private_gate_parent(&case)?;
        let authority = install_synthetic_gate_authority(&case)?;
        let spec = ProcessSpec::new(
            vec![
                OsString::from("admitted-helper"),
                OsString::from("-c"),
                OsString::from("exit 0"),
            ],
            Duration::from_secs(5),
        )?;
        let supervisor = ProcessSupervisor::new(1)?;

        let report = started(run_with_durable_gate(&supervisor, &spec, &authority)?)?;

        assert!(report.is_success(), "report: {report:?}");
        assert!(std::fs::read_dir(gate_parent)?.next().is_none());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn external_source_mutation_during_streaming_removes_the_partial_clone()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::FileExt;

        let case = authority_case("stream-source-mutation")?;
        let image = vec![b'a'; EXECUTABLE_STREAM_CHUNK * 2];
        let executable_path = case.0.join("root/bin/helper");
        std::fs::write(&executable_path, &image)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;
        let source = File::open(&executable_path)?;
        let source_identity = FileIdentity::of(&source.metadata()?);
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&executable_path)?;
        let digest: [u8; 32] = Sha256::digest(&image).into();
        let attestation = ExecutableFileAttestation::new(u64::try_from(image.len())?, digest);
        let pre_copy_metadata = verify_executable_file_attestation(
            &source,
            source_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        )?;
        let root = File::open(case.0.join("root"))?;
        let mut mutated = false;

        let result = seal_external_attested_executable_with_observers(
            &root,
            &source,
            source_identity,
            attestation,
            pre_copy_metadata,
            |offset, _| {
                if !mutated {
                    writer.write_at(b"b", offset)?;
                    writer.sync_all()?;
                    mutated = true;
                }
                Ok(())
            },
            || Ok(()),
        );

        assert!(mutated);
        assert!(result.is_err());
        assert!(!authority_root_has_launch_clone(&case)?);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn external_source_truncation_during_streaming_removes_the_partial_clone()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("external-stream-source-truncation")?;
        let image = vec![b'a'; EXECUTABLE_STREAM_CHUNK * 2];
        let executable_path = case.0.join("root/bin/helper");
        std::fs::write(&executable_path, &image)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;
        let source = File::open(&executable_path)?;
        let source_identity = FileIdentity::of(&source.metadata()?);
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&executable_path)?;
        let digest: [u8; 32] = Sha256::digest(&image).into();
        let attestation = ExecutableFileAttestation::new(u64::try_from(image.len())?, digest);
        let pre_copy_metadata = verify_executable_file_attestation(
            &source,
            source_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        )?;
        let root = File::open(case.0.join("root"))?;
        let truncated_length = u64::try_from(EXECUTABLE_STREAM_CHUNK / 2)?;
        let mut truncated = false;

        let result = seal_external_attested_executable_with_observers(
            &root,
            &source,
            source_identity,
            attestation,
            pre_copy_metadata,
            |_, _| {
                if !truncated {
                    writer.set_len(truncated_length)?;
                    writer.sync_all()?;
                    truncated = true;
                }
                Ok(())
            },
            || Ok(()),
        );

        assert!(truncated);
        assert!(result.is_err());
        assert!(!authority_root_has_launch_clone(&case)?);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn external_source_mode_change_during_streaming_removes_the_partial_clone()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("external-stream-source-mode")?;
        let image = vec![b'a'; EXECUTABLE_STREAM_CHUNK * 2];
        let executable_path = case.0.join("root/bin/helper");
        std::fs::write(&executable_path, &image)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;
        let source = File::open(&executable_path)?;
        let source_identity = FileIdentity::of(&source.metadata()?);
        let digest: [u8; 32] = Sha256::digest(&image).into();
        let attestation = ExecutableFileAttestation::new(u64::try_from(image.len())?, digest);
        let pre_copy_metadata = verify_executable_file_attestation(
            &source,
            source_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        )?;
        let root = File::open(case.0.join("root"))?;
        let mut changed = false;

        let result = seal_external_attested_executable_with_observers(
            &root,
            &source,
            source_identity,
            attestation,
            pre_copy_metadata,
            |_, _| {
                if !changed {
                    std::fs::set_permissions(
                        &executable_path,
                        std::fs::Permissions::from_mode(0o600),
                    )?;
                    changed = true;
                }
                Ok(())
            },
            || Ok(()),
        );

        assert!(changed);
        assert!(result.is_err());
        assert!(!authority_root_has_launch_clone(&case)?);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn external_source_mutation_after_streaming_fails_the_post_copy_digest()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::FileExt;

        let case = authority_case("external-post-copy-source-mutation")?;
        let image = vec![b'a'; EXECUTABLE_STREAM_CHUNK * 2];
        let executable_path = case.0.join("root/bin/helper");
        std::fs::write(&executable_path, &image)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;
        let source = File::open(&executable_path)?;
        let source_identity = FileIdentity::of(&source.metadata()?);
        let writer = std::fs::OpenOptions::new()
            .write(true)
            .open(&executable_path)?;
        let digest: [u8; 32] = Sha256::digest(&image).into();
        let attestation = ExecutableFileAttestation::new(u64::try_from(image.len())?, digest);
        let pre_copy_metadata = verify_executable_file_attestation(
            &source,
            source_identity,
            attestation,
            ExecutableFileMode::RetainedSource,
        )?;
        let root = File::open(case.0.join("root"))?;
        let mut mutated = false;

        let result = seal_external_attested_executable_with_observers(
            &root,
            &source,
            source_identity,
            attestation,
            pre_copy_metadata,
            |_, _| Ok(()),
            || {
                writer.write_at(b"b", 0)?;
                writer.sync_all()?;
                mutated = true;
                Ok(())
            },
        );

        assert!(mutated);
        assert!(result.is_err());
        assert!(!authority_root_has_launch_clone(&case)?);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn interrupted_streaming_fails_and_removes_the_partial_clone()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("stream-copy-interrupted")?;
        let image = vec![b'a'; EXECUTABLE_STREAM_CHUNK * 2];
        let executable_path = case.0.join("root/bin/helper");
        std::fs::write(&executable_path, &image)?;
        std::fs::set_permissions(&executable_path, std::fs::Permissions::from_mode(0o700))?;
        let source = File::open(&executable_path)?;
        let source_identity = FileIdentity::of(&source.metadata()?);
        let digest: [u8; 32] = Sha256::digest(&image).into();
        let attestation = ExecutableFileAttestation::new(u64::try_from(image.len())?, digest);
        let root = File::open(case.0.join("root"))?;

        let result = SealedExecutable::create_from_attested_file_with_observer(
            &root,
            &source,
            source_identity,
            attestation,
            ProcessOwnedDirectoryKind::SealedExecutable,
            |_, _| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Interrupted,
                    "injected stream interruption",
                ))
            },
        );

        let Err(error) = result else {
            return Err(
                std::io::Error::other("interrupted sealed copy unexpectedly succeeded").into(),
            );
        };
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(!authority_root_has_launch_clone(&case)?);
        Ok(())
    }

    #[test]
    fn authorized_launch_rejects_interlayer_executable_substitution_before_spawn()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("executable-swap")?;
        let authority = installed_authority(&case)?;
        let helper = case.0.join("root/bin/helper");
        std::fs::rename(&helper, case.0.join("held-helper"))?;
        let replacement = system_executable(&["/usr/bin/touch", "/bin/touch"])?;
        std::fs::copy(replacement, &helper)?;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o700))?;
        let marker = case.0.join("unadmitted-helper-ran");
        let spec = ProcessSpec::new(
            vec![
                OsString::from("fixture-executable"),
                marker.as_os_str().to_os_string(),
            ],
            Duration::from_secs(1),
        )?;
        let supervisor = ProcessSupervisor::new(1)?;

        assert!(matches!(
            supervisor.run_fixture_authorized(&spec, &authority),
            Err(ProcessError::Spawn(_))
        ));
        assert!(!marker.exists());
        assert!(!supervisor.has_owned_processes());
        Ok(())
    }

    #[test]
    fn authorized_launch_rejects_interlayer_cwd_substitution_before_spawn()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("cwd-swap")?;
        let authority = installed_authority(&case)?;
        let cwd = case.0.join("root/workspaces/run");
        std::fs::rename(&cwd, case.0.join("held-workspace"))?;
        std::fs::rename(case.0.join("outside"), &cwd)?;
        let spec = ProcessSpec::new(
            vec![OsString::from("fixture-executable")],
            Duration::from_secs(1),
        )?;
        let supervisor = ProcessSupervisor::new(1)?;

        assert!(matches!(
            supervisor.run_fixture_authorized(&spec, &authority),
            Err(ProcessError::Spawn(_))
        ));
        assert!(!supervisor.has_owned_processes());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_broker_uses_admitted_cwd_descriptor_after_namespace_swap()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("production-final-cwd-swap")?;
        let authority = installed_production_authority(&case)?;
        let ready = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let swap_ready = Arc::clone(&ready);
        let swap_resume = Arc::clone(&resume);
        let base = case.0.clone();
        let swapper = thread::spawn(move || -> std::io::Result<()> {
            swap_ready.wait();
            std::fs::rename(
                base.join("root/workspaces/run"),
                base.join("held-workspace"),
            )?;
            std::fs::rename(base.join("outside"), base.join("root/workspaces/run"))?;
            swap_resume.wait();
            Ok(())
        });
        let mut spec = ProcessSpec::new(
            vec![
                OsString::from("admitted-helper"),
                OsString::from("-c"),
                OsString::from("pwd"),
            ],
            Duration::from_secs(5),
        )?;
        spec.pre_spawn_barriers = Some((ready, resume));
        let supervisor = ProcessSupervisor::new(1)?;

        let report = started(run_with_durable_gate(&supervisor, &spec, &authority)?)?;
        swapper
            .join()
            .map_err(|_| std::io::Error::other("CWD swapper panicked"))??;

        assert!(report.is_success(), "report: {report:?}");
        assert_eq!(
            report.stdout,
            format!(
                "{}\n",
                std::fs::canonicalize(case.0.join("held-workspace"))?.display()
            )
            .as_bytes()
        );
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_broker_classifies_pre_target_fchdir_failure_as_infrastructure_error()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("production-fchdir-failure")?;
        let authority = installed_production_authority(&case)?;
        let ready = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let chmod_ready = Arc::clone(&ready);
        let chmod_resume = Arc::clone(&resume);
        let cwd = case.0.join("root/workspaces/run");
        let chmod_cwd = cwd.clone();
        let chmodder = thread::spawn(move || -> std::io::Result<()> {
            chmod_ready.wait();
            std::fs::set_permissions(&chmod_cwd, std::fs::Permissions::from_mode(0o000))?;
            chmod_resume.wait();
            Ok(())
        });
        let mut spec = ProcessSpec::new(
            vec![
                OsString::from("admitted-helper"),
                OsString::from("-c"),
                OsString::from("exit 126"),
            ],
            Duration::from_secs(5),
        )?;
        spec.pre_spawn_barriers = Some((ready, resume));
        let supervisor = ProcessSupervisor::new(1)?;

        let result = run_with_durable_gate(&supervisor, &spec, &authority);
        chmodder
            .join()
            .map_err(|_| std::io::Error::other("CWD chmod thread panicked"))??;
        std::fs::set_permissions(&cwd, std::fs::Permissions::from_mode(0o700))?;
        let report = started(result?)?;

        assert_eq!(
            report.termination,
            ProcessTermination::InfrastructureError,
            "report: {report:?}"
        );
        assert!(report.cleanup_complete, "report: {report:?}");
        assert!(!supervisor.has_owned_processes());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_broker_rejects_final_request_and_stdin_substitution()
    -> Result<(), Box<dyn std::error::Error>> {
        let case = authority_case("production-control-swap")?;
        let authority = installed_production_authority(&case)?;
        let marker = case.0.join("unadmitted-target-ran");
        let ready = Arc::new(std::sync::Barrier::new(2));
        let resume = Arc::new(std::sync::Barrier::new(2));
        let swap_ready = Arc::clone(&ready);
        let swap_resume = Arc::clone(&resume);
        let root = case.0.join("root");
        let swapper = thread::spawn(move || -> std::io::Result<()> {
            swap_ready.wait();
            let staging = std::fs::read_dir(&root)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name().is_some_and(|name| {
                        name.to_string_lossy().starts_with(".orchestrator-staging-")
                    })
                })
                .ok_or_else(|| std::io::Error::other("broker staging parent not found"))?;
            let stage = std::fs::read_dir(staging)?
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .find(|path| {
                    path.file_name().is_some_and(|name| {
                        name.to_string_lossy().starts_with(".orchestrator-request-")
                    })
                })
                .ok_or_else(|| std::io::Error::other("broker stage not found"))?;
            std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o700))?;
            std::fs::rename(stage.join("request"), stage.join("held-request"))?;
            std::fs::write(stage.join("request"), b"replacement")?;
            std::fs::set_permissions(
                stage.join("request"),
                std::fs::Permissions::from_mode(0o400),
            )?;
            std::fs::rename(stage.join("stdin"), stage.join("held-stdin"))?;
            std::fs::write(stage.join("stdin"), b"replacement")?;
            std::fs::set_permissions(stage.join("stdin"), std::fs::Permissions::from_mode(0o400))?;
            std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o500))?;
            swap_resume.wait();
            Ok(())
        });
        let mut spec = ProcessSpec::new(
            vec![
                OsString::from("admitted-helper"),
                OsString::from("-c"),
                OsString::from(format!("touch '{}'", marker.display())),
            ],
            Duration::from_secs(5),
        )?
        .with_stdin(b"admitted".to_vec());
        spec.pre_spawn_barriers = Some((ready, resume));
        let supervisor = ProcessSupervisor::new(1)?;

        let (mut gate, gate_authority) = ProcessStartGate::channel(test_request_binding());
        drop(gate_authority);
        let result = supervisor.run_authorized(&spec, &authority, &mut gate);
        swapper
            .join()
            .map_err(|_| std::io::Error::other("control swapper panicked"))??;

        match result? {
            AuthorizedProcessOutcome::NotStarted(receipt) => {
                assert_eq!(receipt.reason, ProcessNotStartedReason::GateProtocol);
            }
            AuthorizedProcessOutcome::Uncertain(receipt) => {
                assert_eq!(
                    receipt.reason,
                    ProcessUncertainReason::CleanupIncomplete(
                        ProcessNotStartedReason::GateProtocol,
                    )
                );
            }
            AuthorizedProcessOutcome::Started(_) => {
                return Err(
                    std::io::Error::other("tampered production setup released the target").into(),
                );
            }
        }
        assert!(!marker.exists());
        let cleanup_deadline = Instant::now() + Duration::from_secs(2);
        while supervisor.has_owned_processes() && Instant::now() < cleanup_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!supervisor.has_owned_processes());
        let retry_spec = ProcessSpec::new(
            vec![OsString::from("admitted-helper")],
            Duration::from_secs(1),
        )?;
        let (mut retry_gate, retry_authority) = ProcessStartGate::channel(test_request_binding());
        drop(retry_authority);
        let retry = supervisor.run_authorized(&retry_spec, &authority, &mut retry_gate);
        assert!(matches!(
            retry,
            Err(error)
                if error.classification()
                    == AuthorizedProcessErrorClassification::ProvenNotStarted
        ));
        Ok(())
    }

    fn shell_argv(script: &str, deadline: Duration) -> Result<ProcessSpec, ProcessError> {
        ProcessSpec::new(
            vec![
                OsString::from("/bin/sh"),
                OsString::from("-c"),
                OsString::from(script),
            ],
            deadline,
        )
    }

    #[test]
    fn rejects_oversized_and_empty_argv() {
        assert!(ProcessSpec::new(Vec::new(), Duration::from_secs(1)).is_err());
        let huge = OsString::from("x".repeat(MAX_ARGUMENT_BYTES + 1));
        assert!(
            ProcessSpec::new(
                vec![OsString::from("/bin/sh"), huge],
                Duration::from_secs(1)
            )
            .is_err()
        );
        assert!(ProcessSpec::new(vec![OsString::from("/bin/sh")], Duration::ZERO).is_err());

        let zero_stall = ProcessSpec::new(vec![OsString::from("/bin/sh")], Duration::from_secs(1))
            .map(|spec| spec.with_stall_timeout(Duration::ZERO));
        assert!(matches!(zero_stall, Ok(spec) if validate_spec(&spec).is_err()));

        let oversized_stall =
            ProcessSpec::new(vec![OsString::from("/bin/sh")], Duration::from_secs(1))
                .map(|spec| spec.with_stall_timeout(MAX_RUN_TIMEOUT + Duration::from_secs(1)));
        assert!(matches!(oversized_stall, Ok(spec) if validate_spec(&spec).is_err()));
    }

    #[test]
    fn captures_success_and_combined_output() -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv("printf out; printf err 1>&2", Duration::from_secs(5))?;
        let report = run(&spec, Path::new("/"))?;
        assert!(report.is_success(), "report: {report:?}");
        assert_eq!(report.stdout, b"out");
        assert_eq!(report.stderr, b"err");
        assert_eq!(report.combined(), b"outerr");
        assert!(!report.truncated);
        Ok(())
    }

    #[test]
    fn live_output_arrives_before_the_child_exits() -> Result<(), Box<dyn std::error::Error>> {
        let (sender, receiver) = output_channel();
        let spec =
            shell_argv("printf ready; sleep 1", Duration::from_secs(5))?.with_output_sender(sender);
        let runner = thread::spawn(move || run(&spec, Path::new("/")));

        let chunk = receiver.recv_timeout(Duration::from_millis(500))?;
        assert_eq!(chunk.bytes, b"ready");

        let report = runner
            .join()
            .map_err(|_| std::io::Error::other("process runner panicked"))??;
        assert_eq!(report.stdout, b"ready");
        Ok(())
    }

    #[test]
    fn live_output_forwards_stdout_and_stderr() -> Result<(), Box<dyn std::error::Error>> {
        let (sender, receiver) = output_channel();
        let spec = shell_argv("printf out; printf err >&2", Duration::from_secs(5))?
            .with_output_sender(sender);
        let report = run(&spec, Path::new("/"))?;
        let first = receiver.recv_timeout(Duration::from_secs(1))?;
        let second = receiver.recv_timeout(Duration::from_secs(1))?;
        let streams = [first.stream, second.stream];

        assert!(streams.contains(&ProcessOutputStream::Stdout));
        assert!(streams.contains(&ProcessOutputStream::Stderr));
        assert_eq!(report.stdout, b"out");
        assert_eq!(report.stderr, b"err");
        Ok(())
    }

    #[test]
    fn unread_live_output_reports_loss_without_changing_capture()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, receiver) = output_channel();
        let spec = shell_argv(
            "dd if=/dev/zero bs=8192 count=64 2>/dev/null",
            Duration::from_secs(5),
        )?
        .with_max_output_bytes(32)
        .with_output_sender(sender);

        let report = run(&spec, Path::new("/"))?;

        assert_eq!(report.termination, ProcessTermination::Exited(0));
        assert_eq!(report.stdout, vec![0; 32]);
        assert_eq!(report.stdout_discarded_bytes, (64 * 8192) - 32);
        assert!(receiver.dropped_bytes() > 0);
        let mut delivered = 0_u64;
        let mut chunks = 0;
        while let Ok(chunk) = receiver.recv_timeout(Duration::ZERO) {
            assert!(chunk.bytes.len() <= READER_CHUNK);
            delivered += u64::try_from(chunk.bytes.len())?;
            chunks += 1;
        }
        assert!(chunks <= 16);
        assert_eq!(delivered + receiver.dropped_bytes(), 64 * 8192);
        Ok(())
    }

    #[test]
    fn dropped_live_output_receiver_does_not_obstruct_cancellation()
    -> Result<(), Box<dyn std::error::Error>> {
        let (sender, receiver) = output_channel();
        drop(receiver);
        let cancellation = CancellationToken::new();
        let spec = shell_argv("while :; do printf x; done", Duration::from_secs(5))?
            .with_term_grace(Duration::from_millis(100))
            .with_cancellation(cancellation.clone())
            .with_output_sender(sender);
        let runner = thread::spawn(move || run(&spec, Path::new("/")));
        thread::sleep(Duration::from_millis(100));
        cancellation.cancel();

        let report = runner
            .join()
            .map_err(|_| std::io::Error::other("process runner panicked"))??;
        assert_eq!(report.termination, ProcessTermination::Cancelled);
        assert!(report.cleanup_complete);
        Ok(())
    }

    #[test]
    fn classifies_nonzero_exit_and_signal() -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv("exit 3", Duration::from_secs(5))?;
        let report = run(&spec, Path::new("/"))?;
        assert_eq!(report.termination, ProcessTermination::Exited(3));

        let spec = shell_argv("kill -9 $$", Duration::from_secs(5))?;
        let report = run(&spec, Path::new("/"))?;
        assert_eq!(report.termination, ProcessTermination::Signaled(9));
        Ok(())
    }

    #[test]
    fn kills_a_process_that_exceeds_its_hard_deadline() -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv("trap '' TERM; sleep 30", Duration::from_millis(400))?
            .with_term_grace(Duration::from_millis(150));
        let started = Instant::now();
        let report = run(&spec, Path::new("/"))?;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "report: {report:?}"
        );
        assert_eq!(
            report.termination,
            ProcessTermination::Timeout,
            "{report:?}"
        );
        assert!(report.kill_sent);
        Ok(())
    }

    #[test]
    fn classifies_a_term_killed_timeout_without_escalation()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv("sleep 30", Duration::from_millis(400))?;
        let report = run(&spec, Path::new("/"))?;
        assert_eq!(
            report.termination,
            ProcessTermination::Timeout,
            "{report:?}"
        );
        assert!(!report.kill_sent);
        Ok(())
    }

    #[test]
    fn returns_promptly_when_a_descendant_outlives_the_leader()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv("sleep 30 &", Duration::from_secs(5))?
            .with_term_grace(Duration::from_millis(100));
        let started = Instant::now();
        let report = run(&spec, Path::new("/"))?;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "report: {report:?}"
        );
        assert_eq!(report.termination, ProcessTermination::Exited(0));
        assert!(report.cleanup_complete);
        Ok(())
    }

    #[test]
    fn counts_only_bytes_beyond_the_output_cap_as_discarded()
    -> Result<(), Box<dyn std::error::Error>> {
        let exact = shell_argv("printf 1234", Duration::from_secs(5))?.with_max_output_bytes(4);
        let report = run(&exact, Path::new("/"))?;
        assert!(!report.truncated, "report: {report:?}");

        let over = shell_argv("printf 123456", Duration::from_secs(5))?.with_max_output_bytes(4);
        let report = run(&over, Path::new("/"))?;
        assert_eq!(report.stdout, b"1234");
        assert_eq!(report.stdout_discarded_bytes, 2);
        assert_eq!(report.termination, ProcessTermination::Exited(0));
        assert!(!report.is_success());
        Ok(())
    }

    #[test]
    fn writes_stdin_concurrently_without_deadlock() -> Result<(), Box<dyn std::error::Error>> {
        let payload = b"hello-stdin\n".repeat(2048);
        let spec = shell_argv("cat", Duration::from_secs(5))?.with_stdin(payload.clone());
        let report = run(&spec, Path::new("/"))?;
        assert!(report.is_success(), "report: {report:?}");
        assert_eq!(report.stdout, payload);
        Ok(())
    }

    #[test]
    fn cancellation_wakes_and_reaps_the_process() -> Result<(), Box<dyn std::error::Error>> {
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let adoption = Arc::new(std::sync::Barrier::new(2));
        let canceller_adoption = Arc::clone(&adoption);
        let canceller = thread::spawn(move || {
            canceller_adoption.wait();
            cancellation.cancel()
        });
        let mut spec = shell_argv("sleep 30", Duration::from_secs(10))?
            .with_cancellation(token)
            .with_term_grace(Duration::from_millis(100));
        spec.post_adoption_barrier = Some(adoption);
        let report = run(&spec, Path::new("/"))?;
        assert!(canceller.join().is_ok());
        assert_eq!(
            report.termination,
            ProcessTermination::Cancelled,
            "{report:?}"
        );
        assert!(report.cancellation_observed);
        assert!(report.spawned);
        assert!(report.cleanup_complete, "report: {report:?}");
        Ok(())
    }

    #[test]
    fn deadline_precedes_late_cancellation_but_records_both()
    -> Result<(), Box<dyn std::error::Error>> {
        let token = CancellationToken::new();
        let cancellation = token.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            cancellation.cancel()
        });
        let spec = shell_argv("trap '' TERM; sleep 30", Duration::from_millis(100))?
            .with_cancellation(token)
            .with_term_grace(Duration::from_millis(200));
        let report = run(&spec, Path::new("/"))?;
        assert!(canceller.join().is_ok());
        assert_eq!(report.termination, ProcessTermination::Timeout);
        assert!(report.deadline_observed);
        assert!(report.cancellation_observed);
        Ok(())
    }

    #[test]
    fn silent_process_stalls_event_driven_and_cleans_group()
    -> Result<(), Box<dyn std::error::Error>> {
        let iterations = Arc::new(Mutex::new(0usize));
        let mut spec = shell_argv("trap '' TERM; sleep 30", Duration::from_secs(5))?
            .with_stall_timeout(Duration::from_millis(120))
            .with_term_grace(Duration::from_millis(50));
        spec.controller_iterations = Some(Arc::clone(&iterations));

        let report = run(&spec, Path::new("/"))?;
        assert_eq!(
            report.termination,
            ProcessTermination::Stalled,
            "{report:?}"
        );
        assert!(report.stall_observed);
        assert!(report.cleanup_complete, "{report:?}");
        assert!(report.kill_sent);
        assert!(
            *lock_unpoisoned(&iterations) < 32,
            "controller iteration count indicates polling"
        );
        Ok(())
    }

    #[test]
    fn periodic_output_activity_prevents_stall() -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv(
            "i=0; while [ \"$i\" -lt 6 ]; do printf x; i=$((i + 1)); sleep 0.06; done",
            Duration::from_secs(3),
        )?
        .with_stall_timeout(Duration::from_millis(250));

        let report = run(&spec, Path::new("/"))?;
        assert!(report.is_success(), "{report:?}");
        assert_eq!(report.stdout, b"xxxxxx");
        assert!(!report.stall_observed);
        Ok(())
    }

    #[test]
    fn delayed_controller_preserves_each_activity_deadline_extension() {
        let queue = EventQueue::default();
        let started = Instant::now();
        let timeout = Duration::from_millis(100);
        queue.arm_stall(Some(timeout), started);

        // Both reads land before the deadline established by the preceding
        // observation. A delayed controller must not see only the newer read:
        // the first extends 100 -> 190 ms, making the 110 ms read valid too.
        queue.push_output_activity(started + Duration::from_millis(90));
        queue.push_output_activity(started + Duration::from_millis(110));

        let snapshot = queue.observe_stall(started + Duration::from_millis(150));
        assert_eq!(
            snapshot.deadline,
            Some(started + Duration::from_millis(210))
        );
        assert_eq!(snapshot.observed_at, None);
        assert!(!snapshot.overflowed);
    }

    #[test]
    fn partial_bounded_output_survives_stall() -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv(
            "printf 123456; trap '' TERM; sleep 30",
            Duration::from_secs(5),
        )?
        .with_stall_timeout(Duration::from_millis(120))
        .with_term_grace(Duration::from_millis(50))
        .with_max_output_bytes(4);

        let report = run(&spec, Path::new("/"))?;
        assert_eq!(
            report.termination,
            ProcessTermination::Stalled,
            "{report:?}"
        );
        assert_eq!(report.stdout, b"1234");
        assert_eq!(report.stdout_discarded_bytes, 2);
        assert!(report.truncated);
        assert!(report.cleanup_complete, "{report:?}");
        Ok(())
    }

    #[test]
    fn terminal_precedence_is_cancel_then_hard_deadline_then_stall_then_exit() {
        let base = Instant::now();
        let terminal_at = base + Duration::from_millis(5);
        let leader_at = base + Duration::from_millis(10);
        let mut state = RunState::new(true);
        state.leader_observed_at = Some(leader_at);
        state.leader_status = Some(ExitStatus::from_raw(0));
        state.leader_reaped = true;
        state.group_absent = true;
        state.stdout_done = true;
        state.stderr_done = true;
        state.stall_observed_at = Some(terminal_at);
        state.cancellation_at = Some(terminal_at);

        assert_eq!(
            state.termination(terminal_at, true),
            ProcessTermination::Cancelled
        );
        state.cancellation_at = None;
        assert_eq!(
            state.termination(terminal_at, true),
            ProcessTermination::Timeout
        );
        assert_eq!(
            state.termination(terminal_at + Duration::from_millis(1), true),
            ProcessTermination::Stalled
        );
        state.stall_observed_at = None;
        assert_eq!(
            state.termination(leader_at + Duration::from_millis(1), true),
            ProcessTermination::Exited(0)
        );
    }

    #[test]
    fn pre_cancelled_token_prevents_spawn() -> Result<(), Box<dyn std::error::Error>> {
        let token = CancellationToken::new();
        assert!(token.cancel());
        let spec = shell_argv("exit 0", Duration::from_secs(5))?.with_cancellation(token);
        let report = run(&spec, Path::new("/"))?;
        assert_eq!(report.termination, ProcessTermination::Cancelled);
        assert!(!report.spawned);
        assert!(report.cleanup_complete);
        Ok(())
    }

    #[test]
    fn absolute_deadline_prevents_relative_budget_extension()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec =
            shell_argv("exit 0", Duration::from_secs(5))?.with_hard_deadline_at(Instant::now());

        let report = run(&spec, Path::new("/"))?;

        assert_eq!(report.termination, ProcessTermination::Timeout);
        assert!(!report.spawned);
        assert!(report.cleanup_complete);
        Ok(())
    }

    #[test]
    fn child_environment_is_clear_except_for_allowlisted_values()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv(
            "printf '%s:%s' \"${SAFE_VALUE-unset}\" \"${HOME-unset}\"",
            Duration::from_secs(5),
        )?
        .with_env("SAFE_VALUE", "allowed");
        let report = run(&spec, Path::new("/"))?;
        assert_eq!(report.stdout, b"allowed:unset");
        Ok(())
    }

    #[test]
    fn explicit_environment_deduplicates_and_cannot_be_weakened_by_inheritance()
    -> Result<(), Box<dyn std::error::Error>> {
        let spec = shell_argv("exit 0", Duration::from_secs(5))?
            .with_env("LANG", "first")
            .with_inherited_env("LANG")
            .with_env("LANG", "last");
        let environment = child_environment(&spec)?;
        let lang = environment
            .iter()
            .filter(|(key, _)| key == "LANG")
            .collect::<Vec<_>>();
        assert_eq!(lang.len(), 1);
        assert_eq!(lang[0].1, "last");
        Ok(())
    }

    #[test]
    fn environment_removal_keys_are_bounded() -> Result<(), Box<dyn std::error::Error>> {
        let mut spec = shell_argv("exit 0", Duration::from_secs(5))?;
        for index in 0..MAX_ENVIRONMENT_ENTRIES {
            spec = spec.with_env_remove(format!("REMOVED_{index}"));
        }
        assert!(matches!(
            validate_spec(&spec),
            Err(ProcessError::InvalidSpec)
        ));
        Ok(())
    }

    #[test]
    fn capacity_registry_rejects_a_second_lease() -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry::new(1)?;
        let first = RegistryLease::reserve(&registry)?;
        let second = RegistryLease::reserve(&registry);
        assert!(
            matches!(second, Err(ProcessError::Spawn(error)) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
        drop(first);
        Ok(())
    }

    #[test]
    fn fresh_idle_supervisor_shuts_down() -> Result<(), Box<dyn std::error::Error>> {
        ProcessSupervisor::new(1)?
            .try_shutdown_idle()
            .map_err(|_| std::io::Error::other("fresh idle shutdown was refused"))?;
        Ok(())
    }

    fn refused_shutdown(
        result: Result<(), ProcessSupervisor>,
        message: &'static str,
    ) -> Result<ProcessSupervisor, std::io::Error> {
        match result {
            Ok(()) => Err(std::io::Error::other(message)),
            Err(supervisor) => Ok(supervisor),
        }
    }

    #[test]
    fn shared_idle_supervisor_refuses_without_closing_admission_then_retries()
    -> Result<(), Box<dyn std::error::Error>> {
        let supervisor = ProcessSupervisor::new(1)?;
        let shared = ProcessSupervisor {
            registry: Arc::clone(&supervisor.registry),
        };

        let supervisor = refused_shutdown(
            supervisor.try_shutdown_idle(),
            "a shared registry accepted shutdown",
        )?;
        let lease = RegistryLease::reserve(&shared.registry)?;
        drop(lease);
        drop(shared);
        supervisor
            .try_shutdown_idle()
            .map_err(|_| std::io::Error::other("unique idle retry was refused"))?;
        Ok(())
    }

    #[test]
    fn active_run_refuses_shutdown_then_succeeds_after_reap()
    -> Result<(), Box<dyn std::error::Error>> {
        let supervisor = ProcessSupervisor::new(1)?;
        let registry = Arc::clone(&supervisor.registry);
        let cancellation = CancellationToken::new();
        let cancel_run = cancellation.clone();
        let adopted = Arc::new(std::sync::Barrier::new(2));
        let mut spec = shell_argv("sleep 30", Duration::from_secs(5))?
            .with_cancellation(cancellation)
            .with_term_grace(Duration::from_millis(100));
        spec.post_adoption_barrier = Some(Arc::clone(&adopted));
        let run = thread::spawn(move || run_inner(&spec, Path::new("/"), &registry));
        adopted.wait();

        let supervisor = refused_shutdown(
            supervisor.try_shutdown_idle(),
            "an active run accepted shutdown",
        )?;
        assert!(supervisor.has_owned_processes());
        cancel_run.cancel();
        let report = run
            .join()
            .map_err(|_| std::io::Error::other("active run panicked"))??;
        assert_eq!(
            report.termination,
            ProcessTermination::Cancelled,
            "{report:?}"
        );
        assert!(report.direct_child_reaped, "{report:?}");
        assert!(report.cleanup_complete, "{report:?}");
        assert!(!supervisor.has_owned_processes());
        supervisor
            .try_shutdown_idle()
            .map_err(|_| std::io::Error::other("shutdown after process reap was refused"))?;
        Ok(())
    }

    #[test]
    fn unresolved_ownership_refuses_shutdown_until_resolved_and_prevents_self_join()
    -> Result<(), Box<dyn std::error::Error>> {
        let supervisor = ProcessSupervisor::new(1)?;
        let unresolved = supervisor.registry.begin_unresolved();

        let supervisor = refused_shutdown(
            supervisor.try_shutdown_idle(),
            "unresolved ownership accepted shutdown",
        )?;
        assert!(Arc::strong_count(&supervisor.registry) > 1);
        assert!(matches!(
            RegistryLease::reserve(&supervisor.registry),
            Err(ProcessError::Spawn(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        drop(unresolved);
        let admitted = RegistryLease::reserve(&supervisor.registry)?;
        drop(admitted);
        supervisor
            .try_shutdown_idle()
            .map_err(|_| std::io::Error::other("shutdown after resolution was refused"))?;
        Ok(())
    }

    #[test]
    fn idle_shutdown_synchronously_joins_its_exact_reaper() -> Result<(), Box<dyn std::error::Error>>
    {
        let (reached_sender, reached_receiver) = channel();
        let (release_sender, release_receiver) = channel();
        let supervisor = ProcessSupervisor {
            registry: Registry::new_with_reaper_exit_gate(
                1,
                ReaperExitGate {
                    reached: reached_sender,
                    release: release_receiver,
                },
            )?,
        };
        let shutdown = thread::spawn(move || supervisor.try_shutdown_idle());

        reached_receiver.recv_timeout(Duration::from_secs(1))?;
        assert!(
            !shutdown.is_finished(),
            "shutdown returned before the retained reaper was released"
        );
        release_sender.send(())?;
        shutdown
            .join()
            .map_err(|_| std::io::Error::other("shutdown probe panicked"))?
            .map_err(|_| std::io::Error::other("idle shutdown was refused"))?;
        Ok(())
    }

    #[test]
    fn process_wide_supervisor_refuses_idle_shutdown() -> Result<(), Box<dyn std::error::Error>> {
        let supervisor = ProcessSupervisor::process_wide()?;
        let supervisor = refused_shutdown(
            supervisor.try_shutdown_idle(),
            "the global registry accepted shutdown",
        )?;
        let lease = RegistryLease::reserve(&supervisor.registry)?;
        drop(lease);
        Ok(())
    }

    #[test]
    fn process_wide_unresolved_ownership_blocks_independent_handles()
    -> Result<(), Box<dyn std::error::Error>> {
        const CHILD_PROBE: &str = "NANIKA_PROCESS_WIDE_UNRESOLVED_CHILD";
        if std::env::var_os(CHILD_PROBE).is_none() {
            let output = std::process::Command::new(std::env::current_exe()?)
                .arg("--exact")
                .arg("tests::process_wide_unresolved_ownership_blocks_independent_handles")
                .arg("--nocapture")
                .env(CHILD_PROBE, "1")
                .output()?;
            assert!(
                output.status.success(),
                "child probe failed: stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return Ok(());
        }

        assert!(!process_wide_has_owned_processes());
        let first = ProcessSupervisor::process_wide()?;
        let second = ProcessSupervisor::process_wide()?;
        let unresolved = first.registry.begin_unresolved();
        assert!(process_wide_has_owned_processes());
        assert!(matches!(
            RegistryLease::reserve(&second.registry),
            Err(ProcessError::Spawn(error))
                if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        drop(unresolved);
        let admitted = RegistryLease::reserve(&second.registry)?;
        drop(admitted);
        assert!(!process_wide_has_owned_processes());
        Ok(())
    }

    #[test]
    fn unresolved_transition_and_release_are_one_admission_state()
    -> Result<(), Box<dyn std::error::Error>> {
        let registry = Registry::new(2)?;
        let previous = registry.begin_unresolved();
        let barrier = Arc::new(std::sync::Barrier::new(3));

        let release_barrier = Arc::clone(&barrier);
        let release = thread::spawn(move || {
            release_barrier.wait();
            drop(previous);
        });

        let replacement_registry = Arc::clone(&registry);
        let replacement_barrier = Arc::clone(&barrier);
        let replacement = thread::spawn(move || {
            replacement_barrier.wait();
            replacement_registry.begin_unresolved()
        });

        barrier.wait();
        assert!(release.join().is_ok());
        let replacement = replacement
            .join()
            .map_err(|_| std::io::Error::other("unresolved transition thread panicked"))?;

        assert_eq!(registry.state().unresolved, 1);
        assert!(matches!(
            RegistryLease::reserve(&registry),
            Err(ProcessError::Spawn(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));

        drop(replacement);
        let admitted = RegistryLease::reserve(&registry)?;
        drop(admitted);
        Ok(())
    }

    #[test]
    fn failed_first_reap_retains_ownership_until_reaper_retries()
    -> Result<(), Box<dyn std::error::Error>> {
        let supervisor = ProcessSupervisor::new(1)?;
        let reaper_gate = Arc::new(Mutex::new(true));
        let mut spec = shell_argv("exit 0", Duration::from_secs(5))?;
        spec.fail_reap_once = true;
        spec.reaper_reap_gate = Some(Arc::clone(&reaper_gate));

        let report = supervisor.run(&spec, Path::new("/"))?;
        assert_eq!(
            report.termination,
            ProcessTermination::InfrastructureError,
            "{report:?}"
        );
        assert!(!report.direct_child_reaped);
        assert!(!report.cleanup_complete);
        assert!(supervisor.has_owned_processes());
        assert!(matches!(
            RegistryLease::reserve(&supervisor.registry),
            Err(ProcessError::Spawn(error)) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        let supervisor = refused_shutdown(
            supervisor.try_shutdown_idle(),
            "reaper-owned unresolved work accepted shutdown",
        )?;

        *lock_unpoisoned(&reaper_gate) = false;
        let retry_deadline = Instant::now() + Duration::from_secs(2);
        while supervisor.has_owned_processes() && Instant::now() < retry_deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(!supervisor.has_owned_processes());
        let admitted = RegistryLease::reserve(&supervisor.registry)?;
        drop(admitted);
        supervisor
            .try_shutdown_idle()
            .map_err(|_| std::io::Error::other("shutdown after reaper resolution was refused"))?;
        Ok(())
    }

    #[test]
    fn hundred_short_processes_release_all_registry_ownership()
    -> Result<(), Box<dyn std::error::Error>> {
        let supervisor = ProcessSupervisor::new(4)?;
        for _ in 0..100 {
            let spec = shell_argv("exit 0", Duration::from_secs(5))?;
            let report = supervisor.run(&spec, Path::new("/"))?;
            assert!(report.is_success(), "report: {report:?}");
        }
        assert!(!supervisor.has_owned_processes());
        Ok(())
    }

    #[test]
    fn env_remove_records_keys_for_the_child() -> Result<(), ProcessError> {
        let spec = ProcessSpec::new(vec![OsString::from("/bin/sh")], Duration::from_secs(1))?
            .with_env_remove("GIT_DIR")
            .with_env_remove("GIT_WORK_TREE");
        assert_eq!(
            spec.env_remove,
            vec![OsString::from("GIT_DIR"), OsString::from("GIT_WORK_TREE")]
        );
        Ok(())
    }
}
