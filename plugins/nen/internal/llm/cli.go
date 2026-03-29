package llm

import (
	"context"
	"fmt"
	"os/exec"
	"strings"
)

// Commander creates a command that can produce output. Override ExecCommand in tests.
type Commander interface {
	Output() ([]byte, error)
}

// ExecCommand is a package variable that can be mocked in tests.
var ExecCommand func(ctx context.Context, name string, arg ...string) Commander = func(ctx context.Context, name string, arg ...string) Commander {
	return exec.CommandContext(ctx, name, arg...)
}

// CallCLI executes the claude CLI with the given model and prompt, returning stdout.
// Command: claude --model MODEL --print --output-format text --max-turns 1 --dangerously-skip-permissions -p PROMPT
func CallCLI(ctx context.Context, model, prompt string) (string, error) {
	if model == "" {
		return "", fmt.Errorf("model is required")
	}
	if prompt == "" {
		return "", fmt.Errorf("prompt is required")
	}

	cmd := ExecCommand(ctx, "claude",
		"--model", model,
		"--print",
		"--output-format", "text",
		"--max-turns", "1",
		"--dangerously-skip-permissions",
		"-p", prompt,
	)

	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("claude exec: %w", err)
	}

	return strings.TrimSpace(string(out)), nil
}
