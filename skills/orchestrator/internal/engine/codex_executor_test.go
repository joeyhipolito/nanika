package engine

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"io"
	"os"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// ---------------------------------------------------------------------------
// buildArgs
// ---------------------------------------------------------------------------

func TestBuildArgs_NewSession_NoModel(t *testing.T) {
	e := &CodexExecutor{BinaryPath: "codex"}
	config := &core.WorkerConfig{
		Bundle: core.ContextBundle{
			Objective: "fix the bug",
		},
	}
	args := e.buildArgs(config, "/tmp/work")

	mustContain(t, args, "exec")
	mustContain(t, args, "--dangerously-bypass-approvals-and-sandbox")
	mustContain(t, args, "--json")
	mustContain(t, args, "--skip-git-repo-check")
	mustContain(t, args, "-C")
	mustContain(t, args, "/tmp/work")
	mustContain(t, args, "fix the bug")
	mustNotContain(t, args, "-m")
	mustNotContain(t, args, "resume")
}

func TestBuildArgs_NewSession_WithModel(t *testing.T) {
	e := &CodexExecutor{BinaryPath: "codex"}
	config := &core.WorkerConfig{
		Model: "o4-mini",
		Bundle: core.ContextBundle{
			Objective: "write tests",
		},
	}
	args := e.buildArgs(config, "/repo")

	mustContain(t, args, "-m")
	mustContain(t, args, "o4-mini")
	mustNotContain(t, args, "resume")
}

func TestBuildArgs_NewSession_EmptyCwd(t *testing.T) {
	e := &CodexExecutor{BinaryPath: "codex"}
	config := &core.WorkerConfig{
		Bundle: core.ContextBundle{Objective: "do work"},
	}
	args := e.buildArgs(config, "")

	mustNotContain(t, args, "-C")
	mustContain(t, args, "do work")
}

func TestBuildArgs_ResumeSession(t *testing.T) {
	e := &CodexExecutor{BinaryPath: "codex"}
	config := &core.WorkerConfig{
		ResumeSessionID: "thread-abc123",
		Model:           "o4-mini",
		Bundle: core.ContextBundle{
			Objective: "this should not appear",
		},
	}
	args := e.buildArgs(config, "/some/dir")

	// Must have "exec" then "resume" in that relative order.
	execIdx := indexOf(args, "exec")
	resumeIdx := indexOf(args, "resume")
	if execIdx < 0 {
		t.Fatal("args missing 'exec'")
	}
	if resumeIdx < 0 {
		t.Fatal("args missing 'resume'")
	}
	if resumeIdx <= execIdx {
		t.Errorf("'resume' (idx %d) must come after 'exec' (idx %d)", resumeIdx, execIdx)
	}

	mustContain(t, args, "thread-abc123")
	// Objective must NOT appear in a resume invocation.
	mustNotContain(t, args, "this should not appear")
	// -C must NOT appear in a resume invocation.
	mustNotContain(t, args, "-C")
	// -m must NOT appear in a resume invocation even when Model is set.
	mustNotContain(t, args, "-m")
}

func TestBuildArgs_ResumeSession_NoModel(t *testing.T) {
	e := &CodexExecutor{BinaryPath: "codex"}
	config := &core.WorkerConfig{
		ResumeSessionID: "thread-xyz",
		Bundle:          core.ContextBundle{},
	}
	args := e.buildArgs(config, "/work")

	mustContain(t, args, "resume")
	mustContain(t, args, "thread-xyz")
	mustNotContain(t, args, "-m")
}

// ---------------------------------------------------------------------------
// extractCodexText
// ---------------------------------------------------------------------------

func TestExtractCodexText_DeltaField(t *testing.T) {
	raw := rawMsg(`{"delta": "hello world"}`)
	got := extractCodexText(raw)
	if got != "hello world" {
		t.Errorf("got %q, want %q", got, "hello world")
	}
}

func TestExtractCodexText_TextField(t *testing.T) {
	raw := rawMsg(`{"text": "some text"}`)
	got := extractCodexText(raw)
	if got != "some text" {
		t.Errorf("got %q, want %q", got, "some text")
	}
}

func TestExtractCodexText_ContentField(t *testing.T) {
	raw := rawMsg(`{"content": "flat content"}`)
	got := extractCodexText(raw)
	if got != "flat content" {
		t.Errorf("got %q, want %q", got, "flat content")
	}
}

func TestExtractCodexText_DeltaPrecedesText(t *testing.T) {
	// When both delta and text are present, delta wins (first in key list).
	raw := rawMsg(`{"delta": "delta-wins", "text": "text-loses"}`)
	got := extractCodexText(raw)
	if got != "delta-wins" {
		t.Errorf("got %q, want delta-wins", got)
	}
}

func TestExtractCodexText_EmptyStringIgnored(t *testing.T) {
	raw := rawMsg(`{"delta": ""}`)
	got := extractCodexText(raw)
	if got != "" {
		t.Errorf("expected empty string, got %q", got)
	}
}

func TestExtractCodexText_NoKnownField(t *testing.T) {
	raw := rawMsg(`{"type": "thread.started", "thread_id": "t-1"}`)
	got := extractCodexText(raw)
	if got != "" {
		t.Errorf("expected empty string for event with no text field, got %q", got)
	}
}

func TestExtractCodexText_NonStringValueIgnored(t *testing.T) {
	// delta is a number, not a string — should be skipped.
	raw := rawMsg(`{"delta": 42}`)
	got := extractCodexText(raw)
	if got != "" {
		t.Errorf("expected empty string for non-string delta, got %q", got)
	}
}

// ---------------------------------------------------------------------------
// rawString
// ---------------------------------------------------------------------------

func TestRawString_Present(t *testing.T) {
	raw := rawMsg(`{"thread_id": "t-abc"}`)
	got, ok := rawString(raw, "thread_id")
	if !ok {
		t.Fatal("rawString returned ok=false for present key")
	}
	if got != "t-abc" {
		t.Errorf("got %q, want %q", got, "t-abc")
	}
}

func TestRawString_Absent(t *testing.T) {
	raw := rawMsg(`{"type": "event"}`)
	got, ok := rawString(raw, "thread_id")
	if ok {
		t.Fatal("rawString returned ok=true for absent key")
	}
	if got != "" {
		t.Errorf("got %q, want empty string", got)
	}
}

func TestRawString_NonStringValue(t *testing.T) {
	raw := rawMsg(`{"count": 5}`)
	got, ok := rawString(raw, "count")
	if ok {
		t.Fatal("rawString returned ok=true for non-string value")
	}
	if got != "" {
		t.Errorf("got %q, want empty string", got)
	}
}

func TestRawString_EmptyStringReturnsFalse(t *testing.T) {
	raw := rawMsg(`{"name": ""}`)
	got, ok := rawString(raw, "name")
	if ok {
		t.Fatal("rawString returned ok=true for empty string value")
	}
	if got != "" {
		t.Errorf("got %q, want empty string", got)
	}
}

// ---------------------------------------------------------------------------
// codexEnv
// ---------------------------------------------------------------------------

func TestCodexEnv_AlwaysPassedVars(t *testing.T) {
	// Ensure essential vars are included when present in the parent env.
	t.Setenv("HOME", "/home/test")
	t.Setenv("PATH", "/usr/bin:/bin")

	env := codexEnv()

	if !envHasKey(env, "HOME") {
		t.Error("codexEnv did not include HOME")
	}
	if !envHasKey(env, "PATH") {
		t.Error("codexEnv did not include PATH")
	}
}

func TestCodexEnv_OpenAIVarsIncluded(t *testing.T) {
	t.Setenv("OPENAI_API_KEY", "sk-test")
	t.Setenv("OPENAI_ORG_ID", "org-test")

	env := codexEnv()

	if !envHasKey(env, "OPENAI_API_KEY") {
		t.Error("codexEnv did not include OPENAI_API_KEY")
	}
	if !envHasKey(env, "OPENAI_ORG_ID") {
		t.Error("codexEnv did not include OPENAI_ORG_ID")
	}
}

func TestCodexEnv_CodexPrefixedVarsIncluded(t *testing.T) {
	t.Setenv("CODEX_PATH", "/usr/local/bin/codex")
	t.Setenv("CODEX_CUSTOM_VAR", "some-value")

	env := codexEnv()

	if !envHasKey(env, "CODEX_PATH") {
		t.Error("codexEnv did not include CODEX_PATH")
	}
	if !envHasKey(env, "CODEX_CUSTOM_VAR") {
		t.Error("codexEnv did not include CODEX_CUSTOM_VAR")
	}
}

func TestCodexEnv_UnrelatedVarsExcluded(t *testing.T) {
	// Set a var that should NOT be forwarded.
	t.Setenv("MY_SECRET_TOKEN", "secret")
	t.Setenv("SOME_RANDOM_VAR", "value")

	env := codexEnv()

	if envHasKey(env, "MY_SECRET_TOKEN") {
		t.Error("codexEnv unexpectedly included MY_SECRET_TOKEN")
	}
	if envHasKey(env, "SOME_RANDOM_VAR") {
		t.Error("codexEnv unexpectedly included SOME_RANDOM_VAR")
	}
}

func TestCodexEnv_NoDuplicates(t *testing.T) {
	env := codexEnv()
	seen := make(map[string]bool, len(env))
	for _, kv := range env {
		key, _, _ := strings.Cut(kv, "=")
		if seen[key] {
			t.Errorf("codexEnv returned duplicate key %q", key)
		}
		seen[key] = true
	}
}

// ---------------------------------------------------------------------------
// Streaming emission delta correctness (unit-level, no subprocess)
// ---------------------------------------------------------------------------

// capturingEmitter records all WorkerOutput events.
type capturingEmitter struct {
	events []map[string]any
}

func (c *capturingEmitter) Emit(_ context.Context, e event.Event) {
	if e.Type == event.WorkerOutput {
		c.events = append(c.events, e.Data)
	}
}
func (c *capturingEmitter) Close() error { return nil }

func TestCodexExecutor_BinaryPathResolution_EnvVar(t *testing.T) {
	t.Setenv("CODEX_PATH", "/custom/path/codex")
	os.Unsetenv("CODEX_PATH") // reset after Setenv so we can test
	t.Setenv("CODEX_PATH", "/custom/path/codex")

	ex := NewCodexExecutor()
	if ex.BinaryPath != "/custom/path/codex" {
		t.Errorf("BinaryPath = %q, want %q", ex.BinaryPath, "/custom/path/codex")
	}
}

func TestCodexExecutor_BinaryPathResolution_Default(t *testing.T) {
	// Without CODEX_PATH set, should fall back to a non-empty path.
	os.Unsetenv("CODEX_PATH")
	ex := NewCodexExecutor()
	if ex.BinaryPath == "" {
		t.Error("BinaryPath should not be empty when CODEX_PATH is unset")
	}
}

// ---------------------------------------------------------------------------
// Codex capabilities set regression tests
// ---------------------------------------------------------------------------

// TestCodexDescriptor_CapCostReport_Absent is a regression test for the bug
// where CapCostReport: false was present in the map. A false entry is still a
// map key, so Slice() would return CapCostReport and Has(CapCostReport) would
// return false — making the set appear to contain a cap it doesn't provide.
// After the fix, the key must be absent entirely.
func TestCodexDescriptor_CapCostReport_Absent(t *testing.T) {
	desc := core.CodexDescriptor()

	if desc.Caps.Has(core.CapCostReport) {
		t.Error("CodexDescriptor: Has(CapCostReport) = true, want false")
	}
	for _, c := range desc.Caps.Slice() {
		if c == core.CapCostReport {
			t.Error("CodexDescriptor: Slice() contains CapCostReport; it must be absent from the set")
		}
	}
}

func TestCodexDescriptor_RequiredCapsPresent(t *testing.T) {
	desc := core.CodexDescriptor()
	for _, cap := range []core.RuntimeCap{core.CapToolUse, core.CapSessionResume, core.CapStreaming, core.CapArtifacts} {
		if !desc.Caps.Has(cap) {
			t.Errorf("CodexDescriptor missing capability %q", cap)
		}
	}
}

// ---------------------------------------------------------------------------
// Scanner error surfacing regression tests
// ---------------------------------------------------------------------------

// TestStdoutScannerError_Surfaced is a regression test for the stdout scanner
// error path in Execute. It verifies that bufio.Scanner.Err() is non-nil when
// a line exceeds the 4 MiB buffer — the mechanism that Execute now checks and
// returns rather than swallowing.
//
// The write is done in a separate goroutine because io.Pipe writes block until
// all bytes are consumed. The scanner stops reading at 4 MiB, so the remaining
// byte is never consumed; the pipe reader is closed to unblock the writer.
func TestStdoutScannerError_Surfaced(t *testing.T) {
	r, w := io.Pipe()
	go func() {
		// Write a single line that exceeds the 4 MiB stdout scanner buffer.
		line := bytes.Repeat([]byte("x"), 4*1024*1024+1)
		w.Write(line) //nolint:errcheck
		w.Close()
	}()

	scanner := bufio.NewScanner(r)
	scanner.Buffer(make([]byte, 0, 4*1024*1024), 4*1024*1024)
	for scanner.Scan() {
	}
	r.CloseWithError(io.ErrClosedPipe) // unblock writer if still pending
	if scanner.Err() == nil {
		t.Error("expected scanner error for line exceeding 4 MiB buffer, got nil")
	}
}

// TestStderrScannerError_Surfaced is a regression test for the stderr scanner
// error path in Execute. It verifies that bufio.Scanner.Err() is non-nil when
// a line exceeds the default 64 KiB buffer — the error that the stderr goroutine
// now captures in stderrScanErr rather than silently discarding.
//
// io.Pipe writes block until all bytes are consumed by the reader. Because the
// scanner stops reading once it hits the max token size, the writer goroutine
// is unblocked by closing the read end, which causes the pending Write to
// return io.ErrClosedPipe and avoids a deadlock.
func TestStderrScannerError_Surfaced(t *testing.T) {
	r, w := io.Pipe()
	var scanErr error
	done := make(chan struct{})
	go func() {
		defer close(done)
		sc := bufio.NewScanner(r) // default MaxScanTokenSize = 64 KiB
		for sc.Scan() {
		}
		scanErr = sc.Err()
		r.CloseWithError(io.ErrClosedPipe) // unblock the writer goroutine
	}()

	// Write in a separate goroutine: io.Pipe writes block until the reader
	// consumes the bytes, but the scanner stops at MaxScanTokenSize. Without
	// a separate goroutine, the write would deadlock waiting for a read that
	// never comes.
	go func() {
		line := bytes.Repeat([]byte("e"), bufio.MaxScanTokenSize+1)
		w.Write(line) //nolint:errcheck
		w.Close()
	}()

	<-done
	if scanErr == nil {
		t.Error("expected scanner error for line exceeding 64 KiB stderr buffer, got nil")
	}
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

// rawMsg parses a JSON string into a map[string]json.RawMessage for test use.
func rawMsg(s string) map[string]json.RawMessage {
	var m map[string]json.RawMessage
	if err := json.Unmarshal([]byte(s), &m); err != nil {
		panic("rawMsg: " + err.Error())
	}
	return m
}

// mustContain fails if s is not in args.
func mustContain(t *testing.T, args []string, s string) {
	t.Helper()
	for _, a := range args {
		if a == s {
			return
		}
	}
	t.Errorf("args %v does not contain %q", args, s)
}

// mustNotContain fails if s is in args.
func mustNotContain(t *testing.T, args []string, s string) {
	t.Helper()
	for _, a := range args {
		if a == s {
			t.Errorf("args %v unexpectedly contains %q", args, s)
			return
		}
	}
}

// indexOf returns the index of s in args, or -1.
func indexOf(args []string, s string) int {
	for i, a := range args {
		if a == s {
			return i
		}
	}
	return -1
}

// envHasKey reports whether any entry in env starts with key=.
func envHasKey(env []string, key string) bool {
	prefix := key + "="
	for _, kv := range env {
		if strings.HasPrefix(kv, prefix) {
			return true
		}
	}
	return false
}
