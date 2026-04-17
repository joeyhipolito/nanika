//! Conformance assertions for DUST-WIRE-SPEC.md §5 (Lifecycle) and §13 (Handshake).
//!
//! Each test spawns a fresh `dust-fixture-minimal` process and verifies a
//! specific lifecycle or handshake invariant.
//!
//! | Test | Spec rule | Assertion |
//! |------|-----------|-----------|
//! | `ready_event_sent_within_5s` | LIFECYCLE-02, HANDSHAKE-02 | fixture sends `ready` within 5 s of socket connect |
//! | `ready_event_has_sequence_one` | HANDSHAKE-02 | `ready.sequence == 1` |
//! | `ready_data_contains_required_fields` | HANDSHAKE-03 | `manifest`, `protocol_version`, `plugin_info` all present |
//! | `plugin_active_after_handshake` | HANDSHAKE-07 | plugin responds to requests after `host_info` |
//! | `premature_request_before_host_info_closes` | LIFECYCLE-02 | sending a request before `host_info` closes the connection |

mod common;

use std::time::Duration;

use dust_conformance::{ConformanceRunner, RECV_TIMEOUT, recv_response};
use dust_core::envelope::{Envelope, EventType, RequestEnvelope};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Assert the connection closes (EOF) within `RECV_TIMEOUT`.
async fn assert_connection_closed(runner: &mut ConformanceRunner, ctx: &str) {
    let result = tokio::time::timeout(RECV_TIMEOUT, runner.recv_frame())
        .await
        .unwrap_or_else(|_| panic!("{ctx}: timed out waiting for connection close"));

    match result {
        Ok(None) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {}
        Ok(Some(frame)) => panic!(
            "{ctx}: expected connection close but received a frame: {frame:?}"
        ),
        Err(e) => panic!("{ctx}: I/O error waiting for connection close: {e}"),
    }
}

// ── LIFECYCLE-02 / HANDSHAKE-02: ready within 5 s of connect ─────────────────

/// Verify the fixture sends a `ready` event as the very first frame within 5 s
/// of the registry connecting (LIFECYCLE-02, HANDSHAKE-02).
///
/// This test does NOT complete the handshake; it only reads the first frame and
/// checks it is a well-formed `ready` event.
#[tokio::test]
async fn ready_event_sent_within_5s() {
    let mut runner = common::spawn_fixture().await;

    let frame = tokio::time::timeout(Duration::from_secs(5), runner.recv_frame())
        .await
        .expect("LIFECYCLE-02: timed out — fixture did not send ready within 5 s")
        .expect("I/O error reading first frame from fixture");

    match frame {
        Some(Envelope::Event(ref e)) if e.event_type == EventType::Ready => {
            // Correct — fixture sent ready as its first frame.
        }
        Some(other) => panic!(
            "LIFECYCLE-02: expected ready event as first frame, got {:?}",
            other.kind()
        ),
        None => panic!("LIFECYCLE-02: fixture closed connection before sending ready"),
    }
}

// ── HANDSHAKE-02: ready.sequence == 1 ────────────────────────────────────────

/// The `ready` event MUST carry `sequence: 1` (HANDSHAKE-02 — it is the first
/// sequenced event emitted by the plugin).
#[tokio::test]
async fn ready_event_has_sequence_one() {
    let mut runner = common::spawn_fixture().await;

    let frame = tokio::time::timeout(Duration::from_secs(5), runner.recv_frame())
        .await
        .expect("timed out waiting for ready event")
        .expect("I/O error reading ready event")
        .expect("fixture closed before sending ready");

    match frame {
        Envelope::Event(ref e) if e.event_type == EventType::Ready => {
            let seq = e.sequence.unwrap_or(0);
            assert_eq!(
                seq, 1,
                "HANDSHAKE-02: ready event must have sequence: 1, got {seq}"
            );
        }
        other => panic!("expected ready event, got: {other:?}"),
    }
}

// ── HANDSHAKE-03: ready.data required fields ──────────────────────────────────

/// The `ready` event `data` field MUST contain `manifest`, `protocol_version`,
/// and `plugin_info` (HANDSHAKE-03).
///
/// `plugin_info` MUST include `pid` (integer) and `started_at` (ISO 8601 string).
#[tokio::test]
async fn ready_data_contains_required_fields() {
    let mut runner = common::spawn_fixture().await;

    let frame = tokio::time::timeout(Duration::from_secs(5), runner.recv_frame())
        .await
        .expect("timed out waiting for ready event")
        .expect("I/O error reading ready event")
        .expect("fixture closed before sending ready");

    let data = match frame {
        Envelope::Event(ref e) if e.event_type == EventType::Ready => e.data.clone(),
        other => panic!("expected ready event, got: {other:?}"),
    };

    // manifest must be present and be an object
    let manifest = data
        .get("manifest")
        .expect("HANDSHAKE-03: ready.data missing `manifest`");
    assert!(
        manifest.is_object(),
        "HANDSHAKE-03: ready.data.manifest must be a JSON object, got: {manifest}"
    );
    assert!(
        manifest.get("name").and_then(|v| v.as_str()).is_some(),
        "HANDSHAKE-03: ready.data.manifest missing string `name`"
    );
    assert!(
        manifest.get("version").and_then(|v| v.as_str()).is_some(),
        "HANDSHAKE-03: ready.data.manifest missing string `version`"
    );

    // protocol_version must be a non-empty string
    let pv = data
        .get("protocol_version")
        .and_then(|v| v.as_str())
        .expect("HANDSHAKE-03: ready.data missing string `protocol_version`");
    assert!(
        !pv.is_empty(),
        "HANDSHAKE-03: ready.data.protocol_version must not be empty"
    );

    // plugin_info must contain pid and started_at
    let pi = data
        .get("plugin_info")
        .expect("HANDSHAKE-03: ready.data missing `plugin_info`");
    assert!(
        pi.get("pid").and_then(|v| v.as_u64()).is_some(),
        "HANDSHAKE-03: ready.data.plugin_info missing integer `pid`"
    );
    assert!(
        pi.get("started_at").and_then(|v| v.as_str()).is_some(),
        "HANDSHAKE-03: ready.data.plugin_info missing string `started_at`"
    );
}

// ── HANDSHAKE-07: plugin active after host_info ───────────────────────────────

/// After the registry sends `host_info`, the plugin MUST transition to `active`
/// and respond normally to subsequent requests (HANDSHAKE-07).
///
/// Verifies this by sending a `manifest` request and asserting a well-formed
/// response arrives.
#[tokio::test]
async fn plugin_active_after_handshake() {
    let mut runner = common::spawn_and_handshake().await;

    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "lc_req_0001".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .expect("send manifest request failed");

    let resp = recv_response(&mut runner, "lc_req_0001")
        .await
        .expect("recv manifest response failed");

    let result = resp
        .result
        .expect("HANDSHAKE-07: manifest response must have a result after handshake");

    assert!(
        result.get("name").and_then(|v| v.as_str()).is_some(),
        "HANDSHAKE-07: manifest result missing string `name`"
    );
    assert!(
        result.get("version").and_then(|v| v.as_str()).is_some(),
        "HANDSHAKE-07: manifest result missing string `version`"
    );
}

// ── LIFECYCLE-02: premature traffic before host_info closes connection ─────────

/// In `handshake_wait`, the plugin is waiting for a `host_info` event.  If the
/// registry sends anything else before `host_info`, the plugin MUST close the
/// connection (LIFECYCLE-02 row: `handshake_wait` → premature_traffic).
///
/// This test reads the `ready` event to confirm the fixture is in handshake_wait,
/// then sends a `request` frame instead of `host_info` and asserts that the
/// connection closes.
#[tokio::test]
async fn premature_request_before_host_info_closes() {
    let mut runner = common::spawn_fixture().await;

    // Consume the ready event so the fixture is now blocked on host_info.
    let frame = tokio::time::timeout(Duration::from_secs(5), runner.recv_frame())
        .await
        .expect("timed out waiting for ready event")
        .expect("I/O error reading ready event")
        .expect("fixture closed before sending ready");

    assert!(
        matches!(&frame, Envelope::Event(e) if e.event_type == EventType::Ready),
        "expected ready event before premature traffic test, got: {frame:?}"
    );

    // Send a request instead of host_info — this is premature traffic.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "premature_001".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .expect("send premature request failed");

    // Fixture must close the connection.
    assert_connection_closed(&mut runner, "premature_request_before_host_info").await;
}
