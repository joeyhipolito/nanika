// Package worker — barok output-compression rule injection.
//
// Barok is the nanika-native variant of the upstream "caveman" lite ruleset.
// It tells the worker LLM to compress prose surfaces in its generated output
// while preserving 13 structural / machine-contract surfaces verbatim. The
// instruction is injected into the worker's CLAUDE.md just before the
// ## Output block so the LLM reads the rule before emitting any token.
//
// Cache-safety invariant (scope §5):
//   - Inject only when IsTerminal=true. Terminal-phase output never re-enters
//     a dependent worker's prompt prefix, so compressed bytes do not break the
//     94.17% cache_read ratio.
//   - Worker CLAUDE.md is already uncacheable per phase (per-phase WorkerDir
//     path + embedded time.Now() at claudemd.go:270), so the ~1–2 KB rule card
//     adds zero cache_creation premium.
//
// Surface this with NANIKA_NO_BAROK=1 to short-circuit injection (debug,
// emergency disable, or A/B comparability during the experiment window).
package worker

import (
	"os"
	"strings"
)

// BarokEnvDisable is the env var that, when set to "1", short-circuits
// InjectBarok to "" and signals the artifact-collection path to skip the
// validator+retry leg entirely.
const BarokEnvDisable = "NANIKA_NO_BAROK"

// BarokPersonas is the v1+delta persona allow-list. Only these personas
// receive the rule card, even on terminal phases.
//
// scope §2 + delta §2: technical-writer, academic-researcher, architect,
// data-analyst, staff-code-reviewer (added by delta because the validator
// makes review-gate parseability risk bounded and recoverable).
var BarokPersonas = []string{
	"technical-writer",
	"academic-researcher",
	"architect",
	"data-analyst",
	"staff-code-reviewer",
}

// barokIntensity classifies the per-persona compression strength chosen in
// delta §2. Drives which rule fragment is emitted into the rule card.
type barokIntensity int

const (
	intensityNone barokIntensity = iota
	// intensityLiteFragment: lite + sentence fragments (drop subject pronouns,
	// drop linking verbs in declaratives). Used for the prose-densest personas.
	intensityLiteFragment
	// intensityLiteSentence: lite, complete sentences only. Articles, hedges,
	// pleasantries compressed; rationale chains preserved.
	intensityLiteSentence
	// intensityLiteNarrative: lite restricted to narrative explanation prose;
	// verdict-shaped lines pass through verbatim.
	intensityLiteNarrative
)

// barokRule bundles per-persona behaviour for the rule card.
type barokRule struct {
	intensity       barokIntensity
	specialCompress string // delta §2 "Special compression" cell
	specialPreserve string // delta §2 "Special preservation" cell (empty = standard list only)
}

// TODO: specialCompress and specialPreserve literals below are candidates for
// future config-file extraction once the barok experiment stabilizes. For now
// they remain embedded to keep tuning feedback loops tight.
//
// barokRules holds per-persona overrides. Personas not present in this map
// are not eligible for barok injection (see BarokPersonas).
var barokRules = map[string]barokRule{
	"technical-writer": {
		intensity:       intensityLiteFragment,
		specialCompress: `Drop subject pronouns ("System returns X" → "Returns X"); drop linking verbs in declaratives.`,
	},
	"academic-researcher": {
		intensity:       intensityLiteFragment,
		specialCompress: `Drop subject pronouns ("System returns X" → "Returns X"); compress hedged clauses ("It is plausible that X may Y" → "X may Y").`,
		specialPreserve: "Citation patterns (`[Author YYYY]`, `(Author, YYYY)`, DOI strings) verbatim.",
	},
	"architect": {
		intensity:       intensityLiteSentence,
		specialCompress: "Articles, pleasantries, and hedges only. No fragments — preserve rationale chains intact.",
		specialPreserve: "ADR section headers (Context, Decision, Candidates Considered, Component Map, Interfaces, Risks, Trade-offs Accepted) verbatim.",
	},
	"data-analyst": {
		intensity:       intensityLiteSentence,
		specialCompress: "Articles, pleasantries, and hedges only. No fragments — preserve quantitative claims intact.",
		specialPreserve: "Numeric expressions with units (e.g. `94.17%`, `$15/M`, `7d`, `12.5×`) verbatim.",
	},
	"staff-code-reviewer": {
		intensity:       intensityLiteNarrative,
		specialCompress: "Compress narrative explanation prose only. Verdict-shaped lines and code citations pass through.",
		specialPreserve: "Verdict markers (`APPROVE:`, `REJECT:`, `BLOCK:`, `NEEDS-CHANGES:`, `NIT:`, `BLOCKING:`) verbatim — including any leading whitespace or bullet prefix.",
	},
}

// IsBarokEligiblePersona reports whether persona is on the barok allow-list.
// Stable for callers that want to gate behaviour without reaching into the
// rule map (e.g. metrics emission, CLI debug subcommands).
func IsBarokEligiblePersona(persona string) bool {
	_, ok := barokRules[persona]
	return ok
}

// BarokIntensityTier returns a stable string label naming the persona's
// compression tier for barok-eligible personas, and "" for any persona not
// on the allow-list. Used by the density observer to group records without
// needing an internal tier enum.
func BarokIntensityTier(persona string) string {
	rule, ok := barokRules[persona]
	if !ok {
		return ""
	}
	switch rule.intensity {
	case intensityLiteFragment:
		return "LiteFragment"
	case intensityLiteSentence:
		return "LiteSentence"
	case intensityLiteNarrative:
		return "LiteNarrative"
	default:
		return ""
	}
}

// BarokDisabled reports whether the NANIKA_NO_BAROK env var is set to "1".
// Centralised so the engine and validator-retry path agree on the predicate.
func BarokDisabled() bool {
	return os.Getenv(BarokEnvDisable) == "1"
}

// InjectBarok returns the ## Output Compression rule-card section for persona
// when isTerminal is true and barok is enabled. Returns "" when:
//   - NANIKA_NO_BAROK=1 is set,
//   - persona is not on the barok allow-list,
//   - or isTerminal is false (non-terminal phase — output would feed a
//     dependent worker's prompt prefix and break cache).
//
// The returned section ends with a single trailing newline so callers can
// concatenate it directly into the CLAUDE.md builder without juggling spacing.
func InjectBarok(persona string, isTerminal bool) string {
	if BarokDisabled() {
		return ""
	}
	if !isTerminal {
		return ""
	}
	rule, ok := barokRules[persona]
	if !ok {
		return ""
	}

	var b strings.Builder
	b.WriteString("## Output Compression\n\n")
	b.WriteString("This phase produces terminal output (no dependent worker phase will read it). ")
	b.WriteString("Compress your generated prose using the rules below. ")
	b.WriteString("The orchestrator runs a mechanical validator on every artifact you write — ")
	b.WriteString("if compression strips a preserved surface, the artifact is regenerated once without compression. ")
	b.WriteString("Stay inside the rules and your output ships on the first pass.\n\n")

	b.WriteString("### COMPRESS (apply to prose surfaces only)\n\n")
	b.WriteString("- Paragraph prose: narrative explanation, rationale, commentary, summaries.\n")
	b.WriteString("- Bulleted-list and numbered-list items whose body is a prose sentence.\n")
	b.WriteString("- Inline parenthetical asides and hedging (\"roughly\", \"it should be noted\", \"in practice\", \"for the most part\").\n")
	b.WriteString("- Articles, pleasantries, filler — the upstream `lite` baseline.\n")
	if rule.specialCompress != "" {
		b.WriteString("- ")
		b.WriteString(rule.specialCompress)
		b.WriteString("\n")
	}
	b.WriteString("\n")

	b.WriteString("### PRESERVE VERBATIM (never compress, abbreviate, or rewrite)\n\n")
	b.WriteString("- Fenced code blocks (``` any language tag) — bytes equal.\n")
	b.WriteString("- Inline code (single backticks) — bytes equal.\n")
	b.WriteString("- Indented (4-space) code blocks — bytes equal.\n")
	b.WriteString("- Markdown headings (all levels) — text and order.\n")
	b.WriteString("- Tables: pipe structure, column count, row count, header row preserved.\n")
	b.WriteString("- YAML frontmatter (top-of-file `---` block and any embedded YAML).\n")
	b.WriteString("- Scratch blocks (everything between `<!-- scratch -->` and `<!-- /scratch -->`) — bytes equal.\n")
	b.WriteString("- Context-bundle sections (`## Context from Prior Work`, `## Prior Phase Notes`, `## Lessons from Past Missions`, `## Worker Identity`).\n")
	b.WriteString("- Learning markers: lines beginning with `LEARNING:`, `FINDING:`, `PATTERN:`, `GOTCHA:`, `DECISION:`.\n")
	b.WriteString("- URLs (any scheme: `http://`, `https://`, `ftp://`, `file://`, `git@…`, naked domains).\n")
	b.WriteString("- File paths, shell commands, CLI invocations.\n")
	b.WriteString("- JSON / structured payloads inside code blocks or tool-result bodies.\n")
	b.WriteString("- Verdict markers if present: `APPROVE:`, `REJECT:`, `BLOCK:`, `NEEDS-CHANGES:`, `NIT:`, `BLOCKING:`.\n")
	if rule.specialPreserve != "" {
		b.WriteString("- ")
		b.WriteString(rule.specialPreserve)
		b.WriteString("\n")
	}
	b.WriteString("\n")

	// Identifier-aware rule applies to every persona (delta §2).
	b.WriteString("### IDENTIFIER-AWARE RULE\n\n")
	b.WriteString("Tokens matching `[a-z]+_[a-z_]+` (snake_case) or `[A-Z][a-z]+[A-Z][a-zA-Z]*` (CamelCase) ")
	b.WriteString("pass through verbatim. Function names, file paths, and identifiers must not be ")
	b.WriteString("decomposed under aggressive compression.\n\n")

	b.WriteString("### QUICK REFERENCE\n\n")
	b.WriteString("```\n")
	b.WriteString("COMPRESS: prose paragraphs, bullet-body prose, list-item prose, parenthetical hedges.\n")
	b.WriteString("PRESERVE: ``` fenced ```, `inline code`, 4-space indented code, # headings,\n")
	b.WriteString("          | tables | (structure), YAML frontmatter, <!-- scratch --> blocks,\n")
	b.WriteString("          ## Context/Prior/Lessons/Identity sections,\n")
	b.WriteString("          LEARNING:/FINDING:/PATTERN:/GOTCHA:/DECISION: marker lines,\n")
	b.WriteString("          URLs, file paths, shell commands, JSON/structured payloads,\n")
	b.WriteString("          APPROVE:/REJECT:/BLOCK:/NEEDS-CHANGES: verdict lines,\n")
	b.WriteString("          snake_case + CamelCase identifiers.\n")
	b.WriteString("```\n\n")

	return b.String()
}

// BarokRuleCardBytes returns the byte length of the rule card emitted for
// persona on a terminal phase. Returns 0 when persona is ineligible or barok
// is disabled. Used by the `orchestrator barok status` subcommand to surface
// the actual injected payload size per persona.
func BarokRuleCardBytes(persona string) int {
	return len(InjectBarok(persona, true))
}
