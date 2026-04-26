---
name: obsidian
description: Obsidian vault CLI — read, write, search, capture, triage, enrich, and ingest into the vault at ~/.alluka/vault/. Prefer this CLI over the Write tool for vault paths so the search index, frontmatter schema, and triage hooks stay in sync — plain Write still works and the memory system reads any valid markdown at the canonical paths regardless of how it was produced. Use when user asks about notes, vault, captures, inbox triage, note search, or importing scout/learnings data.
allowed-tools: Bash(obsidian:*)
argument-hint: "[command] [args]"
keywords: obsidian, vault, notes, capture, triage, search, inbox, knowledge, markdown
category: productivity
version: "0.1.0"
---

# Obsidian - Vault CLI

Read, write, search, and manage your Obsidian vault from the terminal.

## Install

```bash
cd plugins/obsidian
go build -ldflags "-s -w" -o bin/obsidian ./cmd/obsidian-cli
ln -sf $(pwd)/bin/obsidian ~/.alluka/bin/obsidian
```

## When to Use

**Rule of thumb:** if the target path starts with `~/.alluka/vault/`, prefer this CLI. Direct `Write` still produces valid notes the memory reader surfaces, but skips index updates, frontmatter validation, and triage hooks — so `obsidian search`, `obsidian enrich`, and `obsidian maintain` will see stale state until the index is rebuilt.

- User asks to read, write, or search notes in their Obsidian vault → `obsidian read | create | append | search`
- User wants to capture a fleeting note or idea → `obsidian capture`
- User wants to triage their inbox → `obsidian triage`
- User wants to import scout intel or orchestrator learnings into the vault → `obsidian ingest`
- User asks about note health, stale notes, or orphan detection → `obsidian maintain` / `obsidian enrich`
- User wants to sync website metadata into the vault → `obsidian sync`

### Path conventions (so preflight's Narrative Context surfaces the note)

- Today's log → `daily/YYYY-MM-DD.md` (starts with `# <meaningful title>` H1)
- Topic index → `mocs/<topic-slug>.md`
- Session capture → `sessions/<slug>.md`
- Fleeting → `inbox/` (auto via `obsidian capture`)

First H1 becomes the title shown in the preflight brief — make it descriptive.

## Configuration

```bash
obsidian configure              # Interactive setup (vault path + Gemini API key)
obsidian configure show         # Show current config
obsidian doctor                 # Validate installation and config
```

Config lives at `~/.obsidian/config`. Override with `OBSIDIAN_CONFIG_DIR`.

### Multi-vault

The CLI supports two named vaults: `nanika` (default) and `second-brain`. Configure both in `~/.obsidian/config`:

```
vault_path=/path/to/nanika-vault
second_brain_path=/path/to/second-brain
```

Override either path with env vars: `OBSIDIAN_VAULT_PATH` or `OBSIDIAN_SECOND_BRAIN_PATH`.

## Commands

### Read

```bash
obsidian read daily/2026-03-25.md           # Read a note
obsidian read daily/2026-03-25.md --json    # JSON output
obsidian read inbox/20260419.md --vault second-brain  # Read from second-brain vault
```

### Append

```bash
obsidian append daily/2026-03-25.md "New task"
obsidian append daily/2026-03-25.md --section "## Tasks" "- buy milk"
echo "piped text" | obsidian append daily/2026-03-25.md
```

### Capture

Creates a fleeting note in `inbox/`.

```bash
obsidian capture "rough idea about search"
obsidian capture "link worth reading" --source https://example.com
echo "piped text" | obsidian capture
obsidian capture "draft" --json
obsidian capture "quick idea" --vault second-brain    # Capture to second-brain inbox
```

### Create

```bash
obsidian create projects/new-idea.md --title "New Idea" --type idea
obsidian create projects/new-idea.md --tags "go,cli" --status draft
obsidian create projects/new-idea.md --summary "Short description"
obsidian create projects/new-idea.md --context-set "work"
obsidian create projects/new-idea.md --template "99 Templates/idea.md"
```

### List

```bash
obsidian list                   # All notes in vault
obsidian list daily/            # Notes in a folder
obsidian list --json            # JSON output
obsidian list inbox/ --vault second-brain             # List second-brain inbox
```

### Search

```bash
obsidian search "project ideas"                   # Hybrid search (default)
obsidian search "golang" --mode keyword           # Keyword-only
obsidian search "concurrency" --mode semantic     # Semantic (embedding) search
obsidian search "interfaces" --mode hybrid --json
obsidian search "meeting notes" --vault second-brain  # Search second-brain vault
```

### Recall

Surface notes related to a seed query using 2-hop BFS from lexical/frontmatter matches, scored by distance, recency, and tags. Useful for recovering task context mid-session without re-reading artifacts.

```bash
obsidian recall "BFS algorithm"                         # Default: top 5, brief format
obsidian recall "BFS algorithm" --limit 10              # Return up to 10 results
obsidian recall "BFS algorithm" --format json           # JSON output
obsidian recall "BFS algorithm" --format markdown       # Markdown with frontmatter
obsidian recall "BFS algorithm" --format paths          # File paths only
obsidian recall "BFS algorithm" --socket /tmp/obs.sock  # Use specific RPC socket
obsidian recall "BFS algorithm" --no-fallback           # Fail if RPC socket unavailable
```

Default: `--limit 5 --format brief`.

> **Fallback note:** if the RPC socket is unavailable, recall falls back to in-process BFS. Pass `--no-fallback` to disable this and return an error instead.

### Index

Build or update the search index (run before semantic/hybrid search).

```bash
obsidian index
obsidian index --json
```

### Sync

Sync website content metadata into vault notes.

```bash
obsidian sync                   # Sync website to vault
obsidian sync --dry-run         # Preview without writing
obsidian sync --force           # Overwrite unchanged + include unpublished
```

### Enrich

Suggest links, tags, and detect orphan notes.

```bash
obsidian enrich                 # Show suggestions
obsidian enrich --apply         # Write suggested links to notes
obsidian enrich --json
```

### Maintain

Vault health checks and reporting.

```bash
obsidian maintain                         # Health report
obsidian maintain --stale-days 60         # Notes older than 60 days count as stale
obsidian maintain --fix                   # Add frontmatter to notes missing it
obsidian maintain --json
```

### Ingest

Import data from external sources into vault notes.

```bash
obsidian ingest --source scout                              # All scout intel
obsidian ingest --source scout --topic "ai-models"         # Filter by topic
obsidian ingest --source scout --since 7d                  # Last 7 days only
obsidian ingest --source learnings                         # Orchestrator learnings
obsidian ingest --source learnings --domain dev            # Filter by domain
obsidian ingest --source learnings --since 30d
obsidian ingest --source scout --dry-run                   # Preview without writing
obsidian ingest --source scout --json
```

Sources read from:
- `scout`: `~/.scout/intel/`
- `learnings`: `~/.alluka/learnings.db`

### Triage

Review and process notes in `inbox/`.

```bash
obsidian triage                         # List pending notes with age (default)
obsidian triage --older 7d              # Only notes older than 7 days
obsidian triage --auto                  # Classify, enrich, and move each note
obsidian triage --auto --dry-run        # Preview without writing
obsidian triage --auto --json           # Structured output
obsidian triage --auto --quiet          # No output when inbox is clear (cron-safe)
```

### Resurface

Surface old notes relevant to a query.

```bash
obsidian resurface "golang patterns"                    # Hybrid search over old notes
obsidian resurface "golang patterns" --older 14d --limit 3
obsidian resurface --random                             # Random old note (serendipitous)
obsidian resurface --random --older 30d --json
```

Default: `--older 7d --limit 5`.

### Auto-Capture

Capture learnings, workspace artifacts, and scout intel into the vault in one pass.

```bash
obsidian auto-capture                   # All sources
obsidian auto-capture --since 7d        # Limit lookback window
obsidian auto-capture --dry-run         # Preview what would be captured
obsidian auto-capture --json
```

Sources: `~/.alluka/learnings.db`, `~/.alluka/workspaces/`, and scout intel.

### Promote

Detect clusters of related captures and merge into canonical notes.

```bash
obsidian promote                        # Interactive cluster promotion
obsidian promote --dry-run              # Preview clusters without writing
obsidian promote --json
```

### Cluster

Find 3+ untagged captures about unnamed concepts via embedding similarity.

```bash
obsidian cluster                        # Detect and store clusters
obsidian cluster --dry-run              # Preview without storing
obsidian cluster --json
```

### Canonicalize

Create canonical notes from detected clusters with LLM-synthesized summaries.

```bash
obsidian canonicalize                                                   # All stored clusters
obsidian canonicalize --dry-run                                         # Preview
obsidian canonicalize --json
obsidian canonicalize --mode per-capture --capture inbox/20260319.md   # Single capture
```

### Health

Vault diagnostics: inbox depth, orphans, stale captures, classification distribution, link density.

```bash
obsidian health
obsidian health --json
```

## Global Options

```
--json                          Machine-readable JSON output
--vault <nanika|second-brain>   Target vault (default: nanika)
--help, -h                      Show help
--version                       Show version
```

## Multi-vault

Use `--vault second-brain` on any command to target the second vault configured as `second_brain_path`.

```bash
obsidian capture "idea" --vault second-brain
obsidian list --vault second-brain
obsidian search "golang" --vault second-brain
obsidian triage --vault second-brain
```

Both vaults must be configured before use:

```bash
obsidian configure              # Set vault_path (nanika vault)
# Then manually add to ~/.obsidian/config:
# second_brain_path=/path/to/second-brain
```

Or set `OBSIDIAN_SECOND_BRAIN_PATH=/path/to/second-brain` in your environment.

## Cron Setup

Run triage hourly, only output on activity:

```bash
# crontab -e
0 * * * * /usr/local/bin/obsidian triage --auto --quiet 2>&1
```
