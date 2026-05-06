---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260428-1e72be48
created_at: "2026-04-28T03:05:00Z"
confidence: high
depends_on:
  - rust-commands
  - hooks-and-scenes
token_estimate: 3400
---

# REVIEW-M6 — Whim Tauri M6 (terminal + mission-run IPC)

## Summary

M6 wires the dust terminal drawer and the mission-run canvas to real orchestrator state on disk. Phase 1 (Rust) lands a self-contained `mission` module with six async Tauri commands and a `notify`-based watcher. The blocking work is correctly delegated: `list_missions`/`get_mission`/`get_phase` wrap their disk reads in `tokio::task::spawn_blocking` (`mission.rs:223`, `:251`, `:285`), and the long-lived watcher loop runs on a `std::thread::spawn` rather than the async runtime (`mission.rs:426`). Shell-outs to the `orchestrator` CLI use `tokio::process::Command` (`mission.rs:355`, `:371`). The pty quartet is correctly absent — `grep "fn terminal_(open|write|resize|close)"` returns 0 (verified). Two event channels (`whim://mission-event`, `whim://mission-run-output`) emit from the watcher (`mission.rs:466`, `:481`), atomic-replace of the watcher handle is wired through `AppState.mission_watcher` (`lib.rs:1204`, `mission.rs:422`), and `cargo check` is clean.

Phase 2 (TS) ships the surface the spec asked for — `MissionSummary`/`PhaseSummary`/`MissionDetail`/`PhaseDetail`/`GateDecision` types byte-match the Rust serde shapes (`types.ts:96-144`), `TerminalDrawer` is now props-driven with mock defaults so all 51 prior scenes still render unchanged (`TerminalDrawer.tsx:11-14, 41-42`), `LiveTerminalDrawer` and `LiveMissionRunCanvas` are wired correctly (`LiveTerminalDrawer.tsx:18-46`, `LiveMissionRunCanvas.tsx:21-95`), 53 entries land in both `SCENE_COMPONENTS` and `SCENARIO_LIST` (`App.tsx:60-114, 116-170`), build is clean at 84.64 KB gzipped, and both lints pass (`lint:scenes: ok (53 scenes registered)`, `lint:tour: ok (26 anchors referenced, all resolve)`).

But two of the listed blocking criteria are not satisfied:

1. **Criterion 14 (async-listen cleanup race) regresses** — both `useMissions` and `useTerminalLog` use the exact `let unlisten=null; listen(...).then(f=>{unlisten=f}); return ()=>{unlisten?.()}` pattern that the M4/M5 reviews flagged as the carry-over race. The required `cancelled` flag (mission constraint line 94) is missing from the listen-then chain in both hooks, so a fast unmount before `listen()` resolves leaks the listener.
2. **Criterion 6 (`useMissions` prescribed shape)** — four of the spec's listed return fields (`activePhaseId`, `setActivePhaseId`, `activePhase`, `runOutput`) are absent from `useMissions`. `runOutput` was relocated wholesale into `useTerminalLog`, but `activePhaseId`/`setActivePhaseId`/`activePhase` are simply not implemented — there is no path to fetch `get_phase` or to render `PhaseDetail` in any scene shipped with M6.

Both are mechanical fixes; neither requires re-architecture. Verdict: **NEEDS-CHANGES** before merge — the listener leak is the same race we've been chasing across three milestones, and the missing phase-detail surface drops a wired contract from the IPC sketch.

### Blockers

- **`plugins/dust/src/whim/hooks/useMissions.ts:97-114` (async-listen cleanup race not fixed)** Mission constraint line 94 explicitly calls this out as a "carry-over from M4/M5 reviews" and requires a `cancelled` flag in the cleanup. The implementation uses the exact pattern the prior reviews rejected:
  ```ts
  useEffect(() => {
    let unlisten: (() => void) | null = null
    listen<MissionEventPayload>('whim://mission-event', ...).then(f => { unlisten = f })
    return () => { unlisten?.() }
  }, [])
  ```
  Race: if the component unmounts before `listen(...)` resolves, the cleanup runs with `unlisten === null`, then the resolved `f` is captured into the closure that nobody calls — the listener leaks for the rest of the process lifetime. Multiplies on every re-mount (HMR, scene-switch, route change). Criterion 14 fails.
  Why: Tauri's `listen()` returns a Promise; the un-listen handle isn't available synchronously. The cancelled-flag pattern is the canonical fix and is already used elsewhere in the codebase (e.g. `useMissions.ts:44-66` `refresh` uses `cancelled` correctly).
  Fix: gate the `then` on the cancelled flag, and call `f()` immediately if the effect already cancelled before the listener registered:
  ```ts
  useEffect(() => {
    let cancelled = false
    let unlisten: (() => void) | null = null
    listen<MissionEventPayload>('whim://mission-event', ({ payload }) => {
      if (cancelled) return                    // also short-circuit any in-flight callback
      if (payload.mission_id !== activeIdRef.current) return
      // ...same body as today
    }).then(f => {
      if (cancelled) f()                       // unmount beat us — release immediately
      else unlisten = f
    })
    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])
  ```

- **`plugins/dust/src/whim/hooks/useTerminalLog.ts:29-50` (same async-listen cleanup race)** Identical pattern, identical fix. The `listen<MissionRunOutputPayload>('whim://mission-run-output', ...)` chain has no `cancelled` flag; if the consumer (e.g. `LiveTerminalDrawer`) unmounts mid-Promise — easy to trigger because the consumer subscribes the moment `activeMissionId` flips from null to non-null and again on every scene switch — the listener leaks. The leak is even higher-volume here: each leaked listener still pushes lines into a dead `setLines` ref, so React will log warnings until the user reloads the window.
  Fix: same shape as the previous blocker.
  ```ts
  useEffect(() => {
    let cancelled = false
    let unlisten: (() => void) | null = null
    listen<MissionRunOutputPayload>('whim://mission-run-output', ({ payload }) => {
      if (cancelled) return
      // ...same filter + setLines body
    }).then(f => {
      if (cancelled) f()
      else unlisten = f
    })
    return () => {
      cancelled = true
      unlisten?.()
    }
  }, [])
  ```

- **`plugins/dust/src/whim/hooks/useMissions.ts:11-22, 144-156` (prescribed shape: `activePhaseId`/`setActivePhaseId`/`activePhase` missing)** Criterion 6 requires `useMissions` to return the shape spelled out at mission lines 63-75. Four fields are missing from `UseMissionsReturn` and from the returned object: `activePhaseId: string | null`, `setActivePhaseId(p: string | null)`, `activePhase: PhaseDetail | null`, and `runOutput: string[]`. There is no code path in M6 that calls `get_phase` from React — the `get_phase` Tauri command (`mission.rs:284-343`) is registered, dead-codes a tail of the worker log, and is never invoked from any hook or scene. Any consumer that wants per-phase log tail or output-files (e.g. an upcoming review-gate scene at M7) has nothing to call.
  Why: the IPC contract sketch §"mission run" (mission-doc line 20) treats per-phase detail as a first-class surface; pushing the run-log tail and the phase-id setter into the hook is the integration point that the live `MissionProgressScene`/`ReviewGateScene` will eventually wire into. Skipping it now means M7 has to retro-fit the hook signature, which will churn every consumer.
  Fix: in `useMissions`:
  1. Add `const [activePhaseId, setActivePhaseIdState] = useState<string | null>(null)`; add an `activePhaseIdRef`.
  2. Add a `useEffect([activeMissionId, activePhaseId])` that calls `invoke<PhaseDetail>('get_phase', { missionId, phaseId })`, mirrors the existing detail-fetch atomic-replace pattern (with the cancelled-flag fix from the first blocker).
  3. Re-fetch `activePhase` on every `whim://mission-event` for the matching mission_id (atomic replace).
  4. For `runOutput`, the cleanest move is to call `useTerminalLog(activeMissionId, activePhaseId ?? undefined)` inside `useMissions` and re-export `lines` as `runOutput` — keeps both hooks single-purpose and matches the spec's combined surface.
  5. Add `setActivePhaseId` to the returned object (writes both `activePhaseIdRef.current` and the state, mirroring `setActiveMissionId` at lines 118-121).

  This unblocks criterion 6 verbatim and lets `LiveMissionRunCanvas` render per-phase data instead of just the phase-list summary.

### Warnings

- **`plugins/dust/src/whim/hooks/useMissions.ts:125-142` (action signatures deviate from spec)** Mission line 72-74 prescribes `approveGate(missionId, gateId, decision)`, `cancelMission(missionId)`, `rerunPhase(missionId, phaseId)` — three-arg / one-arg / two-arg signatures that pass `missionId` explicitly. The hook implements them as `approveGate(gateId, decision)` / `cancelMission()` / `rerunPhase(phaseId)`, sourcing `missionId` from `activeIdRef.current` and throwing `'no active mission'` if null. Functionally equivalent for the only consumers shipped today (which always operate on the active mission), but a future multi-mission control surface that wants to approve a gate on a *non-active* mission cannot do so without a hook change. Either accept the deviation as a deliberate API choice (then update the spec) or restore the prescribed signatures with `missionId ?? activeIdRef.current` as a default.

- **`plugins/dust/src/whim/components/LiveTerminalDrawer.tsx:10-16` (heuristic line classifier may misclassify markdown)** `is_log_file` in Rust (`mission.rs:166-169`) treats both `*.log` and `output.md` as taillable; markdown bullets like `- foo` are passed verbatim to `classify` (`LiveTerminalDrawer.tsx:10-16`), which falls through to `kind: 'stdout'`. That's harmless for terminal rendering, but the `^!` rule will mis-tag any markdown line beginning with `!` (image embed, attention-block) as `stderr` and color it red. Low blast radius today (worker output.md is rarely emitted line-by-line during a live run), but worth narrowing — either filter out `output.md` from the watcher's emit set, or scope the classifier to `.log` files only by passing the file path through to the classifier.

- **`plugins/dust/src/whim/hooks/useMissions.ts:75-93` (start_mission_run_watcher fire-and-forget)** When `activeMissionId` flips, the effect kicks off `invoke('start_mission_run_watcher', { missionId: activeMissionId })` and only logs to `console.error` on failure (line 86-90). That's reasonable for a non-fatal IPC failure, but it papers over a real failure mode: if `start_mission_run_watcher` returns `Err("workspace … not found")` (`mission.rs:407-409`) — easy to hit because `list_missions` filters for missions with workspaces but a stale list can survive a workspace deletion — the user sees a polished UI that silently never updates. Low priority, but worth surfacing through the existing `error` channel so the UI can at least show "watcher unavailable, refresh manually". Trivial change: `setError(String(err))` alongside the console log.

- **`plugins/dust/src-tauri/src/mission.rs:317-329` (worker_log_tail collects sequentially across files, not last-100-globally)** The 100-line cap is enforced across files, but the loop reads each file in `read_dir` order and appends its tail until the cap. If the worker dir contains `a.log` (300 lines, written first) and `b.log` (50 lines, written second), the user sees the last 100 lines of `a.log` and zero lines of `b.log` even though `b.log` is newer. Mission spec line 46 says "the last 100 lines of any `*.log` or `output.md`" — interpretable either way, but the typical reader expectation for "tail" is "newest content across all sources". Fix (deferrable): sort `log_paths` by `mtime` descending before tailing, or interleave by parsing timestamp prefixes if present. Today's behavior is "first-discovered files win", which is fragile.

- **`plugins/dust/src-tauri/src/mission.rs:411-422` (watcher init runs on async runtime)** `notify::recommended_watcher` and `watcher.watch(...)` perform synchronous setup (file-descriptor opens, kqueue/inotify registration) on the async runtime before the `std::thread::spawn` at line 426 takes over. Setup is fast (sub-ms) so it's not a real-world stall, but for parity with the M4/M5 pattern of "everything synchronous happens in `spawn_blocking`", consider wrapping the init in `tokio::task::spawn_blocking` and `await`-ing the join. Not worth blocking on — the watcher is invoked once per mission switch, not in a hot loop.

- **`plugins/dust/src-tauri/src/mission.rs:472-491` (per-file offset map grows unbounded)** The debounce thread keeps a `HashMap<PathBuf, u64>` of read offsets keyed by the watched file's absolute path. Every new log file in the workspace ever observed during the watcher's lifetime stays in the map until the watcher is replaced. Workspaces with many short-lived worker phases can grow this map indefinitely (one entry per phase × per `*.log` per `output.md`). Bounded by the workspace's total log-file count — practically a few hundred entries — so the leak is small, but if a long-running session triggers it the map will sit on a few KB of `PathBuf`s forever. Fix (deferrable): prune entries older than some TTL or whose path no longer exists during the debounce sweep.

- **`plugins/dust/src/whim/components/LiveMissionRunCanvas.tsx:64-69` (`expectedMs: 60_000` hard-coded)** Every phase renders with a 60s expected duration in the live canvas. The mock `MissionProgressScene` paints expected durations from a richer fixture — when real phases stretch past 60s the live progress bar will redline immediately and stay red. Acceptable for M6 (the field has no source from `checkpoint.json` today) but worth flagging in scratch for whoever wires `phase_persona_runtimes` upstream.

- **`plugins/dust/src/whim/hooks/useMissions.ts:97-114` (race against rapid scene switches)** Even after the cancelled-flag fix above, when the user mounts `LiveMissionRunCanvas`, then `LiveTerminalDrawer` (each calling `useMissions()` independently because hooks aren't context-shared), each instance registers its own `whim://mission-event` listener and each will fire `invoke('list_missions')` on every checkpoint change. That's quadratic in the number of mounted live components. Not blocking for M6 (only two consumers exist) but worth lifting `useMissions` into a shared context provider when M7 adds the rail/top-bar consumers.

### Suggestions

- **`plugins/dust/src/whim/types.ts:135-138` (`MissionEventPayload.checkpoint: unknown`)** The Rust side emits the entire parsed `checkpoint.json` value (`mission.rs:464-468`). Typing it as `unknown` keeps the wire flexible but loses every downstream type-check. Define `CheckpointPayload` in `types.ts` mirroring the `CheckpointFile`/`CheckpointPayload`/`CheckpointPlan` Rust shapes (`mission.rs:74-94`) and use it in `MissionEventPayload` so consumers can index `payload.checkpoint.payload.plan?.phases` with autocomplete.

- **`plugins/dust/src-tauri/src/mission.rs:113-130` (`epoch_to_rfc3339` duplicated from `lib.rs`)** The doc comment on line 113 calls out the duplication explicitly. Either move the helper to a shared `time.rs` module or pull it from a crate like `time` or `chrono` that's likely already in the dep tree. Cleanup, not a bug.

- **`plugins/dust/src-tauri/src/mission.rs:283-343` (`get_phase` is implemented but unreachable)** Worth either wiring it through `useMissions` (per the criterion-6 blocker fix) or marking it with `#[allow(dead_code)]` and a TODO so reviewers don't keep flagging it.

### What's Good

- Atomic-replace pattern preserved in `useMissions.ts:104-106` — every checkpoint event triggers a full `get_mission` re-fetch and replaces `activeMission` rather than mutating it. Same pattern used for the mission summary list (line 107-109), so summary counts (`phases_done`, `phases_failed`) stay current without an in-place edit. The `activeIdRef` mirror (line 40-41, 101) is exactly the right way to read the current filter from a one-shot listener without re-subscribing.

- The `TerminalDrawer` prop refactor (`TerminalDrawer.tsx:11-14, 41-42`) is the textbook backwards-compatible move — defaulting `lines` and `prompt` to the existing `MOCK_TERMINAL_LINES`/`MOCK_TERMINAL_PROMPT` exports means every existing caller (`Scenes.tsx:3118`, `CompositeCanvas.tsx:857`) renders bit-identical output without touching a single call site. `lint:scenes: ok (53 scenes registered)` and the absence of TS errors in `npm run build` are the proof.

- Phase 1's choice to keep the `notify` watcher's debounce loop in a `std::thread::spawn` (`mission.rs:426-495`) — and to receive `notify::Event`s through a `std::sync::mpsc::channel` (line 411) — is exactly right for a long-lived synchronous loop. The `recv_timeout(POLL)` at line 435 with the `Disconnected` arm at line 447 is the cleanest possible exit path: drop the watcher (line 422 atomic-replace), the channel disconnects, the thread exits. No `Arc<AtomicBool>` shutdown flag needed.

- `LiveMissionRunCanvas.tsx:28-32` correctly gates the 1Hz interval on `isRunning` so the elapsed counter doesn't burn cycles when the mission is `done`/`failed`/`pending`. Cleanup (`return () => clearInterval(id)`) is right.

- Status mapping at `LiveMissionRunCanvas.tsx:9-14` covers all four Rust-emitted statuses (`completed`/`running`/`in_progress`/`failed`) plus the `pending` fallthrough, and `pickActivePhase` (line 16-19) returns `null` when no phase is running rather than picking a stale done phase — important for the "all phases done" terminal state.

- Six commands registered (`lib.rs:1225-1231`), watcher handle stored in `AppState.mission_watcher` (`lib.rs:1204`), and the mutex+atomic-replace contract (`mission.rs:422`) is documented inline. `cargo check` clean. `grep "fn terminal_(open|write|resize|close)" plugins/dust/src-tauri/src` returns zero — the pty quartet is correctly absent per risk #2.

## Acceptance criteria — verification matrix

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| 1 | ≥3 phase commits land | **PENDING** | `git log --oneline` shows `8faa7186` (hooks-and-scenes) + `2994c26d` (rust-commands); review commit pending — will land as commit #3 when this artifact is staged. |
| 2 | Build clean; bundle ≤350 KB | **PASS** | `npm run build` → `App-hnPLho9j.js 328.63 kB │ gzip: 84.64 kB` · 0 errors · 0 warnings. |
| 3 | `cargo check` clean | **PASS** | `cd plugins/dust/src-tauri && cargo check` → `Finished dev profile [unoptimized + debuginfo] target(s) in 0.60s`. No blocking I/O on async runtime — `list_missions`/`get_mission`/`get_phase` use `tokio::task::spawn_blocking` (`mission.rs:223, 251, 285`); long-lived watcher loop runs on `std::thread::spawn` (`mission.rs:426`); shell-outs use `tokio::process::Command` (`mission.rs:355, 371`). |
| 4 | Six commands registered | **PASS** | `mission.rs` declares `list_missions:223`, `get_mission:250`, `get_phase:284`, `mission_approve_gate:346`, `mission_cancel:370`, `phase_rerun:386`; all six wired into `tauri::generate_handler!` at `lib.rs:1225-1231` (plus `start_mission_run_watcher:401` at `lib.rs:1231`). |
| 5 | Two new event channels emit | **PASS** | `whim://mission-event` at `mission.rs:466`; `whim://mission-run-output` at `mission.rs:481`. Both emit from the watcher thread spawned in `start_mission_run_watcher`. |
| 6 | `useMissions` and `useTerminalLog` exist with prescribed shapes | **FAIL** | `useTerminalLog.ts:1-53` shape matches spec. `useMissions.ts:11-22` is missing four prescribed fields: `activePhaseId`, `setActivePhaseId`, `activePhase`, `runOutput` (mission spec line 68-71). See blocker above. |
| 7 | Mission/phase types match Rust serde shape | **PASS** | `types.ts:96-144` declares `MissionSummary` (line 96), `PhaseSummary` (107), `MissionDetail` (116, `extends MissionSummary` to flatten the Rust `#[serde(flatten)]` from `mission.rs:43`), `PhaseDetail` (122), `GateDecision = 'Approve' \| 'Reject'` (133, matches Rust `#[serde(rename_all = "PascalCase")]` enum at `mission.rs:64-69`), `MissionEventPayload` (135), `MissionRunOutputPayload` (140). Field names byte-match the Rust shapes. |
| 8 | `LiveTerminalDrawer` and `LiveMissionRunCanvas` render with hooks wired into props | **PASS** | `LiveTerminalDrawer.tsx:18-46` calls `useMissions()` (line 19) + `useTerminalLog(activeMissionId)` (line 20), passes the result into `<TerminalDrawer lines={terminalLines} />` (line 46). `LiveMissionRunCanvas.tsx:21-95` calls `useMissions()` (line 22), builds `liveState.missionRun` from `activeMission.phases` (line 64-69), passes into `<CompositeCanvas state={liveState} />` (line 95). |
| 9 | `SCENARIO_LIST.length` = 53 | **PASS** | `lint:scenes: ok (53 scenes registered)`. Manual count: `App.tsx:117-169` = 53 entries; `App.tsx:61-113` = 53 keys in `SCENE_COMPONENTS`. |
| 10 | 51 prior scenes still render; tour + demo unaffected | **PASS** | `TerminalDrawer` refactor uses default props (`TerminalDrawer.tsx:11-14, 41-42`) so existing call sites at `Scenes.tsx:3118` and `CompositeCanvas.tsx:857` (`<TerminalDrawer defaultOpen={true} />`) get `MOCK_TERMINAL_LINES`/`MOCK_TERMINAL_PROMPT` automatically. `CompositeCanvas` was not refactored — already props-driven via `CompositeCanvasState`. `lint:tour: ok (26 anchors referenced, all resolve)`. Build clean. |
| 11 | Lints pass | **PASS** | `npm run lint:scenes` → `lint:scenes: ok (53 scenes registered)`. `npm run lint:tour` → `lint:tour: ok (26 anchors referenced, all resolve)`. |
| 12 | Mocks not deleted | **PASS** | `mocks/events.ts`, `mocks/terminal.ts`, `mocks/canvasStates.ts` all present. `MOCK_TERMINAL_LINES` (`mocks/terminal.ts:17`) + `MOCK_TERMINAL_PROMPT` (`:30`); `stateMissionRunning` (`mocks/canvasStates.ts:51`) + `stateReviewGate` (`:68`). |
| 13 | Legacy launcher build clean | **PASS** | `npm run build` produces `dist/assets/index-Cs8znuDL.js 143.74 kB │ gzip: 46.27 kB` (the legacy launcher entry; whim app code is in the lazy-loaded `App-hnPLho9j.js` chunk). 0 errors, 0 warnings. |
| 14 | Async-listen cleanup race fixed | **FAIL** | `useMissions.ts:97-114` and `useTerminalLog.ts:29-50` both use the `let unlisten=null; listen(...).then(f=>{unlisten=f}); return ()=>{unlisten?.()}` pattern with no `cancelled` flag. Same race as carry-over from M4/M5 reviews. See blockers. |
| 15 | No pty quartet shipped | **PASS** | `grep -E "fn (terminal_open\|terminal_write\|terminal_resize\|terminal_close)" plugins/dust/src-tauri/src` returns 0 matches. Log-tail only per risk #2. |
| 16 | `REVIEW-M6.md` exists with `### Blockers` and `### Warnings` H3 | **PASS** | This file. |

<!-- scratch -->
M6 review notes for the fix-phase implementer:

1. The two listen-then race fixes are mechanical — apply the cancelled-flag pattern verbatim from the blocker text. Unit tests aren't required (Tauri's `listen` is mocked in test setup); just verify by mounting → unmounting fast and confirming no listener-leak warnings in the Tauri console.

2. For the `useMissions` shape blocker, easiest path is:
   - Add `activePhaseId`/`setActivePhaseId`/`activePhase` state + ref + effect (mirror the `activeMissionId` triple).
   - Compose `useTerminalLog(activeMissionId, activePhaseId ?? undefined)` inside `useMissions` and re-export `lines` as `runOutput`. Keeps `useTerminalLog` independently consumable for `LiveTerminalDrawer` (passes only `activeMissionId`, no phase filter).
   - The `get_phase` Tauri command (`mission.rs:284-343`) is already implemented — just call it.

3. Action signatures (warning, not blocker): if you keep the no-mission-id form, document on the hook that it's deliberately scoped to the active mission. If you switch to the spec form, default to `activeIdRef.current` so today's call sites at `LiveMissionRunCanvas` (none yet) and any future review-gate scene don't have to thread missionId through.

4. The clippy errors flagged by `cargo clippy --all-targets -- -D warnings` are **all pre-existing in `lib.rs`** (lines 794, 1160, 171) and not caused by M6. Don't address them in M6's fix commit — file a follow-up ticket if desired. Default `cargo check` is clean.

5. `is_log_file` includes `output.md` (`mission.rs:166-169`) — fine for now but watch markdown formatting if a worker phase emits a heavy markdown report mid-run; the heuristic classifier in `LiveTerminalDrawer.tsx:10-16` will mis-tag bullet/heading lines.

6. The watcher offset map `HashMap<PathBuf, u64>` in `mission.rs:432` grows unbounded for the watcher's lifetime. Trivial today (workspace has bounded log files); revisit if M7 adds long-running watcher use.

7. M7 will likely lift `useMissions` into a context provider — multiple live components each calling `useMissions()` register independent `whim://mission-event` listeners and each fires its own `list_missions` invoke per checkpoint. Today's two consumers (`LiveTerminalDrawer`, `LiveMissionRunCanvas`) keep this manageable.
<!-- /scratch -->
