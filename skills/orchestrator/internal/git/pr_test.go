package git

import (
	"strings"
	"testing"
)

func TestBuildPRBody_ContainsRequiredFields(t *testing.T) {
	m := PRMetadata{
		Summary:    "add feature X",
		MissionID:  "20260316-abc001",
		PhaseCount: 3,
		Mode:       "parallel",
		Personas:   []string{"implementer", "reviewer"},
		CostUSD:    0.0123,
		Duration:   "1m30s",
		Files:      []string{"cmd/run.go", "internal/git/pr.go"},
	}

	body := BuildPRBody(m)

	checks := []string{
		"## Summary",
		"add feature X",
		"## Mission Details",
		"20260316-abc001",
		"3",
		"parallel",
		"implementer, reviewer",
		"$0.0123",
		"1m30s",
		"## Files Changed",
		"`cmd/run.go`",
		"`internal/git/pr.go`",
		"via orchestrator",
	}
	for _, want := range checks {
		if !strings.Contains(body, want) {
			t.Errorf("PR body missing %q\ngot:\n%s", want, body)
		}
	}
}

func TestBuildPRBody_NoCostOrFilesOmitted(t *testing.T) {
	m := PRMetadata{
		Summary:    "quick fix",
		MissionID:  "20260316-nnn001",
		PhaseCount: 1,
		Mode:       "sequential",
	}

	body := BuildPRBody(m)

	if strings.Contains(body, "## Files Changed") {
		t.Error("PR body should not contain Files Changed section when Files is empty")
	}
	if strings.Contains(body, "| Cost |") {
		t.Error("PR body should not contain Cost row when CostUSD is 0")
	}
}

func TestBuildPRBody_EmptySummaryFallback(t *testing.T) {
	m := PRMetadata{
		MissionID:  "20260316-empty",
		PhaseCount: 2,
		Mode:       "sequential",
	}

	body := BuildPRBody(m)
	if !strings.Contains(body, "_No summary provided._") {
		t.Error("expected fallback text for empty summary")
	}
}

func TestHasGH_ReturnsBool(t *testing.T) {
	// HasGH must return a bool without panicking; we don't assert a specific
	// value because gh may or may not be installed in the test environment.
	_ = HasGH()
}
