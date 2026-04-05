# nen_mcp

MCP server for the nen ability system. Exposes nanika's internal SQLite state — observer findings, learnings, missions, proposals, ko verdicts, scheduler jobs, and tracker issues — as Claude-callable tools via the Model Context Protocol.

## Quick Start

```bash
cd plugins/nen_mcp
go build -ldflags "-s -w" -o bin/nen-mcp ./cmd/nen-mcp
ln -sf "$(pwd)/bin/nen-mcp" ~/bin/nen-mcp
nen-mcp doctor
```

Claude will automatically see the `nanika_*` tools after restarting — no further setup needed if `~/.claude/settings.json` already has the `nen` server entry (registered by `plugin.json`).

## Installation

### Build

```bash
cd plugins/nen_mcp
go build -ldflags "-s -w" -o bin/nen-mcp ./cmd/nen-mcp
```

Requires Go 1.22+. No CGo — uses the pure-Go SQLite driver (`modernc.org/sqlite`).

### Install binary

```bash
ln -sf "$(pwd)/bin/nen-mcp" ~/bin/nen-mcp
```

`~/bin` must be on your `$PATH`. Verify with `which nen-mcp`.

### Register MCP server

Add to `~/.claude/settings.json` under `mcpServers`:

```json
{
  "mcpServers": {
    "nen": {
      "command": "nen-mcp",
      "args": [],
      "type": "stdio"
    }
  }
}
```

This is the entry from `plugin.json`. Claude Code registers it automatically when the plugin is installed via the nanika plugin manager.

### Verify

```bash
nen-mcp doctor
```

All 9 backing stores should show `✓`. Restart Claude Code to pick up the newly registered MCP server.

## Configuration

No configuration file. All paths resolve from environment variables:

| Variable | Default | Purpose |
|---|---|---|
| `ORCHESTRATOR_CONFIG_DIR` | `~/.alluka` | Override orchestrator state root |
| `ALLUKA_HOME` | `~/.alluka` | Fallback state root |
| `VIA_HOME` | — | Legacy fallback (`$VIA_HOME/orchestrator`) |
| `SCHEDULER_CONFIG_DIR` | `~/.alluka/scheduler` | Override scheduler DB directory |
| `TRACKER_DB` | `~/.alluka/tracker.db` | Override tracker DB path |

Resolution order for the orchestrator state root: `ORCHESTRATOR_CONFIG_DIR` → `ALLUKA_HOME` → `VIA_HOME/orchestrator` → `~/.alluka` → `~/.via`.

## Tools

Eight read-only MCP tools, all prefixed `nanika_`:

| Tool | Backing store | Returns |
|---|---|---|
| `nanika_findings` | `nen/findings.db` | Observer findings (drift, anomalies) |
| `nanika_proposals` | `nen/proposals.db` | Shu improvement proposals |
| `nanika_ko_verdicts` | `ko-history.db` | Ko eval run results |
| `nanika_scheduler_jobs` | `scheduler.db` | Cron job definitions |
| `nanika_tracker_issues` | `tracker.db` | Tracker issues |
| `nanika_mission` | `metrics.db` | Mission history |
| `nanika_events` | `events/<id>.jsonl` | Phase-level mission events |
| `nanika_learnings` | `learnings.db` | Learnings by quality score |

Full argument shapes, response schemas, and example queries: [`skills/SKILL.md`](skills/SKILL.md).

## Architecture

```
plugins/nen_mcp/
├── cmd/nen-mcp/
│   ├── main.go       # MCP stdio loop, JSON-RPC dispatch
│   ├── tools.go      # Tool definitions + all 8 handlers
│   └── doctor.go     # nen-mcp doctor subcommand
├── bin/nen-mcp       # Compiled binary (gitignored)
├── skills/
│   └── SKILL.md      # Tool reference for Claude
├── plugin.json       # Plugin manifest + MCP server registration
├── go.mod
└── go.sum
```

**Transport:** stdio JSON-RPC 2.0, MCP protocol `2024-11-05`.

**All tools are read-only.** The server opens every SQLite database in `mode=ro` and never writes. Safe to run alongside active daemons.

**30-second timeout** per tool call. Long-running queries (e.g. large event logs) are capped by the `limit` parameter.

## Extending

### Adding a new tool

1. Add a `tool{}` entry to `listTools()` in `tools.go` with name, description, and `inputSchema`.
2. Add a handler `func handleMyTool(ctx context.Context, rawArgs json.RawMessage) (any, error)`.
3. Add a case to `dispatchTool()`.
4. Add the new backing store to `runDoctor()` in `doctor.go` if it uses a new DB.
5. Update `skills/SKILL.md` with the argument table, response schema, and examples.

Follow the existing handler pattern: unmarshal args into a typed struct, call `openReadOnly()`, use `clampLimit()` for pagination, return `map[string]any` with a top-level count key.

### Adding a new backing store path helper

Mirror the pattern used by `schedulerDBPath()` and `trackerDBPath()`: check an env var override first, then fall back to the standard `~/.alluka/` path. This keeps path resolution testable and consistent with the owning plugin.

## Development

```bash
go build ./cmd/nen-mcp            # Build
go vet ./cmd/nen-mcp              # Lint
go test ./...                     # Tests (currently integration-style via doctor)
nen-mcp doctor                    # Smoke test against live DBs
nen-mcp doctor --json | jq .      # Inspect full doctor output
```

## Links

- MCP protocol spec: `2024-11-05`
- Backing system: `plugins/scheduler`, `plugins/tracker`, `skills/orchestrator`
- Nen ability system: `docs/` (nen architecture)
- Skill reference: [`skills/SKILL.md`](skills/SKILL.md)
