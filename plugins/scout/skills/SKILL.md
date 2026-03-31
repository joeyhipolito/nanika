---
name: scout
description: Gathers intelligence on configurable topics via scout CLI. Use when user asks about news, trends, scraping, intel gathering, or monitoring topics like AI, Go, developer tools, open source.
allowed-tools: Bash(scout:*)
argument-hint: "[topic-name]"
keywords: scout, intel, news, scraping, topics, automation
category: productivity
version: "1.0.0"
---

# Scout - Intelligence Gathering

Periodic intelligence gathering on configurable topics via the standalone `scout` CLI tool.

## Install

```bash
cd plugins/scout
go build -ldflags "-s -w" -o bin/scout ./cmd/scout-cli
ln -sf $(pwd)/bin/scout ~/bin/scout
```

## When to Use

- User asks about recent news or trends on a topic
- User wants to monitor or track a subject
- User mentions intel, scraping, or gathering info
- User asks about AI, Go, developer tools, open source, or software architecture news
- User wants to set up automated topic monitoring

## Commands

### Topic Management

```bash
scout topics                              # List all configured topics
scout topics add "my-topic"               # Add topic with default sources
scout topics add "my-topic" --sources "web,hackernews,devto" --terms "keyword1,keyword2"
scout topics add "my-topic" --devto-tags "go,cli" --lobsters-tags "programming"
scout topics remove "my-topic"            # Remove a topic
```

### Presets

```bash
scout topics preset                       # List all available presets
scout topics preset ai-all                # Install all AI presets
scout topics preset dev-all               # Install all developer presets
scout topics preset all                   # Install all presets
scout topics preset go-development        # Install a single preset
```

### Gather Intel

```bash
scout gather                              # Gather all topics
scout gather "ai-models"                  # Gather specific topic
```

### Browse Intel

```bash
scout intel                               # List topics with item counts
scout intel "ai-models"                   # Latest intel for topic
scout intel "ai-models" --since "7d"      # Filter by recency
scout intel "ai-models" --json            # JSON output
```

### Content Suggestions

```bash
scout suggest                             # Top 5 suggestions from all topics
scout suggest --since "3d"                # Only intel from last 3 days
scout suggest --topic "ai-models"         # Suggestions for one topic
scout suggest --type thread               # Filter by content type (blog/thread/video)
scout suggest --limit 10                  # Show more suggestions
scout suggest --json                      # Machine-readable JSON output
```

### Topic Discovery

```bash
scout discover                            # Analyze intel and show recommendations
scout discover --dry-run                  # Preview what would be applied
scout discover --auto                     # Apply recommendations automatically
scout discover --since "7d"               # Only use recent intel
scout discover --json                     # Machine-readable JSON output
```

### Setup

```bash
scout configure                           # Interactive setup
scout configure show                      # Show current config
scout doctor                              # Health checks for all sources
```

## Configuration

Config file: `~/.scout/config`
Topics stored at: `~/.scout/topics/{name}.json`
Intel stored at: `~/.scout/intel/{topic}/{date}_{source}.json`

## Source Types

| Source | Auth | Notes |
|--------|------|-------|
| rss | None | RSS/Atom feeds |
| web | None | Google News RSS (AND search) |
| googlenews | None | Google News RSS (OR search) |
| reddit | None | Reddit JSON API |
| github | Optional `GITHUB_TOKEN` | GitHub Search API |
| hackernews | None | HN Algolia API |
| devto | Optional `DEVTO_API_KEY` | Dev.to REST API |
| lobsters | None | Lobste.rs RSS |
| medium | None | Medium RSS feeds |
| substack | None | Substack RSS + Google News |
| youtube | None | YouTube RSS; use `--youtube-channels` |
| arxiv | None | arXiv API; use `--arxiv-categories` |
| bluesky | None | Bluesky public API; uses `search_terms` |
| podcast | None | Podcast RSS; use `--podcast-feeds` |
| linkedin-browser | Requires `linkedin` CLI | Runs `linkedin feed --json`; skipped if CLI not installed |
| substack-browser | Requires `substack` CLI | Runs `substack feed --scout`; skipped if CLI not installed |
| google-browser | Chrome CDP | Inline browser scraping; requires Chrome on localhost:9222 |
| x-browser | Chrome CDP | Inline browser scraping; requires Chrome on localhost:9222 |

## Platform CLI Dependencies

Some browser gatherers delegate to platform CLIs rather than inline browser scraping.
These sources are silently skipped when the CLI is not installed — no error is returned.

### `linkedin-browser`

Delegates to the `linkedin` CLI (from `plugins/linkedin`):

```bash
cd plugins/linkedin && go build -o bin/linkedin ./cmd/linkedin-cli && ln -sf $(pwd)/bin/linkedin ~/bin/linkedin
```

The CLI requires Chrome running on `localhost:9222` with an active LinkedIn session (configured via `linkedin configure`).

### `substack-browser`

Delegates to the `substack` CLI (from `plugins/substack`):

```bash
cd plugins/substack && go build -o bin/substack ./cmd/substack-cli && ln -sf $(pwd)/bin/substack ~/bin/substack
```

The CLI requires a valid Substack session cookie (configured via `substack configure`).

## Dashboard Module

Scout emits `scout.intel_gathered` events to the daemon event bus after each gather run. The dashboard command palette surfaces these via the SSE stream.

**Event type**: `scout.intel_gathered`

**Data fields**:
| Field | Type | Description |
|-------|------|-------------|
| `topic` | string | Topic name that was gathered |
| `item_count` | int | Total new items gathered across all sources |

**Dashboard command palette entries** (available when daemon is running):
- `Scout: gather <topic>` — trigger an immediate gather for a topic
- `Scout: intel <topic>` — open latest intel for a topic
- `Scout: suggest` — show content suggestions from recent intel

**Notification channels**: `scout.intel_gathered` is included in the default event set for both Telegram and Discord notifiers. To opt out, set an explicit `events` list in `~/.alluka/channels/telegram.json` or `~/.alluka/channels/discord.json` that omits `"scout.intel_gathered"`.

## Examples

**User**: "what's new with AI models"
**Action**: `scout gather "ai-models"` then `scout intel "ai-models"`

**User**: "set up monitoring for Go development"
**Action**: `scout topics preset go-development` then `scout gather "go-development"`

**User**: "what should I write about this week"
**Action**: `scout suggest --since "7d"`

**User**: "check if all sources are working"
**Action**: `scout doctor`
