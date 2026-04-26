package engine

import (
	"context"
	"fmt"
	"io"
	"os"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/nanika/shared/sdk"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

// PhaseExecutor is the contract for runtime-specific phase execution.
// Each backend (Claude, OpenAI, Gemini, local, etc.) implements this interface.
// The engine selects an executor per phase based on Phase.Runtime.
type PhaseExecutor interface {
	// Execute runs the phase and returns (output, sessionID, cost, error).
	// The semantics of sessionID and cost match those of worker.Execute:
	// sessionID may be empty when unavailable; cost may be nil.
	Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (output, sessionID string, cost *sdk.CostInfo, err error)
}

// RuntimeDescriber is an optional interface that PhaseExecutor implementations
// may implement to declare their runtime capabilities. When present, the engine
// uses the descriptor to validate phase contracts before dispatch.
// Executors that do not implement this interface are treated as capability-unknown
// and receive a warning instead of a hard failure on contract mismatch.
type RuntimeDescriber interface {
	Describe() core.RuntimeDescriptor
}

// ClaudeExecutor is the default PhaseExecutor. It delegates directly to
// worker.Execute and therefore produces identical behaviour to the previous
// hard-wired call site. Zero-value is ready to use.
type ClaudeExecutor struct{}

func (ClaudeExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	return worker.Execute(ctx, config, emitter, verbose)
}

// Describe returns the RuntimeDescriptor for Claude Code, declaring its full
// capability set. This satisfies the RuntimeDescriber interface.
func (ClaudeExecutor) Describe() core.RuntimeDescriptor {
	return core.ClaudeDescriptor()
}

// executorRegistry maps Runtime identifiers to their PhaseExecutor
// implementations. Access is read-only after initialization; no mutex needed.
type executorRegistry map[core.Runtime]PhaseExecutor

var runtimeWarningWriter io.Writer = os.Stdout

// defaultRegistry returns the production registry with RuntimeClaude and
// RuntimeCodex pre-registered. Additional runtimes can be registered by
// calling Engine.RegisterExecutor before the first Execute call.
func defaultRegistry() executorRegistry {
	return executorRegistry{
		core.RuntimeClaude: ClaudeExecutor{},
		core.RuntimeCodex:  NewCodexExecutor(),
	}
}

func (r executorRegistry) has(rt core.Runtime) bool {
	_, ok := r[rt.Effective()]
	return ok
}

// resolve returns the executor for the given runtime. When the runtime is
// unregistered it falls back to RuntimeClaude so the system degrades
// gracefully rather than panicking on unknown values.
func (r executorRegistry) resolve(rt core.Runtime) PhaseExecutor {
	if ex, ok := r[rt.Effective()]; ok {
		return ex
	}
	return r[core.RuntimeClaude]
}

// describe returns the RuntimeDescriptor for the given runtime. If the executor
// implements RuntimeDescriber, its declared descriptor is returned with ok=true.
// Otherwise ok=false, indicating the engine should treat the executor as
// capability-unknown (warn, don't hard-fail).
func (r executorRegistry) describe(rt core.Runtime) (core.RuntimeDescriptor, bool) {
	ex, ok := r[rt.Effective()]
	if !ok {
		return core.RuntimeDescriptor{Name: rt.Effective()}, false
	}
	if d, ok := ex.(RuntimeDescriber); ok {
		return d.Describe(), true
	}
	return core.RuntimeDescriptor{Name: rt.Effective()}, false
}

func warnUnknownRuntime(rt core.Runtime) {
	if rt == "" || rt.Effective() == core.RuntimeClaude {
		return
	}
	fmt.Fprintf(runtimeWarningWriter, "[engine] warning: unknown runtime %q; falling back to %q\n", rt, core.RuntimeClaude)
}
