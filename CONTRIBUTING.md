# Contributing to Nanika

Use GitHub issues in this repository for public bug reports and feature proposals.
Include the command, expected and actual behavior, relevant output, commit, OS,
and toolchain version. Remove credentials and personal runtime data from reports.

Discuss a new component before implementing it. Keep pull requests focused and
explain the problem, resulting behavior, and validation performed.

## Repository layout

- `skills/orchestrator-rs/`: Rust execution pilot and foundation crates; see its README for scoped checks.
- `skills/orchestrator/`: Go mission CLI.
- `skills/decomposer/`: mission planning guidance; its `go.mod` has no Go packages.
- `shared/sdk/`: Go library for the Claude Code CLI.
- `plugins/`: separately built plugins; consult each `plugin.json` and module manifest.
- `personas/`: role and style guidance.
- `docs/`: standards and protocol references.

Keep the full checkout when building: the orchestrator's `go.mod` replaces the
SDK and Nen modules with relative paths inside this repository.

## Local checks

From the repository root, with Go 1.25.4 or newer:

```bash
GOWORK=off make build-orchestrator
(cd skills/orchestrator && GOWORK=off go test ./...)
(cd shared/sdk && GOWORK=off go vet ./...)
(cd shared/sdk && GOWORK=off go test -race -count=1 -timeout=2m ./...)
```

The broad orchestrator suite has known baseline failures; report the exact
failures and compare against your base commit. Do not describe a partial check
as a passing full suite. The dedicated SDK workflow runs on Linux and macOS;
the older all-skills workflow still references retired module paths.

SDK live-provider tests require both `-tags=integration` and
`RUN_CLAUDE_INTEGRATION=1`. They invoke an authenticated Claude CLI and may incur
usage. Ordinary SDK tests use fixtures. See [SDK documentation](shared/sdk/README.md).

Use `gofmt` for Go changes and run the relevant module's tests. For Rust or
frontend plugins, use their own Cargo/npm configuration. Root `make build` and
`make setup` still reference the absent legacy dashboard; use targeted builds.

## Adding skills and plugins

A knowledge skill can be Markdown-only. A CLI plugin normally has a `plugin.json`,
source and build configuration, and `skills/SKILL.md` explaining its commands.
Follow the [skill standard](docs/SKILL-STANDARD.md) and
[plugin protocol](docs/PLUGIN-PROTOCOL.md), checking actual neighboring code where
older examples differ. Add a configuration/health check when appropriate.

Some tracked `.claude/skills` links are unresolved in the public checkout.
Verify each new link from a fresh clone; do not point to a personal checkout or
assume an installed private skill exists. The curated [AGENTS.md](AGENTS.md)
contains usable public references. Preview routing-index generation before
allowing it to rewrite contributor-maintained guidance.

## Review

Keep user-visible CLI changes documented, include a migration path for breaking
flags, and distinguish tested behavior from experimental support. Never commit
provider credentials, local databases, session transcripts, or generated binaries.
