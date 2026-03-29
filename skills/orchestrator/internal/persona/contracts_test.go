package persona

import (
	"os"
	"path/filepath"
	"testing"
)

func TestContractForRole_KnownRoles(t *testing.T) {
	for _, role := range []string{"reviewer", "planner", "implementer"} {
		c := ContractForRole(role)
		if c.Role != role {
			t.Errorf("ContractForRole(%q).Role = %q, want %q", role, c.Role, role)
		}
	}
}

func TestContractForRole_UnknownRole(t *testing.T) {
	c := ContractForRole("unknown")
	if c.Role != "unknown" {
		t.Errorf("ContractForRole(unknown).Role = %q, want %q", c.Role, "unknown")
	}
	if len(c.RequiredPatterns) != 0 {
		t.Errorf("unknown role should have no required patterns, got %d", len(c.RequiredPatterns))
	}
}

func TestValidateOutput_ReviewerMissingPatterns(t *testing.T) {
	// Use temp dir to avoid loading real personas
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	// Reset catalog
	mu.Lock()
	catalog = nil
	mu.Unlock()

	warnings := ValidateOutput("nonexistent-persona", "reviewer", "Some output without required headers")
	if len(warnings) != 2 {
		t.Fatalf("expected 2 warnings for reviewer missing patterns, got %d", len(warnings))
	}

	wantPatterns := map[string]bool{"### Blockers": false, "### Warnings": false}
	for _, w := range warnings {
		wantPatterns[w.Pattern] = true
		if w.Role != "reviewer" {
			t.Errorf("warning.Role = %q, want reviewer", w.Role)
		}
	}
	for p, found := range wantPatterns {
		if !found {
			t.Errorf("missing warning for pattern %q", p)
		}
	}
}

func TestValidateOutput_ReviewerAllPresent(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	mu.Lock()
	catalog = nil
	mu.Unlock()

	output := "## Review\n\n### Blockers\nNone\n\n### Warnings\nNone"
	warnings := ValidateOutput("nonexistent-persona", "reviewer", output)
	if len(warnings) != 0 {
		t.Errorf("expected 0 warnings when all patterns present, got %d", len(warnings))
		for _, w := range warnings {
			t.Logf("  warning: %s", w)
		}
	}
}

func TestValidateOutput_PlannerMissingHeaders(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	mu.Lock()
	catalog = nil
	mu.Unlock()

	warnings := ValidateOutput("nonexistent-persona", "planner", "no headers here just text")
	if len(warnings) != 1 {
		t.Fatalf("expected 1 warning for planner missing ## header, got %d", len(warnings))
	}
	if warnings[0].Pattern != `(?m)^## .+` {
		t.Errorf("unexpected pattern: %s", warnings[0].Pattern)
	}
}

func TestValidateOutput_PlannerWithHeaders(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	mu.Lock()
	catalog = nil
	mu.Unlock()

	output := "## Architecture Decision\n\nWe chose SQLite because..."
	warnings := ValidateOutput("nonexistent-persona", "planner", output)
	if len(warnings) != 0 {
		t.Errorf("expected 0 warnings for planner with ## headers, got %d", len(warnings))
	}
}

func TestValidateOutput_ImplementerMissingFilePaths(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	mu.Lock()
	catalog = nil
	mu.Unlock()

	warnings := ValidateOutput("nonexistent-persona", "implementer", "I made some changes to the code")
	if len(warnings) != 1 {
		t.Fatalf("expected 1 warning for implementer missing file paths, got %d", len(warnings))
	}
}

func TestValidateOutput_ImplementerWithFilePaths(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	mu.Lock()
	catalog = nil
	mu.Unlock()

	output := "Modified internal/persona/contracts.go to add validation"
	warnings := ValidateOutput("nonexistent-persona", "implementer", output)
	if len(warnings) != 0 {
		t.Errorf("expected 0 warnings for implementer with file paths, got %d", len(warnings))
	}
}

func TestValidateOutput_PersonaOutputRequiresOverridesRole(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	// Write a persona file with output_requires that differ from the role default.
	content := `---
role: reviewer
output_requires:
  - "## Findings"
  - "## Recommendation"
---

# Test Reviewer
`
	if err := os.WriteFile(filepath.Join(dir, "test-reviewer.md"), []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	mu.Lock()
	catalog = nil
	mu.Unlock()

	// Output satisfies the role default (### Blockers) but NOT the persona-specific requirements.
	output := "### Blockers\nNone\n### Warnings\nNone"
	warnings := ValidateOutput("test-reviewer", "reviewer", output)
	if len(warnings) != 2 {
		t.Fatalf("expected 2 warnings (persona overrides role), got %d", len(warnings))
	}

	wantPatterns := map[string]bool{"## Findings": false, "## Recommendation": false}
	for _, w := range warnings {
		wantPatterns[w.Pattern] = true
		if w.Message != "required by persona output_requires" {
			t.Errorf("unexpected message: %s", w.Message)
		}
	}
	for p, found := range wantPatterns {
		if !found {
			t.Errorf("missing warning for persona pattern %q", p)
		}
	}
}

func TestValidateOutput_PersonaOutputRequiresAllSatisfied(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	content := `---
role: reviewer
output_requires:
  - "## Findings"
---

# Test Reviewer
`
	if err := os.WriteFile(filepath.Join(dir, "test-reviewer.md"), []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	mu.Lock()
	catalog = nil
	mu.Unlock()

	output := "## Findings\nNo issues found."
	warnings := ValidateOutput("test-reviewer", "reviewer", output)
	if len(warnings) != 0 {
		t.Errorf("expected 0 warnings when persona output_requires satisfied, got %d", len(warnings))
	}
}

func TestValidateOutput_UnknownRole(t *testing.T) {
	dir := t.TempDir()
	SetDir(dir)
	t.Cleanup(func() { SetDir("") })

	mu.Lock()
	catalog = nil
	mu.Unlock()

	warnings := ValidateOutput("nonexistent", "unknown-role", "any output")
	if len(warnings) != 0 {
		t.Errorf("unknown role should produce no warnings, got %d", len(warnings))
	}
}

func TestContractWarning_String(t *testing.T) {
	w := ContractWarning{
		PersonaName: "staff-code-reviewer",
		Role:        "reviewer",
		Pattern:     "### Blockers",
		Message:     "expected in reviewer output",
	}
	s := w.String()
	if s == "" {
		t.Error("ContractWarning.String() returned empty string")
	}
	for _, want := range []string{"staff-code-reviewer", "reviewer", "### Blockers"} {
		if !contains(s, want) {
			t.Errorf("ContractWarning.String() missing %q in %q", want, s)
		}
	}
}

func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(s) > 0 && containsHelper(s, substr))
}

func containsHelper(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}
