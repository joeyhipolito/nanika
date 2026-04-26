package engine

import (
	"os"
	"path/filepath"
	"testing"
)

// TestQA_Phase5ReviewParsesTo3Warnings verifies that ParseReviewFindingsFromArtifact
// correctly parses the staff-code-reviewer-phase-5 review artifact (which uses
// backtick-format warnings) and produces 3 warnings and 0 blockers.
func TestQA_Phase5ReviewParsesTo3Warnings(t *testing.T) {
	const artifactPath = "/Users/joeyhipolito/.alluka/workspaces/20260422-2db5b618/workers/staff-code-reviewer-phase-5/review.md"
	if _, err := os.Stat(artifactPath); err != nil {
		t.Skipf("fixture not found at %s: %v", artifactPath, err)
	}

	findings, err := ParseReviewFindingsFromArtifact(artifactPath)
	if err != nil {
		t.Fatalf("ParseReviewFindingsFromArtifact returned error: %v", err)
	}
	if got := len(findings.Blockers); got != 0 {
		t.Errorf("Blockers = %d, want 0", got)
	}
	if got := len(findings.Warnings); got != 3 {
		t.Errorf("Warnings = %d, want 3", got)
	}
}

// TestQA_Phase6UppercaseReviewMdIsFound verifies that findReviewMdCaseInsensitive
// locates REVIEW.md (uppercase) in the staff-code-reviewer-phase-6 worker dir.
// On macOS (case-insensitive FS) the fast path os.Stat("review.md") succeeds for
// REVIEW.md, so the returned path may use the lowercase spelling but still points
// to the same inode — the important property is that the path is non-empty and readable.
func TestQA_Phase6UppercaseReviewMdIsFound(t *testing.T) {
	const dir = "/Users/joeyhipolito/.alluka/workspaces/20260422-2db5b618/workers/staff-code-reviewer-phase-6"
	const uppercaseFile = "/Users/joeyhipolito/.alluka/workspaces/20260422-2db5b618/workers/staff-code-reviewer-phase-6/REVIEW.md"

	if _, err := os.Stat(uppercaseFile); err != nil {
		t.Skipf("fixture not found at %s: %v", uppercaseFile, err)
	}

	got := findReviewMdCaseInsensitive(dir)
	if got == "" {
		t.Fatal("findReviewMdCaseInsensitive returned empty string — REVIEW.md not found")
	}

	// Verify the returned path is actually readable (it resolves to the REVIEW.md inode).
	if _, err := os.ReadFile(got); err != nil {
		t.Errorf("returned path %q is not readable: %v", got, err)
	}

	// Confirm ParseReviewFindingsFromArtifact also succeeds on the found path.
	findings, err := ParseReviewFindingsFromArtifact(got)
	if err != nil {
		t.Errorf("ParseReviewFindingsFromArtifact(%q) returned error: %v", got, err)
	}
	_ = findings           // content verified separately; here we only care about reachability
	_ = filepath.Base(got) // suppress unused import lint
}
