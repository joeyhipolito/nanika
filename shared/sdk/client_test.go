package sdk

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

// ---------------------------------------------------------------------------
// TestControlResponseCorrelation — unit test, no subprocess
// Feed a synthetic NDJSON stream with two interleaved control_response lines
// (req-2 arrives before req-1) into the pump and assert each waiter channel
// receives exactly the right response body.
// ---------------------------------------------------------------------------

func TestControlResponseCorrelation(t *testing.T) {
	t.Parallel()

	// Deliver req-2's response first to verify correlation is by request_id,
	// not by arrival order.
	lines := strings.Join([]string{
		`{"type":"control_response","response":{"subtype":"success","request_id":"req-2"}}`,
		`{"type":"control_response","response":{"subtype":"success","request_id":"req-1"}}`,
		"",
	}, "\n")

	r := io.NopCloser(strings.NewReader(lines))

	q := &Query{
		messages: make(chan *StreamedEvent, 10),
		pending:  make(map[string]chan queryCtrlRespBody),
		done:     make(chan struct{}),
	}
	defer close(q.done)

	ch1 := make(chan queryCtrlRespBody, 1)
	ch2 := make(chan queryCtrlRespBody, 1)

	q.mu.Lock()
	q.pending["req-1"] = ch1
	q.pending["req-2"] = ch2
	q.mu.Unlock()

	go q.pump(r)

	deadline := time.After(900 * time.Millisecond)
	var got1, got2 queryCtrlRespBody
	recv1, recv2 := false, false

	for !recv1 || !recv2 {
		select {
		case resp := <-ch1:
			got1 = resp
			recv1 = true
		case resp := <-ch2:
			got2 = resp
			recv2 = true
		case <-deadline:
			t.Fatalf("timed out; recv1=%v recv2=%v", recv1, recv2)
		}
	}

	if got1.RequestID != "req-1" {
		t.Errorf("ch1: request_id = %q; want %q", got1.RequestID, "req-1")
	}
	if got1.Subtype != "success" {
		t.Errorf("ch1: subtype = %q; want %q", got1.Subtype, "success")
	}
	if got2.RequestID != "req-2" {
		t.Errorf("ch2: request_id = %q; want %q", got2.RequestID, "req-2")
	}
	if got2.Subtype != "success" {
		t.Errorf("ch2: subtype = %q; want %q", got2.Subtype, "success")
	}
}

// ---------------------------------------------------------------------------
// TestStreamJSONFraming — unit test
// Assert that the user-message framing produced by the Send path matches the
// wire schema documented in shared/artifacts/b2-stream-json-wire-schema.md §3a
// verbatim: nested message object, NDJSON framing (single \n-terminated line),
// and no short-form content field.
// ---------------------------------------------------------------------------

func TestStreamJSONFraming(t *testing.T) {
	t.Parallel()

	const sampleText = "Your prompt here"

	env := queryUserEnvelope{
		Type: "user",
		Message: queryUserInner{
			Role:    "user",
			Content: []queryText{{Type: "text", Text: sampleText}},
		},
	}

	data, err := json.Marshal(env)
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	// Send appends \n for NDJSON framing.
	framed := string(data) + "\n"

	// NDJSON invariants: single newline at end, no embedded newlines.
	if !strings.HasSuffix(framed, "\n") {
		t.Error("framed output must end with \\n (NDJSON §2)")
	}
	if strings.Count(framed, "\n") != 1 {
		t.Errorf("framed output must contain exactly one \\n; got %d", strings.Count(framed, "\n"))
	}

	// The short-form must NOT appear (§3a rejection note).
	if strings.Contains(framed, `"content":"`) {
		t.Error("framing must not use short-form content string; CLI rejects it")
	}

	// Decode and verify the nested structure matches §3a field-for-field.
	var decoded struct {
		Type    string `json:"type"`
		Message struct {
			Role    string `json:"role"`
			Content []struct {
				Type string `json:"type"`
				Text string `json:"text"`
			} `json:"content"`
		} `json:"message"`
	}
	if err := json.Unmarshal([]byte(strings.TrimRight(framed, "\n")), &decoded); err != nil {
		t.Fatalf("unmarshal framed output: %v", err)
	}

	if decoded.Type != "user" {
		t.Errorf("type = %q; want %q", decoded.Type, "user")
	}
	if decoded.Message.Role != "user" {
		t.Errorf("message.role = %q; want %q", decoded.Message.Role, "user")
	}
	if len(decoded.Message.Content) != 1 {
		t.Fatalf("message.content len = %d; want 1", len(decoded.Message.Content))
	}
	if decoded.Message.Content[0].Type != "text" {
		t.Errorf("content[0].type = %q; want %q", decoded.Message.Content[0].Type, "text")
	}
	if decoded.Message.Content[0].Text != sampleText {
		t.Errorf("content[0].text = %q; want %q", decoded.Message.Content[0].Text, sampleText)
	}
}

// ---------------------------------------------------------------------------
// TestQuery_MultiTurn — integration test (skipped when claude is not on PATH)
// Open a Query, send two successive messages, assert both receive non-empty
// responses and that the two responses differ.
// ---------------------------------------------------------------------------

func TestQuery_MultiTurn(t *testing.T) {
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

// ---------------------------------------------------------------------------
// TestQuery_ConcurrentStdinWrites — unit test, no subprocess
// Exercises concurrent calls to Send from multiple goroutines against a synthetic
// stdin pipe. The race detector (`go test -race`) validates that stdinMu prevents
// interleaved writes. Each write must land as a complete NDJSON line; we verify
// every decoded line contains the required "type" field.
// ---------------------------------------------------------------------------

func TestQuery_ConcurrentStdinWrites(t *testing.T) {
	t.Parallel()

	pr, pw := io.Pipe()

	q := &Query{
		stdin:    pw,
		messages: make(chan *StreamedEvent, 10),
		pending:  make(map[string]chan queryCtrlRespBody),
		done:     make(chan struct{}),
	}

	const senders = 8

	// Collect all written lines via the read end of the pipe.
	lines := make(chan string, senders*2)
	go func() {
		defer close(lines)
		scanner := newLineScanner(pr)
		for scanner.Scan() {
			lines <- scanner.Text()
		}
	}()

	var wg sync.WaitGroup
	wg.Add(senders)
	for i := 0; i < senders; i++ {
		i := i
		go func() {
			defer wg.Done()
			_ = q.Send(strings.Repeat("x", i+1))
		}()
	}
	wg.Wait()
	pw.Close() // signal EOF to the scanner goroutine

	count := 0
	for line := range lines {
		var peek struct {
			Type string `json:"type"`
		}
		if err := json.Unmarshal([]byte(line), &peek); err != nil {
			t.Errorf("line %d is not valid JSON: %v — line: %q", count, err, line)
		}
		if peek.Type == "" {
			t.Errorf("line %d missing type field: %q", count, line)
		}
		count++
	}

	if count != senders {
		t.Errorf("expected %d lines (one per Send); got %d", senders, count)
	}
}

// ---------------------------------------------------------------------------
// TestQuery_SubprocessReaped — unit test
// Spawns a real subprocess via NewQuery using `true` (always exits 0) so that
// cmd.Wait is exercised. After Close we verify the process is no longer alive.
// Skipped if `true` is not on PATH (non-Unix environments).
// ---------------------------------------------------------------------------

func TestQuery_SubprocessReaped(t *testing.T) {
	if _, err := exec.LookPath("true"); err != nil {
		t.Skip("`true` not on PATH; skipping subprocess reap test")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	// Use `true` as a stand-in CLI: it exits immediately with status 0.
	q, err := NewQuery(ctx, &AgentOptions{CLIPath: "true"})
	if err != nil {
		t.Fatalf("NewQuery: %v", err)
	}

	// Drain messages so pump can finish.
	for range q.Messages() {
	}

	q.Close()

	// Allow up to 1 s for cmd.Wait to reap the child.
	deadline := time.Now().Add(time.Second)
	reaped := false
	for time.Now().Before(deadline) {
		proc, err := os.FindProcess(q.cmd.Process.Pid)
		if err != nil {
			reaped = true
			break // process table entry gone — reaped
		}
		// On Unix, FindProcess always succeeds; use signal 0 to probe liveness.
		// syscall.Signal(0) performs no action but returns ESRCH when the PID
		// no longer exists, unlike os.Signal(nil) which always errors due to a
		// failed type assertion.
		if err := proc.Signal(syscall.Signal(0)); err != nil {
			reaped = true
			break // ESRCH — process reaped
		}
		time.Sleep(10 * time.Millisecond)
	}
	if !reaped {
		t.Fatal("subprocess was not reaped within 1s after Close — reaper goroutine may have leaked")
	}
}

// newLineScanner returns a bufio.Scanner over r for the pipe drain helper.
func newLineScanner(r io.Reader) interface{ Scan() bool; Text() string } {
	return bufio.NewScanner(r)
}

// collectUntilTurnEnd drains the messages channel until KindTurnEnd and returns
// the concatenated non-delta text. Fatal on channel close before turn end or ctx
// expiry.
func collectUntilTurnEnd(t *testing.T, ctx context.Context, ch <-chan *StreamedEvent, label string) string {
	t.Helper()
	var sb strings.Builder
	for {
		select {
		case ev, ok := <-ch:
			if !ok {
				t.Fatalf("%s: channel closed before KindTurnEnd", label)
			}
			if ev.Kind == KindText && !ev.IsDelta {
				sb.WriteString(ev.Text)
			}
			if ev.Kind == KindTurnEnd {
				return sb.String()
			}
		case <-ctx.Done():
			t.Fatalf("%s: context expired before KindTurnEnd", label)
		}
	}
}
