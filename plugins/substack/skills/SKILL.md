---
name: substack
description: Substack CLI — publish posts, manage drafts, post notes, comment, and automate feed engagement from the terminal. Use when drafting or publishing to Substack, reading the feed, posting notes, commenting, or cross-posting blog content.
allowed-tools: Bash(substack:*)
argument-hint: "[draft-id|post-url|text]"
---

# Substack CLI — Skill Reference

Substack CLI for publishing posts, managing drafts, posting notes, commenting, and automated feed engagement.

## When to Use

- User wants to cross-post or publish a blog post to Substack
- User wants to list, create, or manage Substack drafts
- User wants to read the Substack feed (followed publications)
- User wants to post a Note (short-form Substack content)
- User wants to comment on a Substack post
- User wants to run automated feed engagement on Substack
- `engage` calls `substack comment <post-url> <text>` to post comments
- `scout` uses `substack feed --scout` to gather intel from followed publications

## Commands

### configure

```bash
substack configure                          # Interactive setup (prompts for cookie + publication URL)
substack configure --from-browser chrome    # Extract cookie from Chrome automatically
substack configure --from-browser firefox
substack configure show                     # Show current config (masked)
substack configure show --json
```

### doctor

```bash
substack doctor                             # Verify cookie, connectivity, and publication access
substack doctor --json
```

### draft

Create or update a draft from an MDX file.

```bash
substack draft article.mdx
substack draft article.mdx --tags "go,cli"
substack draft article.mdx --audience everyone
substack draft article.mdx --manifest manifest.json --public-dir ./public
substack draft article.mdx --json
```

**Flags:**
- `--tags <csv>` — comma-separated tag names
- `--audience` — `everyone` (default), `only_paid`, `founding`, `only_free`
- `--manifest <path>` — clip manifest JSON for image uploads
- `--public-dir <path>` — directory for resolving image paths

### drafts

```bash
substack drafts                             # List current drafts
substack drafts --limit 10
substack drafts --json
```

### posts

```bash
substack posts                              # List published posts
substack posts --limit 10
substack posts --json
```

### publish

Schedule or immediately publish a draft.

```bash
substack publish <draft-id>                 # Publish immediately
substack publish <draft-id> --at 2026-04-01T09:00:00Z  # Schedule
substack publish <draft-id> --json
```

### unpublish

```bash
substack unpublish <post-id>
substack unpublish <post-id> --json
```

### feed

Read posts from publications you follow.

```bash
substack feed                               # Recent feed posts
substack feed --limit 20
substack feed --scout                       # Scout-compatible output (used by scout CLI)
substack feed --json
```

### comments

```bash
substack comments <post-url>                # List comments on a post
substack comments <post-url> --limit 20
substack comments <post-url> --json
```

### comment

```bash
substack comment <post-url> "Great post!"
substack comment <post-url> "text" --json
```

### note

Post a short-form Note to Substack.

```bash
substack note "Text of the note"
substack note --file note.txt
substack note "text" --image photo.jpg
substack note --reply-to <note-id> "reply text"
substack note --like <note-id>
substack note --delete <note-id>
substack note --dry-run "Preview without posting"
substack note --json
```

### notes

```bash
substack notes                              # List your recent notes
substack notes --limit 20
substack notes --replies <note-id>          # Show replies on a note
substack notes --json
```

### engage

Automated feed engagement — scans dashboard notes, scores them, drafts comments with Claude, and optionally posts. **Dry-run by default.**

```bash
substack engage                             # Dry-run: scan, score, draft
substack engage --post                      # Actually post comments and reactions
substack engage --persona ~/nanika/personas/founder.md
substack engage --max-comments 2 --max-reacts 5
substack engage --json
```

### query

```bash
substack query status --json                # Auth/config status
substack query items --json                 # Configured items (publication URL, subdomain)
substack query actions --json               # Available actions
```

## Configuration

Config file: `~/.substack/config` (0600 permissions)

| Field | Description |
|-------|-------------|
| `cookie` | `substack.sid=<value>` session cookie |
| `publication_url` | Your publication URL (e.g., `https://yourname.substack.com`) |
| `subdomain` | Auto-extracted from publication URL |

Override config dir: `SUBSTACK_CONFIG_DIR`.

## Quick Start

```bash
substack configure                          # Interactive setup
substack doctor                             # Verify setup
substack drafts                             # List current drafts
substack feed                               # Read followed publications
```

## Examples

**User**: "publish my blog post to Substack"
**Action**: `substack draft article.mdx` then `substack publish <draft-id>`

**User**: "post a note about my new project"
**Action**: `substack note "Just launched X — here's what I learned building it"`

**User**: "what's new in my Substack feed"
**Action**: `substack feed --limit 20 --json`

**User**: "comment on that Substack post"
**Action**: `substack comment <post-url> "Your comment text"`

**User**: "run automated Substack engagement"
**Action**: `substack engage` (dry-run to preview), then `substack engage --post` to publish
