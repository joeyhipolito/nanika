package engine

// Integration-style tests for CodexExecutor.Execute.
//
// All tests use a fake "codex" binary — a shell script written to a temp
// directory — so no real Codex installation is needed. The fake binary emits
// JSONL lines that the executor parses, allowing full end-to-end coverage of
// subprocess argument contracts, session-ID capture, event emission sequence,
// error paths, and context cancellation.

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

// allEventsEmitter captures every emitted event for assertion.
type allEventsEmitter struct {
	mu     sync.Mutex
	events []event.Event
}

func (e *allEventsEmitter) Emit(_ context.Context, ev event.Event) {
	e.mu.Lock()
	e.events = append(e.events, ev)
	e.mu.Unlock()
}

func (e *allEventsEmitter) Close() error { return nil }

func (e *allEventsEmitter) ofType(et event.EventType) []event.Event {
	e.mu.Lock()
	defer e.mu.Unlock()
	var out []event.Event
	for _, ev := range e.events {
		if ev.Type == et {
			out = append(out, ev)
		}
	}
	return out
}

func (e *allEventsEmitter) hasType(et event.EventType) bool {
	return len(e.ofType(et)) > 0
}

// makeFakeCodex writes a /bin/sh script to a temp directory and returns its
// path. The script is chmod 0755. Tests set BinaryPath to this path.
func makeFakeCodex(t *testing.T, script string) string {
	t.Helper()
	path := filepath.Join(t.TempDir(), "codex")
	if err := os.WriteFile(path, []byte("#!/bin/sh\n"+script), 0755); err != nil {
		t.Fatalf("makeFakeCodex: %v", err)
	}
	return path
}

// integConfig returns a minimal WorkerConfig suitable for integration tests.
// The optional apply func can override fields before the config is returned.
func integConfig(t *testing.T, apply func(*core.WorkerConfig)) *core.WorkerConfig {
	t.Helper()
	cfg := &core.WorkerConfig{
		Name:      "integ-worker",
		WorkerDir: t.TempDir(),
		Bundle: core.ContextBundle{
			WorkspaceID: "ws-integ",
			PhaseID:     "ph-integ",
			Objective:   "do the work",
		},
	}
	if apply != nil {
		apply(cfg)
	}
	return cfg
}

// ---------------------------------------------------------------------------
// New-session success path
// ---------------------------------------------------------------------------

// TestExecute_NewSession_SuccessPath exercises the happy path for a new
// session: JSONL is streamed, thread_id is captured as sessionID, text is
// assembled, and the WorkerSpawned → WorkerCompleted event sequence is emitted.
func TestExecute_NewSession_SuccessPath(t *testing.T) {
	script := `
echo '{"thread_id":"t-integ-1","type":"thread.started"}'
echo '{"delta":"hello from codex"}'
echo '{"delta":" and more"}'
echo '{"type":"thread.completed"}'
`
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}
	emitter := &allEventsEmitter{}
	cfg := integConfig(t, nil)

	output, sessionID, cost, err := ex.Execute(context.Background(), cfg, emitter, false)

	if err != nil {
		t.Fatalf("Execute returned unexpected error: %v", err)
	}
	if sessionID != "t-integ-1" {
		t.Errorf("sessionID = %q, want %q", sessionID, "t-integ-1")
	}
	if !strings.Contains(output, "hello from codex") {
		t.Errorf("output missing expected text; got %q", output)
	}
	if cost != nil {
		t.Errorf("cost must be nil for Codex (no CapCostReport), got %+v", cost)
	}
	if !emitter.hasType(event.WorkerSpawned) {
		t.Error("WorkerSpawned event not emitted")
	}
	if !emitter.hasType(event.WorkerCompleted) {
		t.Error("WorkerCompleted event not emitted")
	}
	if emitter.hasType(event.WorkerFailed) {
		t.Error("WorkerFailed must NOT be emitted on success")
	}
}

// ---------------------------------------------------------------------------
// Output file artifact
// ---------------------------------------------------------------------------

// TestExecute_NewSession_OutputFileWritten verifies that Execute writes
// output.md to config.WorkerDir when the subprocess produces text output,
// making it available for artifact collection.
func TestExecute_NewSession_OutputFileWritten(t *testing.T) {
	script := `
echo '{"thread_id":"t-outfile","type":"thread.started"}'
echo '{"delta":"artifact content"}'
`
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}
	cfg := integConfig(t, nil)

	_, _, _, err := ex.Execute(context.Background(), cfg, &allEventsEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute error: %v", err)
	}

	data, readErr := os.ReadFile(filepath.Join(cfg.WorkerDir, "output.md"))
	if readErr != nil {
		t.Fatalf("output.md not written to WorkerDir: %v", readErr)
	}
	if !strings.Contains(string(data), "artifact content") {
		t.Errorf("output.md = %q, want it to contain 'artifact content'", string(data))
	}
}

// ---------------------------------------------------------------------------
// Nonzero exit
// ---------------------------------------------------------------------------

// TestExecute_NonzeroExit_WorkerFailedEmitted verifies that a nonzero exit
// causes WorkerFailed to be emitted and an error wrapping the exit code to be
// returned. WorkerCompleted must NOT be emitted.
func TestExecute_NonzeroExit_WorkerFailedEmitted(t *testing.T) {
	script := `echo 'codex process error' >&2
exit 1`
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}
	emitter := &allEventsEmitter{}
	cfg := integConfig(t, nil)

	_, _, _, err := ex.Execute(context.Background(), cfg, emitter, false)

	if err == nil {
		t.Fatal("expected error for nonzero exit, got nil")
	}
	if !strings.Contains(err.Error(), "codex exited 1") {
		t.Errorf("error = %q, want it to mention 'codex exited 1'", err.Error())
	}
	if !emitter.hasType(event.WorkerFailed) {
		t.Error("WorkerFailed event not emitted on nonzero exit")
	}
	if emitter.hasType(event.WorkerCompleted) {
		t.Error("WorkerCompleted must NOT be emitted on nonzero exit")
	}
}

// ---------------------------------------------------------------------------
// Argument contract: new session via subprocess
// ---------------------------------------------------------------------------

// TestExecute_NewSession_ArgumentContract verifies that Execute passes the
// correct argument shape to the subprocess for a new-session invocation.
// The fake binary records its received args to a file for assertion.
func TestExecute_NewSession_ArgumentContract(t *testing.T) {
	argsFile := filepath.Join(t.TempDir(), "args.txt")
	// printf "%s\n" "$@" prints each arg on its own line.
	script := fmt.Sprintf(`printf "%%s\n" "$@" > %s
echo '{"thread_id":"t-argcheck","type":"thread.started"}'
echo '{"delta":"ok"}'
`, argsFile)

	targetDir := t.TempDir()
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}
	cfg := integConfig(t, func(c *core.WorkerConfig) {
		c.Model = "o4-mini"
		c.TargetDir = targetDir
	})

	_, _, _, err := ex.Execute(context.Background(), cfg, &allEventsEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute error: %v", err)
	}

	raw, readErr := os.ReadFile(argsFile)
	if readErr != nil {
		t.Fatalf("args file not written by fake binary: %v", readErr)
	}
	gotArgs := strings.Split(strings.TrimSpace(string(raw)), "\n")

	mustContain(t, gotArgs, "exec")
	mustContain(t, gotArgs, "--dangerously-bypass-approvals-and-sandbox")
	mustContain(t, gotArgs, "--json")
	mustContain(t, gotArgs, "--skip-git-repo-check")
	mustContain(t, gotArgs, "-m")
	mustContain(t, gotArgs, "o4-mini")
	mustContain(t, gotArgs, "-C")
	mustContain(t, gotArgs, targetDir)
	mustContain(t, gotArgs, "do the work")
	mustNotContain(t, gotArgs, "resume")
}

// ---------------------------------------------------------------------------
// Argument contract: resume session via subprocess
// ---------------------------------------------------------------------------

// TestExecute_ResumeSession_ArgumentContract verifies that Execute passes the
// resume argument shape to the subprocess when ResumeSessionID is set:
// "exec resume <thread_id>" with no -m, no -C, and no objective.
func TestExecute_ResumeSession_ArgumentContract(t *testing.T) {
	argsFile := filepath.Join(t.TempDir(), "args.txt")
	script := fmt.Sprintf(`printf "%%s\n" "$@" > %s
echo '{"thread_id":"t-resume-integ","type":"thread.started"}'
echo '{"delta":"resumed output"}'
`, argsFile)

	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}
	cfg := integConfig(t, func(c *core.WorkerConfig) {
		c.ResumeSessionID = "thread-resume-42"
		c.Model = "o4-mini" // must be suppressed on resume
		c.TargetDir = t.TempDir()
	})

	_, sessionID, _, err := ex.Execute(context.Background(), cfg, &allEventsEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute error: %v", err)
	}
	if sessionID != "t-resume-integ" {
		t.Errorf("sessionID = %q, want %q", sessionID, "t-resume-integ")
	}

	raw, readErr := os.ReadFile(argsFile)
	if readErr != nil {
		t.Fatalf("args file not written by fake binary: %v", readErr)
	}
	gotArgs := strings.Split(strings.TrimSpace(string(raw)), "\n")

	mustContain(t, gotArgs, "exec")
	mustContain(t, gotArgs, "resume")
	mustContain(t, gotArgs, "thread-resume-42")
	// Model selection is not valid for resume invocations.
	mustNotContain(t, gotArgs, "-m")
	mustNotContain(t, gotArgs, "o4-mini")
	// CWD flag is not valid for resume invocations.
	mustNotContain(t, gotArgs, "-C")
	// Objective must not leak into a resume invocation.
	mustNotContain(t, gotArgs, "do the work")
}

// ---------------------------------------------------------------------------
// Session ID: first occurrence wins
// ---------------------------------------------------------------------------

// TestExecute_SessionID_FirstOccurrenceWins verifies that when multiple JSONL
// lines carry a thread_id, only the first value is captured as the sessionID.
func TestExecute_SessionID_FirstOccurrenceWins(t *testing.T) {
	script := `
echo '{"thread_id":"t-first","type":"thread.started"}'
echo '{"delta":"some text"}'
echo '{"thread_id":"t-second","type":"other.event"}'
`
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}

	_, sessionID, _, err := ex.Execute(context.Background(), integConfig(t, nil), &allEventsEmitter{}, false)
	if err != nil {
		t.Fatalf("Execute error: %v", err)
	}
	if sessionID != "t-first" {
		t.Errorf("sessionID = %q, want %q (first occurrence wins)", sessionID, "t-first")
	}
}

// ---------------------------------------------------------------------------
// Context cancellation
// ---------------------------------------------------------------------------

// TestExecute_ContextCancellation_ReturnsError verifies that a cancelled
// context causes Execute to return an error. The fake binary sleeps so it
// will only complete via cancellation.
func TestExecute_ContextCancellation_ReturnsError(t *testing.T) {
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, "sleep 60")}
	cfg := integConfig(t, nil)

	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()

	_, _, _, err := ex.Execute(ctx, cfg, &allEventsEmitter{}, false)
	if err == nil {
		t.Fatal("expected error when context is cancelled, got nil")
	}
}

// ---------------------------------------------------------------------------
// WorkerSpawned metadata
// ---------------------------------------------------------------------------

// TestExecute_WorkerSpawned_ContainsRuntime verifies that the WorkerSpawned
// event carries the expected runtime and model metadata fields.
func TestExecute_WorkerSpawned_ContainsRuntime(t *testing.T) {
	script := `
echo '{"thread_id":"t-meta","type":"thread.started"}'
echo '{"delta":"done"}'
`
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}
	emitter := &allEventsEmitter{}
	cfg := integConfig(t, func(c *core.WorkerConfig) {
		c.Model = "o4-mini"
	})

	if _, _, _, err := ex.Execute(context.Background(), cfg, emitter, false); err != nil {
		t.Fatalf("Execute error: %v", err)
	}

	spawned := emitter.ofType(event.WorkerSpawned)
	if len(spawned) == 0 {
		t.Fatal("no WorkerSpawned event emitted")
	}
	ev := spawned[0]
	if got, _ := ev.Data["runtime"].(string); got != string(core.RuntimeCodex) {
		t.Errorf("WorkerSpawned.Data[runtime] = %q, want %q", got, string(core.RuntimeCodex))
	}
	if got, _ := ev.Data["model"].(string); got != "o4-mini" {
		t.Errorf("WorkerSpawned.Data[model] = %q, want %q", got, "o4-mini")
	}
}

// ---------------------------------------------------------------------------
// No output — no output.md written
// ---------------------------------------------------------------------------

// TestExecute_NoOutput_NoOutputFile verifies that when the subprocess emits
// no text (e.g. a tool-only session), output.md is not created in WorkerDir.
func TestExecute_NoOutput_NoOutputFile(t *testing.T) {
	// Only a structural event, no text delta.
	script := `echo '{"thread_id":"t-notxt","type":"thread.started"}'`
	ex := &CodexExecutor{BinaryPath: makeFakeCodex(t, script)}
	cfg := integConfig(t, nil)

	if _, _, _, err := ex.Execute(context.Background(), cfg, &allEventsEmitter{}, false); err != nil {
		t.Fatalf("Execute error: %v", err)
	}

	outPath := filepath.Join(cfg.WorkerDir, "output.md")
	if _, err := os.Stat(outPath); err == nil {
		t.Error("output.md must not be created when there is no text output")
	}
}
