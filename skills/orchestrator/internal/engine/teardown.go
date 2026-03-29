package engine

import (
	"context"
	"fmt"
	"os"
	"os/signal"
	"sync"
	"syscall"
)

// TeardownManager handles graceful shutdown on interrupt signals.
type TeardownManager struct {
	cancel  context.CancelFunc
	wg      *sync.WaitGroup
	onStop  []func() // callbacks to run on shutdown
	once    sync.Once
	sigCh   chan os.Signal
}

// NewTeardownManager creates a teardown handler that cancels ctx on SIGINT/SIGTERM.
func NewTeardownManager(cancel context.CancelFunc) *TeardownManager {
	tm := &TeardownManager{
		cancel: cancel,
		wg:     &sync.WaitGroup{},
		sigCh:  make(chan os.Signal, 1),
	}

	signal.Notify(tm.sigCh, syscall.SIGINT, syscall.SIGTERM)

	go func() {
		<-tm.sigCh
		tm.Shutdown("received interrupt signal")
	}()

	return tm
}

// OnStop registers a callback to run during shutdown.
func (tm *TeardownManager) OnStop(fn func()) {
	tm.onStop = append(tm.onStop, fn)
}

// TrackPhase increments the wait group for an in-flight phase.
func (tm *TeardownManager) TrackPhase() {
	tm.wg.Add(1)
}

// PhaseComplete decrements the wait group when a phase finishes.
func (tm *TeardownManager) PhaseComplete() {
	tm.wg.Done()
}

// Shutdown performs graceful teardown: cancel context, wait for phases, run callbacks.
func (tm *TeardownManager) Shutdown(reason string) {
	tm.once.Do(func() {
		fmt.Fprintf(os.Stderr, "\n[teardown] %s\n", reason)

		// 1. Cancel context to stop new work
		tm.cancel()

		// 2. Wait for in-flight phases to finish
		fmt.Fprintf(os.Stderr, "[teardown] waiting for in-flight phases...\n")
		tm.wg.Wait()

		// 3. Run registered callbacks (checkpoint save, etc.)
		for _, fn := range tm.onStop {
			func() {
				defer func() {
					if r := recover(); r != nil {
						fmt.Fprintf(os.Stderr, "[teardown] callback panic: %v\n", r)
					}
				}()
				fn()
			}()
		}

		fmt.Fprintf(os.Stderr, "[teardown] complete\n")
	})
}

// Close stops listening for signals.
func (tm *TeardownManager) Close() {
	signal.Stop(tm.sigCh)
}
