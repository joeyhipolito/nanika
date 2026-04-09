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

`~/bin/nanika-context`

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

<!-- NANIKA-AGENTS-MD-START -->
[Nanika Skills Index][root: .claude/skills]IMPORTANT: Prefer retrieval-led reasoning over pre-training-led reasoning. Read skill files before making assumptions.

|agent-browser — Browser automation CLI for AI agents:{.claude/skills/agent-browser/SKILL.md}|`agent-browser open https://example.com/form`|`agent-browser snapshot -i`|`agent-browser fill @e1 "user@example.com"`|`agent-browser fill @e2 "password123"`|`agent-browser click @e3`|`agent-browser wait --load networkidle`|`agent-browser snapshot -i`|`agent-browser open https://example.com && agent-browser wait --load networkidle && agent-browser snapshot -i`|`agent-browser fill @e1 "user@example.com" && agent-browser fill @e2 "password123" && agent-browser click @e3`|`agent-browser open https://example.com && agent-browser wait --load networkidle && agent-browser screenshot page.png`|`agent-browser --auto-connect state save ./auth.json`|`agent-browser --state ./auth.json open https://app.example.com/dashboard`|`agent-browser --profile ~/.myapp open https://app.example.com/login`|`agent-browser --profile ~/.myapp open https://app.example.com/dashboard`|
|ai-seo — When the user wants to optimize content for AI search engines, get cited by LLMs, or appear in AI-generated answers:{.claude/skills/ai-seo/SKILL.md}|
|article — Full article pipeline — from topic research to Substack draft:{.claude/skills/article/SKILL.md}|
|better-auth-best-practices — Configure Better Auth server and client, set up database adapters, manage sessions, add plugins, and handle environment variables:{.claude/skills/better-auth-best-practices/SKILL.md}|
|building-native-ui — Complete guide for building beautiful apps with Expo Router:{.claude/skills/building-native-ui/SKILL.md}|
|channels — Telegram and Discord channel integration for the orchestrator:{.claude/skills/channels/SKILL.md}|
|copy-editing — When the user wants to edit, review, or improve existing marketing copy:{.claude/skills/copy-editing/SKILL.md}|
|copywriting — When the user wants to write, rewrite, or improve marketing copy for any page — including homepage, landing pages, pricing pages, feature pages, about pages, or product pages:{.claude/skills/copywriting/SKILL.md}|
|decomposer — Mission decomposition skill — breaks complex tasks into dependency-aware PHASE lines that the orchestrator executes directly, bypassing its internal LLM decomposer:{.claude/skills/decomposer/SKILL.md}|
|golang-cli — Golang CLI application development:{.claude/skills/golang-cli/SKILL.md}|
|golang-design-patterns — Idiomatic Golang design patterns — functional options, constructors, error flow and cascading, resource management and lifecycle, graceful shutdown, resilience, architecture, dependency injection, data handling, and streaming:{.claude/skills/golang-design-patterns/SKILL.md}|
|golang-error-handling — Idiomatic Golang error handling — creation, wrapping with %w, errors.Is/As, errors.Join, custom error types, sentinel errors, panic/recover, the single handling rule, structured logging with slog, HTTP request logging middleware, and samber/oops for production errors:{.claude/skills/golang-error-handling/SKILL.md}|
|golang-testing — Provides a comprehensive guide for writing production-ready Golang tests:{.claude/skills/golang-testing/SKILL.md}|
|orchestrator — Executes tasks and missions via orchestrator CLI:{.claude/skills/orchestrator/SKILL.md}|`orchestrator run "research golang error handling best practices"`|`orchestrator run --domain personal "plan my Japan trip"`|`orchestrator run ~/.alluka/missions/FEATURE.md`|`orchestrator run --dry-run "build authentication system"`|`orchestrator status`|`orchestrator cleanup`|`orchestrator cleanup --older 7d`|`orchestrator metrics`|`orchestrator metrics --last 10`|`orchestrator metrics --domain dev`|`orchestrator metrics --status failed`|`orchestrator metrics --mission <id>`|`orchestrator metrics --days 30`|
|product-marketing-context — When the user wants to create or update their product marketing context document:{.claude/skills/product-marketing-context/SKILL.md}|
|rust-best-practices — Guide for writing idiomatic Rust code based on Apollo GraphQL's best practices handbook:{.claude/skills/rust-best-practices/SKILL.md}|
|social-content — When the user wants help creating, scheduling, or optimizing social media content for LinkedIn, Twitter/X, Instagram, TikTok, Facebook, or other platforms:{.claude/skills/social-content/SKILL.md}|
|supabase-postgres-best-practices — Postgres performance optimization and best practices from Supabase:{.claude/skills/supabase-postgres-best-practices/SKILL.md}|
|vercel-react-best-practices — React and Next.js performance optimization guidelines from Vercel Engineering:{.claude/skills/vercel-react-best-practices/SKILL.md}|
|youtube-transcript — Extract transcripts from YouTube videos:{.claude/skills/youtube-transcript/SKILL.md}|
<!-- NANIKA-AGENTS-MD-END -->
