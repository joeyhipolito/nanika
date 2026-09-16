# Rust orchestrator

This workspace ships the experimental Rust orchestrator used for local coding,
review, authored missions, recovery, cancellation, and observation. The public
source installation selects Rust for supported commands and retains the Go CLI
for existing plain-text missions and other compatibility commands. The rewrite
is still in progress; source availability does not imply full Go parity.

## Install from source

From the full repository root on macOS or Linux, with stable Rust/Cargo, Go
1.25.4 or newer, Python 3.9 or newer, Git, and native SQLite development libraries:

```sh
python3 scripts/install-rust-orchestrator.py
export PATH="$HOME/.local/bin:$PATH"
orchestrator --engine-info
orchestrator --engine rust --help
orchestrator --engine go --help
```

The installer builds the pilot, its matching process broker, output/usage
helpers, and the Go compatibility binary. It installs a content-addressed bundle
under `~/.local/libexec/nanika-orchestrator/` and activates links in `~/.local/bin`.
Use `--prefix /another/prefix` to choose another installation. An unmanaged binary
or link at a destination is preserved and installation refuses; select another
prefix or move that entry yourself. Existing Nanika daemons/configuration are
not migrated. Check `command -v orchestrator` to ensure another PATH entry does
not shadow this installation.

Upgrades retain old bundles and print `previous_dispatcher`. That path can be
invoked directly to use the previous paired engines, or relink
`<prefix>/bin/orchestrator` to it for rollback. The manifest records hashes,
platform, source revision, and whether the checkout was dirty. This is a local
source build, not a signed/notarized binary distribution or a publisher signature.
`--profile dev` provides a faster debug build. No provider runs during install.

## Command routing

| Command | Engine |
|---|---|
| `code`, `review`, `resume`, `observe`, `view` | Rust |
| `run`, `status`, `cancel` with native selectors such as `--repo` or `--output-dir` | Rust |
| Existing plain-text missions, global status, metrics, audit, daemon | Go |
| `--engine rust ...` / `--engine go ...` | Explicit selection |

The dispatcher reports Go selection on stderr. Selection happens before execution;
a Rust failure is returned directly and never retried through Go. Use `orchestrator --engine rust ...` for an explicit native invocation; the
dispatcher supplies the pilot opt-in and resolves the matching broker location.

## Providers and coding

The native adapters validate **Codex CLI 0.154.0** and **Claude Code 2.1.269**.
Other executable versions are refused. Install/authenticate a supported provider
separately and pass its executable explicitly when your ordinary CLI differs.
Native `code` defaults to Codex; Claude coding requires `--runtime claude`.
Native `review` defaults to Claude. Authored/durable `run` and `resume` currently
support Codex only. Go compatibility has its own Claude-first routing defaults.

```sh
orchestrator code --runtime claude --claude /path/to/claude-2.1.269 \
  --model sonnet --repo /path/to/clean/repo --prompt-file /path/to/task.md \
  --output-dir /path/to/fresh/result 2>/path/to/progress.jsonl
```

The source repository must be clean. The provider works in a tracked-source copy
under `<output>/workspace`; the original source is checked for preservation.
Read `changes.diff`, `answer.md`, and `pilot-result.json`. `code` does not run
verification, and never claims tests passed. Review changes before applying them.

Claude coding is restricted to `Read,Edit,Write,Glob,Grep`, with no Bash, MCP,
plugins, browser, hooks, subagents, or network tool access. Five-turn attempts,
protocol validation, output budgets, and permission denials are enforced.
A provider success message cannot override an earlier rejected tool or denial.
Claude review is tool-less. These boundaries are intentionally narrower than a
normal interactive provider session.

## Authored execution and recovery

Native missions accept dependency-aware PHASE files and an explicit verifier:

```sh
orchestrator run --durable --repo /path/to/clean/repo \
  --mission-file /path/to/mission.md --output-dir /path/to/fresh/run \
  --codex /path/to/codex-0.154.0 -- /path/to/verifier
orchestrator status --output-dir /path/to/run
orchestrator resume --output-dir /path/to/run
```

The verifier must produce recognized test results with a positive executed-test
count for a passing gate; a bare successful exit is insufficient. Durable state
belongs to the private output directory. Native status reads saved state without
starting a provider and does not prove a process is currently alive.
For a live durable owner, read the exact `mission_id` from `manifest.json`, then:

```sh
orchestrator cancel --output-dir /path/to/run --mission exact-mission-id
```

Cancellation acceptance means the owner durably recorded the request; cleanup
and terminal publication can still be pending. Unsupported or ambiguous ownership
is refused. No PID-based cancellation or global Go-state migration is implied.

## Streaming and usage

Native code/review/run emit JSONL progress on stderr. Save that stream as above:

```sh
orchestrator observe --progress-log /path/to/progress.jsonl --follow
orchestrator view --progress-log /path/to/progress.jsonl --follow
```

`observe` also supports `--format json`; `view` requires an interactive terminal.
The viewer shows provider/tool events, phases, usage, and explicit losses when
present. `q` detaches without stopping the producer. Following a log is not proof
of worker liveness. Field-name redaction cannot detect every secret in prose;
keep provider artifacts private when appropriate.

Standalone Claude code/review streams `worker.usage` snapshots correlated by
session and message. Replace previous snapshots for that key; do not sum updates.
Missing counters remain null. Any provider output-channel loss disables further
live usage decoding for that attempt. Final `worker-usage.json` and
`worker-usage-events.jsonl` are independently derived from captured output;
telemetry is best-effort and does not decide execution acceptance.

`orchestrator-usage-replay` analyzes saved Claude JSONL offline. For an explicit
command-output experiment:

```sh
orchestrator-output --portal on --log /path/to/new/full.log -- command args
orchestrator-output --portal off --log /path/to/another/new/full.log -- command args
```

An optional `--output-cap off|on` before `--log` overrides the helper's cap.
Omitting it follows `--portal`. `--portal on --output-cap off` returns raw bytes
and records requested/effective modes separately; `--portal off --output-cap on`
is contradictory and refused before execution or artifact creation. This control
applies only to the explicit helper, not automatic provider integration.

Both modes retain a full fresh log; ON returns a bounded structured result,
OFF returns uncapped captured output after command completion. The helper is
explicit: automatic provider tool-output caps, independent summary controls, and
matched provider savings benchmarks remain development work. Portal is not
silently enabled by installation, and Claude's tool permissions are unchanged.

## Development and validation

From `skills/orchestrator-rs`:

```sh
cargo build --locked -p orchestrator-first-use-pilot --bins \
  -p orchestrator-process --bin orchestrator-process-broker
cargo test --locked -p orchestrator-first-use-pilot
cargo test --locked -p orchestrator-provider-claude --features experimental-first-use-pilot
cargo fmt --all --check
cargo clippy --locked -p orchestrator-first-use-pilot --all-targets -- -D warnings
```

Build the broker before tests on macOS. Public CI runs these commands on macOS
and Linux plus isolated installer/dispatcher smoke tests. These are scoped pilot
gates, not a claim that every historical workspace campaign passes. The export
includes source, local fixtures, the compatibility ledger, and vendored YAML
parser licensing; historical private campaign evidence, private design docs,
and the frozen Go-oracle dependency cache are not shipped. Some foundation
integration tests reference those historical assets and are outside this gate.

The workspace contains core, application, execution, process, provider, Git,
knowledge, daemon, CLI, plugin-host, and first-use-pilot crates. The installed
entrypoint is **orchestrator-first-use-pilot**, not the separate foundation
`orchestrator-cli` binary. Preserve this distinction when changing packaging.
