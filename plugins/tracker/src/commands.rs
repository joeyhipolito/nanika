use crate::{id, models::{Comment, Issue, Link}};
use chrono::Utc;
use comfy_table::{Table, presets::UTF8_FULL};
use rusqlite::{Connection, params};
use std::collections::{HashMap, HashSet};

// ── Issue CRUD ────────────────────────────────────────────────────────────────

pub fn create(
    conn: &Connection,
    title: &str,
    priority: Option<&str>,
    description: Option<&str>,
    assignee: Option<&str>,
    labels: Option<&str>,
    parent_id: Option<&str>,
) -> rusqlite::Result<Issue> {
    let now = Utc::now().to_rfc3339();
    let issue_id = id::generate(title, &now);

    // Get next seq_id
    let next_seq_id: i64 = conn.query_row(
        "SELECT COALESCE(MAX(seq_id), 0) + 1 FROM issues",
        [],
        |row| row.get(0),
    )?;

    // Resolve parent_id if provided in TRK-N format
    let resolved_parent_id = if let Some(pid) = parent_id {
        if pid.starts_with("TRK-") {
            if let Ok(seq_num) = pid[4..].parse::<i64>() {
                conn.query_row(
                    "SELECT id FROM issues WHERE seq_id = ?1",
                    rusqlite::params![seq_num],
                    |row| row.get::<_, String>(0),
                ).ok()
            } else {
                Some(pid.to_string())
            }
        } else {
            Some(pid.to_string())
        }
    } else {
        None
    };

    conn.execute(
        "INSERT INTO issues (id, seq_id, title, description, status, priority, labels, assignee, parent_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            issue_id,
            next_seq_id,
            title,
            description,
            "open",
            priority,
            labels,
            assignee,
            resolved_parent_id,
            now,
            now,
        ],
    )?;

    get(conn, &issue_id)
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Issue> {
    // Check if id is TRK-N format (numeric) and convert to seq_id
    if id.starts_with("TRK-") {
        if let Ok(seq_num) = id[4..].parse::<i64>() {
            return conn.query_row(
                "SELECT id, seq_id, title, description, status, priority, labels, assignee, parent_id, created_at, updated_at
                 FROM issues WHERE seq_id = ?1",
                params![seq_num],
                map_issue,
            );
        }
    }

    // Otherwise, treat as hash ID (trk-XXXX)
    conn.query_row(
        "SELECT id, seq_id, title, description, status, priority, labels, assignee, parent_id, created_at, updated_at
         FROM issues WHERE id = ?1",
        params![id],
        map_issue,
    )
}

pub fn list(
    conn: &Connection,
    status: Option<&str>,
    priority: Option<&str>,
) -> rusqlite::Result<Vec<Issue>> {
    let mut parts = Vec::new();
    let mut query = String::from(
        "SELECT id, seq_id, title, description, status, priority, labels, assignee, parent_id, created_at, updated_at FROM issues WHERE ",
    );

    if status.is_some() {
        parts.push("status = ?1");
    }
    if priority.is_some() {
        parts.push(if status.is_some() { "priority = ?2" } else { "priority = ?1" });
    }

    if parts.is_empty() {
        query.push_str("1=1");
    } else {
        query.push_str(&parts.join(" AND "));
    }
    query.push_str(" ORDER BY seq_id DESC");

    let mut stmt = conn.prepare(&query)?;

    let rows = match (status, priority) {
        (Some(s), Some(p)) => stmt.query_map(params![s, p], map_issue)?,
        (Some(s), None) => stmt.query_map(params![s], map_issue)?,
        (None, Some(p)) => stmt.query_map(params![p], map_issue)?,
        (None, None) => stmt.query_map([], map_issue)?,
    };

    rows.collect()
}

fn map_issue(row: &rusqlite::Row) -> rusqlite::Result<Issue> {
    Ok(Issue {
        id: row.get(0)?,
        seq_id: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        status: row.get(4)?,
        priority: row.get(5)?,
        labels: row.get(6)?,
        assignee: row.get(7)?,
        parent_id: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

pub fn update(
    conn: &Connection,
    id: &str,
    title: Option<&str>,
    status: Option<&str>,
    priority: Option<&str>,
    description: Option<&str>,
    assignee: Option<&str>,
    labels: Option<&str>,
) -> rusqlite::Result<Issue> {
    let now = Utc::now().to_rfc3339();
    let current = get(conn, id)?;
    let resolved_id = &current.id;

    conn.execute(
        "UPDATE issues SET title=?1, status=?2, priority=?3, description=?4, assignee=?5, labels=?6, updated_at=?7 WHERE id=?8",
        params![
            title.unwrap_or(&current.title),
            status.unwrap_or(&current.status),
            priority.or(current.priority.as_deref()),
            description.or(current.description.as_deref()),
            assignee.or(current.assignee.as_deref()),
            labels.or(current.labels.as_deref()),
            now,
            resolved_id,
        ],
    )?;

    get(conn, id)
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    // Resolve TRK-N format to hash ID
    let resolved_id = if id.starts_with("TRK-") {
        if let Ok(seq_num) = id[4..].parse::<i64>() {
            conn.query_row(
                "SELECT id FROM issues WHERE seq_id = ?1",
                rusqlite::params![seq_num],
                |row| row.get::<_, String>(0),
            )?
        } else {
            id.to_string()
        }
    } else {
        id.to_string()
    };

    let affected = conn.execute("DELETE FROM issues WHERE id = ?1", params![&resolved_id])?;
    if affected == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

// ── Links ─────────────────────────────────────────────────────────────────────

/// Valid link types.
const VALID_LINK_TYPES: &[&str] = &["blocks", "relates_to", "supersedes", "duplicates"];

pub fn link(
    conn: &Connection,
    from_id: &str,
    to_id: &str,
    link_type: &str,
) -> Result<Link, String> {
    if !VALID_LINK_TYPES.contains(&link_type) {
        return Err(format!(
            "invalid link type {:?}; must be one of: {}",
            link_type,
            VALID_LINK_TYPES.join(", ")
        ));
    }

    // Verify and resolve both issues
    let from_issue = get(conn, from_id).map_err(|_| format!("issue {from_id} not found"))?;
    let to_issue = get(conn, to_id).map_err(|_| format!("issue {to_id} not found"))?;

    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO links (from_id, to_id, link_type, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![&from_issue.id, &to_issue.id, link_type, now],
    )
    .map_err(|e| e.to_string())?;

    let row_id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT id, from_id, to_id, link_type, created_at FROM links WHERE id = ?1",
        params![row_id],
        map_link,
    )
    .map_err(|e| e.to_string())
}

pub fn unlink(
    conn: &Connection,
    from_id: &str,
    to_id: &str,
    link_type: &str,
) -> Result<(), String> {
    // Verify and resolve both issues
    let from_issue = get(conn, from_id).map_err(|_| format!("issue {from_id} not found"))?;
    let to_issue = get(conn, to_id).map_err(|_| format!("issue {to_id} not found"))?;

    let affected = conn
        .execute(
            "DELETE FROM links WHERE from_id = ?1 AND to_id = ?2 AND link_type = ?3",
            params![&from_issue.id, &to_issue.id, link_type],
        )
        .map_err(|e| e.to_string())?;

    if affected == 0 {
        return Err(format!(
            "no {link_type} link found from {from_id} to {to_id}"
        ));
    }
    Ok(())
}

fn map_link(row: &rusqlite::Row) -> rusqlite::Result<Link> {
    Ok(Link {
        id: row.get(0)?,
        from_id: row.get(1)?,
        to_id: row.get(2)?,
        link_type: row.get(3)?,
        created_at: row.get(4)?,
    })
}

pub fn get_links(conn: &Connection, issue_id: &str) -> rusqlite::Result<Vec<Link>> {
    let mut stmt = conn.prepare(
        "SELECT id, from_id, to_id, link_type, created_at FROM links
         WHERE from_id = ?1 OR to_id = ?1
         ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![issue_id], map_link)?.collect();
    rows
}

// ── Ready / Next ──────────────────────────────────────────────────────────────

/// Returns open issues that have no unresolved transitive blockers.
/// An issue X is blocked if any issue Y in the transitive closure of
/// X's "blocks" predecessors has status != done/cancelled.
pub fn ready(conn: &Connection) -> rusqlite::Result<Vec<Issue>> {
    let open_issues = list(conn, Some("open"), None)?;
    if open_issues.is_empty() {
        return Ok(vec![]);
    }

    // Build blocked_by map: blocked_by[X] = list of issues that directly block X.
    let mut stmt = conn.prepare(
        "SELECT from_id, to_id FROM links WHERE link_type = 'blocks'",
    )?;
    let all_blocks: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut blocked_by: HashMap<String, Vec<String>> = HashMap::new();
    for (from_id, to_id) in &all_blocks {
        blocked_by
            .entry(to_id.clone())
            .or_default()
            .push(from_id.clone());
    }

    // Collect status of every issue.
    let mut stmt = conn.prepare("SELECT id, status FROM issues")?;
    let statuses: HashMap<String, String> = stmt
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?
        .into_iter()
        .collect();

    let mut result = Vec::new();
    for issue in open_issues {
        let mut visited = HashSet::new();
        if !has_unresolved_blocker(&issue.id, &blocked_by, &statuses, &mut visited) {
            result.push(issue);
        }
    }
    Ok(result)
}

/// Recursive DFS: returns true if `issue_id` has any transitive blocker that
/// is not done/cancelled. `visited` prevents infinite loops on cycles.
fn has_unresolved_blocker(
    issue_id: &str,
    blocked_by: &HashMap<String, Vec<String>>,
    statuses: &HashMap<String, String>,
    visited: &mut HashSet<String>,
) -> bool {
    if !visited.insert(issue_id.to_string()) {
        return false; // already explored this node
    }

    let blockers = match blocked_by.get(issue_id) {
        Some(b) => b,
        None => return false,
    };

    for blocker_id in blockers {
        let status = statuses
            .get(blocker_id.as_str())
            .map(|s| s.as_str())
            .unwrap_or("open");

        if status != "done" && status != "cancelled" {
            return true;
        }
        // Even a done blocker may have its own unresolved predecessors.
        if has_unresolved_blocker(blocker_id, blocked_by, statuses, visited) {
            return true;
        }
    }
    false
}

/// Returns the single highest-priority ready issue, or None if none exist.
pub fn next(conn: &Connection) -> rusqlite::Result<Option<Issue>> {
    let mut candidates = ready(conn)?;
    if candidates.is_empty() {
        return Ok(None);
    }
    // Sort by priority (P0 first), then by created_at ascending (oldest first).
    candidates.sort_by(|a, b| {
        priority_rank(a.priority.as_deref()).cmp(&priority_rank(b.priority.as_deref()))
            .then(a.created_at.cmp(&b.created_at))
    });
    Ok(candidates.into_iter().next())
}

fn priority_rank(p: Option<&str>) -> u8 {
    match p {
        Some("P0") => 0,
        Some("P1") => 1,
        Some("P2") => 2,
        Some("P3") => 3,
        _ => 4,
    }
}

// ── Tree ──────────────────────────────────────────────────────────────────────

/// Prints all issues as a parent–child tree using parent_id.
pub fn tree(conn: &Connection) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, seq_id, title, description, status, priority, labels, assignee, parent_id, created_at, updated_at
         FROM issues ORDER BY created_at",
    )?;
    let all: Vec<Issue> = stmt.query_map([], map_issue)?.collect::<rusqlite::Result<Vec<_>>>()?;

    if all.is_empty() {
        println!("No issues found.");
        return Ok(());
    }

    // Build children map.
    let mut children: HashMap<Option<String>, Vec<&Issue>> = HashMap::new();
    for issue in &all {
        children
            .entry(issue.parent_id.clone())
            .or_default()
            .push(issue);
    }

    // Print roots (no parent) recursively.
    if let Some(roots) = children.get(&None) {
        let roots: Vec<&Issue> = roots.to_vec();
        for root in roots {
            print_tree_node(root, &children, 0);
        }
    }

    Ok(())
}

fn print_tree_node(
    issue: &Issue,
    children: &HashMap<Option<String>, Vec<&Issue>>,
    depth: usize,
) {
    let indent = "  ".repeat(depth);
    let priority = issue.priority.as_deref().unwrap_or("-");
    let display_id = issue.seq_id
        .map(|n| format!("TRK-{}", n))
        .unwrap_or_else(|| issue.id.clone());
    println!(
        "{}[{}] {} ({}) [{}]",
        indent, display_id, issue.title, issue.status, priority
    );

    if let Some(kids) = children.get(&Some(issue.id.clone())) {
        let kids: Vec<&Issue> = kids.to_vec();
        for child in kids {
            print_tree_node(child, children, depth + 1);
        }
    }
}

// ── Comments ─────────────────────────────────────────────────────────────────

pub fn comment(
    conn: &Connection,
    issue_id: &str,
    body: &str,
    author: Option<&str>,
) -> Result<Comment, String> {
    let issue = get(conn, issue_id).map_err(|_| format!("issue {issue_id} not found"))?;

    let now = Utc::now().to_rfc3339();
    conn.execute(
        "INSERT INTO comments (issue_id, body, author, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![&issue.id, body, author, now],
    )
    .map_err(|e| e.to_string())?;

    let row_id = conn.last_insert_rowid();
    conn.query_row(
        "SELECT id, issue_id, body, author, created_at FROM comments WHERE id = ?1",
        params![row_id],
        |row| {
            Ok(Comment {
                id: row.get(0)?,
                issue_id: row.get(1)?,
                body: row.get(2)?,
                author: row.get(3)?,
                created_at: row.get(4)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

pub fn get_comments(conn: &Connection, issue_id: &str) -> rusqlite::Result<Vec<Comment>> {
    let mut stmt = conn.prepare(
        "SELECT id, issue_id, body, author, created_at FROM comments
         WHERE issue_id = ?1 ORDER BY created_at",
    )?;
    let rows = stmt.query_map(params![issue_id], |row| {
        Ok(Comment {
            id: row.get(0)?,
            issue_id: row.get(1)?,
            body: row.get(2)?,
            author: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?.collect();
    rows
}

// ── Doctor ────────────────────────────────────────────────────────────────────

pub fn doctor(
    conn: &Connection,
    db_path: &std::path::Path,
    json_output: bool,
) -> Result<String, String> {
    use serde_json::json;

    // Check DB is readable and migrations are applied.
    let db_ok;
    let db_version: i64;
    let issue_count: i64;

    match conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get(0),
    ) {
        Ok(v) => {
            db_ok = true;
            db_version = v;
        }
        Err(_) => {
            db_ok = false;
            db_version = 0;
        }
    }

    issue_count = conn
        .query_row("SELECT COUNT(*) FROM issues", [], |row| row.get(0))
        .unwrap_or(0);

    // Check linear CLI availability.
    let linear_ok = std::process::Command::new("linear")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    let checks = vec![
        json!({"name": "database", "ok": db_ok, "detail": db_path.display().to_string()}),
        json!({"name": "schema_version", "ok": db_version >= 2, "detail": format!("v{}", db_version)}),
        json!({"name": "linear_cli", "ok": linear_ok, "detail": if linear_ok { "available" } else { "not found — import-linear will not work" }}),
    ];

    let all_ok = checks.iter().all(|c| c["ok"].as_bool().unwrap_or(false));

    if json_output {
        let out = json!({
            "ok": all_ok,
            "issue_count": issue_count,
            "db_path": db_path.display().to_string(),
            "checks": checks,
        });
        Ok(out.to_string())
    } else {
        let mut lines = vec![format!("tracker doctor — {}", if all_ok { "healthy" } else { "issues found" })];
        lines.push(format!("  DB:            {} ({})", if db_ok { "OK" } else { "FAIL" }, db_path.display()));
        lines.push(format!("  Schema:        v{}", db_version));
        lines.push(format!("  Issues:        {}", issue_count));
        lines.push(format!("  Linear CLI:    {}", if linear_ok { "OK" } else { "not found" }));
        Ok(lines.join("\n"))
    }
}

// ── Search ────────────────────────────────────────────────────────────────────

/// Full-text search across title and description using LIKE.
pub fn search(conn: &Connection, query: &str) -> rusqlite::Result<Vec<Issue>> {
    let pattern = format!("%{}%", query);
    let mut stmt = conn.prepare(
        "SELECT id, seq_id, title, description, status, priority, labels, assignee, parent_id, created_at, updated_at
         FROM issues
         WHERE title LIKE ?1 OR description LIKE ?1
         ORDER BY seq_id DESC",
    )?;
    let rows = stmt.query_map(params![pattern], map_issue)?.collect();
    rows
}

// ── Display helpers ───────────────────────────────────────────────────────────

pub fn print_issue(conn: &Connection, issue: &Issue) {
    if let Some(seq_id) = issue.seq_id {
        println!("ID:          TRK-{}", seq_id);
        println!("Hash ID:     {}", issue.id);
    } else {
        println!("ID:          {}", issue.id);
    }
    println!("Title:       {}", issue.title);
    println!("Status:      {}", issue.status);
    println!("Priority:    {}", issue.priority.as_deref().unwrap_or("-"));
    println!("Assignee:    {}", issue.assignee.as_deref().unwrap_or("-"));
    println!("Labels:      {}", issue.labels.as_deref().unwrap_or("-"));
    println!("Parent:      {}", issue.parent_id.as_deref().unwrap_or("-"));
    println!("Created:     {}", issue.created_at);
    println!("Updated:     {}", issue.updated_at);
    if let Some(desc) = &issue.description {
        println!("Description:");
        println!("  {}", desc);
    }

    // Links
    if let Ok(links) = get_links(conn, &issue.id) {
        if !links.is_empty() {
            println!("Links:");
            for l in &links {
                if l.from_id == issue.id {
                    println!("  {} --[{}]--> {}", l.from_id, l.link_type, l.to_id);
                } else {
                    println!("  {} --[{}]--> {} (incoming)", l.from_id, l.link_type, l.to_id);
                }
            }
        }
    }

    // Comments
    if let Ok(comments) = get_comments(conn, &issue.id) {
        if !comments.is_empty() {
            println!("Comments:");
            for c in &comments {
                let author = c.author.as_deref().unwrap_or("anonymous");
                println!("  [{}] {}: {}", c.created_at, author, c.body);
            }
        }
    }
}

pub fn print_issues_table(issues: &[Issue]) {
    if issues.is_empty() {
        println!("No issues found.");
        return;
    }

    let mut table = Table::new();
    table.load_preset(UTF8_FULL);
    table.set_header(["ID", "Title", "Status", "Priority", "Assignee"]);

    for issue in issues {
        let display_id = issue.seq_id
            .map(|n| format!("TRK-{}", n))
            .unwrap_or_else(|| issue.id.clone());
        table.add_row([
            &display_id,
            &issue.title,
            &issue.status,
            issue.priority.as_deref().unwrap_or("-"),
            issue.assignee.as_deref().unwrap_or("-"),
        ]);
    }

    println!("{table}");
}
