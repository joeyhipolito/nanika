# Nanika Observability Catalog

Single reference for every durable, emitted, or queryable signal the nanika system exposes. Organized by subsystem; each entry cites the source file, shape/schema, subscription mechanism, and UI suitability.

Hand-synthesized from five domain catalogs produced by the 2026-04-11 observability research missions:

| Domain | Research mission | Workspace | Original catalog |
|---|---|---|---|
| [1. Orchestrator](#1-orchestrator-core) | `2026-04-11-observability-orchestrator` | `20260411-c9e894bc` | `~/.alluka/observability-reports/orchestrator-catalog.md` |
| [2. Nen](#2-nen-ability-system) | `2026-04-11-observability-nen` | `20260411-4ee06523` | `workers/architect-phase-4/nen-observable-signals-catalog.md` |
| [3. Skills & Personas](#3-skills--personas) | `2026-04-11-observability-skills-personas` | `20260411-4dfc4cc2` | `workers/architect-phase-5/signals-catalog.md` |
| [4. Scheduler & Tracker](#4-scheduler--tracker) | `2026-04-11-observability-scheduler-tracker` | `20260411-d436111f` | `workers/architect-phase-5/observable-signals-catalog.md` |
| [5. Learning / Memory / Dream](#5-learning--memory--dream) | `2026-04-11-observability-learning-memory` | `20260411-a652fd1d` | `workers/architect-phase-5/observable-signals-catalog.md` |

Only domain 3 had an automated review pass — mission 3 found 3 blockers + 5 warnings + 5 suggestions (all additive). Domains 1, 2, 4, 5 were not reviewed (review phases hit "Prompt is too long" in Claude and were dropped by operator decision). Path references, schemas, and source refs below are verbatim from the dig phases — spot-check before acting on any single reference.

All file paths are rooted at `$CONFIG_DIR = ~/.alluka` (see `config/config.go:14` `DirName = ".alluka"`; legacy `~/.via` is still accepted) unless otherwise noted. Orchestrator Go source refs are rooted at `skills/orchestrator/internal/`.

---

## Subscription mechanism glossary

| Mechanism | How | Latency | Notes |
|---|---|---|---|
| **push-stream** | In-process event bus subscriber, or UDS fan-out at `~/.alluka/events.sock`, or HTTP SSE at `http://127.0.0.1:7331/api/events` | real-time | Drop-on-slow fan-out; use cursor replay on reconnect |
| **jsonl-tail** | `tail -F`-style read on append-only JSONL | sub-second | Authoritative durable replay for events |
| **file-watch** | fsnotify on a single file | sub-second | |
| **dir-watch** | fsnotify on a directory | sub-second | |
| **json-file poll** | Re-read a JSON snapshot on disk | 1–10s | |
| **sql-poll** | `SELECT` against SQLite | 100ms–30s cadence | WAL mode — readers never block writers |
| **sql-diff** | Polling with `WHERE created_at > ?` or counter-delta | 1–30s | |
| **cli-exec** | Shell out to CLI binary, capture stdout/stderr | 100ms–5s | |
| **mcp** | `tools/call` on `nen-mcp` JSON-RPC server | on demand | Per-tool limits (default 20–100, max 100–500) |
| **in-proc callback** | Library-only, no external surface | n/a | Off-limits to out-of-process consumers |

---

# 1. Orchestrator core

The orchestrator subsystem writes four disjoint trees of durable state plus an in-memory + JSONL event surface and a CLI surface.

## 1.1 Workspace tree — `~/.alluka/workspaces/<id>/`

Created by `core.CreateWorkspace` (`core/workspace.go:79-117`). Workspace ID is `YYYYMMDD-<8-hex>` (`generateID`, `workspace.go:580`). Directory modes `0700`, files `0600` except the worker `stop.sh` hook which is `0700`.

```
workspaces/<id>/
├── mission.md                      # mission prompt body
├── checkpoint.json                 # authoritative resumable state
├── plan.json                       # debug-only plan snapshot
├── pid                             # orchestrator PID for cancel
├── cancel                          # sentinel: presence = cancel requested
├── linear_issue_id                 # optional: "V-5\n"
├── mission_path                    # optional: absolute path to source .md
├── target_id                       # optional: canonical target id "repo:~/..."
├── task_type                       # optional: classifier output
├── pr_url                          # optional: URL of PR created from worktree
├── workers/
│   └── <persona>-<phaseID>/        # one subdir per executed phase
│       ├── CLAUDE.md               # generated role+context prompt
│       ├── output.md               # full worker transcript
│       ├── orchestrator.signal.json  # optional CompletionSignal
│       ├── MEMORY_NEW.md           # worker-appended memory draft
│       └── .claude/
│           ├── settings.local.json # deny rules + low-risk allowlist
│           └── hooks/stop.sh       # learning capture hook
├── artifacts/
│   ├── <phaseID>/…                 # per-phase copies of worker outputs
│   └── merged/…                    # union copy across all phases
├── learnings/
│   └── <persona>-<phaseID>.json    # stop.sh hook output
└── scratch/
    └── <phaseID>/notes.md          # extracted <!-- scratch --> blocks
```

### `mission.md`
- **Writer:** `core.CreateWorkspace` (`workspace.go:105-108`)
- **Format:** raw task text; no frontmatter enforced
- **Purpose:** canonical mission body; `ListWorkspaces` uses its presence to detect v2 workspaces (`workspace.go:185`)

### `checkpoint.json` — authoritative resumable state
- **Writer:** `core.SaveCheckpoint` / `core.SaveCheckpointFull` (`core/checkpoint.go:44-100`), called from `cmd/run.go:435` at start and by the engine between phases (`engine/engine.go:1562`)
- **Write protocol:** atomic temp+rename (`checkpoint.go:82-98`)
- **Envelope (v1):** `{ "version": 1, "payload": { <Checkpoint> } }`. `LoadCheckpoint` also accepts legacy direct-Checkpoint writes (`checkpoint.go:108-138`)

**`Checkpoint`** (`checkpoint.go:14-34`):

| field | type | notes |
|---|---|---|
| `version` | int | checkpoint struct version, currently `2` |
| `workspace_id` | string | always `filepath.Base(wsPath)` |
| `domain` | string | `dev`/`personal`/`work`/`creative`/`academic` |
| `plan` | `*Plan` | full decomposed plan |
| `status` | string | `in_progress` / `completed` / `failed` |
| `started_at` | time.Time | mission start (zero for v1) |
| `linear_issue_id` | string | NOT serialized — enriched from sidecar on load |
| `mission_path` | string | NOT serialized — enriched from sidecar on load |
| `git_repo_root` | string | set when git isolation active |
| `worktree_path` | string | e.g. `~/.alluka/worktrees/<id>` |
| `branch_name` | string | `via/<mission-id>/<slug>` |
| `base_branch` | string | usually `main` |

**Embedded `Plan`** (`core/types.go:9-16`): `id`, `task`, `phases []*Phase`, `execution_mode` (`parallel`/`sequential`), `decomp_source` (`predecomposed`/`decomp.llm`/`decomp.keyword`/`template`), `created_at`.

**Embedded `Phase`** (`core/types.go:33-113`):

Plan-authored fields — `id`, `name`, `objective`, `persona`, `model_tier` (`think`/`work`/`quick`), `skills[]`, `constraints[]`, `dependencies[]`, `expected`, `role` (`planner`/`implementer`/`reviewer`), `target_dir`, `priority` (`P0` bypasses quota gate), `runtime` (`""` → `claude`), `runtime_policy_applied`, `stall_timeout`.

Runtime-mutated fields (updated between checkpoint saves) — `status` (`pending`/`running`/`completed`/`failed`/`skipped`), `output`, `error`, `start_time` / `end_time`, `signal_remainder`, `retries`, `gate_passed`, `output_len`, `parsed_skills[]`, `learnings_retrieved`, `session_id` (Claude resume), `persona_selection_method` (`llm`/`keyword`/`required_review`), `model`, `tokens_in` / `tokens_out`, `tokens_cache_creation` / `tokens_cache_read`, `cost_usd`, `review_iteration`, `origin_phase_id` (for fix phases), `max_review_loops`, `review_blockers[]` / `review_warnings[]`, `changed_files[]`, `worker` (persistent worker name, e.g. `alpha`).

### `plan.json` — debug snapshot
- **Writer:** `cmd/run.go:438-439` (`json.MarshalIndent(plan, "", "  ")` once per run)
- **Schema:** the `Plan` struct at top level, no envelope
- **Note:** nothing in the orchestrator reads this back. `checkpoint.json` is the authoritative resume source

### Workspace sidecar files (all `0600`)

| file | writer | content | read by |
|---|---|---|---|
| `target_id` | `run.go:260` | canonical target id, e.g. `repo:~/nanika/...` | routing / `ResolveTargetDir` |
| `task_type` | `run.go:264` | classifier task type string | routing seed |
| `linear_issue_id` | `run.go:353` | `V-5\n` | `enrichCheckpointSidecars`, `FindWorkspacesByIssue` |
| `mission_path` | `run.go:358` | absolute path to source mission md | `enrichCheckpointSidecars`, `SyncManagedMissionStatus` |
| `pr_url` | `run.go:985` | URL of PR cut from worktree | status/audit display |
| `pid` | `core.WritePID` (`workspace.go:529`) | decimal PID of orchestrator process | `cmd/cancel`, signal handler |
| `cancel` | `core.WriteCancelSentinel` (`workspace.go:552`) | literal `"cancelled"` | `HasCancelSentinel` polled between phases |

### `workers/<persona>-<phaseID>/`

Created by `core.CreateWorkerDir` (`workspace.go:120-135`). Worker name format: `"%s-%s"` persona+phaseID (`worker/spawn.go:16`).

| file | writer | content |
|---|---|---|
| `CLAUDE.md` | `worker/spawn.go:48-49` | `BuildCLAUDEmd(bundle)` — role contract + persona + skills + constraints + prior scratch + learnings + memory hints |
| `.claude/settings.local.json` | `worker/spawn.go:64` | `{"permissions":{"deny":[...],"allow":[...]}}` from `DenyRulesForRole(role)` + `LowRiskTools()` |
| `.claude/hooks/stop.sh` | `worker/spawn.go:78-79` (`0700`) | Learning-capture shell script from `learning.GenerateHookScript`; writes `workspaces/<id>/learnings/<worker>.json` |
| `output.md` | `worker/execute.go:219-222` | Full worker transcript, written after SDK stream terminates |
| `orchestrator.signal.json` | worker process; read by `core.ReadSignalFile` (`core/signal.go:68`) | `CompletionSignal` |
| `MEMORY_NEW.md` | worker per CLAUDE.md instructions (`worker/claudemd.go:218`); merged back by `worker/memory.go` | newline-delimited `MemoryEntry` |

**`CompletionSignal`** (`core/signal.go:56-63`):

| field | type | notes |
|---|---|---|
| `kind` | `CompletionSignalKind` | `ok` / `partial` / `dependency_missing` / `scope_expansion` / `replan_required` / `human_decision_needed` (`signal.go:17-41`) |
| `summary` | string | human summary |
| `missing_input` | `[]string` | inputs expected but missing |
| `suggested_phases` | `[]PhaseDraft` | `{name, objective, persona?, depends_on?, skills?, dependencies?}` |
| `changed_files` | `[]string` | repo-relative changed file list |
| `remainder` | string | for `partial`: unfinished work; copied to `Phase.SignalRemainder` |

### `artifacts/`
- **Writers:** `worker.MergeArtifactsWithMeta` (`worker/execute.go:298-343`)
- **`artifacts/<phaseID>/…`** — per-phase copy of every file the worker wrote (excluding `CLAUDE.md`, `.claude/*`, `workspace-context.md`; see `CollectArtifacts` at `execute.go:268-285`)
- **`artifacts/merged/…`** — same set copied into a flat union directory (`workspace.go:147-149`)
- Markdown artifacts get YAML frontmatter injected if missing (`InjectFrontmatterIfMissing`, `execute.go:321-323`): `produced_by`, `phase`, `workspace`, `created_at`, `confidence`, `depends_on`, `token_estimate`

### `learnings/<worker>.json`
- **Writer:** the `stop.sh` hook generated at `worker/spawn.go:75-76`
- **Purpose:** Claude Code stop-hook artifact; consumed by dream/learning extraction. Shape owned by `internal/learning` (`learning/context.go`)

### `scratch/<phaseID>/notes.md`
- **Writer:** `engine.extractScratch` (`engine/scratch.go:43-61`). Content comes from `<!-- scratch --> … <!-- /scratch -->` markers in worker output (regex `scratchBlockRE` at `scratch.go:21`)
- **Reader:** `engine.collectPriorScratch` (`scratch.go:65-90`) — reads each dependency's notes.md and injects it into the next worker's CLAUDE.md as "Prior Phase Notes"
- **Format:** plain concatenated markdown. Hard-capped at `maxScratchBytes = 4096` (`scratch.go:16`) with truncation marker

## 1.2 Shared JSONL logs — `~/.alluka/*.jsonl`

All `0600`, parent `0700`, append-only.

### `audits.jsonl`
- **Writer:** `audit.SaveReport` (`audit/store.go:26-50`); one JSON object per line
- **Reader:** `audit.LoadReports` (`store.go:54-84`); malformed lines skipped
- **Schema — `audit.AuditReport`** (`audit/types.go:6-22`):

| field | type |
|---|---|
| `workspace_id` / `task` / `domain` / `status` | string |
| `audited_at` | time.Time |
| `linear_issue_id` / `mission_path` | string — mirrored from sidecar |
| `scorecard` | `Scorecard` — 5 axes (1-5) + `overall` |
| `evaluation` | `MissionEvaluation{summary, strengths[], weaknesses[], recommendations[]}` |
| `phases[]` | `PhaseEvaluation{phase_id, phase_name, persona_assigned, persona_ideal, persona_correct, objective_met, issues[], score}` |
| `convergence` | `ConvergenceStatus{converged, drift_phases[], missing_phases[], redundant_work[], assessment}` |
| `decomposer_convergence` | `DecomposerConvergence{skill_md_hash, prompt_source, skill_md_path, rules_extracted}` |
| `changes[]` | `ChangeRecord{phase_id, phase_name, type, target, summary}` |

Scorecard axes: `decomposition_quality`, `persona_fit`, `skill_utilization`, `output_quality`, `rule_compliance`, `overall`. `Recommendation`: `{category, priority high/medium/low, summary, detail}`.

**Gotchas:** grows unbounded — UI must paginate from tail; `audits.jsonl` is the **only** place per-phase `persona_correct`/`persona_ideal` verdicts live (no SQLite mirror). Tail, do not `LoadReports` whole file on a hot path.

### `metrics.jsonl`
- **Writer:** `engine.RecordMetrics` (`engine/metrics.go:135-165`). Appended after each mission completes; same line concurrently upserted into `metrics.db` — JSONL is crash-resilient log, SQLite is best-effort
- **Schema — `engine.MissionMetrics`** (`engine/metrics.go:105-129`): `workspace_id`, `domain`, `task`, `started_at` / `finished_at`, `duration_s`, `phases_total/completed/failed/skipped`, `learnings_retrieved`, `retries_total`, `gate_failures`, `output_len_total`, `status` (`success`/`failure`/`partial`), `decomp_source`, `phases []PhaseMetric`, `tokens_in_total` / `tokens_out_total`, `tokens_cache_creation_total` / `tokens_cache_read_total`, `cost_usd_total`

**`engine.PhaseMetric`** (`engine/metrics.go:24-51`): `id`, `name`, `persona`, `skills[]`, `parsed_skills[]`, `persona_selection_method` (`llm`/`keyword`), `duration_s`, `status`, `retries`, `gate_passed`, `output_len`, `learnings_retrieved`, `error_type` (`rate-limit`/`tool-error`/`gate-failure`/`timeout`/`worker-crash`/`unknown`, `metrics.go:54-61`), `error_message`, `provider` (`anthropic` when model set), `model`, `tokens_*`, `cost_usd`, `worker_name` (persistent worker name when applicable).

### `events/<mission_id>.jsonl`
- **Writer:** `event.FileEmitter` (`event/file.go:23-69`). Path via `EventLogPath` (`file.go:119-128`). Opened `O_CREATE|O_APPEND|O_WRONLY`, mode `0600`
- **Write protocol:** serialized under `fe.mu`; JSON marshal failures silently dropped (`file.go:55-57`); `droppedWrites` counter exposed via `DropStats()`. `LastSequence` scans for resume (`file.go:94-117`) with 1 MB line limit
- **Per-line schema — `event.Event`:**
  - `sequence` — int64 monotonic **per-mission** (bus sequence is separate)
  - `type` — see §1.6
  - `timestamp` — time.Time
  - `mission_id` / `phase_id` / `worker_name`
  - `data` — `map[string]any` variant payload

The `event.Bus` is in-memory only (`event/bus.go:16-27`) — SSE cursor for live subscribers, does not persist. Same for `event.LiveState` (`event/livestate.go`). **The JSONL file is the only durable event surface.**

## 1.3 `metrics.db` — `~/.alluka/metrics.db`

Opened by `metrics.InitDB` (`metrics/db.go:194-231`). Pragmas: `WAL`, `busy_timeout=5000`, `foreign_keys=ON`. Schema created in a single transaction (`db.go:246-398`).

### `missions` (`db.go:257-280`)
PK `id` (= `workspace_id`). Columns: `id`, `domain`, `task`, `started_at` / `finished_at` (DATETIME, RFC3339 UTC), `duration_s`, `phases_total/completed/failed/skipped`, `learnings_retrieved`, `retries_total` / `gate_failures` / `output_len_total`, `status`, `decomp_source` (default `unknown`), `tokens_in_total` / `tokens_out_total`, `tokens_cache_creation_total` / `tokens_cache_read_total`, `cost_usd_total`.

Indexes: `idx_missions_domain`, `idx_missions_status`, `idx_missions_started_at`. Upsert preserves prior non-unknown `decomp_source` over incoming `unknown` (`upsertMissionSQL` at `db.go:37-69`).

### `phases` (`db.go:281-305`)
PK `id` (composite: `workspace_id + "_" + phase_id`). Columns: `id`, `mission_id` (REFERENCES missions(id) ON DELETE CASCADE), `name`, `persona`, `selection_method`, `duration_s`, `status`, `retries`, `gate_passed` (0/1), `output_len`, `learnings_retrieved`, `error_type`, `error_message`, `provider`, `model`, `tokens_*`, `cost_usd`, `parsed_skills` (TEXT, comma-joined), `worker_name`.

Indexes: `idx_phases_mission_id`, `idx_phases_persona`, `idx_phases_worker_name`.

**Gotcha:** no per-phase absolute timestamps — only `duration_s`. For timeline tiles, join with `plan.json` (`core/types.go:Phase.StartTime/EndTime`). `selection_method` is the provenance field for "why this persona".

### `skill_invocations` (`db.go:306-315`)
Columns: `id` (AUTOINCREMENT), `mission_id` (REFERENCES), `phase`, `persona`, `skill_name`, `source` (`declared`/`output_parse`, `db.go:23-26`), `invoked_at`.

Indexes: `idx_skill_invocations_mission_id`, `_skill_name`, `_persona`, `_persona_skill`, `_phase`.

**This is the only honest skill last-used timestamp in the system.** `learnings.db` has `last_used_at`/`used_count` columns but no code writes to them (see §1.4).

### `quota_snapshots` (`db.go:338-351`)
Written by `engine.recordQuotaSnapshotDB` (`engine/metrics.go:187-246`).

| column | notes |
|---|---|
| `id` | AUTOINCREMENT |
| `captured_at` | `m.FinishedAt` |
| `mission_id` | **no FK** — snapshots survive mission deletion |
| `tokens_in` / `tokens_out` / `tokens_cache_read` | totals |
| `cost_usd` | |
| `window_5h_tokens_in` / `window_5h_tokens_out` | rolling totals |
| `window_5h_cost_usd` | |
| `estimated_5h_utilization` | `(effective_in + out) / budget`; `cache_read` excluded; budget default `defaultBudgetTokens = 50_000_000`, override `RYU_5H_BUDGET_TOKENS` |
| `model` | "dominant" model by cost across phases |

Indexes: `idx_quota_snapshots_captured_at`, `_mission_id`.

## 1.4 `learnings.db` — `~/.alluka/learnings.db`

Three independent packages lazily open this one file and run additive `CREATE … IF NOT EXISTS` schema init. All opened WAL; no single owner validates cross-package invariants.

### Learning tables — `internal/learning/db.go`

**`schema_version`** (`db.go:73-77`): `version INTEGER NOT NULL`. `maxSupportedVersion` guard refuses newer DBs.

**`learnings`** (`db.go:94-109`):

| column | type | notes |
|---|---|---|
| `id` | TEXT PK | sha256-derived |
| `type` | TEXT | `insight` / `decision` / `pattern` / `error` / `source` / ... |
| `content` | TEXT | |
| `context` | TEXT | default `''` |
| `domain` | TEXT | |
| `worker_name` | TEXT | migration |
| `workspace_id` | TEXT | migration |
| `tags` | TEXT | comma-joined |
| `seen_count` / `used_count` | INTEGER | |
| `quality_score` | REAL | |
| `created_at` | DATETIME | |
| `last_used_at` | DATETIME | nullable — **unwritten by any code path** |
| `embedding` | BLOB | encoded `[]float32` (3072-dim) |
| `injection_count` | INTEGER | migration |
| `compliance_count` | INTEGER | migration |
| `compliance_rate` | REAL | migration |
| `archived` | INTEGER | migration (soft-delete) |
| `promoted_at` | DATETIME | migration |

Plus FTS5 virtual `learnings_fts(content, context, domain)` with AI/AD/AU sync triggers. Indexes: `idx_learnings_domain`, `_type`, `_domain_archived`.

**Insert dedupe** (`db.go:182-189`): cosine similarity ≥ 0.85 against domain-partitioned candidate set increments `seen_count` instead of inserting.

**Gotchas:**
- **Do NOT read `learnings.last_used_at` or `learnings.used_count` — no code writes them.** Use `skill_invocations.invoked_at` in `metrics.db` for last-used timestamps. Columns exist but are silent zeros.
- No general `updated_at` column — counter and quality-score mutations are invisible to a `WHERE updated_at > ?` subscriber.
- Archival is driven by 4 criteria covering never-surfaced / ignored / low-quality / one-off learnings. Promotion threshold: `quality_score > 0.7`.

### Routing tables — `internal/routing/db.go`

`routing.OpenDB` (`db.go:119-148`) attaches to the same `learnings.db` file; `db.go:10` comment: routing is "additive" and never modifies learnings rows.

**`target_profiles`** (`db.go:157-170`): `target_id TEXT PK`, `target_type`, `language`, `runtime`, `test_command`, `build_command`, `framework`, `key_directories` (migration), `preferred_personas` (comma-joined), `notes`, `created_at`/`updated_at`.

**`routing_patterns`** (`db.go:171-181`): unique `(target_id, persona, task_hint)`, `seen_count`, `confidence REAL DEFAULT 0.2` grows as `MIN(1.0, seen_count * 0.2)`.

**`handoff_patterns`** (`db.go:182-193`): same shape, keyed `(target_id, from_persona, to_persona, task_hint)`.

**`routing_corrections`** (`db.go:194-202`): `target_id`, `assigned_persona`, `ideal_persona`, `task_hint`, `source` (`manual`/`audit`), unique index `idx_routing_corrections_dedup` on all five columns.

**`decomposition_examples`** (`db.go:203-217`): `target_id`, `workspace_id`, `task_summary`, `phase_count`, `execution_mode`, `phases_json` (JSON blob), `decomp_source`, `audit_score`, `decomp_quality`, `persona_fit`. Unique `(target_id, workspace_id)`.

**`decomposition_findings`** (`db.go:218-229`): `target_id`, `workspace_id`, `finding_type` (values: `missing_phase` / `redundant_phase` / `phase_drift` / `wrong_persona` / `low_phase_score` per `audit/types.go:101-107`), `phase_name`, `detail`, `decomp_source`, `audit_score`. Unique `(workspace_id, finding_type, phase_name, detail)`.

**`phase_shape_patterns`** (`db.go:238-248`): `target_id`, `workspace_id`, `phase_count`, `execution_mode` (default `sequential`), `persona_seq` (comma-joined), `outcome` (`success`/`failure`), `task_type` (migration). Unique `(target_id, workspace_id)`. Indexes: `idx_phase_shapes_target`, `_outcome`, `_task_type`.

**`role_assignments`** (`db.go:263-273`): `target_id`, `workspace_id`, `phase_id`, `persona`, `role` (`planner`/`implementer`/`reviewer`), `outcome`. Unique `(workspace_id, phase_id)`.

**`handoff_records`** (`db.go:279-292`): `target_id`, `workspace_id`, `from_phase_id`, `to_phase_id`, `from_role`, `to_role`, `from_persona`, `to_persona`, `summary`. Unique `(workspace_id, from_phase_id, to_phase_id)`.

**`routing_decisions`** (`db.go:301-314`): `mission_id`, `phase_id`, `phase_name`, `persona`, `confidence`, `routing_method`, `outcome` (`pending`/`success`/`failure`, default `pending`), `failure_reason`. Unique `(mission_id, phase_id)`. Closes the red-team feedback loop — decomposer writes `pending`, engine resolves, next decomposer run reads failures as persona-avoidance hints.

**Gotcha:** `routing.OpenDB("")` opens `learnings.db`, not a separate `routing.db`. All routing tables live inside the learnings file.

### Claims table — `internal/claims/db.go`

`claims.OpenDB` (`db.go:34-57`) opens `learnings.db` with `?_journal_mode=WAL&_busy_timeout=5000` and creates:

**`file_claims`** (`db.go:59-70`): `file_path`, `mission_id`, `repo_root`, `claimed_at`, `released_at` (NULL when active). PK `(file_path, mission_id)`. Advisory-only — `CheckConflicts` (`db.go:109`) warns about cross-mission conflicts without blocking.

## 1.5 Persistent worker — `~/.alluka/workers/<name>/`

Single persistent worker: `alpha` (`engine/persistent_worker.go:16` — `persistentWorkerName = "alpha"`). Directory created lazily by `WorkerIdentity.bootstrap` (`worker/identity.go:85-108`). Dir `0700`, files `0600`, all writes atomic (`atomicWrite` at `identity.go:229-239`).

```
workers/alpha/
├── identity.md    # static bootstrap blurb
├── stats.json     # operational counters
└── memory.md      # newline-delimited MemoryEntry records
```

### `identity.md`
One-shot via `bootstrap` (`identity.go:90-94`). Content is `bootstrapIdentityTemplate` (`identity.go:15`):
> `I am <name>, a persistent worker. I accumulate memory and evolve my approach across missions.`

Not rewritten after bootstrap.

### `stats.json` — `identityStats` (`identity.go:20-27`)

| field | type |
|---|---|
| `phases_completed` | int |
| `domains` | map[string]int — per-domain counters, always non-nil on save |
| `total_cost` | float64 — summed `cost_usd` across phases |
| `last_active` | string — RFC3339 UTC, empty when never active |
| `created_at` | string — RFC3339 UTC |
| `evals` | `[]json.RawMessage` — reserved; always `[]` |

Writer: `saveStats` (`identity.go:191-216`).

### `memory.md`
- **Writer:** `saveMemory` (`identity.go:219-226`) — rewrites the full file from `wi.Entries`, one `MemoryEntry` per line
- **Line format** (`worker/memory.go:47-57`):
  ```
  <content> | filed: 2026-04-09T10:15:00Z | by: <persona-or-worker> | type: <user|feedback|project|reference|reflection> | used: <n> | superseded_by: <hash>? | bridged: <time>?
  ```
  Parsed by `ParseMemoryEntry` (`memory.go:63`). Bare-content lines remain backward compatible.
- **Ceiling:** `workerMemoryCeiling = 100` entries (`identity.go:258`). When exceeded, `evictLowestScoring` (`identity.go:287-315`) removes superseded entries first, then lowest recency-weighted score
- **Loading:** `loadMemory` (`identity.go:149-173`) trims oversized files on read to make the ceiling converge
- **Dedup / supersedure:** `AddMemoryEntry` drops exact duplicates by normalized content hash; same-type entries with ≥ `correctionOverlapThreshold = 0.8` Jaccard overlap are treated as corrections and the old one is marked superseded (`memory.go:42-45`, `memory.go:265-281`)

The engine writes a synthetic `[reflection]` memory entry every 10 phases (`engine/persistent_worker.go:108-120`, `selfReflectionInterval = 10`).

Additional files referenced by the memory pipeline but not written by the orchestrator itself:
- `MEMORY_ARCHIVE.md` — lines beyond `memoryCeilingLines = 100` (`worker/memory.go:22`)
- `MEMORY_QUARANTINE.md` — lines matching `imperativePatterns` (`worker/memory.go:27-40`), a prompt-injection guardrail

## 1.6 Event surface — 28 topics, 7 sinks

Event type constants live in `skills/orchestrator/internal/event/events.go` (also referenced as `event/types.go:16-107`). Wire format `<category>.<action>`; constants are `TypeXxxYyy`.

### 28 topics, grouped

**Mission (4):** `mission.started`, `mission.completed`, `mission.failed`, `mission.cancelled`

**Phase (8):** `phase.started`, `phase.completed`, `phase.failed`, `phase.skipped`, `phase.retrying`, `phase.gate_failed`, `phase.gate_passed`, `phase.watchdog_stall`

**Worker (6):** `worker.started` / `worker.spawned`, `worker.output`, `worker.tool_used`, `worker.completed`, `worker.crashed` / `worker.failed`, `worker.memory_flushed`

**Decomposition (3):** `decomposer.emitted` / `decompose.started`, `decompose.completed`, `decompose.fallback`

**Learning / memory (4):** `learning.captured`, `learning.stored` / `learning.extracted`, `learning.injected`, `memory.entry_added`

**Routing (2):** `routing.decision`, `routing.failure_recorded`

**DAG (2):** `dag.dependency_resolved`, `dag.phase_dispatched`

**Role (1):** `role.handoff`

**Contract / review (3):** `contract.validated`, `contract.violated`, `persona.contract_violation`, `review.findings_emitted`, `review.external_requested`

**Git (3):** `git.worktree_created`, `git.committed`, `git.pr_created`

**System (3):** `system.error`, `system.checkpoint_saved`, `signal.scope_expansion`, `signal.replan_required`, `signal.human_decision_needed`

**File overlap (1):** `file_overlap.detected`

**Security (3):** `security.injection_detected` — **defined but never emitted** (reserved slot; zetsu is a pure library, see §2.6); `security.escape_attempted`; `security.settings_violation`; `security.invisible_chars_stripped`

**Envelope shape** (`event/types.go:111-143`):
```go
type Event struct {
    ID        string         // "evt_" + 16 hex
    Type      EventType
    Timestamp time.Time
    Sequence  int64          // monotonic per-bus
    MissionID string
    PhaseID   string         // optional
    WorkerID  string         // optional
    Data      map[string]any // payload varies by type
}
```

### 7 sinks (all implement `Emitter`)

| Sink | Source | Purpose |
|---|---|---|
| `NoOp` | `event/noop.go` | discards; default in tests |
| `FileEmitter` | `event/file.go:23-69` | durable JSONL at `~/.alluka/events/<id>.jsonl` |
| `UDSEmitter` | `event/uds.go` | Unix domain socket broadcast to live subscribers |
| `Bus` | `event/bus.go:16-27` | in-memory pub/sub for SSE |
| `LiveState` | `event/livestate.go` | in-memory mission-state projection |
| `ProjectFromLog` | `event/project.go` | derives current phase/status from JSONL |
| `MultiEmitter` | `event/multi.go` | fan-out wrapper |

Production default (per `engine.NewEngine`) wires `FileEmitter + UDSEmitter + Bus + LiveState` through `MultiEmitter`.

### Sequence semantics
- **Per-mission JSONL sequence** (`file.go:49-52`) — monotonic int64, starts at 1, used by resume. `LastSequence()` scans file for resume
- **Bus sequence** (`bus.go:47`) — separate monotonic int64 used by SSE cursor
- Bus and file emit the same logical event but with **different sequence values** — subscribers must pick the right counter

SSE sanitization (`event/sanitize.go`): tool-input strings truncated to 2KB; non-UTF8 bytes replaced; secrets matching `sk-...` / `Bearer ...` patterns scrubbed.

**Gotcha:** `Bus.Subscribe()` fan-out is drop-on-slow (`bus.go:46-71`) and `subscriberDrops` is a private counter. A UI reconnecting after a lag will silently miss events unless it uses `EventsSince(seq)` (`bus.go:110-128`) with a persisted last-seq cursor.

## 1.7 CLI surface

Backing packages: `cmd/*`, `internal/metrics`, `internal/audit`, `internal/dream`, `internal/preflight`, `internal/learning`, `internal/event`, `internal/routing`, `internal/claims`, `internal/core`, `internal/git`.

### Global flags (`cmd/root.go:36-38`)

| Flag | Default | Effect |
|---|---|---|
| `--verbose,-v` | `false` | Per-item progress (dream, cleanup) |
| `--dry-run` | `false` | Print planned action without mutating |
| `--domain` | `dev` | Task domain (`dev/personal/work/creative/academic`) |

### `orchestrator status`
`cmd/status.go`. Text-only.

Output blocks:
1. **Running missions** — `<mission_id>  started <YYYY-MM-DD HH:MM:SS>  phase: <name>  elapsed: <duration>`. Sorted ascending by start. `elapsed` = `time.Since(startedAt)` truncated to seconds
2. **Workspaces — last 5** — `<workspace-id> [<status>] <completed>/<total> phases[ (<LINEAR-ID>)] — <task snippet>` + optional `PR: <pr_url>`. Task snippet trimmed to 80 chars. `status` event-derived via `event.ProjectFromLog` for live missions, else from checkpoint

Backing: `core.ListWorkspaces()`, `event.ProjectFromLog(missionID)` on `events/<id>.jsonl`, `core.LoadCheckpoint(wsPath)`, `mission.md`, `pr_url` sidecar.

Warnings to stderr.

### `orchestrator metrics`
`cmd/metrics.go`. Text-only. Backed by `metrics.db` + best-effort backfill from `metrics.jsonl` via `db.ImportMissingFromJSONL`.

| Subcommand | Flags | Output columns |
|---|---|---|
| root `metrics` | `--last=20`, `--domain`, `--status`, `--days`, `--decomp-source`, `--worker` (`ephemeral` matches no-worker) | `workspace \| domain \| status \| decomp \| persona \| duration \| phases \| task` + summary `<N> missions • <X> succeeded • <Y> failed • avg <N>s` |
| `personas` | — | `persona \| phases \| avg_dur \| fail% \| avg_retry \| llm% \| kw%` |
| `skills` | — | `skill \| phase \| persona \| source \| uses` (top 100) |
| `trends` | `--days=30` | `day \| missions \| success% \| avg_dur` |
| `routing` | — | Per-persona `total \| success \| failure \| pending \| rate%` + recent failure bullets. **Backing store is `learnings.db`**, not `metrics.db` |
| `routing-methods` | — | `method \| phases \| pct%`. Alert when fallback % > `FallbackAlertThreshold = 30.0`. Excludes `required_review` phases |
| `phases <ws-id>` | — | `phase \| persona \| status \| duration \| parsed_skills` |

### `orchestrator audit`
Root `audit` is a help shell now (`audit.go:15-24`) routing users to `gyo evaluate` / `gyo report` / `ko apply`. **Only `audit scorecard` is wired up.**

#### `audit scorecard`
Flags (`audit.go:49-98`): `--format text|json`, `--domain`, `--last=0`.

**Text output** (`audit/scorecard.go:257-351`): sections "Audit Scorecard", "Metric Trends" table (Cur/Avg/Range/Delta/Trend icon), "Score History" sparklines (`" .:#@"` keyed to 1..5), "Regressions Detected" list. Trend via least-squares slope with ±0.15 threshold (`classifyTrend`, `scorecard.go:185-215`).

**JSON output** (`audit/scorecard.go:374-443`):
```json
{
  "total_audits": N,
  "date_range": "YYYY-MM-DD to YYYY-MM-DD",
  "trends": [{"metric","current","average","min","max","delta","trend","history"}],
  "regressions": [{"metric","workspace_id","audited_at","prev_score","new_score","drop","domain","top_issues"}]
}
```

Backing: `audit.LoadReports()` reads `audits.jsonl` line-by-line. Regression detection (`scorecard.go:218-254`) — consecutive-audit diff, drop ≥ 1 qualifies.

### `orchestrator dream`
`cmd/dream.go`. Backing DB is `learnings.db` via `dream.OpenSQLiteStore(path)`.

#### `dream run`
Flags (`dream.go:41-44`): `--since` (RFC3339 or duration), `--session` (substring), `--force` (bypass file-hash dedup), `--limit=0` (0 → `MaxFilesPerRun = 20`). Wrapped in 10-minute timeout.

**Output** (`dream.go:176-189`, `printDreamReport`):
```
dream: discovered=<D> skipped=<S> processed=<P> chunks=<C> llm-calls=<L> stored=<St> rejected=<R> duration=<d>
```
- `discovered` — JSONL files matched post filters
- `skipped` — too old/short/unchanged/worker-session/parse-error
- `processed` — files that emitted chunks
- `chunks` — `ChunksEmitted`
- `llm-calls` — one per non-duplicate chunk sent to Haiku
- `stored` — learnings that survived `DB.Insert` (cosine ≥ 0.85 final gate)
- `rejected` — dropped by Insert as duplicates
- `duration` — millisecond-rounded

Error footer: `dream: <N> error(s): [<phase>] <path>: <err>` where `phase ∈ hash/parse/chunk/extract/store/process` (`dream/types.go:105-109`).

**Worker session exclusion:** transcripts whose parsed `cwd` is under `~/.alluka/worktrees/` are skipped (`runner.go:174-181`).

**Dedup layers:**

| Layer | Where | What |
|---|---|---|
| 1 | `Store.IsFileProcessed` | SHA-256 of whole transcript file |
| 2 | `Store.IsChunkProcessed` | SHA-256 of normalized chunk text |
| 3 | `learning.DB.Insert` | Cosine similarity ≥ 0.85 |

#### `dream status`
Output: `processed transcripts: <n>` / `processed chunks: <n>`. Backing: `store.Status()` over `processed_transcripts` and `processed_chunks` tables in `learnings.db`.

#### `dream reset`
Drops `processed_transcripts` and `processed_chunks`. Learnings preserved. No confirmation prompt.

### `orchestrator hooks preflight`
`cmd/hooks.go:203-254`, backing `internal/preflight/`.

Flags (`hooks.go:62-64`): `--max-bytes=6144` (0 = unlimited; **ignored in JSON**, `hooks.go:232-234`), `--sections` (comma-filter), `--format text|json`. Env: `NANIKA_NO_INJECT=1` → empty stdout. 5-second timeout.

**Registered sections (priority low→high = kept longest under capacity pressure):**

| Prio | Name | Title | Source |
|---|---|---|---|
| 5 | `mission` | Active Mission | Most-recent `workspaces/<id>/checkpoint.json` + event log |
| 10 | `scheduler` | Scheduler Jobs | `$ALLUKA_HOME/scheduler/scheduler.db` |
| 15 | `nen` | Nen Daemon | `~/.alluka/nen/nen-daemon.stats.json` |
| 20 | `tracker` | Open P0/P1 Issues | `~/.alluka/tracker.db` |
| 30 | `learnings` | Relevant Learnings | `learnings.db` via `FindTopByQuality(domain, 10)` |

**Capacity dropping:** highest-index (= lowest priority = `learnings`) sections go first, then `tracker`, `nen`, `scheduler`. `mission` is always kept (`ComposeWithCapacity`, `brief.go:92-153`). If even the first section alone exceeds `--max-bytes`, its body is truncated to the last newline before the budget and all other sections are dropped (`brief.go:128-150`).

**Text mode** rendered by `Brief.RenderMarkdown()` (`brief.go:158-171`):
```
## Operational Pre-flight

### Active Mission
id: <workspace-id>
status: <checkpoint status>
phase: <current phase name>
last_event: <RFC3339 UTC>

### Scheduler Jobs
- [overdue|failed|overdue+failed] <job name> (next: <RFC3339>)

### Nen Daemon
uptime: <duration>
observers: gyo(<events>[, <err> err]), en(<events>), ryu(<events>), ...
total_events: <n>
last_event: <duration> ago
WARNING: no events for <duration> (last: <RFC3339>)

### Open P0/P1 Issues
- [<id>] <title> (@<assignee>)

### Relevant Learnings
- **[<type>]** <content>
```

**JSON mode** — always emits valid doc even when empty:
```json
{
  "blocks": [
    {"name": "mission", "title": "Active Mission", "body": "id: 20260411-c9e894bc\nstatus: in_progress\nphase: phase-3"},
    {"name": "scheduler", "title": "Scheduler Jobs", "body": "- [overdue] publish-daily (next: 2026-04-11T04:00:00Z)"}
  ]
}
```

**Section internals:**
- **mission** (`preflight/mission.go`) — picks workspace with newest `checkpoint.json` mtime; extracts `MissionID`, `Status`, current phase, last event from `events/<id>.jsonl` (falls back to checkpoint mtime)
- **scheduler** (`preflight/scheduler.go:42-65`) — SQL selects enabled jobs where `next_run_at` overdue or last status `failure`/`timeout`, `LIMIT 20`. DB path: `SCHEDULER_DB` env > `$ALLUKA_HOME/scheduler/scheduler.db` > `~/.scheduler/scheduler.db`
- **nen** (`preflight/nen.go`) — reads JSON stats, formats uptime, alphabetically-sorted observer line, total events, stale warning when `time.Since(last_event) > 10m`. Path: `NEN_STATS` env > `$ALLUKA_HOME/nen/...` > `~/.alluka/nen/nen-daemon.stats.json`
- **tracker** (`preflight/tracker.go:41-50`) — `SELECT id, title, assignee FROM issues WHERE status = 'open' AND priority IN ('P0','P1') ORDER BY priority ASC, created_at ASC LIMIT 10`. DB: `TRACKER_DB` env > `$ALLUKA_HOME/tracker.db` > `~/.alluka/tracker.db`
- **learnings** (`preflight/learnings.go`) — `learning.DB.FindTopByQuality(domain, 10)`. Domain from `NANIKA_DOMAIN` env, default `dev`. **Cold-start** ranking (quality × recency, no embedder call)

Per-section fetch errors are swallowed (`brief.go:74-77`). Capacity drop warning writes stderr: `preflight: dropped sections to fit capacity: <name>, ...`.

### `orchestrator hooks inject-context`
`hooks.go:101-137`, backing `internal/learning/context.go`.

Flags (`hooks.go:44-46`): `--query=""` (omit → cold-start), `--limit=10`, `--max-bytes=0`. `NANIKA_NO_INJECT=1` → empty. 5s timeout.

Output (text only):
```
## Relevant Learnings

- **[<type>]** <content>
```

Backing:

| Condition | Query |
|---|---|
| `--query` set | `db.FindRelevant(ctx, query, domain, limit, embedder)` — embedding ≥ 0.25 |
| `--query` omit | `db.FindTopByQuality(domain, limit)` — no embedder call |

After rendering, matched IDs → `db.RecordInjections(ctx, ids)` for compliance tracking (non-fatal). Truncation (`hooks.go:125-131`): slice to `max-bytes`, re-trim to last `\n`.

### `orchestrator cleanup`
`cmd/cleanup.go`. Default action: remove workspace dirs older than 7 days. Mode flags mutually exclusive; precedence `restore → empty-trash → worktrees → claims → default` (`cleanup.go:41-56`).

Flags: `--worktrees`, `--claims`, `--empty-trash`, `--restore <id>`. Global `--dry-run` / `--verbose` honored.

| Mode | Action |
|---|---|
| default | `time.Now().Add(-7 * 24h)` vs workspace `ModTime`. `removed: <id>` or `would remove: <id>` |
| `--worktrees` | `$ALLUKA_HOME/worktrees/`; skip active (`Status == in_progress`); `git.RemoveWorktree` → trash at `$ALLUKA_HOME/trash/` |
| `--claims` | `claims.OpenDB("").PurgeStaleClaims(7*24h)` |
| `--empty-trash` | Iterate `$ALLUKA_HOME/trash/`; skip mtime > `now-24h` |
| `--restore <ws-id>` | Looks up trash entries (prefix `<id>_` or exact), picks last alphabetically, reads `.nanika-trash-meta.json` (`git.TrashMeta`), validates `OriginalPath` unoccupied, `os.Rename` + `git.RepairWorktree`. On repair failure prints manual command |

### Format matrix

| Command | Text | JSON | Markdown | Notes |
|---|:---:|:---:|:---:|---|
| `status` | ✓ | | | Fixed-width + optional PR line |
| `metrics` (all) | ✓ | | | Fixed-width tables |
| `audit scorecard` | ✓ | ✓ | | |
| `dream run` | ✓ | | | Single key=value summary + error block — **no `--format json`** (gap) |
| `dream status` | ✓ | | | Two counter lines |
| `dream reset` | ✓ | | | One line |
| `hooks preflight` | ✓ | ✓ | ✓ | Text mode is markdown |
| `hooks inject-context` | ✓ | | ✓ | |
| `cleanup` | ✓ | | | |

### Cross-command backing

| Store | Consumers |
|---|---|
| `workspaces/<id>/` | `status`, `metrics` (ids), `cleanup`, `hooks preflight` (mission) |
| `events/<id>.jsonl` | `status`, `hooks preflight` (last_event) |
| `metrics.db` | `metrics` (all except `routing`) |
| `metrics.jsonl` | Backfill into `metrics.db` via `ImportMissingFromJSONL` |
| `learnings.db` | `dream run/status`, `hooks inject-context`, `hooks preflight` (learnings), `metrics routing` |
| `audits.jsonl` | `audit scorecard` |
| `scheduler/scheduler.db` | `hooks preflight` (scheduler) |
| `tracker.db` | `hooks preflight` (tracker) |
| `nen/nen-daemon.stats.json` | `hooks preflight` (nen) |
| `claims.db` | `cleanup --claims` |
| `worktrees/` | `cleanup --worktrees` |
| `trash/` | `cleanup --empty-trash`, `cleanup --restore` |
| `~/.claude/projects/**/*.jsonl` | `dream run` (transcripts to mine) |

## 1.8 Runtime / Cost / Learning / Alpha

### Runtime descriptors — `core/runtime.go`

| Constant | Value | Meaning |
|---|---|---|
| `RuntimeClaude` | `"claude"` | Claude Code CLI subprocess (default) |
| `RuntimeCodex` | `"codex"` | OpenAI Codex CLI subprocess |
| `RuntimeBoth` | `"both"` | Run both in sequence |

`Effective()` (line 36) treats zero value as `RuntimeClaude`. `SelectRuntime()` (line 73) is a three-signal heuristic — role → persona → task-shape.

**`RuntimeCap`** (line 171): `CapToolUse`, `CapSessionResume`, `CapStreaming`, `CapCostReport`, `CapArtifacts`.

**`RuntimeDescriptor`** (line 211): `{Name Runtime, Caps RuntimeCaps}`. Constructors:
- `ClaudeDescriptor()` (line 219) — all 5 caps enabled
- `CodexDescriptor()` (line 237) — **missing `CapCostReport`**

**`PhaseContract`** (line 255) — `ContractForRole()` (line 268):

| Role | Required | Preferred |
|---|---|---|
| Planner | CapToolUse | CapArtifacts, CapCostReport |
| Implementer | CapToolUse, CapArtifacts | CapSessionResume, CapCostReport |
| Reviewer | CapToolUse, CapArtifacts | CapCostReport |

`Validate()` (line 315) returns `ContractResult{Satisfied, Missing[], Warnings[]}`. Executor hard-fails on `Missing`, logs `Warnings`.

### Cost reporting path

**Wire types — `internal/sdk/types.go`:**
```go
type CostInfo struct {                    // line 199
    InputTokens          int              // raw + cache_creation + cache_read
    OutputTokens         int
    TotalCostUSD         float64
    CacheCreationTokens  int
    CacheReadTokens      int
}

type ResultMessage struct {               // lines 98-110
    Cost         *CostInfo                // legacy nested
    TotalCostUSD float64                  // top-level (current CLI)
    Usage        UsageInfo                // nested token counts
}
```

**Phase-level accumulation** — `core/types.go:89-94` fields: `TokensIn`, `TokensOut`, `TokensCacheCreation`, `TokensCacheRead`, `CostUSD`, `Model`.

`engine/engine.go:806-815`:
```go
if phaseCost != nil {
    phase.TokensIn            += phaseCost.InputTokens
    phase.TokensOut           += phaseCost.OutputTokens
    phase.TokensCacheCreation += phaseCost.CacheCreationTokens
    phase.TokensCacheRead     += phaseCost.CacheReadTokens
    phase.CostUSD             += phaseCost.TotalCostUSD
    if phase.Model == "" { phase.Model = config.Model }
}
```

**Flow:**
1. `executor.Execute()` → `*sdk.CostInfo`
2. Engine accumulates into `Phase` fields
3. `metrics.RecordMetrics()` rolls up via `toPhaseMetric()`
4. Writes `metrics.jsonl` + `metrics.db` (`metrics.go:135-165`)
5. `recordQuotaSnapshotDB` captures 5h rolling window (`metrics.go:187-246`)

### Learning-injection bundles

**`ContextBundle`** (`core/types.go:161-201`):
```go
type ContextBundle struct {
    PriorContext  string
    Learnings     string            // pre-formatted markdown list
    Handoffs      []HandoffRecord
    Role          Role
    Runtime       Runtime
    PriorScratch  map[string]string
    WorkerName    string            // persistent worker display name
    WorkerMemory  string            // pre-formatted memory from alpha
}
```

**`Learning`** (`internal/learning/types.go:1-65`): `Type` (`insight`/`pattern`/`error`/`source`/`decision`), `Content` (capped at 500 chars, `capture.go:19`), `Domain`, `WorkerName`, `WorkspaceID`, `InjectionCount`, `ComplianceCount`, `ComplianceRate`, `QualityScore`.

**Default extraction markers** (lines 49-64): `LEARNING:`, `TIL:`, `INSIGHT:`, `FINDING:`, `PATTERN:`, `APPROACH:`, `GOTCHA:`, `FIX:`, `SOURCE:`, `DECISION:`, `TRADEOFF:`.

**Capture functions** — `internal/learning/capture.go`:
- `CaptureFromText()` (line 25) — marker-based extraction, dedup, validate (min 20 chars, terminal punctuation required)
- `CaptureWithFocus()` (line 142) — LLM-guided extraction scoped to persona focus areas (prompt lines 160-175)
- `CaptureFromConversation()` (line 238) — multi-turn dialogue; sets `QualityScore=0.4`

**Injection in ExecutePhase** — `engine/engine.go:629-650`:
```go
focusAreas := persona.GetLearningFocus(phase.Persona)
if e.learningDB != nil && !e.config.DisableLearnings {
    learnings, _ := e.learningDB.FindRelevant(...)
    phase.LearningsRetrieved = len(learnings)
    for _, l := range learnings {
        parts = append(parts, fmt.Sprintf("- [%s] %s", l.Type, l.Content))
    }
    learningsText = strings.Join(parts, "\n")
}
```

`learningsText` → `bundle.Learnings` (engine.go:736) → CLAUDE.md section **"## Lessons from Past Missions"** (`claudemd.go:176-181`).

**Background extraction pipeline** — `engine/engine.go:906-970`. After phase completes:
1. `CaptureFromText` + `CaptureWithFocus` run in parallel goroutines
2. Results merged, inserted into `learnings.db`
3. For persistent workers: `writeLearningsToWorkerMemory()` feeds entries into alpha's in-memory entry list
4. Serialized via `extractMu` + `extractWG` for graceful shutdown

### Persistent worker lifecycle in `ExecutePhase`

| Step | Lines | Action |
|---|---|---|
| Assign | 684-698 | `shouldAssignPersistentWorker()` → lazy `LoadIdentity("alpha")` |
| Memory retrieval | 703-708 | `GetBudgetedMemory(keywords, budget)` |
| Bundle injection | 741 | `bundle.WorkerMemory = workerMemoryText` |
| CLAUDE.md render | `claudemd.go:184-192` | Section injected |
| Post-phase | 1001-1013 | `writeLearningsToWorkerMemory()` + `RecordPhase()` + self-reflect check + `SaveIdentity()` |

**Assignment logic** (`engine/persistent_worker.go:42-64`):
```go
func shouldAssignPersistentWorker(phase *core.Phase, noPersistentWorker bool, sameDayCount int) bool {
    if noPersistentWorker { return false }
    // Exclude review/cleanup/decompose roles
    if sameDayCount >= perRunWorkerCap { return false }  // cap = 5 per run
    return rand.Float64() < persistentWorkerRoll         // 30% probability
}
```

**Memory policies** — `internal/worker/memory.go`:
- **Dedup** (174-187): `contentHash()` + `isDuplicateOf()`
- **Supersedure** (189-192): auto-correction when Jaccard similarity > 0.8 (line 45)
- **Eviction** (283-315): ceiling 100, evict lowest-scoring by recency weight
- **BudgetedMemory** (321-376): returns highest-scoring entries up to budget bytes — scored by keyword overlap + recency

**Phase recording** (`identity.go:243-253`):
```go
func (wi *WorkerIdentity) RecordPhase(domain string, cost float64) {
    wi.PhasesCompleted++
    wi.Domains[domain]++
    wi.TotalCost += cost
    wi.LastActive = time.Now().UTC()
}
```

**Self-reflection** (`engine/persistent_worker.go:108-120`): every 10 phases (`selfReflectionInterval:68`), synthetic entry appended:
```
[reflection] N phases completed; domains: key:count,...; total_cost: X.XXXX; memory_entries: M
```
Type = `"reflection"` (line 71).

## 1.9 File-mode / atomicity summary

| file | mode | atomic? | writer |
|---|---|---|---|
| `checkpoint.json` | 0600 | temp+rename (`checkpoint.go:82-98`) | `core.SaveCheckpoint` |
| `plan.json` | 0600 | no | `cmd/run.go:439` |
| workspace sidecars | 0600 | no | `cmd/run.go:*` |
| `mission.md` | 0600 | no (once at create) | `core.CreateWorkspace` |
| `pid` / `cancel` | 0600 | no | `core.WritePID`, `core.WriteCancelSentinel` |
| `workers/*/CLAUDE.md` | 0600 | no | `worker.Spawn` |
| `workers/*/.claude/hooks/stop.sh` | 0700 | no | `worker.Spawn` |
| `workers/*/output.md` | 0600 | no | `worker.Execute` |
| `workers/*/.claude/settings.local.json` | 0600 | no | `worker.Spawn` |
| `artifacts/...` | 0600 | no | `worker.MergeArtifactsWithMeta` |
| `learnings/*.json` | 0600 | shell-written | `stop.sh` hook |
| `scratch/*/notes.md` | 0600 | no | `engine.extractScratch` |
| `audits.jsonl` | 0600 | append-only | `audit.SaveReport` |
| `metrics.jsonl` | 0600 | append-only | `engine.RecordMetrics` |
| `events/<id>.jsonl` | 0600 | append-only under mutex | `event.FileEmitter` |
| `metrics.db` | sqlite | WAL + 5s busy_timeout + FK ON | `metrics.InitDB` |
| `learnings.db` | sqlite | WAL | `learning.OpenDB`, `routing.OpenDB`, `claims.OpenDB` |
| `workers/alpha/stats.json` | 0600 | temp+rename | `WorkerIdentity.saveStats` |
| `workers/alpha/memory.md` | 0600 | temp+rename | `WorkerIdentity.saveMemory` |
| `workers/alpha/identity.md` | 0600 | no (bootstrap once) | `WorkerIdentity.bootstrap` |

Only three durable files have crash-safe atomic writes: `checkpoint.json`, `workers/alpha/stats.json`, `workers/alpha/memory.md`. Everything else risks truncation on process crash mid-write.

## 1.10 Orchestrator gaps / observations

- **FINDING:** `plan.json` is written once and never read back. `checkpoint.json` is the sole durable state for resume
- **FINDING:** `state.json` (in `README.md:50`) does not exist in v2; dead doc
- **PATTERN:** All three SQLite openers (`learning.OpenDB`, `routing.OpenDB`, `claims.OpenDB`) independently set WAL on the same `learnings.db` file; schema is additive `CREATE IF NOT EXISTS`; no single owner validates cross-package invariants
- **GOTCHA:** `quota_snapshots.mission_id` has no FK to `missions.id` — intentional so snapshots survive mission deletion, but breaks CASCADE
- **GOTCHA:** `event.Bus` sequence is a separate counter from the per-mission JSONL sequence (`event/bus.go:47`, `event/file.go:49-52`). JSONL uses mission-local sequences (resume); SSE cursor uses bus sequence
- **DECISION:** Engine writes `Phase.SessionID` into checkpoints so resume can call `claude --resume <id>` on the still-running Claude CLI session (`engine/engine_test.go:1160-1190`)
- **GOTCHA:** `metrics routing` pulls from `learnings.db`, not `metrics.db`. If `learnings.db` is missing, routing metrics return empty while mission metrics still work
- **GOTCHA:** `orchestrator audit` and `orchestrator audit report` no longer exist — routed to `gyo evaluate`/`gyo report`/`ko apply`. Only `audit scorecard` is wired up

---

# 2. Nen ability system

Nen observes and self-improves. Signals are organized by consumption mode (push, poll, query) because that is what matters to a UI that has to choose.

## 2.1 Live event bus topics (push-stream)

The nen daemon bridges the orchestrator in-process event bus to cross-process subscribers via UDS `~/.alluka/events.sock` and HTTP SSE `http://127.0.0.1:7331/api/events`. JSONL tail at `events/<mission-id>.jsonl` is the degraded fallback.

Same 28 topics as §1.6. Per-topic UI guidance:

| Group | Topic | Typical Data | UI suitability |
|---|---|---|---|
| Mission | `mission.started` | mission_id, decomposed_phases | live-feed, dashboard-card (active count) |
| | `mission.completed` | duration_ms, cost_usd | live-feed, graph (throughput) |
| | `mission.failed` | error, failed_phase | alert-banner, live-feed |
| | `mission.cancelled` | reason | live-feed |
| Phase | `phase.started` | phase_name, persona, skills | live-feed, drilldown |
| | `phase.completed` | duration_ms, output_chars | graph, live-feed |
| | `phase.failed` | error, attempt | alert-banner, drilldown |
| | `phase.skipped` | reason | live-feed |
| | `phase.retrying` | error, attempt | alert-banner, heatmap |
| Worker | `worker.spawned` | persona, binary | live-feed |
| | `worker.output` | chunk (text) | live-feed (streaming tail — **rate-limit hazard**) |
| | `worker.completed` | exit_code, tokens_in/out | live-feed, table |
| | `worker.failed` | error, stderr | alert-banner |
| Decomposition | `decompose.started` | prompt_length | not-ui (ephemeral) |
| | `decompose.completed` | phase_count, model | live-feed, drilldown |
| | `decompose.fallback` | reason | alert-banner (quality signal) |
| Learning | `learning.extracted` | kind, content | audit-trail |
| | `learning.stored` | id, embedding_dim | audit-trail |
| DAG | `dag.dependency_resolved` | depends_on[] | drilldown |
| | `dag.phase_dispatched` | phase_id, worker_id | live-feed |
| Role | `role.handoff` | from_role, to_role | drilldown |
| Contract | `contract.validated` | contract_name | not-ui (noisy) |
| | `contract.violated` | contract_name, detail | alert-banner |
| | `persona.contract_violation` | persona, contract | alert-banner |
| Review | `review.findings_emitted` | count, severity_histogram | dashboard-card |
| | `review.external_requested` | reviewer | live-feed |
| Git | `git.worktree_created` | path | live-feed |
| | `git.committed` | sha, files | audit-trail |
| | `git.pr_created` | url | live-feed, link-card |
| System | `system.error` | error, component | alert-banner |
| | `system.checkpoint_saved` | path | not-ui |
| Signals | `signal.scope_expansion` | summary | alert-banner |
| | `signal.replan_required` | summary | alert-banner |
| | `signal.human_decision_needed` | question | alert-banner |
| File overlap | `file_overlap.detected` | files[], phases[] | drilldown |
| Security | `security.invisible_chars_stripped` | count, location | audit-trail |
| | `security.injection_detected` | pattern, tier | alert-banner |

**Source files:** `skills/orchestrator/internal/event/types.go:16-107` (constants), `internal/event/bus.go:1-148` (in-process bus), `plugins/nen/cmd/nen-daemon/main.go:495-591` (UDS + JSONL bridges).

**Gotcha:** `security.injection_detected` is defined as a topic but **zetsu itself does not publish it** — zetsu is a pure library (see §2.6). A UI wanting live zetsu blocks must add an `Emitter.Emit()` shim at zetsu call sites.

## 2.2 Daemon runtime signals (poll-file)

Short JSON / PID files written atomically by daemons. Best consumed via 1–10s polling.

| Signal | Source | Shape | Subscription |
|---|---|---|---|
| nen-daemon stats | `~/.alluka/nen/nen-daemon.stats.json` — written every 10s by `runDaemon()` (`main.go:625-700`); renderer `cmdStatus` at `main.go:747-848` | `{started_at, total_events, last_event_at, connection_mode:"uds"\|"jsonl", scanners:{name:{routed,errors}}}` | poll 10s |
| nen-daemon liveness | `~/.alluka/nen-daemon.pid` — PID file with `ps -p` staleness check (`main.go:153-196`) | integer PID | poll 30s + `kill -0` probe |
| Scheduler liveness | `~/.alluka/scheduler/daemon.pid` | integer PID | poll 30s |
| UDS socket health | `~/.alluka/daemon.sock` — en scanner probes this in `system-health` | bool | `cli-exec` (dial + close) |
| Peak-hours window | `plugins/nen/peak/peak.go` — `IsPeak()`, `TimeUntilPeakStart/End()`; config at `~/.alluka/peak-hours.json` | `{enabled, weekdays, start_hour, end_hour, tz}` → bool / Duration | `invoke-lib` or read config |

Render all five as a single "Daemon health strip" card.

## 2.3 Active findings (`findings.db`)

The single most UI-valuable state surface in the nen system. All four in-tree scanners plus ko's eval emitter write to this one table; every finding is time-stamped, severity-ranked, scoped, and **deduplicated by semantic key** — so row count = real open issues, not raw event volume.

**Source ref:** `plugins/nen/internal/scan/db.go:87-225` (schema, upsert, indexes); `plugins/nen/internal/scan/types.go:36-89` (`Finding` struct).

**Shape** (`~/.alluka/nen/findings.db:findings`):
```sql
id TEXT PK, ability TEXT, category TEXT, severity TEXT,
title TEXT, description TEXT,
scope_kind TEXT, scope_value TEXT,
evidence TEXT (JSON), source TEXT,
found_at DATETIME, expires_at DATETIME, superseded_by TEXT,
created_at DATETIME
-- Active filter: superseded_by = '' AND (expires_at IS NULL OR expires_at > NOW)
-- Dedup key:     (ability, category, scope_kind, scope_value)
```

**Subscription:**
- Direct: `sql-poll` against `~/.alluka/nen/findings.db` (read-only, WAL)
- Proxied: `mcp` → `nanika_findings` tool (`tools.go:271-366`) with filters `ability`, `severity`, `category`, `active_only`. Default limit 20, max 100

**Facets a UI can pivot on:**

| Facet | Values |
|---|---|
| `ability` | `orchestrator-metrics`, `system-health`, `cost-analysis`, `code-review`, `skill-routing` / `persona-routing` (ko), etc. |
| `severity` | `critical`, `high`, `medium`, `low`, `info` |
| `scope_kind` | `mission`, `phase`, `worker`, `workspace`, `event`, `binary`, `file`, `directory`, `socket`, `eval-config`, `persona` |

**Per-ability category catalog:**

| Ability | Scanner | Categories | UI hook |
|---|---|---|---|
| `orchestrator-metrics` | gyo | `cost-anomaly`, `duration-anomaly`, `failure-rate-anomaly`, `retry-anomaly`, `silent-failure` | anomaly-stream card; severity from z-score |
| `system-health` | en | `binary-freshness`, `workspace-hygiene`, `embedding-coverage`, `dead-weight`, `daemon-health`, `scheduler-health`, `routing-quality`, `mission-activity` | health checklist card |
| `cost-analysis` | ryu | `cost-trend`, `model-efficiency`, `retry-waste`, `output-waste`, `retry-events` | cost dashboard |
| `code-review` | review-scanner | `review-blocker` | drilldown from mission detail |
| `skill-routing` / `persona-routing` / `decomposer` / `phase-planning` / `mission-scoping` | ko eval_emitter (`plugins/nen/cmd/ko/eval_emitter.go:92-158`) | `eval-failure` at pass_rate < 0.60 HIGH, < 0.80 MEDIUM | eval health card |

**PATTERN:** Because dedup is semantic-key-based, a UI does NOT need a diff engine — a refresh query returns current truth. Historical churn is visible via `created_at` vs `found_at` divergence (upsert updates `found_at` but leaves `created_at`).

## 2.4 Self-improvement state

### `shu-findings.json` — component health scores

| | |
|---|---|
| Source | `plugins/nen/cmd/shu/main.go:612-643` (`loadFindings`/`saveFindings`) |
| Shape | `{evaluated_at, results:[{name, score:0-100, trend:"up"\|"down"\|"flat"\|"new", issues:[]}]}` |
| Subscription | `poll-file` (rewritten every `shu evaluate` run — weekly/on-demand) |
| UI suitability | dashboard-card per component |

Components tracked: `engage`, `scout`, `scheduler`, `gmail`, `obsidian`, `ynab`, `linkedin`, `reddit`, `substack`, `youtube`, `elevenlabs`, `ko` (`main.go:484-538`).

### `shu query status` — aggregate rollup

| | |
|---|---|
| Source | `plugins/nen/cmd/shu/main.go:484-538` |
| Shape | `{score:int, critical_count:int, evaluated_at, daemon_running:bool, active_findings:int}` |
| Subscription | `cli-exec` (`shu query status --json`) |
| UI suitability | hero card — single-number system score |

### `ko-history.db` — eval runs and per-test results

| | |
|---|---|
| Source | `plugins/nen/ko/db.go:72-125` |
| Tables | `eval_runs(id, config_path, description, model, started_at, finished_at, total, passed, failed, input_tokens, output_tokens, cost_usd)` + `eval_results(run_id, test_description, passed, output, error, duration_ms, assertions_json, cost_usd, cache_hit)` |
| Subscription | `sql-poll` direct, or `mcp` → `nanika_ko_verdicts` (`tools.go:441-514`) |

**Gotcha:** `ko-history.db` lives at `~/.alluka/ko-history.db` (flat), NOT under `~/.alluka/nen/`. A UI deriving paths from `findings.db`'s parent dir will miss it. Route via `nen-mcp`.

### `ko-cache.db` — LLM response cache

Source: `plugins/nen/ko/cache.go`. Keyed by `(model, prompt_hash)` with TTL. Operational internal; a UI might surface a "cache hit rate" aggregated from `eval_results.cache_hit`.

### `proposals.db`

| | |
|---|---|
| Source | `plugins/nen/cmd/shu/propose.go:643+` + `plugins/nen/ko/quality.go:180-195` |
| Tables | `proposals(dedup_key PK, last_proposed_at, ability, category, tracker_issue)` + `dispatches(id, issue_id, mission_file, workspace_id, started_at, finished_at, outcome)` + `proposal_quality(ability, category, success_count, failure_count, stall_count, total_count, last_updated)` |
| Subscription | `sql-poll` or `mcp` → `nanika_proposals` (`tools.go:372-435`) |

**DECISION:** Quality score uses a `< 3 total → 0.5` neutral band (`quality.go:112-141`). UI rendering this must show score AND sample size — `0.5 neutral-new` must be visually distinct from `0.5 actually-average`. Tooltips aren't enough; use a confidence bar or opacity on low-n rows.

## 2.5 Historical / audit surfaces

| Surface | Source | Subscription | UI |
|---|---|---|---|
| Mission metrics | `metrics.db` — `missions(id, duration_s, phases_failed, phases_total, retries_total, cost_usd_total, started_at)`, `phases(...)` | `sql-poll` or `mcp` → `nanika_mission` (`tools.go:673-786`, filters: `mission_id`, `status`) | table + graph |
| Event log (per mission) | `events/<mission-id>.jsonl` — `{type,timestamp,phase_id,worker_id,data}` | `mcp` → `nanika_events` (`tools.go:792-855`, filters: `mission_id` **required**, `event_type`) or file tail | drilldown |
| Audit reports | `audits.jsonl` — one `AuditReport` per line (`internal/audit/types.go`) | `poll-file` tail | drilldown tied to mission; recommendations feed into `ko apply` |
| Learnings | `learnings.db` with embeddings + FTS | `mcp` → `nanika_learnings` (`tools.go:861-947`, filters: `domain`, `type`, `archived`) | table with full-text search |
| Scheduler jobs | `scheduler/scheduler.db` | `mcp` → `nanika_scheduler_jobs` (`tools.go:520-589`, filter: `enabled_only`) | table |
| Tracker issues | `tracker.db` | `mcp` → `nanika_tracker_issues` (`tools.go:595-667`, filters: `status`, `priority`) | table |
| Remediation missions | `~/.alluka/missions/remediation/<date>-<slug>.md` with YAML frontmatter (`source`, `tracker_issue`, `finding_ids[]`, `severity`, `ability`, `category`, `generated_at`, `domain`, `target`) | `query-fs` (glob + parse) | drilldown from proposals |

**Gotcha:** `nanika_events` requires `mission_id` — no "tail all" mode. Global event feed requires direct bus subscription.

## 2.6 Synchronous checkpoints (invoke-lib / invoke-cli)

Point-in-time answers a UI can call. Not background signals.

| Checkpoint | Source | Shape | Invocation | UI |
|---|---|---|---|---|
| Zetsu pattern match | `plugins/nen/zetsu/zetsu.go` — `CheckChannelMessage(msg)`, `SanitizeObjective(obj)`, `SanitizePriorContext(ctx)` | `Result{Input, Output, Matches:[{Reason, Tier:Flag\|Block}]}` | `invoke-lib` | form-embed — real-time validation on mission-compose |
| Peak hours gate | `plugins/nen/peak/peak.go:63-98` | bool + Duration | `invoke-lib` | dashboard-card badge |
| `ko apply` dry-run | `plugins/nen/internal/audit/apply.go:62-203` — `ApplyRecommendations(workspaceID, dryRun=true)` | structured `ApplyPlan` with file diffs | `cli-exec` (`ko apply --dry-run`) | diff-view |
| `nen-mcp doctor` | `plugins/nen_mcp/cmd/nen-mcp/doctor.go:33-145` | health summary across 9 stores | `cli-exec` (`nen-mcp doctor --json`) | dashboard-card |
| `gyo evaluate` | `plugins/nen/cmd/gyo/main.go` — `gyo evaluate [workspace-id] --format json` | structured report | `cli-exec` | drilldown from mission |
| `ryu report` | `plugins/nen/cmd/ryu/main.go` | cost report | `cli-exec` | drilldown from cost dashboard |

**DECISION:** Zetsu should be surfaced as a form-time validator, not a post-hoc finding. Its call sites are synchronous trust boundaries (channel input, mission objective, prior context); a finding in a table after the fact has no actionable value.

## 2.7 Extension integration surface

### Scanner binary protocol

Source: `plugins/nen/plugin.json:16-40` (registry), `plugins/nen/cmd/nen-daemon/main.go:370-420` (dispatch), `plugins/nen/internal/scan/types.go:65-82` (envelope).

**Registration:** add `scanners.<name>` entry to `~/.alluka/nen/plugin.json` with fields `binary`, `mode:"watch"`, `watch_events:<regex>`, `ability`.

**Invocation contract:** `<binary> --scope '{"kind":"mission","value":"<id>"}'`

**Output contract** — write `Envelope` JSON to stdout:
```go
type Envelope struct {
    SchemaVersion string    // "1"
    ScannerName   string
    ScannedAt     time.Time
    Findings      []Finding
}
```

### nen_mcp — read-only query proxy

Source: `plugins/nen_mcp/cmd/nen-mcp/main.go`, `tools.go:40-126`.

Transport: stdin/stdout JSON-RPC 2.0, MCP protocol `2024-11-05`.

**8 exposed tools, all read-only:**

| Tool | Backing store | Filters | Default / max limit |
|---|---|---|---|
| `nanika_findings` | `nen/findings.db` | ability, severity, category, active_only | 20 / 100 |
| `nanika_proposals` | `nen/proposals.db` | ability | 20 / 100 |
| `nanika_ko_verdicts` | `ko-history.db` | config (substring) | 20 / 100 |
| `nanika_scheduler_jobs` | `scheduler/scheduler.db` | enabled_only | 50 / 200 |
| `nanika_tracker_issues` | `tracker.db` | status, priority | 50 / 200 |
| `nanika_mission` | `metrics.db` | mission_id, status | 20 / 100 |
| `nanika_events` | `events/<id>.jsonl` | mission_id (required), event_type | 100 / 500 |
| `nanika_learnings` | `learnings.db` | domain, type, archived | 20 / 100 |

**Path resolution:** `ORCHESTRATOR_CONFIG_DIR → ALLUKA_HOME → VIA_HOME/orchestrator → ~/.alluka → ~/.via` (`tools.go:198-214`). Scheduler uses `SCHEDULER_CONFIG_DIR`; tracker uses `TRACKER_DB`.

**Response shape:** `{content:[{type:"text", text:"<json>"}], isError?:bool}` — text content is indented JSON the UI must re-parse.

**Recommended read path** for any remote / sandboxed UI — centralizes path resolution, env-var indirection, and limits.

### Event bus Emitter interface

Source: `skills/orchestrator/internal/event/emitter.go:10-13`.

```go
type Emitter interface {
    Emit(ctx context.Context, event Event) // fire-and-forget
    Close() error
}
```

Producer side — a UI would never emit, but a backend "UI action → mission" bridge wires in here.

## 2.8 Nen gaps

| Gap | Impact | Workaround |
|---|---|---|
| `In` scanner not implemented | No "stealth/hygiene" ability coverage; HxH model has a hole | Register a new scanner, or leave slot empty |
| Zetsu does not emit `security.injection_detected` | Topic exists, no publisher | Add `Emitter.Emit` shim at zetsu call sites, or surface at form validation (recommended) |
| `subscriberDrops` is private | Reconnecting UI cannot tell if it missed events | UI must maintain last-seen `Sequence` cursor + `EventsSince(seq)` on reconnect. Expose `subscriberDrops` via `nen-daemon.stats.json` |
| `ko apply` is manual | Dispatch runs and writes audits, but persona/skill files do not change until a human runs `ko apply <ws-id>` | UI "pending apply queue" needs a new marker — sidecar file or new column on `dispatches` |
| Dual-judge disagreements flagged, not failed | `review:true` rows in `eval_results.assertions_json` pass silently | UI must explicitly filter `assertions_json LIKE '%"review":true%'` and render "Needs human review" inbox |
| No unified "global event tail" MCP tool | `nanika_events` requires `mission_id` | Add MCP `nanika_event_stream` or use SSE/UDS directly |
| `ko-history.db` path inconsistency | Flat at `~/.alluka/`, not under `nen/` | Always route via `nen-mcp` |
| `findings.db` has no change stream | Upserts do not emit events — poll only | Poll MCP tool at 5–30s, or emit synthetic `findings.upserted` from `UpsertFinding` |

## 2.9 Recommended UI subscription strategy

**Three candidates considered:**

- **A. Thin MCP proxy** — UI talks only to `nen-mcp`; no direct SQLite, no bus
- **B. SSE + MCP split** — UI subscribes to bus (UDS/SSE) for live feeds, queries `nen-mcp` for tables
- **C. Direct SQLite + direct UDS** — UI opens findings.db / metrics.db / ko-history.db read-only and dials `events.sock` directly

| Axis | A | B | C |
|---|---|---|---|
| Implementation effort | lowest | medium | highest |
| Operational complexity | 1 subprocess | 2 subprocesses | 0 new |
| Live data latency | 5–30s polling | real-time push | real-time push |
| Extensibility | ✔ MCP change, UI unchanged | ✔ same | ✘ every new store needs UI code |
| Solo-maintainer load | lowest | medium | highest |

**DECISION: Choose B (SSE + MCP split).** Live signals and state queries have genuinely different consumption models. Forcing both through MCP polling makes the UI feel dead; going direct duplicates `nen-mcp`'s path-resolution logic.

**Minimum-viable scope (3-month horizon):**
1. UI server reads `nen-mcp` for all tables in §2.3/§2.4/§2.5
2. UI server subscribes to `events.sock` via the `readUDSWithReconnect` pattern from `nen-daemon/main.go:495-537`
3. Persist `last_sequence` to `~/.alluka/ui/cursor.json` and call `EventsSince()` on reconnect
4. Ship exactly 4 widgets: hero health card (`shu query status --json`), findings table (`nanika_findings`), live mission feed (mission + phase topics), daemon health strip

**What is NOT in scope:** zetsu live stream (use form-embed), `ko apply` pending queue (blocked on missing "applied" marker), `worker.output` chunk tailing (perf hazard — drop-on-slow fan-out + high volume).

---

# 3. Skills & Personas

Single reference for everything a Claude-Console-style "Skills and Agents" panel could subscribe to. Signals grouped by function: **inventory** (what exists), **usage** (what's being used), **discovery** (how new/changed capabilities surface).

> **Path correction.** Earlier dig phases cited source refs under `plugins/orchestrator/internal/...`. The actual Go module lives at `skills/orchestrator/internal/...`. Every ref below is against the current tree.

## 3.1 Inventory — "what exists"

### `SKILL.md` files (filesystem)

| | |
|---|---|
| **Source ref** | `.claude/skills/<name>/SKILL.md`, `plugins/*/skills/SKILL.md` — 37 files (22 + 15) |
| **Shape** | YAML frontmatter: `name`, `description`, `version`, `allowed-tools`, `keywords`, `category`, `license`, `author` — **inconsistent**: some versions live at `metadata.version`, others top-level. Coverage: name/description 37/37, version 19/37, allowed-tools 23/37 |
| **Subscription** | Read-on-demand via glob. No watcher today. Recommend: fsnotify on the two parent dirs (plus symlink targets — 18 skills symlink to `~/.agents/skills/`) |
| **UI suitability** | Skill detail pane (description, version, allowed-tools badge, source-link). Listing view: name + description |
| **Gotchas** | Two broken repo-relative symlinks (`requesting-code-review`, `security-best-practices`). `nen_mcp` uses `mcp__*` allowed-tools instead of `Bash(<cli>:*)` — special-case the tool-badge renderer |

### Generated skill index (`CLAUDE.md` block)

| | |
|---|---|
| **Source ref** | `~/nanika/CLAUDE.md` between `<!-- NANIKA-AGENTS-MD-START -->` / `<!-- NANIKA-AGENTS-MD-END -->`, loaded by `worker/skillindex.go:19-46` (`LoadSkillIndex`, `extractAgentsMD`). Regenerated by `scripts/generate-agents-md.sh` |
| **Shape** | Pipe-delimited text: `\|name — description:{path}\|`. Parsed by `ParseSkillNames` (`skillindex.go:51`) and `FormatSkillsForDecomposer` (`skillindex.go:71`). **Lossy** — version, allowed-tools, category are dropped |
| **Subscription** | File read on each engine construction (cached as `Engine.skillIndex`). Never reloaded mid-run. No watcher |
| **UI suitability** | **Poor as primary source.** Use only as "what the worker actually sees" fidelity check. Read SKILL.md for everything else |
| **Gotchas** | Stale whenever `SKILL.md` is added/changed until the script runs. Panel should surface "last generated" (file mtime) and a Refresh action |

### Persona catalog (in-memory)

| | |
|---|---|
| **Source ref** | Struct: `internal/persona/personas.go:20-36`. Loader: `readCatalog` (`personas.go:55-140`). Store: `var catalog map[string]*Persona` guarded by `sync.RWMutex`. Personas: 11 files under `personas/` (10 agent roles + `default` voice) |
| **Shape** | `Persona{ Name, Title, Content, PromptBody, Role, Capabilities[], Triggers[], Handoffs[], InferredRole, Expertise[], WhenToUse[], WhenNotUse[], LearningFocus[], HandsOffTo[], OutputRequires[] }` |
| **Subscription** | **Real-time.** `StartWatcher` (`personas.go:193-250`) uses fsnotify with 50ms debounce on create/write/remove/rename. Polling fallback every 30s if fsnotify unavailable. Mutations flip the whole map atomically |
| **UI suitability** | Agents list; agent detail; handoff graph (via `Handoffs[]`); "which section shape does output require" badge (via `OutputRequires[]`) |
| **Gotchas** | 10 of 11 personas lack `name`/`description` frontmatter — UI should derive display name from filename and `Title` heading. `default` persona has no `Role` and isn't routable. **No `model_tier` hint in any persona** — model selection is runtime, not declared |

## 3.2 Usage — "what's being used"

### `metrics.db` → `phases` table

| | |
|---|---|
| **Source ref** | DDL: `metrics/db.go:282-305`. Insert: `metrics/db.go:614-630`. Path: `~/.alluka/metrics.db` |
| **UI suitability** | **Per-persona**: count, avg duration, retry rate, token/cost totals, error-type histogram. **Per-skill** (via `parsed_skills`): coarse-grained "invoked this run" bitmap — prefer `skill_invocations` for precise counts |
| **Gotchas** | No per-phase absolute timestamps — only `duration_s`. Join with `plan.json` for timelines. `selection_method` is the "why this persona" provenance. `parsed_skills` is concatenated text — not joinable |

### `metrics.db` → `skill_invocations` table

| | |
|---|---|
| **Source ref** | DDL: `metrics/db.go:307-315`. Insert: `metrics/db.go:673-676` |
| **Shape** | `(id, mission_id, phase, persona, skill_name, source ∈ {declared, observed}, invoked_at)` |
| **UI suitability** | **Primary source for skill usage tiles.** "Recently used" = `ORDER BY invoked_at DESC`. "Top skills" = `COUNT(*) GROUP BY skill_name`. "Skills by persona" = `GROUP BY persona, skill_name`. Declared vs observed lets the panel show "assigned but not actually used" gaps |
| **Gotchas** | **This is the only honest skill last-used timestamp in the system.** `learnings.db` has `last_used_at`/`used_count` but no writer — do not read them |

### `learnings.db` → `routing_decisions` table

See §1.4 for full schema. UI uses:
- "Why this persona?" explanation pane
- Confidence distribution histogram
- Per-persona success-rate roll-up (`persona + outcome` index exists)
- Failure drill-down via `failure_reason`

**Gotcha:** shared file with learnings writers — contention possible; poll 2–5s.

### `learnings.db` → `role_assignments` & `handoff_patterns`

`role_assignments` (routing/db.go:263): which persona was tried for which role and whether it worked. `handoff_patterns` (routing/db.go:182): which persona sequences chained successfully.

**UI:** Relationship graph in Agents panel — edges between personas weighted by handoff frequency. "Best persona for role X" tile.

### `audits.jsonl` — post-mission scorecards

See §1.2. The only place `persona_correct` / `persona_ideal` verdicts live.

**UI:** Primary quality signal for Agents panel:
- "% correct" per persona (`persona_correct` aggregated)
- "Persona drift" view (`persona_assigned` vs `persona_ideal`)
- Scorecard trendlines
- Recommendations feed

**Gotcha:** grows unbounded — paginate from tail, do not `LoadReports` whole file.

### `learnings.db` → `learnings` table (persona attribution via `worker_name`)

| | |
|---|---|
| **Source ref** | Worker naming: `worker/spawn.go` — `{persona}-{phase_id}`. `workspace_id` links to mission |
| **Shape** | Learnings have `worker_name`, `workspace_id`, `seen_count`, `injection_count`, `compliance_count`, content fields |
| **UI suitability** | "Lessons generated by persona X" tile — group by `{persona}-*` prefix of `worker_name`. Evidence trail linking persona → insights authored |
| **Gotchas** | **Do not use `learnings.last_used_at` or `learnings.used_count` — no writer.** Use `seen_count`, `injection_count`, `compliance_count` |

## 3.3 Discovery — "how new/changed capabilities surface"

### Persona fsnotify watcher (auto-reload)

| | |
|---|---|
| **Source ref** | `persona/personas.go:193-250` (`StartWatcher`) |
| **Shape** | Side-effecting — mutates `var catalog` in place on any create/write/remove/rename in `personas/`. No event channel exposed today |
| **Subscription** | Already subscribed inside orchestrator process. To expose to UI: add broadcast channel alongside mutex, fan-out to SSE/WebSocket clients |
| **UI suitability** | **Live Agents panel.** Edit a persona file in the editor, see the panel tile update within 50ms. The headline differentiator over static-catalog UIs |

### Skill index manual refresh

| | |
|---|---|
| **Source ref** | `scripts/generate-agents-md.sh`. Consumed by `LoadSkillIndex` at engine construction |
| **Shape** | Shell script rewrites the marked block in `~/nanika/CLAUDE.md` |
| **Subscription** | None. Manual invocation |
| **UI suitability** | "Refresh catalog" button in Skills panel header — shells out, then re-reads `CLAUDE.md` and `SKILL.md` files. Surface "last generated" mtime as freshness badge |
| **Recommended upgrade** | Add fsnotify watcher on `.claude/skills/*/SKILL.md` and `plugins/*/skills/SKILL.md` inside orchestrator, auto-trigger regeneration. Closes the asymmetry vs persona watcher |

### `decomposition_findings` / `phase_shape_patterns` / `routing_corrections`

See §1.4. Structural findings (`wrong_persona`, `missing_phase`, `phase_drift`, `redundant_phase`, `low_phase_score`) produced by the audit loop.

**UI:** Discovery feed — "the system thinks persona X is being misrouted to role Y". Hinge for the self-improving pitch in the Agents panel.

## 3.4 Skills & Personas design decisions

### Transport: poll vs push vs hybrid

**Selected: hybrid — fsnotify push for persona catalog, SQLite/jsonl poll for everything else.** Rejected event bus on operational-complexity grounds; rejected pure poll because persona watcher already gives free push — losing it would be a regression.

### Schema normalization

**Selected: normalize at UI load time** in the `SkillsProvider` adapter. Rejected rewriting the 37 source files (invasive, touches symlinked targets in `~/.agents/skills/`). Rejected a compiled `skills-catalog.json` artifact (drift between source and artifact, same problem as the `CLAUDE.md` index).

### Panel shape

**Selected: split Skills / Agents tabs** rather than unified "Capabilities" view. The hot-reload asymmetry (personas live, skills manual) is a real behavior difference worth teaching via panel shape.

### Interface sketch

```go
type SkillsProvider interface {
    List(ctx) ([]SkillSummary, error)
    Detail(ctx, name) (*SkillDetail, error)
    Refresh(ctx) error
    TopUsed(ctx, window time.Duration) ([]SkillUsage, error)
    RecentlyUsed(ctx, limit int) ([]SkillInvocation, error)
    Subscribe(ctx) (<-chan SkillEvent, error)   // upgrade goal
}

type PersonasProvider interface {
    List(ctx) ([]PersonaSummary, error)
    Detail(ctx, name) (*PersonaDetail, error)
    UsageStats(ctx, persona string) (*PersonaUsage, error)
    RoutingHistory(ctx, persona string, limit int) ([]RoutingDecision, error)
    QualityStats(ctx, persona string) (*PersonaQuality, error)
    Handoffs(ctx) ([]HandoffEdge, error)
    Subscribe(ctx) (<-chan PersonaEvent, error)
}
```

## 3.5 Skills & Personas risks

1. Concurrent writes to `learnings.db` — routing, learnings, role assignments all write to the same SQLite file. Mitigate: single-connection read-only handle, `PRAGMA query_only=ON`, 2s+ poll floor
2. `audits.jsonl` unbounded growth — stream-read from tail, never `LoadReports` on hot path
3. Skill index staleness — "what the decomposer sees" and "what's on disk" diverge until the script runs. Surface "last generated" prominently
4. `parsed_skills` in `phases` is opaque text — do not try to build skill-usage tiles off it. Use `skill_invocations`
5. `selection_method` semantics not enumerated — render as opaque string until someone pins the enum

---

# 4. Scheduler & Tracker

Both subsystems store authoritative state in SQLite. All tables directly readable via WAL mode — UIs can attach read-only and observe without going through CLI. **The two systems do not share a foreign key** — every join is a convention layered on top of freeform fields.

- **Scheduler DB:** `~/.alluka/scheduler/scheduler.db` (WAL)
- **Tracker DB:** `~/.tracker/tracker.db` (WAL, `TRACKER_DB` env override)

## 4.1 Scheduler state (SQLite)

### `jobs`

Source: `plugins/scheduler/internal/db/db.go:233-246` + migrations L163, L196.

Schema (abridged): `id:int64, name:text-unique, command:text, schedule:text, schedule_type:{cron|random|at|every|delay}, random_window:text?, shell:text, enabled:bool, timeout_sec:int, priority:{P0|P1}, last_run_at:iso8601?, next_run_at:iso8601?, created_at, updated_at`.

`name` is unique and is the **primary cross-subsystem join key**.

### `runs`

Source: `db.go:248-259`.

Schema: `id:int64, job_id:int64→CASCADE, status:{pending|running|success|failure|timeout}, exit_code:int?(-1 when exec fails), stdout:text, stderr:text, started_at, finished_at?, duration_ms?`. `idx_runs_started_at DESC` supports efficient tailing. stdout/stderr unbounded — truncate in UI (40 chars in history, 200 in JSONL).

### `job_audit`

Source: `db.go:295-310`, `audit.go:28,91-104`.

Schema: `id, job_id:int (NOT FK — survives delete), op:{create|update|delete}, before_json:text, after_json:text, actor:text, ts`. `actor` resolves from `CLAUDE_CODE_SESSION_ID` → `USER@HOSTNAME` → `"unknown"`. Survives job deletion — useful for "who changed what" history even after job removal.

### `posts`

Source: `db.go:266-277`, migration L222.

Schema: `id, platform, content, args, scheduled_at, status:{pending|done|failed|cancelled}, run_output, published_url, interval`. Shares `status` vocabulary words with tracker (`done`/`cancelled`) — same words, different domain.

### `optimal_times`

Source: `db.go:284-291`, seed L315-412. Schema: `id, platform, day, hour, minute, label`. Reference data; no join.

### `Stats` aggregate

Source: `db.go:1039`. Shape: `{TotalJobs, EnabledJobs, TotalRuns, PendingRuns, RunningRuns, TotalPosts, PendingPosts, DonePosts, FailedPosts}`. Computed on read — dashboard summary card.

**Retention:** None. `runs` and `job_audit` grow indefinitely. UI must paginate or rolling-window query.

## 4.2 Tracker state (SQLite)

### `issues`

Source: `plugins/tracker/migrations/001_initial.sql:1-11` + 002, 003.

Schema: `id:text (trk-XXXX hash), seq_id:int? (TRK-N display), title:text, description:text?, status:{open|in-progress|done|cancelled}, priority:{P0|P1|P2|P3}?, labels:csv?, assignee:text?, parent_id:text?→CASCADE, created_at, updated_at`.

`seq_id` is the UX identifier; `id` hash is the referential identifier. `labels` is freeform CSV — the expected cross-system join seam (see §4.6). `parent_id` is single-parent FK; no depth limit.

### `links`

Source: `migrations/001_initial.sql:13-21`, `src/commands.rs:199`.

Schema: `id:int, from_id:text→CASCADE, to_id:text→CASCADE, link_type:{blocks|relates_to|supersedes|duplicates}, created_at`. Directional — `from_id` blocks `to_id`. Used to compute readiness (§4.6.5).

### `comments`

Source: `migrations/001_initial.sql:23-30`. Schema: `id:int, issue_id:text→CASCADE, body:text, author:text?, created_at`. Detail-pane only.

### `schema_migrations`

Source: `migrations/001_initial.sql:32-35`. Schema: `version:int, applied_at`. Diagnostic.

### Computed projections

- **Tree** (`src/commands.rs:395-439`) — `HashMap<Option<parent_id>, Vec<&Issue>>` DFS traversal. UI can build tree from flat `query action tree` output
- **Ready** (`src/commands.rs:285-357`) — open issues with no unresolved transitive blocker. Cycle-safe DFS with visited set

**Retention:** None. Comments and links accumulate indefinitely.

## 4.3 Scheduler CLI

Every CLI command is a pull-based signal.

| Signal | Command | Source | Output | JSON? |
|---|---|---|---|---|
| Overview stats | `scheduler status` | `plugins/scheduler/internal/cmd/status.go:26-75` | 10-field text: jobs total/enabled, runs total, runs running, posts total/pending, posts done, posts failed, daemon PID, db path, shell, max-concurrent, log-level | ❌ — use `query status` |
| Job list | `scheduler jobs` | `jobs.go:142-200` | 6-col text table: `ID, NAME, SCHEDULE, NEXT RUN, ENABLED, COMMAND` (command truncated to 40) | ❌ — use `query items` |
| Run history | `scheduler logs <id> [--limit N]` | `logs.go:25-78` | Run entries: `[started_at → finished_at] exit N (durMs) — status` + stdout + stderr | ❌ |
| Event stream | `scheduler history [--limit N]` | `history.go:27-84` | 6-col text table: `TIME, STATUS, JOB, EXIT, DURATION, STDERR` (40 chars + "…") | ❌ |
| Immediate exec | `scheduler run <id>` | `run.go` | Text: "running…", stdout, stderr, "finished in Xs (exit N, status: …)" | ❌ — use `query action run` |
| Daemon status | `scheduler query status [--json]` | `query.go:136-194` | `{daemon_running:bool, job_count:int, enabled_count:int, next_run_at:iso8601?}` | ✅ |
| Job list (JSON) | `scheduler query items [--json]` | `query.go:196-267` | `{items:[{id, name, schedule, enabled, last_run?, next_run?, last_exit_code?}], count}` | ✅ |
| Action invocation | `scheduler query action {run\|enable\|disable} <id> [--json]` | `query.go:287-350` | `{ok, job_id, action, message, exit_code?, stdout?, stderr?}` | ✅ |
| Action catalog | `scheduler query actions [--json]` | phase-2 §7 | `{actions:[{name, command, description}]}` | ✅ |
| Cron dry-run | `scheduler jobs add --dry-run` | phase-2 §2 | Next 5 fire times | ❌ |
| `jobs next <id>` | `scheduler jobs next <id>` | phase-2 §2 | "N. DayName, Date Time (in Xd Yh)" or "one-shot: already fired" | ❌ |
| Doctor | `scheduler doctor [--json]` | phase-2 §9 | Health report | ✅ |

**Gotcha:** `scheduler status` does NOT accept `--json`. For machine-readable status, use `scheduler query status --json`.

## 4.4 Tracker CLI

| Signal | Command | Source | Output | JSON? |
|---|---|---|---|---|
| Issue list | `tracker list [--status S] [--priority P]` | `src/commands.rs` (comfy_table UTF8_FULL) | 5-col text table: `ID, Title, Status, Priority, Assignee` | ❌ — use `query items` |
| Issue detail | `tracker show <ID>` | `src/commands.rs` | Key-value text + outgoing/incoming links + comments | ❌ — no JSON equivalent |
| Health/count | `tracker query status [--json]` | `src/query.rs` | `{ok:true, count:int, type:"tracker-status"}` | ✅ |
| Issue list | `tracker query items [--json]` | `src/query.rs` | `{items:[Issue], count}` sorted `seq_id DESC`. Full struct | ✅ |
| Action catalog | `tracker query actions [--json]` | `src/query.rs` | `{actions:[{name, command, description}]}` for `{next, ready, tree}` | ✅ |
| Next issue | `tracker query action next [--json]` | `src/commands.rs:360-371` | `{action:"next", issue:Issue\|null}` | ✅ |
| Ready issues | `tracker query action ready [--json]` | `src/commands.rs:285-357` | `{action:"ready", items:[Issue], count}` | ✅ |
| Tree | `tracker query action tree [--json]` | `src/commands.rs:395-439` | `{action:"tree", items:[Issue]}` flat, ordered `created_at` asc | ✅ |

**Gotcha:** `tracker list` and `tracker show` have **no JSON flag**. UIs needing programmatic access to the `show` shape must reconstruct it from `query items` + a separate links/comments query that **does not currently exist as JSON**.

**DECISION:** Tracker currently lacks a `query action show <id> --json` surface. Implementing that is a prerequisite for any drill-down UI that reuses the `show` layout.

## 4.5 Scheduler push/IPC

| Signal | Location | Source | Schema | Subscription |
|---|---|---|---|---|
| JSONL event log | `~/.alluka/events/scheduler.jsonl` | `daemon_cmd.go:27-36,310` | per-line JSON: `{type:"schedule.completed"\|"schedule.failed", job_id, job_name, command, duration_ms, exit_code, stderr?(≤200 chars), ts:iso8601}` | **tail-able file** (append-only, no rotation) |
| UDS event push | `~/.alluka/orchestrator.sock` | `daemon_cmd.go:312`, `--notify` flag | same shape as JSONL | **push** (500ms timeout, non-blocking, no error logging) |
| Daemon PID | `~/.alluka/scheduler/daemon.pid` | `daemon_cmd.go` | single int | pull (stat + `signal(0)`) |

**No HTTP endpoints.** Scheduler is CLI + file + UDS only.

**Event log retention:** None. Unbounded append-only. UI must implement its own rotation / tail cursor.

**DECISION:** UDS socket has only one consumer slot — orchestrator holds it by default. A UI needs either (a) its own socket path wired via `--notify`, (b) orchestrator-side fan-out, or (c) tailing the JSONL file. Recommend (c) for simplicity.

### Tracker push/IPC

**None exist.** Tracker has no event log, no socket, no push mechanism, no HTTP endpoint. Purely pull-based — a reactive UI must poll `query items` (2–5s active, 30s idle) or attach directly to SQLite in WAL mode and diff.

## 4.6 Cross-system joins

### D.1 Label-based correlation (PRIMARY JOIN)

**Seam:** `tracker.issues.labels` (CSV) ↔ `scheduler.jobs.name` (UNIQUE text).

**Convention:** `scheduler:<job-name>` label prefix on tracker issues. A UI can resolve:

```sql
-- tracker side
SELECT id, seq_id, title, labels FROM issues WHERE labels LIKE '%scheduler:%';
-- scheduler side, for each matched label
SELECT id, next_run_at, enabled FROM jobs WHERE name = ?;
```

- `jobs.name` is `NOT NULL UNIQUE` (`db.go:233`) → 1-to-1 resolution
- Tracker `labels` is freeform CSV → naming discipline required; no schema enforcement
- Symmetrical lookup works both directions

**DECISION:** Label prefix convention is not currently enforced. Document as convention and rely on discipline for v1 — matches nanika's "minimum viable architecture" guidance.

### D.2 Orchestrator missions as bridge

Scheduler jobs and tracker issues are both created/mutated during a mission.

- `scheduler.job_audit.actor` = `CLAUDE_CODE_SESSION_ID` when run from Claude session
- Tracker has **no equivalent actor field** — issue mutations unattributed beyond `comments.author`

**GOTCHA:** `CLAUDE_CODE_SESSION_ID` is set by the harness, not the orchestrator. If orchestrator runs in a detached subprocess, the session ID may not propagate.

### D.3 Priority semantics divergence

Both systems use `P0/P1/...` strings but **with different meanings**:

| Priority | Scheduler (`jobs.priority`) | Tracker (`issues.priority`) |
|---|---|---|
| P0 | "runnable during peak hours" (nen/peak gated) | Critical / emergency, rank 0 |
| P1 | "deferred during peak hours" (default) | High, rank 1 |
| P2 | — (not used) | Medium, rank 2 |
| P3 | — (not used) | Low, rank 3 |
| (unset) | — (always `P1`) | rank 4 (lowest) |

Sources: scheduler `db.go:196` (migration), `daemon_cmd.go:240,270` (peak gate). Tracker `src/commands.rs:360-381`.

**GOTCHA:** Aligning these by rank is misleading. A scheduler `P0` job is not urgent; it is allowed to run during peak hours. Label distinctly (e.g., "peak-ok" vs "critical").

### D.4 Status lifecycle divergence

| System | Column | Values |
|---|---|---|
| Scheduler `runs.status` | execution state | `pending → running → success \| failure \| timeout` |
| Scheduler `posts.status` | publish state | `pending \| done \| failed \| cancelled` |
| Tracker `issues.status` | work state | `open \| in-progress \| done \| cancelled` |

**Overlap words:** `pending`, `done`, `cancelled`, `failed` appear in multiple places but mean different things. UI aggregating across subsystems must **namespace by source** (`scheduler.run.status=failure`, `tracker.issue.status=done`) to prevent misleading pivot tables.

### D.5 Ready / readiness algorithm

| System | "Ready" definition | Source |
|---|---|---|
| Scheduler (implicit) | `enabled = 1 AND next_run_at <= now AND id NOT IN (currently_running_set)` | `daemon_cmd.go` poll loop |
| Tracker (explicit) | `status = 'open' AND no transitive blocker with status ∉ {done, cancelled}` via DFS with visited set | `src/commands.rs:285-357` |

**PATTERN:** Tracker's DFS readiness algorithm is cleaner than scheduler's implicit set subtraction and could be extracted as a reusable primitive. Tracker's cycle-safe DFS is directly portable; scheduler doesn't need it today (no cross-job blocking relationships exist).

### D.6 Time ordering — both ISO 8601 UTC

Scheduler uses `strftime('%Y-%m-%dT%H:%M:%SZ','now')` (second precision); tracker uses RFC3339 (microsecond precision e.g. `2026-04-11T09:45:22.987654Z`). Sort-order operations are correct, but **equality comparisons across subsystems will never match**.

### D.7 Retention gap (shared operational risk)

Both subsystems have **no retention policy** on `scheduler.runs`, `scheduler.job_audit`, `scheduler.jsonl`, `tracker.comments`, `tracker.links`. Any UI tailing these must:
1. Use `seq_id DESC` / `started_at DESC` indexes for pagination
2. Implement rolling-window queries
3. Default every table view to `LIMIT 500` with a "load more" affordance
4. Never issue unbounded `SELECT *`

### D.8 Join recipe summary

| Goal | Recipe | Confidence |
|---|---|---|
| "Given issue TRK-N, what scheduler jobs are attached?" | `SELECT labels FROM issues WHERE seq_id=N` → parse `scheduler:*` tokens → `SELECT * FROM jobs WHERE name IN (...)` | medium — label convention |
| "Given a failed scheduler run, what issue owns it?" | `runs` → no automatic link. Reverse via job name → search `issues.labels` | low — reverse direction needs full scan |
| "Who triggered this change?" | Scheduler: `job_audit.actor`. Tracker: **no equivalent** | partial — tracker gap |
| "Live event feed for the whole system?" | `tail -f ~/.alluka/events/scheduler.jsonl` + poll `tracker query items` | asymmetric — tracker has no event stream |
| "What should I work on next?" | `tracker query action next --json` alone; scheduler has no equivalent | tracker-only |
| "Combined dashboard stats" | `scheduler query status` + `tracker query status`, two subprocess calls | works |

## 4.7 Scheduler+Tracker DECISION markers

| ID | Decision | Recommendation |
|---|---|---|
| **D-1** | How should UIs observe scheduler events? | Tail `~/.alluka/events/scheduler.jsonl`. Don't compete for UDS; don't parse `scheduler history` (perf hazard — reads full JSONL into memory) |
| **D-2** | How should UIs observe tracker changes? | Poll `tracker query items --json` at 2–5s (active) / 30s (idle). A tracker event log is future enhancement |
| **D-3** | How should issues and jobs be joined? | Label convention `scheduler:<job-name>`. Document; do not enforce via schema in v1 |
| **D-4** | Should tracker gain a JSON `show` command? | **Yes** — add `tracker query action show <id> --json` returning `{issue, links, comments}`. Required for drill-down UIs |
| **D-5** | Should priority/status vocabularies be aligned? | **No.** Namespace by source in UI. Alignment would break semantic distinctions |
| **D-6** | Should tracker gain an audit trail parallel to `scheduler.job_audit`? | Not required for v1. Flag as follow-up if cross-system attribution becomes user-visible |
| **D-7** | How should UIs resolve ID formats? | Accept both `TRK-N` and `trk-XXXX` (tracker already normalizes). Scheduler jobs are `int64` only |
| **D-8** | Should retention be added as part of this work? | **No.** Separate workstream. UI must impose its own query limits |

## 4.8 Scheduler+Tracker risks

1. No retention — unbounded growth. Mitigate: default `LIMIT 500` + pagination
2. UDS socket single-consumer — orchestrator holds `~/.alluka/orchestrator.sock`. Tail JSONL instead
3. No cross-system FK — all joins are convention-based on freeform labels
4. Status vocabulary collision — `done`/`cancelled`/`failed`/`pending` in three places with unrelated meanings
5. Priority-letter collision — `P0`/`P1` different semantics
6. Text-only CLI surfaces — formats not stability-guaranteed; JSON-only programmatic clients
7. `scheduler history` reads full JSONL into memory before `--limit` (phase-2 note). At scale (>100k lines) becomes latency/memory hazard. Prefer direct DB queries or own JSONL cursor
8. Tracker has no change-notification — cache-invalidation via mtime brittle in WAL (the WAL file changes, not the DB)

---

# 5. Learning / Memory / Dream

UI-oriented map of every observable signal in the learning, memory, and dream subsystems. Replaces embedded reference templates in the four dig-phase artifacts as the single entry point.

## 5.1 Learnings DB signals

Base: `~/.alluka/learnings.db`. Schema already covered in §1.4 — this section lists consumption-mode signals.

| # | Signal | Source | Shape | Subscription | UI |
|---|---|---|---|---|---|
| 5.1.1 | Insert stream | `learning/db.go:92-169` | row: `id, type, content, domain, quality_score, created_at, embedding` (float32×3072 BLOB), counters | `sql-diff` on `created_at > ?` | History-chart, Poll-dashboard (inserts/min) |
| 5.1.2 | Quality score distribution | `learning/db.go:330-467` | `quality_score REAL ∈ [0,1]`, mutated by decay + search | `sql-poll` histogram | Snapshot-view, Poll-dashboard |
| 5.1.3 | Injection / compliance counters | `learning/db.go:917-948` | `injection_count`, `compliance_count`, `compliance_rate` | `sql-diff` on counters | Poll-dashboard, History-chart |
| 5.1.4 | Archived (soft-delete) flips | `learning/db.go:829-899` | `archived` flip `0→1` by 4 archival criteria | `sql-poll WHERE archived=1` | Poll-dashboard ("dead weight" panel) |
| 5.1.5 | Promotable rows | `learning/db.go:962-987` | `WHERE quality_score > 0.7 AND promoted_at IS NULL AND archived=0` | `sql-poll` | Snapshot-view ("ready to promote") |
| 5.1.6 | FTS sync triggers | `learning/db.go:124-144` | SQLite triggers on `learnings_ai/ad/au` | **in-proc only** | Off-limits |
| 5.1.7 | Embedding presence | `learning/embed.go:20-21` | `embedding BLOB` NULL vs 3072-dim | `sql-poll embedding IS NOT NULL` | Poll-dashboard (coverage %) |
| 5.1.8 | Cleanup / decay events | `learning/db.go:641-741` | row delta after `Cleanup()` (age/score/cap) + decay by compliance tier | `sql-diff` on `quality_score`, row count | History-chart |
| 5.1.9 | Retrieval ranking breakdown | `learning/db.go:439-524` | `relevance×0.5 + quality×0.3 + recency×0.2`; values only at query time | **in-proc only** | Off-limits |

**Cross-cutting gap — no `updated_at`.** The `learnings` table has `created_at` and `last_used_at` but no general `updated_at`. Counter and quality-score mutations are invisible to a `WHERE updated_at > ?` subscriber.

## 5.2 Dream pipeline

Base: `learnings.db` + `skills/orchestrator/internal/dream/`.

| # | Signal | Source | Shape | Subscription | UI |
|---|---|---|---|---|---|
| 5.2.1 | `processed_transcripts` state | `dream/store.go:59-73,147-155` | `(path, content_hash, msg_count, chunk_count, processed_at)` | `sql-poll COUNT(*)` + scan | Poll-dashboard (dedup cache gauge) |
| 5.2.2 | `processed_chunks` state | `dream/store.go:75-88,147-155` | `(transcript_path, chunk_hash, chunk_index, processed_at)` | `sql-poll` | Poll-dashboard |
| 5.2.3 | Dream run report | `cmd/dream.go:176-183`, `dream/types.go:90-102` | `dream: discovered=N skipped=N processed=N chunks=N llm-calls=N stored=N rejected=N duration=T` | `cli-exec` after `dream run` | Snapshot-view; **Fragile** (plain-text, no JSON) |
| 5.2.4 | Run errors | `cmd/dream.go:184-189` | stderr: `[phase] path: err` (phase ∈ `hash/parse/chunk/extract/store/process`) | `cli-exec` stderr | Snapshot-view; Fragile |
| 5.2.5 | Verbose trace lines | `dream/runner.go:160,177,194` | `dream: skip %s (unchanged)` / `(worker session)` / `%s — %d msgs → %d chunks` | `cli-exec` stdout (`--verbose`) | Off-limits for UI (debug only) |
| 5.2.6 | `LearningsRejected` (cosine-dedup hits) | `learning/db.go:181-297`, `dream/types.go:100` | counter in `Report`; increments `seen_count` on matched row | `cli-exec` via report; indirect `sql-poll` on `seen_count` | Poll-dashboard (indirect) |
| 5.2.7 | `ChunksSkipped` (L2 dedup) | `runner.go:213` | counter in `Report.ChunksSkipped` | `cli-exec` | Poll-dashboard |
| 5.2.8 | Filter-skip invisibility | `runner.go:107-114,131-137` | `--since` / `--session` / `--limit` all collapse into `SkippedFile` with no per-reason breakdown | **none** | Gap |

**Cross-cutting gap — text-only report.** The 8-metric run report is printed as one space-delimited line. Any UI that parses it is coupled to field order and separator.

## 5.3 Preflight hook signals

`orchestrator hooks preflight` + upstream state files.

| # | Signal | Source | Shape | Subscription | UI |
|---|---|---|---|---|---|
| 5.3.1 | Preflight brief (JSON) | `internal/preflight/brief.go` | `{"blocks":[{"name","title","body"}]}` | `cli-exec hooks preflight --format json` | Snapshot-view (clean contract) |
| 5.3.2 | Preflight brief (text) | same | markdown `## Operational Pre-flight` + `### <section>` | `cli-exec` | Snapshot-view |
| 5.3.3 | Dropped-sections warning | `internal/preflight/registry.go` | stderr: `preflight: dropped sections to fit capacity: %s` | `cli-exec` stderr | Snapshot-view alert |
| 5.3.4 | Mission checkpoint | `~/.alluka/workspaces/*/checkpoint.json` | JSON: `id, status, phase, last_event` | `file-watch` or JSON poll | **RT-dashboard (fsnotify)** |
| 5.3.5 | Mission event stream | `~/.alluka/events/{mission_id}.jsonl` | append-only JSONL | `jsonl-tail` | **RT-dashboard — best real-time source in the catalog** |
| 5.3.6 | Scheduler overdue/failed | `$ALLUKA_HOME/scheduler/scheduler.db` | rows `WHERE next_run_at < now OR status='failed'` | `sql-poll` | Poll-dashboard |
| 5.3.7 | Nen-daemon stats | `$ALLUKA_HOME/nen/nen-daemon.stats.json` | JSON: `{started_at, total_events, last_event_at, scanners{...}}` | `json-file` poll (stale after 10m) | Poll-dashboard |
| 5.3.8 | Tracker P0/P1 | `$ALLUKA_HOME/tracker.db` | rows `WHERE priority IN ('P0','P1') AND status='open'` | `sql-poll` | Poll-dashboard |
| 5.3.9 | Opt-out suppression | — | `NANIKA_NO_INJECT=1` → empty stdout | env check | Snapshot-view |

**PATTERN:** All five preflight sections also have independent underlying sources. A UI that needs low-latency updates should read the sources directly (5.3.4–5.3.8) and use `preflight --format json` only for one-shot "context brief" panels. Preflight is an assembler for Claude sessions, not a data plane.

## 5.4 Auto-memory system

Base: `~/.claude/projects/-Users-joeyhipolito-nanika/memory/` + persona memory + global memory.

| # | Signal | Source | Shape | Subscription | UI |
|---|---|---|---|---|---|
| 5.4.1 | Per-project memory dir | `~/.claude/projects/-Users-joeyhipolito-nanika/memory/*.md` | one `.md` per entry with YAML frontmatter `name/description/type` | `dir-watch` (fsnotify) | RT-dashboard; **Fragile** (no schema enforcement) |
| 5.4.2 | `MEMORY.md` index | same dir | flat markdown: `- [file.md](file.md) — hook` | `file-watch` | Snapshot-view |
| 5.4.3 | `MEMORY_NEW.md` scratchpad | same dir | frontmatter entries appended during session | `file-watch` (modify) | RT-dashboard ("session writing memory") |
| 5.4.4 | Canonical persona memory | `~/nanika/personas/<persona>/MEMORY.md` | flat log; 100-line ceiling via `enforceMemoryCeiling` | `file-watch` | Poll-dashboard |
| 5.4.5 | Global promoted memory | `~/.alluka/memory/global.md` | append-only deduplicated log | `file-watch` (append) | History-chart (growth), Poll-dashboard |
| 5.4.6 | `seedMemory` | `worker/memory.go:607` | writes MEMORY.md + MEMORY_NEW.md; no stdout trace | **in-proc only** (observable via file creation) | Off-limits |
| 5.4.7 | `mergeMemoryBack` | `worker/memory.go:865` | writes canonical persona MEMORY.md; no external log | **in-proc only** | Off-limits |
| 5.4.8 | `bridge-session` CLI | `cmd/hooks.go:67` | writes to `~/.alluka/memory/global.md`; dedup by content hash; stamps `bridged: DATE | by: bridge` | `cli-exec` at SessionStart | Snapshot-view |
| 5.4.9 | `Used` counter | `worker/memory.go` (canonical parse) | inline `Used: N` in persona MEMORY.md | `file-poll` + regex | **Fragile**; Poll-dashboard |
| 5.4.10 | Auto-promote trigger | `worker/memory.go` `autoPromoteHighUsed` | entries with `Used >= 3` → `global.md` | **in-proc only** (observable via 5.4.5) | Off-limits |

**Cross-cutting — schema is convention-only.** Frontmatter fields (`name`, `description`, `type`) are neither validated at write time nor enforced at read. Past GOTCHA: *"Frontmatter schemas (MEMORY.md, learnings.db, scout topics) are convention-only with zero schema enforcement, making automated processing fragile."* Every UI subscriber must guard.

**Cross-cutting — only `project` and `reference` bridge.** `user` and `feedback` memory types never reach `global.md`. A UI reading only global memory misses half the state.

## 5.5 Persistent worker (alpha) observability

Already fully covered in §1.5 and §1.8. Observable surface from this subsystem's perspective:

| # | Signal | Source | Shape | Subscription | UI |
|---|---|---|---|---|---|
| 5.5.1 | Alpha memory log | `~/.alluka/workers/alpha/memory.md` | append-only flat log with inline stamps | `file-watch` (append) | **RT-dashboard (worker observations feed)** |
| 5.5.2 | Alpha stats | `~/.alluka/workers/alpha/stats.json` | `{phases_completed, domains{}, total_cost, last_active, memory_entries}` | `json-file` poll | Poll-dashboard (worker gauges) |
| 5.5.3 | Alpha identity | `~/.alluka/workers/alpha/identity.md` | static markdown; rarely changes | `file-watch` | Snapshot-view |
| 5.5.4 | `CaptureWithFocus` emission | `learning/capture.go:140-223` | returns `[]Learning`; no external trace | **in-proc only** | Off-limits (observable via 5.1.1) |
| 5.5.5 | 30%/5-cap assignment | `engine/persistent_worker.go` | Go decision; no persisted record | **in-proc only** | Off-limits (observable via 5.5.2 `phases_completed` delta) |

## 5.6 Learning/Memory/Dream gap summary

| Gap | Impact | Mitigation |
|---|---|---|
| No `updated_at` column on `learnings` | Counter and decay mutations invisible to timestamp subscribers | Add column + update trigger, or emit a Go event bus message on `Insert`/`Update` |
| Dream run report is plain-text one-liner | Parse-fragile; version drift breaks UI | Add `--format json` to `dream run` |
| No per-reason breakdown in `SkippedFile` | Can't explain why `Discovered − ProcessedFiles` gap exists | Split into `SkippedUnchanged` / `SkippedAge` / `SkippedSession` / `SkippedWorker` |
| No verbose trace for `--since`/`--session`/`--limit` skips | Debugging requires re-run with different flags | Add gated verbose lines |
| Frontmatter convention-only | Malformed entries break every parser downstream | Validate at write time; surface errors through a signal |
| No event bus for memory or learnings | Every subscriber must fsnotify/poll | Introduce `~/.alluka/events/learning.jsonl` / `memory.jsonl` append-only streams |
| `mergeMemoryBack` and `autoPromoteHighUsed` leave no log | UI can't show "N memories promoted today" | Emit promotion events to the learning event stream above |
| In-proc callbacks (5.1.6, 5.1.9, 5.4.6, 5.4.7, 5.4.10, 5.5.4, 5.5.5) | Invisible to out-of-process UI | Event bus or accept "observable only by effect" |

## 5.7 Learning/Memory/Dream — recommended UI coupling (ordered by signal quality)

1. **Live feed** → `jsonl-tail` on `events/{mission_id}.jsonl` (5.3.5) for mission progress; `file-watch` on `alpha/memory.md` (5.5.1) for worker observations; `file-watch` on `MEMORY_NEW.md` (5.4.3) for in-session writes
2. **Gauges** → `json-file` poll on `nen-daemon.stats.json` (5.3.7) and `alpha/stats.json` (5.5.2) at 10s cadence
3. **Counters** → `sql-poll` on `learnings` row count (5.1.1), `processed_transcripts/_chunks` (5.2.1, 5.2.2) at 30s cadence. Compute delta between polls for rate metrics
4. **Ad-hoc drill-downs** → `cli-exec hooks preflight --format json` (5.3.1) for one-shot context panels; direct SQL on `learnings.db` for power-user queries
5. **Notifications** → `dir-watch` on `~/.claude/projects/.../memory/` (5.4.1) for "new memory saved" toasts

**Avoid coupling UI to:** plain-text dream report (5.2.3), verbose trace lines (5.2.5), frontmatter fields without validation guards (5.4.1), `Used:` inline counter (5.4.9), any in-process callback.

**DECISION:** A learning/memory dashboard should NOT embed the preflight CLI as its data plane. Preflight is an assembler that collapses five independent sources into one markdown brief.

**DECISION:** Schema validation for auto-memory frontmatter is the single highest-leverage investment — every downstream consumer currently pays a parse-fragility tax.

**DECISION:** Introduce lightweight append-only event streams (`events/learning.jsonl`, `events/memory.jsonl`) to convert in-process callbacks into subscribable signals. The minimum viable surface for a real-time dashboard.

---

# 6. Cross-domain notes

## 6.1 Shared stores — one file, multiple owners

| Store | Owners |
|---|---|
| `~/.alluka/metrics.db` | orchestrator engine (write), `orchestrator metrics`, `orchestrator status`, skills/persona UI queries, `nen-mcp` (`nanika_mission`) |
| `~/.alluka/learnings.db` | `learning.OpenDB`, `routing.OpenDB`, `claims.OpenDB` — three additive schemas in one file. Readers: `orchestrator metrics routing`, `orchestrator hooks preflight/inject-context`, `dream run/status`, `nen-mcp` (`nanika_learnings`) |
| `~/.alluka/audits.jsonl` | orchestrator audit writer, `audit scorecard` reader, persona quality UI, `ko apply` (applies recommendations) |
| `~/.alluka/events/<id>.jsonl` | `event.FileEmitter`, `orchestrator status`, `hooks preflight` (last_event), nen-daemon JSONL bridge, `nen-mcp` (`nanika_events`) |
| `~/.alluka/nen/findings.db` | scanners write, `nen-mcp` (`nanika_findings`) reads |
| `~/.alluka/scheduler/scheduler.db` | scheduler daemon writes, `hooks preflight` (scheduler) reads, `nen-mcp` (`nanika_scheduler_jobs`) reads |
| `~/.alluka/tracker.db` | tracker CLI writes, `hooks preflight` (tracker) reads, `nen-mcp` (`nanika_tracker_issues`) reads |

**Pattern:** When surfacing SQLite as a UI signal, open a read-only connection (`PRAGMA query_only=ON`), single connection per provider, floor poll cadence at 2s so WAL checkpointing isn't starved.

## 6.2 Transport pattern map

| Transport | Used by |
|---|---|
| **push-stream (UDS + SSE)** | nen event bridge (orchestrator bus → `~/.alluka/events.sock` + `http://127.0.0.1:7331/api/events`); orchestrator `UDSEmitter` direct |
| **jsonl-tail** | `~/.alluka/events/<id>.jsonl` (mission events), `~/.alluka/events/scheduler.jsonl` (scheduler runs), `~/.alluka/audits.jsonl` (mission audits), `~/.alluka/metrics.jsonl` (mission metrics) |
| **file-watch (fsnotify)** | Persona catalog (`persona/personas.go:193`), `checkpoint.json` (preflight mission section), auto-memory dir, `alpha/memory.md`, `MEMORY_NEW.md` |
| **json-file poll** | `nen-daemon.stats.json`, `alpha/stats.json`, `shu-findings.json` |
| **sql-poll** | Every SQLite surface — `metrics.db`, `learnings.db`, `findings.db`, `scheduler.db`, `tracker.db`, `proposals.db`, `ko-history.db` |
| **cli-exec** | `shu query status`, `scheduler query status`, `tracker query status`, `orchestrator hooks preflight/inject-context`, `dream status`, `nen-mcp doctor`, `gyo evaluate`, `ryu report`, `ko apply --dry-run` |
| **mcp** | 8 `nen-mcp` tools (`nanika_findings`, `_proposals`, `_ko_verdicts`, `_scheduler_jobs`, `_tracker_issues`, `_mission`, `_events`, `_learnings`) |

## 6.3 Top-risk gaps across domains

1. **No `updated_at` on `learnings` table** — every counter mutation invisible to timestamp subscribers (§5.1)
2. **Dream run report is plain-text one-liner** — no `--format json` (§5.2)
3. **Auto-memory frontmatter is convention-only** — parse-fragility tax on every downstream consumer (§5.4)
4. **No event bus for memory or learnings mutations** — fsnotify + poll is the only path (§5.6)
5. **`findings.db` has no change stream** — upserts don't emit events, poll-only (§2.8)
6. **`subscriberDrops` counter is private** — reconnecting subscribers silently miss events; expose via `nen-daemon.stats.json` (§2.8)
7. **`security.injection_detected` topic has no publisher** — zetsu is a pure library (§1.6, §2.8)
8. **`ko apply` has no "applied" marker** — UI "pending apply queue" blocked until `dispatches` gets a column or sidecar file (§2.8)
9. **Tracker has no JSON `show` surface** — drill-down UIs blocked (§4.7 D-4)
10. **Tracker has no audit trail parallel to `scheduler.job_audit`** — cross-system "who did this" attribution is asymmetric (§4.6 D.2)
11. **No retention on `scheduler.runs`, `job_audit`, `scheduler.jsonl`, `tracker.comments`, `tracker.links`, `audits.jsonl`, per-mission `events/<id>.jsonl`** — every surfacing path must paginate or rolling-window (§4.6 D.7)
12. **Skill index has no watcher** — persona catalog hot-reloads, skill index does not. Asymmetry surfaced in UI, not hidden (§3.3)
13. **`CLAUDE_CODE_SESSION_ID` propagation for `job_audit.actor`** — set by harness, not orchestrator; detached subprocess may not propagate (§4.6 D.2)

## 6.4 Top-value signals (for a Claude Console-style UI)

Ranked by signal quality and effort-to-surface:

1. **Persona catalog fsnotify** (§3.3) — already live, 50ms update, sub-process only. Add a broadcast channel for the headline "edit file, see tile change" experience
2. **Findings table** (`findings.db`, §2.3) — dedup by semantic key, so a single refresh query returns truth. Filter-friendly facets built in
3. **Mission event JSONL tail** (`events/<id>.jsonl`, §5.3.5 / §1.2) — the best real-time source in the system
4. **Audit scorecard JSON** (§1.7 `audit scorecard --format json` / `audits.jsonl`) — primary quality signal for personas, includes persona_correct and recommendations
5. **Skill invocations table** (`skill_invocations`, §1.3) — the only honest skill last-used timestamp
6. **Routing decisions table** (`routing_decisions`, §1.4) — "why this persona" + failure drill-down
7. **Alpha memory log tail** (`workers/alpha/memory.md`, §1.5) — append-only worker observations feed
8. **Preflight JSON** (`orchestrator hooks preflight --format json`, §1.7) — one-shot "operational brief" for a panel
9. **`shu query status --json`** (§2.4) — hero card single-number system score
10. **Scheduler event JSONL** (`events/scheduler.jsonl`, §4.5) — live scheduler feed without the UDS competition

---

# 7. Source file index

## Orchestrator
- `internal/core/{workspace,checkpoint,signal,types,runtime}.go`
- `internal/engine/{engine,metrics,scratch,persistent_worker}.go`
- `internal/event/{types,bus,file,uds,noop,livestate,project,multi,sanitize,emitter}.go`
- `internal/worker/{spawn,execute,identity,memory,claudemd,skillindex}.go`
- `internal/persona/personas.go`
- `internal/metrics/db.go`
- `internal/learning/{db,embed,capture,context,types}.go`
- `internal/routing/db.go`
- `internal/claims/db.go`
- `internal/audit/{store,types,scorecard}.go`
- `internal/dream/{store,runner,types}.go`
- `internal/preflight/{brief,registry,mission,scheduler,nen,tracker,learnings}.go`
- `internal/cmd/{root,status,metrics,audit,dream,hooks,cleanup,run}.go`
- `internal/sdk/types.go`

## Nen
- `plugins/nen/plugin.json`
- `plugins/nen/cmd/nen-daemon/main.go`
- `plugins/nen/cmd/{gyo,en,ryu,shu,ko,review-scanner}/main.go`
- `plugins/nen/cmd/shu/{propose,close}.go`
- `plugins/nen/cmd/ko/eval_emitter.go`
- `plugins/nen/ko/{eval,judge,db,quality,cache}.go`
- `plugins/nen/internal/scan/{db,types}.go`
- `plugins/nen/internal/audit/{apply,types}.go`
- `plugins/nen/zetsu/zetsu.go`
- `plugins/nen/peak/peak.go`
- `plugins/nen_mcp/cmd/nen-mcp/{main,tools,doctor}.go`

## Scheduler (Go)
- `plugins/scheduler/internal/db/{db,audit,migrations}.go`
- `plugins/scheduler/internal/cron/cron.go`
- `plugins/scheduler/internal/cmd/{daemon_cmd,status,jobs,logs,history,run,query}.go`
- `plugins/scheduler/internal/cmd/executor/executor.go`
- `plugins/scheduler/skills/SKILL.md`

## Tracker (Rust)
- `plugins/tracker/migrations/{001_initial,002_parent_id,003_seq_id}.sql`
- `plugins/tracker/src/{db,models,id,commands,query,main,import_linear}.rs`

## Scripts
- `scripts/generate-agents-md.sh`
- `scripts/nanika-context.sh`

---

*Hand-synthesized 2026-04-11 from the five observability research missions. When any claim here conflicts with the code, trust the code.*
