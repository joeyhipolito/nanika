---
name: reddit
description: Reddit CLI — submit posts, read feeds, comment, vote, and search from the terminal using browser cookies. Use when posting to Reddit, reading subreddits, replying to threads, searching posts, or automating Reddit engagement.
allowed-tools: Bash(reddit:*)
argument-hint: "[subreddit|post-id|query]"
---

# Reddit CLI — Skill Reference

Reddit CLI for reading feeds, submitting posts, commenting, voting, and searching. Uses browser cookies for authentication.

## When to Use

- User wants to post to a subreddit
- User wants to read their home feed or a specific subreddit
- User wants to search Reddit for posts on a topic
- User wants to reply to or comment on a post
- User wants to upvote or downvote a post or comment
- User asks about Reddit activity or wants to list their posts
- `engage` calls `reddit comment <id> <text>` to post a comment

## Commands

### configure

```bash
reddit configure cookies                    # Extract cookies from Chrome
reddit configure cookies --from-browser firefox  # Extract from Firefox
reddit configure show                       # Show current config
reddit configure show --json
```

Config stored at `~/.reddit/config`.

### doctor

```bash
reddit doctor                               # Verify cookies and connectivity
reddit doctor --json
```

### post

```bash
reddit post --subreddit golang --title "Title" "body text"
reddit post --subreddit golang --title "Title" --url https://example.com
```

**Flags:**
- `--subreddit, -s <sub>` — target subreddit (required)
- `--title, -t <title>` — post title (required)
- `--url, -u <url>` — link URL (link post instead of text post)

### posts

```bash
reddit posts                                # List your recent posts
reddit posts --limit 25
reddit posts --json
```

### feed

```bash
reddit feed                                 # Home feed (hot)
reddit feed --subreddit golang              # Subreddit feed
reddit feed --sort new                      # Sort: hot/new/top/rising
reddit feed --limit 20
reddit feed --json
```

### comments

```bash
reddit comments <post-id>                   # Comment tree for a post
reddit comments <post-id> --limit 50
reddit comments <post-id> --sort best
reddit comments <post-id> --json
```

Sort options: `best`, `top`, `new`, `controversial`, `old`, `qa`.

### comment

```bash
reddit comment <post-or-comment-id> "reply text"
reddit comment t3_abc123 "Great post!"
reddit comment <id> "text" --json
```

Replies to a post (`t3_`) or existing comment (`t1_`). Prefix auto-detected if omitted.

### vote

```bash
reddit vote <post-or-comment-id>            # Upvote (default)
reddit vote <id> --down                     # Downvote
reddit vote <id> --unvote                   # Remove vote
reddit vote <id> --json
```

### search

```bash
reddit search "golang cli tools"
reddit search "query" --subreddit programming
reddit search "query" --sort top --time week
reddit search "query" --limit 10 --json
```

**Flags:**
- `--subreddit, -s <sub>` — restrict to a subreddit
- `--sort` — `relevance` (default), `new`, `top`, `comments`
- `--time` — `hour`, `day`, `week`, `month`, `year`, `all` (default)
- `--limit N` — max results (default: 25)

### query

```bash
reddit query status --json                  # Auth/config status
reddit query items --json                   # Configured items (username)
reddit query actions --json                 # Available actions
```

## Configuration

Config file: `~/.reddit/config` (0600 permissions)

| Field | Description |
|-------|-------------|
| `reddit_session` | Main session cookie |
| `csrf_token` | CSRF protection token |
| `username` | Authenticated username (auto-detected) |

## Quick Start

```bash
reddit configure cookies           # Extract cookies from Chrome
reddit doctor                      # Verify setup
reddit feed --subreddit golang     # Read a subreddit
```

## Examples

**User**: "post to r/golang about my new CLI tool"
**Action**: `reddit post --subreddit golang --title "New CLI tool for X" "Description here"`

**User**: "what's trending on r/programming"
**Action**: `reddit feed --subreddit programming --sort hot --json`

**User**: "search reddit for Go error handling best practices"
**Action**: `reddit search "go error handling best practices" --sort top --time year`

**User**: "reply to that reddit post"
**Action**: `reddit comment <post-id> "Your reply text"`
