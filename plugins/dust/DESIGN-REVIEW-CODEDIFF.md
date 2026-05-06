---
produced_by: staff-code-reviewer
phase: phase-4
workspace: 20260420-3e20fc2d
created_at: "2026-04-20T22:30:00Z"
confidence: high
depends_on:
  - phase-1
  - phase-2
  - phase-3
token_estimate: 3600
---

# DESIGN-REVIEW-CODEDIFF — phase-4 staff-code-reviewer findings

## Summary

The CodeDiff wire schema and both renderers (React + ratatui) land cleanly
and are additive. Every test that was asked to pass — `dust-core` 103/103,
`dust-registry` 63/63, `chat` 21/21, vitest 15/15, `dust-dashboard` release
build — passes. **However, the accept/reject round-trip does not work
end-to-end.** The React and ratatui sides dispatch `op_id =
"code_diff.accept_hunk"` (the design-doc name); the chat plugin's server
routes on `"code_diff:accept" | "code_diff:reject"`. These strings never
match, so every real accept click or `a` keystroke falls through to
`standard_dispatch` → `plugin.action()` → `ActionResult::err("unknown
action: <hunk-id>")`. The disk-write path is also incorrect: `apply_hunk`
strips trailing newlines and duplicates hunk context lines in the output.
Tracker TRK-562 is **NOT** being closed — the blockers must land first.

## Area-by-area checklist

| # | Area | Status | Notes |
|---|------|--------|-------|
| a | Wire schema round-trip, additive-only | **PASS** | `Component::CodeDiff` added after `AgentTurn` with dedicated `Hunk` / `DiffLine` / `DiffLineKind` types; `#[serde(tag = "type", rename_all = "snake_case")]` inherited → on-wire tag `"code_diff"`. No existing variant modified. Serde tests at `dust-core/src/lib.rs:872-960` cover roundtrip, omission semantics, design-doc fixture parity, and unknown-kind rejection. |
| b | React render + accept/reject dispatch + bundle size | **PARTIAL PASS** | Hunk gutters colored per kind, chips wired, file-level `acceptAll` fans out one dispatch per pending hunk (`ComponentRenderer.tsx:627-632`). Bundle-size diff **cannot be measured** — no pre-CodeDiff baseline was captured. By inspection the added ~220 LOC lives in the existing `ComponentRenderer` chunk (~45 KB) → well under 150 KB. vitest 15/15. |
| c | Ratatui render + `a`/`r` keybindings | **PARTIAL PASS** | Selection scoping is correct: `a` and `r` are gated on `code_diff_hunks_at_cursor()` returning `Some` (`app.rs:918-946`). **`r` has a behavior-change collision** with the pre-existing manual-refresh binding — see Warning 1. `[a]ccept`/`[r]eject` hints render only on the selected hunk header. |
| d | Accept/reject correctness, line-ending preservation, atomic tmp+rename, path-traversal guard | **FAIL** | `validate_path` and `fs::rename(tmp, target)` are present. BUT (1) the wrong op_id is routed (Blocker 1), (2) trailing newline is stripped (Blocker 2), (3) context lines are duplicated (Blocker 3), (4) path-traversal guard has no end-to-end test through `handle_code_diff_accept` (Warning 4). |
| e | Updated-tree DataUpdated event flips `applied` per hunk, chat_messages order preserved | **FAIL** | `render_updated_code_diff` is a stub that clones the CodeDiff unchanged (`plugin.rs:196-202`). No `applied` field exists on `Hunk` in `dust-core/src/lib.rs:163-183`. Chat message order is trivially preserved (the handler does not write to the store). |
| f | Non-regression: dust-core / dust-registry / chat / dust-dashboard build + tests + `dust-dash.sh --no-build` | **PASS** | `dust-core` 103/103 passed (+ the four new CodeDiff serde tests); `dust-registry` 63/63 passed; `chat` 21/21 passed; `cargo build --release -p dust-dashboard` clean; `dust-dash.sh --no-build` entry point exists and script body is unchanged. |

## Blockers

### BLOCKER 1 — op_id mismatch breaks accept/reject end-to-end

`plugins/chat/src/server.rs:146`

```rust
matches!(params.op_id.as_deref(), Some("code_diff:accept" | "code_diff:reject"))
```

and `server.rs:205`:

```rust
let result = if op_id == "code_diff:accept" { ... }
```

The frontends dispatch the design-doc name `"code_diff.accept_hunk"` (see
`plugins/dust/src/ComponentRenderer.tsx:14` and
`plugins/dust/dust-dashboard/src/component_renderer.rs:19`). With the
colon/dot mismatch, `is_code_diff` evaluates `false` and the request falls
through to `standard_dispatch` → `plugin.action()` — which pattern-matches
on `item_id` (the hunk id, e.g. `"h-0"`), hits the `Some(other)` arm at
`plugin.rs:315`, and returns `ActionResult::err("unknown action: h-0")`.
No hunk is applied. No telemetry is written. No DataUpdated event is
emitted.

**Fix:** change the two call sites in `server.rs` to match
`"code_diff.accept_hunk"` (single op — reject is client-side per DESIGN
§e), and delete the now-unreachable reject branch OR add a second op
`"code_diff.reject_hunk"` and have the frontends dispatch it. Pick one;
do not leave the string mismatch.

### BLOCKER 2 — `apply_hunk_to_content` strips the trailing newline

`plugins/chat/src/code_diff.rs:82,109`

```rust
let lines: Vec<&str> = content.lines().collect();
// ...
Ok(result.join("\n"))
```

`str::lines()` discards line terminators and `join("\n")` adds only
separators, not a trailing newline. A file whose original bytes end in
`\n` (the normal case) will lose it after apply — violating the task's
"byte-exact file content including line-ending preservation" criterion.

**Fix:** detect whether `content` ends in `"\n"` before the split and,
if so, append `"\n"` to `result.join("\n")` before writing. Add a unit
test that asserts **bytes-equal** (`assert_eq!(on_disk.as_bytes(),
expected.as_bytes())`) — the current tests use `.contains()` and cannot
detect this.

### BLOCKER 3 — `apply_hunk_to_content` duplicates context lines

`plugins/chat/src/code_diff.rs:78-110` copies
`lines[0..old_start_0based]` from the original file, then emits every
`Context`/`Add` line from the hunk, then copies
`lines[old_end..end]`.

When a hunk's `lines[]` include *any* context line whose pre-image
position is `< old_start` or `>= old_end`, that line is emitted twice —
once from the prefix/suffix copy, once from the hunk replay. Even for a
well-formed hunk whose context lines sit exactly at `[old_start,
old_end)`, the implementation cannot verify that; a silent corruption
slides through.

The existing unit test at `code_diff.rs:117-141` is a canary for this
bug — fixture is `old_start=2, old_count=2` with a leading
`Context(line1)`. A correct apply would produce `line1 · line2_modified
· line2_extra · line3 · line4`. This implementation produces `line1 ·
line1 · line2_modified · line2_extra · line3 · line4` (duplicate
line1). The `.contains("line2_modified")` assertion hides it.

**Fix:** replace the "copy prefix + emit hunk + copy suffix" algorithm
with the conventional unified-diff apply: walk the pre-image up to
`old_start-1`, then for each hunk line emit (Context or Add) / skip
(Remove), and after the hunk continue from `old_start + old_count`. Then
add an exact-bytes test that runs the fixture above and asserts
`line1` appears exactly once.

### BLOCKER 4 — updated-tree DataUpdated cannot flip an `applied` state

`plugins/chat/src/plugin.rs:196-202`

```rust
fn render_updated_code_diff(
    &self,
    code_diff: &Component,
    _accepted_hunk_id: &str,
) -> Result<Component, String> {
    Ok(code_diff.clone())
}
```

The returned component is byte-identical to the original. The React
frontend's "Accepted" state is held entirely in local
`useState<Record<string, HunkState>>` (`ComponentRenderer.tsx:613`),
which resets on any thread reload or parent re-mount. The ratatui side
tracks applied ids in `app.code_diff_accepted` (`app.rs:151,979`) but
also clears on thread change (`app.rs:843`). Worse, the task criterion
(e) explicitly asks for a per-hunk `applied` field — there is none on
`Hunk` in `dust-core/src/lib.rs:163-183`.

**Fix (additive-only):** add `#[serde(default, skip_serializing_if =
"std::ops::Not::not")] pub applied: bool` to `dust_core::Hunk`. In
`handle_code_diff_accept`, clone the hunks with `h.applied = (h.id ==
accepted_hunk_id) || h.applied` and emit the rewritten `Component::
CodeDiff` via `data_updated_event`. Add a dust-core roundtrip test that
`applied: false` is still omitted from the wire payload (additive
invariant).

## Warnings

### WARNING 1 — `r` overloads the existing refresh binding

`plugins/dust/dust-dashboard/src/app.rs:925-946`

When a `CodeDiff` is on-screen the `r` key now rejects a hunk; otherwise
it falls through to the legacy refresh-and-re-render path. The design
doc §d explicitly picks `x` as the reject key *because* `r` was already
meaningful. Side-effect: users can no longer refresh while a CodeDiff
is rendered — `r` will silently dismiss the highlighted hunk.

**Fix:** bind reject to `x` per the design, OR bind refresh to
`Ctrl+R` so the two do not alias.

### WARNING 2 — reject round-trip is dead code from the frontend side

`plugins/chat/src/server.rs:205-209` and `plugin.rs:183-194` implement
reject as a server round-trip, but neither frontend ever sends a reject
op (React `handleReject` is purely client-side, ratatui stores into
`code_diff_rejected` without dispatching). The server branch cannot be
reached by the host. Either delete the reject path or have the ratatui
side dispatch `code_diff.reject_hunk` when `r`/`x` fires — matching
Blocker 1's op_id decision.

### WARNING 3 — temp-file name is predictable and collision-prone

`plugins/chat/src/code_diff.rs:60-63`

```rust
let tmp_path = safe_path.with_extension(format!(
    "{}.tmp",
    safe_path.extension().and_then(|e| e.to_str()).unwrap_or("")
));
```

Two concurrent accepts for the same file stomp on one another's
`.tmp`. For extensionless files `with_extension(".tmp")` produces
`file..tmp` (double dot) — harmless but ugly. Use
`tempfile::NamedTempFile::new_in(safe_path.parent().unwrap())` +
`persist(&safe_path)` for atomic rename with a unique name.

### WARNING 4 — path-traversal guard has no integration test

`shared/paths/src/lib.rs:42-72` tests canonicalization unit-by-unit.
`plugins/chat/src/plugin.rs` has no test that passes a traversal path
(`../../etc/passwd`, absolute `/etc/passwd`) as the `path` arg to
`handle_code_diff_accept` and asserts the handler errors *before*
touching disk. Add one. Also note that
`validate_path_accepts_valid_home_path` at `shared/paths/src/lib.rs:48`
passes `~/test.txt` — `PathBuf` does not expand `~`; the test passes
only by accidental canonicalization behavior. Replace with
`$HOME/test.txt`.

## Suggestions

- `apply_hunk_to_content` ignores `new_count`. A validator that asserts
  `lines.iter().filter(|l| l.kind != Remove).count() == new_count as
  usize` catches malformed hunks early. Same for
  `Remove`/`Context` vs `old_count`.
- `plugins/chat/src/code_diff.rs:196-202` — drop the unused
  `_accepted_hunk_id` once Blocker 4 is fixed; or rename to
  `last_accepted_id` and use it.
- `plugins/chat/src/error.rs:12,15` — `MissingArg` and `ThreadNotFound`
  variants are flagged dead by `cargo test`. Either use them in the new
  handlers or drop them.
- `store.rs:27` — `SqliteStore.path` field is flagged unused. Remove or
  expose as `fn path(&self) -> &Path`.

## What's Good

- Wire schema is strictly additive. `Component::CodeDiff` added after
  `AgentTurn` with no modification to existing variants; serde uses the
  same `snake_case` convention as the rest of the module. Four dedicated
  tests (`code_diff_serde_roundtrip`,
  `code_diff_omits_language_when_none`,
  `code_diff_deserializes_from_design_fixture`,
  `code_diff_rejects_unknown_kind`) lock the contract.
- Intaglio tokens (`#DA7757` terracotta, `#F2EAD7` cream) reused
  correctly across both renderers (`ComponentRenderer.tsx:266-267` and
  `component_renderer.rs:395,481,511,544`), so the design language is
  consistent between React and ratatui.
- Selection-scoping of `a`/`r` is gated on
  `code_diff_hunks_at_cursor()` rather than a global match — when no
  CodeDiff is on screen, `a` is a pure no-op (correct) and `r` falls
  through (partially correct — see Warning 1).
- `shared/paths` crate is a clean 27-line extraction; no regression of
  existing `src-tauri/src/lib.rs::validate_path` (the src-tauri copy
  is still the owner of the tauri-side write path and is unchanged).
- Non-regression is clean: `dust-core` 103/103, `dust-registry` 63/63,
  chat 21/21, vitest 15/15, `dust-dashboard` release build clean.

## Tracker

`tracker update TRK-562 --status done` **NOT** executed. The
accept/reject round-trip is non-functional end-to-end (Blocker 1) and
the write path produces incorrect file bytes (Blockers 2, 3). Tracker
should stay open pending the fixes above.

<!-- scratch -->
For the next implementer cleaning these up:

1. Fix Blocker 1 first — it's a one-line change in server.rs but it's
   the reason nothing works end-to-end. After the fix, add an integration
   test in plugins/chat that sends a real Envelope::Request with
   op_id="code_diff.accept_hunk" and asserts a DataUpdated event comes
   back with the updated component.

2. Blockers 2 + 3 are both in apply_hunk_to_content. Rewrite the helper
   to: detect trailing newline, walk pre-image lines 0..old_start-1,
   replay hunk.lines skipping Remove, then continue from
   old_start+old_count. Add a byte-equal test.

3. Blocker 4 — add `applied: bool` with `#[serde(default,
   skip_serializing_if = "std::ops::Not::not")]` to dust-core::Hunk.
   Additive per the wire-schema rule.

4. Warning 1 — change the dashboard's reject key to `x` per DESIGN §d.
   Don't overload `r`.

After fixes land, re-run: `cargo test -p dust-core --lib`,
`cargo test -p dust-registry --lib`, `cd plugins/chat && cargo test`,
`cargo build --release -p dust-dashboard`, `npm test -- --run` from
plugins/dust. Only then run `tracker update TRK-562 --status done`.
<!-- /scratch -->

DECISION: withhold `tracker update TRK-562 --status done` because the
accept/reject dispatch is non-functional (op_id mismatch) and the
write path corrupts files (line-ending and duplicate-context bugs).
The review is structurally complete but the implementation is not.

FINDING: Even with 21/21 chat tests green, the feature is
end-to-end broken. The tests exercise the Rust-only call path
(`plugin.handle_code_diff_accept(...)` directly) and bypass the
server dispatch layer where the op_id mismatch lives. Lesson for
future phases: add at least one integration test that goes through
`dispatch_request` — otherwise the plug-and-socket layer is invisible
to CI.

GOTCHA: `content.lines().collect::<Vec<_>>().join("\n")` is a common
Rust shape that silently drops trailing newlines. Any file-rewrite path
should either use `split_inclusive('\n')` or preserve the trailing-`\n`
state explicitly.
