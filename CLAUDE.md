# Nanika

**Self-improving multi-agent orchestrator for Claude Code.** One sentence in. A team of specialists out.

## Skills

Skills live under `skills/` as Go modules. Skill definitions auto-discover from `.claude/skills/`.

## Structure

```
nanika/
├── .claude/skills/          # Skill definitions (auto-discovered)
│   ├── orchestrator/        # Mission execution skill
│   └── decomposer/          # Mission decomposition skill
├── skills/                  # Skill source code (Go modules)
│   ├── orchestrator/        # Multi-agent mission orchestrator
│   └── decomposer/          # PHASE line format (knowledge-only, no binary)
├── personas/                # Persona markdown files
├── scripts/                 # generate-agents-md.sh, new-mission.sh
└── docs/                    # Skill standard, persona standard
```

## Docs

- `docs/SKILL-STANDARD.md` — Skill conventions
- `docs/PERSONA-STANDARD.md` — Persona conventions
- `.claude/skills/*/SKILL.md` — Individual skill references

## Backlog And Mission Rules

- Linear team `V` / `nanika` is the canonical backlog for status, priority, and execution queue
- runtime mission files live under `~/.alluka/missions/`
- use `~/nanika/scripts/new-mission.sh <slug>` when creating a new mission
- use the `linear` CLI for routine backlog updates

### Writing Mission Files

Always use pre-decomposed PHASE lines to get deterministic execution. Without PHASE lines, the orchestrator's LLM decomposer decides the plan — and it makes mistakes (e.g., injecting code review phases into research missions, wrong dependency ordering).

Reference the decomposer skill for the full format: `.claude/skills/decomposer/SKILL.md`

Quick reference:
```
PHASE: <name> | OBJECTIVE: <concrete deliverable> | PERSONA: <persona> | SKILLS: <skill1,skill2> | DEPENDS: <phase1,phase2>
```

OBJECTIVES must name verifiable success criteria, not directives. Vague objectives are the top driver of gate-failure retries (76% of retry waste in a 7d audit — see `~/.alluka/workspaces/20260414-9ec71068/workers/data-analyst-phase-2/retry-waste-breakdown.md`). Every retry on an Opus phase wastes $3–5.

- ❌ `OBJECTIVE: Verify the implementation and close out tracker issues`
- ✅ `OBJECTIVE: Verify by (1) running <binary> and confirming the three behaviors listed in TRK-XXX, (2) closing the issue via \`tracker update TRK-XXX --status done\`. Success = tracker list shows status=done and binary output matches the expected checklist.`

Rule of thumb: if another worker couldn't tell whether the phase succeeded without re-reading the whole codebase, the objective isn't specific enough.

### Before Running

Always dry-run first:
```bash
orchestrator run <mission-file> --dry-run --verbose
```

Verify:
- No unexpected review phases on research/non-code missions (use `--no-review` if auto-injected)
- Dependencies are correct (phases that need prior output have DEPENDS)
- Persona assignments match the work type

### Research Missions

Research missions that produce no code should use `--no-review`. The auto-injected review phase assumes code output and will review the entire repo instead of the research findings.

```bash
orchestrator run <mission-file> --no-git --no-review
```

### Code Missions

Code missions benefit from the review phase. Let it auto-inject or add explicitly:
```
PHASE: implement | OBJECTIVE: ... | PERSONA: senior-backend-engineer
PHASE: review | OBJECTIVE: Review the implementation for correctness and test coverage | PERSONA: staff-code-reviewer | DEPENDS: implement
```

## Utility Scripts

### nanika-context

`~/bin/nanika-context` (source: `plugins/orchestrator/scripts/nanika-context.sh`)

Prints a system-state snapshot to stdout for manual paste into a Claude session. Runs without spawning any missions or changing state.

```bash
nanika-context              # all sections: learnings, scheduler, tracker, nen
nanika-context learnings    # recent learnings only (cold-start quality ranking)
nanika-context scheduler    # scheduler jobs + recent failures
nanika-context tracker      # open P0/P1 tracker issues
nanika-context nen          # nen-daemon stats + shu health score
```

Sections:
- **learnings** — top 15 learnings by quality score via `orchestrator hooks inject-context`
- **scheduler** — full jobs table + any FAILED entries from the last 50 history events
- **tracker** — `tracker list --status open --priority P0/P1`
- **nen** — `nen-daemon status` + `shu query status`

### orchestrator hooks preflight

Assembles a full operational brief from all registered sections and prints it to stdout. This is the **SessionStart hook** — it runs automatically at the start of every Claude session.

```bash
# Manual invocation (same as SessionStart hook)
orchestrator hooks preflight

# Specific sections only
orchestrator hooks preflight --sections learnings,tracker

# Adjust byte budget
orchestrator hooks preflight --max-bytes 12288

# Machine-readable (no truncation)
orchestrator hooks preflight --format json

# Suppress (CI / automated sessions)
NANIKA_NO_INJECT=1 orchestrator hooks preflight
```

Sections in priority order (highest first): `scheduler` → `tracker` → `learnings` → `nen` → `mission`. When the 6 KB default budget is exceeded, lowest-priority sections are dropped first.

**Manual smoke test:**
```bash
# 1. Verify output is non-empty and under 6 KB
orchestrator hooks preflight | wc -c

# 2. Confirm section headers are present
orchestrator hooks preflight | grep "^### "

# 3. Confirm opt-out works (must print 0)
NANIKA_NO_INJECT=1 orchestrator hooks preflight | wc -c

# 4. Confirm JSON round-trips
orchestrator hooks preflight --format json | jq '.blocks | length'
```

### orchestrator hooks inject-context

Prints learnings only — use inside agent worker prompts that need targeted context without the full operational brief.

```bash
orchestrator hooks inject-context --limit 15 --max-bytes 4096
```

`preflight` is the right choice for SessionStart. `inject-context` is for targeted learnings-only injection inside worker sessions.

### orchestrator dream

Mines Claude Code session transcripts for durable learnings. Walks `~/.claude/projects/` JSONL files, chunks multi-turn conversations into token-bounded windows, and extracts decisions, insights, patterns, and gotchas into the learnings DB using Haiku.

**Three dedup layers prevent flooding the DB:**
1. File-level sha256 — skip unchanged transcripts entirely
2. Chunk-level sha256 — skip re-extraction for identical conversation windows
3. Cosine similarity at 0.85 inside `learning.DB.Insert` — final semantic gate

Worker sessions (cwd inside `~/.alluka/worktrees/`) are excluded — their output is already captured live during phase execution.

```bash
# Run one extraction cycle (processes up to 20 files by default)
orchestrator dream run

# Only look at transcripts modified in the last 24 hours
orchestrator dream run --since 24h

# Filter to a specific session by path substring
orchestrator dream run --session my-project

# Reprocess everything (ignores file hash cache)
orchestrator dream run --force

# Preview what would run without calling Haiku or writing to the DB
orchestrator dream run --dry-run --verbose

# Show processed file/chunk counts
orchestrator dream status

# Clear processing state so the next run re-mines all transcripts
# (does not remove any learnings already stored)
orchestrator dream reset
```

**Relationship to the learning pipeline:**
- `orchestrator dream run` → extracts learnings → writes to `learnings.db`
- `orchestrator hooks preflight` → reads `learnings.db` → injects top learnings into every SessionStart
- `orchestrator hooks inject-context` → same DB, targeted injection inside worker sessions

Dream is the **offline consolidation path**. The live path (`CaptureWithFocus` during phase execution) captures worker session output in real time; dream catches everything else — interactive sessions, experiments, one-off scripts.


### nanika-update

`scripts/nanika-update.sh` (source: `scripts/nanika-update.sh`)

Builds, installs, restarts daemons, and verifies all plugins discovered via `plugin.json`. Runs four phases per plugin: `build → install → restart → verify`. Daemon restart is only performed for `scheduler` and `nen`.

```bash
# Build and install all plugins, restart scheduler and nen daemons, verify binaries
scripts/nanika-update.sh

# Preview what would run without executing anything
scripts/nanika-update.sh --dry-run

# Update only the discord and telegram plugins
scripts/nanika-update.sh --only discord,telegram

# Update everything except dust (slow Rust build)
scripts/nanika-update.sh --skip dust

# Machine-readable output — pipe to jq for status checks
scripts/nanika-update.sh --json | jq '[.[] | select(.status != "ok")]'

# Dry-run in JSON mode — useful for CI diff previews
scripts/nanika-update.sh --dry-run --json
```

<!-- NANIKA-AGENTS-MD-START -->
[Nanika Skills Index][root: .claude/skills]IMPORTANT: Prefer retrieval-led reasoning over pre-training-led reasoning. Read skill files before making assumptions.

|academic-voice — When the user wants to revise academic prose — research proposals, thesis chapters, coursework, literature reviews, methodology sections — so it reads as their own evaluative thinking rather than templated synthesis:{.claude/skills/academic-voice/SKILL.md}|
|agent-browser — Browser automation CLI for AI agents:{.claude/skills/agent-browser/SKILL.md}|
|ai-seo — When the user wants to optimize content for AI search engines, get cited by LLMs, or appear in AI-generated answers:{.claude/skills/ai-seo/SKILL.md}|
|article — Full article pipeline — from topic research to Substack draft:{.claude/skills/article/SKILL.md}|
|better-auth-best-practices — Configure Better Auth server and client, set up database adapters, manage sessions, add plugins, and handle environment variables:{.claude/skills/better-auth-best-practices/SKILL.md}|
|bft — Byzantine-fault-tolerant plan review loop:{.claude/skills/bft/SKILL.md}|
|building-native-ui — Complete guide for building beautiful apps with Expo Router:{.claude/skills/building-native-ui/SKILL.md}|
|channels — Telegram and Discord channel integration for the orchestrator:{.claude/skills/channels/SKILL.md}|
|copy-editing — When the user wants to edit, review, or improve existing marketing copy:{.claude/skills/copy-editing/SKILL.md}|
|copywriting — When the user wants to write, rewrite, or improve marketing copy for any page — including homepage, landing pages, pricing pages, feature pages, about pages, or product pages:{.claude/skills/copywriting/SKILL.md}|
|decomposer — Mission decomposition skill — breaks complex tasks into dependency-aware PHASE lines that the orchestrator executes directly, bypassing its internal LLM decomposer:{.claude/skills/decomposer/SKILL.md}|
|docx — Use this skill whenever the user wants to create, read, edit, or manipulate Word documents (.docx files):{.claude/skills/docx/SKILL.md}|
|golang-cli — Golang CLI application development:{.claude/skills/golang-cli/SKILL.md}|
|golang-design-patterns — Idiomatic Golang design patterns — functional options, constructors, error flow and cascading, resource management and lifecycle, graceful shutdown, resilience, architecture, dependency injection, data handling, and streaming:{.claude/skills/golang-design-patterns/SKILL.md}|
|golang-error-handling — Idiomatic Golang error handling — creation, wrapping with %w, errors.Is/As, errors.Join, custom error types, sentinel errors, panic/recover, the single handling rule, structured logging with slog, HTTP request logging middleware, and samber/oops for production errors:{.claude/skills/golang-error-handling/SKILL.md}|
|golang-testing — Provides a comprehensive guide for writing production-ready Golang tests:{.claude/skills/golang-testing/SKILL.md}|
|orchestrator — Executes tasks and missions via orchestrator CLI:{.claude/skills/orchestrator/SKILL.md}|
|pdf — Use this skill whenever the user wants to do anything with PDF files:{.claude/skills/pdf/SKILL.md}|
|product-marketing-context — When the user wants to create or update their product marketing context document:{.claude/skills/product-marketing-context/SKILL.md}|
|rust-best-practices — Guide for writing idiomatic Rust code based on Apollo GraphQL's best practices handbook:{.claude/skills/rust-best-practices/SKILL.md}|
|session-handoff — Generates a structured handoff summary of the current session so the user can /clear and paste it into a fresh session to keep momentum without carrying token bloat:{.claude/skills/session-handoff/SKILL.md}|
|social-content — When the user wants help creating, scheduling, or optimizing social media content for LinkedIn, Twitter/X, Instagram, TikTok, Facebook, or other platforms:{.claude/skills/social-content/SKILL.md}|
|supabase-postgres-best-practices — Postgres performance optimization and best practices from Supabase:{.claude/skills/supabase-postgres-best-practices/SKILL.md}|
|vercel-react-best-practices — React and Next.js performance optimization guidelines from Vercel Engineering:{.claude/skills/vercel-react-best-practices/SKILL.md}|
|youtube-transcript — Extract transcripts from YouTube videos:{.claude/skills/youtube-transcript/SKILL.md}|
|discord — Send native voice messages and text to Discord channels via the discord CLI:{plugins/discord/skills/SKILL.md}|
|elevenlabs — ElevenLabs TTS CLI — generate voiceover audio, format narration scripts, run forced alignment, and produce timing maps from the terminal:{plugins/elevenlabs/skills/SKILL.md}|
|engage — Cross-platform comment engagement CLI — scan, draft, review, approve, and post comments across YouTube, LinkedIn, Reddit, and Substack:{plugins/engage/skills/SKILL.md}|
|gmail — Reads inbox, triages threads, applies labels, organizes email, and composes/sends email across multiple Gmail accounts via gmail CLI:{plugins/gmail/skills/SKILL.md}|
|linkedin — LinkedIn CLI — publish posts, read feed, comment, react, and automate engagement from the terminal:{plugins/linkedin/skills/SKILL.md}|
|nen_mcp — MCP server for the nen ability system:{plugins/nen_mcp/skills/SKILL.md}|
|obsidian — Obsidian vault CLI — read, write, search, capture, triage, enrich, and ingest into the vault at ~/.alluka/vault/:{plugins/obsidian/skills/SKILL.md}|
|reddit — Reddit CLI — submit posts, read feeds, comment, vote, and search from the terminal using browser cookies:{plugins/reddit/skills/SKILL.md}|
|scheduler — Schedules and runs cron jobs and the nanika publishing pipeline via scheduler CLI:{plugins/scheduler/skills/SKILL.md}|
|scout — Gathers intelligence on configurable topics via scout CLI:{plugins/scout/skills/SKILL.md}|
|substack — Substack CLI — publish posts, manage drafts, post notes, comment, and automate feed engagement from the terminal:{plugins/substack/skills/SKILL.md}|
|telegram — Send voice messages and text to Telegram chats via the telegram CLI:{plugins/telegram/skills/SKILL.md}|
|tracker — Local issue tracker with hierarchical relationships, blocking links, and priority-based ready detection:{plugins/tracker/skills/SKILL.md}|
|ynab — YNAB CLI — manage budgets, transactions, categories, and accounts from the terminal via the YNAB API:{plugins/ynab/skills/SKILL.md}|
|youtube — YouTube CLI — scan channels, post comments, like videos, and manage OAuth from the terminal:{plugins/youtube/skills/SKILL.md}|
<!-- NANIKA-AGENTS-MD-END -->
