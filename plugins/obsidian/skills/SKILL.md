---
name: obsidian
description: Obsidian vault CLI — read, write, search, capture, triage, enrich, and ingest into your Obsidian vault from the terminal. Use when user asks about notes, vault, captures, inbox triage, note search, or importing scout/learnings data.
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
ln -sf $(pwd)/bin/obsidian ~/bin/obsidian
```

## When to Use

- User asks to read, write, or search notes in their Obsidian vault
- User wants to capture a fleeting note or idea
- User wants to triage their Inbox
- User wants to import scout intel or orchestrator learnings into the vault
- User asks about note health, stale notes, or orphan detection
- User wants to sync website metadata into the vault

## Configuration

```bash
obsidian configure              # Interactive setup (vault path + Gemini API key)
obsidian configure show         # Show current config
obsidian doctor                 # Validate installation and config
```

Config lives at `~/.obsidian/config`. Override with `OBSIDIAN_CONFIG_DIR`.

## Commands

### Read

```bash
obsidian read daily/2026-03-25.md           # Read a note
obsidian read daily/2026-03-25.md --json    # JSON output
```

### Append

```bash
obsidian append daily/2026-03-25.md "New task"
obsidian append daily/2026-03-25.md --section "## Tasks" "- buy milk"
echo "piped text" | obsidian append daily/2026-03-25.md
```

### Capture

Creates a fleeting note in `Inbox/`.

```bash
obsidian capture "rough idea about search"
obsidian capture "link worth reading" --source https://example.com
echo "piped text" | obsidian capture
obsidian capture "draft" --json
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
```

### Search

```bash
obsidian search "project ideas"                   # Hybrid search (default)
obsidian search "golang" --mode keyword           # Keyword-only
obsidian search "concurrency" --mode semantic     # Semantic (embedding) search
obsidian search "interfaces" --mode hybrid --json
```

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

Review and process notes in `Inbox/`.

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
obsidian canonicalize --mode per-capture --capture Inbox/20260319.md   # Single capture
```

### Health

Vault diagnostics: inbox depth, orphans, stale captures, classification distribution, link density.

```bash
obsidian health
obsidian health --json
```

## Global Options

```
--json       Machine-readable JSON output
--help, -h   Show help
--version    Show version
```

## Cron Setup

Run triage hourly, only output on activity:

```bash
# crontab -e
0 * * * * /usr/local/bin/obsidian triage --auto --quiet 2>&1
```
