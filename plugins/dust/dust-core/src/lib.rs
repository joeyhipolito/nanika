//! dust-core — protocol types and wire helpers for the Nanika dust plugin system.
//!
//! Wire format: each message is a 4-byte big-endian length prefix followed by
//! a UTF-8 JSON payload. Both host and plugin use the same framing on stdin/stdout.

pub mod envelope;
pub mod error;
pub mod events;
pub mod framing;
pub mod state;

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

// ── TableColumn ──────────────────────────────────────────────────────────────

/// A column definition for the [`Component::Table`] variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableColumn {
    /// Display header for the column.
    pub header: String,
    /// Optional width hint (number of terminal columns or CSS units depending on host).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

impl TableColumn {
    pub fn new(header: impl Into<String>) -> Self {
        Self { header: header.into(), width: None }
    }

    pub fn with_width(mut self, width: u32) -> Self {
        self.width = Some(width);
        self
    }
}

// ── KVPair ───────────────────────────────────────────────────────────────────

/// A single label/value row for [`Component::KeyValue`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KVPair {
    pub label: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_color: Option<Color>,
}

impl KVPair {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self { label: label.into(), value: value.into(), value_color: None }
    }

    pub fn with_color(mut self, color: Color) -> Self {
        self.value_color = Some(color);
        self
    }
}

// ── BadgeVariant ─────────────────────────────────────────────────────────────

/// Visual style hint for a [`Component::Badge`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BadgeVariant {
    Default,
    Outline,
    Filled,
    Subtle,
}

impl Default for BadgeVariant {
    fn default() -> Self {
        Self::Default
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
    /// A tabular grid with named columns and string rows.
    Table {
        columns: Vec<TableColumn>,
        rows: Vec<Vec<String>>,
    },
    /// A vertical list of label/value pairs (e.g. metadata summary).
    KeyValue {
        pairs: Vec<KVPair>,
    },
    /// A small inline label, optionally colored and styled.
    Badge {
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<Color>,
        #[serde(default, skip_serializing_if = "is_default_badge_variant")]
        variant: BadgeVariant,
    },
    /// A progress bar with an optional label.
    Progress {
        /// Current value. Should be in the range `[0, max]`.
        value: f64,
        /// Maximum value.
        max: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        label: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        color: Option<Color>,
    },
}

fn is_default_text_style(s: &TextStyle) -> bool {
    s == &TextStyle::default()
}

fn is_default_badge_variant(v: &BadgeVariant) -> bool {
    v == &BadgeVariant::Default
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

// ── RestartPolicy ─────────────────────────────────────────────────────────────

/// Controls how the registry respawns a plugin after a non-clean exit
/// (HOTPLUG-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestartPolicy {
    /// Never respawn the plugin, even after a crash.
    Never,
    /// Respawn up to 3 times with exponential backoff on non-zero exit (default).
    OnFailure,
    /// Respawn on any exit, including clean exit, using the same backoff schedule.
    Always,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::OnFailure
    }
}

// ── DustManifestBlock ─────────────────────────────────────────────────────────

fn default_heartbeat_interval_ms() -> u32 {
    10_000
}

fn default_shutdown_drain_ms() -> u32 {
    2_000
}

fn default_spawn_timeout_ms() -> u32 {
    5_000
}

/// The `dust:` block inside a `plugin.json` file (HOTPLUG-11).
///
/// Required fields: `binary`, `protocol_version`.
/// All other fields default to the values listed in the spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DustManifestBlock {
    /// Path to the plugin binary, relative to the plugin directory.
    /// MUST NOT contain `..` segments.
    pub binary: String,

    /// The plugin's protocol version as a semver string (e.g. `"1.0.0"`).
    pub protocol_version: String,

    /// Capability names this plugin advertises. Subset of
    /// `{"widget", "command", "scheduler"}`.
    #[serde(default)]
    pub capabilities: Vec<String>,

    /// Respawn policy on non-clean exit. Defaults to `on_failure`.
    #[serde(default)]
    pub restart: RestartPolicy,

    /// Heartbeat interval in milliseconds. Defaults to 10 000 ms. Must be
    /// in the range [1 000, 300 000].
    #[serde(default = "default_heartbeat_interval_ms")]
    pub heartbeat_interval_ms: u32,

    /// Drain deadline in milliseconds. Defaults to 2 000 ms. Must be in
    /// the range [100, 60 000].
    #[serde(default = "default_shutdown_drain_ms")]
    pub shutdown_drain_ms: u32,

    /// Time allowed (ms) from process spawn to socket file appearance.
    /// Defaults to 5 000 ms. Must be in the range [1 000, 60 000].
    #[serde(default = "default_spawn_timeout_ms")]
    pub spawn_timeout_ms: u32,

    /// JSONPath-subset strings identifying fields to redact from logs (§11).
    #[serde(default)]
    pub log_redact: Vec<String>,

    /// Optional arguments passed to the plugin binary on spawn (GAP-01).
    ///
    /// When present, the registry invokes `binary args[0] args[1] …` instead of
    /// `binary` alone. Useful for multi-command CLIs that expose dust-serve as a
    /// subcommand (e.g. `tracker dust-serve`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
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

    // ── New component variants ────────────────────────────────────────────────

    #[test]
    fn table_column_builder() {
        let col = TableColumn::new("Name").with_width(20);
        assert_eq!(col.header, "Name");
        assert_eq!(col.width, Some(20));
    }

    #[test]
    fn table_serde_roundtrip() {
        let comp = Component::Table {
            columns: vec![
                TableColumn::new("Name"),
                TableColumn::new("Status").with_width(10),
            ],
            rows: vec![
                vec!["alpha".into(), "ok".into()],
                vec!["beta".into(), "pending".into()],
            ],
        };
        let json = serde_json::to_string(&comp).unwrap();
        let back: Component = serde_json::from_str(&json).unwrap();
        assert_eq!(comp, back);
        // wire format sanity
        assert!(json.contains("\"type\":\"table\""));
    }

    #[test]
    fn kv_pair_builder() {
        let pair = KVPair::new("Version", "1.2.3").with_color(Color::new(0, 200, 0));
        assert_eq!(pair.label, "Version");
        assert_eq!(pair.value, "1.2.3");
        assert!(pair.value_color.is_some());
    }

    #[test]
    fn keyvalue_serde_roundtrip() {
        let comp = Component::KeyValue {
            pairs: vec![
                KVPair::new("Host", "localhost"),
                KVPair::new("Status", "running").with_color(Color::new(0, 255, 0)),
            ],
        };
        let json = serde_json::to_string(&comp).unwrap();
        let back: Component = serde_json::from_str(&json).unwrap();
        assert_eq!(comp, back);
        assert!(json.contains("\"type\":\"key_value\""));
    }

    #[test]
    fn keyvalue_omits_color_when_absent() {
        let pair = KVPair::new("Key", "Val");
        let json = serde_json::to_string(&pair).unwrap();
        assert!(!json.contains("value_color"));
    }

    #[test]
    fn badge_serde_roundtrip_default_variant() {
        let comp = Component::Badge {
            label: "new".into(),
            color: None,
            variant: BadgeVariant::Default,
        };
        let json = serde_json::to_string(&comp).unwrap();
        let back: Component = serde_json::from_str(&json).unwrap();
        assert_eq!(comp, back);
        // default variant should be omitted from wire format
        assert!(!json.contains("variant"), "default variant should not serialize: {json}");
    }

    #[test]
    fn badge_serde_roundtrip_non_default_variant() {
        let comp = Component::Badge {
            label: "beta".into(),
            color: Some(Color::new(100, 149, 237)),
            variant: BadgeVariant::Outline,
        };
        let json = serde_json::to_string(&comp).unwrap();
        let back: Component = serde_json::from_str(&json).unwrap();
        assert_eq!(comp, back);
        assert!(json.contains("\"variant\":\"outline\""));
    }

    #[test]
    fn progress_serde_roundtrip() {
        let comp = Component::Progress {
            value: 42.5,
            max: 100.0,
            label: Some("Loading…".into()),
            color: Some(Color::new(70, 130, 180)),
        };
        let json = serde_json::to_string(&comp).unwrap();
        let back: Component = serde_json::from_str(&json).unwrap();
        assert_eq!(comp, back);
        assert!(json.contains("\"type\":\"progress\""));
    }

    #[test]
    fn progress_omits_optional_fields_when_absent() {
        let comp = Component::Progress {
            value: 0.0,
            max: 1.0,
            label: None,
            color: None,
        };
        let json = serde_json::to_string(&comp).unwrap();
        assert!(!json.contains("label"));
        assert!(!json.contains("color"));
        let back: Component = serde_json::from_str(&json).unwrap();
        assert_eq!(comp, back);
    }
}
