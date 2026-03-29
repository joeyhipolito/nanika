// Package sdk provides a Go SDK for communicating with Claude Code CLI.
package sdk

import (
	"encoding/json"
	"time"
)

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
// See internal/sdk/DESIGN.md for the full rationale.
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

// SystemMessage represents system-level communications
type SystemMessage struct {
	Type    string                 `json:"type"`
	Subtype string                 `json:"subtype,omitempty"`
	Data    map[string]interface{} `json:"data,omitempty"`
}

func (m *SystemMessage) GetType() string { return MessageTypeSystem }

// ResultMessage represents execution completion
type ResultMessage struct {
	Type         string          `json:"type"`
	Subtype      string          `json:"subtype,omitempty"`
	SessionID    string          `json:"session_id,omitempty"`
	DurationMs   int64           `json:"duration_ms,omitempty"`
	NumTurns     int             `json:"num_turns,omitempty"`
	Cost         *CostInfo       `json:"cost,omitempty"`
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

// CostInfo contains API cost information
type CostInfo struct {
	InputTokens  int     `json:"input_tokens"`
	OutputTokens int     `json:"output_tokens"`
	TotalCostUSD float64 `json:"total_cost_usd"`
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
}
