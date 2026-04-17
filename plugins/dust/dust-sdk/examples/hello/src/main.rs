//! dust-hello — reference plugin for the Nanika dust protocol SDK.
//!
//! Demonstrates how to implement [`DustPlugin`] from `dust-sdk`, including:
//!
//! - **manifest** — declares plugin name, version, and capabilities
//! - **render** — builds a widget with a greeting and a live counter
//! - **action** — increments the counter on demand
//! - **subscribe** (overridden) — returns an empty event snapshot, showing
//!   how to replace the default `ReplayGap` stub with a custom source
//!
//! The SDK's [`run_with_dir()`] helper handles the Unix socket lifecycle,
//! the `ready` / `host_info` handshake, heartbeat echo, and graceful shutdown
//! automatically — no custom connection loop required for these features.
//!
//! # SDK default behaviour for optional methods
//!
//! | Method | Default |
//! |--------|---------|
//! | `subscribe` | `-33007 replay_gap` (no event history) |
//! | `unsubscribe` | `Ok(())` |
//! | `cancel` | `already_complete: true` |
//! | `refresh` | `Ok(())` |
//!
//! This example overrides `subscribe` to return an empty `Vec` instead of a
//! replay-gap error, demonstrating the override pattern. A production plugin
//! would serve events from a [`dust_core::events::EventRing`].

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use dust_sdk::{
    run_with_dir, ActionParams, ActionResult, Capability, Component, DustPlugin, EventEnvelope,
    KVPair, ListItem, PluginManifest, SubscribeError, TextStyle,
};

// ── Plugin identity ─────────────────────────────────────────────────────────

const PLUGIN_ID: &str = "dust-hello";
const PLUGIN_NAME: &str = "Hello Plugin";
const PLUGIN_VERSION: &str = "0.1.0";

// ── Plugin state ─────────────────────────────────────────────────────────────

struct HelloPlugin {
    counter: AtomicU64,
}

impl HelloPlugin {
    fn new() -> Self {
        Self { counter: AtomicU64::new(0) }
    }
}

// ── DustPlugin implementation ────────────────────────────────────────────────

#[async_trait]
impl DustPlugin for HelloPlugin {
    fn plugin_id(&self) -> &str {
        PLUGIN_ID
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: PLUGIN_NAME.into(),
            version: PLUGIN_VERSION.into(),
            description: "Reference dust plugin: greeting widget with a counter.".into(),
            capabilities: vec![
                Capability::Widget { refresh_secs: 60 },
                Capability::Command { prefix: "hello".into() },
            ],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        let counter = self.counter.load(Ordering::Relaxed);
        vec![
            Component::Text {
                content: "Hello from Dust!".into(),
                style: TextStyle { bold: true, ..Default::default() },
            },
            Component::KeyValue {
                pairs: vec![
                    KVPair::new("Plugin", PLUGIN_NAME),
                    KVPair::new("Version", PLUGIN_VERSION),
                    KVPair::new("Counter", counter.to_string()),
                ],
            },
            Component::List {
                title: Some("Actions".into()),
                items: vec![ListItem::new("increment", "Increment counter")],
            },
        ]
    }

    async fn action(&self, params: ActionParams) -> ActionResult {
        let item_id = params.item_id.as_deref().unwrap_or("");
        if item_id != "increment" {
            return ActionResult::err(format!("unknown action: {item_id}"));
        }
        let new_val = self.counter.fetch_add(1, Ordering::Relaxed) + 1;
        ActionResult::ok_with(format!("Counter incremented to {new_val}"))
    }

    /// Override: return an empty event ring instead of a replay-gap error.
    ///
    /// The default [`DustPlugin::subscribe`] returns `-33007 replay_gap`,
    /// which is correct for plugins that keep no event history. This override
    /// returns `Ok(vec![])`, signalling that the plugin is live but has no
    /// historical events to replay from `since_sequence`.
    ///
    /// A production plugin that maintains an [`dust_core::events::EventRing`]
    /// would call `ring.subscribe(since_sequence)` here and map the
    /// `ReplayGap` error to a [`SubscribeError`].
    async fn subscribe(
        &self,
        _since_sequence: u64,
    ) -> Result<Vec<EventEnvelope>, SubscribeError> {
        // Empty ring — no historical events to replay.
        Ok(vec![])
    }
}

// ── Entry point ───────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let plugin = Arc::new(HelloPlugin::new());

    // Resolve socket directory mirroring the registry convention:
    // $XDG_RUNTIME_DIR/nanika/plugins/  or  ~/.alluka/run/plugins/
    let socket_dir = if let Some(xdg) =
        std::env::var_os("XDG_RUNTIME_DIR").filter(|v| !v.is_empty())
    {
        std::path::PathBuf::from(xdg).join("nanika").join("plugins")
    } else {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotFound, "HOME not set")
            })?
            .join(".alluka")
            .join("run")
            .join("plugins")
    };

    eprintln!("{PLUGIN_ID}: starting, socket dir: {}", socket_dir.display());
    run_with_dir(plugin, &socket_dir).await
}
