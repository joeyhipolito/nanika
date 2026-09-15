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
// ErrPreventiveRequiresDuplex prevents a one-shot call from bypassing an
// explicitly requested permission gate.
var ErrPreventiveRequiresDuplex = errors.New("sdk: preventive permissions require NewQuery duplex sessions")

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
	// Usage carries the per-message token usage the claude CLI stream-json
	// format emits on every assistant message. Without an explicit field,
	// encoding/json silently drops undeclared wire keys — the Go-side twin
	// of the TRK-1139 zod strip that hid this same data on the TS side.
	Usage *UsageInfo `json:"usage,omitempty"`
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
	// KindPermissionRequest is an out-of-band control_request the CLI emits
	// under preventive permission mode (--permission-prompt-tool stdio) for
	// every "ask"-tier tool. The consumer must answer via Query.RespondPermission
	// before the CLI proceeds. Only produced on the duplex NewQuery path; the
	// default (bypass) path runs with --dangerously-skip-permissions and never
	// emits it. Additive; old consumers ignore the kind.
	KindPermissionRequest StreamedEventKind = "permission_request"
)

// PermissionMode values for AgentOptions.PermissionMode. The zero value ("")
// retains historical Query defaults: the CLI runs with
// --dangerously-skip-permissions. SubprocessTransport instead preserves CLI
// permission checks by default and requires the literal "bypass" to skip them.
const (
	// PermissionBypass ("") runs the CLI with --dangerously-skip-permissions:
	// the CLI owns tool execution and never asks the client. This is the
	// zero-value behavior for QueryText and NewQuery. The literal
	// "bypass" is accepted as a loud alias for the same behavior.
	PermissionBypass = ""
	// PermissionPreventive activates the control-protocol gate: the CLI emits a
	// can_use_tool control_request for every "ask"-tier tool and blocks until the
	// client answers with a control_response. Valid ONLY on the duplex NewQuery
	// path (it needs --input-format stream-json for the return channel); the
	// one-shot QueryText and SubprocessTransport paths reject it.
	PermissionPreventive = "preventive"
)

// PermissionRequest carries a can_use_tool control_request surfaced to the
// caller as a KindPermissionRequest StreamedEvent. The caller inspects it,
// decides, and answers via Query.RespondPermission keyed on RequestID.
type PermissionRequest struct {
	RequestID string          // control_request.request_id — correlation key for the response
	ToolName  string          // request.tool_name, e.g. "Bash"
	ToolUseID string          // request.tool_use_id
	Input     json.RawMessage // request.input (verbatim; carries {"command","description",...})
	Reason    string          // request.decision_reason, e.g. "This command requires approval"
}

// PermissionDecision is the caller's answer to a PermissionRequest. Allow=false
// denies the tool and surfaces Message to the model as the tool_result error.
type PermissionDecision struct {
	Allow        bool            // true → behavior:"allow"; false → behavior:"deny"
	Message      string          // deny message surfaced to the model (behavior:"deny")
	UpdatedInput json.RawMessage // allow only; nil → the CLI keeps the original input unchanged
}

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

	// KindPermissionRequest field. Nil for every other kind. Carries the
	// can_use_tool control_request the caller must answer via RespondPermission.
	Permission *PermissionRequest

	// Usage carries the per-message token usage from the AssistantMessage
	// this event was extracted from, when the CLI supplied one. Set only on
	// the first event extracted from a given message; nil when the engine
	// surfaces no per-message usage (e.g. codex/glm). Consumers track the
	// last non-nil value seen during a turn to compute a truthful
	// context_pct instead of the cumulative session ledger.
	Usage *UsageInfo
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
	Model          string `json:"model,omitempty"`
	EffortLevel    string `json:"effort_level,omitempty"`
	MaxTurns       int    `json:"max_turns,omitempty"`
	PermissionMode string `json:"permission_mode,omitempty"`
	SystemPrompt   string `json:"system_prompt,omitempty"`
	// AppendSystemPrompt carries a persona/briefing appended to the CLI's default
	// system prompt via --append-system-prompt. Unlike SystemPrompt (which
	// --system-prompt REPLACES the default with, disabling the harness's own
	// CLAUDE.md/skills/settings assembly), append preserves that assembly. When
	// set, the builder does not emit --system-prompt even if SystemPrompt is also
	// set — append wins.
	AppendSystemPrompt string               `json:"append_system_prompt,omitempty"`
	Cwd                string               `json:"cwd,omitempty"`
	CLIPath            string               `json:"-"`
	Timeout            time.Duration        `json:"-"`
	ResumeSessionID    string               `json:"-"` // if set, inject --resume <id> --fork-session; falls back to fresh on start failure
	AllowedEnvVars     []string             `json:"-"` // additional env vars to pass (skill-declared)
	AddDirs            []string             `json:"-"` // additional directories for CLAUDE.md discovery (--add-dir)
	OnChunk            func(string)         `json:"-"` // called with text chunks only; deprecated: prefer OnEvent
	OnEvent            func(*StreamedEvent) `json:"-"` // called for every typed stream event; may be nil
	// PassthroughEnv, when true, forwards the full parent environment to the Claude subprocess
	// instead of the strict base allowlist. Use when the subprocess needs credentials or
	// toolchain paths beyond the base set. AllowedEnvVars are merged idempotently on top.
	PassthroughEnv bool `json:"-"`
	// ContinueConversation, when true, appends --continue to the Claude CLI invocation so
	// the subprocess resumes the most recent conversation. Mutually exclusive with ResumeSessionID.
	ContinueConversation bool `json:"-"`
	// DisableBuiltinTools, when true, appends --tools "" so the subprocess runs with
	// no built-in tools and can only reply as text. Callers that drive their own
	// text-based action protocol (e.g. daemon chat) set this so the model never
	// attempts a built-in tool call that the runtime would reject.
	DisableBuiltinTools bool `json:"-"`
	// DisableMCP, when true, appends --strict-mcp-config --mcp-config
	// '{"mcpServers":{}}' so the subprocess skips loading the user's configured
	// MCP servers entirely. Tool-less spawns (advisors, arbiters, detached
	// advisor jobs) set this: they cannot call tools, so MCP server init is
	// pure spawn latency (~3s measured against a large server set, TRK-1135).
	DisableMCP bool `json:"-"`
}
