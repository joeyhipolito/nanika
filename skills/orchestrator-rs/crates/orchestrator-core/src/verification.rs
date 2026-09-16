//! Structured verification classification and block/warn decisions.

const MAX_CHECK_COUNT: u64 = 1_000_000;

/// Closed-schema counts for required checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerificationSummary {
    pub discovered: u64,
    pub executed: u64,
    pub passed: u64,
    pub failed: u64,
    pub required_skipped: u64,
}

impl VerificationSummary {
    fn is_valid(self) -> bool {
        if [
            self.discovered,
            self.executed,
            self.passed,
            self.failed,
            self.required_skipped,
        ]
        .into_iter()
        .any(|count| count > MAX_CHECK_COUNT)
        {
            return false;
        }
        self.executed.checked_add(self.required_skipped) == Some(self.discovered)
            && self.passed.checked_add(self.failed) == Some(self.executed)
    }
}

/// Supervisor termination facts consumed by verification classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationTermination {
    Exited(i32),
    Signaled(i32),
    TimedOut,
    Stalled,
    Cancelled,
    InfrastructureError,
}

/// The six exhaustive quality-result classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationClass {
    Pass,
    Fail,
    Skip,
    NoTests,
    Timeout,
    InfrastructureError,
}

/// Cancellation bypasses quality classification and can never pass the gate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationOutcome {
    Classified(VerificationClass),
    Cancelled,
}

impl VerificationOutcome {
    /// Only a structured `Pass` outcome satisfies a required verification gate.
    #[must_use]
    pub const fn gate_passed(self) -> bool {
        matches!(self, Self::Classified(VerificationClass::Pass))
    }
}

/// Classifies supervisor facts without inspecting human output.
#[must_use]
pub fn classify_verification(
    termination: VerificationTermination,
    summary: Option<VerificationSummary>,
) -> VerificationOutcome {
    use VerificationClass::{Fail, InfrastructureError, NoTests, Pass, Skip, Timeout};
    use VerificationOutcome::{Cancelled, Classified};
    use VerificationTermination::{
        Cancelled as TermCancelled, Exited, InfrastructureError as HarnessError, Signaled, Stalled,
        TimedOut,
    };

    match termination {
        TimedOut | Stalled => return Classified(Timeout),
        HarnessError | Signaled(_) => return Classified(InfrastructureError),
        TermCancelled => return Cancelled,
        Exited(_) => {}
    }

    let Some(summary) = summary.filter(|summary| summary.is_valid()) else {
        return Classified(InfrastructureError);
    };
    match termination {
        Exited(code) if code != 0 && summary.failed > 0 => Classified(Fail),
        Exited(code) if code != 0 => Classified(InfrastructureError),
        Exited(0) if summary.failed > 0 => Classified(InfrastructureError),
        Exited(0) if summary.discovered == 0 => Classified(NoTests),
        Exited(0) if summary.required_skipped > 0 => Classified(Skip),
        Exited(0) if summary.executed > 0 && summary.passed == summary.executed => Classified(Pass),
        Exited(_) => Classified(InfrastructureError),
        TimedOut | Stalled | TermCancelled | HarnessError | Signaled(_) => {
            Classified(InfrastructureError)
        }
    }
}

/// Policy controlling whether a non-PASS outcome blocks later non-terminal work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationMode {
    Block,
    Warn,
}

/// Policy action kept distinct from the immutable verification classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VerificationAction {
    Continue,
    Block,
    Cancelled,
}

/// Verification policy result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VerificationDecision {
    outcome: VerificationOutcome,
    action: VerificationAction,
    gate_passed: bool,
    warning: bool,
}

impl VerificationDecision {
    #[must_use]
    pub const fn outcome(self) -> VerificationOutcome {
        self.outcome
    }

    #[must_use]
    pub const fn action(self) -> VerificationAction {
        self.action
    }

    #[must_use]
    pub const fn gate_passed(self) -> bool {
        self.gate_passed
    }

    #[must_use]
    pub const fn warning(self) -> bool {
        self.warning
    }
}

/// Applies block/warn policy without ever promoting a non-PASS result.
#[must_use]
pub const fn decide_verification(
    outcome: VerificationOutcome,
    mode: VerificationMode,
) -> VerificationDecision {
    use VerificationAction::{Block, Cancelled, Continue};
    let gate_passed = outcome.gate_passed();
    let action = match (outcome, mode) {
        (VerificationOutcome::Cancelled, _) => Cancelled,
        (VerificationOutcome::Classified(VerificationClass::Pass), _) => Continue,
        (VerificationOutcome::Classified(_), VerificationMode::Block) => Block,
        (VerificationOutcome::Classified(_), VerificationMode::Warn) => Continue,
    };
    VerificationDecision {
        outcome,
        action,
        gate_passed,
        warning: !gate_passed && matches!(action, Continue),
    }
}
