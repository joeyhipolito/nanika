//! Application state and key-event dispatch for the dust dashboard.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dust_core::envelope::{ActionParams, EventEnvelope, EventType};
use dust_core::{ActionResult, Capability, Component, PluginManifest};
use dust_registry::{ConnectionId, Registry};
use tokio::sync::mpsc;

use crate::component_renderer;
use crate::slash_grammar::{parse_slash, SlashCommand};

// ── Tracker ops ───────────────────────────────────────────────────────────────

/// `(op_id, display_label, arg_prompts)` for the six tracker operations.
const TRACKER_OPS: &[(&str, &str, &[&str])] = &[
    ("next",   "Next ready issue",   &[]),
    ("ready",  "List ready issues",  &[]),
    ("tree",   "List all issues",    &[]),
    ("create", "Create issue",       &["Title"]),
    ("update", "Update issue",       &["Issue ID (TRK-N)", "Status"]),
    ("delete", "Delete issue",       &["Issue ID (TRK-N)"]),
];

// ── State machine ─────────────────────────────────────────────────────────────

/// TUI state machine.
///
/// ```text
/// Idle ──(type)──► Searching ──(↑/↓)──► SelectedCapability ──(Enter)──► UILoaded
///   ▲                  │                         │                           │
///   └──────(Esc)───────┘─────────────────────────┘              ┌────────────┤
///                                                                │         Ctrl+K
///                                                            (r/auto)        │
///                                                                │         PaletteOpen
///                                                                │           │ Enter
///                                                                │     PaletteCollectingArg
///                                                                │           │ Enter (last arg)
///                                                                └── ActionDispatched ◄──┘
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppState {
    /// Empty query; showing all registered plugins.
    Idle,
    /// User is typing a query; results are filtered.
    Searching,
    /// User has moved the cursor to a specific result.
    SelectedCapability,
    /// `render_ui` was called; showing the plugin's component tree.
    UILoaded,
    /// `dispatch_action` was called; showing the result.
    ActionDispatched,
    /// Ctrl+K palette open — user is choosing an op from the list.
    PaletteOpen,
    /// Collecting argument `palette_ctx.arg_idx` for the selected op.
    PaletteCollectingArg,
    /// Full-screen chat view entered via `/` from browsing states.
    Chatting,
}

// ── PaletteCtx ────────────────────────────────────────────────────────────────

/// All palette state kept in one place so resets are a single field assignment.
pub struct PaletteCtx {
    /// Highlighted row in the op list (PaletteOpen).
    pub cursor: usize,
    /// Index into [`TRACKER_OPS`] for the chosen op.
    pub op_idx: usize,
    /// Which arg prompt we're currently collecting.
    pub arg_idx: usize,
    /// Text buffer for the arg currently being typed.
    pub buffer: String,
    /// Args already confirmed (one entry per prompt, in order).
    pub collected: Vec<String>,
}

impl Default for PaletteCtx {
    fn default() -> Self {
        Self {
            cursor: 0,
            op_idx: 0,
            arg_idx: 0,
            buffer: String::new(),
            collected: Vec::new(),
        }
    }
}

// ── EventStreamState ──────────────────────────────────────────────────────────

struct EventStreamState {
    plugin_id: String,
    conn_id: ConnectionId,
    subscription_id: String,
    task: tokio::task::JoinHandle<()>,
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub state: AppState,
    /// Current search query.
    pub query: String,
    /// Search results: `(plugin_id, manifest)` pairs sorted by score.
    pub results: Vec<(String, PluginManifest)>,
    /// Index into `results` that the cursor is on.
    pub selected_index: usize,
    /// Components loaded by `render_ui` (populated in `UILoaded` state).
    pub components: Vec<Component>,
    /// Result of the most recent `dispatch_action` call.
    pub action_result: Option<ActionResult>,
    /// Palette state (reset on each Ctrl+K open).
    pub palette_ctx: PaletteCtx,
    /// Transient error or status message shown at the bottom of the detail pane.
    pub status_msg: Option<String>,
    pub should_quit: bool,
    /// Highlighted row index in the main Table component.
    pub table_cursor: usize,
    /// Whether the right-side issue detail pane is open.
    pub detail_open: bool,
    /// All issues from the most recent `tree` action (for detail pane lookup).
    pub cached_issues: Vec<serde_json::Value>,
    /// Receives plugin events forwarded from the background subscription task.
    event_rx: mpsc::UnboundedReceiver<EventEnvelope>,
    event_tx: mpsc::UnboundedSender<EventEnvelope>,
    event_stream: Option<EventStreamState>,
    registry: Arc<Registry>,
    // ── Chat state ─────────────────────────────────────────────────────────────
    /// Plugin ID for the chat plugin (set on enter_chat).
    pub chat_plugin_id: Option<String>,
    /// Thread list from the most recent `list_threads` call.
    pub chat_threads: Vec<serde_json::Value>,
    /// Highlighted row in the thread list.
    pub chat_thread_cursor: usize,
    /// Currently active thread ID (None = new thread on next send).
    pub chat_active_thread_id: Option<String>,
    /// Streaming AgentTurn components from DataUpdated events.
    pub chat_messages: Vec<Component>,
    /// Text buffer for the chat input line.
    pub chat_input: String,
    /// Whether the left thread-list column is visible.
    pub chat_show_threads: bool,
    /// `(component_idx, hunk_idx)` of the currently highlighted CodeDiff
    /// hunk, if any. Set when render_ui returns components containing a
    /// CodeDiff; cleared otherwise. Scopes `a` / `r` keybindings.
    pub code_diff_cursor: Option<(usize, usize)>,
    /// Ids of hunks the user dismissed via `r` (client-side reject).
    pub code_diff_rejected: std::collections::HashSet<String>,
    /// Ids of hunks the user accepted via `a` (dispatch succeeded).
    pub code_diff_accepted: std::collections::HashSet<String>,
    /// `tool_use_id`s of ToolCallBeat components rendered in expanded form.
    /// Toggled by the `t` key in Chatting mode.
    pub tool_call_expanded: HashSet<String>,
}

impl App {
    pub fn new(registry: Arc<Registry>) -> Self {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        Self {
            state: AppState::Idle,
            query: String::new(),
            results: Vec::new(),
            selected_index: 0,
            components: Vec::new(),
            action_result: None,
            palette_ctx: PaletteCtx::default(),
            status_msg: None,
            should_quit: false,
            table_cursor: 0,
            detail_open: false,
            cached_issues: Vec::new(),
            event_rx,
            event_tx,
            event_stream: None,
            registry,
            chat_plugin_id: None,
            chat_threads: Vec::new(),
            chat_thread_cursor: 0,
            chat_active_thread_id: None,
            chat_messages: Vec::new(),
            chat_input: String::new(),
            chat_show_threads: false,
            code_diff_cursor: None,
            code_diff_rejected: std::collections::HashSet::new(),
            code_diff_accepted: std::collections::HashSet::new(),
            tool_call_expanded: HashSet::new(),
        }
    }

    /// Toggle the expanded state for the most recent `ToolCallBeat` in
    /// `chat_messages`. No-op when no beat is present.
    pub fn toggle_latest_tool_beat(&mut self) {
        let latest = self.chat_messages.iter().rev().find_map(|c| match c {
            Component::ToolCallBeat { tool_use_id, .. } => Some(tool_use_id.clone()),
            _ => None,
        });
        if let Some(id) = latest {
            if !self.tool_call_expanded.remove(&id) {
                self.tool_call_expanded.insert(id);
            }
        }
    }

    /// Refresh `code_diff_cursor` from `self.components`.
    ///
    /// Points the cursor at the first hunk of the first `CodeDiff` component,
    /// or clears it if no such component is present.
    fn refresh_code_diff_cursor(&mut self) {
        let found = self.components.iter().enumerate().find_map(|(i, c)| {
            if let Component::CodeDiff { hunks, .. } = c {
                if !hunks.is_empty() {
                    return Some((i, 0usize));
                }
            }
            None
        });
        self.code_diff_cursor = found;
    }

    /// Borrow the `hunks` slice pointed at by `code_diff_cursor`, if any.
    fn code_diff_hunks_at_cursor(&self) -> Option<(usize, usize, &[dust_core::Hunk])> {
        let (cidx, hidx) = self.code_diff_cursor?;
        let hunks = self.components.iter().enumerate().find_map(|(i, c)| {
            if i == cidx {
                if let Component::CodeDiff { hunks, .. } = c {
                    return Some(hunks.as_slice());
                }
            }
            None
        })?;
        Some((cidx, hidx, hunks))
    }

    // ── Registry interaction ──────────────────────────────────────────────────

    /// Re-query the registry and update `results`.
    ///
    /// Preserves the cursor on the same plugin by ID so that hot-plug events
    /// don't shift the selection.
    pub async fn refresh_results(&mut self) {
        let selected_id = self
            .results
            .get(self.selected_index)
            .map(|(id, _)| id.clone());

        self.results = self.registry.search_with_ids(&self.query).await;

        self.selected_index = selected_id
            .and_then(|id| self.results.iter().position(|(pid, _)| *pid == id))
            .unwrap_or_else(|| {
                if self.results.is_empty() {
                    0
                } else {
                    self.selected_index.min(self.results.len() - 1)
                }
            });

        self.state = if self.query.trim().is_empty() {
            AppState::Idle
        } else {
            AppState::Searching
        };
    }

    /// Background poll: sync filesystem → registry, then refresh the results
    /// list without disrupting UI state or clearing loaded components.
    ///
    /// Returns `true` if the results list changed (caller should clear terminal).
    pub async fn poll_registry(&mut self) -> bool {
        let _ = self.registry.sync().await;

        let selected_id = self
            .results
            .get(self.selected_index)
            .map(|(id, _)| id.clone());

        let new_results = self.registry.search_with_ids(&self.query).await;

        let ids_unchanged = new_results.len() == self.results.len()
            && new_results
                .iter()
                .zip(self.results.iter())
                .all(|((a, _), (b, _))| a == b);
        if ids_unchanged {
            return false;
        }

        self.results = new_results;

        self.selected_index = selected_id
            .and_then(|id| self.results.iter().position(|(pid, _)| *pid == id))
            .unwrap_or_else(|| {
                if self.results.is_empty() {
                    0
                } else {
                    self.selected_index.min(self.results.len() - 1)
                }
            });

        true
    }

    // ── Event stream ──────────────────────────────────────────────────────────

    /// Drain one pending plugin event from the mpsc channel (non-blocking).
    pub fn try_recv_event(&mut self) -> Option<EventEnvelope> {
        self.event_rx.try_recv().ok()
    }

    /// Open a live event subscription for `plugin_id`.
    ///
    /// Spawns a background task that forwards broadcast events to `event_tx`.
    /// No-op if a stream is already open for the same plugin.
    async fn open_event_stream(&mut self, plugin_id: &str) {
        if self
            .event_stream
            .as_ref()
            .map_or(false, |s| s.plugin_id == plugin_id)
        {
            return;
        }
        self.close_event_stream().await;

        let handle = match self.registry.connect_and_subscribe(plugin_id, 0).await {
            Ok(h) => h,
            Err(e) => {
                eprintln!("dust-dashboard: subscribe failed for {plugin_id}: {e}");
                return;
            }
        };

        let tx = self.event_tx.clone();
        let mut live_rx = handle.live_rx;
        let task = tokio::spawn(async move {
            loop {
                match live_rx.recv().await {
                    Ok(event) => {
                        if tx.send(event).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => break,
                }
            }
        });

        self.event_stream = Some(EventStreamState {
            plugin_id: plugin_id.to_string(),
            conn_id: handle.conn_id,
            subscription_id: handle.subscription_id,
            task,
        });
    }

    /// Abort the background subscription task and release the connection slot.
    async fn close_event_stream(&mut self) {
        if let Some(stream) = self.event_stream.take() {
            stream.task.abort();
            let _ = self
                .registry
                .disconnect_subscriber(
                    &stream.plugin_id,
                    stream.conn_id,
                    &stream.subscription_id,
                )
                .await;
        }
    }

    /// Handle a plugin event received from the live subscription.
    ///
    /// In Chatting state, streaming components arrive directly in `event.data`
    /// and are stored in `chat_messages`.  In all other states the existing
    /// re-render-via-render_ui path is used.
    pub async fn handle_plugin_event(&mut self, event: EventEnvelope) {
        // Stream errors from the chat plugin must surface — otherwise the
        // blinking caret keeps animating on the last AgentTurn and the user
        // has no signal that the request died.
        if event.event_type == EventType::Error && self.state == AppState::Chatting {
            let message = event
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("stream error")
                .to_string();
            // Force any trailing AgentTurn out of streaming state so the caret
            // stops. Then append a terracotta-styled error Text component.
            if let Some(Component::AgentTurn { streaming, .. }) = self.chat_messages.last_mut() {
                *streaming = false;
            }
            self.chat_messages.push(Component::Text {
                content: format!("⚠ {}", message),
                style: dust_core::TextStyle {
                    bold: true,
                    // Terracotta to match the intaglio error accent
                    color: Some(dust_core::Color::new(0xDA, 0x77, 0x57)),
                    ..Default::default()
                },
            });
            return;
        }

        if event.event_type != EventType::DataUpdated {
            return;
        }

        if self.state == AppState::Chatting {
            if let Ok(components) =
                serde_json::from_value::<Vec<Component>>(event.data.clone())
            {
                if !components.is_empty() {
                    // Detect streaming completion on the last assistant turn.
                    let streaming_done = components.iter().rev().find_map(|c| {
                        if let Component::AgentTurn { role, streaming, .. } = c {
                            if role == "assistant" { Some(!streaming) } else { None }
                        } else {
                            None
                        }
                    }).unwrap_or(false);

                    self.chat_messages = components;

                    // If we don't yet know the thread ID (new conversation),
                    // refresh the list once streaming completes — the plugin
                    // creates the thread synchronously before the first event.
                    if streaming_done && self.chat_active_thread_id.is_none() {
                        self.refresh_chat_threads().await;
                        self.chat_active_thread_id = self
                            .chat_threads
                            .first()
                            .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                            .map(|s| s.to_string());
                    }
                }
            }
            return;
        }

        if matches!(
            self.state,
            AppState::UILoaded
                | AppState::PaletteOpen
                | AppState::PaletteCollectingArg
                | AppState::ActionDispatched
        ) {
            if let Some((plugin_id, _)) = self.results.get(self.selected_index).cloned() {
                match self.registry.render_ui(&plugin_id).await {
                    Ok(components) => {
                        self.components = components;
                        self.refresh_code_diff_cursor();
                        self.status_msg = None;
                    }
                    Err(e) => {
                        self.status_msg = Some(format!("auto-refresh: {e}"));
                    }
                }
                if self.detail_open {
                    self.refresh_issue_cache().await;
                }
            }
        }
    }

    // ── Table selection helpers ───────────────────────────────────────────────

    /// Number of rows in the first `Table` component (0 if none).
    pub fn table_row_count(&self) -> usize {
        self.components
            .iter()
            .find_map(|c| {
                if let Component::Table { rows, .. } = c {
                    Some(rows.len())
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    /// The issue detail JSON for the currently highlighted table row, if any.
    ///
    /// Looks up by the TRK-N id in column 0 of the highlighted row.
    pub fn selected_issue_detail(&self) -> Option<&serde_json::Value> {
        let row = self.components.iter().find_map(|c| {
            if let Component::Table { rows, .. } = c {
                rows.get(self.table_cursor)
            } else {
                None
            }
        })?;
        let id_col = row.first()?;
        self.cached_issues.iter().find(|issue| {
            issue
                .get("seq_id")
                .and_then(|v| v.as_i64())
                .map(|n| format!("TRK-{n}") == *id_col)
                .unwrap_or(false)
        })
    }

    /// Dispatch the `tree` action and cache the result for the detail pane.
    async fn refresh_issue_cache(&mut self) {
        let params = ActionParams {
            op_id: Some("tree".to_string()),
            item_id: None,
            args: HashMap::new(),
        };
        if let Some((plugin_id, _)) = self.results.get(self.selected_index).cloned() {
            if let Ok(result) = self.registry.dispatch_action(&plugin_id, params).await {
                if let Some(data) = result.data {
                    if let Some(arr) = data.as_array() {
                        self.cached_issues = arr.clone();
                        return;
                    }
                }
            }
        }
        self.cached_issues.clear();
    }

    // ── Chat helpers ──────────────────────────────────────────────────────────

    /// Enter the Chatting state: subscribe to the chat plugin, load threads,
    /// and set the most recent thread as active.
    pub async fn enter_chat(&mut self) {
        let all = self.registry.search_with_ids("chat").await;
        let plugin_id = match all.into_iter().find(|(id, _)| id == "chat") {
            Some((id, _)) => id,
            None => {
                self.status_msg = Some("chat plugin not registered".to_string());
                return;
            }
        };

        self.open_event_stream(&plugin_id).await;

        self.chat_plugin_id = Some(plugin_id.clone());
        self.chat_thread_cursor = 0;
        self.chat_messages.clear();
        self.chat_input.clear();

        // Populate thread list.
        let params = ActionParams {
            op_id: None,
            item_id: Some("list_threads".to_string()),
            args: HashMap::new(),
        };
        match self.registry.dispatch_action(&plugin_id, params).await {
            Ok(result) => {
                if let Some(arr) = result.data.as_ref().and_then(|d| d.as_array()) {
                    self.chat_threads = arr.clone();
                }
                self.status_msg = None;
            }
            Err(e) => {
                self.status_msg = Some(format!("list_threads: {e}"));
            }
        }

        self.chat_active_thread_id = self
            .chat_threads
            .first()
            .and_then(|t| t.get("id").and_then(|v| v.as_str()))
            .map(|s| s.to_string());
        self.chat_show_threads = !self.chat_threads.is_empty();
        if let Some(tid) = self.chat_active_thread_id.clone() {
            self.load_chat_messages(&tid).await;
        }
        self.state = AppState::Chatting;
    }

    /// Re-fetch the thread list from the chat plugin.
    async fn refresh_chat_threads(&mut self) {
        let Some(plugin_id) = self.chat_plugin_id.clone() else { return };
        let params = ActionParams {
            op_id: None,
            item_id: Some("list_threads".to_string()),
            args: HashMap::new(),
        };
        if let Ok(result) = self.registry.dispatch_action(&plugin_id, params).await {
            if let Some(arr) = result.data.as_ref().and_then(|d| d.as_array()) {
                self.chat_threads = arr.clone();
            }
        }
    }

    /// Load persisted messages for `thread_id` and render them as AgentTurn
    /// components in `chat_messages`. Called on thread-list navigation so
    /// switching threads reveals their history instead of a blank pane.
    async fn load_chat_messages(&mut self, thread_id: &str) {
        let Some(plugin_id) = self.chat_plugin_id.clone() else { return };
        let mut args = HashMap::new();
        args.insert(
            "thread_id".to_string(),
            serde_json::Value::String(thread_id.to_string()),
        );
        let params = ActionParams {
            op_id: None,
            item_id: Some("list_messages".to_string()),
            args,
        };
        self.chat_messages.clear();
        let result = match self.registry.dispatch_action(&plugin_id, params).await {
            Ok(r) => r,
            Err(e) => {
                self.status_msg = Some(format!("list_messages: {e}"));
                return;
            }
        };
        let arr = match result.data.as_ref().and_then(|d| d.as_array()) {
            Some(a) => a.clone(),
            None => return,
        };
        for msg in arr {
            let role = msg.get("role").and_then(|v| v.as_str()).unwrap_or("user");
            let content = msg
                .get("content")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ts = msg.get("created_at").and_then(|v| v.as_i64()).unwrap_or(0);
            self.chat_messages.push(Component::AgentTurn {
                role: role.to_string(),
                content,
                streaming: false,
                timestamp: Some(ts.max(0) as u64),
            });
        }
    }

    /// Resolve a slash-command prefix to a registered plugin id.
    ///
    /// Scans the full plugin list (not just the current results view, which
    /// is filtered by the search query) and returns the id of the first
    /// plugin advertising `Capability::Command { prefix }` that matches.
    async fn plugin_for_slash_prefix(&self, prefix: &str) -> Option<String> {
        let all = self.registry.search_with_ids("").await;
        all.into_iter().find_map(|(plugin_id, manifest)| {
            let matches = manifest.capabilities.iter().any(|c| match c {
                Capability::Command { prefix: p } => p == prefix,
                _ => false,
            });
            if matches {
                Some(plugin_id)
            } else {
                None
            }
        })
    }

    /// Append a terracotta-styled error Text component to `chat_messages`.
    fn push_chat_error(&mut self, msg: impl Into<String>) {
        self.chat_messages.push(Component::Text {
            content: msg.into(),
            style: dust_core::TextStyle {
                bold: true,
                color: Some(dust_core::Color::new(0xDA, 0x77, 0x57)),
                ..Default::default()
            },
        });
    }

    /// Execute a parsed slash command in the Chatting surface.
    ///
    /// `/ask <text>` dispatches to the chat plugin with the post-prefix text
    /// as the message body (title derived from the same). Other prefixes
    /// resolve to the plugin owning `Capability::Command { prefix }`, then
    /// split the tail into `op_id` + `title` — matches the tracker
    /// `create <title>` convention. Unknown prefixes push a terracotta
    /// "Unknown command" Text into the chat transcript.
    async fn dispatch_slash(&mut self, slash: SlashCommand) {
        // /ask goes to the chat plugin even though its capability prefix is
        // "ask"; keep that branch explicit so `text` / `thread_id` plumbing
        // matches the existing streaming dispatch path.
        if slash.prefix == "ask" {
            let Some(plugin_id) = self.chat_plugin_id.clone() else {
                self.push_chat_error("chat plugin not available");
                return;
            };
            let mut args: HashMap<String, serde_json::Value> = HashMap::new();
            args.insert(
                "text".to_string(),
                serde_json::Value::String(slash.args.clone()),
            );
            if let Some(tid) = &self.chat_active_thread_id {
                args.insert(
                    "thread_id".to_string(),
                    serde_json::Value::String(tid.clone()),
                );
            }
            let params = ActionParams {
                op_id: None,
                item_id: Some("ask".to_string()),
                args,
            };
            if let Err(e) = self.registry.dispatch_action(&plugin_id, params).await {
                self.push_chat_error(format!("/ask failed: {e}"));
            } else {
                self.status_msg = None;
            }
            return;
        }

        let Some(plugin_id) = self.plugin_for_slash_prefix(&slash.prefix).await else {
            self.push_chat_error(format!("Unknown command: /{}", slash.prefix));
            return;
        };

        // Generic command: split args into `first_word = op_id` and the
        // remaining tail. Verbs that need structured arguments parse the
        // tail themselves — for tracker.create that means `args.title`.
        let trimmed = slash.args.trim_start();
        let (op, tail) = match trimmed.find(' ') {
            Some(idx) => (&trimmed[..idx], &trimmed[idx + 1..]),
            None => (trimmed, ""),
        };

        let mut args: HashMap<String, serde_json::Value> = HashMap::new();
        if !tail.is_empty() {
            args.insert(
                "title".to_string(),
                serde_json::Value::String(tail.to_string()),
            );
        }
        let params = ActionParams {
            op_id: if op.is_empty() { None } else { Some(op.to_string()) },
            item_id: None,
            args,
        };
        match self.registry.dispatch_action(&plugin_id, params).await {
            Ok(_) => {
                self.status_msg = None;
            }
            Err(e) => {
                self.push_chat_error(format!("/{} failed: {e}", slash.prefix));
            }
        }
    }

    /// Key handling for the Chatting state.
    async fn handle_key_chatting(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.close_event_stream().await;
                self.chat_messages.clear();
                self.chat_input.clear();
                self.state = AppState::Idle;
            }
            // Ctrl+N — create a new thread and make it active.
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let Some(plugin_id) = self.chat_plugin_id.clone() else { return };
                let mut args = HashMap::new();
                args.insert(
                    "title".to_string(),
                    serde_json::Value::String("New Conversation".to_string()),
                );
                let params = ActionParams {
                    op_id: None,
                    item_id: Some("new_thread".to_string()),
                    args,
                };
                match self.registry.dispatch_action(&plugin_id, params).await {
                    Ok(result) => {
                        if let Some(id) = result
                            .data
                            .as_ref()
                            .and_then(|d| d.get("id"))
                            .and_then(|v| v.as_str())
                        {
                            self.chat_active_thread_id = Some(id.to_string());
                        }
                        self.chat_messages.clear();
                        self.refresh_chat_threads().await;
                        self.chat_show_threads = true;
                        // Move cursor to the newly created thread (first entry).
                        self.chat_thread_cursor = 0;
                        self.status_msg = None;
                    }
                    Err(e) => {
                        self.status_msg = Some(format!("new_thread: {e}"));
                    }
                }
            }
            // Ctrl+T — toggle the thread list column.
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.chat_show_threads {
                    self.chat_show_threads = false;
                } else {
                    self.refresh_chat_threads().await;
                    self.chat_show_threads = true;
                }
            }
            // Enter — parse the input as a slash command first; fall through
            // to the default `ask` dispatch when parseSlash returns None.
            KeyCode::Enter => {
                let raw = self.chat_input.clone();
                let trimmed = raw.trim().to_string();
                if trimmed.is_empty() {
                    return;
                }

                if let Some(slash) = parse_slash(&raw) {
                    self.chat_input.clear();
                    self.dispatch_slash(slash).await;
                    return;
                }

                // Non-slash free-text → existing ask dispatch path.
                let text = trimmed;
                let Some(plugin_id) = self.chat_plugin_id.clone() else { return };
                self.chat_input.clear();

                let mut args: HashMap<String, serde_json::Value> = HashMap::new();
                args.insert("text".to_string(), serde_json::Value::String(text));
                if let Some(tid) = &self.chat_active_thread_id {
                    args.insert(
                        "thread_id".to_string(),
                        serde_json::Value::String(tid.clone()),
                    );
                }
                let params = ActionParams {
                    op_id: None,
                    item_id: Some("ask".to_string()),
                    args,
                };
                match self.registry.dispatch_action(&plugin_id, params).await {
                    Ok(_) => {
                        // Streaming response arrives via DataUpdated events.
                        self.status_msg = None;
                    }
                    Err(e) => {
                        self.status_msg = Some(format!("ask: {e}"));
                    }
                }
            }
            // Up/Down — navigate the thread list; load the newly-selected
            // thread's persisted messages so the right pane reflects its
            // history rather than staying blank until a new ask arrives.
            KeyCode::Up => {
                if self.chat_show_threads && self.chat_thread_cursor > 0 {
                    self.chat_thread_cursor -= 1;
                    self.chat_active_thread_id = self
                        .chat_threads
                        .get(self.chat_thread_cursor)
                        .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                        .map(|s| s.to_string());
                    if let Some(tid) = self.chat_active_thread_id.clone() {
                        self.load_chat_messages(&tid).await;
                    } else {
                        self.chat_messages.clear();
                    }
                }
            }
            KeyCode::Down => {
                if self.chat_show_threads
                    && self.chat_thread_cursor + 1 < self.chat_threads.len()
                {
                    self.chat_thread_cursor += 1;
                    self.chat_active_thread_id = self
                        .chat_threads
                        .get(self.chat_thread_cursor)
                        .and_then(|t| t.get("id").and_then(|v| v.as_str()))
                        .map(|s| s.to_string());
                    if let Some(tid) = self.chat_active_thread_id.clone() {
                        self.load_chat_messages(&tid).await;
                    } else {
                        self.chat_messages.clear();
                    }
                }
            }
            // Ctrl+E — toggle expanded view of the latest ToolCallBeat.
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.toggle_latest_tool_beat();
            }
            KeyCode::Char(c) => {
                self.chat_input.push(c);
            }
            KeyCode::Backspace => {
                self.chat_input.pop();
            }
            _ => {}
        }
    }

    // ── Key handling ──────────────────────────────────────────────────────────

    pub async fn handle_key(&mut self, key: KeyEvent) {
        // Palette states are handled first; they consume all input.
        match self.state {
            AppState::PaletteOpen => {
                self.handle_key_palette_open(key).await;
                return;
            }
            AppState::PaletteCollectingArg => {
                self.handle_key_collecting_arg(key).await;
                return;
            }
            // Chatting state consumes all input before any global shortcuts.
            AppState::Chatting => {
                self.handle_key_chatting(key).await;
                return;
            }
            _ => {}
        }

        // `/` from browsing states enters the chat view.
        if key.code == KeyCode::Char('/')
            && matches!(
                self.state,
                AppState::Idle | AppState::Searching | AppState::SelectedCapability
            )
        {
            self.enter_chat().await;
            return;
        }

        // Ctrl+K opens the palette when a plugin UI is loaded.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('k') {
            if matches!(self.state, AppState::UILoaded | AppState::ActionDispatched) {
                self.palette_ctx = PaletteCtx::default();
                self.state = AppState::PaletteOpen;
            }
            return;
        }

        let state = self.state.clone();
        match state {
            AppState::Idle | AppState::Searching | AppState::SelectedCapability => {
                self.handle_key_browsing(key).await;
            }
            AppState::UILoaded => {
                self.handle_key_ui_loaded(key).await;
            }
            AppState::ActionDispatched => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
                    self.action_result = None;
                    self.state = AppState::UILoaded;
                }
            }
            AppState::Chatting => unreachable!(),
            AppState::PaletteOpen | AppState::PaletteCollectingArg => unreachable!(),
        }
    }

    /// Key handling for Idle / Searching / SelectedCapability.
    async fn handle_key_browsing(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if !self.query.is_empty() {
                    self.query.clear();
                    self.refresh_results().await;
                } else {
                    self.should_quit = true;
                }
            }
            KeyCode::Char(c) => {
                self.query.push(c);
                self.refresh_results().await;
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.refresh_results().await;
            }
            KeyCode::Up => {
                if !self.results.is_empty() {
                    self.selected_index = self.selected_index.saturating_sub(1);
                    self.state = AppState::SelectedCapability;
                }
            }
            KeyCode::Down => {
                if !self.results.is_empty() && self.selected_index + 1 < self.results.len() {
                    self.selected_index += 1;
                    self.state = AppState::SelectedCapability;
                }
            }
            KeyCode::Enter => {
                if let Some((plugin_id, _)) = self.results.get(self.selected_index).cloned() {
                    match self.registry.render_ui(&plugin_id).await {
                        Ok(components) => {
                            self.components = components;
                            self.refresh_code_diff_cursor();
                            self.code_diff_rejected.clear();
                            self.code_diff_accepted.clear();
                            self.state = AppState::UILoaded;
                            self.status_msg = None;
                            self.table_cursor = 0;
                            self.detail_open = false;
                            self.cached_issues.clear();
                            self.open_event_stream(&plugin_id).await;
                        }
                        Err(e) => {
                            self.status_msg = Some(format!("render error: {e}"));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Key handling for UILoaded.
    ///
    /// Enter is intentionally not bound here — use Ctrl+K to open the palette
    /// and select an action explicitly.  `r` triggers a manual refresh.
    async fn handle_key_ui_loaded(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self.detail_open {
                    self.detail_open = false;
                } else {
                    self.close_event_stream().await;
                    self.state = AppState::SelectedCapability;
                    self.components.clear();
                    self.table_cursor = 0;
                    self.cached_issues.clear();
                }
            }
            KeyCode::Up => {
                self.table_cursor = self.table_cursor.saturating_sub(1);
            }
            KeyCode::Down => {
                let count = self.table_row_count();
                if count > 0 && self.table_cursor + 1 < count {
                    self.table_cursor += 1;
                }
            }
            KeyCode::Tab | KeyCode::Right => {
                if !self.detail_open {
                    self.refresh_issue_cache().await;
                    self.detail_open = true;
                }
            }
            // `j` / `k` navigate a highlighted CodeDiff's hunks. Only active
            // when a CodeDiff is on-screen — otherwise the keys are ignored.
            KeyCode::Char('j') => {
                if let Some((_, hidx, hunks)) = self.code_diff_hunks_at_cursor() {
                    if hidx + 1 < hunks.len() {
                        if let Some((_, h)) = self.code_diff_cursor.as_mut().map(|c| (c.0, &mut c.1)) {
                            *h += 1;
                        }
                    }
                }
            }
            KeyCode::Char('k') => {
                if let Some((c, _)) = self.code_diff_cursor {
                    if let Some(cursor) = self.code_diff_cursor.as_mut() {
                        if cursor.1 > 0 {
                            cursor.1 -= 1;
                        } else {
                            // Keep cursor on the first hunk of component c.
                            cursor.0 = c;
                        }
                    }
                }
            }
            // `a` accepts the highlighted hunk. Scoped: no-op when no hunk
            // is highlighted.
            KeyCode::Char('a') => {
                if let Some((cidx, hidx, hunks)) = self.code_diff_hunks_at_cursor() {
                    if let Some(hunk) = hunks.get(hidx).cloned() {
                        self.accept_code_diff_hunk(cidx, &hunk).await;
                    }
                }
            }
            KeyCode::Char('r') => {
                // Scope `r` to hunk-reject when a CodeDiff is highlighted.
                // Otherwise fall through to the legacy refresh behavior.
                if let Some((_cidx, hidx, hunks)) = self.code_diff_hunks_at_cursor() {
                    if let Some(hunk) = hunks.get(hidx) {
                        self.code_diff_rejected.insert(hunk.id.clone());
                    }
                } else if let Some((plugin_id, _)) = self.results.get(self.selected_index).cloned() {
                    match self.registry.render_ui(&plugin_id).await {
                        Ok(components) => {
                            self.components = components;
                            self.refresh_code_diff_cursor();
                            self.status_msg = None;
                        }
                        Err(e) => {
                            self.status_msg = Some(format!("render error: {e}"));
                        }
                    }
                    if self.detail_open {
                        self.refresh_issue_cache().await;
                    }
                }
            }
            _ => {}
        }
    }

    /// Dispatch `code_diff.accept_hunk` for the currently highlighted hunk.
    async fn accept_code_diff_hunk(&mut self, component_idx: usize, hunk: &dust_core::Hunk) {
        let Some(path) = self.components.get(component_idx).and_then(|c| {
            if let Component::CodeDiff { path, .. } = c {
                Some(path.clone())
            } else {
                None
            }
        }) else {
            return;
        };
        let Some((plugin_id, _)) = self.results.get(self.selected_index).cloned() else {
            return;
        };
        let mut args = HashMap::new();
        args.insert("path".to_string(), serde_json::Value::String(path));
        args.insert(
            "hunk_id".to_string(),
            serde_json::Value::String(hunk.id.clone()),
        );
        let params = ActionParams {
            op_id: Some(component_renderer::CODE_DIFF_ACCEPT_OP.to_string()),
            item_id: Some(hunk.id.clone()),
            args,
        };
        match self.registry.dispatch_action(&plugin_id, params).await {
            Ok(_result) => {
                self.code_diff_accepted.insert(hunk.id.clone());
                self.status_msg = None;
            }
            Err(e) => {
                self.status_msg = Some(format!("accept hunk: {e}"));
            }
        }
    }

    /// Key handling while the op list is visible (PaletteOpen).
    async fn handle_key_palette_open(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::UILoaded;
            }
            KeyCode::Up => {
                if self.palette_ctx.cursor > 0 {
                    self.palette_ctx.cursor -= 1;
                }
            }
            KeyCode::Down => {
                if self.palette_ctx.cursor + 1 < TRACKER_OPS.len() {
                    self.palette_ctx.cursor += 1;
                }
            }
            KeyCode::Enter => {
                let op_idx = self.palette_ctx.cursor;
                self.palette_ctx.op_idx = op_idx;
                self.palette_ctx.arg_idx = 0;
                self.palette_ctx.buffer.clear();
                self.palette_ctx.collected.clear();

                if TRACKER_OPS[op_idx].2.is_empty() {
                    self.dispatch_op().await;
                } else {
                    self.state = AppState::PaletteCollectingArg;
                }
            }
            _ => {}
        }
    }

    /// Key handling while collecting an argument (PaletteCollectingArg).
    async fn handle_key_collecting_arg(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::UILoaded;
            }
            KeyCode::Char(c) => {
                self.palette_ctx.buffer.push(c);
            }
            KeyCode::Backspace => {
                self.palette_ctx.buffer.pop();
            }
            KeyCode::Enter => {
                let val = self.palette_ctx.buffer.trim().to_string();
                self.palette_ctx.collected.push(val);
                self.palette_ctx.buffer.clear();
                self.palette_ctx.arg_idx += 1;

                let arg_count = TRACKER_OPS[self.palette_ctx.op_idx].2.len();
                if self.palette_ctx.arg_idx >= arg_count {
                    self.dispatch_op().await;
                }
                // else: stay in PaletteCollectingArg — next prompt will render
            }
            _ => {}
        }
    }

    /// Build `ActionParams` from the palette context and call `dispatch_action`.
    ///
    /// On success, immediately re-renders the UI so the change is visible
    /// without waiting for an event.
    async fn dispatch_op(&mut self) {
        let op_idx = self.palette_ctx.op_idx;
        let (op_id, _, arg_prompts) = TRACKER_OPS[op_idx];
        let collected = self.palette_ctx.collected.clone();

        let mut args: HashMap<String, serde_json::Value> = HashMap::new();
        let mut item_id: Option<String> = None;

        for (i, prompt) in arg_prompts.iter().enumerate() {
            let val = collected.get(i).cloned().unwrap_or_default();
            // Derive the arg key from the first word of the prompt (lower-cased).
            let key = prompt
                .to_lowercase()
                .split_whitespace()
                .next()
                .unwrap_or("arg")
                .to_string();
            if key == "issue" {
                item_id = Some(val.clone());
                args.insert("item_id".to_string(), serde_json::Value::String(val));
            } else {
                args.insert(key, serde_json::Value::String(val));
            }
        }

        let params = ActionParams {
            op_id: Some(op_id.to_string()),
            item_id,
            args,
        };

        // Return to UILoaded before dispatching so Esc during the await still works.
        self.state = AppState::UILoaded;

        if let Some((plugin_id, _)) = self.results.get(self.selected_index).cloned() {
            match self.registry.dispatch_action(&plugin_id, params).await {
                Ok(result) => {
                    // Immediate optimistic re-render for instant feedback.
                    if let Ok(components) = self.registry.render_ui(&plugin_id).await {
                        self.components = components;
                        self.refresh_code_diff_cursor();
                    }
                    self.action_result = Some(result);
                    self.state = AppState::ActionDispatched;
                    self.status_msg = None;
                }
                Err(e) => {
                    self.status_msg = Some(format!("action error: {e}"));
                }
            }
        }
    }

    // ── Helpers used by the UI layer ──────────────────────────────────────────

    /// Format a manifest's capabilities as a short label string.
    pub fn capability_labels(manifest: &PluginManifest) -> String {
        manifest
            .capabilities
            .iter()
            .map(|c| match c {
                Capability::Widget { .. } => "Widget".to_string(),
                Capability::Command { prefix } => format!("cmd:{}", prefix),
                Capability::Scheduler => "Scheduler".to_string(),
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Expose the op list to the UI layer.
    pub fn tracker_ops() -> &'static [(&'static str, &'static str, &'static [&'static str])] {
        TRACKER_OPS
    }
}
