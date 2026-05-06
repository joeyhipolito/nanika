---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260428-3ece4ec1
created_at: "2026-04-28T01:30:00Z"
confidence: high
depends_on:
  - phase-1
  - phase-2
token_estimate: 3500
---

# REVIEW-M2B — TRK-632 Whim Tauri M2b

## Summary

Verdict: **NEEDS-CHANGES**. Phase 2 (rust-shell) is solid and ships all four expected behaviors. Phase 1 (frontend-integration) is partial — the implementer landed only 5 of the 11 prescribed change sets. As a direct consequence:

- `cd plugins/dust && npm run build` exits 2 (~60 TypeScript errors). Root cause: `unplugin-icons/types/react` was added to `tsconfig.app.json:8` but the build script runs bare `tsc &&` (`package.json:8`), which uses `tsconfig.json` — and `tsconfig.json:1-21` has no `types` field. Adding `"types": ["vite/client", "unplugin-icons/types/react"]` to `tsconfig.json` (or switching the build script to `tsc -p tsconfig.app.json`) clears all 54 `~icons/solar/*` TS2307 errors. The remaining 6 errors (RightRail/Tour/TurnDiffInspector unused, RealUsageDemo `import.meta.env`) are pre-existing whim-web bugs that the M2a hermetic copy surfaced but M2a-NOTES did not enumerate; they need surgical fixes in this phase too.
- 6 of 16 acceptance criteria fail outright (build, bundle, tokens import, font @font-face, main.tsx env-flag branch, lint scripts) and 1 is PARTIAL (phase-1 commit count).

Acceptance-criterion verdicts below; blockers / warnings at the end.

## AC1 — 3 phase commits

PARTIAL. `git log --oneline main..HEAD` shows 2 phase commits (`d7879b19 phase-1: …`, `c5fd8187 phase-2: …`). The phase-3 review commit will land when this artifact is committed by the orchestrator — counted as in-flight, not yet on disk.

## AC2 — Scenes.tsx fixes in both files

PASS.
- `plugins/whim-web/src/scenarios/Scenes.tsx` and `plugins/dust/src/whim/scenarios/Scenes.tsx` both contain `<Icon name="Branch" size={14} />` at line 3884.
- `grep -n "ConvItem" plugins/dust/src/whim/scenarios/Scenes.tsx` returns 0 (unused import removed).
- `diff plugins/whim-web/src/scenarios/Scenes.tsx plugins/dust/src/whim/scenarios/Scenes.tsx` exits 0 with empty output (files identical).

## AC3 — `npm run build` exits 0

FAIL. `cd plugins/dust && npm run build` exits 2.

Captured failure summary (`tsc` step):
- 54 × TS2307: `Cannot find module '~icons/solar/*'` at `src/whim/icons/registry.ts:8-66`.
- 1 × TS2339: `Property 'env' does not exist on type 'ImportMeta'` at `src/whim/components/demo/RealUsageDemo.tsx:413`.
- 1 × TS6133: `MODE_ICON' is declared but its value is never read` at `src/whim/components/RightRail.tsx:20`.
- 2 × TS6133: `'lazy' is declared but its value is never read` and `'Suspense' is declared but its value is never read` at `src/whim/components/Tour.tsx:1`.
- 1 × TS6196: `'TourInternal' is declared but never used` at `src/whim/components/Tour.tsx:26`.
- 1 × TS6133: `'onClose' is declared but its value is never read` at `src/whim/components/TurnDiffInspector.tsx:22`.

Root cause for the 54 icon errors: `package.json:8` runs `tsc && vite build`. Bare `tsc` resolves `tsconfig.json`, not `tsconfig.app.json`. `tsconfig.json` (`tsconfig.json:1-21`) does not declare `"types"`, so the `unplugin-icons/types/react` ambient module declarations never enter the program. Manually re-running `npx tsc -p tsconfig.app.json` removes all 54 TS2307 errors (verified during review), confirming the root cause.

The 6 non-icon errors are pre-existing whim-web bugs that were not flagged in `shared/artifacts/whim-tauri-port/M2A-NOTES.md` but the dust `tsc` gate exposes. They need surgical fixes (delete unused declarations; add `vite/client` to types so `ImportMeta.env` resolves). All 6 are concentrated in 4 files.

## AC4 — Bundle ≤ 350 KB gzipped

FAIL. No `plugins/dust/dist/` directory exists because `npm run build` failed at the `tsc` step before reaching `vite build`. Cannot measure `gzip -c plugins/dust/dist/assets/index-*.js | wc -c`.

## AC5 — `unplugin-icons` registered

PASS for the AC text as written (`grep "unplugin-icons" plugins/dust/vite.config.ts plugins/dust/tsconfig.app.json plugins/dust/package.json` returns matches in all three: `vite.config.ts:3` import, `vite.config.ts:7` plugin call, `tsconfig.app.json:8` types entry, `package.json:49` devDependency). However, the integration is functionally broken — see AC3 — because the gate is in `tsconfig.app.json`, not the `tsconfig.json` that `tsc` actually loads at build time.

## AC6 — Tokens reachable from dust globals

FAIL. `grep -E "@import.*whim/styles/tokens.css" plugins/dust/src/globals.css` returns 0 — no `@import` statement was added. The OR clause (`--accent` present in built CSS bundle) cannot be evaluated because no CSS bundle was produced (AC4). Even though `plugins/dust/src/whim/styles/tokens.css:1-27` defines `--accent`, `--accent-2`, `--accent-soft`, `--accent-rim`, no entry-point CSS file imports it; `plugins/dust/src/globals.css` ends after `--diff-gutter-fg` with the existing `*::before` reset rules and never references the whim tokens file.

## AC7 — Fonts vendored OR documented

PARTIAL. Five woff2 files exist at `plugins/dust/src/whim/styles/fonts/` (`inter-tight-400.woff2`, `inter-tight-500.woff2`, `inter-tight-600.woff2`, `jetbrains-mono-400.woff2`, `jetbrains-mono-500.woff2`) — file presence ✓. But `plugins/dust/src/whim/styles/tokens.css` declares `--sans` / `--mono` font-family stacks at lines 25-26 without any `@font-face` block referencing the local woff2 files. Result: the vendored fonts are orphan binaries — never loaded by the browser. Combined with AC6 (tokens.css never imported), the situation is doubly broken.

The fallback documented in the mission (don't enforce `font-src 'self'`; allow `https://fonts.gstatic.com`) was not invoked, so phase 2's CSP at `plugins/dust/src-tauri/tauri.conf.json:29` (`font-src 'self'`) will silently block any fallback `https://fonts.gstatic.com` request the browser tries to make from the existing system-stack fall-through. As shipped, the application will render with system-stack fonts — visually divergent from whim-web.

## AC8 — `main.tsx` branches on env flag

FAIL. `grep "VITE_DUST_LEGACY_LAUNCHER" plugins/dust/src/main.tsx` returns 0. The file (`plugins/dust/src/main.tsx:1-10`) is unchanged from M2a baseline:

```ts
import React from 'react'
import ReactDOM from 'react-dom/client'
import './globals.css'
import { App } from './App'

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
)
```

It imports the legacy launcher (`./App`, the 820×520 Raycast-style UI) by default. The whim canvas at `./whim/App` is never mounted in any build. Phase 2's `tauri.conf.json` window swap to 1280×800 will therefore display the legacy launcher inside an oversized frame — a visible regression at runtime, not a feature.

## AC9 — Lint scripts pass

FAIL. Both scripts are missing:
- `plugins/dust/scripts/lint-scenes-registration.sh` does not exist (`ls plugins/dust/scripts/` shows only `dust-dash.sh`, `dust-shell.sh`, `run-conformance.sh`, `shell-bench.sh`).
- `plugins/dust/scripts/lint-tour-anchors.sh` does not exist.
- `package.json` `scripts` block (lines 6-14) has no `lint:scenes` or `lint:tour` entries.

Cannot run the verification commands.

## AC10 — Tauri window config swapped

PASS. `plugins/dust/src-tauri/tauri.conf.json:13-26` carries all required fields: `"label": "main"`, `"title": "Whim"`, `"width": 1280`, `"height": 800`, `"resizable": true`, `"transparent": false`, `"decorations": true`, `"alwaysOnTop": false`, `"skipTaskbar": false`, `"visible": true`, `"center": true`. `macOSPrivateApi: true` preserved at line 31. Grep returns 6+ matches.

## AC11 — CSP set

PASS. `plugins/dust/src-tauri/tauri.conf.json:29`:

```
"csp": "default-src 'self'; style-src 'self' 'unsafe-inline'; script-src 'self'; font-src 'self'; connect-src 'self' tauri: ipc:; img-src 'self' data:"
```

`grep -E '"csp": ".*default-src' plugins/dust/src-tauri/tauri.conf.json` returns 1. Syntax is valid. Caveat tied to AC7: `font-src 'self'` will block any `fonts.gstatic.com` fallback that a downstream remediation might naïvely re-introduce, so this CSP and AC7 must move together.

## AC12 — ⌥Space behavior changed

PASS. `plugins/dust/src-tauri/src/lib.rs:782-801`:

```rust
.with_handler(move |_app, _shortcut, event| {
    if event.state() != ShortcutState::Pressed { return; }
    let Some(window) = handle.get_webview_window("main") else { return; };
    let visible = window.is_visible().unwrap_or(false);
    if legacy_launcher {
        if visible {
            let _ = window.emit("dust://hide-request", ());
        } else {
            center_on_active_monitor(&handle, &window);
            let _ = window.show();
            let _ = window.set_focus();
        }
    } else if visible {
        window.set_focus().unwrap();
    } else {
        window.show().unwrap();
        window.set_focus().unwrap();
    }
})
```

`legacy_launcher` is read once at startup via `std::env::var("DUST_LEGACY_LAUNCHER").is_ok()` (`lib.rs:734`) — matches the existing env-var pattern (no `cfg(feature)` flags exist in this crate). Default branch (legacy off) calls `show()+set_focus()` on hidden window or `set_focus()` on visible window — exactly what the mission prescribes. Grep `show\(\)|set_focus\(\)` returns 5 matches in the handler block.

## AC13 — Registry retry implemented

PASS. `plugins/dust/src-tauri/src/lib.rs:721-728` defines `is_stale_socket_error()` filtering on `Address already in use`, `EADDRINUSE`, and `File exists` — the exact symptoms the mission called out. `lib.rs:736-759` wraps `Registry::new()` in a single-retry: on stale-socket error, calls `cleanup_runtime_sockets()` and retries once; on retry failure, panics with the original error preserved in the message; on non-stale-socket errors, panics immediately without retry. `cleanup_runtime_sockets()` (`lib.rs:688-719`) mirrors `dust_registry::runtime_dir()` precedence (`$XDG_RUNTIME_DIR/nanika/plugins/` then `~/.alluka/run/plugins/`) and only removes `*.sock` files. Grep `stale|retry|cleanup` returns 8+ matches in the init path. `cargo check -p dust` exits 0.

The "permission-denied is not retried" property is enforced by `is_stale_socket_error` — it does not match `Permission denied`, so any `EACCES` falls into the unconditional `Err(e) =>` arm at `lib.rs:754-757`.

## AC14 — Bench-harness lines preserved

PASS. `grep -cE "\[bench\] " plugins/dust/src/main.tsx plugins/dust/src/App.tsx` returns `src/main.tsx:0` and `src/App.tsx:9`. `git show 43255e4e:plugins/dust/src/{main,App}.tsx | grep -cE "\[bench\]"` returns the same `0` and `9` (M2a baseline before phase 1). No regression.

## AC15 — No edits to scoped-out crates

PASS. `git status --short -- plugins/dust/dust-core plugins/dust/dust-sdk plugins/dust/dust-conformance plugins/dust/dust-registry plugins/dust/dust-dashboard plugins/dust/examples` outputs nothing. `git diff --stat 43255e4e..HEAD -- plugins/dust/dust-*` confirms zero changes to those subtrees across phases 1+2.

## AC16 — REVIEW-M2B.md exists with required structure

PASS once this artifact is committed. File written at `plugins/dust/REVIEW-M2B.md` with one section per acceptance criterion plus `### Blockers` and `### Warnings` H3 sections. Reviewer's chat response starts literally with `### Blockers` (no banned prefatory phrases).

### Blockers

- **`plugins/dust/tsconfig.json:1-21`** `tsc` (default-tsconfig) does not include `"unplugin-icons/types/react"` in its `types` array, so 54 `~icons/solar/*` imports fail TS2307 during `npm run build`. The phase-1 fix landed in `tsconfig.app.json` instead. **Fix:** add `"types": ["vite/client", "unplugin-icons/types/react"]` to `tsconfig.json` (mirroring the `tsconfig.app.json:8` shape), OR change `package.json:8` from `"build": "tsc && vite build"` to `"build": "tsc -p tsconfig.app.json && vite build"`. The latter is preferred — it preserves the M2a `tsconfig.json:14` `"strict": true` semantics without polluting the base config with bundler-only types. **Why:** the build gate must be exit 0 for AC3, AC4, AC6 to even be measurable; right now the entire mission is on hold behind this single config split.

- **`plugins/dust/src/whim/components/Tour.tsx:1`, `:26`** TS6133/TS6196 — `lazy`, `Suspense`, and `TourInternal` are declared but never used. **Fix:** remove the unused symbols from the import on line 1 and delete the dead `TourInternal` declaration block at line 26 (or wire it up if it was intended to be used). **Why:** dust's `noUnusedLocals: true` (`tsconfig.json:15`, `tsconfig.app.json:21`) makes these hard errors, not warnings; `npm run build` cannot exit 0 until they go.

- **`plugins/dust/src/whim/components/RightRail.tsx:20`** TS6133 — `MODE_ICON: Record<RightRailMode, IconName>` declared but never read. **Fix:** delete the constant if it was abandoned in a refactor, or wire it into the JSX render path that needs it. **Why:** same `noUnusedLocals` gate as above.

- **`plugins/dust/src/whim/components/TurnDiffInspector.tsx:22`** TS6133 — `onClose` destructured prop is never used. **Fix:** delete it from the destructuring pattern, or rename to `_onClose` to mark it intentionally unused. The component shape is otherwise fine.

- **`plugins/dust/src/whim/components/demo/RealUsageDemo.tsx:413`** TS2339 — `Property 'env' does not exist on type 'ImportMeta'`. **Fix:** the dust `tsconfig.app.json:8` already includes `"vite/client"` which provides the `ImportMeta.env` augmentation, but `tsc` (using `tsconfig.json`) doesn't. Resolved automatically by the AC3 root-cause fix above (switching to `tsc -p tsconfig.app.json`); no per-file edit needed.

- **`plugins/dust/src/main.tsx:1-10`** Missing `VITE_DUST_LEGACY_LAUNCHER` env-flag branch entirely (AC8). The whim canvas at `./whim/App` is never mounted; the file imports `./App` (legacy launcher) unconditionally. **Fix:** replace the body with the branch shown in the mission "Files to edit" §1.8 (`const useLegacy = import.meta.env.VITE_DUST_LEGACY_LAUNCHER === '1' || import.meta.env.VITE_DUST_LEGACY_LAUNCHER === 'true'` then dynamic-import `./App` vs `./whim/App`). **Why:** without this, phase 2's window swap to 1280×800 ships the legacy 820×520 launcher inside an oversized frame — a regression, not a release. The whole mission objective (mount whim canvas under the new shell) is unmet.

- **`plugins/dust/src/globals.css`, `plugins/dust/src/whim/styles/tokens.css`** Missing `@import './whim/styles/tokens.css';` in `globals.css` (AC6) and missing `@font-face` blocks at the top of `tokens.css` referencing the 5 vendored woff2 files (AC7). **Fix:** append the `@import` line to `globals.css` (after the existing `@tailwind` directives so the cascade order is correct), and prepend 5 `@font-face` blocks to `tokens.css` of the form `@font-face { font-family: 'Inter Tight'; src: url('./fonts/inter-tight-400.woff2') format('woff2'); font-weight: 400; font-style: normal; font-display: swap; }` — one per weight. **Why:** the woff2 files are otherwise orphan binaries and `--sans`/`--mono` declarations resolve to system-stack fallbacks at runtime. The CSP `font-src 'self'` already aligns with self-hosted fonts; no CSP edit needed once the @font-face blocks land.

- **`plugins/dust/scripts/`, `plugins/dust/package.json:6-14`** Missing `lint-scenes-registration.sh` + `lint-tour-anchors.sh` scripts and missing `lint:scenes` / `lint:tour` package.json `scripts` entries (AC9). **Fix:** write the two `set -euo pipefail` shell scripts per the mission's PHASE: frontend-integration §9 spec (grep export-from-Scenes-vs-SCENE_COMPONENTS map; grep `data-tour-anchor` literals across `src/whim/components/**` vs `spotlightTarget` references in `src/whim/tour/steps.ts`); `chmod +x` both; add `"lint:scenes": "scripts/lint-scenes-registration.sh"` and `"lint:tour": "scripts/lint-tour-anchors.sh"` to package.json scripts. **Why:** these enforce risks #12 (Scenes registration drift) and #13 (tour-anchor drift) — they're cheap, and the mission marked them as gating.

### Warnings

- **`plugins/dust/src-tauri/src/lib.rs:750`** `panic!("cannot start without registry: {e}")` on retry failure. The mission asked the original error to "still surface" — here it surfaces only inside a panic message string, not as a `Result::Err` returned to the caller. Acceptable for a startup hook (`#[cfg_attr(mobile, tauri::mobile_entry_point)] pub fn run()` returns `()` and panic is the established failure mode for setup), but worth noting: any future caller that wants to recover from a registry-init failure programmatically will have to refactor. Consider returning `Result<Registry, RegistryError>` from a thin helper and only panicking inside `run()`.

- **`plugins/dust/src-tauri/src/lib.rs:739`** The retry path matches `e if is_stale_socket_error(&e)` and proceeds to call `cleanup_runtime_sockets()` unconditionally. `cleanup_runtime_sockets()` is best-effort and silently swallows `read_dir` errors (`lib.rs:704-711` — only `eprintln!`). If the cleanup itself fails (e.g. read-only `/run` mount, EACCES on the directory), the second `Registry::new()` call may still observe the same stale socket, panic, and the operator gets two rounds of `EADDRINUSE`-shaped logging. Low-impact because the original error is preserved in the panic, but a single sentence in the inner `Err(e2)` log line ("retry: cleanup may have failed; original socket error stands") would help triage.

- **`plugins/dust/src/whim/tour/steps.ts`** Contains the word `amber` (slice-5b carry-over). The mission flagged this as out of scope — noted here only so a future cleanup sweep does not surprise anyone.

- **`plugins/dust/tsconfig.app.json:8`** Now lists `"unplugin-icons/types/react"` correctly, but the file is also configured with `"verbatimModuleSyntax": true` and `"erasableSyntaxOnly": true` — neither is wrong, but they make every type-only import explicit and reject any non-erasable syntax. New code in `src/whim/` that copies idioms from `plugins/whim-web/` (where these flags are off) may surprise the next implementer with import-shape errors. Worth a one-line `CLAUDE.md` note.

- **`plugins/dust/src-tauri/tauri.conf.json:29`** CSP omits `worker-src`, `child-src`, and `frame-src`. Tauri 2 webviews don't typically use those, but if a future feature embeds an iframe (e.g. an in-app docs viewer) the CSP will block it without a clear error message. Not a blocker — call it out in a follow-up doc.

### Suggestions

- **`plugins/dust/package.json:8`** Prefer `"build": "tsc -p tsconfig.app.json && vite build"` over duplicating `types` in the base `tsconfig.json`. Keeps bundler-only types (`vite/client`, `unplugin-icons/types/react`) out of the broader `tsc` graph that `dust-core` and other crates' tooling may consume later.
- **`plugins/dust/src-tauri/src/lib.rs:736-759`** Consider extracting the `Registry::new()` retry block into a `try_init_registry()` helper returning `Result` — keeps `run()` readable and makes the retry logic unit-testable independently of Tauri startup.
- **`plugins/dust/src/whim/styles/tokens.css`** When the `@font-face` blocks land (per AC7 fix), declare them with `font-display: swap` so the system fallback renders during the first paint instead of leaving the user looking at FOIT — important for ⌥Space activations where perceived latency dominates the UX.

## What's Good

- Phase 2's `Registry::new()` retry path is a textbook implementation of risk #11: tight error filter (`is_stale_socket_error` only matches the three named symptoms), single-retry contract, original-error preservation in the panic message, cleanup mirrors `dust_registry::runtime_dir()` exactly. `cargo check -p dust` exits 0.
- Phase 2's ⌥Space handler refactor preserves the legacy collapse-mode launcher behind a runtime env var (`DUST_LEGACY_LAUNCHER`) read once at startup — matches the existing `std::env::var` pattern in the same file rather than introducing a new feature-flag system. This is the right level of abstraction.
- Phase 2's `tauri.conf.json` window swap is precise and complete: every prescribed property is set, `macOSPrivateApi: true` and `visible: true` are deliberately preserved, the title rename `"Dust" → "Whim"` lands cleanly.
- Phase 1's Scenes.tsx surgical fixes are byte-perfect across both `whim-web` and `dust` copies — `diff` empty.
- Phase 1's icon-dependency additions (`unplugin-icons`, `@iconify-json/solar`, `@svgr/core`, `@svgr/plugin-jsx`) are pinned to the same caret ranges already in use in `whim-web` — no version skew.
- Five vendored woff2 files are present at the right path (`src/whim/styles/fonts/`) at reasonable sizes (≈44 KB Inter Tight, ≈31 KB JetBrains Mono) — ready to be wired up once AC7 is fixed.
- `cargo check -p dust` exits 0; phase 2 introduces no Rust-side regressions.
- Bench-harness lines preserved exactly (risk #14): pre and post counts match in both `main.tsx` and `App.tsx`.

<!-- scratch -->
phase-3 review summary for downstream:

phase-1 missed 6 of 11 prescribed change sets. fix order to unblock the build:
1. plugins/dust/tsconfig.json — add `"types": ["vite/client", "unplugin-icons/types/react"]` (or switch package.json:8 build script to `tsc -p tsconfig.app.json && vite build`). this clears 54 of 60 errors.
2. delete unused: Tour.tsx:1 `lazy`, `Suspense`; Tour.tsx:26 `TourInternal`; RightRail.tsx:20 `MODE_ICON`; TurnDiffInspector.tsx:22 `onClose`. (5 errors)
3. RealUsageDemo.tsx:413 ImportMeta.env — auto-resolved by step 1's `vite/client` types entry.

once build passes, follow-up:
4. main.tsx — add VITE_DUST_LEGACY_LAUNCHER env-flag branch + dynamic import `./whim/App` default, `./App` legacy.
5. tokens.css — prepend 5 @font-face blocks (one per weight) referencing `./fonts/{inter-tight,jetbrains-mono}-{400,500,600}.woff2`.
6. globals.css — append `@import './whim/styles/tokens.css';` after @tailwind directives.
7. write scripts/lint-scenes-registration.sh + scripts/lint-tour-anchors.sh, chmod +x, add lint:scenes + lint:tour to package.json scripts.

phase-2 is approved as-is. no rework needed there.

bundle size + dist-CSS verification deferred until build exits 0.
<!-- /scratch -->
