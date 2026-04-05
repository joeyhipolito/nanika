# Architecture: Audit Package

## Context

The orchestrator runs multi-phase missions — decompose a task, assign personas, spawn workers, collect output. Today there is no post-hoc evaluation of whether the decomposition was good, whether personas were correctly assigned, whether workers actually followed their objectives, or whether the output delivered user value. The only quality signal is `engine.CheckGate()`, which checks that output is non-empty and not suspiciously short.

**What exists:**
- `core.Checkpoint` embeds the full `Plan` with per-phase runtime state (status, output, timings, retries, gate results)
- `engine.MissionMetrics` / `PhaseMetric` captures quantitative execution data (durations, retry counts, output lengths)
- `persona` package loads the full persona catalog with WhenToUse/WhenNotUse/HandsOffTo
- `learning.ComplianceScan()` does keyword-overlap checking for learning compliance
- `worker.LoadSkillIndex()` / `FormatSkillsForDecomposer()` loads the skill routing index
- `decompose.loadRulesFromSKILLMD()` loads canonical decomposition rules
- `sdk.QueryText()` provides one-shot LLM calls via Claude CLI
- `router.Resolve(router.TierThink)` → "opus" for complex evaluation
- Worker outputs live at `{wsPath}/workers/{name}/output.md`
- Metrics JSONL at `~/.via/metrics.jsonl`

**What's needed:** A package that loads a completed mission's full execution record, sends it through an LLM evaluator (Opus-tier) with the persona catalog and skill index as context, and produces a structured audit report with per-phase evaluations, an overall scorecard, and actionable recommendations.

**Constraints:** Solo developer. No new infrastructure. Reuse existing packages. Output must be useful in under 30 seconds (single LLM call, not multi-turn).

## Decision

Single `audit` package under `internal/audit/` with a flat structure: types, evaluation logic, prompt building, and report formatting. One LLM call (Opus) evaluates the entire mission at once — no per-phase calls. The audit consumes the checkpoint, worker outputs, persona catalog, and skill index as inputs. CLI subcommand `orchestrator audit` takes a workspace path or ID.

Chose a single-call approach over per-phase evaluation because:
- Opus can handle the full context in one pass (missions are 3-12 phases)
- Cross-phase analysis (was the decomposition coherent?) requires seeing all phases together
- Per-phase calls would cost 3-12x more and take 3-12x longer

## Candidates Considered

| Approach | Effort | Ops Complexity | Extensibility | Fit |
|----------|--------|----------------|---------------|-----|
| Single LLM call, structured JSON output | Low | Low (one call) | Medium | **Selected** — fast, simple, sufficient |
| Per-phase LLM calls + aggregation | High | Medium (N calls) | High | Over-engineered for solo use |
| Rule-based only (no LLM) | Low | Low | Low | Too rigid — misses semantic quality |
| Hybrid: rules first, LLM for edge cases | Medium | Low | Medium | Premature — start with LLM-only, add rules if patterns emerge |

## Component Map

```
internal/audit/
├── DESIGN.md       ← this file
├── types.go        ← AuditReport, MissionEvaluation, Recommendation, Scorecard,
│                      ChangeRecord, DecomposerConvergenceStatus
├── evaluate.go     ← EvaluateMission() — loads data, calls LLM, parses response
├── prompt.go       ← buildEvaluationPrompt() — assembles the LLM evaluation prompt
└── report.go       ← FormatText(), FormatJSON(), FormatMarkdown() — output rendering

internal/cmd/
└── audit.go        ← CLI subcommand registration and flag handling
```

**Data flow:**

```
checkpoint.json ─┐
worker outputs ──┤
persona catalog ─┼──→ buildEvaluationPrompt() ──→ sdk.QueryText(opus) ──→ parseReport() ──→ AuditReport
skill index ─────┤                                                                             │
SKILL.md rules ──┘                                                                    FormatText/JSON/Markdown
```

## Structs (types.go)

```go
package audit

import "time"

// AuditReport is the top-level output of a mission audit.
type AuditReport struct {
    WorkspaceID string              `json:"workspace_id"`
    Task        string              `json:"task"`
    AuditedAt   time.Time           `json:"audited_at"`
    Scorecard   Scorecard           `json:"scorecard"`
    Evaluation  MissionEvaluation   `json:"evaluation"`
    Phases      []PhaseEvaluation   `json:"phases"`
    Convergence ConvergenceStatus   `json:"convergence"`
    Changes     []ChangeRecord      `json:"changes"`
}

// Scorecard is the quantitative summary — 5 axes, each 1-5.
type Scorecard struct {
    DecompositionQuality int `json:"decomposition_quality"` // Was the task broken down well?
    PersonaFit           int `json:"persona_fit"`            // Were the right personas assigned?
    SkillUtilization     int `json:"skill_utilization"`      // Were available skills used effectively?
    OutputQuality        int `json:"output_quality"`         // Did outputs meet objectives?
    RuleCompliance       int `json:"rule_compliance"`        // Did decomposition follow SKILL.md rules?
    Overall              int `json:"overall"`                // Weighted average (computed, not LLM-generated)
}

// MissionEvaluation is the LLM's qualitative assessment of the mission as a whole.
type MissionEvaluation struct {
    Summary         string           `json:"summary"`          // 2-3 sentence verdict
    Strengths       []string         `json:"strengths"`        // What went well
    Weaknesses      []string         `json:"weaknesses"`       // What went wrong
    Recommendations []Recommendation `json:"recommendations"`  // Actionable improvements
}

// PhaseEvaluation is the LLM's assessment of a single phase.
type PhaseEvaluation struct {
    PhaseID         string   `json:"phase_id"`
    PhaseName       string   `json:"phase_name"`
    PersonaAssigned string   `json:"persona_assigned"`
    PersonaIdeal    string   `json:"persona_ideal"`     // What the LLM thinks it should have been
    PersonaCorrect  bool     `json:"persona_correct"`   // Does assigned == ideal?
    ObjectiveMet    bool     `json:"objective_met"`      // Did the output satisfy the objective?
    Issues          []string `json:"issues"`             // Specific problems found
    Score           int      `json:"score"`              // 1-5 for this phase
}

// Recommendation is a specific, actionable improvement.
type Recommendation struct {
    Category string `json:"category"` // "decomposition", "persona", "skill", "process"
    Priority string `json:"priority"` // "high", "medium", "low"
    Summary  string `json:"summary"`  // One-line description
    Detail   string `json:"detail"`   // How to implement it
}

// ChangeRecord captures a concrete file or system change made during the mission.
// Extracted from worker outputs by scanning for tool-use patterns.
type ChangeRecord struct {
    PhaseID   string `json:"phase_id"`
    PhaseName string `json:"phase_name"`
    Type      string `json:"type"`  // "file_created", "file_modified", "command_run", "api_called"
    Target    string `json:"target"` // file path, command, or API endpoint
    Summary   string `json:"summary"`
}

// ConvergenceStatus evaluates whether the decomposer's plan and the actual
// execution converged — did phases do what was planned, or did workers drift?
type ConvergenceStatus struct {
    Converged     bool     `json:"converged"`      // Overall: did execution match the plan?
    DriftPhases   []string `json:"drift_phases"`   // Phase names where output diverged from objective
    MissingPhases []string `json:"missing_phases"` // Work that should have been a phase but wasn't
    RedundantWork []string `json:"redundant_work"`  // Overlapping work between phases
    Assessment    string   `json:"assessment"`      // LLM narrative on convergence
}
```

**Design rationale for struct choices:**

- `Scorecard` uses 1-5 integers, not floats. Humans understand "3 out of 5" instantly. The LLM produces integers more reliably than calibrated floats.
- `Overall` is computed as `(decomp + persona + skill + output + rules) / 5`, not LLM-generated. Keeps the aggregate deterministic.
- `PhaseEvaluation.PersonaIdeal` lets us catch systematic persona misassignment across missions (e.g., "architect was used for implementation 4 times this week").
- `ChangeRecord` is extracted from worker output, not from the LLM evaluation. It's a factual record, not a judgment. This gives us a manifest of what the mission actually touched.
- `ConvergenceStatus` is the key insight the current system lacks: did the decomposer's plan survive contact with reality?

## Interfaces (evaluate.go)

```go
package audit

import (
    "context"

    "github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// EvaluateOptions configures the audit evaluation.
type EvaluateOptions struct {
    WorkspacePath string // full path to workspace dir
    Model         string // override model (default: opus via router.TierThink)
    Verbose       bool   // emit progress to stderr
}

// EvaluateMission loads a completed mission and produces an audit report.
// Steps:
//   1. Load checkpoint from workspace
//   2. Read worker outputs from workers/{name}/output.md
//   3. Load persona catalog via persona.All()
//   4. Load skill index via worker.LoadSkillIndex()
//   5. Load decomposition rules via decompose rules path
//   6. Build evaluation prompt
//   7. Call sdk.QueryText() with opus model
//   8. Parse structured JSON from LLM response
//   9. Compute derived fields (Overall score, ChangeRecords from outputs)
//  10. Return AuditReport
func EvaluateMission(ctx context.Context, opts EvaluateOptions) (*AuditReport, error)

// loadWorkerOutputs reads all output.md files from the workspace workers/ dir.
// Returns map[phaseName]outputText.
func loadWorkerOutputs(wsPath string) (map[string]string, error)

// extractChanges scans worker outputs for file creation/modification patterns.
// Looks for Write/Edit/Bash tool use patterns in Claude output.
func extractChanges(phases []*core.Phase, outputs map[string]string) []ChangeRecord

// parseReport extracts the AuditReport from the LLM's JSON response.
// The LLM is instructed to output JSON between ```json fences.
func parseReport(raw string, wsID, task string) (*AuditReport, error)
```

## LLM Evaluation Prompt (prompt.go)

The prompt is the core deliverable. It must:
1. Give the LLM the full mission context (task, plan, outputs)
2. Provide the evaluation criteria (persona catalog rules, decomposition rules)
3. Request structured JSON output that maps to our structs
4. Be critical — the default should be "what went wrong" not "everything is great"

```go
// buildEvaluationPrompt assembles the full evaluation prompt.
// Inputs are pre-loaded by EvaluateMission().
func buildEvaluationPrompt(
    task string,
    plan *core.Plan,
    outputs map[string]string,
    personaCatalog string,
    skillIndex string,
    decompositionRules string,
) string
```

**Prompt template:**

```
You are a mission auditor for an AI orchestration system. Your job is to
critically evaluate a completed mission — identifying what went wrong, what
could be improved, and whether the decomposition and execution were sound.

Be harsh. The goal is to surface problems, not to be encouraging. A score
of 3/5 means "acceptable." 4/5 means "good." 5/5 is rare and means
"could not be improved." 1-2/5 means serious problems.

## Evaluation Criteria

### Persona Catalog
For each phase, check whether the assigned persona was the best match.
The persona's "When to Use" and "When NOT to Use" sections define the
boundaries. A backend-engineer doing architecture work is a misassignment.
A storyteller doing code review is a misassignment.

{personaCatalog}

### Decomposition Rules (from SKILL.md)
These are the canonical rules the decomposer should have followed.
Check whether the plan violated any of them.

{decompositionRules}

### Available Skills
Check whether phases that could have benefited from a skill were assigned it,
and whether phases were assigned skills they didn't need.

{skillIndex}

## Mission Under Audit

### Original Task
{task}

### Decomposed Plan
{planSummary — phase name, objective, persona, skills, dependencies, status}

### Phase Outputs
{for each phase: "## Phase: {name} (persona: {persona}, status: {status})\n{output truncated to 3000 chars}"}

## Output Format

Respond with a JSON object between ```json and ``` fences. The JSON must
match this exact structure:

{jsonSchema}

### Scoring Guide
- decomposition_quality: Did the decomposition follow SKILL.md rules?
  Correct number of phases? Good boundaries? Proper dependency chains?
- persona_fit: Were personas correctly matched to phase objectives?
  Check against WhenToUse/WhenNotToUse.
- skill_utilization: Were available skills assigned when beneficial?
  Were unnecessary skills assigned?
- output_quality: Did phase outputs actually accomplish their objectives?
  Were outputs substantive or thin?
- rule_compliance: Did the decomposition follow the specific rules in
  the Decomposition Rules section?

### Convergence Assessment
For the convergence field, evaluate:
- Did each phase's output match its stated objective, or did workers drift?
- Was there work done that should have been planned as a separate phase?
- Did multiple phases duplicate effort?

Be specific. Name phases. Quote rules that were violated. Give concrete
recommendations, not vague advice like "improve decomposition."
```

**Why this prompt shape:**

- **Persona catalog injected in full** rather than summarized, because the LLM needs the actual WhenToUse/WhenNotUse text to judge persona fit. `persona.FormatForDecomposer()` is too compressed — it drops WhenNotUse.
- **Output truncated to 3000 chars per phase** to fit in context. Most worker outputs are 2-5K chars; truncation loses tail-end detail but preserves the substantive work.
- **JSON fences** rather than raw JSON because Claude is more reliable with fenced output blocks than naked JSON responses. `parseReport()` extracts between the fences.
- **Harsh default** because evaluation systems that default to positive are useless. Every mission getting 4-5/5 teaches nothing.

## CLI Interface (cmd/audit.go)

```
orchestrator audit [workspace-id-or-path] [flags]

Flags:
  --format string    Output format: text, json, markdown (default "text")
  --model string     Override evaluation model (default: opus)
  --last int         Audit the Nth most recent mission (default 1 = latest)
  --verbose          Show evaluation progress

Examples:
  orchestrator audit                          # audit the most recent mission
  orchestrator audit 20260228-f12991ff        # audit by workspace ID
  orchestrator audit --last 3                 # audit 3rd most recent
  orchestrator audit --format json            # machine-readable output
  orchestrator audit --format markdown        # for pasting into notes
```

**Argument resolution logic:**

```go
func resolveWorkspace(arg string, last int) (string, error) {
    // 1. If arg looks like a path (contains /), use directly
    // 2. If arg looks like a workspace ID (YYYYMMDD-hex), resolve to ~/.via/workspaces/{id}
    // 3. If no arg, use --last to pick from ListWorkspaces()
    // 4. Validate with core.ValidateWorkspacePath()
}
```

**Text output format (default):**

```
╔══════════════════════════════════════════════╗
║  Mission Audit: 20260228-f12991ff            ║
╠══════════════════════════════════════════════╣

Task: Design the audit package architecture...

Scorecard
─────────────────────────────
  Decomposition   ████░  4/5
  Persona Fit     ███░░  3/5
  Skill Usage     ██░░░  2/5
  Output Quality  ████░  4/5
  Rule Compliance ████░  4/5
  ─────────────────────────
  Overall         ███▒░  3/5

Summary
───────
The mission decomposed correctly into 4 phases but failed to
assign skills to 3 phases that needed them. The scaffold and
core-commands phases ran in parallel despite core-commands
having a DEPENDS on scaffold.

Phases
──────
  ✓ scaffold (backend-engineer) .............. 4/5
  ✓ core-commands (backend-engineer) ......... 4/5
  ✗ admin-ui (backend-engineer) .............. 3/5
    → Should have used frontend-engineer persona
  ✓ plugin (backend-engineer) ................ 4/5

Convergence: DRIFT DETECTED
  → admin-ui: output focused on Go templates but
    objective specified htmx + Tailwind CSS patterns
  → core-commands: duplicated schema setup already done in scaffold

Recommendations
───────────────
  [HIGH] Assign frontend-engineer persona when the objective
         mentions "admin UI", "calendar view", or any UI work
  [MED]  Add golang-pro skill to phases doing Go implementation
  [LOW]  Merge scaffold + plugin into single phase — both are
         project setup work
```

## Report Rendering (report.go)

```go
// FormatText renders the audit report as colored terminal output.
// Uses ANSI escape codes for the progress bars and status indicators.
func FormatText(report *AuditReport) string

// FormatJSON renders the audit report as pretty-printed JSON.
func FormatJSON(report *AuditReport) (string, error)

// FormatMarkdown renders the audit report as markdown for note-capture tools.
func FormatMarkdown(report *AuditReport) string
```

## Risks

1. **LLM output parsing fragility.** The LLM might not produce valid JSON, or might include commentary outside the fences. Mitigation: `parseReport()` uses regex to extract between ` ```json ` and ` ``` ` fences, with a fallback that tries to parse the entire response as JSON. If both fail, return a degraded report with the raw LLM text as `Evaluation.Summary`.

2. **Context window limits.** A 12-phase mission with 5K chars per output = 60K chars of output alone, plus persona catalog and rules. With Opus's 200K context, this fits comfortably. But if worker outputs grow (e.g., a phase that generates 50K of code), the 3K-per-phase truncation becomes important. Mitigation: truncate with a note `[truncated from {N} chars]` so the LLM knows it's incomplete.

3. **Evaluation quality drift.** The LLM might give inflated scores or generic recommendations. Mitigation: the prompt explicitly says "be harsh" and "5/5 is rare." Track score distributions over time via metrics.jsonl to detect inflation.

4. **Persona catalog changes.** If personas are added/renamed, historical audits might reference non-existent personas in `PersonaIdeal`. This is acceptable — the audit is a point-in-time evaluation, not a live reference.

5. **Cost.** One Opus call per audit. At ~$15/1M input tokens, a typical audit prompt (~30K tokens) costs ~$0.45. Acceptable for post-hoc evaluation, but not for running on every mission automatically. The CLI is manual-trigger only.

## Trade-offs Accepted

- **Single LLM call over per-phase analysis.** Gives up granular per-phase depth in exchange for cross-phase coherence analysis and 1/N cost. The convergence assessment specifically requires seeing all phases together.

- **Truncated worker outputs.** Gives up evaluation of full output detail in exchange for fitting within a single context window. The 3K limit captures the substantive work in most cases; implementation details in the tail are less relevant for architectural audit.

- **No automatic trigger.** Gives up continuous quality monitoring in exchange for zero overhead on normal missions. If a pattern emerges where audits are always run, we can add a `--audit` flag to `orchestrator run` later.

- **No persistence of audit reports.** Audit output goes to stdout. No database, no JSONL log. If we later want to track score trends, we add `RecordAudit()` following the `engine.RecordMetrics()` pattern. Not doing it now because the value is unclear until we see real audit data.

- **JSON-in-markdown fence parsing over function calling.** Claude's function calling / tool_use could produce structured output more reliably. But `sdk.QueryText()` is a one-shot text interface — adding structured output would require changing the SDK. The fence pattern is good enough and keeps the SDK unchanged.

- **`ChangeRecord` is best-effort extraction.** Scanning worker outputs for file paths and tool patterns is heuristic. It'll miss some changes and might false-positive on quoted paths in documentation. Acceptable for an audit overview — if precise change tracking is needed, the event log is the authoritative source.

---

DECISION: Flat package, single LLM call, JSON-in-fences output parsing. No new dependencies. Reuses checkpoint, persona, sdk, worker, and router packages. CLI is `orchestrator audit` with workspace resolution and format flags.

PATTERN: Inject the full evaluation criteria (persona catalog, SKILL.md rules) into the LLM prompt rather than expecting the LLM to know them. The LLM evaluates against the specific rules we provide, not its general training. This makes audits reproducible when rules change.

LEARNING: Worker outputs are the primary evidence for evaluation quality, but they vary wildly in length (1K-50K chars). Truncation with explicit markers is better than hoping everything fits — the LLM can reason about "this output was truncated from 47K chars" as a signal itself.

GOTCHA: `persona.FormatForDecomposer()` only includes Name + Title + WhenToUse + HandsOffTo. For audit, we need WhenNotUse too — a dedicated `FormatForAudit()` function or direct iteration over `persona.All()` is needed. Don't reuse the decomposer summary for evaluation.
