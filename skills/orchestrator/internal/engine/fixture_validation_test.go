package engine

import (
	"os"
	"path/filepath"
	"testing"
)

// TestFixtureValidation_TRK608 validates the three real review artifacts from the
// Validation section of the TRK-608 mission against the patched lookup and parser.
func TestFixtureValidation_TRK608(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("could not determine home dir: %v", err)
	}

	cases := []struct {
		name          string
		workerDir     string
		wantFile      string // expected filename to be found by lookup
		wantBlockers  int
		wantWarnings  int
	}{
		{
			name:         "fixture1_prompt_tune_greeting_review",
			workerDir:    filepath.Join(home, ".alluka/workspaces/20260422-077b4f44/workers/staff-code-reviewer-phase-3"),
			wantFile:     "prompt-tune-greeting-review.md",
			wantBlockers: 0,
			wantWarnings: 1,
		},
		{
			name:         "fixture2_clean_approval",
			workerDir:    filepath.Join(home, ".alluka/workspaces/20260422-75479aac/workers/staff-code-reviewer-phase-6"),
			wantFile:     "review.md",
			wantBlockers: 0,
			wantWarnings: 0,
		},
		{
			name:         "fixture3_review_re_review",
			workerDir:    filepath.Join(home, ".alluka/workspaces/20260422-b429bce0/workers/staff-code-reviewer-phase-5"),
			wantFile:     "review-re-review.md",
			wantBlockers: 0,
			wantWarnings: 2,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			// 1. Verify lookup finds the expected file.
			found := findReviewMdCaseInsensitive(tc.workerDir)
			if found == "" {
				t.Fatalf("findReviewMdCaseInsensitive(%q): returned empty — lookup miss", tc.workerDir)
			}
			if filepath.Base(found) != tc.wantFile {
				t.Fatalf("lookup returned %q, want %q", filepath.Base(found), tc.wantFile)
			}

			// 2. Parse the artifact and verify (blockers, warnings) counts.
			findings, err := ParseReviewFindingsFromArtifact(found)
			if err != nil {
				t.Fatalf("ParseReviewFindingsFromArtifact(%q): %v", found, err)
			}
			if got := len(findings.Blockers); got != tc.wantBlockers {
				t.Errorf("blockers: got %d, want %d", got, tc.wantBlockers)
			}
			if got := len(findings.Warnings); got != tc.wantWarnings {
				t.Errorf("warnings: got %d, want %d", got, tc.wantWarnings)
			}

			// 3. Confirm hasReviewHeaders returns true (required for artifact adoption).
			data, _ := os.ReadFile(found)
			if !hasReviewHeaders(string(data)) {
				t.Errorf("hasReviewHeaders returned false — artifact would be rejected by engine")
			}
		})
	}
}
