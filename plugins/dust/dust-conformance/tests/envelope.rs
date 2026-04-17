//! Conformance assertions for DUST-WIRE-SPEC.md §3 — Envelope.
//!
//! Each test spawns a fresh `dust-fixture-minimal`, completes the handshake,
//! and verifies a specific envelope-level invariant.
//!
//! | Test | Spec rule | Assertion |
//! |------|-----------|-----------|
//! | `valid_request_returns_valid_response` | ENVELOPE-01/02 | result present, id matches |
//! | `duplicate_request_id_returns_32600` | ENVELOPE-06 | error.code == -32600 |
//! | `unknown_method_returns_32601` | ENVELOPE-07 | error.code == -32601 |
//! | `response_result_and_error_mutually_exclusive` | ENVELOPE-04 | never both set |

mod common;

use dust_conformance::{recv_raw_response, recv_response};
use dust_core::envelope::{Envelope, RequestEnvelope};

// ── ENVELOPE-01/02: valid request → valid response ────────────────────────────

/// Send a `manifest` request and assert the response contains a well-formed
/// result with at least `name` and `version` fields and the correct request ID.
#[tokio::test]
async fn valid_request_returns_valid_response() {
    let mut runner = common::spawn_and_handshake().await;

    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "env_req_0001".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .expect("send manifest request failed");

    let resp = recv_response(&mut runner, "env_req_0001")
        .await
        .expect("recv_response failed");

    let result = resp.result.expect("manifest response must have result");
    assert!(
        result.get("name").and_then(|v| v.as_str()).is_some(),
        "manifest result must contain a string `name` field, got: {result}"
    );
    assert!(
        result.get("version").and_then(|v| v.as_str()).is_some(),
        "manifest result must contain a string `version` field, got: {result}"
    );
    assert!(
        resp.error.is_none(),
        "manifest response must not have an error field when result is set"
    );
}

// ── ENVELOPE-06: duplicate request ID → -32600 ───────────────────────────────

/// Send two requests with the same ID.  The fixture must return a valid
/// response for the first and an error with code -32600 for the second.
#[tokio::test]
async fn duplicate_request_id_returns_32600() {
    let mut runner = common::spawn_and_handshake().await;

    // First request — must succeed.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "dup_id_001".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .expect("send first request failed");

    let first = recv_raw_response(&mut runner)
        .await
        .expect("recv first response failed");
    assert_eq!(first.id, "dup_id_001", "first response id mismatch");
    assert!(first.result.is_some(), "first response must have result");
    assert!(first.error.is_none(), "first response must not have error");

    // Second request — same ID, must be rejected with -32600.
    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "dup_id_001".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .expect("send duplicate request failed");

    let second = recv_raw_response(&mut runner)
        .await
        .expect("recv duplicate response failed");
    assert_eq!(second.id, "dup_id_001", "duplicate response id mismatch");

    let err = second
        .error
        .expect("duplicate request must produce an error response");
    assert_eq!(
        err.code, -32600,
        "duplicate request must return error code -32600, got {}",
        err.code
    );
    assert!(
        second.result.is_none(),
        "error response must not have a result field"
    );
}

// ── ENVELOPE-07: unknown method → -32601 ─────────────────────────────────────

/// Send a request with a method name that the fixture doesn't implement.
/// The fixture must respond with error code -32601 (method not found).
#[tokio::test]
async fn unknown_method_returns_32601() {
    let mut runner = common::spawn_and_handshake().await;

    runner
        .send_frame(&Envelope::Request(RequestEnvelope {
            id: "unknown_method_001".into(),
            method: "nonexistent_method_xyz".into(),
            params: serde_json::Value::Null,
        }))
        .await
        .expect("send unknown method request failed");

    let resp = recv_raw_response(&mut runner)
        .await
        .expect("recv response for unknown method failed");
    assert_eq!(resp.id, "unknown_method_001", "response id mismatch");

    let err = resp
        .error
        .expect("unknown method must produce an error response");
    assert_eq!(
        err.code, -32601,
        "unknown method must return error code -32601 (method not found), got {}",
        err.code
    );
    assert!(
        resp.result.is_none(),
        "error response must not have a result field"
    );
}

// ── ENVELOPE-04: result and error are mutually exclusive ──────────────────────

/// For every response the fixture produces (manifest, render, action, unknown),
/// assert that exactly one of `result` / `error` is set — never both, never neither.
///
/// This verifies ENVELOPE-04: "A response MUST contain exactly one of `result`
/// or `error`.  A receiver MUST close the connection if both or neither are set."
#[tokio::test]
async fn response_result_and_error_mutually_exclusive() {
    let mut runner = common::spawn_and_handshake().await;

    let cases: &[(&str, &str)] = &[
        ("excl_req_001", "manifest"),
        ("excl_req_002", "render"),
        ("excl_req_003", "action"),
        ("excl_req_004", "events.subscribe"),
        ("excl_req_005", "no_such_method"),
    ];

    for (id, method) in cases {
        runner
            .send_frame(&Envelope::Request(RequestEnvelope {
                id: id.to_string(),
                method: method.to_string(),
                params: serde_json::Value::Null,
            }))
            .await
            .unwrap_or_else(|e| panic!("send request {id}/{method} failed: {e}"));

        let resp = recv_raw_response(&mut runner)
            .await
            .unwrap_or_else(|e| panic!("recv response {id}/{method} failed: {e}"));

        assert_eq!(resp.id, *id, "response id mismatch for {method}");

        let has_result = resp.result.is_some();
        let has_error = resp.error.is_some();

        assert!(
            has_result ^ has_error,
            "ENVELOPE-04 violation for method `{method}` (id={id}): \
             result_set={has_result}, error_set={has_error} — exactly one must be set"
        );
    }
}
