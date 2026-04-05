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

<!-- NANIKA-AGENTS-MD-START -->
[Nanika Skills Index][root: .claude/skills]IMPORTANT: Prefer retrieval-led reasoning over pre-training-led reasoning. Read skill files before making assumptions.

|agent-browser — Browser automation CLI for AI agents:{.claude/skills/agent-browser/SKILL.md}|`agent-browser open https://example.com/form`|`agent-browser snapshot -i`|`agent-browser fill @e1 "user@example.com"`|`agent-browser fill @e2 "password123"`|`agent-browser click @e3`|`agent-browser wait --load networkidle`|`agent-browser snapshot -i`|`agent-browser open https://example.com && agent-browser wait --load networkidle && agent-browser snapshot -i`|`agent-browser fill @e1 "user@example.com" && agent-browser fill @e2 "password123" && agent-browser click @e3`|`agent-browser open https://example.com && agent-browser wait --load networkidle && agent-browser screenshot page.png`|`agent-browser --auto-connect state save ./auth.json`|`agent-browser --state ./auth.json open https://app.example.com/dashboard`|`agent-browser --profile ~/.myapp open https://app.example.com/login`|`agent-browser --profile ~/.myapp open https://app.example.com/dashboard`|
|better-auth-best-practices — Configure Better Auth server and client, set up database adapters, manage sessions, add plugins, and handle environment variables:{.claude/skills/better-auth-best-practices/SKILL.md}|
|building-native-ui — Complete guide for building beautiful apps with Expo Router:{.claude/skills/building-native-ui/SKILL.md}|
|channels — Telegram and Discord channel integration for the orchestrator:{.claude/skills/channels/SKILL.md}|
|decomposer — Mission decomposition skill — breaks complex tasks into dependency-aware PHASE lines that the orchestrator executes directly, bypassing its internal LLM decomposer:{.claude/skills/decomposer/SKILL.md}|
|orchestrator — Executes tasks and missions via orchestrator CLI:{.claude/skills/orchestrator/SKILL.md}|`orchestrator run "research golang error handling best practices"`|`orchestrator run --domain personal "plan my Japan trip"`|`orchestrator run ~/.alluka/missions/FEATURE.md`|`orchestrator run --dry-run "build authentication system"`|`orchestrator status`|`orchestrator learn`|`orchestrator cleanup`|`orchestrator cleanup --older 7d`|`orchestrator metrics`|`orchestrator metrics --last 10`|`orchestrator metrics --domain dev`|`orchestrator metrics --status failed`|`orchestrator metrics --mission <id>`|`orchestrator metrics --days 30`|
|supabase-postgres-best-practices — Postgres performance optimization and best practices from Supabase:{.claude/skills/supabase-postgres-best-practices/SKILL.md}|
|vercel-react-best-practices — React and Next.js performance optimization guidelines from Vercel Engineering:{.claude/skills/vercel-react-best-practices/SKILL.md}|
|youtube-transcript — Extract transcripts from YouTube videos:{.claude/skills/youtube-transcript/SKILL.md}|
|discord — Send native voice messages and text to Discord channels via the discord CLI:{plugins/discord/skills/SKILL.md}|`discord send-voice-message --channel <channel-id> --audio /path/to/audio.mp3`|`discord send-voice-message --channel <channel-id> --audio /path/to/audio.ogg --json`|`discord reply --channel <channel-id> --message "Hello from nanika"`|`discord reply --channel <channel-id> --message "Mission complete" --json`|`discord query status --json`|`discord query items --json`|`discord query actions --json`|`discord doctor`|`discord doctor --json`|
|scheduler — Schedules and runs cron jobs via scheduler CLI:{plugins/scheduler/skills/SKILL.md}|`scheduler doctor`|`scheduler doctor --json`|`scheduler daemon`|`scheduler daemon --notify`|`scheduler daemon --once`|`scheduler daemon --stop`|`scheduler daemon >> ~/.alluka/logs/scheduler.log 2>&1 &`|`scheduler init`|`scheduler init --force`|`scheduler init`|`scheduler daemon`|`scheduler jobs`|`scheduler jobs add --name "daily-backup" --cron "0 2 * * *" --command "tar czf /tmp/backup.tgz ~/docs"`|`scheduler jobs add --name "health-check" --cron "*/5 * * * *" --command "curl -s localhost:8080/health"`|
|telegram — Send voice messages and text to Telegram chats via the telegram CLI:{plugins/telegram/skills/SKILL.md}|`telegram send-voice-message --chat <chat-id> --audio /path/to/audio.mp3`|`telegram send-voice-message --chat <chat-id> --audio /path/to/audio.ogg --json`|`telegram reply --chat <chat-id> --message "Hello from nanika"`|`telegram reply --chat <chat-id> --message "Mission complete" --json`|`telegram query status --json`|`telegram query items --json`|`telegram query actions --json`|`telegram doctor`|`telegram doctor --json`|
|nen_mcp — MCP server exposing nanika internal state — query findings, learnings, missions, proposals, ko verdicts, scheduler jobs, and tracker issues via Claude MCP tools:{plugins/nen_mcp/skills/SKILL.md}|`nanika_findings {}`|`nanika_findings { "severity": "high" }`|`nanika_learnings { "domain": "dev" }`|`nanika_mission { "status": "failure" }`|`nanika_events { "mission_id": "msn_..." }`|`nanika_tracker_issues { "status": "open", "priority": "P0" }`|`nanika_scheduler_jobs { "enabled_only": true }`|`nanika_proposals {}`|`nanika_ko_verdicts { "config": "persona-drift" }`|
|tracker — Local issue tracker with hierarchical relationships, blocking links, and priority-based ready detection:{plugins/tracker/skills/SKILL.md}|`tracker create "Task title"`|`tracker create "Task" --priority P0 --description "Details"`|`tracker create "Subtask" --parent trk-ABC1`|`tracker create "Task" --labels "backend,urgent" --assignee me`|`tracker show trk-ABC1`|`tracker list`|`tracker list --status open`|`tracker list --priority P0`|`tracker update trk-ABC1 --status in-progress`|`tracker update trk-ABC1 --priority P0 --assignee alice`|`tracker delete trk-ABC1`|`tracker link trk-ABC1 trk-XYZ2 --type blocks`|`tracker link trk-ABC1 trk-XYZ2 --type relates_to`|`tracker unlink trk-ABC1 trk-XYZ2 --type blocks`|
<!-- NANIKA-AGENTS-MD-END -->
