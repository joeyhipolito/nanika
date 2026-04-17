# Wails Retirement Audit — 2026-04-13

Pre-cleanup audit per `docs/dust-protocol-plan.md` §3.10.

## Active repo refs (kept — historical / now-legacy)
- `docs/PLUGIN-PROTOCOL.md` — rewritten with deprecation pointer to DUST-WIRE-SPEC.md
- `docs/dust-protocol-plan.md` — historical plan record
- `skills/orchestrator/docs/eval-reports/low-confidence-routing.md` — historical eval report
- `plugins/engage/docs/planner-ui-redesign.md` — historical dashboard-era planning doc
- `plugins/dashboard/launch.json` — inside plugins/dashboard, deleted with the directory

## External refs (historical, leave alone)
- `~/.claude/history.jsonl` — auto-managed conversation log
- `~/.claude/paste-cache/` — auto-managed cache
- ~20 old missions in `~/.alluka/missions/` from March 2026 — historical audit trail

## Deleted by this cleanup
- `plugins/dashboard/` (entire Wails app)
- `plugins/dust/plugins/` (nested prototype plugins: dummy, dust-health, dust-scheduler, dust-tracker, hello-plugin, system-plugin)
- 15 orphaned plugins/*/ui/ directories (no longer consumed by any UI host)

## Not applicable
- No active MCP bridge tool registrations (`nanika_navigate`/`nanika_notify`/`nanika_reply`) found in `~/.claude`
- No scheduled jobs reference dashboard
- No persona memory references dashboard
