use std::sync::Arc;

use async_trait::async_trait;
use dust_sdk::{ActionResult, Capability, Component, DustPlugin, ListItem, PluginManifest};
use sysinfo::System;

struct SystemPlugin;

#[async_trait]
impl DustPlugin for SystemPlugin {
    fn plugin_id(&self) -> &str {
        "system-plugin"
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "System".into(),
            version: "0.1.0".into(),
            description: "CPU, memory, and uptime at a glance. Keywords: system, cpu, memory, uptime.".into(),
            capabilities: vec![Capability::Command {
                prefix: "system".into(),
            }],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        let mut sys = System::new_all();
        sys.refresh_all();

        let cpu = sys.global_cpu_usage();
        let total_mem = sys.total_memory();
        let used_mem = sys.used_memory();
        let mem_pct = if total_mem > 0 {
            (used_mem as f64 / total_mem as f64 * 100.0) as u64
        } else {
            0
        };
        let uptime = System::uptime();
        let uptime_str = format!(
            "{}h {}m",
            uptime / 3600,
            (uptime % 3600) / 60
        );

        vec![Component::List {
            title: Some("System Info".into()),
            items: vec![
                ListItem::new("cpu", &format!("CPU Usage     {:.1}%", cpu)),
                ListItem::new(
                    "memory",
                    &format!(
                        "Memory        {} / {} MB  ({}%)",
                        used_mem / 1024 / 1024,
                        total_mem / 1024 / 1024,
                        mem_pct
                    ),
                ),
                ListItem::new("uptime", &format!("Uptime        {}", uptime_str)),
            ],
        }]
    }

    async fn action(&self, _params: serde_json::Value) -> ActionResult {
        ActionResult::ok()
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dust_sdk::run(Arc::new(SystemPlugin)).await
}
