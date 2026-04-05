---
name: nen_mcp
description: MCP server for the nen ability system. Exposes nanika's internal state — findings, learnings, missions, proposals, ko verdicts, scheduler jobs, and tracker issues — as Claude-callable tools via the Model Context Protocol.
allowed-tools: mcp__nen__nanika_findings, mcp__nen__nanika_proposals, mcp__nen__nanika_ko_verdicts, mcp__nen__nanika_scheduler_jobs, mcp__nen__nanika_tracker_issues, mcp__nen__nanika_mission, mcp__nen__nanika_events, mcp__nen__nanika_learnings
argument-hint: "[tool] [arguments]"
---

# nen_mcp — Nanika MCP Server

Read-only MCP server that surfaces nanika's internal SQLite state to Claude. Use it to answer questions about what nanika is observing, what it has learned, what missions have run, and what the scheduler and tracker contain — without leaving the conversation.

## When to Use

- Checking active nen observer findings (drift, anomalies, security)
- Reviewing shu improvement proposals
- Querying mission history and phase-level event logs
- Inspecting scheduler job definitions
- Listing tracker issues by status or priority
- Retrieving learnings ordered by quality score

## Quick Start

```bash
nen-mcp doctor          # Verify all 9 backing stores are reachable
nen-mcp doctor --json   # Same, as JSON (includes tool schema list)
nen-mcp version         # Print version and MCP protocol version
```

The server runs as a stdio MCP process — Claude calls it via MCP tools, not via Bash.

---

## Tool Catalog

### `nanika_findings`

List active nen observer findings from `findings.db`.

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `ability` | string | — | Filter by ability name (`gyo`, `en`, `ryu`, `in`, `zetsu`) |
| `severity` | string | — | Filter by severity (`high`, `medium`, `low`) |
| `category` | string | — | Filter by category |
| `active_only` | boolean | `true` | Exclude superseded and expired findings |
| `limit` | integer | `20` | Max results (ceiling: 100) |

**Response schema:**

```json
{
  "count": 2,
  "findings": [
    {
      "id": "fin_abc123",
      "ability": "gyo",
      "category": "persona-drift",
      "severity": "high",
      "title": "Persona drift detected in senior-backend-engineer",
      "description": "...",
      "scope_kind": "persona",
      "scope_value": "senior-backend-engineer",
      "evidence": "...",
      "source": "gyo-scanner",
      "found_at": "2026-04-05T10:00:00Z",
      "expires_at": "",
      "superseded_by": "",
      "created_at": "2026-04-05T10:00:00Z"
    }
  ]
}
```

**Example queries:**

```
nanika_findings {}                                   → all active findings (default limit 20)
nanika_findings { "severity": "high" }              → high-severity findings only
nanika_findings { "ability": "gyo", "limit": 5 }   → last 5 gyo findings
nanika_findings { "active_only": false }             → include superseded/expired
```

---

### `nanika_proposals`

List shu improvement proposals from `proposals.db`.

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `ability` | string | — | Filter by ability name |
| `limit` | integer | `20` | Max results (ceiling: 100) |

**Response schema:**

```json
{
  "count": 1,
  "proposals": [
    {
      "dedup_key": "shu:gyo:persona-drift-mitigation",
      "last_proposed_at": "2026-04-05T12:00:00Z",
      "ability": "shu",
      "category": "persona-drift",
      "tracker_issue": "trk-ABC1"
    }
  ]
}
```

**Example queries:**

```
nanika_proposals {}                        → recent proposals
nanika_proposals { "ability": "shu" }     → shu proposals only
```

---

### `nanika_ko_verdicts`

List ko eval run verdicts from `ko-history.db`.

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `config` | string | — | Substring match on `config_path` |
| `limit` | integer | `20` | Max results (ceiling: 100) |

**Response schema:**

```json
{
  "count": 1,
  "verdicts": [
    {
      "id": "ko_xyz789",
      "config_path": "/path/to/eval.yaml",
      "description": "Persona drift detection eval",
      "model": "claude-sonnet-4-6",
      "started_at": "2026-04-05T09:00:00Z",
      "finished_at": "2026-04-05T09:05:00Z",
      "total": 20,
      "passed": 18,
      "failed": 2,
      "input_tokens": 45000,
      "output_tokens": 12000,
      "cost_usd": 0.14
    }
  ]
}
```

**Example queries:**

```
nanika_ko_verdicts {}                              → recent eval runs
nanika_ko_verdicts { "config": "persona-drift" }  → runs matching config path
```

---

### `nanika_scheduler_jobs`

List scheduler jobs from `scheduler.db`.

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `enabled_only` | boolean | `false` | Only return enabled jobs |
| `limit` | integer | `50` | Max results (ceiling: 200) |

**Response schema:**

```json
{
  "count": 3,
  "jobs": [
    {
      "id": "job_abc",
      "name": "daily-engage",
      "command": "engage commit --count 3",
      "schedule": "0 9 * * *",
      "schedule_type": "cron",
      "enabled": true,
      "priority": "normal",
      "timeout_sec": 300,
      "last_run_at": "2026-04-05T09:00:00Z",
      "next_run_at": "2026-04-06T09:00:00Z",
      "created_at": "2026-03-01T00:00:00Z"
    }
  ]
}
```

**Example queries:**

```
nanika_scheduler_jobs {}                          → all jobs
nanika_scheduler_jobs { "enabled_only": true }   → enabled jobs only
```

---

### `nanika_tracker_issues`

List tracker issues from `tracker.db`.

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `status` | string | — | Filter: `open`, `closed`, `in-progress` |
| `priority` | string | — | Filter: `P0`, `P1`, `P2`, `P3` |
| `limit` | integer | `50` | Max results (ceiling: 200) |

**Response schema:**

```json
{
  "count": 2,
  "issues": [
    {
      "id": "trk-ABC1",
      "title": "Improve persona drift detection",
      "description": "...",
      "status": "open",
      "priority": "P1",
      "labels": "nen,gyo",
      "assignee": "",
      "created_at": "2026-04-01T00:00:00Z",
      "updated_at": "2026-04-05T10:00:00Z"
    }
  ]
}
```

**Example queries:**

```
nanika_tracker_issues {}                             → all issues
nanika_tracker_issues { "status": "open" }          → open issues
nanika_tracker_issues { "priority": "P0" }          → P0 issues
nanika_tracker_issues { "status": "open", "priority": "P1", "limit": 10 }
```

---

### `nanika_mission`

Get one mission by ID or list recent missions from `metrics.db`.

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `mission_id` | string | — | Return a specific mission by ID (skips list mode) |
| `status` | string | — | Filter by status: `success`, `failure`, or `partial` |
| `limit` | integer | `20` | Max results when listing (ceiling: 100) |

**Response schema — single mission:**

```json
{
  "id": "msn_20260405_abc",
  "domain": "dev",
  "task": "Implement nen-mcp doctor subcommand",
  "started_at": "2026-04-05T08:00:00Z",
  "finished_at": "2026-04-05T09:30:00Z",
  "status": "success",
  "phases_total": 3,
  "phases_completed": 3,
  "phases_failed": 0,
  "phases_skipped": 0,
  "retries_total": 0,
  "cost_usd_total": 0.42,
  "decomp_source": "phases"
}
```

**Response schema — list:**

```json
{
  "count": 5,
  "missions": [ /* same shape as single, repeated */ ]
}
```

**Example queries:**

```
nanika_mission {}                                         → 20 most recent missions
nanika_mission { "status": "failure" }                   → failed missions
nanika_mission { "mission_id": "msn_20260405_abc" }      → specific mission
```

---

### `nanika_events`

Read mission events from the JSONL event log at `events/<mission_id>.jsonl`.

**`mission_id` is required.**

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `mission_id` | string | **required** | Mission whose event log to read |
| `event_type` | string | — | Filter by event type (e.g. `phase.started`, `mission.completed`) |
| `limit` | integer | `100` | Max events (ceiling: 500) |

**Response schema:**

```json
{
  "mission_id": "msn_20260405_abc",
  "count": 12,
  "events": [
    { "type": "mission.started", "ts": "2026-04-05T08:00:00Z", ... },
    { "type": "phase.started",   "phase": "scaffold-plugin", "ts": "...", ... },
    { "type": "phase.completed", "phase": "scaffold-plugin", "ts": "...", ... }
  ]
}
```

Events are raw JSON objects from the JSONL log. Shape varies by `type`. Malformed lines are silently skipped.

**Example queries:**

```
nanika_events { "mission_id": "msn_20260405_abc" }
nanika_events { "mission_id": "msn_20260405_abc", "event_type": "phase.started" }
nanika_events { "mission_id": "msn_20260405_abc", "limit": 500 }
```

---

### `nanika_learnings`

List learnings from `learnings.db`, ordered by `quality_score DESC`.

**Arguments:**

| Argument | Type | Default | Description |
|---|---|---|---|
| `domain` | string | — | Filter by domain (`dev`, `personal`) |
| `type` | string | — | Filter by type (`insight`, `pattern`, `error`, `decision`) |
| `archived` | boolean | `false` | Include archived learnings |
| `limit` | integer | `20` | Max results (ceiling: 100) |

**Response schema:**

```json
{
  "count": 3,
  "learnings": [
    {
      "id": "lrn_abc123",
      "type": "pattern",
      "content": "Build the daemon first — one infrastructure investment saves weeks across features.",
      "context": "nen-daemon planning",
      "domain": "dev",
      "worker_name": "senior-backend-engineer",
      "workspace_id": "20260406-e9798f3a",
      "tags": "architecture,daemon",
      "seen_count": 5,
      "used_count": 2,
      "quality_score": 0.92,
      "created_at": "2026-04-01T00:00:00Z",
      "archived": false
    }
  ]
}
```

**Example queries:**

```
nanika_learnings {}                                     → top 20 by quality score
nanika_learnings { "domain": "dev" }                   → dev learnings only
nanika_learnings { "type": "pattern", "limit": 10 }    → top 10 patterns
nanika_learnings { "archived": true }                  → include archived
```

---

## Backing Stores

All tools are read-only. The server never writes to any database.

| Store | Path | Used by |
|---|---|---|
| `learnings.db` | `$ALLUKA_HOME/learnings.db` | `nanika_learnings` |
| `metrics.db` | `$ALLUKA_HOME/metrics.db` | `nanika_mission` |
| `nen/findings.db` | `$ALLUKA_HOME/nen/findings.db` | `nanika_findings` |
| `nen/proposals.db` | `$ALLUKA_HOME/nen/proposals.db` | `nanika_proposals` |
| `ko-history.db` | `$ALLUKA_HOME/ko-history.db` | `nanika_ko_verdicts` |
| `scheduler.db` | `$ALLUKA_HOME/scheduler/scheduler.db` | `nanika_scheduler_jobs` |
| `tracker.db` | `$ALLUKA_HOME/tracker.db` | `nanika_tracker_issues` |
| `events/` | `$ALLUKA_HOME/events/` | `nanika_events` |

Config dir resolution order (mirrors orchestrator): `ORCHESTRATOR_CONFIG_DIR` → `ALLUKA_HOME` → `VIA_HOME/orchestrator` → `~/.alluka` → `~/.via`

---

## Schema Stability Promise

**Field additions are non-breaking.** New fields may appear in response objects in any release. Consumers must ignore unknown fields.

**Field removals and renames are breaking changes** and will not happen without a major version bump (`"version"` in `plugin.json`).

**Current version: 0.1.0.** All response shapes above are stable as of this version.

If a backing store is missing or unreadable, the tool returns an error content block with `isError: true` — it does not return partial data or a degraded shape.

---

## Troubleshooting

| Problem | Fix |
|---|---|
| `backing store not found: ...` | Run `nen-mcp doctor` — the missing store path is shown. The DB may not exist yet if the corresponding subsystem hasn't run. |
| Tool returns empty array | The store exists but has no matching rows. Adjust filters or remove `active_only`. |
| `nen` tools not visible in Claude | Run `nen-mcp doctor` to verify the binary is installed, then check `~/.claude/settings.json` has the `nen` mcpServer entry. |
| MCP server not starting | Ensure `nen-mcp` is on `$PATH` (`which nen-mcp`). Rebuild with `go build -ldflags "-s -w" -o bin/nen-mcp ./cmd/nen-mcp` and re-link. |

```bash
nen-mcp doctor       # check all 9 stores
which nen-mcp        # confirm binary is on PATH
nen-mcp version      # confirm correct version is installed
```
