use std::sync::Arc;

use async_trait::async_trait;
use dust_sdk::{ActionResult, Capability, Component, DustPlugin, ListItem, PluginManifest};
use serde::Deserialize;
use tokio::process::Command;

struct SchedulerPlugin;

// ── Scheduler status JSON types ───────────────────────────────────────────────

#[derive(Debug, Deserialize, Default)]
struct SchedulerStatus {
    #[serde(rename = "daemon_running", default)]
    running: bool,
    #[serde(default)]
    pid: Option<u32>,
    #[serde(rename = "next_run_at", default)]
    next_run: Option<String>,
    #[serde(rename = "job_count", default)]
    jobs_total: Option<u32>,
    #[serde(rename = "enabled_count", default)]
    jobs_enabled: Option<u32>,
    #[serde(default)]
    jobs_disabled: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct SchedulerJob {
    name: String,
    #[serde(rename = "schedule", default)]
    cron: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    last_exit_code: Option<i32>,
    #[serde(default)]
    #[allow(dead_code)]
    command: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SchedulerItems {
    #[serde(default)]
    jobs: Vec<SchedulerJob>,
    #[serde(default)]
    items: Vec<SchedulerJob>,
}

// ── CLI helpers ───────────────────────────────────────────────────────────────

async fn run_scheduler(args: &[&str]) -> Option<serde_json::Value> {
    let output = Command::new("scheduler")
        .args(args)
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    serde_json::from_slice(&output.stdout).ok()
}

// ── DustPlugin impl ───────────────────────────────────────────────────────────

#[async_trait]
impl DustPlugin for SchedulerPlugin {
    fn plugin_id(&self) -> &str {
        "dust-scheduler"
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "Scheduler".into(),
            version: "0.1.0".into(),
            description: "Daemon status, next run, and cron job list. Keywords: scheduler, cron, jobs, daemon.".into(),
            capabilities: vec![
                Capability::Widget { refresh_secs: 30 },
                Capability::Command { prefix: "scheduler".into() },
            ],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        let (status_val, items_val) = tokio::join!(
            run_scheduler(&["query", "status", "--json"]),
            run_scheduler(&["query", "items", "--json"]),
        );

        let mut components: Vec<Component> = Vec::new();

        // ── Status text ───────────────────────────────────────────────────────
        let status: SchedulerStatus = status_val
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();

        let daemon_line = if status.running {
            match status.pid {
                Some(pid) => format!("Daemon   running  (pid {})", pid),
                None => "Daemon   running".into(),
            }
        } else {
            "Daemon   stopped".into()
        };

        let next_run_line = match &status.next_run {
            Some(t) => format!("Next     {}", t),
            None => "Next     —".into(),
        };

        let total = status.jobs_total.unwrap_or(0);
        let enabled = status.jobs_enabled.unwrap_or(0);
        let disabled = status.jobs_disabled.unwrap_or(0);
        let counts_line = format!("Jobs     {}  ({} enabled, {} disabled)", total, enabled, disabled);

        let summary = format!("{}\n{}\n{}", daemon_line, next_run_line, counts_line);
        components.push(Component::Text {
            content: summary,
            style: Default::default(),
        });

        // ── Job list ──────────────────────────────────────────────────────────
        let jobs: Vec<SchedulerJob> = items_val
            .and_then(|v| {
                // Try {jobs:[...]} then {items:[...]} then top-level array
                if let Ok(parsed) = serde_json::from_value::<SchedulerItems>(v.clone()) {
                    let j = if !parsed.jobs.is_empty() { parsed.jobs } else { parsed.items };
                    if !j.is_empty() { return Some(j); }
                }
                serde_json::from_value::<Vec<SchedulerJob>>(v).ok()
            })
            .unwrap_or_default();

        if !jobs.is_empty() {
            let items: Vec<ListItem> = jobs
                .iter()
                .map(|job| {
                    let enabled_badge = match job.enabled {
                        Some(true) | None => "●",
                        Some(false) => "○",
                    };
                    let cron = job.cron.as_deref().unwrap_or("—");
                    let exit = match job.last_exit_code {
                        Some(0) => " ok".into(),
                        Some(c) => format!(" exit {}", c),
                        None => String::new(),
                    };
                    let label = format!("{} {}  {}{}",
                        enabled_badge,
                        job.name,
                        cron,
                        exit,
                    );
                    ListItem::new(job.name.clone(), label)
                })
                .collect();

            components.push(Component::List {
                title: Some("Jobs".into()),
                items,
            });
        }

        if components.is_empty() {
            components.push(Component::Text {
                content: "scheduler not available".into(),
                style: Default::default(),
            });
        }

        components
    }

    async fn action(&self, _params: serde_json::Value) -> ActionResult {
        ActionResult::ok()
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dust_sdk::run(Arc::new(SchedulerPlugin)).await
}
