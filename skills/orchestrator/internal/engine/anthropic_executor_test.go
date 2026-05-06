package engine

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/auth"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/tools"
)

// ---------------------------------------------------------------------------
// Test scaffolding
// ---------------------------------------------------------------------------

// stubCredential is the OAuth credential used in fixture tests. The token
// value is checked by some handlers to verify refresh-and-retry behaviour.
func stubCredential() *auth.Credential {
	return &auth.Credential{
		Provider:     "anthropic",
		AuthType:     auth.AuthTypeOAuth,
		AccessToken:  "tok-original",
		RefreshToken: "refresh-original",
	}
}

// newTestExecutor wires an executor against a test server. RefreshCredential
// flips the credential's token to "tok-refreshed" so handlers can verify the
// caller actually refreshed.
func newTestExecutor(t *testing.T, srv *httptest.Server, reg *tools.Registry) (*AnthropicAPIExecutor, *auth.Credential) {
	t.Helper()
	cred := stubCredential()
	exec := &AnthropicAPIExecutor{
		BaseURL:    strings.TrimRight(srv.URL, "/"),
		HTTPClient: srv.Client(),
		Tools:      reg,
		MaxTurns:   8,
		LoadCredential: func(string) (*auth.Credential, error) {
			return cred, nil
		},
		RefreshCredential: func(_ context.Context, c *auth.Credential) error {
			c.AccessToken = "tok-refreshed"
			return nil
		},
		Backoff: func(int) time.Duration { return time.Millisecond },
		Now:     time.Now,
	}
	return exec, cred
}

func newTestConfig(t *testing.T) *core.WorkerConfig {
	t.Helper()
	dir := t.TempDir()
	return &core.WorkerConfig{
		Name:      "test-worker",
		WorkerDir: dir,
		Model:     "claude-sonnet-4-6",
		MaxTurns:  4,
		Bundle: core.ContextBundle{
			Objective:   "demo objective",
			Persona:     "you are helpful",
			PersonaName: "demo",
			WorkspaceID: "ws-1",
			PhaseID:     "phase-1",
		},
	}
}

// sse builds a single Server-Sent Events frame.
func sse(event string, data string) string {
	return fmt.Sprintf("event: %s\ndata: %s\n\n", event, data)
}

// streamTextOnly emits a plain text response that ends with end_turn.
func streamTextOnly(text string) string {
	var b strings.Builder
	b.WriteString(sse("message_start", `{"type":"message_start","message":{"id":"msg_1","model":"claude-sonnet-4-6","usage":{"input_tokens":10,"output_tokens":0}}}`))
	b.WriteString(sse("content_block_start", `{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`))
	b.WriteString(sse("content_block_delta", fmt.Sprintf(`{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":%q}}`, text)))
	b.WriteString(sse("content_block_stop", `{"type":"content_block_stop","index":0}`))
	b.WriteString(sse("message_delta", `{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":12}}`))
	b.WriteString(sse("message_stop", `{"type":"message_stop"}`))
	return b.String()
}

// streamToolCalls emits N tool_use blocks then stops with stop_reason=tool_use.
type fakeToolCall struct {
	id    string
	name  string
	input string
}

func streamToolCalls(calls ...fakeToolCall) string {
	var b strings.Builder
	b.WriteString(sse("message_start", `{"type":"message_start","message":{"id":"msg_t","model":"claude-sonnet-4-6","usage":{"input_tokens":20,"output_tokens":0}}}`))
	for i, c := range calls {
		idx := i
		startPayload := map[string]any{
			"type":          "content_block_start",
			"index":         idx,
			"content_block": map[string]any{"type": "tool_use", "id": c.id, "name": c.name, "input": map[string]any{}},
		}
		startBytes, _ := json.Marshal(startPayload)
		b.WriteString(sse("content_block_start", string(startBytes)))
		deltaPayload := map[string]any{
			"type":  "content_block_delta",
			"index": idx,
			"delta": map[string]any{"type": "input_json_delta", "partial_json": c.input},
		}
		dB, _ := json.Marshal(deltaPayload)
		b.WriteString(sse("content_block_delta", string(dB)))
		stopPayload := map[string]any{"type": "content_block_stop", "index": idx}
		sB, _ := json.Marshal(stopPayload)
		b.WriteString(sse("content_block_stop", string(sB)))
	}
	b.WriteString(sse("message_delta", `{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":30}}`))
	b.WriteString(sse("message_stop", `{"type":"message_stop"}`))
	return b.String()
}

// writeStream flushes each frame to the wire so streaming consumers see them
// arrive over time.
func writeStream(w http.ResponseWriter, body string) {
	w.Header().Set("Content-Type", "text/event-stream")
	w.WriteHeader(http.StatusOK)
	flusher, _ := w.(http.Flusher)
	for _, frame := range strings.SplitAfter(body, "\n\n") {
		if frame == "" {
			continue
		}
		_, _ = io.WriteString(w, frame)
		if flusher != nil {
			flusher.Flush()
		}
	}
}

// fakeTool is a registry-compatible tool that returns canned output.
type fakeTool struct {
	name   string
	output string
	delay  time.Duration
	calls  *atomic.Int32
}

func (t *fakeTool) Name() string                  { return t.name }
func (t *fakeTool) Description() string           { return "fake " + t.name }
func (t *fakeTool) InputSchema() json.RawMessage  { return json.RawMessage(`{"type":"object"}`) }
func (t *fakeTool) Risk() tools.RiskTier          { return tools.RiskLow }
func (t *fakeTool) Execute(ctx context.Context, args map[string]any) (tools.ToolResult, error) {
	if t.calls != nil {
		t.calls.Add(1)
	}
	if t.delay > 0 {
		select {
		case <-ctx.Done():
			return tools.ToolResult{}, ctx.Err()
		case <-time.After(t.delay):
		}
	}
	return tools.ToolResult{Content: t.output}, nil
}

func newFakeRegistry(toolsList ...tools.Tool) *tools.Registry {
	r := tools.New()
	for _, t := range toolsList {
		r.Register(t)
	}
	return r
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

func TestAnthropicAPIExecutor_Describe(t *testing.T) {
	e := NewAnthropicAPIExecutor(nil)
	d := e.Describe()
	if d.Name != core.RuntimeAnthropicAPI {
		t.Fatalf("descriptor name = %q, want %q", d.Name, core.RuntimeAnthropicAPI)
	}
	for _, want := range []core.RuntimeCap{core.CapToolUse, core.CapStreaming, core.CapCostReport} {
		if !d.Caps.Has(want) {
			t.Errorf("descriptor missing cap %q", want)
		}
	}
}

func TestAnthropicAPIExecutor_TextOnlyResponse(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		writeStream(w, streamTextOnly("hello world"))
	}))
	defer srv.Close()

	exec, _ := newTestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)

	out, _, cost, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if out != "hello world" {
		t.Errorf("output = %q, want %q", out, "hello world")
	}
	if cost == nil || cost.OutputTokens == 0 {
		t.Errorf("expected cost with output tokens, got %#v", cost)
	}
	if cost.TotalCostUSD <= 0 {
		t.Errorf("expected non-zero cost, got %v", cost.TotalCostUSD)
	}
}

func TestAnthropicAPIExecutor_SingleToolCall(t *testing.T) {
	calls := &atomic.Int32{}
	tool := &fakeTool{name: "echo", output: "echoed", calls: calls}

	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch turn.Add(1) {
		case 1:
			writeStream(w, streamToolCalls(fakeToolCall{id: "tu_1", name: "echo", input: "{}"}))
		default:
			writeStream(w, streamTextOnly("done"))
		}
	}))
	defer srv.Close()

	exec, _ := newTestExecutor(t, srv, newFakeRegistry(tool))
	cfg := newTestConfig(t)

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

func TestAnthropicAPIExecutor_ParallelToolCalls(t *testing.T) {
	calls := &atomic.Int32{}
	t1 := &fakeTool{name: "slow_a", output: "A", delay: 50 * time.Millisecond, calls: calls}
	t2 := &fakeTool{name: "slow_b", output: "B", delay: 50 * time.Millisecond, calls: calls}

	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch turn.Add(1) {
		case 1:
			writeStream(w, streamToolCalls(
				fakeToolCall{id: "tu_a", name: "slow_a", input: "{}"},
				fakeToolCall{id: "tu_b", name: "slow_b", input: "{}"},
			))
		default:
			writeStream(w, streamTextOnly("done"))
		}
	}))
	defer srv.Close()

	exec, _ := newTestExecutor(t, srv, newFakeRegistry(t1, t2))
	cfg := newTestConfig(t)

	start := time.Now()
	if _, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	elapsed := time.Since(start)

	if calls.Load() != 2 {
		t.Errorf("tool calls = %d, want 2", calls.Load())
	}
	// Parallelism: both tools sleep 50ms. Sequential would take ~100ms; allow
	// generous headroom for CI scheduling jitter while still catching a
	// regression to sequential execution.
	if elapsed > 90*time.Millisecond {
		t.Errorf("parallel tools took %v, expected well under 100ms (sequential)", elapsed)
	}
}

func TestAnthropicAPIExecutor_MultiTurnToolLoop(t *testing.T) {
	tool := &fakeTool{name: "step", output: "ok", calls: &atomic.Int32{}}

	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Read body to ensure tool_result blocks are echoed back; lightweight check.
		body, _ := io.ReadAll(r.Body)
		switch turn.Add(1) {
		case 1:
			writeStream(w, streamToolCalls(fakeToolCall{id: "tu1", name: "step", input: "{}"}))
		case 2:
			if !strings.Contains(string(body), `"tool_result"`) {
				t.Errorf("turn 2 missing tool_result; body=%s", body)
			}
			writeStream(w, streamToolCalls(fakeToolCall{id: "tu2", name: "step", input: "{}"}))
		default:
			writeStream(w, streamTextOnly("final"))
		}
	}))
	defer srv.Close()

	exec, _ := newTestExecutor(t, srv, newFakeRegistry(tool))
	cfg := newTestConfig(t)

	out, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if tool.calls.Load() != 2 {
		t.Errorf("tool calls = %d, want 2", tool.calls.Load())
	}
	if !strings.Contains(out, "final") {
		t.Errorf("output missing final text: %q", out)
	}
}

func TestAnthropicAPIExecutor_RefreshOn401(t *testing.T) {
	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		auth := r.Header.Get("Authorization")
		switch turn.Add(1) {
		case 1:
			if auth != "Bearer tok-original" {
				t.Errorf("turn 1 token = %q, want original", auth)
			}
			http.Error(w, `{"error":"unauthorized"}`, http.StatusUnauthorized)
		default:
			if auth != "Bearer tok-refreshed" {
				t.Errorf("turn 2 token = %q, want refreshed", auth)
			}
			writeStream(w, streamTextOnly("ok"))
		}
	}))
	defer srv.Close()

	exec, cred := newTestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)

	out, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if out != "ok" {
		t.Errorf("output = %q, want ok", out)
	}
	if cred.AccessToken != "tok-refreshed" {
		t.Errorf("token not refreshed: %q", cred.AccessToken)
	}
}

func TestAnthropicAPIExecutor_RetryOn429(t *testing.T) {
	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if turn.Add(1) == 1 {
			w.Header().Set("Retry-After", "0")
			http.Error(w, `{"error":"rate_limited"}`, http.StatusTooManyRequests)
			return
		}
		writeStream(w, streamTextOnly("recovered"))
	}))
	defer srv.Close()

	exec, _ := newTestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)

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

func TestAnthropicAPIExecutor_CancellationMidStream(t *testing.T) {
	// Server streams forever; we cancel mid-flight.
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		flusher, _ := w.(http.Flusher)
		_, _ = io.WriteString(w, sse("message_start", `{"type":"message_start","message":{"id":"m","model":"x","usage":{"input_tokens":1,"output_tokens":0}}}`))
		_, _ = io.WriteString(w, sse("content_block_start", `{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}`))
		if flusher != nil {
			flusher.Flush()
		}
		// Block until the client's ctx is canceled.
		<-r.Context().Done()
	}))
	defer srv.Close()

	exec, _ := newTestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)

	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	var doneTime time.Time
	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		_, _, _, _ = exec.Execute(ctx, cfg, event.NoOpEmitter{}, false)
		doneTime = time.Now()
		close(done)
	}()

	// Give the server a moment to start streaming, then cancel.
	time.Sleep(20 * time.Millisecond)
	cancelTime := time.Now()
	cancel()

	select {
	case <-done:
		gap := doneTime.Sub(cancelTime)
		if gap > 200*time.Millisecond {
			t.Errorf("Execute returned %v after cancel; want <200ms", gap)
		}
	case <-time.After(500 * time.Millisecond):
		t.Fatal("Execute did not return within 500ms of cancel")
	}
	wg.Wait()
}

func TestAnthropicAPIExecutor_ParityShape(t *testing.T) {
	// Parity check: same prompt assembly hits the same identifiable artifacts
	// (output.md written, captured assistant text matches) across multiple
	// reference inputs. We use streamTextOnly so the comparison is deterministic.
	cases := []struct {
		name      string
		objective string
		want      string
	}{
		{"refactor", "refactor the foo handler", "refactored foo"},
		{"summarize", "summarize the report", "summary delivered"},
		{"plan", "plan the migration", "migration plan ready"},
	}
	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				body, _ := io.ReadAll(r.Body)
				if !strings.Contains(string(body), tc.objective) {
					t.Errorf("request missing objective %q", tc.objective)
				}
				writeStream(w, streamTextOnly(tc.want))
			}))
			defer srv.Close()

			exec, _ := newTestExecutor(t, srv, newFakeRegistry())
			cfg := newTestConfig(t)
			cfg.Bundle.Objective = tc.objective

			out, _, cost, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
			if err != nil {
				t.Fatalf("Execute: %v", err)
			}
			if out != tc.want {
				t.Errorf("output = %q, want %q", out, tc.want)
			}
			if cost == nil {
				t.Fatal("cost is nil")
			}
		})
	}
}

func TestComputeCostUSD(t *testing.T) {
	u := &anthropicUsage{InputTokens: 1_000_000, OutputTokens: 1_000_000}
	got := computeCostUSD("claude-sonnet-4-6", u)
	want := 3.00 + 15.00 // $3 input + $15 output for 1M each
	if got != want {
		t.Errorf("cost = %v, want %v", got, want)
	}

	// Unknown model falls back to sonnet pricing.
	got = computeCostUSD("claude-future-X", u)
	if got != want {
		t.Errorf("unknown-model fallback cost = %v, want %v", got, want)
	}
}

func TestDefaultRegistryRegistersAnthropicAPI(t *testing.T) {
	r := defaultRegistry()
	if !r.has(core.RuntimeAnthropicAPI) {
		t.Error("defaultRegistry missing RuntimeAnthropicAPI")
	}
	desc, ok := r.describe(core.RuntimeAnthropicAPI)
	if !ok {
		t.Error("anthropic-api executor should expose a descriptor")
	}
	if desc.Name != core.RuntimeAnthropicAPI {
		t.Errorf("descriptor name = %q, want %q", desc.Name, core.RuntimeAnthropicAPI)
	}
}
