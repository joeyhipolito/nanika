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

// queryInboundCtrlReq is the INBOUND control_request the CLI emits under
// preventive permission mode. subtype=="can_use_tool" carries the tool the
// classifier flagged "ask"; the client answers with a control_response via
// RespondPermission. Wire shape captured in
// internal/tui/shell/testdata/claude-control-protocol/claude-allow.jsonl.
type queryInboundCtrlReq struct {
	Type      string                  `json:"type"`
	RequestID string                  `json:"request_id"`
	Request   queryInboundCtrlReqBody `json:"request"`
}

type queryInboundCtrlReqBody struct {
	Subtype        string          `json:"subtype"`
	ToolName       string          `json:"tool_name"`
	ToolUseID      string          `json:"tool_use_id"`
	Input          json.RawMessage `json:"input"`
	DecisionReason string          `json:"decision_reason"`
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

	done        chan struct{}
	processDone chan struct{}
	groupDone   chan struct{}
	closeOnce   sync.Once
	retireOnce  sync.Once
}

// NewQuery spawns the Claude CLI in bidirectional stream-json mode and returns
// a Query handle ready to accept Send calls. The subprocess is started
// immediately; call Close when done to release resources.
//
// Returns ErrConflictingResumeFlags if both ContinueConversation and
// ResumeSessionID are set.
func NewQuery(ctx context.Context, opts *AgentOptions) (*Query, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
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
	}
	// This is the duplex path: --input-format stream-json above gives the return
	// channel preventive mode needs. The zero value still emits exactly
	// --dangerously-skip-permissions in this same slot, byte-identical to before.
	args = append(args, permissionArgs(opts, true)...)

	args = append(args, queryOptFlags(opts)...)
	if opts != nil {
		if opts.ContinueConversation {
			args = append(args, "--continue")
		}
		if opts.ResumeSessionID != "" {
			args = append(args, "--resume", opts.ResumeSessionID, "--fork-session")
		}
	}

	// Query owns context cancellation itself so it can retire the entire process
	// group. exec.CommandContext only kills the root process and also requires
	// cmd.Wait, whose pipe-closing behavior cannot safely represent our stronger
	// root-reap + reader-drain + group-extinction barrier.
	cmd := exec.Command(cliPath, args...)
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
		cmd:         cmd,
		stdin:       stdin,
		messages:    make(chan *StreamedEvent, 100),
		pending:     make(map[string]chan queryCtrlRespBody),
		done:        make(chan struct{}),
		processDone: make(chan struct{}),
		groupDone:   make(chan struct{}),
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

	// exec.CommandContext only targets the root process. Mirror context
	// cancellation through Close so descendants in Claude's dedicated process
	// group enter the same verified retirement path.
	go func() {
		select {
		case <-ctx.Done():
			_ = q.Close()
		case <-q.processDone:
		}
	}()

	readersDone := make(chan struct{})
	go func() {
		wg.Wait()
		close(readersDone)
	}()

	// Reap the root independently from pipe EOF. A descendant may inherit the
	// stdout/stderr descriptors after Claude exits; waiting for readers first
	// would prevent retirement from ever starting. Once the root is reaped, its
	// group is retired, inherited descriptors reach EOF, and only then does the
	// public barrier close.
	go func() {
		state, _ := cmd.Process.Wait()
		cmd.ProcessState = state
		_ = q.stdin.Close()
		q.beginProcessGroupRetirement()
		<-q.groupDone
		<-readersDone
		close(q.processDone)
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
	if err := q.writeStdin(data); err != nil {
		return fmt.Errorf("write user message: %w", err)
	}
	return nil
}

// Messages returns the read-only channel of typed events produced by the CLI.
// The channel is closed when the subprocess exits or Close is called.
func (q *Query) Messages() <-chan *StreamedEvent {
	return q.messages
}

// ProcessDone is closed only after both output pumps have drained and the
// Claude subprocess has been reaped. On supported Unix platforms it additionally
// proves that Claude's dedicated process group is extinct. Process-group
// extinction does not prove that a descendant which deliberately changed its
// session or process group is gone.
// Close initiates teardown and returns immediately.
func (q *Query) ProcessDone() <-chan struct{} {
	return q.processDone
}

// ProcessGroupRetirementSupported reports whether this build can signal and
// prove extinction of the subprocess's dedicated process group. Unsupported
// platforms still reap the root and drain its pipes. No platform implementation
// in this package claims proof for descendants that escape that group.
func ProcessGroupRetirementSupported() bool {
	return processGroupRetirementSupported()
}

// Interrupt sends a control_request with subtype=interrupt and waits up to five
// seconds for the correlated control_response. An earlier context cancellation
// bounds a blocked stdin write and closes the query so retirement can proceed.
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
	if err := q.writeStdinWithContext(ctx, data); err != nil {
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

// writeStdinWithContext bounds a potentially blocked pipe write. On context
// cancellation it closes the query, which interrupts an os.File pipe write and
// starts the same process-group retirement path used by explicit shutdown.
func (q *Query) writeStdinWithContext(ctx context.Context, data []byte) error {
	select {
	case <-ctx.Done():
		_ = q.Close()
		return ctx.Err()
	case <-q.done:
		if err := ctx.Err(); err != nil {
			return err
		}
		return fmt.Errorf("session closed")
	default:
	}

	writeDone := make(chan error, 1)
	go func() {
		writeDone <- q.writeStdin(data)
	}()

	select {
	case err := <-writeDone:
		return err
	case <-ctx.Done():
		_ = q.Close()
		return ctx.Err()
	case <-q.done:
		if err := ctx.Err(); err != nil {
			return err
		}
		return fmt.Errorf("session closed")
	}
}

func (q *Query) writeStdin(data []byte) error {
	q.stdinMu.Lock()
	n, err := q.stdin.Write(data)
	q.stdinMu.Unlock()
	if err != nil {
		return err
	}
	if n != len(data) {
		return io.ErrShortWrite
	}
	return nil
}

// Close shuts down the subprocess and frees internal resources.
//
// Shutdown order:
//
//  1. Signal `q.done` and close stdin — this is the friendly shutdown
//     path; claude is supposed to drain pending writes and exit.
//  2. On supported Unix platforms, after a short grace window,
//     syscall.Kill(-pgid, SIGTERM) so subprocesses Claude spawned (MCP
//     servers, tool helpers) also die. Without this, smoke testing showed
//     Claude grandchildren reparenting to PID 1.
//  3. On those platforms, SIGKILL the group as a final backstop.
//
// Close is safe to call multiple times.
func (q *Query) Close() error {
	q.closeOnce.Do(func() {
		close(q.done)
		if q.stdin != nil {
			q.stdin.Close()
		}
		q.beginProcessGroupRetirement()
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

const (
	queryFriendlyShutdownGrace = 250 * time.Millisecond
	queryTerminateGrace        = 250 * time.Millisecond
	queryGroupProbeInterval    = 10 * time.Millisecond
)

// beginProcessGroupRetirement starts the single teardown owner for the process
// group. Close and the parent reaper may race to call it; retireOnce prevents
// duplicate signal sequences against the same group.
func (q *Query) beginProcessGroupRetirement() {
	q.retireOnce.Do(func() {
		go q.retireProcessGroup()
	})
}

// retireProcessGroup gives the friendly stdin-close path time to finish, then
// escalates to SIGTERM and SIGKILL. groupDone closes only after signal 0 reports
// ESRCH for the dedicated Unix process group. If the group cannot be retired, this
// goroutine intentionally keeps groupDone (and therefore ProcessDone) open: a
// caller must never mistake a still-capable process-group member for a completed
// process-group cancellation barrier.
func (q *Query) retireProcessGroup() {
	if !processGroupRetirementSupported() {
		// Unsupported platforms still own and terminate the root process.
		if q.cmd != nil && q.cmd.Process != nil {
			_ = q.cmd.Process.Kill()
		}
		close(q.groupDone)
		return
	}

	pid := q.Pid()
	if pid <= 0 || waitForProcessGroupExtinction(pid, 0) {
		close(q.groupDone)
		return
	}

	if waitForProcessGroupExtinction(pid, queryFriendlyShutdownGrace) {
		close(q.groupDone)
		return
	}

	_ = killProcessGroup(pid, syscall.SIGTERM)
	if waitForProcessGroupExtinction(pid, queryTerminateGrace) {
		close(q.groupDone)
		return
	}

	_ = killProcessGroup(pid, syscall.SIGKILL)
	for !waitForProcessGroupExtinction(pid, time.Second) {
		// Fail closed. SIGKILL is retried in case the first signal raced with a
		// just-forked group member; ProcessDone stays open until ESRCH.
		_ = killProcessGroup(pid, syscall.SIGKILL)
	}
	close(q.groupDone)
}

// waitForProcessGroupExtinction polls the process-group existence probe for up
// to timeout. A zero timeout is a single non-blocking probe.
func waitForProcessGroupExtinction(pid int, timeout time.Duration) bool {
	if processGroupExtinct(pid) {
		return true
	}
	if timeout <= 0 {
		return false
	}

	deadline := time.Now().Add(timeout)
	for {
		remaining := time.Until(deadline)
		if remaining <= 0 {
			return processGroupExtinct(pid)
		}
		pause := queryGroupProbeInterval
		if remaining < pause {
			pause = remaining
		}
		time.Sleep(pause)
		if processGroupExtinct(pid) {
			return true
		}
	}
}

// RespondPermission answers a KindPermissionRequest by writing a control_response
// back through the same stdin+mutex the Send/Interrupt paths use. reqID must be
// the PermissionRequest.RequestID being answered.
//
// Allow → {"behavior":"allow"} (with "updatedInput" only when d.UpdatedInput is
// non-nil — the CLI keeps the original input when it is omitted). Deny →
// {"behavior":"deny","message":d.Message}, surfaced to the model as the
// tool_result error. Wire shape: claude-allow.jsonl / claude-deny.jsonl.
func (q *Query) RespondPermission(reqID string, d PermissionDecision) error {
	var inner map[string]any
	if d.Allow {
		inner = map[string]any{"behavior": "allow"}
		if d.UpdatedInput != nil {
			inner["updatedInput"] = json.RawMessage(d.UpdatedInput)
		}
	} else {
		inner = map[string]any{"behavior": "deny", "message": d.Message}
	}
	env := map[string]any{
		"type": "control_response",
		"response": map[string]any{
			"subtype":    "success",
			"request_id": reqID,
			"response":   inner,
		},
	}
	data, err := json.Marshal(env)
	if err != nil {
		return fmt.Errorf("marshal control_response: %w", err)
	}
	data = append(data, '\n')
	if err := q.writeStdin(data); err != nil {
		return fmt.Errorf("write control_response: %w", err)
	}
	return nil
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

		if peek.Type == "control_request" {
			// Preventive permission mode: the CLI is asking whether a tool may
			// run. Surface it as an event and keep reading — the consumer answers
			// asynchronously via RespondPermission. Blocking the pump here would
			// deadlock Interrupt (its control_response could never be read).
			var req queryInboundCtrlReq
			if json.Unmarshal(line, &req) == nil && req.Request.Subtype == "can_use_tool" {
				select {
				case q.messages <- &StreamedEvent{
					Kind: KindPermissionRequest,
					Permission: &PermissionRequest{
						RequestID: req.RequestID,
						ToolName:  req.Request.ToolName,
						ToolUseID: req.Request.ToolUseID,
						Input:     req.Request.Input,
						Reason:    req.Request.DecisionReason,
					},
				}:
				case <-q.done:
					return
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
