//! Wire framing — 4-byte big-endian length-prefix framing for the Dust protocol.
//!
//! Every frame on the wire is:
//! ```text
//! ┌─────────────────────────┬──────────────────────┐
//! │ 4-byte length (u32 BE)  │  UTF-8 JSON payload  │
//! └─────────────────────────┴──────────────────────┘
//! ```
//!
//! The length field contains the exact byte count of the payload, **not** including
//! the 4-byte prefix itself.  See DUST-WIRE-SPEC.md §2 for the full normative table.

use std::fmt;
use std::io::{self, Read, Write};

use serde::Serialize;
use serde_json::{Map, Value};

/// Maximum permitted payload size per **FRAME-03**: 1 MiB (`0x00100000`).
pub const MAX_FRAME_SIZE: u32 = 0x0010_0000;

/// The five valid `kind` strings defined in the v1 protocol.
const VALID_KINDS: &[&str] = &["request", "response", "event", "heartbeat", "shutdown"];

// ── FrameError ───────────────────────────────────────────────────────────────

/// All error conditions defined in the §2 normative framing table.
///
/// When any variant other than [`FrameError::Disconnected`] is returned, the
/// caller MUST close the connection and transition the plugin to the `dead` state.
#[derive(Debug)]
pub enum FrameError {
    /// **FRAME-06**: EOF received while reading the 4-byte length prefix.
    ///
    /// This is a clean disconnect, not a protocol violation.  No error is logged.
    Disconnected,

    /// **FRAME-05**: `length` field exceeds [`MAX_FRAME_SIZE`].
    ///
    /// Log `frame_oversized` with the received length value.
    Oversized { length: u32 },

    /// **FRAME-07**: EOF received after reading the length prefix but before
    /// `expected` payload bytes were available.
    ///
    /// Log `frame_truncated` with the byte counts.
    Truncated { received: usize, expected: usize },

    /// **FRAME-08**: payload bytes are not valid UTF-8.
    ///
    /// Log `frame_utf8_error`.
    Utf8Error(std::string::FromUtf8Error),

    /// **FRAME-09**: payload is valid UTF-8 but not parseable as JSON.
    ///
    /// Log `frame_malformed_json`.  Per **FRAME-16** the receiver MUST NOT
    /// attempt to extract an `id` field from the broken payload.
    MalformedJson(serde_json::Error),

    /// **FRAME-10**: payload is valid JSON but the top-level value is not a
    /// JSON object (e.g. array, string, number, or `null`).
    NonObject,

    /// **FRAME-11**: payload is a JSON object that contains no `kind` field.
    MissingKind,

    /// **FRAME-12**: `kind` value is not one of the five defined protocol kinds.
    UnknownKind(String),

    /// **FRAME-13 / FRAME-15**: per-frame read deadline elapsed (500 ms from
    /// the first buffered payload byte).
    ///
    /// Triggered when the underlying reader returns [`io::ErrorKind::TimedOut`]
    /// or [`io::ErrorKind::WouldBlock`] during payload reads.
    /// Log `frame_read_timeout`.
    ReadTimeout,

    /// **FRAME-14**: per-frame write deadline elapsed (1 000 ms from the first
    /// written frame byte).
    ///
    /// Triggered when the underlying writer returns [`io::ErrorKind::TimedOut`]
    /// or [`io::ErrorKind::WouldBlock`].
    /// Log `frame_write_timeout`.
    WriteTimeout,

    /// An I/O error not covered by the variants above.
    Io(io::Error),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => write!(f, "disconnected"),
            Self::Oversized { length } => {
                write!(f, "frame_oversized: length {length} exceeds {MAX_FRAME_SIZE}")
            }
            Self::Truncated { received, expected } => {
                write!(f, "frame_truncated: received {received} of {expected} bytes")
            }
            Self::Utf8Error(e) => write!(f, "frame_utf8_error: {e}"),
            Self::MalformedJson(e) => write!(f, "frame_malformed_json: {e}"),
            Self::NonObject => {
                write!(f, "frame_non_object: top-level JSON value is not an object")
            }
            Self::MissingKind => write!(f, "frame_missing_kind: envelope has no `kind` field"),
            Self::UnknownKind(k) => {
                write!(f, "frame_unknown_kind: `{k}` is not a recognized protocol kind")
            }
            Self::ReadTimeout => write!(f, "frame_read_timeout"),
            Self::WriteTimeout => write!(f, "frame_write_timeout"),
            Self::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Utf8Error(e) => Some(e),
            Self::MalformedJson(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => Self::ReadTimeout,
            _ => Self::Io(e),
        }
    }
}

// ── read_frame ───────────────────────────────────────────────────────────────

/// Read one complete frame from `reader` and validate it through FRAME-12.
///
/// # Return values
///
/// | Return | Meaning |
/// |--------|---------|
/// | `Ok(None)` | Zero-length frame (**FRAME-04**) or clean EOF on the length prefix (**FRAME-06**). No action required. |
/// | `Ok(Some(obj))` | A validated JSON object whose `kind` field is one of the five defined kinds. |
/// | `Err(e)` | Protocol violation — the caller MUST close the connection. |
///
/// # Timeout mapping
///
/// If the underlying reader returns [`io::ErrorKind::TimedOut`] or
/// [`io::ErrorKind::WouldBlock`] during payload reads, the error is mapped to
/// [`FrameError::ReadTimeout`].  Callers that need the 500 ms per-frame deadline
/// (**FRAME-13**) should set `SO_RCVTIMEO` (or equivalent) on the underlying
/// socket before calling this function.
pub fn read_frame<R: Read>(
    reader: &mut R,
) -> Result<Option<Map<String, Value>>, FrameError> {
    // ── Step 1: read the 4-byte big-endian length prefix ──────────────────
    let mut len_buf = [0u8; 4];
    let mut prefix_read = 0usize;
    while prefix_read < 4 {
        match reader.read(&mut len_buf[prefix_read..]) {
            // FRAME-06: EOF while reading the length prefix → clean disconnect.
            Ok(0) => return Ok(None),
            Ok(n) => prefix_read += n,
            Err(e) if e.kind() == io::ErrorKind::TimedOut
                || e.kind() == io::ErrorKind::WouldBlock =>
            {
                return Err(FrameError::ReadTimeout);
            }
            Err(e) => return Err(FrameError::Io(e)),
        }
    }

    let length = u32::from_be_bytes(len_buf);

    // ── Step 2: FRAME-04 — zero-length frame, accepted, no effect ─────────
    if length == 0 {
        return Ok(None);
    }

    // ── Step 3: FRAME-05 — reject oversized frames ─────────────────────────
    if length > MAX_FRAME_SIZE {
        return Err(FrameError::Oversized { length });
    }

    // ── Step 4: read exactly `length` payload bytes ────────────────────────
    let expected = length as usize;
    let mut payload = vec![0u8; expected];
    let mut received = 0usize;
    while received < expected {
        match reader.read(&mut payload[received..]) {
            // FRAME-07: EOF after the length prefix but before all bytes.
            Ok(0) => {
                return Err(FrameError::Truncated { received, expected });
            }
            Ok(n) => received += n,
            Err(e) if e.kind() == io::ErrorKind::TimedOut
                || e.kind() == io::ErrorKind::WouldBlock =>
            {
                // FRAME-13 / FRAME-15: per-frame read deadline.
                return Err(FrameError::ReadTimeout);
            }
            Err(e) => return Err(FrameError::Io(e)),
        }
    }

    // ── Step 5: FRAME-08 — validate UTF-8 ─────────────────────────────────
    let text = String::from_utf8(payload).map_err(FrameError::Utf8Error)?;

    // ── Step 6: FRAME-09 — parse JSON ─────────────────────────────────────
    let value: Value = serde_json::from_str(&text).map_err(FrameError::MalformedJson)?;

    // ── Step 7: FRAME-10 — must be a JSON object ───────────────────────────
    let obj = match value {
        Value::Object(map) => map,
        _ => return Err(FrameError::NonObject),
    };

    // ── Step 8: FRAME-11 — must have a string `kind` field ────────────────
    let kind = match obj.get("kind") {
        Some(Value::String(k)) => k.clone(),
        _ => return Err(FrameError::MissingKind),
    };

    // ── Step 9: FRAME-12 — `kind` must be one of the five defined values ──
    if !VALID_KINDS.contains(&kind.as_str()) {
        return Err(FrameError::UnknownKind(kind));
    }

    Ok(Some(obj))
}

// ── write_frame ──────────────────────────────────────────────────────────────

/// Serialize `msg` to JSON and write one complete frame to `writer`.
///
/// The frame consists of a 4-byte big-endian length prefix followed by the
/// UTF-8 JSON payload.  The writer is flushed after the payload is written.
///
/// Returns [`FrameError::Oversized`] if the serialized payload exceeds
/// [`MAX_FRAME_SIZE`].
///
/// # Timeout mapping
///
/// If the underlying writer returns [`io::ErrorKind::TimedOut`] or
/// [`io::ErrorKind::WouldBlock`] at any point, the error is mapped to
/// [`FrameError::WriteTimeout`] (**FRAME-14**).  Callers that need the 1 s
/// per-frame write deadline should set `SO_SNDTIMEO` on the underlying socket
/// before calling this function.
pub fn write_frame<W: Write, T: Serialize>(writer: &mut W, msg: &T) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| FrameError::Io(io::Error::new(io::ErrorKind::InvalidData, e)))?;

    let length = payload.len();
    if length > MAX_FRAME_SIZE as usize {
        return Err(FrameError::Oversized {
            length: length.min(u32::MAX as usize) as u32,
        });
    }

    let map_write_err = |e: io::Error| match e.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => FrameError::WriteTimeout,
        _ => FrameError::Io(e),
    };

    writer
        .write_all(&(length as u32).to_be_bytes())
        .map_err(map_write_err)?;
    writer.write_all(&payload).map_err(map_write_err)?;
    writer.flush().map_err(map_write_err)?;

    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ── Helpers ───────────────────────────────────────────────────────────

    fn make_frame(payload: &[u8]) -> Vec<u8> {
        let mut buf = (payload.len() as u32).to_be_bytes().to_vec();
        buf.extend_from_slice(payload);
        buf
    }

    fn make_valid_frame(kind: &str) -> Vec<u8> {
        let json = format!(r#"{{"kind":"{kind}","ts":"2026-04-12T00:00:00.000Z"}}"#);
        make_frame(json.as_bytes())
    }

    // Reader that always returns TimedOut.
    struct TimeoutReader;
    impl Read for TimeoutReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
        }
    }

    // Reader that sends its initial bytes then returns TimedOut (slowloris).
    struct SlowlorisReader {
        data: Vec<u8>,
        pos: usize,
    }
    impl Read for SlowlorisReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.pos < self.data.len() {
                buf[0] = self.data[self.pos];
                self.pos += 1;
                Ok(1)
            } else {
                Err(io::Error::new(io::ErrorKind::TimedOut, "slowloris"))
            }
        }
    }

    // Writer that always returns TimedOut.
    struct TimeoutWriter;
    impl Write for TimeoutWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    // ── FRAME-04: zero-length frame ───────────────────────────────────────

    #[test]
    fn frame_04_zero_length_is_accepted() {
        let buf = 0u32.to_be_bytes().to_vec();
        let mut cursor = Cursor::new(buf);
        let result = read_frame(&mut cursor);
        assert!(
            matches!(result, Ok(None)),
            "zero-length frame must return Ok(None)"
        );
    }

    // ── FRAME-05: oversized frame (read path) ─────────────────────────────

    #[test]
    fn frame_05_read_oversized_length_prefix() {
        let oversized = MAX_FRAME_SIZE + 1;
        let buf = oversized.to_be_bytes().to_vec(); // no payload needed
        let mut cursor = Cursor::new(buf);
        let result = read_frame(&mut cursor);
        assert!(
            matches!(result, Err(FrameError::Oversized { length }) if length == MAX_FRAME_SIZE + 1),
            "must return Oversized for length > MAX_FRAME_SIZE"
        );
    }

    // ── FRAME-05: oversized frame (write path) ────────────────────────────

    #[test]
    fn frame_05_write_oversized_payload() {
        // Build a payload that serializes to just over MAX_FRAME_SIZE bytes.
        // We use a JSON string value large enough to exceed the limit.
        let big_string = "x".repeat(MAX_FRAME_SIZE as usize + 1);
        let msg = serde_json::json!({ "kind": "heartbeat", "data": big_string });
        let mut buf: Vec<u8> = Vec::new();
        let result = write_frame(&mut buf, &msg);
        assert!(
            matches!(result, Err(FrameError::Oversized { .. })),
            "write_frame must reject payloads > MAX_FRAME_SIZE"
        );
    }

    // ── FRAME-06: EOF on length prefix ────────────────────────────────────

    #[test]
    fn frame_06_eof_on_length_prefix() {
        let mut cursor = Cursor::new(vec![]); // empty reader
        let result = read_frame(&mut cursor);
        assert!(
            matches!(result, Ok(None)),
            "clean EOF on length prefix must return Ok(None)"
        );
    }

    // ── FRAME-07: truncated payload ───────────────────────────────────────

    #[test]
    fn frame_07_truncated_payload() {
        // Length prefix says 10 bytes, but only 5 bytes of payload follow.
        let mut buf = 10u32.to_be_bytes().to_vec();
        buf.extend_from_slice(b"hello"); // 5 bytes, not 10
        let mut cursor = Cursor::new(buf);
        let result = read_frame(&mut cursor);
        assert!(
            matches!(result, Err(FrameError::Truncated { received: 5, expected: 10 })),
            "must return Truncated with correct byte counts"
        );
    }

    // ── FRAME-08: non-UTF-8 payload ───────────────────────────────────────

    #[test]
    fn frame_08_non_utf8_payload() {
        let payload: &[u8] = &[0xFF, 0xFE, 0xFD]; // invalid UTF-8
        let buf = make_frame(payload);
        let mut cursor = Cursor::new(buf);
        assert!(
            matches!(read_frame(&mut cursor), Err(FrameError::Utf8Error(_))),
            "non-UTF-8 payload must return Utf8Error"
        );
    }

    // ── FRAME-09: valid UTF-8, not JSON ───────────────────────────────────

    #[test]
    fn frame_09_malformed_json() {
        let payload = b"this is not json {{{";
        let buf = make_frame(payload);
        let mut cursor = Cursor::new(buf);
        assert!(
            matches!(read_frame(&mut cursor), Err(FrameError::MalformedJson(_))),
            "non-JSON UTF-8 must return MalformedJson"
        );
    }

    // ── FRAME-10: valid JSON but not an object ────────────────────────────

    #[test]
    fn frame_10_json_array_is_non_object() {
        let payload = b"[1, 2, 3]";
        let buf = make_frame(payload);
        let mut cursor = Cursor::new(buf);
        assert!(
            matches!(read_frame(&mut cursor), Err(FrameError::NonObject)),
            "JSON array must return NonObject"
        );
    }

    #[test]
    fn frame_10_json_null_is_non_object() {
        let buf = make_frame(b"null");
        let mut cursor = Cursor::new(buf);
        assert!(matches!(read_frame(&mut cursor), Err(FrameError::NonObject)));
    }

    // ── FRAME-11: object without `kind` ───────────────────────────────────

    #[test]
    fn frame_11_missing_kind_field() {
        let payload = br#"{"id":"req_1","method":"manifest"}"#;
        let buf = make_frame(payload);
        let mut cursor = Cursor::new(buf);
        assert!(
            matches!(read_frame(&mut cursor), Err(FrameError::MissingKind)),
            "object without `kind` must return MissingKind"
        );
    }

    // ── FRAME-12: unrecognized kind ───────────────────────────────────────

    #[test]
    fn frame_12_unknown_kind_value() {
        let payload = br#"{"kind":"rpc","id":"req_1"}"#;
        let buf = make_frame(payload);
        let mut cursor = Cursor::new(buf);
        assert!(
            matches!(read_frame(&mut cursor), Err(FrameError::UnknownKind(k)) if k == "rpc"),
            "unrecognized kind must return UnknownKind with the offending value"
        );
    }

    // ── FRAME-13: read timeout ────────────────────────────────────────────

    #[test]
    fn frame_13_read_timeout_on_length_prefix() {
        let mut reader = TimeoutReader;
        assert!(
            matches!(read_frame(&mut reader), Err(FrameError::ReadTimeout)),
            "TimedOut from reader must map to ReadTimeout"
        );
    }

    // ── FRAME-14: write timeout ───────────────────────────────────────────

    #[test]
    fn frame_14_write_timeout() {
        let msg = serde_json::json!({"kind":"heartbeat","ts":"2026-04-12T00:00:00.000Z"});
        let mut writer = TimeoutWriter;
        assert!(
            matches!(write_frame(&mut writer, &msg), Err(FrameError::WriteTimeout)),
            "TimedOut from writer must map to WriteTimeout"
        );
    }

    // ── FRAME-15: slowloris (subset of FRAME-13) ──────────────────────────

    #[test]
    fn frame_15_slowloris_read_timeout_during_payload() {
        // Send a length prefix declaring 100 bytes, then drip one byte then timeout.
        let expected_len = 100u32;
        let mut data = expected_len.to_be_bytes().to_vec();
        data.push(b'{'); // only 1 byte of payload before the timeout fires
        let mut reader = SlowlorisReader { data, pos: 0 };
        assert!(
            matches!(read_frame(&mut reader), Err(FrameError::ReadTimeout)),
            "slowloris reader must trigger ReadTimeout"
        );
    }

    // ── Round-trip: happy path ────────────────────────────────────────────

    #[test]
    fn round_trip_valid_heartbeat_frame() {
        let msg = serde_json::json!({"kind":"heartbeat","ts":"2026-04-12T00:00:00.000Z"});
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, &msg).expect("write_frame must succeed");

        let mut cursor = Cursor::new(buf);
        let obj = read_frame(&mut cursor)
            .expect("read_frame must succeed")
            .expect("must be Some");

        assert_eq!(obj.get("kind").and_then(|v| v.as_str()), Some("heartbeat"));
    }

    #[test]
    fn valid_kinds_round_trip() {
        for kind in VALID_KINDS {
            let buf = make_valid_frame(kind);
            let mut cursor = Cursor::new(buf);
            let result = read_frame(&mut cursor);
            assert!(
                matches!(result, Ok(Some(_))),
                "kind `{kind}` must be accepted"
            );
        }
    }

    #[test]
    fn frame_error_display_covers_all_variants() {
        // Smoke test that Display doesn't panic for any variant.
        let errors: &[FrameError] = &[
            FrameError::Disconnected,
            FrameError::Oversized { length: 2_000_000 },
            FrameError::Truncated { received: 3, expected: 10 },
            FrameError::NonObject,
            FrameError::MissingKind,
            FrameError::UnknownKind("bad".into()),
            FrameError::ReadTimeout,
            FrameError::WriteTimeout,
        ];
        for e in errors {
            let s = e.to_string();
            assert!(!s.is_empty(), "Display must not produce empty string");
        }
    }
}
