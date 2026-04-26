<p align="center">
  <img src="assets/logo.png" alt="Nanika" width="200" />
</p>

<h1 align="center">Nanika</h1>

<p align="center"><em>One sentence in. A team of specialists out.</em></p>
<p align="center">A self-improving multi-agent orchestrator for Claude Code.<br>
<sub>Experimental Codex support available — not fully tested.</sub></p>

```
You: "research AI agent memory systems and write a report"

  orchestrator decomposes the task:
    PHASE: research    | PERSONA: architect           | OBJECTIVE: Compare 5 agent memory approaches
    PHASE: write       | PERSONA: technical-writer    | OBJECTIVE: Draft the report | DEPENDS: research
    PHASE: review      | PERSONA: staff-code-reviewer | OBJECTIVE: Review for accuracy | DEPENDS: write

  3 specialized workers execute in dependency order.
  Each worker is a Claude Code (or Codex) session with a persona prompt.
  Results flow between phases. Review gates enforce quality.
  Nen observers watch for anomalies. Findings feed back into self-improvement.
```

## How It Works

### Mission Execution

Everything flows through the **orchestrator** — a Go CLI that turns natural language tasks into multi-phase, multi-agent missions.

```
task → decompose → plan → spawn workers → collect artifacts → review → done
```

1. **Decompose** — the task is broken into PHASE lines with personas, dependencies, and objectives. You can pre-decompose (deterministic) or let the LLM decide.
2. **Route** — each phase is assigned a model tier (think/work/quick) and runtime (Claude Code or Codex).
3. **Spawn** — workers execute in parallel where dependencies allow. Each worker gets a persona prompt, skill access, and a workspace.
4. **Gate** — review phases enforce quality. If a review fails, the engine can inject fix + re-review cycles.
5. **Learn** — mission metrics (duration, retries, failures) are stored. Nen observers analyze patterns.

### Event Bus

The orchestrator daemon emits structured events to JSONL files and a Unix domain socket. Any subscriber can watch:

```
orchestrator daemon  →  events.sock (UDS)  →  nen-daemon (scanners)
                     →  events/*.jsonl     →  dashboard (UI)
                                           →  discord/telegram (notifications)
```

Plugins are **subscribers, not dependencies**. The orchestrator runs fine without any of them installed.

### Self-Improvement (Nen)

Named after *Hunter x Hunter's* Nen abilities. The system watches itself and gets better:

| Ability | Role | How |
|---------|------|-----|
| **Shu** | Broad sweep | Evaluates all component health scores, flags degradation |
| **Gyo** | Observe + diagnose | Watches mission metrics, detects anomalies (z-score), answers *why* things failed |
| **Ko** | Eval engine | Promptfoo-compatible YAML test runner — runs assertions against LLM output to verify prompt quality |
| **En** | System health | Binary freshness, workspace hygiene, daemon reachability |
| **Ryu** | Cost analysis | Surfaces cost trends, model efficiency gaps, retry waste, minimal-output phases |
| **Zetsu** | Suppress exposure | Strips untrusted input at trust boundaries so workers are invisible to injection |

The loop: **Shu** finds "decomposer accuracy dropped" → **Gyo** diagnoses "persona mis-routing on implementation tasks" → **Ko** re-runs evals, verifies the regression → you fix the prompt, **Ko** confirms scores improve.

Gyo, En, and Ryu run automatically via `nen-daemon` while missions execute. Run `shu evaluate` for broad sweeps and `ko evaluate` for targeted eval suites — manually or on a cron.

When findings exceed severity thresholds, `shu propose` auto-generates remediation missions and tracker issues. You approve via `shu review`, and the scheduler dispatches approved missions automatically.

> **Want heartbeat-style proactivity?** Use the scheduler to run any command on a cron — `scheduler jobs add --name "check-inbox" --cron "*/30 * * * *" --command "your-script"`. Nanika's self-improvement is findings-driven rather than timer-driven, but the scheduler gives you both.

### Plugin Protocol

Every plugin exposes a uniform query interface so the dashboard (and other plugins) can discover and render them without knowing implementation details:

```bash
<plugin> query status --json   →  { "status": "ok", ... }
<plugin> query items --json    →  { "items": [...], "count": N }
<plugin> query actions --json  →  { "actions": [{ "name", "command", "description" }] }
```

Declared in `plugin.json`. The dashboard polls these to render plugin cards, and plugins with `"ui": true` ship custom React components.

### Personas

Workers aren't generic — each gets a persona that defines expertise, tone, and methodology:

```
PHASE: design    | PERSONA: architect               | OBJECTIVE: Define the API contract
PHASE: implement | PERSONA: senior-backend-engineer  | OBJECTIVE: Build the service
PHASE: review    | PERSONA: security-auditor         | OBJECTIVE: Audit auth flow | DEPENDS: implement
```

10 included: `academic-researcher` · `architect` · `data-analyst` · `devops-engineer` · `qa-engineer` · `security-auditor` · `senior-backend-engineer` · `senior-frontend-engineer` · `staff-code-reviewer` · `technical-writer`

## Architecture

```
┌────────────────────────────────────────────────────────┐
│  Claude Code  (reads CLAUDE.md → discovers skills)     │
├────────────────────────────────────────────────────────┤
│  Orchestrator                                          │
│  ┌─────────────┐  decomposes task into phases          │
│  │ decomposer  │  assigns personas + dependencies      │
│  └─────────────┘  spawns workers (Claude Code / Codex) │
│         │                                              │
│         ▼  workers call plugins via SKILL.md           │
├────────────────────────────────────────────────────────┤
│  Plugins  (CLIs in ~/bin, via plugin.json)             │
│                                                        │
│  nen ········ self-improvement (Shu, Gyo, Ko, En, Ryu) │
│  tracker ···· local issue tracking (Rust)              │
│  scheduler ·· cron jobs + dispatch loop                │
│  discord ···· channel notifications + voice messages   │
│  telegram ··· channel notifications + voice messages   │
│         ▲                                              │
│         │  subscribe to events                         │
├────────────────────────────────────────────────────────┤
│  Event Bus  (JSONL files + UDS socket)                 │
│  orchestrator emits → nen, dashboard, channels consume │
├────────────────────────────────────────────────────────┤
│  ~/.alluka/                                            │
│  missions/ · workspaces/ · metrics.db · findings.db    │
└────────────────────────────────────────────────────────┘
```

**Skills** are the brain — orchestration and planning:
- **orchestrator** — multi-agent mission execution engine with daemon, event bus, quality gates
- **decomposer** — breaks tasks into dependency-aware PHASE lines (knowledge-only, no binary)

**Plugins** are the hands — domain-specific CLIs that skills invoke:
- **Core**: **nen** (self-improvement scanners + eval engine)
- **Recommended**: **scout** (intelligence gathering), **obsidian** (vault CLI), **tracker** (issue tracking, Rust), **scheduler** (cron + publishing), **gmail** (multi-account), **engage** (cross-platform comments)
- **Optional**: **linkedin**, **youtube**, **reddit**, **substack**, **elevenlabs** (TTS), **ynab** (budgets), **dashboard** (macOS Spotlight overlay, Wails)
- **Channels**: **discord** / **telegram** — notifications + native voice messages
- **Examples**: **example-hello** / **example-bookmarks** — starter plugins for learning the system

## Quick Start

```bash
git clone https://github.com/joeyhipolito/nanika
cd nanika
scripts/install.sh
```

The installer is interactive — checks prerequisites, lets you pick plugins, builds and installs, then runs doctor checks on everything it installed.

```bash
scripts/install.sh                        # Interactive — pick what to install
scripts/install.sh --core                 # Core only (orchestrator + nen + tracker + scheduler)
scripts/install.sh --all                  # Core + discord, telegram, dashboard
scripts/install.sh --plugins discord      # Core + specific plugins
scripts/install.sh --no-interactive       # CI: core only, no prompts
scripts/install.sh --dry-run              # Show what would be installed
scripts/install.sh --repair              # Re-check prereqs, rebuild broken plugins
```

Open in Claude Code — it reads `CLAUDE.md` and discovers all skills automatically:

```bash
cd nanika
claude
# "research golang error handling best practices and write a report"
```

## Scripts

```bash
scripts/install.sh              # Interactive installer
scripts/new-mission.sh <slug>   # Create a mission file in ~/.alluka/missions/
scripts/generate-agents-md.sh   # Regenerate the AGENTS.md routing index
scripts/nanika-update.sh        # Build, install, and verify all plugins; restart daemons
```

After adding a plugin or skill, run `generate-agents-md.sh` to update the routing index so the orchestrator can discover it.

### Install Skills

Workers automatically use installed [Claude Code skills](https://skill.sh) during missions. More skills = smarter workers.

After installing a skill, regenerate the routing index so workers can discover it:

```bash
scripts/generate-agents-md.sh
```

## Building Your Own Plugin

A plugin needs three things:

1. **A CLI binary** — Go or Rust. Callable from the shell.
2. **A `plugin.json`** — name, build command, install command, query protocol.
3. **A `skills/SKILL.md`** — tells Claude Code when and how to invoke it.

```
plugins/my-plugin/
├── plugin.json             # Build, install, query declarations
├── skills/SKILL.md         # Claude Code skill definition
├── cmd/my-plugin/main.go   # CLI entry point
└── go.mod
```

After creating your plugin, register it:

```bash
scripts/generate-agents-md.sh   # Updates AGENTS.md + CLAUDE.md routing index
make build-plugin-my-plugin     # Build the binary
make install-plugin-my-plugin   # Install to ~/.alluka/bin/
```

See [SKILL-STANDARD.md](docs/SKILL-STANDARD.md) for the full specification.

## Extending Nanika

- **[Plugin Protocol](docs/PLUGIN-PROTOCOL.md)** — Full reference for `plugin.json`, the query protocol, dashboard microfrontend contract, and custom UI bundles.
- **[Event Bus](docs/EVENT-BUS.md)** — How to subscribe to orchestrator events (mission lifecycle, phase completions, anomalies) via the JSONL log or Unix domain socket.

## Requirements

| Dependency | Version | Required for |
|-----------|---------|-------------|
| Go | >= 1.25 | Skills and most plugins |
| Claude Code | latest | Agent integration |
| Rust/Cargo | latest | `tracker` plugin (optional) |
| Node.js | >= 22 | `dashboard` plugin (optional) |
| Wails | v2 | `dashboard` plugin (optional) |

The installer checks only what you need based on selected plugins.

## Uninstall

```bash
make uninstall              # Stop daemons, remove launchd plists
make clean                  # Remove build artifacts
rm -rf ~/.alluka/bin/{orchestrator,shu,gyo,en,ryu,tracker,scheduler,discord,telegram}  # Remove binaries
rm -rf ~/.alluka/           # Remove all runtime data (missions, databases, logs)
```

## The Name

**Nanika** (ナニカ) and **Alluka** are from *Hunter x Hunter*. Alluka is the vessel; Nanika is the wish-granting intelligence inside.

- **`nanika/`** — the intelligence layer (skills, routing, orchestration)
- **`~/.alluka/`** — the vessel (runtime state, missions, metrics, findings)

The Nen abilities (Shu, Gyo, Ko, En, Ryu, Zetsu) are also HxH references — each maps to a real self-improvement capability.

## License

MIT
