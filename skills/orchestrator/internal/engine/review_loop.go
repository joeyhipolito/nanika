package engine

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
)

var reviewMdCaseInsensitivePattern = regexp.MustCompile(`(?i)(^|[-_])review\.md$`)

// legacyVerdictLinePattern matches a bare verdict label on the first line of an
// artifact — e.g. "FAIL: blockers found" or "PASS" — that predates the rule
// requiring artifacts to open with YAML frontmatter ("---" on line 1). A single
// matching line is stripped before parsing so old artifacts still load cleanly.
//
// The pattern requires the word to be followed by optional whitespace+colon or
// end-of-string so compound words like "fail-safe" and "pass-through" are not
// stripped: `\b` alone matches at punctuation boundaries including "-".
var legacyVerdictLinePattern = regexp.MustCompile(`(?i)^(fail|pass)(\s*$|\s*:)`)

// ReviewFindings holds structured output parsed from a staff-code-reviewer phase.
// The raw review text is not stored here — it reaches the fix phase automatically
// via the sequential executor's priorContext chain (outputs slice in engine.go).
type ReviewFindings struct {
	Blockers []ReviewItem // BLOCKER-severity findings
	Warnings []ReviewItem // WARNING-severity findings
}

// Passed reports whether the review found no blockers.
func (f ReviewFindings) Passed() bool { return len(f.Blockers) == 0 }

// ReviewItem is a single finding from a code review.
type ReviewItem struct {
	Location    string // "file.go:42" or empty
	Description string // the full finding text
}

// ParseReviewFindings extracts structured findings from a staff-code-reviewer
// output. It looks for "### Blockers" and "### Warnings" sections and collects
// "- **[" prefixed items until the next "###" header or end of string.
//
// Parsing is fail-open: malformed or empty output returns Passed()==true so
// a mis-formatted review never injects a spurious fix phase.
func ParseReviewFindings(output string) ReviewFindings {
	var f ReviewFindings
	if output == "" {
		return f
	}

	// Legacy compatibility: strip a single leading verdict line (e.g. "FAIL: …"
	// or "PASS") that predates the YAML-frontmatter-first rule. Only the very
	// first non-empty line is candidates; subsequent lines are untouched.
	output = stripLegacyVerdictLine(output)

	lines := strings.Split(output, "\n")
	f.Blockers = parseSection(lines, "### Blockers")
	f.Warnings = parseSection(lines, "### Warnings")
	return f
}

// ParseReviewFindingsFromArtifact reads the file at artifactPath and parses
// it with ParseReviewFindings. Returns an os.ErrNotExist-wrapped error when
// the file is absent so callers can distinguish "no artifact" from "artifact
// present but unparseable" (the latter returns nil error with empty findings).
func ParseReviewFindingsFromArtifact(artifactPath string) (ReviewFindings, error) {
	data, err := os.ReadFile(artifactPath)
	if err != nil {
		return ReviewFindings{}, err
	}
	return ParseReviewFindings(string(data)), nil
}

// findReviewMdCaseInsensitive attempts to locate a review artifact in the given
// directory. It accepts both the canonical "review.md" and custom-slugged variants
// like "prompt-tune-greeting-review.md" or "review-re-review.md" by matching
// the pattern (?i)(^|[-_])review\.md$. When multiple matches exist the
// most-recently-modified file wins.
func findReviewMdCaseInsensitive(dir string) string {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return ""
	}

	var bestPath string
	var bestMtime int64
	for _, entry := range entries {
		if entry.IsDir() || !reviewMdCaseInsensitivePattern.MatchString(entry.Name()) {
			continue
		}
		full := filepath.Join(dir, entry.Name())
		fi, err := entry.Info()
		if err != nil {
			continue
		}
		mtime := fi.ModTime().UnixNano()
		if bestPath == "" || mtime > bestMtime {
			bestPath = full
			bestMtime = mtime
		}
	}
	return bestPath
}

// hasReviewHeaders reports whether text contains both "### Blockers" and
// "### Warnings" as line-start headers. Used to distinguish a well-formed
// review artifact (even when both sections are empty) from unstructured prose.
func hasReviewHeaders(text string) bool {
	hasBlockers, hasWarnings := false, false
	for _, line := range strings.Split(text, "\n") {
		trimmed := strings.TrimSpace(line)
		if trimmed == "### Blockers" {
			hasBlockers = true
		} else if trimmed == "### Warnings" {
			hasWarnings = true
		}
		if hasBlockers && hasWarnings {
			return true
		}
	}
	return false
}

// reviewOutputLooksMalformed reports whether the reviewer output is non-empty,
// has no parsed findings, AND lacks a structural header at the start of any
// line. Line-start matching prevents prose mentions of the format string
// (e.g. backtick-quoted `### Blockers` inside a scratchpad note) from
// defeating the safeguard.
func reviewOutputLooksMalformed(output string, findings ReviewFindings) bool {
	trimmed := strings.TrimSpace(output)
	if trimmed == "" {
		return false
	}
	if len(findings.Blockers) > 0 || len(findings.Warnings) > 0 {
		return false
	}
	for _, line := range strings.Split(trimmed, "\n") {
		line = strings.TrimSpace(line)
		if line == "### Blockers" || line == "### Warnings" {
			return false
		}
	}
	return true
}

// parseSection scans lines for a header matching sectionHeader, then collects
// items prefixed with "- **[" or "- **`" until the next "###" header or end of slice.
// Each item may span multiple lines (e.g., "Fix:" continuations).
func parseSection(lines []string, sectionHeader string) []ReviewItem {
	var items []ReviewItem
	inSection := false
	var current *ReviewItem

	flush := func() {
		if current != nil {
			current.Description = strings.TrimSpace(current.Description)
			items = append(items, *current)
			current = nil
		}
	}

	for _, line := range lines {
		trimmed := strings.TrimSpace(line)

		if strings.HasPrefix(trimmed, "### ") {
			if inSection {
				// Leaving the section — flush any in-progress item and stop.
				flush()
				break
			}
			if trimmed == sectionHeader {
				inSection = true
			}
			continue
		}

		if !inSection {
			continue
		}

		// Match both "- **[" (bracket) and "- **`" (backtick) formats.
		if strings.HasPrefix(trimmed, "- **[") || strings.HasPrefix(trimmed, "- **`") {
			flush()
			loc, desc := parseItemLine(trimmed)
			current = &ReviewItem{Location: loc, Description: desc}
			continue
		}

		// Continuation line for the current item (e.g., Fix: …, Why: …)
		if current != nil && trimmed != "" {
			current.Description += " " + trimmed
		}
	}
	flush()
	return items
}

// parseItemLine extracts the location and description from a line like:
//
//   - **[file.go:42]** Description text.
//   - **`file.go:42`** Description text.
//
// Supports both [location] (bracket) and `location` (backtick) formats.
// Returns ("", trimmed) when the location notation is absent.
func parseItemLine(line string) (location, description string) {
	// Strip leading "- "
	line = strings.TrimPrefix(line, "- ")

	// Extract **[location]** prefix if present (bracket format).
	if strings.HasPrefix(line, "**[") {
		end := strings.Index(line, "]**")
		if end > 0 {
			location = line[3:end]
			description = strings.TrimSpace(line[end+3:])
			return
		}
	}

	// Extract **`location`** prefix if present (backtick format).
	if strings.HasPrefix(line, "**`") {
		end := strings.Index(line, "`**")
		if end > 0 {
			location = line[3:end]
			description = strings.TrimSpace(line[end+3:])
			return
		}
	}

	description = line
	return
}

// stripLegacyVerdictLine removes a single leading line from s when that line
// matches legacyVerdictLinePattern (case-insensitive FAIL/PASS prefix). Only
// the first non-empty line is checked; everything else is returned unchanged.
func stripLegacyVerdictLine(s string) string {
	idx := strings.IndexByte(s, '\n')
	if idx < 0 {
		// Single-line input: strip entirely if it matches, otherwise keep.
		if legacyVerdictLinePattern.MatchString(strings.TrimSpace(s)) {
			return ""
		}
		return s
	}
	firstLine := strings.TrimSpace(s[:idx])
	if legacyVerdictLinePattern.MatchString(firstLine) {
		return s[idx+1:]
	}
	return s
}

// IsReviewGate reports whether p should trigger the autonomous review loop.
// There are two supported forms:
//   - auto-injected required review gates
//   - explicit reviewer phases authored in predecomposed missions, but only
//     when they are real code-review steps over implementation output
//
// The explicit path is intentionally narrow so standalone review-only missions
// or advisory phases do not unexpectedly start injecting fix loops.
func (e *Engine) IsReviewGate(p *core.Phase) bool {
	if p == nil {
		return false
	}
	if p.PersonaSelectionMethod == core.SelectionRequiredReview {
		return true
	}
	if !isLoopableReviewPersona(p.Persona) || p.Role != core.RoleReviewer {
		return false
	}
	for _, depID := range p.Dependencies {
		dep, ok := e.phases[depID]
		if ok && dep != nil && dep.Role == core.RoleImplementer {
			return true
		}
	}
	return false
}

func isLoopableReviewPersona(name string) bool {
	// Primary: check frontmatter role=reviewer with code review capability.
	// Only personas with "code review" capability produce structured output
	// (### Blockers / ### Warnings) that the fix loop can parse and act on.
	if persona.HasRole(name, "reviewer") && persona.HasCapability(name, "code review") {
		return true
	}
	// Fallback: hardcoded slug for personas without frontmatter.
	return strings.ToLower(name) == "staff-code-reviewer"
}

// defaultMaxReviewLoops is the loop bound used when Phase.MaxReviewLoops == 0.
const defaultMaxReviewLoops = 2

// defaultMaxParseRetries is the maximum number of times a review phase can be
// retried when the output is malformed (missing ### Blockers / ### Warnings).
const defaultMaxParseRetries = 1

const maxFixObjectiveSummaryLen = 1200

// injectFixPhase creates a fix phase and appends it to e.plan.Phases.
// Returns the new phase, or nil when:
//   - findings.Passed() (no blockers)
//   - reviewPhase.ReviewIteration >= effective max loops
//   - reviewPhase has no implementation dependencies to copy persona from
//
// The fix phase:
//   - Uses the persona of the first implementation dependency of the review phase.
//   - Has a concise objective listing blocker summaries (the full review arrives
//     via the prior context system — no need to duplicate it here).
//   - Inherits TargetDir and Skills from the implementation phase.
//   - Depends on the review phase so it receives review output as prior context.
func (e *Engine) injectFixPhase(reviewPhase *core.Phase, findings ReviewFindings) *core.Phase {
	if findings.Passed() {
		return nil
	}

	maxLoops := reviewPhase.MaxReviewLoops
	if maxLoops <= 0 {
		maxLoops = defaultMaxReviewLoops
	}
	if reviewPhase.ReviewIteration >= maxLoops {
		return nil
	}

	// Find the first implementation dependency to inherit persona/skills/target.
	var implPhase *core.Phase
	for _, depID := range reviewPhase.Dependencies {
		if dep, ok := e.phases[depID]; ok {
			implPhase = dep
			break
		}
	}
	if implPhase == nil {
		return nil
	}

	// Build a concise objective: "Fix the following blockers: <summaries>."
	// Full review content arrives via the prior context plumbing, so we only
	// need a short reference here to avoid double-injecting the review.
	var summaries []string
	for _, b := range findings.Blockers {
		s := b.Description
		if b.Location != "" {
			s = fmt.Sprintf("[%s] %s", b.Location, b.Description)
		}
		summaries = append(summaries, truncateReviewSummary(s))
	}
	joinedSummaries := strings.Join(summaries, "; ")
	if len(joinedSummaries) > maxFixObjectiveSummaryLen {
		joinedSummaries = joinedSummaries[:maxFixObjectiveSummaryLen] + "..."
	}
	objective := fmt.Sprintf(
		"Fix the following code review blockers identified in the prior review phase: %s",
		joinedSummaries,
	)

	fixPhase := &core.Phase{
		ID:              fmt.Sprintf("phase-%d", len(e.plan.Phases)+1),
		Name:            "fix",
		Objective:       objective,
		Persona:         implPhase.Persona,
		ModelTier:       implPhase.ModelTier,
		Runtime:         implPhase.Runtime,
		Skills:          append([]string{}, implPhase.Skills...), // defensive copy
		TargetDir:       implPhase.TargetDir,
		Dependencies:    []string{reviewPhase.ID},
		Status:          core.StatusPending,
		Role:            core.RoleImplementer, // fix phases are always implementer work
		ReviewIteration: reviewPhase.ReviewIteration + 1,
		OriginPhaseID:   implPhase.ID,
		MaxReviewLoops:  reviewPhase.MaxReviewLoops, // unused in fix phases; propagated for inspection
	}

	e.plan.Phases = append(e.plan.Phases, fixPhase)
	return fixPhase
}

func truncateReviewSummary(summary string) string {
	const maxSummaryLen = 240
	summary = strings.TrimSpace(summary)
	if len(summary) <= maxSummaryLen {
		return summary
	}
	return summary[:maxSummaryLen] + "..."
}

func flattenReviewItems(items []ReviewItem) []string {
	out := make([]string, len(items))
	for i, item := range items {
		if item.Location != "" {
			out[i] = fmt.Sprintf("[%s] %s", item.Location, item.Description)
		} else {
			out[i] = item.Description
		}
	}
	return out
}

// injectRetryReviewPhase creates a retry review gate when the original review
// returned non-empty but unstructured output (missing ### Blockers / ### Warnings
// sections). The retry depends on the same phases as the original review so the
// reviewer sees the same implementation output without any fix being applied.
//
// Returns nil when:
//   - The parse-failure counter is already at its max (fail-closed), OR
//   - The loop bound is already exhausted (ReviewIteration >= max).
func (e *Engine) injectRetryReviewPhase(review *core.Phase) *core.Phase {
	// Fail-closed when parse retry cap is reached.
	if review.ParseRetryCount >= defaultMaxParseRetries {
		return nil
	}

	maxLoops := review.MaxReviewLoops
	if maxLoops <= 0 {
		maxLoops = defaultMaxReviewLoops
	}
	if review.ReviewIteration >= maxLoops {
		return nil
	}

	retry := &core.Phase{
		ID:   fmt.Sprintf("phase-%d", len(e.plan.Phases)+1),
		Name: "re-review",
		Objective: fmt.Sprintf(
			"Re-attempt the code review for phase %q. The previous review output was malformed "+
				"(missing ### Blockers / ### Warnings sections). Produce properly structured output "+
				"with ### Blockers and ### Warnings sections, listing each finding as either \"- **[location]** description\" (bracket form) or \"- **`location`** description\" (backtick form).",
			review.Name,
		),
		Persona:                review.Persona,
		ModelTier:              review.ModelTier,
		Runtime:                review.Runtime,
		Dependencies:           append([]string{}, review.Dependencies...),
		Status:                 core.StatusPending,
		Role:                   core.RoleReviewer,
		PersonaSelectionMethod: core.SelectionRequiredReview,
		ReviewIteration:        review.ReviewIteration,
		MaxReviewLoops:         review.MaxReviewLoops,
		OriginPhaseID:          review.ID,
		ParseRetryCount:        review.ParseRetryCount + 1,
	}

	e.plan.Phases = append(e.plan.Phases, retry)
	return retry
}

// injectReReviewPhase creates a follow-up review gate after a fix phase,
// continuing the bounded reviewer–implementer loop. The re-review:
//   - Uses the original review phase's persona and MaxReviewLoops.
//   - Sets PersonaSelectionMethod = SelectionRequiredReview so the engine
//     recognises it as a review gate and calls handleReviewLoop on completion.
//   - Depends on fixPhase so it receives the fix output as prior context.
//   - Carries the same ReviewIteration as fixPhase so injectFixPhase's loop
//     bound is evaluated correctly if blockers persist after the fix.
//
// injectReReviewPhase always injects the re-review (no early exit on loop
// exhaustion): even when no further fix can be injected, the re-review
// provides a final quality signal and ensures any remaining findings are
// emitted via emitReviewFindings.
func (e *Engine) injectReReviewPhase(originReview *core.Phase, fixPhase *core.Phase) *core.Phase {
	reReview := &core.Phase{
		ID:   fmt.Sprintf("phase-%d", len(e.plan.Phases)+1),
		Name: "re-review",
		Objective: fmt.Sprintf(
			"Re-review the changes made in response to the previous code review (fix iteration %d of %d). "+
				"Produce the same structured ### Blockers / ### Warnings output so the engine can evaluate convergence.",
			fixPhase.ReviewIteration, func() int {
				m := originReview.MaxReviewLoops
				if m <= 0 {
					return defaultMaxReviewLoops
				}
				return m
			}(),
		),
		Persona:                originReview.Persona,
		ModelTier:              originReview.ModelTier,
		Runtime:                originReview.Runtime,
		Dependencies:           []string{fixPhase.ID},
		Status:                 core.StatusPending,
		Role:                   core.RoleReviewer,
		PersonaSelectionMethod: core.SelectionRequiredReview,
		ReviewIteration:        fixPhase.ReviewIteration,
		MaxReviewLoops:         originReview.MaxReviewLoops,
		OriginPhaseID:          originReview.ID,
	}

	e.plan.Phases = append(e.plan.Phases, reReview)
	return reReview
}

// mergeReviewFindings combines findings from two review executors (Claude and
// Codex) into a single ReviewFindings. Blockers are deduplicated: two entries
// are considered the same when they share the same Location AND their
// descriptions are substantially similar (one contains the other, or they share
// at least half their non-trivial words). When a duplicate is found the entry
// with the longer description is kept — it typically has more actionable detail.
// Warnings are unioned without deduplication.
func mergeReviewFindings(claude, codex ReviewFindings) ReviewFindings {
	merged := ReviewFindings{
		Warnings: append(append([]ReviewItem{}, claude.Warnings...), codex.Warnings...),
	}
	merged.Blockers = deduplicateBlockers(append(append([]ReviewItem{}, claude.Blockers...), codex.Blockers...))
	return merged
}

func deduplicateBlockers(items []ReviewItem) []ReviewItem {
	out := make([]ReviewItem, 0, len(items))
	for _, candidate := range items {
		dup := false
		for j, existing := range out {
			if blockersAreSimilar(existing, candidate) {
				if len(candidate.Description) > len(existing.Description) {
					out[j] = candidate
				}
				dup = true
				break
			}
		}
		if !dup {
			out = append(out, candidate)
		}
	}
	return out
}

// blockersAreSimilar returns true when a and b describe the same finding.
// Two blockers match when their locations agree (both empty, or both equal)
// AND their descriptions overlap substantially.
func blockersAreSimilar(a, b ReviewItem) bool {
	if a.Location != b.Location {
		return false
	}
	da := strings.ToLower(strings.TrimSpace(a.Description))
	db := strings.ToLower(strings.TrimSpace(b.Description))
	if da == db {
		return true
	}
	if strings.Contains(da, db) || strings.Contains(db, da) {
		return true
	}
	return reviewWordOverlap(da, db) >= 0.5
}

// reviewWordOverlap returns the Jaccard similarity of the non-trivial words in
// two description strings. Returns 0 when either string is empty after
// filtering.
func reviewWordOverlap(a, b string) float64 {
	wa, wb := reviewWordSet(a), reviewWordSet(b)
	if len(wa) == 0 || len(wb) == 0 {
		return 0
	}
	shared := 0
	for w := range wa {
		if wb[w] {
			shared++
		}
	}
	maxLen := len(wa)
	if len(wb) > maxLen {
		maxLen = len(wb)
	}
	return float64(shared) / float64(maxLen)
}

var reviewTrivialWords = map[string]bool{
	"the": true, "a": true, "an": true, "is": true, "in": true, "on": true,
	"at": true, "to": true, "of": true, "and": true, "or": true, "for": true,
	"with": true, "this": true, "that": true, "it": true, "be": true,
}

func reviewWordSet(s string) map[string]bool {
	words := strings.Fields(s)
	set := make(map[string]bool, len(words))
	for _, w := range words {
		w = strings.TrimRight(w, ".,;:!?—")
		if w != "" && !reviewTrivialWords[w] {
			set[w] = true
		}
	}
	return set
}

// emitReviewFindings emits a review.findings_emitted event carrying all parsed
// findings from a review gate. Called unconditionally from handleReviewLoop so
// that:
//   - Non-blocking warnings are never silently discarded.
//   - Unresolved blockers at loop-exhaustion are preserved in the event log.
//   - Callers polling the event stream always see the full finding set.
func (e *Engine) emitReviewFindings(ctx context.Context, phase *core.Phase, findings ReviewFindings) {
	blockerDescs := flattenReviewItems(findings.Blockers)
	warnDescs := flattenReviewItems(findings.Warnings)
	phase.ReviewBlockers = append([]string{}, blockerDescs...)
	phase.ReviewWarnings = append([]string{}, warnDescs...)
	e.emit(ctx, event.ReviewFindingsEmitted, phase.ID, "", map[string]any{
		"blocker_count":    len(findings.Blockers),
		"warning_count":    len(findings.Warnings),
		"passed":           findings.Passed(),
		"review_iteration": phase.ReviewIteration,
		"blockers":         blockerDescs,
		"warnings":         warnDescs,
	})
}
