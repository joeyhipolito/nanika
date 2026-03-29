# Nanika Scripts

Utility scripts for Nanika project maintenance.

## generate-agents-md.sh

Auto-generates the routing index block in `CLAUDE.md` and `AGENTS.md` by scanning `plugins/*/skills/*/SKILL.md`.

### Usage

```bash
# Generate and update AGENTS.md + CLAUDE.md
./scripts/generate-agents-md.sh

# Dry run (preview output without writing)
./scripts/generate-agents-md.sh --dry-run
```

## Removed Scripts

The following scripts were removed during cleanup (Feb 2026):

- **Quota scripts** (`update-quota-from-*.sh`) — superseded by `orchestrator detect`
- **Validation scripts** (`validate-*.sh`, `verify-*.sh`) — validated archived features
- **Test scripts** (`test-*.sh`) — tested archived features
- **Packaging** (`package-skill.sh`) — unused dist packaging
- **Status system** (`update-system-status.sh`, hooks/) — inactive feature
## Mission / Backlog Scripts

- `bootstrap-linear-backlog.sh` — seeds or fully refreshes the current Nanika implementation backlog in Linear; prefer the `linear` CLI for routine issue/project updates
- `new-mission.sh <slug>` — creates a local mission file under `~/.alluka/missions/`
