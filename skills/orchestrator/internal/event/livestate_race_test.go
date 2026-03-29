package event

import (
	"fmt"
	"sync"
	"testing"
)

// TestLiveState_ConcurrentReadWrite exercises the RWMutex under the race
// detector: many goroutines call Mission() while another goroutine is
// continuously publishing events that drive the writer path in apply().
func TestLiveState_ConcurrentReadWrite(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	const (
		missionID = "m-race"
		readers   = 10
		iters     = 50
	)

	b.Publish(New(MissionStarted, missionID, "", "", nil))
	waitForSnap(t, ls, missionID)

	var wg sync.WaitGroup

	// Concurrent readers: snapshot + iterate Phases while writes are in flight.
	wg.Add(readers)
	for range readers {
		go func() {
			defer wg.Done()
			for range iters {
				snap := ls.Mission(missionID)
				if snap == nil {
					continue
				}
				_ = snap.Status
				for _, p := range snap.Phases {
					_ = p.Status
				}
			}
		}()
	}

	// Concurrent writer: publish phase events that drive apply() under write lock.
	wg.Add(1)
	go func() {
		defer wg.Done()
		for i := range iters {
			phaseID := fmt.Sprintf("p%d", i)
			b.Publish(New(PhaseStarted, missionID, phaseID, "",
				map[string]any{"name": fmt.Sprintf("phase-%d", i)}))
		}
	}()

	wg.Wait()
}

// TestLiveState_ConcurrentMultiMission races reads across many missions
// simultaneously with writes spread across all of them, ensuring no
// cross-mission locking bugs surface under the race detector.
func TestLiveState_ConcurrentMultiMission(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	const missions = 5

	// Prime all missions.
	for i := range missions {
		mid := fmt.Sprintf("m-multi-%d", i)
		b.Publish(New(MissionStarted, mid, "", "", nil))
	}
	for i := range missions {
		waitForSnap(t, ls, fmt.Sprintf("m-multi-%d", i))
	}

	var wg sync.WaitGroup

	// One reader goroutine per mission.
	for i := range missions {
		mid := fmt.Sprintf("m-multi-%d", i)
		wg.Add(1)
		go func(id string) {
			defer wg.Done()
			for range 30 {
				snap := ls.Mission(id)
				if snap != nil {
					_ = snap.Status
				}
			}
		}(mid)
	}

	// One writer goroutine publishing terminal events across all missions.
	wg.Add(1)
	go func() {
		defer wg.Done()
		for i := range missions {
			mid := fmt.Sprintf("m-multi-%d", i)
			b.Publish(New(MissionCompleted, mid, "", "", nil))
		}
	}()

	wg.Wait()
}

// TestLiveState_ConcurrentCloneIntegrity verifies that clones returned by
// Mission() are not affected by subsequent writes under the race detector.
// A mutation of the returned clone must not race with the writer path.
func TestLiveState_ConcurrentCloneIntegrity(t *testing.T) {
	b := NewBus()
	ls := NewLiveState(b)
	defer ls.Close()

	b.Publish(New(MissionStarted, "m-clone", "", "", nil))
	b.Publish(New(PhaseStarted, "m-clone", "p1", "", map[string]any{"name": "build"}))
	waitForPhase(t, ls, "m-clone", "p1")

	var wg sync.WaitGroup

	// Grab clones and mutate them concurrently while writes happen.
	const goroutines = 8
	wg.Add(goroutines)
	for range goroutines {
		go func() {
			defer wg.Done()
			for range 20 {
				snap := ls.Mission("m-clone")
				if snap == nil {
					continue
				}
				// Mutate the clone — must not race with internal map writes.
				snap.Status = "mutated"
				if p, ok := snap.Phases["p1"]; ok {
					p.Status = "mutated"
				}
			}
		}()
	}

	// Concurrent writes updating the phase status.
	wg.Add(1)
	go func() {
		defer wg.Done()
		for range 20 {
			b.Publish(New(PhaseCompleted, "m-clone", "p1", "", nil))
			b.Publish(New(PhaseStarted, "m-clone", "p1", "", map[string]any{"name": "build"}))
		}
	}()

	wg.Wait()
}
