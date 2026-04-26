// Package worker — barok density observer.
//
// ObserveDensity is the log-only companion to the barok validator. While the
// validator (barok_validator.go) is gate-wired and triggers a retry on
// PRESERVE violations, the density observer measures how aggressively the
// compressed artifact actually shrank prose surfaces — articles, linking
// verbs, fragment ratio, avg sentence length, total bytes. Its output feeds
// scripts/experiment-snapshot.sh (## Barok Compliance section), not any
// runtime gate.
//
// Contract (DESIGN-DELTA §Interfaces, IMPLEMENTATION-SPEC §2a):
//   - Observe accepts ([]byte, DensityMeta), returns nothing.
//   - Any internal failure (mkdir, marshal, write) is swallowed via slog.Warn.
//     A failure here MUST NOT propagate and MUST NOT fail the phase.
//   - Output dir is $NANIKA_BAROK_DENSITY_DIR if set, else ~/.alluka/barok-density/.
//   - Filename is <workspace>-<persona>-<phase_id>-<basename(artifact)>.json;
//     including the artifact basename prevents collisions when a phase emits
//     multiple .md artifacts (common for architect, data-analyst).
package worker

import (
	"encoding/json"
	"log/slog"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"
)

// BarokDensityEnvDir is the env var that overrides the default output
// directory. Useful for tests and isolated snapshot runs.
const BarokDensityEnvDir = "NANIKA_BAROK_DENSITY_DIR"

// DensityMeta travels with each observation so downstream readers
// (snapshot script) can group by intensity tier without reparsing phase rows.
type DensityMeta struct {
	WorkspaceID   string
	Persona       string
	PhaseID       string
	PhaseName     string
	IntensityTier string // "LiteFragment" | "LiteSentence" | "LiteNarrative" | ""
	ArtifactPath  string
}

// DensityRecord is the on-disk JSON shape. Exported so tests and the
// snapshot script's consumer stay in sync.
type DensityRecord struct {
	WorkspaceID      string    `json:"workspace_id"`
	Persona          string    `json:"persona"`
	PhaseID          string    `json:"phase_id"`
	PhaseName        string    `json:"phase_name"`
	IntensityTier    string    `json:"intensity_tier"`
	ArtifactPath     string    `json:"artifact_path"`
	TotalBytes       int       `json:"total_bytes"`
	ArticleCount     int       `json:"article_count"`
	LinkingVerbCount int       `json:"linking_verb_count"`
	AvgSentenceLen   float64   `json:"avg_sentence_len"`
	FragmentRatio    float64   `json:"fragment_ratio"`
	ComputedAt       time.Time `json:"computed_at"`
}

// Regexes pre-compiled at package init so per-artifact observation is pure
// compute. Case-insensitive, word-boundaried.
var (
	articleRe     = regexp.MustCompile(`(?i)\b(a|an|the)\b`)
	linkingVerbRe = regexp.MustCompile(`(?i)\b(is|are|was|were|be|been|being|am)\b`)

	// fencedCodeBlockRe strips ``` ... ``` blocks (lazy, spans multiple lines).
	fencedCodeBlockRe = regexp.MustCompile("(?s)```.*?```")
	// inlineCodeRe strips `inline code` spans.
	inlineCodeRe = regexp.MustCompile("`[^`]*`")
)

// stripCodeRegions removes fenced and inline code so prose counters do not
// match identifiers, SQL keywords, or language tokens inside code. Mirrors
// caveman's validate_code_blocks exclusion approach.
func stripCodeRegions(data []byte) []byte {
	out := fencedCodeBlockRe.ReplaceAll(data, []byte(""))
	out = inlineCodeRe.ReplaceAll(out, []byte(""))
	return out
}

// splitSentences splits prose on sentence terminators (. ! ?) and returns
// non-empty trimmed sentences. Intentionally simple — caveman's measure.py
// uses the same heuristic. Newlines are NOT terminators: hard-wrapped prose
// (architect, data-analyst output) would otherwise shatter into fragments and
// inflate fragment_ratio relative to unwrapped artifacts of the same intensity.
func splitSentences(prose string) []string {
	// Replace terminators with a shared delimiter, then split.
	replacer := strings.NewReplacer(
		"!", "|",
		"?", "|",
		".", "|",
	)
	joined := replacer.Replace(prose)
	raw := strings.Split(joined, "|")
	out := make([]string, 0, len(raw))
	for _, s := range raw {
		s = strings.TrimSpace(s)
		if s != "" {
			out = append(out, s)
		}
	}
	return out
}

// countWords returns a whitespace-split word count.
func countWords(s string) int {
	return len(strings.Fields(s))
}

// isFragment classifies a sentence as a fragment when it (a) has no linking
// verb AND (b) is short (< 4 words). Mirrors caveman's fragment heuristic
// from validate_bullets — imperative command-style lines like "Drop ref" or
// "Wrap in useMemo" count as fragments; full clauses do not.
func isFragment(sentence string) bool {
	if linkingVerbRe.MatchString(sentence) {
		return false
	}
	return countWords(sentence) < 4
}

// ComputeDensity returns a DensityRecord populated with the five metrics
// defined in DESIGN-DELTA §2. Meta, ArtifactPath, and ComputedAt are left to
// the caller (Observe sets them). Exported so tests can exercise the pure
// compute without touching the filesystem.
func ComputeDensity(data []byte) DensityRecord {
	rec := DensityRecord{
		TotalBytes: len(data),
	}

	stripped := stripCodeRegions(data)
	proseText := string(stripped)

	rec.ArticleCount = len(articleRe.FindAllString(proseText, -1))
	rec.LinkingVerbCount = len(linkingVerbRe.FindAllString(proseText, -1))

	sentences := splitSentences(proseText)
	if len(sentences) == 0 {
		return rec
	}

	var totalWords, fragmentCount int
	for _, s := range sentences {
		totalWords += countWords(s)
		if isFragment(s) {
			fragmentCount++
		}
	}
	rec.AvgSentenceLen = float64(totalWords) / float64(len(sentences))
	rec.FragmentRatio = float64(fragmentCount) / float64(len(sentences))
	return rec
}

// ObserveDensity computes the density metrics for data and writes one JSON
// record to the barok-density directory. Fire-and-forget: internal failures
// are logged via slog.Warn and swallowed. Does NOT return an error — any
// change to this signature is a spec violation (DESIGN-DELTA §Component Map,
// IMPLEMENTATION-SPEC §2a).
func ObserveDensity(data []byte, meta DensityMeta) {
	dir, err := densityDir()
	if err != nil {
		slog.Warn("barok density: resolving output dir",
			"workspace", meta.WorkspaceID,
			"phase", meta.PhaseID,
			"error", err)
		return
	}
	if mkErr := os.MkdirAll(dir, 0o700); mkErr != nil {
		slog.Warn("barok density: creating output dir",
			"dir", dir,
			"error", mkErr)
		return
	}

	rec := ComputeDensity(data)
	rec.WorkspaceID = meta.WorkspaceID
	rec.Persona = meta.Persona
	rec.PhaseID = meta.PhaseID
	rec.PhaseName = meta.PhaseName
	rec.IntensityTier = meta.IntensityTier
	rec.ArtifactPath = meta.ArtifactPath
	rec.ComputedAt = time.Now().UTC()

	name := densityFilename(meta)
	path := filepath.Join(dir, name)

	payload, err := json.MarshalIndent(rec, "", "  ")
	if err != nil {
		slog.Warn("barok density: marshaling record",
			"workspace", meta.WorkspaceID,
			"phase", meta.PhaseID,
			"error", err)
		return
	}
	if err := os.WriteFile(path, payload, 0o600); err != nil {
		slog.Warn("barok density: writing record",
			"path", path,
			"error", err)
		return
	}
}

// densityDir resolves the output directory. Respects $NANIKA_BAROK_DENSITY_DIR
// first, else defaults to ~/.alluka/barok-density/.
func densityDir() (string, error) {
	if override := os.Getenv(BarokDensityEnvDir); override != "" {
		return override, nil
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(home, ".alluka", "barok-density"), nil
}

// densityFilename builds the per-artifact record filename. Including the
// artifact basename prevents collision when a phase emits multiple .md files.
func densityFilename(meta DensityMeta) string {
	base := filepath.Base(meta.ArtifactPath)
	if base == "" || base == "." || base == "/" {
		base = "artifact"
	}
	// Strip common extensions to keep the filename readable; json is appended.
	base = strings.TrimSuffix(base, filepath.Ext(base))
	parts := []string{
		sanitizeSegment(meta.WorkspaceID),
		sanitizeSegment(meta.Persona),
		sanitizeSegment(meta.PhaseID),
		sanitizeSegment(base),
	}
	return strings.Join(parts, "-") + ".json"
}

// sanitizeSegment replaces path separators with underscores so nothing escapes
// the density dir. Empty segments become "unknown" for predictable filenames.
func sanitizeSegment(s string) string {
	if s == "" {
		return "unknown"
	}
	s = strings.ReplaceAll(s, string(os.PathSeparator), "_")
	s = strings.ReplaceAll(s, "/", "_")
	return s
}
