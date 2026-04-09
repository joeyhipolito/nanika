//! Application state and key-event dispatch for the dust dashboard.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use dust_core::{ActionResult, Capability, Component, PluginManifest};
use dust_registry::Registry;

// ── State machine ─────────────────────────────────────────────────────────────

/// TUI state machine.
///
/// ```text
/// Idle ──(type)──► Searching ──(↑/↓)──► SelectedCapability ──(Enter)──► UILoaded ──(Enter)──► ActionDispatched
///   ▲                  │                         │                           │                        │
///   └──────(Esc)───────┘─────────────────────────┘                          └──────────(Esc)──────────┘
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
    /// Whether the Ctrl+K action palette overlay is visible.
    pub action_palette_open: bool,
    /// Transient error or status message shown at the bottom of the detail pane.
    pub status_msg: Option<String>,
    pub should_quit: bool,
    registry: Arc<Registry>,
}

impl App {
    pub fn new(registry: Arc<Registry>) -> Self {
        Self {
            state: AppState::Idle,
            query: String::new(),
            results: Vec::new(),
            selected_index: 0,
            components: Vec::new(),
            action_result: None,
            action_palette_open: false,
            status_msg: None,
            should_quit: false,
            registry,
        }
    }

    // ── Registry interaction ──────────────────────────────────────────────────

    /// Re-query the registry and update `results`.
    ///
    /// Preserves the cursor on the same plugin by ID so that hot-plug events
    /// (new plugin added, registry reordered) don't shift the selection.
    pub async fn refresh_results(&mut self) {
        // Remember which plugin is currently selected so we can re-find it.
        let selected_id = self
            .results
            .get(self.selected_index)
            .map(|(id, _)| id.clone());

        self.results = self.registry.search_with_ids(&self.query).await;

        // Re-anchor cursor to the same plugin ID, falling back to clamping.
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
        // Reconcile on-disk state with the registry (catches missed Remove events).
        let _ = self.registry.sync().await;

        // Only refresh the list — don't clear components or reset state.
        let selected_id = self
            .results
            .get(self.selected_index)
            .map(|(id, _)| id.clone());

        let new_results = self.registry.search_with_ids(&self.query).await;

        // Skip if nothing changed.
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

    // ── Key handling ──────────────────────────────────────────────────────────

    pub async fn handle_key(&mut self, key: KeyEvent) {
        // Ctrl+K → toggle action palette regardless of current state.
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('k') {
            self.action_palette_open = !self.action_palette_open;
            return;
        }

        // While the palette is open, only Esc closes it; other keys are swallowed.
        if self.action_palette_open {
            if key.code == KeyCode::Esc {
                self.action_palette_open = false;
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
                            self.state = AppState::UILoaded;
                            self.status_msg = None;
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
    async fn handle_key_ui_loaded(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state = AppState::SelectedCapability;
                self.components.clear();
            }
            KeyCode::Enter => {
                if let Some((plugin_id, _)) = self.results.get(self.selected_index).cloned() {
                    match self
                        .registry
                        .dispatch_action(&plugin_id, serde_json::Value::Null)
                        .await
                    {
                        Ok(result) => {
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
            _ => {}
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
}
