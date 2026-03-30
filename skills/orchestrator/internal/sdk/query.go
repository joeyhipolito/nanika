package sdk

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"time"
)

// queryBuildArgs returns the base args for the claude CLI, without prompt or resume flags.
func queryBuildArgs(opts *AgentOptions) []string {
	args := []string{
		"--output-format", "stream-json",
		"--print",
		"--verbose",
		"--include-partial-messages",
		"--dangerously-skip-permissions",
	}
	if opts != nil {
		if opts.Model != "" {
			args = append(args, "--model", opts.Model)
		}
		if opts.MaxTurns > 0 {
			args = append(args, "--max-turns", fmt.Sprintf("%d", opts.MaxTurns))
		}
		if opts.SystemPrompt != "" {
			args = append(args, "--system-prompt", opts.SystemPrompt)
		}
		for _, dir := range opts.AddDirs {
			args = append(args, "--add-dir", dir)
		}
	}
	return args
}

// queryStartCmd creates, configures, and starts a claude CLI exec.Cmd for the given args.
// It returns the started cmd plus stdout/stderr pipes, or an error.
func queryStartCmd(ctx context.Context, cliPath string, args []string, opts *AgentOptions) (*exec.Cmd, io.ReadCloser, io.ReadCloser, error) {
	cmd := exec.CommandContext(ctx, cliPath, args...)
	if opts != nil && opts.Cwd != "" {
		cmd.Dir = opts.Cwd
	}
	cmd.Env = commandEnv(opts)

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, nil, nil, fmt.Errorf("stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, nil, nil, fmt.Errorf("stderr pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, nil, nil, fmt.Errorf("start claude: %w", err)
	}
	return cmd, stdout, stderr, nil
}

// QueryText runs a one-shot Claude session and returns the text response.
// If opts.ResumeSessionID is set, it injects --resume <id> --fork-session flags.
// If the subprocess fails to start with those flags, it falls back to a fresh start.
func QueryText(ctx context.Context, prompt string, opts *AgentOptions) (string, error) {
	cliPath := "claude"
	if opts != nil && opts.CLIPath != "" {
		cliPath = opts.CLIPath
	}

	baseArgs := queryBuildArgs(opts)

	// Build final args, optionally with resume flags.
	resumeID := ""
	if opts != nil {
		resumeID = opts.ResumeSessionID
	}

	buildFinalArgs := func(rid string) []string {
		args := make([]string, len(baseArgs))
		copy(args, baseArgs)
		if rid != "" {
			args = append(args, "--resume", rid, "--fork-session")
		}
		return append(args, "-p", prompt)
	}

	threshold := stallThreshold()
	watchCtx, watchCancel := context.WithCancel(ctx)
	defer watchCancel()

	cmd, stdout, stderr, err := queryStartCmd(watchCtx, cliPath, buildFinalArgs(resumeID), opts)
	if err != nil && resumeID != "" {
		fmt.Fprintf(os.Stderr, "[query] session resume %s failed (%v); retrying without resume\n", resumeID, err)
		cmd, stdout, stderr, err = queryStartCmd(watchCtx, cliPath, buildFinalArgs(""), opts)
	}
	if err != nil {
		return "", err
	}

	transport := &SubprocessTransport{
		stdout:   stdout,
		stderr:   stderr,
		messages: make(chan []byte, 100),
		errors:   make(chan error, 10),
		done:     make(chan struct{}),
	}
	transport.lastOutputTime.Store(time.Now().UnixNano())

	go transport.readStdout()
	go transport.readStderr()
	go transport.watchStall(watchCancel, threshold)

	var textParts []string

	var onChunk func(string)
	if opts != nil {
		onChunk = opts.OnChunk
	}

	var onEvent func(*StreamedEvent)
	if opts != nil {
		onEvent = opts.OnEvent
	}

	waitCh := make(chan error, 1)
	go func() {
		waitCh <- cmd.Wait()
	}()

	var (
		waitErr      error
		processExited bool
		messages     = transport.messages
		drainTimer   <-chan time.Time
	)

consumeLoop:
	for messages != nil || !processExited {
		select {
		case data, ok := <-messages:
			if !ok {
				messages = nil
				continue
			}
			msg := parseMessageBestEffort(data)
			if msg == nil {
				continue
			}
			for _, ev := range extractEvents(msg) {
				if ev.Kind == KindText && ev.Text != "" {
					if ev.IsDelta {
						// Partial delta: forward to caller for live streaming.
						// Do NOT accumulate — the final AssistantMessage carries
						// the authoritative text and will be added to textParts.
						if onChunk != nil {
							onChunk(ev.Text)
						}
					} else {
						// Complete AssistantMessage block: accumulate for return value.
						// onChunk already received the incremental deltas for this text.
						textParts = append(textParts, ev.Text)
					}
				}
				if ev.Kind == KindTurnEnd && len(textParts) > 0 {
					textParts = append(textParts, "\n\n")
				}
				if onEvent != nil {
					onEvent(ev)
				}
			}

		case waitErr = <-waitCh:
			processExited = true
			// Unblock any stuck readers and watchdog once the subprocess exits.
			transport.Close()
			// Allow a short grace window for any final buffered stdout to land.
			drainTimer = time.After(2 * time.Second)
			if messages == nil {
				break consumeLoop
			}

		case <-drainTimer:
			// The process is already gone; stop waiting for a clean EOF on stdout.
			messages = nil
		}
	}
	transport.Close()

	output := strings.Join(textParts, "")

	if waitErr != nil {
		if output != "" {
			return output, waitErr
		}
		if exitErr, ok := waitErr.(*exec.ExitError); ok {
			return "", fmt.Errorf("claude exited %d: %s", exitErr.ExitCode(), string(transport.stderrBuf))
		}
		return "", waitErr
	}

	return output, nil
}

func parseMessageBestEffort(data []byte) Message {
	var raw map[string]interface{}
	if err := json.Unmarshal(data, &raw); err != nil {
		return nil
	}

	msgType, _ := raw["type"].(string)

	switch msgType {
	case MessageTypeAssistant:
		var msg AssistantMessage
		json.Unmarshal(data, &msg)
		return &msg
	case MessageTypeResult:
		var msg ResultMessage
		json.Unmarshal(data, &msg)
		return &msg
	case MessageTypeStream:
		var msg StreamEvent
		json.Unmarshal(data, &msg)
		return &msg
	default:
		return &GenericMessage{Type: msgType, Raw: raw}
	}
}

// extractEvents converts a parsed Message into zero or more typed StreamedEvents.
// AssistantMessages produce KindText and KindToolUse events for each content block.
// ResultMessages produce a single KindTurnEnd event.
// StreamEvents (partial token deltas) produce KindText when a text delta is present.
func extractEvents(msg Message) []*StreamedEvent {
	switch m := msg.(type) {
	case *AssistantMessage:
		var events []*StreamedEvent
		for _, block := range m.GetContent() {
			switch block.Type {
			case BlockTypeText:
				if block.Text != "" {
					events = append(events, &StreamedEvent{Kind: KindText, Text: block.Text})
				}
			case BlockTypeToolUse:
				events = append(events, &StreamedEvent{
					Kind:      KindToolUse,
					ToolID:    block.ID,
					ToolName:  block.Name,
					ToolInput: block.Input,
				})
			}
		}
		return events

	case *ResultMessage:
		// Prefer the legacy nested Cost if present; otherwise build CostInfo from
		// the top-level total_cost_usd and usage fields that the current CLI emits.
		cost := m.Cost
		if cost == nil && (m.TotalCostUSD != 0 || m.Usage != nil) {
			cost = &CostInfo{TotalCostUSD: m.TotalCostUSD}
			if m.Usage != nil {
				cost.InputTokens = m.Usage.InputTokens + m.Usage.CacheCreationInputTokens + m.Usage.CacheReadInputTokens
				cost.OutputTokens = m.Usage.OutputTokens
				cost.CacheCreationTokens = m.Usage.CacheCreationInputTokens
				cost.CacheReadTokens = m.Usage.CacheReadInputTokens
			}
		}
		ev := &StreamedEvent{
			Kind:       KindTurnEnd,
			NumTurns:   m.NumTurns,
			DurationMs: m.DurationMs,
			SessionID:  m.SessionID,
			Cost:       cost,
		}
		if m.Subtype == "error" {
			ev.IsError = true
			ev.ErrorMsg = m.ErrorMessage
		}
		return []*StreamedEvent{ev}

	case *StreamEvent:
		// Partial token delta from --include-partial-messages.
		if m.Event != nil && m.Event.Delta != nil && m.Event.Delta.Text != "" {
			return []*StreamedEvent{{Kind: KindText, Text: m.Event.Delta.Text, IsDelta: true}}
		}
		return nil

	default:
		return nil
	}
}
