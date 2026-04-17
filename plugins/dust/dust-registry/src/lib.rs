//! dust-registry — host-side plugin registry for the Nanika dust dashboard.
//!
//! [`Registry`] manages the full lifecycle of installed dust plugins:
//! spawning processes, performing the §13 handshake, routing render/action
//! calls, heartbeating, and gracefully shutting down plugins.
//!
//! # Plugin discovery
//!
//! The registry watches `~/nanika/plugins/` recursively for `plugin.json`
//! files.  Events are debounced by 200 ms (HOTPLUG-02).  Each `plugin.json`
//! must contain a `dust` block (`DustManifestBlock`) and a `name` field
//! conforming to the plugin ID grammar (`^[a-z][a-z0-9_-]{1,63}$`,
//! TRANSPORT-15).
//!
//! # Runtime directory & socket naming
//!
//! Sockets live at `$XDG_RUNTIME_DIR/nanika/plugins/<id>.sock` when
//! `XDG_RUNTIME_DIR` is set and non-empty, or at
//! `~/.alluka/run/plugins/<id>.sock` otherwise (TRANSPORT-01 / TRANSPORT-02).
//! The runtime directory is created with mode `0700` (TRANSPORT-04).  Stale
//! sockets from a previous registry run are removed on startup (TRANSPORT-13).

pub mod observability;

use std::collections::{HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::observability::ObservabilityWriter;

use dust_core::envelope::{
    ActionParams, Envelope, EventEnvelope, EventType, HeartbeatEnvelope, RequestEnvelope,
    ResponseEnvelope, ShutdownEnvelope, ShutdownReason,
};
use dust_core::events::EventRing;
use dust_core::state::{DeadReason, PluginState};
use dust_core::{ActionResult, Capability, Component, DustManifestBlock, PluginManifest};
use notify::{Event, EventKind, RecursiveMode, Watcher};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;

// ── Constants ─────────────────────────────────────────────────────────────────

/// Registry software version — sent to plugins in `host_info`.
const REGISTRY_VERSION: &str = "0.1.0";
/// Minimum protocol version the registry accepts.
const PROTOCOL_VERSION_MIN: &str = "1.0.0";
/// Maximum protocol version the registry accepts.
const PROTOCOL_VERSION_MAX: &str = "1.999.999";
/// Number of consecutive missed heartbeats before a plugin is declared dead
/// (HEARTBEAT-02).
const HEARTBEAT_MISS_THRESHOLD: u32 = 3;
/// Maximum time to wait for the plugin's `ready` event during handshake (§5.2).
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Binary existence poll interval while a plugin is active (HOTPLUG-07).
const BINARY_POLL_INTERVAL: Duration = Duration::from_secs(5);
/// Maximum concurrent subscriber connections per plugin (PRESSURE-01).
const MAX_SUBSCRIBERS: usize = 16;
/// Maximum in-flight IPC requests per plugin per direction (§12 PRESSURE-02).
///
/// Applies to `render`, `action`, and `refresh_manifest` calls combined.
/// Returns -33005 Busy when the limit is reached.
const MAX_IN_FLIGHT_REQUESTS: usize = 100;
/// Maximum lifecycle (registry) connections per plugin (§12 PRESSURE-03).
///
/// The registry maintains exactly one primary connection per plugin handle.
/// A collision (second spawn attempt) is returned as error code -33004.
/// Enforced by collision detection in `spawn_plugin`.
#[allow(dead_code)]
const MAX_REGISTRY_CONNECTIONS_PER_PLUGIN: usize = 1;
/// Capacity of the per-plugin event broadcast channel.
const EVENT_BROADCAST_CAPACITY: usize = 2_048;

// ── ConnectionId ──────────────────────────────────────────────────────────────

/// Opaque identifier for a subscriber connection.
///
/// Used to enforce one-subscription-per-connection (REPLAY-08) and the
/// 16-connection limit (PRESSURE-01).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnectionId(pub u64);

// ── SubscriberMap ─────────────────────────────────────────────────────────────

struct ConnectionEntry {
    /// `Some(subscription_id)` when this connection has an active subscription.
    subscription_id: Option<String>,
}

/// Tracks open subscriber connections and their subscription state for a single
/// plugin.
pub struct SubscriberMap {
    connections: HashMap<ConnectionId, ConnectionEntry>,
}

impl SubscriberMap {
    fn new() -> Self {
        Self {
            connections: HashMap::new(),
        }
    }

    /// Register a new subscriber connection.
    ///
    /// Returns `-33005 Busy` if the 16-connection limit is already reached
    /// (PRESSURE-01).
    pub fn open_connection(&mut self, id: ConnectionId) -> Result<(), i32> {
        if self.connections.len() >= MAX_SUBSCRIBERS {
            return Err(-33005);
        }
        self.connections.insert(id, ConnectionEntry { subscription_id: None });
        Ok(())
    }

    /// Remove a connection and release its subscription, if any.
    pub fn close_connection(&mut self, id: ConnectionId) {
        self.connections.remove(&id);
    }

    /// Mark `id` as having an active subscription and return the subscription ID.
    ///
    /// Returns `-33005 Busy` when the connection already has a subscription
    /// (REPLAY-08), or `-32602 InvalidParams` when `id` is not registered.
    fn set_subscribed(&mut self, id: ConnectionId) -> Result<String, i32> {
        let entry = self.connections.get_mut(&id).ok_or(-32602i32)?;
        if entry.subscription_id.is_some() {
            return Err(-33005);
        }
        let sub_id = new_subscription_id();
        entry.subscription_id = Some(sub_id.clone());
        Ok(sub_id)
    }

    /// Remove the active subscription on `id`.
    ///
    /// Returns `-32602 InvalidParams` when `subscription_id` does not match,
    /// or when `id` is not registered.
    fn clear_subscription(&mut self, id: ConnectionId, subscription_id: &str) -> Result<(), i32> {
        let entry = self.connections.get_mut(&id).ok_or(-32602i32)?;
        if entry.subscription_id.as_deref() != Some(subscription_id) {
            return Err(-32602);
        }
        entry.subscription_id = None;
        Ok(())
    }

    /// Whether `id` is registered (connection is open).
    pub fn is_connected(&self, id: ConnectionId) -> bool {
        self.connections.contains_key(&id)
    }

    /// Whether `id` has an active subscription.
    pub fn is_subscribed(&self, id: ConnectionId) -> bool {
        self.connections
            .get(&id)
            .map_or(false, |e| e.subscription_id.is_some())
    }
}

// ── SubscribeResult ───────────────────────────────────────────────────────────

/// Successful result of [`handle_subscribe_request`].
pub struct SubscribeResult {
    /// Handle for `events.unsubscribe`; not used for event routing (REPLAY-04).
    pub subscription_id: String,
    /// All retained events with `sequence >= since_sequence` (REPLAY-04).
    pub events: Vec<EventEnvelope>,
    /// Sequence number the next live event will carry (REPLAY-04).
    pub next_sequence: u64,
    /// Live push channel — receive new events as the plugin emits them.
    pub live_rx: tokio::sync::broadcast::Receiver<EventEnvelope>,
}

impl std::fmt::Debug for SubscribeResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscribeResult")
            .field("subscription_id", &self.subscription_id)
            .field("events_len", &self.events.len())
            .field("next_sequence", &self.next_sequence)
            .field("live_rx", &"<broadcast::Receiver>")
            .finish()
    }
}

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

    /// `plugin.json` could not be read or its `dust` block was invalid.
    #[error("manifest parse error: {0}")]
    ManifestParse(String),

    /// The `name` field in `plugin.json` does not match the plugin ID grammar.
    #[error("invalid plugin ID {id:?}: {reason}")]
    InvalidPluginId { id: String, reason: String },

    /// Spawn refused because an active handle already exists for the same ID
    /// (HOTPLUG-09 / HOTPLUG-10).
    ///
    /// Maps to error code **-33004** (§12 PRESSURE-03: registry connections per
    /// plugin = 1).  The registry enforces exactly one primary lifecycle
    /// connection per plugin; a second spawn attempt is rejected with this error.
    #[error("collision: plugin {0} is already active")]
    Collision(String),
}

impl RegistryError {
    /// Numeric error code for protocol-level error responses.
    ///
    /// | Variant | Code |
    /// |---------|------|
    /// | `Collision` | -33004 (registry connections per plugin limit) |
    /// | `PluginError { code, .. }` | code |
    /// | others | -32603 (internal error) |
    pub fn code(&self) -> i32 {
        match self {
            Self::Collision(_) => -33004,
            Self::PluginError { code, .. } => *code,
            _ => -32603,
        }
    }
}

// ── PluginHandle ──────────────────────────────────────────────────────────────

/// A live plugin instance tracked by the registry.
///
/// The handle owns the background connection task.  Dropping the handle:
/// 1. Aborts the background task.
/// 2. Sends `HostExit` through the shutdown channel (best-effort).
/// 3. Issues a best-effort `SIGKILL` to the child process.
pub struct PluginHandle {
    /// Plugin manifest received in the `ready` event.
    pub manifest: PluginManifest,
    /// Lifecycle state, updated by the background task (§5).
    ///
    /// Starts at [`PluginState::Spawned`] and progresses to `Active` during
    /// `spawn_plugin`. The background task advances and terminates at `Dead`.
    pub state: Arc<std::sync::Mutex<PluginState>>,
    /// Unix socket path for this plugin.
    pub socket_path: PathBuf,
    /// Resolved absolute path to the plugin executable.
    pub executable_path: PathBuf,
    /// Absolute path to the `plugin.json` that spawned this handle.
    pub manifest_path: PathBuf,
    /// The parsed `dust` block from `plugin.json` (heartbeat interval, drain
    /// timeout, etc.).
    pub dust: DustManifestBlock,
    /// Shared child process handle.  The background task uses this for SIGKILL
    /// after drain.
    child: Arc<tokio::sync::Mutex<Child>>,
    /// Channel to request graceful shutdown from outside the background task.
    shutdown_tx: Option<tokio::sync::oneshot::Sender<ShutdownReason>>,
    /// Background lifecycle task.  Stored so it lives as long as the handle.
    _conn_task: Option<tokio::task::JoinHandle<()>>,
    /// Per-plugin event ring for replay (REPLAY-15).
    pub event_ring: Arc<std::sync::Mutex<EventRing>>,
    /// Broadcast sender — used to push live events to active subscribers.
    pub event_tx: tokio::sync::broadcast::Sender<EventEnvelope>,
    /// Tracks open subscriber connections and their subscription state.
    pub subscribers: Arc<std::sync::Mutex<SubscriberMap>>,
    /// Count of in-flight IPC requests (render / action / refresh_manifest).
    ///
    /// Incremented before each call, decremented on completion.  When this
    /// reaches `MAX_IN_FLIGHT_REQUESTS` the next caller gets -33005 Busy
    /// (§12 PRESSURE-02).
    pub in_flight: Arc<AtomicU64>,
}

impl Drop for PluginHandle {
    fn drop(&mut self) {
        // Cancel the background task so it does not linger.
        if let Some(t) = self._conn_task.take() {
            t.abort();
        }
        // Signal graceful shutdown (best-effort; fails if task already exited).
        let _ = self.shutdown_tx.take().map(|tx| tx.send(ShutdownReason::HostExit));
        // Best-effort SIGKILL — only succeeds if the task is not currently
        // holding the lock.
        if let Ok(mut guard) = self.child.try_lock() {
            let _ = guard.start_kill();
        }
    }
}

// ── Registry ──────────────────────────────────────────────────────────────────

/// Debounce map: canonical manifest path → pending task handle.
type DebounceMap = std::sync::Mutex<HashMap<PathBuf, tokio::task::JoinHandle<()>>>;

/// Host-side registry of all live dust plugins.
///
/// The inner map is keyed by plugin-id (from the `name` field in
/// `plugin.json`). Concurrent reads are cheap; writes (spawn / remove) take
/// an exclusive lock.
pub struct Registry {
    plugins: Arc<RwLock<HashMap<String, PluginHandle>>>,
    runtime_dir: PathBuf,
    /// Shared observability writer — receives all non-heartbeat plugin frames.
    pub obs: Arc<ObservabilityWriter>,
    /// Keeps the background watcher task alive for the lifetime of Registry.
    _watch_task: tokio::task::JoinHandle<()>,
}

impl Registry {
    /// Scan `~/nanika/plugins/*/plugin.json`, spawn every plugin found, and
    /// start the filesystem watcher that hot-plugs new plugins and removes
    /// dead ones.
    pub async fn new() -> Result<Self, RegistryError> {
        let plugins_dir = plugins_dir()?;
        let runtime_dir = runtime_dir()?;

        // Ensure both directories exist with correct permissions.
        tokio::fs::create_dir_all(&plugins_dir).await?;
        ensure_runtime_dir(&runtime_dir).await?;

        // ── TRANSPORT-13: stale socket cleanup ────────────────────────────────
        //
        // Enumerate known plugin IDs from on-disk manifests, then remove any
        // `.sock` file whose ID is NOT in that set.
        let known_ids = collect_plugin_ids(&plugins_dir).await;
        if let Err(e) = cleanup_stale_sockets(&runtime_dir, &known_ids).await {
            eprintln!("dust-registry: stale socket cleanup error: {e}");
        }

        // Shared observability writer for all plugins.
        let obs = Arc::new(ObservabilityWriter::new());

        // ── Initial scan ──────────────────────────────────────────────────────
        let mut plugins_initial: HashMap<String, PluginHandle> = HashMap::new();
        if let Ok(mut dir) = tokio::fs::read_dir(&plugins_dir).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let manifest_path = entry.path().join("plugin.json");
                if !tokio::fs::try_exists(&manifest_path).await.unwrap_or(false) {
                    continue;
                }
                let existing_ids: HashSet<String> = plugins_initial.keys().cloned().collect();
                match spawn_plugin(&manifest_path, &runtime_dir, &existing_ids, Arc::clone(&obs)).await {
                    Ok(handle) => {
                        let id = handle.manifest.name.clone();
                        plugins_initial.insert(id, handle);
                    }
                    Err(e) => eprintln!(
                        "dust-registry: skipping {}: {e}",
                        manifest_path.display()
                    ),
                }
            }
        }

        let plugins: Arc<RwLock<HashMap<String, PluginHandle>>> =
            Arc::new(RwLock::new(plugins_initial));

        // ── Filesystem watcher ────────────────────────────────────────────────
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<notify::Result<Event>>();
        let tx_notify = tx;
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| {
                let _ = tx_notify.send(res);
            })
            .map_err(|e| RegistryError::Watch(e.to_string()))?;

        watcher
            .watch(&plugins_dir, RecursiveMode::Recursive)
            .map_err(|e| RegistryError::Watch(e.to_string()))?;

        let plugins_clone = Arc::clone(&plugins);
        let runtime_dir_clone = runtime_dir.clone();
        let obs_watch = Arc::clone(&obs);
        let debounce: Arc<DebounceMap> =
            Arc::new(std::sync::Mutex::new(HashMap::new()));

        let watch_task = tokio::spawn(async move {
            let _watcher = watcher;
            while let Some(event_result) = rx.recv().await {
                match event_result {
                    Ok(event) => {
                        for path in &event.paths {
                            if path.file_name().and_then(|n| n.to_str()) != Some("plugin.json") {
                                continue;
                            }
                            schedule_manifest_event(
                                path.clone(),
                                event.kind,
                                Arc::clone(&plugins_clone),
                                Arc::clone(&debounce),
                                runtime_dir_clone.clone(),
                                Arc::clone(&obs_watch),
                            );
                        }
                    }
                    Err(e) => eprintln!("dust-registry: watch error: {e}"),
                }
            }
        });

        Ok(Self {
            plugins,
            runtime_dir,
            obs,
            _watch_task: watch_task,
        })
    }

    // ── Search ────────────────────────────────────────────────────────────────

    /// Like [`search`] but also returns the plugin-id alongside each manifest.
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
    /// score.  An empty query returns all registered plugins.
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
    /// Opens a **subscriber** connection (per §6.2) and calls `render`.
    /// Returns -33005 Busy when 100 requests are already in flight (§12).
    pub async fn render_ui(&self, plugin_id: &str) -> Result<Vec<Component>, RegistryError> {
        let (socket_path, in_flight_arc) = self.socket_and_in_flight(plugin_id).await?;
        let _guard = acquire_in_flight(&in_flight_arc)?;
        let resp = ipc_call(&socket_path, "render", serde_json::Value::Null).await?;
        check_response_error(&resp)?;
        let result = resp.result.unwrap_or(serde_json::Value::Array(vec![]));
        serde_json::from_value(result)
            .map_err(|e| RegistryError::Ipc(format!("parse render result: {e}")))
    }

    // ── IPC: action ───────────────────────────────────────────────────────────

    /// Dispatch an action to `plugin_id` with typed `params`.
    ///
    /// Opens a **subscriber** connection (per §6.2) and calls `action`.
    /// Returns -33005 Busy when 100 requests are already in flight (§12).
    pub async fn dispatch_action(
        &self,
        plugin_id: &str,
        params: ActionParams,
    ) -> Result<ActionResult, RegistryError> {
        let (socket_path, in_flight_arc) = self.socket_and_in_flight(plugin_id).await?;
        let _guard = acquire_in_flight(&in_flight_arc)?;
        let params_value = serde_json::to_value(&params)
            .map_err(|e| RegistryError::Ipc(format!("serialize params: {e}")))?;
        let resp = ipc_call(&socket_path, "action", params_value).await?;
        check_response_error(&resp)?;
        let result = resp.result.unwrap_or(serde_json::Value::Null);
        serde_json::from_value(result)
            .map_err(|e| RegistryError::Ipc(format!("parse action result: {e}")))
    }

    // ── IPC: refresh_manifest ─────────────────────────────────────────────────

    /// Ask `plugin_id` to re-read its manifest and return the updated version.
    ///
    /// Called by the registry when `plugin.json` is modified in-place.  On
    /// success the cached manifest in the handle is updated atomically.
    /// Returns -33005 Busy when 100 requests are already in flight (§12).
    pub async fn refresh_manifest(
        &self,
        plugin_id: &str,
    ) -> Result<PluginManifest, RegistryError> {
        let (socket_path, in_flight_arc) = self.socket_and_in_flight(plugin_id).await?;
        let _guard = acquire_in_flight(&in_flight_arc)?;
        let resp =
            ipc_call(&socket_path, "refresh_manifest", serde_json::Value::Null).await?;
        check_response_error(&resp)?;
        let result = resp.result.ok_or_else(|| {
            RegistryError::Ipc("empty result from refresh_manifest".into())
        })?;
        let new_manifest: PluginManifest = serde_json::from_value(result)
            .map_err(|e| {
                RegistryError::Ipc(format!("parse manifest from refresh_manifest: {e}"))
            })?;

        // Swap the cached manifest in the handle.
        {
            let mut guard = self.plugins.write().await;
            if let Some(h) = guard.get_mut(plugin_id) {
                h.manifest = new_manifest.clone();
            }
        }

        Ok(new_manifest)
    }

    // ── Sync ──────────────────────────────────────────────────────────────────

    /// Reconcile in-memory state with the filesystem.
    ///
    /// - Removes entries that are `Dead` or whose `plugin.json` no longer
    ///   exists on disk.
    /// - Spawns any plugins present on disk that aren't registered yet.
    ///
    /// Called periodically by the dashboard so removals are caught even if
    /// the `notify` watcher misses or delays a `Remove` event.
    pub async fn sync(&self) -> Result<(), RegistryError> {
        let plugins_dir = plugins_dir()?;

        // Collect all current plugin.json files on disk, keyed by plugin ID.
        let mut on_disk: HashMap<String, PathBuf> = HashMap::new();
        if let Ok(mut dir) = tokio::fs::read_dir(&plugins_dir).await {
            while let Ok(Some(entry)) = dir.next_entry().await {
                let manifest_path = entry.path().join("plugin.json");
                if !tokio::fs::try_exists(&manifest_path).await.unwrap_or(false) {
                    continue;
                }
                match parse_plugin_json(&manifest_path).await {
                    Ok((id, _dust)) => {
                        on_disk.insert(id, manifest_path);
                    }
                    Err(e) => eprintln!(
                        "dust-registry: sync — parse failed for {}: {e}",
                        manifest_path.display()
                    ),
                }
            }
        }

        // Remove dead or missing plugins.
        {
            let mut guard = self.plugins.write().await;
            let stale: Vec<String> = guard
                .iter()
                .filter(|(id, h)| {
                    !on_disk.contains_key(*id)
                        || *h.state.lock().unwrap() == PluginState::Dead
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale {
                eprintln!("dust-registry: sync — removed {id}");
                guard.remove(&id);
            }
        }

        // Spawn plugins present on disk but not yet registered.
        for (id, manifest_path) in on_disk {
            let is_active = self
                .plugins
                .read()
                .await
                .get(&id)
                .map(|h| *h.state.lock().unwrap() != PluginState::Dead)
                .unwrap_or(false);
            if is_active {
                continue;
            }
            let existing_ids: HashSet<String> = self
                .plugins
                .read()
                .await
                .iter()
                .filter(|(_, h)| *h.state.lock().unwrap() != PluginState::Dead)
                .map(|(id, _)| id.clone())
                .collect();
            match spawn_plugin(&manifest_path, &self.runtime_dir, &existing_ids, Arc::clone(&self.obs)).await {
                Ok(handle) => {
                    let mut guard = self.plugins.write().await;
                    let name = handle.manifest.name.clone();
                    guard.entry(id).or_insert_with(|| {
                        eprintln!("dust-registry: sync — hot-plugged {name}");
                        handle
                    });
                }
                Err(e) => eprintln!(
                    "dust-registry: sync — spawn failed for {}: {e}",
                    manifest_path.display()
                ),
            }
        }

        Ok(())
    }

    // ── IPC: events.subscribe / events.unsubscribe ────────────────────────────

    /// Open a new subscriber connection slot for `plugin_id`.
    ///
    /// Must be called before [`Registry::subscribe_plugin_events`].  Returns
    /// `-33005 Busy` when the 16-connection limit is reached (PRESSURE-01).
    pub async fn open_subscriber_connection(
        &self,
        plugin_id: &str,
        conn_id: ConnectionId,
    ) -> Result<(), RegistryError> {
        let guard = self.plugins.read().await;
        let handle = guard
            .get(plugin_id)
            .ok_or_else(|| RegistryError::NotFound(plugin_id.into()))?;
        // Bind to a local so the MutexGuard drops before `guard` does.
        let result = handle.subscribers.lock().unwrap().open_connection(conn_id);
        result.map_err(|code| RegistryError::PluginError {
            code,
            message: "busy: subscriber connection limit reached".into(),
        })
    }

    /// Close a subscriber connection slot and release any active subscription.
    pub async fn close_subscriber_connection(
        &self,
        plugin_id: &str,
        conn_id: ConnectionId,
    ) -> Result<(), RegistryError> {
        let guard = self.plugins.read().await;
        let handle = guard
            .get(plugin_id)
            .ok_or_else(|| RegistryError::NotFound(plugin_id.into()))?;
        handle.subscribers.lock().unwrap().close_connection(conn_id);
        Ok(())
    }

    /// Subscribe to events for `plugin_id` via subscriber connection `conn_id`.
    ///
    /// Returns a snapshot of all retained events with
    /// `sequence >= since_sequence` plus a live broadcast receiver for new
    /// events as they arrive from the plugin.  The caller must have previously
    /// opened the connection with [`Registry::open_subscriber_connection`].
    pub async fn subscribe_plugin_events(
        &self,
        plugin_id: &str,
        conn_id: ConnectionId,
        since_sequence: u64,
    ) -> Result<SubscribeResult, RegistryError> {
        let guard = self.plugins.read().await;
        let handle = guard
            .get(plugin_id)
            .ok_or_else(|| RegistryError::NotFound(plugin_id.into()))?;
        handle_subscribe_request(
            &handle.event_ring,
            &handle.event_tx,
            &handle.subscribers,
            conn_id,
            since_sequence,
        )
    }

    /// End the live push subscription identified by `subscription_id` on
    /// connection `conn_id`.
    pub async fn unsubscribe_plugin_events(
        &self,
        plugin_id: &str,
        conn_id: ConnectionId,
        subscription_id: &str,
    ) -> Result<(), RegistryError> {
        let guard = self.plugins.read().await;
        let handle = guard
            .get(plugin_id)
            .ok_or_else(|| RegistryError::NotFound(plugin_id.into()))?;
        handle_unsubscribe_request(&handle.subscribers, conn_id, subscription_id)
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// Return the socket path and in-flight counter for an active plugin.
    ///
    /// Used by IPC methods to acquire the in-flight guard before calling
    /// `ipc_call`.  Both values are cloned out of the read lock so callers
    /// do not hold the lock across I/O.
    async fn socket_and_in_flight(
        &self,
        plugin_id: &str,
    ) -> Result<(PathBuf, Arc<AtomicU64>), RegistryError> {
        let guard = self.plugins.read().await;
        let h = guard
            .get(plugin_id)
            .ok_or_else(|| RegistryError::NotFound(plugin_id.into()))?;
        Ok((h.socket_path.clone(), Arc::clone(&h.in_flight)))
    }
}

// ── Debounce scheduler ────────────────────────────────────────────────────────

/// Schedule processing of a manifest filesystem event, debounced by 200 ms.
fn schedule_manifest_event(
    path: PathBuf,
    kind: EventKind,
    plugins: Arc<RwLock<HashMap<String, PluginHandle>>>,
    debounce: Arc<DebounceMap>,
    runtime_dir: PathBuf,
    obs: Arc<ObservabilityWriter>,
) {
    let mut map = debounce.lock().unwrap();
    if let Some(h) = map.remove(&path) {
        h.abort();
    }
    let path2 = path.clone();
    let debounce2 = Arc::clone(&debounce);
    let handle = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        debounce2.lock().unwrap().remove(&path2);
        handle_manifest_event(kind, &path2, &plugins, &runtime_dir, &obs).await;
    });
    map.insert(path, handle);
}

// ── Filesystem event handler ──────────────────────────────────────────────────

async fn handle_manifest_event(
    kind: EventKind,
    path: &Path,
    plugins: &Arc<RwLock<HashMap<String, PluginHandle>>>,
    runtime_dir: &Path,
    obs: &Arc<ObservabilityWriter>,
) {
    match kind {
        EventKind::Create(_) | EventKind::Modify(_) => {
            let manifest_path = path;
            let (id, new_dust) = match parse_plugin_json(manifest_path).await {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "dust-registry: manifest_parse_failure for {}: {e}",
                        manifest_path.display()
                    );
                    return;
                }
            };

            // Determine if an active handle exists and get its socket path.
            let (is_active, socket_path_opt) = {
                let guard = plugins.read().await;
                if let Some(h) = guard.get(&id) {
                    let active = *h.state.lock().unwrap() == PluginState::Active;
                    (active, Some(h.socket_path.clone()))
                } else {
                    (false, None)
                }
            };

            if is_active {
                // Plugin is Active and plugin.json changed — call refresh_manifest
                // so the plugin can re-read its own manifest, then swap the cache.
                if matches!(kind, EventKind::Modify(_)) {
                    if let Some(socket_path) = socket_path_opt {
                        match ipc_call(
                            &socket_path,
                            "refresh_manifest",
                            serde_json::Value::Null,
                        )
                        .await
                        {
                            Ok(resp) => {
                                let parse_ok = check_response_error(&resp).is_ok();
                                if parse_ok {
                                    if let Some(result) = resp.result {
                                        match serde_json::from_value::<PluginManifest>(result) {
                                            Ok(new_manifest) => {
                                                let mut guard = plugins.write().await;
                                                if let Some(h) = guard.get_mut(&id) {
                                                    h.manifest = new_manifest;
                                                    h.dust = new_dust;
                                                    eprintln!(
                                                        "dust-registry: refreshed manifest for {id}"
                                                    );
                                                }
                                            }
                                            Err(e) => eprintln!(
                                                "dust-registry: refresh_manifest parse error \
                                                 for {id}: {e}"
                                            ),
                                        }
                                    }
                                }
                            }
                            Err(e) => eprintln!(
                                "dust-registry: refresh_manifest failed for {id}: {e}"
                            ),
                        }
                    }
                }
                return;
            }

            // Plugin is not active — spawn it.
            // Collect non-dead plugin IDs for collision detection.
            let existing_ids: HashSet<String> = plugins
                .read()
                .await
                .iter()
                .filter(|(_, h)| *h.state.lock().unwrap() != PluginState::Dead)
                .map(|(id, _)| id.clone())
                .collect();

            match spawn_plugin(manifest_path, runtime_dir, &existing_ids, Arc::clone(obs)).await {
                Ok(handle) => {
                    let mut guard = plugins.write().await;
                    let name = handle.manifest.name.clone();
                    guard.entry(id).or_insert_with(|| {
                        eprintln!("dust-registry: hot-plugged {name}");
                        handle
                    });
                }
                Err(e) => eprintln!(
                    "dust-registry: hot-plug failed for {}: {e}",
                    manifest_path.display()
                ),
            }
        }
        EventKind::Remove(_) => {
            let id = match path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|n| n.to_str())
            {
                Some(s) => s.to_owned(),
                None => {
                    eprintln!(
                        "dust-registry: cannot derive plugin ID from {}",
                        path.display()
                    );
                    return;
                }
            };
            if plugins.write().await.remove(&id).is_some() {
                eprintln!("dust-registry: removed {id}");
            }
        }
        _ => {}
    }
}

// ── Spawn + full lifecycle ────────────────────────────────────────────────────

/// Spawn a plugin binary, perform the §13 handshake, and start the background
/// task that manages heartbeat, binary polling, and graceful shutdown.
///
/// Returns a [`PluginHandle`] in the `Active` state.  All pre-active errors
/// return [`RegistryError`] and the child is killed as part of cleanup.
async fn spawn_plugin(
    manifest_path: &Path,
    runtime_dir: &Path,
    existing_ids: &HashSet<String>,
    obs: Arc<ObservabilityWriter>,
) -> Result<PluginHandle, RegistryError> {
    let (plugin_id, dust) = parse_plugin_json(manifest_path).await?;

    // HOTPLUG-09: refuse to spawn a second active instance.
    if existing_ids.contains(&plugin_id) {
        return Err(RegistryError::Collision(plugin_id));
    }

    let plugin_dir = manifest_path.parent().ok_or_else(|| {
        RegistryError::ManifestParse(format!(
            "manifest path has no parent: {}",
            manifest_path.display()
        ))
    })?;

    // HOTPLUG-06: validate binary path.
    if dust.binary.contains("..") {
        return Err(RegistryError::ManifestParse(format!(
            "dust.binary {:?} must not contain '..' segments",
            dust.binary
        )));
    }
    let exe_path = plugin_dir.join(&dust.binary);
    if !exe_path.starts_with(plugin_dir) {
        return Err(RegistryError::ManifestParse(format!(
            "dust.binary resolves outside the plugin directory: {}",
            exe_path.display()
        )));
    }

    let socket_path = runtime_dir.join(format!("{plugin_id}.sock"));

    // HOTPLUG-10: probe existing socket for a live peer.
    if tokio::fs::try_exists(&socket_path).await.unwrap_or(false) {
        match UnixStream::connect(&socket_path).await {
            Ok(_stream) => {
                // Another live instance is already bound.
                return Err(RegistryError::Collision(plugin_id));
            }
            Err(_) => {
                // Stale socket — safe to remove.
                let _ = tokio::fs::remove_file(&socket_path).await;
            }
        }
    }

    // ── Spawn ──────────────────────────────────────────────────────────────────
    let mut cmd = Command::new(&exe_path);
    if let Some(ref args) = dust.args {
        cmd.args(args);
    }
    let child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false)
        .spawn()
        .map_err(RegistryError::Io)?;

    let child_arc = Arc::new(tokio::sync::Mutex::new(child));
    let state = Arc::new(std::sync::Mutex::new(PluginState::Spawned));

    // ── Wait for socket (Spawned state) ───────────────────────────────────────
    let timeout_ms = dust.spawn_timeout_ms;
    let sock_clone = socket_path.clone();
    let wait_result = tokio::time::timeout(
        Duration::from_millis(u64::from(timeout_ms)),
        async move {
            loop {
                if tokio::fs::try_exists(&sock_clone).await.unwrap_or(false) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        },
    )
    .await;

    if wait_result.is_err() {
        let _ = child_arc.lock().await.start_kill();
        return Err(RegistryError::Ipc(format!(
            "plugin {plugin_id} did not bind socket within {timeout_ms} ms"
        )));
    }

    // ── Connect (Spawned → Connected) ─────────────────────────────────────────
    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(e) => {
            let _ = child_arc.lock().await.start_kill();
            return Err(RegistryError::Ipc(format!(
                "connect to {plugin_id}: {e}"
            )));
        }
    };

    // Best-effort: set socket file permissions to 0600 (TRANSPORT-05).
    let _ = tokio::fs::set_permissions(
        &socket_path,
        std::fs::Permissions::from_mode(0o600),
    )
    .await;

    *state.lock().unwrap() = PluginState::Connected;

    let (mut read_half, mut write_half) = stream.into_split();

    // ── HandshakeWait (Connected → HandshakeWait) ─────────────────────────────
    *state.lock().unwrap() = PluginState::HandshakeWait;

    // Wait up to HANDSHAKE_TIMEOUT for the plugin's `ready` event.
    let ready_result =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, read_ready_event(&mut read_half)).await;

    let (manifest, protocol_version) = match ready_result {
        Ok(Ok(r)) => r,
        Ok(Err(e)) => {
            let _ = child_arc.lock().await.start_kill();
            return Err(RegistryError::Ipc(format!(
                "handshake failed for {plugin_id}: {e}"
            )));
        }
        Err(_elapsed) => {
            let _ = child_arc.lock().await.start_kill();
            return Err(RegistryError::Ipc(format!(
                "handshake timeout for {plugin_id} (waited {}s)",
                HANDSHAKE_TIMEOUT.as_secs()
            )));
        }
    };

    // ── Validate protocol version (VERSION-03) ────────────────────────────────
    if !is_version_supported(&protocol_version) {
        let env = Envelope::Shutdown(ShutdownEnvelope {
            reason: ShutdownReason::VersionMismatch,
        });
        let _ = write_envelope_to_half(&mut write_half, &env).await;
        let _ = child_arc.lock().await.start_kill();
        return Err(RegistryError::Ipc(format!(
            "plugin {plugin_id} version {protocol_version} outside supported range \
             [{PROTOCOL_VERSION_MIN}, {PROTOCOL_VERSION_MAX}]"
        )));
    }

    // ── Send host_info (HANDSHAKE-05) — HandshakeWait → Active ───────────────
    if let Err(e) = send_host_info(&mut write_half, 1).await {
        let _ = child_arc.lock().await.start_kill();
        return Err(e);
    }
    *state.lock().unwrap() = PluginState::Active;

    // ── Start background lifecycle task ───────────────────────────────────────
    let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel::<ShutdownReason>();
    let state_clone = Arc::clone(&state);
    let child_clone = Arc::clone(&child_arc);
    let exe_clone = exe_path.clone();
    let dust_clone = dust.clone();
    let id_clone = plugin_id.clone();

    // Extract the widget refresh interval from the manifest capabilities.
    // Use the first Widget capability with refresh_secs > 0.
    let widget_refresh_secs = manifest.capabilities.iter().find_map(|cap| {
        if let Capability::Widget { refresh_secs } = cap {
            if *refresh_secs > 0 { Some(*refresh_secs) } else { None }
        } else {
            None
        }
    });

    // Per-plugin event ring + broadcast channel for subscriber live push.
    let event_ring = Arc::new(std::sync::Mutex::new(EventRing::new()));
    // The initial receiver is dropped immediately; live receivers are created
    // on each events.subscribe call via event_tx.subscribe().
    let (event_tx, _init_rx) =
        tokio::sync::broadcast::channel::<EventEnvelope>(EVENT_BROADCAST_CAPACITY);
    let subscribers = Arc::new(std::sync::Mutex::new(SubscriberMap::new()));

    let conn_task = tokio::spawn(plugin_active_task(
        id_clone,
        read_half,
        write_half,
        dust_clone,
        exe_clone,
        state_clone,
        shutdown_rx,
        child_clone,
        widget_refresh_secs,
        Arc::clone(&event_ring),
        event_tx.clone(),
        obs,
    ));

    Ok(PluginHandle {
        manifest,
        state,
        socket_path,
        executable_path: exe_path,
        manifest_path: manifest_path.to_owned(),
        dust,
        child: child_arc,
        shutdown_tx: Some(shutdown_tx),
        _conn_task: Some(conn_task),
        event_ring,
        event_tx,
        subscribers,
        in_flight: Arc::new(AtomicU64::new(0)),
    })
}

// ── Background lifecycle task ─────────────────────────────────────────────────

/// Background task for a plugin that has completed the handshake and is in the
/// `Active` state.
///
/// Responsibilities:
/// - Send a heartbeat every `heartbeat_interval_ms` (HEARTBEAT-01).
/// - Track received heartbeats; declare dead after `HEARTBEAT_MISS_THRESHOLD`
///   consecutive misses (HEARTBEAT-02).
/// - Poll the binary path every [`BINARY_POLL_INTERVAL`] seconds; send
///   `binary_deleted` shutdown if the binary disappears (HOTPLUG-07/08).
/// - Accept and execute a graceful shutdown request: send shutdown envelope,
///   transition to `Draining`, wait `shutdown_drain_ms`, then SIGKILL
///   (SHUTDOWN-07).
async fn plugin_active_task(
    plugin_id: String,
    mut reader: tokio::net::unix::OwnedReadHalf,
    write_half: tokio::net::unix::OwnedWriteHalf,
    dust: DustManifestBlock,
    exe_path: PathBuf,
    state: Arc<std::sync::Mutex<PluginState>>,
    mut shutdown_rx: tokio::sync::oneshot::Receiver<ShutdownReason>,
    child: Arc<tokio::sync::Mutex<Child>>,
    // When the plugin declares a `Widget` capability with `refresh_secs > 0`,
    // the registry emits a `refresh` event at this interval (§Widget-refresh).
    widget_refresh_secs: Option<u32>,
    event_ring: Arc<std::sync::Mutex<EventRing>>,
    event_tx: tokio::sync::broadcast::Sender<EventEnvelope>,
    obs: Arc<ObservabilityWriter>,
) {
    let write_half = Arc::new(tokio::sync::Mutex::new(write_half));
    let heartbeat_interval = Duration::from_millis(u64::from(dust.heartbeat_interval_ms));
    let drain_timeout = Duration::from_millis(u64::from(dust.shutdown_drain_ms));
    let miss_deadline = heartbeat_interval * HEARTBEAT_MISS_THRESHOLD;

    // Ticker: fires every heartbeat_interval — used to send heartbeats AND
    // check the miss window.
    let mut heartbeat_tick = tokio::time::interval(heartbeat_interval);
    heartbeat_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Ticker: fires every BINARY_POLL_INTERVAL seconds.
    let mut binary_poll_tick = tokio::time::interval(BINARY_POLL_INTERVAL);
    binary_poll_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Widget refresh ticker (optional): fires every refresh_secs seconds.
    // Uses interval_at so the first tick is delayed by one full period.
    let mut widget_refresh_tick = widget_refresh_secs.filter(|&s| s > 0).map(|secs| {
        let dur = Duration::from_secs(u64::from(secs));
        let mut t =
            tokio::time::interval_at(tokio::time::Instant::now() + dur, dur);
        t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        t
    });

    // Record when we last received a heartbeat from the plugin.
    let mut last_heartbeat_at = tokio::time::Instant::now();

    let dead_reason: DeadReason = loop {
        tokio::select! {
            biased;

            // ── Shutdown request (highest priority) ───────────────────────
            result = &mut shutdown_rx => {
                let reason = result.unwrap_or(ShutdownReason::HostExit);
                // Send shutdown envelope.
                {
                    let env = Envelope::Shutdown(ShutdownEnvelope { reason });
                    let mut w = write_half.lock().await;
                    let _ = write_envelope_to_half(&mut *w, &env).await;
                }
                // Transition to Draining.
                if let Ok(mut s) = state.lock() {
                    *s = PluginState::Draining;
                }
                // Drain: wait for plugin to close or for the drain deadline.
                let drain_result = tokio::time::timeout(drain_timeout, async {
                    loop {
                        match read_envelope_half(&mut reader).await {
                            Ok(None) | Err(_) => {
                                return DeadReason::ProcessExitedDuringDrain;
                            }
                            Ok(Some(_)) => {}
                        }
                    }
                })
                .await;
                break match drain_result {
                    Ok(r) => r,
                    Err(_elapsed) => DeadReason::DrainTimeout,
                };
            }

            // ── Inbound frame from plugin ──────────────────────────────────
            result = read_envelope_half(&mut reader) => {
                match result {
                    Ok(Some(Envelope::Heartbeat(_))) => {
                        // Heartbeats MUST NOT be logged (ENVELOPE-09).
                        last_heartbeat_at = tokio::time::Instant::now();
                    }
                    // Push plugin-originated events into the ring, broadcast to
                    // subscribers (REPLAY-15/REPLAY-06), and log to observability.
                    Ok(Some(Envelope::Event(e))) => {
                        obs.send(&plugin_id, Envelope::Event(e.clone()), dust.log_redact.clone());
                        event_ring.lock().unwrap().push(e.clone());
                        // Ignore SendError — fires when there are no active receivers.
                        let _ = event_tx.send(e);
                    }
                    // Other non-heartbeat frames are forwarded to observability.
                    Ok(Some(other)) => {
                        obs.send(&plugin_id, other, dust.log_redact.clone());
                    }
                    // Clean disconnect or I/O error → process exited.
                    Ok(None) | Err(_) => {
                        break DeadReason::ProcessExited;
                    }
                }
            }

            // ── Send heartbeat + check miss count ──────────────────────────
            _ = heartbeat_tick.tick() => {
                // Send our heartbeat to the plugin.
                {
                    let env = Envelope::Heartbeat(HeartbeatEnvelope { ts: utc_now() });
                    let mut w = write_half.lock().await;
                    let _ = write_envelope_to_half(&mut *w, &env).await;
                }
                // Check if the plugin has missed too many heartbeats.
                if last_heartbeat_at.elapsed() > miss_deadline {
                    eprintln!(
                        "dust-registry: heartbeat_timeout for {plugin_id} \
                         (no heartbeat for {}ms)",
                        last_heartbeat_at.elapsed().as_millis()
                    );
                    break DeadReason::HeartbeatTimeout;
                }
            }

            // ── Binary existence polling (HOTPLUG-07) ─────────────────────
            _ = binary_poll_tick.tick() => {
                if !binary_exists(&exe_path).await {
                    eprintln!("dust-registry: binary deleted for {plugin_id}");
                    // Send shutdown with BinaryDeleted reason.
                    {
                        let env = Envelope::Shutdown(ShutdownEnvelope {
                            reason: ShutdownReason::BinaryDeleted,
                        });
                        let mut w = write_half.lock().await;
                        let _ = write_envelope_to_half(&mut *w, &env).await;
                    }
                    // Transition to Draining.
                    if let Ok(mut s) = state.lock() {
                        *s = PluginState::Draining;
                    }
                    // Drain with timeout.
                    let drain_result = tokio::time::timeout(drain_timeout, async {
                        loop {
                            match read_envelope_half(&mut reader).await {
                                Ok(None) | Err(_) => {
                                    return DeadReason::ProcessExitedDuringDrain;
                                }
                                Ok(Some(_)) => {}
                            }
                        }
                    })
                    .await;
                    break match drain_result {
                        Ok(r) => r,
                        Err(_) => DeadReason::DrainTimeout,
                    };
                }
            }

            // ── Widget refresh timer (§Widget-refresh) ─────────────────────
            // Fires every `widget_refresh_secs` seconds when the plugin
            // declares a Widget capability with a positive refresh interval.
            // Uses `std::future::pending()` to make this branch never fire
            // when no widget refresh is configured.
            _ = async {
                if let Some(t) = &mut widget_refresh_tick {
                    let _ = t.tick().await;
                } else {
                    std::future::pending::<()>().await;
                }
            } => {
                let env = Envelope::Event(EventEnvelope {
                    id: new_event_id(),
                    event_type: EventType::Refresh,
                    ts: utc_now(),
                    sequence: None,
                    data: serde_json::json!({}),
                });
                let mut w = write_half.lock().await;
                let _ = write_envelope_to_half(&mut *w, &env).await;
            }
        }
    };

    // ── Transition to Dead, SIGKILL ───────────────────────────────────────────
    if let Ok(mut s) = state.lock() {
        *s = PluginState::Dead;
    }
    let _ = child.lock().await.start_kill();
    eprintln!("dust-registry: plugin {plugin_id} dead (reason: {dead_reason})");
}

// ── Handshake helpers ─────────────────────────────────────────────────────────

/// Read the plugin's `ready` event from the handshake read half.
///
/// Returns `(manifest, protocol_version)` on success.  Any envelope other
/// than a `ready` event is classified as `premature_traffic` and returned as
/// an error.
async fn read_ready_event(
    reader: &mut tokio::net::unix::OwnedReadHalf,
) -> Result<(PluginManifest, String), RegistryError> {
    let envelope = read_envelope_half(reader)
        .await?
        .ok_or_else(|| {
            RegistryError::Ipc("connection closed before ready event".into())
        })?;

    match envelope {
        Envelope::Event(e) if e.event_type == EventType::Ready => {
            let protocol_version = e
                .data
                .get("protocol_version")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    RegistryError::Ipc(
                        "ready event missing `protocol_version` field".into(),
                    )
                })?
                .to_owned();

            let manifest_value = e
                .data
                .get("manifest")
                .cloned()
                .unwrap_or(serde_json::Value::Null);

            let manifest: PluginManifest = serde_json::from_value(manifest_value)
                .map_err(|err| {
                    RegistryError::Ipc(format!("parse manifest from ready event: {err}"))
                })?;

            Ok((manifest, protocol_version))
        }
        _ => Err(RegistryError::Ipc(
            "expected ready event (premature_traffic)".into(),
        )),
    }
}

/// Send the `host_info` event to the plugin (HANDSHAKE-05).
async fn send_host_info(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    consumer_count: u32,
) -> Result<(), RegistryError> {
    let env = Envelope::Event(EventEnvelope {
        id: new_event_id(),
        event_type: EventType::HostInfo,
        ts: utc_now(),
        sequence: None, // REPLAY-12: registry events carry no sequence
        data: serde_json::json!({
            "host_name": "dust",
            "host_version": REGISTRY_VERSION,
            "protocol_version_supported": {
                "min": PROTOCOL_VERSION_MIN,
                "max": PROTOCOL_VERSION_MAX
            },
            "consumer_count": consumer_count
        }),
    });
    write_envelope_to_half(writer, &env).await
}

// ── Async framing helpers ─────────────────────────────────────────────────────

/// Read one framed envelope from an owned read half.
///
/// Returns `Ok(None)` on clean EOF.  Returns `Err` on I/O errors or
/// deserialization failures.
async fn read_envelope_half(
    reader: &mut tokio::net::unix::OwnedReadHalf,
) -> Result<Option<Envelope>, RegistryError> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(RegistryError::Io(e)),
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 {
        return Ok(None); // FRAME-04
    }
    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .await
        .map_err(RegistryError::Io)?;
    let envelope: Envelope = serde_json::from_slice(&buf)
        .map_err(|e| RegistryError::Ipc(format!("parse envelope: {e}")))?;
    Ok(Some(envelope))
}

/// Write one framed envelope to an owned write half.
async fn write_envelope_to_half(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    envelope: &Envelope,
) -> Result<(), RegistryError> {
    let payload = serde_json::to_vec(envelope)
        .map_err(|e| RegistryError::Ipc(format!("serialize envelope: {e}")))?;
    let len = (payload.len() as u32).to_be_bytes();
    writer.write_all(&len).await.map_err(RegistryError::Io)?;
    writer
        .write_all(&payload)
        .await
        .map_err(RegistryError::Io)?;
    writer.flush().await.map_err(RegistryError::Io)?;
    Ok(())
}

// ── Manifest parsing ──────────────────────────────────────────────────────────

/// Read and parse a `plugin.json` file, returning the validated plugin ID and
/// the parsed `dust` block.
async fn parse_plugin_json(
    manifest_path: &Path,
) -> Result<(String, DustManifestBlock), RegistryError> {
    let content = tokio::fs::read_to_string(manifest_path).await.map_err(|e| {
        RegistryError::ManifestParse(format!("read {}: {e}", manifest_path.display()))
    })?;

    let v: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        RegistryError::ManifestParse(format!(
            "invalid JSON in {}: {e}",
            manifest_path.display()
        ))
    })?;

    let name = v["name"]
        .as_str()
        .ok_or_else(|| {
            RegistryError::ManifestParse(format!(
                "missing `name` field in {}",
                manifest_path.display()
            ))
        })?
        .to_owned();

    validate_plugin_id(&name)?;

    let dust_value = v
        .get("dust")
        .ok_or_else(|| {
            RegistryError::ManifestParse(format!(
                "missing `dust` block in {}",
                manifest_path.display()
            ))
        })?
        .clone();

    let dust: DustManifestBlock = serde_json::from_value(dust_value).map_err(|e| {
        RegistryError::ManifestParse(format!(
            "invalid `dust` block in {}: {e}",
            manifest_path.display()
        ))
    })?;

    Ok((name, dust))
}

// ── Plugin ID validation ──────────────────────────────────────────────────────

/// Validate a plugin ID against `^[a-z][a-z0-9_-]{1,63}$` (TRANSPORT-15).
fn validate_plugin_id(id: &str) -> Result<(), RegistryError> {
    let bytes = id.as_bytes();

    if bytes.len() < 2 || bytes.len() > 64 {
        return Err(RegistryError::InvalidPluginId {
            id: id.to_owned(),
            reason: format!(
                "length {} is outside the allowed range [2, 64]",
                bytes.len()
            ),
        });
    }

    if !bytes[0].is_ascii_lowercase() {
        return Err(RegistryError::InvalidPluginId {
            id: id.to_owned(),
            reason: "first character must be a lowercase ASCII letter [a-z]".into(),
        });
    }

    for &b in &bytes[1..] {
        if !b.is_ascii_lowercase() && !b.is_ascii_digit() && b != b'-' && b != b'_' {
            return Err(RegistryError::InvalidPluginId {
                id: id.to_owned(),
                reason: format!(
                    "character {:?} is not allowed; only [a-z0-9_-] permitted after the \
                     first character",
                    b as char
                ),
            });
        }
    }

    Ok(())
}

// ── Stale socket cleanup (TRANSPORT-13) ───────────────────────────────────────

/// Remove any `.sock` files in `runtime_dir` whose stem (the plugin ID part)
/// is NOT in `known_ids`.
///
/// Sockets whose ID IS in `known_ids` are left alone — they may belong to a
/// live instance.
pub(crate) async fn cleanup_stale_sockets(
    runtime_dir: &Path,
    known_ids: &[String],
) -> Result<(), RegistryError> {
    let known: HashSet<&str> = known_ids.iter().map(|s| s.as_str()).collect();

    let mut dir = match tokio::fs::read_dir(runtime_dir).await {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(RegistryError::Io(e)),
    };

    while let Ok(Some(entry)) = dir.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("sock") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_owned();
        if !known.contains(stem.as_str()) {
            eprintln!("dust-registry: removing stale socket {}", path.display());
            let _ = tokio::fs::remove_file(&path).await;
        }
    }

    Ok(())
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// Returns the runtime directory for plugin socket files.
///
/// Uses `$XDG_RUNTIME_DIR/nanika/plugins/` when the variable is set and
/// non-empty (TRANSPORT-01), otherwise falls back to
/// `~/.alluka/run/plugins/` (TRANSPORT-02).
fn runtime_dir() -> Result<PathBuf, RegistryError> {
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("nanika").join("plugins"));
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| {
            RegistryError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "neither XDG_RUNTIME_DIR nor HOME is set",
            ))
        })?;
    Ok(home.join(".alluka").join("run").join("plugins"))
}

/// Create `dir` (and all parents) with mode `0700` (TRANSPORT-04).
async fn ensure_runtime_dir(dir: &Path) -> Result<(), RegistryError> {
    tokio::fs::create_dir_all(dir).await?;
    tokio::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(RegistryError::Io)
}

/// Returns `~/nanika/plugins/` — the root watched for plugin manifests
/// (HOTPLUG-01).
fn plugins_dir() -> Result<PathBuf, RegistryError> {
    std::env::var_os("HOME")
        .map(|h| PathBuf::from(h).join("nanika").join("plugins"))
        .ok_or_else(|| {
            RegistryError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "HOME environment variable not set",
            ))
        })
}

/// Scan `plugins_dir` and return the IDs of all parseable `plugin.json` files.
async fn collect_plugin_ids(plugins_dir: &Path) -> Vec<String> {
    let mut ids = Vec::new();
    if let Ok(mut dir) = tokio::fs::read_dir(plugins_dir).await {
        while let Ok(Some(entry)) = dir.next_entry().await {
            let mp = entry.path().join("plugin.json");
            if tokio::fs::try_exists(&mp).await.unwrap_or(false) {
                if let Ok((id, _)) = parse_plugin_json(&mp).await {
                    ids.push(id);
                }
            }
        }
    }
    ids
}

// ── Protocol helpers ──────────────────────────────────────────────────────────

/// Accept any protocol version whose major component is `1` (range
/// `1.0.0`–`1.999.999`).
fn is_version_supported(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    if parts.len() != 3 {
        return false;
    }
    parts[0] == "1"
        && parts[1].parse::<u64>().is_ok()
        && parts[2].parse::<u64>().is_ok()
}

/// Check whether the binary at `path` exists and has at least one executable
/// bit set.
async fn binary_exists(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(m) => m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

// ── Timestamp helpers ─────────────────────────────────────────────────────────

/// Return the current UTC time as an ISO 8601 string with millisecond
/// precision (e.g. `"2026-04-12T09:30:00.123Z"`).
fn utc_now() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    epoch_ms_to_iso8601(dur.as_secs(), dur.subsec_millis() as u64)
}

fn epoch_ms_to_iso8601(epoch_secs: u64, ms: u64) -> String {
    let (y, mo, d) = civil_from_days((epoch_secs / 86400) as i64);
    let tod = epoch_secs % 86400;
    let h = tod / 3600;
    let mi = (tod % 3600) / 60;
    let s = tod % 60;
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{ms:03}Z")
}

/// Howard Hinnant's `civil_from_days` algorithm: convert days-since-epoch to
/// `(year, month, day)`.
fn civil_from_days(z: i64) -> (i64, u64, u64) {
    let z = z + 719_468;
    let era: i64 = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

// ── Event ID generator ────────────────────────────────────────────────────────

static NEXT_EVENT_ID: AtomicU64 = AtomicU64::new(1);

fn new_event_id() -> String {
    let n = NEXT_EVENT_ID.fetch_add(1, Ordering::Relaxed);
    format!("evt_{n:016x}")
}

// ── IPC helpers (subscriber connections for render / action) ──────────────────

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> String {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed).to_string()
}

/// Open a new (subscriber) connection to a plugin socket, send a request, and
/// return the response.  Used by [`Registry::render_ui`] and
/// [`Registry::dispatch_action`].
async fn ipc_call(
    socket_path: &Path,
    method: &str,
    params: serde_json::Value,
) -> Result<ResponseEnvelope, RegistryError> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .map_err(|e| RegistryError::Ipc(format!("connect to {}: {e}", socket_path.display())))?;

    let envelope = Envelope::Request(RequestEnvelope {
        id: next_id(),
        method: method.into(),
        params,
    });

    let payload = serde_json::to_vec(&envelope)
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

    let envelope: Envelope = serde_json::from_slice(&resp_buf)
        .map_err(|e| RegistryError::Ipc(format!("parse response: {e}")))?;

    match envelope {
        Envelope::Response(r) => Ok(r),
        _ => Err(RegistryError::Ipc(
            "expected response envelope from plugin".into(),
        )),
    }
}

fn check_response_error(resp: &ResponseEnvelope) -> Result<(), RegistryError> {
    if let Some(err) = &resp.error {
        return Err(RegistryError::PluginError {
            code: err.code,
            message: err.message.clone(),
        });
    }
    Ok(())
}

// ── In-flight request guard (§12 PRESSURE-02) ─────────────────────────────────

/// Increment the in-flight counter and return a guard that decrements it on
/// drop.
///
/// Returns `-33005 Busy` when the limit is already reached.
fn acquire_in_flight(counter: &Arc<AtomicU64>) -> Result<InFlightGuard, RegistryError> {
    // fetch_add is optimistic — increment first, then check.
    let prev = counter.fetch_add(1, Ordering::AcqRel);
    if prev >= MAX_IN_FLIGHT_REQUESTS as u64 {
        // Immediately undo the increment — we're not actually going to call.
        counter.fetch_sub(1, Ordering::Relaxed);
        return Err(RegistryError::PluginError {
            code: -33005,
            message: "busy: in-flight request limit reached".into(),
        });
    }
    Ok(InFlightGuard { counter: Arc::clone(counter) })
}

/// RAII guard that decrements the in-flight counter when dropped.
struct InFlightGuard {
    counter: Arc<AtomicU64>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::Relaxed);
    }
}

// ── Subscription ID generator ─────────────────────────────────────────────────

static NEXT_SUBSCRIPTION_ID: AtomicU64 = AtomicU64::new(1);

fn new_subscription_id() -> String {
    let n = NEXT_SUBSCRIPTION_ID.fetch_add(1, Ordering::Relaxed);
    format!("sub_{n:016x}")
}

// ── events.subscribe / events.unsubscribe handlers ────────────────────────────

/// Handle an `events.subscribe` request from a subscriber connection.
///
/// # Protocol
///
/// 1. Creates a live-push [`tokio::sync::broadcast::Receiver`] **before**
///    locking the ring, so no events are missed between snapshot and live.
/// 2. Reads the retained snapshot from the ring.
/// 3. Marks `conn_id` as subscribed in the [`SubscriberMap`].
///
/// # Errors
///
/// | Error | Code |
/// |---|---|
/// | `conn_id` not registered | `-32602 InvalidParams` |
/// | `conn_id` already has an active subscription | `-33005 Busy` (REPLAY-08) |
/// | `since_sequence` older than oldest retained event | `-33007 ReplayGap` (REPLAY-05) |
pub fn handle_subscribe_request(
    ring: &std::sync::Mutex<EventRing>,
    event_tx: &tokio::sync::broadcast::Sender<EventEnvelope>,
    subs: &std::sync::Mutex<SubscriberMap>,
    conn_id: ConnectionId,
    since_sequence: u64,
) -> Result<SubscribeResult, RegistryError> {
    // Create the live receiver FIRST so no events are lost between snapshot
    // read and broadcast subscription (snapshot + live have possible overlap;
    // subscribers deduplicate by sequence).
    let live_rx = event_tx.subscribe();

    // Read snapshot from ring.
    let (events, next_seq) = {
        let ring_guard = ring.lock().unwrap();
        let events = ring_guard
            .subscribe(since_sequence)
            .map_err(|gap| RegistryError::PluginError {
                code: -33007,
                message: format!(
                    "replay_gap: oldest_available={}, requested={}",
                    gap.oldest_available, gap.requested
                ),
            })?;
        let next_seq = ring_guard.next_sequence();
        (events, next_seq)
    };

    // Register subscription on the connection.
    let subscription_id = subs
        .lock()
        .unwrap()
        .set_subscribed(conn_id)
        .map_err(|code| RegistryError::PluginError {
            code,
            message: if code == -33005 {
                "busy: connection already has an active subscription".into()
            } else {
                "invalid_params: connection not registered".into()
            },
        })?;

    Ok(SubscribeResult {
        subscription_id,
        events,
        next_sequence: next_seq,
        live_rx,
    })
}

/// Handle an `events.unsubscribe` request.
///
/// # Errors
///
/// Returns `-32602 InvalidParams` when `conn_id` is not registered or
/// `subscription_id` does not match the active subscription.
pub fn handle_unsubscribe_request(
    subs: &std::sync::Mutex<SubscriberMap>,
    conn_id: ConnectionId,
    subscription_id: &str,
) -> Result<(), RegistryError> {
    subs.lock()
        .unwrap()
        .clear_subscription(conn_id, subscription_id)
        .map_err(|code| RegistryError::PluginError {
            code,
            message: "invalid_params: subscription_id not found".into(),
        })
}

// ── Fuzzy search ──────────────────────────────────────────────────────────────

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use dust_core::{Capability, RestartPolicy};

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn manifest(name: &str, desc: &str, caps: Vec<Capability>) -> PluginManifest {
        PluginManifest {
            name: name.into(),
            version: "0.1.0".into(),
            description: desc.into(),
            capabilities: caps,
            icon: None,
        }
    }

    /// Construct a `DustManifestBlock` with test-friendly short intervals.
    fn test_dust(heartbeat_ms: u32, drain_ms: u32) -> DustManifestBlock {
        DustManifestBlock {
            binary: "bin/plugin".into(),
            protocol_version: "1.0.0".into(),
            capabilities: vec![],
            restart: RestartPolicy::OnFailure,
            heartbeat_interval_ms: heartbeat_ms,
            shutdown_drain_ms: drain_ms,
            spawn_timeout_ms: 5_000,
            log_redact: vec![],
            args: None,
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

    // ── validate_plugin_id ────────────────────────────────────────────────────

    #[test]
    fn valid_plugin_ids() {
        for id in &[
            "ab",
            "dust-tracker",
            "my_plugin",
            "a1",
            "hello-world-123",
            &("a".repeat(1) + &"b".repeat(63)),
        ] {
            assert!(
                validate_plugin_id(id).is_ok(),
                "expected {id:?} to be valid"
            );
        }
    }

    #[test]
    fn invalid_plugin_ids() {
        assert!(validate_plugin_id("a").is_err());
        assert!(validate_plugin_id(&"a".repeat(65)).is_err());
        assert!(validate_plugin_id("1plugin").is_err());
        assert!(validate_plugin_id("Plugin").is_err());
        assert!(validate_plugin_id("my-Plugin").is_err());
        assert!(validate_plugin_id("my plugin").is_err());
        assert!(validate_plugin_id("my.plugin").is_err());
        assert!(validate_plugin_id("").is_err());
    }

    // ── is_version_supported ──────────────────────────────────────────────────

    #[test]
    fn version_supported_major_one() {
        assert!(is_version_supported("1.0.0"));
        assert!(is_version_supported("1.2.3"));
        assert!(is_version_supported("1.999.999"));
    }

    #[test]
    fn version_not_supported_wrong_major() {
        assert!(!is_version_supported("0.9.0"));
        assert!(!is_version_supported("2.0.0"));
        assert!(!is_version_supported("10.0.0"));
    }

    #[test]
    fn version_not_supported_malformed() {
        assert!(!is_version_supported("1.0"));
        assert!(!is_version_supported("1"));
        assert!(!is_version_supported(""));
        assert!(!is_version_supported("abc"));
        assert!(!is_version_supported("1.x.0"));
    }

    // ── utc_now / epoch_ms_to_iso8601 ─────────────────────────────────────────

    #[test]
    fn epoch_zero_is_unix_epoch() {
        assert_eq!(epoch_ms_to_iso8601(0, 0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn utc_now_is_iso8601_shaped() {
        let ts = utc_now();
        // Must match YYYY-MM-DDTHH:MM:SS.mmmZ (24 chars)
        assert_eq!(ts.len(), 24, "unexpected timestamp format: {ts}");
        assert!(ts.ends_with('Z'), "must end with Z: {ts}");
        assert!(ts.contains('T'), "must contain T: {ts}");
    }

    // ── State machine (delegated to dust_core::state) ─────────────────────────
    //
    // The exhaustive state machine tests live in dust-core/src/state.rs.
    // These tests verify the integration surface used by the registry.

    #[test]
    fn state_machine_valid_transitions() {
        use dust_core::state::PluginState;
        let pairs = [
            (PluginState::Spawned, PluginState::Connected),
            (PluginState::Spawned, PluginState::Dead),
            (PluginState::Connected, PluginState::HandshakeWait),
            (PluginState::Connected, PluginState::Dead),
            (PluginState::HandshakeWait, PluginState::Active),
            (PluginState::HandshakeWait, PluginState::Draining),
            (PluginState::HandshakeWait, PluginState::Dead),
            (PluginState::Active, PluginState::Draining),
            (PluginState::Active, PluginState::Dead),
            (PluginState::Draining, PluginState::Dead),
        ];
        for (from, to) in pairs {
            assert!(
                from.transition(to).is_ok(),
                "expected {from} → {to} to be valid"
            );
        }
    }

    #[test]
    fn state_machine_invalid_transitions() {
        use dust_core::state::PluginState;
        // Dead is terminal.
        for to in [
            PluginState::Spawned,
            PluginState::Connected,
            PluginState::HandshakeWait,
            PluginState::Active,
            PluginState::Draining,
            PluginState::Dead,
        ] {
            assert!(
                PluginState::Dead.transition(to).is_err(),
                "Dead → {to} must be invalid"
            );
        }
        // Skipping states.
        assert!(PluginState::Spawned.transition(PluginState::Active).is_err());
        assert!(PluginState::Connected.transition(PluginState::Active).is_err());
        // Backward transitions.
        assert!(PluginState::Active.transition(PluginState::Connected).is_err());
        assert!(PluginState::Draining.transition(PluginState::Active).is_err());
    }

    // ── Stale socket cleanup ──────────────────────────────────────────────────

    #[tokio::test]
    async fn stale_socket_cleanup_removes_unknown_ids() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().to_path_buf();

        // Create two socket files: one known, one stale.
        tokio::fs::File::create(runtime_dir.join("known-plugin.sock"))
            .await
            .unwrap();
        tokio::fs::File::create(runtime_dir.join("stale-plugin.sock"))
            .await
            .unwrap();

        let known = vec!["known-plugin".to_string()];
        cleanup_stale_sockets(&runtime_dir, &known).await.unwrap();

        assert!(
            runtime_dir.join("known-plugin.sock").exists(),
            "known socket must not be removed"
        );
        assert!(
            !runtime_dir.join("stale-plugin.sock").exists(),
            "stale socket must be removed"
        );
    }

    #[tokio::test]
    async fn stale_socket_cleanup_removes_all_when_no_known_ids() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().to_path_buf();

        for name in &["alpha.sock", "beta.sock", "gamma.sock"] {
            tokio::fs::File::create(runtime_dir.join(name))
                .await
                .unwrap();
        }

        cleanup_stale_sockets(&runtime_dir, &[]).await.unwrap();

        for name in &["alpha.sock", "beta.sock", "gamma.sock"] {
            assert!(
                !runtime_dir.join(name).exists(),
                "{name} should have been removed"
            );
        }
    }

    #[tokio::test]
    async fn stale_socket_cleanup_ignores_non_sock_files() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().to_path_buf();

        tokio::fs::File::create(runtime_dir.join("notes.txt"))
            .await
            .unwrap();
        tokio::fs::File::create(runtime_dir.join("stale.sock"))
            .await
            .unwrap();

        cleanup_stale_sockets(&runtime_dir, &[]).await.unwrap();

        // Non-.sock file must survive.
        assert!(runtime_dir.join("notes.txt").exists());
        // .sock file with no known ID must be removed.
        assert!(!runtime_dir.join("stale.sock").exists());
    }

    #[tokio::test]
    async fn stale_socket_cleanup_noop_on_missing_runtime_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        // Must not panic or return Err.
        cleanup_stale_sockets(&missing, &[]).await.unwrap();
    }

    // ── Collision detection ───────────────────────────────────────────────────

    #[tokio::test]
    async fn spawn_plugin_refuses_if_id_in_existing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let plugins_dir = dir.path().join("plugins");
        let runtime_dir = dir.path().join("run");
        tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
        tokio::fs::create_dir_all(&runtime_dir).await.unwrap();

        // Write a minimal plugin.json.
        let plugin_dir = plugins_dir.join("my-plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        tokio::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name":"my-plugin","dust":{"binary":"bin","protocol_version":"1.0.0"}}"#,
        )
        .await
        .unwrap();

        // Pretend "my-plugin" already has an active handle.
        let mut existing = HashSet::new();
        existing.insert("my-plugin".to_string());

        let obs = Arc::new(ObservabilityWriter::new_with_path(
            dir.path().join("obs.jsonl"),
        ));
        let result = spawn_plugin(
            &plugin_dir.join("plugin.json"),
            &runtime_dir,
            &existing,
            obs,
        )
        .await;

        assert!(
            matches!(result, Err(RegistryError::Collision(_))),
            "expected Collision, got some other error"
        );
    }

    #[tokio::test]
    async fn spawn_plugin_allows_when_id_not_in_existing_ids() {
        let dir = tempfile::tempdir().unwrap();
        let plugins_dir = dir.path().join("plugins");
        let runtime_dir = dir.path().join("run");
        tokio::fs::create_dir_all(&plugins_dir).await.unwrap();
        tokio::fs::create_dir_all(&runtime_dir).await.unwrap();

        let plugin_dir = plugins_dir.join("my-plugin");
        tokio::fs::create_dir_all(&plugin_dir).await.unwrap();
        // The binary doesn't exist, so spawn_plugin will fail with an Io error
        // (not Collision) — that's the expected behavior.
        tokio::fs::write(
            plugin_dir.join("plugin.json"),
            r#"{"name":"my-plugin","dust":{"binary":"nonexistent-binary","protocol_version":"1.0.0"}}"#,
        )
        .await
        .unwrap();

        let existing: HashSet<String> = HashSet::new(); // empty — no existing plugins

        let obs = Arc::new(ObservabilityWriter::new_with_path(
            dir.path().join("obs.jsonl"),
        ));
        let result = spawn_plugin(
            &plugin_dir.join("plugin.json"),
            &runtime_dir,
            &existing,
            obs,
        )
        .await;

        // Must NOT be a Collision error — it should be an Io/spawn error.
        assert!(
            !matches!(result, Err(RegistryError::Collision(_))),
            "must not return Collision when ID is not in existing set"
        );
    }

    // ── Heartbeat timeout ─────────────────────────────────────────────────────

    /// Build a minimal fake plugin listener that performs the ready handshake
    /// but then goes silent (no heartbeats).  Returns the socket path and a
    /// task handle for the fake server.
    async fn spawn_silent_plugin(
        socket_path: &Path,
        manifest_name: &str,
    ) -> tokio::task::JoinHandle<()> {
        use tokio::net::UnixListener;

        let listener = UnixListener::bind(socket_path).unwrap();
        let name = manifest_name.to_owned();

        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (mut reader, mut writer) = stream.into_split();

            // Read the host_info event (registry sends it after ready).
            // First, send the ready event.
            let ready = Envelope::Event(EventEnvelope {
                id: "evt_0000000000000001".into(),
                event_type: EventType::Ready,
                ts: "2026-04-12T00:00:00.000Z".into(),
                sequence: Some(1),
                data: serde_json::json!({
                    "manifest": {
                        "name": name,
                        "version": "0.1.0",
                        "description": "fake plugin for testing",
                        "capabilities": []
                    },
                    "protocol_version": "1.0.0",
                    "plugin_info": {"pid": 0, "started_at": "2026-04-12T00:00:00.000Z"}
                }),
            });

            let payload = serde_json::to_vec(&ready).unwrap();
            let len = (payload.len() as u32).to_be_bytes();
            let _ = writer.write_all(&len).await;
            let _ = writer.write_all(&payload).await;
            let _ = writer.flush().await;

            // Read (and ignore) the host_info from the registry.
            let mut len_buf = [0u8; 4];
            let _ = reader.read_exact(&mut len_buf).await;
            let host_info_len = u32::from_be_bytes(len_buf) as usize;
            let mut tmp = vec![0u8; host_info_len];
            let _ = reader.read_exact(&mut tmp).await;

            // Now go silent — no heartbeats, no responses.
            // Keep the connection open until the registry kills it.
            tokio::time::sleep(Duration::from_secs(60)).await;
        })
    }

    #[tokio::test]
    async fn heartbeat_timeout_kills_plugin_after_three_misses() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().to_path_buf();
        let socket_path = runtime_dir.join("hb-test.sock");

        // Start a fake plugin that does the handshake but sends no heartbeats.
        let _server = spawn_silent_plugin(&socket_path, "hb-test").await;

        // Give the fake plugin a moment to bind.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Create a socket pair to simulate the registry-side of the connection:
        // connect to the fake plugin socket directly.
        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let (mut read_half, mut write_half) = stream.into_split();

        // Registry sends ready? No — the registry READS the ready, then sends
        // host_info. But here we ARE the registry side.  Let's drive the
        // handshake manually and then hand the halves to plugin_active_task.

        // Read the ready event from the fake plugin.
        let (_mf, pv) = read_ready_event(&mut read_half).await.unwrap();
        assert_eq!(pv, "1.0.0");

        // Send host_info.
        send_host_info(&mut write_half, 1).await.unwrap();

        // Use a very short heartbeat interval so the test runs in <500 ms.
        let dust = test_dust(50 /* ms */, 25 /* drain ms */);

        let state = Arc::new(std::sync::Mutex::new(PluginState::Active));
        let (_shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        // Use /bin/sleep as a stand-in process — the task will SIGKILL it when
        // the heartbeat timeout fires.
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("/bin/sleep must exist");
        let child_arc = Arc::new(tokio::sync::Mutex::new(child));

        let state_clone = Arc::clone(&state);
        let test_ring = Arc::new(std::sync::Mutex::new(EventRing::new()));
        let (test_tx, _test_rx) = tokio::sync::broadcast::channel(16);
        let test_obs = {
            let dir = tempfile::tempdir().unwrap();
            Arc::new(ObservabilityWriter::new_with_path(dir.keep().join("obs.jsonl")))
        };
        let _task = tokio::spawn(plugin_active_task(
            "hb-test".into(),
            read_half,
            write_half,
            dust,
            PathBuf::from("/bin/sleep"),
            state_clone,
            shutdown_rx,
            Arc::clone(&child_arc),
            None, // no widget refresh for this test
            test_ring,
            test_tx,
            test_obs,
        ));

        // Wait for 3 missed intervals plus a buffer.
        // 3 × 50 ms = 150 ms; we wait 400 ms to be safe.
        tokio::time::sleep(Duration::from_millis(400)).await;

        assert_eq!(
            *state.lock().unwrap(),
            PluginState::Dead,
            "plugin must be Dead after heartbeat timeout"
        );
    }

    // ── Shutdown drain ────────────────────────────────────────────────────────

    /// Build a fake plugin that performs the ready handshake and then
    /// cooperates: it reads the shutdown envelope and closes the connection.
    async fn spawn_cooperative_plugin(
        socket_path: &Path,
        manifest_name: &str,
    ) -> tokio::task::JoinHandle<()> {
        use tokio::net::UnixListener;

        let listener = UnixListener::bind(socket_path).unwrap();
        let name = manifest_name.to_owned();

        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (mut reader, mut writer) = stream.into_split();

            // Send ready.
            let ready = Envelope::Event(EventEnvelope {
                id: "evt_0000000000000001".into(),
                event_type: EventType::Ready,
                ts: "2026-04-12T00:00:00.000Z".into(),
                sequence: Some(1),
                data: serde_json::json!({
                    "manifest": {
                        "name": name,
                        "version": "0.1.0",
                        "description": "cooperative fake plugin",
                        "capabilities": []
                    },
                    "protocol_version": "1.0.0",
                    "plugin_info": {"pid": 0, "started_at": "2026-04-12T00:00:00.000Z"}
                }),
            });
            let payload = serde_json::to_vec(&ready).unwrap();
            let len = (payload.len() as u32).to_be_bytes();
            let _ = writer.write_all(&len).await;
            let _ = writer.write_all(&payload).await;
            let _ = writer.flush().await;

            // Read and discard host_info.
            let mut len_buf = [0u8; 4];
            let _ = reader.read_exact(&mut len_buf).await;
            let n = u32::from_be_bytes(len_buf) as usize;
            let mut tmp = vec![0u8; n];
            let _ = reader.read_exact(&mut tmp).await;

            // Wait for a message (heartbeat or shutdown), then close.
            loop {
                let mut lb = [0u8; 4];
                if reader.read_exact(&mut lb).await.is_err() {
                    break;
                }
                let mlen = u32::from_be_bytes(lb) as usize;
                let mut mbuf = vec![0u8; mlen];
                if reader.read_exact(&mut mbuf).await.is_err() {
                    break;
                }
                if let Ok(Envelope::Shutdown(_)) = serde_json::from_slice(&mbuf) {
                    // Acknowledge shutdown by closing the connection.
                    break;
                }
                // Otherwise continue (heartbeats).
            }
            // Connection closes when this task exits.
        })
    }

    #[tokio::test]
    async fn graceful_shutdown_transitions_to_dead_within_drain_window() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().to_path_buf();
        let socket_path = runtime_dir.join("shutdown-test.sock");

        let _server = spawn_cooperative_plugin(&socket_path, "shutdown-test").await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let (mut read_half, mut write_half) = stream.into_split();

        // Handshake.
        let _ = read_ready_event(&mut read_half).await.unwrap();
        send_host_info(&mut write_half, 1).await.unwrap();

        let dust = test_dust(200 /* heartbeat ms */, 100 /* drain ms */);
        let state = Arc::new(std::sync::Mutex::new(PluginState::Active));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("/bin/sleep must exist");
        let child_arc = Arc::new(tokio::sync::Mutex::new(child));

        let state_clone = Arc::clone(&state);
        let test_ring = Arc::new(std::sync::Mutex::new(EventRing::new()));
        let (test_tx, _test_rx) = tokio::sync::broadcast::channel(16);
        let test_obs = {
            let dir = tempfile::tempdir().unwrap();
            Arc::new(ObservabilityWriter::new_with_path(dir.keep().join("obs.jsonl")))
        };
        let _task = tokio::spawn(plugin_active_task(
            "shutdown-test".into(),
            read_half,
            write_half,
            dust,
            PathBuf::from("/bin/sleep"),
            state_clone,
            shutdown_rx,
            Arc::clone(&child_arc),
            None, // no widget refresh for this test
            test_ring,
            test_tx,
            test_obs,
        ));

        // Verify Active before sending shutdown.
        assert_eq!(*state.lock().unwrap(), PluginState::Active);

        // Trigger graceful shutdown.
        shutdown_tx.send(ShutdownReason::PluginDisable).unwrap();

        // After drain completes (cooperative plugin closes promptly) the state
        // should reach Dead within the drain window + buffer.
        // drain_ms = 100; we wait up to 500 ms.
        let mut attempts = 0;
        loop {
            if *state.lock().unwrap() == PluginState::Dead {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
            attempts += 1;
            assert!(attempts < 25, "plugin never transitioned to Dead");
        }
    }

    #[tokio::test]
    async fn shutdown_drain_times_out_when_plugin_stays_open() {
        let dir = tempfile::tempdir().unwrap();
        let runtime_dir = dir.path().to_path_buf();
        let socket_path = runtime_dir.join("drain-timeout-test.sock");

        // Fake plugin: does the handshake but ignores the shutdown envelope.
        let _server = spawn_silent_plugin(&socket_path, "drain-timeout-test").await;
        tokio::time::sleep(Duration::from_millis(10)).await;

        let stream = UnixStream::connect(&socket_path).await.unwrap();
        let (mut read_half, mut write_half) = stream.into_split();

        let _ = read_ready_event(&mut read_half).await.unwrap();
        send_host_info(&mut write_half, 1).await.unwrap();

        // Very short drain timeout (50 ms) so the test doesn't take long.
        let dust = test_dust(5_000 /* heartbeat: large so it doesn't interfere */, 50 /* drain ms */);
        let state = Arc::new(std::sync::Mutex::new(PluginState::Active));
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("/bin/sleep must exist");
        let child_arc = Arc::new(tokio::sync::Mutex::new(child));

        let state_clone = Arc::clone(&state);
        let test_ring = Arc::new(std::sync::Mutex::new(EventRing::new()));
        let (test_tx, _test_rx) = tokio::sync::broadcast::channel(16);
        let test_obs = {
            let dir = tempfile::tempdir().unwrap();
            Arc::new(ObservabilityWriter::new_with_path(dir.keep().join("obs.jsonl")))
        };
        let _task = tokio::spawn(plugin_active_task(
            "drain-timeout-test".into(),
            read_half,
            write_half,
            dust,
            PathBuf::from("/bin/sleep"),
            state_clone,
            shutdown_rx,
            Arc::clone(&child_arc),
            None, // no widget refresh for this test
            test_ring,
            test_tx,
            test_obs,
        ));

        // Trigger shutdown.
        shutdown_tx.send(ShutdownReason::HostExit).unwrap();

        // The plugin keeps its connection open (silent), so drain times out
        // after 50 ms.  We wait 300 ms.
        tokio::time::sleep(Duration::from_millis(300)).await;

        assert_eq!(
            *state.lock().unwrap(),
            PluginState::Dead,
            "plugin must be Dead after drain timeout"
        );
    }

    // ── Subscriber management — ring + subscribe / unsubscribe ────────────────

    fn make_event(seq: u64) -> EventEnvelope {
        EventEnvelope {
            id: format!("evt_{seq:016x}"),
            event_type: EventType::DataUpdated,
            ts: "2026-04-12T00:00:00.000Z".into(),
            sequence: Some(seq),
            data: serde_json::json!({}),
        }
    }

    type TestComponents = (
        Arc<std::sync::Mutex<EventRing>>,
        tokio::sync::broadcast::Sender<EventEnvelope>,
        Arc<std::sync::Mutex<SubscriberMap>>,
    );

    fn make_test_components() -> TestComponents {
        let ring = Arc::new(std::sync::Mutex::new(EventRing::new()));
        let (tx, _rx) = tokio::sync::broadcast::channel(EVENT_BROADCAST_CAPACITY);
        let subs = Arc::new(std::sync::Mutex::new(SubscriberMap::new()));
        (ring, tx, subs)
    }

    // ── Ring push + eviction by count ─────────────────────────────────────────

    #[test]
    fn registry_ring_eviction_by_count() {
        use dust_core::events::MAX_RING_EVENTS;
        let (ring, _, _) = make_test_components();
        let mut r = ring.lock().unwrap();
        for i in 1..=(MAX_RING_EVENTS as u64 + 1) {
            r.push(make_event(i));
        }
        assert_eq!(r.len(), MAX_RING_EVENTS);
        assert_eq!(r.oldest_sequence(), Some(2));
    }

    // ── Ring push + eviction by byte bound ────────────────────────────────────

    #[test]
    fn registry_ring_eviction_by_byte_bound() {
        let (ring, _, _) = make_test_components();
        let mut r = ring.lock().unwrap();
        // Two 300 KiB events exceed 512 KiB → first is evicted on second push.
        let big_payload = "x".repeat(300 * 1_024);
        let big_event = |seq: u64| EventEnvelope {
            id: format!("evt_{seq:016x}"),
            event_type: EventType::DataUpdated,
            ts: "2026-04-12T00:00:00.000Z".into(),
            sequence: Some(seq),
            data: serde_json::json!({ "payload": big_payload }),
        };
        r.push(big_event(1));
        r.push(big_event(2));
        assert_eq!(r.len(), 1, "seq=1 must be evicted");
        assert_eq!(r.oldest_sequence(), Some(2));
    }

    // ── Subscribe with valid cursor ───────────────────────────────────────────

    #[test]
    fn registry_subscribe_valid_cursor() {
        let (ring, tx, subs) = make_test_components();
        {
            let mut r = ring.lock().unwrap();
            for i in 1..=5u64 {
                r.push(make_event(i));
            }
        }
        subs.lock().unwrap().open_connection(ConnectionId(1)).unwrap();
        let result = handle_subscribe_request(&ring, &tx, &subs, ConnectionId(1), 3).unwrap();
        assert_eq!(result.events.len(), 3);
        assert_eq!(result.events[0].sequence, Some(3));
        assert_eq!(result.events[2].sequence, Some(5));
        assert_eq!(result.next_sequence, 6);
    }

    // ── Subscribe with too-old cursor → replay_gap ────────────────────────────

    #[test]
    fn registry_subscribe_replay_gap() {
        use dust_core::events::MAX_RING_EVENTS;
        let (ring, tx, subs) = make_test_components();
        {
            let mut r = ring.lock().unwrap();
            for i in 1..=(MAX_RING_EVENTS as u64 + 1) {
                r.push(make_event(i));
            }
        }
        subs.lock().unwrap().open_connection(ConnectionId(1)).unwrap();
        let err = handle_subscribe_request(&ring, &tx, &subs, ConnectionId(1), 1).unwrap_err();
        assert!(
            matches!(err, RegistryError::PluginError { code: -33007, .. }),
            "expected -33007 ReplayGap, got {err:?}"
        );
    }

    // ── Subscribe with future cursor → empty + live push ─────────────────────

    #[tokio::test]
    async fn registry_subscribe_future_cursor_and_live() {
        let (ring, tx, subs) = make_test_components();
        {
            let mut r = ring.lock().unwrap();
            for i in 1..=3u64 {
                r.push(make_event(i));
            }
        }
        subs.lock().unwrap().open_connection(ConnectionId(1)).unwrap();
        let result = handle_subscribe_request(&ring, &tx, &subs, ConnectionId(1), 100).unwrap();
        assert!(result.events.is_empty(), "future cursor must return empty snapshot");
        assert_eq!(result.next_sequence, 4);

        let mut live_rx = result.live_rx;
        // Simulate the plugin emitting a new event.
        let event4 = make_event(4);
        ring.lock().unwrap().push(event4.clone());
        let _ = tx.send(event4.clone());

        let received = tokio::time::timeout(
            std::time::Duration::from_millis(200),
            async { live_rx.recv().await },
        )
        .await
        .expect("timed out waiting for live event")
        .expect("broadcast recv error");
        assert_eq!(received.sequence, Some(4));
    }

    // ── Unsubscribe ───────────────────────────────────────────────────────────

    #[test]
    fn registry_unsubscribe() {
        let (ring, tx, subs) = make_test_components();
        subs.lock().unwrap().open_connection(ConnectionId(1)).unwrap();
        let result = handle_subscribe_request(&ring, &tx, &subs, ConnectionId(1), 0).unwrap();
        let sub_id = result.subscription_id.clone();

        // Unsubscribe must succeed.
        handle_unsubscribe_request(&subs, ConnectionId(1), &sub_id).unwrap();

        // A second unsubscribe with the same ID must fail (no active subscription).
        let err = handle_unsubscribe_request(&subs, ConnectionId(1), &sub_id).unwrap_err();
        assert!(matches!(err, RegistryError::PluginError { code: -32602, .. }));

        // Can subscribe again after unsubscribing.
        let result2 = handle_subscribe_request(&ring, &tx, &subs, ConnectionId(1), 0).unwrap();
        assert_ne!(
            result2.subscription_id, sub_id,
            "new subscription must get a fresh subscription_id"
        );
    }

    // ── Second subscribe on same connection rejected ───────────────────────────

    #[test]
    fn registry_second_subscribe_rejected() {
        let (ring, tx, subs) = make_test_components();
        subs.lock().unwrap().open_connection(ConnectionId(1)).unwrap();

        handle_subscribe_request(&ring, &tx, &subs, ConnectionId(1), 0).unwrap();

        let err = handle_subscribe_request(&ring, &tx, &subs, ConnectionId(1), 0).unwrap_err();
        assert!(
            matches!(err, RegistryError::PluginError { code: -33005, .. }),
            "expected -33005 Busy, got {err:?}"
        );
    }

    // ── Subscriber connection limit (16) ──────────────────────────────────────

    #[test]
    fn registry_subscriber_connection_limit() {
        let (_, _, subs) = make_test_components();
        let mut subs_guard = subs.lock().unwrap();
        for i in 1..=(MAX_SUBSCRIBERS as u64) {
            subs_guard
                .open_connection(ConnectionId(i))
                .unwrap_or_else(|_| panic!("connection {i} should succeed"));
        }
        // 17th connection must fail with -33005 Busy.
        let err = subs_guard
            .open_connection(ConnectionId(MAX_SUBSCRIBERS as u64 + 1))
            .unwrap_err();
        assert_eq!(err, -33005, "17th connection must return -33005");
    }

    // ── Close connection releases subscription ────────────────────────────────

    #[test]
    fn registry_close_connection_frees_slot() {
        let (_, _, subs) = make_test_components();
        let mut subs_guard = subs.lock().unwrap();

        // Fill all 16 slots.
        for i in 1..=(MAX_SUBSCRIBERS as u64) {
            subs_guard.open_connection(ConnectionId(i)).unwrap();
        }
        // 17th must fail.
        assert!(subs_guard.open_connection(ConnectionId(17)).is_err());

        // Close one slot.
        subs_guard.close_connection(ConnectionId(1));

        // Now a new connection should succeed.
        subs_guard
            .open_connection(ConnectionId(17))
            .expect("slot freed by close_connection must accept new connection");
    }
}
