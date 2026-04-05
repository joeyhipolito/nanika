---
produced_by: technical-writer
phase: phase-2
workspace: 20260329-0ec406b5
created_at: "2026-03-29T20:15:00Z"
confidence: high
depends_on: []
token_estimate: 2600
---

# Plugin Protocol

Nanika plugins are standalone CLIs that expose a uniform query interface. Any subscriber — the orchestrator, another plugin, or an external tool you write — can discover a plugin's capabilities, read its state, and trigger its actions without knowing its implementation details.

## Overview

The plugin system has two layers:

1. **Discovery** — Subscribers scan `~/nanika/plugins/*/plugin.json` to find plugins and their metadata.
2. **Query** — Subscribers invoke `<binary> query {status|items|actions|action} --json` to fetch data or trigger behavior.

## Plugin Discovery

### File Layout

```
~/nanika/plugins/<name>/
├── plugin.json                    # Plugin manifest
├── bin/<binary>                   # Compiled binary (CLI)
└── skills/
    └── SKILL.md                   # Skill definition (how agents call it)
```

### Manifest Scan

A plugin is discoverable when:

- `plugin.json` is present at `~/nanika/plugins/<name>/plugin.json`
- `api_version >= 1`
- The JSON parses cleanly

Subscribers should re-scan on demand — plugins installed or updated at runtime appear on the next scan.

## plugin.json Schema

### Required Fields

| Field | Type | Notes |
|-------|------|-------|
| `name` | string | Unique plugin identifier; used in CLI paths, IDs, and module names. Lowercase, no spaces. |
| `version` | string | SemVer (e.g. `1.0.0`). No functional use; for documentation. |
| `api_version` | int | Must be `1` for discovery. |

### Optional Fields

| Field | Type | Notes |
|-------|------|-------|
| `description` | string | One-liner shown in subscriber UIs. |
| `icon` | string | Icon key (e.g. `ListCheck`, `Calendar`). Subscribers may map this to a glyph. |
| `binary` | string | CLI binary name (e.g. `tracker`). Resolved via `$PATH` lookup. If missing, the plugin is not queryable. |
| `build` | string | Build command (e.g. `cargo build --release`). For documentation only; not executed by subscribers. |
| `install` | string | Install command. For documentation only. |
| `tags` | []string | Searchable keywords (e.g. `["issue-tracking", "task-management"]`). |
| `provides` | []string | Array of query types this plugin provides. Example: `["status", "items", "actions"]`. |
| `actions` | object | Maps action keys to command templates or objects. See Query Protocol. |
| `repository` | object | Source metadata: `type` (git), `url`, `path`. For documentation. |

### Example: tracker

```json
{
  "name": "tracker",
  "version": "0.1.0",
  "api_version": 1,
  "description": "Local issue tracker with hierarchical relationships",
  "icon": "ListCheck",
  "binary": "tracker",
  "build": "cargo build --release",
  "install": "cp target/release/tracker ~/bin/tracker",
  "tags": ["issue-tracking", "task-management"],
  "provides": ["status", "items", "actions"],
  "actions": {
    "status": "tracker query status --json",
    "items": "tracker query items --json",
    "actions": "tracker query actions --json"
  },
  "repository": {
    "type": "git",
    "url": "https://github.com/joeyhipolito/nanika",
    "path": "plugins/tracker"
  }
}
```

### Example: scheduler

```json
{
  "name": "scheduler",
  "version": "1.0.0",
  "api_version": 1,
  "description": "Local job scheduler and dispatch loop",
  "icon": "Calendar",
  "binary": "scheduler",
  "build": "go build -ldflags \"-s -w\" -o bin/scheduler ./cmd/scheduler-cli",
  "install": "ln -sf $(pwd)/bin/scheduler ~/bin/scheduler",
  "tags": ["scheduler", "cron", "jobs"],
  "provides": ["query status", "query items", "query action"],
  "actions": {
    "status": {
      "cmd": ["scheduler", "query", "status", "--json"],
      "description": "Daemon running state, job count, next scheduled run time"
    },
    "items": {
      "cmd": ["scheduler", "query", "items", "--json"],
      "description": "List all jobs"
    },
    "action_run": {
      "cmd": ["scheduler", "query", "action", "run", "<job_id>", "--json"],
      "description": "Execute a job immediately"
    }
  }
}
```

## Query Protocol

### Overview

Subscribers call `<binary> query <type> --json` and expect JSON output on stdout.

### Query Types

**status** — Overview and health of the plugin

```bash
<binary> query status --json
```

Return a JSON object (any shape) representing the plugin's overall status. Example:

```json
{
  "ok": true,
  "count": 42,
  "type": "tracker-status"
}
```

**items** — Itemized list for display in a table

```bash
<binary> query items --json
```

Return a JSON array of objects, where each object is a table row. Columns are inferred from the first item's keys.

```json
{
  "items": [
    { "id": "trk-1", "title": "Fix login bug", "status": "in-progress", "priority": "P0" },
    { "id": "trk-2", "title": "Add dark mode", "status": "open", "priority": "P1" }
  ],
  "count": 2
}
```

Or just an array:

```json
[
  { "id": "job-1", "name": "daily-backup", "last_run": "2026-03-29T08:00:00Z", "next_run": "2026-03-30T02:00:00Z" }
]
```

**actions** — List of available actions

```bash
<binary> query actions --json
```

Return a JSON array of action definitions:

```json
{
  "actions": [
    {
      "name": "next",
      "command": "tracker query action next",
      "description": "Show the highest-priority ready issue"
    }
  ]
}
```

**action &lt;verb&gt; [&lt;id&gt;]** — Execute a single action

```bash
<binary> query action run <job_id> --json
<binary> query action approve --json
```

Return a JSON object describing the result. Shape is plugin-defined, but should include `ok: boolean`:

```json
{
  "ok": true,
  "message": "Job executed successfully",
  "exit_code": 0
}
```

### JSON Envelope (Optional)

Plugins may wrap responses in an envelope for clarity. Subscribers should parse the actual data (array, object) as JSON — no strict envelope format is enforced. Typical subscribers use `json.Unmarshal(data, &target)` where `target` matches the expected shape (array for items, object for status).

### Action Command Templates

In `plugin.json`, actions can be:

1. **String** — Direct shell command:
   ```json
   "actions": {
     "status": "tracker query status --json"
   }
   ```

2. **Object** — Command with metadata:
   ```json
   "actions": {
     "status": {
       "cmd": ["tracker", "query", "status", "--json"],
       "description": "Current status"
     }
   }
   ```

3. **Per-item actions** — Contain ID placeholders detected by regex `/<[^>]+>/`:
   ```json
   "actions": {
     "action_run": {
       "cmd": ["scheduler", "query", "action", "run", "<job_id>", "--json"],
       "description": "Execute a job"
     }
   }
   ```

### Timeouts

Subscribers should bound query execution. Recommended defaults:

- **Status/items**: 15 seconds
- **Actions**: 30 seconds

If a query hangs or fails, the subscriber should surface the error to the caller rather than retry silently.

## Plugin Development Checklist

### 1. Create Manifest

```json
{
  "name": "myname",
  "version": "0.1.0",
  "api_version": 1,
  "description": "...",
  "binary": "myname",
  "build": "...",
  "tags": ["..."]
}
```

### 2. Implement CLI Queries

```bash
# Build your binary to accept these commands:
myname query status --json   # Returns JSON object
myname query items --json    # Returns JSON array
myname query actions --json  # Returns { actions: [...] }
myname query action <verb> [<id>] --json  # Returns action result
```

Queries should be idempotent and complete within their timeouts.

### 3. Deploy

Symlink or copy the binary to `~/bin/<name>`:

```bash
ln -s $(pwd)/bin/myname ~/bin/myname
```

Any subscriber scanning `~/nanika/plugins/` will pick the plugin up on the next refresh.

## Binary Resolution

Subscribers should resolve the plugin binary in this order:

1. Read `plugin.json` and extract the `binary` field.
2. Look it up via `exec.LookPath(binary)` (i.e. check `$PATH`).
3. Fall back to `~/nanika/bin/<name>` if `$PATH` lookup fails.

When launching plugin subprocesses, enrich `$PATH` with common user paths so plugins installed via `go install` or symlinked into `~/bin` are reachable:

- `~/bin`
- `~/.local/bin`
- `~/go/bin`
- `/opt/homebrew/bin`
- `/usr/local/bin`

## Patterns and Anti-Patterns

### DO

- **Return clean JSON** — Status/items with consistent field names make subscribers' lives easier.
- **Handle timeouts gracefully** — Queries should return quickly; cache heavy operations.
- **Implement query status** — Even if it's just `{ "ok": true }`, it confirms the plugin is registered and reachable.
- **Keep side effects behind `query action`** — `status`/`items`/`actions` should be read-only.
- **Document action templates** — Subscribers that auto-generate UIs rely on the `description` fields.

### DON'T

- **Return invalid JSON** — Partial or malformed responses break every subscriber.
- **Assume `$PATH`** — Plugins might be invoked from a sandboxed parent; rely on `~/bin` or absolute symlinks.
- **Forget `--json`** — All query commands must output JSON, not human-readable text.
- **Mix read and write** — `status`/`items` should never mutate state.

---

**PATTERN:** All plugins follow the same query protocol (status/items/actions/action) so subscribers can provide a generic fallback UI. This makes plugin-specific rendering optional rather than required.
