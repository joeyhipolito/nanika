package router

import (
	"testing"
)

func TestClassifyTier_ThinkSignals(t *testing.T) {
	tests := []struct {
		task    string
		persona string
		want    ModelTier
	}{
		// Think tier from task keywords
		{"design the database schema", "backend-engineer", TierThink},
		{"plan the migration strategy", "backend-engineer", TierThink},
		{"audit security of the API", "backend-engineer", TierThink},
		{"review architecture for scalability", "backend-engineer", TierThink},
		{"threat model the auth system", "backend-engineer", TierThink},
		{"vulnerability scan results analysis", "backend-engineer", TierThink},
		{"system design for notifications", "backend-engineer", TierThink},
		{"data model for user profiles", "backend-engineer", TierThink},
		{"api design for the REST endpoints", "backend-engineer", TierThink},

		// Think tier from persona escalation
		{"implement the config loader", "architect", TierThink},
		{"look at the database code", "security-auditor", TierThink},
		{"review the authentication changes", "staff-code-reviewer", TierThink},

		// Reviewer + security → Think
		{"review the auth code for security issues", "reviewer", TierThink},
		{"check for vulnerabilities in the API", "reviewer", TierThink},
	}

	for _, tt := range tests {
		got := ClassifyTier(tt.task, tt.persona)
		if got != tt.want {
			t.Errorf("ClassifyTier(%q, %q) = %q, want %q", tt.task, tt.persona, got, tt.want)
		}
	}
}

func TestClassifyTier_WorkDefault(t *testing.T) {
	tests := []struct {
		task    string
		persona string
	}{
		{"implement the user registration endpoint", "backend-engineer"},
		{"write unit tests for the parser", "qa-engineer"},
		{"research Go error handling patterns", "academic-researcher"},
		{"write a blog post about the rewrite", "technical-writer"},
		{"build the login form with React", "senior-frontend-engineer"},
		{"create API documentation", "technical-writer"},
		{"analyze learning database stats", "data-analyst"},
	}

	for _, tt := range tests {
		got := ClassifyTier(tt.task, tt.persona)
		if got != TierWork {
			t.Errorf("ClassifyTier(%q, %q) = %q, want %q", tt.task, tt.persona, got, TierWork)
		}
	}
}

func TestClassifyTier_QuickSignals(t *testing.T) {
	tests := []struct {
		task    string
		persona string
	}{
		{"format the Go files", "backend-engineer"},
		{"rename the variable to camelCase", "backend-engineer"},
		{"fix typo in README", "technical-writer"},
		{"update comment in config.go", "backend-engineer"},
		{"add docstring to the exported function", "backend-engineer"},
		{"simple fix for the broken import", "backend-engineer"},
		{"minor change to the error message", "backend-engineer"},
	}

	for _, tt := range tests {
		got := ClassifyTier(tt.task, tt.persona)
		if got != TierQuick {
			t.Errorf("ClassifyTier(%q, %q) = %q, want %q", tt.task, tt.persona, got, TierQuick)
		}
	}
}

func TestClassifyTier_PersonaOverridesQuick(t *testing.T) {
	// Even if the task looks simple, architect persona should escalate to Think
	got := ClassifyTier("rename the config field", "architect")
	if got != TierThink {
		t.Errorf("architect persona should escalate to Think even for simple tasks, got %q", got)
	}
}

func TestResolve(t *testing.T) {
	tests := []struct {
		tier ModelTier
		want string
	}{
		{TierThink, "opus"},
		{TierWork, "sonnet"},
		{TierQuick, "haiku"},
		{"unknown", "sonnet"}, // fallback
	}

	for _, tt := range tests {
		got := Resolve(tt.tier)
		if got != tt.want {
			t.Errorf("Resolve(%q) = %q, want %q", tt.tier, got, tt.want)
		}
	}
}

func TestResolveEffort(t *testing.T) {
	tests := []struct {
		name    string
		tier    ModelTier
		persona string
		want    string
	}{
		{"staff code reviewer always high", TierWork, "staff-code-reviewer", "high"},
		{"architect think high", TierThink, "architect", "high"},
		{"work defaults medium", TierWork, "senior-backend-engineer", "medium"},
		{"quick defaults low", TierQuick, "technical-writer", "low"},
	}

	for _, tt := range tests {
		if got := ResolveEffort(tt.tier, tt.persona); got != tt.want {
			t.Errorf("%s: ResolveEffort(%q, %q) = %q, want %q", tt.name, tt.tier, tt.persona, got, tt.want)
		}
	}
}

func TestClassifyComplexity_Simple(t *testing.T) {
	simple := []string{
		"fix the bug",
		"add a test",
		"update the README",
		"refactor the parser",
		"deploy the app",
	}

	for _, task := range simple {
		if ClassifyComplexity(task) {
			t.Errorf("ClassifyComplexity(%q) = true, want false (short simple task)", task)
		}
	}
}

func TestClassifyComplexity_Complex(t *testing.T) {
	complex := []struct {
		task string
		why  string
	}{
		{"research Go error handling patterns and then implement a unified package with tests", "long + 'and then'"},
		{"first research the API, then build the client, finally write docs", "'first' + 'then' + 'finally'"},
		{"design and implement the authentication system", "'design and implement'"},
		{"research and write a comprehensive guide to Go testing", "'research and'"},
		{"build multiple CLI commands for the orchestrator", "'multiple'"},
		{"implement several new features for the dashboard", "'several'"},
		{"plan and execute the database migration strategy", "'plan and execute'"},
		// Long task (>20 words)
		{"create a comprehensive error handling system with custom error types error wrapping structured logging and integration with the existing monitoring infrastructure", "word count > 20"},
	}

	for _, tt := range complex {
		if !ClassifyComplexity(tt.task) {
			t.Errorf("ClassifyComplexity(%q) = false, want true (%s)", tt.task, tt.why)
		}
	}
}
