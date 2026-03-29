package event

import "context"

// NoOpEmitter discards all events. Used as the default emitter so callers
// never need to nil-check before emitting.
type NoOpEmitter struct{}

func (NoOpEmitter) Emit(_ context.Context, _ Event) {}
func (NoOpEmitter) Close() error                    { return nil }
func (NoOpEmitter) DropStats() DropStats            { return DropStats{} }
