use rusqlite::Connection;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::process::Command;

#[derive(Debug, Deserialize)]
struct LinearViewJson {
    #[allow(dead_code)]
    identifier: String,
    title: String,
    description: Option<String>,
    state: LinearState,
}

#[derive(Debug, Deserialize)]
struct LinearState {
    name: String,
}

/// Generate a deterministic tracker ID from a Linear issue ID.
/// Uses SHA256("linear:" + linear_id) so NAN-XX always maps to the same trk-XXXX.
pub fn tracker_id_for_linear(linear_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"linear:");
    hasher.update(linear_id.as_bytes());
    let result = hasher.finalize();
    let hex = hex::encode(&result[..2]);
    format!("trk-{}", hex.to_uppercase())
}

/// Map Linear state name to tracker status.
fn map_state(state_name: &str) -> &'static str {
    match state_name.to_lowercase().as_str() {
        "in progress" | "started" => "in-progress",
        "done" | "completed" => "done",
        "canceled" | "cancelled" => "cancelled",
        _ => "open", // backlog, todo, triage, unstarted
    }
}

/// Map Linear priority icon (from `linear issue list` output) to P0-P3.
/// Icons: ⚠⚠⚠ = Urgent, ▄▆█ = High, ▄▆  = Medium, --- = No priority
fn map_priority(icon: &str) -> Option<&'static str> {
    // Compare by unicode content (icon is 3 chars)
    let chars: Vec<char> = icon.chars().collect();
    if chars.len() < 3 {
        return None;
    }
    match chars[0] {
        '⚠' => Some("P0"),                  // Urgent: ⚠⚠⚠
        '▄' if chars[2] == '█' => Some("P1"), // High: ▄▆█
        '▄' => Some("P2"),                   // Medium: ▄▆ (space)
        _ => None,                            // No priority: ---
    }
}

/// Fetch all issue IDs and priorities from `linear issue list` output.
/// Returns map of linear_id -> priority (None = no priority).
fn fetch_list_priorities(team: &str) -> Result<HashMap<String, Option<String>>, String> {
    let output = Command::new("linear")
        .args([
            "issue",
            "list",
            "--team",
            team,
            "--all-states",
            "-A",
            "--sort",
            "priority",
            "--limit",
            "0",
            "--no-pager",
        ])
        .output()
        .map_err(|e| format!("running 'linear issue list': {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("linear issue list failed: {}", stderr.trim()));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut result = HashMap::new();

    for line in raw.lines() {
        // Strip ANSI escape codes
        let clean = strip_ansi(line);
        let clean = clean.trim();

        // Skip header line (starts with ◌) or empty lines
        if clean.is_empty() || clean.starts_with('◌') {
            continue;
        }

        // First 3 chars: priority icon
        let chars: Vec<char> = clean.chars().collect();
        if chars.len() < 5 {
            continue;
        }
        let icon: String = chars[..3].iter().collect();

        // After icon + space, find "NAN-\d+"
        let after_icon = clean[icon.len()..].trim_start();
        if !after_icon.starts_with("NAN-") {
            continue;
        }

        let id_end = after_icon
            .chars()
            .position(|c| c == ' ')
            .unwrap_or(after_icon.len());
        let linear_id = &after_icon[..id_end];

        // Validate: NAN-digits
        if linear_id.len() <= 4 || !linear_id[4..].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }

        let priority = map_priority(&icon).map(|s| s.to_string());
        result.insert(linear_id.to_string(), priority);
    }

    Ok(result)
}

/// Strip ANSI escape codes from a string.
fn strip_ansi(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_escape = false;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\x1b' && i + 1 < bytes.len() && bytes[i + 1] == b'[' {
            in_escape = true;
            i += 2;
        } else if in_escape {
            if bytes[i].is_ascii_alphabetic() {
                in_escape = false;
            }
            i += 1;
        } else {
            // Safe: push single byte if ASCII, else collect full char
            if bytes[i] < 128 {
                result.push(bytes[i] as char);
            } else {
                // Multi-byte UTF-8: find char boundary and push
                if let Some(c) = s[i..].chars().next() {
                    result.push(c);
                    i += c.len_utf8() - 1; // -1 because i += 1 below
                }
            }
            i += 1;
        }
    }
    result
}

/// Fetch full details for a single Linear issue via `linear issue view --json`.
fn fetch_view(linear_id: &str) -> Result<LinearViewJson, String> {
    let output = Command::new("linear")
        .args(["issue", "view", linear_id, "--json"])
        .output()
        .map_err(|e| format!("running 'linear issue view': {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("linear issue view {} failed: {}", linear_id, stderr.trim()));
    }

    let json_str = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str::<LinearViewJson>(&json_str)
        .map_err(|e| format!("parsing JSON for {}: {}", linear_id, e))
}

/// Check if a Linear issue is already imported by searching for its label.
fn is_already_imported(conn: &Connection, linear_id: &str) -> bool {
    let label = format!("linear:{}", linear_id);
    conn.query_row(
        "SELECT COUNT(*) FROM issues WHERE labels LIKE ?1",
        rusqlite::params![format!("%{}%", label)],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
        > 0
}

/// Import all issues from a Linear team into the tracker DB.
pub fn run(conn: &Connection, team: &str) -> Result<(), String> {
    println!("Fetching issues from Linear team {}...", team);

    let priorities = fetch_list_priorities(team)?;
    if priorities.is_empty() {
        println!("No issues found for team {}.", team);
        return Ok(());
    }

    let total = priorities.len();
    println!("Found {} issues. Importing (skipping already-imported)...\n", total);

    let mut imported = 0;
    let mut skipped = 0;
    let mut failed = 0;

    // Sort by NAN number for deterministic ordering
    let mut ids: Vec<String> = priorities.keys().cloned().collect();
    ids.sort_by_key(|id| {
        id.trim_start_matches("NAN-")
            .parse::<u64>()
            .unwrap_or(u64::MAX)
    });

    for linear_id in &ids {
        if is_already_imported(conn, linear_id) {
            let tracker_id = tracker_id_for_linear(linear_id);
            println!("  SKIP  {} -> {} (already imported)", linear_id, tracker_id);
            skipped += 1;
            continue;
        }

        match fetch_view(linear_id) {
            Ok(view) => {
                let tracker_id = tracker_id_for_linear(linear_id);
                let status = map_state(&view.state.name);
                let label = format!("linear:{}", linear_id);
                let priority = priorities.get(linear_id).and_then(|p| p.as_deref());
                let now = chrono::Utc::now().to_rfc3339();

                conn.execute(
                    "INSERT OR IGNORE INTO issues \
                     (id, title, description, status, priority, labels, assignee, parent_id, created_at, updated_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, ?7, ?7)",
                    rusqlite::params![
                        tracker_id,
                        view.title,
                        view.description,
                        status,
                        priority,
                        label,
                        now,
                    ],
                )
                .map_err(|e| format!("inserting {}: {}", linear_id, e))?;

                println!(
                    "  OK    {} -> {} [{}] ({})",
                    linear_id, tracker_id, status, view.state.name
                );
                imported += 1;
            }
            Err(e) => {
                eprintln!("  FAIL  {}: {}", linear_id, e);
                failed += 1;
            }
        }
    }

    println!(
        "\nDone: {} imported, {} skipped, {} failed (of {} total)",
        imported, skipped, failed, total
    );
    Ok(())
}
