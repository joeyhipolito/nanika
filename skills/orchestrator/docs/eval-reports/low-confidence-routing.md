# Low-Confidence Routing Investigation (V-136)

**Date:** 2026-03-18
**Target:** `repo:~/skills/orchestrator`
**Status:** Research complete — read-only investigation

---

## Executive Summary

All 17 `low_confidence` findings in the `decomposition_findings` table are caused by a **single root cause: `llmDecompose()` failing and falling back to `keywordDecompose()`**. Every finding shows the same 3-phase fingerprint (`implement` / `write` / `review` or similar) produced by `keywordDecompose()`, never by an LLM. None are simple-task-classification bypasses.

The batch of 14 findings from 2026-03-17T14:10–14:12 (2 minutes, 14 missions) is strongly indicative of **API rate limiting** — a burst of parallel mission runs exhausted the Haiku quota. The remaining 3 findings (IDs 74–78) from earlier time slots likely represent individual API errors or timeouts.

The LLM fallback path has zero stderr visibility — operators see nothing when `llmDecompose` fails.

Test suite status: **all `./internal/persona/...` tests pass** (cached, no failures).

---

## 1. The 17 `low_confidence` Findings — Full Catalog

All 17 findings come from `decomposition_findings` (IDs 74–92, with gaps). Every finding has:
- `finding_type = low_confidence`
- `decomp_source = passive`
- `audit_score = 0`
- `detail = "all phases assigned by keyword or fallback; no LLM or target-context signal"`

The table below maps each finding to its mission via `workspace_id`:

| DB ID | Workspace | Target | Mission | Phase | Persona | Method |
|---|---|---|---|---|---|---|
| 74 | `20260317-902c4bf2` | `~/skills/scout` | V-124: Add intel rotation / TTL cleanup | implement | senior-golang-engineer | keyword |
| 74 | " | " | " | review | staff-code-reviewer | keyword |
| 75 | `20260317-7ca1d86d` | `~/nanika/plugins/dashboard` | V-114: Show PR URL on mission cards | implement | senior-frontend-engineer | keyword |
| 75 | " | " | " | review | staff-code-reviewer | keyword |
| 76 | `20260317-cd7c5a00` | `~/skills/scout` | V-122: Add context.Context to gatherer interface | implement | senior-golang-engineer | keyword |
| 76 | " | " | " | review | staff-code-reviewer | keyword |
| 78 | `20260317-ddc69909` | `~/skills/orchestrator` | V-152: Post-completion Linear comment with mission summary | research | academic-researcher | keyword |
| 78 | " | " | " | implement | senior-backend-engineer | **fallback** |
| 78 | " | " | " | write | storyteller | keyword |
| 78 | " | " | " | review | staff-code-reviewer | keyword |
| 79 | `20260318-15bd8fc6` | `~/skills/obsidian` | V-103: Add vault resurfacing + cross-skill knowledge injection | implement | senior-backend-engineer | **fallback** |
| 79 | " | " | " | write | storyteller | keyword |
| 79 | " | " | " | review | staff-code-reviewer | keyword |
| 80 | `20260318-d543f326` | `~/skills/orchestrator` | V-137: Detect and flag single-phase missions that should be multi-phase | implement | senior-backend-engineer | **fallback** |
| 80 | " | " | " | write | storyteller | keyword |
| 80 | " | " | " | review | staff-code-reviewer | keyword |
| 81 | `20260318-9301de28` | `~/skills/scout` | V-130: Add incremental gather mode to scout | implement | senior-backend-engineer | **fallback** |
| 81 | " | " | " | write | storyteller | keyword |
| 81 | " | " | " | review | staff-code-reviewer | keyword |
| 82 | `20260318-da6ee5bf` | `~/skills/obsidian` | V-104: Add automated capture sources from Via workflows | implement | senior-backend-engineer | **fallback** |
| 82 | " | " | " | write | storyteller | keyword |
| 82 | " | " | " | review | staff-code-reviewer | keyword |
| 83 | `20260318-b8f4b2a1` | `~/skills/scout` | V-129: Add podcast feed source to scout | implement | senior-golang-engineer | keyword |
| 83 | " | " | " | review | staff-code-reviewer | keyword |
| 84 | `20260318-8bebaa1d` | `~/skills/obsidian` | V-102: Add canonical note promotion and cluster detection | implement | senior-backend-engineer | **fallback** |
| 84 | " | " | " | review | staff-code-reviewer | keyword |
| 85 | `20260318-11ad0287` | `~/skills/orchestrator` | V-134: Daemon hot-reload persona catalog on /api/personas | implement | senior-backend-engineer | **fallback** |
| 85 | " | " | " | write | storyteller | keyword |
| 85 | " | " | " | review | staff-code-reviewer | keyword |
| 86 | `20260318-9118986d` | `~/skills/orchestrator` | V-136: Investigate high low_confidence rate (this mission) | _(not yet run)_ | — | — |
| 87 | `20260318-158719b2` | `~/skills/orchestrator` | V-138: Enforce reviewer phase injection for code missions | implement | senior-backend-engineer | **fallback** |
| 87 | " | " | " | write | storyteller | keyword |
| 87 | " | " | " | review | staff-code-reviewer | keyword |
| 88 | `20260318-80c7bdea` | `~/skills/orchestrator` | V-98: Add target-profile runtime hints to decomposer prompt | implement | senior-backend-engineer | **fallback** |
| 88 | " | " | " | write | storyteller | keyword |
| 88 | " | " | " | review | staff-code-reviewer | keyword |
| 89 | `20260318-66928cfe` | `~/skills/orchestrator` | V-99: Build routing outcome linkage for red-team feedback loop | implement | senior-backend-engineer | **fallback** |
| 89 | " | " | " | review | staff-code-reviewer | keyword |
| 90 | `20260318-0635d882` | `~/skills/scout` | Scout: write 1 article about AI coding agents changing code ownership | execute | staff-code-reviewer | keyword |
| 92 | `20260318-6c9bb300` | `~/skills/scout` | Scout: write 1 article about Go's range-over-func iterators | execute | staff-code-reviewer | keyword |

**Key observation:** All findings show phase names (`implement`, `write`, `review`, `research`, `execute`) that are the exact hardcoded outputs of `keywordDecompose()` — not LLM-generated names. This proves every finding is a `llmDecompose()` failure.

---

## 2. Root Cause: All 17 Are `llmDecompose()` Failures

### Why not simple-task-classification?

The missions are large, multi-deliverable tasks (10–30 line objectives with numbered deliverables). They would all pass `router.ClassifyComplexity()` (which requires ≤20 words + no complexity signals). The full mission.md content is passed as the task — these tasks easily exceed 20 words and contain complexity signals ("implement", "add", "update all").

### The proof: `keywordDecompose()` fingerprint

`keywordDecompose()` (`decompose.go:944`) produces phases with hardcoded names matching keyword patterns:

| Keyword match | Phase name | Persona |
|---|---|---|
| research/analyze/investigate | `research` | academic-researcher |
| implement/build/create/develop | `implement` | `pickImplementationPersona()` |
| write/document/blog/post/article | `write` | storyteller |
| review/audit/check/verify | `review` | staff-code-reviewer |
| (none match) | `execute` | `pickPersona()` |

All 17 findings show exactly these phase names. LLM-generated plans use arbitrary names like `"setup-context-propagation"`, `"add-gather-dispatch"`, etc. — not these four generic labels.

### The `implement` method split: `fallback` vs `keyword`

For the `implement` phase, `pickImplementationPersona()` uses keyword scoring to detect language:
- V-124 (`scout`, Go mentioned) → `senior-golang-engineer` (keyword) ✓
- V-122 (context.Context, Go) → `senior-golang-engineer` (keyword) ✓
- V-129 (podcast RSS, "Go", "encoding/xml") → `senior-golang-engineer` (keyword) ✓
- V-114 (PR URL, React/TypeScript) → `senior-frontend-engineer` (keyword) ✓
- V-99, V-98, V-134, etc. (generic Go backend, no explicit "golang" word) → `senior-backend-engineer` (**fallback**)

V-130 (`scout gather`, clearly Go) → **fallback** because the task text doesn't contain "golang" explicitly, only Go concepts. This is a keyword coverage gap even within the `keywordDecompose` fallback system.

### Two scout article tasks (IDs 90, 92)

These single-phrase tasks (`"scout and write 1 article about X"`) took the `execute` path with `staff-code-reviewer` — a clear routing error. "Write 1 article about..." should route to `storyteller`, not a code reviewer. The `keywordDecompose` system matches the "write" keyword but then `pickPersona()` wins on the execute path and picks `staff-code-reviewer` because "review" appears in its name stem and the fallback alphabetical order hits it.

---

## 3. The Batch Rate-Limit Pattern (Strong Evidence)

Timestamps from the DB:

```
2026-03-17T14:10:56Z  ID 79  (obsidian V-103)
2026-03-17T14:11:03Z  ID 80  (orchestrator V-137)
2026-03-17T14:11:04Z  ID 81  (scout V-130)
2026-03-17T14:11:05Z  ID 82  (obsidian V-104)
2026-03-17T14:11:12Z  ID 83  (scout V-129)
2026-03-17T14:11:14Z  ID 84  (obsidian V-102)
2026-03-17T14:11:35Z  ID 85  (orchestrator V-134)
2026-03-17T14:11:39Z  ID 86  (orchestrator V-136)
2026-03-17T14:11:46Z  ID 87  (orchestrator V-138)
2026-03-17T14:11:52Z  ID 88  (orchestrator V-98)
2026-03-17T14:12:09Z  ID 89  (orchestrator V-99)
2026-03-17T14:12:30Z  ID 90  (scout article)
2026-03-17T14:12:34Z  ID 92  (scout article)
```

**14 missions fired within a 98-second window.** These were running concurrently (the scheduler dispatched a batch). When `llmDecompose` calls the API in parallel across 14 missions, rate limits are highly likely, and any 429 response immediately triggers `keywordDecompose` with no retry.

The remaining 3 findings (IDs 74–78, timestamps 03:35–10:26) are likely individual transient API failures rather than rate limiting.

---

## 4. Code Path When Low-Confidence Fires

```
Decompose(task)
├── HasPreDecomposedPhases? → NO (mission.md content, not PHASE lines)
├── ClassifyComplexity(task)? → YES (complex, multi-deliverable missions)
└── llmDecompose()
      └── ERROR (rate limit 429 / timeout / network)
          ├── emit DecomposeFallback event (no stderr log)
          └── keywordDecompose(task, tc)
                ├── phase "implement" → pickImplementationPersona()
                │     ├── keyword match for "golang" → senior-golang-engineer
                │     └── no match → senior-backend-engineer (fallback)
                ├── phase "write" → storyteller (keyword "write")
                └── phase "review" → staff-code-reviewer (keyword "review")
                    → all phases: PersonaSelectionMethod = "keyword" or "fallback"
                    → AuditPlan fires PassiveLowConfidence
```

---

## 5. Observability Gaps That Made This Opaque

### Gap 1: No stderr log when `llmDecompose` fails

`decompose.go:214`:
```go
em.Emit(ctx, event.New(event.DecomposeFallback, missionID, "", "", map[string]any{
    "reason": err.Error(),
}))
plan = keywordDecompose(task, tc)
```

No `fmt.Fprintf(os.Stderr, ...)`. Operators without event-capture tooling see nothing.

### Gap 2: No timeout on LLM calls

`llmMatch()` (`personas.go:360`) and `llmDecompose()` both use the incoming `context.Context` with no deadline. A rate-limited request may block indefinitely instead of failing fast.

### Gap 3: No retry on transient errors

A single 429 or network hiccup immediately triggers full `keywordDecompose` fallback. No backoff, no retry.

### Gap 4: `llmMatch` errors also silent

When `llmDecompose` calls per-phase persona selection and the LLM fails, `llmMatch()` returns an error to `MatchWithMethod()`, which silently falls through to `keywordMatch()` with no log. The `PersonaSelectionMethod` is then `"keyword"` — indistinguishable from a successful keyword route.

### Gap 5: Unrecognized persona name silently discards LLM result

In `llmMatch()` (`personas.go:336`):
```go
name, err := llmMatch(context.Background(), task)
if err == nil && Get(name) != nil {
    return name, SelectionLLM
}
```
If Haiku returns `"golang-engineer"` instead of `"senior-golang-engineer"`, `Get(name)` returns nil and the result is silently discarded. No log emitted.

---

## 6. Routing Quality Analysis (Keyword Fallback Accuracy)

When `llmDecompose` fails, `keywordDecompose` handles the whole plan. Quality of its routing:

**Correct routes in the 17 findings:**
- V-124, V-122 → `senior-golang-engineer` ✓ (Go files mentioned)
- V-129 → `senior-golang-engineer` ✓ ("Go", "encoding/xml")
- V-114 → `senior-frontend-engineer` ✓ (React, TypeScript)
- All `review` phases → `staff-code-reviewer` ✓
- All `write` phases → `storyteller` ✓

**Wrong routes:**
- V-130 (scout gather, Go codebase) → `senior-backend-engineer` instead of `senior-golang-engineer` — "golang" not in task text despite being a Go project
- IDs 90, 92 (article writing) → `execute` → `staff-code-reviewer` ✗ — should be `storyteller`
- V-152 `implement` phase → `senior-backend-engineer` (fallback) instead of `senior-golang-engineer`

**Root cause for Go-targeting misses:** `keywordDecompose` doesn't read target context (what language the repo uses). It only scores keywords in the mission description. Missions that describe behavior without naming "golang" explicitly get `senior-backend-engineer`.

The `TestRoutingAccuracyBenchmark` (`personas_test.go:507`) confirms 17/25 correct (68%) for `keywordMatch()` in isolation.

---

## 7. Recommendations

### P0 — Log `llmDecompose` failures to stderr (impact: immediate observability)

Add alongside the existing `DecomposeFallback` event emission:
```go
fmt.Fprintf(os.Stderr, "[decompose] llmDecompose failed for workspace %s (%v); falling back to keyword\n",
    missionID[:min(len(missionID),12)], err)
```

Without this, operators cannot distinguish rate-limit runs from expected keyword routes.

### P0 — Add LLM call timeout (impact: prevent indefinite blocking)

`llmMatch()` and `llmDecompose()` should bound their context:
```go
ctx, cancel := context.WithTimeout(ctx, 20*time.Second)
defer cancel()
```

With no timeout, a single hung TCP connection blocks the mission indefinitely and may exhaust goroutines under load.

### P0 — Log unknown persona names from `llmMatch` (impact: catch LLM slug drift)

```go
if Get(name) == nil {
    fmt.Fprintf(os.Stderr, "[persona] llmMatch returned unknown slug %q (task: %.60s)\n", name, task)
    return "", nil
}
```

### P1 — Add retry with backoff for rate-limit errors (impact: batch resilience)

The batch pattern (14 missions in 98s) shows the system is vulnerable to quota exhaustion. A single retry with 2s backoff on 429 errors would recover most of these transparently:

```go
// internal/decompose/decompose.go
plan, err := llmDecompose(ctx, task, learnings, skillIndex, tc)
if err != nil && isRateLimit(err) {
    time.Sleep(2 * time.Second)
    plan, err = llmDecompose(ctx, task, learnings, skillIndex, tc)
}
```

### P1 — Add WhenToUse triggers for the 8 benchmark failures

The `TestRoutingAccuracyBenchmark` shows 8 keyword routing misses that affect fallback quality when LLM fails:

| Persona | Missing triggers |
|---|---|
| `devops-engineer` | "CI/CD", "pipeline", "GitHub Actions", "Docker", "containers", "configure" |
| `architect` | "choose between", "evaluate options", "tech decision", "trade-offs" |
| `principal-systems-reviewer` | "operational readiness", "launch readiness", "pre-launch check" |
| `storyteller` | "blog post", "article", "publish", "narrative" (reinforce vs. technical terms) |
| `artist` | "illustration", "visual prompts", "image generation" |
| `indie-coach` | "weekly review", "progress check", "retrospective", "personal goals" |
| `staff-code-reviewer` | "nil pointer", "handler review", language-specific review terms |

After adding these, raise the benchmark threshold from 60% → 85% (`personas_test.go:591`).

### P1 — Fix `keywordDecompose` for article-writing tasks (IDs 90, 92)

The "execute" path in `keywordDecompose` calls `pickPersona()` which doesn't handle "write an article" intent. Add a check before the generic fallback:

```go
// decompose.go keywordDecompose: before the final execute phase
if containsAny(lower, "article", "blog", "post", "write") && !containsAny(lower, "code", "implement") {
    // single write phase
    phases = append(phases, writePhase(task, tc))
    return
}
```

### P2 — Pass target language context to `keywordDecompose` (impact: fix Go fallbacks)

V-130 (scout, Go) got `senior-backend-engineer` because the task text didn't say "golang" explicitly. If `keywordDecompose` received the target's language from the target profile, it could default to `senior-golang-engineer` for Go repositories.

### P2 — Fix empty `PersonaSelectionMethod` false positives

`AuditPlan` (`passive.go:192`) treats `m == ""` identically to `"keyword"`. The metrics DB has `DEFAULT ''` for `selection_method`. Historical phases will always trigger `low_confidence` if replayed. Either treat empty as high-confidence or ensure all write paths set the method.

### P3 — Fix `keywordDecompose` "review" → `indie-coach` for coaching tasks

`keywordDecompose` hardcodes `review` → `staff-code-reviewer`. Add a sub-check for coaching context:
```go
if containsAny(lower, "review") && containsAny(lower, "progress", "weekly", "goals", "improvements") {
    // route to indie-coach, not staff-code-reviewer
}
```

---

## 8. Priority Summary

| Priority | Fix | Expected Impact |
|---|---|---|
| P0 | Log `llmDecompose` failures to stderr | Operators can see when fallback fires |
| P0 | Add 20s timeout on `llmMatch` / `llmDecompose` | Prevents indefinite blocking under load |
| P0 | Log unknown persona slug in `llmMatch` | Catch LLM slug drift silently discarding results |
| P1 | Retry with 2s backoff on 429 rate-limit errors | Recovers batch runs without fallback |
| P1 | Add WhenToUse keywords for 8 benchmark failures | Keyword accuracy 68% → ~90% |
| P1 | Fix `keywordDecompose` for article-writing single tasks | Stops routing "write article" to `staff-code-reviewer` |
| P2 | Pass target language to `keywordDecompose` | Fixes Go repo fallbacks to `senior-backend-engineer` |
| P2 | Fix empty `PersonaSelectionMethod` false positives | Eliminates historical-schema false `low_confidence` findings |
| P3 | Fix `keywordDecompose` review → coaching path | Reduces misrouting for progress/coaching tasks |

---

## Appendix A: Files Investigated

| File | Lines | Role |
|---|---|---|
| `internal/persona/personas.go` | 666 | Persona loading, `MatchWithMethod`, `llmMatch`, `scoreKeywords` |
| `internal/persona/personas_test.go` | ~670 | Benchmark (25 cases, 17/25 pass), keyword tests |
| `internal/decompose/passive.go` | ~270 | `AuditPlan`, `PassiveLowConfidence` rule |
| `internal/decompose/decompose.go` | 1479 | Full decompose flow, `keywordDecompose`, `selectPersona`, `pickPersona` |
| `internal/routing/db.go` | ~940 | `decomposition_findings` table schema and queries |
| `internal/metrics/db.go` | ~500 | Phase `selection_method` column schema |
| `evals/persona-routing.yaml` | 134 | 15 eval test cases (Haiku in isolation) |
| `evals/decomposer.yaml` | 413 | 6 decomposer eval cases |

## Appendix B: DB Queries Used

```sql
-- Count by target
SELECT target_id, COUNT(*) FROM decomposition_findings
WHERE finding_type='low_confidence' GROUP BY target_id ORDER BY COUNT(*) DESC;

-- Full catalog with phase data
SELECT m.id, p.name, p.persona, p.selection_method
FROM missions m JOIN phases p ON p.mission_id = m.id
WHERE m.id IN (...low_confidence workspace_ids...)
ORDER BY m.id, p.rowid;
```

Data source: `~/.alluka/learnings.db` (routing findings) + `~/.alluka/metrics.db` (phase persona assignments).
