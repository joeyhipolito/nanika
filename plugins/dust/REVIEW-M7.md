---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260428-662d84a1
created_at: "2026-04-28T03:10:00Z"
confidence: high
depends_on:
  - rust-commands
  - hooks-and-scenes
token_estimate: 2400
---

# REVIEW-M7 — Whim Tauri M7 (rail + top-bar + commit + notifications)

## Summary

M7 wires four remaining whim surfaces to live Tauri commands: left rail (projects/routines/pins), top action bar (git status), commit summary card (commit metadata + PR), and notifications canvas. Backend ships 17 new `#[tauri::command]` functions across `rail.rs`, `git.rs`, `commit_summary.rs`, `notifications.rs` (Phase 1, commit `f776b0e2`); frontend ships four hooks + four Live wrappers + four registered scenes (Phase 2, commit `261d9e4c`). Build is clean, both lints pass, vitest is green, bundle is 87 KB gzipped (75 % under the 350 KB cap), `cargo check` is clean, async-listen cleanup race is fixed in all four hooks, FIFO cap is enforced server-side, `WHIM_ALLOW_PUSH` gate is in place. **Verdict: NEEDS-CHANGES** — one BLOCKER (`parse_git_status` parser is wrong, returns unknown branch + empty changes for all repo states) plus one criterion-5 gap (`whim://notification-update` is listened on but never emitted).

## Acceptance criteria — file:line evidence

| # | Criterion | Status | Evidence |
|---|-----------|--------|----------|
| 1 | ≥ 3 phase commits land | ✅ | `git log` shows `f776b0e2` rust-commands, `261d9e4c` hooks-and-scenes; this review is phase 3 |
| 2 | Build clean; bundle ≤ 350 KB | ✅ | `npm run build` → `App-DiTjJ7Vu.js 336.72 kB │ gzip: 87.00 kB`; well under cap |
| 3 | `cargo check` clean | ✅ | `cargo check` → `Finished dev profile … in 0.60s`, 0 errors, 0 warnings |
| 4 | ≥ 12 new Tauri commands | ✅ | `grep -c '#\[tauri::command\]'`: rail.rs=4, git.rs=6, commit_summary.rs=3, notifications.rs=4 → 17 total; all registered in `lib.rs:1241-1260` |
| 5 | 3 new event channels emit | ⚠️ partial | `notifications.rs:118` (`whim://routines-changed`) + `notifications.rs:145` (`whim://notification`); **`whim://notification-update` is consumed at `useNotifications.ts:71` but no backend `app.emit("whim://notification-update", …)` exists** — see Blocker below |
| 6 | Pin storage uses `pins.json` (option b) | ✅ | `rail.rs:113` `.join(".alluka/whim/pins.json")`; `rail.rs:108-124` (`read_pins`); `rail.rs:127-142` (`write_pins`) |
| 7 | Notifications persisted to `notifications.json`, 500-cap, FIFO | ✅ | `notifications.rs:18` `const NOTIFICATION_FIFO_CAP: usize = 500`; `notifications.rs:149` `notifications.insert(0, notif)` (new at front); `notifications.rs:152-154` `if notifications.len() > NOTIFICATION_FIFO_CAP { notifications.truncate(NOTIFICATION_FIFO_CAP); }` (drops tail = oldest = FIFO) |
| 8 | `git_push` gated by `WHIM_ALLOW_PUSH` | ✅ | `git.rs:88-91` — `if std::env::var("WHIM_ALLOW_PUSH").is_err() { return Err("push not allowed: set WHIM_ALLOW_PUSH environment variable".to_string()); }` runs before any `git push` shell-out |
| 9 | Four hooks exist | ✅ | `useRail.ts`, `useGitRepo.ts`, `useCommitSummary.ts`, `useNotifications.ts` all under `src/whim/hooks/` |
| 10 | Four live scenes registered; `SCENARIO_LIST.length = 57` | ✅ | `App.tsx:118-121` (SCENE_COMPONENTS map), `App.tsx:178-181` (SCENARIO_LIST entries with `ref: '§M7'`), `lint:scenes` → `ok (57 scenes registered)` |
| 11 | Existing 53 scenes still render | ✅ | `LeftRail.tsx:2` imports types only from mocks (`RecentItem`, `Routine`); `LeftRail.tsx:19` is fully prop-driven; `lint:scenes` passes with 57 = 53 + 4; `vitest run` → 32/32 pass |
| 12 | Lints pass | ✅ | `lint:scenes` → ok (57); `lint:tour` → ok (26 anchors resolve); `cargo clippy` reports only 6 non-fatal style warnings (see Warnings) |
| 13 | Mocks not deleted | ✅ | `ls src/whim/mocks/` — `leftrail.ts`, `notifications.ts`, `projects.ts` etc. all present; `LiveLeftRail.tsx:4` still imports `RecentItem`, `Routine as MockRoutine` from `../mocks/leftrail` |
| 14 | Legacy launcher build clean | ✅ | `lib.rs:1126` reads `DUST_LEGACY_LAUNCHER` at runtime — no conditional compilation, single build artifact serves both modes; `cargo check` covers it |
| 15 | Async-listen cleanup race fixed in all 4 new hooks | ✅ | `useRail.ts:56-75` (cancelled flag set before listen, cleanup flips + calls unlisten); `useGitRepo.ts:64-86` (same pattern + clears debounce timer); `useNotifications.ts:42-62` (whim://notification) and `useNotifications.ts:67-86` (whim://notification-update) — both listeners follow the pattern; `useCommitSummary.ts:21-38` uses cancelled-flag pattern around the two `invoke()` promises (no `listen()` in this hook). Every listener-setup `useEffect` includes `if (cancelled) { f(); return }` in the `.then(f => …)` arm to invoke the resolved unlisten if unmount races the listen-promise resolution |
| 16 | `REVIEW-M7.md` exists with `### Blockers` + `### Warnings` H3 | ✅ | This file |

## Particular-attention checks

- **(a) `git_push` gate** — `git.rs:89` checks `WHIM_ALLOW_PUSH` *before* spawning `git push`; error message is descriptive. ✅
- **(b) Notifications 500-cap with FIFO drop** — `notifications.rs:149` inserts at index 0; `notifications.rs:152-154` truncates the tail; client-side mirror at `useNotifications.ts:6` (`CLIENT_FIFO_CAP = 500`) and `useNotifications.ts:48-51` (slice-after-insert). ✅
- **(c) Async-listen cleanup race** — pattern is uniform across all four hooks (see criterion 15 row). The `then(f => { if (cancelled) { f(); return } unlisten = f })` arm is the M4/M5/M6 carry-over fix and it is present everywhere a `listen()` is awaited. ✅
- **(d) Build + cargo check + lints** — all clean (see criteria 2/3/12). Clippy warnings exist but are stylistic, not blocking.
- **(e) 53 prior scenes still render** — `lint:scenes` confirms 57 (= 53 + 4); presentational components are prop-driven (no refactor was needed). ✅
- **(f) Pin storage uses `pins.json`** — `rail.rs:113` confirms option b, matching risk #6 default. ✅

### Blockers

- **`plugins/dust/src-tauri/src/git.rs:142-202`** `parse_git_status` is broken — it splits each line on `'\t'` (`line.split('\t').collect()`) but `git status --porcelain=v2 --branch` is **space-separated**, not tab-separated. Verified by `git status --porcelain=v2 --branch | od -c`: header lines look like `# branch.head␣main\n` and ordinary changes look like `1␣A.␣N...␣<6 mode/hash fields>␣<path>\n` with no tabs. Result: `parts.len() > 1` is false on `# branch.head` (so `branch` is never set and falls back to `"unknown"` at line 193), and `parts.len() >= 2` is false on every change line (so `changes` stays empty and `has_staged`/`has_unstaged` stay `false`). Every call to `get_repo_status` returns `RepoStatus { branch: "unknown", changes: [], has_staged: false, has_unstaged: false }`, regardless of actual repo state. This silently breaks the **primary purpose** of M7's top-action-bar surface — `LiveTopActionBar.tsx:22-27` will always render breadcrumbs as `[repoName, "unknown"]` with no dirty marker, and the dirty-marker logic in `LiveTopActionBar.tsx:23` (`status.has_staged || status.has_unstaged`) is dead code in practice.
  - **Fix:** parse line-by-line on whitespace, keying off the line-type prefix (`#`, `1`, `2`, `?`, `!`, `u`):
    ```rust
    for line in output.lines() {
        if let Some(rest) = line.strip_prefix("# branch.head ") {
            branch = rest.trim().to_string();
            continue;
        }
        if line.starts_with('#') { continue; }
        // Ordinary changes: "1 XY ..."
        if let Some(rest) = line.strip_prefix("1 ").or_else(|| line.strip_prefix("2 ")) {
            let mut it = rest.splitn(9, ' ');
            let xy = it.next().unwrap_or("");
            // Field 9 is the path (or path<TAB>origPath for renames).
            let path = it.nth(7).unwrap_or("").split('\t').next().unwrap_or("");
            // ... apply XY logic, push FileChange ...
        }
        // Untracked '?' / ignored '!' handled here too.
    }
    ```
  - **Why:** without this fix, criteria 5 + 11 *appear* green (the wiring exists, scenes render) but the live data the wiring carries is meaningless — the user-facing breadcrumb regression is a far worse outcome than a build failure would have been, because nothing flags it. A unit test on `parse_git_status` against a captured `--porcelain=v2 --branch` fixture would catch this and should land alongside the fix.

- **`plugins/dust/src-tauri/src/notifications.rs` (whole file)** — criterion 5 requires three event channels to emit (`whim://routines-changed`, `whim://notification`, `whim://notification-update`). Only the first two have `app.emit(…)` sites (lines 118 and 145). **`whim://notification-update` is never emitted by the backend** even though `useNotifications.ts:71` subscribes to it for atomic-replace dismiss propagation. As shipped, `notification_dismiss` (`notifications.rs:51-57`) and `notification_dismiss_all` (`notifications.rs:61-67`) mutate `notifications.json` without notifying any frontend — the only reason the UI updates is `useNotifications.ts:93-95` and `:100`'s optimistic local replace. If a second window or any out-of-band mutation occurs, the unread count and read-state will diverge from disk indefinitely.
  - **Fix:** in `notification_dismiss`, after `persist_notifications(&notifications)?`, locate the dismissed notification and emit it: `let _ = app.emit("whim://notification-update", &notif);`. Same for `notification_dismiss_all` (one emit per dismissed entry, or a single emit with the whole vec — pick one and document). Either flavour requires the command to take `app: tauri::AppHandle` like `start_routines_watcher` does.
  - **Why:** the listener exists, the criterion explicitly enumerates this channel, and without the emit the dismiss path is silently single-window-only. This is the kind of cross-surface gap that is invisible until a second consumer (banner + HUD + badge) tries to share state.

### Warnings

- **`plugins/dust/src-tauri/src/rail.rs:73`** `list_routines()` shells out to `scheduler query items` rather than the spec's `scheduler list --json` (mission.md:49). The implementer's choice may be correct — `scheduler --help` likely supports both — but the deviation is undocumented. If `query items` is not a valid subcommand on the user's installed scheduler binary, the rail will surface a routine-fetch error at runtime. Suggest either (a) adding a one-line comment citing `scheduler --help` showing `query items` is the canonical JSON path, or (b) reverting to `scheduler list --json` as specified.

- **`plugins/dust/src-tauri/src/rail.rs:28-31` / `rail.rs:108-142`** Mission spec (mission.md:50-51) calls for `read_pins() -> Vec<String>` and `write_pins(pins: Vec<String>)` storing thread-ids. Implementation uses `Vec<PinnedItem>` with `{id, title}`. Functionally fine for v0 — the title is denormalized client-side — but every pinned title becomes stale if the underlying thread is renamed. Suggest either a doc-comment noting the trade-off or stripping back to the spec'd `Vec<String>` and resolving titles at render time from `useChat().threads`.

- **`plugins/dust/src-tauri/src/notifications.rs:71-122`** `start_routines_watcher` watches `~/.config/scheduler/scheduler.db`. The path is hard-coded with no fallback or override. If the user's scheduler runs out of `~/.alluka/run/...` (matches the runtime dir convention used elsewhere in this codebase, see `lib.rs:1080-1093`), the watcher fires only when `~/.config/scheduler/` itself is touched (the parent is watched non-recursively at line 104) and `whim://routines-changed` will never fire in practice. Verify the path against the live scheduler plugin or read it from `scheduler --paths` / similar.

- **`plugins/dust/src-tauri/src/notifications.rs:88-98`** The watcher closure swallows `notify::Result::Err` with `eprintln!` and continues. After a transient FS error the watcher is still alive but may have lost its subscription — there is no liveness check that re-arms it. Acceptable for v0; flag for a follow-up to surface persistent errors via `whim://notification`.

- **`plugins/dust/src-tauri/src/notifications.rs:114-120`** The forward task (`tokio::spawn`) holding `app.emit` is never aborted; if `start_routines_watcher` is called twice (which can happen on hot-reload during development), the previous task leaks. Either (a) store the `JoinHandle` in `AppState` next to `mission_watcher` and abort the old one before spawning the new, or (b) document that `start_routines_watcher` must be called exactly once per app instance.

- **`plugins/dust/src-tauri/src/commit_summary.rs:117-139`** `parse_commit_summary` blindly takes `lines.get(0..3)` for hash/author/date/message and concatenates the rest as `stats`. If `git show --stat` ever emits the commit body across multiple lines (multiline commit message), the body lines after `%s` will be silently treated as stats. Mitigation is small — the format string `%H%n%an%n%ai%n%s` keeps `%s` to the subject line — but flagging in case a reviewer changes the format string later.

- **`plugins/dust/src-tauri/src/notifications.rs:128`** `emit_notification` is gated by `#[allow(dead_code)]` because no surface emits notifications yet. The mission lists this helper as the integration point for "other surfaces (e.g. mission events)"; without a single concrete caller, the FIFO cap path (insert + truncate) has no runtime exercise. Suggest hooking at least one mission-event emitter through `emit_notification` (e.g. `mission::mission_approve_gate` success path) so the code is exercised in dev.

- **`plugins/dust/src/whim/hooks/useGitRepo.ts:64-86`** The fs-changed listener and the initial-fetch effect both call `fetchStatus`/`get_repo_status`, but the initial-fetch effect (`useGitRepo.ts:45-60`) does *not* set `error = null` on success, only on failure. If the very first fetch errors and the second succeeds via the watcher, `error` will linger. Minor — set `setError(null)` next to `setStatus(s)` in the success branch.

- **`plugins/dust/src/whim/components/LiveTopActionBar.tsx:5` / `LiveCommitSummaryCard.tsx:6`** `DEMO_REPO_ROOT = '/Users/joeyhipolito/nanika'` is hard-coded in two places. The mission acknowledges this for v0 (mission.md:81 and the implementer's notes), but the hardcode means the scenes will not work for any other developer. Suggest reading from `list_projects()[0].repo_root` (already exposed by `useRail`) and passing it down as a prop, with the literal as a fallback.

- **clippy warnings (non-blocking):** `cargo clippy` reports 6 style nits — `commit_summary.rs:123` (`lines.get(0)` → `lines.first()`), `notifications.rs:116` (redundant `if let Some(_) = …`), `rail.rs:74` (redundant `&[…]`), plus 3 in pre-existing files (`mission.rs:171`, `lib.rs:803`, `lib.rs:1168`). Worth a one-shot pass with `cargo clippy --fix --lib -p dust-tauri` before the next mission lands.

## Suggestions

- The `Routine` Rust struct (`rail.rs:19-25`) carries `status: String` as the wire field but the value is one of two literal strings (`"enabled"` / `"disabled"`). Promoting to an enum (`#[serde(rename_all = "lowercase")] enum RoutineStatus { Enabled, Disabled }`) would make the surface self-documenting and eliminate the stringly-typed branch in `LiveLeftRail`.
- `useNotifications.ts:46-51` builds the next array with `[payload, ...curr.filter(n => n.id !== payload.id)]` — this is O(n) per arrival. For a 500-entry cap it's negligible, but if the same `id` is emitted twice in quick succession (e.g. a backend retry), the dedup keeps the *new* payload at the front. Confirm that's the desired semantics; the alternative is "first write wins" via `if (curr.some(n => n.id === payload.id)) return curr`.
- `useCommitSummary.ts:32` calls `get_pr_metadata` with `branch: commit` (e.g. literal `"HEAD"`). On a detached HEAD this will fail; on an attached HEAD `gh pr view HEAD` resolves through the current branch. Document the assumption in a one-line comment so a future caller passing a SHA understands why PR lookup may return null.
- The Tauri command names mix snake_case verbs (`git_push`, `git_commit`, `git_stage_all`, `notification_dismiss`) and noun-first patterns (`list_notifications`, `read_pins`). Not blocking, but a future grouping pass that consolidates to one style would help discoverability in `tauri::generate_handler!`.

## What's good

- **Clean separation by concern.** Splitting Phase 1 into four files (`rail.rs`, `git.rs`, `commit_summary.rs`, `notifications.rs`) mirrors the M2-M6 conventions and keeps `lib.rs` from ballooning. The `mod` declarations at `lib.rs:21-25` and the grouped `tauri::generate_handler!` block at `lib.rs:1241-1260` make the new surfaces easy to audit.
- **`WHIM_ALLOW_PUSH` gate is implemented exactly as specified** — the env check is the *first* statement of `git_push` (`git.rs:89`) and runs unconditionally before any subprocess spawn. The error message tells the user how to opt in. This is the right shape for a destructive-action gate.
- **FIFO cap is real, not just declarative.** `notifications.rs:149` inserts at the front, `:152-154` truncates the tail; semantically correct FIFO drop. The 500-entry constant is exposed (`NOTIFICATION_FIFO_CAP`) rather than hard-coded inline, so a future tuning pass is one-line.
- **Async-listen cleanup race is correctly handled in all four hooks.** The `let cancelled = false` / cleanup-flips / `if (cancelled) { f(); return }` pattern at the `.then(f => …)` arm is the M4/M5/M6 carry-over fix, and it is present everywhere it needs to be — no regression. `useGitRepo.ts:81-85` correctly clears the debounce timer in addition to flipping the cancelled flag.
- **Atomic-replace discipline is preserved.** Every state setter produces a new array/object — no `.push()` / `.splice()` / mutation-in-place anywhere in the four hooks. `useNotifications.ts:48-50` and `:73-75` build new arrays even on the listener path.
- **Bundle headroom.** 87 KB gzipped against a 350 KB cap is excellent for a four-surface live-wiring milestone — no inadvertent dependency drag.
- **53 prior scenes survive without a refactor.** The implementer correctly identified that `LeftRail`/`ProjectsTree`/`TopActionBar`/`CommitSummaryCard` were already prop-driven and that a Live-wrapper composition pattern (adapt wire shapes at the boundary) was sufficient. This is the right move — touching 53 scenes to plumb live data would have been a regression risk an order of magnitude larger than the value of the M7 wiring itself.
- **Compression of the dual-channel notification model is correct.** Splitting `whim://notification` (append) from `whim://notification-update` (replace by id) gives the frontend exactly the two semantic operations it needs. The fact that the latter isn't emitted yet (see Blocker 2) is a backend-side gap, not a design flaw.

<!-- scratch -->
M7 Phase 3 review done. Two blockers: (1) git porcelain v2 parser splits on `\t` instead of space — every `get_repo_status` call returns `branch="unknown"` + empty changes; LiveTopActionBar breadcrumb is broken at runtime even though build/lints/tests are green. (2) `whim://notification-update` is consumed by `useNotifications.ts:71` but never emitted by the backend — `notification_dismiss` / `notification_dismiss_all` need to take `app: AppHandle` and emit after persist. Multiple warnings (scheduler subcommand drift, hardcoded DEMO_REPO_ROOT, watcher leak on double-spawn, dead-code `emit_notification` with no caller). All 16 acceptance criteria addressed with file:line evidence. Build/cargo-check/clippy/lints/vitest all clean modulo 6 stylistic clippy nits.

For the implementer fix-up phase:
- Rewrite `parse_git_status` to consume porcelain v2 line-by-line on whitespace; key on line prefix (`# branch.head `, `1 `, `2 `, `?`, `!`, `u`). Add a unit test with a captured fixture.
- Add `app: tauri::AppHandle` arg to `notification_dismiss` + `notification_dismiss_all` and emit `whim://notification-update` after `persist_notifications`.
- Optional: the 6 clippy nits would close out cleanly with `cargo clippy --fix --lib -p dust-tauri`.
<!-- /scratch -->

DECISION: NEEDS-CHANGES. Two blockers must land before merge: (1) porcelain v2 parser fix in `git.rs:142-202`, (2) `whim://notification-update` emit in `notifications.rs` dismiss paths. Build + cargo check + lints + vitest are all clean; the issues are functional gaps that don't surface as CI failures, which is precisely why they need explicit reviewer attention.
