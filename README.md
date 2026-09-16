# Nanika

Nanika is a Go mission orchestrator: it decomposes a task into phases and runs each phase through an AI coding agent. The default execution runtime is Claude Code. An explicit Codex runtime is also supported, and experimental direct API executors are present in source. This repository does **not** include the Rust orchestrator rewrite or Portal — those are not part of this public checkout.

## Components

| Path | Language | What it is |
|---|---|---|
| `skills/orchestrator` | Go | Mission orchestrator CLI. Depends on local `../../plugins/nen` and `../../shared/sdk`; must be built from a full clone. |
| `skills/decomposer` | knowledge-only | Has a `go.mod` but no Go packages; decomposition guidance, not a runnable binary. |
| `shared/sdk` | Go | Claude CLI SDK library used by the orchestrator. |
| `plugins/nen` | Go | Health checks and evals. |
| `plugins/tracker` | Rust | Issue-tracking CLI. |
| `plugins/scheduler` | Go | Cron-style scheduling. |
| `plugins/discord`, `plugins/telegram` | Go | Chat channel integrations. |
| `plugins/nen_mcp` | Go | MCP server. |
| `plugins/obsidian` | Go | Vault CLI/indexer. |
| `plugins/dust` | Tauri + Rust + React | Experimental desktop app and protocol source. |

Not present in this checkout: a Wails-based `plugins/dashboard`, and `scout`, `gmail`, `engage`, `social` plugins, or `example-hello`/`example-bookmarks`. Optional plugin runtimes don't need to run for the orchestrator to work, but the `nen` and SDK **source** are local build dependencies for the orchestrator binary.

## Quickstart

```
git clone https://github.com/joeyhipolito/nanika.git
cd nanika
GOWORK=off make build-orchestrator
./bin/orchestrator --help
mkdir -p "$HOME/.alluka"
./bin/orchestrator --nanika-dir "$PWD" --personas-dir "$PWD/personas" run --no-comment "YOUR TASK"
```

The `--help` step does not start a mission; the build writes a local binary. The final `run` command starts provider work and may edit your selected target — only run it against a task/target you intend to change.

Use the orchestrator's actual `--help` output for flags; the flags above (`--nanika-dir`, `--personas-dir`, `run --no-comment`) are the ones used in this quickstart, not an exhaustive list.

### Requirements

- Go 1.25.4 or newer, per `go.mod`.
- Claude CLI installed and authenticated, for Claude Code execution.
- Codex CLI installed and authenticated, if you use the Codex runtime.
- Keep the checkout intact — components use local relative dependencies (e.g. `../../plugins/nen`, `../../shared/sdk`), so building from a partial copy will fail.
- Shell scripts under `scripts/` require `bash` and `python3`.
- `plugins/tracker` and `dust` need Rust/Cargo; `dust` additionally needs Node/Tauri tooling.

### Optional install

```
GOWORK=off make install-orchestrator
export PATH="$HOME/.alluka/bin:$PATH"
```

This installs the orchestrator binary into `~/.alluka/bin`.

## Runtimes

- **Claude (default).** Tier aliases: `think=opus`, `work=sonnet`, `quick=haiku`.
- **Codex (optional, explicit).** Current source maps all tiers to `gpt-5.4`. This is a source-level default, not a recommendation about model availability or fitness. Codex is never auto-selected — you must request it explicitly.
- **API executor (experimental).** Executor names `anthropic-api`, `openai-api`, `openrouter`, and `gemini-api` exist in source. These are not production-parity alternatives to the Claude/Codex CLI runtimes.
- Gemini CLI is not a default prerequisite.

## Missions and phases

Mission files live under `~/.alluka/missions`. `scripts/new-mission.sh` writes a dated template you edit. Each phase line has the form:

```
PHASE: <name> | OBJECTIVE: <deliverable> | PERSONA: <persona> | DEPENDS: <prior-phase> | RUNTIME: <runtime>
```

`DEPENDS` and `RUNTIME` are optional. Built-in runtime policy defaults to Claude;
configuration or explicit routing can override it. Example personas: `architect`, `senior-backend-engineer`, `staff-code-reviewer`, `technical-writer`. Example:

```
PHASE: design | OBJECTIVE: Draft the API contract for the export endpoint | PERSONA: architect
PHASE: implement | OBJECTIVE: Implement the export endpoint per the design | PERSONA: senior-backend-engineer | DEPENDS: design
PHASE: review | OBJECTIVE: Review the implementation for correctness and style | PERSONA: staff-code-reviewer | DEPENDS: implement
PHASE: docs | OBJECTIVE: Write user-facing docs for the export endpoint | PERSONA: technical-writer | DEPENDS: implement | RUNTIME: claude
```

Note that `--dry-run` previews are not guaranteed to be a pure offline operation for natural-language tasks: decomposition of a free-text objective can itself call an LLM.

## Legacy install scripts

`scripts/install.sh --core` builds and installs the orchestrator plus `nen`, `tracker`, and `scheduler`, wiring up symlinks/config and optionally managing daemons. `--all` additionally adds `discord` and `telegram`. These scripts are not needed for a minimal CLI setup — prefer the targeted `make build-orchestrator` path above unless you want the full daemon-managed install.

`scripts/nanika-update.sh` rebuilds, reinstalls, and restarts the actual plugins on your machine — treat it as an operator action, not a read-only check.

Bulk targets like `make build`/`make setup` currently reference the missing dashboard component; prefer targeted per-component builds (e.g. `make build-orchestrator`) over the bulk targets.

## Validation and known limits

- `shared/sdk`: 69 tests, race-enabled, passing; the orchestrator Go CLI builds cleanly, as of this snapshot.
- Broader orchestrator test suites have baseline failures at this snapshot — do not assume all orchestrator tests are green.
- The old "all skills" CI references paths that have since been retired.
- A dedicated SDK CI workflow runs on Linux and macOS.

## Skills discovery

Skills are tracked in their own directories, with additional links under `.claude/skills`. Some of those legacy links are broken in this public snapshot, so skill discovery is not guaranteed to be automatic — check `AGENTS.md` for the current catalog. The canonical decomposer skill definition is at `skills/decomposer/.claude/skills/decomposer/SKILL.md`.

## Documentation

- [`skills/orchestrator/README.md`](skills/orchestrator/README.md)
- [`shared/sdk/README.md`](shared/sdk/README.md)
- [`scripts/README.md`](scripts/README.md)
- [`CONTRIBUTING.md`](CONTRIBUTING.md)
- [`docs/SKILL-STANDARD.md`](docs/SKILL-STANDARD.md)
- [`docs/PERSONA-STANDARD.md`](docs/PERSONA-STANDARD.md)
- [`docs/PLUGIN-PROTOCOL.md`](docs/PLUGIN-PROTOCOL.md)
- [`docs/EVENT-BUS.md`](docs/EVENT-BUS.md)
- [`docs/OSS-UPDATE-2026-09.md`](docs/OSS-UPDATE-2026-09.md)
- [`plugins/dust/README.md`](plugins/dust/README.md)

## License

MIT — see [`LICENSE`](LICENSE).
