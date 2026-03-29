package sdk

import (
	"io"
	"strings"
	"testing"
	"time"
)

// TestStallThresholdDefault verifies the default is 5 minutes when no env var is set.
func TestStallThresholdDefault(t *testing.T) {
	t.Setenv("ORCHESTRATOR_STALL_TIMEOUT", "")
	got := stallThreshold()
	if got != 5*time.Minute {
		t.Errorf("want 5m, got %s", got)
	}
}

// TestStallThresholdEnvVar verifies a valid env var overrides the default.
func TestStallThresholdEnvVar(t *testing.T) {
	t.Setenv("ORCHESTRATOR_STALL_TIMEOUT", "10m")
	got := stallThreshold()
	if got != 10*time.Minute {
		t.Errorf("want 10m, got %s", got)
	}
}

// TestStallThresholdInvalidEnvVar verifies an unparseable env var falls back to the default.
func TestStallThresholdInvalidEnvVar(t *testing.T) {
	t.Setenv("ORCHESTRATOR_STALL_TIMEOUT", "notaduration")
	got := stallThreshold()
	if got != 5*time.Minute {
		t.Errorf("want 5m default on bad env var, got %s", got)
	}
}

// TestStallThresholdZeroEnvVar verifies a zero-value duration falls back to the default.
func TestStallThresholdZeroEnvVar(t *testing.T) {
	t.Setenv("ORCHESTRATOR_STALL_TIMEOUT", "0s")
	got := stallThreshold()
	if got != 5*time.Minute {
		t.Errorf("want 5m default on zero duration, got %s", got)
	}
}

// TestWatchStallFiresOnStall verifies the watchdog calls cancel when no output has
// arrived within the threshold.
func TestWatchStallFiresOnStall(t *testing.T) {
	threshold := 50 * time.Millisecond

	tr := &SubprocessTransport{
		done: make(chan struct{}),
	}
	// Set lastOutputTime well before the threshold so the watchdog fires immediately.
	tr.lastOutputTime.Store(time.Now().Add(-threshold * 2).UnixNano())

	cancelled := make(chan struct{})
	cancel := func() { close(cancelled) }

	go tr.watchStall(cancel, threshold)

	select {
	case <-cancelled:
		// good
	case <-time.After(5 * time.Second):
		t.Fatal("watchdog did not fire within 5s; expected stall cancellation")
	}
}

// TestWatchStallDoesNotFireWithActivity verifies the watchdog does NOT cancel when
// output keeps arriving before the threshold expires.
func TestWatchStallDoesNotFireWithActivity(t *testing.T) {
	threshold := 200 * time.Millisecond

	tr := &SubprocessTransport{
		done: make(chan struct{}),
	}
	tr.lastOutputTime.Store(time.Now().UnixNano())

	cancelCalled := false
	cancel := func() { cancelCalled = true }

	go tr.watchStall(cancel, threshold)

	// Refresh lastOutputTime every 50ms — well within the 200ms threshold.
	ticker := time.NewTicker(50 * time.Millisecond)
	defer ticker.Stop()
	deadline := time.After(400 * time.Millisecond)
	for {
		select {
		case <-ticker.C:
			tr.lastOutputTime.Store(time.Now().UnixNano())
		case <-deadline:
			close(tr.done) // shut down watchdog
			time.Sleep(20 * time.Millisecond)
			if cancelCalled {
				t.Error("watchdog fired cancel despite fresh output")
			}
			return
		}
	}
}

// TestWatchStallExitsOnDone verifies the watchdog exits cleanly when t.done is closed.
func TestWatchStallExitsOnDone(t *testing.T) {
	threshold := 10 * time.Minute // very long — should never fire during this test

	tr := &SubprocessTransport{
		done: make(chan struct{}),
	}
	tr.lastOutputTime.Store(time.Now().UnixNano())

	exited := make(chan struct{})
	go func() {
		tr.watchStall(func() {}, threshold)
		close(exited)
	}()

	close(tr.done)

	select {
	case <-exited:
		// good
	case <-time.After(2 * time.Second):
		t.Fatal("watchStall goroutine did not exit after t.done was closed")
	}
}

// TestLastOutputTimeUpdatedByReadStdout verifies that readStdout bumps lastOutputTime
// when a non-empty line arrives.
func TestLastOutputTimeUpdatedByReadStdout(t *testing.T) {
	pr, pw := io.Pipe()

	tr := &SubprocessTransport{
		stdout:   pr,
		messages: make(chan []byte, 10),
		done:     make(chan struct{}),
	}

	before := time.Now().UnixNano()
	tr.lastOutputTime.Store(before)

	go tr.readStdout()

	// Write one NDJSON line, then close the writer so readStdout reaches EOF.
	pw.Write([]byte("{\"type\":\"assistant\"}\n"))
	pw.Close()

	// Drain the message so the goroutine can proceed past the send.
	select {
	case <-tr.messages:
	case <-time.After(time.Second):
		t.Fatal("no message received from readStdout")
	}

	// Wait for readStdout to finish (it closes messages on return).
	select {
	case _, ok := <-tr.messages:
		if ok {
			t.Fatal("unexpected extra message")
		}
		// channel closed — readStdout returned
	case <-time.After(time.Second):
		t.Fatal("readStdout did not finish after pipe was closed")
	}

	if tr.lastOutputTime.Load() <= before {
		t.Error("lastOutputTime was not updated after receiving a line")
	}
}

// TestCloseSignalsWatchdog verifies that Close() closes t.done, causing a running
// watchdog goroutine to exit rather than leak.
func TestCloseSignalsWatchdog(t *testing.T) {
	threshold := 10 * time.Minute

	tr := &SubprocessTransport{
		done: make(chan struct{}),
	}
	tr.lastOutputTime.Store(time.Now().UnixNano())

	exited := make(chan struct{})
	go func() {
		tr.watchStall(func() {}, threshold)
		close(exited)
	}()

	tr.Close()

	select {
	case <-exited:
		// good
	case <-time.After(2 * time.Second):
		t.Fatal("watchStall did not exit after Close()")
	}
}

func TestCommandEnvIncludesExplicitEffortLevel(t *testing.T) {
	t.Setenv("CLAUDE_CODE_EFFORT_LEVEL", "")
	env := commandEnv(&AgentOptions{EffortLevel: "high"})

	found := false
	for _, kv := range env {
		if kv == "CLAUDE_CODE_EFFORT_LEVEL=high" {
			found = true
			break
		}
	}
	if !found {
		t.Fatal("CLAUDE_CODE_EFFORT_LEVEL=high not found in command env")
	}
}

func TestCommandEnvDoesNotLeakArbitraryVars(t *testing.T) {
	t.Setenv("SHOULD_NOT_LEAK", "secret")
	env := commandEnv(&AgentOptions{EffortLevel: "medium"})

	for _, kv := range env {
		if strings.HasPrefix(kv, "SHOULD_NOT_LEAK=") {
			t.Fatal("commandEnv leaked a non-allowlisted variable")
		}
	}
}
