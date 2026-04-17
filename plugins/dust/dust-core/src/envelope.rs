//! Protocol envelope types for the Dust wire protocol (DUST-WIRE-SPEC.md §3).
//!
//! Every framed JSON payload is one of five envelope kinds, discriminated by a
//! top-level `kind` field:
//!
//! | Kind | Direction |
//! |------|-----------|
//! | `request` | Either side may initiate |
//! | `response` | Method handler → requester |
//! | `event` | Either side may push |
//! | `heartbeat` | Either side |
//! | `shutdown` | Registry → plugin only |

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── MessageKind ──────────────────────────────────────────────────────────────

/// The discriminant for the five protocol envelope kinds (§3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Request,
    Response,
    Event,
    Heartbeat,
    Shutdown,
}

impl MessageKind {
    /// The canonical wire string for this kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Response => "response",
            Self::Event => "event",
            Self::Heartbeat => "heartbeat",
            Self::Shutdown => "shutdown",
        }
    }
}

impl fmt::Display for MessageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ── EnvelopeError ────────────────────────────────────────────────────────────

/// Validation errors for envelope-level invariants.
///
/// Currently covers the `result`/`error` exclusivity rule defined in
/// **ENVELOPE-04**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvelopeError {
    /// **ENVELOPE-04**: a `response` envelope contains both `result` and `error`.
    ///
    /// The receiver MUST close the connection.
    BothResultAndError,

    /// **ENVELOPE-04**: a `response` envelope contains neither `result` nor `error`.
    ///
    /// The receiver MUST close the connection.
    NeitherResultNorError,
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BothResultAndError => {
                write!(f, "response contains both `result` and `error` (ENVELOPE-04)")
            }
            Self::NeitherResultNorError => {
                write!(f, "response contains neither `result` nor `error` (ENVELOPE-04)")
            }
        }
    }
}

impl std::error::Error for EnvelopeError {}

// ── ShutdownReason ───────────────────────────────────────────────────────────

/// Reason codes for a `shutdown` envelope (**ENVELOPE-12**).
///
/// Sent by the registry to the plugin only (**ENVELOPE-11**).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShutdownReason {
    /// The host process is shutting down.
    HostExit,
    /// The plugin was disabled by the user or configuration.
    PluginDisable,
    /// Handshake protocol version is outside the host's supported range.
    VersionMismatch,
    /// The plugin's manifest file was deleted from disk.
    WatcherDelete,
    /// The plugin binary was removed or is no longer executable.
    BinaryDeleted,
    /// The file watcher encountered an unrecoverable error for this plugin.
    WatcherError,
    /// A consumer reported an unrecoverable error for this plugin.
    ConsumerError,
    /// The plugin failed to respond within the host's deadline.
    Timeout,
}

// ── EventType ────────────────────────────────────────────────────────────────

/// The v1 event type vocabulary (**ENVELOPE-07**).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    /// Plugin has completed initialization (plugin → host).
    Ready,
    /// Host configuration pushed after handshake (host → plugin).
    HostInfo,
    /// Plugin lifecycle state change (plugin → host).
    StatusChanged,
    /// Long-running operation progress (plugin → host).
    Progress,
    /// Structured log record (plugin → host).
    Log,
    /// Non-fatal error notification (plugin → host).
    Error,
    /// Domain data changed (plugin → host).
    DataUpdated,
    /// Host requests the plugin re-render or re-fetch (host → plugin).
    Refresh,
    /// Consumer visibility state change (host → plugin).
    VisibilityChanged,
}

// ── ActionParams ─────────────────────────────────────────────────────────────

/// Typed parameters for action-dispatch requests (**LIFECYCLE-05**).
///
/// An `op_id` groups multiple `progress` events under a single logical
/// operation for heartbeat-pause tracking.  An `item_id` identifies a specific
/// list item being acted upon.  Additional key-value pairs are carried in
/// `args`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ActionParams {
    /// Groups progress events for heartbeat-pause tracking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub op_id: Option<String>,

    /// Identifies the specific list item being acted upon.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,

    /// Arbitrary action-specific key-value arguments.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub args: HashMap<String, Value>,
}

// ── ErrorObject ──────────────────────────────────────────────────────────────

/// The structured `error` object inside a `response` envelope (**ENVELOPE-05**).
///
/// The `code` field MUST be one of the codes defined in §4.
/// The `data` field MAY carry additional structured context (see **ERROR-02**
/// for the `plugin_code` convention).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    /// Error code from the §4 registry (stored as `i32` for wire compatibility).
    pub code: i32,
    /// Human-readable error description.
    pub message: String,
    /// Optional additional structured context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

// ── Per-kind envelope structs ────────────────────────────────────────────────

/// Body of a `request` envelope (**ENVELOPE-02**).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    /// Unique request identifier matching `req_<16hex>` (**ENVELOPE-14**).
    pub id: String,

    /// The method name to invoke.
    pub method: String,

    /// Method-specific parameters.  If absent on the wire the receiver MUST
    /// treat it as `{}` (**ENVELOPE-02**); we use `Value::Null` as the default.
    #[serde(default)]
    pub params: Value,
}

/// Body of a `response` envelope (**ENVELOPE-03**).
///
/// Exactly one of `result` or `error` MUST be present (**ENVELOPE-04**).
/// Call [`ResponseEnvelope::validate`] after deserialization to enforce this.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    /// Mirrors the `id` of the originating request.
    pub id: String,

    /// Present if and only if the method succeeded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Present if and only if the method failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorObject>,
}

impl ResponseEnvelope {
    /// Enforce **ENVELOPE-04**: exactly one of `result` or `error` must be present.
    ///
    /// Returns `Err` when the response contains both fields or neither field.
    pub fn validate(&self) -> Result<(), EnvelopeError> {
        match (&self.result, &self.error) {
            (Some(_), Some(_)) => Err(EnvelopeError::BothResultAndError),
            (None, None) => Err(EnvelopeError::NeitherResultNorError),
            _ => Ok(()),
        }
    }
}

/// Body of an `event` envelope (**ENVELOPE-06**).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventEnvelope {
    /// Unique event identifier matching `evt_<16hex>` (**ENVELOPE-14**).
    pub id: String,

    /// One of the defined event types (**ENVELOPE-07**).
    #[serde(rename = "type")]
    pub event_type: EventType,

    /// ISO 8601 timestamp with millisecond precision.
    pub ts: String,

    /// Monotonically increasing per plugin connection.
    /// MUST be present on plugin-originated events; MAY be absent on host events.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sequence: Option<u64>,

    /// Event-specific payload.
    pub data: Value,
}

/// Body of a `heartbeat` envelope (**ENVELOPE-08**).
///
/// Heartbeats carry no `id` field and MUST NOT be logged (**ENVELOPE-09**).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeartbeatEnvelope {
    /// ISO 8601 timestamp with millisecond precision.
    pub ts: String,
}

/// Body of a `shutdown` envelope (**ENVELOPE-10**).
///
/// Unidirectional: registry → plugin only (**ENVELOPE-11**).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShutdownEnvelope {
    /// One of the eight defined reason codes (**ENVELOPE-12**).
    pub reason: ShutdownReason,
}

// ── Envelope ─────────────────────────────────────────────────────────────────

/// A complete protocol envelope, discriminated by the `kind` field (§3.1).
///
/// Serializes/deserializes using serde's internally-tagged enum representation,
/// so the `kind` field appears at the top level of the JSON object alongside the
/// kind-specific fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Envelope {
    Request(RequestEnvelope),
    Response(ResponseEnvelope),
    Event(EventEnvelope),
    Heartbeat(HeartbeatEnvelope),
    Shutdown(ShutdownEnvelope),
}

impl Envelope {
    /// Return the [`MessageKind`] discriminant for this envelope.
    pub fn kind(&self) -> MessageKind {
        match self {
            Self::Request(_) => MessageKind::Request,
            Self::Response(_) => MessageKind::Response,
            Self::Event(_) => MessageKind::Event,
            Self::Heartbeat(_) => MessageKind::Heartbeat,
            Self::Shutdown(_) => MessageKind::Shutdown,
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── Envelope kind: request ────────────────────────────────────────────

    #[test]
    fn request_envelope_serde_round_trip() {
        let env = Envelope::Request(RequestEnvelope {
            id: "req_a1b2c3d4e5f67890".into(),
            method: "manifest".into(),
            params: json!({}),
        });
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""kind":"request""#), "kind tag missing");
        assert!(json.contains(r#""method":"manifest""#));

        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.kind(), MessageKind::Request);
    }

    // ── Envelope kind: response (success) ────────────────────────────────

    #[test]
    fn response_envelope_success_serde_round_trip() {
        let env = Envelope::Response(ResponseEnvelope {
            id: "req_a1b2c3d4e5f67890".into(),
            result: Some(json!({"name": "tracker", "version": "1.2.0"})),
            error: None,
        });
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""kind":"response""#));
        assert!(!json.contains("\"error\""), "error field must be absent");

        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.kind(), MessageKind::Response);

        if let Envelope::Response(r) = &back {
            r.validate().expect("valid response must pass validation");
        }
    }

    // ── Envelope kind: response (error) ───────────────────────────────────

    #[test]
    fn response_envelope_error_serde_round_trip() {
        let env = Envelope::Response(ResponseEnvelope {
            id: "req_a1b2c3d4e5f67890".into(),
            result: None,
            error: Some(ErrorObject {
                code: -32601,
                message: "method not found".into(),
                data: None,
            }),
        });
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""kind":"response""#));
        assert!(!json.contains("\"result\""), "result field must be absent");

        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
    }

    // ── Envelope kind: event ──────────────────────────────────────────────

    #[test]
    fn event_envelope_serde_round_trip() {
        let env = Envelope::Event(EventEnvelope {
            id: "evt_f0e1d2c3b4a59687".into(),
            event_type: EventType::StatusChanged,
            ts: "2026-04-12T09:30:00.123Z".into(),
            sequence: Some(42),
            data: json!({"status": "ready"}),
        });
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""kind":"event""#));
        assert!(json.contains(r#""type":"status_changed""#));
        assert!(json.contains(r#""sequence":42"#));

        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.kind(), MessageKind::Event);
    }

    // ── Envelope kind: heartbeat ──────────────────────────────────────────

    #[test]
    fn heartbeat_envelope_serde_round_trip() {
        let env = Envelope::Heartbeat(HeartbeatEnvelope {
            ts: "2026-04-12T09:30:00.123Z".into(),
        });
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""kind":"heartbeat""#));
        assert!(!json.contains("\"id\""), "heartbeat must have no id field");

        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.kind(), MessageKind::Heartbeat);
    }

    // ── Envelope kind: shutdown ───────────────────────────────────────────

    #[test]
    fn shutdown_envelope_serde_round_trip() {
        let env = Envelope::Shutdown(ShutdownEnvelope {
            reason: ShutdownReason::HostExit,
        });
        let json = serde_json::to_string(&env).unwrap();
        assert!(json.contains(r#""kind":"shutdown""#));
        assert!(json.contains(r#""reason":"host_exit""#));

        let back: Envelope = serde_json::from_str(&json).unwrap();
        assert_eq!(env, back);
        assert_eq!(back.kind(), MessageKind::Shutdown);
    }

    // ── All shutdown reasons serialize correctly ───────────────────────────

    #[test]
    fn all_shutdown_reasons_round_trip() {
        let reasons = [
            ShutdownReason::HostExit,
            ShutdownReason::PluginDisable,
            ShutdownReason::VersionMismatch,
            ShutdownReason::WatcherDelete,
            ShutdownReason::BinaryDeleted,
            ShutdownReason::WatcherError,
            ShutdownReason::ConsumerError,
            ShutdownReason::Timeout,
        ];
        for reason in reasons {
            let env = Envelope::Shutdown(ShutdownEnvelope { reason });
            let json = serde_json::to_string(&env).unwrap();
            let back: Envelope = serde_json::from_str(&json).unwrap();
            assert_eq!(env, back, "round-trip failed for {reason:?}");
        }
    }

    // ── ENVELOPE-04: result/error exclusivity ─────────────────────────────

    #[test]
    fn envelope_04_both_result_and_error_is_invalid() {
        let r = ResponseEnvelope {
            id: "req_1".into(),
            result: Some(json!({})),
            error: Some(ErrorObject {
                code: -32603,
                message: "oops".into(),
                data: None,
            }),
        };
        assert_eq!(r.validate(), Err(EnvelopeError::BothResultAndError));
    }

    #[test]
    fn envelope_04_neither_result_nor_error_is_invalid() {
        let r = ResponseEnvelope {
            id: "req_1".into(),
            result: None,
            error: None,
        };
        assert_eq!(r.validate(), Err(EnvelopeError::NeitherResultNorError));
    }

    #[test]
    fn envelope_04_only_result_is_valid() {
        let r = ResponseEnvelope {
            id: "req_1".into(),
            result: Some(json!({"ok": true})),
            error: None,
        };
        assert!(r.validate().is_ok());
    }

    #[test]
    fn envelope_04_only_error_is_valid() {
        let r = ResponseEnvelope {
            id: "req_1".into(),
            result: None,
            error: Some(ErrorObject { code: -32601, message: "not found".into(), data: None }),
        };
        assert!(r.validate().is_ok());
    }

    // ── ActionParams ──────────────────────────────────────────────────────

    #[test]
    fn action_params_serde_round_trip() {
        let mut params = ActionParams {
            op_id: Some("op_abc".into()),
            item_id: Some("item_123".into()),
            args: HashMap::new(),
        };
        params.args.insert("limit".into(), json!(50));

        let json = serde_json::to_string(&params).unwrap();
        let back: ActionParams = serde_json::from_str(&json).unwrap();
        assert_eq!(params, back);
    }

    #[test]
    fn action_params_omits_empty_args() {
        let params = ActionParams::default();
        let json = serde_json::to_string(&params).unwrap();
        assert!(!json.contains("args"), "empty args must be omitted: {json}");
    }

    // ── MessageKind Display ───────────────────────────────────────────────

    #[test]
    fn message_kind_display() {
        assert_eq!(MessageKind::Request.to_string(), "request");
        assert_eq!(MessageKind::Response.to_string(), "response");
        assert_eq!(MessageKind::Event.to_string(), "event");
        assert_eq!(MessageKind::Heartbeat.to_string(), "heartbeat");
        assert_eq!(MessageKind::Shutdown.to_string(), "shutdown");
    }
}
