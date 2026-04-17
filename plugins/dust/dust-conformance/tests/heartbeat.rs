//! Conformance assertions for DUST-WIRE-SPEC.md §7 — Heartbeat Rules.
//!
//! Tests use `DUST_HEARTBEAT_INTERVAL_MS=800` so the fixture's heartbeat timer
//! fires within a second, keeping the test suite fast.
//!
//! | Test | Spec rule | Assertion |
//! |------|-----------|-----------|
//! | `fixture_sends_heartbeat_proactively` | HEARTBEAT-01 | fixture emits a heartbeat within 2 × interval without the runner sending one first |
//! | `missing_registry_heartbeats_close_connection` | HEARTBEAT-02 | after 3 missed intervals from the registry, fixture closes the connection |

mod common;

use std::time::Duration;

use dust_conformance::ConformanceRunner;
use dust_core::envelope::Envelope;

/// Heartbeat interval injected into the fixture for these tests (ms).
const HB_MS: u64 = 800;

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Read frames from `runner` until EOF or `timeout` elapses.
///
/// Returns `true` if the connection closed before the timeout, `false` otherwise.
/// Heartbeat and other frames are silently drained.
async fn drain_until_closed(runner: &mut ConformanceRunner, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false; // timed out without seeing EOF
        }
        match tokio::time::timeout(remaining, runner.recv_frame()).await {
            // Clean EOF
            Ok(Ok(None)) => return true,
            // Any frame — keep draining
            Ok(Ok(Some(_))) => {}
            // UnexpectedEof counts as closed
            Ok(Err(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return true;
            }
            // Other I/O error — treat as unexpected
            Ok(Err(_)) | Err(_) => return false,
        }
    }
}

// ── HEARTBEAT-01: fixture sends proactive heartbeats ─────────────────────────

/// Verify the fixture emits at least one heartbeat proactively, without the
/// conformance runner sending a heartbeat first (HEARTBEAT-01).
///
/// The runner completes the handshake and then waits passively.  The fixture's
/// heartbeat timer fires after `HB_MS` ms; the runner asserts a heartbeat frame
/// arrives within `2 × HB_MS + 500 ms` (generous margin for scheduling jitter).
#[tokio::test]
async fn fixture_sends_heartbeat_proactively() {
    let mut runner = common::spawn_and_handshake_with_hb_ms(HB_MS).await;

    // Do NOT send any heartbeat.  Wait for the fixture's timer to fire.
    let wait = Duration::from_millis(HB_MS * 2 + 500);

    let frame = tokio::time::timeout(wait, runner.recv_frame())
        .await
        .expect("HEARTBEAT-01: timed out — fixture did not send a proactive heartbeat")
        .expect("I/O error reading heartbeat frame");

    assert!(
        matches!(frame, Some(Envelope::Heartbeat(_))),
        "HEARTBEAT-01: expected a proactive heartbeat frame, got: {frame:?}"
    );
}

// ── HEARTBEAT-02: missing registry heartbeats close the connection ────────────

/// Verify the fixture closes the connection after 3 consecutive missed
/// heartbeat intervals from the registry (HEARTBEAT-02).
///
/// The runner completes the handshake but deliberately sends no heartbeats.
/// The fixture checks for misses on every timer tick (every `HB_MS` ms) and
/// closes the connection once `3 × HB_MS` ms have elapsed since the last
/// received heartbeat.
///
/// Total test budget: `3 × HB_MS + 2 × HB_MS` grace = 5 × HB_MS + margin.
#[tokio::test]
async fn missing_registry_heartbeats_close_connection() {
    let mut runner = common::spawn_and_handshake_with_hb_ms(HB_MS).await;

    // The fixture initialises `last_registry_hb` to `now()` right when the
    // dispatch loop starts (immediately after the handshake).  It checks for
    // 3 consecutive misses every `HB_MS` ms, so the connection should close
    // after approximately `3 × HB_MS` ms.
    //
    // We give it `5 × HB_MS + 500 ms` to account for scheduling jitter and
    // the extra heartbeat frames the fixture sends before closing.

    let budget = Duration::from_millis(HB_MS * 5 + 500);

    assert!(
        drain_until_closed(&mut runner, budget).await,
        "HEARTBEAT-02: fixture did not close the connection after 3 missed heartbeat intervals \
         (waited {}ms without sending any heartbeat from the registry)",
        budget.as_millis()
    );
}
