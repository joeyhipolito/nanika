package engine

import (
	"context"
	"strings"
	"sync"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// phaseToolCaptureEmitter forwards events to the underlying emitter while
// collecting per-phase tool-use chunks for metrics parsing.
type phaseToolCaptureEmitter struct {
	phaseID string
	inner   event.Emitter

	mu         sync.Mutex
	transcript strings.Builder
}

func newPhaseToolCaptureEmitter(phaseID string, inner event.Emitter) *phaseToolCaptureEmitter {
	return &phaseToolCaptureEmitter{
		phaseID: phaseID,
		inner:   inner,
	}
}

func (e *phaseToolCaptureEmitter) Emit(ctx context.Context, ev event.Event) {
	if ev.Type == event.WorkerOutput && ev.PhaseID == e.phaseID {
		if kind, _ := ev.Data["event_kind"].(string); kind == "tool_use" {
			if chunk, _ := ev.Data["chunk"].(string); chunk != "" {
				e.mu.Lock()
				e.transcript.WriteString(chunk)
				if !strings.HasSuffix(chunk, "\n") {
					e.transcript.WriteByte('\n')
				}
				e.mu.Unlock()
			}
		}
	}
	if e.inner != nil {
		e.inner.Emit(ctx, ev)
	}
}

func (e *phaseToolCaptureEmitter) Close() error { return nil }

func (e *phaseToolCaptureEmitter) Transcript() string {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.transcript.String()
}
