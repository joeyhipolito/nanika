---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260428-c2b222ab
created_at: "2026-04-28T01:00:00Z"
confidence: high
depends_on:
  [types-and-hook, scenario-wiring]
token_estimate: 2400
---

# REVIEW-M3 — Whim Tauri M3 (chat surface → dust IPC)

## Summary

Phase 1 (types + `useChat` hook + Rust wrapper decision) is solid: atomic-replace and stale-thread guarding are implemented correctly, the type layer is the canonical source of `ConvItem`/`Component`/`ChatEvent`, and `MOCK_CONVERSATION` is genuinely removed from the mocks file (only inline reuse inside `Scenes.tsx` for `DocumentModeScene`).

Phase 2 ships `LiveCompositeCanvas.tsx` and a working composer textarea wired through `onComposerSubmit`, but **the live scene is never registered**. Neither `SCENE_COMPONENTS` nor `SCENARIO_LIST` in `App.tsx` mention `live-composite-canvas` or import `LiveCompositeCanvas`. The route `#/s/live-composite-canvas` cannot be reached, so AC7 (live scene boots) and AC11 (SCENARIO_LIST length 48) both fail. This is a blocker — the mission's Phase 2 spec lists App registration as required work.

Verdict: NEEDS-CHANGES. Ship blockers below.

## Acceptance criteria

### 1. Phase commits land — PASS (advisory)

`git log --oneline main..HEAD`:

- `88764b87 phase scenario-wiring: …`
- `b04524db phase types-and-hook: …`

Two phase commits at review time; the review commit lands as the third. Mission AC requires `≥ 3` so this is satisfied at merge time.

### 2. Build clean + bundle ≤ 350 KB — PASS

`cd plugins/dust && npm run build` exits 0 (after `npm install`).

`gzip -c plugins/dust/dist/assets/*.js | wc -c` → `123448` bytes (~123 KB), well under the 350 KB ceiling. Per-asset gzip from build output: `App-*.js` 77.35 KB, `index-*.js` 46.27 KB.

### 3. `useChat` hook exists — PASS

`plugins/dust/src/whim/hooks/useChat.ts:23` — `export function useChat(threadId?: string): UseChatReturn`. `UseChatReturn` at lines 8-17 exposes `conversation`, `threads`, `activeThreadId`, `setActiveThreadId`, `ask`, `newThread`, `loading`, `error`.

### 4. `types.ts` exists with `ConvItem` and `Component` — PASS

`plugins/dust/src/whim/types.ts:9` — `export type { Component } from '../types'`.
`plugins/dust/src/whim/types.ts:41-46` — `export type ConvItem = …` (4-arm discriminated union: `user`, `tool-beat`, `doc-turn`, `commit-summary`).
Also defines `ChatEvent` at `:30-34`, `ThreadMeta` at `:13-18`, `StoredMessage` at `:20-26`, plus `unwrapData<T>` helper.

### 5. `MOCK_CONVERSATION` deleted from mocks/conversation.ts — PASS

`plugins/dust/src/whim/mocks/conversation.ts` is two stub comments only:

```
// chat conversation now sourced from dust IPC; see ../hooks/useChat.ts
// ConvItem type has moved to ../types.ts
```

`grep -c "MOCK_CONVERSATION" plugins/dust/src/whim/mocks/conversation.ts` → `0`. The const is genuinely deleted, not renamed. (Note: the const is *inlined* into `Scenes.tsx:45-73` for `DocumentModeScene` use, with a comment explaining why — this is acceptable; the AC scope is the mocks file.)

### 6. `ConvItem` imports rewritten — PASS

`grep -rln "from.*mocks/conversation" plugins/dust/src/whim` returns `types.ts` and `Scenes.tsx`, but inspection shows neither contains an `import` statement — only a comment in each (`types.ts:37`, `Scenes.tsx:43`) and an unrelated string literal (`Scenes.tsx:64`, `:3203`). All real `ConvItem` imports route through `../types` (e.g., `CompositeCanvas.tsx:18` — `import type { ConvItem } from '../types'`).

### 7. Live scene boots — FAIL (BLOCKER)

`LiveCompositeCanvas.tsx` exists at `plugins/dust/src/whim/components/LiveCompositeCanvas.tsx:36` and is correctly authored: it calls `useChat(threadId)`, maps `Component[] → ConvItem[]` via `componentToConvItem` (`:16-28`), merges into `stateDefault` with `liveConversation` override (`:47-57`), and renders `<CompositeCanvas state={liveState} onComposerSubmit={chat.ask} />`. The composer textarea in `CompositeCanvas.tsx:822-842` is wired to `onComposerSubmit` via `handleComposerKeyDown` at `:384-393`.

But the scene is never registered:

- `grep -n "LiveCompositeCanvas\|live-composite-canvas" plugins/dust/src/whim/App.tsx plugins/dust/src/whim/scenarios/Scenes.tsx plugins/dust/src/whim/scenarios/index.ts` → **0 matches**.
- `App.tsx:54-102` (`SCENE_COMPONENTS` map) does not include `'live-composite-canvas': LiveCompositeCanvasScene`.
- `App.tsx:104-151` (`SCENARIO_LIST`) does not include the `{ id: 'live-composite-canvas', … }` entry.
- `Scenes.tsx` does not export a `LiveCompositeCanvasScene` wrapper.

Visiting `#/s/live-composite-canvas` therefore falls through `parseHash` to "scene not found." The code-level wiring inside `LiveCompositeCanvas.tsx` is correct but unreachable. Mission Phase 2 explicitly lists this registration as required work.

### 8. Atomic-replace semantics — PASS

`plugins/dust/src/whim/hooks/useChat.ts:78-81`:

```ts
if (payload.event_type === 'data_updated') {
  // Atomic replace — never append. Backend sends the full Component[].
  setConversation(payload.data as Component[])
  setLoading(false)
}
```

The handler calls `setConversation(payload.data as Component[])` — the entire array is replaced, never spread or appended. Matches the `App.tsx:665` legacy pattern and the DUST-AUDIT §7.4 contract.

### 9. Active-thread ref pattern — PASS

`useChat.ts:32` declares `activeThreadIdRef = useRef<string | null>(threadId ?? null)`. The stale-thread guard at `:62-69`:

```ts
const subscribed = activeThreadIdRef.current
if (
  payload.thread_id !== null &&
  subscribed !== null &&
  payload.thread_id !== subscribed
) {
  return
}
```

Events from prior subscriptions whose `thread_id` does not match the current ref are dropped. `setActiveThreadId` (`:111-146`) updates `activeThreadIdRef.current` synchronously *before* dispatching the async unsubscribe/subscribe chain, so no events from the old thread can leak through. `newThread` (`:176-196`) clears the ref to `null` so the first server-assigned `thread_id` pins the active thread (`:73-76`). Pattern matches `App.tsx:665` exactly.

### 10. `chat_load_thread` wrapper landed-or-skipped — PASS (skipped, documented)

`grep "fn chat_load_thread" plugins/dust/src-tauri/src/lib.rs` → 0 matches. Decision documented in two places:

- `plugins/dust/src/whim/hooks/useChat.ts:19-21` — comment block: "skipping the Rust wrapper — `dispatch_action(chat, list_messages)` returns `StoredMessage[]` which we map to `Component[]` here."
- `senior-frontend-engineer-phase-1/phase-1-summary.md:26` — "DECISION: Skipped `chat_load_thread` Rust wrapper. Adding a dedicated Tauri command would duplicate logic that's already expressible from the frontend."

`useChat.ts:121-127` calls `dispatch_action(chat, ask, list_messages, { thread_id })` directly. The IPC contract permits skipping the wrapper. PASS.

### 11. Existing scenes preserved + 1 new — FAIL (BLOCKER)

Required: `SCENARIO_LIST.length === 48`. Actual: `46`.

`awk '/^export const SCENARIO_LIST/,/^]/' plugins/dust/src/whim/App.tsx | grep -cE '^\s*\{ id:'` → `46`.

Two failures rolled into one criterion:

1. The mission's "47 prior" count appears to refer to `SCENE_COMPONENTS` (47 entries via `grep -E "Scene" | wc -l` on the map), not `SCENARIO_LIST` (46 entries). `right-rail` is a pre-existing `SCENE_COMPONENTS` entry with no matching `SCENARIO_LIST` row (`/usr/bin/diff` between extracted keys shows `'right-rail'` only on the map side). This is **pre-existing drift**, not introduced by M3 — but it means the mission's expected baseline of 47 was off by one.
2. M3 itself adds nothing to either map. The `live-composite-canvas` entry that AC11 requires (and that mission Phase 2 specs explicitly) is missing.

Visiting `#/s/composite-canvas`, `#/s/composite-canvas-streaming`, `#/s/composite-canvas-mission`, `#/s/composite-canvas-review-gate` still resolves correctly — `Scenes.tsx:3376,3448,3480,3654` mount `<CompositeCanvas state={state} />` with no `liveConversation` override, so they fall back to `FIXTURES[state.conversationFixture]` at `CompositeCanvas.tsx:381`. No regressions on the existing chat-driven canvas scenes.

Fix: add `'live-composite-canvas': LiveCompositeCanvasScene` to `SCENE_COMPONENTS`, append a `SCENARIO_LIST` entry, and export a `LiveCompositeCanvasScene` wrapper from `Scenes.tsx`. The pre-existing `right-rail` SCENARIO_LIST gap is out of scope for M3 but worth noting.

### 12. Tour + real-usage demo unaffected — PASS

`grep -n "useChat\|chat_subscribe\|liveConversation\|onComposerSubmit" plugins/dust/src/whim/components/tour/TourScene.tsx plugins/dust/src/whim/components/demo/RealUsageDemo.tsx` → 0 matches. Both scenes operate purely on mock canvas states from `mocks/canvasStates.ts` and `mocks/events.ts`. Neither imports the new live-chat plumbing, so they cannot regress on M3 changes. The 26 tour steps and 30 real-usage steps remain mock-driven, exactly as required.

### 13. Lints pass — PASS

```
$ bash plugins/dust/scripts/lint-scenes-registration.sh
lint:scenes: ok (47 scenes registered)
$ bash plugins/dust/scripts/lint-tour-anchors.sh
lint:tour: ok (26 anchors referenced, all resolve)
```

Both exit 0. Note that `lint-scenes-registration.sh` only validates `Scenes.tsx ↔ SCENE_COMPONENTS` parity — it does *not* validate `SCENE_COMPONENTS ↔ SCENARIO_LIST` parity, which is why the missing `live-composite-canvas` registration and the pre-existing `right-rail` gap both go undetected. See Warnings.

### 14. No regressions on legacy launcher — PASS

`VITE_DUST_LEGACY_LAUNCHER=1 npm run build` exits 0. Bundle output:

```
dist/assets/App-CWmCmHlm.js                    341.87 kB │ gzip: 108.53 kB
dist/assets/index-ColuyIot.js                  143.76 kB │ gzip:  46.28 kB
…
✓ built in 4.09s
```

The legacy launcher path still compiles without TypeScript or Vite errors.

### 15. `REVIEW-M3.md` exists — PASS

This file at `plugins/dust/REVIEW-M3.md`. Sections cover criteria 1–14, plus `### Blockers` and `### Warnings` H3 below.

### Blockers

- **`plugins/dust/src/whim/App.tsx:54`** Register `'live-composite-canvas': LiveCompositeCanvasScene` in `SCENE_COMPONENTS`.
  Why: AC7 requires `#/s/live-composite-canvas` to mount `<LiveCompositeCanvas />`; the route currently falls through to "scene not found" because the map has no entry. The component is fully wired internally — only the registration is missing.
  Fix:
  1. In `Scenes.tsx`, add `export function LiveCompositeCanvasScene() { return <LiveCompositeCanvas /> }` (import `LiveCompositeCanvas` from `../components/LiveCompositeCanvas`).
  2. In `App.tsx` import block at `:2-50`, add `LiveCompositeCanvasScene` to the destructured names from `./scenarios/Scenes`.
  3. In `SCENE_COMPONENTS` (`App.tsx:54-102`), append `'live-composite-canvas': LiveCompositeCanvasScene,` after `'real-usage-demo'`.
  4. In `SCENARIO_LIST` (`App.tsx:104-151`), append `{ id: 'live-composite-canvas', ref: '§M3', title: 'Live Composite Canvas (real chat)', desc: 'Wired to dust chat plugin via dispatch_action + chat_subscribe · type a prompt and press Enter to send' }`.

- **`plugins/dust/src/whim/App.tsx:104`** `SCENARIO_LIST.length === 46`, not 48 as AC11 requires. After the registration above, length will be 47. The remaining gap is a pre-existing `right-rail` mismatch — `'right-rail': RightRailScene` exists in `SCENE_COMPONENTS` (`App.tsx:73`) but has no corresponding `SCENARIO_LIST` row. This was not introduced by M3, but AC11 names the literal count `48`.
  Why: AC11 verifies `SCENARIO_LIST.length` directly. Without both fixes, the count is 47 at most.
  Fix: After the live-scene registration above, also add a `SCENARIO_LIST` entry for `right-rail` to match `SCENE_COMPONENTS:73`. Suggested: `{ id: 'right-rail', ref: '§S2.X', title: 'Right Rail', desc: 'Right-rail container with mode tab strip and content panes' }`. If the architect intended `right-rail` to be hidden from the index, document the exclusion and revise AC11 to `47`.

### Warnings

- **`plugins/dust/scripts/lint-scenes-registration.sh:42`** The lint script only walks `SCENE_COMPONENTS`; it does not enforce `SCENE_COMPONENTS ↔ SCENARIO_LIST` parity. That is why both the missing `live-composite-canvas` SCENARIO_LIST entry and the pre-existing `right-rail` gap escape detection. Consider extending the script to require parity in both directions — would have caught this regression at lint time.

- **`plugins/dust/src/whim/components/LiveCompositeCanvas.tsx:36`** The component receives `threadId?` but never calls `chat.setActiveThreadId(threadId)` after mount in the case where the prop changes. Currently `useChat` handles the initial-mount subscribe via `useEffect(…, [])` at `useChat.ts:101-105`, but if a parent ever re-mounts `<LiveCompositeCanvas threadId={…} />` with a different value via prop change rather than a new key, the new threadId would be ignored. Not a blocker for the current scene-registration use case (no `threadId` prop is passed), but worth flagging for thread-rail integration in M7.

- **`plugins/dust/src/whim/hooks/useChat.ts:54-97`** The persistent listener effect uses `// eslint-disable-line react-hooks/exhaustive-deps` to suppress the empty-deps warning. The pattern is correct (single listener for the component lifetime, with `activeThreadIdRef` as the synchronous source of truth), but the eslint-disable obscures the intentionality. Consider replacing with a brief comment explaining why empty deps is correct here, plus an inline `// eslint-disable-next-line` only over the closing paren of the dep array — keeps the suppression closer to the lint signal.

- **`plugins/dust/src/whim/components/LiveCompositeCanvas.tsx:22`** `JSON.stringify(c.result, null, 2)` is called inline in `componentToConvItem` and runs on every conversation re-render (memoized only at `useMemo<ConvItem[]>` boundary). For tool calls with large payloads this allocates work on every `data_updated` event. Consider memoizing per-component or letting the `ToolBeat` component handle JSON formatting lazily.

- **`plugins/dust/src/whim/components/CompositeCanvas.tsx:822-842`** The composer textarea uses uncontrolled `useRef`-based reads. This means the parent `LiveCompositeCanvas` cannot clear the textarea programmatically (e.g., on submit failure or when an external thread switch should reset draft state). Phase 2 cleared on Enter via `composerRef.current.value = ''` (line 390), which works but means draft state is invisible to React. Acceptable for M3, but if M7 needs draft persistence per thread or external clears, this becomes a blocker.

- **`plugins/dust/src/whim/scenarios/Scenes.tsx:45-73`** The inlined `MOCK_CONVERSATION` const for `DocumentModeScene` has no consumer outside this file. Fine, but a `// eslint-disable-next-line @typescript-eslint/no-unused-vars` may be needed depending on the eslint config — the build passes so this is informational.

### Suggestions

- Mission AC11 says "47 prior + live-composite-canvas = 48". The actual prior count of `SCENARIO_LIST` is 46 (pre-existing). When the architect drafts M4-M7 missions, recount from current `SCENARIO_LIST.length` rather than mirroring the M3 number, and consider closing the `right-rail` SCENARIO_LIST gap explicitly.

- The `componentToConvItem` mapper at `LiveCompositeCanvas.tsx:16-28` returns `null` for unknown component types and silently drops them via `flatMap`. For debugging during M4-M7 wire-up, consider a `console.warn` on unknown types behind a `VITE_DUST_DEBUG` env check — would surface backend/frontend protocol drift early.

- `useChat.ts:142-144` collapses error from `setActiveThreadId` into `setError(String(err))`. Consider `setError(err instanceof Error ? err.message : String(err))` for cleaner messages — same pattern as the `data_updated` handler at `:83-87`.

### What's Good

- **Atomic-replace and stale-thread guard are textbook.** `useChat.ts:78-81` (atomic replace) and `:62-76` (ref-synchronized stale-event drop + first-event thread pinning) match the `App.tsx:665` reference pattern exactly. The first-delta-pins-thread idiom for `newThread` is subtle and correctly implemented.

- **The non-breaking `liveConversation` override.** `CompositeCanvas.tsx:67-68,381` adds `liveConversation?: ConvItem[]` to the state and resolves with `state.liveConversation ?? FIXTURES[state.conversationFixture]`. Zero changes required to the 47 existing scene fixtures — surgical and reversible.

- **Optional `onComposerSubmit` keeps mocks silent.** `CompositeCanvas.tsx:825,835` makes the textarea render with no placeholder text and `var(--faint)` color when the prop is absent. Mock-driven scenes are visually unchanged; only the live scene gets a functional textarea.

- **Type layer consolidation.** `types.ts` is the single source of truth for `ConvItem`, `Component`, `ChatEvent`, `ThreadMeta`, `StoredMessage`, and the `unwrapData` helper. The re-export of `Component` from `../types` keeps the whim layer from reaching two levels up — clean module boundary.

- **Documented decisions over silent skips.** The `chat_load_thread` skip is called out in both the source comment (`useChat.ts:19-21`) and the phase summary, and the cleanup behavior of `chat_unsubscribe` on unmount (`useChat.ts:93-96`) is intentional.
