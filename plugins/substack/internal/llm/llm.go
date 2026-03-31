package llm

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"time"
)

// QueryText sends a one-shot prompt to the Claude CLI and returns the text response.
// Model should be a Claude model name (e.g. "claude-haiku-4-5-20251001", "claude-sonnet-4-6").
func QueryText(ctx context.Context, prompt, model string) (string, error) {
	// Ensure claude is available
	claudePath, err := exec.LookPath("claude")
	if err != nil {
		return "", fmt.Errorf("claude CLI not found in PATH: install it via npm or download from https://claude.ai/code")
	}

	ctx, cancel := context.WithTimeout(ctx, 60*time.Second)
	defer cancel()

	cmd := exec.CommandContext(ctx, claudePath,
		"--print",
		"--output-format", "text",
		"--model", model,
		"--max-turns", "1",
		"-p", prompt,
	)

	// Filtered env: only pass through essentials
	cmd.Env = filterEnv(os.Environ())

	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		if ctx.Err() == context.DeadlineExceeded {
			return "", fmt.Errorf("claude CLI timed out after 60s")
		}
		return "", fmt.Errorf("claude CLI failed: %w\nstderr: %s", err, stderr.String())
	}

	return stdout.String(), nil
}

// filterEnv keeps PATH, HOME, ANTHROPIC_API_KEY and a few essentials.
func filterEnv(env []string) []string {
	keep := map[string]bool{
		"PATH":              true,
		"HOME":              true,
		"USER":              true,
		"ANTHROPIC_API_KEY": true,
		"TERM":              true,
		"LANG":              true,
		"TMPDIR":            true,
	}
	var filtered []string
	for _, e := range env {
		for i := 0; i < len(e); i++ {
			if e[i] == '=' {
				if keep[e[:i]] {
					filtered = append(filtered, e)
				}
				break
			}
		}
	}
	return filtered
}
