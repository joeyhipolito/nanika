package ko

import (
	"context"
	"testing"
)

func TestModelPrices(t *testing.T) {
	tests := []struct {
		model         string
		wantInput     float64
		wantOutput    float64
	}{
		// Opus 4 — newer (4-6, 4-5)
		{"claude-opus-4-6", 5.00, 25.00},
		{"claude-opus-4-5", 5.00, 25.00},
		{"claude-opus-4-5-20251201", 5.00, 25.00},
		// Opus 4 — older (4-1, 4-0)
		{"claude-opus-4-1", 15.00, 75.00},
		{"claude-opus-4-0", 15.00, 75.00},
		// Opus 4 unknown minor (fallback to older pricing via "claude-opus-4" prefix)
		{"claude-opus-4-9", 15.00, 75.00},
		// Sonnet 4
		{"claude-sonnet-4-6", 3.00, 15.00},
		// Haiku 4.5
		{"claude-haiku-4-5", 1.00, 5.00},
		{"claude-haiku-4-5-20251001", 1.00, 5.00},
		// Legacy 3.x
		{"claude-3-5-haiku-20241022", 0.80, 4.00},
		{"claude-3-5-sonnet-20241022", 3.00, 15.00},
		{"claude-3-opus-20240229", 15.00, 75.00},
		// Unknown → opus-4-6 pricing
		{"claude-unknown-model", 5.00, 25.00},
	}

	for _, tc := range tests {
		gotIn, gotOut := modelPrices(tc.model)
		if gotIn != tc.wantInput || gotOut != tc.wantOutput {
			t.Errorf("modelPrices(%q) = (%.2f, %.2f), want (%.2f, %.2f)",
				tc.model, gotIn, gotOut, tc.wantInput, tc.wantOutput)
		}
	}
}

func TestCostUSD_BasicTokens(t *testing.T) {
	tests := []struct {
		name     string
		model    string
		usage    TokenUsage
		wantCost float64
	}{
		{
			name:  "opus-4-6 1M input + 1M output",
			model: "claude-opus-4-6",
			usage: TokenUsage{InputTokens: 1_000_000, OutputTokens: 1_000_000},
			// $5 + $25 = $30
			wantCost: 30.00,
		},
		{
			name:  "opus-4-1 1M input + 1M output",
			model: "claude-opus-4-1",
			usage: TokenUsage{InputTokens: 1_000_000, OutputTokens: 1_000_000},
			// $15 + $75 = $90
			wantCost: 90.00,
		},
		{
			name:  "haiku-4-5 1M input + 1M output",
			model: "claude-haiku-4-5",
			usage: TokenUsage{InputTokens: 1_000_000, OutputTokens: 1_000_000},
			// $1 + $5 = $6
			wantCost: 6.00,
		},
		{
			name:  "sonnet-4 500k input + 200k output",
			model: "claude-sonnet-4-6",
			usage: TokenUsage{InputTokens: 500_000, OutputTokens: 200_000},
			// 0.5*$3 + 0.2*$15 = $1.50 + $3.00 = $4.50
			wantCost: 4.50,
		},
		{
			name:  "zero tokens",
			model: "claude-opus-4-6",
			usage: TokenUsage{},
			wantCost: 0.00,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := CostUSD(tc.model, tc.usage)
			if abs(got-tc.wantCost) > 1e-9 {
				t.Errorf("CostUSD(%q, ...) = %.9f, want %.9f", tc.model, got, tc.wantCost)
			}
		})
	}
}

func TestCostUSD_CachePricing(t *testing.T) {
	// Using opus-4-6: input=$5/1M, output=$25/1M
	// cache_creation_5m = 1.25 * $5 = $6.25/1M
	// cache_creation_1h = 2.0  * $5 = $10.00/1M
	// cache_read         = 0.1  * $5 = $0.50/1M
	model := "claude-opus-4-6"

	tests := []struct {
		name     string
		usage    TokenUsage
		wantCost float64
	}{
		{
			name:  "1M cache_creation_5m only",
			usage: TokenUsage{CacheCreation5m: 1_000_000},
			// 1M * $5 * 1.25 = $6.25
			wantCost: 6.25,
		},
		{
			name:  "1M cache_creation_1h only",
			usage: TokenUsage{CacheCreation1h: 1_000_000},
			// 1M * $5 * 2.0 = $10.00
			wantCost: 10.00,
		},
		{
			name:  "1M cache_read only",
			usage: TokenUsage{CacheRead: 1_000_000},
			// 1M * $5 * 0.1 = $0.50
			wantCost: 0.50,
		},
		{
			name: "combined: 100k input + 50k output + 200k cache_creation_5m + 300k cache_read",
			usage: TokenUsage{
				InputTokens:     100_000,
				OutputTokens:    50_000,
				CacheCreation5m: 200_000,
				CacheRead:       300_000,
			},
			// input:    0.1M * $5     = $0.50
			// output:   0.05M * $25   = $1.25
			// cc5m:     0.2M * $6.25  = $1.25
			// cread:    0.3M * $0.50  = $0.15
			// total: $3.15
			wantCost: 3.15,
		},
		{
			name: "all cache types 1M each",
			usage: TokenUsage{
				CacheCreation5m: 1_000_000,
				CacheCreation1h: 1_000_000,
				CacheRead:       1_000_000,
			},
			// $6.25 + $10.00 + $0.50 = $16.75
			wantCost: 16.75,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := CostUSD(model, tc.usage)
			if abs(got-tc.wantCost) > 1e-9 {
				t.Errorf("CostUSD(%q, ...) = %.9f, want %.9f", model, got, tc.wantCost)
			}
		})
	}
}

func TestRecordUsage_AccumulatesCacheFields(t *testing.T) {
	ctx, usage := WithUsageRecorder(context.Background())

	RecordUsage(ctx, TokenUsage{
		InputTokens:     100,
		OutputTokens:    50,
		CacheCreation5m: 200,
		CacheCreation1h: 300,
		CacheRead:       400,
	})
	RecordUsage(ctx, TokenUsage{
		InputTokens:     10,
		OutputTokens:    5,
		CacheCreation5m: 20,
		CacheCreation1h: 30,
		CacheRead:       40,
	})

	if usage.InputTokens != 110 {
		t.Errorf("InputTokens = %d, want 110", usage.InputTokens)
	}
	if usage.OutputTokens != 55 {
		t.Errorf("OutputTokens = %d, want 55", usage.OutputTokens)
	}
	if usage.CacheCreation5m != 220 {
		t.Errorf("CacheCreation5m = %d, want 220", usage.CacheCreation5m)
	}
	if usage.CacheCreation1h != 330 {
		t.Errorf("CacheCreation1h = %d, want 330", usage.CacheCreation1h)
	}
	if usage.CacheRead != 440 {
		t.Errorf("CacheRead = %d, want 440", usage.CacheRead)
	}
}

func TestCostTracker_Record(t *testing.T) {
	var tracker CostTracker

	tracker.Record(TokenUsage{
		InputTokens:     1_000_000,
		OutputTokens:    500_000,
		CacheCreation5m: 200_000,
		CacheCreation1h: 100_000,
		CacheRead:       300_000,
	})
	tracker.Record(TokenUsage{
		InputTokens: 500_000,
		OutputTokens: 250_000,
	})

	total := tracker.Total()
	if total.InputTokens != 1_500_000 {
		t.Errorf("InputTokens = %d, want 1_500_000", total.InputTokens)
	}
	if total.OutputTokens != 750_000 {
		t.Errorf("OutputTokens = %d, want 750_000", total.OutputTokens)
	}
	if total.CacheCreation5m != 200_000 {
		t.Errorf("CacheCreation5m = %d, want 200_000", total.CacheCreation5m)
	}
	if total.CacheCreation1h != 100_000 {
		t.Errorf("CacheCreation1h = %d, want 100_000", total.CacheCreation1h)
	}
	if total.CacheRead != 300_000 {
		t.Errorf("CacheRead = %d, want 300_000", total.CacheRead)
	}

	// opus-4-6: $5/$25 input/output
	// 1.5M in * $5 = $7.50
	// 0.75M out * $25 = $18.75
	// 0.2M cc5m * $6.25 = $1.25
	// 0.1M cc1h * $10 = $1.00
	// 0.3M cread * $0.50 = $0.15
	// total = $28.65
	wantCost := 28.65
	got := CostUSD("claude-opus-4-6", total)
	if abs(got-wantCost) > 1e-9 {
		t.Errorf("CostUSD total = %.9f, want %.9f", got, wantCost)
	}
}

func abs(x float64) float64 {
	if x < 0 {
		return -x
	}
	return x
}
