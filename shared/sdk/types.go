// Package sdk provides a Go SDK for communicating with Claude Code CLI.
package sdk

import (
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
)

// ErrConflictingResumeFlags is returned when both ContinueConversation and ResumeSessionID
// are set simultaneously. They are mutually exclusive: ContinueConversation resumes the most
// recent conversation while ResumeSessionID targets a specific session by ID.
var ErrConflictingResumeFlags = errors.New("sdk: ContinueConversation and ResumeSessionID are mutually exclusive")

// ExitError is returned by QueryText when the claude subprocess exits with a non-zero code.
// It carries the parsed exit code and the last 20 lines of stderr so callers can inspect
// them without re-parsing the error string.
type ExitError struct {
	Code   int
	Stderr string
}

func (e *ExitError) Error() string {
	if e.Stderr == "" {
		return fmt.Sprintf("claude exited %d", e.Code)
	}
	return fmt.Sprintf("claude exited %d: %s", e.Code, e.Stderr)
}

// Message types
const (
	MessageTypeUser      = "user"
	MessageTypeAssistant = "assistant"
	MessageTypeSystem    = "system"
	MessageTypeResult    = "result"
	MessageTypeStream    = "stream_event"
)

// Content block types — values must match the Claude API wire format.
// "text" and "tool_use" intentionally overlap with StreamedEventKind values
// below because both reference the same API concepts at different abstraction
// layers. These are untyped string constants compared against JSON "type" fields.
//
// V-23 scope: read-model unification with two on-disk authoring trees retained.
// See shared/sdk/DESIGN.md for the full rationale.
const (
	BlockTypeText       = "text"
	BlockTypeToolUse    = "tool_use"
	BlockTypeToolResult = "tool_result"
)

// Message is the interface for all message types
type Message interface {
	GetType() string
}

// AssistantMessage represents a Claude response
type AssistantMessage struct {
	Type    string                 `json:"type"`
	Message *AssistantMessageInner `json:"message,omitempty"`
	Content []ContentBlock         `json:"content,omitempty"`
}

// AssistantMessageInner is the nested message structure from Claude CLI
type AssistantMessageInner struct {
	Content    []ContentBlock `json:"content"`
	StopReason string         `json:"stop_reason,omitempty"`
}

func (m *AssistantMessage) GetType() string { return MessageTypeAssistant }

// GetContent returns content from either nested or direct format
func (m *AssistantMessage) GetContent() []ContentBlock {
	if m.Message != nil && len(m.Message.Content) > 0 {
		return m.Message.Content
	}
	return m.Content
}

// UserMessage represents a user-side turn from the Claude CLI stream.
// In practice these arrive only after a tool call: the CLI emits a
// `user`-typed message whose `message.content` contains one or more
// `tool_result` blocks correlating back to the prior assistant `tool_use`.
type UserMessage struct {
	Type    string            `json:"type"`
	Message *UserMessageInner `json:"message,omitempty"`
	Content []ToolResultBlock `json:"content,omitempty"`
}

type UserMessageInner struct {
	Role    string            `json:"role,omitempty"`
	Content []ToolResultBlock `json:"content"`
}

func (m *UserMessage) GetType() string { return MessageTypeUser }

func (m *UserMessage) GetContent() []ToolResultBlock {
	if m.Message != nil && len(m.Message.Content) > 0 {
		return m.Message.Content
	}
	return m.Content
}

// ToolResultBlock represents a `tool_result` content block emitted on a
// user-typed message after a tool invocation.
//
// Claude's wire format allows `content` to be either a plain string or
// an array of structured blocks (e.g. `text` blocks). UnmarshalJSON
// normalizes both into the `Content` string field; array form is
// flattened by joining the inner `text` fields.
type ToolResultBlock struct {
	Type      string `json:"type"`
	ToolUseID string `json:"tool_use_id,omitempty"`
	Content   string `json:"content,omitempty"`
	IsError   bool   `json:"is_error,omitempty"`
}

func (b *ToolResultBlock) UnmarshalJSON(data []byte) error {
	var raw struct {
		Type      string          `json:"type"`
		ToolUseID string          `json:"tool_use_id"`
		Content   json.RawMessage `json:"content"`
		IsError   bool            `json:"is_error"`
	}
	if err := json.Unmarshal(data, &raw); err != nil {
		return err
	}
	b.Type = raw.Type
	b.ToolUseID = raw.ToolUseID
	b.IsError = raw.IsError
	if len(raw.Content) == 0 {
		return nil
	}
	var s string
	if err := json.Unmarshal(raw.Content, &s); err == nil {
		b.Content = s
		return nil
	}
	var inner []struct {
		Type string `json:"type"`
		Text string `json:"text"`
	}
	if err := json.Unmarshal(raw.Content, &inner); err == nil {
		var parts []string
		for _, p := range inner {
			if p.Text != "" {
				parts = append(parts, p.Text)
			}
		}
		b.Content = strings.Join(parts, "\n")
		return nil
	}
	// Last resort: store raw JSON so callers see *something* rather than
	// silently dropping the result.
	b.Content = string(raw.Content)
	return nil
}

// SystemMessage represents system-level communications
type SystemMessage struct {
	Type    string                 `json:"type"`
	Subtype string                 `json:"subtype,omitempty"`
	Data    map[string]interface{} `json:"data,omitempty"`
}

func (m *SystemMessage) GetType() string { return MessageTypeSystem }

// UsageInfo holds token counts from the Claude CLI result message.
// The CLI emits these inside a "usage" object on the result line.
type UsageInfo struct {
	InputTokens              int `json:"input_tokens"`
	OutputTokens             int `json:"output_tokens"`
	CacheCreationInputTokens int `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int `json:"cache_read_input_tokens"`
}

// ResultMessage represents execution completion.
// Note: the Claude CLI emits cost as a top-level "total_cost_usd" field and
// token counts inside a nested "usage" object — not as a "cost" sub-object.
// The legacy Cost field is retained for any hypothetical future wire-format
// change but is not populated by the current CLI.
type ResultMessage struct {
	Type         string          `json:"type"`
	Subtype      string          `json:"subtype,omitempty"`
	SessionID    string          `json:"session_id,omitempty"`
	DurationMs   int64           `json:"duration_ms,omitempty"`
	NumTurns     int             `json:"num_turns,omitempty"`
	Cost         *CostInfo       `json:"cost,omitempty"`
	TotalCostUSD float64         `json:"total_cost_usd,omitempty"`
	Usage        *UsageInfo      `json:"usage,omitempty"`
	Result       json.RawMessage `json:"result,omitempty"`
	ErrorCode    string          `json:"error_code,omitempty"`
	ErrorMessage string          `json:"error_message,omitempty"`
}

func (m *ResultMessage) GetType() string { return MessageTypeResult }

// StreamEvent represents a streaming partial message from --include-partial-messages.
// The real wire format nests the text under event.delta.text, not a flat content field.
type StreamEvent struct {
	Type  string      `json:"type"`
	Event *DeltaEvent `json:"event,omitempty"`
}

func (m *StreamEvent) GetType() string { return MessageTypeStream }

// DeltaEvent is the inner event payload of a stream_event message.
type DeltaEvent struct {
	Type  string     `json:"type"`
	Index int        `json:"index,omitempty"`
	Delta *DeltaBody `json:"delta,omitempty"`
}

// DeltaBody holds the delta content for a stream event.
type DeltaBody struct {
	Type string `json:"type"`
	Text string `json:"text,omitempty"`
}

// StreamedEventKind classifies events extracted from the Claude CLI stream.
type StreamedEventKind string

// Values mirror BlockType constants above (same Claude API wire format).
// Allowlisted in core/id_uniqueness_test.go — see sdkWireFormatAllowlist.
const (
	// KindText is a text response chunk from an assistant turn.
	KindText StreamedEventKind = "text"
	// KindToolUse is a tool invocation from an assistant turn.
	KindToolUse StreamedEventKind = "tool_use"
	// KindToolResult is the result of a previously announced tool call.
	// Claude emits these inside a `user`-typed message after each tool
	// invocation; the field carries `tool_use_id`, `content`, and an
	// optional `is_error`.
	KindToolResult StreamedEventKind = "tool_result"
	// KindTurnEnd marks the boundary of a completed assistant turn.
	KindTurnEnd StreamedEventKind = "turn_end"
)

// StreamedEvent is a typed, parsed event extracted from the Claude CLI NDJSON stream.
// The sdk message loop produces these; worker.Execute consumes them via OnEvent.
type StreamedEvent struct {
	Kind StreamedEventKind

	// KindText fields.
	Text string
	// IsDelta is true when this text chunk came from a StreamEvent partial delta
	// (--include-partial-messages), false when it came from a complete AssistantMessage
	// content block. Callers that handle OnChunk should not double-count: deltas are
	// the live stream, AssistantMessage blocks are the authoritative final text.
	IsDelta bool

	// KindToolUse fields.
	ToolID    string
	ToolName  string
	ToolInput json.RawMessage

	// KindToolResult fields. ToolID is shared with KindToolUse and
	// correlates the result back to the originating call.
	ToolOutput  string
	ToolIsError bool

	// KindTurnEnd fields.
	NumTurns   int
	DurationMs int64
	IsError    bool
	ErrorMsg   string
	SessionID  string    // populated from ResultMessage.SessionID on KindTurnEnd
	Cost       *CostInfo // populated from ResultMessage.Cost on KindTurnEnd; nil when unavailable
}

// GenericMessage wraps unknown message types
type GenericMessage struct {
	Type string                 `json:"type"`
	Raw  map[string]interface{} `json:"-"`
}

func (m *GenericMessage) GetType() string { return m.Type }

// ContentBlock represents a content block within a message
type ContentBlock struct {
	Type      string          `json:"type"`
	Text      string          `json:"text,omitempty"`
	ID        string          `json:"id,omitempty"`
	Name      string          `json:"name,omitempty"`
	Input     json.RawMessage `json:"input,omitempty"`
	Content   string          `json:"content,omitempty"`
	ToolUseID string          `json:"tool_use_id,omitempty"`
}

// CostInfo contains API cost information.
// InputTokens is the sum of all input tokens (raw + cache_creation + cache_read) for backward compatibility.
// CacheCreationTokens and CacheReadTokens carry the split so callers can attribute prompt-cache costs.
type CostInfo struct {
	InputTokens         int     `json:"input_tokens"`
	OutputTokens        int     `json:"output_tokens"`
	TotalCostUSD        float64 `json:"total_cost_usd"`
	CacheCreationTokens int     `json:"cache_creation_tokens,omitempty"`
	CacheReadTokens     int     `json:"cache_read_tokens,omitempty"`
}

// AgentOptions configures agent behavior
type AgentOptions struct {
	Model           string        `json:"model,omitempty"`
	EffortLevel     string        `json:"effort_level,omitempty"`
	MaxTurns        int           `json:"max_turns,omitempty"`
	PermissionMode  string        `json:"permission_mode,omitempty"`
	SystemPrompt    string        `json:"system_prompt,omitempty"`
	Cwd             string        `json:"cwd,omitempty"`
	CLIPath         string        `json:"-"`
	Timeout         time.Duration `json:"-"`
	ResumeSessionID string        `json:"-"` // if set, inject --resume <id> --fork-session; falls back to fresh on start failure
	AllowedEnvVars  []string              `json:"-"` // additional env vars to pass (skill-declared)
	AddDirs         []string              `json:"-"` // additional directories for CLAUDE.md discovery (--add-dir)
	OnChunk         func(string)          `json:"-"` // called with text chunks only; deprecated: prefer OnEvent
	OnEvent         func(*StreamedEvent)  `json:"-"` // called for every typed stream event; may be nil
	// PassthroughEnv, when true, forwards the full parent environment to the Claude subprocess
	// instead of the strict base allowlist. Use when the subprocess needs credentials or
	// toolchain paths beyond the base set. AllowedEnvVars are merged idempotently on top.
	PassthroughEnv bool `json:"-"`
	// ContinueConversation, when true, appends --continue to the Claude CLI invocation so
	// the subprocess resumes the most recent conversation. Mutually exclusive with ResumeSessionID.
	ContinueConversation bool `json:"-"`
}
