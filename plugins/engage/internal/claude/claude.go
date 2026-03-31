// Package claude provides a minimal wrapper around the claude CLI subprocess.
package claude

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"strings"
)

const (
	// ModelSonnet is the capable model used for drafting.
	ModelSonnet = "claude-sonnet-4-6"

	// DefaultCLI is the name of the claude binary.
	DefaultCLI = "claude"
)

// allowedEnvKeys is the set of env vars forwarded to the claude subprocess.
var allowedEnvKeys = []string{
	"HOME", "PATH", "LANG", "TERM", "USER", "SHELL", "TMPDIR",
	"ANTHROPIC_API_KEY", "ALLUKA_HOME",
}

// Query sends a single-turn prompt to Claude and returns the text response.
func Query(ctx context.Context, model, systemPrompt, userPrompt string) (string, error) {
	path, err := exec.LookPath(DefaultCLI)
	if err != nil {
		return "", fmt.Errorf("claude CLI not found in PATH: %w", err)
	}

	args := []string{
		"--output-format", "stream-json",
		"--print",
		"--verbose",
		"--dangerously-skip-permissions",
		"--model", model,
		"--max-turns", "1",
	}
	if systemPrompt != "" {
		args = append(args, "--system-prompt", systemPrompt)
	}
	args = append(args, "-p", userPrompt)

	cmd := exec.CommandContext(ctx, path, args...)
	cmd.Env = filteredEnv()

	out, err := cmd.Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			return "", fmt.Errorf("claude: %s", strings.TrimSpace(string(ee.Stderr)))
		}
		return "", fmt.Errorf("running claude: %w", err)
	}
	return parseTextFromNDJSON(out)
}

// Available reports whether the claude CLI is present in PATH.
func Available() bool {
	_, err := exec.LookPath(DefaultCLI)
	return err == nil
}

// filteredEnv builds an env slice containing only the allowed var names.
func filteredEnv() []string {
	osEnv := os.Environ()
	allowed := make(map[string]bool, len(allowedEnvKeys))
	for _, key := range allowedEnvKeys {
		allowed[key] = true
	}
	var env []string
	for _, pair := range osEnv {
		idx := strings.IndexByte(pair, '=')
		if idx < 0 {
			continue
		}
		if allowed[pair[:idx]] {
			env = append(env, pair)
		}
	}
	return env
}

// streamEvent is the NDJSON envelope from claude --output-format stream-json.
type streamEvent struct {
	Type    string `json:"type"`
	Subtype string `json:"subtype,omitempty"`
	Result  struct {
		Type    string `json:"type"`
		Content []struct {
			Type string `json:"type"`
			Text string `json:"text"`
		} `json:"content"`
	} `json:"result,omitempty"`
}

// parseTextFromNDJSON extracts the assistant text from a claude stream-json response.
func parseTextFromNDJSON(data []byte) (string, error) {
	scanner := bufio.NewScanner(bytes.NewReader(data))
	for scanner.Scan() {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) == 0 {
			continue
		}
		var ev streamEvent
		if err := json.Unmarshal(line, &ev); err != nil {
			continue
		}
		if ev.Type == "result" {
			var parts []string
			for _, c := range ev.Result.Content {
				if c.Type == "text" && c.Text != "" {
					parts = append(parts, c.Text)
				}
			}
			if len(parts) > 0 {
				return strings.Join(parts, ""), nil
			}
		}
	}
	// Fallback: look for assistant message events.
	scanner = bufio.NewScanner(bytes.NewReader(data))
	var lastText string
	for scanner.Scan() {
		line := bytes.TrimSpace(scanner.Bytes())
		if len(line) == 0 {
			continue
		}
		var raw map[string]json.RawMessage
		if err := json.Unmarshal(line, &raw); err != nil {
			continue
		}
		if t, ok := raw["type"]; ok {
			var typ string
			if err := json.Unmarshal(t, &typ); err == nil && typ == "assistant" {
				if msg, ok := raw["message"]; ok {
					var m struct {
						Content []struct {
							Type string `json:"type"`
							Text string `json:"text"`
						} `json:"content"`
					}
					if err := json.Unmarshal(msg, &m); err == nil {
						for _, c := range m.Content {
							if c.Type == "text" {
								lastText = c.Text
							}
						}
					}
				}
			}
		}
	}
	if lastText != "" {
		return lastText, nil
	}
	return "", fmt.Errorf("no text content found in claude response")
}
