package sdk

import (
	"bufio"
	"context"
	"fmt"
	"io"
	"os"
	"os/exec"
	"strings"
	"sync"
	"sync/atomic"
	"time"
)

// baseAllowedEnvVars is the fixed set of environment variables passed to
// Claude CLI subprocesses. Everything else is stripped to prevent credential
// leakage. Skills may declare additional vars via AgentOptions.AllowedEnvVars.
var baseAllowedEnvVars = map[string]bool{
	"HOME":   true,
	"PATH":   true,
	"LANG":   true,
	"TERM":   true,
	"USER":   true,
	"SHELL":  true,
	"TMPDIR": true,
}

// filteredEnv returns an environment slice containing only allowed variables.
// allowed is the set of extra var names (e.g. skill-declared) beyond the base set.
func filteredEnv(allowed []string) []string {
	extra := make(map[string]bool, len(allowed))
	for _, v := range allowed {
		extra[v] = true
	}

	var env []string
	for _, kv := range os.Environ() {
		key, _, _ := strings.Cut(kv, "=")
		if baseAllowedEnvVars[key] || extra[key] {
			env = append(env, kv)
		}
	}
	env = append(env, "CLAUDE_CODE_ENTRYPOINT=orchestrator-cli")
	return env
}

// commandEnv returns the sanitized subprocess environment plus explicit runtime overrides.
// Explicit overrides are appended directly so callers do not need the variable to exist
// in the parent shell.
func commandEnv(opts *AgentOptions) []string {
	var extraVars []string
	if opts != nil {
		extraVars = opts.AllowedEnvVars
	}
	env := filteredEnv(extraVars)
	if opts != nil && opts.EffortLevel != "" {
		env = append(env, "CLAUDE_CODE_EFFORT_LEVEL="+opts.EffortLevel)
	}
	if opts != nil && len(opts.AddDirs) > 0 {
		env = append(env, "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1")
	}
	return env
}

// SubprocessTransport communicates with Claude CLI via subprocess
type SubprocessTransport struct {
	cmd            *exec.Cmd
	stdin          io.WriteCloser
	stdout         io.ReadCloser
	stderr         io.ReadCloser
	messages       chan []byte
	errors         chan error
	done           chan struct{}
	writeMu        sync.Mutex
	closed         bool
	exitError      error
	stderrBuf      []byte
	lastOutputTime atomic.Int64    // unix nanos; updated on every stdout line received
	stallCancel    context.CancelFunc // cancels the subprocess context on stall detection
}

// stallThreshold returns the stall detection timeout. It reads ORCHESTRATOR_STALL_TIMEOUT
// as a Go duration string (e.g. "10m", "300s"). Defaults to 5 minutes.
func stallThreshold() time.Duration {
	if v := os.Getenv("ORCHESTRATOR_STALL_TIMEOUT"); v != "" {
		if d, err := time.ParseDuration(v); err == nil && d > 0 {
			return d
		}
	}
	return 5 * time.Minute
}

// watchStall polls lastOutputTime and calls cancel if no output arrives within threshold.
// It exits when t.done is closed (subprocess finished or Close() called).
func (t *SubprocessTransport) watchStall(cancel context.CancelFunc, threshold time.Duration) {
	// Poll at threshold/2, capped at 30s. For the default 5-minute threshold this
	// gives a 30s poll interval (~10 checks per window). For very short thresholds
	// (e.g. in tests) we poll at threshold/2 to keep precision without spinning.
	interval := threshold / 2
	if interval > 30*time.Second {
		interval = 30 * time.Second
	}

	ticker := time.NewTicker(interval)
	defer ticker.Stop()

	for {
		select {
		case <-ticker.C:
			last := time.Unix(0, t.lastOutputTime.Load())
			if time.Since(last) > threshold {
				fmt.Fprintf(os.Stderr, "watchdog: no worker output for %s, cancelling\n", threshold)
				cancel()
				return
			}
		case <-t.done:
			return
		}
	}
}

// Start starts the Claude CLI subprocess.
// If opts.ResumeSessionID is set, it injects --resume <id> --fork-session flags.
// If the subprocess fails to start with those flags, it falls back to a fresh start.
func (t *SubprocessTransport) Start(ctx context.Context, opts *AgentOptions) error {
	resumeID := ""
	if opts != nil {
		resumeID = opts.ResumeSessionID
	}
	if err := t.doStart(ctx, opts, resumeID); err != nil {
		if resumeID != "" {
			fmt.Fprintf(os.Stderr, "[transport] session resume %s failed (%v); retrying without resume\n", resumeID, err)
			return t.doStart(ctx, opts, "")
		}
		return err
	}
	return nil
}

// doStart is the inner implementation of Start. resumeID, when non-empty, appends
// --resume <resumeID> --fork-session to the subprocess args.
func (t *SubprocessTransport) doStart(ctx context.Context, opts *AgentOptions, resumeID string) error {
	cliPath := "claude"
	if opts != nil && opts.CLIPath != "" {
		cliPath = opts.CLIPath
	}

	args := []string{
		"--output-format", "stream-json",
		"--print",
		"--verbose",
		"--include-partial-messages",
	}

	if opts != nil {
		if opts.Model != "" {
			args = append(args, "--model", opts.Model)
		}
		if opts.MaxTurns > 0 {
			args = append(args, "--max-turns", fmt.Sprintf("%d", opts.MaxTurns))
		}
		if opts.PermissionMode == "bypass" {
			args = append(args, "--dangerously-skip-permissions")
		}
		if opts.SystemPrompt != "" {
			args = append(args, "--system-prompt", opts.SystemPrompt)
		}
	}

	if resumeID != "" {
		args = append(args, "--resume", resumeID, "--fork-session")
	}

	threshold := stallThreshold()
	if opts != nil && opts.Timeout > 0 {
		threshold = opts.Timeout
	}
	stallCtx, stallCancel := context.WithCancel(ctx)
	t.stallCancel = stallCancel
	t.lastOutputTime.Store(time.Now().UnixNano())

	// Cancel stallCtx if doStart returns an error so the context is never leaked.
	started := false
	defer func() {
		if !started {
			stallCancel()
		}
	}()

	t.cmd = exec.CommandContext(stallCtx, cliPath, args...)

	if opts != nil && opts.Cwd != "" {
		t.cmd.Dir = opts.Cwd
	}

	// Only pass allowlisted env vars to prevent credential leakage, plus
	// explicit runtime overrides like effort level.
	t.cmd.Env = commandEnv(opts)

	var err error
	t.stdin, err = t.cmd.StdinPipe()
	if err != nil {
		return fmt.Errorf("stdin pipe: %w", err)
	}
	t.stdout, err = t.cmd.StdoutPipe()
	if err != nil {
		return fmt.Errorf("stdout pipe: %w", err)
	}
	t.stderr, err = t.cmd.StderrPipe()
	if err != nil {
		return fmt.Errorf("stderr pipe: %w", err)
	}

	if err := t.cmd.Start(); err != nil {
		return fmt.Errorf("start claude: %w", err)
	}
	started = true

	t.messages = make(chan []byte, 100)
	t.errors = make(chan error, 10)
	t.done = make(chan struct{})

	go t.readStdout()
	go t.readStderr()
	go t.watchStall(stallCancel, threshold)

	return nil
}

func (t *SubprocessTransport) readStdout() {
	defer close(t.messages)

	scanner := bufio.NewScanner(t.stdout)
	buf := make([]byte, 0, 10*1024*1024)
	scanner.Buffer(buf, 10*1024*1024)

	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		msg := make([]byte, len(line))
		copy(msg, line)
		t.lastOutputTime.Store(time.Now().UnixNano())

		select {
		case t.messages <- msg:
		case <-t.done:
			return
		}
	}
}

func (t *SubprocessTransport) readStderr() {
	scanner := bufio.NewScanner(t.stderr)
	for scanner.Scan() {
		t.stderrBuf = append(t.stderrBuf, []byte(scanner.Text()+"\n")...)
	}
}

// Close closes the transport
func (t *SubprocessTransport) Close() error {
	t.writeMu.Lock()
	if t.closed {
		t.writeMu.Unlock()
		return nil
	}
	t.closed = true
	t.writeMu.Unlock()

	close(t.done)
	if t.stallCancel != nil {
		t.stallCancel()
	}
	if t.stdin != nil {
		t.stdin.Close()
	}
	if t.stdout != nil {
		t.stdout.Close()
	}
	if t.stderr != nil {
		t.stderr.Close()
	}
	return nil
}

// Wait waits for the process to exit
func (t *SubprocessTransport) Wait() error {
	if t.cmd == nil {
		return nil
	}
	err := t.cmd.Wait()
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			t.exitError = fmt.Errorf("claude exited %d: %s", exitErr.ExitCode(), string(t.stderrBuf))
			return t.exitError
		}
		return err
	}
	return nil
}

// Kill forcefully terminates the process
func (t *SubprocessTransport) Kill() error {
	if t.cmd == nil || t.cmd.Process == nil {
		return nil
	}
	return t.cmd.Process.Kill()
}
