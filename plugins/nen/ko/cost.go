package ko

import (
	"context"
	"fmt"
	"strings"
)

// TokenUsage records input and output token counts from a single API call.
// CacheHit is set to true when the response came from cache (zero tokens consumed).
// CacheCreation5m and CacheCreation1h track tokens written to cache (5-minute and
// 1-hour TTL tiers respectively). CacheRead tracks tokens read from cache.
type TokenUsage struct {
	InputTokens      int
	OutputTokens     int
	CacheHit         bool
	CacheCreation5m  int // tokens written to 5-minute cache (billed at 1.25x input rate)
	CacheCreation1h  int // tokens written to 1-hour cache (billed at 2x input rate)
	CacheRead        int // tokens read from cache (billed at 0.1x input rate)
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
		ptr.CacheCreation5m += u.CacheCreation5m
		ptr.CacheCreation1h += u.CacheCreation1h
		ptr.CacheRead += u.CacheRead
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
// Cache pricing:
//   - CacheCreation5m: 1.25x the input rate (5-minute TTL tier)
//   - CacheCreation1h: 2.0x the input rate (1-hour TTL tier)
//   - CacheRead: 0.1x the input rate
func CostUSD(model string, usage TokenUsage) float64 {
	inputPer1M, outputPer1M := modelPrices(model)
	cost := float64(usage.InputTokens)/1_000_000*inputPer1M +
		float64(usage.OutputTokens)/1_000_000*outputPer1M +
		float64(usage.CacheCreation5m)/1_000_000*inputPer1M*1.25 +
		float64(usage.CacheCreation1h)/1_000_000*inputPer1M*2.0 +
		float64(usage.CacheRead)/1_000_000*inputPer1M*0.1
	return cost
}

// modelPrices returns (inputPer1M, outputPer1M) USD for a model name.
// Falls back to opus-4-6 pricing for unknown models.
func modelPrices(model string) (inputPer1M, outputPer1M float64) {
	switch {
	// Opus 4 — newer models (4-6, 4-5) at reduced price
	case strings.HasPrefix(model, "claude-opus-4-6"),
		strings.HasPrefix(model, "claude-opus-4-5"):
		return 5.00, 25.00
	// Opus 4 — older models (4-1, 4-0) at original price
	case strings.HasPrefix(model, "claude-opus-4-1"),
		strings.HasPrefix(model, "claude-opus-4-0"),
		strings.HasPrefix(model, "claude-opus-4"):
		return 15.00, 75.00
	case strings.HasPrefix(model, "claude-haiku-4-5"):
		return 1.00, 5.00
	case strings.HasPrefix(model, "claude-haiku-4"):
		return 1.00, 5.00
	case strings.HasPrefix(model, "claude-sonnet-4"):
		return 3.00, 15.00
	case strings.HasPrefix(model, "claude-3-5-haiku"):
		return 0.80, 4.00
	case strings.HasPrefix(model, "claude-3-5-sonnet"):
		return 3.00, 15.00
	case strings.HasPrefix(model, "claude-3-opus"):
		return 15.00, 75.00
	default:
		return 5.00, 25.00 // conservative default: opus-4-6 pricing
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
	t.total.CacheCreation5m += u.CacheCreation5m
	t.total.CacheCreation1h += u.CacheCreation1h
	t.total.CacheRead += u.CacheRead
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
