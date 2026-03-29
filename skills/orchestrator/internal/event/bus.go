package event

import (
	"context"
	"sync"
	"sync/atomic"
)

const busCapacity = 1000

// Bus is a fixed-capacity ring buffer with non-blocking fan-out to subscribers.
//
// When the ring is full, the oldest event is overwritten (newest events are
// always preserved). Subscribers that cannot keep up miss events rather than
// stall the publisher — fan-out is always non-blocking.
type Bus struct {
	mu     sync.Mutex
	buf    [busCapacity]Event
	write  int  // index of the next slot to write
	count  int  // number of valid entries (0..busCapacity)
	seq    atomic.Int64 // monotonic global sequence; assigned at Publish

	subs            map[uint64]chan Event
	nextID          uint64
	subscriberDrops atomic.Int64
}

// NewBus allocates an empty Bus ready for use.
func NewBus() *Bus {
	return &Bus{subs: make(map[uint64]chan Event)}
}

// Publish stores ev in the ring buffer and fans it out to all current
// subscribers (non-blocking). If the ring is full the oldest entry is
// silently overwritten.
//
// The bus always assigns its own monotonically-increasing global sequence,
// regardless of any sequence already present on the event. This guarantees
// that every event in the ring buffer has a unique, globally-ordered cursor
// that is stable across missions — preventing the collision where two
// concurrent missions each start their MultiEmitter at seq=1 and produce
// identical sequence numbers that break SSE replay deduplication.
//
// Mission-local sequences (set by MultiEmitter/FileEmitter) remain in the
// JSONL log for per-mission replay; the bus sequence is the SSE cursor only.
func (b *Bus) Publish(ev Event) {
	ev.Sequence = b.seq.Add(1)

	b.mu.Lock()
	b.buf[b.write] = ev
	b.write = (b.write + 1) % busCapacity
	if b.count < busCapacity {
		b.count++
	}
	// Snapshot subscriber channels under lock so we don't hold it during sends.
	subs := make([]chan Event, 0, len(b.subs))
	for _, ch := range b.subs {
		subs = append(subs, ch)
	}
	b.mu.Unlock()

	// Non-blocking send to each subscriber.
	for _, ch := range subs {
		select {
		case ch <- ev:
		default:
			// Subscriber channel full; drop for this subscriber.
			b.subscriberDrops.Add(1)
		}
	}
}

// Subscribe registers a new subscriber and returns its ID and a buffered
// channel that receives events. The channel holds up to 64 events; a slow
// consumer misses events rather than blocking the publisher.
// Call Unsubscribe(id) when done to release resources.
func (b *Bus) Subscribe() (uint64, <-chan Event) {
	ch := make(chan Event, 64)
	b.mu.Lock()
	id := b.nextID
	b.nextID++
	b.subs[id] = ch
	b.mu.Unlock()
	return id, ch
}

// Unsubscribe removes the subscriber and closes its channel.
func (b *Bus) Unsubscribe(id uint64) {
	b.mu.Lock()
	ch, ok := b.subs[id]
	if ok {
		delete(b.subs, id)
	}
	b.mu.Unlock()
	if ok {
		close(ch)
	}
}

// SubscriberDrops returns the cumulative number of events dropped because a
// subscriber's delivery channel was full at publish time. A non-zero value
// indicates a slow consumer; events are still in the ring buffer and may be
// replayed via EventsSince.
func (b *Bus) SubscriberDrops() int64 {
	return b.subscriberDrops.Load()
}

// EventsSince returns all buffered events with Sequence > seq, in insertion order.
// Pass seq=0 to replay all buffered events.
func (b *Bus) EventsSince(seq int64) []Event {
	b.mu.Lock()
	defer b.mu.Unlock()

	if b.count == 0 {
		return nil
	}

	// Oldest entry is at (write - count + N) % N.
	start := (b.write - b.count + busCapacity) % busCapacity
	var out []Event
	for i := 0; i < b.count; i++ {
		ev := b.buf[(start+i)%busCapacity]
		if ev.Sequence > seq {
			out = append(out, ev)
		}
	}
	return out
}

// BusEmitter wraps a Bus and implements the Emitter interface by publishing
// each event to the bus. Close is a no-op.
type BusEmitter struct {
	bus *Bus
}

// NewBusEmitter returns an Emitter backed by b.
func NewBusEmitter(bus *Bus) *BusEmitter {
	return &BusEmitter{bus: bus}
}

func (be *BusEmitter) Emit(_ context.Context, ev Event) { be.bus.Publish(ev) }
func (be *BusEmitter) Close() error                     { return nil }

// DropStats reports slow-subscriber drops observed by the bus.
func (be *BusEmitter) DropStats() DropStats {
	return DropStats{SubscriberDrops: be.bus.SubscriberDrops()}
}
