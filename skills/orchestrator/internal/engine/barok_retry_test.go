package engine

// barok_retry_test.go — regression tests for the barok validator-failure retry path.
//
// Six cases are verified:
//
//  (a) validator failure triggers exactly one retry invocation with barok disabled
//  (b) retry success → phase completes with BarokRetry=1 (same scenario as a)
//  (c) retry structural-validation failure → phase fails, no second retry
//  (d) no validator failure → BarokRetry=0, no extra invocation
//  (e) NANIKA_NO_BAROK=1 → no compression, no validation, no retry
//  (f) pathological input that would cause infinite recursion if retry cap were
//      missing — verifies cap fires (executor called exactly once)

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/nanika/shared/sdk"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

// ---------------------------------------------------------------------------
// Test fixtures
// ---------------------------------------------------------------------------

// validArtifact is a structurally-sound markdown artifact: balanced fences,
// well-formed YAML frontmatter, and no open scratch blocks.
const validArtifact = `---
produced_by: technical-writer
phase: barok-phase
workspace: barok-ws
created_at: "2026-04-17T00:00:00Z"
confidence: high
depends_on: []
---

# Report

Some valid content here. No unbalanced code fences.
`

// invalidArtifact has an unclosed fenced code block — ValidateArtifactStructure
// must flag it.
const invalidArtifact = "---\n" +
	"produced_by: technical-writer\n" +
	"---\n\n" +
	"# Report\n\n" +
	"```go\n" +
	"func open() {\n" +
	"    // fence never closed\n"

// pathologicalArtifact has an odd number of fenced code markers — the content
// that would cause unbounded recursion if retryBarokPhase called
// runBarokValidation recursively. Three unclosed fences guarantee the validator
// flags it every time regardless of how many retries fire.
const pathologicalArtifact = "```go\n" + // fence 1: unclosed
	"func open1() {\n" +
	"```python\n" + // fence 2: unclosed (nested)
	"def open2():\n" +
	"    pass\n" +
	"```bash\n" + // fence 3: unclosed — total=3, odd → validator rejects
	"echo 'recursive nightmare'\n"

// ---------------------------------------------------------------------------
// barokCapturingExecutor
// ---------------------------------------------------------------------------

// barokCapturingExecutor is a PhaseExecutor that:
//   - records the number of times Execute is called
//   - captures the ContextBundle passed on each call
//   - writes callContent[call_index] to config.WorkerDir/report.md (when non-empty)
//   - returns execErr on every call when non-nil
type barokCapturingExecutor struct {
	calls       int
	bundles     []core.ContextBundle
	callContent []string // indexed by call order; "" means write nothing
	execErr     error
}

func (b *barokCapturingExecutor) Execute(_ context.Context, config *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	b.bundles = append(b.bundles, config.Bundle)
	if b.calls < len(b.callContent) && b.callContent[b.calls] != "" {
		p := filepath.Join(config.WorkerDir, "report.md")
		_ = os.WriteFile(p, []byte(b.callContent[b.calls]), 0600)
	}
	b.calls++
	if b.execErr != nil {
		return "", "", nil, b.execErr
	}
	return "done", fmt.Sprintf("sess-retry-%d", b.calls), nil, nil
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

// newBarokTestEngine builds a minimal Engine with a barok-test runtime registered.
func newBarokTestEngine(t *testing.T, exec PhaseExecutor) *Engine {
	t.Helper()
	wsPath := t.TempDir()
	ws := &core.Workspace{ID: "barok-ws", Path: wsPath, Domain: "dev"}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil).WithEmitter(event.NoOpEmitter{})
	e.RegisterExecutor(core.Runtime("barok-test"), exec)
	return e
}

// setupBarokDirs creates the worker dir with a placeholder CLAUDE.md and
// returns its path. The phaseArtifactDir is created per-case by
// setupBarokArtifactDir so parallel subtests each get an isolated artifact
// directory inside their own temp workspace.
func setupBarokDirs(t *testing.T) string {
	t.Helper()
	workerDir := t.TempDir()
	if err := os.WriteFile(filepath.Join(workerDir, "CLAUDE.md"), []byte("# placeholder"), 0600); err != nil {
		t.Fatalf("write CLAUDE.md: %v", err)
	}
	return workerDir
}

// newBarokPhase builds a minimal Phase that the retry path can execute.
func newBarokPhase() *core.Phase {
	return &core.Phase{
		ID:      "phase-barok",
		Name:    "barok-phase",
		Runtime: core.Runtime("barok-test"),
		Status:  core.StatusPending,
	}
}

// newBarokConfig builds a terminal, eligible WorkerConfig.
func newBarokConfig(workerDir string) *core.WorkerConfig {
	return &core.WorkerConfig{
		Name:      "barok-phase",
		WorkerDir: workerDir,
		Bundle: core.ContextBundle{
			IsTerminal:  true,
			PersonaName: "technical-writer",
		},
	}
}

// ---------------------------------------------------------------------------
// TestBarokRetry — table-driven regression suite (cases a–d, f)
// ---------------------------------------------------------------------------

// runBarokRetryCase is the shared assertion body used by both the table-driven
// test and the env-disable test. The executor's retry content and error mode
// are configured on exec by the caller before invoking this helper.
func runBarokRetryCase(
	t *testing.T,
	e *Engine,
	workerDir string,
	initialArtifact string,
	exec *barokCapturingExecutor,
	wantErr bool,
	wantBarokAppl int,
	wantBarokRtry int,
	wantCallCount int,
	wantSkipBarok bool,
) {
	t.Helper()
	phaseArtifactDir := setupBarokArtifactDir(t, e.workspace.Path, initialArtifact)

	phase := newBarokPhase()
	config := newBarokConfig(workerDir)

	err := e.runBarokValidation(context.Background(), phase, config, phaseArtifactDir)

	if (err != nil) != wantErr {
		t.Errorf("runBarokValidation() error = %v, wantErr %v", err, wantErr)
	}
	if phase.BarokApplied != wantBarokAppl {
		t.Errorf("phase.BarokApplied = %d, want %d", phase.BarokApplied, wantBarokAppl)
	}
	if phase.BarokRetry != wantBarokRtry {
		t.Errorf("phase.BarokRetry = %d, want %d", phase.BarokRetry, wantBarokRtry)
	}
	if exec.calls != wantCallCount {
		t.Errorf("executor call count = %d, want %d", exec.calls, wantCallCount)
	}
	if wantSkipBarok {
		if len(exec.bundles) == 0 {
			t.Fatal("expected executor to be called at least once, but it was not")
		}
		if !exec.bundles[0].SkipBarokInjection {
			t.Error("retry executor was called with SkipBarokInjection=false; want true")
		}
	}
	// Cap invariant: retryBarokPhase never re-enters runBarokValidation, so the
	// executor can be called at most once regardless of content.
	if exec.calls > 1 {
		t.Errorf("retry cap violated: executor called %d times, max is 1", exec.calls)
	}
}

// setupBarokArtifactDir creates the phaseArtifactDir and merged dir, writes the
// initial artifact, and returns the phaseArtifactDir path.
func setupBarokArtifactDir(t *testing.T, wsPath, initialArtifact string) string {
	t.Helper()
	phaseArtifactDir := filepath.Join(wsPath, "artifacts", "phase-barok")
	if err := os.MkdirAll(phaseArtifactDir, 0700); err != nil {
		t.Fatalf("mkdir phaseArtifactDir: %v", err)
	}
	if initialArtifact != "" {
		if err := os.WriteFile(filepath.Join(phaseArtifactDir, "report.md"), []byte(initialArtifact), 0600); err != nil {
			t.Fatalf("write initial artifact: %v", err)
		}
	}
	mergedDir := core.MergedArtifactsDir(wsPath)
	if err := os.MkdirAll(mergedDir, 0700); err != nil {
		t.Fatalf("mkdir mergedDir: %v", err)
	}
	return phaseArtifactDir
}

func TestBarokRetry(t *testing.T) {
	t.Parallel()

	tests := []struct {
		name string

		// initial content of phaseArtifactDir/report.md
		initialArtifact string
		// content the executor writes to workerDir/report.md on its first call;
		// "" means the executor writes nothing (old artifact stays in phaseArtifactDir)
		retryArtifact string
		// if non-nil, the executor returns this error (simulates spawn/execute failure)
		executorErr error

		// expected outcomes
		wantErr       bool
		wantBarokAppl int // phase.BarokApplied
		wantBarokRtry int // phase.BarokRetry
		wantCallCount int // executor call count
		wantSkipBarok bool // retry executor was called with SkipBarokInjection=true
	}{
		{
			// (a) + (b): validator flags the initial artifact → exactly one retry
			// with SkipBarokInjection=true → retry succeeds → BarokRetry=1.
			name:            "(a)+(b) validator failure triggers single retry with barok disabled; success sets BarokRetry=1",
			initialArtifact: invalidArtifact,
			retryArtifact:   validArtifact,
			wantErr:         false,
			wantBarokAppl:   1,
			wantBarokRtry:   1,
			wantCallCount:   1,
			wantSkipBarok:   true,
		},
		{
			// (c): validator flags initial artifact → retry fires → retry artifact
			// also fails validation → phase fails, no second retry.
			name:            "(c) retry structural failure causes phase to fail; no second retry",
			initialArtifact: invalidArtifact,
			retryArtifact:   invalidArtifact,
			wantErr:         true,
			wantBarokAppl:   1,
			wantBarokRtry:   0, // not set on failure
			wantCallCount:   1, // cap: exactly one retry
		},
		{
			// (d): initial artifact is already valid → no retry, BarokRetry stays 0.
			name:            "(d) no validator failure means no retry, BarokRetry stays 0",
			initialArtifact: validArtifact,
			retryArtifact:   "",
			wantErr:         false,
			wantBarokAppl:   1,
			wantBarokRtry:   0,
			wantCallCount:   0,
		},
		{
			// (f): pathological input — content with deeply mismatched fences that
			// would cause infinite recursion if retryBarokPhase called
			// runBarokValidation recursively. The cap is architectural (retry
			// validates with ValidateArtifactStructure directly, no re-entry), so
			// the executor must be called exactly once regardless of how broken the
			// content is.
			name:            "(f) pathological input: retry cap fires, executor called exactly once",
			initialArtifact: pathologicalArtifact,
			retryArtifact:   pathologicalArtifact,
			wantErr:         true,
			wantBarokAppl:   1,
			wantBarokRtry:   0,
			wantCallCount:   1, // cap: never more than one retry invocation
		},
		{
			// (g): executor returns an error on the retry invocation — covers the
			// executor-error branch in retryBarokPhase (the `if execErr != nil`
			// return). The retry fired (executor was called once) but failed
			// before re-validation could run, so BarokRetry stays 0 and the phase
			// surfaces the wrapped error up to the caller.
			name:            "(g) executor error on retry propagates as phase failure; BarokRetry stays 0",
			initialArtifact: invalidArtifact,
			retryArtifact:   "", // executor errors before writing any artifact
			executorErr:     fmt.Errorf("simulated executor failure"),
			wantErr:         true,
			wantBarokAppl:   1,
			wantBarokRtry:   0,
			wantCallCount:   1,
			wantSkipBarok:   true, // bundle still flipped before Execute
		},
	}

	for _, tt := range tests {
		tt := tt
		t.Run(tt.name, func(t *testing.T) {
			t.Parallel()

			exec := &barokCapturingExecutor{
				callContent: []string{tt.retryArtifact},
				execErr:     tt.executorErr,
			}
			e := newBarokTestEngine(t, exec)
			workerDir := setupBarokDirs(t)

			runBarokRetryCase(t, e, workerDir,
				tt.initialArtifact,
				exec,
				tt.wantErr, tt.wantBarokAppl, tt.wantBarokRtry, tt.wantCallCount, tt.wantSkipBarok,
			)
		})
	}
}

// ---------------------------------------------------------------------------
// TestBarokRetry_EnvDisabled — case (e): NANIKA_NO_BAROK=1
// ---------------------------------------------------------------------------

// TestBarokRetry_EnvDisabled verifies case (e): when NANIKA_NO_BAROK=1 is set,
// runBarokValidation short-circuits before reading any artifacts or spawning a
// retry worker. t.Setenv is incompatible with t.Parallel, so this case lives in
// its own serial test function.
func TestBarokRetry_EnvDisabled(t *testing.T) {
	t.Setenv(worker.BarokEnvDisable, "1")

	exec := &barokCapturingExecutor{
		callContent: []string{""},
	}
	e := newBarokTestEngine(t, exec)
	workerDir := setupBarokDirs(t)

	// initialArtifact is structurally invalid — it would fail validation if
	// the barok path were not short-circuited by the env var.
	runBarokRetryCase(t, e, workerDir,
		invalidArtifact, exec,
		false, // wantErr: no error — env disables all checks
		0,     // wantBarokAppl: BarokApplied never set (early return before phase.BarokApplied=1)
		0,     // wantBarokRtry
		0,     // wantCallCount: executor never called
		false, // wantSkipBarok
	)
}
