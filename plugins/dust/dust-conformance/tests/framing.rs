//! Conformance assertions for DUST-WIRE-SPEC.md §2 — Framing.
//!
//! Each test spawns a fresh `dust-fixture-minimal` process, completes the
//! ready/host_info handshake, injects a specific framing violation, and
//! asserts the expected outcome.
//!
//! | Test | Spec rule | Expected outcome |
//! |------|-----------|-----------------|
//! | `oversized_frame_closes_connection` | FRAME-05 | connection closed |
//! | `zero_length_frame_no_error` | FRAME-04 | connection stays open |
//! | `malformed_utf8_closes_connection` | FRAME-08 | connection closed |
//! | `non_object_json_closes_connection` | FRAME-10 | connection closed |
//! | `missing_kind_closes_connection` | FRAME-11 | connection closed |
//! | `unknown_kind_closes_connection` | FRAME-12 | connection closed |

mod common;

use std::time::Duration;

use dust_conformance::{ConformanceRunner, RECV_TIMEOUT};
use dust_core::envelope::{Envelope, HeartbeatEnvelope};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Assert that `runner.recv_frame()` returns `Ok(None)` (connection closed)
/// within `RECV_TIMEOUT`. Panics with a descriptive message otherwise.
async fn assert_connection_closed(runner: &mut ConformanceRunner, ctx: &str) {
    let result = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
        .await
        .unwrap_or_else(|_| panic!("{ctx}: timed out waiting for connection close"));

    match result {
        Ok(None) => {} // connection closed cleanly — expected
        Ok(Some(frame)) => panic!(
            "{ctx}: expected connection close but received a frame: {frame:?}"
        ),
        // UnexpectedEof is also a valid form of the fixture closing
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Err(e) => panic!("{ctx}: I/O error waiting for connection close: {e}"),
    }
}

// ── FRAME-05: oversized frame ─────────────────────────────────────────────────

/// Send a length prefix of `MAX_FRAME_SIZE + 1` (0x00_10_00_01) and assert the
/// fixture closes the connection without sending any frame back.
#[tokio::test]
async fn oversized_frame_closes_connection() {
    let mut runner = common::spawn_and_handshake().await;

    // Length = 1 MiB + 1 = 0x00100001.  The fixture checks the prefix before
    // reading payload bytes, so we don't need to send any payload.
    let oversized_prefix: [u8; 4] = 0x0010_0001_u32.to_be_bytes();
    runner
        .send_raw_bytes(&oversized_prefix)
        .await
        .expect("send_raw_bytes failed");

    assert_connection_closed(&mut runner, "oversized_frame").await;
}

// ── FRAME-04: zero-length frame ───────────────────────────────────────────────

/// Send a zero-length frame and assert the connection is NOT closed (FRAME-04
/// must be silently discarded).  Then send a heartbeat to confirm the fixture
/// is still alive and responds normally.
#[tokio::test]
async fn zero_length_frame_no_error() {
    let mut runner = common::spawn_and_handshake().await;

    // Send zero-length frame: 4-byte prefix of 0x00000000.
    let zero_len: [u8; 4] = 0u32.to_be_bytes();
    runner
        .send_raw_bytes(&zero_len)
        .await
        .expect("send_raw_bytes failed");

    // Give the fixture a moment to process the zero-length frame.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The connection should still be open — send a heartbeat and expect an echo.
    use dust_conformance::utc_now;
    runner
        .send_frame(&Envelope::Heartbeat(HeartbeatEnvelope { ts: utc_now() }))
        .await
        .expect("send heartbeat failed");

    let response = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
        .await
        .expect("timed out waiting for heartbeat echo after zero-length frame")
        .expect("I/O error reading heartbeat echo");

    assert!(
        matches!(response, Some(Envelope::Heartbeat(_))),
        "expected heartbeat echo after zero-length frame, got: {response:?}"
    );
}

// ── FRAME-08: malformed UTF-8 ─────────────────────────────────────────────────

/// Send a valid length prefix followed by bytes that are not valid UTF-8.
/// The fixture must close the connection.
#[tokio::test]
async fn malformed_utf8_closes_connection() {
    let mut runner = common::spawn_and_handshake().await;

    // 0xFF, 0xFE, 0xFD are not valid UTF-8.
    let bad_utf8: &[u8] = &[0xFF, 0xFE, 0xFD];
    runner
        .send_raw_frame(bad_utf8)
        .await
        .expect("send_raw_frame failed");

    assert_connection_closed(&mut runner, "malformed_utf8").await;
}

// ── FRAME-10: valid JSON, not an object ───────────────────────────────────────

/// Send a valid JSON array.  The top-level value is not an object, so the
/// fixture must close the connection (FRAME-10).
#[tokio::test]
async fn non_object_json_closes_connection() {
    let mut runner = common::spawn_and_handshake().await;

    runner
        .send_raw_frame(b"[1,2,3]")
        .await
        .expect("send_raw_frame failed");

    assert_connection_closed(&mut runner, "non_object_json").await;
}

// ── FRAME-11: JSON object missing `kind` ──────────────────────────────────────

/// Send a valid JSON object that has no `kind` field.
/// The fixture must close the connection (FRAME-11).
#[tokio::test]
async fn missing_kind_closes_connection() {
    let mut runner = common::spawn_and_handshake().await;

    runner
        .send_raw_frame(br#"{"id":"req_1","method":"manifest"}"#)
        .await
        .expect("send_raw_frame failed");

    assert_connection_closed(&mut runner, "missing_kind").await;
}

// ── FRAME-12: unrecognized `kind` value ───────────────────────────────────────

/// Send a JSON object whose `kind` is not one of the five defined values.
/// The fixture must close the connection (FRAME-12).
#[tokio::test]
async fn unknown_kind_closes_connection() {
    let mut runner = common::spawn_and_handshake().await;

    runner
        .send_raw_frame(br#"{"kind":"rpc","id":"req_1"}"#)
        .await
        .expect("send_raw_frame failed");

    assert_connection_closed(&mut runner, "unknown_kind").await;
}
