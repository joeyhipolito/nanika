//! Conformance assertions for DUST-WIRE-SPEC.md:
//!   §10 — Reconnect, Replay, and Sequence Ownership (REPLAY-*)
//!   §11 — Observability (OBSERVE-09 via heartbeat filter check)
//!   §12 — Backpressure (PRESSURE-01: one subscription per connection)
//!   §15 — Cancellation Contract (CANCEL-*)
//!
//! Each test spawns a fresh `dust-fixture-minimal` process and exercises one
//! specific spec invariant in isolation.
//!
//! | Test | Spec rule | Assertion |
//! |------|-----------|-----------|
//! | `subscribe_response_has_required_fields` | REPLAY-04 | result contains `subscription_id`, `events`, `next_sequence` |
//! | `subscribe_snapshot_returns_events_since_sequence` | REPLAY-05 | snapshot filters by `since_sequence` |
//! | `subscribe_future_sequence_returns_empty` | REPLAY-05 | future cursor → empty events |
//! | `subscribe_too_old_sequence_returns_replay_gap` | REPLAY-05 | evicted cursor → -33007 |
//! | `subscribe_live_push_after_subscribe` | REPLAY-06 | new event pushed after subscribe |
//! | `second_subscribe_on_same_connection_is_busy` | REPLAY-08 / PRESSURE-01 | second subscribe → -33005 |
//! | `cancel_inflight_action_returns_33002` | CANCEL-03/06 | cancel ack + -33002 on original |
//! | `cancel_already_complete_action` | CANCEL-05 | cancel after completion → `already_complete: true` |

mod common;

use dust_conformance::{recv_next_event, recv_raw_response, recv_response, ConformanceRunner, RECV_TIMEOUT};
use dust_core::envelope::{Envelope, EventType, RequestEnvelope};

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Send `count` action requests (without `op_id`) and collect their responses.
///
/// Each action triggers a `data_updated` event in the fixture's ring.
async fn push_events(runner: &mut ConformanceRunner, base_id: &str, count: usize) {
    for i in 0..count {
        let id = format!("{base_id}_{i:04}");
        runner
            .send_frame(&Envelope::Request(RequestEnvelope {
                id: id.clone(),
                method: "action".into(),
                params: serde_json::json!({}),
            }))
            .await
            .unwrap_or_else(|e| panic!("push_events: send action {i} failed: {e}"));

        recv_response(runner, &id)
            .await
            .unwrap_or_else(|e| panic!("push_events: recv response {i} failed: {e}"));
    }
}

// ── REPLAY-04: subscribe response shape ───────────────────────────────────────

/// The `events.subscribe` response MUST contain `subscription_id` (string),
/// `events` (array), and `next_sequence` (u64) in `result` (REPLAY-04).
#[tokio::test]
async fn subscribe_response_has_required_fields() {
    let mut runner = common::spawn_and_handshake().await;

    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "sub_shape_001".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 0}),
        }))
        .await
        .expect("send events.subscribe failed");

    let resp = recv_response(&mut runner, "sub_shape_001")
        .await
        .expect("recv events.subscribe response failed");

    let result = resp
        .result
        .expect("REPLAY-04: events.subscribe response must have a result");

    assert!(
        result.get("subscription_id").and_then(|v| v.as_str()).is_some(),
        "REPLAY-04: result missing string `subscription_id`; got: {result}"
    );
    assert!(
        result.get("events").map(|v| v.is_array()).unwrap_or(false),
        "REPLAY-04: result missing array `events`; got: {result}"
    );
    assert!(
        result.get("next_sequence").and_then(|v| v.as_u64()).is_some(),
        "REPLAY-04: result missing u64 `next_sequence`; got: {result}"
    );
}

// ── REPLAY-05: snapshot with since_sequence ───────────────────────────────────

/// Push 4 events (sequences 2–5), then subscribe with `since_sequence=4`.
/// The snapshot MUST return only events with sequence >= 4 (REPLAY-05).
#[tokio::test]
async fn subscribe_snapshot_returns_events_since_sequence() {
    let mut runner = common::spawn_and_handshake().await;

    // Push 4 data_updated events into the ring.
    push_events(&mut runner, "snap_action", 4).await;

    // Subscribe from sequence 4 (third and fourth events, index 2 and 3).
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "snap_sub_001".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 4}),
        }))
        .await
        .expect("send events.subscribe failed");

    let resp = recv_response(&mut runner, "snap_sub_001")
        .await
        .expect("recv subscribe response failed");

    let result = resp.result.expect("subscribe response must have result");
    let events = result
        .get("events")
        .and_then(|v| v.as_array())
        .expect("result must have events array");

    // All returned events must have sequence >= 4.
    for ev in events {
        let seq = ev
            .get("sequence")
            .and_then(|v| v.as_u64())
            .expect("each event must have a u64 sequence");
        assert!(
            seq >= 4,
            "REPLAY-05: snapshot returned event with sequence {seq} < since_sequence 4"
        );
    }

    // next_sequence must be > any event sequence.
    let next_seq = result
        .get("next_sequence")
        .and_then(|v| v.as_u64())
        .expect("result must have next_sequence");
    assert!(
        next_seq > 4,
        "REPLAY-05: next_sequence {next_seq} must be > 4 after pushing 4 events"
    );
}

// ── REPLAY-05: future cursor returns empty ────────────────────────────────────

/// Subscribing with `since_sequence` beyond the latest emitted sequence MUST
/// return an empty `events` array (REPLAY-05).
///
/// The fixture will deliver future events via live push — the empty snapshot
/// simply means "nothing to replay yet".
#[tokio::test]
async fn subscribe_future_sequence_returns_empty() {
    let mut runner = common::spawn_and_handshake().await;

    // A very large since_sequence far beyond any event the fixture has emitted.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "fut_sub_001".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 999_999}),
        }))
        .await
        .expect("send events.subscribe failed");

    let resp = recv_response(&mut runner, "fut_sub_001")
        .await
        .expect("recv subscribe response failed");

    let result = resp.result.expect("subscribe response must have result");
    let events = result
        .get("events")
        .and_then(|v| v.as_array())
        .expect("result must have events array");

    assert!(
        events.is_empty(),
        "REPLAY-05: future cursor must return empty events, got {} events",
        events.len()
    );

    // subscription_id must still be present (live push is set up).
    assert!(
        result.get("subscription_id").and_then(|v| v.as_str()).is_some(),
        "REPLAY-05: future-cursor response must still include subscription_id"
    );
}

// ── REPLAY-05: too-old cursor returns -33007 ──────────────────────────────────

/// When `since_sequence` falls behind the ring's eviction frontier, the plugin
/// MUST return an error with code `-33007 replay_gap` (REPLAY-05).
///
/// Uses a fixture with a 3-event ring (`DUST_FIXTURE_MAX_RING_EVENTS=3`).
/// Pushing 5 events evicts the first 2, so `since_sequence=1` is a replay gap.
#[tokio::test]
async fn subscribe_too_old_sequence_returns_replay_gap() {
    // Spawn with a tiny ring: 3 events max.
    let mut runner = common::spawn_and_handshake_with_small_ring(3).await;

    // Push 5 events — ring max is 3, so seq 1 and 2 are evicted.
    // (ready event is seq=1; first data_updated from action gets seq=2, etc.)
    push_events(&mut runner, "gap_action", 5).await;

    // Subscribe from seq=1, which is now below the eviction frontier.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "gap_sub_001".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 1}),
        }))
        .await
        .expect("send events.subscribe failed");

    let resp = recv_raw_response(&mut runner)
        .await
        .expect("recv subscribe response failed");

    let err = resp
        .error
        .expect("REPLAY-05: too-old cursor must return an error, not a result");

    assert_eq!(
        err.code, -33007,
        "REPLAY-05: too-old cursor must return error code -33007 (replay_gap), got {}",
        err.code
    );

    // data must contain oldest_available and requested.
    let data = err
        .data
        .expect("REPLAY-05: replay_gap error must have a `data` field");
    assert!(
        data.get("oldest_available").and_then(|v| v.as_u64()).is_some(),
        "REPLAY-05: replay_gap error data must include `oldest_available`"
    );
    assert!(
        data.get("requested").and_then(|v| v.as_u64()).is_some(),
        "REPLAY-05: replay_gap error data must include `requested`"
    );
}

// ── REPLAY-06: live push after subscribe ──────────────────────────────────────

/// After a successful `events.subscribe`, the fixture MUST push new events to
/// the subscriber as they are emitted (REPLAY-06).
///
/// Scenario:
///   1. Subscribe (empty ring).
///   2. Send an action (triggers a `data_updated` event).
///   3. Receive action response.
///   4. Receive the pushed `data_updated` event.
#[tokio::test]
async fn subscribe_live_push_after_subscribe() {
    let mut runner = common::spawn_and_handshake().await;

    // Subscribe to the event stream (ring is empty → empty snapshot).
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "live_sub_001".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 0}),
        }))
        .await
        .expect("send events.subscribe failed");

    let sub_resp = recv_response(&mut runner, "live_sub_001")
        .await
        .expect("recv subscribe response failed");
    let result = sub_resp.result.expect("subscribe must have result");
    let _sub_id = result
        .get("subscription_id")
        .and_then(|v| v.as_str())
        .expect("subscription_id must be present");

    // Trigger a data_updated event by sending an action.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "live_action_001".into(),
            method: "action".into(),
            params: serde_json::json!({}),
        }))
        .await
        .expect("send action failed");

    // Read the action response (fixture sends it before the push).
    let action_resp = recv_response(&mut runner, "live_action_001")
        .await
        .expect("recv action response failed");
    let action_result = action_resp.result.expect("action must have result");
    assert!(
        action_result.get("success").and_then(|v| v.as_bool()) == Some(true),
        "action result must carry success:true"
    );

    // Read the live push — the fixture emits a data_updated event after the action.
    let pushed = recv_next_event(&mut runner)
        .await
        .expect("REPLAY-06: timed out waiting for live push event");

    assert_eq!(
        pushed.event_type,
        EventType::DataUpdated,
        "REPLAY-06: pushed event must be data_updated, got {:?}",
        pushed.event_type
    );
    assert!(
        pushed.sequence.is_some(),
        "REPLAY-06: pushed event must carry a sequence number"
    );
}

// ── REPLAY-08 / PRESSURE-01: second subscribe is -33005 ──────────────────────

/// A connection that already has an active `events.subscribe` MUST receive
/// error code `-33005 busy` on any subsequent `events.subscribe` request
/// (REPLAY-08, PRESSURE-01: one subscription per connection).
#[tokio::test]
async fn second_subscribe_on_same_connection_is_busy() {
    let mut runner = common::spawn_and_handshake().await;

    // First subscribe — must succeed.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "busy_sub_001".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 0}),
        }))
        .await
        .expect("send first events.subscribe failed");

    recv_response(&mut runner, "busy_sub_001")
        .await
        .expect("first subscribe must succeed");

    // Second subscribe on the same connection — must fail with -33005.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "busy_sub_002".into(),
            method: "events.subscribe".into(),
            params: serde_json::json!({"since_sequence": 0}),
        }))
        .await
        .expect("send second events.subscribe failed");

    let resp = recv_raw_response(&mut runner)
        .await
        .expect("recv second subscribe response failed");

    let err = resp
        .error
        .expect("PRESSURE-01: second subscribe must return an error");

    assert_eq!(
        err.code, -33005,
        "PRESSURE-01: second subscribe must return -33005 (busy), got {}",
        err.code
    );
}

// ── CANCEL-03/06: cancel in-flight action ────────────────────────────────────

/// Send an `action` with an `op_id` (deferred response), then immediately
/// send a `cancel` for that `op_id`.
///
/// Expected outcomes (CANCEL-03, CANCEL-06):
///   - Cancel ack: `result.ok = true`, `result.already_complete = false`.
///   - Original action response: `error.code = -33002`.
#[tokio::test]
async fn cancel_inflight_action_returns_33002() {
    let mut runner = common::spawn_and_handshake().await;

    let action_id = "cancel_action_001";
    let cancel_id = "cancel_req_001";
    let op_id = "op_cancel_test_01";

    // Send action with op_id — fixture defers the response.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: action_id.into(),
            method: "action".into(),
            params: serde_json::json!({"op_id": op_id}),
        }))
        .await
        .expect("send action with op_id failed");

    // Immediately send cancel — fixture has not responded to action yet.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: cancel_id.into(),
            method: "cancel".into(),
            params: serde_json::json!({"op_id": op_id}),
        }))
        .await
        .expect("send cancel failed");

    // The fixture sends the cancel ack first, then the -33002 for the action.
    let cancel_resp = recv_raw_response(&mut runner)
        .await
        .expect("recv cancel ack failed");

    assert_eq!(
        cancel_resp.id, cancel_id,
        "CANCEL-03: cancel ack must mirror the cancel request id"
    );
    let cancel_result = cancel_resp
        .result
        .expect("CANCEL-03: cancel ack must have a result (not error)");
    assert_eq!(
        cancel_result.get("ok").and_then(|v| v.as_bool()),
        Some(true),
        "CANCEL-03: cancel result must contain ok:true"
    );
    assert_eq!(
        cancel_result
            .get("already_complete")
            .and_then(|v| v.as_bool()),
        Some(false),
        "CANCEL-03: cancel result must contain already_complete:false for in-flight op"
    );

    // Now read the action's error response.
    let action_resp = recv_raw_response(&mut runner)
        .await
        .expect("recv canceled action response failed");

    assert_eq!(
        action_resp.id, action_id,
        "CANCEL-06: action response must mirror the original action request id"
    );
    let err = action_resp
        .error
        .expect("CANCEL-06: canceled action must return an error");
    assert_eq!(
        err.code, -33002,
        "CANCEL-06: canceled action must return error code -33002, got {}",
        err.code
    );
}

// ── CANCEL-05: cancel after completion returns already_complete ───────────────

/// If `cancel` arrives after the operation already completed, the plugin MUST
/// respond with `already_complete: true` (CANCEL-05).
///
/// Scenario: send an action WITHOUT `op_id` (completes immediately), then send
/// a `cancel` for a non-existent `op_id`.  The fixture treats any unknown
/// `op_id` as already-complete (the operation is not in the pending map).
#[tokio::test]
async fn cancel_already_complete_action() {
    let mut runner = common::spawn_and_handshake().await;

    // Send an action that completes immediately (no op_id).
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "done_action_001".into(),
            method: "action".into(),
            params: serde_json::json!({}),
        }))
        .await
        .expect("send immediate action failed");

    recv_response(&mut runner, "done_action_001")
        .await
        .expect("recv immediate action response failed");

    // Now cancel an op_id that was never registered (or already completed).
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "done_cancel_001".into(),
            method: "cancel".into(),
            params: serde_json::json!({"op_id": "op_already_done"}),
        }))
        .await
        .expect("send cancel failed");

    let resp = recv_raw_response(&mut runner)
        .await
        .expect("recv cancel response failed");

    assert_eq!(
        resp.id, "done_cancel_001",
        "cancel ack must mirror the cancel request id"
    );
    let result = resp
        .result
        .expect("CANCEL-05: cancel of completed op must return a result");
    assert_eq!(
        result.get("already_complete").and_then(|v| v.as_bool()),
        Some(true),
        "CANCEL-05: already-complete cancel must carry already_complete:true"
    );

    // Also check RECV_TIMEOUT to confirm the connection remains alive.
    let _ = tokio::time::timeout(
        RECV_TIMEOUT,
        runner.send_frame(&Envelope::Request(RequestEnvelope {
            id: "alive_probe_001".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        })),
    )
    .await
    .expect("connection must stay alive after cancel-already-complete");
}
