//! Pure monotonic watchdog decisions and supervisor-turn precedence.

use std::time::{Duration, Instant};
use thiserror::Error;

/// Nanoseconds from one clock-local monotonic origin.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MonoInstant(u64);

impl MonoInstant {
    /// Constructs a logical monotonic instant.
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Returns nanoseconds from the clock-local origin.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    fn checked_add(self, duration: Duration) -> Option<Self> {
        let nanos = u64::try_from(duration.as_nanos()).ok()?;
        self.0.checked_add(nanos).map(Self)
    }
}

/// Injected monotonic time source. It grants no wall-clock or effect authority.
pub trait MonotonicClock {
    /// Returns the current logical instant.
    fn now(&self) -> MonoInstant;
}

/// Injected source of activity snapshots. Process existence is not an implementation.
pub trait ActivitySource {
    /// Returns the latest validated activity generation.
    fn snapshot(&self) -> ActivitySnapshot;
}

/// Standard monotonic clock with a private local origin.
#[derive(Debug)]
pub struct StdMonotonicClock {
    origin: Instant,
}

impl Default for StdMonotonicClock {
    fn default() -> Self {
        Self::new()
    }
}

impl StdMonotonicClock {
    /// Starts a new local monotonic epoch.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl MonotonicClock for StdMonotonicClock {
    fn now(&self) -> MonoInstant {
        let nanos = self.origin.elapsed().as_nanos();
        MonoInstant(u64::try_from(nanos).unwrap_or(u64::MAX))
    }
}

/// Valid source of worker activity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityKind {
    Spawn,
    Stdout,
    Stderr,
    Heartbeat,
    Progress,
}

/// One validated activity generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivitySnapshot {
    pub generation: u64,
    pub at: MonoInstant,
    pub kind: ActivityKind,
}

impl ActivitySnapshot {
    /// Creates an activity snapshot already validated by its observation adapter.
    #[must_use]
    pub const fn new(generation: u64, at: MonoInstant, kind: ActivityKind) -> Self {
        Self {
            generation,
            at,
            kind,
        }
    }
}

/// Pure watchdog result. Liveness is deliberately not a success result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchdogDecision {
    Continue {
        deadline: MonoInstant,
    },
    Stalled {
        last_activity: MonoInstant,
        deadline: MonoInstant,
    },
}

/// Invalid clock or activity input requiring infrastructure cancellation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WatchdogError {
    #[error("stall timeout must be positive and representable")]
    InvalidTimeout,
    #[error("monotonic clock moved backward")]
    ClockMovedBackward,
    #[error("activity generation or time moved backward")]
    ActivityMovedBackward,
    #[error("activity snapshot is later than the observing clock")]
    ActivityFromFuture,
    #[error("activity generation was reused with different content")]
    ActivityConflict,
    #[error("stall deadline overflowed the monotonic domain")]
    DeadlineOverflow,
}

/// Stateful pure watchdog driven only by an injected clock and validated snapshots.
#[derive(Debug)]
pub struct Watchdog {
    timeout: Duration,
    last_activity: ActivitySnapshot,
    last_now: MonoInstant,
}

impl Watchdog {
    /// Seeds generation/time at spawn. Invalid timeouts fail before process admission.
    pub fn new(
        timeout: Duration,
        initial_activity: ActivitySnapshot,
    ) -> Result<Self, WatchdogError> {
        if timeout.is_zero() || u64::try_from(timeout.as_nanos()).is_err() {
            return Err(WatchdogError::InvalidTimeout);
        }
        Ok(Self {
            timeout,
            last_activity: initial_activity,
            last_now: initial_activity.at,
        })
    }

    /// Evaluates one turn. New activity is incorporated before the exact deadline check.
    pub fn evaluate<C: MonotonicClock>(
        &mut self,
        clock: &C,
        latest_activity: ActivitySnapshot,
    ) -> Result<WatchdogDecision, WatchdogError> {
        self.evaluate_at(clock.now(), latest_activity)
    }

    /// Evaluates one turn from injected time and activity sources.
    pub fn evaluate_source<C: MonotonicClock, S: ActivitySource>(
        &mut self,
        clock: &C,
        activity: &S,
    ) -> Result<WatchdogDecision, WatchdogError> {
        self.evaluate(clock, activity.snapshot())
    }

    fn evaluate_at(
        &mut self,
        now: MonoInstant,
        latest_activity: ActivitySnapshot,
    ) -> Result<WatchdogDecision, WatchdogError> {
        if now < self.last_now || now < self.last_activity.at {
            return Err(WatchdogError::ClockMovedBackward);
        }
        if latest_activity.generation < self.last_activity.generation
            || latest_activity.at < self.last_activity.at
        {
            return Err(WatchdogError::ActivityMovedBackward);
        }
        if latest_activity.at > now {
            return Err(WatchdogError::ActivityFromFuture);
        }
        if latest_activity.generation == self.last_activity.generation
            && latest_activity != self.last_activity
        {
            return Err(WatchdogError::ActivityConflict);
        }

        let effective_activity = if latest_activity.generation > self.last_activity.generation {
            latest_activity
        } else {
            self.last_activity
        };
        let deadline = effective_activity
            .at
            .checked_add(self.timeout)
            .ok_or(WatchdogError::DeadlineOverflow)?;

        self.last_activity = effective_activity;
        self.last_now = now;
        if now >= deadline {
            Ok(WatchdogDecision::Stalled {
                last_activity: effective_activity.at,
                deadline,
            })
        } else {
            Ok(WatchdogDecision::Continue { deadline })
        }
    }
}

/// Inputs observed during one ordered supervisor turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SupervisorTurnInput {
    pub external_cancelled: bool,
    pub direct_exit_observed: bool,
    pub hard_deadline: MonoInstant,
    pub latest_activity: ActivitySnapshot,
}

/// Ordered supervisor decision. No variant represents success from mere liveness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SupervisorTurnDecision {
    Cancel,
    ExitObserved,
    HardTimeout,
    Continue {
        deadline: MonoInstant,
    },
    Stalled {
        last_activity: MonoInstant,
        deadline: MonoInstant,
    },
}

/// Applies cancellation, exit, hard-timeout, then stall precedence exactly once.
pub fn evaluate_supervisor_turn<C: MonotonicClock>(
    watchdog: &mut Watchdog,
    clock: &C,
    input: SupervisorTurnInput,
) -> Result<SupervisorTurnDecision, WatchdogError> {
    if input.external_cancelled {
        return Ok(SupervisorTurnDecision::Cancel);
    }
    if input.direct_exit_observed {
        return Ok(SupervisorTurnDecision::ExitObserved);
    }
    let now = clock.now();
    if now >= input.hard_deadline {
        return Ok(SupervisorTurnDecision::HardTimeout);
    }
    match watchdog.evaluate_at(now, input.latest_activity)? {
        WatchdogDecision::Continue { deadline } => {
            Ok(SupervisorTurnDecision::Continue { deadline })
        }
        WatchdogDecision::Stalled {
            last_activity,
            deadline,
        } => Ok(SupervisorTurnDecision::Stalled {
            last_activity,
            deadline,
        }),
    }
}
