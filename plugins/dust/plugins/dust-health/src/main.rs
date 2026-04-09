use std::sync::Arc;

use async_trait::async_trait;
use dust_sdk::{ActionResult, Capability, Component, DustPlugin, ListItem, PluginManifest, TextStyle};
use rusqlite::{Connection, OpenFlags};
use tokio::process::Command;

struct HealthPlugin;

/// Run a CLI command and return its stdout. On failure returns the error message.
async fn run_cmd(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .map_err(|e| format!("spawn {program}: {e}"))?;

    if output.status.success() {
        String::from_utf8(output.stdout).map_err(|e| format!("utf8: {e}"))
    } else {
        Err(format!(
            "exit {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

struct ShuStatus {
    score: u64,
    critical_count: u64,
    daemon_running: bool,
    active_findings: u64,
}

async fn fetch_shu_status() -> Result<ShuStatus, String> {
    let stdout = run_cmd("shu", &["query", "status", "--json"]).await?;
    let v: serde_json::Value =
        serde_json::from_str(&stdout).map_err(|e| format!("parse shu status: {e}"))?;

    Ok(ShuStatus {
        score: v["score"].as_u64().unwrap_or(0),
        critical_count: v["critical_count"].as_u64().unwrap_or(0),
        daemon_running: v["daemon_running"].as_bool().unwrap_or(false),
        active_findings: v["active_findings"].as_u64().unwrap_or(0),
    })
}

struct OrchestratorHealth {
    total_learnings: u64,
    injected: u64,
    avg_compliance_rate: u64,
}

async fn fetch_orchestrator_health() -> Result<OrchestratorHealth, String> {
    // `orchestrator doctor --json` is not a valid subcommand; fall back to
    // `orchestrator stats` (text output) and parse the key numbers.
    let stdout = run_cmd("orchestrator", &["doctor", "--json"]).await;

    // First attempt: the spec'd command; second attempt: stats text.
    let text = match stdout {
        Ok(s) => {
            // If it's JSON try to pull numbers from it
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                return Ok(OrchestratorHealth {
                    total_learnings: v["total"].as_u64().unwrap_or(0),
                    injected: v["injected"].as_u64().unwrap_or(0),
                    avg_compliance_rate: v["avg_compliance_rate"].as_u64().unwrap_or(0),
                });
            }
            s
        }
        Err(_) => run_cmd("orchestrator", &["stats"]).await?,
    };

    // Parse text like:
    //   learnings: 2040 total, 2018 with embeddings
    //   compliance:  914 injected, avg rate 34%
    let mut total = 0u64;
    let mut injected = 0u64;
    let mut avg_rate = 0u64;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("learnings:") {
            // "learnings: 2040 total, ..."
            if let Some(n) = line
                .split_whitespace()
                .nth(1)
                .and_then(|s| s.parse::<u64>().ok())
            {
                total = n;
            }
        } else if line.starts_with("compliance:") {
            // "compliance:  914 injected, avg rate 34%"
            let parts: Vec<&str> = line.split_whitespace().collect();
            if let Some(n) = parts.get(1).and_then(|s| s.parse::<u64>().ok()) {
                injected = n;
            }
            // "avg rate 34%" — find the token ending with '%'
            if let Some(pct) = parts
                .iter()
                .find(|s| s.ends_with('%'))
                .and_then(|s| s.trim_end_matches('%').parse::<u64>().ok())
            {
                avg_rate = pct;
            }
        }
    }

    Ok(OrchestratorHealth {
        total_learnings: total,
        injected,
        avg_compliance_rate: avg_rate,
    })
}

struct LearningStats {
    total: i64,
    high_quality: i64,
    avg_quality: f64,
    dev_count: i64,
}

fn read_learning_stats() -> Result<LearningStats, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let db_path = format!("{home}/.alluka/learnings.db");

    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("open {db_path}: {e}"))?;

    conn.query_row(
        "SELECT \
            COUNT(*), \
            SUM(CASE WHEN quality_score >= 0.7 THEN 1 ELSE 0 END), \
            COALESCE(AVG(quality_score), 0.0), \
            SUM(CASE WHEN domain = 'dev' THEN 1 ELSE 0 END) \
         FROM learnings WHERE archived = 0",
        [],
        |row| {
            Ok(LearningStats {
                total: row.get(0)?,
                high_quality: row.get(1)?,
                avg_quality: row.get(2)?,
                dev_count: row.get(3)?,
            })
        },
    )
    .map_err(|e| format!("query learning stats: {e}"))
}

fn score_icon(score: u64) -> &'static str {
    match score {
        90..=100 => "●",
        70..=89 => "◐",
        _ => "○",
    }
}

#[async_trait]
impl DustPlugin for HealthPlugin {
    fn plugin_id(&self) -> &str {
        "dust-health"
    }

    async fn manifest(&self) -> PluginManifest {
        PluginManifest {
            name: "Health".into(),
            version: "0.1.0".into(),
            description: "Nanika system health: orchestrator stats, shu score, and learnings quality. Keywords: health, doctor, shu, orchestrator, learnings.".into(),
            capabilities: vec![
                Capability::Widget { refresh_secs: 120 },
                Capability::Command {
                    prefix: "health".into(),
                },
            ],
            icon: None,
        }
    }

    async fn render(&self) -> Vec<Component> {
        // Fan out all three data sources concurrently.
        let (shu_result, orch_result, db_result) = tokio::join!(
            fetch_shu_status(),
            fetch_orchestrator_health(),
            tokio::task::spawn_blocking(read_learning_stats),
        );

        let db_result = db_result.unwrap_or_else(|e| Err(format!("spawn: {e}")));

        let mut components: Vec<Component> = Vec::new();

        // ── Header ──────────────────────────────────────────────────────────
        components.push(Component::Text {
            content: "Nanika Health".into(),
            style: TextStyle {
                bold: true,
                ..Default::default()
            },
        });

        // ── Shu status ───────────────────────────────────────────────────────
        let shu_items = match shu_result {
            Ok(s) => vec![
                ListItem::new(
                    "shu-score",
                    format!(
                        "{} Score          {}",
                        score_icon(s.score),
                        s.score
                    ),
                ),
                ListItem::new(
                    "shu-findings",
                    format!("  Active findings  {}", s.active_findings),
                ),
                ListItem::new(
                    "shu-critical",
                    format!("  Critical          {}", s.critical_count),
                ),
                ListItem::new(
                    "shu-daemon",
                    format!(
                        "  Daemon            {}",
                        if s.daemon_running { "running" } else { "stopped" }
                    ),
                ),
            ],
            Err(e) => vec![ListItem::new("shu-err", format!("shu unavailable: {e}"))],
        };

        components.push(Component::List {
            title: Some("Shu Status".into()),
            items: shu_items,
        });

        // ── Orchestrator health ──────────────────────────────────────────────
        let orch_items = match orch_result {
            Ok(o) => vec![
                ListItem::new(
                    "orch-learnings",
                    format!("  Learnings (orch)  {}", o.total_learnings),
                ),
                ListItem::new(
                    "orch-injected",
                    format!("  Injected          {}", o.injected),
                ),
                ListItem::new(
                    "orch-compliance",
                    format!("  Avg compliance    {}%", o.avg_compliance_rate),
                ),
            ],
            Err(e) => vec![ListItem::new(
                "orch-err",
                format!("orchestrator unavailable: {e}"),
            )],
        };

        components.push(Component::List {
            title: Some("Orchestrator".into()),
            items: orch_items,
        });

        // ── Learnings DB stats ───────────────────────────────────────────────
        let db_items = match db_result {
            Ok(s) => vec![
                ListItem::new("db-total", format!("  Active            {}", s.total)),
                ListItem::new("db-dev", format!("  Dev domain        {}", s.dev_count)),
                ListItem::new(
                    "db-hq",
                    format!("  High quality      {}", s.high_quality),
                ),
                ListItem::new(
                    "db-avg",
                    format!("  Avg quality       {:.2}", s.avg_quality),
                ),
            ],
            Err(e) => vec![ListItem::new(
                "db-err",
                format!("learnings.db unavailable: {e}"),
            )],
        };

        components.push(Component::List {
            title: Some("Learnings DB".into()),
            items: db_items,
        });

        components
    }

    async fn action(&self, _params: serde_json::Value) -> ActionResult {
        ActionResult::ok()
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    dust_sdk::run(Arc::new(HealthPlugin)).await
}
