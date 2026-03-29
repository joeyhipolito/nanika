# Contributing to Nanika

## Bug Reports

Open a GitHub issue with:
- What you ran (command, flags, input)
- What you expected vs. what happened
- Relevant output or error messages
- OS and Go version (`go version`)

## Skill Proposals

New skills should solve a real, recurring need. Before opening a PR:

1. Check the backlog — it may already be planned (Linear team `V` / `nanika`)
2. Open an issue describing the skill, its CLI surface, and the problem it solves
3. Wait for a thumbs-up before building — avoids wasted effort

A valid skill has:
- A single-purpose CLI binary under `skills/<name>/`
- A skill definition at `.claude/skills/<name>/SKILL.md`
- At minimum a `doctor` subcommand that validates configuration
- Follows the conventions in `docs/SKILL-STANDARD.md`

## Code Style

- Go: `gofmt`, standard library preferred, no unnecessary dependencies
- Error messages lowercase, no trailing punctuation
- CLIs use `cobra` + `viper` consistent with existing skills
- Keep commands composable — prefer `--json` output flags for machine consumption

Run before submitting:

```bash
make test-<skill>   # e.g. make test-orchestrator
make build-<skill>
```

## Pull Request Process

1. Fork the repo and create a feature branch (`feat/my-skill`, `fix/orchestrator-crash`)
2. Keep PRs focused — one skill or one fix per PR
3. PR description should explain *why*, not just *what* — the diff shows the what
4. A maintainer will review within a few days; address feedback promptly

## What We Won't Accept

- Skills that require paid third-party services with no free tier
- Breaking changes to existing CLI flags without a migration path
- PRs that haven't been discussed via issue first (for new skills)
