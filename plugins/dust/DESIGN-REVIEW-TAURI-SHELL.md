---
produced_by: staff-code-reviewer
phase: phase-7
workspace: 20260420-8fe44c74
created_at: "2026-04-20T03:45:00Z"
confidence: high
depends_on:
  - phase-6
token_estimate: 2800
---

# Design Review — Tauri Shell

**Scope:** `plugins/dust/src-tauri/**`, `plugins/dust/src/**`, `plugins/dust/scripts/`
**Diff base:** HEAD~4 (covers implement-summon-and-hide, implement-search-loop, implement-quick-edit, implement-size-morphing, benchmark-perceived-latency)

---

## Summary

The Tauri shell lands a Raycast-style launcher with three window modes (default / expanded / collapsed), global ⌥Space hotkey, debounced fuzzy search, a CodeMirror 6 inline editor activated from FileRef chips, and a benchmark harness for four latency buckets. The architecture is clean and well-scoped; no extraneous features were introduced. One **blocking** security gap exists in the file I/O commands, and two **warnings** surface in the React async lifecycle. Everything else passes.

---

## Checklist

### (a) Tauri Plugin Usage

| Check | Result | Notes |
|---|---|---|
| No `unsafe` blocks | ✅ PASS | Zero `unsafe` in `lib.rs` |
| Hotkey cleaned up on quit | ✅ PASS | `tauri-plugin-global-shortcut` 2.x owns the OS registration and tears it down on `AppHandle` drop; no manual cleanup needed |
| No leaked window handles | ✅ PASS | Window obtained via `handle.get_webview_window("main")` at call-site; not stored statically |
| `write_file` path validation | ❌ FAIL — **BLOCKER** | See finding B-1 below |
| `read_file` path validation | ❌ FAIL — **BLOCKER** | Same root cause as B-1 |
| Multiple ⌥Space presses do not spawn extra windows | ✅ PASS | Handler checks `window.is_visible()` on each press; only one "main" window exists |

### (b) React State Correctness

| Check | Result | Notes |
|---|---|---|
| No unnecessary re-renders on keystroke | ✅ PASS | `SearchBar` re-renders with new `value` — correct and unavoidable; no wasted renders downstream |
| Stale-closure in keyboard handler | ✅ PASS | `handleKeyDown` deps list is correct; `windowModeRef` used for mutable reads, `transitionTo` is stable |
| Effect cleanup on unmount | ✅ PASS | All three effects return cleanup: Tauri listener unlisten array, `removeEventListener`, `clearTimeout` |
| `EditorView` disposed on close | ⚠️ WARN | Race condition — see finding W-1 |
| `onClose` stale closure in keymap | ⚠️ WARN | See finding W-2 |

### (c) Window State-Machine Correctness

| Check | Result | Notes |
|---|---|---|
| No unreachable transitions | ✅ PASS | All six directed edges (default↔expanded, default↔collapsed, expanded→default, collapsed→default) are reachable |
| Esc always exits | ✅ PASS | Global handler: expanded/collapsed → default; `handleKeyDown` Escape: default → hide |
| Multiple ⌥Space presses do not spawn extra windows | ✅ PASS | See (a) above; `hideWindow` guard also blocks double-fire during animation |
| QuickEdit cannot open on non-FileRef | ✅ PASS | `handleOpenFile` only wired to `RenderFileRef` — structural enforcement |

### (d) Scope Discipline

| Check | Result | Notes |
|---|---|---|
| No tab bar | ✅ PASS | No `TabBar` component anywhere in `src/` |
| No second concurrent buffer | ✅ PASS | Single `editingFile` state slot; opening a second FileRef replaces the first |
| No file picker | ✅ PASS | Files reachable only via FileRef chips emitted by plugins |
| No LSP client | ✅ PASS | No LSP imports; `@codemirror/lang-*` provides syntax only |

### (e) TUI Non-Regression

| Check | Result | Notes |
|---|---|---|
| `dust-dashboard` Cargo deps unchanged | ✅ PASS | `dust-core` and `dust-registry` not in the diff; only `src-tauri/` and frontend files changed |
| `dust-dash.sh --no-build` likely unaffected | ✅ PASS | Script has no dependency on the Tauri crate; TUI sockets and plugin discovery path unchanged |
| `cargo build --release -p dust-dashboard` | ⚠️ NOT RUN | Review phase cannot execute builds; confirmed no shared source touched |

### (f) Latency Budget (phase-6 benchmark results)

| Metric | p95 | Budget | Result |
|---|---|---|---|
| (a) hotkey → pane-visible | 87 ms | 120 ms | ✅ PASS |
| (b) keystroke → results | 57 ms | 50 ms | ❌ FAIL |
| (c) Enter → component render | 154 ms | 250 ms | ✅ PASS |
| (d) ⌘⇧E → editor ready | 148 ms | 150 ms | ✅ PASS |

Bucket (b) misses by 7 ms. Root cause: 40 ms debounce at `App.tsx:124` consumes 80% of the 50 ms budget before IPC begins. Fix: change `40` → `10`.

---

## Blockers

### B-1 — `read_file` / `write_file` accept arbitrary filesystem paths (lib.rs:147–164)

```rust
#[tauri::command]
async fn read_file(path: String) -> Result<String, String> {
    std::fs::read_to_string(&path).map_err(|e| e.to_string())
}

#[tauri::command]
async fn write_file(path: String, content: String, app: tauri::AppHandle) -> Result<(), String> {
    let tmp = format!("{}.tmp", path);
    fs::write(&tmp, &content).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    ...
}
```

`path` is a raw string from the frontend — no canonicalization, no prefix check. A malicious or buggy plugin can supply `../../.ssh/authorized_keys` or any absolute path. The task spec explicitly requires "no traversal outside cwd or a user-approved root."

**Fix:** Canonicalize the path and reject anything outside an allowed prefix. Minimal implementation:

```rust
fn validate_path(raw: &str) -> Result<std::path::PathBuf, String> {
    let p = std::path::PathBuf::from(raw);
    let canonical = std::fs::canonicalize(&p)
        .or_else(|_| {
            // file doesn't exist yet — canonicalize parent
            p.parent()
                .ok_or_else(|| "no parent".to_string())
                .and_then(|par| std::fs::canonicalize(par).map_err(|e| e.to_string()))
                .map(|par| par.join(p.file_name().unwrap_or_default()))
        })
        .map_err(|e| e.to_string())?;
    let home = std::env::var("HOME").unwrap_or_default();
    if !canonical.starts_with(&home) {
        return Err(format!("path outside allowed root: {}", canonical.display()));
    }
    Ok(canonical)
}
```

Apply in both `read_file` and `write_file`, using the canonicalized path for all I/O operations.

---

## Warnings

### W-1 — `EditorView` orphan race in `QuickEditor` (QuickEditor.tsx:70–165)

`init()` checks `destroyed` after each `await` but the `view` variable is assigned after the final await. If the cleanup function runs (setting `destroyed = true`) between the last `if (destroyed) return` guard and `view = new EditorView(...)`, the cleanup sees `view === null` and returns without destroying it. The `init()` continuation then assigns an `EditorView` that is never cleaned up.

This is unlikely in practice (the gap is a single synchronous statement following two async calls), but it is a real race under aggressive unmount scenarios (e.g., Esc pressed before file load completes).

**Fix:** Check `destroyed` immediately before mounting:

```typescript
view = new EditorView({ state, parent: container ?? undefined })
if (destroyed) { view.destroy(); return }
```

### W-2 — `onClose` captured stale in `saveKeymap` (QuickEditor.tsx:86–103)

`saveKeymap` is created once inside `init()` at mount time. The `useEffect` dependency array is `[path]`, so if the parent re-renders and passes a new `onClose` reference, the keymap still holds the old closure. In the current codebase `handleCloseEditor` has `[]` deps and is stable, so this does not fire. But it is fragile — if deps change the Esc/Save keymap silently drifts.

**Fix:** Either add `onClose` to the `useEffect` dep array (which re-creates the editor on prop change) or extract the keymap into a ref:

```typescript
const onCloseRef = useRef(onClose)
useEffect(() => { onCloseRef.current = onClose }, [onClose])
// inside saveKeymap:
run() { onCloseRef.current(); return true }
```

---

## Suggestions

### S-1 — `get_plugin_info` scans all plugins on each call (lib.rs:85–110)

`get_plugin_info` calls `registry.search_with_ids("")` (returns all plugins) then finds the matching entry by ID. This is O(n) on every Enter keypress. For the current plugin count it is imperceptible, but a simple lookup would be cheaper.

### S-2 — Debounce 40 → 10 ms (App.tsx:124)

Required to pass the bucket (b) latency budget (currently ❌ FAIL at 57 ms p95 vs 50 ms budget).

---

## What's Good

- **Atomic writes**: `write_file` writes to `{path}.tmp` then renames — correct for crash safety and avoids partial reads.
- **Race guard on render_ui**: `detailLoadId` ref correctly cancels stale loads when Enter is pressed rapidly.
- **Destroyed flag pattern**: `QuickEditor` checks `destroyed` after each await, preventing use of unmounted DOM. The approach is correct — W-1 is a hairline gap in the final assignment.
- **Ref-mirrored state for event listeners**: `windowModeRef` mirrors `windowMode` state so all listener closures see current mode without re-registration. Clean.
- **State machine coverage**: All six window-mode transitions are reachable and tested; Esc behavior is well-separated between global and per-mode handlers.
- **Scope discipline**: No tab bar, file picker, LSP, or second buffer introduced despite the significant feature surface.

FINDING: write_file and read_file in lib.rs:147-164 accept arbitrary filesystem paths — no path canonicalization or allowed-root check. This is a BLOCKER per the task spec.
FINDING: QuickEditor has an EditorView orphan race: cleanup can run (view still null) while init() is between its last destroyed-check and the `view = new EditorView(...)` assignment.
PATTERN: windowModeRef mirrors windowMode state so event-listener closures never capture stale window mode — eliminates a whole class of stale-closure bugs in Tauri window management.
GOTCHA: 40ms debounce in App.tsx consumes 80% of the 50ms keystroke→results budget. Bucket (b) fails at 57ms p95. Fix is one-line: change 40 → 10.
