package dream

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"
)

// rawLine is the minimal envelope decoded from every JSONL line.
// Fields are only populated for the message types we care about.
type rawLine struct {
	Type    string          `json:"type"`
	Subtype string          `json:"subtype,omitempty"`
	// cwd appears on system/init lines at the top level.
	Cwd     string          `json:"cwd,omitempty"`
	// message holds the nested user/assistant message object.
	Message json.RawMessage `json:"message,omitempty"`
}

// rawMsg is the nested message object inside user/assistant lines.
type rawMsg struct {
	Role    string          `json:"role"`
	// content may be a JSON string or a JSON array of content blocks.
	Content json.RawMessage `json:"content,omitempty"`
}

// rawBlock is one content block inside a message's content array.
type rawBlock struct {
	Type string `json:"type"`
	Text string `json:"text,omitempty"`
}

// ParseTranscript reads a Claude Code JSONL transcript file and returns the
// conversation messages and the working directory recorded in the system/init
// line. Corrupt or unrecognised lines are silently skipped (defensive parsing).
// Partial reads (scanner overflow) are accepted — whatever was parsed is returned.
func ParseTranscript(path string) (msgs []ConvMessage, cwd string, err error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, "", fmt.Errorf("opening transcript %s: %w", path, err)
	}
	defer f.Close()

	// 1 MB line buffer — JSONL transcripts can contain large tool outputs.
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1<<20), 1<<20)

	lineNum := 0
	for scanner.Scan() {
		lineNum++
		raw := scanner.Bytes()
		if len(raw) == 0 {
			continue
		}

		var line rawLine
		if err := json.Unmarshal(raw, &line); err != nil {
			continue // skip corrupt lines without failing the whole file
		}

		switch line.Type {
		case "system":
			if line.Subtype == "init" && line.Cwd != "" {
				cwd = line.Cwd
			}

		case "user", "assistant":
			if len(line.Message) == 0 {
				continue
			}
			var msg rawMsg
			if err := json.Unmarshal(line.Message, &msg); err != nil {
				continue
			}
			text := parseContent(msg.Content)
			if text == "" {
				continue
			}
			msgs = append(msgs, ConvMessage{
				Role:   line.Type,
				Text:   text,
				SeqNum: lineNum,
			})
		}
	}
	// scanner.Err() is non-nil only for I/O errors, not for token-too-large.
	// Either way, return what we have — partial transcripts are still useful.
	return msgs, cwd, nil
}

// parseContent extracts text from a content field that may be either a JSON
// string or a JSON array of typed content blocks. Unknown block types and
// tool_result blocks are ignored so that tool noise doesn't flood the LLM.
func parseContent(raw json.RawMessage) string {
	if len(raw) == 0 {
		return ""
	}

	// Try array of content blocks first (most common format).
	var blocks []rawBlock
	if err := json.Unmarshal(raw, &blocks); err == nil {
		var parts []string
		for _, b := range blocks {
			if b.Type == "text" && strings.TrimSpace(b.Text) != "" {
				parts = append(parts, strings.TrimSpace(b.Text))
			}
		}
		return strings.Join(parts, "\n")
	}

	// Fall back to plain JSON string.
	var s string
	if err := json.Unmarshal(raw, &s); err == nil {
		return strings.TrimSpace(s)
	}

	return ""
}

// isWorkerSession returns true when cwd indicates the transcript came from an
// orchestrator worker session (run in a worktree) rather than a human session.
// Worker transcripts are already captured by CaptureWithFocus during live
// execution — re-mining them would create duplicate learnings.
func isWorkerSession(cwd string) bool {
	if cwd == "" {
		return false
	}
	return strings.Contains(cwd, "/.alluka/worktrees/") ||
		strings.Contains(cwd, "/.via/worktrees/") ||
		strings.Contains(cwd, "/.alluka/workers/")
}
