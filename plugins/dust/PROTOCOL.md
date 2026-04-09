# Dust Plugin Protocol

Nanika's plugin system for the dust dashboard. Binary plugins communicate via length-prefixed JSON over Unix sockets.

## Wire Format

Each message is a 4-byte big-endian `u32` length prefix followed by UTF-8 JSON.

| Byte Offset | Type | Value | Description |
|---|---|---|---|
| 0–3 | `u32` BE | `n` | Length of JSON payload in bytes |
| 4–(4+n) | UTF-8 | JSON object | Request or Response |

**Example: Send a manifest request**
```
Request:
{
  "id": "req-1",
  "method": "manifest",
  "params": null
}

Hex:
00 00 00 3B  {"id":"req-1","method":"manifest","params":null}
|           |
length (59) payload
```

Use the wire helpers in `dust-core`:
```rust
dust_core::write_message(&mut writer, &request)?;
let response: Response = dust_core::read_message(&mut reader)?;
```

## Socket Convention

Plugins bind a Unix socket at `/tmp/dust/<plugin-id>.sock`.

The `plugin_id()` must be:
- **URL-safe**: alphanumeric, hyphen, underscore only
- **Stable**: does not change between versions
- **Unique**: among all dashboard plugins

**Examples:**
- `hello-plugin` → `/tmp/dust/hello-plugin.sock`
- `dust-tracker` → `/tmp/dust/dust-tracker.sock`
- `github-watcher` → `/tmp/dust/github-watcher.sock`

The `/tmp/dust` directory is created automatically by `dust_sdk::run()`. Stale socket files from a previous process are removed on startup.

## PluginManifest Schema

Returned by the `manifest` request. Describes the plugin to the dashboard.

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Display name, e.g. `"Hello Plugin"` |
| `version` | string | yes | SemVer, e.g. `"0.1.0"` |
| `description` | string | yes | One-liner. Keywords go here if needed. |
| `capabilities` | array | yes | List of `Capability` objects (see below) |
| `icon` | string | optional | Icon key from dashboard icon map, e.g. `"Like"`, `"Mission"` |

**Example:**
```json
{
  "name": "Hello Plugin",
  "version": "0.1.0",
  "description": "Greets the world. Keywords: hello, greet.",
  "capabilities": [
    {
      "kind": "command",
      "prefix": "hello"
    }
  ],
  "icon": null
}
```

## Capability Schema

A plugin advertises one or more capabilities to the dashboard.

### Capability::Widget
```json
{
  "kind": "widget",
  "refresh_secs": 30
}
```
Plugin can render a persistent widget in the sidebar. `refresh_secs=0` disables auto-refresh.

### Capability::Command
```json
{
  "kind": "command",
  "prefix": "hello"
}
```
Plugin responds to user commands from the command palette. The `prefix` disambiguates commands across plugins (e.g., `hello <search-term>`).

### Capability::Scheduler
```json
{
  "kind": "scheduler"
}
```
Plugin can handle background scheduled jobs (defined separately, not via the protocol).

## Component Model

A plugin's `render()` method returns an array of UI components. Each component is a tagged JSON object.

### Component::Text
```json
{
  "type": "text",
  "content": "Status: Running",
  "style": {
    "bold": true,
    "italic": false,
    "underline": false,
    "color": {
      "r": 255,
      "g": 128,
      "b": 0
    }
  }
}
```

| Field | Type | Default | Description |
|---|---|---|---|
| `content` | string | — | Text to render |
| `style.bold` | bool | false | Bold text |
| `style.italic` | bool | false | Italic text |
| `style.underline` | bool | false | Underline text |
| `style.color` | object | null | RGB color (r, g, b: 0–255) |

### Component::List
```json
{
  "type": "list",
  "title": "Results",
  "items": [
    {
      "id": "item-1",
      "label": "First Result",
      "description": "Optional secondary text",
      "icon": "Star",
      "disabled": false
    }
  ]
}
```

| Field | Type | Default | Description |
|---|---|---|---|
| `title` | string | null | Optional list heading |
| `items` | array | — | Array of ListItem objects |
| `items[].id` | string | — | Stable identifier for action dispatch |
| `items[].label` | string | — | Primary display text |
| `items[].description` | string | null | Secondary text below label |
| `items[].icon` | string | null | Icon key from dashboard icon map |
| `items[].disabled` | bool | false | Greyed out; actions cannot be dispatched |

### Component::Markdown
```json
{
  "type": "markdown",
  "content": "# Heading\n\nParagraph with **bold**."
}
```
Rendered as CommonMark by the dashboard.

### Component::Divider
```json
{
  "type": "divider"
}
```
Horizontal separator.

## Request/Response Structure

### Request
```json
{
  "id": "req-1",
  "method": "manifest",
  "params": null
}
```

| Field | Type | Description |
|---|---|---|
| `id` | string | Correlation ID; echoed in matching Response |
| `method` | string | Method name: `"manifest"`, `"render"`, or `"action"` |
| `params` | value | Method-specific parameters; `null` for manifest/render |

### Response
```json
{
  "id": "req-1",
  "result": { "name": "Hello Plugin", "version": "0.1.0", "..." },
  "error": null
}
```

| Field | Type | Description |
|---|---|---|
| `id` | string | Must match request `id` |
| `result` | value | Serialized result on success; omitted on error |
| `error` | object | Error object on failure; omitted on success |

### ResponseError
```json
{
  "code": -32601,
  "message": "method not found"
}
```

| Field | Type | Description |
|---|---|---|
| `code` | int | Error code (follows JSON-RPC convention) |
| `message` | string | Human-readable error message |

## SDK Usage

Use `dust-sdk` to implement a plugin without writing wire protocol code. Depend on `dust-sdk` in your `Cargo.toml`:

```toml
[dependencies]
dust-sdk = { path = "../dust-sdk" }
tokio = { version = "1", features = ["rt", "net", "io-util", "fs"] }
async-trait = "0.1"
```

Implement the `DustPlugin` trait:

```rust
use std::sync::Arc;
use async_trait::async_trait;
use dust_sdk::{
    ActionResult, Capability, Component, DustPlugin, ListItem, PluginManifest,
};

struct HelloPlugin;

#[async_trait]
impl DustPlugin for HelloPlugin {
    fn plugin_id(&self) -> &str {
        "hello-plugin"
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "Hello Plugin".into(),
            version: "0.1.0".into(),
            description: "Greets the world. Keywords: hello, greet.".into(),
            capabilities: vec![Capability::Command {
                prefix: "hello".into(),
            }],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        vec![Component::List {
            title: Some("Greetings".into()),
            items: vec![
                ListItem::new("hello-world", "Hello, World!"),
                ListItem::new("hello-nanika", "Hello, Nanika!"),
                ListItem::new("hello-dust", "Hello, Dust!"),
            ],
        }]
    }

    async fn action(&self, params: serde_json::Value) -> ActionResult {
        ActionResult::ok()
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dust_sdk::run(Arc::new(HelloPlugin)).await
}
```

## Action Flow

**Trigger:** User selects an item in a list (or triggers a command).

**Step 1: Dashboard sends action request**
```json
{
  "id": "req-2",
  "method": "action",
  "params": {
    "item_id": "hello-world"
  }
}
```

**Step 2: Plugin receives request**
The SDK's `dispatch()` calls your `action()` method with the params.

**Step 3: Plugin returns ActionResult**
```rust
async fn action(&self, params: serde_json::Value) -> ActionResult {
    let item_id = params["item_id"].as_str().unwrap_or("?");
    ActionResult::ok_with(format!("Clicked {}", item_id))
}
```

**Step 4: SDK returns Response**
```json
{
  "id": "req-2",
  "result": {
    "success": true,
    "message": "Clicked hello-world",
    "data": null
  },
  "error": null
}
```

### ActionResult Constructors

| Method | Example | Result |
|---|---|---|
| `ActionResult::ok()` | `ActionResult::ok()` | `success: true` with no message |
| `ActionResult::ok_with(msg)` | `ActionResult::ok_with("Done")` | `success: true` with message |
| `ActionResult::err(msg)` | `ActionResult::err("File not found")` | `success: false` with message |

## Method Reference

### manifest
**Request:** `{ "method": "manifest", "params": null }`

**Response:** `{ "result": PluginManifest }`

Returns the plugin manifest. Called once at startup and on every dashboard refresh. Keep this fast.

### render
**Request:** `{ "method": "render", "params": null }`

**Response:** `{ "result": [Component, ...] }`

Returns the current UI state as an array of components. Called periodically (based on widget refresh intervals) or on demand after an action.

### action
**Request:** `{ "method": "action", "params": <json> }`

**Response:** `{ "result": ActionResult }`

Executes a user action. The `params` object contains:
- `item_id` (string): The ID of the list item that was clicked (if applicable)
- Any additional fields the plugin defines

## Error Handling

### Parse Errors
If the plugin receives invalid JSON:
1. Eprintln the error (for logging)
2. Continue listening for the next request
3. Do **not** send a response

### Unknown Methods
Return an error response:
```json
{
  "id": "req-X",
  "result": null,
  "error": {
    "code": -32601,
    "message": "method not found: <method>"
  }
}
```

### Action Failures
Return ActionResult with `success: false`:
```rust
ActionResult::err("Database connection failed")
```

The dashboard shows the error message to the user.

## Constraints

### DO NOT:
- **Create a `ui/` subfolder.** The plugin is a backend service; the dashboard is the UI.
- **Import `Ratatui` or TUI crates.** No terminal UI logic. Render via JSON components only.
- **Share state with the dashboard process.** Use only the request/response protocol.
- **Assume a specific refresh interval.** The dashboard controls when `render()` is called.
- **Block in async methods.** Long-running work should spawn a background task.
- **Log to stdout.** Use `eprintln!()` for stderr logging only.

### DO:
- **Keep `plugin_id()` stable.** Changing it breaks the socket path.
- **Keep `manifest()` fast.** It's called on every dashboard refresh.
- **Use builder methods** for `ListItem` and `ActionResult`:
  ```rust
  ListItem::new("id", "label")
  ActionResult::ok_with("message")
  ActionResult::err("error")
  ```
- **Validate `params` in `action()`** before using them:
  ```rust
  let item_id = params["item_id"].as_str().ok_or("missing item_id")?;
  ```

## Logging

Plugins run as background processes. Use `eprintln!()` to write to stderr for debugging:

```rust
eprintln!("dust: action executed with params: {:?}", params);
```

The orchestrator captures stderr in the plugin's log file.

## Examples

### Hello Plugin (minimal)
See `plugins/hello-plugin/src/main.rs` for the reference implementation.

### Building a Plugin

1. Create a new binary crate:
   ```bash
   cargo new --name my-plugin plugins/dust/plugins/my-plugin
   ```

2. Add to the workspace (`plugins/dust/Cargo.toml`):
   ```toml
   [workspace]
   members = ["dust-core", "dust-sdk", "plugins/my-plugin"]
   ```

3. Implement `DustPlugin`:
   ```rust
   #[async_trait]
   impl DustPlugin for MyPlugin {
       fn plugin_id(&self) -> &str { "my-plugin" }
       async fn manifest(&self) -> PluginManifest { /* ... */ }
       async fn render(&self) -> Vec<Component> { /* ... */ }
       async fn action(&self, params: serde_json::Value) -> ActionResult { /* ... */ }
   }
   ```

4. Run the plugin:
   ```bash
   cargo run -p my-plugin
   ```
   It binds to `/tmp/dust/my-plugin.sock` and waits for connections.

5. Test with `nc`:
   ```bash
   echo -ne '\x00\x00\x00\x3B{"id":"1","method":"manifest","params":null}' | nc -U /tmp/dust/my-plugin.sock
   ```

## Testing

Use the test helpers in `dust-core`:

```rust
#[test]
fn test_roundtrip() {
    let req = Request {
        id: "1".into(),
        method: "manifest".into(),
        params: serde_json::Value::Null,
    };

    let mut buf = Vec::new();
    dust_core::write_message(&mut buf, &req).unwrap();

    let mut cursor = std::io::Cursor::new(buf);
    let decoded: Request = dust_core::read_message(&mut cursor).unwrap();

    assert_eq!(decoded.id, "1");
}
```

The SDK provides integration tests that spawn a real Unix socket server and send requests. See `dust-sdk/src/lib.rs::tests::run_bind_and_roundtrip`.

## Design Decisions

- **Length-prefixed framing:** Unambiguous message boundaries without a closing delimiter. Works with streaming protocols.
- **Big-endian u32:** Matches network byte order (used by other distributed systems).
- **No heartbeat:** Plugins are request-driven; the dashboard controls lifecycle.
- **No multiplexing:** One request/response per stream. Simple and safe.
- **JSON payloads:** Human-readable, widely supported, works with any language.
- **Unix sockets, not TCP:** Tighter security model; no network exposure; `/tmp/dust` is the only coordination point.
