package sdk

// Wire schema: shared/artifacts/b2-stream-json-wire-schema.md
// Empirically verified against Claude CLI v2.1.118.
// Bidirectional stream-json mode requires --input-format stream-json alongside
// --output-format stream-json --print --verbose on the CLI invocation.

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os/exec"
	"sync"
	"sync/atomic"
	"syscall"
	"time"
)

// ---- stdin wire types -------------------------------------------------------

// queryUserEnvelope is the accepted user-message shape (§3a of wire schema).
// The short-form {"type":"user","content":"..."} is rejected by the CLI.
type queryUserEnvelope struct {
	Type    string         `json:"type"`
	Message queryUserInner `json:"message"`
}

type queryUserInner struct {
	Role    string      `json:"role"`
	Content []queryText `json:"content"`
}

type queryText struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

// queryCtrlReq is the interrupt control_request shape (§3b of wire schema).
// The nested `request` object is required; the CLI rejects the envelope if absent.
type queryCtrlReq struct {
	Type      string        `json:"type"`
	Request   queryCtrlBody `json:"request"`
	RequestID string        `json:"request_id,omitempty"`
}

type queryCtrlBody struct {
	Subtype string `json:"subtype"`
}

// ---- stdout wire types ------------------------------------------------------

// queryCtrlResp is the control_response envelope (§4e of wire schema).
type queryCtrlResp struct {
	Type     string            `json:"type"`
	Response queryCtrlRespBody `json:"response"`
}

type queryCtrlRespBody struct {
	Subtype   string `json:"subtype"`
	RequestID string `json:"request_id,omitempty"`
	Error     string `json:"error,omitempty"`
}

// ---- request-ID counter -----------------------------------------------------

var queryReqCounter int64

func nextQueryReqID() string {
	return fmt.Sprintf("req-%d", atomic.AddInt64(&queryReqCounter, 1))
}

// ---- Query ------------------------------------------------------------------

// Query is a long-lived, bidirectional Claude CLI session using stream-json mode.
// Callers write prompts via Send, consume typed events via Messages, and may signal
// the current turn to stop via Interrupt. Close shuts the subprocess down.
//
// Wire schema: shared/artifacts/b2-stream-json-wire-schema.md
type Query struct {
	cmd      *exec.Cmd
	stdin    io.WriteCloser
	stdinMu  sync.Mutex // serializes concurrent writes from Send and Interrupt
	messages chan *StreamedEvent

	mu      sync.Mutex
	pending map[string]chan queryCtrlRespBody

	done      chan struct{}
	closeOnce sync.Once
}

// NewQuery spawns the Claude CLI in bidirectional stream-json mode and returns
// a Query handle ready to accept Send calls. The subprocess is started
// immediately; call Close when done to release resources.
//
// Returns ErrConflictingResumeFlags if both ContinueConversation and
// ResumeSessionID are set.
func NewQuery(ctx context.Context, opts *AgentOptions) (*Query, error) {
	if opts != nil && opts.ContinueConversation && opts.ResumeSessionID != "" {
		return nil, ErrConflictingResumeFlags
	}

	cliPath := "claude"
	if opts != nil && opts.CLIPath != "" {
		cliPath = opts.CLIPath
	}

	// Bidirectional stream-json requires --input-format stream-json in addition
	// to the standard output flags. See §1 of the wire schema artifact.
	args := []string{
		"--output-format", "stream-json",
		"--input-format", "stream-json",
		"--print",
		"--verbose",
		"--include-partial-messages",
		"--dangerously-skip-permissions",
	}

	if opts != nil {
		if opts.Model != "" {
			args = append(args, "--model", opts.Model)
		}
		if opts.MaxTurns > 0 {
			args = append(args, "--max-turns", fmt.Sprintf("%d", opts.MaxTurns))
		}
		if opts.SystemPrompt != "" {
			args = append(args, "--system-prompt", opts.SystemPrompt)
		}
		for _, dir := range opts.AddDirs {
			args = append(args, "--add-dir", dir)
		}
		if opts.ContinueConversation {
			args = append(args, "--continue")
		}
		if opts.ResumeSessionID != "" {
			args = append(args, "--resume", opts.ResumeSessionID, "--fork-session")
		}
	}

	cmd := exec.CommandContext(ctx, cliPath, args...)
	if opts != nil && opts.Cwd != "" {
		cmd.Dir = opts.Cwd
	}
	cmd.Env = commandEnv(opts)
	// Spawn claude in its own process group so the launcher can deliver
	// SIGTERM to the whole group on teardown. Without Setpgid the claude
	// grandchild gets reparented to PID 1 when nanika tui-server is
	// SIGKILLed, leaking a long-lived process that holds cookies, env, and
	// any in-flight tool state. See impl-smoke.md Step 9 for the trace
	// (PIDs 30101, 31667, 32030 reproducing the leak).
	setProcessGroup(cmd)

	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, fmt.Errorf("stdin pipe: %w", err)
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, fmt.Errorf("stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return nil, fmt.Errorf("stderr pipe: %w", err)
	}
	if err := cmd.Start(); err != nil {
		return nil, fmt.Errorf("start claude: %w", err)
	}

	q := &Query{
		cmd:      cmd,
		stdin:    stdin,
		messages: make(chan *StreamedEvent, 100),
		pending:  make(map[string]chan queryCtrlRespBody),
		done:     make(chan struct{}),
	}

	var wg sync.WaitGroup
	wg.Add(2)

	go func() {
		defer wg.Done()
		q.pump(stdout)
	}()
	go func() {
		defer wg.Done()
		q.drainStderr(stderr)
	}()

	// Reap the child once both pipes are drained so it never becomes a zombie.
	go func() {
		wg.Wait()
		cmd.Wait() //nolint:errcheck // exit status captured by pump via channel close
	}()

	return q, nil
}

// Send writes a user message to the CLI's stdin.
// The wire format is the full nested message object (§3a); the short-form is
// rejected by the CLI with a JS TypeError.
func (q *Query) Send(text string) error {
	env := queryUserEnvelope{
		Type: "user",
		Message: queryUserInner{
			Role:    "user",
			Content: []queryText{{Type: "text", Text: text}},
		},
	}
	data, err := json.Marshal(env)
	if err != nil {
		return fmt.Errorf("marshal user message: %w", err)
	}
	data = append(data, '\n')
	q.stdinMu.Lock()
	_, err = q.stdin.Write(data)
	q.stdinMu.Unlock()
	if err != nil {
		return fmt.Errorf("write user message: %w", err)
	}
	return nil
}

// Messages returns the read-only channel of typed events produced by the CLI.
// The channel is closed when the subprocess exits or Close is called.
func (q *Query) Messages() <-chan *StreamedEvent {
	return q.messages
}

// Interrupt sends a control_request with subtype=interrupt and waits up to 5 seconds
// for the correlated control_response. Returns an error if the CLI reports a failure,
// the session is already closed, or the 5-second deadline is exceeded.
//
// Wire format: §3b of shared/artifacts/b2-stream-json-wire-schema.md.
// The request_id is echoed back inside response.request_id for correlation (§4e).
func (q *Query) Interrupt(ctx context.Context) error {
	reqID := nextQueryReqID()

	ch := make(chan queryCtrlRespBody, 1)
	q.mu.Lock()
	q.pending[reqID] = ch
	q.mu.Unlock()

	defer func() {
		q.mu.Lock()
		delete(q.pending, reqID)
		q.mu.Unlock()
	}()

	req := queryCtrlReq{
		Type:      "control_request",
		Request:   queryCtrlBody{Subtype: "interrupt"},
		RequestID: reqID,
	}
	data, err := json.Marshal(req)
	if err != nil {
		return fmt.Errorf("marshal control_request: %w", err)
	}
	data = append(data, '\n')
	q.stdinMu.Lock()
	_, err = q.stdin.Write(data)
	q.stdinMu.Unlock()
	if err != nil {
		return fmt.Errorf("write control_request: %w", err)
	}

	timer := time.NewTimer(5 * time.Second)
	defer timer.Stop()

	select {
	case resp := <-ch:
		if resp.Subtype == "error" {
			return fmt.Errorf("interrupt: %s", resp.Error)
		}
		return nil
	case <-timer.C:
		return fmt.Errorf("interrupt: no response within 5s")
	case <-ctx.Done():
		return ctx.Err()
	case <-q.done:
		return fmt.Errorf("interrupt: session closed")
	}
}

// Close shuts down the subprocess and frees internal resources.
//
// Shutdown order:
//
//  1. Signal `q.done` and close stdin — this is the friendly shutdown
//     path; claude is supposed to drain pending writes and exit.
//  2. After a short grace window, syscall.Kill(-pgid, SIGTERM) so any
//     subprocess claude itself spawned (MCP servers, tool helpers) also
//     dies. Without this, smoke testing showed claude grandchildren
//     reparenting to PID 1 (impl-smoke.md Step 9, PIDs 30101/31667/32030).
//  3. SIGKILL the group as a final backstop.
//
// Close is safe to call multiple times.
func (q *Query) Close() error {
	q.closeOnce.Do(func() {
		close(q.done)
		if q.stdin != nil {
			q.stdin.Close()
		}
		// Best-effort group teardown. We do not Wait here — pump's reaper
		// goroutine already calls cmd.Wait. The signals are advisory:
		// claude that has already exited will not see them.
		go q.killGroupAfter(250 * time.Millisecond)
	})
	return nil
}

// Pid returns the spawned claude process's PID, or 0 when the subprocess
// is not running. Exposed so the launcher can include it in shutdown logs
// or address it from a signal handler.
func (q *Query) Pid() int {
	if q.cmd == nil || q.cmd.Process == nil {
		return 0
	}
	return q.cmd.Process.Pid
}

// killGroupAfter waits for grace so claude can drain its stdin pipe and
// exit naturally, then SIGTERMs the process group, and finally SIGKILLs
// it after another grace window. Both signals are best-effort: a kill on
// an already-dead pgid returns ESRCH which we ignore. The shutdown path
// cannot surface errors anyway.
func (q *Query) killGroupAfter(grace time.Duration) {
	pid := q.Pid()
	if pid <= 0 {
		return
	}
	time.Sleep(grace)
	_ = killProcessGroup(pid, syscall.SIGTERM)
	time.Sleep(grace)
	_ = killProcessGroup(pid, syscall.SIGKILL)
}

// pump is the goroutine-owned stdout reader. It routes control_response lines to
// the pending correlation map and all other lines to the messages channel.
func (q *Query) pump(stdout io.ReadCloser) {
	defer close(q.messages)
	defer stdout.Close()

	scanner := bufio.NewScanner(stdout)
	buf := make([]byte, 0, 10*1024*1024)
	scanner.Buffer(buf, 10*1024*1024)

	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}

		// Peek at the type field to decide routing without a full decode.
		var peek struct {
			Type string `json:"type"`
		}
		if json.Unmarshal(line, &peek) != nil {
			continue
		}

		if peek.Type == "control_response" {
			var resp queryCtrlResp
			if json.Unmarshal(line, &resp) != nil {
				continue
			}
			q.mu.Lock()
			ch, ok := q.pending[resp.Response.RequestID]
			q.mu.Unlock()
			if ok {
				select {
				case ch <- resp.Response:
				default:
				}
			}
			continue
		}

		msg := parseMessageBestEffort(line)
		if msg == nil {
			continue
		}
		for _, ev := range extractEvents(msg) {
			select {
			case q.messages <- ev:
			case <-q.done:
				return
			}
		}
	}
}

// drainStderr reads stderr to prevent the subprocess from blocking on a full
// pipe buffer. Output is discarded; diagnostics are available via cmd.Wait.
func (q *Query) drainStderr(stderr io.ReadCloser) {
	defer stderr.Close()
	scanner := bufio.NewScanner(stderr)
	for scanner.Scan() {
		// intentionally discarded
	}
}
