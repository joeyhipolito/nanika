---
name: engage
description: Cross-platform comment engagement CLI — scan, draft, review, approve, and post comments across YouTube, LinkedIn, Reddit, and Substack. Use when drafting engagement comments, reviewing pending drafts, running the automated engagement loop, or posting approved comments.
allowed-tools: Bash(engage:*)
argument-hint: "[draft-id]"
---

# engage — Cross-Platform Comment Engagement CLI

`engage` is a CLI tool for discovering, drafting, reviewing, and posting comments across YouTube, LinkedIn, Reddit, and Substack. It uses platform CLIs for data access and the `claude` CLI for LLM-assisted drafting.

## When to Use

- User wants to draft comments for engagement across social platforms
- User wants to review or approve pending engagement drafts
- User wants to post approved comments to YouTube, LinkedIn, Reddit, or Substack
- User asks about the automated engagement loop or pipeline
- User wants to check engagement history
- The scheduler runs `engage scan && engage draft --reschedule-post` daily

## Quick Reference

```bash
engage doctor                                        Check all required CLIs are installed
engage scan                                          Scan all platforms for opportunities
engage draft                                         Draft comments for top opportunities
engage draft --reschedule-post                       Draft + schedule first commit run for tomorrow
engage adapt <source> --platforms <platforms>       Adapt content for specific platforms
engage adapt article.md --platforms linkedin,x      Adapt local file for multiple platforms
engage review                                        Review pending drafts
engage approve <id>                                  Approve a draft for posting
engage reject <id> [--note "reason"]                 Reject a draft
engage post                                          Post all approved drafts immediately
engage post <id>                                     Post one approved draft
engage commit                                        Post up to 3 oldest approved drafts (daemon-safe)
engage commit --count 5 --reschedule                 Post up to 5 and reschedule next run if drafts remain
engage history                                       Show posting history
engage history --since 7d                            Show last 7 days of history
```

## Full Workflow

### 1. Check prerequisites

```
engage doctor
```

Verifies that all platform CLIs (`linkedin`, `youtube`, `reddit`, `substack`) and `claude` are available in PATH. Fix any missing CLIs before proceeding.

### 2. Scan for opportunities

```
engage scan
engage scan --platform youtube,linkedin
engage scan --limit 10 --json
```

Calls each platform CLI to surface recent posts, videos, and articles worth commenting on. Returns metadata only (no full text or comments yet).

### 3. Draft comments

```
engage draft
engage draft --platform linkedin --limit 5
engage draft --persona joey --skip-authenticity-pass
engage draft --reschedule-post
```

For each opportunity:
1. Calls `Enrich()` to fetch the full post body and existing comments.
2. Injects top 8 existing comments into the prompt so the draft doesn't repeat common angles.
3. Runs a drafting pass (claude Sonnet) to generate the comment.
4. Runs an authenticity rewrite pass (non-fatal — falls back to original draft on failure).
5. Saves the draft to `~/.alluka/engage/queue/<id>.json` in `pending` state.

If `--reschedule-post` is set and at least one draft was created, schedules a one-shot `engage-commit-YYYYMMDD` job via the scheduler CLI for tomorrow at a random time between 08:00 and 20:59. This seeds the posting loop without manual intervention.

Persona files live in `~/nanika/personas/<name>.md`. Override with `ENGAGE_PERSONAS_DIR`.

### 3a. Adapt content for multiple platforms (optional)

```
engage adapt article.md --platforms linkedin,x,reddit
engage adapt https://example.com/blog-post --platforms youtube,substack
engage adapt content.txt --persona joey --platforms linkedin --dry-run
```

The `adapt` subcommand takes user-provided content (file or URL) and generates platform-specific versions. For each platform:

1. Reads source content from a file path or URL
2. Composes a platform-specific system prompt with audience/constraint guidance
3. Calls claude Sonnet to adapt the content
4. Saves each adaptation to `~/.alluka/engage/queue/<id>.json` in `pending` state

This is useful for:
- Repurposing a blog post across LinkedIn, Reddit, Twitter, etc.
- Adapting a long-form article into platform-specific snippets
- Maintaining a consistent voice across platforms while respecting each platform's conventions

Adaptations appear in the review queue alongside engagement drafts and can be approved, rejected, or posted using the same workflow.

### 4. Review drafts

```
engage review
engage review --state all
engage review --state approved
```

Lists drafts with context: post title, author, URL, existing comment excerpts, and the generated draft. States: `pending`, `approved`, `rejected`, `posted`.

### 5. Approve or reject

```
engage approve linkedin-abc123-20260325-120000
engage reject  linkedin-abc123-20260325-120000 --note "too generic"
```

Transitions the draft to `approved` or `rejected`. Only approved drafts can be posted.

### 6. Post

```
engage post
engage post --dry-run
engage post linkedin-abc123-20260325-120000
```

Posts each approved draft by calling the appropriate platform CLI:

| Platform  | CLI call                              |
|-----------|---------------------------------------|
| LinkedIn  | `linkedin comment <urn> <text>`       |
| YouTube   | `youtube comment <video-id> <text>`   |
| Reddit    | `reddit comment <post-id> <text>`     |
| Substack  | `substack comment <post-url> <text>`  |

On success:
- Transitions the queue item to `posted`.
- Writes a history record to `~/.alluka/engage/history/<id>.json`.

Use `--dry-run` to preview without sending.

### 7. View history

```
engage history
engage history --since 7d
engage history --since 24h
```

Shows all posted engagements sorted by most recent. History records include platform, URL, comment text, posted timestamp, and (if fetched later) likes and replies received.

## Automated Draft → Review → Post Loop

The full pipeline runs hands-free via the scheduler daemon, with a human review step in the middle.

### Flow overview

```
09:00 daily  engage scan && engage draft --reschedule-post
                  ↓ (if drafts created, schedules engage-commit-YYYYMMDD for tomorrow 08–20h)
human runs   engage review  →  engage approve / engage reject
                  ↓ (approved drafts sit in queue until the scheduled run)
next day     engage commit --count 3 --reschedule
                  ↓ (posts oldest 3 approved, then reschedules itself for the day after if drafts remain)
             → loop continues until queue is empty
```

### Setting up the loop

```bash
# One-time: create the daily-engage scheduler job (includes --reschedule-post)
scheduler init

# Start the daemon
scheduler daemon

# After the first draft run, review and approve some drafts
engage review
engage approve <id>

# The scheduled engage-commit-YYYYMMDD job will fire automatically the next day.
# To seed manually (e.g. drafts already exist):
engage commit --count 3 --reschedule
```

### Flags

| Command | Flag | Effect |
|---|---|---|
| `engage draft` | `--reschedule-post` | After drafting, schedule a one-shot `engage-commit-YYYYMMDD` job for tomorrow at a random 08:00–20:59 time |
| `engage commit` | `--count N` | Post up to N oldest approved drafts (default 3) |
| `engage commit` | `--reschedule` | After posting, if approved drafts remain, schedule the next `engage-commit-YYYYMMDD` run for the following day |

### How the loop self-sustains

1. `daily-engage` drafts new content and seeds `engage-commit-YYYYMMDD` for tomorrow.
2. Human reviews queue and approves drafts.
3. The one-shot job fires: posts up to 3, then (if drafts remain) schedules itself again for the day after.
4. Each posting run cleans up yesterday's and today's `engage-commit-*` jobs before re-queuing.
5. If the queue empties, no job is scheduled — the loop pauses until the next `draft --reschedule-post` run creates new drafts.

## Storage Layout

```
~/.alluka/engage/
  queue/     # Draft JSON files (<platform>-<id>-<timestamp>.json)
  history/   # Posted engagement records (hist-<platform>-<id>-<timestamp>.json)
```

Override base directory: `ALLUKA_HOME=/path/to/dir`

## Environment Variables

| Variable             | Default                 | Purpose                            |
|----------------------|-------------------------|------------------------------------|
| `ALLUKA_HOME`        | `~/.alluka`             | Base directory for queue/history   |
| `ENGAGE_PERSONAS_DIR`| `~/nanika/personas`     | Directory containing persona files |

## Draft Queue States

```
pending  →  approved  →  posted
         ↘  rejected
```

- `pending`: Generated by `engage draft`, awaiting review.
- `approved`: Approved by `engage approve`, ready for `engage post`.
- `rejected`: Rejected by `engage reject`, not posted.
- `posted`: Successfully published via the platform CLI.

## Platform CLI Dependencies

| CLI        | Used for                    | Install source             |
|------------|-----------------------------|----------------------------|
| `linkedin` | feed scan, comment posting  | `plugins/linkedin`         |
| `youtube`  | video scan, comment posting | `plugins/youtube`          |
| `reddit`   | feed scan, comment posting  | `plugins/reddit`           |
| `substack` | feed scan, comment posting  | `plugins/substack`         |
| `claude`   | LLM drafting                | `npm install -g @anthropic-ai/claude-code` |

## Examples

**User**: "draft engagement comments for today"
**Action**: `engage scan && engage draft`

**User**: "adapt my blog post for LinkedIn and Reddit"
**Action**: `engage adapt ~/blog-post.md --platforms linkedin,reddit`

**User**: "adapt a URL for Twitter and YouTube"
**Action**: `engage adapt https://example.com/article --platforms x,youtube`

**User**: "review my pending engagement drafts"
**Action**: `engage review`

**User**: "post my approved engagement drafts"
**Action**: `engage commit --count 3`

**User**: "show recent engagement history"
**Action**: `engage history --since 7d`

## Build

```bash
cd plugins/engage
go build -o ~/bin/engage ./cmd/engage-cli
engage --help
```
