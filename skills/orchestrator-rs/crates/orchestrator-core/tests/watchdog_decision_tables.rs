use orchestrator_core::{
    ActivityKind, ActivitySnapshot, ActivitySource, MonoInstant, MonotonicClock,
    SupervisorTurnDecision, SupervisorTurnInput, Watchdog, WatchdogDecision, WatchdogError,
    evaluate_supervisor_turn,
};
use std::{cell::Cell, time::Duration};

type TestResult = Result<(), WatchdogError>;

struct ManualClock(Cell<MonoInstant>);

impl ManualClock {
    fn new(nanos: u64) -> Self {
        Self(Cell::new(MonoInstant::from_nanos(nanos)))
    }

    fn set(&self, nanos: u64) {
        self.0.set(MonoInstant::from_nanos(nanos));
    }
}

impl MonotonicClock for ManualClock {
    fn now(&self) -> MonoInstant {
        self.0.get()
    }
}

struct ManualActivity(Cell<ActivitySnapshot>);

impl ManualActivity {
    fn new(snapshot: ActivitySnapshot) -> Self {
        Self(Cell::new(snapshot))
    }

    fn set(&self, snapshot: ActivitySnapshot) {
        self.0.set(snapshot);
    }
}

impl ActivitySource for ManualActivity {
    fn snapshot(&self) -> ActivitySnapshot {
        self.0.get()
    }
}

fn activity(generation: u64, at: u64) -> ActivitySnapshot {
    ActivitySnapshot::new(
        generation,
        MonoInstant::from_nanos(at),
        ActivityKind::Stdout,
    )
}

#[test]
fn watchdog_decision_table() -> TestResult {
    let clock = ManualClock::new(19);
    let mut watchdog = Watchdog::new(Duration::from_nanos(10), activity(1, 10))?;
    let source = ManualActivity::new(activity(1, 10));

    assert_eq!(
        watchdog.evaluate_source(&clock, &source)?,
        WatchdogDecision::Continue {
            deadline: MonoInstant::from_nanos(20),
        },
        "WD-CONTINUE-BEFORE"
    );

    clock.set(20);
    source.set(activity(1, 10));
    assert!(
        matches!(
            watchdog.evaluate(&clock, activity(1, 10))?,
            WatchdogDecision::Stalled { .. }
        ),
        "WD-STALL-AT-BOUNDARY"
    );

    let mut watchdog = Watchdog::new(Duration::from_nanos(10), activity(1, 10))?;
    assert_eq!(
        watchdog.evaluate(&clock, activity(2, 20))?,
        WatchdogDecision::Continue {
            deadline: MonoInstant::from_nanos(30),
        },
        "WD-RESET-AT-BOUNDARY"
    );
    clock.set(30);
    assert!(
        matches!(
            watchdog.evaluate(&clock, activity(2, 20))?,
            WatchdogDecision::Stalled { .. }
        ),
        "WD-REPEAT-NOT-ACTIVITY"
    );
    Ok(())
}

#[test]
fn watchdog_invalid_time_table() -> TestResult {
    let cases = [
        (
            "WD-BACKWARD-CLOCK",
            9,
            activity(1, 10),
            activity(1, 10),
            WatchdogError::ClockMovedBackward,
        ),
        (
            "WD-BACKWARD-ACTIVITY",
            20,
            activity(2, 20),
            activity(1, 10),
            WatchdogError::ActivityMovedBackward,
        ),
        (
            "WD-FUTURE-ACTIVITY",
            20,
            activity(1, 10),
            activity(2, 21),
            WatchdogError::ActivityFromFuture,
        ),
    ];
    for (row, now, initial, observed, expected) in cases {
        let clock = ManualClock::new(now);
        let mut watchdog = Watchdog::new(Duration::from_nanos(10), initial)?;
        assert_eq!(watchdog.evaluate(&clock, observed), Err(expected), "{row}");
    }

    let clock = ManualClock::new(u64::MAX - 2);
    let initial = activity(1, u64::MAX - 2);
    let mut watchdog = Watchdog::new(Duration::from_nanos(10), initial)?;
    assert_eq!(
        watchdog.evaluate(&clock, initial),
        Err(WatchdogError::DeadlineOverflow),
        "WD-DEADLINE-OVERFLOW"
    );
    assert!(
        matches!(
            Watchdog::new(Duration::ZERO, activity(1, 0)),
            Err(WatchdogError::InvalidTimeout)
        ),
        "WD-ZERO-TIMEOUT"
    );
    Ok(())
}

#[test]
fn watchdog_precedence_table() -> TestResult {
    struct Case {
        row: &'static str,
        cancel: bool,
        exit: bool,
        hard_deadline: u64,
        latest: ActivitySnapshot,
        expected: SupervisorTurnDecision,
    }

    let cases = [
        Case {
            row: "WP-CANCEL-FIRST",
            cancel: true,
            exit: true,
            hard_deadline: 20,
            latest: activity(1, 10),
            expected: SupervisorTurnDecision::Cancel,
        },
        Case {
            row: "WP-EXIT-BEFORE-TIMEOUT",
            cancel: false,
            exit: true,
            hard_deadline: 20,
            latest: activity(1, 10),
            expected: SupervisorTurnDecision::ExitObserved,
        },
        Case {
            row: "WP-HARD-BEFORE-STALL",
            cancel: false,
            exit: false,
            hard_deadline: 20,
            latest: activity(1, 10),
            expected: SupervisorTurnDecision::HardTimeout,
        },
        Case {
            row: "WP-ACTIVITY-BEFORE-STALL",
            cancel: false,
            exit: false,
            hard_deadline: 40,
            latest: activity(2, 20),
            expected: SupervisorTurnDecision::Continue {
                deadline: MonoInstant::from_nanos(30),
            },
        },
        Case {
            row: "WP-STALL-LAST",
            cancel: false,
            exit: false,
            hard_deadline: 40,
            latest: activity(1, 10),
            expected: SupervisorTurnDecision::Stalled {
                last_activity: MonoInstant::from_nanos(10),
                deadline: MonoInstant::from_nanos(20),
            },
        },
        Case {
            row: "WP-ALIVE-IS-NOT-SUCCESS",
            cancel: false,
            exit: false,
            hard_deadline: 40,
            latest: activity(1, 10),
            expected: SupervisorTurnDecision::Continue {
                deadline: MonoInstant::from_nanos(20),
            },
        },
    ];

    for case in cases {
        let now = if case.row == "WP-ALIVE-IS-NOT-SUCCESS" {
            19
        } else {
            20
        };
        let clock = ManualClock::new(now);
        let mut watchdog = Watchdog::new(Duration::from_nanos(10), activity(1, 10))?;
        let decision = evaluate_supervisor_turn(
            &mut watchdog,
            &clock,
            SupervisorTurnInput {
                external_cancelled: case.cancel,
                direct_exit_observed: case.exit,
                hard_deadline: MonoInstant::from_nanos(case.hard_deadline),
                latest_activity: case.latest,
            },
        )?;
        assert_eq!(decision, case.expected, "{}", case.row);
    }
    Ok(())
}
