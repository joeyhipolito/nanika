package git_test

import (
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/git"
)

func TestBranchName_Format(t *testing.T) {
	branch := git.BranchName("20260316-ab12cd34", "implement login feature")
	if !strings.HasPrefix(branch, "via/20260316-ab12cd34/") {
		t.Errorf("BranchName missing expected prefix: %s", branch)
	}
	slug := strings.TrimPrefix(branch, "via/20260316-ab12cd34/")
	if slug != "implement-login-feature" {
		t.Errorf("slug = %q, want %q", slug, "implement-login-feature")
	}
}

func TestBranchName_SlugMaxLen(t *testing.T) {
	longTask := strings.Repeat("a", 100)
	branch := git.BranchName("id", longTask)
	slug := strings.TrimPrefix(branch, "via/id/")
	if len(slug) > 40 {
		t.Errorf("slug length %d exceeds 40: %s", len(slug), slug)
	}
}

func TestBranchName_SlugNoTrailingHyphen(t *testing.T) {
	// Task that would produce trailing hyphen at truncation boundary.
	// 38 'a's + "- extra" → truncation at 40 may land on a hyphen.
	task := strings.Repeat("a", 38) + "- extra words here"
	branch := git.BranchName("id", task)
	slug := strings.TrimPrefix(branch, "via/id/")
	if strings.HasSuffix(slug, "-") {
		t.Errorf("slug has trailing hyphen: %s", slug)
	}
}

func TestBranchName_SpecialCharsStripped(t *testing.T) {
	branch := git.BranchName("id", "Fix: bug #123 (urgent!)")
	slug := strings.TrimPrefix(branch, "via/id/")
	for _, ch := range slug {
		if !((ch >= 'a' && ch <= 'z') || (ch >= '0' && ch <= '9') || ch == '-') {
			t.Errorf("slug contains disallowed character %q: %s", ch, slug)
		}
	}
}

func TestBranchName_Lowercase(t *testing.T) {
	branch := git.BranchName("id", "IMPLEMENT OAUTH2 FLOW")
	slug := strings.TrimPrefix(branch, "via/id/")
	if slug != strings.ToLower(slug) {
		t.Errorf("slug is not lowercase: %s", slug)
	}
}

func TestBranchName_EmptyTask(t *testing.T) {
	branch := git.BranchName("id", "")
	slug := strings.TrimPrefix(branch, "via/id/")
	if slug == "" {
		t.Errorf("slug should not be empty for empty task, got empty string")
	}
}

func TestBranchName_OnlySpecialChars(t *testing.T) {
	branch := git.BranchName("id", "!@#$%^&*()")
	slug := strings.TrimPrefix(branch, "via/id/")
	if slug == "" {
		t.Errorf("slug should not be empty for all-special-chars task")
	}
	if strings.HasPrefix(slug, "-") || strings.HasSuffix(slug, "-") {
		t.Errorf("slug has leading/trailing hyphens: %s", slug)
	}
}

func TestBranchName_MultipleSpaces(t *testing.T) {
	branch := git.BranchName("id", "add   user   auth")
	slug := strings.TrimPrefix(branch, "via/id/")
	if strings.Contains(slug, "--") {
		t.Errorf("slug has consecutive hyphens: %s", slug)
	}
}

func TestBranchName_NumbersPreserved(t *testing.T) {
	branch := git.BranchName("id", "fix issue 42")
	slug := strings.TrimPrefix(branch, "via/id/")
	if !strings.Contains(slug, "42") {
		t.Errorf("slug should preserve numbers: %s", slug)
	}
}
