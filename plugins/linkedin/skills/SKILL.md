---
name: linkedin
description: LinkedIn CLI — publish posts, read feed, comment, react, and automate engagement from the terminal. Use when posting to LinkedIn, reading the feed, commenting on posts, reacting, or running automated engagement.
allowed-tools: Bash(linkedin:*)
argument-hint: "[command]"
keywords: linkedin, social, feed, post, engage, comment, react
category: productivity
version: "1.0.0"
---

# linkedin

LinkedIn CLI — publish posts, read feed, comment, react, and automate engagement from the terminal.

## When to Use

- User wants to publish a post or article to LinkedIn
- User wants to read their LinkedIn feed
- User wants to comment on or react to a LinkedIn post
- User asks about their recent LinkedIn posts
- User wants to run automated feed engagement with Claude
- `engage` calls `linkedin comment <urn> <text>` to post a comment

## Build

```bash
cd plugins/linkedin
go build -ldflags "-s -w" -o bin/linkedin ./cmd/linkedin-cli
ln -sf $(pwd)/bin/linkedin ~/bin/linkedin
```

Or via plugin.json:
```bash
# build
go build -ldflags "-s -w" -o bin/linkedin ./cmd/linkedin-cli
# install symlink
ln -sf $(pwd)/bin/linkedin ~/bin/linkedin
```

## Prerequisites

- **Posting / commenting / reacting**: OAuth setup via `linkedin configure`
- **Feed reading**: Chrome with remote debugging running on port 9222 (`linkedin chrome --launch`)
- **Engage command**: `claude` CLI in PATH (uses Claude SDK for comment drafting)

## Commands

### configure

Set up OAuth authentication (opens browser for authorization).

```bash
linkedin configure
linkedin configure show           # Show current config (masked secrets)
linkedin configure chrome         # Test Chrome CDP connection
```

### doctor

Run diagnostic checks — verifies OAuth token, config file, Chrome CDP connection, and LinkedIn session.

```bash
linkedin doctor
linkedin doctor --json
```

### chrome

Print or launch the Chrome remote debugging command.

```bash
linkedin chrome                   # Print the launch command
linkedin chrome --launch          # Actually launch Chrome
```

Chrome sessions persist across restarts in `~/.chrome-linkedin`.

### post

Create a LinkedIn post (text, image, or from MDX file).

```bash
linkedin post "Hello LinkedIn!"
linkedin post "Check this out" --image photo.jpg
linkedin post --file article.mdx
linkedin post --file article.mdx --image cover.jpg
linkedin post "Draft post" --visibility CONNECTIONS
linkedin post "Hello" --json       # Returns post ID and URL as JSON
```

**Options:**
- `--image <path>` — attach an image
- `--file <mdx>` — create post from MDX file (strips JSX components, applies 3000-char limit)
- `--visibility PUBLIC|CONNECTIONS` — default: PUBLIC

### posts

List your recent posts.

```bash
linkedin posts
linkedin posts --limit 5
linkedin posts --json
```

**Options:**
- `--limit N` — number of posts to return (default: 10)

### feed

Read your LinkedIn feed via Chrome CDP (requires Chrome with remote debugging).

```bash
linkedin feed
linkedin feed --limit 20
linkedin feed --json
```

**Options:**
- `--limit N` — number of feed items (default: 10)

### comments

Read comments on a post by URN.

```bash
linkedin comments urn:li:activity:1234567890
linkedin comments 1234567890
linkedin comments urn:li:activity:1234567890 --json
```

### comment

Post a comment on a LinkedIn post.

```bash
linkedin comment urn:li:activity:1234567890 "Great post!"
linkedin comment 1234567890 "Thanks for sharing"
```

### react

React to a LinkedIn post.

```bash
linkedin react 1234567890                         # Default: LIKE
linkedin react 1234567890 --type CELEBRATE
linkedin react 1234567890 --type EMPATHY
linkedin react 1234567890 --type INTEREST
linkedin react 1234567890 --type APPRECIATION
linkedin react 1234567890 --type ENTERTAINMENT
```

### engage

Automated feed engagement — scans feed, scores posts, drafts comments with Claude, and optionally posts. **Dry-run by default.**

```bash
linkedin engage                                         # Dry-run: scan, score, draft — print results
linkedin engage --post                                  # Actually post comments and reactions
linkedin engage --persona ~/nanika/personas/founder.md  # Use persona voice
linkedin engage --posts-file ~/.linkedin/substack-posts.json  # Ground comments in articles
linkedin engage --site-url https://yourname.substack.com # Article link base URL
linkedin engage --max-comments 2 --max-reacts 5
linkedin engage --json
```

**Decision thresholds:**
- relevance ≥ 7 → grounded comment referencing a Substack article
- interest ≥ 6 → opinion comment
- interest ≥ 4 → react only
- else → skip

**State file:** `~/.linkedin/engaged.json` (auto-prunes after 30 days, prevents re-engaging same posts)

**Article grounding setup:**
```bash
substack posts --json > ~/.linkedin/substack-posts.json
```

## Global Options

- `--json` — output in JSON format (works with all commands)
- `--version` / `-v` — show version
- `--help` / `-h` — show help

## Configuration

Config stored at `~/.linkedin/config` (or `$LINKEDIN_CONFIG_DIR/config`):

```
client_id=<LinkedIn app client ID>
client_secret=<LinkedIn app client secret>
access_token=<OAuth access token>
token_expiry=<RFC3339 expiry time>
person_urn=urn:li:person:<ID>
chrome_debug_url=http://localhost:9222
```

Create a LinkedIn app at [linkedin.com/developers/apps](https://www.linkedin.com/developers/apps) with:
- **Sign In with LinkedIn using OpenID Connect** product enabled
- **Share on LinkedIn** product enabled
- Redirect URI set to `http://localhost:8484/callback`

## Examples

**User**: "post this article to LinkedIn"
**Action**: `linkedin post --file article.mdx`

**User**: "what's in my LinkedIn feed"
**Action**: `linkedin feed --limit 20 --json`

**User**: "comment on that LinkedIn post"
**Action**: `linkedin comment <urn> "Your comment text"`

**User**: "run LinkedIn engagement"
**Action**: `linkedin engage` (dry-run), then `linkedin engage --post` to publish
