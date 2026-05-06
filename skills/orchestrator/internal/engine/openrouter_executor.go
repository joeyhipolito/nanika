package engine

import (
	"net/http"
	"os"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/auth"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/engine/apiloop"
	"github.com/joeyhipolito/orchestrator-cli/internal/tools"
)

// NewOpenRouterAPIExecutor returns an OpenAI-compatible executor pointed at
// OpenRouter. OpenRouter speaks the Chat Completions API verbatim, so the
// runtime is a thin wrapper over OpenAIAPIExecutor: same Provider, different
// base URL, different credential lookup key, and a richer pricing table whose
// keys are the namespaced model IDs OpenRouter requires
// (e.g. "anthropic/claude-3.5-sonnet", "openai/gpt-4o").
//
// Optional analytics headers (HTTP-Referer, X-Title) are read from
// OPENROUTER_HTTP_REFERER and OPENROUTER_X_TITLE so they can be set per-host
// without recompilation; both default to empty (no header set).
func NewOpenRouterAPIExecutor(reg *tools.Registry) *OpenAIAPIExecutor {
	base := os.Getenv("OPENROUTER_API_URL")
	if base == "" {
		base = "https://openrouter.ai/api"
	}
	hdrs := map[string]string{}
	if v := os.Getenv("OPENROUTER_HTTP_REFERER"); v != "" {
		hdrs["HTTP-Referer"] = v
	}
	if v := os.Getenv("OPENROUTER_X_TITLE"); v != "" {
		hdrs["X-Title"] = v
	}
	return &OpenAIAPIExecutor{
		BaseURL:           strings.TrimRight(base, "/"),
		HTTPClient:        &http.Client{Timeout: 0},
		Tools:             reg,
		MaxTurns:          50,
		CredProvider:      "openrouter",
		DefaultModel:      "openai/gpt-4o",
		PricingTable:      defaultOpenRouterPricing,
		ExtraHeaders:      hdrs,
		RuntimeName:       core.RuntimeOpenRouter,
		LoadCredential:    auth.LoadCredential,
		RefreshCredential: auth.RefreshOAuth,
		Backoff:           apiloop.DefaultBackoff,
		Now:               time.Now,
	}
}

// defaultOpenRouterPricing covers the most common namespaced models. Prices
// match OpenRouter's published rates for each underlying provider as of
// writing; users hitting an unlisted model fall back to gpt-4o pricing in
// openaiProvider.ComputeCostUSD. Add entries here when shipping new models.
//
// OpenRouter charges essentially the same as the underlying provider plus a
// small surcharge passed through automatically.
var defaultOpenRouterPricing = map[string]modelPricing{
	"openai/gpt-4o":                    {InputPerMTok: 2.50, OutputPerMTok: 10.00},
	"openai/gpt-4o-mini":               {InputPerMTok: 0.15, OutputPerMTok: 0.60},
	"openai/gpt-4-turbo":               {InputPerMTok: 10.00, OutputPerMTok: 30.00},
	"anthropic/claude-3.5-sonnet":      {InputPerMTok: 3.00, OutputPerMTok: 15.00},
	"anthropic/claude-3.5-haiku":       {InputPerMTok: 0.80, OutputPerMTok: 4.00},
	"anthropic/claude-3-opus":          {InputPerMTok: 15.00, OutputPerMTok: 75.00},
	"google/gemini-pro-1.5":            {InputPerMTok: 1.25, OutputPerMTok: 5.00},
	"meta-llama/llama-3.1-70b-instruct": {InputPerMTok: 0.40, OutputPerMTok: 0.40},
	"mistralai/mistral-large":          {InputPerMTok: 2.00, OutputPerMTok: 6.00},
}
