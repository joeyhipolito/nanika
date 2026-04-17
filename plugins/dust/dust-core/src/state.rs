//! Plugin lifecycle state machine (DUST-WIRE-SPEC.md §5).
//!
//! The registry tracks every plugin through exactly six states. `Dead` is
//! terminal — no transitions out. All valid transitions are defined in the
//! §5.2 state transition table and enforced by [`PluginState::transition`].

use std::fmt;

// ── PluginState ───────────────────────────────────────────────────────────────

/// The six states of a plugin lifecycle as observed by the registry (LIFECYCLE-01).
///
/// The `Dead` state is terminal — no transitions out of it are permitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PluginState {
    /// Process spawned; registry waiting for the socket file to appear.
    Spawned,
    /// Registry has `connect()`ed to the socket; read loop not yet started.
    Connected,
    /// Read loop running; waiting for the plugin's `ready` event.
    HandshakeWait,
    /// Handshake complete; normal bidirectional traffic in progress.
    Active,
    /// Registry sent `shutdown`; waiting for in-flight requests to settle.
    Draining,
    /// Terminal. Process is dead or the connection is permanently closed.
    Dead,
}

impl fmt::Display for PluginState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Spawned => "spawned",
            Self::Connected => "connected",
            Self::HandshakeWait => "handshake_wait",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Dead => "dead",
        })
    }
}

// ── DeadReason ────────────────────────────────────────────────────────────────

/// Diagnostic reasons for a plugin transitioning to [`PluginState::Dead`]
/// (LIFECYCLE-08).
///
/// These are registry-internal values used for logging and observability.
/// They are **not** the shutdown reason vocabulary from ENVELOPE-12 — they
/// MUST NOT appear in `shutdown` envelope `reason` fields. See
/// `dust_core::envelope::ShutdownReason` for the on-wire vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeadReason {
    /// Socket file never appeared within `spawn_timeout_ms`.
    SocketNeverAppeared,
    /// Plugin process exited before binding its socket.
    PluginExitedBeforeBind,
    /// No inbound frame arrived within 5 s of the socket connect (counted from
    /// when the registry started the read loop).
    HandshakeTimeout,
    /// Plugin sent traffic other than a `ready` event during `handshake_wait`.
    PrematureTraffic,
    /// Plugin process exited between socket bind and the registry starting its
    /// read loop.
    PluginExitedBeforeHandshake,
    /// Plugin process exited while `HandshakeWait` was in progress.
    PluginExitedDuringHandshake,
    /// Missed 3 consecutive heartbeats while in `active` state.
    HeartbeatTimeout,
    /// Plugin process exited normally while in `active` state.
    ProcessExited,
    /// Plugin process exited while the connection was in `draining` state.
    ProcessExitedDuringDrain,
    /// Drain deadline elapsed without all in-flight requests completing.
    DrainTimeout,
}

impl fmt::Display for DeadReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SocketNeverAppeared => "socket_never_appeared",
            Self::PluginExitedBeforeBind => "plugin_exited_before_bind",
            Self::HandshakeTimeout => "handshake_timeout",
            Self::PrematureTraffic => "premature_traffic",
            Self::PluginExitedBeforeHandshake => "plugin_exited_before_handshake",
            Self::PluginExitedDuringHandshake => "plugin_exited_during_handshake",
            Self::HeartbeatTimeout => "heartbeat_timeout",
            Self::ProcessExited => "process_exited",
            Self::ProcessExitedDuringDrain => "process_exited_during_drain",
            Self::DrainTimeout => "drain_timeout",
        })
    }
}

// ── PluginStateError ──────────────────────────────────────────────────────────

/// Error returned when a state transition is not permitted by the §5 state
/// table (LIFECYCLE-02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginStateError {
    /// The state the transition was attempted from.
    pub from: PluginState,
    /// The state the transition was attempted to.
    pub to: PluginState,
}

impl fmt::Display for PluginStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid state transition: {} → {}", self.from, self.to)
    }
}

impl std::error::Error for PluginStateError {}

// ── Transition table ──────────────────────────────────────────────────────────

impl PluginState {
    /// Attempt to transition from `self` to `to`.
    ///
    /// Returns `Ok(to)` if the transition is permitted by the §5.2 state
    /// table. Returns `Err(`[`PluginStateError`]`)` for any invalid or
    /// terminal-state transition.
    ///
    /// # Valid transitions (LIFECYCLE-02)
    ///
    /// | From           | To             |
    /// |----------------|----------------|
    /// | Spawned        | Connected      |
    /// | Spawned        | Dead           |
    /// | Connected      | HandshakeWait  |
    /// | Connected      | Dead           |
    /// | HandshakeWait  | Active         |
    /// | HandshakeWait  | Draining       |
    /// | HandshakeWait  | Dead           |
    /// | Active         | Draining       |
    /// | Active         | Dead           |
    /// | Draining       | Dead           |
    ///
    /// Every other `(from, to)` pair — including any transition out of
    /// `Dead` — is rejected.
    pub fn transition(self, to: PluginState) -> Result<PluginState, PluginStateError> {
        let valid = matches!(
            (self, to),
            (PluginState::Spawned, PluginState::Connected)
                | (PluginState::Spawned, PluginState::Dead)
                | (PluginState::Connected, PluginState::HandshakeWait)
                | (PluginState::Connected, PluginState::Dead)
                | (PluginState::HandshakeWait, PluginState::Active)
                | (PluginState::HandshakeWait, PluginState::Draining)
                | (PluginState::HandshakeWait, PluginState::Dead)
                | (PluginState::Active, PluginState::Draining)
                | (PluginState::Active, PluginState::Dead)
                | (PluginState::Draining, PluginState::Dead)
        );
        if valid {
            Ok(to)
        } else {
            Err(PluginStateError { from: self, to })
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_transitions_succeed() {
        let cases = [
            (PluginState::Spawned, PluginState::Connected),
            (PluginState::Spawned, PluginState::Dead),
            (PluginState::Connected, PluginState::HandshakeWait),
            (PluginState::Connected, PluginState::Dead),
            (PluginState::HandshakeWait, PluginState::Active),
            (PluginState::HandshakeWait, PluginState::Draining),
            (PluginState::HandshakeWait, PluginState::Dead),
            (PluginState::Active, PluginState::Draining),
            (PluginState::Active, PluginState::Dead),
            (PluginState::Draining, PluginState::Dead),
        ];
        for (from, to) in cases {
            assert_eq!(
                from.transition(to),
                Ok(to),
                "expected {from} → {to} to be valid"
            );
        }
    }

    #[test]
    fn dead_is_terminal() {
        let all_states = [
            PluginState::Spawned,
            PluginState::Connected,
            PluginState::HandshakeWait,
            PluginState::Active,
            PluginState::Draining,
            PluginState::Dead,
        ];
        for to in all_states {
            assert!(
                PluginState::Dead.transition(to).is_err(),
                "Dead → {to} must be invalid (Dead is terminal)"
            );
        }
    }

    #[test]
    fn skipping_states_is_invalid() {
        assert!(PluginState::Spawned.transition(PluginState::HandshakeWait).is_err());
        assert!(PluginState::Spawned.transition(PluginState::Active).is_err());
        assert!(PluginState::Spawned.transition(PluginState::Draining).is_err());
        assert!(PluginState::Connected.transition(PluginState::Active).is_err());
        assert!(PluginState::Connected.transition(PluginState::Draining).is_err());
    }

    #[test]
    fn backward_transitions_are_invalid() {
        assert!(PluginState::Active.transition(PluginState::Spawned).is_err());
        assert!(PluginState::Active.transition(PluginState::Connected).is_err());
        assert!(PluginState::Active.transition(PluginState::HandshakeWait).is_err());
        assert!(PluginState::Draining.transition(PluginState::Active).is_err());
        assert!(PluginState::HandshakeWait.transition(PluginState::Spawned).is_err());
    }

    #[test]
    fn error_carries_from_and_to() {
        let err = PluginState::Spawned
            .transition(PluginState::Active)
            .unwrap_err();
        assert_eq!(err.from, PluginState::Spawned);
        assert_eq!(err.to, PluginState::Active);
        assert!(err.to_string().contains("spawned"));
        assert!(err.to_string().contains("active"));
    }

    #[test]
    fn state_display() {
        assert_eq!(PluginState::Spawned.to_string(), "spawned");
        assert_eq!(PluginState::HandshakeWait.to_string(), "handshake_wait");
        assert_eq!(PluginState::Dead.to_string(), "dead");
    }

    #[test]
    fn dead_reason_display() {
        assert_eq!(
            DeadReason::SocketNeverAppeared.to_string(),
            "socket_never_appeared"
        );
        assert_eq!(DeadReason::DrainTimeout.to_string(), "drain_timeout");
        assert_eq!(
            DeadReason::ProcessExitedDuringDrain.to_string(),
            "process_exited_during_drain"
        );
    }
}
