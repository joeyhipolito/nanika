use crate::commands;
use crate::models::Issue;
use rusqlite::Connection;
use serde_json::json;

/// Handle the query subcommand tree for plugin integration.
pub fn query_cmd(args: &[String], json_output: bool) -> Result<String, String> {
    if args.is_empty() {
        return Err("query requires a subcommand: status, items, or action".to_string());
    }

    match args[0].as_str() {
        "status" => query_status(json_output),
        "items" => query_items(json_output),
        "actions" => query_actions(json_output),
        "action" => {
            if args.len() < 2 {
                return Err("action requires a subcommand (e.g. next, ready)".to_string());
            }
            query_action(&args[1], json_output)
        }
        _ => Err(format!("unknown query subcommand: {}", args[0])),
    }
}

fn get_connection() -> Result<Connection, String> {
    let db_path = crate::db::default_db_path();
    crate::db::open(&db_path).map_err(|e| e.to_string())
}

/// query status --json
/// Returns plugin status and issue count.
fn query_status(_json_output: bool) -> Result<String, String> {
    let conn = get_connection()?;

    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
        .map_err(|e| format!("counting issues: {}", e))?;

    let response = json!({
        "ok": true,
        "count": count,
        "type": "tracker-status"
    });

    serde_json::to_string_pretty(&response).map_err(|e| format!("serializing response: {}", e))
}

/// query items --json
/// Returns all issues in the uniform envelope format.
#[derive(serde::Serialize)]
struct ItemsResponse {
    items: Vec<Issue>,
    count: usize,
}

fn query_items(_json_output: bool) -> Result<String, String> {
    let conn = get_connection()?;

    let issues = commands::list(&conn, None, None)
        .map_err(|e| format!("listing issues: {}", e))?;

    let count = issues.len();
    let response = ItemsResponse { items: issues, count };

    serde_json::to_string_pretty(&response).map_err(|e| format!("serializing response: {}", e))
}

/// query actions --json
/// Returns list of available actions with descriptions.
#[derive(serde::Serialize)]
struct Action {
    name: String,
    command: String,
    description: String,
}

#[derive(serde::Serialize)]
struct ActionsResponse {
    actions: Vec<Action>,
}

fn query_actions(_json_output: bool) -> Result<String, String> {
    let actions = vec![
        Action {
            name: "next".to_string(),
            command: "tracker query action next".to_string(),
            description: "Show the highest-priority ready issue".to_string(),
        },
        Action {
            name: "ready".to_string(),
            command: "tracker query action ready".to_string(),
            description: "List open issues with no blocking issues".to_string(),
        },
        Action {
            name: "tree".to_string(),
            command: "tracker query action tree".to_string(),
            description: "Show issues as a parent-child tree".to_string(),
        },
    ];

    let response = ActionsResponse { actions };
    serde_json::to_string_pretty(&response).map_err(|e| format!("serializing response: {}", e))
}

/// query action <name> --json
/// Triggers a specific action and returns results.
fn query_action(action_name: &str, _json_output: bool) -> Result<String, String> {
    let conn = get_connection()?;

    match action_name {
        "next" => {
            let issue = commands::next(&conn)
                .map_err(|e| format!("getting next issue: {}", e))?;
            let response = json!({"action": "next", "issue": issue});
            serde_json::to_string_pretty(&response).map_err(|e| format!("serializing response: {}", e))
        }
        "ready" => {
            let issues = commands::ready(&conn)
                .map_err(|e| format!("getting ready issues: {}", e))?;
            let count = issues.len();
            let response = json!({
                "action": "ready",
                "items": issues,
                "count": count
            });
            serde_json::to_string_pretty(&response).map_err(|e| format!("serializing response: {}", e))
        }
        "tree" => {
            // tree prints to stdout, so we'll return a JSON response instead
            let issues = commands::list(&conn, None, None)
                .map_err(|e| format!("listing issues: {}", e))?;
            let response = json!({
                "action": "tree",
                "items": issues
            });
            serde_json::to_string_pretty(&response).map_err(|e| format!("serializing response: {}", e))
        }
        _ => Err(format!("unknown action: {}", action_name)),
    }
}
