//! dust-core — protocol types and wire helpers for the Nanika dust plugin system.
//!
//! Wire format: each message is a 4-byte big-endian length prefix followed by
//! a UTF-8 JSON payload. Both host and plugin use the same framing on stdin/stdout.

use std::io::{self, Read, Write};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

// ── Color ────────────────────────────────────────────────────────────────────

/// 24-bit RGB color.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

// ── TextStyle ────────────────────────────────────────────────────────────────

/// Text rendering hints for UI components.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TextStyle {
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<Color>,
}


// ── ListItem ─────────────────────────────────────────────────────────────────

/// A single row in a list component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ListItem {
    /// Stable identifier used for action dispatch.
    pub id: String,
    /// Primary display label.
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub disabled: bool,
}

impl ListItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            description: None,
            icon: None,
            disabled: false,
        }
    }
}

// ── Component ────────────────────────────────────────────────────────────────

/// UI components that a plugin can render in the dashboard.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Component {
    Text {
        content: String,
        #[serde(default, skip_serializing_if = "is_default_text_style")]
        style: TextStyle,
    },
    List {
        items: Vec<ListItem>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    /// Raw markdown rendered by the host.
    Markdown {
        content: String,
    },
    /// A horizontal divider.
    Divider,
}

fn is_default_text_style(s: &TextStyle) -> bool {
    s == &TextStyle::default()
}

// ── Capability ───────────────────────────────────────────────────────────────

/// Capabilities a plugin advertises to the dashboard host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Capability {
    /// Plugin can render a widget in the sidebar.
    Widget {
        /// Refresh interval hint in seconds. 0 = no auto-refresh.
        #[serde(default)]
        refresh_secs: u32,
    },
    /// Plugin responds to user commands from the command palette.
    Command {
        /// Prefix used to disambiguate commands, e.g. "tracker".
        prefix: String,
    },
    /// Plugin can handle background scheduled jobs.
    Scheduler,
}

// ── PluginManifest ───────────────────────────────────────────────────────────

/// Describes the plugin to the dashboard host (returned on `manifest` request).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<Capability>,
    /// Icon key from the dashboard icon map, e.g. "Like", "Mission".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

// ── ActionResult ─────────────────────────────────────────────────────────────

/// Outcome of a plugin action invocation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionResult {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

impl ActionResult {
    pub fn ok() -> Self {
        Self {
            success: true,
            message: None,
            data: None,
        }
    }

    pub fn ok_with(message: impl Into<String>) -> Self {
        Self {
            success: true,
            message: Some(message.into()),
            data: None,
        }
    }

    pub fn err(message: impl Into<String>) -> Self {
        Self {
            success: false,
            message: Some(message.into()),
            data: None,
        }
    }
}

// ── Request / Response ───────────────────────────────────────────────────────

/// A request sent from the dashboard host to a plugin process.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    /// Correlation ID — echoed back in the matching [`Response`].
    pub id: String,
    /// Method name, e.g. `"manifest"`, `"render"`, `"action"`.
    pub method: String,
    /// Method-specific parameters. `null` when a method takes no params.
    #[serde(default)]
    pub params: serde_json::Value,
}

/// A response sent from a plugin process back to the dashboard host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Must match the [`Request::id`] that triggered this response.
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

/// Structured error payload inside a [`Response`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i32,
    pub message: String,
}

impl Response {
    /// Build a successful response carrying a serializable result.
    pub fn ok<T: Serialize>(id: impl Into<String>, result: &T) -> Self {
        Self {
            id: id.into(),
            result: Some(serde_json::to_value(result).expect("result must be serializable")),
            error: None,
        }
    }

    /// Build an error response.
    pub fn err(id: impl Into<String>, code: i32, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            result: None,
            error: Some(ResponseError {
                code,
                message: message.into(),
            }),
        }
    }
}

// ── Wire helpers ─────────────────────────────────────────────────────────────

/// Write a single message to `writer` using the 4-byte-length-prefix framing.
///
/// The length field is the byte length of the JSON payload encoded as a
/// big-endian `u32`. After writing the payload the writer is flushed.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, msg: &T) -> io::Result<()> {
    let payload = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let len: u32 = payload
        .len()
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "message too large"))?;

    writer.write_all(&len.to_be_bytes())?;
    writer.write_all(&payload)?;
    writer.flush()
}

/// Read a single message from `reader` using the 4-byte-length-prefix framing.
///
/// Blocks until the full message arrives. Returns `Err` with
/// `ErrorKind::UnexpectedEof` when the reader closes mid-stream.
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload)?;

    serde_json::from_slice(&payload)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn round_trip_request() {
        let req = Request {
            id: "req-1".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        };

        let mut buf = Vec::new();
        write_message(&mut buf, &req).unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: Request = read_message(&mut cursor).unwrap();

        assert_eq!(decoded, req);
    }

    #[test]
    fn round_trip_response_ok() {
        let manifest = PluginManifest {
            name: "dust-demo".into(),
            version: "0.1.0".into(),
            description: "Demo plugin".into(),
            capabilities: vec![Capability::Widget { refresh_secs: 30 }],
            icon: None,
        };
        let resp = Response::ok("req-1", &manifest);

        let mut buf = Vec::new();
        write_message(&mut buf, &resp).unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: Response = read_message(&mut cursor).unwrap();

        assert_eq!(decoded.id, "req-1");
        assert!(decoded.result.is_some());
        assert!(decoded.error.is_none());
    }

    #[test]
    fn round_trip_response_err() {
        let resp = Response::err("req-2", -32601, "method not found");

        let mut buf = Vec::new();
        write_message(&mut buf, &resp).unwrap();

        let mut cursor = Cursor::new(buf);
        let decoded: Response = read_message(&mut cursor).unwrap();

        assert_eq!(decoded.error.as_ref().unwrap().code, -32601);
    }

    #[test]
    fn list_item_builder() {
        let item = ListItem::new("item-1", "Hello");
        assert_eq!(item.id, "item-1");
        assert!(!item.disabled);
    }

    #[test]
    fn action_result_constructors() {
        assert!(ActionResult::ok().success);
        assert!(!ActionResult::err("oops").success);
        assert_eq!(
            ActionResult::err("oops").message.as_deref(),
            Some("oops")
        );
    }

    #[test]
    fn component_serialization_roundtrip() {
        let comp = Component::List {
            items: vec![ListItem::new("a", "Alpha")],
            title: Some("Results".into()),
        };
        let json = serde_json::to_string(&comp).unwrap();
        let back: Component = serde_json::from_str(&json).unwrap();
        assert_eq!(comp, back);
    }

    #[test]
    fn color_and_text_style() {
        let style = TextStyle {
            bold: true,
            color: Some(Color::new(255, 0, 128)),
            ..Default::default()
        };
        let json = serde_json::to_string(&style).unwrap();
        let back: TextStyle = serde_json::from_str(&json).unwrap();
        assert_eq!(back.color.unwrap().r, 255);
    }
}
