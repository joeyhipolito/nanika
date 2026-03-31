---
name: gmail
description: Reads inbox, triages threads, applies labels, organizes email, and composes/sends email across multiple Gmail accounts via gmail CLI. Use when user asks about email, inbox, reading messages, labeling, follow-up tracking, email triage, sending email, drafts, calendar events, or Google Drive files.
allowed-tools: Bash(gmail:*)
argument-hint: "[subcommand or thread-id]"
keywords: email, gmail, inbox, triage, labels, threads, search, organize, send, reply, draft, compose, calendar, drive, google
category: productivity
version: "0.2.0"
---

# Gmail - Multi-Account Email, Calendar, and Drive

Email triage, search, organization, and composition via the standalone `gmail` CLI tool.
Supports multiple Gmail accounts with a unified inbox view, plus Google Calendar and Drive access.

## When to Use

- User mentions email, inbox, or Gmail
- User wants to read, search, or triage emails
- User asks about unread messages or email threads
- User wants to label, archive, or organize email
- User needs to search across multiple email accounts
- User wants to send an email or reply to a thread
- User wants to create, list, or send drafts
- User asks about upcoming calendar events or wants to create events
- User needs to list, search, or download Google Drive files

## Commands

### Setup

```bash
gmail configure work
gmail configure personal
gmail configure show
gmail configure show --json
gmail accounts
gmail accounts --json
gmail accounts remove work
gmail doctor
gmail doctor --json
```

### Inbox

```bash
gmail inbox
gmail inbox --unread
gmail inbox --limit 50
gmail inbox --account work
gmail inbox --account work --unread --json
```

### Thread Reading

```bash
gmail thread 18abc123 --account work
gmail thread 18abc123 --account work --json
```

### Search

```bash
gmail search "from:boss@company.com"
gmail search "subject:invoice is:unread"
gmail search "after:2026-02-01 label:important"
gmail search "has:attachment" --account work
gmail search "is:unread" --limit 10 --json
gmail search "from:noreply" --account personal --limit 5
```

### Labels

```bash
gmail labels
gmail labels --account work
gmail labels --json
gmail label 18abc123 "Action-Required" --account work
gmail label 18abc123 --remove "FYI" --account work
gmail label --create "FollowUp" --account work
```

### Filters

```bash
gmail filters
gmail filters --account work
gmail filters --json
gmail filter --create --from "sender@example.com" --label "Dev" --account work
gmail filter --create --query "from:noreply@supabase.io" --label "Dev/Infra" --account work
gmail filter --create --from "*.trademe.co.nz" --label "Noise" --archive --account work
gmail filter --create --subject "payment failed" --label "Action/Urgent" --star --account work
gmail filter --create --from "news@example.com" --mark-read --archive --account work
gmail filter --create --has-attachment --label "Has-Attachments" --account work
gmail filter --create --from "alerts@pagerduty.com" --label "Ops" --never-spam --account work
gmail filter --delete ANe1Bmj123 --account work
```

### Mark State

```bash
gmail mark 18abc123 --read --account work
gmail mark 18abc123 --unread --account work
gmail mark 18abc123 --archive --account work
gmail mark 18abc123 --trash --account work
```

### Sending Email

```bash
# Send a new email
gmail send --to recipient@example.com --subject "Hello" "Email body here" --account work

# Send with CC and BCC
gmail send --to to@example.com --cc cc@example.com --bcc bcc@example.com --subject "Hi" "body" --account work

# Send with HTML body (plain text + HTML multipart/alternative)
gmail send --to to@example.com --subject "Hi" "Plain text" --html "<p>HTML version</p>" --account work
```

### Replying to Threads

```bash
gmail reply 18abc123 "Thanks for your message!" --account work
gmail reply 18abc123 "Thanks for your message!" --account work --json
```

### Drafts

```bash
# Create a draft
gmail draft create --to recipient@example.com --subject "Draft subject" "Draft body" --account work

# Create draft with CC and HTML
gmail draft create --to to@example.com --cc cc@example.com --subject "Hi" "body" --html "<p>body</p>" --account work

# List drafts (all accounts)
gmail draft list
gmail draft list --json

# List drafts for one account
gmail draft list --account work
gmail draft list --account work --json

# Send a saved draft
gmail draft send <draft-id> --account work
```

### Calendar

```bash
# List upcoming events (default: 10)
gmail calendar list --account work
gmail calendar list --limit 20 --account work
gmail calendar list --account work --json

# Create an event
gmail calendar create --summary "Team standup" --start 2026-03-25T09:00:00Z --end 2026-03-25T09:30:00Z --account work
gmail calendar create --summary "Client call" --start 2026-03-25T14:00:00Z --end 2026-03-25T15:00:00Z --description "Quarterly review" --location "Zoom" --attendee client@example.com --account work
gmail calendar create --summary "Meeting" --start 2026-03-25T10:00:00Z --end 2026-03-25T11:00:00Z --timezone America/Los_Angeles --account work

# Check free/busy availability
gmail calendar available --start 2026-03-25T09:00:00Z --end 2026-03-25T17:00:00Z --account work
```

### Drive

```bash
# List recent files (default: 10)
gmail drive list --account work
gmail drive list --limit 20 --account work
gmail drive list --account work --json

# Search files by name or content
gmail drive search "Q1 report" --account work
gmail drive search "budget 2026" --limit 5 --account work
gmail drive search "presentation" --account work --json

# Download a file
gmail drive download 1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms --account work
gmail drive download 1BxiMVs0XRA5nFMdKvBdBZjgmUUqptlbs74OgVE2upms --output ~/Downloads/report.pdf --account work
```

## Configuration

Config directory: `~/.gmail/`

```
~/.gmail/
├── config              # client_id, client_secret (shared across accounts)
├── accounts.json       # registered accounts
└── tokens/             # per-account OAuth2 tokens
    ├── work.json
    └── personal.json
```

Credentials can also be supplied via environment variables:

```bash
export GMAIL_CLIENT_ID=your-client-id
export GMAIL_CLIENT_SECRET=your-client-secret
```

The config file takes priority over environment variables when both are present.

Override the config directory:

```bash
export GMAIL_CONFIG_DIR=/custom/path
```

## OAuth Setup

Before configuring accounts, you need Google Cloud OAuth credentials:

1. Go to [console.cloud.google.com](https://console.cloud.google.com) and create or select a project
2. Enable the **Gmail API**, **Google Calendar API**, and **Google Drive API**
3. Create credentials: APIs & Services → Credentials → Create Credentials → OAuth 2.0 Client ID
4. Choose **Desktop application** as the application type
5. Note the **Client ID** and **Client Secret**
6. Run `gmail configure <alias>` and paste them when prompted

The OAuth flow opens a browser window for you to approve access. After approval, the token is saved to `~/.gmail/tokens/<alias>.json` and refreshes automatically.

## OAuth Scopes

| Scope | Purpose |
|-------|---------|
| `gmail.modify` | Read messages, apply/remove labels, archive |
| `gmail.compose` | Create draft messages |
| `gmail.send` | Send messages on your behalf |
| `gmail.settings.basic` | Read and manage filters |
| `calendar` | Read and manage calendar events |
| `drive.readonly` | List, search, and download Drive files |

## Build

```bash
cd plugins/gmail
go build -ldflags "-s -w" -o bin/gmail ./cmd/gmail-cli
ln -sf $(pwd)/bin/gmail ~/bin/gmail
```

## Troubleshooting

| Problem | Solution |
|---------|----------|
| `No accounts configured` | Run `gmail configure <alias>` with your OAuth credentials |
| `token refresh failed` | Re-run `gmail configure <alias>` to re-authorize |
| `State mismatch` during OAuth | Browser was closed mid-flow — run configure again |
| `Insufficient Permission` / 403 | Gmail/Calendar/Drive API not enabled, or wrong scopes — re-run configure |
| `Rate limit` / 429 error | Gmail API quota exceeded — retry automatically happens |
| `gmail doctor` shows token invalid | Token expired — re-run `gmail configure <alias>` |
| Config file not found | Run `gmail configure <alias>` — creates `~/.gmail/config` |

## Multi-Account Usage

- **Unified inbox**: `gmail inbox` merges all accounts, tagged by alias
- **Filter by account**: `gmail inbox --account work`
- **Write ops require --account**: `gmail label`, `gmail mark`, `gmail thread`, `gmail calendar`, `gmail drive`
- **Search all**: `gmail search "query"` searches across all accounts

## Global Flags

| Flag | Description |
|------|-------------|
| `--json` | Output as JSON (machine-readable) |
| `--account <alias>` | Target a specific configured account |
| `--help`, `-h` | Show usage |
| `--version`, `-v` | Show version |

## Examples

**User**: "check my email"
**Action**: `gmail inbox --json`

**User**: "any unread work emails?"
**Action**: `gmail inbox --unread --account work --json`

**User**: "find emails from John about the project"
**Action**: `gmail search "from:john subject:project" --json`

**User**: "archive that thread"
**Action**: `gmail mark <thread-id> --archive --account <alias>`

**User**: "send an email to john@example.com saying the report is ready"
**Action**: `gmail send --to john@example.com --subject "Report ready" "The report is ready for your review." --account work`

**User**: "reply to that thread saying thanks"
**Action**: `gmail reply <thread-id> "Thanks!" --account work`

**User**: "save a draft to john about the meeting"
**Action**: `gmail draft create --to john@example.com --subject "Meeting notes" "Here are the notes from today's meeting." --account work`

**User**: "what's on my calendar this week?"
**Action**: `gmail calendar list --limit 20 --account work --json`

**User**: "schedule a meeting with sarah tomorrow at 2pm"
**Action**: `gmail calendar create --summary "Meeting with Sarah" --start 2026-03-26T14:00:00Z --end 2026-03-26T15:00:00Z --attendee sarah@example.com --account work`

**User**: "find that Q1 budget spreadsheet in Drive"
**Action**: `gmail drive search "Q1 budget" --account work --json`

All commands support `--json` for machine-readable output.
