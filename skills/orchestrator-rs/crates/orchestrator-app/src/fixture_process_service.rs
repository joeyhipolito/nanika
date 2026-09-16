//! Fixture-only adapter from execution intents to the owned process authority.

use crate::{
    CancellationToken, FixtureProcessAuthority, FixtureProcessSpec, OwnedChildState, ProcessReport,
    ProcessTermination, SupervisorError,
};
use orchestrator_exec::{
    Cancellation, ProcessBudget, ProcessExitStatus, ProcessPreflight, ProcessPurpose,
    ProcessReceipt, ProcessRequest, ProcessService, ProcessServiceError, ProcessServiceErrorKind,
    ProcessTerminationReceipt, ServiceContractError,
};
use std::{ffi::OsString, fmt, path::PathBuf, time::Duration};

struct FixtureProcessServiceErrors {
    denied: ProcessServiceError,
    outside_root: ProcessServiceError,
    not_enrolled: ProcessServiceError,
    invalid: ProcessServiceError,
    spawn: ProcessServiceError,
    unavailable: ProcessServiceError,
}

impl FixtureProcessServiceErrors {
    fn new() -> Result<Self, ServiceContractError> {
        Ok(Self {
            denied: ProcessServiceError::new(
                ProcessServiceErrorKind::Denied,
                "fixture process request is outside its enrolled helper policy",
            )?,
            outside_root: ProcessServiceError::new(
                ProcessServiceErrorKind::OutsideRoot,
                "fixture process request names an unenrolled working root",
            )?,
            not_enrolled: ProcessServiceError::new(
                ProcessServiceErrorKind::NotEnrolled,
                "fixture process request names an unenrolled executable",
            )?,
            invalid: ProcessServiceError::new(
                ProcessServiceErrorKind::InvalidRequest,
                "fixture process request cannot satisfy its bounded policy",
            )?,
            spawn: ProcessServiceError::new(
                ProcessServiceErrorKind::Spawn,
                "fixture process authority could not produce a complete receipt",
            )?,
            unavailable: ProcessServiceError::new(
                ProcessServiceErrorKind::Unavailable,
                "fixture process authority is unavailable",
            )?,
        })
    }
}

/// Attempt-bound fixture adapter for one enrolled helper and workspace CWD.
///
/// The adapter admits only `ProviderWorker`, the authority's exact executable
/// ID and lexical workspace root, an empty caller environment, and the bounded
/// arguments/stdin/output/deadline already validated by `orchestrator-exec`.
/// HOME, TMPDIR, PATH, and the fixture protocol environment remain owned by
/// [`FixtureProcessAuthority`]. This type has no production constructor.
pub struct FixtureProcessService {
    authority: FixtureProcessAuthority,
    cancellation: CancellationToken,
    executable_id: String,
    working_root: PathBuf,
    errors: FixtureProcessServiceErrors,
}

impl FixtureProcessService {
    pub fn new(
        authority: FixtureProcessAuthority,
        cancellation: CancellationToken,
    ) -> Result<Self, ServiceContractError> {
        let (executable_id, working_root) = authority.process_service_binding();
        Ok(Self {
            executable_id: executable_id.to_owned(),
            working_root,
            authority,
            cancellation,
            errors: FixtureProcessServiceErrors::new()?,
        })
    }

    /// Returns a clone of the exact token wired into every admitted child.
    #[must_use]
    pub fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }

    /// Monotonically cancels this attempt and wakes any admitted child.
    pub fn cancel(&self) -> bool {
        self.cancellation.cancel()
    }

    #[must_use]
    pub fn has_unresolved_processes(&self) -> bool {
        self.authority.has_unresolved_processes()
    }

    pub(crate) fn working_root(&self) -> &std::path::Path {
        &self.working_root
    }

    fn admit_request(&self, request: &ProcessRequest) -> Result<(), ProcessServiceError> {
        if request.purpose() != ProcessPurpose::ProviderWorker
            || !request.expose_environment().is_empty()
        {
            return Err(self.errors.denied.clone());
        }
        if request.executable_id() != self.executable_id {
            return Err(self.errors.not_enrolled.clone());
        }
        if request.working_root() != self.working_root {
            return Err(self.errors.outside_root.clone());
        }
        Ok(())
    }

    fn deadline_receipt(&self) -> Result<ProcessReceipt, ProcessServiceError> {
        ProcessReceipt::new(
            ProcessTerminationReceipt::DeadlineExceeded,
            Vec::new(),
            Vec::new(),
            0,
            0,
            true,
            Duration::ZERO,
        )
        .map_err(|_| self.errors.invalid.clone())
    }

    fn cancelled_receipt(&self) -> Result<ProcessReceipt, ProcessServiceError> {
        ProcessReceipt::new(
            ProcessTerminationReceipt::Cancelled,
            Vec::new(),
            Vec::new(),
            0,
            0,
            true,
            Duration::ZERO,
        )
        .map_err(|_| self.errors.invalid.clone())
    }

    fn map_report(
        &self,
        report: ProcessReport,
        truncated_output_acknowledged: bool,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        map_process_report(report, truncated_output_acknowledged, &self.errors)
    }

    fn map_supervisor_error(&self, error: SupervisorError) -> ProcessServiceError {
        match error {
            SupervisorError::AuthorityMismatch => self.errors.unavailable.clone(),
            SupervisorError::InvalidLimits => self.errors.invalid.clone(),
            SupervisorError::Capability => self.errors.denied.clone(),
            SupervisorError::Process(crate::ProcessError::InvalidSpec) => {
                self.errors.invalid.clone()
            }
            SupervisorError::Process(crate::ProcessError::Spawn(_)) => self.errors.spawn.clone(),
        }
    }
}

fn map_process_report(
    report: ProcessReport,
    truncated_output_acknowledged: bool,
    errors: &FixtureProcessServiceErrors,
) -> Result<ProcessReceipt, ProcessServiceError> {
    let mut termination = match report.termination {
        ProcessTermination::Exited(code) => ProcessTerminationReceipt::Exited(
            ProcessExitStatus::code(code).map_err(|_| errors.spawn.clone())?,
        ),
        ProcessTermination::Signaled(signal) => ProcessTerminationReceipt::Exited(
            ProcessExitStatus::signal(signal).map_err(|_| errors.spawn.clone())?,
        ),
        ProcessTermination::Timeout => ProcessTerminationReceipt::DeadlineExceeded,
        ProcessTermination::Stalled => ProcessTerminationReceipt::Stalled,
        ProcessTermination::Cancelled => ProcessTerminationReceipt::Cancelled,
        ProcessTermination::OutputLimit => ProcessTerminationReceipt::OutputLimit,
        ProcessTermination::InfrastructureError => ProcessTerminationReceipt::SupervisorFailure,
        ProcessTermination::UnresolvedOwnership => ProcessTerminationReceipt::UnresolvedOwnership,
    };
    let ownership_consistent = if report.spawned {
        report.pid.is_some()
            && report.pgid.is_some()
            && (!report.cleanup_complete || (report.direct_child_reaped && report.group_absent))
    } else {
        report.pid.is_none()
            && report.pgid.is_none()
            && !report.term_sent
            && !report.kill_sent
            && !report.direct_child_reaped
            && report.group_absent
            && report.cleanup_complete
    };
    let cleanup_consistent = report.kill_sent == report.escalated_to_kill
        && ownership_consistent
        && (!matches!(termination, ProcessTerminationReceipt::Cancelled)
            || report.cancellation_observed)
        && (!matches!(termination, ProcessTerminationReceipt::DeadlineExceeded)
            || report.deadline_observed)
        && (!matches!(termination, ProcessTerminationReceipt::Stalled) || report.stall_observed)
        && (report.truncated
            == (report.stdout_discarded_bytes != 0 || report.stderr_discarded_bytes != 0));
    if !cleanup_consistent || !report.infrastructure_failures.is_empty() {
        termination = if report.cleanup_complete {
            ProcessTerminationReceipt::SupervisorFailure
        } else {
            ProcessTerminationReceipt::UnresolvedOwnership
        };
    }
    let ownership_released = report.cleanup_complete
        && report.group_absent
        && (!report.spawned || report.direct_child_reaped);
    let discarded = report.stdout_discarded_bytes != 0 || report.stderr_discarded_bytes != 0;
    let mut receipt = ProcessReceipt::new(
        termination,
        report.stdout,
        report.stderr,
        report.stdout_discarded_bytes,
        report.stderr_discarded_bytes,
        ownership_released,
        report.elapsed,
    )
    .map_err(|_| errors.spawn.clone())?;
    if discarded && truncated_output_acknowledged {
        receipt = receipt.with_truncated_output_acknowledged();
    }
    Ok(receipt)
}

impl fmt::Debug for FixtureProcessService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FixtureProcessService")
            .field("kind", &"attempt-bound-fixture-process-service")
            .field("cancelled", &self.cancellation.is_cancelled())
            .field("has_unresolved_processes", &self.has_unresolved_processes())
            .finish()
    }
}

impl Cancellation for FixtureProcessService {
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

impl ProcessService for FixtureProcessService {
    fn finish_preflight(
        &self,
        request: &ProcessRequest,
        preflight: ProcessPreflight<'_>,
    ) -> Result<(), ProcessServiceError> {
        self.admit_request(request)?;
        if preflight.bind(self, request).is_some() {
            Ok(())
        } else {
            Err(self.errors.invalid.clone())
        }
    }

    fn execute(
        &self,
        request: &ProcessRequest,
        budget: ProcessBudget,
    ) -> Result<ProcessReceipt, ProcessServiceError> {
        self.admit_request(request)?;
        if self.cancellation.is_cancelled() {
            return self.cancelled_receipt();
        }
        if budget.remaining().is_zero() {
            return self.deadline_receipt();
        }
        let mut specification = FixtureProcessSpec::new(
            request.expose_arguments().iter().map(OsString::from),
            budget.remaining(),
        )
        .map_err(|_| self.errors.invalid.clone())?
        .with_hard_deadline_at(budget.hard_deadline())
        .with_max_output_bytes(request.max_output_bytes())
        .with_cancellation(self.cancellation.clone());
        if !budget.stall_window().is_zero() {
            specification = specification
                .with_stall_timeout(budget.stall_window())
                .map_err(|_| self.errors.invalid.clone())?;
        }
        if let Some(stdin) = request.expose_stdin() {
            specification = specification.with_stdin(stdin.to_vec());
        }
        let report = self
            .authority
            .run(&specification)
            .map_err(|error| self.map_supervisor_error(error))?;
        self.map_report(report.process, request.truncated_output_acknowledged())
    }
}

impl OwnedChildState for FixtureProcessService {
    fn has_unresolved_children(&self) -> bool {
        self.has_unresolved_processes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcessInfrastructureFailure;

    fn report(index: u8, termination: ProcessTermination) -> ProcessReport {
        let cleanup_complete = termination != ProcessTermination::UnresolvedOwnership;
        ProcessReport {
            termination,
            stdout: vec![b'o', index],
            stderr: vec![b'e', index],
            truncated: true,
            stdout_discarded_bytes: u64::from(index) + 1,
            stderr_discarded_bytes: u64::from(index) + 2,
            elapsed: Duration::from_millis(u64::from(index) + 3),
            spawned: true,
            pid: Some(1000 + u32::from(index)),
            pgid: Some(1000 + u32::from(index)),
            kernel_identity: None,
            cancellation_observed: termination == ProcessTermination::Cancelled,
            deadline_observed: termination == ProcessTermination::Timeout,
            stall_observed: termination == ProcessTermination::Stalled,
            term_sent: false,
            kill_sent: false,
            escalated_to_kill: false,
            direct_child_reaped: cleanup_complete,
            group_absent: cleanup_complete,
            cleanup_complete,
            infrastructure_failures: if termination == ProcessTermination::InfrastructureError {
                vec![ProcessInfrastructureFailure::GroupControl]
            } else {
                Vec::new()
            },
        }
    }

    #[test]
    fn process_termination_mapping_table() -> Result<(), Box<dyn std::error::Error>> {
        let errors = FixtureProcessServiceErrors::new()?;
        let rows = [
            (
                ProcessTermination::Exited(0),
                ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?),
            ),
            (
                ProcessTermination::Exited(7),
                ProcessTerminationReceipt::Exited(ProcessExitStatus::code(7)?),
            ),
            (
                ProcessTermination::Signaled(9),
                ProcessTerminationReceipt::Exited(ProcessExitStatus::signal(9)?),
            ),
            (
                ProcessTermination::Timeout,
                ProcessTerminationReceipt::DeadlineExceeded,
            ),
            (
                ProcessTermination::Stalled,
                ProcessTerminationReceipt::Stalled,
            ),
            (
                ProcessTermination::Cancelled,
                ProcessTerminationReceipt::Cancelled,
            ),
            (
                ProcessTermination::OutputLimit,
                ProcessTerminationReceipt::OutputLimit,
            ),
            (
                ProcessTermination::InfrastructureError,
                ProcessTerminationReceipt::SupervisorFailure,
            ),
            (
                ProcessTermination::UnresolvedOwnership,
                ProcessTerminationReceipt::UnresolvedOwnership,
            ),
        ];

        for (index, (termination, expected)) in rows.into_iter().enumerate() {
            let index = u8::try_from(index)?;
            let source = report(index, termination);
            let expected_stdout = source.stdout.clone();
            let expected_stderr = source.stderr.clone();
            let stdout_discarded = source.stdout_discarded_bytes;
            let stderr_discarded = source.stderr_discarded_bytes;
            let elapsed = source.elapsed;
            let ownership_released = source.cleanup_complete;

            let receipt = map_process_report(source, false, &errors)?;

            assert_eq!(receipt.termination(), expected);
            assert_eq!(receipt.expose_stdout(), expected_stdout);
            assert_eq!(receipt.expose_stderr(), expected_stderr);
            assert_eq!(receipt.stdout_discarded(), stdout_discarded);
            assert_eq!(receipt.stderr_discarded(), stderr_discarded);
            assert_eq!(receipt.elapsed(), elapsed);
            assert_eq!(receipt.ownership_released(), ownership_released);
        }
        Ok(())
    }

    #[test]
    fn not_spawned_deadline_is_released_without_reap_evidence()
    -> Result<(), Box<dyn std::error::Error>> {
        let errors = FixtureProcessServiceErrors::new()?;
        let source = ProcessReport {
            termination: ProcessTermination::Timeout,
            stdout: Vec::new(),
            stderr: Vec::new(),
            truncated: false,
            stdout_discarded_bytes: 0,
            stderr_discarded_bytes: 0,
            elapsed: Duration::ZERO,
            spawned: false,
            pid: None,
            pgid: None,
            kernel_identity: None,
            cancellation_observed: false,
            deadline_observed: true,
            stall_observed: false,
            term_sent: false,
            kill_sent: false,
            escalated_to_kill: false,
            direct_child_reaped: false,
            group_absent: true,
            cleanup_complete: true,
            infrastructure_failures: Vec::new(),
        };

        let receipt = map_process_report(source, false, &errors)?;

        assert_eq!(
            receipt.termination(),
            ProcessTerminationReceipt::DeadlineExceeded
        );
        assert!(receipt.ownership_released());
        Ok(())
    }
}
