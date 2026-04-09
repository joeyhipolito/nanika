//! Tauri 2 host for the dust desktop app.
//!
//! Bridges dust-registry to Tauri commands consumed by the React frontend.
//!
//! - `search_capabilities` — fuzzy-search, returns Vec<CapabilityMatch>
//! - `get_plugin_info`     — manifest + liveness for one plugin
//! - `render_ui`           — first Component from registry render
//! - `dispatch_action`     — execute a plugin action

use dust_core::Component;
use dust_registry::Registry;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::{Manager, State};

// ---------------------------------------------------------------------------
// Wire types — serialised to JSON for the React frontend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityMatch {
    pub plugin_id: String,
    pub plugin_name: String,
    pub capability: CapabilityInfo,
    pub score: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfoManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub capabilities: Vec<CapabilityInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub manifest: PluginInfoManifest,
    pub healthy: bool,
}

// ---------------------------------------------------------------------------
// Managed state
// ---------------------------------------------------------------------------

pub struct AppState {
    pub registry: Arc<Registry>,
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command]
async fn search_capabilities(
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<CapabilityMatch>, String> {
    let results = state.registry.search_with_ids(&query).await;
    let mut matches = Vec::new();
    for (plugin_id, manifest) in results {
        for cap in &manifest.capabilities {
            let (cap_id, cap_name, cap_desc, keywords) = capability_fields(cap);
            matches.push(CapabilityMatch {
                plugin_id: plugin_id.clone(),
                plugin_name: manifest.name.clone(),
                capability: CapabilityInfo { id: cap_id, name: cap_name, description: cap_desc, keywords },
                score: 1.0,
            });
        }
    }
    Ok(matches)
}

#[tauri::command]
async fn get_plugin_info(
    plugin_id: String,
    state: State<'_, AppState>,
) -> Result<PluginInfo, String> {
    let results = state.registry.search_with_ids("").await;
    let (_, manifest) = results
        .into_iter()
        .find(|(id, _)| *id == plugin_id)
        .ok_or_else(|| format!("plugin not found: {plugin_id}"))?;

    let capabilities = manifest.capabilities.iter().map(|c| {
        let (id, name, desc, keywords) = capability_fields(c);
        CapabilityInfo { id, name, description: desc, keywords }
    }).collect();

    Ok(PluginInfo {
        manifest: PluginInfoManifest {
            id: plugin_id,
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            capabilities,
        },
        healthy: true,
    })
}

#[tauri::command]
async fn render_ui(
    plugin_id: String,
    _capability_id: String,
    _query: String,
    state: State<'_, AppState>,
) -> Result<Option<Component>, String> {
    let components = state
        .registry
        .render_ui(&plugin_id)
        .await
        .map_err(|e| e.to_string())?;
    Ok(components.into_iter().next())
}

#[tauri::command]
async fn dispatch_action(
    plugin_id: String,
    _capability_id: String,
    _action_id: String,
    params: serde_json::Value,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    state
        .registry
        .dispatch_action(&plugin_id, params)
        .await
        .map(|r| r.success)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn capability_fields(cap: &dust_core::Capability) -> (String, String, String, Vec<String>) {
    match cap {
        dust_core::Capability::Command { prefix } => (
            format!("cmd:{prefix}"),
            format!("Command: {prefix}"),
            format!("Invoke '{prefix}' commands"),
            vec![prefix.clone(), "command".into()],
        ),
        dust_core::Capability::Widget { refresh_secs } => (
            "widget".into(),
            "Widget".into(),
            if *refresh_secs > 0 { format!("Auto-refreshes every {refresh_secs}s") } else { "Static widget".into() },
            vec!["widget".into()],
        ),
        dust_core::Capability::Scheduler => (
            "scheduler".into(),
            "Scheduler".into(),
            "Handles background scheduled jobs".into(),
            vec!["scheduler".into(), "schedule".into()],
        ),
    }
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let registry = tauri::async_runtime::block_on(async {
        Registry::new().await.unwrap_or_else(|e| {
            eprintln!("[dust] registry init failed: {e}");
            panic!("cannot start without registry: {e}");
        })
    });

    tauri::Builder::default()
        .setup(|app| {
            use tauri_plugin_global_shortcut::{
                Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
            };
            let handle = app.handle().clone();
            app.handle().plugin(
                tauri_plugin_global_shortcut::Builder::new()
                    .with_handler(move |_app, _shortcut, event| {
                        if event.state() != ShortcutState::Pressed { return; }
                        let Some(window) = handle.get_webview_window("main") else { return; };
                        let visible = window.is_visible().unwrap_or(false);
                        if visible { let _ = window.hide(); }
                        else { let _ = window.show(); let _ = window.set_focus(); }
                    })
                    .build(),
            )?;
            let alt_space = Shortcut::new(Some(Modifiers::ALT), Code::Space);
            app.handle().global_shortcut().register(alt_space)?;
            Ok(())
        })
        .manage(AppState { registry: Arc::new(registry) })
        .invoke_handler(tauri::generate_handler![
            search_capabilities,
            get_plugin_info,
            render_ui,
            dispatch_action,
        ])
        .run(tauri::generate_context!())
        .expect("error while running dust desktop");
}
