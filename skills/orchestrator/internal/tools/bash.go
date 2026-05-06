package tools

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os/exec"
	"strings"
	"time"
)

const defaultBashTimeout = 120 * time.Second

// BashTool executes shell commands with a configurable timeout.
// Stdout and stderr are combined in the output.
// Implements Streamer to write output as it arrives.
type BashTool struct {
	defaultTimeout time.Duration
}

// NewBashTool returns a BashTool with a 2-minute default timeout.
func NewBashTool() Tool {
	return &BashTool{defaultTimeout: defaultBashTimeout}
}

func (b *BashTool) Name() string { return "bash" }
func (b *BashTool) Risk() RiskTier { return RiskHigh }

func (b *BashTool) Description() string {
	return "Execute a shell command. Stdout and stderr are combined. " +
		"Use timeout_seconds to override the default 120s limit."
}

func (b *BashTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"command": String("Shell command to execute"),
		"timeout_seconds": {
			Type:        "integer",
			Description: "Maximum execution time in seconds (default 120, max 600)",
			Minimum:     ptr(1),
			Default:     120,
		},
		"working_dir": String("Working directory for the command (optional)"),
	}, []string{"command"})
}

func (b *BashTool) Execute(ctx context.Context, args map[string]any) (ToolResult, error) {
	cmd, timeout, workDir, err := b.parseArgs(args)
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}

	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	var buf bytes.Buffer
	if err := b.run(ctx, cmd, workDir, &buf); err != nil {
		content := buf.String()
		if content == "" {
			content = err.Error()
		} else {
			content = strings.TrimRight(content, "\n") + "\n" + err.Error()
		}
		return ToolResult{IsError: true, Content: content}, nil
	}
	return ToolResult{Content: buf.String()}, nil
}

// Stream writes command output to out as it arrives.
func (b *BashTool) Stream(ctx context.Context, args map[string]any, out io.Writer) error {
	cmd, timeout, workDir, err := b.parseArgs(args)
	if err != nil {
		return fmt.Errorf("bash stream: %w", err)
	}

	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()

	return b.run(ctx, cmd, workDir, out)
}

func (b *BashTool) parseArgs(args map[string]any) (cmd string, timeout time.Duration, workDir string, err error) {
	raw, ok := args["command"]
	if !ok {
		return "", 0, "", fmt.Errorf("bash: missing required argument: command")
	}
	cmd, ok = raw.(string)
	if !ok {
		return "", 0, "", fmt.Errorf("bash: command must be a string")
	}
	if strings.TrimSpace(cmd) == "" {
		return "", 0, "", fmt.Errorf("bash: command must not be empty")
	}

	timeout = b.defaultTimeout
	if raw, ok := args["timeout_seconds"]; ok {
		secs, _ := toFloat(raw)
		if secs > 0 {
			if secs > 600 {
				secs = 600
			}
			timeout = time.Duration(secs) * time.Second
		}
	}

	if raw, ok := args["working_dir"]; ok {
		workDir, _ = raw.(string)
	}

	return cmd, timeout, workDir, nil
}

func (b *BashTool) run(ctx context.Context, command, workDir string, out io.Writer) error {
	c := exec.CommandContext(ctx, "bash", "-c", command)
	if workDir != "" {
		c.Dir = workDir
	}
	c.Stdout = out
	c.Stderr = out
	return c.Run()
}

// toFloat converts numeric JSON values (float64 or int) to float64.
func toFloat(v any) (float64, bool) {
	switch n := v.(type) {
	case float64:
		return n, true
	case int:
		return float64(n), true
	case int64:
		return float64(n), true
	}
	return 0, false
}
