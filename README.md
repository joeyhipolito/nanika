# Nanika

Nanika is an AI mission orchestrator with a Rust execution pilot and a Go compatibility engine. The Rust-first source installer selects native coding, review, durable execution, cancellation, and streaming observation while retaining Go for existing mission commands. The rewrite remains in progress. See the [Rust guide](skills/orchestrator-rs/README.md) for supported provider versions and current limits.

## Components

| Path | Language | What it is |
|---|---|---|
| `skills/orchestrator-rs` | Rust | Native execution pilot, matching process broker, inspector, and explicit Portal helpers. |
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
python3 scripts/install-rust-orchestrator.py
export PATH="$HOME/.local/bin:$PATH"
orchestrator --engine-info
orchestrator --help
```

Installation builds locally and does not start providers. Use the [Rust guide](skills/orchestrator-rs/README.md) for a first coding run, saved progress streaming, and durable missions. Use `orchestrator --engine go --help` for the legacy CLI.

### Requirements

- Stable Rust/Cargo, Go 1.25.4 or newer, Python 3.9 or newer, Git, and native SQLite development libraries.
- macOS or Linux for the Rust execution pilot.
- Claude CLI installed and authenticated for Claude execution; the Rust adapter currently requires exactly 2.1.269.
- Codex CLI installed and authenticated for Codex execution; the Rust adapter currently requires exactly 0.154.0.
- Keep the checkout intact — components use local relative dependencies (e.g. `../../plugins/nen`, `../../shared/sdk`), so building from a partial copy will fail.
- Shell scripts under `scripts/` require `bash` and `python3`.
- `plugins/tracker` and `dust` need Rust/Cargo; `dust` additionally needs Node/Tauri tooling.

### Go-only build and install

```
GOWORK=off make install-orchestrator
export PATH="$HOME/.alluka/bin:$PATH"
```

This installs the Go-only binary into `~/.alluka/bin`. It is separate from the Rust-first installer; PATH order determines which command runs.

## Go compatibility runtimes

- **Claude (default).** Tier aliases: `think=opus`, `work=sonnet`, `quick=haiku`.
- **Codex (optional, explicit).** Current source maps all tiers to `gpt-5.4`. This is a source-level default, not a recommendation about model availability or fitness. Codex is never auto-selected — you must request it explicitly.
- **API executor (experimental).** Executor names `anthropic-api`, `openai-api`, `openrouter`, and `gemini-api` exist in source. These are not production-parity alternatives to the Claude/Codex CLI runtimes.
- Gemini CLI is not a default prerequisite.

## Go missions and phases

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

`scripts/install.sh --core` builds and installs the orchestrator plus `nen`, `tracker`, and `scheduler`, wiring up symlinks/config and optionally managing daemons. `--all` additionally adds `discord` and `telegram`. These are Go compatibility installers. They can replace a Go entrypoint in `~/.alluka/bin`; they do not manage the new Rust bundle. Keep `~/.local/bin` first on PATH for Rust-first dispatch, or reinstall the Rust bundle after changing command links. Prefer the Rust source installer above for native execution.

`scripts/nanika-update.sh` rebuilds, reinstalls, and restarts the actual plugins on your machine — treat it as an operator action, not a read-only check.

Bulk targets like `make build`/`make setup` currently reference the missing dashboard component; prefer targeted per-component builds (e.g. `make build-orchestrator`) over the bulk targets.

## Validation and known limits

- `shared/sdk`: 69 tests, race-enabled, passing; the orchestrator Go CLI builds cleanly, as of this snapshot.
- Broader orchestrator test suites have baseline failures at this snapshot — do not assume all orchestrator tests are green.
- The old "all skills" CI references paths that have since been retired.
- Dedicated SDK and Rust pilot CI workflows run on Linux and macOS.

## Skills discovery

Skills are tracked in their own directories, with additional links under `.claude/skills`. Some of those legacy links are broken in this public snapshot, so skill discovery is not guaranteed to be automatic — check `AGENTS.md` for the current catalog. The canonical decomposer skill definition is at `skills/decomposer/.claude/skills/decomposer/SKILL.md`.

## Documentation

- [`skills/orchestrator-rs/README.md`](skills/orchestrator-rs/README.md)
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
