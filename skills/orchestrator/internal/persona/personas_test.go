package persona

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

func TestLoad(t *testing.T) {
	if err := Load(); err != nil {
		t.Fatalf("Load() failed: %v", err)
	}

	all := All()
	if len(all) == 0 {
		t.Fatal("Load() produced empty catalog")
	}

	dirEntries, err := os.ReadDir(personasDir())
	if err != nil {
		t.Fatalf("could not read personas dir to derive expected count: %v", err)
	}
	wantCount := 0
	for _, e := range dirEntries {
		if !e.IsDir() && filepath.Ext(e.Name()) == ".md" {
			wantCount++
		}
	}
	if len(all) != wantCount {
		t.Errorf("expected %d personas (matching disk count), got %d", wantCount, len(all))
		for name := range all {
			t.Logf("  loaded: %s", name)
		}
	}

	// Every persona should have content and title
	for name, p := range all {
		if p.Content == "" {
			t.Errorf("persona %q has empty content", name)
		}
		if p.Title == "" {
			t.Errorf("persona %q has empty title", name)
		}
		if len(p.WhenToUse) == 0 {
			t.Errorf("persona %q has no WhenToUse triggers — matching will never pick it", name)
		}
	}
}

func TestGet(t *testing.T) {
	Load()

	tests := []struct {
		name   string
		exists bool
	}{
		{"senior-backend-engineer", true},
		{"architect", true},
		{"academic-researcher", true},
		{"security-auditor", true},
		{"data-analyst", true},
		{"technical-writer", true},
		{"senior-frontend-engineer", true},
		{"qa-engineer", true},
		{"devops-engineer", true},
		{"staff-code-reviewer", true},
		// Should NOT exist — renamed or removed
		{"senior-golang-engineer", false},
		{"senior-rust-engineer", false},
		{"backend-engineer", false},
		{"frontend-engineer", false},
		{"code-reviewer", false},
		{"researcher", false},
		{"storyteller", false},
		{"artist", false},
		{"principal-systems-reviewer", false},
		{"journaler", false},
		{"indie-coach", false},
		{"methodologist", false},
		{"implementer", false},
		{"reviewer", false},
		{"writer", false},
		{"nonexistent", false},
	}

	for _, tt := range tests {
		p := Get(tt.name)
		if tt.exists && p == nil {
			t.Errorf("Get(%q) = nil, want non-nil", tt.name)
		}
		if !tt.exists && p != nil {
			t.Errorf("Get(%q) = non-nil, want nil", tt.name)
		}
	}
}

func TestGetPrompt(t *testing.T) {
	Load()

	prompt := GetPrompt("architect")
	if prompt == "" {
		t.Fatal("GetPrompt(architect) returned empty string")
	}

	// Should contain key sections
	if !containsStr(prompt, "## Identity") {
		t.Error("architect prompt missing ## Identity section")
	}
	if !containsStr(prompt, "## When to Use") {
		t.Error("architect prompt missing ## When to Use section")
	}

	// Frontmatter must be stripped — the raw YAML block must not appear in the prompt.
	if containsStr(prompt, "---\nrole:") || containsStr(prompt, "---\ncapabilities:") {
		t.Error("GetPrompt returned content with frontmatter still present")
	}
}

// TestKeywordMatch_PersonaSelection tests the offline keyword fallback.
// The LLM-based Match() is tested separately as an integration test.
// Keyword matching is the safety net — it doesn't need to be perfect,
// but it should be reasonable for common cases.
func TestKeywordMatch_PersonaSelection(t *testing.T) {
	Load()

	tests := []struct {
		task       string
		acceptable []string // keyword matching is fuzzy, allow reasonable alternatives
		desc       string
	}{
		// Strong signals — keyword fallback should get these right
		{"design the authentication system for the API", []string{"architect"}, "system design → architect"},
		{"implement the user registration endpoint in Go", []string{"senior-backend-engineer"}, "Go implementation → senior-backend-engineer"},
		{"build the login form with React", []string{"senior-frontend-engineer"}, "React → senior-frontend-engineer"},
		{"research the best Go frameworks for REST APIs", []string{"architect"}, "technical comparison research → architect"},
		{"audit the reddit CLI for security vulnerabilities", []string{"security-auditor", "staff-code-reviewer"}, "security audit"},
		{"write unit tests for the decomposer package", []string{"qa-engineer"}, "unit tests → qa-engineer"},
		{"set up the CI/CD pipeline for the orchestrator", []string{"devops-engineer"}, "CI/CD → devops-engineer"},
		{"write a blog post about our orchestrator rewrite", []string{"technical-writer", "academic-researcher"}, "technical article/blog post"},
		{"conduct a literature review on pair programming productivity", []string{"academic-researcher"}, "literature review"},
		{"review the pull request for the auth module", []string{"staff-code-reviewer", "qa-engineer"}, "PR review"},
	}

	failures := 0
	for _, tt := range tests {
		got := keywordMatch(tt.task)
		ok := false
		for _, a := range tt.acceptable {
			if got == a {
				ok = true
				break
			}
		}
		if !ok {
			t.Errorf("MISMATCH: %s\n  task:       %q\n  got:        %s\n  acceptable: %v", tt.desc, tt.task, got, tt.acceptable)
			failures++
			t.Logf("  top 3: %v", MatchTop(tt.task, 3))
		}
	}

	if failures > 0 {
		t.Logf("\n%d/%d keyword matching failures", failures, len(tests))
	}
}

// TestKeywordMatch_AmbiguousTasks tests edge cases with keyword fallback.
func TestKeywordMatch_AmbiguousTasks(t *testing.T) {
	Load()

	tests := []struct {
		task       string
		acceptable []string
		desc       string
	}{
		{
			"research and implement a caching layer",
			[]string{"architect", "senior-backend-engineer", "academic-researcher"},
			"mixed research+implementation",
		},
		{
			"write tests and documentation for the API",
			[]string{"qa-engineer", "technical-writer"},
			"mixed testing+docs",
		},
		{
			"design and build the notification system",
			[]string{"architect", "senior-backend-engineer"},
			"mixed design+build",
		},
		{
			"review and fix security vulnerabilities in the auth module",
			[]string{"security-auditor", "staff-code-reviewer", "senior-backend-engineer"},
			"mixed review+fix+security",
		},
		{
			"analyze performance data and optimize the query engine",
			[]string{"data-analyst", "senior-backend-engineer", "architect"},
			"mixed analysis+optimization",
		},
	}

	for _, tt := range tests {
		got := keywordMatch(tt.task)
		acceptable := false
		for _, a := range tt.acceptable {
			if got == a {
				acceptable = true
				break
			}
		}
		if !acceptable {
			t.Errorf("UNEXPECTED: %s\n  task:       %q\n  got:        %s\n  acceptable: %v", tt.desc, tt.task, got, tt.acceptable)
		}
	}
}

// TestMatch_DefaultFallback ensures unknown tasks get a valid persona (not hardcoded).
func TestMatch_DefaultFallback(t *testing.T) {
	Load()

	obscure := []string{
		"do the thing",
		"help me",
		"go",
		"",
	}

	for _, task := range obscure {
		got := Match(task)
		if got == "" {
			t.Errorf("Match(%q) returned empty string", task)
		}
		// We just verify it returns SOMETHING valid
		if Get(got) == nil {
			t.Errorf("Match(%q) = %q which is not a valid persona", task, got)
		}
	}
}

// TestMatchTop verifies top-N matching returns reasonable rankings.
func TestMatchTop_Rankings(t *testing.T) {
	Load()

	top := MatchTop("audit the CLI for security vulnerabilities and write a report", 3)
	if len(top) == 0 {
		t.Fatal("MatchTop returned empty")
	}

	// security-auditor should be in top 3
	found := false
	for _, name := range top {
		if name == "security-auditor" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("security-auditor not in top 3 for security audit task: got %v", top)
	}
}

// TestFormatForDecomposer ensures the decomposer summary is well-formed.
func TestFormatForDecomposer(t *testing.T) {
	Load()

	summary := FormatForDecomposer()
	if summary == "" {
		t.Fatal("FormatForDecomposer returned empty string")
	}

	// Should mention every persona
	for name := range All() {
		if !containsStr(summary, name) {
			t.Errorf("FormatForDecomposer() missing persona %q", name)
		}
	}
}

// TestExtractSection verifies the section extraction logic.
func TestExtractSection(t *testing.T) {
	content := `# Test Persona

## When to Use

- Writing unit tests
- Integration testing
- End-to-end test automation

## When NOT to Use

- Performance optimization
- Architecture design

## Principles

1. Test first.
`

	whenToUse := extractSection(content, "## When to Use")
	if len(whenToUse) != 3 {
		t.Errorf("expected 3 WhenToUse items, got %d: %v", len(whenToUse), whenToUse)
	}

	whenNotUse := extractSection(content, "## When NOT to Use")
	if len(whenNotUse) != 2 {
		t.Errorf("expected 2 WhenNotUse items, got %d: %v", len(whenNotUse), whenNotUse)
	}
}

// TestExtractHandoffs verifies handoff parsing from WhenNotUse entries.
func TestExtractHandoffs(t *testing.T) {
	cat := map[string]*Persona{
		"architect":               {},
		"senior-backend-engineer": {},
		"technical-writer":        {},
	}

	tests := []struct {
		name  string
		items []string
		want  []string
	}{
		{
			"single handoff",
			[]string{"Technical synthesis (hand off to technical-writer)"},
			[]string{"technical-writer"},
		},
		{
			"multiple handoffs",
			[]string{
				"Writing code (hand off to senior-backend-engineer)",
				"Designing systems (hand off to architect)",
			},
			[]string{"architect", "senior-backend-engineer"},
		},
		{
			"no handoffs",
			[]string{"Performance optimization", "Architecture design"},
			nil,
		},
		{
			"unknown persona filtered out",
			[]string{"hand off to nonexistent-persona"},
			nil,
		},
		{
			"deduplicates",
			[]string{
				"Writing code (hand off to senior-backend-engineer)",
				"Also writing code (hand off to senior-backend-engineer)",
			},
			[]string{"senior-backend-engineer"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := extractHandoffs(tt.items, cat)
			if len(got) != len(tt.want) {
				t.Fatalf("extractHandoffs() = %v, want %v", got, tt.want)
			}
			for i := range got {
				if got[i] != tt.want[i] {
					t.Errorf("extractHandoffs()[%d] = %q, want %q", i, got[i], tt.want[i])
				}
			}
		})
	}
}

// TestParseFrontmatter verifies the YAML frontmatter parser handles common cases.
func TestParseFrontmatter(t *testing.T) {
	tests := []struct {
		name     string
		content  string
		wantFM   frontmatter
		wantBody string
	}{
		{
			name:     "no frontmatter",
			content:  "# Title\n\n## Identity\nHello",
			wantFM:   frontmatter{},
			wantBody: "# Title\n\n## Identity\nHello",
		},
		{
			name:    "full frontmatter",
			content: "---\nrole: implementer\ncapabilities:\n  - Go development\n  - HTTP servers\ntriggers:\n  - implement\n  - build\nhandoffs:\n  - architect\n---\n\n# Title",
			wantFM: frontmatter{
				Role:         "implementer",
				Capabilities: []string{"Go development", "HTTP servers"},
				Triggers:     []string{"implement", "build"},
				Handoffs:     []string{"architect"},
			},
			wantBody: "# Title",
		},
		{
			name:     "opening --- but no closing --- is not frontmatter",
			content:  "---\nrole: foo\n",
			wantFM:   frontmatter{},
			wantBody: "---\nrole: foo\n",
		},
		{
			name:     "role only",
			content:  "---\nrole: planner\n---\n\n# Architect",
			wantFM:   frontmatter{Role: "planner"},
			wantBody: "# Architect",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fm, body := parseFrontmatter(tt.content)
			if fm.Role != tt.wantFM.Role {
				t.Errorf("Role = %q, want %q", fm.Role, tt.wantFM.Role)
			}
			if len(fm.Capabilities) != len(tt.wantFM.Capabilities) {
				t.Errorf("Capabilities = %v, want %v", fm.Capabilities, tt.wantFM.Capabilities)
			}
			if len(fm.Triggers) != len(tt.wantFM.Triggers) {
				t.Errorf("Triggers = %v, want %v", fm.Triggers, tt.wantFM.Triggers)
			}
			if len(fm.Handoffs) != len(tt.wantFM.Handoffs) {
				t.Errorf("Handoffs = %v, want %v", fm.Handoffs, tt.wantFM.Handoffs)
			}
			if body != tt.wantBody {
				t.Errorf("body = %q, want %q", body, tt.wantBody)
			}
		})
	}
}

// TestFrontmatterFieldsOnLoad verifies that structured frontmatter fields are
// populated when a persona file with frontmatter is loaded.
// Uses a synthetic temp dir so the test is independent of the live personas path.
func TestFrontmatterFieldsOnLoad(t *testing.T) {
	dir := t.TempDir()
	content := "---\nrole: implementer\ncapabilities:\n  - Go development\n  - HTTP servers\ntriggers:\n  - implement\n  - build\nhandoffs:\n  - qa-engineer\n---\n\n# Test Engineer — Does things\n\n## Identity\nTest identity.\n\n## Goal\nTest goal.\n\n## When to Use\n- implement tasks\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n"
	writePersona(t, dir, "test-engineer", content)
	// A second persona for the handoff target.
	writePersona(t, dir, "qa-engineer", "# QA Engineer\n\n## Identity\nX\n\n## Goal\nX\n\n## When to Use\n- testing\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	p := Get("test-engineer")
	if p == nil {
		t.Fatal("test-engineer not loaded")
	}

	if p.Role != "implementer" {
		t.Errorf("Role = %q, want %q", p.Role, "implementer")
	}
	if len(p.Capabilities) != 2 {
		t.Errorf("Capabilities = %v, want 2 items", p.Capabilities)
	}
	if len(p.Triggers) != 2 || p.Triggers[0] != "implement" {
		t.Errorf("Triggers = %v, want [implement build]", p.Triggers)
	}
	// WhenToUse should keep the richer markdown bullets for keyword scoring.
	if len(p.WhenToUse) != 1 || p.WhenToUse[0] != "implement tasks" {
		t.Errorf("WhenToUse = %v, want markdown section [implement tasks]", p.WhenToUse)
	}
	// HandsOffTo must resolve frontmatter handoffs against catalog.
	found := false
	for _, h := range p.HandsOffTo {
		if h == "qa-engineer" {
			found = true
		}
	}
	if !found {
		t.Errorf("HandsOffTo = %v, expected qa-engineer", p.HandsOffTo)
	}
	// PromptBody must not start with --- (frontmatter stripped).
	if strings.HasPrefix(p.PromptBody, "---") {
		t.Errorf("PromptBody still contains frontmatter: %q", p.PromptBody[:20])
	}
	// PromptBody must contain the heading.
	if !containsStr(p.PromptBody, "# Test Engineer") {
		t.Errorf("PromptBody missing heading, got: %q", p.PromptBody[:50])
	}
	// GetPrompt must return the stripped body.
	prompt := GetPrompt("test-engineer")
	if strings.HasPrefix(prompt, "---") {
		t.Errorf("GetPrompt returned content with frontmatter: %q", prompt[:20])
	}
}

// TestFilterHandoffs verifies that filterHandoffs returns only catalog-resident names.
func TestFilterHandoffs(t *testing.T) {
	cat := map[string]*Persona{
		"architect":               {},
		"senior-backend-engineer": {},
	}

	got := filterHandoffs([]string{"senior-backend-engineer", "unknown", "architect", "architect"}, cat)
	want := []string{"architect", "senior-backend-engineer"}
	if len(got) != len(want) {
		t.Fatalf("filterHandoffs() = %v, want %v", got, want)
	}
	for i := range got {
		if got[i] != want[i] {
			t.Errorf("filterHandoffs()[%d] = %q, want %q", i, got[i], want[i])
		}
	}
}

// TestHandoffPopulatedOnLoad verifies HandsOffTo is populated after Load().
func TestHandoffPopulatedOnLoad(t *testing.T) {
	Load()

	ar := Get("academic-researcher")
	if ar == nil {
		t.Fatal("academic-researcher persona not found")
	}

	found := false
	for _, h := range ar.HandsOffTo {
		if h == "technical-writer" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("academic-researcher.HandsOffTo = %v, expected 'technical-writer' to be present", ar.HandsOffTo)
	}
}

// TestFormatForDecomposerIncludesHandoffs verifies handoff lines appear in decomposer output.
func TestFormatForDecomposerIncludesHandoffs(t *testing.T) {
	Load()

	summary := FormatForDecomposer()

	if !containsStr(summary, "Hands off to:") {
		t.Error("FormatForDecomposer() output missing 'Hands off to:' lines")
	}

	if !containsStr(summary, "technical-writer") {
		t.Error("FormatForDecomposer() should mention technical-writer as a handoff target")
	}
}

// TestKeywordMatch_NegativeScoring verifies WhenNotUse penalties differentiate
// personas that would otherwise tie or lose to backend-engineer.
func TestKeywordMatch_NegativeScoring(t *testing.T) {
	Load()

	tests := []struct {
		task    string
		want    string
		notWant string
		desc    string
	}{
		{
			"audit the authentication system for security vulnerabilities",
			"security-auditor",
			"senior-backend-engineer",
			"security audit → security-auditor, not senior-backend-engineer",
		},
		{
			"design the plugin architecture for the orchestrator system",
			"architect",
			"senior-backend-engineer",
			"system design → architect, not senior-backend-engineer",
		},
		{
			"review the pull request for the auth module",
			"staff-code-reviewer",
			"senior-backend-engineer",
			"PR review → staff-code-reviewer, not senior-backend-engineer",
		},
	}

	for _, tt := range tests {
		got := keywordMatch(tt.task)
		if got == tt.notWant {
			t.Errorf("BIAS: %s\n  task: %q\n  got:  %s\n  want: %s\n  top3: %v",
				tt.desc, tt.task, got, tt.want, MatchTop(tt.task, 3))
		}
	}
}

// TestFormatForDecomposer_DeterministicOrder verifies stable output across calls.
func TestFormatForDecomposer_DeterministicOrder(t *testing.T) {
	Load()

	first := FormatForDecomposer()
	for i := 0; i < 10; i++ {
		if got := FormatForDecomposer(); got != first {
			t.Errorf("FormatForDecomposer() returned different output on call %d", i+1)
			break
		}
	}
}

// TestMatchWithMethod verifies selection method is returned and is a known value.
func TestMatchWithMethod(t *testing.T) {
	Load()

	name, method := MatchWithMethod("write unit tests for the API handler")
	if Get(name) == nil {
		t.Errorf("MatchWithMethod returned invalid persona %q", name)
	}
	if method == "" {
		t.Error("MatchWithMethod returned empty SelectionMethod")
	}
	switch method {
	case SelectionLLM, SelectionKeyword, SelectionFallback:
		// valid
	default:
		t.Errorf("MatchWithMethod method = %q, want one of llm/keyword/fallback", method)
	}
}

// TestHandoffIntegrity verifies every HandsOffTo target is a real persona in the catalog.
// A broken handoff (e.g., "hand off to the relevant engineer") silently breaks
// the decomposer's ability to chain phases.
func TestHandoffIntegrity(t *testing.T) {
	Load()

	for name, p := range All() {
		for _, target := range p.HandsOffTo {
			if Get(target) == nil {
				t.Errorf("%s hands off to %q which is not a valid persona", name, target)
			}
		}
	}
}

// TestSectionCompliance verifies every persona has the required markdown sections.
// These sections are either parsed by the code (WhenToUse, WhenNotUse, LearningFocus)
// or expected by convention (Identity, Goal, etc.).
// Checks PromptBody (frontmatter stripped) so the YAML block is not counted.
func TestSectionCompliance(t *testing.T) {
	Load()

	requiredSections := []string{
		"## Identity",
		"## Goal",
		"## When to Use",
		"## When NOT to Use",
		"## Self-Check",
	}

	for name, p := range All() {
		body := p.PromptBody
		if body == "" {
			body = p.Content
		}
		for _, section := range requiredSections {
			if !containsStr(body, section) {
				t.Errorf("persona %q missing required section %q", name, section)
			}
		}
	}
}

// TestRoutingAccuracyBenchmark is a formal benchmark with ground-truth task→persona
// mappings. The accuracy score here is the primary metric for measuring persona
// routing quality across commits. Each task has exactly ONE correct persona.
// Run: go test ./internal/persona/... -run TestRoutingAccuracyBenchmark -v
func TestRoutingAccuracyBenchmark(t *testing.T) {
	Load()

	// Ground-truth: task description → expected correct persona.
	// These represent unambiguous real-world tasks where one persona is clearly best.
	benchmark := []struct {
		task    string
		correct string
		desc    string
	}{
		{"fix the goroutine leak in the worker pool", "senior-backend-engineer", "goroutine bug"},
		{"implement a REST endpoint for user authentication in Go", "senior-backend-engineer", "Go REST endpoint"},
		{"add SQLite migration for the new learnings table", "senior-backend-engineer", "DB migration"},

		// Frontend engineering
		{"build the dashboard component with React and Tailwind", "senior-frontend-engineer", "React component"},
		{"fix the CSS layout bug in the navigation bar", "senior-frontend-engineer", "CSS fix"},

		// Architecture
		{"design the plugin architecture for the orchestrator", "architect", "system design"},
		{"choose between PostgreSQL and SQLite for the new service", "architect", "tech decision"},

		// Code review
		{"review the pull request for the authentication module", "staff-code-reviewer", "PR review"},
		{"assess the integration readiness of the new auth subsystem", "architect", "systems review"},
		{"validate contract stability between auth and payment services", "architect", "cross-service contract"},
		{"review the deployment rollback path for the new migration", "devops-engineer", "deployment rollback"},
		{"operational readiness check before launching the new queue", "devops-engineer", "operational readiness"},
		{"review this Go handler for nil pointer issues", "staff-code-reviewer", "code-level negative case"},

		// QA
		{"write integration tests for the API endpoints", "qa-engineer", "integration tests"},
		{"set up end-to-end test automation for the CLI", "qa-engineer", "e2e tests"},

		// Security
		{"audit the authentication system for security vulnerabilities", "security-auditor", "security audit"},

		// Research
		{"research the latest Go frameworks for building REST APIs", "architect", "tech research"},
		{"conduct a literature review on pair programming productivity", "academic-researcher", "literature review"},

		// DevOps
		{"set up the CI/CD pipeline with GitHub Actions", "devops-engineer", "CI/CD setup"},
		{"configure Docker containers for the service", "devops-engineer", "Docker config"},

		// Data analysis
		{"analyze the performance metrics from the last 30 days", "data-analyst", "metrics analysis"},

		// Documentation / technical synthesis
		{"write API documentation for the orchestrator endpoints", "technical-writer", "API docs"},
		{"write a technical article about our orchestrator architecture", "technical-writer", "technical article"},
	}

	correct := 0
	total := len(benchmark)
	for _, tt := range benchmark {
		got := keywordMatch(tt.task)
		if got == tt.correct {
			correct++
		} else {
			t.Logf("MISS [%s]: want %s, got %s (top3: %v)",
				tt.desc, tt.correct, got, MatchTop(tt.task, 3))
		}
	}

	accuracy := float64(correct) * 100 / float64(total)
	t.Logf("\n=== Routing Accuracy Benchmark ===")
	t.Logf("Score: %d/%d (%.1f%%)", correct, total, accuracy)
	t.Logf("==================================")

	// Threshold: fail if accuracy drops below 60%.
	// As personas improve, raise this threshold.
	if accuracy < 60 {
		t.Errorf("routing accuracy %.1f%% is below 60%% threshold", accuracy)
	}
}

// TestSelectionMethodLabels verifies all selection-source constants are defined
// with the expected string values. These labels appear in phase metadata and audit
// output; changing them is a breaking change for downstream consumers.
func TestSelectionMethodLabels(t *testing.T) {
	tests := []struct {
		method SelectionMethod
		want   string
	}{
		{SelectionExplicit, "explicit"},
		{SelectionCorrection, "correction"},
		{SelectionTargetProfile, "target_profile"},
		{SelectionRoutingPattern, "routing_pattern"},
		{SelectionLLM, "llm"},
		{SelectionKeyword, "keyword"},
		{SelectionFallback, "fallback"},
	}
	for _, tt := range tests {
		if string(tt.method) != tt.want {
			t.Errorf("SelectionMethod constant value = %q, want %q", tt.method, tt.want)
		}
	}
}

// TestKeywordMatchWithMethod_FallbackLabel verifies that tasks with no recognizable
// keywords return SelectionFallback (not SelectionKeyword) so callers can distinguish
// a true keyword match from an alphabetical-default assignment.
func TestKeywordMatchWithMethod_FallbackLabel(t *testing.T) {
	Load()

	// These tasks have no recognizable keyword signals; scoring returns 0 for all
	// personas, triggering the alphabetical fallback path.
	obscure := []string{"", "xyzzy plugh", "do the thing"}

	for _, task := range obscure {
		name, method := keywordMatchWithMethod(task)
		if name == "" {
			t.Errorf("keywordMatchWithMethod(%q) returned empty persona name", task)
			continue
		}
		if Get(name) == nil {
			t.Errorf("keywordMatchWithMethod(%q) = %q is not in catalog", task, name)
			continue
		}
		if method != SelectionFallback {
			t.Errorf("keywordMatchWithMethod(%q) method = %q, want %q (no keyword match → alphabetical fallback)", task, method, SelectionFallback)
		}
	}
}

// TestKeywordMatchWithMethod_KeywordLabel verifies that tasks with clear keyword
// signals return SelectionKeyword, not SelectionFallback.
func TestKeywordMatchWithMethod_KeywordLabel(t *testing.T) {
	Load()

	tests := []struct {
		task string
		desc string
	}{
		{"implement a REST endpoint in Go", "implementation keyword"},
		{"audit the authentication system for security vulnerabilities", "security audit"},
		{"write unit tests for the parser package", "testing keyword"},
		{"design the plugin architecture for the orchestrator", "architecture keyword"},
	}

	for _, tt := range tests {
		_, method := keywordMatchWithMethod(tt.task)
		if method != SelectionKeyword {
			t.Errorf("[%s] keywordMatchWithMethod(%q) method = %q, want %q", tt.desc, tt.task, method, SelectionKeyword)
		}
	}
}

// ---- Hot-reload tests --------------------------------------------------------

// TestReload_AddedPersona verifies that Reload() picks up a newly created persona file.
func TestReload_AddedPersona(t *testing.T) {
	dir := t.TempDir()
	writePersona(t, dir, "alpha", "# Alpha Engineer\n\n## Identity\nTest\n\n## Goal\nTest\n\n## When to Use\n- alpha tasks\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}
	if Get("alpha") == nil {
		t.Fatal("alpha persona not found after Load")
	}
	if Get("beta") != nil {
		t.Fatal("beta should not exist yet")
	}

	// Add a second persona file, then reload.
	writePersona(t, dir, "beta", "# Beta Engineer\n\n## Identity\nTest\n\n## Goal\nTest\n\n## When to Use\n- beta tasks\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	if err := Reload(); err != nil {
		t.Fatalf("Reload: %v", err)
	}
	if Get("beta") == nil {
		t.Fatal("beta persona not found after Reload")
	}
}

// TestReload_RemovedPersona verifies that Reload() drops a deleted persona.
func TestReload_RemovedPersona(t *testing.T) {
	dir := t.TempDir()
	writePersona(t, dir, "gamma", "# Gamma\n\n## Identity\nTest\n\n## Goal\nTest\n\n## When to Use\n- gamma tasks\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}
	if Get("gamma") == nil {
		t.Fatal("gamma not loaded")
	}

	if err := os.Remove(filepath.Join(dir, "gamma.md")); err != nil {
		t.Fatalf("remove: %v", err)
	}
	if err := Reload(); err != nil {
		t.Fatalf("Reload: %v", err)
	}
	if Get("gamma") != nil {
		t.Fatal("gamma should be gone after Reload")
	}
}

// TestConcurrentAccess verifies that concurrent reads and a reload do not race.
func TestConcurrentAccess(t *testing.T) {
	dir := t.TempDir()
	for i := range 5 {
		writePersona(t, dir, fmt.Sprintf("p%d", i), fmt.Sprintf("# P%d\n\n## Identity\nX\n\n## Goal\nX\n\n## When to Use\n- x\n\n## When NOT to Use\n- y\n\n## Self-Check\n- ok\n", i))
	}
	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 300*time.Millisecond)
	defer cancel()

	var wg sync.WaitGroup

	// Concurrent readers.
	for range 10 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for ctx.Err() == nil {
				_ = All()
				_ = Names()
				_ = Get("p0")
			}
		}()
	}

	// Concurrent reloader.
	wg.Add(1)
	go func() {
		defer wg.Done()
		for ctx.Err() == nil {
			_ = Reload()
			time.Sleep(10 * time.Millisecond)
		}
	}()

	wg.Wait()
}

// TestStartWatcher_HotReload verifies the file watcher picks up a new persona file.
func TestStartWatcher_HotReload(t *testing.T) {
	dir := t.TempDir()
	writePersona(t, dir, "watch-base", "# WatchBase\n\n## Identity\nX\n\n## Goal\nX\n\n## When to Use\n- base\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	StartWatcher(ctx)

	// Write a new persona file and wait for the watcher to reload (up to 2s).
	writePersona(t, dir, "watch-new", "# WatchNew\n\n## Identity\nX\n\n## Goal\nX\n\n## When to Use\n- new\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n")

	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		if Get("watch-new") != nil {
			return // success
		}
		time.Sleep(50 * time.Millisecond)
	}
	t.Error("watcher did not reload within 2s after new persona file was created")
}

// writePersona is a test helper that writes a minimal persona .md file to dir.
func writePersona(t *testing.T, dir, name, content string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, name+".md"), []byte(content), 0644); err != nil {
		t.Fatalf("writePersona(%s): %v", name, err)
	}
}

func containsStr(s, substr string) bool {
	return len(s) > 0 && len(substr) > 0 && (s == substr || len(s) > len(substr) && indexOf(s, substr) >= 0)
}

func indexOf(s, substr string) int {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return i
		}
	}
	return -1
}

// ---- Hot-plug flow tests (full scenarios) ------------------------------------

// TestHotPlug_SyntheticPersonaDiscovered verifies that a newly added persona with
// unique capabilities and triggers is discovered on Load() and routed to by keyword
// matching via its frontmatter triggers.
func TestHotPlug_SyntheticPersonaDiscovered(t *testing.T) {
	dir := t.TempDir()

	// Baseline persona so the catalog is never a singleton.
	writePersona(t, dir, "baseline-engineer", minimalPersonaBody("Baseline Engineer", "baseline tasks"))

	// Synthetic persona with triggers that cannot collide with any real persona.
	// Tokens "zygomorphic" and "xenolithic" are unique non-words not present in
	// any real persona's keyword signals.
	const syntheticContent = `---
role: implementer
capabilities:
  - zygomorphic-calibration
  - xenolithic-synthesis
triggers:
  - zygomorphic
  - xenolithic
handoffs:
  - baseline-engineer
---

# Zygomorphic Calibration Engineer — Calibrates xenolithic resonance

## Identity
Synthetic test persona for hot-plug verification.

## Goal
Be discovered via the hot-plug catalog flow.

## When to Use
- zygomorphic calibration work
- xenolithic synthesis tasks

## When NOT to Use
- normal engineering tasks

## Self-Check
- triggers are unique and non-colliding
`
	writePersona(t, dir, "zygomorphic-engineer", syntheticContent)

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	// 1. Persona is discoverable by name.
	p := Get("zygomorphic-engineer")
	if p == nil {
		t.Fatal("zygomorphic-engineer not found after Load")
	}

	// 2. Frontmatter fields are correctly parsed.
	if p.Role != "implementer" {
		t.Errorf("Role = %q, want %q", p.Role, "implementer")
	}
	if len(p.Capabilities) != 2 {
		t.Errorf("Capabilities = %v, want 2 items", p.Capabilities)
	}
	if len(p.Triggers) != 2 {
		t.Errorf("Triggers = %v, want 2 items", p.Triggers)
	}
	// WhenToUse should preserve the markdown section when present.
	if len(p.WhenToUse) != 2 || p.WhenToUse[0] != "zygomorphic calibration work" {
		t.Errorf("WhenToUse = %v, want markdown section items", p.WhenToUse)
	}
	// HandsOffTo must resolve the frontmatter handoff against the catalog.
	handoffFound := false
	for _, h := range p.HandsOffTo {
		if h == "baseline-engineer" {
			handoffFound = true
		}
	}
	if !handoffFound {
		t.Errorf("HandsOffTo = %v, expected baseline-engineer", p.HandsOffTo)
	}
	// PromptBody must not contain frontmatter.
	if strings.HasPrefix(p.PromptBody, "---") {
		t.Errorf("PromptBody still contains frontmatter block")
	}

	// 3. Keyword routing selects the synthetic persona via its unique triggers.
	got := keywordMatch("zygomorphic calibration work")
	if got != "zygomorphic-engineer" {
		t.Errorf("keywordMatch(zygomorphic) = %q, want zygomorphic-engineer (top3: %v)",
			got, MatchTop("zygomorphic calibration work", 3))
	}
	got2 := keywordMatch("xenolithic synthesis work")
	if got2 != "zygomorphic-engineer" {
		t.Errorf("keywordMatch(xenolithic) = %q, want zygomorphic-engineer (top3: %v)",
			got2, MatchTop("xenolithic synthesis work", 3))
	}
}

// TestHotPlug_SyntheticPersonaRemoved verifies that deleting a persona file and
// calling Reload() removes it from the catalog and stops routing to it.
func TestHotPlug_SyntheticPersonaRemoved(t *testing.T) {
	dir := t.TempDir()
	writePersona(t, dir, "baseline-engineer", minimalPersonaBody("Baseline Engineer", "baseline tasks"))
	writePersona(t, dir, "vestigial-engineer", syntheticPersonaWithTrigger("vestigial"))

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	// Pre-condition: persona is present and routes correctly.
	if Get("vestigial-engineer") == nil {
		t.Fatal("vestigial-engineer not found after initial Load")
	}
	got := keywordMatch("vestigial task")
	if got != "vestigial-engineer" {
		t.Errorf("pre-remove: keywordMatch(vestigial) = %q, want vestigial-engineer", got)
	}

	// Remove the persona file from disk.
	if err := os.Remove(filepath.Join(dir, "vestigial-engineer.md")); err != nil {
		t.Fatalf("remove: %v", err)
	}

	if err := Reload(); err != nil {
		t.Fatalf("Reload: %v", err)
	}

	// Persona must be absent from catalog.
	if Get("vestigial-engineer") != nil {
		t.Error("vestigial-engineer still in catalog after Reload with file deleted")
	}

	// Routing must no longer return the removed persona.
	got2 := keywordMatch("vestigial task")
	if got2 == "vestigial-engineer" {
		t.Error("keywordMatch still routes to removed vestigial-engineer after Reload")
	}
	// Whatever gets returned must still be a valid persona.
	if Get(got2) == nil {
		t.Errorf("keywordMatch after removal returned invalid persona %q", got2)
	}
}

// TestAllRealPersonasFrontmatterRouting verifies that every production persona:
//   - has Role, Capabilities, and Triggers populated from YAML frontmatter
//   - retains non-empty WhenToUse guidance for keyword fallback
//   - appears in the top-3 keyword results for a task built from its distinctive triggers
//
// Uses the worktree's personas/ directory (../../../../personas relative to the package)
// so the test always reflects the in-progress frontmatter additions, not the main-branch state.
func TestAllRealPersonasFrontmatterRouting(t *testing.T) {
	// Point at the worktree personas directory where frontmatter has been added.
	// From skills/orchestrator/internal/persona/, ../../../../ is the repo root.
	SetDir("../../../../personas")
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load real personas from worktree: %v", err)
	}

	// Ground-truth routing table: persona → a task constructed from its most
	// distinctive frontmatter trigger terms.
	routingTable := []struct {
		name string
		task string
	}{
		{"academic-researcher", "conduct a scholarly meta-analysis literature review"},
		{"architect", "design the system architecture and component boundaries trade-off"},
		{"data-analyst", "analyze statistics and data quality metrics trends"},
		{"devops-engineer", "configure Docker pipeline infrastructure monitoring deploy"},
		{"qa-engineer", "write tests for flaky fixtures and edge cases coverage"},
		{"security-auditor", "audit for vulnerability CVE injection threat model"},
		{"senior-backend-engineer", "implement backend database endpoint Go code"},
		{"senior-frontend-engineer", "build React frontend component with Tailwind Next.js"},
		{"staff-code-reviewer", "review pull request PR diff and merge"},
		{"technical-writer", "write documentation README runbook guide"},
	}

	for _, tt := range routingTable {
		t.Run(tt.name, func(t *testing.T) {
			p := Get(tt.name)
			if p == nil {
				t.Fatalf("persona %q not found in catalog", tt.name)
			}

			// Frontmatter fields must be populated for every production persona.
			if p.Role == "" {
				t.Errorf("Role is empty — frontmatter not parsed for %s", tt.name)
			}
			if len(p.Capabilities) == 0 {
				t.Errorf("Capabilities is empty — frontmatter not parsed for %s", tt.name)
			}
			if len(p.Triggers) == 0 {
				t.Errorf("Triggers is empty — frontmatter not parsed for %s", tt.name)
			}

			if len(p.WhenToUse) == 0 {
				t.Errorf("WhenToUse is empty for %s — keyword fallback has no guidance", tt.name)
			}

			// Routing: persona must appear in top-3 for a task constructed from
			// its distinctive trigger terms.
			top3 := MatchTop(tt.task, 3)
			found := false
			for _, candidate := range top3 {
				if candidate == tt.name {
					found = true
					break
				}
			}
			if !found {
				t.Errorf("%s not in top-3 for task %q\n  top3: %v\n  keyword score check: run MatchTop with -v",
					tt.name, tt.task, top3)
			}
		})
	}
}

// TestBackwardCompat_NoFrontmatter verifies that a persona WITHOUT YAML frontmatter
// still loads correctly and works end-to-end:
//   - WhenToUse is extracted from the ## When to Use markdown section
//   - Role/Capabilities/Triggers are zero-valued (no frontmatter)
//   - GetPrompt returns the full content without any stripping
//   - Keyword routing picks it up via markdown-extracted triggers
//   - NamesWithRole falls back to provided fallbackNames when no frontmatter roles exist
func TestBackwardCompat_NoFrontmatter(t *testing.T) {
	dir := t.TempDir()

	// Plain markdown — no opening "---" at all.
	const legacyContent = `# Legacy Engineer — Does legacy things

## Identity
A persona from before frontmatter was introduced.

## Goal
Demonstrate backward compatibility.

## When to Use
- legacy migration tasks
- retrofitting old codebases

## When NOT to Use
- new greenfield projects

## Self-Check
- compatible
`
	writePersona(t, dir, "legacy-engineer", legacyContent)

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	p := Get("legacy-engineer")
	if p == nil {
		t.Fatal("legacy-engineer not found")
	}

	// Frontmatter fields must be zero — no frontmatter in file.
	if p.Role != "" {
		t.Errorf("Role = %q, want empty (no frontmatter)", p.Role)
	}
	if len(p.Capabilities) != 0 {
		t.Errorf("Capabilities = %v, want nil (no frontmatter)", p.Capabilities)
	}
	if len(p.Triggers) != 0 {
		t.Errorf("Triggers = %v, want nil (no frontmatter)", p.Triggers)
	}

	// WhenToUse must fall back to the markdown ## When to Use section.
	if len(p.WhenToUse) != 2 {
		t.Errorf("WhenToUse = %v, want 2 items from markdown section", p.WhenToUse)
	} else {
		if p.WhenToUse[0] != "legacy migration tasks" {
			t.Errorf("WhenToUse[0] = %q, want %q", p.WhenToUse[0], "legacy migration tasks")
		}
	}

	// GetPrompt must return the full content (nothing to strip).
	prompt := GetPrompt("legacy-engineer")
	if prompt == "" {
		t.Fatal("GetPrompt returned empty for legacy persona")
	}
	if !strings.HasPrefix(prompt, "# Legacy Engineer") {
		t.Errorf("GetPrompt should start with heading, got: %q...", prompt[:min(30, len(prompt))])
	}
	if strings.Contains(prompt, "---\nrole:") {
		t.Error("GetPrompt returned frontmatter markers for a persona with no frontmatter")
	}

	// Keyword routing must work via the markdown-extracted WhenToUse.
	got := keywordMatch("legacy migration task")
	if got != "legacy-engineer" {
		t.Errorf("keywordMatch(legacy migration) = %q, want legacy-engineer", got)
	}

	// NamesWithRole fallback: catalog has no frontmatter roles, so fallbackNames must be returned.
	result := NamesWithRole("implementer", []string{"legacy-engineer"})
	found := false
	for _, n := range result {
		if n == "legacy-engineer" {
			found = true
			break
		}
	}
	if !found {
		t.Errorf("NamesWithRole fallback = %v, want to include legacy-engineer", result)
	}
}

func TestMixedCatalog_LegacyPersonaRoleAndCapabilityFallback(t *testing.T) {
	dir := t.TempDir()

	const frontmatterContent = `---
role: implementer
capabilities:
  - Go development
triggers:
  - implement
---

# Modern Engineer

## Identity
Modern persona.

## Goal
Ship code.

## When to Use
- implement services

## When NOT to Use
- reviews

## Self-Check
- ok
`

	const legacyReviewer = `# Legacy Reviewer

## Identity
Legacy markdown-only reviewer.

## Goal
Review changes safely.

## Expertise
- code review
- regression analysis

## When to Use
- review pull requests
- inspect diffs for regressions

## When NOT to Use
- implementation work

## Self-Check
- ok
`

	writePersona(t, dir, "modern-engineer", frontmatterContent)
	writePersona(t, dir, "legacy-reviewer", legacyReviewer)

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	if !HasRole("legacy-reviewer", "reviewer") {
		t.Fatal("legacy-reviewer should participate in role routing even without frontmatter")
	}
	if !HasCapability("legacy-reviewer", "code review") {
		t.Fatal("legacy-reviewer should expose ## Expertise items as legacy capabilities")
	}

	reviewers := NamesWithRole("reviewer", nil)
	if len(reviewers) != 1 || reviewers[0] != "legacy-reviewer" {
		t.Fatalf("NamesWithRole(reviewer) = %v, want [legacy-reviewer]", reviewers)
	}
}

func TestNamesSortedDeterministically(t *testing.T) {
	dir := t.TempDir()
	writePersona(t, dir, "gamma", minimalPersonaBody("Gamma", "gamma tasks"))
	writePersona(t, dir, "alpha", minimalPersonaBody("Alpha", "alpha tasks"))
	writePersona(t, dir, "beta", minimalPersonaBody("Beta", "beta tasks"))

	SetDir(dir)
	t.Cleanup(func() { SetDir(""); mu.Lock(); catalog = nil; mu.Unlock() })

	if err := Load(); err != nil {
		t.Fatalf("Load: %v", err)
	}

	got := Names()
	want := []string{"alpha", "beta", "gamma"}
	if len(got) != len(want) {
		t.Fatalf("Names() len = %d, want %d (%v)", len(got), len(want), got)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("Names()[%d] = %q, want %q (full=%v)", i, got[i], want[i], got)
		}
	}
}

// TestKeywordMatch_Predictability verifies that keyword matching is fully deterministic:
// the same task produces the same persona assignment across 5 consecutive calls.
// This guards against non-determinism from map iteration order or other hidden randomness.
func TestKeywordMatch_Predictability(t *testing.T) {
	Load()

	tasks := []string{
		"implement a REST API endpoint in Go with database access",
		"audit the authentication system for security vulnerabilities",
		"design the plugin architecture for the orchestrator system",
		"write unit tests for the decomposer package",
		"build the dashboard component with React and Tailwind",
		"conduct a literature review on distributed systems",
		"analyze performance metrics from the last 30 days",
		"set up CI/CD pipeline with GitHub Actions",
		"write API documentation for the orchestrator endpoints",
		"review the pull request for the auth module",
	}

	const runs = 5
	for _, task := range tasks {
		first := keywordMatch(task)
		if first == "" || Get(first) == nil {
			t.Errorf("keywordMatch(%q) run 1 returned invalid persona %q", task, first)
			continue
		}
		for i := 1; i < runs; i++ {
			got := keywordMatch(task)
			if got != first {
				t.Errorf("keywordMatch(%q) run %d = %q, want %q (non-deterministic!)", task, i+1, got, first)
			}
		}
	}
}

// minimalPersonaBody returns a minimal valid persona body (no frontmatter) for use in
// hot-plug tests that need a real-looking catalog entry.
func minimalPersonaBody(title, trigger string) string {
	return fmt.Sprintf("# %s\n\n## Identity\nTest.\n\n## Goal\nTest.\n\n## When to Use\n- %s\n\n## When NOT to Use\n- nothing\n\n## Self-Check\n- ok\n", title, trigger)
}

// syntheticPersonaWithTrigger returns a minimal frontmatter persona whose unique
// trigger is the given token. Used in TestHotPlug_SyntheticPersonaRemoved.
func syntheticPersonaWithTrigger(trigger string) string {
	return fmt.Sprintf("---\nrole: implementer\ncapabilities:\n  - %s\ntriggers:\n  - %s\n---\n\n# Synthetic %s Engineer\n\n## Identity\nSynthetic.\n\n## Goal\nTest hot-plug removal.\n\n## When to Use\n- %s tasks\n\n## When NOT to Use\n- anything real\n\n## Self-Check\n- ok\n",
		trigger, trigger, trigger, trigger)
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

// TestLLMRoutingAccuracyBenchmark runs the same 23-case benchmark through the LLM
// persona selector. Skipped by default — requires a live claude CLI session.
//
// Run with:
//
//	go test ./internal/persona/... -run TestLLMRoutingAccuracyBenchmark -v
//	PERSONA_MATCH_MODEL=sonnet go test ./internal/persona/... -run TestLLMRoutingAccuracyBenchmark -v
func TestLLMRoutingAccuracyBenchmark(t *testing.T) {
	if os.Getenv("RUN_LLM_BENCHMARK") == "" {
		t.Skip("set RUN_LLM_BENCHMARK=1 to run (requires live claude CLI, slow)")
	}

	Load()

	benchmark := []struct {
		task    string
		correct string
		desc    string
	}{
		{"fix the goroutine leak in the worker pool", "senior-backend-engineer", "goroutine bug"},
		{"implement a REST endpoint for user authentication in Go", "senior-backend-engineer", "Go REST endpoint"},
		{"add SQLite migration for the new learnings table", "senior-backend-engineer", "DB migration"},
		{"build the dashboard component with React and Tailwind", "senior-frontend-engineer", "React component"},
		{"fix the CSS layout bug in the navigation bar", "senior-frontend-engineer", "CSS fix"},
		{"design the plugin architecture for the orchestrator", "architect", "system design"},
		{"choose between PostgreSQL and SQLite for the new service", "architect", "tech decision"},
		{"review the pull request for the authentication module", "staff-code-reviewer", "PR review"},
		{"assess the integration readiness of the new auth subsystem", "architect", "systems review"},
		{"validate contract stability between auth and payment services", "architect", "cross-service contract"},
		{"review the deployment rollback path for the new migration", "devops-engineer", "deployment rollback"},
		{"operational readiness check before launching the new queue", "devops-engineer", "operational readiness"},
		{"review this Go handler for nil pointer issues", "staff-code-reviewer", "code-level negative case"},
		{"write integration tests for the API endpoints", "qa-engineer", "integration tests"},
		{"set up end-to-end test automation for the CLI", "qa-engineer", "e2e tests"},
		{"audit the authentication system for security vulnerabilities", "security-auditor", "security audit"},
		{"research the latest Go frameworks for building REST APIs", "architect", "tech research"},
		{"conduct a literature review on pair programming productivity", "academic-researcher", "literature review"},
		{"set up the CI/CD pipeline with GitHub Actions", "devops-engineer", "CI/CD setup"},
		{"configure Docker containers for the service", "devops-engineer", "Docker config"},
		{"analyze the performance metrics from the last 30 days", "data-analyst", "metrics analysis"},
		{"write API documentation for the orchestrator endpoints", "technical-writer", "API docs"},
		{"write a technical article about our orchestrator architecture", "technical-writer", "technical article"},
	}

	model := llmMatchModel
	t.Logf("Model: %s", model)

	correct := 0
	total := len(benchmark)
	for _, tt := range benchmark {
		name, err := llmMatch(context.Background(), tt.task)
		if err != nil {
			t.Logf("ERROR [%s]: %v", tt.desc, err)
			continue
		}
		if name == tt.correct {
			correct++
		} else {
			t.Logf("MISS  [%s]: want %s, got %s", tt.desc, tt.correct, name)
		}
	}

	accuracy := float64(correct) * 100 / float64(total)
	t.Logf("\n=== LLM Routing Accuracy (model=%s) ===", model)
	t.Logf("Score: %d/%d (%.1f%%)", correct, total, accuracy)
	t.Logf("=========================================")
}

