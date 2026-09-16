# Public Nanika agent guide

Read [CLAUDE.md](CLAUDE.md) and [CONTRIBUTING.md](CONTRIBUTING.md) before making
changes. Use retrieval from this public checkout to establish behavior; installed
private tools can differ from the source here.

## Checked references

| Task | Reference |
|------|-----------|
| Build, install, or run Rust native execution | [Rust guide](skills/orchestrator-rs/README.md) |
| Build or run the Go mission CLI | [Orchestrator README](skills/orchestrator/README.md) |
| Decompose a mission into PHASE lines | [Decomposer skill](skills/decomposer/.claude/skills/decomposer/SKILL.md) |
| Use the shared Claude Code Go SDK | [SDK README](shared/sdk/README.md) |
| Track local issues | [Tracker skill](plugins/tracker/skills/SKILL.md) |
| Schedule local jobs | [Scheduler skill](plugins/scheduler/skills/SKILL.md) |
| Work with an Obsidian vault | [Obsidian skill](plugins/obsidian/skills/SKILL.md) |
| Use Nen via MCP | [Nen MCP skill](plugins/nen_mcp/skills/SKILL.md) |
| Send authorized Discord messages | [Discord skill](plugins/discord/skills/SKILL.md) |
| Send authorized Telegram messages | [Telegram skill](plugins/telegram/skills/SKILL.md) |
| Work on the desktop/protocol source | [Dust README](plugins/dust/README.md) |

This is a curated public index. Some `.claude/skills` symlinks resolve to absent
sources in a fresh clone; the orchestrator skill symlink is one of them. Use the
README above for CLI behavior. Do not require private skills, repositories,
backlog systems, or local absolute paths to contribute.

## Execution and review

For complex work, use a mission when the installed CLI is available and configured;
verify its version and syntax first. Keep phases bounded, make dependencies
explicit, and review resulting changes. Mission execution and natural-language
planning previews can invoke providers. Do not run them merely to check docs.

Build/test only affected modules and report known baseline failures separately.
Sending messages, changing external systems, and publishing require the user's
authorization. Inspect changes before committing and preserve unrelated work.

The routing-index generator rewrites AGENTS.md and portions of CLAUDE.md from
local skill discovery. Preview it first; regeneration is not required for a
documentation or source-only change.
