//go:build integration

package sdk

import (
	"context"
	"os"
	"os/exec"
	"testing"
	"time"
)

// ---------------------------------------------------------------------------
// TestQuery_MultiTurn — integration test (skipped when claude is not on PATH)
// Open a Query, send two successive messages, assert both receive non-empty
// responses and that the two responses differ.
// ---------------------------------------------------------------------------

func TestQuery_MultiTurn(t *testing.T) {
	if os.Getenv("RUN_CLAUDE_INTEGRATION") != "1" {
		t.Skip("set RUN_CLAUDE_INTEGRATION=1 to run the live Claude test")
	}
	if _, err := exec.LookPath("claude"); err != nil {
		t.Skip("claude not on PATH; skipping integration test")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	q, err := NewQuery(ctx, &AgentOptions{PassthroughEnv: true})
	if err != nil {
		t.Fatalf("NewQuery: %v", err)
	}
	defer q.Close()

	if err := q.Send("say hi"); err != nil {
		t.Fatalf("Send 1: %v", err)
	}

	turn1 := collectUntilTurnEnd(t, ctx, q.Messages(), "first turn")

	if err := q.Send("say bye"); err != nil {
		t.Fatalf("Send 2: %v", err)
	}

	turn2 := collectUntilTurnEnd(t, ctx, q.Messages(), "second turn")

	if turn1 == "" {
		t.Error("first turn response is empty")
	}
	if turn2 == "" {
		t.Error("second turn response is empty")
	}
	if turn1 == turn2 {
		t.Errorf("both turns returned identical text %q; expected different responses", turn1)
	}
}

// ---------------------------------------------------------------------------
// TestQuery_Interrupt — integration test (skipped when claude is not on PATH)
// Open a Query, send a long-output prompt, interrupt after the first event
// arrives, assert the turn ends within 10s, then assert a subsequent Send
// still receives a response.
// ---------------------------------------------------------------------------

func TestQuery_Interrupt(t *testing.T) {
	if os.Getenv("RUN_CLAUDE_INTEGRATION") != "1" {
		t.Skip("set RUN_CLAUDE_INTEGRATION=1 to run the live Claude test")
	}
	if _, err := exec.LookPath("claude"); err != nil {
		t.Skip("claude not on PATH; skipping integration test")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()

	q, err := NewQuery(ctx, &AgentOptions{PassthroughEnv: true})
	if err != nil {
		t.Fatalf("NewQuery: %v", err)
	}
	defer q.Close()

	if err := q.Send("list 500 prime numbers"); err != nil {
		t.Fatalf("Send: %v", err)
	}

	// Wait for the first text event before interrupting.
	firstDeadline := time.After(20 * time.Second)
	gotFirst := false
	for !gotFirst {
		select {
		case ev, ok := <-q.Messages():
			if !ok {
				t.Fatal("channel closed before first event arrived")
			}
			if ev.Kind == KindText {
				gotFirst = true
			}
		case <-firstDeadline:
			t.Fatal("timeout waiting for first text event before interrupt")
		}
	}

	intCtx, intCancel := context.WithTimeout(ctx, 10*time.Second)
	defer intCancel()

	if err := q.Interrupt(intCtx); err != nil {
		// Non-fatal: the turn may have ended naturally before the interrupt landed.
		t.Logf("Interrupt returned (non-fatal): %v", err)
	}

	// Assert the turn ends within 10s.
	endDeadline := time.After(10 * time.Second)
	turnEnded := false
	for !turnEnded {
		select {
		case ev, ok := <-q.Messages():
			if !ok {
				turnEnded = true
			} else if ev.Kind == KindTurnEnd {
				turnEnded = true
			}
		case <-endDeadline:
			t.Fatal("turn did not end within 10s after interrupt")
		}
	}

	// A subsequent Send must still get a response.
	if err := q.Send("say OK"); err != nil {
		t.Fatalf("Send after interrupt: %v", err)
	}

	afterDeadline := time.After(30 * time.Second)
	gotAfter := false
	for !gotAfter {
		select {
		case ev, ok := <-q.Messages():
			if !ok {
				gotAfter = true
			} else if ev.Kind == KindTurnEnd {
				gotAfter = true
			}
		case <-afterDeadline:
			t.Fatal("no response after interrupt+send within 30s")
		}
	}
}
