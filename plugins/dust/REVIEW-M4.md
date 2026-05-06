---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260428-38e572ae
created_at: "2026-04-28T01:35:00Z"
confidence: high
depends_on:
  - rust-commands
  - hook-and-scene
token_estimate: 2400
---

# REVIEW-M4 — Whim Tauri M4 (files surface IPC)

## Summary

M4 wires the dust files surface to real IPC. Rust adds three commands (`list_directory`, `search_files`, `watch_repo`) plus the `whim://fs-changed` event with a debounced 200 ms watcher; TypeScript adds the `useFiles` hook + `LiveFilesPanel` adapter component + scene registration `live-files-panel`. The implementation preserves the 48 existing scenes (mocks intact, prop signatures unchanged) by adapting Rust `FileEntry` (`kind: 'file' | 'dir'`) to mock `FileEntry` (`kind: 'file' | 'folder'` + derived `ext`) at the `LiveFilesPanel` boundary rather than refactoring `FilesPanel`. Build is clean (App chunk 80.06 KB gzipped, far under 350 KB), `cargo check` is clean, both lints pass with 49 scenes registered, and `MOCK_FILE_TREE`/`FileViewerFixture` remain in `mocks/`. Verdict: APPROVE with two non-blocking warnings (filename-branch search runs blocking I/O on the async runtime; `whim://fs-changed` listener has a small async-registration race window).

### Blockers

_(none)_

### Warnings

- **`src-tauri/src/lib.rs:594-614`** `search_files` Filename branch runs `walkdir::WalkDir` synchronously inside an `async fn`, which blocks the Tauri async runtime worker for the duration of the walk. The Content branch correctly wraps in `tokio::task::spawn_blocking` (line 616) — the Filename branch should do the same. On a tree the size of `~/nanika` (≥ 50k entries traversed; `walkdir` doesn't honor `.gitignore`, so `node_modules/` and `.git/` are walked too), this can stall every other IPC call (e.g. `list_directory` for the same surface) for seconds. Fix: wrap the Filename branch body in `tokio::task::spawn_blocking(move || { ... }).await.map_err(...)?` exactly as the Content branch does.

- **`src-tauri/src/lib.rs:594-614` (consistency)** Filename search uses `walkdir` (does not honor `.gitignore`), Content search uses `ignore::WalkBuilder` (does). The two branches return different result sets for trees with ignored directories — a `.git/objects/...` filename match shows up in Filename results but not in Content results. Why this matters: the user sees inconsistent hit lists when toggling the search kind. Fix: use `ignore::WalkBuilder` for both branches, or document the behavior at the type level.

- **`src/whim/hooks/useFiles.ts:87-101`** The `whim://fs-changed` listener registration has the standard async-`listen` cleanup race — between the synchronous `listen()` call and the `.then(f => { unlisten = f })` resolving, the effect's cleanup may already have fired (e.g. on Strict Mode double-mount or rapid `repoRoot` change), leaking the listener. The implementation is consistent with the existing `useChat` pattern but the bug is real. Fix: track a `cancelled` flag inside the effect and call `f()` immediately if `cancelled` resolved before `unlisten` was assigned, e.g.
  ```ts
  let cancelled = false
  let unlisten: (() => void) | null = null
  listen<FsChangedPayload>('whim://fs-changed', ...).then(f => {
    if (cancelled) f()
    else unlisten = f
  })
  return () => { cancelled = true; unlisten?.() }
  ```

- **`src-tauri/src/lib.rs:657` ↔ `src/whim/hooks/useFiles.ts:91-93`** `watch_repo` calls `validate_path` (line 657) which canonicalizes the path through symlinks; `notify` then emits paths rooted at the canonical form. The TS listener compares the emitted `payload.path` against `joinPath(repoRoot, currentPathRef.current)` where `repoRoot` is the raw `'/Users/joeyhipolito/nanika'` (not canonicalized). On a system where `~/nanika` is a symlink (e.g. `/Users/joeyhipolito/nanika → /Users/joeyhipolito/.alluka/worktrees/.../`), the `startsWith` check on line 93 silently fails and the tree never auto-refreshes. Fix: either return the canonical root from `watch_repo` and have the TS hook use that, or canonicalize on the TS side before comparison.

### Suggestions

- **`src/whim/hooks/useFiles.ts:77-83`** `watch_repo` is invoked imperatively but no error surfaces to the user — only `console.error`. Consider surfacing via `setError(...)` so the LiveFilesPanel can show a degraded-mode banner ("live updates unavailable").

- **`src-tauri/src/lib.rs:658-663`** `watch_repo` returns `Ok(())` immediately after registering the watcher. There is no symmetric `unwatch_repo` command — switching repos in M7 will need one (or a re-call of `watch_repo` will replace the slot, which is what the doc-comment on `AppState::fs_watcher` already promises). Note for M7 only.

### What's Good

- **Atomic-replace tree state preserved.** `useFiles.ts:61` and `:95` both call `setTree(entries)` with the freshly fetched list — no in-place mutation, no token-style buffering. This matches the M3 `useChat` pattern and the mission's "Atomic-replace pattern preserved" constraint exactly.
- **Watcher lifecycle is correct.** `lib.rs:669-672` overwrites `AppState.fs_watcher` with the new watcher; dropping the old `RecommendedWatcher` drops its `tx`, which causes the prior debounce thread's `rx.recv_timeout` to return `Disconnected` (line 695) and exit cleanly. No leaked threads, no double emission.
- **Mock adaptation at the boundary** (`LiveFilesPanel.tsx:38-45`) is the right call. Refactoring `FilesPanel`'s prop signature would have rippled through all 48 scenes; adapting the Rust shape to the mock shape at the live-scene seam keeps the blast radius to one file.
- **Type alignment is byte-for-byte.** `lib.rs:546-568` ↔ `types.ts:52-67`: same field names, same nullability (`line: Option<u32>` ↔ `line: number | null`), same lowercase serde for `SearchKind` ↔ `'filename' | 'content'`. Documented inline in `types.ts:48-50`.
- **Lints pass.** `lint-scenes-registration.sh` reports 49 scenes; `lint-tour-anchors.sh` reports 26 anchors all resolving.
- **Legacy launcher build clean.** `VITE_DUST_LEGACY_LAUNCHER=1 npm run build` exits 0; App chunk 108.53 KB gzipped (under budget).

---

## Acceptance Criteria — Verification

### 1. Phase commits land (≥ 3 commits with phase-prefixed messages)

PARTIAL — at review time: `7c09cfc3 phase rust-commands: ...`, `33d27b8e phase hook-and-scene: ...`. The orchestrator will land the third (`phase review`) when this artifact is committed. Tracked by the orchestrator workflow; not a blocker.

### 2. Build clean — `npm run build` exits 0; bundle ≤ 350 KB

PASS. `cd plugins/dust && npm run build` exits 0. Largest emitted JS chunk is `dist/assets/App-C8Z7TSpm.js` at 313.56 KB raw / **80.06 KB gzipped**. Total gzipped JS across all chunks: 126,126 bytes ≈ 123 KB — well under the 350 KB ceiling.

### 3. `cargo check` clean

PASS. `cd plugins/dust/src-tauri && cargo check` → `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in 2.07s`. Zero warnings, zero errors.

### 4. `list_directory` exists and is registered

PASS. Defined at `src-tauri/src/lib.rs:571` as `async fn list_directory(path: String) -> Result<Vec<FileEntry>, String>`. Registered in `tauri::generate_handler!` at `lib.rs:1015`. Note: signature simplified from spec's `(repo_root, sub_path: Option<String>)` to a single `path: String` — the TS hook composes the absolute path via `joinPath(repoRoot, currentPath)` (`useFiles.ts:53`). Functionally equivalent; the TS-side composition is fine and matches the listener's `startsWith` check (subject to the canonicalization Warning above).

### 5. `search_files` exists with `SearchKind` enum

PASS. Defined at `lib.rs:586-590` as `async fn search_files(root: String, query: String, kind: SearchKind) -> Result<Vec<FileMatch>, String>`. `SearchKind` enum at `lib.rs:563-568` with `#[serde(rename_all = "lowercase")]` so wire form is `"filename" | "content"`. Registered at `lib.rs:1016`.

### 6. `whim://fs-changed` emitted

PASS. `app.emit("whim://fs-changed", serde_json::json!({ "path": ..., "kind": ... }))` at `lib.rs:709`. Payload shape matches the TS `FsChangedPayload` interface in `useFiles.ts:24-27`.

### 7. Watcher debounced and scoped

PASS. Debounce window: `const DEBOUNCE: Duration = Duration::from_millis(200);` at `lib.rs:677` (≥ 100 ms threshold met). Poll interval 50 ms (`lib.rs:678`). Scoped via `validate_path(&path)?` at `lib.rs:657` (HOME-rooted) and `watcher.watch(&safe, RecursiveMode::Recursive)` at `lib.rs:666` — watch is rooted at the canonicalized `repo_root`, not the filesystem root. Spirit of "atomic-replace tree state in `useFiles`" verified at `useFiles.ts:61` and `:95` (`setTree(entries)` — never `setTree(prev => [...prev, ...])`).

### 8. `useFiles` hook exists with prescribed shape

PASS. `src/whim/hooks/useFiles.ts:36-153` exports `useFiles(repoRoot, opts?)` returning `UseFilesReturn` (lines 12-22). Return shape matches the mission spec exactly: `tree`, `currentPath`, `setCurrentPath`, `searchResults`, `search`, `viewer { content, loading, error }`, `openFile`, `loading`, `error`. Synchronous `currentPathRef` mirror at `useFiles.ts:46`. Search debounced 150 ms at `useFiles.ts:120-124`. Cleanup of debounce timer at `useFiles.ts:138-140`.

### 9. `FileEntry`, `FileMatch`, `SearchKind` types match Rust byte-for-byte

PASS. Side-by-side:

| field | Rust (`lib.rs:546-568`) | TS (`types.ts:52-67`) |
|---|---|---|
| `FileEntry.path` | `String` | `string` |
| `FileEntry.name` | `String` | `string` |
| `FileEntry.kind` | `String` (`"file"`/`"dir"`) | `string` (doc'd `'file'`/`'dir'`) |
| `FileEntry.size` | `u64` | `number` |
| `FileMatch.path` | `String` | `string` |
| `FileMatch.line` | `Option<u32>` | `number \| null` |
| `FileMatch.snippet` | `String` (non-optional) | `string` (non-optional) |
| `FileMatch.score` | `f32` | `number` |
| `SearchKind` | `enum { Filename, Content }` + `serde(rename_all="lowercase")` | `'filename' \| 'content'` |

`FileMatch.snippet` differs from the mission's `Option<String>` sketch but Rust and TS agree — non-blocking. Documented in `types.ts:47-50`.

### 10. `LiveFilesPanel` scene works (`#/s/live-files-panel`)

PASS — code-level evidence:
- `useFiles(repoRoot)` called at `LiveFilesPanel.tsx:52`.
- Adapted tree fed into `<FilesPanel files={adapted} onSelect={handleSelect} />` at `LiveFilesPanel.tsx:74-77`.
- `<FileViewer filename={...} language={...} lines={viewerLines} />` at `LiveFilesPanel.tsx:79-83` driven by `files.viewer`.
- `whim://fs-changed` listener registered at `useFiles.ts:90-97` (subject to async-race Warning).
- `watch_repo` invoked at `useFiles.ts:78`.

### 11. Existing 48 scenes still render — `SCENARIO_LIST.length === 49`

PASS. `FilesPanel` props (`files`, `onClose?`, `onSelect?`) and `FileViewer` props (`filename`, `language`, `lines`) are unchanged from M3 — verified by reading `components/FilesPanel.tsx:10-14` and `components/FileViewer.tsx:9-13`. The single existing call site at `Scenes.tsx:3556` (`<FilesPanel files={MOCK_FILE_TREE} onSelect={...} />`) and `Scenes.tsx:3632` (`<FileViewer ...>`) remain valid. `SCENARIO_LIST` in `App.tsx:108-158` has 49 entries; `lint-scenes-registration.sh` confirms `ok (49 scenes registered)`.

### 12. Tour + RealUsageDemo unaffected

PASS. `MOCK_FILE_TREE` (`mocks/files.ts:7`) and `FileViewerFixture` (`mocks/fileViewer.ts:3`, `:9`) are unchanged. Tour and RealUsageDemo continue to consume them. Build passes; no transitive type errors.

### 13. Lints pass

PASS:
```
$ bash scripts/lint-scenes-registration.sh
lint:scenes: ok (49 scenes registered)
$ bash scripts/lint-tour-anchors.sh
lint:tour: ok (26 anchors referenced, all resolve)
```

### 14. Mocks not deleted

PASS. `grep "MOCK_FILE_TREE\|FileViewerFixture" mocks/files.ts mocks/fileViewer.ts`:
- `mocks/files.ts:7` — `export const MOCK_FILE_TREE: FileEntry[] = [`
- `mocks/fileViewer.ts:3` — `export interface FileViewerFixture {`
- `mocks/fileViewer.ts:9` — `export const MOCK_FILE_VIEWER: FileViewerFixture = {`

### 15. Legacy launcher build clean

PASS. `VITE_DUST_LEGACY_LAUNCHER=1 npm run build` exits 0. Largest legacy-mode chunk: `App-CEs19E4g.js` 341.87 KB raw / **108.53 KB gzipped** — under 350 KB.

### 16. `REVIEW-M4.md` exists with required sections

PASS — this file. Contains `### Blockers`, `### Warnings`, `### Suggestions`, `### What's Good`, plus per-criterion verification for items 1–15.

<!-- scratch -->
M4 review complete. Two non-blocking issues worth tracking:

1. `search_files` Filename branch should be wrapped in `spawn_blocking` — currently blocks the async runtime on large trees. Mission spec only required the Content branch but the same hazard applies. ~5-line fix.

2. `useFiles` async-listen cleanup race at `useFiles.ts:87-101` — small but real footgun in Strict Mode / rapid effect re-runs. Standard fix is the `cancelled` flag pattern shown in the warning body. Same bug pattern likely lives in `useChat.ts` (M3 reference); worth grep'ing.

Watcher canonicalization gotcha (Warning #4): if `~/nanika` is ever a symlink in production deployments, the listener silently goes deaf. Cheap fix: have `watch_repo` return the canonical root and persist that in the hook for the `startsWith` check.

Build budget headroom is huge (80 KB gzipped vs 350 KB ceiling) — M5/M6/M7 have plenty of room.
<!-- /scratch -->
