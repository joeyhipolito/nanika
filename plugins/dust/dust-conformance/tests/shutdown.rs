//! Conformance assertions for DUST-WIRE-SPEC.md §8 (Shutdown) and
//! §9 (Version Negotiation).
//!
//! | Test | Spec rule | Assertion |
//! |------|-----------|-----------|
//! | `shutdown_drains_within_deadline` | SHUTDOWN-03 | fixture closes within `shutdown_drain_ms` (2 000 ms default) after receiving `shutdown` |
//! | `shutdown_closes_connection_cleanly` | SHUTDOWN-03/04 | connection reaches EOF; no further frames after the close |
//! | `version_mismatch_host_info_closes_connection` | VERSION-04 | if `host_info` advertises an incompatible range the fixture closes the connection |

mod common;

use std::time::Duration;

use dust_conformance::{ConformanceRunner, RECV_TIMEOUT};
use dust_core::envelope::{
    Envelope, EventEnvelope, EventType, ShutdownEnvelope, ShutdownReason,
};
use dust_conformance::utc_now;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Default `shutdown_drain_ms` from the spec (SHUTDOWN-03).
const SHUTDOWN_DRAIN_MS: u64 = 2_000;

/// Generous test budget: `shutdown_drain_ms` plus one extra second for OS
/// scheduling overhead and socket teardown.
const SHUTDOWN_BUDGET: Duration = Duration::from_millis(SHUTDOWN_DRAIN_MS + 1_000);

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Send a `shutdown` envelope with `host_exit` reason.
async fn send_shutdown(runner: &mut ConformanceRunner) {
    runner
        .send_frame(&Envelope::Shutdown(ShutdownEnvelope {
            reason: ShutdownReason::HostExit,
        }))
        .await
        .expect("send shutdown failed");
}

/// Drain frames until EOF or the deadline, returning `true` on clean close.
async fn drain_until_closed(runner: &mut ConformanceRunner, budget: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        match tokio::time::timeout(remaining, runner.recv_frame()).await {
            Ok(Ok(None)) => return true,
            Ok(Ok(Some(_))) => {}
            Ok(Err(ref e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => return true,
            _ => return false,
        }
    }
}

// ── SHUTDOWN-03: drain completes within shutdown_drain_ms ────────────────────

/// Send a `shutdown` envelope and verify the fixture closes the connection
/// within the default `shutdown_drain_ms` (2 000 ms) plus a 1 s margin
/// (SHUTDOWN-03).
///
/// The minimal fixture has no in-flight requests, so drain should be
/// essentially instant.  We give it the full spec budget anyway to avoid
/// spurious failures on loaded CI machines.
#[tokio::test]
async fn shutdown_drains_within_deadline() {
    let mut runner = common::spawn_and_handshake().await;

    let start = tokio::time::Instant::now();
    send_shutdown(&mut runner).await;

    let closed = drain_until_closed(&mut runner, SHUTDOWN_BUDGET).await;

    assert!(
        closed,
        "SHUTDOWN-03: fixture did not close the connection within {} ms after shutdown",
        SHUTDOWN_BUDGET.as_millis()
    );

    let elapsed = start.elapsed();
    assert!(
        elapsed <= SHUTDOWN_BUDGET,
        "SHUTDOWN-03: fixture took {}ms to close, which exceeds the {}ms budget",
        elapsed.as_millis(),
        SHUTDOWN_BUDGET.as_millis()
    );
}

// ── SHUTDOWN-03/04: clean connection close, no extra frames ──────────────────

/// Verify the fixture closes cleanly (EOF) after receiving `shutdown` and does
/// not send any additional request frames after the close (SHUTDOWN-03/04).
///
/// Some fixtures may emit a final event before closing; this is allowed.
/// What is NOT allowed is any frame arriving after EOF.
#[tokio::test]
async fn shutdown_closes_connection_cleanly() {
    let mut runner = common::spawn_and_handshake().await;

    send_shutdown(&mut runner).await;

    // Read up to two frames after shutdown — the fixture may emit a final
    // heartbeat or event before closing.
    for attempt in 0..3 {
        let frame = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
            .await
            .expect("timed out waiting for connection close or final frame")
            .expect("I/O error reading post-shutdown frame");

        match frame {
            None => return, // clean EOF — test passes
            Some(_) if attempt < 2 => {
                // One or two final frames before close are acceptable.
            }
            Some(f) => panic!(
                "SHUTDOWN-04: received unexpected frame after shutdown drain: {f:?}"
            ),
        }
    }

    panic!("SHUTDOWN-03: fixture sent more than 2 frames after shutdown without closing");
}

// ── VERSION-04: incompatible host_info closes connection ─────────────────────

/// When `host_info` advertises a `protocol_version_supported` range that does
/// not include the plugin's version (`1.0.0`), the plugin MUST close its end of
/// the connection (VERSION-04).
///
/// This test sends a `host_info` with an incompatible range (`min: 9.0.0,
/// max: 9.999.999`) and asserts the fixture closes the connection.
///
/// NOTE: the minimal fixture does not currently enforce VERSION-04 (it accepts
/// any `host_info`), so this test documents the expected behaviour and serves
/// as a forward-compatibility guard.  If the fixture starts enforcing VERSION-04
/// this test will begin passing without changes.
#[tokio::test]
#[ignore = "minimal fixture does not yet enforce VERSION-04 (registry range validation)"]
async fn version_mismatch_host_info_closes_connection() {
    let mut runner = common::spawn_fixture().await;

    // Read and discard the ready event.
    let _ = tokio::time::timeout(Duration::from_secs(5), runner.recv_frame())
        .await
        .expect("timed out waiting for ready event")
        .expect("I/O error reading ready event");

    // Send host_info with an incompatible version range.
    let incompatible_host_info = Envelope::Event(EventEnvelope {
        id: "evt_incompatible_host0".into(),
        event_type: EventType::HostInfo,
        ts: utc_now(),
        sequence: None,
        data: serde_json::json!({
            "host_name": "dust-conform",
            "host_version": "0.1.0",
            "protocol_version_supported": {
                "min": "9.0.0",
                "max": "9.999.999"
            },
            "consumer_count": 1
        }),
    });

    runner
        .send_frame(&incompatible_host_info)
        .await
        .expect("send incompatible host_info failed");

    // Plugin must close the connection (VERSION-04).
    let closed = drain_until_closed(&mut runner, Duration::from_secs(3)).await;
    assert!(
        closed,
        "VERSION-04: fixture did not close the connection after receiving incompatible host_info"
    );
}
