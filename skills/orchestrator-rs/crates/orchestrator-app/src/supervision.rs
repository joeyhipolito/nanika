//! Fixture admission and protocol checks over the production process kernel.
//!
//! This module owns no child process, signal loop, reader thread, registry, or
//! reaper. [`FixtureProcessAuthority`] validates fixture capabilities and then
//! delegates lifecycle ownership to [`orchestrator_process::ProcessSupervisor`].

use crate::{
    ExecutableCapability, WorkspaceAuthority,
    fs_util::{FileIdentity, identity, open_dir_path_nofollow},
};
use cap_std::fs::Dir;
use orchestrator_core::{
    VerificationOutcome, VerificationSummary, VerificationTermination, classify_verification,
};
use orchestrator_process::{
    CancellationToken, FixtureProcessLaunchAuthority, ProcessError, ProcessReport, ProcessSpec,
    ProcessSupervisor, ProcessTermination,
};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    fmt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use thiserror::Error;

const MAX_CONTROL_FRAME: usize = 16 * 1024;
const CONTROL_PREFIX: &[u8] = b"ORCHESTRATOR_FIXTURE_V1 ";
const MAX_STALL_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// Fixed process-capacity and cleanup bounds for one fixture authority.
#[derive(Clone, Copy, Debug)]
pub struct SupervisorLimits {
    max_owned_groups: usize,
    term_grace: Duration,
    cleanup_grace: Duration,
}

impl Default for SupervisorLimits {
    fn default() -> Self {
        Self {
            max_owned_groups: 32,
            term_grace: Duration::from_secs(10),
            cleanup_grace: Duration::from_secs(2),
        }
    }
}

impl SupervisorLimits {
    /// Creates reduced deterministic fixture deadlines.
    pub fn for_tests(
        term_grace: Duration,
        cleanup_grace: Duration,
    ) -> Result<Self, SupervisorError> {
        if term_grace.is_zero() || cleanup_grace.is_zero() {
            return Err(SupervisorError::InvalidLimits);
        }
        Ok(Self {
            max_owned_groups: 4,
            term_grace,
            cleanup_grace,
        })
    }

    /// Overrides the process-group capacity while retaining all cleanup bounds.
    pub fn with_max_owned_groups(mut self, maximum: usize) -> Result<Self, SupervisorError> {
        if maximum == 0 {
            return Err(SupervisorError::InvalidLimits);
        }
        self.max_owned_groups = maximum;
        Ok(self)
    }
}

/// Fixture command arguments and policies added to the sealed executable.
///
/// The executable and CWD are supplied only by [`FixtureProcessAuthority`].
/// Values deliberately have no revealing `Debug` implementation.
pub struct FixtureProcessSpec {
    arguments: Vec<OsString>,
    hard_timeout: Duration,
    hard_deadline_at: Option<Instant>,
    stall_timeout: Option<Duration>,
    max_output_bytes: Option<usize>,
    stdin: Option<Vec<u8>>,
    cancellation: Option<CancellationToken>,
}

impl FixtureProcessSpec {
    /// Creates a fixture specification without exposing its installed executable.
    pub fn new<I, S>(arguments: I, hard_timeout: Duration) -> Result<Self, SupervisorError>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let arguments = arguments.into_iter().map(Into::into).collect::<Vec<_>>();
        let mut validation = Vec::with_capacity(arguments.len().saturating_add(1));
        validation.push(OsString::from("fixture-executable"));
        validation.extend(arguments.iter().cloned());
        ProcessSpec::new(validation, hard_timeout)?;
        Ok(Self {
            arguments,
            hard_timeout,
            hard_deadline_at: None,
            stall_timeout: None,
            max_output_bytes: None,
            stdin: None,
            cancellation: None,
        })
    }

    /// Preserves an upstream monotonic deadline across adapter translation.
    #[must_use]
    pub(crate) fn with_hard_deadline_at(mut self, deadline: Instant) -> Self {
        self.hard_deadline_at = Some(deadline);
        self
    }

    /// Records an explicit output-retention cap for each stream.
    #[must_use]
    pub fn with_max_output_bytes(mut self, cap: usize) -> Self {
        self.max_output_bytes = Some(cap);
        self
    }

    /// Supplies bounded stdin to the production supervisor.
    #[must_use]
    pub fn with_stdin(mut self, bytes: Vec<u8>) -> Self {
        self.stdin = Some(bytes);
        self
    }

    /// Wires event-driven cancellation into the production supervisor.
    #[must_use]
    pub fn with_cancellation(mut self, token: CancellationToken) -> Self {
        self.cancellation = Some(token);
        self
    }

    /// Sets the production supervisor's bounded stdout/stderr inactivity timeout.
    pub fn with_stall_timeout(mut self, timeout: Duration) -> Result<Self, SupervisorError> {
        if timeout.is_zero() || timeout > MAX_STALL_TIMEOUT {
            return Err(SupervisorError::Process(ProcessError::InvalidSpec));
        }
        self.stall_timeout = Some(timeout);
        Ok(self)
    }
}

/// Fixture admission or production-supervisor failure.
#[derive(Error)]
pub enum SupervisorError {
    /// Fixture authority and workspace do not share an admitted root.
    #[error("fixture process authority and workspace do not share an admitted root")]
    AuthorityMismatch,
    /// Capacity or cleanup limits were invalid.
    #[error("invalid supervisor limits")]
    InvalidLimits,
    /// A fixture executable, CWD, or boundary capability failed verification.
    #[error("fixture capability validation failed")]
    Capability,
    /// The production process kernel rejected or failed the command.
    #[error("production process supervision failed")]
    Process(
        #[from]
        #[source]
        ProcessError,
    ),
}

impl fmt::Debug for SupervisorError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AuthorityMismatch => "SupervisorError::AuthorityMismatch",
            Self::InvalidLimits => "SupervisorError::InvalidLimits",
            Self::Capability => "SupervisorError::Capability",
            Self::Process(_) => "SupervisorError::Process",
        })
    }
}

/// Closed-schema fixture protocol facts parsed from bounded stderr.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FixtureProtocolReport {
    /// Whether ready, child, verification, and done frames formed a valid stream.
    pub complete: bool,
    /// Maximum of reported and observed logical-child starts.
    pub logical_children_started: u64,
    /// Maximum of reported and observed logical-child reaps.
    pub logical_children_reaped: u64,
    /// Optional structured verification summary.
    pub verification_summary: Option<VerificationSummary>,
}

/// Production process receipt plus fixture-only protocol facts.
pub struct FixtureProcessReport {
    /// Receipt produced by the single production lifecycle kernel.
    pub process: ProcessReport,
    /// Pure protocol classification over the retained stderr bytes.
    pub protocol: FixtureProtocolReport,
}

impl fmt::Debug for FixtureProcessReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureProcessReport")
            .field("process", &self.process)
            .field("protocol", &self.protocol)
            .finish()
    }
}

impl FixtureProcessReport {
    /// Requires both production cleanup success and complete fixture protocol.
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.process.is_success()
            && self.protocol.complete
            && self.protocol.logical_children_started == self.protocol.logical_children_reaped
            && self
                .protocol
                .verification_summary
                .is_none_or(|_| self.verification_outcome().gate_passed())
    }

    /// Classifies the structured summary without inspecting human output.
    #[must_use]
    pub fn verification_outcome(&self) -> VerificationOutcome {
        let termination = match self.process.termination {
            ProcessTermination::Exited(code) => VerificationTermination::Exited(code),
            ProcessTermination::Signaled(signal) => VerificationTermination::Signaled(signal),
            ProcessTermination::Timeout => VerificationTermination::TimedOut,
            ProcessTermination::Stalled => VerificationTermination::Stalled,
            ProcessTermination::Cancelled => VerificationTermination::Cancelled,
            ProcessTermination::OutputLimit
            | ProcessTermination::InfrastructureError
            | ProcessTermination::UnresolvedOwnership => {
                VerificationTermination::InfrastructureError
            }
        };
        classify_verification(termination, self.protocol.verification_summary)
    }
}

/// One sealed fixture executable and CWD backed by the production supervisor.
///
/// On macOS this authority is fixture-only and is not eligible for production
/// enrollment. Before Stage 5 macOS live execution, Nanika requires an owned
/// `fchdir` broker or an independently audited FD-aware spawn wrapper so the
/// CWD transition is atomic rather than an `F_GETPATH` namespace check.
pub struct FixtureProcessAuthority {
    executable: ExecutableCapability,
    cwd_directory: Dir,
    cwd_identity: FileIdentity,
    cwd_relative: PathBuf,
    environment: Vec<(OsString, OsString)>,
    limits: SupervisorLimits,
    supervisor: ProcessSupervisor,
    launch: FixtureProcessLaunchAuthority,
}

impl fmt::Debug for FixtureProcessAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureProcessAuthority")
            .field("limits", &self.limits)
            .field("supervisor", &self.supervisor)
            .finish_non_exhaustive()
    }
}

impl FixtureProcessAuthority {
    /// Binds the production kernel to one fixture executable and workspace.
    pub fn new(
        executable: ExecutableCapability,
        workspace: &WorkspaceAuthority,
        limits: SupervisorLimits,
    ) -> Result<Self, SupervisorError> {
        if limits.max_owned_groups == 0
            || limits.term_grace.is_zero()
            || limits.cleanup_grace.is_zero()
        {
            return Err(SupervisorError::InvalidLimits);
        }
        if !workspace.shares_boundary(&executable.boundary) {
            return Err(SupervisorError::AuthorityMismatch);
        }
        let (executable_file, executable_image) = executable
            .open_verified()
            .map_err(|_| SupervisorError::Capability)?;
        let (cwd_directory, cwd_identity, cwd_relative) = workspace
            .process_cwd_binding()
            .map_err(|_| SupervisorError::Capability)?;
        let root = executable.boundary.canonical_path().to_path_buf();
        let launch = FixtureProcessLaunchAuthority::new(
            executable
                .boundary
                .directory()
                .try_clone()
                .map(cap_std::fs::Dir::into_std_file)
                .map_err(|_| SupervisorError::Capability)?,
            executable_file,
            Path::new("bin").join(&executable.label),
            cwd_directory
                .try_clone()
                .map(cap_std::fs::Dir::into_std_file)
                .map_err(|_| SupervisorError::Capability)?,
            cwd_relative.clone(),
            &executable_image,
        )?;
        let environment = vec![
            (OsString::from("HOME"), root.join("home").into_os_string()),
            (OsString::from("TMPDIR"), root.join("tmp").into_os_string()),
            (OsString::from("PATH"), root.join("bin").into_os_string()),
            (
                OsString::from("ORCHESTRATOR_FIXTURE_PROTOCOL"),
                OsString::from("1"),
            ),
        ];
        let supervisor = ProcessSupervisor::new(limits.max_owned_groups)?;
        Ok(Self {
            executable,
            cwd_directory,
            cwd_identity,
            cwd_relative,
            environment,
            limits,
            supervisor,
            launch,
        })
    }

    /// Returns whether the production registry still owns active or unresolved work.
    #[must_use]
    pub fn has_unresolved_processes(&self) -> bool {
        self.supervisor.has_owned_processes()
    }

    pub(crate) fn process_service_binding(&self) -> (&str, PathBuf) {
        (
            &self.executable.label,
            self.executable
                .boundary
                .canonical_path()
                .join(&self.cwd_relative),
        )
    }

    /// Runs a fixture command through the production ownership kernel.
    pub fn run(
        &self,
        fixture: &FixtureProcessSpec,
    ) -> Result<FixtureProcessReport, SupervisorError> {
        self.executable
            .verify()
            .map_err(|_| SupervisorError::Capability)?;
        self.verify_cwd()?;

        let mut argv = Vec::with_capacity(fixture.arguments.len().saturating_add(1));
        argv.push(OsString::from("fixture-executable"));
        argv.extend(fixture.arguments.iter().cloned());
        let mut spec = ProcessSpec::new(argv, fixture.hard_timeout)?
            .with_term_grace(self.limits.term_grace)
            .with_cleanup_grace(self.limits.cleanup_grace);
        if let Some(deadline) = fixture.hard_deadline_at {
            spec = spec.with_hard_deadline_at(deadline);
        }
        for (name, value) in &self.environment {
            spec = spec.with_env(name.clone(), value.clone());
        }
        if let Some(cap) = fixture.max_output_bytes {
            spec = spec.with_max_output_bytes(cap);
        }
        if let Some(timeout) = fixture.stall_timeout {
            spec = spec.with_stall_timeout(timeout);
        }
        if let Some(stdin) = &fixture.stdin {
            spec = spec.with_stdin(stdin.clone());
        }
        if let Some(cancellation) = &fixture.cancellation {
            spec = spec.with_cancellation(cancellation.clone());
        }

        let process = self
            .supervisor
            .run_fixture_authorized(&spec, &self.launch)?;
        let protocol = parse_fixture_protocol(&process.stderr);
        Ok(FixtureProcessReport { process, protocol })
    }

    fn verify_cwd(&self) -> Result<(), SupervisorError> {
        self.executable
            .boundary
            .verify()
            .map_err(|_| SupervisorError::Capability)?;
        let mapped =
            open_dir_path_nofollow(self.executable.boundary.directory(), &self.cwd_relative)
                .map_err(|_| SupervisorError::Capability)?;
        let mapped_metadata = mapped
            .dir_metadata()
            .map_err(|_| SupervisorError::Capability)?;
        let held_metadata = self
            .cwd_directory
            .dir_metadata()
            .map_err(|_| SupervisorError::Capability)?;
        if identity(&mapped_metadata) != self.cwd_identity
            || identity(&held_metadata) != self.cwd_identity
        {
            return Err(SupervisorError::Capability);
        }
        Ok(())
    }
}

/// Parses the closed fixture-control schema from retained stderr bytes.
#[must_use]
pub fn parse_fixture_protocol(stderr: &[u8]) -> FixtureProtocolReport {
    let mut state = ProtocolState::default();
    for line in stderr.split(|byte| *byte == b'\n') {
        state.ingest(line);
    }
    state.snapshot()
}

#[derive(Default, Debug)]
struct ProtocolState {
    next_sequence: u64,
    ready_seen: bool,
    children_started: BTreeSet<String>,
    children_reaped: BTreeSet<String>,
    done_counts: Option<(u64, u64)>,
    verification: Option<VerificationSummary>,
    invalid: bool,
}

impl ProtocolState {
    fn ingest(&mut self, line: &[u8]) {
        if !line.starts_with(CONTROL_PREFIX) {
            return;
        }
        if line.len() > MAX_CONTROL_FRAME {
            self.invalid = true;
            return;
        }
        let payload = &line[CONTROL_PREFIX.len()..];
        let Ok(frame) = serde_json::from_slice::<ControlFrame>(payload) else {
            self.invalid = true;
            return;
        };
        if frame.version != 1 || frame.seq != self.next_sequence.saturating_add(1) {
            self.invalid = true;
            return;
        }
        self.next_sequence = frame.seq;
        if self.done_counts.is_some()
            || (!self.ready_seen && frame.kind != "ready")
            || (self.verification.is_some() && frame.kind != "done")
        {
            self.invalid = true;
            return;
        }
        match frame.kind.as_str() {
            "ready" => {
                if self.ready_seen || serde_json::from_value::<EmptyData>(frame.data).is_err() {
                    self.invalid = true;
                }
                self.ready_seen = true;
            }
            "activity" => {
                if serde_json::from_value::<EmptyData>(frame.data).is_err() {
                    self.invalid = true;
                }
            }
            "child" => match serde_json::from_value::<ChildData>(frame.data) {
                Ok(data) if valid_logical_id(&data.id) && data.state == "started" => {
                    if !self.children_started.insert(data.id) {
                        self.invalid = true;
                    }
                }
                Ok(data) if valid_logical_id(&data.id) && data.state == "reaped" => {
                    if !self.children_started.contains(&data.id)
                        || !self.children_reaped.insert(data.id)
                    {
                        self.invalid = true;
                    }
                }
                _ => self.invalid = true,
            },
            "verification" => match serde_json::from_value::<VerificationData>(frame.data) {
                Ok(data) if self.verification.is_none() && valid_logical_id(&data.scenario) => {
                    self.verification = Some(VerificationSummary {
                        discovered: data.discovered,
                        executed: data.executed,
                        passed: data.passed,
                        failed: data.failed,
                        required_skipped: data.required_skipped,
                    });
                }
                _ => self.invalid = true,
            },
            "done" => match serde_json::from_value::<DoneData>(frame.data) {
                Ok(data) if self.done_counts.is_none() => {
                    self.done_counts = Some((data.children_started, data.children_reaped));
                }
                _ => self.invalid = true,
            },
            _ => self.invalid = true,
        }
    }

    fn snapshot(&self) -> FixtureProtocolReport {
        let (reported_started, reported_reaped) = self.done_counts.unwrap_or((0, 0));
        let observed_started = self.children_started.len() as u64;
        let observed_reaped = self.children_reaped.len() as u64;
        FixtureProtocolReport {
            complete: !self.invalid
                && self.ready_seen
                && self.done_counts.is_some()
                && reported_started == reported_reaped
                && reported_started == observed_started
                && reported_reaped == observed_reaped,
            logical_children_started: reported_started.max(observed_started),
            logical_children_reaped: reported_reaped.max(observed_reaped),
            verification_summary: self.verification,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ControlFrame {
    version: u32,
    seq: u64,
    kind: String,
    data: serde_json::Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyData {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildData {
    id: String,
    state: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationData {
    scenario: String,
    discovered: u64,
    executed: u64,
    passed: u64,
    failed: u64,
    required_skipped: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DoneData {
    children_started: u64,
    children_reaped: u64,
}

fn valid_logical_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_parser_accepts_balanced_child_lifecycle() {
        let report = parse_fixture_protocol(
            br#"ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":1,"kind":"ready","data":{}}
ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":2,"kind":"child","data":{"id":"child-1","state":"started"}}
ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":3,"kind":"child","data":{"id":"child-1","state":"reaped"}}
ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":4,"kind":"done","data":{"children_started":1,"children_reaped":1}}
"#,
        );
        assert!(report.complete, "{report:?}");
        assert_eq!(report.logical_children_started, 1);
        assert_eq!(report.logical_children_reaped, 1);
    }

    #[test]
    fn protocol_parser_rejects_sequence_gap_duplicate_and_unreaped_child() {
        for bytes in [
            br#"ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":2,"kind":"ready","data":{}}
"#
            .as_slice(),
            br#"ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":1,"kind":"ready","data":{}}
ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":2,"kind":"ready","data":{}}
"#
            .as_slice(),
            br#"ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":1,"kind":"ready","data":{}}
ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":2,"kind":"child","data":{"id":"child-1","state":"started"}}
ORCHESTRATOR_FIXTURE_V1 {"version":1,"seq":3,"kind":"done","data":{"children_started":1,"children_reaped":0}}
"#
            .as_slice(),
        ] {
            assert!(!parse_fixture_protocol(bytes).complete);
        }
    }

    #[test]
    fn fixture_spec_accepts_a_bounded_stall_policy() -> Result<(), SupervisorError> {
        let timeout = Duration::from_millis(50);
        let spec = FixtureProcessSpec::new(["sleep"], Duration::from_secs(1))?
            .with_stall_timeout(timeout)?;
        assert_eq!(spec.stall_timeout, Some(timeout));
        assert!(matches!(
            FixtureProcessSpec::new(["sleep"], Duration::from_secs(1))?
                .with_stall_timeout(Duration::ZERO),
            Err(SupervisorError::Process(ProcessError::InvalidSpec))
        ));
        Ok(())
    }
}
