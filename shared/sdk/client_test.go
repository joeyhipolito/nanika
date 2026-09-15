package sdk

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"
)

type blockingWriteCloser struct {
	started   chan struct{}
	unblocked chan struct{}
	startOnce sync.Once
	closeOnce sync.Once
}

func newBlockingWriteCloser() *blockingWriteCloser {
	return &blockingWriteCloser{
		started:   make(chan struct{}),
		unblocked: make(chan struct{}),
	}
}

func (w *blockingWriteCloser) Write([]byte) (int, error) {
	w.startOnce.Do(func() { close(w.started) })
	<-w.unblocked
	return 0, errors.New("writer closed")
}

func (w *blockingWriteCloser) Close() error {
	w.closeOnce.Do(func() { close(w.unblocked) })
	return nil
}

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

func TestQuery_InterruptContextClosesBlockedStdinWrite(t *testing.T) {
	stdin := newBlockingWriteCloser()
	q := &Query{
		stdin:       stdin,
		messages:    make(chan *StreamedEvent),
		pending:     make(map[string]chan queryCtrlRespBody),
		done:        make(chan struct{}),
		processDone: make(chan struct{}),
		groupDone:   make(chan struct{}),
	}

	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	interruptDone := make(chan error, 1)
	go func() {
		interruptDone <- q.Interrupt(ctx)
	}()

	select {
	case <-stdin.started:
	case <-time.After(time.Second):
		t.Fatal("interrupt did not begin the blocking stdin write")
	}

	select {
	case err := <-interruptDone:
		if !errors.Is(err, context.DeadlineExceeded) {
			t.Fatalf("Interrupt error = %v, want context deadline", err)
		}
	case <-time.After(time.Second):
		t.Fatal("Interrupt was not bounded by its context")
	}

	select {
	case <-q.done:
	default:
		t.Fatal("timed-out interrupt did not initiate query retirement")
	}
}

// ---------------------------------------------------------------------------
// TestQuery_SubprocessReaped — unit test
// Spawns a real subprocess via NewQuery using `true` (always exits 0) so that
// root process reaping is exercised. After Close we verify the process is gone.
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
	select {
	case <-q.ProcessDone():
	case <-time.After(time.Second):
		t.Fatal("ProcessDone did not close after subprocess teardown")
	}

	// ProcessDone is the public barrier; independently probe the process table to
	// verify that the barrier does not close before Process.Wait has reaped the child.
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

// TestQuery_ProcessDoneWaitsForProcessGroupExtinction is a regression for the
// cancellation barrier. The fake CLI exits after starting a TERM-resistant
// grandchild. That grandchild waits for a release file before attempting a
// delayed marker write. If ProcessDone closes after only cmd.Wait (the old
// behavior), the test releases a still-live grandchild and observes the write.
// With the process-group barrier, release happens only after SIGKILL + ESRCH.
func TestQuery_ProcessDoneWaitsForProcessGroupExtinction(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("Unix process-group semantics")
	}

	dir := t.TempDir()
	scriptPath := filepath.Join(dir, "stubborn-cli.sh")
	readyPath := filepath.Join(dir, "ready")
	releasePath := filepath.Join(dir, "release")
	markerPath := filepath.Join(dir, "marker")
	const script = `#!/bin/sh
dir=$(CDPATH= cd "$(dirname "$0")" && pwd)
(
  trap '' HUP TERM
  exec </dev/null
  : > "$dir/ready"
  while [ ! -f "$dir/release" ]; do
    sleep 0.01
  done
  sleep 0.05
  : > "$dir/marker"
) &
while [ ! -f "$dir/ready" ]; do
  sleep 0.01
done
exit 0
`
	if err := os.WriteFile(scriptPath, []byte(script), 0o700); err != nil {
		t.Fatalf("write fake CLI: %v", err)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	q, err := NewQuery(ctx, &AgentOptions{CLIPath: scriptPath})
	if err != nil {
		t.Fatalf("NewQuery: %v", err)
	}
	defer q.Close()

	readyDeadline := time.Now().Add(2 * time.Second)
	for {
		if _, err := os.Stat(readyPath); err == nil {
			break
		} else if !os.IsNotExist(err) {
			t.Fatalf("stat ready file: %v", err)
		}
		if time.Now().After(readyDeadline) {
			t.Fatal("stubborn grandchild did not report ready")
		}
		time.Sleep(10 * time.Millisecond)
	}

	q.Close()
	select {
	case <-q.ProcessDone():
	case <-time.After(3 * time.Second):
		t.Fatal("ProcessDone did not close after process-group retirement")
	}
	if !processGroupExtinct(q.Pid()) {
		t.Fatal("ProcessDone closed before the Unix process group was extinct")
	}

	if err := os.WriteFile(releasePath, []byte("release\n"), 0o600); err != nil {
		t.Fatalf("release grandchild: %v", err)
	}
	time.Sleep(300 * time.Millisecond)
	if _, err := os.Stat(markerPath); err == nil {
		t.Fatal("grandchild wrote a marker after ProcessDone closed")
	} else if !os.IsNotExist(err) {
		t.Fatalf("stat marker file: %v", err)
	}
}

// newLineScanner returns a bufio.Scanner over r for the pipe drain helper.
func newLineScanner(r io.Reader) interface {
	Scan() bool
	Text() string
} {
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
