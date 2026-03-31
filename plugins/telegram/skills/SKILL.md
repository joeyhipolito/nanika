---
name: telegram
description: Send voice messages and text to Telegram chats via the telegram CLI. Use when sending audio as a Telegram voice message or checking chat config.
allowed-tools: Bash(telegram:*)
argument-hint: "send-voice-message --chat <id> --audio <path> | reply --chat <id> --message <text>"
---

# telegram — Voice Messages & Chat Messaging

Sends audio files as Telegram voice messages (OGG/Opus with duration) and text messages to configured chats.

Config: `~/.alluka/channels/telegram.json` — bot token and optional chat ID list.

## When to Use

- User wants to send an audio file to Telegram as a voice message
- User wants to verify Telegram bot config is working
- After `elevenlabs generate`, pass the output as a voice message

## Commands

### send-voice-message

Send an audio file as a Telegram voice message. Handles conversion to OGG/Opus, waveform generation, and duration extraction automatically.

```bash
telegram send-voice-message --chat <chat-id> --audio /path/to/audio.mp3
telegram send-voice-message --chat <chat-id> --audio /path/to/audio.ogg --json
```

**Supported input formats:** `mp3`, `ogg`, `wav`, `m4a`, `webm` (any format ffmpeg can decode)

**Requirements:** `ffmpeg` in PATH (`brew install ffmpeg`)

### reply

Send a plain text message to a Telegram chat.

```bash
telegram reply --chat <chat-id> --message "Hello from nanika"
telegram reply --chat <chat-id> --message "Mission complete" --json
```

### query

JSON-native subcommands for dashboard and agent use.

```bash
telegram query status --json    # bot token status, chat count
telegram query items --json     # list configured chat IDs
telegram query actions --json   # available CLI actions
```

### doctor

Verify ffmpeg availability, config, and bot API reachability.

```bash
telegram doctor
telegram doctor --json
```

## Config Format

`~/.alluka/channels/telegram.json`:
```json
{
  "bot_token": "<your-bot-token>",
  "chat_ids": ["<chat-id>"]
}
```

Get your bot token from [@BotFather](https://t.me/BotFather). Chat IDs can be found via [@userinfobot](https://t.me/userinfobot) or the Telegram API.

## Examples

**User**: "send this audio to Telegram"
**Action**: `telegram send-voice-message --chat <id> --audio /path/to/audio.mp3`

**User**: "generate a voice message and send to Telegram"
**Action**:
```bash
elevenlabs generate narration.txt --output /tmp/
telegram send-voice-message --chat <id> --audio /tmp/narration.mp3
```
