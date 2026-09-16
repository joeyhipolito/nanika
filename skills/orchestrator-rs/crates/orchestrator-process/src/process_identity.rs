//! Stable kernel identities for supervised processes.

use std::fmt;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use rustix::process::{Pid, test_kill_process_group};
use thiserror::Error;

const MAX_START_IDENTITY_BYTES: usize = 96;
#[cfg(any(target_os = "linux", test))]
const MAX_LINUX_STAT_BYTES: usize = 4 * 1024;
#[cfg(any(target_os = "linux", test))]
const MAX_LINUX_BOOT_ID_BYTES: usize = 37;

/// An opaque process identity observed directly from the operating-system kernel.
///
/// Construction is deliberately restricted to [`Self::observe`]. The caller
/// must retain its owned process handle while observing so the numeric PID
/// cannot be reused. The returned identity proves that two consecutive kernel
/// observations agreed with the supplied PID and process-group ID; supplying
/// numeric IDs alone does not establish process ownership.
#[derive(Clone)]
pub struct KernelProcessIdentity {
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
}

impl KernelProcessIdentity {
    /// Observes a stable identity for one live, caller-owned process.
    ///
    /// Linux identities contain the validated kernel boot UUID and `/proc`
    /// start ticks. Darwin identities contain the process start seconds and
    /// microseconds returned by `proc_pidinfo`.
    ///
    /// # Errors
    ///
    /// Returns an error when either identifier is invalid, the process is not
    /// live, the kernel record disagrees with the supplied process group, or
    /// consecutive observations do not describe the same process.
    pub fn observe(pid: u32, process_group_id: u32) -> Result<Self, ProcessIdentityError> {
        validate_identifier(pid)?;
        validate_identifier(process_group_id)?;

        let first = observe_once(pid)?;
        let second = match observe_once(pid) {
            Err(ProcessIdentityError::ProcessNotLive) => {
                return Err(ProcessIdentityError::IdentityChanged);
            }
            result => result?,
        };
        if first != second {
            return Err(ProcessIdentityError::IdentityChanged);
        }
        if first.pid != pid || first.process_group_id != process_group_id {
            return Err(ProcessIdentityError::KernelRecordMismatch);
        }
        if !valid_start_identity(&first.process_start_identity) {
            return Err(ProcessIdentityError::InvalidKernelRecord);
        }

        Ok(Self {
            pid,
            process_group_id,
            process_start_identity: first.process_start_identity,
        })
    }

    /// Returns the kernel-observed process ID.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the kernel-observed process-group ID.
    #[must_use]
    pub fn process_group_id(&self) -> u32 {
        self.process_group_id
    }

    /// Returns the bounded, platform-qualified process-start identity.
    #[must_use]
    pub fn process_start_identity(&self) -> &str {
        &self.process_start_identity
    }
}

impl fmt::Debug for KernelProcessIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("KernelProcessIdentity(REDACTED)")
    }
}

/// Ownership-neutral classification of one persisted process incarnation.
///
/// This classification only describes whether the recorded PID, process
/// group, and platform start identity still name the same stable live kernel
/// process. It conveys no authority to signal, kill, wait for, or reap it.
#[derive(Debug, Eq, PartialEq)]
pub enum RecordedProcessIdentityStatus {
    /// Two stable kernel observations exactly matched every persisted field.
    ExactLive,
    /// The recorded leader incarnation is absent and two non-signalling group
    /// existence probes both proved the recorded process group absent.
    ExactGroupAbsent(ExactProcessGroupAbsence),
    /// The recorded leader incarnation is absent or mismatched, but the
    /// numeric process group exists or permission prevented an absence proof.
    LeaderAbsentGroupPresent,
}

/// Opaque proof that one exact recorded leader incarnation and process group
/// were absent under the inspector's two-observation contract.
///
/// The fields are private and this type is intentionally neither [`Clone`]
/// nor [`Copy`]. Only [`inspect_recorded_process_identity`] can construct it.
#[derive(Eq, PartialEq)]
pub struct ExactProcessGroupAbsence {
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
}

impl ExactProcessGroupAbsence {
    /// Returns the persisted leader PID bound into this proof.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Returns the persisted process-group ID bound into this proof.
    #[must_use]
    pub fn process_group_id(&self) -> u32 {
        self.process_group_id
    }

    /// Returns the persisted platform-qualified start identity bound into this proof.
    #[must_use]
    pub fn process_start_identity(&self) -> &str {
        &self.process_start_identity
    }
}

impl fmt::Debug for ExactProcessGroupAbsence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExactProcessGroupAbsence(REDACTED)")
    }
}

/// Inspects a persisted process identity without claiming process ownership.
///
/// The kernel is observed twice. [`RecordedProcessIdentityStatus::ExactLive`]
/// is returned only when both observations are identical and exactly match
/// `pid`, `process_group_id`, and `expected_process_start_identity`.
/// [`RecordedProcessIdentityStatus::ExactGroupAbsent`] is returned only when
/// the recorded leader incarnation is no longer current and two subsequent
/// non-signalling process-group probes both prove absence. A present group or
/// permission-denied probe yields
/// [`RecordedProcessIdentityStatus::LeaderAbsentGroupPresent`]. Observation
/// errors, unsupported platforms, and malformed persisted identities remain
/// errors.
///
/// This function never signals, kills, waits for, or reaps a process, and its
/// result must not be treated as process ownership.
///
/// # Errors
///
/// Returns [`ProcessIdentityError`] when persisted fields are malformed, the
/// platform is unsupported, a kernel observation fails, or observations
/// change in a way that leaves the recorded incarnation live but unstable.
pub fn inspect_recorded_process_identity(
    pid: u32,
    process_group_id: u32,
    expected_process_start_identity: &str,
) -> Result<RecordedProcessIdentityStatus, ProcessIdentityError> {
    validate_identifier(pid)?;
    validate_identifier(process_group_id)?;
    validate_recorded_start_identity(expected_process_start_identity)?;

    // Always take both observations. A single missing or matching record is
    // not sufficient to classify a persisted incarnation.
    let first = observe_once(pid);
    let second = observe_once(pid);
    let leader = classify_recorded_observations(
        pid,
        process_group_id,
        expected_process_start_identity,
        first,
        second,
    )?;
    if leader == RecordedLeaderIdentityStatus::ExactLive {
        return Ok(RecordedProcessIdentityStatus::ExactLive);
    }

    // The leader incarnation is absent. Prove the whole numeric group absent
    // with two non-signalling existence probes; any observed presence (or
    // EPERM, which cannot prove absence) remains conservative.
    let first_group = observe_process_group_once(process_group_id);
    let second_group = observe_process_group_once(process_group_id);
    classify_recorded_process_group(
        pid,
        process_group_id,
        expected_process_start_identity,
        first_group,
        second_group,
    )
}

/// Failures that prevent a stable kernel process identity from being issued.
#[derive(Debug, Error)]
pub enum ProcessIdentityError {
    /// A PID or process-group ID was zero or outside the platform `pid_t` range.
    #[error("kernel process identity requires positive bounded identifiers")]
    InvalidIdentifier,
    /// A persisted process-start identity was malformed or did not use the
    /// current platform's canonical qualified representation.
    #[error("recorded kernel process identity is invalid")]
    InvalidRecordedIdentity,
    /// The process did not have a live kernel record.
    #[error("kernel process identity requires a live process")]
    ProcessNotLive,
    /// The kernel record disagreed with the supplied PID or process-group ID.
    #[error("kernel process identity did not match the supplied process identifiers")]
    KernelRecordMismatch,
    /// Consecutive observations did not identify the same process incarnation.
    #[error("kernel process identity changed between observations")]
    IdentityChanged,
    /// A kernel record was malformed, unbounded, or internally invalid.
    #[error("kernel process identity record is invalid")]
    InvalidKernelRecord,
    /// The platform has no enrolled kernel identity implementation.
    #[error("kernel process identity is unsupported on this platform")]
    UnsupportedPlatform,
    /// A fixed kernel observation operation failed.
    #[error("kernel process identity observation failed during {operation}")]
    Operation {
        /// Non-sensitive operation label.
        operation: &'static str,
        /// Underlying operating-system error.
        #[source]
        source: std::io::Error,
    },
}

impl ProcessIdentityError {
    fn operation(operation: &'static str, source: std::io::Error) -> Self {
        Self::Operation { operation, source }
    }
}

#[derive(Eq, PartialEq)]
struct KernelObservation {
    pid: u32,
    process_group_id: u32,
    process_start_identity: String,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum RecordedLeaderIdentityStatus {
    ExactLive,
    ExactAbsent,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ProcessGroupObservation {
    PresentOrUnproven,
    Absent,
}

fn classify_recorded_observations(
    pid: u32,
    process_group_id: u32,
    expected_process_start_identity: &str,
    first: Result<KernelObservation, ProcessIdentityError>,
    second: Result<KernelObservation, ProcessIdentityError>,
) -> Result<RecordedLeaderIdentityStatus, ProcessIdentityError> {
    match (first, second) {
        (Err(error), _) if !matches!(error, ProcessIdentityError::ProcessNotLive) => Err(error),
        (_, Err(error)) if !matches!(error, ProcessIdentityError::ProcessNotLive) => Err(error),
        (Ok(first), Ok(second)) => {
            let second_matches = observation_matches_recorded(
                &second,
                pid,
                process_group_id,
                expected_process_start_identity,
            );
            if first == second && second_matches {
                Ok(RecordedLeaderIdentityStatus::ExactLive)
            } else if !second_matches {
                Ok(RecordedLeaderIdentityStatus::ExactAbsent)
            } else {
                // The recorded incarnation appeared only in the second
                // observation. It may now be live, but it was not stable
                // across the required observation pair.
                Err(ProcessIdentityError::IdentityChanged)
            }
        }
        (Err(ProcessIdentityError::ProcessNotLive), Err(ProcessIdentityError::ProcessNotLive))
        | (Ok(_), Err(ProcessIdentityError::ProcessNotLive)) => {
            Ok(RecordedLeaderIdentityStatus::ExactAbsent)
        }
        (Err(ProcessIdentityError::ProcessNotLive), Ok(second)) => {
            if observation_matches_recorded(
                &second,
                pid,
                process_group_id,
                expected_process_start_identity,
            ) {
                Err(ProcessIdentityError::IdentityChanged)
            } else {
                Ok(RecordedLeaderIdentityStatus::ExactAbsent)
            }
        }
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn classify_recorded_process_group(
    pid: u32,
    process_group_id: u32,
    process_start_identity: &str,
    first: Result<ProcessGroupObservation, ProcessIdentityError>,
    second: Result<ProcessGroupObservation, ProcessIdentityError>,
) -> Result<RecordedProcessIdentityStatus, ProcessIdentityError> {
    match (first, second) {
        (Err(error), _) | (_, Err(error)) => Err(error),
        (Ok(ProcessGroupObservation::Absent), Ok(ProcessGroupObservation::Absent)) => Ok(
            RecordedProcessIdentityStatus::ExactGroupAbsent(ExactProcessGroupAbsence {
                pid,
                process_group_id,
                process_start_identity: process_start_identity.to_owned(),
            }),
        ),
        (Ok(_), Ok(_)) => Ok(RecordedProcessIdentityStatus::LeaderAbsentGroupPresent),
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn observe_process_group_once(
    process_group_id: u32,
) -> Result<ProcessGroupObservation, ProcessIdentityError> {
    let raw =
        i32::try_from(process_group_id).map_err(|_| ProcessIdentityError::InvalidIdentifier)?;
    let group = Pid::from_raw(raw).ok_or(ProcessIdentityError::InvalidIdentifier)?;
    match test_kill_process_group(group) {
        Ok(()) | Err(rustix::io::Errno::PERM) => Ok(ProcessGroupObservation::PresentOrUnproven),
        Err(rustix::io::Errno::SRCH) => Ok(ProcessGroupObservation::Absent),
        Err(source) => Err(ProcessIdentityError::operation(
            "probe process group existence",
            std::io::Error::from(source),
        )),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn observe_process_group_once(
    _process_group_id: u32,
) -> Result<ProcessGroupObservation, ProcessIdentityError> {
    Err(ProcessIdentityError::UnsupportedPlatform)
}

fn observation_matches_recorded(
    observation: &KernelObservation,
    pid: u32,
    process_group_id: u32,
    expected_process_start_identity: &str,
) -> bool {
    observation.pid == pid
        && observation.process_group_id == process_group_id
        && observation.process_start_identity == expected_process_start_identity
}

fn validate_identifier(identifier: u32) -> Result<(), ProcessIdentityError> {
    if !valid_identifier(identifier) {
        Err(ProcessIdentityError::InvalidIdentifier)
    } else {
        Ok(())
    }
}

fn valid_identifier(identifier: u32) -> bool {
    identifier != 0 && i32::try_from(identifier).is_ok()
}

fn valid_start_identity(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_START_IDENTITY_BYTES
        && value.bytes().all(|byte| byte.is_ascii_graphic())
}

#[cfg(target_os = "linux")]
fn validate_recorded_start_identity(value: &str) -> Result<(), ProcessIdentityError> {
    let Some(qualified) = value.strip_prefix("linux:") else {
        return Err(ProcessIdentityError::InvalidRecordedIdentity);
    };
    let Some((boot_id, start_ticks)) = qualified.split_once(':') else {
        return Err(ProcessIdentityError::InvalidRecordedIdentity);
    };
    let parsed_boot_id = parse_linux_boot_id(boot_id.as_bytes())
        .ok_or(ProcessIdentityError::InvalidRecordedIdentity)?;
    let parsed_start_ticks = parse_ascii_u64(start_ticks.as_bytes())
        .filter(|ticks| *ticks > 0)
        .ok_or(ProcessIdentityError::InvalidRecordedIdentity)?;
    if !valid_start_identity(value)
        || parsed_boot_id != boot_id
        || format!("linux:{parsed_boot_id}:{parsed_start_ticks}") != value
    {
        return Err(ProcessIdentityError::InvalidRecordedIdentity);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn validate_recorded_start_identity(value: &str) -> Result<(), ProcessIdentityError> {
    let Some(qualified) = value.strip_prefix("darwin:") else {
        return Err(ProcessIdentityError::InvalidRecordedIdentity);
    };
    let Some((seconds, microseconds)) = qualified.split_once(':') else {
        return Err(ProcessIdentityError::InvalidRecordedIdentity);
    };
    let seconds = parse_ascii_u64(seconds.as_bytes())
        .filter(|seconds| *seconds > 0)
        .ok_or(ProcessIdentityError::InvalidRecordedIdentity)?;
    let microseconds = parse_ascii_u32(microseconds.as_bytes())
        .filter(|microseconds| *microseconds < 1_000_000)
        .ok_or(ProcessIdentityError::InvalidRecordedIdentity)?;
    if !valid_start_identity(value) || format!("darwin:{seconds}:{microseconds}") != value {
        return Err(ProcessIdentityError::InvalidRecordedIdentity);
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn validate_recorded_start_identity(_value: &str) -> Result<(), ProcessIdentityError> {
    Err(ProcessIdentityError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn observe_once(pid: u32) -> Result<KernelObservation, ProcessIdentityError> {
    let boot_id = read_linux_boot_id()?;
    let stat_path = format!("/proc/{pid}/stat");
    let stat = match std::fs::read(stat_path) {
        Ok(stat) => stat,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(ProcessIdentityError::ProcessNotLive);
        }
        Err(source) => {
            return Err(ProcessIdentityError::operation("read process stat", source));
        }
    };
    let record =
        parse_linux_process_stat(&stat).ok_or(ProcessIdentityError::InvalidKernelRecord)?;
    if !record.is_live() {
        return Err(ProcessIdentityError::ProcessNotLive);
    }

    Ok(KernelObservation {
        pid: record.pid,
        process_group_id: record.process_group_id,
        process_start_identity: format!("linux:{boot_id}:{}", record.start_ticks),
    })
}

#[cfg(target_os = "linux")]
fn read_linux_boot_id() -> Result<String, ProcessIdentityError> {
    let bytes = std::fs::read("/proc/sys/kernel/random/boot_id")
        .map_err(|source| ProcessIdentityError::operation("read kernel boot identity", source))?;
    parse_linux_boot_id(&bytes).ok_or(ProcessIdentityError::InvalidKernelRecord)
}

#[cfg(target_os = "macos")]
fn observe_once(pid: u32) -> Result<KernelObservation, ProcessIdentityError> {
    const DARWIN_ZOMBIE_STATUS: u32 = 5;

    let record = proc_pidinfo::proc_pidinfo::<proc_pidinfo::ProcBSDInfo>(proc_pidinfo::Pid(pid))
        .map_err(|source| ProcessIdentityError::operation("query proc_pidinfo", source))?;
    let record = record.ok_or(ProcessIdentityError::ProcessNotLive)?;
    if record.pbi_status == 0 {
        return Err(ProcessIdentityError::InvalidKernelRecord);
    }
    if record.pbi_status == DARWIN_ZOMBIE_STATUS {
        return Err(ProcessIdentityError::ProcessNotLive);
    }
    if record.pbi_start_tvusec >= 1_000_000 {
        return Err(ProcessIdentityError::InvalidKernelRecord);
    }

    Ok(KernelObservation {
        pid: record.pbi_pid.0,
        process_group_id: record.pbi_pgid,
        process_start_identity: format!(
            "darwin:{}:{}",
            record.pbi_start_tvsec, record.pbi_start_tvusec
        ),
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn observe_once(_pid: u32) -> Result<KernelObservation, ProcessIdentityError> {
    Err(ProcessIdentityError::UnsupportedPlatform)
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Eq, PartialEq)]
struct LinuxProcessStat {
    pid: u32,
    state: u8,
    process_group_id: u32,
    start_ticks: u64,
}

#[cfg(target_os = "linux")]
impl LinuxProcessStat {
    fn is_live(&self) -> bool {
        !matches!(self.state, b'Z' | b'X' | b'x')
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_process_stat(stat: &[u8]) -> Option<LinuxProcessStat> {
    if stat.is_empty() || stat.len() > MAX_LINUX_STAT_BYTES || stat.contains(&0) {
        return None;
    }
    let command_start = stat.iter().position(|byte| *byte == b'(')?;
    let command_end = stat.iter().rposition(|byte| *byte == b')')?;
    if command_start >= command_end {
        return None;
    }
    let pid_bytes = stat.get(..command_start)?.strip_suffix(b" ")?;
    let pid = parse_ascii_u32(pid_bytes)?;
    if !valid_identifier(pid) {
        return None;
    }
    let mut fields = stat
        .get(command_end + 1..)?
        .split(|byte| byte.is_ascii_whitespace())
        .filter(|field| !field.is_empty());
    let state_field = fields.next()?;
    if state_field.len() != 1 || !state_field[0].is_ascii_alphabetic() {
        return None;
    }
    parse_ascii_u32(fields.next()?)?;
    let process_group_id = parse_ascii_u32(fields.next()?)?;
    if !valid_identifier(process_group_id) {
        return None;
    }
    let start_ticks = parse_ascii_u64(fields.nth(16)?)?;

    Some(LinuxProcessStat {
        pid,
        state: state_field[0],
        process_group_id,
        start_ticks,
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_linux_boot_id(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.len() > MAX_LINUX_BOOT_ID_BYTES {
        return None;
    }
    let value = bytes.strip_suffix(b"\n").unwrap_or(bytes);
    if value.len() != 36 {
        return None;
    }
    let mut nonzero = false;
    for (index, byte) in value.iter().copied().enumerate() {
        if matches!(index, 8 | 13 | 18 | 23) {
            if byte != b'-' {
                return None;
            }
        } else if !matches!(byte, b'0'..=b'9' | b'a'..=b'f') {
            return None;
        } else if byte != b'0' {
            nonzero = true;
        }
    }
    if !nonzero {
        return None;
    }
    std::str::from_utf8(value).ok().map(str::to_owned)
}

fn parse_ascii_u32(bytes: &[u8]) -> Option<u32> {
    u32::try_from(parse_ascii_u64(bytes)?).ok()
}

fn parse_ascii_u64(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() {
        return None;
    }
    bytes.iter().try_fold(0_u64, |value, byte| {
        let digit = byte.checked_sub(b'0').filter(|digit| *digit <= 9)?;
        value.checked_mul(10)?.checked_add(u64::from(digit))
    })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::os::unix::process::CommandExt;
    use std::process::{Child, Command, Stdio};

    use rustix::process::{Pid, getpgid};

    use super::{
        KernelObservation, KernelProcessIdentity, LinuxProcessStat, ProcessGroupObservation,
        ProcessIdentityError, RecordedLeaderIdentityStatus, RecordedProcessIdentityStatus,
        classify_recorded_observations, classify_recorded_process_group,
        inspect_recorded_process_identity, parse_linux_boot_id, parse_linux_process_stat,
    };

    struct TestChild(Child);

    impl TestChild {
        fn spawn() -> std::io::Result<Self> {
            Command::new("/bin/sleep")
                .arg(OsString::from("30"))
                .process_group(0)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map(Self)
        }

        fn id(&self) -> u32 {
            self.0.id()
        }

        fn stop(&mut self) -> std::io::Result<()> {
            self.0.kill()?;
            self.0.wait().map(|_| ())
        }
    }

    impl Drop for TestChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn process_group_id(pid: Option<u32>) -> Result<u32, Box<dyn std::error::Error>> {
        let pid = pid.map(i32::try_from).transpose()?.and_then(Pid::from_raw);
        Ok(u32::try_from(getpgid(pid)?.as_raw_pid())?)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn different_valid_start_identity(actual: &str) -> Result<String, Box<dyn std::error::Error>> {
        if let Some(qualified) = actual.strip_prefix("linux:") {
            let (boot_id, ticks) = qualified
                .split_once(':')
                .ok_or("Linux identity is missing start ticks")?;
            let ticks: u64 = ticks.parse()?;
            let different = if ticks < u64::MAX {
                ticks + 1
            } else {
                ticks - 1
            };
            return Ok(format!("linux:{boot_id}:{different}"));
        }
        if let Some(qualified) = actual.strip_prefix("darwin:") {
            let (seconds, microseconds) = qualified
                .split_once(':')
                .ok_or("Darwin identity is missing start microseconds")?;
            let microseconds: u32 = microseconds.parse()?;
            let different = if microseconds < 999_999 {
                microseconds + 1
            } else {
                microseconds - 1
            };
            return Ok(format!("darwin:{seconds}:{different}"));
        }
        Err("identity is not qualified for a supported platform".into())
    }

    #[test]
    fn observe_returns_current_process_identifiers() -> Result<(), Box<dyn std::error::Error>> {
        let pid = std::process::id();
        let process_group_id = process_group_id(None)?;

        let identity = KernelProcessIdentity::observe(pid, process_group_id)?;

        assert_eq!(
            (identity.pid(), identity.process_group_id()),
            (pid, process_group_id)
        );
        Ok(())
    }

    #[test]
    fn observe_returns_identity_for_owned_child() -> Result<(), Box<dyn std::error::Error>> {
        let child = TestChild::spawn()?;
        let pid = child.id();
        let process_group_id = process_group_id(Some(pid))?;

        let identity = KernelProcessIdentity::observe(pid, process_group_id)?;

        assert_eq!(identity.pid(), pid);
        Ok(())
    }

    #[test]
    fn observe_rejects_wrong_process_group() -> Result<(), Box<dyn std::error::Error>> {
        let pid = std::process::id();
        let process_group_id = process_group_id(None)?;
        let wrong_group = if process_group_id < i32::MAX as u32 {
            process_group_id + 1
        } else {
            process_group_id - 1
        };

        let error = KernelProcessIdentity::observe(pid, wrong_group)
            .err()
            .ok_or("wrong process group unexpectedly produced an identity")?;

        assert!(matches!(error, ProcessIdentityError::KernelRecordMismatch));
        Ok(())
    }

    #[test]
    fn observe_rejects_zero_pid() -> Result<(), Box<dyn std::error::Error>> {
        let process_group_id = process_group_id(None)?;

        let error = KernelProcessIdentity::observe(0, process_group_id)
            .err()
            .ok_or("zero PID unexpectedly produced an identity")?;

        assert!(matches!(error, ProcessIdentityError::InvalidIdentifier));
        Ok(())
    }

    #[test]
    fn debug_redacts_all_process_identity_fields() -> Result<(), Box<dyn std::error::Error>> {
        let identity = KernelProcessIdentity::observe(std::process::id(), process_group_id(None)?)?;

        assert_eq!(format!("{identity:?}"), "KernelProcessIdentity(REDACTED)");
        Ok(())
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn recorded_identity_inspection_classifies_two_exact_observations_as_live()
    -> Result<(), Box<dyn std::error::Error>> {
        let pid = std::process::id();
        let process_group_id = process_group_id(None)?;
        let identity = KernelProcessIdentity::observe(pid, process_group_id)?;

        let status = inspect_recorded_process_identity(
            pid,
            process_group_id,
            identity.process_start_identity(),
        )?;

        assert_eq!(status, RecordedProcessIdentityStatus::ExactLive);
        Ok(())
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn recorded_identity_inspection_keeps_present_group_after_leader_mismatch()
    -> Result<(), Box<dyn std::error::Error>> {
        let pid = std::process::id();
        let process_group_id = process_group_id(None)?;
        let identity = KernelProcessIdentity::observe(pid, process_group_id)?;
        let mismatched = different_valid_start_identity(identity.process_start_identity())?;

        let status = inspect_recorded_process_identity(pid, process_group_id, &mismatched)?;

        assert_eq!(
            status,
            RecordedProcessIdentityStatus::LeaderAbsentGroupPresent
        );
        Ok(())
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn recorded_identity_inspection_proves_stopped_private_group_absent()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut child = TestChild::spawn()?;
        let pid = child.id();
        let process_group_id = process_group_id(Some(pid))?;
        let identity = KernelProcessIdentity::observe(pid, process_group_id)?;
        child.stop()?;

        let status = inspect_recorded_process_identity(
            pid,
            process_group_id,
            identity.process_start_identity(),
        )?;

        let proof = match status {
            RecordedProcessIdentityStatus::ExactGroupAbsent(proof) => proof,
            other => return Err(format!("expected exact group absence, got {other:?}").into()),
        };
        assert_eq!(proof.pid(), pid);
        assert_eq!(proof.process_group_id(), process_group_id);
        assert_eq!(
            proof.process_start_identity(),
            identity.process_start_identity()
        );
        assert_eq!(format!("{proof:?}"), "ExactProcessGroupAbsence(REDACTED)");
        Ok(())
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn recorded_identity_inspection_rejects_malformed_expected_identity()
    -> Result<(), Box<dyn std::error::Error>> {
        let error = inspect_recorded_process_identity(
            std::process::id(),
            process_group_id(None)?,
            "unqualified identity",
        )
        .err()
        .ok_or("malformed expected identity was accepted")?;

        assert!(matches!(
            error,
            ProcessIdentityError::InvalidRecordedIdentity
        ));
        Ok(())
    }

    #[test]
    fn recorded_leader_classification_requires_two_stable_exact_observations() {
        let exact = || KernelObservation {
            pid: 41,
            process_group_id: 42,
            process_start_identity: "test:start:1".to_owned(),
        };
        let changed = || KernelObservation {
            pid: 41,
            process_group_id: 43,
            process_start_identity: "test:start:2".to_owned(),
        };

        let live = classify_recorded_observations(41, 42, "test:start:1", Ok(exact()), Ok(exact()));
        let absent =
            classify_recorded_observations(41, 42, "test:start:1", Ok(exact()), Ok(changed()));
        let unstable =
            classify_recorded_observations(41, 42, "test:start:1", Ok(changed()), Ok(exact()));

        assert!(matches!(live, Ok(RecordedLeaderIdentityStatus::ExactLive)));
        assert!(matches!(
            absent,
            Ok(RecordedLeaderIdentityStatus::ExactAbsent)
        ));
        assert!(matches!(
            unstable,
            Err(ProcessIdentityError::IdentityChanged)
        ));
    }

    #[test]
    fn recorded_group_classification_requires_two_absence_proofs() {
        let absent = classify_recorded_process_group(
            41,
            42,
            "test:start:1",
            Ok(ProcessGroupObservation::Absent),
            Ok(ProcessGroupObservation::Absent),
        );
        let appeared = classify_recorded_process_group(
            41,
            42,
            "test:start:1",
            Ok(ProcessGroupObservation::Absent),
            Ok(ProcessGroupObservation::PresentOrUnproven),
        );
        let permission_unproven = classify_recorded_process_group(
            41,
            42,
            "test:start:1",
            Ok(ProcessGroupObservation::PresentOrUnproven),
            Ok(ProcessGroupObservation::PresentOrUnproven),
        );
        let operation_failed = classify_recorded_process_group(
            41,
            42,
            "test:start:1",
            Err(ProcessIdentityError::operation(
                "test group probe",
                std::io::Error::other("injected probe failure"),
            )),
            Ok(ProcessGroupObservation::Absent),
        );

        assert!(matches!(
            absent,
            Ok(RecordedProcessIdentityStatus::ExactGroupAbsent(_))
        ));
        assert!(matches!(
            appeared,
            Ok(RecordedProcessIdentityStatus::LeaderAbsentGroupPresent)
        ));
        assert!(matches!(
            permission_unproven,
            Ok(RecordedProcessIdentityStatus::LeaderAbsentGroupPresent)
        ));
        assert!(matches!(
            operation_failed,
            Err(ProcessIdentityError::Operation { .. })
        ));
    }

    #[test]
    fn linux_stat_parser_accepts_embedded_closing_parentheses() {
        let stat = b"123 (worker ) name) S 1 456 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765 20";

        let record = parse_linux_process_stat(stat);

        assert_eq!(
            record,
            Some(LinuxProcessStat {
                pid: 123,
                state: b'S',
                process_group_id: 456,
                start_ticks: 98_765,
            })
        );
    }

    #[test]
    fn linux_stat_parser_rejects_truncated_fields() {
        let stat = b"123 (worker) S 1 456";

        let record = parse_linux_process_stat(stat);

        assert_eq!(record, None);
    }

    #[test]
    fn linux_stat_parser_rejects_non_numeric_start_ticks() {
        let stat = b"123 (worker) S 1 456 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 nope";

        let record = parse_linux_process_stat(stat);

        assert_eq!(record, None);
    }

    #[test]
    fn linux_stat_parser_rejects_nul_in_command() {
        let stat = b"123 (work\0er) S 1 456 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 98765";

        let record = parse_linux_process_stat(stat);

        assert_eq!(record, None);
    }

    #[test]
    fn linux_boot_id_parser_accepts_canonical_uuid_with_newline() {
        let boot_id = parse_linux_boot_id(b"12345678-90ab-cdef-1234-567890abcdef\n");

        assert_eq!(
            boot_id.as_deref(),
            Some("12345678-90ab-cdef-1234-567890abcdef")
        );
    }

    #[test]
    fn linux_boot_id_parser_rejects_uppercase_uuid() {
        let boot_id = parse_linux_boot_id(b"12345678-90ab-cdef-1234-567890abcdeF\n");

        assert_eq!(boot_id, None);
    }

    #[test]
    fn linux_boot_id_parser_rejects_all_zero_uuid() {
        let boot_id = parse_linux_boot_id(b"00000000-0000-0000-0000-000000000000\n");

        assert_eq!(boot_id, None);
    }

    #[test]
    fn linux_boot_id_parser_rejects_embedded_whitespace() {
        let boot_id = parse_linux_boot_id(b"12345678-90ab-cdef-1234-567890abcde \n");

        assert_eq!(boot_id, None);
    }
}
