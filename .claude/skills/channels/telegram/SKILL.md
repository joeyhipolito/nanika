---
name: telegram-channel
description: Handles inbound Telegram messages as orchestrator commands when running with --channels plugin:telegram@claude-plugins-official
license: MIT
metadata:
  author: joey
  version: "0.1.0"
---

# Telegram Channel Skill

When running as a Telegram channel session, this skill teaches you how to handle
inbound messages from Telegram and route them to orchestrator commands.

## Session Context

You are running as a long-lived Claude Code session with
`--channels plugin:telegram@claude-plugins-official`. Messages arrive as
`<channel source="telegram" ...>` events. You have access to the `reply` tool
to send responses back to Telegram.

## Command Routing

Interpret inbound messages and map them to orchestrator actions:

| User Intent | Action |
|-------------|--------|
| Task-like request ("research X", "build Y", "compare A vs B") | Confirm with user, then `orchestrator run "<task>"` |
| "status" or "what's running" | `orchestrator status` |
| "metrics" or "how did it go" | `orchestrator metrics --last 5` |
| "cancel <id>" or "stop" | `orchestrator cancel <id>` |
| Question about codebase | Answer directly, no orchestrator call |
| Unclear intent | Ask for clarification |

## Response Rules

1. **Keep it short.** Telegram messages render on phone screens. Max 4096 chars per message.
2. **Use monospace for IDs and commands.** Wrap mission IDs, phase names, and CLI output in backtick blocks.
3. **No wide tables.** Use vertical key-value layout instead.
4. **Confirm before launching missions.** Never auto-run `orchestrator run` — always confirm the task with the user first.
5. **Reply immediately with mission ID.** After `orchestrator run`, reply with the mission ID and phase plan. Don't wait for completion — the daemon notifier handles progress updates.
6. **Chunk long output.** If output exceeds 4000 chars, break into multiple replies.

## Example Flows

### Mission Launch
```
User: research the best practices for Go error handling
You: I'll create a mission for that. Running:
     orchestrator run "research Go error handling best practices" --no-review

     Mission 20260322-abc123 started
     3 phases: research → synthesize → write-report
```

### Status Check
```
User: status
You: Active missions:
     `20260322-abc123` — research Go error handling (phase 2/3: synthesize)
     `20260322-def456` — completed 5m ago
```

## Voice Message Handling

When a voice message arrives (attachment with audio MIME type like `audio/ogg`, `audio/mpeg`, `audio/mp4`, `audio/wav`), reply asking the user to resend the request as text. Inbound transcription is not wired up in the base release — plug in your own transcription CLI if you want it, then download with `download_attachment(chat_id, message_id)` and route the transcript through the same command-routing table above.

## What NOT To Do

- Don't run missions without user confirmation
- Don't send raw JSON or unformatted CLI output
- Don't repeat the daemon's push notifications (it handles progress updates separately)
- Don't attempt to edit or cancel missions without explicit user request
