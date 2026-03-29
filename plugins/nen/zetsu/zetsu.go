// Package zetsu detects and neutralizes prompt injection patterns in untrusted
// text before it reaches Claude workers. It implements pattern-based detection
// derived from an analysis of the orchestrator's trust boundaries:
//
//   - Channel messages (Telegram/Discord) routed to orchestrator run
//   - PHASE line OBJECTIVE fields parsed from mission files
//   - Prior phase context injected into downstream worker prompts
//
// Two entry points cover the two highest-risk paths:
//   - CheckChannelMessage — full pattern scan on inbound channel text
//   - SanitizeObjective  — sanitize a single PHASE OBJECTIVE field value
//
// Both functions log warnings to stderr when patterns are detected and return
// a Result that carries the (possibly modified) output. Content is never
// silently dropped: flagged content is logged and returned with an annotation;
// blocked content has the matching span replaced with [injection-blocked].
package zetsu

import (
	"fmt"
	"os"
	"regexp"
	"strings"
)

// Tier classifies the severity of a detected pattern.
type Tier int

const (
	// TierFlag indicates suspicious content that is logged but preserved.
	// The operator is warned; execution continues with the original text.
	TierFlag Tier = iota + 1

	// TierBlock indicates a high-confidence injection attempt. The matching
	// span is replaced with [injection-blocked] before the text is used.
	TierBlock
)

// Match describes one detected pattern in a piece of text.
type Match struct {
	Reason string // human-readable description of the pattern
	Tier   Tier
}

// Result is the output of a zetsu scan.
type Result struct {
	Input   string
	Output  string  // sanitized output; equals Input when no block patterns fired
	Matches []Match // all detected patterns, across both tiers
}

// Sanitized reports whether any block-tier pattern fired (output differs from input).
func (r Result) Sanitized() bool { return r.Output != r.Input }

// Clean reports whether no patterns were detected.
func (r Result) Clean() bool { return len(r.Matches) == 0 }

// pattern pairs a compiled regex with its tier and a reason string.
type pattern struct {
	re     *regexp.Regexp
	tier   Tier
	reason string
}

// blockPatterns match high-confidence injection attempts in any context.
// When a pattern fires, every non-overlapping match is replaced with
// [injection-blocked] in the output.
//
// GOTCHA: keep patterns anchored or use word boundaries — overly broad
// patterns produce false positives on normal task descriptions.
var blockPatterns = []*pattern{
	// System-prompt override imperatives.
	{
		re:     regexp.MustCompile(`(?i)\bignore\b.{0,30}\b(all\s+)?(previous|prior|above|your)\b.{0,30}\b(instructions?|directives?|rules?|constraints?|guidelines?)\b`),
		tier:   TierBlock,
		reason: "system-prompt override: ignore-instructions pattern",
	},
	{
		re:     regexp.MustCompile(`(?i)\bdisregard\b.{0,30}\b(your|all|previous|prior)?\b.{0,30}\b(instructions?|rules?|guidelines?|training|programming)\b`),
		tier:   TierBlock,
		reason: "system-prompt override: disregard-instructions pattern",
	},
	{
		re:     regexp.MustCompile(`(?i)\bnew\s+(system\s+prompt|instructions?|directives?)\b`),
		tier:   TierBlock,
		reason: "system-prompt override: new-instructions injection",
	},

	// Identity / persona hijack.
	{
		re:     regexp.MustCompile(`(?i)\b(you\s+are\s+now|act\s+as\s+(an?\s+)?(unrestricted|unfiltered|evil|malicious|harmful|different|new|my)\s+(ai|assistant|model|bot|chatbot|persona|agent)|pretend\s+(to\s+be|you\s+are)|your\s+(new|true|real|actual)\s+(identity|persona|role|name)\s+is)\b`),
		tier:   TierBlock,
		reason: "persona hijack: identity-override pattern",
	},
	{
		re:     regexp.MustCompile(`(?i)\byou\s+have\s+(no\s+)?(restrictions?|limits?|constraints?|rules?|guidelines?)\b`),
		tier:   TierBlock,
		reason: "persona hijack: constraint-nullification pattern",
	},

	// Permission / jailbreak escalation.
	{
		re:     regexp.MustCompile(`(?i)\b(permission[\s_-]?mode|bypass[\s_-]?mode|developer[\s_-]?mode|admin[\s_-]?mode|jailbreak)\b`),
		tier:   TierBlock,
		reason: "permission escalation: mode-override pattern",
	},

	// XML / message-boundary injection (fake Claude conversation tags).
	{
		re:     regexp.MustCompile(`(?i)</?(?:system|human|assistant|instructions?|claude)\b[^>]{0,100}>`),
		tier:   TierBlock,
		reason: "XML boundary injection: fake Claude conversation tag",
	},

	// Fake orchestrator / PHASE output injected into objective text.
	// A PHASE: line inside an objective is always injected — legitimate
	// PHASE lines come from the mission file, not from within an objective field.
	{
		re:     regexp.MustCompile(`(?m)^\s*PHASE\s*:\s*\S`),
		tier:   TierBlock,
		reason: "PHASE line injection in objective field",
	},

	// YAML frontmatter injection (produced_by, phase, workspace fields).
	// Checks for the combination of a --- fence followed by orchestrator-specific keys.
	{
		re:     regexp.MustCompile(`(?mi)^---\s*\n(?:[^\n]*\n){0,5}(?:produced_by|phase|workspace)\s*:`),
		tier:   TierBlock,
		reason: "YAML frontmatter injection: orchestrator metadata keys embedded in input",
	},
}

// flagPatterns match suspicious but ambiguous content. When these fire the
// text is returned unchanged but a warning is logged.
var flagPatterns = []*pattern{
	// Learning / finding markers in untrusted input create a persistent vector:
	// if ingested by the learning pipeline they can poison future decompositions.
	{
		re:     regexp.MustCompile(`(?m)^(LEARNING|FINDING|GOTCHA|PATTERN|DECISION)\s*:`),
		tier:   TierFlag,
		reason: "learning-marker pattern in untrusted input (potential learnings DB poisoning)",
	},

	// Markdown section headings that shadow orchestrator CLAUDE.md sections.
	{
		re:     regexp.MustCompile(`(?im)^#{1,3}\s+(your\s+task|system|instructions?|persona|role|constraints?|available\s+tools)\s*$`),
		tier:   TierFlag,
		reason: "markdown section injection: heading shadows orchestrator CLAUDE.md structure",
	},

	// Prior-context XML tag escape — content wrapping itself in result-like tags
	// to bleed across the phase-N-output → phase-N+1-prompt boundary.
	{
		re:     regexp.MustCompile(`(?i)</?(?:prior[_\s]?context|phase[_\s]?output|worker[_\s]?result|tool[_\s]?result)\b[^>]{0,100}>`),
		tier:   TierFlag,
		reason: "prior-context XML tag: potential cross-phase boundary escape",
	},
}

// applyBlockPatterns replaces all matches of block-tier patterns with
// [injection-blocked] and returns the list of matched reasons.
func applyBlockPatterns(text string) (string, []Match) {
	var matches []Match
	for _, p := range blockPatterns {
		locs := p.re.FindAllStringIndex(text, -1)
		if len(locs) == 0 {
			continue
		}
		matches = append(matches, Match{Reason: p.reason, Tier: TierBlock})
		text = p.re.ReplaceAllString(text, "[injection-blocked]")
	}
	return text, matches
}

// applyFlagPatterns scans for flag-tier patterns without modifying text.
func applyFlagPatterns(text string) []Match {
	var matches []Match
	for _, p := range flagPatterns {
		if p.re.MatchString(text) {
			matches = append(matches, Match{Reason: p.reason, Tier: TierFlag})
		}
	}
	return matches
}

// logResult writes a warning to stderr for each detected match.
// context identifies the call site (e.g., "channel-message", "phase-objective").
func logResult(context string, r Result) {
	for _, m := range r.Matches {
		tier := "flagged"
		if m.Tier == TierBlock {
			tier = "blocked"
		}
		fmt.Fprintf(os.Stderr,
			"zetsu: %s [%s] in %s — content %s\n",
			m.Reason, tier, context,
			func() string {
				if m.Tier == TierBlock {
					return "neutralized with [injection-blocked]"
				}
				return "preserved with warning"
			}(),
		)
	}
}

// CheckChannelMessage scans an inbound channel message (e.g., from Telegram)
// for prompt injection patterns. It applies both block and flag pattern tiers.
// Block patterns replace matching spans; flag patterns log a warning only.
// The result is always non-empty; the caller should use Result.Output as the
// sanitized task text passed to the orchestrator.
//
// Call this before the task text enters orchestrator run / decompose.
func CheckChannelMessage(msg string) Result {
	sanitized, blockMatches := applyBlockPatterns(msg)
	flagMatches := applyFlagPatterns(sanitized) // scan post-block output
	all := append(blockMatches, flagMatches...)

	r := Result{Input: msg, Output: sanitized, Matches: all}
	if !r.Clean() {
		logResult("channel-message", r)
	}
	return r
}

// SanitizeObjective sanitizes the OBJECTIVE field extracted from a single
// PHASE line before it is stored in a Phase struct or injected into a worker
// prompt. Only block-tier patterns apply: flag patterns produce too many false
// positives in short, imperative objective text.
//
// Call this on each objective string immediately after PHASE line parsing.
func SanitizeObjective(objective string) Result {
	sanitized, blockMatches := applyBlockPatterns(objective)
	r := Result{Input: objective, Output: sanitized, Matches: blockMatches}
	if !r.Clean() {
		logResult("phase-objective", r)
	}
	return r
}

// SanitizePriorContext sanitizes the prior-phase output string before it is
// injected into the next phase worker's prompt. Both block and flag tiers are
// applied: block patterns replace matching spans with [injection-blocked], and
// flag patterns log a warning while preserving the content.
//
// Call this on the assembled priorContext string immediately before it is
// passed to executePhase, covering both sequential and parallel execution paths.
func SanitizePriorContext(ctx string) Result {
	sanitized, blockMatches := applyBlockPatterns(ctx)
	flagMatches := applyFlagPatterns(sanitized)
	all := append(blockMatches, flagMatches...)
	r := Result{Input: ctx, Output: sanitized, Matches: all}
	if !r.Clean() {
		logResult("prior-context", r)
	}
	return r
}

// ReasonSummary returns a deduplicated, semicolon-separated list of all
// detected pattern reasons. Useful for structured logging or event data.
func ReasonSummary(matches []Match) string {
	seen := make(map[string]bool, len(matches))
	var parts []string
	for _, m := range matches {
		if !seen[m.Reason] {
			seen[m.Reason] = true
			parts = append(parts, m.Reason)
		}
	}
	return strings.Join(parts, "; ")
}
