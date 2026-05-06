package engine

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/auth"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/engine/apiloop"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/tools"
)

// ---------------------------------------------------------------------------
// Test scaffolding (OpenAI)
// ---------------------------------------------------------------------------

func newOpenAITestExecutor(t *testing.T, srv *httptest.Server, reg *tools.Registry) *OpenAIAPIExecutor {
	t.Helper()
	exec := NewOpenAIAPIExecutor(reg)
	exec.BaseURL = strings.TrimRight(srv.URL, "/")
	exec.HTTPClient = srv.Client()
	exec.MaxTurns = 8
	exec.LoadCredential = func(string) (*auth.Credential, error) {
		return &auth.Credential{Provider: "openai", AuthType: auth.AuthTypeAPIKey, APIKey: "sk-test"}, nil
	}
	exec.Backoff = func(int) time.Duration { return time.Millisecond }
	return exec
}

// openaiTextStream emits a single chat.completion.chunk text response.
func openaiTextStream(text, model string) string {
	var b strings.Builder
	chunk1 := fmt.Sprintf(`{"id":"c1","object":"chat.completion.chunk","model":%q,"choices":[{"index":0,"delta":{"role":"assistant","content":%q},"finish_reason":null}]}`, model, text)
	chunk2 := fmt.Sprintf(`{"id":"c1","object":"chat.completion.chunk","model":%q,"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":12,"total_tokens":22}}`, model)
	b.WriteString("data: " + chunk1 + "\n\n")
	b.WriteString("data: " + chunk2 + "\n\n")
	b.WriteString("data: [DONE]\n\n")
	return b.String()
}

type openaiToolCallSpec struct {
	id    string
	name  string
	input string
}

// openaiToolCallStream emits a streaming tool_calls response.
func openaiToolCallStream(model string, calls ...openaiToolCallSpec) string {
	var b strings.Builder
	// First chunk: assistant role + first tool call header.
	for i, c := range calls {
		head := map[string]any{
			"id":     "c1",
			"object": "chat.completion.chunk",
			"model":  model,
			"choices": []map[string]any{{
				"index": 0,
				"delta": map[string]any{
					"role": "assistant",
					"tool_calls": []map[string]any{{
						"index": i,
						"id":    c.id,
						"type":  "function",
						"function": map[string]any{
							"name":      c.name,
							"arguments": "",
						},
					}},
				},
				"finish_reason": nil,
			}},
		}
		hb, _ := json.Marshal(head)
		b.WriteString("data: " + string(hb) + "\n\n")
		// Stream the arguments as a delta on the same index.
		argChunk := map[string]any{
			"id":     "c1",
			"object": "chat.completion.chunk",
			"model":  model,
			"choices": []map[string]any{{
				"index": 0,
				"delta": map[string]any{
					"tool_calls": []map[string]any{{
						"index":    i,
						"function": map[string]any{"arguments": c.input},
					}},
				},
				"finish_reason": nil,
			}},
		}
		ab, _ := json.Marshal(argChunk)
		b.WriteString("data: " + string(ab) + "\n\n")
	}
	finish := fmt.Sprintf(`{"id":"c1","object":"chat.completion.chunk","model":%q,"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":20,"completion_tokens":30,"total_tokens":50}}`, model)
	b.WriteString("data: " + finish + "\n\n")
	b.WriteString("data: [DONE]\n\n")
	return b.String()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

func TestOpenAIAPIExecutor_Describe(t *testing.T) {
	e := NewOpenAIAPIExecutor(nil)
	d := e.Describe()
	if d.Name != core.RuntimeOpenAIAPI {
		t.Fatalf("descriptor name = %q, want %q", d.Name, core.RuntimeOpenAIAPI)
	}
	for _, want := range []core.RuntimeCap{core.CapToolUse, core.CapStreaming, core.CapCostReport} {
		if !d.Caps.Has(want) {
			t.Errorf("descriptor missing cap %q", want)
		}
	}
}

func TestOpenAIAPIExecutor_TextOnlyResponse(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		writeStream(w, openaiTextStream("hello openai", "gpt-4o"))
	}))
	defer srv.Close()

	exec := newOpenAITestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)
	cfg.Model = "gpt-4o"

	out, _, cost, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if out != "hello openai" {
		t.Errorf("output = %q, want %q", out, "hello openai")
	}
	if cost == nil || cost.OutputTokens == 0 {
		t.Errorf("expected cost with output tokens, got %#v", cost)
	}
	if cost.TotalCostUSD <= 0 {
		t.Errorf("expected non-zero cost, got %v", cost.TotalCostUSD)
	}
}

func TestOpenAIAPIExecutor_SingleToolCall(t *testing.T) {
	calls := &atomic.Int32{}
	tool := &fakeTool{name: "echo", output: "echoed", calls: calls}

	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		body, _ := io.ReadAll(r.Body)
		switch turn.Add(1) {
		case 1:
			// Verify our request shape: tools array with type=function.
			if !strings.Contains(string(body), `"type":"function"`) {
				t.Errorf("turn 1: request missing function-type tools: %s", body)
			}
			writeStream(w, openaiToolCallStream("gpt-4o", openaiToolCallSpec{id: "call_1", name: "echo", input: "{}"}))
		default:
			// Subsequent turn must include the tool result as a "tool" role message.
			if !strings.Contains(string(body), `"role":"tool"`) {
				t.Errorf("turn 2: missing tool role message: %s", body)
			}
			writeStream(w, openaiTextStream("done", "gpt-4o"))
		}
	}))
	defer srv.Close()

	exec := newOpenAITestExecutor(t, srv, newFakeRegistry(tool))
	cfg := newTestConfig(t)
	cfg.Model = "gpt-4o"

	out, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if calls.Load() != 1 {
		t.Errorf("tool calls = %d, want 1", calls.Load())
	}
	if !strings.Contains(out, "done") {
		t.Errorf("output missing final text: %q", out)
	}
}

func TestOpenAIAPIExecutor_ParallelToolCalls(t *testing.T) {
	calls := &atomic.Int32{}
	t1 := &fakeTool{name: "slow_a", output: "A", delay: 50 * time.Millisecond, calls: calls}
	t2 := &fakeTool{name: "slow_b", output: "B", delay: 50 * time.Millisecond, calls: calls}

	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch turn.Add(1) {
		case 1:
			writeStream(w, openaiToolCallStream("gpt-4o",
				openaiToolCallSpec{id: "call_a", name: "slow_a", input: "{}"},
				openaiToolCallSpec{id: "call_b", name: "slow_b", input: "{}"},
			))
		default:
			writeStream(w, openaiTextStream("done", "gpt-4o"))
		}
	}))
	defer srv.Close()

	exec := newOpenAITestExecutor(t, srv, newFakeRegistry(t1, t2))
	cfg := newTestConfig(t)
	cfg.Model = "gpt-4o"

	start := time.Now()
	if _, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	elapsed := time.Since(start)

	if calls.Load() != 2 {
		t.Errorf("tool calls = %d, want 2", calls.Load())
	}
	if elapsed > 90*time.Millisecond {
		t.Errorf("parallel tools took %v, expected well under 100ms (sequential)", elapsed)
	}
}

func TestOpenAIAPIExecutor_RetryOn429(t *testing.T) {
	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if turn.Add(1) == 1 {
			w.Header().Set("Retry-After", "0")
			http.Error(w, `{"error":"rate_limited"}`, http.StatusTooManyRequests)
			return
		}
		writeStream(w, openaiTextStream("recovered", "gpt-4o"))
	}))
	defer srv.Close()

	exec := newOpenAITestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)
	cfg.Model = "gpt-4o"

	out, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if out != "recovered" {
		t.Errorf("output = %q, want recovered", out)
	}
	if turn.Load() != 2 {
		t.Errorf("expected 2 attempts, got %d", turn.Load())
	}
}

func TestOpenAIAPIExecutor_AuthHeader(t *testing.T) {
	var seen string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = r.Header.Get("Authorization")
		writeStream(w, openaiTextStream("ok", "gpt-4o"))
	}))
	defer srv.Close()

	exec := newOpenAITestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)
	cfg.Model = "gpt-4o"

	if _, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if seen != "Bearer sk-test" {
		t.Errorf("Authorization = %q, want %q", seen, "Bearer sk-test")
	}
}

func TestOpenAIAPIExecutor_ComputeCostUSD(t *testing.T) {
	p := &openaiProvider{pricing: defaultOpenAIPricing}
	u := &apiloop.Usage{InputTokens: 1_000_000, OutputTokens: 1_000_000}
	got := p.ComputeCostUSD("gpt-4o", u)
	want := 2.50 + 10.00
	if got != want {
		t.Errorf("gpt-4o cost = %v, want %v", got, want)
	}
	got = p.ComputeCostUSD("gpt-4o-mini", u)
	want = 0.15 + 0.60
	if got != want {
		t.Errorf("gpt-4o-mini cost = %v, want %v", got, want)
	}
	// Unknown model falls back to gpt-4o pricing so cost is non-zero.
	got = p.ComputeCostUSD("future-model-xyz", u)
	if got <= 0 {
		t.Errorf("expected non-zero fallback cost, got %v", got)
	}
}

func TestDefaultRegistryRegistersOpenAIAPI(t *testing.T) {
	r := defaultRegistry()
	if !r.has(core.RuntimeOpenAIAPI) {
		t.Error("defaultRegistry missing RuntimeOpenAIAPI")
	}
	desc, ok := r.describe(core.RuntimeOpenAIAPI)
	if !ok {
		t.Error("openai-api executor should expose a descriptor")
	}
	if desc.Name != core.RuntimeOpenAIAPI {
		t.Errorf("descriptor name = %q, want %q", desc.Name, core.RuntimeOpenAIAPI)
	}
}
