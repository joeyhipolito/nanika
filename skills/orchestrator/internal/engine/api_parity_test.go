package engine

import (
	"context"
	"io"
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
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// TestAPIParity_AcrossProviders runs the same WorkerConfig against all three
// HTTP-API executors (Anthropic, OpenAI, OpenRouter) using fixture servers
// that mimic each provider's wire format. It verifies that across runtimes:
//
//   - the worker emits non-empty output text
//   - tools were invoked when the assistant requested them
//   - cost > 0 (each provider has different pricing tables; the gate only
//     checks non-zero so a price change in any one provider doesn't break it)
//
// This is the success criterion for the apiloop refactor: the same agentic
// loop drives three different providers through the same WorkerConfig with
// shape-compatible results.
func TestAPIParity_AcrossProviders(t *testing.T) {
	type providerCase struct {
		name      string
		newExec   func(*testing.T, *httptest.Server, *tools.Registry) parityExec
		toolStub  func() string
		textStub  func() string
		model     string
	}

	cases := []providerCase{
		{
			name: "anthropic",
			newExec: func(t *testing.T, srv *httptest.Server, reg *tools.Registry) parityExec {
				e, _ := newTestExecutor(t, srv, reg)
				return parityExec{
					exec: e,
					describe: func() core.RuntimeDescriptor { return e.Describe() },
				}
			},
			toolStub: func() string {
				return streamToolCalls(fakeToolCall{id: "tu_x", name: "echo", input: "{}"})
			},
			textStub: func() string { return streamTextOnly("done-anthropic") },
			model:    "claude-sonnet-4-6",
		},
		{
			name: "openai",
			newExec: func(t *testing.T, srv *httptest.Server, reg *tools.Registry) parityExec {
				e := newOpenAITestExecutor(t, srv, reg)
				return parityExec{
					exec: e,
					describe: func() core.RuntimeDescriptor { return e.Describe() },
				}
			},
			toolStub: func() string {
				return openaiToolCallStream("gpt-4o",
					openaiToolCallSpec{id: "call_x", name: "echo", input: "{}"})
			},
			textStub: func() string { return openaiTextStream("done-openai", "gpt-4o") },
			model:    "gpt-4o",
		},
		{
			name: "openrouter",
			newExec: func(t *testing.T, srv *httptest.Server, reg *tools.Registry) parityExec {
				e := newOpenRouterTestExecutor(t, srv, reg)
				return parityExec{
					exec: e,
					describe: func() core.RuntimeDescriptor { return e.Describe() },
				}
			},
			toolStub: func() string {
				return openaiToolCallStream("anthropic/claude-3.5-sonnet",
					openaiToolCallSpec{id: "call_x", name: "echo", input: "{}"})
			},
			textStub: func() string {
				return openaiTextStream("done-openrouter", "anthropic/claude-3.5-sonnet")
			},
			model: "anthropic/claude-3.5-sonnet",
		},
	}

	for _, tc := range cases {
		tc := tc
		t.Run(tc.name, func(t *testing.T) {
			toolCalls := &atomic.Int32{}
			tool := &fakeTool{name: "echo", output: "echoed", calls: toolCalls}
			reg := newFakeRegistry(tool)

			var turn atomic.Int32
			srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				body, _ := io.ReadAll(r.Body)
				if !strings.Contains(string(body), "demo objective") {
					t.Errorf("%s: request missing objective: %s", tc.name, body)
				}
				switch turn.Add(1) {
				case 1:
					writeStream(w, tc.toolStub())
				default:
					writeStream(w, tc.textStub())
				}
			}))
			defer srv.Close()

			pe := tc.newExec(t, srv, reg)
			cfg := newTestConfig(t)
			cfg.Model = tc.model

			out, _, cost, err := pe.exec.Execute(context.Background(), cfg, event.NoOpEmitter{}, false)
			if err != nil {
				t.Fatalf("%s: Execute: %v", tc.name, err)
			}
			if strings.TrimSpace(out) == "" {
				t.Errorf("%s: output text is empty", tc.name)
			}
			if toolCalls.Load() == 0 {
				t.Errorf("%s: tools were not invoked", tc.name)
			}
			if cost == nil || cost.TotalCostUSD <= 0 {
				t.Errorf("%s: expected non-zero cost, got %#v", tc.name, cost)
			}

			d := pe.describe()
			if !d.Caps.Has(core.CapToolUse) || !d.Caps.Has(core.CapStreaming) || !d.Caps.Has(core.CapCostReport) {
				t.Errorf("%s: descriptor missing required caps: %#v", tc.name, d.Caps)
			}
		})
	}
}

// parityExec is a thin wrapper so the parity test can hold the three concrete
// executor types behind a uniform Execute call without resorting to package-
// level interfaces that would leak into production code.
type parityExec struct {
	exec     interface {
		Execute(context.Context, *core.WorkerConfig, event.Emitter, bool) (string, string, *sdk.CostInfo, error)
	}
	describe func() core.RuntimeDescriptor
}

// Sanity check: each runtime's auth credential resolution uses the right key.
func TestAPIParity_CredentialKeys(t *testing.T) {
	cases := []struct {
		name    string
		want    string
		provNew func() string
	}{
		{"anthropic", "anthropic", func() string { return (anthropicProvider{}).CredentialProvider() }},
		{"openai", "openai", func() string {
			return (&openaiProvider{credProvider: "openai"}).CredentialProvider()
		}},
		{"openrouter", "openrouter", func() string {
			return (&openaiProvider{credProvider: "openrouter"}).CredentialProvider()
		}},
	}
	for _, tc := range cases {
		if got := tc.provNew(); got != tc.want {
			t.Errorf("%s: CredentialProvider = %q, want %q", tc.name, got, tc.want)
		}
	}
}

// silence unused-import warning when individual subtests are commented.
var _ = auth.AuthTypeAPIKey
var _ = time.Millisecond
