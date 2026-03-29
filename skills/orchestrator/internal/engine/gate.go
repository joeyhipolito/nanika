package engine

import (
	"fmt"
	"path/filepath"
	"strings"
)

// GateResult describes the outcome of a quality gate check.
type GateResult struct {
	Passed bool
	Reason string
}

// CheckGate validates worker output meets minimum quality.
// Checks: existence (non-empty), format (has substance), and artifact presence.
// When expectedPaths is non-empty, each pattern must match at least one file on disk.
// Gate is warning-only (fail-forward): callers log the reason but continue execution.
func CheckGate(output string, expectedPaths []string) GateResult {
	// Existence gate: output must be non-empty
	trimmed := strings.TrimSpace(output)
	if trimmed == "" {
		return GateResult{Passed: false, Reason: "empty output"}
	}

	// Format gate: output must have minimum substance (not just error text)
	if len(trimmed) < 100 {
		// Very short output is suspicious — check if it's just an error
		lower := strings.ToLower(trimmed)
		errorPhrases := []string{
			"i cannot", "i'm unable", "error:", "failed to",
			"permission denied", "not found",
		}
		for _, phrase := range errorPhrases {
			if strings.Contains(lower, phrase) {
				return GateResult{
					Passed: false,
					Reason: fmt.Sprintf("output appears to be an error (%d chars)", len(trimmed)),
				}
			}
		}
	}

	// Artifact gate: each expected path pattern must match at least one file.
	if len(expectedPaths) > 0 {
		var missing []string
		for _, pattern := range expectedPaths {
			matches, err := filepath.Glob(pattern)
			if err != nil || len(matches) == 0 {
				missing = append(missing, pattern)
			}
		}
		if len(missing) > 0 {
			return GateResult{
				Passed: false,
				Reason: fmt.Sprintf("expected artifacts not found: %s", strings.Join(missing, ", ")),
			}
		}
	}

	return GateResult{Passed: true}
}
