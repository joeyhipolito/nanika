package event

import (
	"context"
	"sync"
	"testing"
	"time"
)

// TestBusPublishSubscribe verifies basic publish/subscribe round-trip.
func TestBusPublishSubscribe(t *testing.T) {
	b := NewBus()
	id, ch := b.Subscribe()
	defer b.Unsubscribe(id)

	ev := New(MissionStarted, "m1", "", "", nil)
	b.Publish(ev)

	select {
	case got := <-ch:
		if got.MissionID != "m1" {
			t.Fatalf("expected mission_id m1, got %q", got.MissionID)
		}
		if got.Sequence != 1 {
			t.Fatalf("expected sequence 1, got %d", got.Sequence)
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout waiting for event")
	}
}

// TestBusSequenceMonotonic verifies each Publish increments the global sequence.
func TestBusSequenceMonotonic(t *testing.T) {
	b := NewBus()
	id, ch := b.Subscribe()
	defer b.Unsubscribe(id)

	for i := int64(1); i <= 5; i++ {
		b.Publish(New(MissionStarted, "m1", "", "", nil))
		ev := <-ch
		if ev.Sequence != i {
			t.Fatalf("event %d: expected sequence %d, got %d", i, i, ev.Sequence)
		}
	}
}

// TestBusRingOverflow verifies oldest events are overwritten when the ring is full.
func TestBusRingOverflow(t *testing.T) {
	b := NewBus()

	for i := 0; i < busCapacity+1; i++ {
		b.Publish(New(MissionStarted, "m1", "", "", nil))
	}

	events := b.EventsSince(0)
	if len(events) != busCapacity {
		t.Fatalf("expected %d buffered events, got %d", busCapacity, len(events))
	}

	// After 1001 publishes, the oldest surviving event has sequence 2.
	if events[0].Sequence != 2 {
		t.Fatalf("expected oldest sequence=2 after overflow, got %d", events[0].Sequence)
	}
	if events[busCapacity-1].Sequence != int64(busCapacity+1) {
		t.Fatalf("expected newest sequence=%d, got %d", busCapacity+1, events[busCapacity-1].Sequence)
	}
}

// TestBusEventsSince verifies replay returns only events with Sequence > seq.
func TestBusEventsSince(t *testing.T) {
	b := NewBus()

	for i := 0; i < 10; i++ {
		b.Publish(New(MissionStarted, "m1", "", "", nil))
	}

	// Events since sequence 5 should return sequences 6..10.
	events := b.EventsSince(5)
	if len(events) != 5 {
		t.Fatalf("expected 5 events since seq=5, got %d", len(events))
	}
	if events[0].Sequence != 6 {
		t.Fatalf("expected first returned sequence=6, got %d", events[0].Sequence)
	}

	// Events since 0 returns all 10.
	all := b.EventsSince(0)
	if len(all) != 10 {
		t.Fatalf("expected 10 events since seq=0, got %d", len(all))
	}

	// Events since 10 returns nothing.
	none := b.EventsSince(10)
	if len(none) != 0 {
		t.Fatalf("expected 0 events since seq=10, got %d", len(none))
	}
}

// TestBusNonBlocking verifies Publish never blocks even with a full subscriber channel.
func TestBusNonBlocking(t *testing.T) {
	b := NewBus()
	// Subscribe but never drain — channel fills up at 64 events.
	_, _ = b.Subscribe()

	done := make(chan struct{})
	go func() {
		for i := 0; i < busCapacity*2; i++ {
			b.Publish(New(MissionStarted, "m1", "", "", nil))
		}
		close(done)
	}()

	select {
	case <-done:
		// Publisher completed without blocking.
	case <-time.After(500 * time.Millisecond):
		t.Fatal("Publish blocked on slow subscriber")
	}
}

// TestBusUnsubscribe verifies the channel is closed after Unsubscribe.
func TestBusUnsubscribe(t *testing.T) {
	b := NewBus()
	id, ch := b.Subscribe()
	b.Unsubscribe(id)

	// Channel should be closed.
	select {
	case _, ok := <-ch:
		if ok {
			t.Fatal("expected closed channel after Unsubscribe")
		}
	default:
		t.Fatal("expected closed channel to be readable (zero value)")
	}

	// Subsequent publishes must not panic or block.
	b.Publish(New(MissionStarted, "m1", "", "", nil))
}

// TestBusConcurrentPublish verifies concurrent publishers produce unique sequences.
func TestBusConcurrentPublish(t *testing.T) {
	b := NewBus()
	const goroutines = 20
	const perGoroutine = 50

	var wg sync.WaitGroup
	for i := 0; i < goroutines; i++ {
		wg.Add(1)
		go func() {
			defer wg.Done()
			for j := 0; j < perGoroutine; j++ {
				b.Publish(New(MissionStarted, "m1", "", "", nil))
			}
		}()
	}
	wg.Wait()

	events := b.EventsSince(0)
	total := goroutines * perGoroutine
	// With total <= busCapacity we should have all events buffered.
	if total <= busCapacity && len(events) != total {
		t.Fatalf("expected %d events, got %d", total, len(events))
	}

	// Verify all sequences are unique.
	seen := make(map[int64]bool, len(events))
	for _, ev := range events {
		if seen[ev.Sequence] {
			t.Fatalf("duplicate sequence %d", ev.Sequence)
		}
		seen[ev.Sequence] = true
	}
}

// TestBusSubscriberDrops verifies SubscriberDrops increments when a subscriber
// channel is full and events cannot be delivered.
func TestBusSubscriberDrops(t *testing.T) {
	b := NewBus()
	// Subscribe but never drain — channel capacity is 64.
	_, _ = b.Subscribe()

	// Publish more events than the channel can hold.
	const over = 100
	for i := 0; i < over; i++ {
		b.Publish(New(MissionStarted, "m1", "", "", nil))
	}

	drops := b.SubscriberDrops()
	if drops == 0 {
		t.Fatal("SubscriberDrops() = 0; expected > 0 after flooding slow subscriber")
	}
	// Exactly (over - 64) events should have been dropped (channel buffers 64).
	const subCap = 64
	want := int64(over - subCap)
	if drops != want {
		t.Fatalf("SubscriberDrops() = %d; want %d", drops, want)
	}
}

// TestBusSubscriberDropsZeroWhenDrained verifies SubscriberDrops stays zero
// when a subscriber keeps up with the publisher.
func TestBusSubscriberDropsZeroWhenDrained(t *testing.T) {
	b := NewBus()
	id, ch := b.Subscribe()
	defer b.Unsubscribe(id)

	const n = 10
	for i := 0; i < n; i++ {
		b.Publish(New(MissionStarted, "m1", "", "", nil))
		<-ch // drain immediately
	}

	if drops := b.SubscriberDrops(); drops != 0 {
		t.Fatalf("SubscriberDrops() = %d; want 0 for a drained subscriber", drops)
	}
}

// TestBusEmitter verifies BusEmitter satisfies the Emitter interface.
func TestBusEmitter(t *testing.T) {
	b := NewBus()
	var em Emitter = NewBusEmitter(b)

	id, ch := b.Subscribe()
	defer b.Unsubscribe(id)

	em.Emit(context.Background(), New(PhaseStarted, "m1", "p1", "", nil))
	if err := em.Close(); err != nil {
		t.Fatalf("Close: %v", err)
	}

	select {
	case ev := <-ch:
		if ev.Type != PhaseStarted {
			t.Fatalf("expected PhaseStarted, got %s", ev.Type)
		}
	case <-time.After(100 * time.Millisecond):
		t.Fatal("timeout waiting for event from BusEmitter")
	}
}
