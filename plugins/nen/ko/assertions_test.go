package ko

import (
	"context"
	"testing"
)

func TestAssertContains(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		value    string
		wantPass bool
	}{
		{
			name:     "substring found",
			output:   "hello world",
			value:    "world",
			wantPass: true,
		},
		{
			name:     "substring not found",
			output:   "hello world",
			value:    "goodbye",
			wantPass: false,
		},
		{
			name:     "empty value matches",
			output:   "hello",
			value:    "",
			wantPass: true,
		},
		{
			name:     "case sensitive",
			output:   "Hello World",
			value:    "world",
			wantPass: false,
		},
		{
			name:     "multiline contains",
			output:   "line 1\nline 2\nline 3",
			value:    "line 2",
			wantPass: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertContains(tt.output, tt.value)
			if got != tt.wantPass {
				t.Errorf("AssertContains() = %v, wantPass %v; message: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertNotContains(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		value    string
		wantPass bool
	}{
		{
			name:     "substring not found",
			output:   "hello world",
			value:    "goodbye",
			wantPass: true,
		},
		{
			name:     "substring found",
			output:   "hello world",
			value:    "world",
			wantPass: false,
		},
		{
			name:     "case sensitive different case",
			output:   "Hello World",
			value:    "hello",
			wantPass: true, // "Hello World" does NOT contain lowercase "hello"
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertNotContains(tt.output, tt.value)
			if got != tt.wantPass {
				t.Errorf("AssertNotContains() = %v, wantPass %v; message: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertEquals(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		value    string
		wantPass bool
	}{
		{
			name:     "exact match",
			output:   "hello",
			value:    "hello",
			wantPass: true,
		},
		{
			name:     "trimmed match",
			output:   "  hello  \n",
			value:    "hello",
			wantPass: true,
		},
		{
			name:     "no match",
			output:   "hello",
			value:    "world",
			wantPass: false,
		},
		{
			name:     "case sensitive",
			output:   "Hello",
			value:    "hello",
			wantPass: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertEquals(tt.output, tt.value)
			if got != tt.wantPass {
				t.Errorf("AssertEquals() = %v, wantPass %v; message: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertMatches(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		pattern  string
		wantPass bool
		wantErr  bool
	}{
		{
			name:     "simple pattern match",
			output:   "hello123world",
			pattern:  `\d+`,
			wantPass: true,
		},
		{
			name:     "pattern no match",
			output:   "hello world",
			pattern:  `\d+`,
			wantPass: false,
		},
		{
			name:     "full match",
			output:   "hello",
			pattern:  `^hello$`,
			wantPass: true,
		},
		{
			name:     "partial match",
			output:   "hello world",
			pattern:  `hello`,
			wantPass: true,
		},
		{
			name:     "invalid regex",
			output:   "test",
			pattern:  `[invalid`,
			wantErr:  true,
			wantPass: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertMatches(tt.output, tt.pattern)
			if tt.wantErr {
				if got != false {
					t.Errorf("AssertMatches() expected error, got pass; message: %s", msg)
				}
			} else if got != tt.wantPass {
				t.Errorf("AssertMatches() = %v, wantPass %v; message: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertIsJSON(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		wantPass bool
	}{
		{
			name:     "valid json object",
			output:   `{"key": "value"}`,
			wantPass: true,
		},
		{
			name:     "valid json array",
			output:   `[1, 2, 3]`,
			wantPass: true,
		},
		{
			name:     "valid json string",
			output:   `"hello"`,
			wantPass: true,
		},
		{
			name:     "valid json number",
			output:   `42`,
			wantPass: true,
		},
		{
			name:     "valid json with whitespace",
			output:   "  \n  {\"key\": \"value\"}  \n  ",
			wantPass: true,
		},
		{
			name:     "invalid json",
			output:   `{key: "value"}`,
			wantPass: false,
		},
		{
			name:     "empty string",
			output:   "",
			wantPass: false,
		},
		{
			name:     "only whitespace",
			output:   "   \n   ",
			wantPass: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertIsJSON(tt.output)
			if got != tt.wantPass {
				t.Errorf("AssertIsJSON() = %v, wantPass %v; message: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertLength(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		criteria string
		wantPass bool
		wantErr  bool
	}{
		{
			name:     "min length pass",
			output:   "hello world",
			criteria: "min:5",
			wantPass: true,
		},
		{
			name:     "min length fail",
			output:   "hi",
			criteria: "min:5",
			wantPass: false,
		},
		{
			name:     "max length pass",
			output:   "hello",
			criteria: "max:10",
			wantPass: true,
		},
		{
			name:     "max length fail",
			output:   "hello world is too long",
			criteria: "max:10",
			wantPass: false,
		},
		{
			name:     "range pass",
			output:   "hello",
			criteria: "range:3,10",
			wantPass: true,
		},
		{
			name:     "range fail low",
			output:   "hi",
			criteria: "range:3,10",
			wantPass: false,
		},
		{
			name:     "range fail high",
			output:   "this is way too long string",
			criteria: "range:3,10",
			wantPass: false,
		},
		{
			name:     "equals pass",
			output:   "hello",
			criteria: "equals:5",
			wantPass: true,
		},
		{
			name:     "equals fail",
			output:   "hello",
			criteria: "equals:10",
			wantPass: false,
		},
		{
			name:     "invalid format",
			output:   "test",
			criteria: "invalid",
			wantErr:  true,
			wantPass: false,
		},
		{
			name:     "invalid min value",
			output:   "test",
			criteria: "min:abc",
			wantErr:  true,
			wantPass: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertLength(tt.output, tt.criteria)
			if tt.wantErr {
				if got != false {
					t.Errorf("AssertLength() expected error, got pass; message: %s", msg)
				}
			} else if got != tt.wantPass {
				t.Errorf("AssertLength() = %v, wantPass %v; message: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertStartsWith(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		value    string
		wantPass bool
	}{
		{"matches prefix", "hello world", "hello", true},
		{"no match", "hello world", "world", false},
		{"empty prefix", "hello", "", true},
		{"exact match", "hello", "hello", true},
		{"longer prefix", "hi", "hello", false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertStartsWith(tt.output, tt.value)
			if got != tt.wantPass {
				t.Errorf("AssertStartsWith() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertEndsWith(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		value    string
		wantPass bool
	}{
		{"matches suffix", "hello world", "world", true},
		{"no match", "hello world", "hello", false},
		{"empty suffix", "hello", "", true},
		{"exact match", "hello", "hello", true},
		{"longer suffix", "hi", "world", false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertEndsWith(tt.output, tt.value)
			if got != tt.wantPass {
				t.Errorf("AssertEndsWith() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertContainsAll(t *testing.T) {
	tests := []struct {
		name      string
		output    string
		valueJSON string
		wantPass  bool
		wantErr   bool
	}{
		{"all present", "foo bar baz", `["foo","bar","baz"]`, true, false},
		{"one missing", "foo bar", `["foo","bar","baz"]`, false, false},
		{"empty list", "foo", `[]`, true, false},
		{"invalid json", "foo", `not-json`, false, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertContainsAll(tt.output, tt.valueJSON)
			if tt.wantErr {
				if got {
					t.Errorf("AssertContainsAll() expected error, got pass")
				}
			} else if got != tt.wantPass {
				t.Errorf("AssertContainsAll() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertContainsAny(t *testing.T) {
	tests := []struct {
		name      string
		output    string
		valueJSON string
		wantPass  bool
		wantErr   bool
	}{
		{"first matches", "foo bar", `["foo","baz"]`, true, false},
		{"second matches", "bar baz", `["foo","baz"]`, true, false},
		{"none match", "qux", `["foo","bar"]`, false, false},
		{"empty list", "foo", `[]`, false, false},
		{"invalid json", "foo", `not-json`, false, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertContainsAny(tt.output, tt.valueJSON)
			if tt.wantErr {
				if got {
					t.Errorf("AssertContainsAny() expected error, got pass")
				}
			} else if got != tt.wantPass {
				t.Errorf("AssertContainsAny() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertMaxLength(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		maxLen   int
		wantPass bool
	}{
		{"under limit", "hello", 10, true},
		{"at limit", "hello", 5, true},
		{"over limit", "hello world", 5, false},
		{"empty under limit", "", 1, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertMaxLength(tt.output, tt.maxLen)
			if got != tt.wantPass {
				t.Errorf("AssertMaxLength() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertMinLength(t *testing.T) {
	tests := []struct {
		name     string
		output   string
		minLen   int
		wantPass bool
	}{
		{"over minimum", "hello world", 5, true},
		{"at minimum", "hello", 5, true},
		{"under minimum", "hi", 5, false},
		{"empty under minimum", "", 1, false},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertMinLength(tt.output, tt.minLen)
			if got != tt.wantPass {
				t.Errorf("AssertMinLength() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertCost(t *testing.T) {
	tests := []struct {
		name      string
		costUSD   float64
		threshold float64
		wantPass  bool
	}{
		{"under threshold", 0.001, 0.01, true},
		{"at threshold", 0.01, 0.01, true},
		{"over threshold", 0.02, 0.01, false},
		{"zero cost", 0, 0.01, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertCost(tt.costUSD, tt.threshold)
			if got != tt.wantPass {
				t.Errorf("AssertCost() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertLatency(t *testing.T) {
	tests := []struct {
		name        string
		latencyMs   int64
		thresholdMs float64
		wantPass    bool
	}{
		{"under threshold", 100, 500, true},
		{"at threshold", 500, 500, true},
		{"over threshold", 600, 500, false},
		{"zero latency", 0, 100, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertLatency(tt.latencyMs, tt.thresholdMs)
			if got != tt.wantPass {
				t.Errorf("AssertLatency() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertNot(t *testing.T) {
	noMeta := AssertionMeta{}
	tests := []struct {
		name      string
		assertion AssertionConfig
		output    string
		wantPass  bool
	}{
		{
			name: "inverts passing assertion",
			assertion: AssertionConfig{
				Type: "not",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "error"},
				},
			},
			output:   "everything is fine",
			wantPass: true,
		},
		{
			name: "inverts failing assertion",
			assertion: AssertionConfig{
				Type: "not",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "error"},
				},
			},
			output:   "an error occurred",
			wantPass: false,
		},
		{
			name:      "no sub-assertion returns error",
			assertion: AssertionConfig{Type: "not"},
			output:    "anything",
			wantPass:  false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertNot(context.Background(), tt.assertion, tt.output, noMeta)
			if got != tt.wantPass {
				t.Errorf("AssertNot() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertAll(t *testing.T) {
	noMeta := AssertionMeta{}
	tests := []struct {
		name      string
		assertion AssertionConfig
		output    string
		wantPass  bool
	}{
		{
			name: "all pass",
			assertion: AssertionConfig{
				Type: "assert-all",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo"},
					{Type: "contains", Value: "bar"},
				},
			},
			output:   "foo and bar",
			wantPass: true,
		},
		{
			name: "one fails",
			assertion: AssertionConfig{
				Type: "assert-all",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo"},
					{Type: "contains", Value: "baz"},
				},
			},
			output:   "foo and bar",
			wantPass: false,
		},
		{
			name:      "empty sub-assertions",
			assertion: AssertionConfig{Type: "assert-all"},
			output:    "anything",
			wantPass:  false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertAll(context.Background(), tt.assertion, tt.output, noMeta)
			if got != tt.wantPass {
				t.Errorf("AssertAll() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertAny(t *testing.T) {
	noMeta := AssertionMeta{}
	tests := []struct {
		name      string
		assertion AssertionConfig
		output    string
		wantPass  bool
	}{
		{
			name: "first passes",
			assertion: AssertionConfig{
				Type: "assert-any",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo"},
					{Type: "contains", Value: "baz"},
				},
			},
			output:   "foo and bar",
			wantPass: true,
		},
		{
			name: "none pass",
			assertion: AssertionConfig{
				Type: "assert-any",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "baz"},
					{Type: "contains", Value: "qux"},
				},
			},
			output:   "foo and bar",
			wantPass: false,
		},
		{
			name:      "empty sub-assertions",
			assertion: AssertionConfig{Type: "assert-any"},
			output:    "anything",
			wantPass:  false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertAny(context.Background(), tt.assertion, tt.output, noMeta)
			if got != tt.wantPass {
				t.Errorf("AssertAny() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestAssertWeighted(t *testing.T) {
	noMeta := AssertionMeta{}
	tests := []struct {
		name      string
		assertion AssertionConfig
		output    string
		wantPass  bool
	}{
		{
			name: "all pass exceeds default threshold",
			assertion: AssertionConfig{
				Type: "weighted",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo", Weight: 1.0},
					{Type: "contains", Value: "bar", Weight: 1.0},
				},
			},
			output:   "foo and bar",
			wantPass: true,
		},
		{
			name: "half pass meets default threshold",
			assertion: AssertionConfig{
				Type: "weighted",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo", Weight: 1.0},
					{Type: "contains", Value: "baz", Weight: 1.0},
				},
			},
			output:   "foo and bar",
			wantPass: true, // 0.5 >= 0.5 default threshold
		},
		{
			name: "none pass fails",
			assertion: AssertionConfig{
				Type: "weighted",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "baz", Weight: 1.0},
					{Type: "contains", Value: "qux", Weight: 1.0},
				},
			},
			output:   "foo and bar",
			wantPass: false,
		},
		{
			name: "custom threshold respected",
			assertion: AssertionConfig{
				Type:      "weighted",
				Threshold: 0.8,
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo", Weight: 1.0},
					{Type: "contains", Value: "baz", Weight: 1.0},
				},
			},
			output:   "foo and bar",
			wantPass: false, // 0.5 < 0.8
		},
		{
			name: "unequal weights favour heavy pass",
			assertion: AssertionConfig{
				Type:      "weighted",
				Threshold: 0.6,
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo", Weight: 0.9},
					{Type: "contains", Value: "baz", Weight: 0.1},
				},
			},
			output:   "foo and bar", // only foo passes: 0.9/1.0 = 0.9
			wantPass: true,
		},
		{
			name:      "empty sub-assertions",
			assertion: AssertionConfig{Type: "weighted"},
			output:    "anything",
			wantPass:  false,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, msg := AssertWeighted(context.Background(), tt.assertion, tt.output, noMeta)
			if got != tt.wantPass {
				t.Errorf("AssertWeighted() = %v, want %v; msg: %s", got, tt.wantPass, msg)
			}
		})
	}
}

func TestRunAssertion(t *testing.T) {
	noMeta := AssertionMeta{}
	tests := []struct {
		name       string
		assertion  AssertionConfig
		output     string
		meta       AssertionMeta
		wantPass   bool
		assertType string
	}{
		{
			name:       "contains assertion",
			assertion:  AssertionConfig{Type: "contains", Value: "world"},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   true,
			assertType: "contains",
		},
		{
			name:       "regex assertion",
			assertion:  AssertionConfig{Type: "regex", Value: `\d+`},
			output:     "test 123",
			meta:       noMeta,
			wantPass:   true,
			assertType: "regex",
		},
		{
			name:       "json assertion",
			assertion:  AssertionConfig{Type: "is-json", Value: ""},
			output:     `{"key": "value"}`,
			meta:       noMeta,
			wantPass:   true,
			assertType: "is-json",
		},
		{
			name:       "starts-with pass",
			assertion:  AssertionConfig{Type: "starts-with", Value: "hello"},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   true,
			assertType: "starts-with",
		},
		{
			name:       "ends-with pass",
			assertion:  AssertionConfig{Type: "ends-with", Value: "world"},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   true,
			assertType: "ends-with",
		},
		{
			name:       "contains-all pass",
			assertion:  AssertionConfig{Type: "contains-all", Value: `["hello","world"]`},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   true,
			assertType: "contains-all",
		},
		{
			name:       "contains-any pass",
			assertion:  AssertionConfig{Type: "contains-any", Value: `["hello","nope"]`},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   true,
			assertType: "contains-any",
		},
		{
			name:       "max-length pass",
			assertion:  AssertionConfig{Type: "max-length", Value: "20"},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   true,
			assertType: "max-length",
		},
		{
			name:       "max-length fail",
			assertion:  AssertionConfig{Type: "max-length", Value: "5"},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   false,
			assertType: "max-length",
		},
		{
			name:       "min-length pass",
			assertion:  AssertionConfig{Type: "min-length", Value: "5"},
			output:     "hello world",
			meta:       noMeta,
			wantPass:   true,
			assertType: "min-length",
		},
		{
			name:       "min-length fail",
			assertion:  AssertionConfig{Type: "min-length", Value: "20"},
			output:     "hello",
			meta:       noMeta,
			wantPass:   false,
			assertType: "min-length",
		},
		{
			name:      "cost pass",
			assertion: AssertionConfig{Type: "cost", Threshold: 0.05},
			output:    "anything",
			meta:      AssertionMeta{CostUSD: 0.01},
			wantPass:  true,
			assertType: "cost",
		},
		{
			name:      "cost fail",
			assertion: AssertionConfig{Type: "cost", Threshold: 0.005},
			output:    "anything",
			meta:      AssertionMeta{CostUSD: 0.01},
			wantPass:  false,
			assertType: "cost",
		},
		{
			name:      "latency pass",
			assertion: AssertionConfig{Type: "latency", Threshold: 1000},
			output:    "anything",
			meta:      AssertionMeta{LatencyMs: 200},
			wantPass:  true,
			assertType: "latency",
		},
		{
			name:      "latency fail",
			assertion: AssertionConfig{Type: "latency", Threshold: 100},
			output:    "anything",
			meta:      AssertionMeta{LatencyMs: 500},
			wantPass:  false,
			assertType: "latency",
		},
		{
			name: "not pass",
			assertion: AssertionConfig{
				Type:   "not",
				Assert: []AssertionConfig{{Type: "contains", Value: "error"}},
			},
			output:     "all good",
			meta:       noMeta,
			wantPass:   true,
			assertType: "not",
		},
		{
			name: "assert-all pass",
			assertion: AssertionConfig{
				Type: "assert-all",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo"},
					{Type: "contains", Value: "bar"},
				},
			},
			output:     "foo bar",
			meta:       noMeta,
			wantPass:   true,
			assertType: "assert-all",
		},
		{
			name: "assert-any pass",
			assertion: AssertionConfig{
				Type: "assert-any",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo"},
					{Type: "contains", Value: "baz"},
				},
			},
			output:     "foo bar",
			meta:       noMeta,
			wantPass:   true,
			assertType: "assert-any",
		},
		{
			name: "weighted pass",
			assertion: AssertionConfig{
				Type: "weighted",
				Assert: []AssertionConfig{
					{Type: "contains", Value: "foo", Weight: 1.0},
					{Type: "contains", Value: "bar", Weight: 1.0},
				},
			},
			output:     "foo bar",
			meta:       noMeta,
			wantPass:   true,
			assertType: "weighted",
		},
		{
			name:       "unknown assertion type",
			assertion:  AssertionConfig{Type: "unknown", Value: "test"},
			output:     "output",
			meta:       noMeta,
			wantPass:   false,
			assertType: "unknown",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := RunAssertion(context.Background(), tt.assertion, tt.output, tt.meta)
			if result.Passed != tt.wantPass {
				t.Errorf("RunAssertion() passed = %v, want %v; message: %s", result.Passed, tt.wantPass, result.Message)
			}
			if result.Type != tt.assertType {
				t.Errorf("RunAssertion() type = %s, want %s", result.Type, tt.assertType)
			}
		})
	}
}
