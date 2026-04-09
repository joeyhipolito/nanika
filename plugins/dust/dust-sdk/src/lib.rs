//! dust-sdk — async plugin runtime for the Nanika dust dashboard system.
//!
//! Implement [`DustPlugin`] on your type and call [`run()`] to bind a Unix
//! socket at `/tmp/dust/<plugin-id>.sock`. The runtime accepts connections,
//! reads length-prefixed JSON [`Request`]s, dispatches them to your trait
//! methods, and writes [`Response`]s back using the same framing.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
pub use dust_core::{
    ActionResult, Capability, Color, Component, ListItem, PluginManifest, Request, Response,
    TextStyle,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

// ── DustPlugin trait ─────────────────────────────────────────────────────────

/// Implement this trait to expose a plugin to the Nanika dust dashboard.
///
/// `run()` uses `plugin_id()` to derive the socket path. All other methods
/// are dispatched from incoming [`Request`] messages.
#[async_trait]
pub trait DustPlugin: Send + Sync + 'static {
    /// Stable, URL-safe identifier, e.g. `"dust-tracker"`.
    ///
    /// Determines the socket path: `/tmp/dust/<plugin-id>.sock`.
    fn plugin_id(&self) -> &str;

    /// Return the plugin manifest (name, version, capabilities, …).
    ///
    /// Dispatched on `Request { method: "manifest", … }`.
    async fn manifest(&self) -> PluginManifest;

    /// Render the plugin's current UI as a list of [`Component`]s.
    ///
    /// Dispatched on `Request { method: "render", … }`.
    async fn render(&self) -> Vec<Component>;

    /// Execute a user action with the given params JSON.
    ///
    /// Dispatched on `Request { method: "action", params: … }`.
    async fn action(&self, params: serde_json::Value) -> ActionResult;
}

// ── run() ────────────────────────────────────────────────────────────────────

/// Bind `/tmp/dust/<plugin-id>.sock` and serve requests until the process
/// exits or an unrecoverable I/O error occurs.
///
/// Each accepted connection is handled in its own Tokio task. Stale socket
/// files from a previous run are removed automatically.
pub async fn run<P: DustPlugin>(plugin: Arc<P>) -> std::io::Result<()> {
    let socket_dir = PathBuf::from("/tmp/dust");
    tokio::fs::create_dir_all(&socket_dir).await?;

    let socket_path = socket_dir.join(format!("{}.sock", plugin.plugin_id()));

    // Remove stale socket from a previous process run.
    let _ = tokio::fs::remove_file(&socket_path).await;

    let listener = UnixListener::bind(&socket_path)?;

    loop {
        let (stream, _addr) = listener.accept().await?;
        let plugin = Arc::clone(&plugin);
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, plugin).await {
                // Only log non-EOF errors — EOF is normal when host closes.
                if e.kind() != std::io::ErrorKind::UnexpectedEof {
                    eprintln!("dust-sdk: connection error: {e}");
                }
            }
        });
    }
}

// ── Connection handler ───────────────────────────────────────────────────────

async fn handle_connection<P: DustPlugin>(
    mut stream: UnixStream,
    plugin: Arc<P>,
) -> std::io::Result<()> {
    loop {
        // Read 4-byte big-endian length prefix — mirrors dust-core framing.
        let mut len_buf = [0u8; 4];
        match stream.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf) as usize;

        let mut payload = vec![0u8; len];
        stream.read_exact(&mut payload).await?;

        let req: Request = match serde_json::from_slice(&payload) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("dust-sdk: parse error: {e}");
                continue;
            }
        };

        let resp = dispatch(&*plugin, req).await;
        write_response(&mut stream, &resp).await?;
    }
}

async fn write_response(stream: &mut UnixStream, resp: &Response) -> std::io::Result<()> {
    let payload = serde_json::to_vec(resp)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let len = payload
        .len()
        .try_into()
        .map(u32::to_be_bytes)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "response too large"))?;

    stream.write_all(&len).await?;
    stream.write_all(&payload).await?;
    stream.flush().await
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

async fn dispatch<P: DustPlugin>(plugin: &P, req: Request) -> Response {
    match req.method.as_str() {
        "manifest" => {
            let manifest = plugin.manifest().await;
            Response::ok(&req.id, &manifest)
        }
        "render" => {
            let components = plugin.render().await;
            Response::ok(&req.id, &components)
        }
        "action" => {
            let result = plugin.action(req.params).await;
            Response::ok(&req.id, &result)
        }
        method => Response::err(
            &req.id,
            -32601,
            format!("method not found: {method}"),
        ),
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use dust_core::Capability;

    struct TestPlugin;

    #[async_trait]
    impl DustPlugin for TestPlugin {
        fn plugin_id(&self) -> &str {
            "test-plugin"
        }

        async fn manifest(&self) -> PluginManifest {
            PluginManifest {
                name: "Test".into(),
                version: "0.1.0".into(),
                description: "A test plugin".into(),
                capabilities: vec![Capability::Widget { refresh_secs: 0 }],
                icon: None,
            }
        }

        async fn render(&self) -> Vec<Component> {
            vec![Component::Text {
                content: "hello".into(),
                style: TextStyle::default(),
            }]
        }

        async fn action(&self, _params: serde_json::Value) -> ActionResult {
            ActionResult::ok()
        }
    }

    #[tokio::test]
    async fn dispatch_manifest() {
        let plugin = TestPlugin;
        let req = Request {
            id: "1".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        };
        let resp = dispatch(&plugin, req).await;
        assert_eq!(resp.id, "1");
        assert!(resp.result.is_some());
        assert!(resp.error.is_none());
        assert_eq!(resp.result.unwrap()["name"], "Test");
    }

    #[tokio::test]
    async fn dispatch_render() {
        let plugin = TestPlugin;
        let req = Request {
            id: "2".into(),
            method: "render".into(),
            params: serde_json::Value::Null,
        };
        let resp = dispatch(&plugin, req).await;
        assert!(resp.result.is_some());
        let arr = resp.result.unwrap();
        assert!(arr.is_array());
        assert_eq!(arr.as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn dispatch_action() {
        let plugin = TestPlugin;
        let req = Request {
            id: "3".into(),
            method: "action".into(),
            params: serde_json::json!({"key": "value"}),
        };
        let resp = dispatch(&plugin, req).await;
        assert!(resp.result.is_some());
        assert!(resp.result.unwrap()["success"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn dispatch_unknown_method() {
        let plugin = TestPlugin;
        let req = Request {
            id: "4".into(),
            method: "unknown".into(),
            params: serde_json::Value::Null,
        };
        let resp = dispatch(&plugin, req).await;
        assert!(resp.error.is_some());
        assert_eq!(resp.error.unwrap().code, -32601);
    }

    #[tokio::test]
    async fn run_bind_and_roundtrip() {
        use std::time::Duration;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::UnixStream;

        let plugin = Arc::new(TestPlugin);
        let plugin_id = plugin.plugin_id().to_string();
        let socket_path = format!("/tmp/dust/{}.sock", plugin_id);

        // Clean up before test.
        let _ = tokio::fs::remove_file(&socket_path).await;

        // Spawn the server in the background.
        let server_plugin = Arc::clone(&plugin);
        tokio::spawn(async move {
            run(server_plugin).await.ok();
        });

        // Give the listener time to bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Connect and send a "manifest" request.
        let mut client = UnixStream::connect(&socket_path).await.unwrap();

        let req = Request {
            id: "rt-1".into(),
            method: "manifest".into(),
            params: serde_json::Value::Null,
        };
        let payload = serde_json::to_vec(&req).unwrap();
        let len = (payload.len() as u32).to_be_bytes();
        client.write_all(&len).await.unwrap();
        client.write_all(&payload).await.unwrap();
        client.flush().await.unwrap();

        // Read response.
        let mut len_buf = [0u8; 4];
        client.read_exact(&mut len_buf).await.unwrap();
        let resp_len = u32::from_be_bytes(len_buf) as usize;
        let mut resp_buf = vec![0u8; resp_len];
        client.read_exact(&mut resp_buf).await.unwrap();

        let resp: Response = serde_json::from_slice(&resp_buf).unwrap();
        assert_eq!(resp.id, "rt-1");
        assert!(resp.result.is_some());
    }
}
