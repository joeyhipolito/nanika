# Experiments

Running record of skills-index / orchestrator efficiency experiments. Each entry is a discrete variant tested against a documented baseline with pre-declared revert triggers.

## Framework

Each experiment follows the same shape:

1. **Baseline snapshot** at `~/.alluka/missions/artifacts/skills-index-<variant>-<date>.md` — per-phase averages, cache split, per-persona breakdown, revert-trigger thresholds.
2. **Branch isolation**: each strategic variant lives on its own `exp/<variant>` branch off main. Caveman → `exp/caveman`, Advisor → `exp/advisor`, etc. Variants are not interleaved on main.
3. **Cutover** recorded in `~/.alluka/missions/artifacts/skills-index-cutovers.md`. Requires rebuild + daemon restart; phases spanning a cutover are discarded.
4. **Watch window**: full 7-day cycle, calendar-aligned to the Anthropic `seven_day_util` reset epoch (read from `usage_snapshots.seven_day_resets_at`). Window starts within ~1h of a reset and ends 7 days later. The 100-phase / 2-week fallback applies only to maintenance variants where 7-day calendar alignment isn't justified.
5. **Control weeks between strategic variants**: at least one full 7-day window of "current main, no variant" before/after each strategic experiment, so each variant compares against a fresh baseline rather than a stale aggregate. Without control weeks, accumulated drift (model-side changes, mission-mix shifts, self-improvement effects) gets attributed to the variant.
6. **Self-improvement stays ON during windows**: Shu close-sweep, Gyo scanners, nen-daemon, dream pipeline all run as normal — the variant's measured performance includes any compounding with self-improvement, which is the system's real-world behavior. Freezing them would yield a purer signal but a less honest one.
7. **Manual interventions are frozen during windows**: no persona prompt edits, no schema changes, no ad-hoc binary deploys. If a real bug forces a mid-window change, the affected window is annotated `confounded:<commit>` in the cutover log and excluded from the variant's accepted dataset.
8. **Revert triggers** (vs accepted baseline):
   - `gate_passed %` drops > 10 pp
   - `failed %` rises > 5 pp
   - `retries avg` rises > 20 %
   - `skill declared count` drops > 15 %
   - `seven_day_util` per-mission delta rises > 30 % (real plan-budget signal — added 2026-04-15 once OAuth probe data is denser)
9. **Tracking**: `scripts/experiment-snapshot.sh` runs live diff against the active window. Snapshot must flag whether the window has crossed a `seven_day_util` reset (full cycle = clean signal; partial = warn).
10. **Calendar example**: `seven_day_util` resets every Friday 04:00 UTC. Each variant's window runs Fri 04:30 UTC → next Fri 03:30 UTC. Five strategic variants in queue → five sequential weeks. Control weeks slot into any week without a queued variant.

## Queue

Each row is one full 7-day window on its own `exp/<variant>` branch. Control weeks (no variant) slot in between as needed.

| # | Variant | Ticket | Branch | Window plan | Status | Scope |
|---|---------|--------|--------|-------------|--------|-------|
| ✅ | **Option E** — strip command tails, keep headers | — | `main` (4671bcec) | 102-phase window, closed 2026-04-13 | Shipped | Drop CLI command-tail examples from skills-index block. Block −60.5% (14,698 → 5,801 B); CLI discovery held via `output_parse`. |
| ✅ | **Option C** — worker-only skills-index removal | TRK-463 (P2) | `main` (18d0445f) | 111-phase window, closed 2026-04-16T03:30:53Z | Shipped | Delete worker-side SkillIndex injection + plumbing; keep `LoadSkillIndex()` for decomposer. Gate pass 100%, retries −94% vs E. Cost deltas (per-phase $0.65, per-mission $3.30) confounded by mission-mix shift — quality signal clean, cost signal directional. 96.3% of skill invocations via `output_parse` (runtime CLI discovery). Snapshot: `skills-index-option-C-2026-04-17.md`. |
| 1 | **Caveman-native output compression** | TRK-462 (P1) | `exp/caveman` (not created) | Full 7-day cycle, starts at next Anthropic 7d util reset (~2026-04-17 04:00 UTC) | Queued | In-house cache-safe output compression for prose-heavy personas. NOT upstream caveman plugin — cache-key stability concerns. Baseline to beat: Option C aggregate. |
| 2 | **Emission / Adviser Strategy** | TRK-455 (P1) | `exp/advisor` (not created) | Full 7-day cycle, week after Caveman closes | Queued | Decomposer-level routing of phases to cheaper models based on complexity. Called "the dominant cost lever, 4–6× bigger than any caveman option." |
| 3 | **Transmutation — composable constraint modules** | TRK-487 (P1) | `exp/transmutation` (not created) | Full 7-day cycle, week after Advisor closes | Queued | Decompose persona files into stackable constraint modules (output contract, failure guards, domain rules). Ditch persona identity layer. |
| 4 | **Opus adaptive-thinking off + effort=max for think-tier** | TRK-495 (P2) | `exp/opus-tuning` (not created) | Full 7-day cycle, week after Transmutation closes | Queued | Per-tier env injection for think-tier worker spawns. Restore pre-Feb-2026 reasoning depth on reviewers/architects without paying max effort on every work-tier phase. |
| 5 | **LSP-enabled workers (Go/Rust/TS)** | TRK-503 (P2) | `exp/lsp-workers` (not created) | Full 7-day cycle, week after Opus tuning closes. Prerequisite: ~15-min empirical test to confirm LSP reaches workers (settings-gated vs env-gated) before window opens. | Queued | Baseline (mission 20260415-36937ee8): 19 dev missions, 95% of tool calls are Read+Grep, median 18 calls / 12,253 tokens per mission. Projected >38% tool-result token reduction. Requires engineering-persona update (LSP-first bootstrap rule) + possible 1-line `transport.go:19-27` allowlist patch. |
| C | **Control / current main** | — | `main` | Slots between strategic variants when none are queued | Implicit | "Do nothing" baseline measured against the same 7-day calendar so each variant has a fresh comparison anchor. |

## Why caveman is deferred

The upstream caveman plugin changes output shape, which busts the 94.7% cache-read ratio that's doing most of the cost-reduction work. A cache-aware native implementation will target only the prose slice that isn't cached anyway (phase outputs), leaving structural markers and code blocks untouched. It's a cross-cutting optimization layered on top of whichever variants ship — not an A/B variant itself.

## Running snapshots

```bash
# Live diff against the active cutover window (reads from ~/.alluka/metrics.db)
scripts/experiment-snapshot.sh

# Machine-readable
scripts/experiment-snapshot.sh --json

# Force a custom window
scripts/experiment-snapshot.sh --since 2026-04-11T03:05:00
```

Both **Baseline** (pre-experiment, d7) and **Option E** numbers are embedded in the script as comparison columns. Update these when a new variant is accepted.

## How to run a new variant

1. Create `exp/<nen-category>` branch and land the change.
2. Rebuild + install + codesign the orchestrator binary (`scripts/nanika-update.sh`).
3. Restart the orchestrator + nen daemons (cache invalidation — see `rebuild + daemon restart` in the cutover log rules).
4. Append a new row to `skills-index-cutovers.md` with `end_ts: -`; set the prior row's `end_ts` to the restart time.
5. Let the window run; monitor with `experiment-snapshot.sh`.
6. At 100 phases / 2 weeks, write `skills-index-<variant>-<date>.md` following the baseline schema.
7. Decide: accept (update script's embedded comparison baseline), revert, or extend the window.

## Known instrumentation gaps

- ~~**`phases.provider` does not record runtime selection**~~ — **resolved 2026-04-13** (TRK-492 merged). `providerForPhase` now derives from `p.Runtime`.
- ~~**`phases.tokens_cache_{creation,read}` always 0 in metrics.db**~~ — **resolved 2026-04-14** (TRK-494 merged). INSERT SQL now writes both columns; existing rows stay 0 (no backfill).
- ~~**No real Anthropic plan-utilization data**~~ — **resolved 2026-04-15** (OAuth probe merged + wire-format fix). New `usage_snapshots` table records real `five_hour`, `seven_day`, `seven_day_sonnet` utilization per mission completion.
- **Snapshot script doesn't yet read `usage_snapshots`** — pending. Wire `## Plan Util` section into `experiment-snapshot.sh` so per-mission util delta is visible alongside cost. Targeted before caveman starts.
- **No randomized variant tagging at phase level** — all variants compare aggregates over sequential time windows. Acceptable for directional signal; not sufficient for causal claims. Future work: phase-level random assignment so within-mission paired comparison is possible.

## References

- Baseline: `~/.alluka/missions/artifacts/skills-index-baseline-2026-04-11.md`
- Option E snapshot: `~/.alluka/missions/artifacts/skills-index-option-E-2026-04-13.md`
- Cutover log: `~/.alluka/missions/artifacts/skills-index-cutovers.md`
- Caveman evaluation: `~/.alluka/missions/artifacts/caveman-integration-evaluation.md`
- Caveman → Option E arc: `~/.alluka/missions/artifacts/caveman-to-option-e-findings.md`
