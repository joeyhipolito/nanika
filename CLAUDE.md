# Working in the public Nanika repository

This checkout ships a Rust execution pilot with Go compatibility, a Claude Code Go
SDK, reusable guidance, and the plugins in the root [README](README.md).
See the [Rust guide](skills/orchestrator-rs/README.md) for native command routing,
pinned provider versions, and the remaining rewrite boundaries.

## Source and commands

Read [AGENTS.md](AGENTS.md) for verified skill/reference paths and
[CONTRIBUTING.md](CONTRIBUTING.md) for validation. Build the CLI from the full
repository: `GOWORK=off make build-orchestrator`. Do not use the root bulk build
as a release gate; it still references an absent legacy dashboard.

The Go CLI's shipped defaults use Claude; native Rust code defaults to Codex and review to Claude. Explicit Codex and API runtime support
must be assessed from this checkout's code and `--help`; do not infer defaults
from another installed Nanika version. See the
[orchestrator reference](skills/orchestrator/README.md).

## Mission authoring

A mission can contain dependency-aware PHASE lines:

```text
PHASE: inspect | OBJECTIVE: Identify the affected entrypoints and write a proposed change with validation steps | PERSONA: architect
PHASE: implement | OBJECTIVE: Apply the agreed change and record the targeted test results | PERSONA: senior-backend-engineer | DEPENDS: inspect
PHASE: review | OBJECTIVE: Review the diff and report any blocking regressions with file references | PERSONA: staff-code-reviewer | DEPENDS: implement
```

Use the [decomposer guide](skills/decomposer/.claude/skills/decomposer/SKILL.md)
for the format. Name concrete outputs and acceptance checks. Planning previews
with `run --dry-run` can still invoke decomposition for natural-language tasks;
they are not an offline validation command.

`bash scripts/new-mission.sh <slug>` creates a dated template beneath
`~/.alluka/missions/`. Public contributions and bug reports use GitHub issues;
no private tracker or Linear workspace is required.

## Working boundaries

Inspect current changes before editing and preserve unrelated work. Keep provider
credentials and runtime artifacts out of commits. Review changes and report what
was actually tested. Do not run installers, daemon restarts, publishing commands,
or live-provider tests as routine documentation checks.

The `.claude/skills` directory contains legacy links, including unresolved ones.
Use the checked paths in AGENTS.md rather than assuming all skills auto-discover.
Run `bash scripts/generate-agents-md.sh --dry-run` to inspect what the local
installation sees before considering regeneration.
