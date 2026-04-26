package engine

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/git"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// CodexExecutor runs phases via the OpenAI Codex CLI (codex exec).
//
// Binary path resolution order:
//  1. CODEX_PATH environment variable (set at the time New is called)
//  2. "codex" looked up via PATH (exec.LookPath)
//  3. Bare "codex" string (let the OS resolve it at exec time)
//
// The executor emits WorkerSpawned, WorkerOutput, WorkerCompleted, and
// WorkerFailed events so Codex-owned phases appear in the event log alongside
// Claude-owned phases.
type CodexExecutor struct {
	// BinaryPath is the resolved path to the codex binary.
	BinaryPath string
}

// NewCodexExecutor creates a CodexExecutor, resolving the binary path from
// CODEX_PATH env var, then PATH lookup, then falling back to bare "codex".
func NewCodexExecutor() *CodexExecutor {
	path := os.Getenv("CODEX_PATH")
	if path == "" {
		if resolved, err := exec.LookPath("codex"); err == nil {
			path = resolved
		}
	}
	if path == "" {
		path = "codex"
	}
	return &CodexExecutor{BinaryPath: path}
}

// Describe returns the RuntimeDescriptor for the Codex runtime, declaring its
// capability set. Satisfies the RuntimeDescriber interface so the engine can
// validate phase contracts before dispatch.
func (e *CodexExecutor) Describe() core.RuntimeDescriptor {
	return core.CodexDescriptor()
}

// Execute runs a Codex session for the given worker config.
//
// For implementer phases, it invokes:
//
//	codex exec --dangerously-bypass-approvals-and-sandbox --json
//	           --skip-git-repo-check [-m model] [-C cwd] <objective>
//
// For reviewer phases with RuntimeCodex (RoleReviewer + RuntimeCodex), it also
// invokes the codex exec path via buildArgs — not codex review — so that the
// Codex CLI runs in its standard agentic mode for review objectives.
//
// For reviewer phases without RuntimeCodex (RoleReviewer + any other runtime),
// it invokes:
//
//	codex review --dangerously-bypass-approvals-and-sandbox --json
//	             --skip-git-repo-check --base <branch> [-m model] [-C cwd] <objective>
//
// When config.ResumeSessionID is set (a prior thread_id) on implementer phases:
//
//	codex exec resume --dangerously-bypass-approvals-and-sandbox --json
//	                  --skip-git-repo-check <thread_id>
//
// The executor streams the JSONL event log from stdout, emitting
// WorkerOutput events for each text chunk and capturing the thread_id
// returned by the "thread.started" event as the session ID.
func (e *CodexExecutor) Execute(
	ctx context.Context,
	config *core.WorkerConfig,
	emitter event.Emitter,
	verbose bool,
) (output, sessionID string, cost *sdk.CostInfo, err error) {
	missionID := config.Bundle.WorkspaceID
	phaseID := config.Bundle.PhaseID

	emitter.Emit(ctx, event.New(event.WorkerSpawned, missionID, phaseID, config.Name, map[string]any{
		"model":   config.Model,
		"runtime": string(core.RuntimeCodex),
		"dir":     config.WorkerDir,
	}))

	start := time.Now()

	// Determine working directory: prefer TargetDir when set.
	cwd := config.WorkerDir
	if config.TargetDir != "" {
		cwd = config.TargetDir
	}

	// RoleReviewer + RuntimeCodex: use the standard codex exec path so the CLI
	// runs in agentic mode for review objectives (not codex review).
	// All other RoleReviewer combinations use the codex review path.
	if config.Bundle.Role == core.RoleReviewer && config.Bundle.Runtime != core.RuntimeCodex {
		return e.executeReview(ctx, config, cwd, emitter, verbose, start)
	}

	// Implementer/default path
	args := e.buildArgs(config, cwd)
	cmd := exec.CommandContext(ctx, e.BinaryPath, args...)
	cmd.Dir = cwd
	cmd.Env = codexEnv()

	if verbose {
		fmt.Printf("[%s] codex exec %s\n", config.Name, strings.Join(args, " "))
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return "", "", nil, fmt.Errorf("codex stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return "", "", nil, fmt.Errorf("codex stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return "", "", nil, fmt.Errorf("start codex: %w", err)
	}

	// Collect stderr for error messages.
	var stderrLines []string
	var stderrScanErr error
	stderrDone := make(chan struct{})
	go func() {
		defer close(stderrDone)
		sc := bufio.NewScanner(stderr)
		for sc.Scan() {
			stderrLines = append(stderrLines, sc.Text())
		}
		stderrScanErr = sc.Err()
	}()

	// Parse JSONL from stdout, accumulating text and capturing the thread_id.
	var textParts []string
	var capturedSessionID string
	var lastEmit = start
	var emittedLen int // byte length of text already sent in streaming WorkerOutput events

	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 0, 4*1024*1024), 4*1024*1024)

	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}

		var raw map[string]json.RawMessage
		if jsonErr := json.Unmarshal(line, &raw); jsonErr != nil {
			continue
		}

		// Capture thread_id (Codex session identifier) from any event.
		if tid, ok := rawString(raw, "thread_id"); ok && capturedSessionID == "" {
			capturedSessionID = tid
		}

		// Extract text from this event (handles multiple event shapes).
		text := extractCodexText(raw)
		if text != "" {
			textParts = append(textParts, text)

			// Throttle WorkerOutput events to ~1 Hz.
			// Emit only the delta (text accumulated since the last emission)
			// so consumers receive incremental chunks, not the full text each time.
			if time.Since(lastEmit) >= time.Second {
				full := strings.Join(textParts, "")
				chunk := full[emittedLen:]
				if chunk != "" {
					emitter.Emit(ctx, event.New(event.WorkerOutput, missionID, phaseID, config.Name, map[string]any{
						"chunk":     chunk,
						"streaming": true,
					}))
					emittedLen = len(full)
				}
				lastEmit = time.Now()
			}
		}
	}

	// Capture stdout scanner error before waiting on stderr / cmd.Wait so that
	// a truncated JSONL stream is always surfaced even when the process exits 0.
	stdoutScanErr := scanner.Err()

	// Wait for stderr goroutine to finish reading.
	<-stderrDone

	waitErr := cmd.Wait()
	duration := time.Since(start)
	output = strings.Join(textParts, "")

	if capturedSessionID != "" {
		sessionID = capturedSessionID
	}

	// A stdout scanner error means the JSONL output was corrupted or truncated.
	// Surface it regardless of the process exit code.
	if stdoutScanErr != nil {
		return output, sessionID, nil, fmt.Errorf("codex stdout scanner: %w", stdoutScanErr)
	}

	if waitErr != nil {
		stderrText := strings.Join(stderrLines, "\n")
		if stderrScanErr != nil {
			stderrText += fmt.Sprintf(" [stderr truncated: %v]", stderrScanErr)
		}
		emitter.Emit(ctx, event.New(event.WorkerFailed, missionID, phaseID, config.Name, map[string]any{
			"error":      waitErr.Error(),
			"duration":   duration.String(),
			"output_len": len(output),
		}))
		if verbose {
			fmt.Printf("[%s] codex failed in %s\n", config.Name, duration.Round(time.Second))
		}
		if exitErr, ok := waitErr.(*exec.ExitError); ok {
			return output, sessionID, nil, fmt.Errorf("codex exited %d: %s", exitErr.ExitCode(), stderrText)
		}
		return output, sessionID, nil, fmt.Errorf("codex: %w", waitErr)
	}

	// On a clean exit, still surface a stderr scanner error so callers are
	// aware that error output may have been truncated.
	if stderrScanErr != nil {
		return output, sessionID, nil, fmt.Errorf("codex stderr scanner: %w", stderrScanErr)
	}

	// Write text output to WorkerDir so the artifact collection picks it up.
	if output != "" {
		outPath := filepath.Join(config.WorkerDir, "output.md")
		if writeErr := os.WriteFile(outPath, []byte(output), 0600); writeErr != nil && verbose {
			fmt.Printf("[%s] warning: could not write output.md: %v\n", config.Name, writeErr)
		}
	}

	emitter.Emit(ctx, event.New(event.WorkerOutput, missionID, phaseID, config.Name, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))
	emitter.Emit(ctx, event.New(event.WorkerCompleted, missionID, phaseID, config.Name, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))

	if verbose {
		fmt.Printf("[%s] codex completed in %s (%d chars)\n", config.Name, duration.Round(time.Second), len(output))
	}

	return output, sessionID, nil, nil
}

// executeReview runs a codex review session for review phases.
// It invokes: codex review --base <branch> with the review objective.
// The output is parsed into ReviewFindings format for use by the review loop.
func (e *CodexExecutor) executeReview(
	ctx context.Context,
	config *core.WorkerConfig,
	cwd string,
	emitter event.Emitter,
	verbose bool,
	start time.Time,
) (output, sessionID string, cost *sdk.CostInfo, err error) {
	missionID := config.Bundle.WorkspaceID
	phaseID := config.Bundle.PhaseID

	// Get the current branch to use as the review base.
	baseBranch, err := git.CurrentBranch(cwd)
	if err != nil {
		emitter.Emit(ctx, event.New(event.WorkerFailed, missionID, phaseID, config.Name, map[string]any{
			"error": fmt.Sprintf("git.CurrentBranch: %v", err),
		}))
		return "", "", nil, fmt.Errorf("get current branch for review: %w", err)
	}

	reviewBaseFlags := []string{
		"--dangerously-bypass-approvals-and-sandbox",
		"--json",
		"--skip-git-repo-check",
	}
	args := append([]string{"review"}, reviewBaseFlags...)
	args = append(args, "--base", baseBranch)
	if config.Model != "" {
		args = append(args, "-m", config.Model)
	}
	if config.EffortLevel != "" {
		args = append(args, "-c", "model_reasoning_effort="+config.EffortLevel)
	}
	if cwd != "" {
		args = append(args, "-C", cwd)
	}
	args = append(args, config.Bundle.Objective)
	cmd := exec.CommandContext(ctx, e.BinaryPath, args...)
	cmd.Dir = cwd
	cmd.Env = codexEnv()

	if verbose {
		fmt.Printf("[%s] codex review --base %s\n", config.Name, baseBranch)
	}

	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return "", "", nil, fmt.Errorf("codex stdout pipe: %w", err)
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		return "", "", nil, fmt.Errorf("codex stderr pipe: %w", err)
	}

	if err := cmd.Start(); err != nil {
		return "", "", nil, fmt.Errorf("start codex review: %w", err)
	}

	// Collect stderr for error messages.
	var stderrLines []string
	var stderrScanErr error
	stderrDone := make(chan struct{})
	go func() {
		defer close(stderrDone)
		sc := bufio.NewScanner(stderr)
		for sc.Scan() {
			stderrLines = append(stderrLines, sc.Text())
		}
		stderrScanErr = sc.Err()
	}()

	// Parse JSONL from stdout, accumulating text and capturing the thread_id.
	var textParts []string
	var capturedSessionID string
	var lastEmit = start
	var emittedLen int

	scanner := bufio.NewScanner(stdout)
	scanner.Buffer(make([]byte, 0, 4*1024*1024), 4*1024*1024)

	for scanner.Scan() {
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}

		var raw map[string]json.RawMessage
		if jsonErr := json.Unmarshal(line, &raw); jsonErr != nil {
			continue
		}

		// Capture thread_id from any event.
		if tid, ok := rawString(raw, "thread_id"); ok && capturedSessionID == "" {
			capturedSessionID = tid
		}

		// Extract text from this event.
		text := extractCodexText(raw)
		if text != "" {
			textParts = append(textParts, text)

			// Throttle WorkerOutput events to ~1 Hz.
			if time.Since(lastEmit) >= time.Second {
				full := strings.Join(textParts, "")
				chunk := full[emittedLen:]
				if chunk != "" {
					emitter.Emit(ctx, event.New(event.WorkerOutput, missionID, phaseID, config.Name, map[string]any{
						"chunk":     chunk,
						"streaming": true,
					}))
					emittedLen = len(full)
				}
				lastEmit = time.Now()
			}
		}
	}

	// Capture stdout scanner error.
	stdoutScanErr := scanner.Err()

	// Wait for stderr goroutine.
	<-stderrDone

	waitErr := cmd.Wait()
	duration := time.Since(start)
	output = strings.Join(textParts, "")

	if capturedSessionID != "" {
		sessionID = capturedSessionID
	}

	// A stdout scanner error means the JSONL output was corrupted or truncated.
	if stdoutScanErr != nil {
		return output, sessionID, nil, fmt.Errorf("codex review stdout scanner: %w", stdoutScanErr)
	}

	if waitErr != nil {
		stderrText := strings.Join(stderrLines, "\n")
		if stderrScanErr != nil {
			stderrText += fmt.Sprintf(" [stderr truncated: %v]", stderrScanErr)
		}
		emitter.Emit(ctx, event.New(event.WorkerFailed, missionID, phaseID, config.Name, map[string]any{
			"error":      waitErr.Error(),
			"duration":   duration.String(),
			"output_len": len(output),
		}))
		if verbose {
			fmt.Printf("[%s] codex review failed in %s\n", config.Name, duration.Round(time.Second))
		}
		if exitErr, ok := waitErr.(*exec.ExitError); ok {
			return output, sessionID, nil, fmt.Errorf("codex review exited %d: %s", exitErr.ExitCode(), stderrText)
		}
		return output, sessionID, nil, fmt.Errorf("codex review: %w", waitErr)
	}

	if stderrScanErr != nil {
		return output, sessionID, nil, fmt.Errorf("codex review stderr scanner: %w", stderrScanErr)
	}

	// Write text output to WorkerDir.
	if output != "" {
		outPath := filepath.Join(config.WorkerDir, "output.md")
		if writeErr := os.WriteFile(outPath, []byte(output), 0600); writeErr != nil && verbose {
			fmt.Printf("[%s] warning: could not write output.md: %v\n", config.Name, writeErr)
		}
	}

	emitter.Emit(ctx, event.New(event.WorkerOutput, missionID, phaseID, config.Name, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))
	emitter.Emit(ctx, event.New(event.WorkerCompleted, missionID, phaseID, config.Name, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))

	if verbose {
		fmt.Printf("[%s] codex review completed in %s (%d chars)\n", config.Name, duration.Round(time.Second), len(output))
	}

	return output, sessionID, nil, nil
}

// buildArgs constructs the codex exec argument list.
// When config.ResumeSessionID is non-empty, it builds a resume invocation.
func (e *CodexExecutor) buildArgs(config *core.WorkerConfig, cwd string) []string {
	baseFlags := []string{
		"--dangerously-bypass-approvals-and-sandbox",
		"--json",
		"--skip-git-repo-check",
	}

	if config.ResumeSessionID != "" {
		// Resume invocations: model selection is ignored — the session already
		// has an associated model; passing -m is not valid for resume.
		// codex exec resume <thread_id>
		args := append([]string{"exec", "resume"}, baseFlags...)
		args = append(args, config.ResumeSessionID)
		return args
	}

	// New session: optionally select a model and effort level.
	// codex exec [-m model] [-c model_reasoning_effort=<level>] [-C <cwd>] <objective>
	// -C sets the working root inside codex (redundant with cmd.Dir but explicit).
	args := append([]string{"exec"}, baseFlags...)
	if config.Model != "" {
		args = append(args, "-m", config.Model)
	}
	if config.EffortLevel != "" {
		args = append(args, "-c", "model_reasoning_effort="+config.EffortLevel)
	}
	if cwd != "" {
		args = append(args, "-C", cwd)
	}
	args = append(args, config.Bundle.Objective)
	return args
}

// codexEnv returns the subprocess environment for the Codex CLI.
// It passes through the base set of env vars plus OpenAI credentials and
// any CODEX_* / OPENAI_* variables present in the parent process.
func codexEnv() []string {
	alwaysPass := map[string]bool{
		"HOME":   true,
		"PATH":   true,
		"LANG":   true,
		"TERM":   true,
		"USER":   true,
		"SHELL":  true,
		"TMPDIR": true,
		// OpenAI auth
		"OPENAI_API_KEY":  true,
		"OPENAI_ORG_ID":   true,
		"OPENAI_BASE_URL": true,
	}

	var env []string
	for _, kv := range os.Environ() {
		key, _, _ := strings.Cut(kv, "=")
		if alwaysPass[key] ||
			strings.HasPrefix(key, "CODEX_") ||
			strings.HasPrefix(key, "OPENAI_") {
			env = append(env, kv)
		}
	}
	return env
}

// extractCodexText returns the text content from a decoded JSONL event.
// It handles multiple event shapes emitted by different Codex CLI versions:
//
//   - {"delta": "..."} — response.output_text.delta
//   - {"text": "..."}  — response.output_text.done / agent_message
//   - {"content": "..."} — flat content field
func extractCodexText(raw map[string]json.RawMessage) string {
	for _, key := range []string{"delta", "text", "content"} {
		v, ok := raw[key]
		if !ok {
			continue
		}
		var s string
		if err := json.Unmarshal(v, &s); err == nil && s != "" {
			return s
		}
	}
	return ""
}

// rawString decodes the named field from a raw JSON object as a string.
// Returns ("", false) when the field is absent or not a JSON string.
func rawString(raw map[string]json.RawMessage, key string) (string, bool) {
	v, ok := raw[key]
	if !ok {
		return "", false
	}
	var s string
	if err := json.Unmarshal(v, &s); err != nil {
		return "", false
	}
	return s, s != ""
}
