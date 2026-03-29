# Nanika

**Self-improving multi-agent orchestrator for Claude Code.** One sentence in. A team of specialists out.

## Skills

Skills live under `skills/` as Go modules. Skill definitions auto-discover from `.claude/skills/`.

## Structure

```
nanika/
├── .claude/skills/          # Skill definitions (auto-discovered)
│   ├── orchestrator/        # Mission execution skill
│   ├── decomposer/          # Mission decomposition skill
│   └── channels/            # Channel integration skill
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

|channels — Telegram and Discord channel integration for the orchestrator:{.claude/skills/channels/SKILL.md}|
|decomposer — Mission decomposition skill — breaks complex tasks into dependency-aware PHASE lines that the orchestrator executes directly, bypassing its internal LLM decomposer:{.claude/skills/decomposer/SKILL.md}|
|orchestrator — Executes tasks and missions via orchestrator CLI:{.claude/skills/orchestrator/SKILL.md}|`orchestrator run "research golang error handling best practices"`|`orchestrator run --domain personal "plan my Japan trip"`|`orchestrator run ~/.alluka/missions/FEATURE.md`|`orchestrator run --dry-run "build authentication system"`|`orchestrator status`|`orchestrator learn`|`orchestrator cleanup`|`orchestrator cleanup --older 7d`|`orchestrator metrics`|`orchestrator metrics --last 10`|`orchestrator metrics --domain dev`|`orchestrator metrics --status failed`|`orchestrator metrics --mission <id>`|`orchestrator metrics --days 30`|
|nen — Self-improvement scanners (Shu, Gyo, Ko, En, Ryu): evaluate health, diagnose failures, analyze costs:{plugins/nen/SKILL.md}|`shu evaluate`|`shu propose`|`shu review`|`ko evaluate`|`nen-daemon start`|`nen-daemon status`|
|discord — Send native voice messages and text to Discord channels via the discord CLI:{plugins/discord/skills/SKILL.md}|`discord send-voice-message --channel <channel-id> --audio /path/to/audio.mp3`|`discord send-voice-message --channel <channel-id> --audio /path/to/audio.ogg --json`|`discord reply --channel <channel-id> --message "Hello from nanika"`|`discord reply --channel <channel-id> --message "Mission complete" --json`|`discord query status --json`|`discord query items --json`|`discord query actions --json`|`discord doctor`|`discord doctor --json`|
|scheduler — Schedules and runs cron jobs and the nanika publishing pipeline via scheduler CLI:{plugins/scheduler/skills/SKILL.md}|`scheduler doctor`|`scheduler doctor --json`|`scheduler daemon`|`scheduler daemon --notify`|`scheduler daemon --once`|`scheduler daemon --stop`|`scheduler daemon >> ~/.alluka/logs/scheduler.log 2>&1 &`|`scheduler init`|`scheduler init --force`|`scheduler init`|`scheduler daemon`|`scheduler jobs`|`scheduler jobs add --name "daily-backup" --cron "0 2 * * *" --command "tar czf /tmp/backup.tgz ~/docs"`|`scheduler jobs add --name "health-check" --cron "*/5 * * * *" --command "curl -s localhost:8080/health"`|
|telegram — Send voice messages and text to Telegram chats via the telegram CLI:{plugins/telegram/skills/SKILL.md}|`telegram send-voice-message --chat <chat-id> --audio /path/to/audio.mp3`|`telegram send-voice-message --chat <chat-id> --audio /path/to/audio.ogg --json`|`telegram reply --chat <chat-id> --message "Hello from nanika"`|`telegram reply --chat <chat-id> --message "Mission complete" --json`|`telegram query status --json`|`telegram query items --json`|`telegram query actions --json`|`telegram doctor`|`telegram doctor --json`|
|tracker — Local issue tracker with hierarchical relationships, blocking links, and priority-based ready detection:{plugins/tracker/skills/SKILL.md}|`tracker create "Task title"`|`tracker create "Task" --priority P0 --description "Details"`|`tracker create "Subtask" --parent trk-ABC1`|`tracker create "Task" --labels "backend,urgent" --assignee me`|`tracker show trk-ABC1`|`tracker list`|`tracker list --status open`|`tracker list --priority P0`|`tracker update trk-ABC1 --status in-progress`|`tracker update trk-ABC1 --priority P0 --assignee alice`|`tracker delete trk-ABC1`|`tracker link trk-ABC1 trk-XYZ2 --type blocks`|`tracker link trk-ABC1 trk-XYZ2 --type relates_to`|`tracker unlink trk-ABC1 trk-XYZ2 --type blocks`|
<!-- NANIKA-AGENTS-MD-END -->
