# Nanika maintenance scripts

Run these from the repository root with Bash. Read a script before using it:
several install binaries, modify local configuration, or control daemons.

| Script | Behavior |
|--------|----------|
| `install.sh` | Legacy interactive installer for the Go orchestrator and a fixed plugin selection; installs/configures local tools and may set up services. |
| `nanika-update.sh` | Builds, installs, restarts, and verifies discovered plugins. Supports `--only`, `--skip`, and `--dry-run`. |
| `new-mission.sh <slug>` | Creates a new dated mission template under `~/.alluka/missions/`; refuses an existing filename. |
| `generate-agents-md.sh` | Discovers `.claude/skills` and plugin skill definitions, then rewrites routing guidance in AGENTS.md/CLAUDE.md. |
| `audit-bins.sh` | Audits the local binary installation; consult the script for expected paths. |
| `test-first-check.sh` | Checks a commit range for test-first conventions. |
| `chaos-obsidian.sh` | Obsidian fault/recovery exercises; use its dry-run mode before an actual exercise. |
| `experiment-snapshot.sh` | Captures experiment state; intended for an already configured local environment. |

## Minimal CLI setup

The bulk installer is optional. For just the orchestrator:

```bash
GOWORK=off make build-orchestrator
./bin/orchestrator --help
GOWORK=off make install-orchestrator
```

The install target writes `~/.alluka/bin/orchestrator`; add that directory to
PATH. The full source checkout is required for relative Go module dependencies.

## Installer selection

```bash
bash scripts/install.sh --core
bash scripts/install.sh --all
bash scripts/install.sh --plugins discord
```

Core selects `orchestrator`, `nen`, `tracker`, and `scheduler`. `--all` adds
`discord` and `telegram`; it does not install Dust, Obsidian, or Nen MCP. Tracker
requires Cargo. Additional components have their own manifests. Root bulk
`make build`/`make setup` still reference the missing Wails dashboard and are not
the recommended path for this snapshot.

## Preview maintenance

```bash
bash scripts/generate-agents-md.sh --dry-run
bash scripts/nanika-update.sh --dry-run --only scheduler
```

Skill discovery depends on the actual local links. Some tracked links are broken
in a fresh public clone. The checked [agent index](../AGENTS.md) is maintained
manually; running the generator without `--dry-run` replaces it with local output.

`new-mission.sh` uses [templates/mission.md](../templates/mission.md) when present.
Its historical frontmatter may include a Linear field; public contribution work
uses GitHub issues and does not require a private Linear workspace.

## Rust-first source installation

`python3 scripts/install-rust-orchestrator.py` builds paired Rust pilot/broker,
explicit output/usage helpers, and Go compatibility into versioned bundles under
`~/.local`. `--prefix` chooses another prefix; `--profile dev` uses debug builds.
The installer refuses unmanaged destination entries and activates the dispatcher
last. It does not start providers or migrate daemons. See the
[Rust guide](../skills/orchestrator-rs/README.md) for routing and rollback.

`orchestrator-dispatch.py` is installed inside each bundle; invoke its installed
link rather than running the source script before its sibling engines exist.

Legacy `install.sh` and `nanika-update.sh` manage Go/plugin entries in `~/.alluka/bin`. Keep the Rust bundle prefix first on PATH when using both; those legacy scripts do not upgrade or migrate the Rust bundle.
