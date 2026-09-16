use orchestrator_core::{
    VerificationAction, VerificationClass, VerificationMode, VerificationOutcome,
    VerificationSummary, VerificationTermination, classify_verification, decide_verification,
};

fn summary(
    discovered: u64,
    executed: u64,
    passed: u64,
    failed: u64,
    required_skipped: u64,
) -> VerificationSummary {
    VerificationSummary {
        discovered,
        executed,
        passed,
        failed,
        required_skipped,
    }
}

#[test]
fn verification_classification_table() {
    use VerificationClass::{Fail, InfrastructureError, NoTests, Pass, Skip, Timeout};
    use VerificationOutcome::{Cancelled, Classified};
    use VerificationTermination::{
        Cancelled as TermCancelled, Exited, InfrastructureError as HarnessError, Signaled, Stalled,
        TimedOut,
    };

    let cases = [
        (
            "VR-PASS",
            Exited(0),
            Some(summary(2, 2, 2, 0, 0)),
            Classified(Pass),
        ),
        (
            "VR-FAIL",
            Exited(1),
            Some(summary(2, 2, 1, 1, 0)),
            Classified(Fail),
        ),
        (
            "VR-SKIP-ONLY",
            Exited(0),
            Some(summary(2, 0, 0, 0, 2)),
            Classified(Skip),
        ),
        (
            "VR-SKIP-MIXED",
            Exited(0),
            Some(summary(3, 2, 2, 0, 1)),
            Classified(Skip),
        ),
        (
            "VR-NO-TESTS",
            Exited(0),
            Some(summary(0, 0, 0, 0, 0)),
            Classified(NoTests),
        ),
        ("VR-COMMAND-TIMEOUT", TimedOut, None, Classified(Timeout)),
        (
            "VR-STALL-TIMEOUT",
            Stalled,
            Some(summary(1, 1, 1, 0, 0)),
            Classified(Timeout),
        ),
        ("VR-CANCELLED", TermCancelled, None, Cancelled),
        (
            "VR-NONZERO-NO-FAIL-EVIDENCE",
            Exited(2),
            None,
            Classified(InfrastructureError),
        ),
        (
            "VR-ZERO-WITH-FAILURES",
            Exited(0),
            Some(summary(1, 1, 0, 1, 0)),
            Classified(InfrastructureError),
        ),
        (
            "VR-SIGNAL",
            Signaled(9),
            Some(summary(1, 1, 0, 1, 0)),
            Classified(InfrastructureError),
        ),
        (
            "VR-HARNESS-ERROR",
            HarnessError,
            Some(summary(1, 1, 1, 0, 0)),
            Classified(InfrastructureError),
        ),
        (
            "VR-MISSING-SUMMARY",
            Exited(0),
            None,
            Classified(InfrastructureError),
        ),
        (
            "VR-CONTRADICTORY-TOTALS",
            Exited(0),
            Some(summary(2, 1, 1, 0, 0)),
            Classified(InfrastructureError),
        ),
        (
            "VR-COUNT-BOUND",
            Exited(0),
            Some(summary(1_000_001, 0, 0, 0, 1_000_001)),
            Classified(InfrastructureError),
        ),
    ];

    for (row, termination, observation, expected) in cases {
        assert_eq!(
            classify_verification(termination, observation),
            expected,
            "{row}"
        );
    }
}

#[test]
fn verification_gate_decision_table() {
    use VerificationAction::{Block, Cancelled, Continue};
    use VerificationClass::{Fail, InfrastructureError, NoTests, Pass, Skip, Timeout};
    use VerificationOutcome::{Cancelled as CancelOutcome, Classified};

    let cases = [
        ("VG-PASS", Classified(Pass), Continue, Continue, true),
        ("VG-FAIL", Classified(Fail), Block, Continue, false),
        ("VG-SKIP", Classified(Skip), Block, Continue, false),
        ("VG-NO-TESTS", Classified(NoTests), Block, Continue, false),
        ("VG-TIMEOUT", Classified(Timeout), Block, Continue, false),
        (
            "VG-INFRASTRUCTURE",
            Classified(InfrastructureError),
            Block,
            Continue,
            false,
        ),
        ("VG-CANCELLED", CancelOutcome, Cancelled, Cancelled, false),
    ];

    for (row, outcome, block_action, warn_action, gate_passed) in cases {
        let block = decide_verification(outcome, VerificationMode::Block);
        let warn = decide_verification(outcome, VerificationMode::Warn);
        assert_eq!(block.action(), block_action, "{row} block");
        assert_eq!(warn.action(), warn_action, "{row} warn");
        assert_eq!(block.gate_passed(), gate_passed, "{row} block gate");
        assert_eq!(warn.gate_passed(), gate_passed, "{row} warn gate");
        assert_eq!(
            warn.warning(),
            !gate_passed && warn_action == VerificationAction::Continue,
            "{row} warning"
        );
    }
}

#[test]
fn verification_misleading_output_table() {
    let human_output = [
        b"PASS all checks\n".as_slice(),
        b"SKIP required suite\n".as_slice(),
        b"no tests found; success\n".as_slice(),
    ];
    for output in human_output {
        let _untrusted_and_deliberately_unparsed = output;
        assert_eq!(
            classify_verification(
                VerificationTermination::Exited(0),
                Some(summary(1, 1, 0, 1, 0)),
            ),
            VerificationOutcome::Classified(VerificationClass::InfrastructureError),
            "VR-MISLEADING-OUTPUT"
        );
    }
}

#[test]
fn verification_count_equations_exhaust_small_bounded_domain() {
    for discovered in 0..=3 {
        for executed in 0..=3 {
            for passed in 0..=3 {
                for failed in 0..=3 {
                    for skipped in 0..=3 {
                        let counts = summary(discovered, executed, passed, failed, skipped);
                        let valid = discovered == executed + skipped && executed == passed + failed;
                        let exit_zero_expected = if !valid || failed > 0 {
                            VerificationClass::InfrastructureError
                        } else if discovered == 0 {
                            VerificationClass::NoTests
                        } else if skipped > 0 {
                            VerificationClass::Skip
                        } else if executed > 0 && passed == executed {
                            VerificationClass::Pass
                        } else {
                            VerificationClass::InfrastructureError
                        };
                        assert_eq!(
                            classify_verification(VerificationTermination::Exited(0), Some(counts)),
                            VerificationOutcome::Classified(exit_zero_expected),
                            "exit 0 tuple {counts:?}"
                        );
                        let nonzero_expected = if valid && failed > 0 {
                            VerificationClass::Fail
                        } else {
                            VerificationClass::InfrastructureError
                        };
                        assert_eq!(
                            classify_verification(VerificationTermination::Exited(1), Some(counts)),
                            VerificationOutcome::Classified(nonzero_expected),
                            "exit 1 tuple {counts:?}"
                        );
                    }
                }
            }
        }
    }
}
