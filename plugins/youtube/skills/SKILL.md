---
name: youtube
description: YouTube CLI — scan channels, post comments, like videos, and manage OAuth from the terminal. Use when scanning YouTube channels for recent videos, posting comments, liking videos, or setting up YouTube API credentials.
allowed-tools: Bash(youtube:*)
argument-hint: "[command]"
keywords: youtube, video, social, engagement, comment, like, scan
category: productivity
version: "0.1.0"
---

# youtube

YouTube CLI — scan channels for recent videos, post top-level comments, like videos, and manage OAuth2 credentials. Built on the YouTube Data API v3.

## When to Use

- User wants to scan YouTube channels for recent videos
- User wants to post a comment on a YouTube video
- User wants to like a YouTube video
- User asks about YouTube engagement or wants to check recent videos
- `engage` calls `youtube comment <video-id> <text>` to post a comment
- User wants to set up or verify YouTube API credentials

## Build

```bash
cd plugins/youtube
go build -ldflags "-s -w" -o bin/youtube ./cmd/youtube-cli
ln -sf $(pwd)/bin/youtube ~/bin/youtube
```

Or via plugin.json:
```bash
go build -ldflags "-s -w" -o bin/youtube ./cmd/youtube-cli
ln -sf $(pwd)/bin/youtube ~/bin/youtube
```

## Prerequisites

- **Scanning**: Google Cloud project with YouTube Data API v3 enabled + API key
- **Commenting / liking**: OAuth2 credentials (`youtube auth`)
- **Transcript enrichment** (optional): `uv` + `get_transcript.py` script at `~/nanika/.claude/skills/youtube-transcript/scripts/get_transcript.py`

## Commands

### configure

Set up or update `~/.alluka/youtube-config.json` interactively.

```bash
youtube configure                # Interactive setup
youtube configure show           # Show current config (masked secrets)
youtube configure show --json    # JSON output
```

Config fields:

| Field           | Description                                    |
|-----------------|------------------------------------------------|
| `api_key`       | YouTube Data API v3 key (for scan/search)      |
| `client_id`     | OAuth2 client ID (for comment/like)            |
| `client_secret` | OAuth2 client secret (for comment/like)        |
| `channels`      | List of channel IDs to scan                    |
| `budget`        | Max quota units per day (default: 10000)       |

Example `~/.alluka/youtube-config.json`:
```json
{
  "api_key": "AIzaSy...",
  "client_id": "1234567890-abc.apps.googleusercontent.com",
  "client_secret": "GOCSPX-...",
  "channels": ["UCxxxxxx", "UCyyyyyy"],
  "budget": 5000
}
```

### auth

Set up OAuth2 credentials for posting comments and likes.

```bash
youtube auth                     # Print authorization URL (prompts for code if interactive)
youtube auth --code <code>       # Exchange authorization code for tokens
```

**First-time setup:**
1. Run `youtube auth` — copy the URL and open it in your browser.
2. Grant access, copy the authorization code shown by Google.
3. Run `youtube auth --code <code>` to exchange and save the token.

Token is saved to `~/.alluka/youtube-oauth.json` (0600 permissions). Tokens refresh automatically on next comment/like when expired.

### doctor

Run diagnostic checks — verifies config, API key, OAuth token, channels, and quota usage.

```bash
youtube doctor
youtube doctor --json
```

Checks:
- `binary` — youtube in PATH
- `config` — config file exists and parses
- `api_key` — API key present
- `oauth_token` — token present and not expired
- `channels` — channels configured
- `quota` — today's unit usage vs budget

### scan

Scan configured channels and/or topic queries for recent videos. Returns structured candidate list.

```bash
youtube scan                                      # Scan configured channels (last 24h)
youtube scan --since 7d                           # Videos from last 7 days
youtube scan --topics "go cli,platform eng"       # Search by topic (100 units each)
youtube scan --limit 5                            # Limit results
youtube scan --json                               # Structured JSON output
youtube scan --topics "golang" --limit 10 --json  # Combined
```

**Options:**
- `--limit N` — max candidates to return (default: 20)
- `--since <duration>` — age filter, e.g. `24h`, `7d`, `48h` (default: `24h`)
- `--topics <csv>` — comma-separated search queries; each costs 100 quota units

**JSON output schema:**
```json
[
  {
    "id": "dQw4w9WgXcQ",
    "platform": "youtube",
    "url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    "title": "Video title",
    "body": "Description or transcript (first 3000 chars)",
    "author": "UCxxxxxx",
    "created_at": "2026-03-25T00:00:00Z",
    "meta": {
      "channel_id": "UCxxxxxx",
      "video_id": "dQw4w9WgXcQ"
    }
  }
]
```

**Quota:** 100 units per channel scan or topic query. Stops early when budget is reached.

### comment

Post a top-level comment on a YouTube video. Requires OAuth (`youtube auth`).

```bash
youtube comment dQw4w9WgXcQ "Great video!"
youtube comment https://www.youtube.com/watch?v=dQw4w9WgXcQ "Nice work"
youtube comment dQw4w9WgXcQ "Thanks for this" --json
```

**Quota:** 50 units per comment.

**JSON output:**
```json
{
  "id": "UgyXxxxx",
  "platform": "youtube",
  "url": "https://www.youtube.com/watch?v=dQw4w9WgXcQ&lc=UgyXxxxx",
  "created_at": "2026-03-25T01:26:42Z"
}
```

### like

Like a YouTube video via `videos.rate`. Requires OAuth (`youtube auth`).

```bash
youtube like dQw4w9WgXcQ
youtube like https://youtu.be/dQw4w9WgXcQ
youtube like dQw4w9WgXcQ --json
```

**Quota:** 50 units per like.

## Global Options

- `--json` — structured JSON output (works with all commands)
- `--version` / `-v` — show version
- `--help` / `-h` — show this help

## Quota Reference

YouTube Data API v3 resets daily at midnight Pacific time. Default budget is 10,000 units/day (matches Google's free quota tier).

| Operation              | Units |
|------------------------|-------|
| `scan` (per channel)   | 100   |
| `scan` (per topic)     | 100   |
| `comment`              | 50    |
| `like`                 | 50    |

Quota usage is tracked in `~/.alluka/youtube.db`. Check with `youtube doctor`.

## Configuration Paths

| File                              | Purpose                        |
|-----------------------------------|--------------------------------|
| `~/.alluka/youtube-config.json`   | API key, OAuth creds, channels |
| `~/.alluka/youtube-oauth.json`    | OAuth2 access + refresh tokens |
| `~/.alluka/youtube.db`            | Quota usage tracking           |

Override base directory with `ALLUKA_HOME` environment variable.

## Examples

**User**: "scan my YouTube channels for recent videos"
**Action**: `youtube scan --since 7d --json`

**User**: "comment on this YouTube video"
**Action**: `youtube comment dQw4w9WgXcQ "Great video!"`

**User**: "like that video"
**Action**: `youtube like dQw4w9WgXcQ`

**User**: "check my YouTube setup"
**Action**: `youtube doctor`
