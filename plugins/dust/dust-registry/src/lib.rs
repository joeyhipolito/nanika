//! dust-registry — host-side plugin registry for the Nanika dust dashboard.
//!
//! [`Registry`] manages the full lifecycle of installed dust plugins:
//! spawning processes, fetching manifests over IPC, routing render/action
//! calls, and hot-plugging new executables detected by a filesystem watcher.
//!
//! # Socket naming convention
//!
//! The registry derives each plugin's Unix socket path from its executable
//! filename stem. A binary named `~/.dust/plugins/dust-tracker` must call
//! `dust_sdk::run()` with a `plugin_id()` that returns `"dust-tracker"`, so
//! the socket lands at `/tmp/dust/dust-tracker.sock`. This 1-to-1 mapping
//! lets the registry connect to the socket without any prior protocol
//! exchange.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use dust_core::{ActionResult, Capability, Component, PluginManifest, Request, Response};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;

// ── Error ─────────────────────────────────────────────────────────────────────

/// Errors returned by [`Registry`] operations.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("plugin not found: {0}")]
    NotFound(String),

    /// The plugin process returned a structured error payload.
    #[error("plugin error {code}: {message}")]
    PluginError { code: i32, message: String },

    #[error("filesystem watch error: {0}")]
    Watch(String),
}

// ── PluginHandle ──────────────────────────────────────────────────────────────

/// A live plugin instance tracked by the registry.
///
/// Dropping this handle sends `SIGKILL` to the child process via
/// [`Child::start_kill`]. The process is not awaited on drop; the OS
/// reaps it when the registry exits.
pub struct PluginHandle {
    /// Plugin manifest returned by the plugin on startup.
    pub manifest: PluginManifest,
    /// Unix socket path (`/tmp/dust/<plugin-id>.sock`).
    pub socket_path: PathBuf,
    /// Absolute path to the plugin executable.
    pub executable_path: PathBuf,
    /// Spawned child process.
    child: Child,
}

impl Drop for PluginHandle {
    fn drop(&mut self) {
        // Best-effort SIGKILL; errors (already exited, etc.) are ignored.
        let _ = self.child.start_kill();
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Host-side registry of all live dust plugins.
///
/// The inner map is keyed by plugin-id (the executable filename stem).
/// Concurrent reads are cheap; writes (spawn / remove) take an exclusive lock.
pub struct Registry {
    plugins: Arc<RwLock<HashMap<String, PluginHandle>>>,
    /// Keeps the background watcher task alive for the lifetime of Registry.
    _watch_task: tokio::task::JoinHandle<()>,
}

impl Registry {
    /// Scan `~/.dust/plugins/`, spawn every executable found, and start the
    /// filesystem watcher that hot-plugs new executables and removes dead ones.
    pub async fn new() -> Result<Self, RegistryError> {
        let plugins_dir = plugins_dir()?;
        tokio::fs::create_dir_all(&plugins_dir).await?;

        let plugins: Arc<RwLock<HashMap<String, PluginHandle>>> =
            Arc::new(RwLock::new(HashMap::new()));

        // ── Initial scan ──────────────────────────────────────────────────────
        let mut dir = tokio::fs::read_dir(&plugins_dir).await?;
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if !is_executable_file(&path) {
                continue;
            }
            match spawn_plugin(&path).await {
                Ok(handle) => {
                    let id = plugin_id_from_path(&path);
                    plugins.write().await.insert(id, handle);
                }
                Err(e) => eprintln!("dust-registry: skipping {}: {e}", path.display()),
            }
        }

        // ── Filesystem watcher ────────────────────────────────────────────────
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<Event>>();

        let tx_notify = tx;
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                // The closure runs on the notify thread; send into the async channel.
                let _ = tx_notify.send(res);
            })
            .map_err(|e| RegistryError::Watch(e.to_string()))?;

        watcher
            .watch(&plugins_dir, RecursiveMode::NonRecursive)
            .map_err(|e| RegistryError::Watch(e.to_string()))?;

        let plugins_clone = Arc::clone(&plugins);
        let watch_task = tokio::spawn(async move {
            // Keep the watcher alive inside the task so it continues firing.
            let _watcher = watcher;
            while let Some(event_result) = rx.recv().await {
                match event_result {
                    Ok(event) => handle_fs_event(event, &plugins_clone).await,
                    Err(e) => eprintln!("dust-registry: watch error: {e}"),
                }
            }
        });

        Ok(Self {
            plugins,
            _watch_task: watch_task,
        })
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Like [`search`] but also returns the plugin-id alongside each manifest.
    ///
    /// The plugin-id is the executable filename stem used by [`Registry::render_ui`]
    /// and [`Registry::dispatch_action`].
    pub async fn search_with_ids(&self, query: &str) -> Vec<(String, PluginManifest)> {
        let guard = self.plugins.read().await;
        if query.trim().is_empty() {
            return guard
                .iter()
                .map(|(id, h)| (id.clone(), h.manifest.clone()))
                .collect();
        }

        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(f32, String, PluginManifest)> = guard
            .iter()
            .filter_map(|(id, h)| {
                let score = fuzzy_score(&h.manifest, &terms);
                (score > 0.0).then(|| (score, id.clone(), h.manifest.clone()))
            })
            .collect();

        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.2.name.cmp(&b.2.name))
        });

        scored.into_iter().map(|(_, id, m)| (id, m)).collect()
    }

    /// Fuzzy-match `query` against each plugin's name, description, and
    /// capability keywords. Returns matching manifests sorted by descending
    /// score. An empty query returns all registered plugins.
    pub async fn search(&self, query: &str) -> Vec<PluginManifest> {
        let guard = self.plugins.read().await;
        if query.trim().is_empty() {
            return guard.values().map(|h| h.manifest.clone()).collect();
        }

        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(f32, PluginManifest)> = guard
            .values()
            .filter_map(|h| {
                let score = fuzzy_score(&h.manifest, &terms);
                (score > 0.0).then(|| (score, h.manifest.clone()))
            })
            .collect();

        // Descending by score; tie-break by name for determinism.
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.name.cmp(&b.1.name))
        });
        scored.into_iter().map(|(_, m)| m).collect()
    }

    // ── IPC: render ───────────────────────────────────────────────────────────

    /// Ask `plugin_id` to render its current UI components.
    ///
    /// Sends `{ method: "render" }` and deserializes the returned component list.
    pub async fn render_ui(&self, plugin_id: &str) -> Result<Vec<Component>, RegistryError> {
        let socket_path = self.socket_path_for(plugin_id).await?;
        let resp = ipc_call(&socket_path, "render", serde_json::Value::Null).await?;
        check_response_error(&resp)?;
        let result = resp.result.unwrap_or(serde_json::Value::Array(vec![]));
        serde_json::from_value(result)
            .map_err(|e| RegistryError::Ipc(format!("parse render result: {e}")))
    }

    // ── IPC: action ───────────────────────────────────────────────────────────

    /// Dispatch an action to `plugin_id` with `params`.
    ///
    /// Sends `{ method: "action", params }` and deserializes the returned
    /// [`ActionResult`].
    pub async fn dispatch_action(
        &self,
        plugin_id: &str,
        params: serde_json::Value,
    ) -> Result<ActionResult, RegistryError> {
        let socket_path = self.socket_path_for(plugin_id).await?;
        let resp = ipc_call(&socket_path, "action", params).await?;
        check_response_error(&resp)?;
        let result = resp.result.unwrap_or(serde_json::Value::Null);
        serde_json::from_value(result)
            .map_err(|e| RegistryError::Ipc(format!("parse action result: {e}")))
    }

    // ── Sync ──────────────────────────────────────────────────────────────────

    /// Reconcile in-memory state with the filesystem.
    ///
    /// - Removes entries whose executable no longer exists on disk.
    /// - Spawns any executables present on disk that aren't registered yet.
    ///
    /// Called periodically by the dashboard so removals are caught even if
    /// the `notify` watcher misses or delays a Remove event.
    pub async fn sync(&self) -> Result<(), RegistryError> {
        let plugins_dir = plugins_dir()?;

        // Collect all current executables on disk.
        let mut on_disk: HashMap<String, PathBuf> = HashMap::new();
        if let Ok(mut dir) = tokio::fs::read_dir(&plugins_dir).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let path = entry.path();
                if is_executable_file(&path) {
                    let id = plugin_id_from_path(&path);
                    on_disk.insert(id, path);
                }
            }
        }

        // Remove plugins that have disappeared from disk.
        {
            let mut guard = self.plugins.write().await;
            let stale: Vec<String> = guard
                .keys()
                .filter(|id| !on_disk.contains_key(*id))
                .cloned()
                .collect();
            for id in stale {
                eprintln!("dust-registry: sync — removed {id}");
                guard.remove(&id);
            }
        }

        // Spawn plugins present on disk but not yet registered.
        for (id, path) in on_disk {
            if self.plugins.read().await.contains_key(&id) {
                continue;
            }
            match spawn_plugin(&path).await {
                Ok(handle) => {
                    let mut guard = self.plugins.write().await;
                    let name = handle.manifest.name.clone();
                    guard.entry(id).or_insert_with(|| {
                        eprintln!("dust-registry: sync — hot-plugged {name}");
                        handle
                    });
                }
                Err(e) => eprintln!("dust-registry: sync — spawn failed for {}: {e}", path.display()),
            }
        }

        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    async fn socket_path_for(&self, plugin_id: &str) -> Result<PathBuf, RegistryError> {
        self.plugins
            .read()
            .await
            .get(plugin_id)
            .map(|h| h.socket_path.clone())
            .ok_or_else(|| RegistryError::NotFound(plugin_id.into()))
    }
}

// ── Filesystem event handler ──────────────────────────────────────────────────

async fn handle_fs_event(event: Event, plugins: &Arc<RwLock<HashMap<String, PluginHandle>>>) {
    match event.kind {
        EventKind::Create(_) => {
            for path in &event.paths {
                if !is_executable_file(path) {
                    continue;
                }
                let id = plugin_id_from_path(path);
                // Fast read-path check — avoid spawning if already registered.
                if plugins.read().await.contains_key(&id) {
                    continue;
                }
                match spawn_plugin(path).await {
                    Ok(handle) => {
                        // Re-check under write lock to guard the TOCTOU window.
                        // `or_insert_with` only calls the closure (and moves `handle` in)
                        // when the key is absent; if already present the closure is skipped
                        // and `handle` is dropped, which SIGKILLs the duplicate.
                        let mut guard = plugins.write().await;
                        let name = handle.manifest.name.clone();
                        guard.entry(id).or_insert_with(|| {
                            eprintln!("dust-registry: hot-plugged {name}");
                            handle
                        });
                    }
                    Err(e) => {
                        eprintln!("dust-registry: hot-plug failed for {}: {e}", path.display())
                    }
                }
            }
        }
        EventKind::Remove(_) => {
            for path in &event.paths {
                let id = plugin_id_from_path(path);
                // Removing the handle from the map drops it, which triggers
                // SIGKILL on the child via PluginHandle::drop.
                if plugins.write().await.remove(&id).is_some() {
                    eprintln!("dust-registry: removed {id}");
                }
            }
        }
        _ => {}
    }
}

// ── Spawn helper ──────────────────────────────────────────────────────────────

/// Spawn a plugin executable, wait for it to bind its socket, and fetch the
/// manifest via IPC.
async fn spawn_plugin(exe_path: &Path) -> Result<PluginHandle, RegistryError> {
    let plugin_id = plugin_id_from_path(exe_path);
    let socket_path = PathBuf::from("/tmp/dust").join(format!("{plugin_id}.sock"));

    // Remove any stale socket left by a previous run.
    let _ = tokio::fs::remove_file(&socket_path).await;

    let child = Command::new(exe_path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false)
        .spawn()
        .map_err(RegistryError::Io)?;

    // Wait up to 5 s for the plugin to bind its socket.
    let sock_clone = socket_path.clone();
    tokio::time::timeout(Duration::from_secs(5), async move {
        loop {
            if tokio::fs::try_exists(&sock_clone).await.unwrap_or(false) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .map_err(|_| {
        RegistryError::Ipc(format!(
            "plugin {plugin_id} did not bind /tmp/dust/{plugin_id}.sock within 5 s"
        ))
    })?;

    // Fetch manifest.
    let resp = ipc_call(&socket_path, "manifest", serde_json::Value::Null).await?;
    check_response_error(&resp)?;
    let result = resp.result.ok_or_else(|| {
        RegistryError::Ipc(format!("plugin {plugin_id} returned empty manifest response"))
    })?;
    let manifest: PluginManifest = serde_json::from_value(result)
        .map_err(|e| RegistryError::Ipc(format!("parse manifest from {plugin_id}: {e}")))?;

    Ok(PluginHandle {
        manifest,
        socket_path,
        executable_path: exe_path.to_owned(),
        child,
    })
}

// ── IPC helpers ───────────────────────────────────────────────────────────────

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

/// Connect to a plugin's Unix socket, send a length-prefixed JSON request,
/// and return the decoded response.
///
/// Uses the same 4-byte big-endian length-prefix framing as `dust-core` and
/// `dust-sdk`.
async fn ipc_call(
    socket_path: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<Response, RegistryError> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| RegistryError::Ipc(format!("connect to {}: {e}", socket_path.display())))?;

    let req = Request {
        id: next_id(),
        method: method.into(),
        params,
    };

    let payload = serde_json::to_vec(&req)
        .map_err(|e| RegistryError::Ipc(format!("serialize request: {e}")))?;

    let len = u32::try_from(payload.len())
        .map(u32::to_be_bytes)
        .map_err(|_| RegistryError::Ipc("request payload too large".into()))?;

    stream
        .write_all(&len)
        .await
        .map_err(|e| RegistryError::Ipc(format!("write length prefix: {e}")))?;
    stream
        .write_all(&payload)
        .await
        .map_err(|e| RegistryError::Ipc(format!("write payload: {e}")))?;
    stream
        .flush()
        .await
        .map_err(|e| RegistryError::Ipc(format!("flush: {e}")))?;

    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .map_err(|e| RegistryError::Ipc(format!("read response length: {e}")))?;

    let resp_len = u32::from_be_bytes(len_buf) as usize;
    let mut resp_buf = vec![0u8; resp_len];
    stream
        .read_exact(&mut resp_buf)
        .await
        .map_err(|e| RegistryError::Ipc(format!("read response body: {e}")))?;

    serde_json::from_slice::<Response>(&resp_buf)
        .map_err(|e| RegistryError::Ipc(format!("parse response: {e}")))
}

fn check_response_error(resp: &Response) -> Result<(), RegistryError> {
    if let Some(err) = &resp.error {
        return Err(RegistryError::PluginError {
            code: err.code,
            message: err.message.clone(),
        });
    }
    Ok(())
}

// ── Fuzzy search ──────────────────────────────────────────────────────────────

/// Score `manifest` against a pre-lowercased list of query terms.
///
/// The corpus is the plugin's name, description, and capability keywords
/// (`"widget"`, `"scheduler"`, or a command prefix). Score is the fraction
/// of terms that appear as substrings in the corpus (0.0 – 1.0).
fn fuzzy_score(manifest: &PluginManifest, terms: &[&str]) -> f32 {
    if terms.is_empty() {
        return 1.0;
    }
    let mut corpus = format!(
        "{} {}",
        manifest.name.to_lowercase(),
        manifest.description.to_lowercase()
    );
    for cap in &manifest.capabilities {
        match cap {
            Capability::Command { prefix } => {
                corpus.push(' ');
                corpus.push_str(&prefix.to_lowercase());
                corpus.push_str(" command");
            }
            Capability::Widget { .. } => corpus.push_str(" widget"),
            Capability::Scheduler => corpus.push_str(" scheduler"),
        }
    }
    let matched = terms.iter().filter(|t| corpus.contains(**t)).count();
    matched as f32 / terms.len() as f32
}

// ── Path helpers ──────────────────────────────────────────────────────────────

fn plugins_dir() -> Result<PathBuf, RegistryError> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join(".dust").join("plugins"))
        .ok_or_else(|| {
            RegistryError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "HOME environment variable not set",
            ))
        })
}

fn plugin_id_from_path(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    if !path.is_file() {
        return false;
    }
    std::fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file() && path.extension().map(|e| e == "exe").unwrap_or(false)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use dust_core::Capability;

    fn manifest(name: &str, desc: &str, caps: Vec<Capability>) -> PluginManifest {
        PluginManifest {
            name: name.into(),
            version: "0.1.0".into(),
            description: desc.into(),
            capabilities: caps,
            icon: None,
        }
    }

    // ── fuzzy_score ───────────────────────────────────────────────────────────

    #[test]
    fn score_matches_name() {
        let m = manifest("dust-tracker", "issue tracker", vec![]);
        assert!(fuzzy_score(&m, &["tracker"]) > 0.0);
    }

    #[test]
    fn score_matches_description() {
        let m = manifest("dust-foo", "A budgeting tool", vec![]);
        assert!(fuzzy_score(&m, &["budgeting"]) > 0.0);
    }

    #[test]
    fn score_matches_command_prefix() {
        let m = manifest(
            "dust-cmd",
            "Command plugin",
            vec![Capability::Command {
                prefix: "track".into(),
            }],
        );
        assert!(fuzzy_score(&m, &["track"]) > 0.0);
    }

    #[test]
    fn score_matches_widget_keyword() {
        let m = manifest("dust-w", "A widget", vec![Capability::Widget { refresh_secs: 30 }]);
        assert!(fuzzy_score(&m, &["widget"]) > 0.0);
    }

    #[test]
    fn score_matches_scheduler_keyword() {
        let m = manifest("dust-sched", "Runs jobs", vec![Capability::Scheduler]);
        assert!(fuzzy_score(&m, &["scheduler"]) > 0.0);
    }

    #[test]
    fn score_no_match_returns_zero() {
        let m = manifest("dust-foo", "A demo", vec![]);
        assert_eq!(fuzzy_score(&m, &["xyzxyz"]), 0.0);
    }

    #[test]
    fn score_partial_match() {
        let m = manifest("dust-tracker", "Issue tracker", vec![]);
        // "tracker" matches, "xyzxyz" does not → 0.5
        let score = fuzzy_score(&m, &["tracker", "xyzxyz"]);
        assert!((score - 0.5).abs() < f32::EPSILON);
    }

    #[test]
    fn score_empty_terms_returns_full() {
        let m = manifest("dust-foo", "desc", vec![]);
        assert_eq!(fuzzy_score(&m, &[]), 1.0);
    }

    #[test]
    fn score_case_insensitive() {
        let m = manifest("Dust-Tracker", "Issue TRACKER", vec![]);
        assert!(fuzzy_score(&m, &["tracker"]) > 0.0);
    }

    // ── plugin_id_from_path ────────────────────────────────────────────────────

    #[test]
    fn id_from_path_extracts_stem() {
        assert_eq!(
            plugin_id_from_path(Path::new("/home/user/.dust/plugins/dust-tracker")),
            "dust-tracker"
        );
        assert_eq!(
            plugin_id_from_path(Path::new("/foo/bar/my-plugin")),
            "my-plugin"
        );
    }
}
