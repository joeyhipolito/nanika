//! Tauri commands for commit and PR information.
//!
//! Uses git and gh CLI for fetching metadata about commits and PRs.

use serde::{Deserialize, Serialize};
use std::process::Command;

// ── Wire types ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitSummary {
    pub hash: String,
    pub author: String,
    pub date: String,
    pub message: String,
    pub stats: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequestMetadata {
    pub number: u32,
    pub title: String,
    pub state: String,
    pub author: String,
    pub created_at: String,
    pub url: String,
}

// ── Commands ──────────────────────────────────────────────────────────────────

/// Get commit summary via `git show --stat --format=...`
#[tauri::command]
pub fn get_commit_summary(repo_root: String, commit: String) -> Result<CommitSummary, String> {
    let output = Command::new("git")
        .arg("show")
        .arg("--stat")
        .arg("--format=%H%n%an%n%ai%n%s")
        .arg(&commit)
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run git show: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git show failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("git output not valid UTF-8: {e}"))?;

    parse_commit_summary(&stdout)
}

/// Get PR metadata via `gh pr view <branch>`.
/// Returns error if gh is not installed or if branch has no associated PR.
#[tauri::command]
pub fn get_pr_metadata(repo_root: String, branch: String) -> Result<PullRequestMetadata, String> {
    let output = Command::new("gh")
        .arg("pr")
        .arg("view")
        .arg(&branch)
        .arg("--json")
        .arg("number,title,state,author,createdAt,url")
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run gh pr view: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr view failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("gh output not valid UTF-8: {e}"))?;

    parse_pr_response(&stdout)
}

/// Create a PR via `gh pr create`. Requires gh to be installed and authenticated.
#[tauri::command]
pub fn create_pr(
    repo_root: String,
    title: String,
    body: Option<String>,
    draft: Option<bool>,
) -> Result<String, String> {
    let mut cmd = Command::new("gh");
    cmd.arg("pr").arg("create").arg("--title").arg(&title);

    if let Some(b) = body {
        cmd.arg("--body").arg(b);
    }

    if draft.unwrap_or(false) {
        cmd.arg("--draft");
    }

    let output = cmd
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("failed to run gh pr create: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gh pr create failed: {stderr}"));
    }

    let stdout = String::from_utf8(output.stdout)
        .map_err(|e| format!("gh output not valid UTF-8: {e}"))?;

    // Extract PR URL from output
    Ok(stdout.trim().to_string())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_commit_summary(output: &str) -> Result<CommitSummary, String> {
    let lines: Vec<&str> = output.lines().collect();
    if lines.is_empty() {
        return Err("empty git show output".to_string());
    }

    let hash = lines.get(0).unwrap_or(&"").to_string();
    let author = lines.get(1).unwrap_or(&"unknown").to_string();
    let date = lines.get(2).unwrap_or(&"unknown").to_string();
    let message = lines.get(3).unwrap_or(&"").to_string();

    // Stats start after the commit info header
    let stats_start = if lines.len() > 4 { 4 } else { lines.len() };
    let stats = lines[stats_start..].join("\n");

    Ok(CommitSummary {
        hash,
        author,
        date,
        message,
        stats,
    })
}

fn parse_pr_response(output: &str) -> Result<PullRequestMetadata, String> {
    #[derive(Deserialize)]
    struct GhPr {
        number: u32,
        title: String,
        state: String,
        author: Author,
        #[serde(rename = "createdAt")]
        created_at: String,
        url: String,
    }

    #[derive(Deserialize)]
    struct Author {
        login: String,
    }

    let pr: GhPr = serde_json::from_str(output)
        .map_err(|e| format!("failed to parse gh pr response: {e}"))?;

    Ok(PullRequestMetadata {
        number: pr.number,
        title: pr.title,
        state: pr.state,
        author: pr.author.login,
        created_at: pr.created_at,
        url: pr.url,
    })
}
