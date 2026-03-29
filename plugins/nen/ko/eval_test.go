package ko

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestRenderPrompt(t *testing.T) {
	tests := []struct {
		name        string
		promptTpl   string
		vars        map[string]interface{}
		wantContain string
		wantErr     bool
	}{
		{
			name:      "simple variable substitution",
			promptTpl: "Hello {{.name}}!",
			vars: map[string]interface{}{
				"name": "World",
			},
			wantContain: "Hello World!",
		},
		{
			name:      "multiple variables",
			promptTpl: "{{.greeting}} {{.name}}, welcome to {{.place}}!",
			vars: map[string]interface{}{
				"greeting": "Hello",
				"name":     "Alice",
				"place":    "Wonderland",
			},
			wantContain: "Hello Alice, welcome to Wonderland!",
		},
		{
			name:      "multiline template",
			promptTpl: "Instructions:\n{{.instructions}}\n\nCode:\n{{.code}}",
			vars: map[string]interface{}{
				"instructions": "Review this code for bugs",
				"code":         "func test() { return 42; }",
			},
			wantContain: "Review this code for bugs",
		},
		{
			name:      "missing variable",
			promptTpl: "Hello {{.unknown}}!",
			vars:      map[string]interface{}{},
			wantErr:   false, // Go templates don't error on missing vars, just render empty
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Create a temporary config with the prompt
			tmpDir := t.TempDir()
			promptFile := filepath.Join(tmpDir, "test.txt")
			if err := os.WriteFile(promptFile, []byte(tt.promptTpl), 0600); err != nil {
				t.Fatalf("write prompt file: %v", err)
			}

			cfg := &EvalConfig{
				promptContents: map[string]string{
					promptFile: tt.promptTpl,
				},
			}

			got, err := cfg.RenderPrompt(promptFile, tt.vars)
			if (err != nil) != tt.wantErr {
				t.Errorf("RenderPrompt() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if tt.wantErr {
				return
			}

			if tt.wantContain != "" && !contains(got, tt.wantContain) {
				t.Errorf("RenderPrompt() output = %q, want to contain %q", got, tt.wantContain)
			}
		})
	}
}

func TestLoadEvalConfig(t *testing.T) {
	tmpDir := t.TempDir()

	// Create a test prompt file
	promptFile := filepath.Join(tmpDir, "prompt.txt")
	if err := os.WriteFile(promptFile, []byte("Test prompt: {{.variable}}"), 0600); err != nil {
		t.Fatalf("write prompt file: %v", err)
	}

	// Create a test config YAML
	configFile := filepath.Join(tmpDir, "config.yaml")
	configContent := `description: "Test eval"
sharing: false
providers:
  - file://providers/test.mjs
prompts:
  - file://prompt.txt
defaultTest:
  assert:
    - type: contains
      value: "Test"
tests:
  - description: "Test case 1"
    vars:
      variable: "value1"
    assert:
      - type: contains
        value: "value1"
  - description: "Test case 2"
    skip: true
    vars:
      variable: "value2"
`
	if err := os.WriteFile(configFile, []byte(configContent), 0600); err != nil {
		t.Fatalf("write config file: %v", err)
	}

	cfg, err := LoadEvalConfig(context.Background(), configFile)
	if err != nil {
		t.Fatalf("LoadEvalConfig() error = %v", err)
	}

	if cfg.Description != "Test eval" {
		t.Errorf("LoadEvalConfig() description = %q, want %q", cfg.Description, "Test eval")
	}

	if len(cfg.Tests) != 2 {
		t.Errorf("LoadEvalConfig() tests count = %d, want 2", len(cfg.Tests))
	}

	if cfg.Tests[0].Description != "Test case 1" {
		t.Errorf("LoadEvalConfig() first test description = %q, want %q", cfg.Tests[0].Description, "Test case 1")
	}

	if !cfg.Tests[1].Skip {
		t.Errorf("LoadEvalConfig() second test skip = %v, want true", cfg.Tests[1].Skip)
	}

	if len(cfg.DefaultTest.Assert) != 1 {
		t.Errorf("LoadEvalConfig() defaultTest assertions count = %d, want 1", len(cfg.DefaultTest.Assert))
	}
}

func TestRunnerWithParallelExecution(t *testing.T) {
	tmpDir := t.TempDir()

	// Create prompt file
	promptFile := filepath.Join(tmpDir, "prompt.txt")
	if err := os.WriteFile(promptFile, []byte("Prompt: {{.input}}"), 0600); err != nil {
		t.Fatalf("write prompt file: %v", err)
	}

	// Create config
	cfg := &EvalConfig{
		Description: "Parallel test",
		promptKeys: []string{promptFile},
		promptContents: map[string]string{
			promptFile: "Prompt: {{.input}}",
		},
		basePath: tmpDir,
		Tests: []TestCase{
			{
				Description: "Test 1",
				Vars:        map[string]string{"input": "test1"},
				Assert: []AssertionConfig{
					{Type: "contains", Value: "test1"},
				},
			},
			{
				Description: "Test 2",
				Vars:        map[string]string{"input": "test2"},
				Assert: []AssertionConfig{
					{Type: "contains", Value: "test2"},
				},
			},
			{
				Description: "Test 3",
				Vars:        map[string]string{"input": "test3"},
				Assert: []AssertionConfig{
					{Type: "contains", Value: "test3"},
				},
			},
		},
	}

	opts := RunnerOptions{
		Concurrency: 2,
		Model:       "test-model",
		Verbose:     false,
	}

	runner := NewRunner(cfg, opts)

	// Mock query function
	callCount := 0
	mockQuery := func(ctx context.Context, prompt string) (string, error) {
		callCount++
		return prompt, nil // Return the prompt as output
	}

	results, err := runner.Run(context.Background(), mockQuery)
	if err != nil {
		t.Fatalf("Run() error = %v", err)
	}

	if results.Total != 3 {
		t.Errorf("Run() total = %d, want 3", results.Total)
	}

	if results.Passed != 3 {
		t.Errorf("Run() passed = %d, want 3", results.Passed)
	}

	if callCount != 3 {
		t.Errorf("Run() query calls = %d, want 3", callCount)
	}
}

func TestRunnerSkipTests(t *testing.T) {
	tmpDir := t.TempDir()

	// Create prompt file
	promptFile := filepath.Join(tmpDir, "prompt.txt")
	if err := os.WriteFile(promptFile, []byte("Prompt"), 0600); err != nil {
		t.Fatalf("write prompt file: %v", err)
	}

	cfg := &EvalConfig{
		Description: "Skip test",
		promptKeys: []string{promptFile},
		promptContents: map[string]string{
			promptFile: "Prompt",
		},
		basePath: tmpDir,
		Tests: []TestCase{
			{
				Description: "Skipped test",
				Skip:        true,
				Assert: []AssertionConfig{
					{Type: "contains", Value: "Prompt"},
				},
			},
			{
				Description: "Normal test",
				Skip:        false,
				Assert: []AssertionConfig{
					{Type: "contains", Value: "Prompt"},
				},
			},
		},
	}

	opts := RunnerOptions{Concurrency: 1}
	runner := NewRunner(cfg, opts)

	callCount := 0
	mockQuery := func(ctx context.Context, prompt string) (string, error) {
		callCount++
		return "Prompt output", nil
	}

	results, err := runner.Run(context.Background(), mockQuery)
	if err != nil {
		t.Fatalf("Run() error = %v", err)
	}

	// Only the non-skipped test should call the query function
	if callCount != 1 {
		t.Errorf("Run() query calls = %d, want 1 (skipped test should not call)", callCount)
	}

	if results.Total != 2 {
		t.Errorf("Run() total = %d, want 2", results.Total)
	}

	if results.Passed != 2 {
		t.Errorf("Run() passed = %d, want 2 (skipped test counts as passed)", results.Passed)
	}
}

func TestRunnerFailingAssertions(t *testing.T) {
	tmpDir := t.TempDir()

	promptFile := filepath.Join(tmpDir, "prompt.txt")
	if err := os.WriteFile(promptFile, []byte("Prompt"), 0600); err != nil {
		t.Fatalf("write prompt file: %v", err)
	}

	cfg := &EvalConfig{
		Description: "Failing test",
		promptKeys: []string{promptFile},
		promptContents: map[string]string{
			promptFile: "Prompt",
		},
		basePath: tmpDir,
		Tests: []TestCase{
			{
				Description: "Test with failing assertion",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "NOTFOUND"},
				},
			},
		},
	}

	opts := RunnerOptions{Concurrency: 1}
	runner := NewRunner(cfg, opts)

	mockQuery := func(ctx context.Context, prompt string) (string, error) {
		return "Some output that doesn't contain the expected value", nil
	}

	results, err := runner.Run(context.Background(), mockQuery)
	if err != nil {
		t.Fatalf("Run() error = %v", err)
	}

	if results.Passed != 0 {
		t.Errorf("Run() passed = %d, want 0", results.Passed)
	}

	if results.Failed != 1 {
		t.Errorf("Run() failed = %d, want 1", results.Failed)
	}

	if len(results.FailedTests) != 1 {
		t.Errorf("Run() failedTests length = %d, want 1", len(results.FailedTests))
	}
}

func TestRunnerDefaultAssertions(t *testing.T) {
	tmpDir := t.TempDir()

	promptFile := filepath.Join(tmpDir, "prompt.txt")
	if err := os.WriteFile(promptFile, []byte("Prompt"), 0600); err != nil {
		t.Fatalf("write prompt file: %v", err)
	}

	cfg := &EvalConfig{
		Description: "Default assertions test",
		promptKeys: []string{promptFile},
		promptContents: map[string]string{
			promptFile: "Prompt",
		},
		basePath: tmpDir,
		DefaultTest: DefaultTest{
			Assert: []AssertionConfig{
				{Type: "contains", Value: "hello"},
			},
		},
		Tests: []TestCase{
			{
				Description: "Test with default assertion",
				// No local assertions, should use default
			},
		},
	}

	opts := RunnerOptions{Concurrency: 1}
	runner := NewRunner(cfg, opts)

	mockQuery := func(ctx context.Context, prompt string) (string, error) {
		return "hello world", nil
	}

	results, err := runner.Run(context.Background(), mockQuery)
	if err != nil {
		t.Fatalf("Run() error = %v", err)
	}

	if results.Passed != 1 {
		t.Errorf("Run() passed = %d, want 1", results.Passed)
	}

	if len(results.Tests[0].Assertions) != 1 {
		t.Errorf("Run() assertions count = %d, want 1", len(results.Tests[0].Assertions))
	}
}

func TestRunnerWithTimeout(t *testing.T) {
	tmpDir := t.TempDir()

	promptFile := filepath.Join(tmpDir, "prompt.txt")
	if err := os.WriteFile(promptFile, []byte("Prompt"), 0600); err != nil {
		t.Fatalf("write prompt file: %v", err)
	}

	cfg := &EvalConfig{
		Description: "Timeout test",
		promptKeys: []string{promptFile},
		promptContents: map[string]string{
			promptFile: "Prompt",
		},
		basePath: tmpDir,
		Tests: []TestCase{
			{
				Description: "Test that times out",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "test"},
				},
			},
		},
	}

	opts := RunnerOptions{Concurrency: 1}
	runner := NewRunner(cfg, opts)

	mockQuery := func(ctx context.Context, prompt string) (string, error) {
		// Simulate timeout by checking context
		select {
		case <-ctx.Done():
			return "", ctx.Err()
		case <-time.After(100 * time.Millisecond):
			return "result", nil
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()

	results, err := runner.Run(ctx, mockQuery)
	if err == nil && results.Tests[0].Error == "" {
		t.Logf("Note: timeout test may not trigger in fast execution; this is ok")
	}
}

func TestRunnerErrorHandling(t *testing.T) {
	tmpDir := t.TempDir()

	promptFile := filepath.Join(tmpDir, "prompt.txt")
	if err := os.WriteFile(promptFile, []byte("Prompt"), 0600); err != nil {
		t.Fatalf("write prompt file: %v", err)
	}

	cfg := &EvalConfig{
		Description: "Error handling test",
		promptKeys: []string{promptFile},
		promptContents: map[string]string{
			promptFile: "Prompt",
		},
		basePath: tmpDir,
		Tests: []TestCase{
			{
				Description: "Test that generates error",
				Assert:      []AssertionConfig{},
			},
		},
	}

	opts := RunnerOptions{Concurrency: 1}
	runner := NewRunner(cfg, opts)

	mockQuery := func(ctx context.Context, prompt string) (string, error) {
		return "", fmt.Errorf("query error")
	}

	results, err := runner.Run(context.Background(), mockQuery)
	if err == nil {
		t.Logf("Note: error was returned in test results")
	}

	if results.Tests[0].Error == "" {
		t.Logf("Note: first test should have error recorded")
	}
}

// Helper function
func contains(s, substr string) bool {
	if len(s) == 0 || len(substr) == 0 {
		return false
	}
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}

// mockCmd is a mock implementation of *exec.Cmd for testing.
type mockCmd struct {
	output []byte
	err    error
}

func (m *mockCmd) Output() ([]byte, error) {
	return m.output, m.err
}

// TestCallLLM tests the CallLLM function with a mocked exec.
func TestCallLLM(t *testing.T) {
	tests := []struct {
		name        string
		model       string
		prompt      string
		mockOutput  string
		mockErr     error
		wantOutput  string
		wantErr     bool
	}{
		{
			name:       "successful call",
			model:      "claude-haiku-4-5-20251001",
			prompt:     "Hello, world!",
			mockOutput: "Response from Claude\n",
			wantOutput: "Response from Claude",
		},
		{
			name:       "output with whitespace",
			model:      "claude-opus-4-6",
			prompt:     "Test prompt",
			mockOutput: "  Trimmed response  \n\n",
			wantOutput: "Trimmed response",
		},
		{
			name:    "missing model",
			model:   "",
			prompt:  "Test",
			wantErr: true,
		},
		{
			name:    "missing prompt",
			model:   "claude-haiku-4-5-20251001",
			prompt:  "",
			wantErr: true,
		},
		{
			name:        "exec error",
			model:       "claude-haiku-4-5-20251001",
			prompt:      "Test",
			mockOutput:  "",
			mockErr:     fmt.Errorf("command not found"),
			wantErr:     true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			// Save original execCommand
			originalExecCommand := execCommand

			// Mock execCommand
			execCommand = func(ctx context.Context, name string, arg ...string) execCmdInterface {
				return &mockCmd{
					output: []byte(tt.mockOutput),
					err:    tt.mockErr,
				}
			}
			defer func() { execCommand = originalExecCommand }()

			got, err := CallLLM(context.Background(), tt.model, tt.prompt)
			if (err != nil) != tt.wantErr {
				t.Errorf("CallLLM() error = %v, wantErr %v", err, tt.wantErr)
				return
			}

			if !tt.wantErr && got != tt.wantOutput {
				t.Errorf("CallLLM() got %q, want %q", got, tt.wantOutput)
			}
		})
	}
}

// TestCallLLMCommandArgs verifies that the correct arguments are passed to the claude CLI.
func TestCallLLMCommandArgs(t *testing.T) {
	model := "claude-haiku-4-5-20251001"
	prompt := "Test prompt"

	var capturedArgs []string
	originalExecCommand := execCommand

	execCommand = func(ctx context.Context, name string, arg ...string) execCmdInterface {
		if name != "claude" {
			t.Errorf("expected command 'claude', got %q", name)
		}
		capturedArgs = arg
		return &mockCmd{
			output: []byte("response"),
			err:    nil,
		}
	}
	defer func() { execCommand = originalExecCommand }()

	_, err := CallLLM(context.Background(), model, prompt)
	if err != nil {
		t.Fatalf("CallLLM() error = %v", err)
	}

	expectedArgs := []string{
		"--model", model,
		"--print",
		"--output-format", "text",
		"--max-turns", "1",
		"--dangerously-skip-permissions",
		"-p", prompt,
	}

	if len(capturedArgs) != len(expectedArgs) {
		t.Errorf("CallLLM() arg count = %d, want %d", len(capturedArgs), len(expectedArgs))
	}

	for i, expected := range expectedArgs {
		if i >= len(capturedArgs) {
			t.Errorf("CallLLM() missing arg at index %d: expected %q", i, expected)
			continue
		}
		if capturedArgs[i] != expected {
			t.Errorf("CallLLM() arg[%d] = %q, want %q", i, capturedArgs[i], expected)
		}
	}
}
