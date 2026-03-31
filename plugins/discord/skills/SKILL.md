---
name: discord
description: Send native voice messages and text to Discord channels via the discord CLI. Use when sending audio as a Discord voice message or checking channel config.
allowed-tools: Bash(discord:*)
argument-hint: "send-voice-message --channel <id> --audio <path> | reply --channel <id> --message <text>"
---

# discord — Native Voice Messages & Channel Messaging

Sends audio files as native Discord voice messages (with playback bar and waveform) and text messages to configured channels.

Config: `~/.alluka/channels/discord.json` — same file used by the orchestrator daemon notifier.

## When to Use

- User wants to send an audio file to Discord as a voice message
- User wants to verify Discord channel config is working
- After `elevenlabs generate`, pass the output as a voice message

## Commands

### send-voice-message

Send an audio file as a native Discord voice message. Handles MP3 → OGG/Opus conversion, waveform generation, and the three-step upload flow automatically.

```bash
discord send-voice-message --channel <channel-id> --audio /path/to/audio.mp3
discord send-voice-message --channel <channel-id> --audio /path/to/audio.ogg --json
```

**Supported input formats:** `mp3`, `ogg`, `wav`, `m4a`, `webm` (any format ffmpeg can decode)

**Requirements:** `ffmpeg` in PATH (`brew install ffmpeg`)

### reply

Send a plain text message to a Discord channel.

```bash
discord reply --channel <channel-id> --message "Hello from nanika"
discord reply --channel <channel-id> --message "Mission complete" --json
```

### query

JSON-native subcommands for dashboard and agent use.

```bash
discord query status --json    # bot token status, channel count
discord query items --json     # list configured channel IDs
discord query actions --json   # available CLI actions
```

### doctor

Verify ffmpeg availability and config.

```bash
discord doctor
discord doctor --json
```

## Config Format

`~/.alluka/channels/discord.json`:
```json
{
  "bot_token": "<your-bot-token>",
  "channel_ids": ["<channel-id>"]
}
```

## Examples

**User**: "send this audio to Discord"
**Action**: `discord send-voice-message --channel <id> --audio /path/to/audio.mp3`

**User**: "generate a voice message and send to Discord"
**Action**:
```bash
elevenlabs generate narration.txt --output /tmp/
discord send-voice-message --channel <id> --audio /tmp/narration.mp3
```
