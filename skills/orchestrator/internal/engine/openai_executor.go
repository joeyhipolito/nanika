package engine

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/auth"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/engine/apiloop"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/tools"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// OpenAIAPIExecutor runs phases by talking directly to the OpenAI Chat
// Completions API. It implements PhaseExecutor by delegating to the shared
// apiloop package and supplying an OpenAI-specific Provider.
//
// Tool calls use the OpenAI "tools" + "function_call"/"tool" role format.
// Cancellation, retry, OAuth refresh (n/a here — API key only), and tool
// dispatch are all handled by apiloop.Run.
type OpenAIAPIExecutor struct {
	BaseURL    string         // OpenAI API base, default https://api.openai.com
	HTTPClient *http.Client   // HTTP client used for all requests
	Tools      *tools.Registry // tool registry; nil → apiloop.SharedRegistry
	MaxTurns   int            // hard cap on agentic turns; 0 → 50

	// CredProvider lets OpenRouter reuse this executor by swapping the
	// credential lookup key (e.g. "openrouter") and base URL while keeping
	// the OpenAI Chat Completions wire format.
	CredProvider string

	// DefaultModel is used when WorkerConfig.Model is empty.
	DefaultModel string

	// PricingTable is consulted by the executor's cost math. Keys are model IDs.
	// OpenRouter passes a different table because its model IDs are namespaced
	// ("anthropic/claude-3.5-sonnet").
	PricingTable map[string]modelPricing

	// ExtraHeaders are added to every request. OpenRouter uses this to attach
	// HTTP-Referer and X-Title for analytics.
	ExtraHeaders map[string]string

	// RuntimeName is the descriptor name returned by Describe().
	RuntimeName core.Runtime

	LoadCredential    func(provider string) (*auth.Credential, error)
	RefreshCredential func(ctx context.Context, cred *auth.Credential) error

	Backoff func(attempt int) time.Duration
	Now     func() time.Time
}

// NewOpenAIAPIExecutor returns an executor with production defaults wired up.
// reg may be nil; when so the executor lazy-loads the registry on first call.
func NewOpenAIAPIExecutor(reg *tools.Registry) *OpenAIAPIExecutor {
	base := os.Getenv("OPENAI_API_URL")
	if base == "" {
		base = "https://api.openai.com"
	}
	return &OpenAIAPIExecutor{
		BaseURL:           strings.TrimRight(base, "/"),
		HTTPClient:        &http.Client{Timeout: 0},
		Tools:             reg,
		MaxTurns:          50,
		CredProvider:      "openai",
		DefaultModel:      "gpt-4o",
		PricingTable:      defaultOpenAIPricing,
		RuntimeName:       core.RuntimeOpenAIAPI,
		LoadCredential:    auth.LoadCredential,
		RefreshCredential: auth.RefreshOAuth,
		Backoff:           apiloop.DefaultBackoff,
		Now:               time.Now,
	}
}

// Describe implements RuntimeDescriber.
func (e *OpenAIAPIExecutor) Describe() core.RuntimeDescriptor {
	switch e.RuntimeName {
	case core.RuntimeOpenRouter:
		return core.OpenRouterDescriptor()
	default:
		return core.OpenAIAPIDescriptor()
	}
}

// Execute runs the phase against the OpenAI-compatible API by delegating to
// apiloop.Run with an openaiProvider tied to this executor's settings.
func (e *OpenAIAPIExecutor) Execute(
	ctx context.Context,
	config *core.WorkerConfig,
	emitter event.Emitter,
	verbose bool,
) (string, string, *sdk.CostInfo, error) {
	prov := &openaiProvider{
		runtime:      e.RuntimeName,
		credProvider: e.CredProvider,
		defaultModel: e.DefaultModel,
		pricing:      e.PricingTable,
		extraHeaders: e.ExtraHeaders,
	}
	cfg := apiloop.Config{
		Provider:          prov,
		BaseURL:           e.BaseURL,
		HTTPClient:        e.HTTPClient,
		Tools:             e.resolveRegistry(ctx),
		MaxTurns:          e.MaxTurns,
		LoadCredential:    e.LoadCredential,
		RefreshCredential: e.RefreshCredential,
		Backoff:           e.Backoff,
		Now:               e.Now,
	}
	return apiloop.Run(ctx, cfg, config, emitter, verbose)
}

func (e *OpenAIAPIExecutor) resolveRegistry(ctx context.Context) *tools.Registry {
	if e.Tools != nil {
		return e.Tools
	}
	return apiloop.SharedRegistry(ctx)
}

// ---------------------------------------------------------------------------
// Provider implementation
// ---------------------------------------------------------------------------

// openaiProvider implements apiloop.Provider for the OpenAI Chat Completions
// API and any wire-compatible reuse (OpenRouter).
type openaiProvider struct {
	runtime      core.Runtime
	credProvider string
	defaultModel string
	pricing      map[string]modelPricing
	extraHeaders map[string]string
}

func (p *openaiProvider) Name() string                { return string(p.runtime) }
func (p *openaiProvider) CredentialProvider() string  { return p.credProvider }
func (p *openaiProvider) Endpoint() string            { return "/v1/chat/completions" }
func (p *openaiProvider) RefreshOn401(_ *auth.Credential) bool {
	// OpenAI / OpenRouter use API keys; there is no refresh path.
	return false
}

func (p *openaiProvider) ResolveModel(model string) string {
	if model == "" {
		return p.defaultModel
	}
	provider := "openai"
	if p.runtime == core.RuntimeOpenRouter {
		provider = "openrouter"
	}
	return resolveModelAlias(provider, model)
}

func (p *openaiProvider) SetAuthHeaders(h http.Header, cred *auth.Credential) {
	if cred != nil && cred.APIKey != "" {
		h.Set("Authorization", "Bearer "+cred.APIKey)
	} else if cred != nil && cred.AccessToken != "" {
		h.Set("Authorization", "Bearer "+cred.AccessToken)
	}
	for k, v := range p.extraHeaders {
		h.Set(k, v)
	}
}

func (p *openaiProvider) BuildRequest(req apiloop.Request) ([]byte, error) {
	body := openaiRequest{
		Model:         req.Model,
		MaxTokens:     req.MaxTokens,
		Stream:        req.Stream,
		Messages:      toOpenAIMessages(req.System, req.Messages),
		Tools:         toOpenAITools(req.Tools),
		StreamOptions: &openaiStreamOptions{IncludeUsage: req.Stream},
	}
	return json.Marshal(body)
}

func (p *openaiProvider) ComputeCostUSD(model string, u *apiloop.Usage) float64 {
	if u == nil {
		return 0
	}
	price, ok := lookupOpenAIPricing(p.pricing, model)
	if !ok {
		// Unknown-model fallback: use gpt-4o pricing so cost is non-zero
		// rather than silently dropping to $0 on a new model ID.
		price = modelPricing{InputPerMTok: 2.50, OutputPerMTok: 10.00}
	}
	const million = 1_000_000.0
	input := float64(u.InputTokens) * price.InputPerMTok / million
	cacheRead := float64(u.CacheReadTokens) * price.InputPerMTok * 0.5 / million
	output := float64(u.OutputTokens) * price.OutputPerMTok / million
	return input + cacheRead + output
}

// ParseStream consumes OpenAI's SSE stream of chat.completion.chunk events.
// Tool calls arrive as deltas with `tool_calls[i].function.arguments` chunks
// that need to be concatenated until the chunk's finish_reason settles.
func (p *openaiProvider) ParseStream(ctx context.Context, body io.Reader, onText func(string)) (apiloop.StreamResult, error) {
	type partialToolCall struct {
		id    string
		name  string
		args  strings.Builder
	}
	var (
		textBuf      strings.Builder
		toolPartials = map[int]*partialToolCall{}
		toolOrder    []int // preserve emission order
		stopReason   string
		usage        *apiloop.Usage
		modelID      string
	)

	scanErr := apiloop.ScanSSE(body, func(f apiloop.SSEFrame) bool {
		if ctx.Err() != nil {
			return false
		}
		if f.Data == "" {
			return true
		}
		if strings.TrimSpace(f.Data) == "[DONE]" {
			return false
		}
		var chunk openaiChunk
		if err := json.Unmarshal([]byte(f.Data), &chunk); err != nil {
			return true
		}
		if chunk.Model != "" {
			modelID = chunk.Model
		}
		if chunk.Usage != nil {
			usage = &apiloop.Usage{
				InputTokens:     chunk.Usage.PromptTokens,
				OutputTokens:    chunk.Usage.CompletionTokens,
				CacheReadTokens: chunk.Usage.PromptTokensDetails.CachedTokens,
			}
		}
		for _, ch := range chunk.Choices {
			if ch.Delta.Content != "" {
				textBuf.WriteString(ch.Delta.Content)
				if onText != nil {
					onText(ch.Delta.Content)
				}
			}
			for _, tc := range ch.Delta.ToolCalls {
				idx := tc.Index
				entry, ok := toolPartials[idx]
				if !ok {
					entry = &partialToolCall{}
					toolPartials[idx] = entry
					toolOrder = append(toolOrder, idx)
				}
				if tc.ID != "" {
					entry.id = tc.ID
				}
				if tc.Function.Name != "" {
					entry.name = tc.Function.Name
				}
				if tc.Function.Arguments != "" {
					entry.args.WriteString(tc.Function.Arguments)
				}
			}
			if ch.FinishReason != "" {
				stopReason = mapFinishReason(ch.FinishReason)
			}
		}
		return true
	})
	if scanErr != nil {
		return apiloop.StreamResult{}, fmt.Errorf("read stream: %w", scanErr)
	}

	var blocks []apiloop.Block
	if textBuf.Len() > 0 {
		blocks = append(blocks, apiloop.Block{Type: "text", Text: textBuf.String()})
	}
	for _, idx := range toolOrder {
		entry := toolPartials[idx]
		input := json.RawMessage("{}")
		if s := strings.TrimSpace(entry.args.String()); s != "" {
			input = json.RawMessage(s)
		}
		blocks = append(blocks, apiloop.Block{
			Type:      "tool_use",
			ToolUseID: entry.id,
			ToolName:  entry.name,
			Input:     input,
		})
	}
	return apiloop.StreamResult{
		StopReason: stopReason,
		Blocks:     blocks,
		Usage:      usage,
		Model:      modelID,
	}, nil
}

// mapFinishReason converts OpenAI's finish_reason values into the
// apiloop-standard stop_reason vocabulary used by the loop.
func mapFinishReason(r string) string {
	switch r {
	case "tool_calls", "function_call":
		return "tool_use"
	case "stop":
		return "end_turn"
	default:
		return r
	}
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

type openaiRequest struct {
	Model         string                `json:"model"`
	MaxTokens     int                   `json:"max_tokens,omitempty"`
	Messages      []openaiMessage       `json:"messages"`
	Tools         []openaiTool          `json:"tools,omitempty"`
	Stream        bool                  `json:"stream,omitempty"`
	StreamOptions *openaiStreamOptions  `json:"stream_options,omitempty"`
}

type openaiStreamOptions struct {
	IncludeUsage bool `json:"include_usage"`
}

type openaiMessage struct {
	Role       string             `json:"role"`
	Content    string             `json:"content,omitempty"`
	ToolCalls  []openaiToolCall   `json:"tool_calls,omitempty"`
	ToolCallID string             `json:"tool_call_id,omitempty"`
	Name       string             `json:"name,omitempty"`
}

type openaiToolCall struct {
	ID       string             `json:"id"`
	Type     string             `json:"type"` // "function"
	Function openaiFunctionCall `json:"function"`
	Index    int                `json:"index,omitempty"` // streaming only
}

type openaiFunctionCall struct {
	Name      string `json:"name,omitempty"`
	Arguments string `json:"arguments,omitempty"`
}

type openaiTool struct {
	Type     string         `json:"type"` // "function"
	Function openaiFunction `json:"function"`
}

type openaiFunction struct {
	Name        string          `json:"name"`
	Description string          `json:"description,omitempty"`
	Parameters  json.RawMessage `json:"parameters,omitempty"`
}

type openaiChunk struct {
	ID      string         `json:"id"`
	Model   string         `json:"model"`
	Choices []openaiChoice `json:"choices"`
	Usage   *openaiUsage   `json:"usage,omitempty"`
}

type openaiChoice struct {
	Index        int          `json:"index"`
	Delta        openaiDelta  `json:"delta"`
	FinishReason string       `json:"finish_reason"`
}

type openaiDelta struct {
	Role      string                 `json:"role,omitempty"`
	Content   string                 `json:"content,omitempty"`
	ToolCalls []openaiDeltaToolCall  `json:"tool_calls,omitempty"`
}

type openaiDeltaToolCall struct {
	Index    int                `json:"index"`
	ID       string             `json:"id,omitempty"`
	Type     string             `json:"type,omitempty"`
	Function openaiFunctionCall `json:"function"`
}

type openaiUsage struct {
	PromptTokens          int `json:"prompt_tokens"`
	CompletionTokens      int `json:"completion_tokens"`
	TotalTokens           int `json:"total_tokens"`
	PromptTokensDetails   struct {
		CachedTokens int `json:"cached_tokens"`
	} `json:"prompt_tokens_details"`
}

// toOpenAIMessages flattens the provider-neutral message stream into OpenAI's
// chat-message format. Anthropic packs both tool_use and tool_result blocks
// inside content arrays; OpenAI splits them: tool_calls live on the assistant
// message, tool_results become standalone {role: "tool"} messages.
func toOpenAIMessages(system string, in []apiloop.Message) []openaiMessage {
	out := make([]openaiMessage, 0, len(in)+1)
	if system != "" {
		out = append(out, openaiMessage{Role: "system", Content: system})
	}
	for _, m := range in {
		switch m.Role {
		case "assistant":
			msg := openaiMessage{Role: "assistant"}
			var text strings.Builder
			for _, b := range m.Blocks {
				switch b.Type {
				case "text":
					text.WriteString(b.Text)
				case "tool_use":
					args := string(b.Input)
					if args == "" {
						args = "{}"
					}
					msg.ToolCalls = append(msg.ToolCalls, openaiToolCall{
						ID:   b.ToolUseID,
						Type: "function",
						Function: openaiFunctionCall{
							Name:      b.ToolName,
							Arguments: args,
						},
					})
				}
			}
			msg.Content = text.String()
			out = append(out, msg)

		case "user":
			// A user message may contain a mix of text and tool_result blocks.
			// Emit role="tool" entries for each tool_result and one role="user"
			// entry for any remaining text, in original order.
			var pendingText strings.Builder
			flushText := func() {
				if pendingText.Len() == 0 {
					return
				}
				out = append(out, openaiMessage{Role: "user", Content: pendingText.String()})
				pendingText.Reset()
			}
			for _, b := range m.Blocks {
				switch b.Type {
				case "text":
					pendingText.WriteString(b.Text)
				case "tool_result":
					flushText()
					out = append(out, openaiMessage{
						Role:       "tool",
						ToolCallID: b.ToolUseID,
						Content:    b.Output,
					})
				}
			}
			flushText()

		default:
			// Fall back to "user" for unknown roles to preserve content.
			var text strings.Builder
			for _, b := range m.Blocks {
				if b.Type == "text" {
					text.WriteString(b.Text)
				}
			}
			out = append(out, openaiMessage{Role: m.Role, Content: text.String()})
		}
	}
	return out
}

func toOpenAITools(defs []apiloop.ToolDef) []openaiTool {
	if len(defs) == 0 {
		return nil
	}
	out := make([]openaiTool, 0, len(defs))
	for _, d := range defs {
		params := d.Schema
		if len(params) == 0 {
			params = json.RawMessage(`{"type":"object"}`)
		}
		out = append(out, openaiTool{
			Type: "function",
			Function: openaiFunction{
				Name:        d.Name,
				Description: d.Description,
				Parameters:  params,
			},
		})
	}
	return out
}

// ---------------------------------------------------------------------------
// Pricing
// ---------------------------------------------------------------------------

// defaultOpenAIPricing covers the OpenAI native models the orchestrator is
// likely to dispatch. Prices are per million tokens. New IDs fall back to
// gpt-4o pricing in ComputeCostUSD so cost is never silently zero.
var defaultOpenAIPricing = map[string]modelPricing{
	"gpt-4o":              {InputPerMTok: 2.50, OutputPerMTok: 10.00},
	"gpt-4o-mini":         {InputPerMTok: 0.15, OutputPerMTok: 0.60},
	"gpt-4-turbo":         {InputPerMTok: 10.00, OutputPerMTok: 30.00},
	"gpt-4":               {InputPerMTok: 30.00, OutputPerMTok: 60.00},
	"gpt-3.5-turbo":       {InputPerMTok: 0.50, OutputPerMTok: 1.50},
	"o1-preview":          {InputPerMTok: 15.00, OutputPerMTok: 60.00},
	"o1-mini":             {InputPerMTok: 3.00, OutputPerMTok: 12.00},
	"o1":                  {InputPerMTok: 15.00, OutputPerMTok: 60.00},
}

func lookupOpenAIPricing(table map[string]modelPricing, model string) (modelPricing, bool) {
	if p, ok := table[model]; ok {
		return p, true
	}
	for prefix, p := range table {
		if strings.HasPrefix(model, prefix) {
			return p, true
		}
	}
	return modelPricing{}, false
}
