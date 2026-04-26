package worker

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/nanika/shared/sdk"
)

// buildWorkerPrompt constructs a composite prompt from prior context and objective.
// The prior context is framed as source material using XML tags, with the objective
// placed last to leverage recency bias. If PriorContext exceeds 8000 chars, it is
// truncated with a note indicating the truncation.
func buildWorkerPrompt(bundle core.ContextBundle) string {
	const maxPriorLen = 8000

	var buf strings.Builder

	// Preamble: frame prior context as source material, not instructions.
	buf.WriteString("You will be given source material from prior phase output below, followed by a task.\n\n")

	// Include prior context if available.
	if bundle.PriorContext != "" {
		priorContext := bundle.PriorContext
		if len(priorContext) > maxPriorLen {
			priorContext = priorContext[:maxPriorLen] + "\n[Note: Prior context truncated; original was " + fmt.Sprintf("%d", len(bundle.PriorContext)) + " characters]"
		}
		buf.WriteString("<prior_phase_output>\n")
		buf.WriteString(priorContext)
		buf.WriteString("\n</prior_phase_output>\n\n")
	}

	// Objective last: recency bias.
	buf.WriteString("Task: ")
	buf.WriteString(bundle.Objective)

	return buf.String()
}

// Execute runs a Claude session in the worker directory.
// The emitter receives worker lifecycle events; pass event.NoOpEmitter{} to discard them.
// Returns (output, sessionID, cost, error). sessionID and cost are populated from the
// ResultMessage on both success and failure; cost may be nil if unavailable.
func Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	if verbose {
		fmt.Printf("[%s] starting (%s, model=%s)\n", config.Name, config.Bundle.PersonaName, config.Model)
	}

	missionID := config.Bundle.WorkspaceID
	phaseID := config.Bundle.PhaseID

	// Emit point 6: worker.spawned
	emitter.Emit(ctx, event.New(event.WorkerSpawned, missionID, phaseID, config.Name, map[string]any{
		"model":        config.Model,
		"effort_level": config.EffortLevel,
		"persona":      config.Bundle.PersonaName,
		"dir":          config.WorkerDir,
	}))

	start := time.Now()

	// Throttled streaming: accumulate text chunks and emit worker.output at most once per second.
	// Tool-use events bypass the throttle and are forwarded immediately.
	// QueryText calls OnEvent synchronously from its message loop (same goroutine),
	// so no mutex is required for pendingChunk, lastEmit, or capturedSessionID.
	var (
		pendingChunk      strings.Builder
		lastEmit          = start
		capturedSessionID string
		capturedCost      *sdk.CostInfo
	)

	flushChunk := func() {
		if pendingChunk.Len() == 0 {
			return
		}
		chunk := pendingChunk.String()
		pendingChunk.Reset()
		lastEmit = time.Now()
		emitter.Emit(ctx, event.New(event.WorkerOutput, missionID, phaseID, config.Name, map[string]any{
			"chunk":      chunk,
			"event_kind": "text",
			"streaming":  true,
		}))
	}

	onEvent := func(ev *sdk.StreamedEvent) {
		switch ev.Kind {
		case sdk.KindText:
			pendingChunk.WriteString(ev.Text)
			if time.Since(lastEmit) >= time.Second {
				flushChunk()
			}

		case sdk.KindToolUse:
			// Flush any pending text so the tool call appears after it in sequence.
			flushChunk()
			emitter.Emit(ctx, event.New(event.WorkerOutput, missionID, phaseID, config.Name, map[string]any{
				"chunk":      formatToolUse(ev),
				"event_kind": "tool_use",
				"tool_name":  ev.ToolName,
				"streaming":  true,
			}))
			lastEmit = time.Now()

		case sdk.KindTurnEnd:
			// Flush any trailing text when a turn completes.
			flushChunk()
			// Capture session ID so the engine can persist it for resumption.
			if ev.SessionID != "" {
				capturedSessionID = ev.SessionID
			}
			// Capture cost info for metrics attribution.
			if ev.Cost != nil {
				capturedCost = ev.Cost
			}
		}
	}

	// When TargetDir is set the worker executes inside the target repo;
	// WorkerDir is the artifact output directory written to by CLAUDE.md instructions.
	cwd := config.WorkerDir
	var addDirs []string
	if config.TargetDir != "" {
		cwd = config.TargetDir
		// Surface the worker's CLAUDE.md to the subprocess even though cwd is the target repo.
		addDirs = []string{config.WorkerDir}
	}

	// Seed persona memory into worker's Claude auto-memory directory.
	if err := seedMemory(config.Bundle.PersonaName, config.WorkerDir, cwd, config.Bundle.Objective); err != nil {
		if verbose {
			fmt.Printf("[%s] warning: memory seed failed: %v\n", config.Name, err)
		}
	}

	// Clean up deny-rule settings.local.json from target repo after execution
	// to avoid polluting git status. Restores any pre-existing file.
	if config.TargetDir != "" {
		defer cleanupTargetSettings(config.TargetDir)
	}

	prompt := buildWorkerPrompt(config.Bundle)

	output, err := sdk.QueryText(ctx, prompt, &sdk.AgentOptions{
		Model:           config.Model,
		EffortLevel:     config.EffortLevel,
		MaxTurns:        config.MaxTurns,
		Timeout:         config.StallTimeout,
		Cwd:             cwd,
		PermissionMode:  "bypass",
		ResumeSessionID: config.ResumeSessionID,
		AddDirs:         addDirs,
		OnEvent:         onEvent,
	})

	// Flush any chunk that didn't hit the 1-second threshold.
	flushChunk()

	duration := time.Since(start)

	if err != nil {
		var exitErr *sdk.ExitError
		exitCode, stderrTail := 0, ""
		if errors.As(err, &exitErr) {
			exitCode = exitErr.Code
			stderrTail = exitErr.Stderr
		}
		// Emit point 7: worker.failed
		emitter.Emit(ctx, event.New(event.WorkerFailed, missionID, phaseID, config.Name, map[string]any{
			"error":       err.Error(),
			"duration":    duration.String(),
			"output_len":  len(output),
			"exit_code":   exitCode,
			"stderr_tail": stderrTail,
		}))

		if verbose {
			fmt.Printf("[%s] failed in %s\n", config.Name, duration.Round(time.Second))
		}

		if output != "" {
			return output, capturedSessionID, capturedCost, fmt.Errorf("worker %s failed: %w", config.Name, err)
		}
		return "", capturedSessionID, capturedCost, fmt.Errorf("worker %s failed: %w", config.Name, err)
	}

	// Emit point 7 (success): worker.completed (worker.output was already streamed above)
	emitter.Emit(ctx, event.New(event.WorkerOutput, missionID, phaseID, config.Name, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))
	emitter.Emit(ctx, event.New(event.WorkerCompleted, missionID, phaseID, config.Name, map[string]any{
		"output_len": len(output),
		"duration":   duration.String(),
	}))

	if verbose {
		fmt.Printf("[%s] completed in %s (%d chars output)\n", config.Name, duration.Round(time.Second), len(output))
	}

	// Write output to worker dir for artifact collection
	if output != "" {
		outputPath := filepath.Join(config.WorkerDir, "output.md")
		if err := os.WriteFile(outputPath, []byte(output), 0600); err != nil {
			return output, capturedSessionID, capturedCost, fmt.Errorf("writing output to %s: %w", outputPath, err)
		}
	}

	// Fire-and-forget: scan worker dir (including output.md just written above) for
	// LEARNING/FINDING/PATTERN/DECISION/GOTCHA markers and persist to the learning DB.
	// Capture config fields by value so the goroutine holds no reference to config.
	workerDirSnap := config.WorkerDir
	workerNameSnap := config.Name
	domainSnap := config.Bundle.Domain
	wsIDSnap := config.Bundle.WorkspaceID
	go capturePhaseOutput(workerDirSnap, workerNameSnap, domainSnap, wsIDSnap)

	return output, capturedSessionID, capturedCost, nil
}

// formatToolUse returns a human-readable one-line summary of a tool call for
// inclusion in the worker output stream. The format is "[tool: Name arg]\n".
func formatToolUse(ev *sdk.StreamedEvent) string {
	var input map[string]json.RawMessage
	if err := json.Unmarshal(ev.ToolInput, &input); err != nil {
		return fmt.Sprintf("[tool: %s]\n", ev.ToolName)
	}

	// Extract the most descriptive single argument for common tool shapes.
	for _, key := range []string{"file_path", "notebook_path", "path", "pattern", "command", "query", "url", "prompt"} {
		raw, ok := input[key]
		if !ok {
			continue
		}
		var s string
		if err := json.Unmarshal(raw, &s); err != nil || s == "" {
			continue
		}
		if len(s) > 60 {
			s = s[:60] + "…"
		}
		return fmt.Sprintf("[tool: %s %s]\n", ev.ToolName, s)
	}
	return fmt.Sprintf("[tool: %s]\n", ev.ToolName)
}


// CollectArtifacts gathers all files created by a worker (excluding CLAUDE.md and hooks).
func CollectArtifacts(workerDir string) ([]string, error) {
	var artifacts []string

	err := filepath.WalkDir(workerDir, func(path string, d os.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return nil
		}

		// Skip orchestrator files
		rel, _ := filepath.Rel(workerDir, path)
		if rel == "CLAUDE.md" || rel == "workspace-context.md" ||
			strings.HasPrefix(rel, ".claude/") {
			return nil
		}

		artifacts = append(artifacts, path)
		return nil
	})

	return artifacts, err
}

// MergeArtifacts copies worker artifacts to the merged artifacts directory.
// Errors from individual file reads, directory creation, and writes are accumulated
// and returned as a joined error so callers can surface them as warnings without
// abandoning the remaining artifacts.
func MergeArtifacts(workerDir, phaseArtifactDir, mergedDir string) error {
	return MergeArtifactsWithMeta(workerDir, phaseArtifactDir, mergedDir, ArtifactMeta{})
}

// MergeArtifactsWithMeta copies worker artifacts to the merged artifacts directory,
// injecting YAML frontmatter into markdown files that do not already have it.
// When meta.ProducedBy is empty no frontmatter is injected (backward-compatible with MergeArtifacts).
func MergeArtifactsWithMeta(workerDir, phaseArtifactDir, mergedDir string, meta ArtifactMeta) error {
	artifacts, err := CollectArtifacts(workerDir)
	if err != nil {
		return err
	}

	var errs []error

	for _, src := range artifacts {
		data, err := os.ReadFile(src)
		if err != nil {
			errs = append(errs, fmt.Errorf("reading artifact %s: %w", src, err))
			continue
		}

		rel, _ := filepath.Rel(workerDir, src)

		// Reject path traversal in artifact relative paths
		if strings.Contains(rel, "..") {
			continue
		}

		// Inject YAML frontmatter into markdown artifacts when metadata is available.
		if meta.ProducedBy != "" && isMarkdown(rel) {
			data = InjectFrontmatterIfMissing(data, meta)
		}

		// Write to phase-specific dir
		phaseDst := filepath.Join(phaseArtifactDir, rel)
		if err := os.MkdirAll(filepath.Dir(phaseDst), 0700); err != nil {
			errs = append(errs, fmt.Errorf("creating dir for %s: %w", phaseDst, err))
		} else if err := os.WriteFile(phaseDst, data, 0600); err != nil {
			errs = append(errs, fmt.Errorf("writing artifact %s: %w", phaseDst, err))
		}

		// Write to merged dir
		mergedDst := filepath.Join(mergedDir, rel)
		if err := os.MkdirAll(filepath.Dir(mergedDst), 0700); err != nil {
			errs = append(errs, fmt.Errorf("creating dir for %s: %w", mergedDst, err))
		} else if err := os.WriteFile(mergedDst, data, 0600); err != nil {
			errs = append(errs, fmt.Errorf("writing artifact %s: %w", mergedDst, err))
		}
	}

	return errors.Join(errs...)
}

// isMarkdown reports whether the relative artifact path is a markdown file.
func isMarkdown(rel string) bool {
	ext := strings.ToLower(filepath.Ext(rel))
	return ext == ".md" || ext == ".mdx"
}
