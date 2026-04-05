---
name: channels
description: >
  Telegram and Discord channel integration for the orchestrator. Handles inbound
  messages as orchestrator commands and pushes mission lifecycle events back.
license: MIT
metadata:
  author: joey
  version: "0.1.0"
---

# Channels

Bridges external messaging platforms to the nanika orchestrator via Claude Code Channels.

## Supported Platforms

- **Telegram** — see [telegram/SKILL.md](telegram/SKILL.md) for inbound message routing rules

## Architecture

Two-layer split:

- **Inbound**: Anthropic's official channel plugin forwards messages into your Claude Code session.
  This skill teaches Claude to interpret them as orchestrator commands.
- **Outbound**: The orchestrator daemon pushes mission lifecycle events (started, phase progress,
  completed, failed) directly to Telegram Bot API. Config at `~/.alluka/channels/telegram.json`.

## Voice Message Handling

Inbound voice messages are not transcribed by this skill. If a voice note arrives, ask the user to resend the request as text, or wire up your own transcription CLI and route the output back through normal command handling.

Outbound voice messages (sending a prerecorded audio file *to* a channel) are supported directly by the `discord` and `telegram` plugins via `send-voice-message`.

## Setup

1. Create a Telegram bot via [BotFather](https://t.me/BotFather)
2. Install the official plugin: `/plugin install telegram@claude-plugins-official`
3. Configure: `/telegram:configure <token>`
4. Create `~/.alluka/channels/telegram.json`:
   ```json
   {
     "bot_token": "<your-token>",
     "notify_chat_ids": [<your-chat-id>],
     "events": ["mission.started", "mission.completed", "mission.failed", "phase.failed"]
   }
   ```
5. Launch: `claude --channels plugin:telegram@claude-plugins-official`
