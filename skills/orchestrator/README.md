# Go mission orchestrator

This is the mission CLI shipped in the public Nanika repository. It decomposes
tasks, selects personas and runtimes, schedules dependent phases, captures
artifacts and metrics, and runs review gates. This source is experimental;
passing a local build does not establish provider or repository-wide parity.

## Build and install

Keep the full repository: this module uses local replacements for
`../../shared/sdk` and `../../plugins/nen`. From the repository root:

```bash
GOWORK=off make build-orchestrator
./bin/orchestrator --help
./bin/orchestrator run --help
GOWORK=off make install-orchestrator
export PATH="$HOME/.alluka/bin:$PATH"
```

Go 1.25.4 or newer is required by `go.mod`. `make build` inside this module builds
`skills/orchestrator/bin/orchestrator`; its `make install` symlinks that binary
into `~/.alluka/bin`. Root build targets instead use the root `bin/` directory.

Authenticate the provider CLI before executing work. Claude execution needs
`claude`; explicit Codex execution needs `codex`. Gemini CLI is not a default
prerequisite. Optional plugin daemons need not run, but Nen's source module is a
build dependency.

## Run a task or mission

From the repository root:

```bash
mkdir -p "$HOME/.alluka"
./bin/orchestrator --nanika-dir "$PWD" --personas-dir "$PWD/personas" run --no-comment "Describe the task to perform"
./bin/orchestrator --nanika-dir "$PWD" --personas-dir "$PWD/personas" run --no-comment /path/to/mission.md
```

Those commands execute provider work and may modify the selected target. The
explicit repository/persona paths avoid the default `~/nanika` assumption.
`--no-comment` disables automatic completion comments to Linear.

Mission bodies can use pre-decomposed PHASE lines:

```text
PHASE: design | OBJECTIVE: Write the proposed interface and its acceptance checks | PERSONA: architect
PHASE: implement | OBJECTIVE: Implement the agreed interface and record targeted tests | PERSONA: senior-backend-engineer | DEPENDS: design
PHASE: review | OBJECTIVE: Review the implementation and report blocking findings | PERSONA: staff-code-reviewer | DEPENDS: implement
```

See the [decomposer guide](../decomposer/.claude/skills/decomposer/SKILL.md).
`--dry-run` previews execution but natural-language decomposition may still call
a provider. Use `--help` for a command check that does not start a mission.
`--no-review` skips automatic review-phase injection; `--no-git` disables Git
worktree isolation. Use those only when appropriate for the task.

## Runtime and model defaults

Defaults below describe this source snapshot, not current provider availability.
Automatic runtime selection remains Claude; Codex auto-selection is disabled.
Explicit `RUNTIME: codex` on a phase selects the Codex executor.

| Tier | Claude alias | Codex source mapping |
|------|--------------|----------------------|
| think | `opus` | `gpt-5.4` |
| work | `sonnet` | `gpt-5.4` |
| quick | `haiku` | `gpt-5.4` |

Claude effort defaults are high/medium/low by tier; architecture, security, and
review personas use high. Codex effort defaults are xhigh/high/medium, with
architecture/security/review using xhigh. A `--model` override is available.
Check compatibility with your installed provider before a real mission.

The engine also contains `anthropic-api`, `openai-api`, `openrouter`, and
`gemini-api` executors. These are optional direct-API paths with separate provider
configuration; their presence is not a claim of production parity. Consult
`internal/engine` and `run --help` before selecting them.

For phases whose runtime was assigned by policy, precedence is `--runtime`, then
`NANIKA_DEFAULT_RUNTIME`, then `model_tiers.<tier>.runtime` in `config.yaml`, then
the built-in policy. An explicitly authored phase runtime takes precedence.
This version reads the `model_tiers` routing table.

## State and inspection

State normally lives under `~/.alluka` when that directory exists; otherwise
this version falls back to `~/.via`. `ORCHESTRATOR_CONFIG_DIR` takes precedence,
then `ALLUKA_HOME`, then the legacy `VIA_HOME/orchestrator` path.

The tree holds workspaces, missions, metrics, learnings, and audits. Read command
help for the available inspection options:

```bash
orchestrator status --help
orchestrator metrics --help
orchestrator audit --help
orchestrator events --help
```

Cleanup removes local workspace data. Audit apply changes guidance files and may
invoke a provider. Daemon and notification commands require their own setup.

## Development status

```bash
cd skills/orchestrator
GOWORK=off go build ./...
GOWORK=off go test ./...
```

The September public refresh verified the build. Broad tests have baseline
failures in daemon, engine, persona, and preflight packages; report them rather
than treating a partial suite as green. See the
[refresh note](../../docs/OSS-UPDATE-2026-09.md) and
[SDK documentation](../../shared/sdk/README.md).

The Rust orchestrator rewrite and Portal tooling are not shipped here. The Rust
tracker and Dust components are separate projects within this repository.

[MIT license](../../LICENSE).
