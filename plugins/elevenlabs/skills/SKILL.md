---
name: elevenlabs
description: ElevenLabs TTS CLI — generate voiceover audio, format narration scripts, run forced alignment, and produce timing maps from the terminal. Use when generating voiceovers, formatting narration scripts for TTS, listing available voices, or assembling audio with silence gaps.
allowed-tools: Bash(elevenlabs:*)
argument-hint: "[script-file|audio-file]"
---

# elevenlabs — ElevenLabs TTS CLI

ElevenLabs TTS CLI for voiceover generation, timing maps, and forced alignment from the terminal.

Config is stored at `~/.alluka/elevenlabs/config`. Override with `ELEVENLABS_CONFIG_DIR`.

## When to Use

- User wants to generate a voiceover from a narration script
- User wants to format a markdown narration script for ElevenLabs TTS
- User wants to list available ElevenLabs voices
- User wants to produce a timing map for audio-visual sync
- User wants to run forced alignment between audio and a transcript
- User wants to assemble final audio with silence gaps inserted

## Commands

### configure

Set API key, default voice ID, and model interactively. Verifies the API key before saving.

```bash
elevenlabs configure
elevenlabs configure show
elevenlabs configure show --json
```

### doctor

Verify API key, quota, connectivity, and config health.

```bash
elevenlabs doctor
elevenlabs doctor --json
```

### voices

List all available voices, marking the configured default.

```bash
elevenlabs voices
elevenlabs voices --json
```

### format

Format a narration script markdown file for ElevenLabs TTS. Strips scene blocks and headers, normalizes numbers and abbreviations, adds v3 audio tags (`[documentary style]`, `[pause]`, `[long pause]`). Writes a sidecar clip-manifest JSON alongside the formatted text.

```bash
elevenlabs format narration-script.md
elevenlabs format narration-script.md --output narration-elevenlabs.txt
```

**Output:**
- `narration-elevenlabs.txt` — formatted TTS input
- `narration-clip-manifest.json` — clip manifest sidecar

### generate

Generate voiceover audio and a timing map from formatted TTS text. Calls the ElevenLabs API and writes audio + `timing-map.json`.

```bash
elevenlabs generate narration-elevenlabs.txt
elevenlabs generate narration-elevenlabs.txt --voice pNInz6obpgDQGcFmaJgB
elevenlabs generate narration-elevenlabs.txt --output ./output/
elevenlabs generate narration-elevenlabs.txt --seed 42 --speed 1.1
elevenlabs generate narration-elevenlabs.txt --format opus_48000_32
```

**Flags:**
- `--voice <id>` — ElevenLabs voice ID (uses config default if not set)
- `--output <path>` — output directory or audio file path
- `--seed <N>` — random seed for reproducible output (0–4294967295)
- `--speed <N>` — speech speed (0.7–1.2, default 1.0)
- `--format <fmt>` — audio format (default: `mp3_44100_128`)

**Output:**
- `<name>-voiceover.mp3` — synthesized audio
- `<name>-timing-map.json` — word-level timing data

### timing

Read a timing-map.json and produce a human-readable clip alignment guide.

```bash
elevenlabs timing timing-map.json
elevenlabs timing timing-map.json --json
```

### assemble

Splice silence gaps into a voiceover using a timing map and clip manifest, producing a final audio file with pauses inserted at the correct positions.

```bash
elevenlabs assemble voiceover.mp3
elevenlabs assemble voiceover.mp3 --output final-voiceover.mp3
```

### align

Run forced alignment on a voiceover audio file against a transcript. Calls the ElevenLabs `/v1/forced-alignment` API and produces clip windows JSON.

```bash
elevenlabs align voiceover.mp3 transcript.txt
elevenlabs align voiceover.mp3 transcript.txt --output narration-clip-windows.json
elevenlabs align voiceover.mp3 transcript.txt --window 10
```

**Flags:**
- `--output <path>` — output path (default: `<audio-dir>/narration-clip-windows.json`)
- `--window <secs>` — window duration in seconds (default: 8)

## Full Voiceover Workflow

```bash
# 1. Configure once
elevenlabs configure

# 2. Format narration script
elevenlabs format narration-script.md

# 3. Generate audio + timing map
elevenlabs generate narration-elevenlabs.txt --voice pNInz6obpgDQGcFmaJgB

# 4. Inspect timing
elevenlabs timing narration-timing-map.json

# 5. Assemble final with pauses
elevenlabs assemble narration-voiceover.mp3

# 6. (Optional) Run forced alignment for clip windows
elevenlabs align narration-voiceover.mp3 transcript.txt
```

## Examples

**User**: "generate a voiceover for my narration script"
**Action**: `elevenlabs format narration-script.md` then `elevenlabs generate narration-elevenlabs.txt`

**User**: "what voices are available"
**Action**: `elevenlabs voices`

**User**: "check my ElevenLabs setup"
**Action**: `elevenlabs doctor`

**User**: "create a timing map from my voiceover"
**Action**: `elevenlabs generate narration-elevenlabs.txt` (timing map is produced automatically)
