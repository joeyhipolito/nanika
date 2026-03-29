package event

import (
	"context"
	"sync/atomic"
)

// MultiEmitter fans events out to multiple child emitters.
//
// A single monotonic sequence counter is shared across the fan-out so all
// consumers see the same sequence numbers. Fan-out is synchronous to preserve
// ordering — child emitters must not block for extended periods.
type MultiEmitter struct {
	emitters []Emitter
	seq      atomic.Int64
}

// NewMultiEmitter wraps the given emitters in a fan-out emitter.
// Passing zero emitters creates a no-op fan-out.
func NewMultiEmitter(emitters ...Emitter) *MultiEmitter {
	return &MultiEmitter{emitters: emitters}
}

// NewMultiEmitterFromSeq is like NewMultiEmitter but primes the sequence
// counter to initialSeq so the first emitted event gets sequence initialSeq+1.
// Use this on mission resume so the log sequence is never reset.
func NewMultiEmitterFromSeq(initialSeq int64, emitters ...Emitter) *MultiEmitter {
	m := &MultiEmitter{emitters: emitters}
	m.seq.Store(initialSeq)
	return m
}

// Emit assigns the next sequence number and dispatches to all child emitters.
func (m *MultiEmitter) Emit(ctx context.Context, ev Event) {
	ev.Sequence = m.seq.Add(1)
	for _, e := range m.emitters {
		e.Emit(ctx, ev)
	}
}

// Close closes all child emitters, returning the first error encountered.
// All emitters are closed even if an earlier one returns an error.
func (m *MultiEmitter) Close() error {
	var first error
	for _, e := range m.emitters {
		if err := e.Close(); err != nil && first == nil {
			first = err
		}
	}
	return first
}

// DropStats aggregates drop counters from child emitters that expose them.
func (m *MultiEmitter) DropStats() DropStats {
	var stats DropStats
	for _, e := range m.emitters {
		reporter, ok := e.(DropReporter)
		if !ok {
			continue
		}
		stats = stats.Merge(reporter.DropStats())
	}
	return stats
}
