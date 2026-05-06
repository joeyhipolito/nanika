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

// AnthropicAPIExecutor runs phases by talking directly to the Anthropic
// Messages API over HTTP. It implements PhaseExecutor by delegating to the
// shared apiloop package and supplying an Anthropic-specific Provider.
//
// Zero value is not usable: call NewAnthropicAPIExecutor or build one with
// the registry, HTTP client, and pricing inputs you want.
type AnthropicAPIExecutor struct {
	BaseURL    string         // Anthropic API base, default https://api.anthropic.com
	HTTPClient *http.Client   // HTTP client used for all requests
	Tools      *tools.Registry // tool registry; nil → apiloop.SharedRegistry
	MaxTurns   int            // hard cap on agentic turns; 0 → 50

	LoadCredential    func(provider string) (*auth.Credential, error)
	RefreshCredential func(ctx context.Context, cred *auth.Credential) error

	Backoff func(attempt int) time.Duration
	Now     func() time.Time
}

// NewAnthropicAPIExecutor returns an executor with production defaults wired
// up: real HTTP client, real auth helpers, exponential backoff, 50-turn cap.
// When reg is nil the executor lazy-loads a registry on first Execute call.
func NewAnthropicAPIExecutor(reg *tools.Registry) *AnthropicAPIExecutor {
	base := os.Getenv("ANTHROPIC_API_URL")
	if base == "" {
		base = "https://api.anthropic.com"
	}
	return &AnthropicAPIExecutor{
		BaseURL:           strings.TrimRight(base, "/"),
		HTTPClient:        &http.Client{Timeout: 0},
		Tools:             reg,
		MaxTurns:          50,
		LoadCredential:    auth.LoadCredential,
		RefreshCredential: auth.RefreshOAuth,
		Backoff:           apiloop.DefaultBackoff,
		Now:               time.Now,
	}
}

// Describe implements RuntimeDescriber.
func (e *AnthropicAPIExecutor) Describe() core.RuntimeDescriptor {
	return core.AnthropicAPIDescriptor()
}

// Execute runs the phase against the Anthropic API by delegating to apiloop.
func (e *AnthropicAPIExecutor) Execute(
	ctx context.Context,
	config *core.WorkerConfig,
	emitter event.Emitter,
	verbose bool,
) (string, string, *sdk.CostInfo, error) {
	cfg := apiloop.Config{
		Provider:          &anthropicProvider{},
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

func (e *AnthropicAPIExecutor) resolveRegistry(ctx context.Context) *tools.Registry {
	if e.Tools != nil {
		return e.Tools
	}
	return apiloop.SharedRegistry(ctx)
}

// ---------------------------------------------------------------------------
// Provider implementation
// ---------------------------------------------------------------------------

// anthropicProvider implements apiloop.Provider for the Anthropic Messages API.
type anthropicProvider struct{}

func (anthropicProvider) Name() string                { return string(core.RuntimeAnthropicAPI) }
func (anthropicProvider) CredentialProvider() string  { return "anthropic" }
func (anthropicProvider) Endpoint() string            { return "/v1/messages" }
func (anthropicProvider) RefreshOn401(c *auth.Credential) bool {
	return c != nil && c.AuthType == auth.AuthTypeOAuth
}

func (anthropicProvider) ResolveModel(model string) string {
	if model == "" {
		return "claude-sonnet-4-6"
	}
	return resolveModelAlias("anthropic", model)
}

func (anthropicProvider) SetAuthHeaders(h http.Header, cred *auth.Credential) {
	h.Set("anthropic-version", "2023-06-01")
	if cred == nil {
		return
	}
	if cred.AuthType == auth.AuthTypeOAuth {
		h.Set("Authorization", "Bearer "+cred.AccessToken)
		h.Set("anthropic-beta", "oauth-2025-04-20")
		return
	}
	h.Set("x-api-key", cred.APIKey)
}

func (anthropicProvider) BuildRequest(req apiloop.Request) ([]byte, error) {
	body := anthropicRequest{
		Model:     req.Model,
		MaxTokens: req.MaxTokens,
		System:    req.System,
		Stream:    req.Stream,
		Messages:  toAnthropicMessages(req.Messages),
		Tools:     toAnthropicTools(req.Tools),
	}
	return json.Marshal(body)
}

func (anthropicProvider) ComputeCostUSD(model string, u *apiloop.Usage) float64 {
	return computeCostUSD(model, toAnthropicUsage(u))
}

// ParseStream consumes Anthropic's SSE event stream and returns a parsed
// StreamResult. The Anthropic format is event-driven: content_block_start,
// content_block_delta, content_block_stop, message_delta, message_stop. We
// accumulate per-index partial state and assemble Block values at stop time.
func (anthropicProvider) ParseStream(ctx context.Context, body io.Reader, onText func(string)) (apiloop.StreamResult, error) {
	type partial struct {
		typ        string
		text       strings.Builder
		toolID     string
		toolName   string
		toolJSONIn strings.Builder
	}
	var (
		blocks     []apiloop.Block
		current    = make(map[int]*partial)
		stopReason string
		usage      *apiloop.Usage
		modelID    string
	)

	scanErr := apiloop.ScanSSE(body, func(f apiloop.SSEFrame) bool {
		if ctx.Err() != nil {
			return false
		}
		if f.Data == "" {
			return true
		}
		var raw map[string]json.RawMessage
		if err := json.Unmarshal([]byte(f.Data), &raw); err != nil {
			return true
		}
		switch jsonString(raw, "type") {
		case "message_start":
			if msg, ok := raw["message"]; ok {
				var m struct {
					Model string         `json:"model"`
					Usage *anthropicUsage `json:"usage"`
				}
				if err := json.Unmarshal(msg, &m); err == nil {
					if m.Model != "" {
						modelID = m.Model
					}
					if m.Usage != nil {
						usage = fromAnthropicUsage(m.Usage)
					}
				}
			}

		case "content_block_start":
			idx := jsonInt(raw, "index")
			if cb, ok := raw["content_block"]; ok {
				var b anthropicContentBlock
				if err := json.Unmarshal(cb, &b); err == nil {
					current[idx] = &partial{typ: b.Type, toolID: b.ID, toolName: b.Name}
				}
			}

		case "content_block_delta":
			idx := jsonInt(raw, "index")
			p, ok := current[idx]
			if !ok {
				return true
			}
			if d, ok := raw["delta"]; ok {
				var dl struct {
					Type        string `json:"type"`
					Text        string `json:"text"`
					PartialJSON string `json:"partial_json"`
				}
				if err := json.Unmarshal(d, &dl); err == nil {
					switch dl.Type {
					case "text_delta":
						p.text.WriteString(dl.Text)
						if onText != nil {
							onText(dl.Text)
						}
					case "input_json_delta":
						p.toolJSONIn.WriteString(dl.PartialJSON)
					}
				}
			}

		case "content_block_stop":
			idx := jsonInt(raw, "index")
			p, ok := current[idx]
			if !ok {
				return true
			}
			switch p.typ {
			case "text":
				blocks = append(blocks, apiloop.Block{Type: "text", Text: p.text.String()})
			case "tool_use":
				input := json.RawMessage("{}")
				if s := strings.TrimSpace(p.toolJSONIn.String()); s != "" {
					input = json.RawMessage(s)
				}
				blocks = append(blocks, apiloop.Block{
					Type:      "tool_use",
					ToolUseID: p.toolID,
					ToolName:  p.toolName,
					Input:     input,
				})
			}
			delete(current, idx)

		case "message_delta":
			if d, ok := raw["delta"]; ok {
				var dl struct {
					StopReason string `json:"stop_reason"`
				}
				if err := json.Unmarshal(d, &dl); err == nil && dl.StopReason != "" {
					stopReason = dl.StopReason
				}
			}
			if u, ok := raw["usage"]; ok {
				var got anthropicUsage
				if err := json.Unmarshal(u, &got); err == nil {
					if usage == nil {
						usage = fromAnthropicUsage(&got)
					} else {
						// Anthropic reports cumulative output tokens here; merge by replacement.
						if got.OutputTokens > 0 {
							usage.OutputTokens = got.OutputTokens
						}
						if got.InputTokens > 0 {
							usage.InputTokens = got.InputTokens
						}
					}
				}
			}

		case "message_stop":
			// loop ends naturally
		}
		return true
	})
	if scanErr != nil {
		return apiloop.StreamResult{}, fmt.Errorf("read stream: %w", scanErr)
	}
	return apiloop.StreamResult{
		StopReason: stopReason,
		Blocks:     blocks,
		Usage:      usage,
		Model:      modelID,
	}, nil
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

type anthropicMessage struct {
	Role    string                  `json:"role"`
	Content []anthropicContentBlock `json:"content"`
}

type anthropicContentBlock struct {
	Type      string          `json:"type"`
	Text      string          `json:"text,omitempty"`
	ID        string          `json:"id,omitempty"`
	Name      string          `json:"name,omitempty"`
	Input     json.RawMessage `json:"input,omitempty"`
	ToolUseID string          `json:"tool_use_id,omitempty"`
	Content   string          `json:"content,omitempty"`
	IsError   bool            `json:"is_error,omitempty"`
}

type anthropicTool struct {
	Name        string          `json:"name"`
	Description string          `json:"description"`
	InputSchema json.RawMessage `json:"input_schema"`
}

type anthropicRequest struct {
	Model     string             `json:"model"`
	MaxTokens int                `json:"max_tokens"`
	System    string             `json:"system,omitempty"`
	Messages  []anthropicMessage `json:"messages"`
	Tools     []anthropicTool    `json:"tools,omitempty"`
	Stream    bool               `json:"stream,omitempty"`
}

// anthropicUsage matches the Anthropic API usage block shape exactly. Kept as
// its own type (rather than reusing apiloop.Usage) because field names track
// the wire format.
type anthropicUsage struct {
	InputTokens              int `json:"input_tokens"`
	OutputTokens             int `json:"output_tokens"`
	CacheCreationInputTokens int `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int `json:"cache_read_input_tokens"`
}

func fromAnthropicUsage(u *anthropicUsage) *apiloop.Usage {
	if u == nil {
		return nil
	}
	return &apiloop.Usage{
		InputTokens:         u.InputTokens,
		OutputTokens:        u.OutputTokens,
		CacheCreationTokens: u.CacheCreationInputTokens,
		CacheReadTokens:     u.CacheReadInputTokens,
	}
}

func toAnthropicUsage(u *apiloop.Usage) *anthropicUsage {
	if u == nil {
		return nil
	}
	return &anthropicUsage{
		InputTokens:              u.InputTokens,
		OutputTokens:             u.OutputTokens,
		CacheCreationInputTokens: u.CacheCreationTokens,
		CacheReadInputTokens:     u.CacheReadTokens,
	}
}

// toAnthropicMessages serializes provider-neutral messages into the Anthropic
// content-block format. Anthropic packs both tool_use and tool_result blocks
// inside content arrays (assistant for tool_use, user for tool_result), so a
// single user message containing tool_results maps 1:1.
func toAnthropicMessages(in []apiloop.Message) []anthropicMessage {
	out := make([]anthropicMessage, 0, len(in))
	for _, m := range in {
		am := anthropicMessage{Role: m.Role}
		for _, b := range m.Blocks {
			switch b.Type {
			case "text":
				am.Content = append(am.Content, anthropicContentBlock{Type: "text", Text: b.Text})
			case "tool_use":
				am.Content = append(am.Content, anthropicContentBlock{
					Type:  "tool_use",
					ID:    b.ToolUseID,
					Name:  b.ToolName,
					Input: b.Input,
				})
			case "tool_result":
				am.Content = append(am.Content, anthropicContentBlock{
					Type:      "tool_result",
					ToolUseID: b.ToolUseID,
					Content:   b.Output,
					IsError:   b.IsError,
				})
			}
		}
		out = append(out, am)
	}
	return out
}

func toAnthropicTools(defs []apiloop.ToolDef) []anthropicTool {
	if len(defs) == 0 {
		return nil
	}
	out := make([]anthropicTool, 0, len(defs))
	for _, d := range defs {
		out = append(out, anthropicTool{
			Name:        d.Name,
			Description: d.Description,
			InputSchema: d.Schema,
		})
	}
	return out
}

// ---------------------------------------------------------------------------
// Pricing
// ---------------------------------------------------------------------------

type modelPricing struct {
	InputPerMTok  float64
	OutputPerMTok float64
}

var modelPriceTable = map[string]modelPricing{
	"claude-opus-4-7":            {InputPerMTok: 15.00, OutputPerMTok: 75.00},
	"claude-opus-4-6":            {InputPerMTok: 15.00, OutputPerMTok: 75.00},
	"claude-sonnet-4-6":          {InputPerMTok: 3.00, OutputPerMTok: 15.00},
	"claude-sonnet-4-5":          {InputPerMTok: 3.00, OutputPerMTok: 15.00},
	"claude-haiku-4-5":           {InputPerMTok: 1.00, OutputPerMTok: 5.00},
	"claude-3-5-sonnet-20241022": {InputPerMTok: 3.00, OutputPerMTok: 15.00},
	"claude-3-5-haiku-20241022":  {InputPerMTok: 0.80, OutputPerMTok: 4.00},
}

// computeCostUSD applies the pricing table to the usage block. Unknown models
// fall back to Sonnet pricing so cost is non-zero even when a new model ID
// shows up before this table is updated.
func computeCostUSD(model string, u *anthropicUsage) float64 {
	if u == nil {
		return 0
	}
	price, ok := lookupPricing(model)
	if !ok {
		price = modelPricing{InputPerMTok: 3.00, OutputPerMTok: 15.00}
	}
	const million = 1_000_000.0
	input := float64(u.InputTokens) * price.InputPerMTok / million
	cacheCreate := float64(u.CacheCreationInputTokens) * price.InputPerMTok * 1.25 / million
	cacheRead := float64(u.CacheReadInputTokens) * price.InputPerMTok * 0.1 / million
	output := float64(u.OutputTokens) * price.OutputPerMTok / million
	return input + cacheCreate + cacheRead + output
}

func lookupPricing(model string) (modelPricing, bool) {
	if p, ok := modelPriceTable[model]; ok {
		return p, true
	}
	for prefix, p := range modelPriceTable {
		if strings.HasPrefix(model, prefix) {
			return p, true
		}
	}
	return modelPricing{}, false
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

func jsonString(raw map[string]json.RawMessage, key string) string {
	v, ok := raw[key]
	if !ok {
		return ""
	}
	var s string
	if err := json.Unmarshal(v, &s); err != nil {
		return ""
	}
	return s
}

func jsonInt(raw map[string]json.RawMessage, key string) int {
	v, ok := raw[key]
	if !ok {
		return 0
	}
	var n int
	if err := json.Unmarshal(v, &n); err != nil {
		return 0
	}
	return n
}
