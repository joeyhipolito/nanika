package engine

import (
	"context"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/auth"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/tools"
)

func newOpenRouterTestExecutor(t *testing.T, srv *httptest.Server, reg *tools.Registry) *OpenAIAPIExecutor {
	t.Helper()
	exec := NewOpenRouterAPIExecutor(reg)
	exec.BaseURL = strings.TrimRight(srv.URL, "/")
	exec.HTTPClient = srv.Client()
	exec.MaxTurns = 8
	exec.LoadCredential = func(provider string) (*auth.Credential, error) {
		if provider != "openrouter" {
			t.Errorf("LoadCredential called with %q, want %q", provider, "openrouter")
		}
		return &auth.Credential{Provider: "openrouter", AuthType: auth.AuthTypeAPIKey, APIKey: "or-test"}, nil
	}
	exec.Backoff = func(int) time.Duration { return time.Millisecond }
	return exec
}

func TestOpenRouterAPIExecutor_Describe(t *testing.T) {
	e := NewOpenRouterAPIExecutor(nil)
	d := e.Describe()
	if d.Name != core.RuntimeOpenRouter {
		t.Fatalf("descriptor name = %q, want %q", d.Name, core.RuntimeOpenRouter)
	}
	for _, want := range []core.RuntimeCap{core.CapToolUse, core.CapStreaming, core.CapCostReport} {
		if !d.Caps.Has(want) {
			t.Errorf("descriptor missing cap %q", want)
		}
	}
}

func TestOpenRouterAPIExecutor_TextOnly(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// OpenRouter is OpenAI-compatible, so the same wire shape applies.
		writeStream(w, openaiTextStream("hello router", "anthropic/claude-3.5-sonnet"))
	}))
	defer srv.Close()

	exec := newOpenRouterTestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)
	cfg.Model = "anthropic/claude-3.5-sonnet"

	out, _, cost, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if out != "hello router" {
		t.Errorf("output = %q, want hello router", out)
	}
	if cost == nil || cost.TotalCostUSD <= 0 {
		t.Errorf("expected non-zero cost via openrouter pricing, got %#v", cost)
	}
}

func TestOpenRouterAPIExecutor_AuthHeader(t *testing.T) {
	var seen string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		seen = r.Header.Get("Authorization")
		writeStream(w, openaiTextStream("ok", "openai/gpt-4o"))
	}))
	defer srv.Close()

	exec := newOpenRouterTestExecutor(t, srv, newFakeRegistry())
	cfg := newTestConfig(t)
	cfg.Model = "openai/gpt-4o"

	if _, _, _, err := exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false); err != nil {
		t.Fatalf("Execute: %v", err)
	}
	if seen != "Bearer or-test" {
		t.Errorf("Authorization = %q, want %q", seen, "Bearer or-test")
	}
}

func TestOpenRouterAPIExecutor_ToolCall(t *testing.T) {
	calls := &atomic.Int32{}
	tool := &fakeTool{name: "ping", output: "pong", calls: calls}

	var turn atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch turn.Add(1) {
		case 1:
			writeStream(w, openaiToolCallStream("anthropic/claude-3.5-sonnet",
				openaiToolCallSpec{id: "call_x", name: "ping", input: "{}"}))
		default:
			writeStream(w, openaiTextStream("done", "anthropic/claude-3.5-sonnet"))
		}
	}))
	defer srv.Close()

	exec := newOpenRouterTestExecutor(t, srv, newFakeRegistry(tool))
	cfg := newTestConfig(t)
	cfg.Model = "anthropic/claude-3.5-sonnet"

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

func TestDefaultRegistryRegistersOpenRouter(t *testing.T) {
	r := defaultRegistry()
	if !r.has(core.RuntimeOpenRouter) {
		t.Error("defaultRegistry missing RuntimeOpenRouter")
	}
	desc, ok := r.describe(core.RuntimeOpenRouter)
	if !ok {
		t.Error("openrouter executor should expose a descriptor")
	}
	if desc.Name != core.RuntimeOpenRouter {
		t.Errorf("descriptor name = %q, want %q", desc.Name, core.RuntimeOpenRouter)
	}
}
