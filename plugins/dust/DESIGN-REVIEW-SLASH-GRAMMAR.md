---
produced_by: staff-code-reviewer
phase: phase-3
workspace: 20260420-5b7a3652
created_at: "2026-04-20T12:10:00Z"
confidence: high
depends_on:
  - implement-slash-grammar
  - design-slash-grammar
token_estimate: 2100
---

# Design Review — Slash-Command Grammar (TRK-560)

## Summary

The parser layer is clean and both the TypeScript and Rust ports honour the
§5.3 test vector table verbatim. Verification suites are all green
(`cargo test -p chat` 26/26, `cargo test -p dust-registry --lib` 63/63,
`cargo test` dust-dashboard 14/14, vitest `slashGrammar` 14/14, tsc clean,
`cargo build --release -p dust-dashboard` clean).

However the **Tauri dispatch path has a blocking lookup bug**: it resolves
the slash prefix by scanning the current fuzzy-search `results`, but that
list was filtered using the *raw* slash query (`/tracker create foo`). The
registry's `fuzzy_score` doesn't match `/tracker` against any plugin's
corpus (corpus entries never contain a leading `/`), so `results` is empty
for every `/prefix…` input — which makes every known command (including
`/tracker` and `/ask`) fall through to the "Unknown command" terracotta
banner. The TUI side gets this right via `plugin_for_slash_prefix`
(searches all plugins and filters by `Capability::Command { prefix }`),
but Tauri needs an equivalent path that isn't coupled to the user's
current query.

## Area Checklist

| Area | Result | Notes |
|------|--------|-------|
| (a) Parser correctness — test matrix coverage | **PASS** | All 14 rows of §5.3 mirrored as both vitest cases (`plugins/dust/src/slashGrammar.test.ts`) and Rust unit tests (`plugins/dust/dust-dashboard/src/slash_grammar.rs`). Empty input, bare `/`, uppercase, leading-digit, underscore, tab separator, mid-line slash, and leading-space are all rejected. Byte-for-byte field parity between the TS and Rust outputs. |
| (b) Prefix lookup — case sensitivity, no collision, Ask Claude fallback | **FAIL (Tauri) / PASS (TUI)** | Design §5 fixes prefix to lowercase, so case-sensitive lookup is correct. TUI `plugin_for_slash_prefix` (`app.rs:639`) walks all plugins with an empty-query `search_with_ids` — correct. Tauri `dispatchSlash` (`App.tsx:172`) reuses the fuzzy-filtered `results`, but `fuzzy_score` has no match for `/prefix…` queries (see Blocker 1) so no prefix ever resolves. Ask Claude fallback still fires for *non-slash* empty-match queries because `parseSlash` returns null on `hi /foo`, leading whitespace, uppercase, etc., and `handleEnter` falls through to `displayResults[selectedIndex]` in that branch. |
| (c) Dispatch correctness — `/tracker create foo` → `args={title:"foo"}` | **PASS (by construction)** | Both sides split `slash.args.trim_start()` on the first space to separate op from tail, then send `op_id=op`, `args={title:tail}` (`App.tsx:203-216`, `app.rs:715-732`). Tracker's `create` op reads `args.title` (`plugins/tracker/src/dust_serve.rs:651`). The raw `/tracker create foo` string is never forwarded. Note: this path is only exercised by the TUI today — Tauri is blocked behind the lookup bug in (b). |
| (d) Error surface — terracotta Component::Text, no silent swallow | **MIXED** | TUI pushes a `Component::Text` with `Color::new(0xDA, 0x77, 0x57)` into `chat_messages` via `push_chat_error` (`app.rs:655-664`). Tauri surfaces a terracotta `<div role="alert">` banner under the search bar (`App.tsx:563-575`, colour `#DA7757`). Neither is silent, but the two surfaces are different primitives — not a blocker, called out as WARNING 3. |
| (e) Non-regression — `cargo test -p chat` 26+/green, dust-registry 63/63, vitest, release build | **PASS (test suites) / RISK (runtime flow)** | All four verification commands pass cleanly. However the "type text → Ask Claude" runtime flow in Tauri routes through the same `dispatch_action` wrapper (`actionId: 'ask'`) that assumes `op_id=action_id`, and `server.rs:141` only triggers streaming when `item_id` equals `"ask"` — see WARNING 2. Not introduced by this phase but called out because the task explicitly asks to verify non-regression. |

## Blockers

### BLOCKER 1 — Tauri slash lookup uses fuzzy-filtered results (`plugins/dust/src/App.tsx:172`)

`dispatchSlash` resolves the prefix against the current search `results`:

```ts
const match = results.find(r => r.capability.keywords.includes(slash.prefix))
if (!match) {
  setSlashError(`Unknown command: /${slash.prefix}`)
  return true
}
```

`results` is populated by the debounced `invoke('search_capabilities', { query })`
effect at line 147, and `query` is the raw input line — e.g. `"/tracker create foo"`.
`dust_registry::fuzzy_score` splits the query on whitespace and requires each
term to appear as a substring in the plugin corpus (`dust-registry/src/lib.rs:2051-2073`).
The corpus is built from name, description, and `"{prefix} command"` — it never
contains a leading `/`. So `"/tracker"` doesn't match, the plugin scores 0, and
`results` is empty for every `/prefix…` input the user types.

**Effect:** every known slash command (including the design's canonical
`/tracker create foo` and `/ask hello`) is rejected as "Unknown command".
The dispatch code path in App.tsx:181-217 is unreachable.

**Fix suggestions (pick one):**

1. Do an independent lookup: add a Tauri command (e.g. `list_command_prefixes`)
   that returns `Vec<(prefix, plugin_id, capability_id)>` built from
   `registry.search_with_ids("").await` filtered by `Capability::Command { .. }`.
   `dispatchSlash` queries that map instead of `results`. Mirrors the TUI's
   `plugin_for_slash_prefix` and keeps the fuzzy search untouched.
2. Strip the leading slash before calling `search_capabilities` — e.g. rewrite
   `query` to `slash.prefix` or `slash.raw.slice(1)` inside `dispatchSlash`.
   Cheaper but couples the UI's visible `results` list to the keystroke rather
   than the canonical typed query; may surprise users who expect the results
   panel to reflect what they typed.
3. In `fuzzy_score`, treat a leading `/` on a term as a command-prefix
   literal (strip it, then require the stripped token to equal a
   `Capability::Command { prefix }`). Changes registry semantics globally;
   only worth doing if we want the results list to show the matching
   command plugin while the user is typing.

Option 1 is lowest-risk and matches the TUI's design.

## Warnings

### WARNING 1 — `dispatchSlash` return type is misleading (`App.tsx:167-220`)

The function is declared `(slash: SlashCommand): boolean` but every path
returns `true`. The single caller (`handleEnter`, line 230-232) ignores the
return value. Either drop the return (`void`) or actually use the boolean
to short-circuit the `displayResults[selectedIndex]` path only when dispatch
happened. As written, the signature suggests a control-flow contract that
doesn't exist.

### WARNING 2 — Tauri `dispatch_action` wrapper maps `actionId` to `op_id` (`src-tauri/src/lib.rs:179-183`) while chat streaming triggers on `item_id`

`dispatch_action` builds `ActionParams { op_id: Some(action_id), item_id:
args.id, args }`. Every Tauri-side chat invocation (the existing Ask Claude
fallback at `App.tsx:243-249`, the new `/ask` branch at `App.tsx:183-190`,
and the `⌘N` new-thread at `App.tsx:411-416`) sends `actionId: 'ask'` /
`'new_thread'` without an `id` — so `item_id` is `None`.

`plugins/chat/src/server.rs:139-142` triggers streaming on
`matches!(params.item_id.as_deref(), Some("ask") | Some("continue"))`; with
`item_id=None` it stays false and the request falls through to
`standard_dispatch` → `plugin.action`, which at line 535 returns
`"item_id required"`. I did not exercise this at runtime, so this is flagged
as a warning rather than a blocker — but if it reproduces, both the existing
Ask Claude flow and the new `/ask` slash dispatch are broken in Tauri.

**Fix suggestion:** in `src-tauri/src/lib.rs`, route `chat` / `ask` actions
through `item_id` instead of `op_id`, or change the frontend to pass
`actionId` via a dedicated wrapper that sets `item_id`. The TUI already does
this correctly (`app.rs:694-698`, `app.rs:824-828`).

### WARNING 3 — Inconsistent error surface between TUI and Tauri

Task (d) asks for "terracotta `Component::Text` in both Tauri and TUI".
TUI satisfies this literally (`app.rs:655-664`). Tauri surfaces a plain
`<div role="alert">` with inline terracotta `color: #DA7757`
(`App.tsx:563-575`). Both are visible and terracotta, so this is not a
silent swallow, but the visual treatment and ARIA wrapper differ.
Acceptable if intentional (Tauri has a dedicated banner slot above
`<ResultsList>`); worth confirming with the design owner.

### WARNING 4 — No U+00A0 → U+0020 normalisation before `parseSlash`

Design §6 R3 explicitly states "the chat input layer normalises U+00A0 →
U+0020 on the first separator before calling parseSlash". Neither
`handleEnter` (`App.tsx:226-234`) nor `handle_key_chatting` (`app.rs:798-809`)
performs this. On macOS with smart-substitution, `/foo\u00A0bar` will
silently fall through to the free-text path instead of dispatching. Low-risk
in practice but violates the design contract and is easy to miss later.

### WARNING 5 — Build artefacts committed

`plugins/dust/tsconfig.node.tsbuildinfo` and
`plugins/dust/tsconfig.tsbuildinfo` are included in commit `d1d41eb2` but
are generated output. They should be in `.gitignore`.

## Suggestions

- `slashGrammar.test.ts` and the Rust tests assert the parser output but no
  test covers the `/foo\u00A0bar` case from §6 R3 — add one red test for
  each side to pin the behaviour once the normalisation is added
  (WARNING 4).
- The `plugin_for_slash_prefix` helper in `app.rs:639-652` does a full
  `search_with_ids("")` every slash. Fine for dozens of plugins; if the
  registry grows, cache a `HashMap<prefix, plugin_id>` keyed off manifest
  events. Not needed today.
- Consider pulling the op/tail split in both `App.tsx:202-216` and
  `app.rs:715-732` into a shared helper (e.g. `splitArgs(slash.args) ->
  (op, tail)`) so future verbs that want a different split rule have one
  place to change.

## What's Good

- The parser ports are byte-for-byte equivalent; the Rust scanner avoids a
  regex dep with an eight-line state machine that tracks the spec exactly.
- The §5.3 test vector table is wired verbatim in both implementations —
  `parseSlash('')`, `/`, `/Foo`, `/foo_bar`, `/foo\tbaz`, and the leading-
  space case all have explicit red tests. The normative suite from the
  design doc is now CI-enforced on both sides.
- TUI `dispatch_slash` cleanly separates the `ask` streaming branch from
  generic command dispatch (`app.rs:674-741`) and surfaces errors through a
  single `push_chat_error` helper rather than scattering colour literals.
- `handleEnter` correctly short-circuits via `parseSlash` *before* touching
  `displayResults` (`App.tsx:229-233`), preserving the existing free-text
  → Ask Claude fallback for non-slash input (non-regression intent held).
- Verification bundle green end-to-end: 14/14 vitest, 14/14 dust-dashboard,
  63/63 dust-registry, 26/26 chat, tsc clean, release build clean.

## Tracker

`tracker update TRK-560 --status done` — **not executed**. Success
criterion in the task requires "pass/fail per area plus the tracker
confirmation". BLOCKER 1 makes the Tauri path (b, c, e) non-functional, so
the phase is not green end-to-end. Defer the status update until Blocker 1
is addressed.

<!-- scratch -->
For the implementer addressing Blocker 1: the Tauri fix wants a new Tauri
command that returns the prefix→plugin map. Draft shape:

```rust
#[tauri::command]
async fn list_command_prefixes(state: State<'_, AppState>)
    -> Result<Vec<CommandPrefix>, String> {
    let all = state.registry.search_with_ids("").await;
    let mut out = Vec::new();
    for (plugin_id, m) in all {
        for c in &m.capabilities {
            if let dust_core::Capability::Command { prefix } = c {
                out.push(CommandPrefix {
                    prefix: prefix.clone(),
                    plugin_id: plugin_id.clone(),
                    capability_id: format!("cmd:{prefix}"),
                });
            }
        }
    }
    Ok(out)
}
```

Call once on App mount (or refresh on a plugin-added event) and cache in
state; `dispatchSlash` reads from that map instead of `results`. Add a
vitest that simulates typing `/tracker create foo`, asserts the invoke
call shape `{ pluginId: 'tracker', actionId: 'create', params: { title:
'foo' } }`.

Secondary: WARNING 2 may also bite `/ask`. Worth exercising the Tauri
`/ask hello` end-to-end (launch dust, type, confirm a streaming delta
arrives) before declaring done. If the Ask Claude fallback was already
shipping pre-this-phase, the bug pre-exists and needs its own tracker.
<!-- /scratch -->

FINDING: Tauri `dispatchSlash` uses the fuzzy-filtered `results` list to
resolve prefix → plugin, but the raw `/prefix` query never matches the
registry's corpus, so every slash command is reported as unknown. The
equivalent TUI code (`plugin_for_slash_prefix`) uses an unfiltered
empty-query search and works correctly.

PATTERN: Port parsers byte-for-byte between runtimes by pinning the
normative acceptance suite in the design doc and wiring each row as an
explicit test on both sides (§5.3 of DESIGN-SLASH-GRAMMAR.md). This review
hit zero parser-behaviour deviations across TS and Rust.

DECISION: Reject this phase pending BLOCKER 1 fix. Keep the parser code
and tests as-is; focus the fix on the Tauri dispatch lookup.
