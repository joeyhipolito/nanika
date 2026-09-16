//! Total mapping from supervised production outcomes to durable evidence.

use crate::{
    EffectEvidence, EffectResolution, ProcessNotStartedEvidenceReason, ProcessUncertaintyEvidence,
    RuntimeStoreError, StartedProcessFailureEvidence,
    runtime_store::{
        AuthorizedLauncherIdentity, ClaimedProcessAttempt, PrivateProcessLedgerStore,
        ProcessAttemptBinding, ProcessTerminalClaim,
    },
};
use orchestrator_exec::{
    ProcessExitStatus, ProcessReceipt, ProcessRequest, ProcessTerminationReceipt,
    ServiceContractError,
};
use orchestrator_process::{
    AuthorizedProcessOutcome, KernelProcessIdentity, ProcessNotStartedReason, ProcessReport,
    ProcessTermination, ProcessUncertainReason,
};

/// Opaque pre-commit plan. Its receipt is inaccessible until the exact durable
/// resolution has either committed or been proven by an idempotent reread.
pub(crate) struct MappedProcessOutcome {
    binding: ProcessAttemptBinding,
    resolution: EffectResolution,
    receipt: ProcessReceipt,
}

impl MappedProcessOutcome {
    pub(crate) fn bind(
        self,
        claimed: ClaimedProcessAttempt,
    ) -> Result<PendingProcessOutcome, RuntimeStoreError> {
        self.bind_terminal(claimed.into_terminal())
    }

    pub(crate) fn bind_terminal(
        self,
        terminal: ProcessTerminalClaim,
    ) -> Result<PendingProcessOutcome, RuntimeStoreError> {
        if !terminal.matches_binding(&self.binding) {
            return Err(RuntimeStoreError::InvalidOutboxTransition);
        }
        Ok(PendingProcessOutcome {
            terminal,
            resolution: self.resolution,
            receipt: self.receipt,
        })
    }
}

/// One-shot terminal capability. It owns the non-cloneable exact claim, so a
/// second receipt plan cannot replay the same durable observation.
pub(crate) struct PendingProcessOutcome {
    terminal: ProcessTerminalClaim,
    resolution: EffectResolution,
    receipt: ProcessReceipt,
}

impl PendingProcessOutcome {
    pub(crate) fn commit(
        self,
        store: &mut PrivateProcessLedgerStore,
        observed_at_utc: &str,
    ) -> Result<ProcessReceipt, RuntimeStoreError> {
        let expected = self.resolution.clone();
        if store
            .resolve_claimed_process(&self.terminal, self.resolution, observed_at_utc)
            .is_ok()
        {
            return Ok(self.receipt);
        }
        let retry_error = match store.resolve_claimed_process(
            &self.terminal,
            expected.clone(),
            observed_at_utc,
        ) {
            Ok(_) => return Ok(self.receipt),
            Err(error) => error,
        };
        if store.claimed_process_resolution_is_durable(
            &self.terminal,
            &expected,
            observed_at_utc,
        )? {
            Ok(self.receipt)
        } else {
            Err(retry_error)
        }
    }
}

pub(crate) fn map_authorized_process_outcome(
    outcome: AuthorizedProcessOutcome,
    request: &ProcessRequest,
    authorized_identity: Option<AuthorizedLauncherIdentity>,
    attempt: &ClaimedProcessAttempt,
) -> Result<MappedProcessOutcome, ServiceContractError> {
    if !attempt.matches_request(request) {
        return Err(ServiceContractError::InvalidExitStatus);
    }
    map_authorized_process_outcome_bound(
        outcome,
        request,
        authorized_identity,
        attempt.outcome_binding(),
    )
}

fn map_authorized_process_outcome_bound(
    outcome: AuthorizedProcessOutcome,
    request: &ProcessRequest,
    authorized_identity: Option<AuthorizedLauncherIdentity>,
    binding: ProcessAttemptBinding,
) -> Result<MappedProcessOutcome, ServiceContractError> {
    let authorization_bound = authorized_identity
        .as_ref()
        .is_none_or(|identity| identity.matches_attempt_binding(&binding));
    match outcome {
        AuthorizedProcessOutcome::NotStarted(not_started) => {
            let authorization_consistent = authorization_bound
                && authorized_identity.as_ref().is_none_or(|expected| {
                    not_started.launcher_spawned
                        && not_started
                            .launcher_identity
                            .as_ref()
                            .is_some_and(|identity| authorized_matches_kernel(expected, identity))
                        && matches!(
                            not_started.reason,
                            ProcessNotStartedReason::Cancelled
                                | ProcessNotStartedReason::Deadline
                                | ProcessNotStartedReason::GateProtocol
                        )
                });
            if !authorization_consistent
                || !not_started_is_consistent(
                    not_started.reason,
                    not_started.launcher_spawned,
                    not_started.launcher_identity.as_ref(),
                    not_started.cancellation_observed,
                    not_started.deadline_observed,
                )
            {
                return uncertainty_plan(
                    binding,
                    ProcessUncertaintyEvidence::SupervisorOutcomeLost,
                    ProcessTerminationReceipt::SupervisorFailure,
                    Vec::new(),
                    Vec::new(),
                    0,
                    0,
                    true,
                    not_started.elapsed,
                    false,
                );
            }
            let termination = match not_started.reason {
                ProcessNotStartedReason::Cancelled => ProcessTerminationReceipt::Cancelled,
                ProcessNotStartedReason::Deadline => ProcessTerminationReceipt::DeadlineExceeded,
                ProcessNotStartedReason::SpawnFailed
                | ProcessNotStartedReason::GateRejected
                | ProcessNotStartedReason::GateIndeterminate
                | ProcessNotStartedReason::GateProtocol => {
                    ProcessTerminationReceipt::SupervisorFailure
                }
            };
            Ok(MappedProcessOutcome {
                binding,
                resolution: EffectResolution::NotStarted(EffectEvidence::process_not_started(
                    map_not_started_reason(not_started.reason),
                )),
                receipt: ProcessReceipt::new(
                    termination,
                    Vec::new(),
                    Vec::new(),
                    0,
                    0,
                    true,
                    not_started.elapsed,
                )?,
            })
        }
        AuthorizedProcessOutcome::Started(report) => map_started_report(
            report,
            request,
            authorized_identity.as_ref(),
            authorization_bound,
            binding,
        ),
        AuthorizedProcessOutcome::Uncertain(uncertain) => {
            let report = uncertain.report;
            let ownership_released = ownership_released(&report);
            let identity_consistent = authorization_bound
                && match (&authorized_identity, uncertain.reason) {
                    (Some(expected), ProcessUncertainReason::ReleaseDelivery)
                    | (Some(expected), ProcessUncertainReason::CleanupIncomplete(_)) => uncertain
                        .launcher_identity
                        .as_ref()
                        .is_some_and(|identity| {
                            authorized_matches_kernel(expected, identity)
                                && launcher_matches_report(identity, &report)
                        }),
                    (None, ProcessUncertainReason::CleanupIncomplete(_)) => uncertain
                        .launcher_identity
                        .as_ref()
                        .map_or(report.kernel_identity.is_none(), |identity| {
                            launcher_matches_report(identity, &report)
                        }),
                    (None, ProcessUncertainReason::UngatedExecution) => {
                        uncertain.launcher_identity.is_none()
                    }
                    (Some(_), ProcessUncertainReason::UngatedExecution)
                    | (None, ProcessUncertainReason::ReleaseDelivery) => false,
                };
            let context_consistent = report_context_is_consistent(&report)
                && !(matches!(
                    uncertain.reason,
                    ProcessUncertainReason::CleanupIncomplete(_)
                ) && ownership_released);
            let evidence = if !identity_consistent || !context_consistent {
                if ownership_released {
                    ProcessUncertaintyEvidence::SupervisorOutcomeLost
                } else {
                    ProcessUncertaintyEvidence::StartedOwnershipUnresolved
                }
            } else {
                match uncertain.reason {
                    ProcessUncertainReason::ReleaseDelivery => {
                        ProcessUncertaintyEvidence::ReleaseDelivery
                    }
                    ProcessUncertainReason::CleanupIncomplete(reason) => {
                        ProcessUncertaintyEvidence::CleanupIncomplete(map_not_started_reason(
                            reason,
                        ))
                    }
                    ProcessUncertainReason::UngatedExecution => {
                        ProcessUncertaintyEvidence::UngatedExecution
                    }
                }
            };
            let termination = if ownership_released {
                ProcessTerminationReceipt::SupervisorFailure
            } else {
                ProcessTerminationReceipt::UnresolvedOwnership
            };
            uncertainty_plan(
                binding,
                evidence,
                termination,
                report.stdout,
                report.stderr,
                report.stdout_discarded_bytes,
                report.stderr_discarded_bytes,
                ownership_released,
                report.elapsed,
                request.truncated_output_acknowledged(),
            )
        }
    }
}

fn map_started_report(
    report: ProcessReport,
    request: &ProcessRequest,
    authorized_identity: Option<&AuthorizedLauncherIdentity>,
    authorization_bound: bool,
    binding: ProcessAttemptBinding,
) -> Result<MappedProcessOutcome, ServiceContractError> {
    let ownership_released = ownership_released(&report);
    let exact_identity = authorization_bound
        && authorized_identity.is_some_and(|expected| started_identity_is_exact(&report, expected));
    let output_was_truncated = report.stdout_discarded_bytes != 0
        || report.stderr_discarded_bytes != 0
        || report.truncated;
    let retained_output_within_request = report.stdout.len() <= request.max_output_bytes()
        && report.stderr.len() <= request.max_output_bytes();

    let (resolution, termination) = if !ownership_released {
        (
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
            )),
            ProcessTerminationReceipt::UnresolvedOwnership,
        )
    } else if !exact_identity || !report_context_is_consistent(&report) {
        (
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::SupervisorOutcomeLost,
            )),
            ProcessTerminationReceipt::SupervisorFailure,
        )
    } else if !retained_output_within_request || !report.infrastructure_failures.is_empty() {
        process_failure(
            StartedProcessFailureEvidence::InfrastructureFailure,
            ProcessTerminationReceipt::SupervisorFailure,
        )?
    } else if output_was_truncated && !request.truncated_output_acknowledged() {
        process_failure(
            StartedProcessFailureEvidence::OutputLimit,
            ProcessTerminationReceipt::OutputLimit,
        )?
    } else {
        map_started_termination(report.termination)?
    };

    let mut receipt = ProcessReceipt::new(
        termination,
        report.stdout,
        report.stderr,
        report.stdout_discarded_bytes,
        report.stderr_discarded_bytes,
        ownership_released,
        report.elapsed,
    )?;
    if output_was_truncated && request.truncated_output_acknowledged() {
        receipt = receipt.with_truncated_output_acknowledged();
    }
    Ok(MappedProcessOutcome {
        binding,
        resolution,
        receipt,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "total mapping requires every retained supervision observation"
)]
fn uncertainty_plan(
    binding: ProcessAttemptBinding,
    evidence: ProcessUncertaintyEvidence,
    termination: ProcessTerminationReceipt,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_discarded: u64,
    stderr_discarded: u64,
    ownership_released: bool,
    elapsed: std::time::Duration,
    truncation_acknowledged: bool,
) -> Result<MappedProcessOutcome, ServiceContractError> {
    let mut receipt = ProcessReceipt::new(
        termination,
        stdout,
        stderr,
        stdout_discarded,
        stderr_discarded,
        ownership_released,
        elapsed,
    )?;
    if truncation_acknowledged && (stdout_discarded != 0 || stderr_discarded != 0) {
        receipt = receipt.with_truncated_output_acknowledged();
    }
    Ok(MappedProcessOutcome {
        binding,
        resolution: EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(evidence)),
        receipt,
    })
}

fn map_started_termination(
    termination: ProcessTermination,
) -> Result<(EffectResolution, ProcessTerminationReceipt), ServiceContractError> {
    Ok(match termination {
        ProcessTermination::Exited(0) => (
            EffectResolution::Succeeded(EffectEvidence::exit_observed_success()),
            ProcessTerminationReceipt::Exited(ProcessExitStatus::code(0)?),
        ),
        ProcessTermination::Exited(code) => (
            EffectResolution::Failed(
                EffectEvidence::exit_observed_failure(code).map_err(runtime_error_as_contract)?,
            ),
            ProcessTerminationReceipt::Exited(ProcessExitStatus::code(code)?),
        ),
        ProcessTermination::Signaled(signal) => (
            EffectResolution::ProcessFailed(
                EffectEvidence::process_failed(StartedProcessFailureEvidence::Signaled(signal))
                    .map_err(runtime_error_as_contract)?,
            ),
            ProcessTerminationReceipt::Exited(ProcessExitStatus::signal(signal)?),
        ),
        ProcessTermination::Timeout => process_failure(
            StartedProcessFailureEvidence::Deadline,
            ProcessTerminationReceipt::DeadlineExceeded,
        )?,
        ProcessTermination::Stalled => process_failure(
            StartedProcessFailureEvidence::Stalled,
            ProcessTerminationReceipt::Stalled,
        )?,
        ProcessTermination::Cancelled => process_failure(
            StartedProcessFailureEvidence::Cancelled,
            ProcessTerminationReceipt::Cancelled,
        )?,
        ProcessTermination::OutputLimit => process_failure(
            StartedProcessFailureEvidence::OutputLimit,
            ProcessTerminationReceipt::OutputLimit,
        )?,
        ProcessTermination::InfrastructureError => process_failure(
            StartedProcessFailureEvidence::InfrastructureFailure,
            ProcessTerminationReceipt::SupervisorFailure,
        )?,
        ProcessTermination::UnresolvedOwnership => (
            EffectResolution::ProcessUncertain(EffectEvidence::process_uncertain(
                ProcessUncertaintyEvidence::StartedOwnershipUnresolved,
            )),
            ProcessTerminationReceipt::UnresolvedOwnership,
        ),
    })
}

fn process_failure(
    evidence: StartedProcessFailureEvidence,
    receipt: ProcessTerminationReceipt,
) -> Result<(EffectResolution, ProcessTerminationReceipt), ServiceContractError> {
    Ok((
        EffectResolution::ProcessFailed(
            EffectEvidence::process_failed(evidence).map_err(runtime_error_as_contract)?,
        ),
        receipt,
    ))
}

const fn map_not_started_reason(
    reason: ProcessNotStartedReason,
) -> ProcessNotStartedEvidenceReason {
    match reason {
        ProcessNotStartedReason::Cancelled => ProcessNotStartedEvidenceReason::Cancelled,
        ProcessNotStartedReason::Deadline => ProcessNotStartedEvidenceReason::Deadline,
        ProcessNotStartedReason::SpawnFailed => ProcessNotStartedEvidenceReason::SpawnFailed,
        ProcessNotStartedReason::GateRejected => ProcessNotStartedEvidenceReason::GateRejected,
        ProcessNotStartedReason::GateIndeterminate => {
            ProcessNotStartedEvidenceReason::GateIndeterminate
        }
        ProcessNotStartedReason::GateProtocol => ProcessNotStartedEvidenceReason::GateProtocol,
    }
}

fn ownership_released(report: &ProcessReport) -> bool {
    report.cleanup_complete
        && report.group_absent
        && (!report.spawned || report.direct_child_reaped)
}

fn started_identity_is_exact(
    report: &ProcessReport,
    authorized_identity: &AuthorizedLauncherIdentity,
) -> bool {
    match (report.kernel_identity.as_ref(), report.pid, report.pgid) {
        (Some(identity), Some(pid), Some(pgid)) => {
            report.spawned
                && identity.pid() == pid
                && identity.process_group_id() == pgid
                && authorized_identity.matches(
                    identity.pid(),
                    identity.process_group_id(),
                    identity.process_start_identity(),
                )
        }
        _ => false,
    }
}

fn authorized_matches_kernel(
    authorized_identity: &AuthorizedLauncherIdentity,
    kernel_identity: &KernelProcessIdentity,
) -> bool {
    authorized_identity.matches(
        kernel_identity.pid(),
        kernel_identity.process_group_id(),
        kernel_identity.process_start_identity(),
    )
}

fn launcher_matches_report(launcher: &KernelProcessIdentity, report: &ProcessReport) -> bool {
    report.spawned
        && report.pid == Some(launcher.pid())
        && report.pgid == Some(launcher.process_group_id())
        && report.kernel_identity.as_ref().is_none_or(|identity| {
            identity.pid() == launcher.pid()
                && identity.process_group_id() == launcher.process_group_id()
                && identity.process_start_identity() == launcher.process_start_identity()
        })
}

fn report_context_is_consistent(report: &ProcessReport) -> bool {
    let discarded = report.stdout_discarded_bytes != 0 || report.stderr_discarded_bytes != 0;
    report.kill_sent == report.escalated_to_kill
        && (!matches!(report.termination, ProcessTermination::Cancelled)
            || report.cancellation_observed)
        && (!matches!(report.termination, ProcessTermination::Timeout) || report.deadline_observed)
        && (!matches!(report.termination, ProcessTermination::Stalled) || report.stall_observed)
        && (!matches!(report.termination, ProcessTermination::OutputLimit) || discarded)
        && (!matches!(report.termination, ProcessTermination::UnresolvedOwnership)
            || !ownership_released(report))
        && report.truncated == discarded
}

fn not_started_is_consistent(
    reason: ProcessNotStartedReason,
    launcher_spawned: bool,
    launcher_identity: Option<&KernelProcessIdentity>,
    cancellation_observed: bool,
    deadline_observed: bool,
) -> bool {
    let launcher_consistent = launcher_spawned || launcher_identity.is_none();
    launcher_consistent
        && match reason {
            ProcessNotStartedReason::Cancelled => cancellation_observed,
            ProcessNotStartedReason::Deadline => deadline_observed,
            ProcessNotStartedReason::SpawnFailed => !launcher_spawned,
            ProcessNotStartedReason::GateRejected | ProcessNotStartedReason::GateIndeterminate => {
                launcher_spawned && launcher_identity.is_some()
            }
            ProcessNotStartedReason::GateProtocol => launcher_spawned,
        }
}

fn runtime_error_as_contract(_: RuntimeStoreError) -> ServiceContractError {
    ServiceContractError::InvalidExitStatus
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EffectEvidenceCode, OutboxState};
    use orchestrator_exec::{ProcessPurpose, ProcessTerminationReceipt};
    use orchestrator_process::{
        ProcessInfrastructureFailure, ProcessNotStartedReceipt, ProcessUncertainReceipt,
    };
    use rustix::process::getpgid;
    use std::time::Duration;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn map_authorized_process_outcome(
        outcome: AuthorizedProcessOutcome,
        request: &ProcessRequest,
        mut authorized_identity: Option<AuthorizedLauncherIdentity>,
    ) -> Result<MappedProcessOutcome, ServiceContractError> {
        let binding = ProcessAttemptBinding::for_test(request.fingerprint(), 1)
            .map_err(runtime_error_as_contract)?;
        if let Some(identity) = &mut authorized_identity {
            identity.bind_for_test(&binding);
        }
        super::map_authorized_process_outcome_bound(outcome, request, authorized_identity, binding)
    }

    fn request(
        cap: usize,
        acknowledge_truncation: bool,
    ) -> Result<ProcessRequest, ServiceContractError> {
        let request = ProcessRequest::new(
            ProcessPurpose::ProviderWorker,
            "fixture-provider",
            "/fixture/workspace",
        )?
        .with_max_output_bytes(cap)?;
        Ok(if acknowledge_truncation {
            request.with_truncated_output_acknowledged()
        } else {
            request
        })
    }

    fn identity() -> Result<KernelProcessIdentity, Box<dyn std::error::Error>> {
        let pgid = u32::try_from(getpgid(None)?.as_raw_pid())?;
        Ok(KernelProcessIdentity::observe(std::process::id(), pgid)?)
    }

    fn report(
        termination: ProcessTermination,
    ) -> Result<ProcessReport, Box<dyn std::error::Error>> {
        let identity = identity()?;
        Ok(ProcessReport {
            termination,
            stdout: b"ok".to_vec(),
            stderr: Vec::new(),
            truncated: false,
            stdout_discarded_bytes: 0,
            stderr_discarded_bytes: 0,
            elapsed: Duration::from_millis(3),
            spawned: true,
            pid: Some(identity.pid()),
            pgid: Some(identity.process_group_id()),
            kernel_identity: Some(identity),
            cancellation_observed: termination == ProcessTermination::Cancelled,
            deadline_observed: termination == ProcessTermination::Timeout,
            stall_observed: termination == ProcessTermination::Stalled,
            term_sent: false,
            kill_sent: false,
            escalated_to_kill: false,
            direct_child_reaped: true,
            group_absent: true,
            cleanup_complete: true,
            infrastructure_failures: if termination == ProcessTermination::InfrastructureError {
                vec![ProcessInfrastructureFailure::GroupControl]
            } else {
                Vec::new()
            },
        })
    }

    fn authorized_identity(
        report: &ProcessReport,
    ) -> Result<AuthorizedLauncherIdentity, Box<dyn std::error::Error>> {
        let identity = report
            .kernel_identity
            .as_ref()
            .ok_or("test report is missing its kernel identity")?;
        Ok(AuthorizedLauncherIdentity::from_kernel_identity(
            identity,
            request(32, false)?.fingerprint(),
        ))
    }

    fn resolution_evidence(plan: &MappedProcessOutcome) -> (&EffectEvidence, OutboxState) {
        match &plan.resolution {
            EffectResolution::Succeeded(evidence) => (evidence, OutboxState::Succeeded),
            EffectResolution::Failed(evidence)
            | EffectResolution::ProcessFailed(evidence)
            | EffectResolution::NotStarted(evidence) => (evidence, OutboxState::Failed),
            EffectResolution::Uncertain(evidence)
            | EffectResolution::ProcessUncertain(evidence) => (evidence, OutboxState::Uncertain),
            EffectResolution::ObservedAbsent(_)
            | EffectResolution::RetryFailed(_)
            | EffectResolution::OperatorAuthorizedRetry(_) => {
                unreachable!("process mapper produced a retry resolution")
            }
        }
    }

    #[test]
    fn clean_started_exit_maps_to_success() -> TestResult {
        let report = report(ProcessTermination::Exited(0))?;
        let authorized = authorized_identity(&report)?;
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(report),
            &request(32, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Succeeded);
        assert_eq!(evidence.code(), EffectEvidenceCode::ExitObservedSuccess);
        assert!(plan.receipt.is_success());
        Ok(())
    }

    #[test]
    fn request_specific_output_cap_violation_is_finite_infrastructure_failure() -> TestResult {
        let mut oversized = report(ProcessTermination::Exited(0))?;
        let authorized = authorized_identity(&oversized)?;
        oversized.stdout = b"five!".to_vec();
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(oversized),
            &request(4, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Failed);
        assert_eq!(
            evidence.started_process_failure(),
            Some(StartedProcessFailureEvidence::InfrastructureFailure)
        );
        assert_eq!(
            plan.receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );
        Ok(())
    }

    #[test]
    fn acknowledged_truncation_is_explicit_and_policy_consistent() -> TestResult {
        for acknowledged in [false, true] {
            let mut truncated = report(ProcessTermination::Exited(0))?;
            let authorized = authorized_identity(&truncated)?;
            truncated.truncated = true;
            truncated.stdout_discarded_bytes = 7;
            let plan = map_authorized_process_outcome(
                AuthorizedProcessOutcome::Started(truncated),
                &request(32, acknowledged)?,
                Some(authorized),
            )?;
            let (evidence, state) = resolution_evidence(&plan);
            if acknowledged {
                assert_eq!(state, OutboxState::Succeeded);
                assert_eq!(evidence.code(), EffectEvidenceCode::ExitObservedSuccess);
                assert!(plan.receipt.truncated_output_acknowledged());
                assert!(plan.receipt.is_success());
            } else {
                assert_eq!(state, OutboxState::Failed);
                assert_eq!(
                    evidence.started_process_failure(),
                    Some(StartedProcessFailureEvidence::OutputLimit)
                );
                assert_eq!(
                    plan.receipt.termination(),
                    ProcessTerminationReceipt::OutputLimit
                );
            }
        }
        Ok(())
    }

    #[test]
    fn exact_identity_mismatch_never_becomes_success_or_finite_failure() -> TestResult {
        let mut mismatch = report(ProcessTermination::Exited(0))?;
        let authorized = authorized_identity(&mismatch)?;
        mismatch.pid = mismatch.pid.and_then(|pid| pid.checked_add(1));
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(mismatch),
            &request(32, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Uncertain);
        assert_eq!(
            evidence.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        assert_eq!(
            plan.receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );
        assert!(plan.receipt.ownership_released());
        Ok(())
    }

    #[test]
    fn self_consistent_report_from_a_different_released_process_is_uncertain() -> TestResult {
        let report = report(ProcessTermination::Exited(0))?;
        let kernel = report
            .kernel_identity
            .as_ref()
            .ok_or("test report is missing its kernel identity")?;
        let different_pid = kernel.pid().checked_add(1).ok_or("pid overflow")?;
        let authorized = AuthorizedLauncherIdentity::from_test_parts(
            different_pid,
            kernel.process_group_id(),
            kernel.process_start_identity(),
            request(32, false)?.fingerprint(),
        );
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(report),
            &request(32, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Uncertain);
        assert_eq!(
            evidence.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        assert_eq!(
            plan.receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );
        Ok(())
    }

    #[test]
    fn actual_cleanup_gap_is_the_only_started_unresolved_ownership_mapping() -> TestResult {
        let mut unresolved = report(ProcessTermination::UnresolvedOwnership)?;
        let authorized = authorized_identity(&unresolved)?;
        unresolved.cleanup_complete = false;
        unresolved.group_absent = false;
        unresolved.direct_child_reaped = false;
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(unresolved),
            &request(32, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Uncertain);
        assert_eq!(
            evidence.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::StartedOwnershipUnresolved)
        );
        assert_eq!(
            plan.receipt.termination(),
            ProcessTerminationReceipt::UnresolvedOwnership
        );
        assert!(!plan.receipt.ownership_released());
        Ok(())
    }

    #[test]
    fn unresolved_ownership_with_proven_cleanup_is_outcome_lost() -> TestResult {
        let report = report(ProcessTermination::UnresolvedOwnership)?;
        let authorized = authorized_identity(&report)?;
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(report),
            &request(32, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Uncertain);
        assert_eq!(
            evidence.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        assert_eq!(
            plan.receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );
        assert!(plan.receipt.ownership_released());
        Ok(())
    }

    #[test]
    fn cancellation_after_authorization_remains_proven_not_started() -> TestResult {
        let identity = identity()?;
        let authorized = AuthorizedLauncherIdentity::from_kernel_identity(
            &identity,
            request(32, false)?.fingerprint(),
        );
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::NotStarted(ProcessNotStartedReceipt {
                reason: ProcessNotStartedReason::Cancelled,
                launcher_spawned: true,
                launcher_identity: Some(identity),
                elapsed: Duration::from_millis(3),
                cancellation_observed: true,
                deadline_observed: false,
            }),
            &request(32, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Failed);
        assert_eq!(
            evidence.process_not_started_reason(),
            Some(ProcessNotStartedEvidenceReason::Cancelled)
        );
        assert_eq!(
            plan.receipt.termination(),
            ProcessTerminationReceipt::Cancelled
        );
        assert!(plan.receipt.ownership_released());
        Ok(())
    }

    #[test]
    fn release_delivery_uncertainty_with_closed_ownership_is_not_unresolved_ownership() -> TestResult
    {
        let report = report(ProcessTermination::Exited(0))?;
        let authorized = authorized_identity(&report)?;
        let launcher_identity = report.kernel_identity.clone();
        let plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Uncertain(ProcessUncertainReceipt {
                reason: ProcessUncertainReason::ReleaseDelivery,
                launcher_identity,
                report,
            }),
            &request(32, false)?,
            Some(authorized),
        )?;
        let (evidence, state) = resolution_evidence(&plan);
        assert_eq!(state, OutboxState::Uncertain);
        assert_eq!(
            evidence.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::ReleaseDelivery)
        );
        assert_eq!(
            plan.receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );
        assert!(plan.receipt.ownership_released());
        Ok(())
    }

    #[test]
    fn contradictory_uncertain_identity_and_cleanup_are_normalized() -> TestResult {
        let mut mismatched = report(ProcessTermination::Exited(0))?;
        let authorized = authorized_identity(&mismatched)?;
        let launcher_identity = mismatched.kernel_identity.clone();
        mismatched.pgid = mismatched.pgid.and_then(|pgid| pgid.checked_add(1));
        let identity_plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Uncertain(ProcessUncertainReceipt {
                reason: ProcessUncertainReason::ReleaseDelivery,
                launcher_identity,
                report: mismatched,
            }),
            &request(32, false)?,
            Some(authorized),
        )?;
        assert_eq!(
            resolution_evidence(&identity_plan).0.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );

        let closed = report(ProcessTermination::Exited(0))?;
        let authorized = authorized_identity(&closed)?;
        let cleanup_plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Uncertain(ProcessUncertainReceipt {
                reason: ProcessUncertainReason::CleanupIncomplete(
                    ProcessNotStartedReason::GateProtocol,
                ),
                launcher_identity: closed.kernel_identity.clone(),
                report: closed,
            }),
            &request(32, false)?,
            Some(authorized),
        )?;
        assert_eq!(
            resolution_evidence(&cleanup_plan).0.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        assert_eq!(
            cleanup_plan.receipt.termination(),
            ProcessTerminationReceipt::SupervisorFailure
        );

        let mut not_spawned = report(ProcessTermination::Exited(0))?;
        let authorized = authorized_identity(&not_spawned)?;
        let launcher_identity = not_spawned.kernel_identity.take();
        not_spawned.spawned = false;
        not_spawned.pid = None;
        not_spawned.pgid = None;
        not_spawned.direct_child_reaped = false;
        let impossible_plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Uncertain(ProcessUncertainReceipt {
                reason: ProcessUncertainReason::ReleaseDelivery,
                launcher_identity,
                report: not_spawned,
            }),
            &request(32, false)?,
            Some(authorized),
        )?;
        assert_eq!(
            resolution_evidence(&impossible_plan)
                .0
                .process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        assert!(impossible_plan.receipt.ownership_released());
        Ok(())
    }

    #[test]
    fn cleanup_incomplete_preserves_pre_release_and_authorized_contexts() -> TestResult {
        for authorized in [false, true] {
            let mut incomplete = report(ProcessTermination::Exited(0))?;
            incomplete.cleanup_complete = false;
            incomplete.group_absent = false;
            incomplete.direct_child_reaped = false;
            let expected = authorized
                .then(|| authorized_identity(&incomplete))
                .transpose()?;
            let launcher_identity = incomplete.kernel_identity.clone();
            let reason = if authorized {
                ProcessNotStartedReason::GateProtocol
            } else {
                ProcessNotStartedReason::GateRejected
            };
            let plan = map_authorized_process_outcome(
                AuthorizedProcessOutcome::Uncertain(ProcessUncertainReceipt {
                    reason: ProcessUncertainReason::CleanupIncomplete(reason),
                    launcher_identity,
                    report: incomplete,
                }),
                &request(32, false)?,
                expected,
            )?;
            let (evidence, state) = resolution_evidence(&plan);
            assert_eq!(state, OutboxState::Uncertain);
            assert_eq!(
                evidence.process_uncertainty(),
                Some(ProcessUncertaintyEvidence::CleanupIncomplete(
                    map_not_started_reason(reason)
                ))
            );
            assert_eq!(
                plan.receipt.termination(),
                ProcessTerminationReceipt::UnresolvedOwnership
            );
            assert!(!plan.receipt.ownership_released());
        }
        Ok(())
    }

    #[test]
    fn contradictory_terminal_flags_and_legacy_output_limit_are_outcome_lost() -> TestResult {
        let mut cancelled = report(ProcessTermination::Cancelled)?;
        let authorized = authorized_identity(&cancelled)?;
        cancelled.cancellation_observed = false;
        let cancelled_plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(cancelled),
            &request(32, false)?,
            Some(authorized),
        )?;
        assert_eq!(
            resolution_evidence(&cancelled_plan).0.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );

        let output_limit = report(ProcessTermination::OutputLimit)?;
        let authorized = authorized_identity(&output_limit)?;
        let output_plan = map_authorized_process_outcome(
            AuthorizedProcessOutcome::Started(output_limit),
            &request(32, false)?,
            Some(authorized),
        )?;
        assert_eq!(
            resolution_evidence(&output_plan).0.process_uncertainty(),
            Some(ProcessUncertaintyEvidence::SupervisorOutcomeLost)
        );
        Ok(())
    }
}
