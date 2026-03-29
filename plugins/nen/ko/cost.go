package ko

import (
	"context"
	"fmt"
	"strings"
)

// TokenUsage records input and output token counts from a single API call.
// CacheHit is set to true when the response came from cache (zero tokens consumed).
type TokenUsage struct {
	InputTokens  int
	OutputTokens int
	CacheHit     bool
}

// usageContextKey is the context key for the per-call usage recorder.
type usageContextKey struct{}

// cacheHitContextKey is the context key for the cache-hit flag.
type cacheHitContextKey struct{}

// WithUsageRecorder attaches a fresh *TokenUsage to ctx.
// The query function should call RecordUsage to populate it.
// runTest reads the pointer after queryFn returns.
func WithUsageRecorder(ctx context.Context) (context.Context, *TokenUsage) {
	u := &TokenUsage{}
	return context.WithValue(ctx, usageContextKey{}, u), u
}

// RecordUsage writes u into the usage recorder stored in ctx, if any.
func RecordUsage(ctx context.Context, u TokenUsage) {
	if ptr, ok := ctx.Value(usageContextKey{}).(*TokenUsage); ok && ptr != nil {
		ptr.InputTokens += u.InputTokens
		ptr.OutputTokens += u.OutputTokens
	}
}

// RecordCacheHit marks the usage recorder in ctx as a cache hit (zero tokens consumed).
func RecordCacheHit(ctx context.Context) {
	if ptr, ok := ctx.Value(usageContextKey{}).(*TokenUsage); ok && ptr != nil {
		ptr.CacheHit = true
	}
}

// CostUSD calculates the cost in USD for the given model and token usage.
// Prices are per million tokens (input / output) as of 2026-03.
func CostUSD(model string, usage TokenUsage) float64 {
	inputPer1M, outputPer1M := modelPrices(model)
	return float64(usage.InputTokens)/1_000_000*inputPer1M +
		float64(usage.OutputTokens)/1_000_000*outputPer1M
}

// modelPrices returns (inputPer1M, outputPer1M) USD for a model name.
// Falls back to opus-4 pricing for unknown models.
func modelPrices(model string) (inputPer1M, outputPer1M float64) {
	switch {
	case strings.HasPrefix(model, "claude-haiku-4"):
		return 0.80, 4.00
	case strings.HasPrefix(model, "claude-sonnet-4"):
		return 3.00, 15.00
	case strings.HasPrefix(model, "claude-opus-4"):
		return 15.00, 75.00
	case strings.HasPrefix(model, "claude-3-5-haiku"):
		return 0.80, 4.00
	case strings.HasPrefix(model, "claude-3-5-sonnet"):
		return 3.00, 15.00
	case strings.HasPrefix(model, "claude-3-opus"):
		return 15.00, 75.00
	default:
		return 15.00, 75.00 // conservative default
	}
}

// CostTracker aggregates token usage across multiple test calls.
// Not goroutine-safe; use from one goroutine (the OnResult callback is serial).
type CostTracker struct {
	total TokenUsage
	hits  int // cache hits (zero tokens consumed)
}

// Record adds usage from one query call.
func (t *CostTracker) Record(u TokenUsage) {
	t.total.InputTokens += u.InputTokens
	t.total.OutputTokens += u.OutputTokens
}

// RecordHit increments the cache-hit counter (no tokens consumed).
func (t *CostTracker) RecordHit() {
	t.hits++
}

// Total returns aggregate token usage across all recorded calls.
func (t *CostTracker) Total() TokenUsage { return t.total }

// FormatSummary returns a human-readable cost line for the given model.
func (t *CostTracker) FormatSummary(model string) string {
	cost := CostUSD(model, t.total)
	s := fmt.Sprintf("tokens: %d in / %d out  cost: $%.6f",
		t.total.InputTokens, t.total.OutputTokens, cost)
	if t.hits > 0 {
		s += fmt.Sprintf("  cache hits: %d", t.hits)
	}
	return s
}
