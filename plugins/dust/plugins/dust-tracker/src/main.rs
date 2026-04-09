use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use dust_sdk::{ActionResult, Capability, Component, DustPlugin, ListItem, PluginManifest};
use serde::Deserialize;
use tokio::process::Command;

const TRACKER_TIMEOUT: Duration = Duration::from_secs(10);

struct TrackerPlugin;

#[derive(Deserialize)]
struct TrackerStatus {
    count: u64,
    ok: bool,
}

#[derive(Deserialize)]
struct TrackerItem {
    id: String,
    seq_id: Option<u64>,
    title: String,
    status: String,
    priority: Option<String>,
    assignee: Option<String>,
}

#[derive(Deserialize)]
struct TrackerItemsResponse {
    items: Vec<TrackerItem>,
}

async fn run_tracker(args: &[&str]) -> Result<String, String> {
    let out = tokio::time::timeout(
        TRACKER_TIMEOUT,
        Command::new("tracker").args(args).output(),
    )
    .await
    .map_err(|_| "tracker CLI timed out after 10 s".to_string())?
    .map_err(|e| format!("failed to run tracker: {}", e))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

#[async_trait]
impl DustPlugin for TrackerPlugin {
    fn plugin_id(&self) -> &str {
        "dust-tracker"
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "Tracker".into(),
            version: "0.1.0".into(),
            description: "Local issue tracker — open issues, ready queue, and next up. Keywords: tracker, issues, tasks, backlog, next, ready, tree.".into(),
            capabilities: vec![Capability::Command {
                prefix: "tracker".into(),
            }],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        let mut components = Vec::new();

        // Status header
        let status_text = match run_tracker(&["query", "status", "--json"]).await {
            Ok(raw) => match serde_json::from_str::<TrackerStatus>(&raw) {
                Ok(s) if s.ok => format!("Tracker — {} total issues", s.count),
                Ok(_) => "Tracker — status unavailable".into(),
                Err(_) => "Tracker — failed to parse status".into(),
            },
            Err(e) => format!("Tracker — error: {}", e),
        };
        components.push(Component::Text {
            content: status_text,
            style: Default::default(),
        });

        // Open items list (first 20, sorted by priority from API)
        let items_result = run_tracker(&["query", "items", "--json"]).await;
        match items_result {
            Ok(raw) => match serde_json::from_str::<TrackerItemsResponse>(&raw) {
                Ok(resp) => {
                    let open: Vec<&TrackerItem> = resp
                        .items
                        .iter()
                        .filter(|i| i.status == "open")
                        .take(20)
                        .collect();

                    if open.is_empty() {
                        components.push(Component::Text {
                            content: "No open issues.".into(),
                            style: Default::default(),
                        });
                    } else {
                        let list_items: Vec<ListItem> = open
                            .iter()
                            .map(|i| {
                                let seq = i
                                    .seq_id
                                    .map(|n| format!("TRK-{}", n))
                                    .unwrap_or_else(|| i.id.clone());
                                let label = format!(
                                    "[{}] {} {}",
                                    i.priority.as_deref().unwrap_or("--"),
                                    seq,
                                    i.title
                                );
                                let mut item = ListItem::new(&i.id, label);
                                item.description = i
                                    .assignee
                                    .as_deref()
                                    .map(|a| format!("assignee: {}", a));
                                item
                            })
                            .collect();

                        components.push(Component::List {
                            title: Some("Open Issues".into()),
                            items: list_items,
                        });
                    }
                }
                Err(e) => {
                    components.push(Component::Text {
                        content: format!("Failed to parse items: {}", e),
                        style: Default::default(),
                    });
                }
            },
            Err(e) => {
                components.push(Component::Text {
                    content: format!("Failed to fetch items: {}", e),
                    style: Default::default(),
                });
            }
        }

        components
    }

    async fn action(&self, params: serde_json::Value) -> ActionResult {
        let subcommand = params
            .get("subcommand")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match subcommand {
            "next" => match run_tracker(&["next"]).await {
                Ok(output) => ActionResult::ok_with(output),
                Err(e) => ActionResult::err(format!("tracker next failed: {}", e)),
            },
            "ready" => match run_tracker(&["ready"]).await {
                Ok(output) => ActionResult::ok_with(output),
                Err(e) => ActionResult::err(format!("tracker ready failed: {}", e)),
            },
            "tree" => match run_tracker(&["tree"]).await {
                Ok(output) => ActionResult::ok_with(output),
                Err(e) => ActionResult::err(format!("tracker tree failed: {}", e)),
            },
            "" => ActionResult::err(
                "missing subcommand - supported: next, ready, tree",
            ),
            other => ActionResult::err(format!(
                "unknown subcommand '{}' - supported: next, ready, tree",
                other
            )),
        }
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dust_sdk::run(Arc::new(TrackerPlugin)).await
}
