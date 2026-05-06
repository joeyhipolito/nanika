// Package apiloop is the shared agentic message-loop used by HTTP-API
// executors (Anthropic, OpenAI, OpenRouter).
//
// The loop is parameterized on a Provider so each backend supplies only the
// parts that genuinely differ: request shaping, SSE/JSON parsing, tool-format
// translation, auth headers, and cost math. Everything else — retry/backoff,
// 401 + OAuth-refresh, parallel tool dispatch, message accumulation, output
// emission, and artifact writing — lives here exactly once.
//
// Gemini support is deferred. When added, it will register a third Provider
// implementation that targets the Generative Language API; no changes to this
// package should be required.
package apiloop

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/auth"
	"github.com/joeyhipolito/orchestrator-cli/internal/tools"
)

// Block is a provider-neutral content block. The Block kind drives both
// per-provider serialization (in Provider.BuildRequest) and the agentic loop's
// own routing decisions in Run.
type Block struct {
	Type      string          // "text" | "tool_use" | "tool_result"
	Text      string          // populated when Type == "text"
	ToolUseID string          // tool_use.id (and tool_result.tool_use_id)
	ToolName  string          // tool_use.name
	Input     json.RawMessage // tool_use.input — opaque JSON
	Output    string          // tool_result.content
	IsError   bool            // tool_result.is_error
}

// Message bundles a turn's role with its content blocks.
type Message struct {
	Role   string // "user" | "assistant" | "system"
	Blocks []Block
}

// Usage is the token-count breakdown returned by every provider. Cache fields
// are zero for providers that do not bill prompt caching separately.
type Usage struct {
	InputTokens         int
	OutputTokens        int
	CacheCreationTokens int
	CacheReadTokens     int
}

// StreamResult is the parsed outcome of a single HTTP request to the provider.
type StreamResult struct {
	StopReason string  // "end_turn" | "tool_use" | "max_tokens" | "stop"
	Blocks     []Block // assistant turn content blocks (text + tool_use)
	Usage      *Usage  // token counts; nil when the provider didn't emit any
	Model      string  // model id reported by the provider
}

// ToolDef describes one available tool in the provider-neutral shape. The
// Provider re-renders these into its own tool schema during BuildRequest.
type ToolDef struct {
	Name        string
	Description string
	Schema      json.RawMessage // JSON Schema draft 7 input schema
}

// Request is the per-call payload Run hands to the Provider for serialization.
type Request struct {
	Model     string
	MaxTokens int
	System    string
	Messages  []Message
	Tools     []ToolDef
	Stream    bool
}

// PhaseCtx bundles the identifiers carried in every emitted event.
type PhaseCtx struct {
	MissionID  string
	PhaseID    string
	WorkerName string
}

// Provider plugs in per-API differences. Implementations are expected to be
// stateless; concurrency is the loop's responsibility, not the provider's.
type Provider interface {
	// Name returns a short identifier used in error messages and logs.
	Name() string

	// CredentialProvider is the key passed to LoadCredential, e.g. "anthropic".
	CredentialProvider() string

	// Endpoint returns the path appended to BaseURL for the messages call.
	Endpoint() string

	// BuildRequest serializes a provider-neutral Request into a request body.
	BuildRequest(req Request) ([]byte, error)

	// SetAuthHeaders applies provider-specific authorization headers.
	SetAuthHeaders(h http.Header, cred *auth.Credential)

	// ParseStream consumes the HTTP response body and returns the parsed
	// stream result. onText is invoked with each text-delta chunk so the
	// caller can throttle and forward to the worker output emitter.
	ParseStream(ctx context.Context, body io.Reader, onText func(string)) (StreamResult, error)

	// ComputeCostUSD applies the provider's pricing table to a Usage block.
	ComputeCostUSD(model string, u *Usage) float64

	// RefreshOn401 reports whether a 401 should trigger an OAuth refresh and
	// retry. API-key providers return false; OAuth providers return true.
	RefreshOn401(cred *auth.Credential) bool

	// ResolveModel returns the configured model or a sensible per-provider
	// default when the input is empty.
	ResolveModel(model string) string
}

// Config carries the dependencies Run needs. Most fields default to sensible
// production values when zero (see normalizeConfig).
type Config struct {
	Provider          Provider
	BaseURL           string
	HTTPClient        *http.Client
	Tools             *tools.Registry
	MaxTurns          int
	LoadCredential    func(string) (*auth.Credential, error)
	RefreshCredential func(context.Context, *auth.Credential) error
	Backoff           func(int) time.Duration
	Now               func() time.Time
}

// APIError is returned by the streaming layer for non-2xx HTTP responses so
// the retry loop can match on status without re-sniffing the body.
type APIError struct {
	Status     int
	Body       string
	RetryAfter time.Duration
}

func (e *APIError) Error() string {
	if t := http.StatusText(e.Status); t != "" {
		return fmt.Sprintf("apiloop: status %d %s: %s", e.Status, t, e.Body)
	}
	return fmt.Sprintf("apiloop: status %d: %s", e.Status, e.Body)
}
