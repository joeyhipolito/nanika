---
produced_by: architect
phase: phase-1
workspace: 20260428-acb15835
created_at: "2026-04-28T00:00:00Z"
confidence: high
depends_on:
  - plugins/whim-web/DESIGN-SLICE-6.md
token_estimate: 1900
---

# Real-usage demo — step draft (30 steps)

This is the working draft for `src/demo/script.ts`. Each row enumerates one `DemoStep` per the shape defined in `DESIGN-SLICE-6.md` §(b). The implementer renders this table into TypeScript — narration/explanation become the two HUD strings; `mode`/`surface` populate the `patch`; `key` becomes `pressedKey`; `cursor` becomes `cursor.{x,y,visible}` (percentages of the 1440×900 stage); `dur` becomes `durationMs`.

Surface key: **P0** = pill, **PAL** = palette (820×520), **PD** = palette-detail, **CV** = canvas (composite), **DF** = diff viewer.
Mode key: **i** = idle, **h** = hover, **t** = type, **v** = voice.

| #  | id                  | mode | surface | key      | cursor (x%, y%, vis) | narration ("what action")                          | explanation ("what surfaces and why")                                                                                                  | dur ms |
|----|---------------------|------|---------|----------|----------------------|----------------------------------------------------|----------------------------------------------------------------------------------------------------------------------------------------|--------|
| 1  | d1-rest             | i    | P0      | —        | (88, 92, false)      | Whim sits at rest.                                 | 63×9 capsule on the bottom-left edge. No labels, no chrome. Discoverability comes from one global hotkey, not from chrome.             | 2200   |
| 2  | d2-cursor-approach  | i    | P0      | —        | (50→12, 92, true)    | I move toward it.                                  | Hover zone is a 73×19 wrapper around the 63×9 idle pill — five-px padding so mouseLeave never fires mid-expand.                        | 1800   |
| 3  | d3-hover-preview    | h    | P0      | —        | (12, 92, true)       | I hover.                                           | Pill morphs to 126×36 with 18 px radius. Reveals input + mic + conv-count badge. Same DOM node — only `width`/`height`/`radius` move.   | 2200   |
| 4  | d4-summon-key       | h    | P0      | ⌥Space   | (12, 92, false)      | I press ⌥Space.                                    | KeyHUD flashes the chord. The pill is about to take focus — substrate doesn't switch yet, the user owns the transition.                | 1200   |
| 5  | d5-pill-to-palette  | t    | PAL     | —        | (50, 50, false)      | The palette opens in place.                        | 820×520 palette. Same component as the pill, scale prop flipped. Width interpolates over 250 ms — no pop-in window.                    | 2600   |
| 6  | d6-type-prompt      | t    | PAL     | —        | (50, 32, false)      | I type "open the diff for the last commit".        | Composer is `recording` *off*, `drafting` *on*. Caret advances character-by-character; placeholder fades. Mode communicated by icon.   | 3800   |
| 7  | d7-grouped-results  | t    | PAL     | —        | (50, 48, false)      | Results group themselves.                          | Actions · Recent · Projects · Tracker · Plugins. Top match wins focus; ↑↓ moves; ↵ selects. No tabs, no filters.                       | 2400   |
| 8  | d8-arrow-down       | t    | PAL     | ↓        | (50, 48, false)      | I press ↓ to widen the search.                     | Visual-only key — the demo doesn't intercept arrows; it shows them. Recognition over recall: every shortcut sits beside its action.    | 1400   |
| 9  | d9-voice-decide     | t    | PAL     | —        | (50, 32, false)      | I'd rather speak it.                               | Narration cue. The composer is still focused — voice doesn't open a new surface; it sub-states the same composer.                      | 1400   |
| 10 | d10-vhold-down      | t    | PAL     | V        | (50, 32, false)      | I hold V.                                          | After 200 ms of V-hold, composer enters `recording-vhold`. A 9-bin overlay paints over the input row. Any mode change cancels.         | 1800   |
| 11 | d11-vhold-record    | v    | PAL     | V (held) | (50, 32, false)      | I dictate.                                         | Live STT partial transcript renders below the bins. The pill mode flag flips to `voice` so the L0 box doesn't spuriously redraw.       | 3200   |
| 12 | d12-vhold-release   | t    | PAL     | —        | (50, 32, false)      | I release V.                                       | Transcript pastes into the composer with caret at the end. Whim never auto-submits a dictation — the user always edits before sending.| 2000   |
| 13 | d13-edit-prompt     | t    | PAL     | —        | (50, 32, false)      | I tweak the prompt.                                | Caret moves; chips below the input update token estimate live. Chips are status, not nav — clicking opens a popover, never a window.   | 2400   |
| 14 | d14-submit          | t    | PAL     | ↵        | (50, 32, false)      | I press ↵.                                         | Optimistic submit. The user's turn echoes into a chat rail before the workspace is allocated. Perceived latency is owned, not spun.    | 1600   |
| 15 | d15-palette-to-cv   | t    | CV      | —        | (50, 50, false)      | The substrate flips to the mission canvas.         | Palette children fade out, canvas children fade in — same root div, scale interpolated. The transcript anchors to the eye's last fix.  | 2200   |
| 16 | d16-runlog          | t    | CV      | —        | (24, 56, true)       | A run log appears, one row per phase.              | Phase rows transition ○ → ✓ with a live duration counter. Tool chatter stays hidden — one line per *phase*, not per tool call.         | 3200   |
| 17 | d17-terminal-open   | t    | CV      | ⌘J       | (24, 56, false)      | I press ⌘J for the terminal.                       | A 240-px drawer docks at the bottom; the canvas reflows up. Docks push, overlays float — that's the rule.                              | 2200   |
| 18 | d18-terminal-close  | t    | CV      | ⌘J       | (24, 56, false)      | I press ⌘J again.                                  | Drawer collapses; canvas reflows down. Toggle is symmetric — closing is hide, not reset.                                               | 1600   |
| 19 | d19-rail-files      | t    | CV      | ⌘B       | (88, 24, true)       | I open the right rail with ⌘B.                     | 300-px rail mounts in `files` mode by default — the FilesPanel from slice-2. Rail mode survives close + reopen.                        | 2400   |
| 20 | d20-mission-done    | t    | CV      | —        | (50, 50, false)      | The mission completes.                             | A Changed-Files card surfaces inline (CHANGED FILES (N) • +X / −Y) with a per-file plain-English summary. First-class block, not banner.| 2600   |
| 21 | d21-view-diff       | t    | CV      | ⌘D       | (50, 64, true)       | I press ⌘D to view the diff.                       | The right rail flips to `turn-diff` mode and the diff viewer takes the hero column. Modal: the next 11 keys live only here.            | 2000   |
| 22 | d22-diff-hero       | t    | DF      | —        | (50, 50, true)       | The diff viewer is Whim's hero.                    | Side-by-side hunks, current hunk haloed. The grammar is modal — y/n/j/k/a/Y/⇧A/⌘[/⌘]/c/⌘Z/r — inert outside the viewer.                  | 3200   |
| 23 | d23-accept-hunk     | t    | DF      | y        | (50, 50, false)      | I press y to accept this hunk.                     | The hunk applies; cursor advances to the next. Single-key actions because review fatigue is the failure mode, not slow apply.          | 1800   |
| 24 | d24-next-hunk       | t    | DF      | j        | (50, 50, false)      | j moves to the next hunk.                          | j/k navigate hunks; ⌘[/⌘] navigate files. The same modal vocabulary as Vim, scoped to the diff viewer only.                            | 1600   |
| 25 | d25-reject-hunk     | t    | DF      | n        | (50, 50, false)      | I press n to reject this one.                      | The hunk reverts; the chat rail records the rejection so the model can re-read on retry. r re-applies after a fix.                     | 1800   |
| 26 | d26-accept-file     | t    | DF      | Y        | (50, 50, false)      | Y accepts the whole file.                          | Cap accent for primary action — the keycap glows. Bulk operations exist but require a separate key, never a misclick away.             | 1600   |
| 27 | d27-commit          | t    | DF      | c        | (50, 50, false)      | c commits the accepted set.                        | Commit message pre-fills from the mission's plain-English summaries. Commits are keystrokes, not modal forms.                          | 2200   |
| 28 | d28-back-to-canvas  | t    | CV      | Esc      | (50, 50, false)      | Esc steps back one rung.                           | Esc never skips a rung. Diff (L4) → canvas (L3). The Tour itself owns its rung; one Esc, one step back.                                | 1800   |
| 29 | d29-canvas-to-pill  | i    | P0      | Esc      | (12, 92, true)       | Esc again — back to rest.                          | Canvas (L3) → query (L1) → pill (L0). The substrate collapses by reversing the same scale interpolation it expanded with.              | 2400   |
| 30 | d30-final-rest      | i    | P0      | —        | (88, 92, false)      | Whim returns to the edge.                          | 63×9 again. Same node, same anchor, same dock. The session left no chrome behind — the workspace is a side effect, not a destination.   | 2400   |

**Total runtime:** ≈ 70 s (sum of dur ms = 70 200 ms).

**Notes for implementer (do not render into TS):**
- Steps 6 and 11 (`d6`, `d11`) need `ticks[]` for character-by-character typing and partial-transcript appearance respectively. All other steps land their patch on `atMs: 0`.
- `cursor.x` `12` parks at the bottom-left dock anchor (40 % rule from `App.css:49`); `88` parks at the bottom-right dock anchor (60 %). Steps that don't move the cursor inherit the previous position — implementer should *not* repeat the cursor patch every step.
- `pressedKey` is one-shot: the HUD owns its own 600 ms decay. Setting `pressedKey: null` is unnecessary; the HUD handles it.
- Steps 17/18 and 19 demonstrate `⌘J` and `⌘B` as **demonstrated** keys (visual only) — the demo does not intercept them. Same convention as `Tour.tsx` slice-5.
- Steps 22–27 inside the diff viewer must guarantee `data-tour-anchor='diff-hunk-active'` is present — reuse the slice-5b sentinel pattern from `tour/TourScene.tsx:243`.

<!-- scratch -->
- 30 steps — within the 25–35 budget. Five wrap-back steps (28–30) bring the demo back to L0 so the loop reads as one full session, not a one-way tour.
- Cursor coordinates are stage-relative percentages (1440×900). Implementer translates via `getBoundingClientRect` on `[data-demo-root]`.
- Voice steps (10–12) hold `mode: 'voice'` for one beat to pin the L0 box at 126×36 even though the surface is the palette — see DESIGN-SLICE-6 §(a).
- Diff hunk anchor (steps 22–27) sentinel must be present before step 22 mounts; otherwise SpotlightRing equivalent in the demo (if any) snaps to a stale rect.
<!-- /scratch -->
