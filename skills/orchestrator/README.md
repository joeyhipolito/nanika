# orchestrator

Multi-agent mission orchestrator. Spawns specialized agents, routes tasks to optimal LLMs, and coordinates work across isolated workspaces. The execution engine behind all Nanika missions.

## Features

- **Task decomposition** — auto-classifies tasks into phases, spawns planner/researcher/writer/reviewer agents
- **Intelligent routing** — routes each phase to the right LLM (Opus for security, Sonnet for code, Gemini Flash for research)
- **Mission files** — run pre-written `.md` mission files with PHASE lines for deterministic execution
- **Workspace isolation** — each mission gets its own directory under `~/.alluka/workspaces/`
- **Domain awareness** — routes tasks to dev, personal, work, creative, or academic contexts
- **Dry-run mode** — preview the full execution plan before committing
- **Metrics** — execution history, success rates, and per-domain statistics
- **Audit system** — LLM-evaluated quality scoring, trend lines, and automated improvement application
- **JSON output** — machine-readable format for scripting

## Installation

### Prerequisites

- Go 1.25 or later
- Claude CLI and Gemini CLI in PATH (for agent spawning)

### Build and Install

```bash
make install        # Build and symlink to ~/bin
orchestrator status # Verify everything works
```

### Build from Source

```bash
make build          # Build for current platform → bin/orchestrator
```

## Configuration

Workspaces and state live under `~/.alluka/`:

```
~/.alluka/
├── workspaces/     # Active and completed mission workspaces
│   └── {domain}/{id}/
│       ├── agents/          # Spawned agent instances
│       ├── shared/
│       │   ├── context/     # Task context files
│       │   ├── messages/    # Inter-agent messages
│       │   ├── artifacts/   # Output files
│       │   └── state.json
│       └── learnings/
├── missions/       # Mission definition files
├── audits.jsonl    # Audit history
└── learnings.db    # Shared SQLite DB
```

No config file required — the CLI reads domain and routing from flags and task content.

## Commands

### Running Tasks

```bash
# Auto-classify and run a task
orchestrator run "research golang error handling best practices"

# Run with an explicit domain
orchestrator run --domain personal "plan my Japan trip"
orchestrator run --domain creative "write a thread about Go generics"

# Run a mission file
orchestrator run ~/.alluka/missions/FEATURE.md

# Preview execution plan without running
orchestrator run --dry-run "build authentication system"

# Research missions (no code review needed)
orchestrator run --no-git --no-review ~/.alluka/missions/RESEARCH.md
```

### Status and Workspaces

```bash
orchestrator status                     # Show active workspaces
orchestrator learn                      # Capture learnings from completed workspaces
orchestrator cleanup                    # Remove completed workspaces
orchestrator cleanup --older 7d         # Remove workspaces older than 7 days
```

### Metrics

```bash
orchestrator metrics                    # Summary of last 7 days
orchestrator metrics --last 10          # Last 10 missions
orchestrator metrics --domain dev       # Filter by domain
orchestrator metrics --status failed    # Show only failed missions
orchestrator metrics --mission <id>     # Detail for a specific mission
orchestrator metrics --days 30          # Last 30 days
orchestrator metrics --json             # JSON output
```

### Audit

```bash
orchestrator audit                              # Audit most recent mission
orchestrator audit 20260228-f12991ff            # Audit by workspace ID
orchestrator audit --last 3                     # Audit 3rd most recent
orchestrator audit --format markdown            # Markdown output

orchestrator audit scorecard                    # Trend lines across all audits
orchestrator audit scorecard --domain dev       # Filter by domain
orchestrator audit scorecard --last 10          # Last 10 audits
orchestrator audit scorecard --format json

orchestrator audit report                       # Display latest saved report
orchestrator audit report 20260228-f12991ff     # Display specific report

orchestrator audit apply 20260228-f12991ff      # Apply recommendations
orchestrator audit apply 20260228-f12991ff --dry-run
```

## Architecture

```
main.go                         # Entry point
cmd/                            # Generator utilities
├── gen-decomposer-prompt/
└── gen-persona-selector-prompt/
internal/
├── cmd/                        # Command implementations
│   ├── run.go                  # Mission execution (plan, route, spawn)
│   ├── audit.go                # LLM quality evaluation
│   ├── metrics.go              # Execution history and statistics
│   ├── status.go               # Active workspace display
│   ├── cleanup.go              # Workspace lifecycle management
│   ├── learn.go                # Learning capture
│   ├── routing.go              # Task classification and LLM routing
│   ├── templates.go            # Agent template definitions
│   ├── daemon.go               # Background execution support
│   └── events.go               # Event log management
├── core/                       # Core domain types
│   ├── types.go                # Mission, Phase, Agent types
│   ├── workspace.go            # Workspace creation and management
│   ├── checkpoint.go           # State persistence
│   ├── runtime.go              # Agent spawning and coordination
│   ├── role.go                 # Persona and role resolution
│   └── template.go             # Agent template rendering
└── metrics/
    └── db.go                   # SQLite metrics persistence
```

### Design decisions

- **PHASE lines over LLM decomposition** — pre-written PHASE lines in mission files bypass the orchestrator's LLM planner, giving deterministic execution order and dependency resolution
- **Isolated workspaces** — each mission runs in its own directory so agents can't interfere with each other or the host repo
- **Least-squares audit scoring** — the scorecard uses regression over historical audits to detect declining trends before they become failures
- **Audit apply loop** — recommendations are turned into file diffs by a second LLM call, snapshotted, and written to persona/SKILL.md files so improvements persist across missions

### Intelligent Routing

| Task Type | Routes To | Why |
|-----------|-----------|-----|
| Security | Claude Opus | Highest accuracy needed |
| Implementation | Claude Sonnet | Best code quality |
| Review | Claude Sonnet | Good analysis |
| Research | Gemini Flash | Fast, cheap |
| Exploration | Gemini Flash | Broad search |
| Reasoning | Gemini Thinking | Deep analysis |
| Creative | Gemini Pro | Good prose |

### Mission File Format

```markdown
# Mission: Title

## Objective
Goal description.

PHASE: discover | OBJECTIVE: Research prior art for X | PERSONA: academic-researcher
PHASE: implement | OBJECTIVE: Build Y | PERSONA: senior-backend-engineer | DEPENDS: discover
PHASE: review | OBJECTIVE: Review implementation | PERSONA: staff-code-reviewer | DEPENDS: implement
```

## Development

```bash
make build              # Build for current platform
make install            # Build and install to ~/bin
make test               # Run unit tests
make clean              # Remove build artifacts
```

## License

MIT License. See [LICENSE](LICENSE) for details.
