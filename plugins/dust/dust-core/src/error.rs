//! Error code registry and structured error type for the Dust wire protocol (§4).
//!
//! The error code registry is a **closed enum** in v1 (**ERROR-01**).  Plugins
//! MUST NOT invent codes at the top-level `code` field of a response error
//! object (**ERROR-06**).  Domain-specific errors MUST be surfaced via the
//! `error.data.plugin_code` field (**ERROR-02**).

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ── DustErrorCode ─────────────────────────────────────────────────────────────

/// The closed set of error codes defined in §4 of the Dust Wire Protocol.
///
/// The underlying `i32` value is the canonical wire representation.  Use
/// [`DustErrorCode::code`] to obtain the integer, or `i32::from(code)`.
///
/// Two codes are **reserved for logging only** and MUST NOT appear in a
/// response `error.code` field:
/// - [`DustErrorCode::ParseError`] (`-32700`) — connection is closed on parse
///   failure per **FRAME-09**.
/// - [`DustErrorCode::FrameOversized`] (`-33009`) — connection is closed per
///   **FRAME-05**.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum DustErrorCode {
    // ── JSON-RPC lineage codes (-327xx) ───────────────────────────────────

    /// `-32700` — **Reserved for logging only** (**FRAME-09**).
    ///
    /// The receiver closes the connection on JSON parse failure rather than
    /// sending this code.  Implementations MUST NOT send `-32700` in a response.
    ParseError = -32700,

    /// `-32600` — Envelope is malformed, contains a duplicate `id`, or is
    /// missing a required field (**ENVELOPE-15**).
    InvalidRequest = -32600,

    /// `-32601` — The `method` field names a method the receiver does not implement.
    MethodNotFound = -32601,

    /// `-32602` — The `params` object fails method-specific validation.
    InvalidParams = -32602,

    /// `-32603` — An unexpected error occurred on the receiver side.
    ///
    /// Receivers SHOULD treat occurrences of this code as bugs.
    InternalError = -32603,

    // ── Dust-specific codes (-330xx) ──────────────────────────────────────

    /// `-33001` — The request exceeded its per-request deadline.
    Timeout = -33001,

    /// `-33002` — The operation was canceled via a `cancel` request.
    Canceled = -33002,

    /// `-33003` — Peer advertised a protocol version outside the supported range.
    UnsupportedVersion = -33003,

    /// `-33004` — Peer credential check failed or caller's role is denied.
    Unauthorized = -33004,

    /// `-33005` — Backpressure: in-flight or queue limit reached.
    ///
    /// The sender SHOULD retry with exponential backoff.
    Busy = -33005,

    /// `-33006` — Receiver is draining and rejects new requests.
    ///
    /// The sender MUST NOT retry until reconnection.
    ShuttingDown = -33006,

    /// `-33007` — `since_sequence` cursor is older than the ring buffer's
    /// retention window; the subscriber missed events.
    ReplayGap = -33007,

    /// `-33008` — Registry-side synthetic error for a plugin in the `dead` state.
    PluginDead = -33008,

    /// `-33009` — **Reserved for logging only** (**FRAME-05**).
    ///
    /// When **FRAME-05** fires the implementation logs `frame_oversized` but
    /// closes the connection rather than sending this code.
    /// Implementations MUST NOT send `-33009` in a response.
    FrameOversized = -33009,
}

impl DustErrorCode {
    /// The integer error code as it appears on the wire.
    pub fn code(self) -> i32 {
        self as i32
    }

    /// The canonical snake_case name for this error code.
    pub fn name(self) -> &'static str {
        match self {
            Self::ParseError => "parse_error",
            Self::InvalidRequest => "invalid_request",
            Self::MethodNotFound => "method_not_found",
            Self::InvalidParams => "invalid_params",
            Self::InternalError => "internal_error",
            Self::Timeout => "timeout",
            Self::Canceled => "canceled",
            Self::UnsupportedVersion => "unsupported_version",
            Self::Unauthorized => "unauthorized",
            Self::Busy => "busy",
            Self::ShuttingDown => "shutting_down",
            Self::ReplayGap => "replay_gap",
            Self::PluginDead => "plugin_dead",
            Self::FrameOversized => "frame_oversized",
        }
    }

    /// Look up a [`DustErrorCode`] by its integer value.
    ///
    /// Returns `None` if `code` is not in the §4 registry.
    pub fn from_i32(code: i32) -> Option<Self> {
        match code {
            -32700 => Some(Self::ParseError),
            -32600 => Some(Self::InvalidRequest),
            -32601 => Some(Self::MethodNotFound),
            -32602 => Some(Self::InvalidParams),
            -32603 => Some(Self::InternalError),
            -33001 => Some(Self::Timeout),
            -33002 => Some(Self::Canceled),
            -33003 => Some(Self::UnsupportedVersion),
            -33004 => Some(Self::Unauthorized),
            -33005 => Some(Self::Busy),
            -33006 => Some(Self::ShuttingDown),
            -33007 => Some(Self::ReplayGap),
            -33008 => Some(Self::PluginDead),
            -33009 => Some(Self::FrameOversized),
            _ => None,
        }
    }
}

impl From<DustErrorCode> for i32 {
    fn from(code: DustErrorCode) -> i32 {
        code as i32
    }
}

impl fmt::Display for DustErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name(), self.code())
    }
}

// ── DustError ─────────────────────────────────────────────────────────────────

/// A structured error object as used in `response` envelopes (**ENVELOPE-05**).
///
/// The `code` field is stored as `i32` for direct JSON serialization.  Use
/// [`DustError::from`] with a [`DustErrorCode`] to construct a well-typed error.
///
/// # Plugin-specific errors
///
/// Plugins that need domain-specific error codes MUST set `data.plugin_code`
/// rather than inventing new top-level codes (**ERROR-02**).
///
/// ```rust
/// # use dust_core::error::{DustError, DustErrorCode};
/// # use serde_json::json;
/// let err = DustError::new(DustErrorCode::InternalError, "database locked")
///     .with_data(json!({ "plugin_code": "DB_LOCKED" }));
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DustError {
    /// Error code from the §4 registry.
    pub code: i32,
    /// Human-readable description.
    pub message: String,
    /// Optional additional structured context (**ENVELOPE-05**, **ERROR-02**).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl DustError {
    /// Construct a [`DustError`] from a registry code and message.
    pub fn new(code: DustErrorCode, message: impl Into<String>) -> Self {
        Self {
            code: code.code(),
            message: message.into(),
            data: None,
        }
    }

    /// Attach structured context data to this error.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }
}

impl From<DustErrorCode> for DustError {
    /// Build a [`DustError`] using the code's canonical name as the message.
    fn from(code: DustErrorCode) -> Self {
        Self::new(code, code.name())
    }
}

impl fmt::Display for DustError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for DustError {}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── One test per error code constant ─────────────────────────────────

    #[test]
    fn code_parse_error() {
        assert_eq!(DustErrorCode::ParseError.code(), -32700);
        assert_eq!(DustErrorCode::ParseError.name(), "parse_error");
        assert_eq!(DustErrorCode::from_i32(-32700), Some(DustErrorCode::ParseError));
    }

    #[test]
    fn code_invalid_request() {
        assert_eq!(DustErrorCode::InvalidRequest.code(), -32600);
        assert_eq!(DustErrorCode::InvalidRequest.name(), "invalid_request");
        assert_eq!(DustErrorCode::from_i32(-32600), Some(DustErrorCode::InvalidRequest));
    }

    #[test]
    fn code_method_not_found() {
        assert_eq!(DustErrorCode::MethodNotFound.code(), -32601);
        assert_eq!(DustErrorCode::MethodNotFound.name(), "method_not_found");
        assert_eq!(DustErrorCode::from_i32(-32601), Some(DustErrorCode::MethodNotFound));
    }

    #[test]
    fn code_invalid_params() {
        assert_eq!(DustErrorCode::InvalidParams.code(), -32602);
        assert_eq!(DustErrorCode::InvalidParams.name(), "invalid_params");
        assert_eq!(DustErrorCode::from_i32(-32602), Some(DustErrorCode::InvalidParams));
    }

    #[test]
    fn code_internal_error() {
        assert_eq!(DustErrorCode::InternalError.code(), -32603);
        assert_eq!(DustErrorCode::InternalError.name(), "internal_error");
        assert_eq!(DustErrorCode::from_i32(-32603), Some(DustErrorCode::InternalError));
    }

    #[test]
    fn code_timeout() {
        assert_eq!(DustErrorCode::Timeout.code(), -33001);
        assert_eq!(DustErrorCode::Timeout.name(), "timeout");
        assert_eq!(DustErrorCode::from_i32(-33001), Some(DustErrorCode::Timeout));
    }

    #[test]
    fn code_canceled() {
        assert_eq!(DustErrorCode::Canceled.code(), -33002);
        assert_eq!(DustErrorCode::Canceled.name(), "canceled");
        assert_eq!(DustErrorCode::from_i32(-33002), Some(DustErrorCode::Canceled));
    }

    #[test]
    fn code_unsupported_version() {
        assert_eq!(DustErrorCode::UnsupportedVersion.code(), -33003);
        assert_eq!(DustErrorCode::UnsupportedVersion.name(), "unsupported_version");
        assert_eq!(DustErrorCode::from_i32(-33003), Some(DustErrorCode::UnsupportedVersion));
    }

    #[test]
    fn code_unauthorized() {
        assert_eq!(DustErrorCode::Unauthorized.code(), -33004);
        assert_eq!(DustErrorCode::Unauthorized.name(), "unauthorized");
        assert_eq!(DustErrorCode::from_i32(-33004), Some(DustErrorCode::Unauthorized));
    }

    #[test]
    fn code_busy() {
        assert_eq!(DustErrorCode::Busy.code(), -33005);
        assert_eq!(DustErrorCode::Busy.name(), "busy");
        assert_eq!(DustErrorCode::from_i32(-33005), Some(DustErrorCode::Busy));
    }

    #[test]
    fn code_shutting_down() {
        assert_eq!(DustErrorCode::ShuttingDown.code(), -33006);
        assert_eq!(DustErrorCode::ShuttingDown.name(), "shutting_down");
        assert_eq!(DustErrorCode::from_i32(-33006), Some(DustErrorCode::ShuttingDown));
    }

    #[test]
    fn code_replay_gap() {
        assert_eq!(DustErrorCode::ReplayGap.code(), -33007);
        assert_eq!(DustErrorCode::ReplayGap.name(), "replay_gap");
        assert_eq!(DustErrorCode::from_i32(-33007), Some(DustErrorCode::ReplayGap));
    }

    #[test]
    fn code_plugin_dead() {
        assert_eq!(DustErrorCode::PluginDead.code(), -33008);
        assert_eq!(DustErrorCode::PluginDead.name(), "plugin_dead");
        assert_eq!(DustErrorCode::from_i32(-33008), Some(DustErrorCode::PluginDead));
    }

    #[test]
    fn code_frame_oversized() {
        assert_eq!(DustErrorCode::FrameOversized.code(), -33009);
        assert_eq!(DustErrorCode::FrameOversized.name(), "frame_oversized");
        assert_eq!(DustErrorCode::from_i32(-33009), Some(DustErrorCode::FrameOversized));
    }

    // ── from_i32 rejects unknown codes ────────────────────────────────────

    #[test]
    fn from_i32_unknown_code_returns_none() {
        assert_eq!(DustErrorCode::from_i32(0), None);
        assert_eq!(DustErrorCode::from_i32(-1), None);
        assert_eq!(DustErrorCode::from_i32(-99999), None);
    }

    // ── Into<i32> ─────────────────────────────────────────────────────────

    #[test]
    fn into_i32_conversion() {
        let code: i32 = DustErrorCode::MethodNotFound.into();
        assert_eq!(code, -32601);
    }

    // ── Display ───────────────────────────────────────────────────────────

    #[test]
    fn error_code_display_includes_name_and_value() {
        let s = DustErrorCode::InvalidParams.to_string();
        assert!(s.contains("invalid_params"), "display must contain name: {s}");
        assert!(s.contains("-32602"), "display must contain code value: {s}");
    }

    // ── DustError construction ────────────────────────────────────────────

    #[test]
    fn dust_error_new() {
        let err = DustError::new(DustErrorCode::MethodNotFound, "no such method");
        assert_eq!(err.code, -32601);
        assert_eq!(err.message, "no such method");
        assert!(err.data.is_none());
    }

    #[test]
    fn dust_error_from_code_uses_canonical_name() {
        let err = DustError::from(DustErrorCode::Unauthorized);
        assert_eq!(err.code, -33004);
        assert_eq!(err.message, "unauthorized");
    }

    #[test]
    fn dust_error_with_data() {
        let err = DustError::new(DustErrorCode::InternalError, "db locked")
            .with_data(json!({"plugin_code": "DB_LOCKED"}));
        assert!(err.data.is_some());
        assert_eq!(err.data.as_ref().unwrap()["plugin_code"], "DB_LOCKED");
    }

    // ── DustError serde round-trip ────────────────────────────────────────

    #[test]
    fn dust_error_serde_round_trip_without_data() {
        let err = DustError::new(DustErrorCode::Busy, "too many in-flight requests");
        let json = serde_json::to_string(&err).unwrap();
        assert!(!json.contains("data"), "absent data must not serialize");
        let back: DustError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    #[test]
    fn dust_error_serde_round_trip_with_data() {
        let err = DustError::new(DustErrorCode::InvalidParams, "bad field")
            .with_data(json!({"field": "op_id", "reason": "must be UUID"}));
        let json = serde_json::to_string(&err).unwrap();
        let back: DustError = serde_json::from_str(&json).unwrap();
        assert_eq!(err, back);
    }

    // ── DustError Display and Error impls ─────────────────────────────────

    #[test]
    fn dust_error_display() {
        let err = DustError::new(DustErrorCode::Timeout, "deadline exceeded");
        let s = err.to_string();
        assert!(s.contains("-33001"), "display must include code: {s}");
        assert!(s.contains("deadline exceeded"), "display must include message: {s}");
    }

    #[test]
    fn dust_error_is_std_error() {
        let err = DustError::from(DustErrorCode::PluginDead);
        // Verify it satisfies std::error::Error by calling source().
        let boxed: Box<dyn std::error::Error> = Box::new(err);
        assert!(boxed.source().is_none());
    }
}
