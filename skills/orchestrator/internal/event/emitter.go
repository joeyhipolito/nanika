package event

import "context"

// Emitter dispatches orchestrator events.
//
// Emit is fire-and-forget: the engine never blocks waiting for delivery.
// Implementations must be safe for concurrent use from multiple goroutines.
// Close flushes any pending writes and releases underlying resources.
type Emitter interface {
	Emit(ctx context.Context, event Event)
	Close() error
}

// DropStats aggregates best-effort delivery losses observed by emitters and
// the in-memory bus. All fields are cumulative counters.
type DropStats struct {
	FileDroppedWrites int64
	UDSDroppedWrites  int64
	SubscriberDrops   int64
}

// Any reports whether any delivery loss has been observed.
func (s DropStats) Any() bool {
	return s.FileDroppedWrites > 0 || s.UDSDroppedWrites > 0 || s.SubscriberDrops > 0
}

// Merge returns the element-wise sum of two drop-stat snapshots.
func (s DropStats) Merge(other DropStats) DropStats {
	return DropStats{
		FileDroppedWrites: s.FileDroppedWrites + other.FileDroppedWrites,
		UDSDroppedWrites:  s.UDSDroppedWrites + other.UDSDroppedWrites,
		SubscriberDrops:   s.SubscriberDrops + other.SubscriberDrops,
	}
}

// DropReporter is implemented by emitters or wrappers that can report
// cumulative delivery-loss counters to operator-facing surfaces.
type DropReporter interface {
	DropStats() DropStats
}
