---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260428-3f6db065
created_at: "2026-04-28T01:55:00Z"
confidence: high
depends_on:
  - rust-commands
  - hook-and-scene
token_estimate: 3200
---

# REVIEW-M5 — Whim Tauri M5 (diff surface IPC)

## Summary

M5 wires the dust diff surface to real git IPC. Phase 1 (Rust) lands cleanly: three async Tauri commands (`list_changed_files`, `get_file_diff`, `reject_hunk`) shell out to `tokio::process::Command` so no blocking ops run on the async runtime, the parser handles `git status --porcelain=v1 -z` rename records correctly, and all three are registered in `tauri::generate_handler!`. `cargo check` is clean.

Phase 2 (TS) **deviates substantially from the mission spec** in five ways that map directly onto blocking acceptance criteria. The implementer built `plugins/dust/src/whim/hooks/useDiffs.ts` (plural) and `components/LiveDiffPanel.tsx` instead of the prescribed `hooks/useDiff.ts` (singular), `components/LiveTurnDiffInspector.tsx`, and `whim/protocol.ts`; types were left in `mocks/diffs.ts` rather than being mirrored into `whim/types.ts`; and most critically the accept code path **does not actually dispatch the action** — `LiveDiffPanel.handleAccept` (line 202‑208) calls `void Promise.resolve(ACCEPT_HUNK_OP)`, which is a literal no‑op that captures the constant but never invokes IPC. The `dispatch_action(chat, code_diff.accept_hunk, …)` call required by the mission spec (line 59) is missing entirely. There is also no re‑fetch on accept/reject as the spec requires, and the existing `TurnDiffInspector` was never refactored to take props — so criterion 8 ("`useDiff` output passes into `TurnDiffInspector` props") cannot be satisfied at the code level.

Build, scene/tour lints, and tests pass; bundle is well under budget (App chunk 82.41 KB gzipped); legacy launcher build is clean (108.53 KB gzipped); 50 scenes registered; mocks intact. But criteria **5, 6, 7, 8 fail outright**, and the missing dispatch makes the live scene a presentational stub rather than a wired surface. Verdict: **NEEDS-CHANGES** — Phase 2 must be redone to match the spec before M5 can ship.

### Blockers

- **`plugins/dust/src/whim/protocol.ts` (missing file)** Criterion 5 requires `plugins/dust/src/whim/protocol.ts` exporting `CODE_DIFF_ACCEPT_OP` (and `CODE_DIFF_REJECT_OP`). The file does not exist; the constant was placed in `hooks/useDiffs.ts:8` under the wrong name (`ACCEPT_HUNK_OP`, not `CODE_DIFF_ACCEPT_OP`), and `CODE_DIFF_REJECT_OP` is absent entirely. The verification command in the criterion (`grep "CODE_DIFF_ACCEPT_OP" plugins/dust/src/whim/protocol.ts`) returns 0, not 1.
  Why: the mission spec line 75 says "`CODE_DIFF_ACCEPT_OP` consumed from `protocol.ts`, not inlined as a string anywhere in whim" — the legacy launcher's `ComponentRenderer.tsx:17` should re‑import from `protocol.ts` so both surfaces consume one source of truth. Today both files inline `'code_diff.accept_hunk'` independently.
  Fix: create `plugins/dust/src/whim/protocol.ts` that exports `CODE_DIFF_ACCEPT_OP = 'code_diff.accept_hunk'` and `CODE_DIFF_REJECT_OP = 'reject_hunk'`. Update `src/ComponentRenderer.tsx:17` and `hooks/useDiffs.ts:8` to import from it. Delete the local `ACCEPT_HUNK_OP` re‑export.

- **`plugins/dust/src/whim/hooks/useDiff.ts` (missing file)** Criterion 6 requires `useDiff` (singular) at `hooks/useDiff.ts` with the prescribed return shape: `changedFiles`, `activePath`, `setActivePath`, `hunks`, `acceptHunk`, `rejectHunk`, `loading`, `error`. The implementer created `hooks/useDiffs.ts` (plural) with a different shape: `files`, `loading`, `error`, `loadDiff`, `rejectHunk`, `refresh` — no `activePath`/`setActivePath`/`hunks`/`acceptHunk`. Selection state is held in the component (`LiveDiffPanel.tsx:188-200`), so the hook is not reusable as the mission intended.
  Fix: rename the file to `useDiff.ts`, hoist `activePath`/`setActivePath` into the hook, expose `hunks` as the active file's hunks, and add `acceptHunk` (see next blocker). Delete `useDiffs.ts` once consumers are migrated.

- **`plugins/dust/src/whim/hooks/useDiffs.ts:1-77` (missing `acceptHunk`)** The mission spec line 59-60 explicitly requires `acceptHunk(hunkId)` to call `invoke('dispatch_action', { plugin_id: 'chat', action_id: CODE_DIFF_ACCEPT_OP, params: { hunk_id } })` and then re‑fetch `list_changed_files`. The hook has no `acceptHunk` at all, and the consumer's `LiveDiffPanel.handleAccept` (`components/LiveDiffPanel.tsx:202-208`) just sets local state and runs `void Promise.resolve(ACCEPT_HUNK_OP)` — a literal no‑op that captures the constant but never invokes any IPC. The accept side of the diff surface is therefore not wired.
  Why: `code_diff.accept_hunk` is the action that triggers the chat plugin to actually apply the hunk against working tree state. Without `dispatch_action`, the live scene is purely visual — clicking Accept changes the badge and does nothing else. This breaks the M5 deliverable's primary user flow.
  Fix: add `acceptHunk(hunkId)` to the hook calling `invoke('dispatch_action', { plugin_id: 'chat', action_id: CODE_DIFF_ACCEPT_OP, params: { hunk_id } })`, then `refresh()` to re-fetch the changed files list. Wire `LiveDiffPanel.handleAccept` to call it instead of `void Promise.resolve(...)`. Apply the same re-fetch pattern to `rejectHunk` (currently `useDiffs.ts:72-74` rejects but does not refresh).

- **`plugins/dust/src/whim/types.ts:1-80` (missing types)** Criterion 7 requires `ChangedFile`, `Hunk`, `HunkLine` to be in `whim/types.ts`. None of them are; `useDiffs.ts:3` and `LiveDiffPanel.tsx:3` both import from `../mocks/diffs.ts`. The mission spec line 53 says these must mirror the existing whim mock shape so `TurnDiffInspector` can consume both mock and real data through one type — that centralization didn't happen.
  Additionally, the type used (`mocks/diffs.ts:11-16`) does **not** match the Rust serde shape (`src-tauri/src/lib.rs:723-744`):
  - Rust `GitHunk { id, header, lines }` — TS mock `Hunk { id, header, lines, forcedFailOnApply? }` — extra optional `forcedFailOnApply` is fine for fixtures.
  - Rust `ChangedFile { path, additions, deletions, why, hunks }` — TS `ChangedFile { path, additions, deletions, why, hunks }` — matches.
  - Rust `HunkLine { kind: String (renamed to "type"), content }` ↔ TS `HunkLine { type: HunkLineType, content }` — matches via `#[serde(rename = "type")]` (`lib.rs:725`).
  So the byte-level match is OK, but the **organization is wrong**: types live in `mocks/`, not `types.ts`. Importing real-data types from a `mocks/` module is a code-smell that will break the moment someone trims the mock file.
  Fix: move `ChangedFile`, `Hunk`, `HunkLine`, `HunkLineType` from `mocks/diffs.ts` to `whim/types.ts`. Have `mocks/diffs.ts` re-export them (or import from `types.ts`) so the mock fixture data continues to type-check.

- **`plugins/dust/src/whim/components/LiveTurnDiffInspector.tsx` (missing file) ↔ `components/TurnDiffInspector.tsx:16-22` (not refactored)** Criterion 8 requires `LiveTurnDiffInspector` to render `<TurnDiffInspector files={…} hunks={…} activePath={…} onSelectFile={…} onAcceptHunk={…} onRejectHunk={…} />`. The file does not exist; instead a brand-new `LiveDiffPanel.tsx` was authored from scratch with its own inline file-tab strip and hunk-card. `TurnDiffInspector` itself was not refactored — `interface TurnDiffInspectorProps { onClose?: () => void }` (`TurnDiffInspector.tsx:16-18`) still hard-codes `MOCK_TURNS`/`MOCK_HUNK_LINES`/`MOCK_COLLAPSED_CONTEXT` imports (lines 1-9) and accepts no data props. Code-level evidence that "`useDiff` output passes into `TurnDiffInspector` props" therefore cannot exist.
  Why: the mission spec is explicit that `TurnDiffInspector` is the canonical diff surface and the live scene must drive it via real IPC — that's the point of M5. The inline `LiveDiffPanel` does not exercise the existing component, leaves `TurnDiffInspector` mock-only, and forks the diff UI surface.
  Fix: refactor `TurnDiffInspector` to accept `files`/`hunks`/`activePath`/`onSelectFile`/`onAcceptHunk`/`onRejectHunk` as props (mirror M4's `FilesPanel` boundary refactor). Update every existing `<TurnDiffInspector …>` site in `Scenes.tsx` to pass mock data explicitly so the 49 prior scenes keep rendering. Then create `components/LiveTurnDiffInspector.tsx` that wraps `useDiff('/Users/joeyhipolito/nanika')` and renders `<TurnDiffInspector …>` driven by hook output. Replace `LiveDiffPanelScene` with `LiveTurnDiffInspectorScene`; update the App.tsx scene id from `'live-diff-panel'` to `'live-turn-diff-inspector'` and the title/desc to match the mission spec line 65.

- **`plugins/dust/src/whim/hooks/useDiffs.ts:46-70` (no re-fetch on mutation)** Mission constraint line 76 ("Atomic-replace pattern preserved in `useDiff` — `whim://fs-changed` triggers re-fetch, not in-place edit") combined with spec line 59 ("Re-fetch on every accept/reject") requires `acceptHunk`/`rejectHunk` to call `list_changed_files` after the IPC settles. Today neither does — `rejectHunk` (line 72-74) calls `rejectHunkCmd` and ignores the result; there is no `acceptHunk`. After a mutation the UI is stale until manual refresh.
  Fix: in the redesigned `acceptHunk`/`rejectHunk`, await the invoke and call `refresh()` (or inline `setFiles(await listChangedFiles(repoRoot))`) before returning. Optionally subscribe to `whim://fs-changed` to invalidate on out-of-band edits.

### Warnings

- **`plugins/dust/src-tauri/src/lib.rs:782-784`** `cargo clippy` flags `clippy::manual_strip` on the new diff-line parser: the code tests `line.starts_with(' ')` and then slices `line[1..]` instead of using `strip_prefix(' ')`. The same pattern exists for `+` (line 778-779) and `-` (line 780-781) but those use `[1..]` on a guarded prefix and clippy only catches the `' '` arm. Default `cargo clippy` reports as a warning; under `-D warnings` it fails. Project lints today are `lint:scenes` + `lint:tour` (both pass), so this does not gate criterion 11, but it's worth cleaning up. Fix: `} else if let Some(rest) = line.strip_prefix(' ') { cur_lines.push(HunkLine { kind: "ctx".to_string(), content: rest.to_string() }); }`. While in the file, apply the same pattern to the `+` and `-` branches for consistency.

- **`plugins/dust/src-tauri/src/lib.rs:765-790` (hunk id collision risk)** `cur_id = format!("{path}:{old_start}:{new_start}")` synthesizes hunk ids from `(old_start, new_start)`. For a freshly added file, all hunks have `old_start = 0`, and a multi-hunk new file (rare via `git diff` since new files emit one hunk, but possible after `git diff --no-renames` or with `--inter-hunk-context=0`) would collide. The mock fixture uses indexed ids (`hunk-0-0`, `hunk-1-0`, …) which never collide. Fix: incorporate a per-file index suffix (`{path}:{old_start}:{new_start}:{idx}`) or fall back to `{path}#{idx}` — minor but worth doing before users start trusting hunk-ids as primary keys.

- **`plugins/dust/src-tauri/src/lib.rs:885-901` (no validate_path on repo_root or path)** `get_file_diff` and `list_changed_files` accept `repo_root` (and `path`) directly from the renderer and pass them as `current_dir` / arg to a child process. M4's `validate_path` (referenced in REVIEW-M4.md) gates symlink escape and absolute-path injection on the files surface; the diff commands skip this. A malicious renderer could pass `repo_root = "/"` or `path = "../../etc/passwd"` and have `git diff` enumerate or expose state outside the project. Fix: thread the same `validate_path` (or an equivalent allowlist of canonicalized prefixes) through both diff commands. This is a hardening warning rather than a present-day exploit (renderer is co-trusted today), but it should be on the M7 watchlist.

- **`plugins/dust/src/whim/hooks/useDiffs.ts:54-62` (cancellation flag protects fetch but error path leaks)** The mount-effect uses a `cancelled` flag on both success and error paths (lines 59-60), which is correct — criterion 14 (async-listen cleanup race) is satisfied for the initial fetch even though no `listen()` is registered (the hook does not subscribe to `whim://fs-changed`). One small leak: the `loadDiff` callback (line 64-70) does not use a cancellation flag, so a stale promise resolving after unmount calls `setFiles`. React 18+ swallows that warning quietly, but if you migrate to a stricter store (zustand, jotai) it'll log. Fix: track an outer `cancelled` ref or wrap in `AbortController`.

- **`plugins/dust/src/whim/components/LiveDiffPanel.tsx:12` (hard-coded repo_root)** `const DEMO_REPO_ROOT = '/Users/joeyhipolito/nanika'` is fine for the demo scene but ties the live scene to one user. M4's `LiveFilesPanel` has the same convention so this is consistent with prior phases — note it for M7's project-picker work.

### Suggestions

- **`plugins/dust/src/ComponentRenderer.tsx:17`** Once `protocol.ts` lands, change this line to `import { CODE_DIFF_ACCEPT_OP } from './whim/protocol'` (or vice-versa, depending on the desired dependency direction). Inlining the same string in two files is the bug the mission was trying to prevent.

- **`plugins/dust/src-tauri/src/lib.rs:840-863`** `list_changed_files` runs `git diff --numstat` and `git diff --numstat --cached` sequentially. They're independent; `tokio::join!` would let them run in parallel for a small wall-clock win on large repos. Not worth doing standalone but cheap to slot in next time the file is touched.

- **`plugins/dust/src-tauri/src/lib.rs:904-908`** `reject_hunk` is a stub that `eprintln!`s and returns `Ok(())`. The mission accepts this for v0 (mission line 45) but the spec also suggested `tracing::info!`. Switch to `tracing::info!(target: "dust::diff", hunk_id = %hunk_id, "reject_hunk")` for parity with the rest of the Rust code's structured logging.

- **`plugins/dust/src/whim/components/LiveDiffPanel.tsx:189-200`** `selectedFile = files.find(f => f.path === selectedPath) ?? files[0] ?? null` re-runs every render. After the hook refactor, this state belongs in the hook (`activePath` / `hunks`), not the component.

### What's Good

- **Async-clean Rust handlers.** `list_changed_files`, `get_file_diff`, and `reject_hunk` all use `tokio::process::Command` with `.output().await` (`lib.rs:799-802`, `:886-891`, `:905-907`). No `std::process::Command`, no `spawn_blocking` shenanigans, no synchronous I/O on the async runtime — criterion 15 is satisfied cleanly. This avoids the bug class M4's review flagged on `search_files`.
- **Porcelain v1 -z parsing handles renames.** `lib.rs:818-833` correctly skips the second NUL record when status code contains `R` or `C`, so renamed files don't double-count or absorb the next entry's path. Subtle bug class avoided.
- **Hunk-header parser is regex-free.** `parse_hunk_header` (`lib.rs:747-755`) uses `strip_prefix` and `split_once` instead of regex, matching the Rust best-practices skill and avoiding a regex dep just for this.
- **Type byte-alignment with serde rename.** `HunkLine { kind, content }` with `#[serde(rename = "type")]` (`lib.rs:725`) lines up with the TS shape `{ type, content }` exactly. Even though the type centralization is wrong (see blocker), the wire shape is correct.
- **Command registration is complete.** All three commands are listed in `tauri::generate_handler!` (`lib.rs:1209-1211`); criterion 4's grep returns 3.
- **Build + lints + tests + legacy launcher all pass.** `npm run build` exits 0 (App 82.41 KB gzip, 4× under budget); `npm run lint:scenes` reports 50 scenes; `npm run lint:tour` reports 26 anchors all resolving; `cargo check` exits 0; `npm test` 32/32 passing; `VITE_DUST_LEGACY_LAUNCHER=1 npm run build` exits 0 (App 108.53 KB gzip). Criteria 2/3/9/10/11/13 all satisfied at the build level.
- **Mocks preserved.** `mocks/diffs.ts` and `mocks/turnDiffs.ts` both still on disk; criterion 12 satisfied.

---

## Acceptance Criteria — Verification

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| 1 | ≥ 3 phase commits land | ⏳ pending | `git log main..HEAD` shows 2 commits (`f48ec8bd` rust-commands, `dacdc5e7` hook-and-scene); the review commit will be the third |
| 2 | Build clean; bundle ≤ 350 KB | ✅ pass | `npm run build` exits 0; `dist/assets/App-CJeE5Wkj.js` 320.20 KB raw / **82.41 KB gzip** (4× under budget) |
| 3 | `cargo check` clean | ✅ pass | `cd src-tauri && cargo check` finishes "0 errors, 0 warnings" |
| 4 | 3 commands registered | ✅ pass | `lib.rs:796` `list_changed_files`, `:877` `get_file_diff`, `:905` `reject_hunk`; all three in `generate_handler!` at `:1209-1211` |
| 5 | `protocol.ts` with `CODE_DIFF_ACCEPT_OP` matching legacy | ❌ **FAIL** | `protocol.ts` does not exist; constant placed in `hooks/useDiffs.ts:8` as `ACCEPT_HUNK_OP`; `CODE_DIFF_REJECT_OP` missing entirely. Legacy value (`'code_diff.accept_hunk'` at `ComponentRenderer.tsx:17`) is correct, but the centralization point is missing |
| 6 | `useDiff` hook at `hooks/useDiff.ts` with prescribed shape | ❌ **FAIL** | File is `hooks/useDiffs.ts` (plural), wrong name. Return shape `{ files, loading, error, loadDiff, rejectHunk, refresh }` does not match required `{ changedFiles, activePath, setActivePath, hunks, acceptHunk, rejectHunk, loading, error }`. `acceptHunk` missing |
| 7 | `ChangedFile`/`Hunk`/`HunkLine` in `whim/types.ts`, matching Rust + mock | ❌ **FAIL** | `types.ts:1-80` does not contain any of the three types. `useDiffs.ts:3` and `LiveDiffPanel.tsx:3` import them from `mocks/diffs.ts:4-23`. Wire-level shape happens to match Rust (`lib.rs:723-744`) but organization is wrong |
| 8 | `LiveTurnDiffInspector` renders without crash; `useDiff` output → `TurnDiffInspector` props | ❌ **FAIL** | `components/LiveTurnDiffInspector.tsx` does not exist. `components/TurnDiffInspector.tsx:16-18` still defines `interface TurnDiffInspectorProps { onClose?: () => void }` and hard-codes `MOCK_TURNS`/`MOCK_HUNK_LINES` imports (lines 1-9) — never accepts `files`/`hunks`/`onAcceptHunk`/`onRejectHunk`. The implementer authored a fork (`LiveDiffPanel.tsx`) instead. Additionally, `LiveDiffPanel.handleAccept` (`:202-208`) `void Promise.resolve(ACCEPT_HUNK_OP)` is a no-op — the accept action is never dispatched |
| 9 | Existing 49 scenes still render; `SCENARIO_LIST.length` = 50 | ✅ pass | `awk '/SCENARIO_LIST/,0' App.tsx \| grep -c "^  { id:"` = 50; `lint:scenes` reports `ok (50 scenes registered)` |
| 10 | Tour + RealUsageDemo unaffected | ✅ pass | `App.tsx:103-104` `'tour': TourScene` and `'real-usage-demo': RealUsageDemoScene` registrations unchanged; `Scenes.tsx:4148` `RealUsageDemoScene` re-export intact; `lint:tour` reports `26 anchors referenced, all resolve` |
| 11 | Lints pass | ✅ pass (project lints) ⚠️ (clippy) | `npm run lint:scenes` ok; `npm run lint:tour` ok; `cargo clippy` emits 1 new warning (`clippy::manual_strip` at `lib.rs:783`) — does not fail default clippy but would fail under `-D warnings` |
| 12 | Mocks not deleted | ✅ pass | `mocks/diffs.ts` and `mocks/turnDiffs.ts` both present |
| 13 | Legacy launcher build clean | ✅ pass | `VITE_DUST_LEGACY_LAUNCHER=1 npm run build` exits 0; App chunk **108.53 KB gzip** (under budget) |
| 14 | Async-listen cleanup race fixed | ✅ pass (vacuously) | `useDiffs.ts` does not register a `whim://fs-changed` listener — the race class does not apply. The mount-effect at `:54-62` does use a `cancelled` flag correctly on both success and error paths. Note: the hook should ideally subscribe to `whim://fs-changed` per the mission's optional optimization, but lack-of-subscription is not a regression |
| 15 | No blocking git ops on async runtime | ✅ pass | `lib.rs:797`, `:882`, `:905` all use `use tokio::process::Command` with `.output().await`. No `std::process::Command`, no synchronous `git2`, no `spawn_blocking` workaround needed |
| 16 | `REVIEW-M5.md` exists with sections + `### Blockers` + `### Warnings` | ✅ pass | This file at `plugins/dust/REVIEW-M5.md`; `### Blockers` at line 30, `### Warnings` at line 50; Summary, Suggestions, What's Good, Acceptance Criteria all present |

**Counts:** ✅ 11 pass · ⏳ 1 pending (commit count, satisfied by this review) · ❌ 4 fail (criteria 5, 6, 7, 8)

## Verdict

**NEEDS-CHANGES.** The Rust phase is solid and ships as-is. The TS phase needs to be reworked to match the spec: create `protocol.ts`, rename to `useDiff.ts` with the correct return shape and a real `acceptHunk` that calls `dispatch_action`, move types into `whim/types.ts`, refactor `TurnDiffInspector` to take props, and create `LiveTurnDiffInspector` that drives it via `useDiff`. The current `LiveDiffPanel` works as a visual demo but does not satisfy four blocking acceptance criteria and leaves the accept code path entirely disconnected from the chat plugin.

Estimated rework: 60–90 minutes (most of it is mechanical — moving types, renaming files, refactoring `TurnDiffInspector` props, replacing the `void Promise.resolve` line with the real `invoke`).
