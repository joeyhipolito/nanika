package decompose

import (
	"strings"
	"testing"
)

// TestFormatTargetContext_TestCommandInjected verifies the test command appears verbatim.
func TestFormatTargetContext_TestCommandInjected(t *testing.T) {
	tc := &TargetContext{
		TestCommand: "cargo test --workspace",
	}
	out := formatTargetContext(tc)
	if !strings.Contains(out, "cargo test --workspace") {
		t.Errorf("TestCommand not injected verbatim; got:\n%s", out)
	}
}

// TestFormatTargetContext_BuildCommandInjected verifies the build command appears verbatim.
func TestFormatTargetContext_BuildCommandInjected(t *testing.T) {
	tc := &TargetContext{
		BuildCommand: "make release",
	}
	out := formatTargetContext(tc)
	if !strings.Contains(out, "make release") {
		t.Errorf("BuildCommand not injected verbatim; got:\n%s", out)
	}
}

// TestFormatTargetContext_MultipleKeyDirectories verifies multiple dirs are comma-joined.
func TestFormatTargetContext_MultipleKeyDirectories(t *testing.T) {
	tc := &TargetContext{
		KeyDirectories: []string{"src", "lib", "api"},
	}
	out := formatTargetContext(tc)
	if !strings.Contains(out, "src, lib, api") {
		t.Errorf("KeyDirectories not formatted correctly; got:\n%s", out)
	}
}

// ─── Prompt injection ─────────────────────────────────────────────────────────

// TestBuildDecomposerPrompt_ContainsTargetProfileBlock verifies that a non-nil TargetContext
// causes a ## Target Context section to appear in the generated prompt.
func TestBuildDecomposerPrompt_ContainsTargetProfileBlock(t *testing.T) {
	tc := &TargetContext{
		TargetID:     "repo:~/skills/orchestrator",
		Language:     "go",
		TestCommand:  "go test ./...",
		BuildCommand: "make build",
		Framework:    "cobra",
	}

	prompt := buildDecomposerPrompt("implement a new feature", "", "", tc)

	if !strings.Contains(prompt, "## Target Context") {
		t.Error("prompt missing '## Target Context' section when TargetContext is provided")
	}
	for _, want := range []string{"go test ./...", "make build", "cobra"} {
		if !strings.Contains(prompt, want) {
			t.Errorf("prompt missing target profile value %q", want)
		}
	}
}

// TestBuildDecomposerPrompt_NoTargetContextWhenNil verifies that a nil TargetContext
// produces no ## Target Context section in the prompt.
func TestBuildDecomposerPrompt_NoTargetContextWhenNil(t *testing.T) {
	prompt := buildDecomposerPrompt("implement a new feature", "", "", nil)

	if strings.Contains(prompt, "## Target Context") {
		t.Error("prompt should not contain '## Target Context' when TargetContext is nil")
	}
}

// TestBuildDecomposerPrompt_TaskTextPresent verifies the task text always appears.
func TestBuildDecomposerPrompt_TaskTextPresent(t *testing.T) {
	task := "add rate-limiting middleware to the API"
	prompt := buildDecomposerPrompt(task, "", "", nil)

	if !strings.Contains(prompt, task) {
		t.Errorf("prompt missing task text %q", task)
	}
}

// TestBuildDecomposerPrompt_LearningsInjected verifies learnings appear in the prompt.
func TestBuildDecomposerPrompt_LearningsInjected(t *testing.T) {
	learnings := "Always add integration tests for new API endpoints."
	prompt := buildDecomposerPrompt("add a new endpoint", learnings, "", nil)

	if !strings.Contains(prompt, learnings) {
		t.Errorf("prompt missing learnings text %q", learnings)
	}
}

// TestBuildDecomposerPrompt_RoutingLearningsSection verifies that TopPatterns produce
// a ## Routing Learnings section in the prompt.
func TestBuildDecomposerPrompt_RoutingLearningsSection(t *testing.T) {
	tc := &TargetContext{
		TargetID: "repo:~/skills/orchestrator",
		TopPatterns: []RoutingHint{
			{Persona: "senior-backend-engineer", Confidence: 0.8, TaskHint: "implement"},
		},
	}

	prompt := buildDecomposerPrompt("implement a feature", "", "", tc)

	if !strings.Contains(prompt, "## Routing Learnings") {
		t.Error("prompt missing '## Routing Learnings' when TopPatterns are provided")
	}
	if !strings.Contains(prompt, "senior-backend-engineer") {
		t.Error("prompt missing persona from TopPatterns")
	}
}

// TestBuildDecomposerPrompt_TargetProfileBeforeTask verifies that ## Target Context
// appears before ## Task to Decompose in the prompt (stronger prior positioning).
func TestBuildDecomposerPrompt_TargetProfileBeforeTask(t *testing.T) {
	tc := &TargetContext{
		Language:    "go",
		TestCommand: "go test ./...",
	}

	prompt := buildDecomposerPrompt("implement a feature", "", "", tc)

	ctxIdx := strings.Index(prompt, "## Target Context")
	taskIdx := strings.Index(prompt, "## Task to Decompose")
	if ctxIdx == -1 {
		t.Fatal("prompt missing '## Target Context'")
	}
	if taskIdx == -1 {
		t.Fatal("prompt missing '## Task to Decompose'")
	}
	if ctxIdx > taskIdx {
		t.Error("## Target Context should appear before ## Task to Decompose")
	}
}
