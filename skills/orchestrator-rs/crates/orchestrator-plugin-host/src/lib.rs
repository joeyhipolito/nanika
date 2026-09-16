//! Lazy, bounded host for first-party framed fixture plugins.
//!
//! Production composition is deliberately fail-closed: the reviewed catalog is
//! empty. The public API can consume only an opaque enrollment minted by that
//! catalog; it exposes no path, executable-image, argv, or launch constructor.

#![cfg(unix)]
#![cfg_attr(test, allow(dead_code, clippy::panic))]

use orchestrator_process::{
    CancellationToken, FixtureProcessLaunchAuthority, ProcessError, ProcessReport, ProcessSpec,
    ProcessSupervisor, ProcessTermination,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fmt,
    fs::File,
    io::{self, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
#[cfg(test)]
use std::{
    fs,
    net::SocketAddr,
    path::{Component, Path},
};
use thiserror::Error;

/// The only protocol version accepted by this host slice.
pub const PROTOCOL_VERSION: u16 = 1;
/// Portable binary schema used to bind execution receipts.
pub const RECEIPT_SCHEMA_VERSION: u16 = 1;
const FRAME_HEADER_BYTES: usize = size_of::<u32>();
const RECEIPT_DOMAIN: &[u8] = b"orchestrator-plugin-receipt\0";
/// Absolute ceiling for any wire frame.
pub const HARD_MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Absolute ceiling for a host command queue.
pub const HARD_MAX_QUEUE_DEPTH: usize = 256;
/// Absolute ceiling for concurrent callers.
pub const HARD_MAX_CONCURRENCY: usize = 64;
/// Absolute ceiling for declared resource requests on one request.
pub const HARD_MAX_DECLARED_RESOURCE_REQUESTS: usize = 64;
/// Absolute ceiling for request, child, and idle deadlines.
pub const HARD_MAX_DEADLINE: Duration = Duration::from_secs(24 * 60 * 60);

/// Explicit finite budgets for one host.
#[derive(Clone, Debug)]
pub struct Limits {
    pub max_frame_bytes: usize,
    pub queue_depth: usize,
    pub max_concurrency: usize,
    pub max_output_bytes: usize,
    pub max_declared_resource_requests: usize,
    pub max_declared_resource_units: u64,
    pub request_deadline: Duration,
    pub child_deadline: Duration,
    pub idle_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_frame_bytes: 256 * 1024,
            queue_depth: 16,
            max_concurrency: 8,
            max_output_bytes: 64 * 1024,
            max_declared_resource_requests: 16,
            max_declared_resource_units: 1_000_000,
            request_deadline: Duration::from_secs(2),
            child_deadline: Duration::from_secs(30),
            idle_timeout: Duration::from_millis(250),
        }
    }
}

/// One bounded resource quantity declared by the requestor.
///
/// This is deliberately not named or represented as measured usage.
#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct DeclaredResourceRequest {
    pub name: String,
    pub units: u64,
}

impl fmt::Debug for DeclaredResourceRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeclaredResourceRequest")
            .field("name", &"<redacted>")
            .field("units", &self.units)
            .finish()
    }
}

/// One explicit capability granted by the reviewed enrollment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityGrant {
    pub name: String,
}

/// A protocol request.
#[derive(Clone, Deserialize, Serialize)]
pub struct PluginRequest {
    pub id: u64,
    pub operation: String,
    pub payload: Value,
    #[serde(default)]
    pub declared_resources: Vec<DeclaredResourceRequest>,
}

/// A protocol response.
#[derive(Clone, Deserialize, Serialize)]
pub struct PluginResponse {
    pub id: u64,
    pub result: Value,
}

/// Stable, non-path plugin identity minted by the enrollment authority.
#[derive(Clone, Eq, PartialEq)]
pub struct PluginIdentity([u8; 32]);

impl PluginIdentity {
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex(&self.0)
    }
}

impl fmt::Debug for PluginIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PluginIdentity")
            .field(&self.to_hex())
            .finish()
    }
}

/// Truthful classification of the exchange represented by a receipt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum ReceiptOutcome {
    /// A version-matched response with the requested identity was accepted.
    ResponseAccepted = 1,
    /// A response was received but rejected by the protocol validator.
    ProtocolRejected = 2,
}

/// Receipt binding enrollment, grants, declared requests, measured time, and
/// the exact framed request/response bytes without retaining payload bytes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExecutionReceipt {
    pub receipt_schema_version: u16,
    pub protocol_version: u16,
    pub plugin_identity: PluginIdentity,
    pub activation_id: u64,
    pub request_id: u64,
    pub granted_capabilities: Vec<CapabilityGrant>,
    pub declared_resource_request_count: u32,
    pub declared_resource_request_units: u64,
    pub outcome: ReceiptOutcome,
    pub measured_exchange_duration_ns: u64,
    pub request_frame_bytes: u32,
    pub response_frame_bytes: u32,
    pub receipt_sha256_hex: String,
}

/// Successful response plus its execution receipt.
#[derive(Clone)]
pub struct HostResponse {
    pub response: PluginResponse,
    pub receipt: ExecutionReceipt,
}

impl fmt::Debug for PluginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginRequest")
            .field("id", &self.id)
            .field("operation", &"<redacted>")
            .field("payload", &"<redacted>")
            .field(
                "declared_resource_request_count",
                &self.declared_resources.len(),
            )
            .finish()
    }
}

impl fmt::Debug for PluginResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PluginResponse")
            .field("id", &self.id)
            .field("result", &"<redacted>")
            .finish()
    }
}

impl fmt::Debug for HostResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HostResponse")
            .field("response", &self.response)
            .field("receipt", &self.receipt)
            .finish()
    }
}

/// Read-only lifecycle observations used by gates and diagnostics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HostDiagnostics {
    pub activations: u64,
    pub manager_wakeups: u64,
    pub in_flight: usize,
    pub processing: usize,
    pub queued_requests: usize,
    pub active: bool,
    /// Live manager threads retained by this host (zero or one).
    pub manager_threads: usize,
    /// Live isolated supervisor reaper threads retained by this host.
    pub supervisor_threads: usize,
    /// Live loopback listeners retained by this host (zero or one).
    pub listeners: usize,
    /// Calls currently executing under this host's isolated supervisor.
    pub supervised_runs: usize,
    /// Whether this host observed unresolved ownership while retiring.
    pub unresolved_ownership: bool,
    /// Whether this host has permanently closed new runtime admission.
    pub admission_closed: bool,
    /// Timed accept/idle waits that have elapsed across all generations.
    pub periodic_wakeups: u64,
}

/// Opaque authority for one catalog-enrolled plugin.
///
/// There is intentionally no public constructor and no access to the retained
/// files or executable bytes. Production composition can obtain values only
/// from [`ProductionCatalog::enroll`].
///
/// ```compile_fail
/// use orchestrator_plugin_host::EnrolledPlugin;
/// let forged = EnrolledPlugin { config: unsafe { std::mem::zeroed() } };
/// ```
pub struct EnrolledPlugin {
    config: Arc<AdmittedConfig>,
}

/// Reviewed production plugin catalog.
///
/// B5 composition has not enrolled a plugin yet, so this catalog is empty and
/// every lookup fails before allocating runtime or network resources.
#[derive(Default)]
pub struct ProductionCatalog {
    _private: (),
}

impl ProductionCatalog {
    /// Returns the fail-closed production catalog.
    #[must_use]
    pub fn reviewed_b5() -> Self {
        Self::default()
    }

    /// Resolves a stable plugin name to opaque launch authority.
    pub fn enroll(&self, name: &str, limits: Limits) -> Result<EnrolledPlugin, HostError> {
        validate_limits(&limits)?;
        validate_plugin_name(name)?;
        Err(HostError::NotEnrolled)
    }
}

/// Host admission, protocol, and lifecycle failures.
#[derive(Debug, Error)]
pub enum HostError {
    #[error("plugin enrollment is invalid")]
    InvalidEnrollment,
    #[error("plugin is not enrolled in the reviewed production catalog")]
    NotEnrolled,
    #[error("unsupported plugin protocol version")]
    UnsupportedVersion,
    #[error("plugin request exceeds a configured bound")]
    BoundExceeded,
    #[error("plugin host queue is full")]
    QueueFull,
    #[error("plugin host concurrency limit is reached")]
    ConcurrencyLimit,
    #[error("plugin request deadline expired")]
    Deadline,
    #[error("plugin child deadline expired")]
    ChildDeadline,
    #[error("plugin child exceeded its output budget")]
    OutputLimit,
    #[error("plugin process crashed or disconnected")]
    Crashed,
    #[error("malformed plugin frame")]
    MalformedFrame,
    #[error("plugin host I/O failed during {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("plugin supervisor failed")]
    Supervisor,
    #[error("plugin host manager failed")]
    Manager,
}

fn io_error(operation: &'static str, source: io::Error) -> HostError {
    HostError::Io { operation, source }
}

struct AdmittedConfig {
    root: File,
    executable: File,
    executable_relative: PathBuf,
    cwd: File,
    cwd_relative: PathBuf,
    image: Arc<[u8]>,
    argv_tail: Vec<OsString>,
    plugin_identity: PluginIdentity,
    granted_capabilities: Vec<CapabilityGrant>,
    limits: Limits,
    #[cfg(test)]
    panic_manager_after_spawn: bool,
    #[cfg(test)]
    fail_protocol_setup: bool,
    #[cfg(test)]
    fail_cleanup: bool,
}

struct PreparedRequest {
    request: PluginRequest,
    frame: FrameBytes,
    deadline: Instant,
}

struct FrameBytes {
    exact: Vec<u8>,
}

impl FrameBytes {
    fn body(&self) -> &[u8] {
        &self.exact[FRAME_HEADER_BYTES..]
    }

    fn len_u32(&self) -> u32 {
        u32::try_from(self.exact.len()).unwrap_or(u32::MAX)
    }
}

struct State {
    lifecycle: RuntimeState,
    retained_failure: Option<RetainedRuntime>,
    activations: u64,
    wakeups: u64,
    periodic_wakeups: u64,
    unresolved_ownership: bool,
}

enum RuntimeState {
    Stopped { next_generation: Option<u64> },
    Running { generation: u64, runtime: Runtime },
}

struct RetainedRuntime {
    supervisor: Arc<ProcessSupervisor>,
    telemetry: Arc<RuntimeTelemetry>,
    unresolved_ownership: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            lifecycle: RuntimeState::Stopped {
                next_generation: Some(1),
            },
            retained_failure: None,
            activations: 0,
            wakeups: 0,
            periodic_wakeups: 0,
            unresolved_ownership: false,
        }
    }
}

struct Runtime {
    sender: SyncSender<Command>,
    cancellation: CancellationToken,
    telemetry: Arc<RuntimeTelemetry>,
    join: JoinHandle<ManagerExit>,
}

struct RuntimeTelemetry {
    manager_thread: AtomicBool,
    supervisor_thread: AtomicBool,
    listener: AtomicBool,
    accepting_requests: AtomicBool,
    supervised_runs: AtomicUsize,
    unresolved_ownership: AtomicBool,
    periodic_wakeups: AtomicU64,
}

impl Default for RuntimeTelemetry {
    fn default() -> Self {
        Self {
            manager_thread: AtomicBool::new(false),
            supervisor_thread: AtomicBool::new(false),
            listener: AtomicBool::new(false),
            accepting_requests: AtomicBool::new(true),
            supervised_runs: AtomicUsize::new(0),
            unresolved_ownership: AtomicBool::new(false),
            periodic_wakeups: AtomicU64::new(0),
        }
    }
}

struct ManagerExit {
    wakeups: u64,
    result: Result<(), HostError>,
    retained_supervisor: Option<Arc<ProcessSupervisor>>,
    unresolved_ownership: bool,
}

struct ManagerContext {
    config: Arc<AdmittedConfig>,
    supervisor: Arc<ProcessSupervisor>,
    telemetry: Arc<RuntimeTelemetry>,
    processing: Arc<AtomicUsize>,
    queued_requests: Arc<Mutex<usize>>,
    #[cfg(test)]
    lifecycle_evidence: Arc<Mutex<LifecycleEvidence>>,
}

#[cfg(test)]
#[derive(Default)]
struct LifecycleEvidence {
    last_listener_address: Option<SocketAddr>,
    cleanup_proofs: u64,
    last_child_pid: Option<u32>,
    last_child_pgid: Option<u32>,
}

struct RuntimeHandle {
    generation: u64,
    sender: SyncSender<Command>,
    telemetry: Arc<RuntimeTelemetry>,
}

struct SupervisedProcess {
    cancellation: CancellationToken,
    join: Option<JoinHandle<Result<ProcessReport, ProcessError>>>,
    telemetry: Arc<RuntimeTelemetry>,
    #[cfg(test)]
    lifecycle_evidence: Arc<Mutex<LifecycleEvidence>>,
    #[cfg(test)]
    fail_cleanup: bool,
}

enum Command {
    Request(PreparedRequest, SyncSender<Result<HostResponse, HostError>>),
    Shutdown,
}

/// Lazy host for one admitted first-party plugin executable.
pub struct PluginHost {
    config: Arc<AdmittedConfig>,
    state: Mutex<State>,
    in_flight: AtomicUsize,
    processing: Arc<AtomicUsize>,
    queued_requests: Arc<Mutex<usize>>,
    #[cfg(test)]
    lifecycle_evidence: Arc<Mutex<LifecycleEvidence>>,
}

impl PluginHost {
    /// Builds a lazy host from an opaque catalog enrollment.
    pub fn new(enrollment: EnrolledPlugin) -> Self {
        Self {
            config: enrollment.config,
            state: Mutex::new(State::default()),
            in_flight: AtomicUsize::new(0),
            processing: Arc::new(AtomicUsize::new(0)),
            queued_requests: Arc::new(Mutex::new(0)),
            #[cfg(test)]
            lifecycle_evidence: Arc::new(Mutex::new(LifecycleEvidence::default())),
        }
    }

    /// Sends one bounded request, lazily activating the plugin if necessary.
    pub fn request(&self, request: PluginRequest) -> Result<HostResponse, HostError> {
        let mut request = prepare_request(request, &self.config.limits)?;
        let deadline = request.deadline;
        let _permit = RequestPermit::acquire(&self.in_flight, self.config.limits.max_concurrency)?;
        let (runtime, reply_rx) = loop {
            let runtime = self.ensure_runtime(deadline)?;
            if Instant::now() >= deadline {
                return Err(HostError::Deadline);
            }
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            let mut queued = lock(&self.queued_requests);
            if !runtime.telemetry.accepting_requests.load(Ordering::Acquire) {
                drop(queued);
                if let Some(error) = self.retire_runtime_error(runtime.generation) {
                    return Err(error);
                }
                continue;
            }
            match runtime.sender.try_send(Command::Request(request, reply_tx)) {
                Ok(()) => {
                    *queued = queued.saturating_add(1);
                    drop(queued);
                    break (runtime, reply_rx);
                }
                Err(TrySendError::Full(_)) => return Err(HostError::QueueFull),
                Err(TrySendError::Disconnected(command)) => {
                    drop(queued);
                    let Command::Request(returned_request, _returned_reply) = command else {
                        return Err(HostError::Manager);
                    };
                    if let Some(error) = self.retire_runtime_error(runtime.generation) {
                        return Err(error);
                    }
                    // The idle manager dropped its receiver before admission,
                    // so the operation was not observed and is safe to retry
                    // against a replacement generation under the same deadline.
                    request = returned_request;
                }
            }
        };
        match reply_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(result) => {
                if Instant::now() >= deadline {
                    self.shutdown_generation(runtime.generation);
                    Err(HostError::Deadline)
                } else {
                    result
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.shutdown_generation(runtime.generation);
                Err(HostError::Deadline)
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(self
                .retire_runtime_error(runtime.generation)
                .unwrap_or(HostError::Crashed)),
        }
    }

    /// Cancels and synchronously reaps the active plugin. It is idempotent.
    pub fn shutdown(&self) {
        let mut state = lock(&self.state);
        retire_runtime(&mut state, None, false, true);
        refresh_retained_failure(&mut state);
    }

    /// Returns counters without activating the host.
    pub fn diagnostics(&self) -> HostDiagnostics {
        let mut state = lock(&self.state);
        retire_runtime(&mut state, None, true, false);
        refresh_retained_failure(&mut state);
        let runtime = match &state.lifecycle {
            RuntimeState::Running { runtime, .. } => Some(runtime),
            RuntimeState::Stopped { .. } => None,
        };
        HostDiagnostics {
            activations: state.activations,
            manager_wakeups: state.wakeups,
            in_flight: self.in_flight.load(Ordering::Acquire),
            processing: self.processing.load(Ordering::Acquire),
            queued_requests: *lock(&self.queued_requests),
            active: runtime.is_some(),
            manager_threads: usize::from(
                runtime.is_some_and(|runtime| {
                    runtime.telemetry.manager_thread.load(Ordering::Acquire)
                }) || state.retained_failure.as_ref().is_some_and(|retained| {
                    retained.telemetry.manager_thread.load(Ordering::Acquire)
                }),
            ),
            supervisor_threads: usize::from(
                runtime.is_some_and(|runtime| {
                    runtime.telemetry.supervisor_thread.load(Ordering::Acquire)
                }) || state.retained_failure.as_ref().is_some_and(|retained| {
                    retained.telemetry.supervisor_thread.load(Ordering::Acquire)
                }),
            ),
            listeners: usize::from(
                runtime.is_some_and(|runtime| runtime.telemetry.listener.load(Ordering::Acquire))
                    || state.retained_failure.as_ref().is_some_and(|retained| {
                        retained.telemetry.listener.load(Ordering::Acquire)
                    }),
            ),
            supervised_runs: runtime.map_or(0, |runtime| {
                runtime.telemetry.supervised_runs.load(Ordering::Acquire)
            }) + state.retained_failure.as_ref().map_or(0, |retained| {
                retained.telemetry.supervised_runs.load(Ordering::Acquire)
            }),
            unresolved_ownership: runtime.is_some_and(|runtime| {
                runtime
                    .telemetry
                    .unresolved_ownership
                    .load(Ordering::Acquire)
            }) || state.unresolved_ownership
                || state
                    .retained_failure
                    .as_ref()
                    .is_some_and(|retained| retained.unresolved_ownership),
            admission_closed: matches!(
                state.lifecycle,
                RuntimeState::Stopped {
                    next_generation: None
                }
            ),
            periodic_wakeups: state
                .periodic_wakeups
                .saturating_add(runtime.map_or(0, |runtime| {
                    runtime.telemetry.periodic_wakeups.load(Ordering::Acquire)
                })),
        }
    }

    fn ensure_runtime(&self, deadline: Instant) -> Result<RuntimeHandle, HostError> {
        let mut state = lock(&self.state);
        retire_runtime(&mut state, None, true, false);
        refresh_retained_failure(&mut state);
        if state.retained_failure.is_some() {
            return Err(HostError::Supervisor);
        }
        if Instant::now() >= deadline {
            return Err(HostError::Deadline);
        }
        if let RuntimeState::Running {
            generation,
            runtime,
        } = &state.lifecycle
        {
            return Ok(RuntimeHandle {
                generation: *generation,
                sender: runtime.sender.clone(),
                telemetry: Arc::clone(&runtime.telemetry),
            });
        }
        let generation = match state.lifecycle {
            RuntimeState::Stopped { next_generation } => next_generation,
            RuntimeState::Running { .. } => unreachable!("running runtime returned above"),
        }
        .ok_or(HostError::Supervisor)?;
        let (sender, receiver) = mpsc::sync_channel(self.config.limits.queue_depth);
        let cancellation = CancellationToken::new();
        let manager_cancellation = cancellation.clone();
        let supervisor = Arc::new(ProcessSupervisor::new(1).map_err(|_| HostError::Supervisor)?);
        let telemetry = Arc::new(RuntimeTelemetry::default());
        telemetry.manager_thread.store(true, Ordering::Release);
        telemetry.supervisor_thread.store(true, Ordering::Release);
        let processing = Arc::clone(&self.processing);
        let queued_requests = Arc::clone(&self.queued_requests);
        let config = Arc::clone(&self.config);
        let manager = ManagerContext {
            config,
            supervisor: Arc::clone(&supervisor),
            telemetry: Arc::clone(&telemetry),
            processing,
            queued_requests,
            #[cfg(test)]
            lifecycle_evidence: Arc::clone(&self.lifecycle_evidence),
        };
        let join = match thread::Builder::new()
            .name("orchestrator-plugin-host".into())
            .spawn(move || run_manager_owner(generation, receiver, manager_cancellation, manager))
        {
            Ok(join) => join,
            Err(source) => {
                if let Err(supervisor) = shutdown_retained_supervisor(supervisor) {
                    let unresolved_ownership = supervisor.has_owned_processes();
                    if unresolved_ownership {
                        state.unresolved_ownership = true;
                        state.lifecycle = RuntimeState::Stopped {
                            next_generation: None,
                        };
                    }
                    state.retained_failure = Some(RetainedRuntime {
                        supervisor,
                        telemetry,
                        unresolved_ownership,
                    });
                }
                return Err(io_error("start plugin manager", source));
            }
        };
        drop(supervisor);
        state.activations = state.activations.saturating_add(1);
        state.lifecycle = RuntimeState::Running {
            generation,
            runtime: Runtime {
                sender: sender.clone(),
                cancellation,
                telemetry,
                join,
            },
        };
        let RuntimeState::Running { runtime, .. } = &state.lifecycle else {
            unreachable!("runtime was just installed");
        };
        Ok(RuntimeHandle {
            generation,
            sender,
            telemetry: Arc::clone(&runtime.telemetry),
        })
    }

    fn retire_runtime_error(&self, generation: u64) -> Option<HostError> {
        let mut state = lock(&self.state);
        retire_runtime(&mut state, Some(generation), false, false)
    }

    fn shutdown_generation(&self, generation: u64) {
        let mut state = lock(&self.state);
        retire_runtime(&mut state, Some(generation), false, true);
    }
}

/// Retires one runtime while retaining the lifecycle mutex through `join`.
/// This makes stop/reap/start a single total order and prevents an old caller
/// from taking or cancelling a replacement generation.
fn retire_runtime(
    state: &mut State,
    expected_generation: Option<u64>,
    only_if_finished: bool,
    cancel: bool,
) -> Option<HostError> {
    let (generation, finished) = match &state.lifecycle {
        RuntimeState::Stopped { .. } => return None,
        RuntimeState::Running {
            generation,
            runtime,
        } => (*generation, runtime.join.is_finished()),
    };
    if expected_generation.is_some_and(|expected| expected != generation)
        || only_if_finished && !finished
    {
        return None;
    }

    let next_generation = generation.checked_add(1);
    let RuntimeState::Running { runtime, .. } = std::mem::replace(
        &mut state.lifecycle,
        RuntimeState::Stopped { next_generation },
    ) else {
        unreachable!("running lifecycle was inspected above");
    };
    if cancel {
        runtime.cancellation.cancel();
        // Cancellation wakes the supervisor and closes the peer stream. Never
        // block cleanup behind an already-full request queue.
        let _ = runtime.sender.try_send(Command::Shutdown);
    }
    let join_result = runtime.join.join();
    state.periodic_wakeups = state
        .periodic_wakeups
        .saturating_add(runtime.telemetry.periodic_wakeups.load(Ordering::Acquire));
    match join_result {
        Ok(exit) => {
            state.wakeups = state.wakeups.saturating_add(exit.wakeups);
            if exit.unresolved_ownership {
                state.unresolved_ownership = true;
                state.lifecycle = RuntimeState::Stopped {
                    next_generation: None,
                };
            }
            if let Some(supervisor) = exit.retained_supervisor {
                state.retained_failure = Some(RetainedRuntime {
                    supervisor,
                    telemetry: runtime.telemetry,
                    unresolved_ownership: exit.unresolved_ownership,
                });
            }
            match exit.result {
                Ok(()) if state.retained_failure.is_none() => None,
                Ok(()) => Some(HostError::Supervisor),
                Err(error) => Some(error),
            }
        }
        Err(_) => {
            state.unresolved_ownership = true;
            state.lifecycle = RuntimeState::Stopped {
                next_generation: None,
            };
            Some(HostError::Manager)
        }
    }
}

fn refresh_retained_failure(state: &mut State) {
    let retryable = state.retained_failure.as_ref().is_some_and(|retained| {
        !retained.supervisor.has_owned_processes()
            && !retained.telemetry.manager_thread.load(Ordering::Acquire)
            && !retained.telemetry.listener.load(Ordering::Acquire)
    });
    if retryable {
        if let Some(retained) = state.retained_failure.take() {
            if let Err(supervisor) = shutdown_retained_supervisor(retained.supervisor) {
                state.retained_failure = Some(RetainedRuntime {
                    supervisor,
                    telemetry: retained.telemetry,
                    unresolved_ownership: retained.unresolved_ownership,
                });
            } else {
                retained
                    .telemetry
                    .supervisor_thread
                    .store(false, Ordering::Release);
            }
        }
    }
}

fn shutdown_retained_supervisor(
    supervisor: Arc<ProcessSupervisor>,
) -> Result<(), Arc<ProcessSupervisor>> {
    match Arc::try_unwrap(supervisor) {
        Ok(supervisor) => supervisor.try_shutdown_idle().map_err(Arc::new),
        Err(supervisor) => Err(supervisor),
    }
}

impl Drop for PluginHost {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_manager_owner(
    activation: u64,
    receiver: Receiver<Command>,
    cancellation: CancellationToken,
    manager: ManagerContext,
) -> ManagerExit {
    let telemetry = Arc::clone(&manager.telemetry);
    let supervisor = Arc::clone(&manager.supervisor);
    let owner_cancellation = cancellation.clone();
    telemetry.supervisor_thread.store(true, Ordering::Release);
    let _cancel_on_exit = CancelOnDrop(cancellation.clone());
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_manager(activation, receiver, cancellation, manager)
    }));
    owner_cancellation.cancel();
    telemetry.accepting_requests.store(false, Ordering::Release);

    let unresolved_ownership = outcome.is_err()
        || telemetry.unresolved_ownership.load(Ordering::Acquire)
        || supervisor.has_owned_processes();
    telemetry
        .unresolved_ownership
        .store(unresolved_ownership, Ordering::Release);
    let retained_supervisor = match shutdown_retained_supervisor(supervisor) {
        Ok(()) => {
            telemetry.supervisor_thread.store(false, Ordering::Release);
            None
        }
        Err(supervisor) => Some(supervisor),
    };
    let (wakeups, result) = match outcome {
        Ok(Ok(wakeups)) => (wakeups, Ok(())),
        Ok(Err(error)) => (0, Err(error)),
        Err(_) => (0, Err(HostError::Manager)),
    };
    ManagerExit {
        wakeups,
        result,
        retained_supervisor,
        unresolved_ownership,
    }
}

fn run_manager(
    activation: u64,
    receiver: Receiver<Command>,
    cancellation: CancellationToken,
    manager: ManagerContext,
) -> Result<u64, HostError> {
    let ManagerContext {
        config,
        supervisor,
        telemetry,
        processing,
        queued_requests,
        #[cfg(test)]
        lifecycle_evidence,
    } = manager;
    telemetry.manager_thread.store(true, Ordering::Release);
    let _manager_guard = FlagReset(&telemetry.manager_thread);
    let _queue_reset = QueueReset(Arc::clone(&queued_requests));
    // Mint a one-activation supervisor authority from the exact descriptors
    // retained at enrollment. No namespace path is reopened here.
    let authority = fixture_authority(&config)?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .map_err(|source| io_error("bind plugin loopback listener", source))?;
    telemetry.listener.store(true, Ordering::Release);
    let _listener_guard = FlagReset(&telemetry.listener);
    let address = listener
        .local_addr()
        .map_err(|source| io_error("inspect plugin loopback listener", source))?;
    #[cfg(test)]
    {
        lock(&lifecycle_evidence).last_listener_address = Some(address);
    }
    if !address.ip().is_loopback() {
        return Err(HostError::InvalidEnrollment);
    }
    listener
        .set_nonblocking(true)
        .map_err(|source| io_error("configure plugin listener", source))?;

    let mut secret = [0_u8; 32];
    getrandom::fill(&mut secret).map_err(|source| {
        io_error(
            "generate plugin handshake secret",
            io::Error::other(format!("OS entropy unavailable: {source}")),
        )
    })?;

    let mut argv = vec![
        OsString::from("enrolled-orchestrator-plugin"),
        OsString::from(address.to_string()),
    ];
    argv.extend(config.argv_tail.iter().cloned());
    let spec = ProcessSpec::new(argv, config.limits.child_deadline)
        .map_err(|_| HostError::Supervisor)?
        .with_stdin(secret.to_vec())
        .with_cancellation(cancellation.clone())
        .with_max_output_bytes(config.limits.max_output_bytes)
        .with_term_grace(Duration::from_millis(100))
        .with_cleanup_grace(Duration::from_secs(1));
    let process = thread::Builder::new()
        .name("orchestrator-plugin-child".into())
        .spawn({
            let telemetry = Arc::clone(&telemetry);
            move || {
                telemetry.supervised_runs.fetch_add(1, Ordering::AcqRel);
                let _run_guard = CounterDecrement(&telemetry.supervised_runs);
                supervisor.run_fixture_authorized(&spec, &authority)
            }
        })
        .map_err(|source| io_error("start plugin supervisor", source))?;
    let process = SupervisedProcess::new(
        cancellation.clone(),
        process,
        Arc::clone(&telemetry),
        #[cfg(test)]
        Arc::clone(&lifecycle_evidence),
        #[cfg(test)]
        config.fail_cleanup,
    );

    #[cfg(test)]
    {
        // Scoped to the three lines that need it rather than to the whole
        // manager, so a real `panic!` introduced later is still denied.
        #[allow(clippy::panic, reason = "test-only manager unwind injection")]
        if config.panic_manager_after_spawn {
            panic!("injected manager unwind after child supervision started");
        }
    }

    let activation_deadline = Instant::now()
        .checked_add(config.limits.request_deadline)
        .ok_or(HostError::BoundExceeded)?;
    let mut stream = loop {
        match listener.accept() {
            Ok((mut candidate, peer)) => {
                if !peer.ip().is_loopback() {
                    drop(candidate);
                    continue;
                }
                let remaining = activation_deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    process.cleanup()?;
                    return Err(HostError::Deadline);
                }
                if let Err(source) = candidate.set_nonblocking(false) {
                    let error = io_error("configure plugin handshake stream", source);
                    process.cleanup()?;
                    return Err(error);
                }
                let handshake_deadline = Instant::now()
                    .checked_add(Duration::from_millis(500))
                    .ok_or(HostError::BoundExceeded)?
                    .min(activation_deadline);
                match read_frame(
                    &mut candidate,
                    config.limits.max_frame_bytes,
                    handshake_deadline,
                ) {
                    Ok(WireMessage::Hello {
                        version,
                        secret: supplied,
                    }) if supplied.0 == secret => {
                        if version != PROTOCOL_VERSION {
                            process.cleanup()?;
                            return Err(HostError::UnsupportedVersion);
                        }
                        break candidate;
                    }
                    _ => {
                        // An unauthenticated connector never replaces the owned
                        // child or extends its original activation deadline.
                        drop(candidate);
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if process.is_finished() {
                    process.cleanup()?;
                    return Err(HostError::Crashed);
                }
                if Instant::now() >= activation_deadline {
                    process.cleanup()?;
                    return Err(HostError::Deadline);
                }
                thread::park_timeout(Duration::from_millis(2));
                telemetry.periodic_wakeups.fetch_add(1, Ordering::AcqRel);
            }
            Err(source) => {
                process.cleanup()?;
                return Err(io_error("accept plugin connection", source));
            }
        }
    };
    let mut failed_request = None;
    let protocol_result: Result<u64, HostError> = (|| {
        #[cfg(test)]
        if config.fail_protocol_setup {
            return Err(HostError::MalformedFrame);
        }
        // Accepted streams may inherit nonblocking mode on supported kernels.
        // Framing uses bounded blocking I/O with explicit deadlines.
        stream
            .set_nonblocking(false)
            .map_err(|source| io_error("configure plugin stream", source))?;
        stream
            .set_read_timeout(Some(config.limits.request_deadline))
            .map_err(|source| io_error("set plugin read deadline", source))?;
        stream
            .set_write_timeout(Some(config.limits.request_deadline))
            .map_err(|source| io_error("set plugin write deadline", source))?;
        let mut wakeups = 0_u64;
        loop {
            match receiver.recv_timeout(config.limits.idle_timeout) {
                Ok(Command::Request(request, reply)) => {
                    let mut queued = lock(&queued_requests);
                    *queued = queued.saturating_sub(1);
                    drop(queued);
                    wakeups = wakeups.saturating_add(1);
                    processing.store(1, Ordering::Release);
                    let exchange_result = exchange(&mut stream, activation, request, &config);
                    processing.store(0, Ordering::Release);
                    match exchange_result {
                        Ok(response) => {
                            let _ = reply.send(Ok(response));
                        }
                        Err(error) => {
                            failed_request = Some((reply, error));
                            break;
                        }
                    }
                }
                Ok(Command::Shutdown) => {
                    wakeups = wakeups.saturating_add(1);
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    let queued = lock(&queued_requests);
                    if *queued != 0 {
                        drop(queued);
                        continue;
                    }
                    telemetry.accepting_requests.store(false, Ordering::Release);
                    drop(queued);
                    if !cancellation.is_cancelled() {
                        telemetry.periodic_wakeups.fetch_add(1, Ordering::AcqRel);
                    }
                    break;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    telemetry.accepting_requests.store(false, Ordering::Release);
                    break;
                }
            }
        }
        Ok(wakeups)
    })();
    {
        let _queued = lock(&queued_requests);
        telemetry.accepting_requests.store(false, Ordering::Release);
    }
    let cleanup_result = process.cleanup();
    processing.store(0, Ordering::Release);
    match (protocol_result, cleanup_result) {
        (Err(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(protocol_error), Ok(_)) => Err(protocol_error),
        (Ok(_), Err(error)) => {
            if let Some((reply, _)) = failed_request {
                let _ = reply.send(Err(HostError::Supervisor));
            }
            Err(error)
        }
        (Ok(wakeups), Ok(report)) => {
            if let Some((reply, protocol_error)) = failed_request {
                let error = if report.deadline_observed
                    || report.termination == ProcessTermination::Timeout
                    || report.elapsed >= config.limits.child_deadline
                {
                    HostError::ChildDeadline
                } else if report.truncated
                    || report.stdout_discarded_bytes != 0
                    || report.stderr_discarded_bytes != 0
                    || report.termination == ProcessTermination::OutputLimit
                {
                    HostError::OutputLimit
                } else {
                    protocol_error
                };
                let _ = reply.send(Err(error));
            }
            Ok(wakeups)
        }
    }
}

impl SupervisedProcess {
    fn new(
        cancellation: CancellationToken,
        join: JoinHandle<Result<ProcessReport, ProcessError>>,
        telemetry: Arc<RuntimeTelemetry>,
        #[cfg(test)] lifecycle_evidence: Arc<Mutex<LifecycleEvidence>>,
        #[cfg(test)] fail_cleanup: bool,
    ) -> Self {
        Self {
            cancellation,
            join: Some(join),
            telemetry,
            #[cfg(test)]
            lifecycle_evidence,
            #[cfg(test)]
            fail_cleanup,
        }
    }

    fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn cleanup(mut self) -> Result<ProcessReport, HostError> {
        self.cancellation.cancel();
        let Some(process) = self.join.take() else {
            self.mark_cleanup_unresolved();
            return Err(HostError::Supervisor);
        };
        let report = match process.join() {
            Ok(Ok(report)) => report,
            // `ProcessError` is restricted to validation/setup/spawn failure;
            // once spawn succeeds the supervisor returns a `ProcessReport`.
            // With no admitted child, this error is safely retryable.
            Ok(Err(_)) => return Err(HostError::Supervisor),
            Err(_) => {
                self.mark_cleanup_unresolved();
                return Err(HostError::Supervisor);
            }
        };
        if !cleanup_is_proven(&report) {
            self.mark_cleanup_unresolved();
            return Err(HostError::Supervisor);
        }
        #[cfg(test)]
        {
            let mut evidence = lock(&self.lifecycle_evidence);
            evidence.cleanup_proofs = evidence.cleanup_proofs.saturating_add(1);
            if report.pid.is_some() {
                evidence.last_child_pid = report.pid;
            }
            if report.pgid.is_some() {
                evidence.last_child_pgid = report.pgid;
            }
        }
        #[cfg(test)]
        if self.fail_cleanup {
            self.mark_cleanup_unresolved();
            return Err(HostError::Supervisor);
        }
        Ok(report)
    }

    fn mark_cleanup_unresolved(&self) {
        // A missing join or incomplete child/group cleanup proof remains
        // safety-significant even when a later snapshot observes no process.
        // Preserve that uncertainty so no new ownership domain can be minted.
        self.telemetry
            .unresolved_ownership
            .store(true, Ordering::Release);
    }
}

fn cleanup_is_proven(report: &ProcessReport) -> bool {
    report.cleanup_complete
        && report.group_absent
        && if report.spawned {
            report.direct_child_reaped
        } else {
            report.pid.is_none() && report.pgid.is_none()
        }
}

impl Drop for SupervisedProcess {
    fn drop(&mut self) {
        self.cancellation.cancel();
        if let Some(process) = self.join.take() {
            let _ = process.join();
        }
    }
}

fn fixture_authority(config: &AdmittedConfig) -> Result<FixtureProcessLaunchAuthority, HostError> {
    FixtureProcessLaunchAuthority::new(
        config
            .root
            .try_clone()
            .map_err(|source| io_error("clone retained plugin root", source))?,
        config
            .executable
            .try_clone()
            .map_err(|source| io_error("clone retained plugin executable", source))?,
        config.executable_relative.clone(),
        config
            .cwd
            .try_clone()
            .map_err(|source| io_error("clone retained plugin working directory", source))?,
        config.cwd_relative.clone(),
        &config.image,
    )
    .map_err(|_| HostError::InvalidEnrollment)
}

fn exchange(
    stream: &mut TcpStream,
    activation: u64,
    prepared: PreparedRequest,
    config: &AdmittedConfig,
) -> Result<HostResponse, HostError> {
    let remaining = prepared.deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(HostError::Deadline);
    }
    let PreparedRequest {
        request,
        frame: request_frame,
        deadline,
    } = prepared;
    let measured_from = Instant::now();
    write_raw_frame(stream, &request_frame, deadline)?;
    let response_frame = read_raw_frame(stream, config.limits.max_frame_bytes, deadline)?;
    let wire: WireMessage =
        serde_json::from_slice(response_frame.body()).map_err(|_| HostError::MalformedFrame)?;
    let WireMessage::Response {
        version: PROTOCOL_VERSION,
        response,
    } = wire
    else {
        return Err(HostError::MalformedFrame);
    };
    if response.id != request.id {
        return Err(HostError::MalformedFrame);
    }
    let measured_exchange_duration_ns = duration_ns(measured_from.elapsed());
    let outcome = ReceiptOutcome::ResponseAccepted;
    let declared_resource_request_count =
        u32::try_from(request.declared_resources.len()).unwrap_or(u32::MAX);
    let declared_resource_request_units = request
        .declared_resources
        .iter()
        .fold(0_u64, |total, resource| {
            total.saturating_add(resource.units)
        });
    let receipt_sha256_hex = receipt_digest(ReceiptBinding {
        receipt_schema_version: RECEIPT_SCHEMA_VERSION,
        protocol_version: PROTOCOL_VERSION,
        plugin_identity: &config.plugin_identity,
        activation_id: activation,
        request_id: request.id,
        granted_capabilities: &config.granted_capabilities,
        declared_resource_request_count,
        declared_resource_request_units,
        outcome,
        measured_exchange_duration_ns,
        request_frame: &request_frame,
        response_frame: &response_frame,
    })?;
    Ok(HostResponse {
        response,
        receipt: ExecutionReceipt {
            receipt_schema_version: RECEIPT_SCHEMA_VERSION,
            protocol_version: PROTOCOL_VERSION,
            plugin_identity: config.plugin_identity.clone(),
            activation_id: activation,
            request_id: request.id,
            granted_capabilities: config.granted_capabilities.clone(),
            declared_resource_request_count,
            declared_resource_request_units,
            outcome,
            measured_exchange_duration_ns,
            request_frame_bytes: request_frame.len_u32(),
            response_frame_bytes: response_frame.len_u32(),
            receipt_sha256_hex,
        },
    })
}

#[derive(Clone, Copy)]
struct ReceiptBinding<'a> {
    receipt_schema_version: u16,
    protocol_version: u16,
    plugin_identity: &'a PluginIdentity,
    activation_id: u64,
    request_id: u64,
    granted_capabilities: &'a [CapabilityGrant],
    declared_resource_request_count: u32,
    declared_resource_request_units: u64,
    outcome: ReceiptOutcome,
    measured_exchange_duration_ns: u64,
    request_frame: &'a FrameBytes,
    response_frame: &'a FrameBytes,
}

fn receipt_digest(binding: ReceiptBinding<'_>) -> Result<String, HostError> {
    receipt_digest_with_directions(binding, 1, 2)
}

fn receipt_digest_with_directions(
    binding: ReceiptBinding<'_>,
    request_direction: u8,
    response_direction: u8,
) -> Result<String, HostError> {
    let mut hasher = Sha256::new();
    hasher.update(RECEIPT_DOMAIN);
    hasher.update(binding.receipt_schema_version.to_be_bytes());
    hasher.update(binding.protocol_version.to_be_bytes());
    hasher.update(binding.plugin_identity.0);
    hasher.update(binding.activation_id.to_be_bytes());
    hasher.update(binding.request_id.to_be_bytes());
    hash_count(&mut hasher, binding.granted_capabilities.len())?;
    for grant in binding.granted_capabilities {
        hash_bytes(&mut hasher, grant.name.as_bytes())?;
    }
    hasher.update(binding.declared_resource_request_count.to_be_bytes());
    hasher.update(binding.declared_resource_request_units.to_be_bytes());
    hasher.update([binding.outcome as u8]);
    hasher.update(binding.measured_exchange_duration_ns.to_be_bytes());
    hash_transcript_frame(&mut hasher, request_direction, binding.request_frame);
    hash_transcript_frame(&mut hasher, response_direction, binding.response_frame);
    Ok(hex(&hasher.finalize()))
}

fn hash_count(hasher: &mut Sha256, count: usize) -> Result<(), HostError> {
    hasher.update(
        u32::try_from(count)
            .map_err(|_| HostError::BoundExceeded)?
            .to_be_bytes(),
    );
    Ok(())
}

fn hash_bytes(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), HostError> {
    hasher.update(
        u32::try_from(bytes.len())
            .map_err(|_| HostError::BoundExceeded)?
            .to_be_bytes(),
    );
    hasher.update(bytes);
    Ok(())
}

fn hash_transcript_frame(hasher: &mut Sha256, direction: u8, frame: &FrameBytes) {
    hasher.update([direction]);
    hasher.update(frame.len_u32().to_be_bytes());
    hasher.update(&frame.exact);
}

fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(char::from_digit(u32::from(*byte >> 4), 16).unwrap_or('0'));
        encoded.push(char::from_digit(u32::from(*byte & 0x0f), 16).unwrap_or('0'));
    }
    encoded
}

/// Protocol envelope shared only with first-party fixture executables.
#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WireMessage {
    Hello {
        version: u16,
        secret: HandshakeSecret,
    },
    Request {
        version: u16,
        request: PluginRequest,
    },
    Response {
        version: u16,
        response: PluginResponse,
    },
}

/// Fixed-size secret that is serializable only as the required handshake
/// field. It deliberately implements no `Debug` or display formatting.
#[derive(Deserialize, Serialize)]
struct HandshakeSecret([u8; 32]);

/// Constructs one exact big-endian length-prefixed JSON frame.
fn encode_frame(value: &WireMessage, maximum: usize) -> Result<FrameBytes, HostError> {
    let body = serde_json::to_vec(value).map_err(|_| HostError::MalformedFrame)?;
    let exact_length = FRAME_HEADER_BYTES
        .checked_add(body.len())
        .ok_or(HostError::BoundExceeded)?;
    if body.is_empty() || exact_length > maximum || exact_length > HARD_MAX_FRAME_BYTES {
        return Err(HostError::BoundExceeded);
    }
    let length = u32::try_from(body.len()).map_err(|_| HostError::BoundExceeded)?;
    let mut exact = Vec::with_capacity(exact_length);
    exact.extend_from_slice(&length.to_be_bytes());
    exact.extend_from_slice(&body);
    Ok(FrameBytes { exact })
}

/// Writes the exact frame bytes that were constructed and will be receipted.
fn write_raw_frame(
    stream: &mut TcpStream,
    frame: &FrameBytes,
    deadline: Instant,
) -> Result<(), HostError> {
    write_all_until(stream, &frame.exact, deadline)
}

fn read_frame(
    stream: &mut TcpStream,
    maximum: usize,
    deadline: Instant,
) -> Result<WireMessage, HostError> {
    let frame = read_raw_frame(stream, maximum, deadline)?;
    serde_json::from_slice(frame.body()).map_err(|_| HostError::MalformedFrame)
}

fn read_raw_frame(
    stream: &mut TcpStream,
    maximum: usize,
    deadline: Instant,
) -> Result<FrameBytes, HostError> {
    let mut header = [0_u8; 4];
    read_exact_until(stream, &mut header, deadline, "read plugin frame header")?;
    let length =
        usize::try_from(u32::from_be_bytes(header)).map_err(|_| HostError::BoundExceeded)?;
    let exact_length = FRAME_HEADER_BYTES
        .checked_add(length)
        .ok_or(HostError::BoundExceeded)?;
    if length == 0 || exact_length > maximum || exact_length > HARD_MAX_FRAME_BYTES {
        return Err(HostError::BoundExceeded);
    }
    let mut exact = Vec::with_capacity(exact_length);
    exact.extend_from_slice(&header);
    exact.resize(exact_length, 0);
    read_exact_until(
        stream,
        &mut exact[FRAME_HEADER_BYTES..],
        deadline,
        "read plugin frame body",
    )?;
    Ok(FrameBytes { exact })
}

fn write_all_until(
    stream: &mut TcpStream,
    mut bytes: &[u8],
    deadline: Instant,
) -> Result<(), HostError> {
    while !bytes.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HostError::Deadline);
        }
        stream
            .set_write_timeout(Some(remaining))
            .map_err(|source| io_error("set plugin request write deadline", source))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(io_error(
                    "write plugin frame",
                    io::Error::from(io::ErrorKind::WriteZero),
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(HostError::Deadline);
            }
            Err(error) => return Err(io_error("write plugin frame", error)),
        }
    }
    if Instant::now() >= deadline {
        Err(HostError::Deadline)
    } else {
        Ok(())
    }
}

fn read_exact_until(
    stream: &mut TcpStream,
    mut bytes: &mut [u8],
    deadline: Instant,
    operation: &'static str,
) -> Result<(), HostError> {
    while !bytes.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(HostError::Deadline);
        }
        stream
            .set_read_timeout(Some(remaining))
            .map_err(|source| io_error("set plugin request read deadline", source))?;
        match stream.read(bytes) {
            Ok(0) => return Err(HostError::Crashed),
            Ok(read) => bytes = &mut bytes[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                ) =>
            {
                return Err(HostError::Deadline);
            }
            Err(error) => return Err(io_error(operation, error)),
        }
    }
    if Instant::now() >= deadline {
        Err(HostError::Deadline)
    } else {
        Ok(())
    }
}

fn validate_limits(limits: &Limits) -> Result<(), HostError> {
    if limits.max_frame_bytes <= FRAME_HEADER_BYTES
        || limits.max_frame_bytes > HARD_MAX_FRAME_BYTES
        || limits.queue_depth == 0
        || limits.queue_depth > HARD_MAX_QUEUE_DEPTH
        || limits.max_concurrency == 0
        || limits.max_concurrency > HARD_MAX_CONCURRENCY
        || limits.max_output_bytes == 0
        || limits.max_output_bytes > HARD_MAX_FRAME_BYTES
        || limits.max_declared_resource_requests == 0
        || limits.max_declared_resource_requests > HARD_MAX_DECLARED_RESOURCE_REQUESTS
        || limits.max_declared_resource_units == 0
        || limits.request_deadline.is_zero()
        || limits.request_deadline > HARD_MAX_DEADLINE
        || limits.child_deadline.is_zero()
        || limits.child_deadline > HARD_MAX_DEADLINE
        || limits.idle_timeout.is_zero()
        || limits.idle_timeout > HARD_MAX_DEADLINE
    {
        return Err(HostError::BoundExceeded);
    }
    Ok(())
}

fn validate_request(request: &PluginRequest, limits: &Limits) -> Result<(), HostError> {
    if request.operation.is_empty()
        || request.operation.len() > 128
        || request.declared_resources.len() > limits.max_declared_resource_requests
        || request.declared_resources.iter().any(|claim| {
            claim.name.is_empty()
                || claim.name.len() > 128
                || claim.name.contains('/')
                || claim.name.contains('\\')
                || claim.name == "."
                || claim.name == ".."
        })
        || request
            .declared_resources
            .iter()
            .try_fold(0_u64, |total, claim| total.checked_add(claim.units))
            .is_none_or(|total| total > limits.max_declared_resource_units)
    {
        return Err(HostError::BoundExceeded);
    }
    Ok(())
}

fn prepare_request(request: PluginRequest, limits: &Limits) -> Result<PreparedRequest, HostError> {
    validate_request(&request, limits)?;
    let frame = encode_frame(
        &WireMessage::Request {
            version: PROTOCOL_VERSION,
            request: request.clone(),
        },
        limits.max_frame_bytes,
    )?;
    // Decode the exact bytes that will be transmitted. This prevents runtime
    // activation if the protocol schema and serialized request ever diverge.
    let decoded: WireMessage =
        serde_json::from_slice(frame.body()).map_err(|_| HostError::MalformedFrame)?;
    if !matches!(
        decoded,
        WireMessage::Request {
            version: PROTOCOL_VERSION,
            ..
        }
    ) {
        return Err(HostError::UnsupportedVersion);
    }
    let deadline = Instant::now()
        .checked_add(limits.request_deadline)
        .ok_or(HostError::BoundExceeded)?;
    Ok(PreparedRequest {
        request,
        frame,
        deadline,
    })
}

fn validate_plugin_name(name: &str) -> Result<(), HostError> {
    if name.is_empty()
        || name.len() > 128
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(HostError::InvalidEnrollment);
    }
    Ok(())
}

#[cfg(test)]
fn opaque_plugin_identity(stable_name: &str) -> PluginIdentity {
    let mut hasher = Sha256::new();
    hasher.update(b"orchestrator-plugin-identity\0");
    hasher.update(stable_name.as_bytes());
    PluginIdentity(hasher.finalize().into())
}

#[cfg(test)]
fn validate_relative(path: &Path) -> Result<(), HostError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(HostError::InvalidEnrollment);
    }
    Ok(())
}

#[cfg(test)]
fn validate_beneath(root: &Path, relative: &Path, directory: bool) -> Result<(), HostError> {
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(HostError::InvalidEnrollment);
        };
        current.push(name);
        let metadata = fs::symlink_metadata(&current)
            .map_err(|source| io_error("inspect fixture enrollment", source))?;
        if metadata.file_type().is_symlink() {
            return Err(HostError::InvalidEnrollment);
        }
    }
    let canonical = fs::canonicalize(&current)
        .map_err(|source| io_error("canonicalize fixture enrollment", source))?;
    let valid_type = if directory {
        canonical.is_dir()
    } else {
        canonical.is_file()
    };
    if !canonical.starts_with(root) || !valid_type {
        return Err(HostError::InvalidEnrollment);
    }
    Ok(())
}

#[cfg(test)]
fn enroll_fixture(
    fixture_root: &Path,
    executable_relative: &Path,
    working_directory_relative: &Path,
    admitted_executable_image: &[u8],
    limits: Limits,
    behavior: Option<&str>,
) -> Result<EnrolledPlugin, HostError> {
    use std::os::unix::fs::PermissionsExt;

    validate_limits(&limits)?;
    validate_relative(executable_relative)?;
    validate_relative(working_directory_relative)?;
    let root = fs::canonicalize(fixture_root)
        .map_err(|source| io_error("canonicalize fixture root", source))?;
    let root_meta =
        fs::symlink_metadata(&root).map_err(|source| io_error("inspect fixture root", source))?;
    if !root_meta.is_dir() || root_meta.permissions().mode() & 0o077 != 0 {
        return Err(HostError::InvalidEnrollment);
    }
    validate_beneath(&root, executable_relative, false)?;
    validate_beneath(&root, working_directory_relative, true)?;
    let retained_root =
        File::open(&root).map_err(|source| io_error("retain fixture root", source))?;
    let retained_executable = File::open(root.join(executable_relative))
        .map_err(|source| io_error("retain fixture executable", source))?;
    let retained_cwd = File::open(root.join(working_directory_relative))
        .map_err(|source| io_error("retain fixture working directory", source))?;
    // Validate the exact retained tuple once at enrollment; activations only
    // clone these already-open capabilities.
    FixtureProcessLaunchAuthority::new(
        retained_root
            .try_clone()
            .map_err(|source| io_error("clone fixture root for validation", source))?,
        retained_executable
            .try_clone()
            .map_err(|source| io_error("clone fixture executable for validation", source))?,
        executable_relative.to_path_buf(),
        retained_cwd
            .try_clone()
            .map_err(|source| io_error("clone fixture cwd for validation", source))?,
        working_directory_relative.to_path_buf(),
        admitted_executable_image,
    )
    .map_err(|_| HostError::InvalidEnrollment)?;
    Ok(EnrolledPlugin {
        config: Arc::new(AdmittedConfig {
            root: retained_root,
            executable: retained_executable,
            executable_relative: executable_relative.to_path_buf(),
            cwd: retained_cwd,
            cwd_relative: working_directory_relative.to_path_buf(),
            image: Arc::from(admitted_executable_image),
            argv_tail: behavior.into_iter().map(OsString::from).collect(),
            plugin_identity: opaque_plugin_identity("first-party-fixture"),
            granted_capabilities: vec![CapabilityGrant {
                name: "fixture.invoke".into(),
            }],
            limits,
            panic_manager_after_spawn: behavior == Some("manager-panic"),
            fail_protocol_setup: behavior == Some("dual-fault"),
            fail_cleanup: behavior == Some("dual-fault"),
        }),
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct RequestPermit<'a>(&'a AtomicUsize);

struct QueueReset(Arc<Mutex<usize>>);

struct FlagReset<'a>(&'a AtomicBool);

struct CancelOnDrop(CancellationToken);

struct CounterDecrement<'a>(&'a AtomicUsize);

impl Drop for FlagReset<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Drop for QueueReset {
    fn drop(&mut self) {
        *lock(&self.0) = 0;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

impl Drop for CounterDecrement<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl<'a> RequestPermit<'a> {
    fn acquire(counter: &'a AtomicUsize, maximum: usize) -> Result<Self, HostError> {
        counter
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < maximum).then_some(current + 1)
            })
            .map_err(|_| HostError::ConcurrencyLimit)?;
        Ok(Self(counter))
    }
}

impl Drop for RequestPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
#[path = "../tests/lazy_host_e2e.rs"]
pub(crate) mod tests;
