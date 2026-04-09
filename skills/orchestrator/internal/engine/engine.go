// Package engine provides DAG-based parallel execution of phases.
package engine

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/git"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/persona"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
	"github.com/joeyhipolito/nen/zetsu"
)

// maxObjectiveBytes is the maximum allowed byte length for a phase OBJECTIVE.
// Oversized objectives produce prompts that exceed model context limits.
const maxObjectiveBytes = 8192

// Engine executes a plan's phases with dependency resolution.
type Engine struct {
	workspace   *core.Workspace
	config      *core.OrchestratorConfig
	embedder    *learning.Embedder
	learningDB  *learning.DB
	skillIndex  string // cached skill routing index for all workers
	emitter     event.Emitter
	executors   executorRegistry // runtime → PhaseExecutor; read-only after New()
	buildRunner BuildRunner      // nil disables build verification; set by New()

	mu                sync.Mutex
	phases            map[string]*core.Phase // id -> phase
	plan              *core.Plan             // retained for checkpoint saves
	startTime         time.Time              // when Execute was called; passed to every checkpoint
	injectedLearnings []learning.Learning    // accumulated across all phases, for compliance scan

	// Background learning extraction: at most one extraction runs at a time per
	// mission (extractMu) and extractWG tracks outstanding goroutines for
	// graceful shutdown before terminal events are emitted.
	extractMu sync.Mutex
	extractWG sync.WaitGroup
}

// New creates a new execution engine with a NoOpEmitter and the default
// executor registry (RuntimeClaude pre-registered). Call WithEmitter to
// attach a real emitter and RegisterExecutor to add additional runtimes
// before Execute.
func New(ws *core.Workspace, config *core.OrchestratorConfig, embedder *learning.Embedder, db *learning.DB, skillIndex string) *Engine {
	return &Engine{
		workspace:   ws,
		config:      config,
		embedder:    embedder,
		learningDB:  db,
		skillIndex:  skillIndex,
		emitter:     event.NoOpEmitter{},
		executors:   defaultRegistry(),
		phases:      make(map[string]*core.Phase),
		buildRunner: execBuildRunner,
	}
}

// RegisterExecutor registers a PhaseExecutor for the given runtime identifier.
// Must be called before Execute. Registering RuntimeClaude replaces the default.
func (e *Engine) RegisterExecutor(rt core.Runtime, ex PhaseExecutor) {
	if e.executors == nil {
		e.executors = defaultRegistry()
	}
	e.executors[rt] = ex
}

// resolveExecutor returns the PhaseExecutor for the given runtime, falling
// back to ClaudeExecutor when the registry is nil (e.g. in tests that build
// Engine directly) or when the runtime is not registered.
func (e *Engine) resolveExecutor(rt core.Runtime) PhaseExecutor {
	if e.executors != nil {
		if !e.executors.has(rt) {
			warnUnknownRuntime(rt)
		}
		return e.executors.resolve(rt)
	}
	warnUnknownRuntime(rt)
	return ClaudeExecutor{}
}

// WithEmitter sets the emitter used for state-transition events.
// Must be called before Execute. Returns e for chaining.
func (e *Engine) WithEmitter(em event.Emitter) *Engine {
	e.emitter = em
	return e
}

// InjectedLearnings returns all learnings that were injected into worker context
// bundles during this execution. Call after Execute returns.
func (e *Engine) InjectedLearnings() []learning.Learning {
	e.mu.Lock()
	defer e.mu.Unlock()
	result := make([]learning.Learning, len(e.injectedLearnings))
	copy(result, e.injectedLearnings)
	return result
}

// emit is a convenience wrapper that constructs and dispatches an event.
func (e *Engine) emit(ctx context.Context, typ event.EventType, phaseID, workerID string, data map[string]any) {
	e.emitter.Emit(ctx, event.New(typ, e.workspace.ID, phaseID, workerID, data))
}

// Execute runs all phases in a plan, respecting dependencies.
func (e *Engine) Execute(ctx context.Context, plan *core.Plan) (*core.ExecutionResult, error) {
	start := time.Now()
	e.startTime = start

	// Wrap context with cancellation for graceful teardown
	ctx, cancel := context.WithCancel(ctx)
	tm := NewTeardownManager(cancel)
	defer tm.Close()
	defer cancel()

	// Retain plan for checkpoint saves
	e.plan = plan

	// Validate phase ID uniqueness before indexing — duplicate IDs would
	// cause silent overwrites in the phase map and corrupt execution state.
	if err := core.ValidatePhaseIDs(plan); err != nil {
		return nil, fmt.Errorf("plan validation: %w", err)
	}

	// Index phases
	for _, p := range plan.Phases {
		e.phases[p.ID] = p
	}

	// Emit point 1: mission.started
	e.emit(ctx, event.MissionStarted, "", "", map[string]any{
		"task":           plan.Task,
		"phases":         len(plan.Phases),
		"execution_mode": plan.ExecutionMode,
	})

	var result *core.ExecutionResult
	var err error

	if e.config.ForceSequential || plan.ExecutionMode == "sequential" {
		result, err = e.executeSequential(ctx, plan, start, tm)
	} else {
		result, err = e.executeParallel(ctx, plan, start, tm)
	}

	// Drain background extraction goroutines before emitting terminal events.
	// This ensures learning.stored events arrive before mission.completed/failed.
	e.extractWG.Wait()

	// Record metrics
	if result != nil {
		metrics := buildMetrics(e.workspace, plan, result, start)
		if merr := RecordMetrics(metrics); merr != nil && e.config.Verbose {
			fmt.Printf("[engine] metrics record failed: %v\n", merr)
		}
	}

	// Emit point 2: mission.completed, mission.failed, or mission.cancelled.
	//
	// Use context.Background() for these terminal emissions: ctx may already
	// be cancelled at this point (cancellation path), and we must guarantee
	// exactly one mission-terminal event is always written regardless.
	emitCtx := context.Background()
	switch {
	case errors.Is(err, context.Canceled) || errors.Is(err, context.DeadlineExceeded):
		e.emit(emitCtx, event.MissionCancelled, "", "", map[string]any{
			"task":             plan.Task,
			"duration":        time.Since(start).String(),
			"duration_seconds": time.Since(start).Seconds(),
		})
	case err != nil || (result != nil && !result.Success):
		errMsg := ""
		if err != nil {
			errMsg = err.Error()
		} else if result != nil {
			errMsg = result.Error
		}
		e.emit(emitCtx, event.MissionFailed, "", "", map[string]any{
			"task":             plan.Task,
			"phase_count":     len(plan.Phases),
			"error":           errMsg,
			"duration":        time.Since(start).String(),
			"duration_seconds": time.Since(start).Seconds(),
		})
	default:
		artifacts := 0
		if result != nil {
			artifacts = len(result.Artifacts)
		}
		e.emit(emitCtx, event.MissionCompleted, "", "", map[string]any{
			"task":             plan.Task,
			"phase_count":     len(plan.Phases),
			"execution_mode":  plan.ExecutionMode,
			"duration":        time.Since(start).String(),
			"duration_seconds": time.Since(start).Seconds(),
			"artifacts":       artifacts,
		})
	}

	return result, err
}

func (e *Engine) executeSequential(ctx context.Context, plan *core.Plan, start time.Time, tm *TeardownManager) (*core.ExecutionResult, error) {
	result := &core.ExecutionResult{Plan: plan, Success: true}
	phaseOutputs := make(map[string]string)

	// Index-based loop: len(plan.Phases) is re-evaluated each iteration so
	// fix phases appended by handleReviewLoop are picked up automatically.
	for i := 0; i < len(plan.Phases); i++ {
		// Check for cancellation before starting each phase so that
		// never-executed phases reach a terminal state (skipped) instead of
		// staying permanently pending in LiveState and the checkpoint.
		if ctx.Err() != nil {
			e.skipRemainingSequential(ctx, plan, i)
			result.Success = false
			result.Error = "cancelled"
			result.Duration = time.Since(start)
			return result, ctx.Err()
		}

		phase := plan.Phases[i]
		// Skip any phase that has already reached a terminal state.
		// This covers checkpoint-resume (completed/failed phases from a prior
		// run) as well as phases pre-marked skipped during decomposition.
		if phase.Status.IsTerminal() {
			continue
		}

		tm.TrackPhase()
		var priorParts []string
		for _, depID := range phase.Dependencies {
			if out, ok := phaseOutputs[depID]; ok {
				priorParts = append(priorParts, out)
			}
		}
		priorContext := zetsu.SanitizePriorContext(strings.Join(priorParts, "\n\n---\n\n")).Output
		output, err := e.executePhase(ctx, phase, priorContext)
		tm.PhaseComplete()

		if err != nil {
			// Quota-gate skip: phase.Status is already StatusSkipped; don't
			// treat this as a mission failure or cascade-skip dependents.
			if errors.Is(err, errQuotaGateSkip) {
				e.saveCheckpoint(ctx)
				continue
			}

			phase.Status = core.StatusFailed
			phase.Error = err.Error()
			result.Success = false
			result.Error = fmt.Sprintf("phase %s failed: %v", phase.Name, err)

			// If the context was cancelled (the executor returned because ctx
			// expired), skip ALL remaining pending phases immediately and
			// return the cancellation error so Execute() emits
			// mission.cancelled rather than mission.failed.
			if ctx.Err() != nil {
				e.skipRemainingSequential(ctx, plan, i+1)
				e.saveCheckpoint(ctx)
				result.Error = "cancelled"
				result.Duration = time.Since(start)
				return result, ctx.Err()
			}

			skipped := e.skipSequentialDependents(ctx, plan, i, phase.ID)
			e.saveCheckpoint(ctx)

			if e.config.Verbose {
				fmt.Printf("[engine] phase %s failed: %v\n", phase.Name, err)
				if skipped > 0 {
					fmt.Printf("[engine] skipped %d dependent phase(s) after failure in %s\n", skipped, phase.Name)
				}
			}
			// Continue with remaining phases, but never execute phases whose
			// declared dependencies are already failed/skipped.
			continue
		}

		// Build verification: after a fix phase succeeds (LLM-wise), run the
		// project's build command to catch compilation errors. If the build
		// fails, mark the fix as failed and inject a retry fix+re-review pair
		// so the agent can address the build error deterministically.
		if phase.Name == "fix" {
			if buildErr := e.verifyAndRetry(ctx, phase); buildErr != nil {
				phase.Status = core.StatusFailed
				phase.Error = buildErr.Error()
				result.Success = false

				skipped := e.skipSequentialDependents(ctx, plan, i, phase.ID)
				e.saveCheckpoint(ctx)
				if e.config.Verbose {
					fmt.Printf("[engine] fix phase %s failed build verification: %v\n", phase.Name, buildErr)
					if skipped > 0 {
						fmt.Printf("[engine] skipped %d dependent phase(s) after build failure in %s\n", skipped, phase.Name)
					}
				}
				continue
			}
		}

		// Check whether this completed phase is a review gate with blockers,
		// and if so inject a fix phase. The index loop will reach it on the
		// next iteration because len(plan.Phases) is re-evaluated.
		if fixPhase := e.handleReviewLoop(ctx, phase, output, priorContext); fixPhase != nil {
			e.phases[fixPhase.ID] = fixPhase
			if e.config.Verbose {
				fmt.Printf("[engine] review loop: injected fix phase %s\n", fixPhase.Name)
			}
		}

		e.commitPhaseWork(phase)
		e.saveCheckpoint(ctx)
		phaseOutputs[phase.ID] = output
	}

	var allOutputs []string
	for _, p := range plan.Phases {
		if out, ok := phaseOutputs[p.ID]; ok {
			allOutputs = append(allOutputs, out)
		}
	}
	result.Output = strings.Join(allOutputs, "\n\n---\n\n")
	result.Duration = time.Since(start)
	result.Artifacts = e.collectAllArtifacts()
	return result, nil
}

func (e *Engine) executeParallel(ctx context.Context, plan *core.Plan, start time.Time, tm *TeardownManager) (*core.ExecutionResult, error) {
	result := &core.ExecutionResult{Plan: plan, Success: true}

	// Semaphore for concurrency control
	maxConcurrent := e.config.MaxConcurrent
	if maxConcurrent <= 0 {
		maxConcurrent = 3
	}
	sem := make(chan struct{}, maxConcurrent)

	// Track completed phase outputs
	var outputMu sync.Mutex
	phaseOutputs := make(map[string]string)

	// Completion channel
	type phaseResult struct {
		phaseID string
		output  string
		err     error
	}

	// Track pending phases. Phases already in a terminal state (from checkpoint
	// resume) are excluded so they are never dispatched and never expected in
	// completionCh. The completed counter is pre-seeded with their count.
	pending := make(map[string]bool)
	for _, p := range plan.Phases {
		if !p.Status.IsTerminal() {
			pending[p.ID] = true
		}
	}
	completionCh := make(chan phaseResult, len(pending))

	// Dispatch ready phases
	dispatch := func() {
		for id := range pending {
			phase := e.phases[id]
			if phase.Status != core.StatusPending {
				continue
			}
			if !e.depsCompleted(phase) {
				continue
			}

			// Collect prior context from dependencies
			outputMu.Lock()
			var priorParts []string
			for _, depID := range phase.Dependencies {
				if out, ok := phaseOutputs[depID]; ok {
					priorParts = append(priorParts, out)
				}
			}
			priorContext := zetsu.SanitizePriorContext(strings.Join(priorParts, "\n\n---\n\n")).Output
			outputMu.Unlock()

			phase.Status = core.StatusRunning
			now := time.Now()
			phase.StartTime = &now

			// Emit point: dag.phase_dispatched
			e.emit(ctx, event.DAGPhaseDispatched, phase.ID, "", map[string]any{
				"name":    phase.Name,
				"persona": phase.Persona,
				"skills":  phase.Skills,
			})

			// Emit dag.dependency_resolved when a phase becomes dispatchable.
			// Only meaningful when the phase actually has dependencies.
			if len(phase.Dependencies) > 0 {
				e.emit(ctx, event.DAGDependencyResolved, phase.ID, "", map[string]any{
					"resolved_deps": phase.Dependencies,
				})
			}

			tm.TrackPhase()
			go func(p *core.Phase, prior string) {
				sem <- struct{}{}        // acquire
				defer func() { <-sem }() // release
				defer tm.PhaseComplete()

				output, err := e.executePhase(ctx, p, prior)
				completionCh <- phaseResult{p.ID, output, err}
			}(phase, priorContext)
		}
	}

	// Initial dispatch
	dispatch()

	// Event loop: wait for completions, dispatch newly-unblocked phases
	completed := len(plan.Phases) - len(pending)
	total := len(plan.Phases)

	for completed < total {
		select {
		case <-ctx.Done():
			// Emit terminal events for phases that were never dispatched.
			// These have Status == Pending and no goroutine — they will never
			// receive a completion result, so we mark them skipped here before
			// the function returns, ensuring LiveState and the event log always
			// see every phase reach a terminal state.
			//
			// Use context.Background() so the phase.skipped events write even
			// though ctx is already cancelled at this point.
			emitCtx := context.Background()
			for id := range pending {
				phase := e.phases[id]
				if phase.Status == core.StatusPending {
					phase.Status = core.StatusSkipped
					phase.Error = "skipped: mission cancelled"
					e.emit(emitCtx, event.PhaseSkipped, id, "", map[string]any{
						"reason": "mission cancelled",
					})
				}
			}
			// mission.cancelled is emitted by Execute() after this returns,
			// centralising all mission-terminal events in one place and
			// preventing the double-emission that occurred when this function
			// emitted mission.cancelled and Execute() then also emitted
			// mission.failed because err != nil.
			result.Success = false
			result.Error = "cancelled"
			result.Duration = time.Since(start)
			return result, ctx.Err()

		case pr := <-completionCh:
			completed++
			delete(pending, pr.phaseID)

			phase := e.phases[pr.phaseID]
			now := time.Now()
			phase.EndTime = &now

			if errors.Is(pr.err, errQuotaGateSkip) {
				// Quota-gate skip: phase.Status is already StatusSkipped.
				// Don't treat this as a mission failure or cascade-skip dependents.
			} else if pr.err != nil {
				phase.Status = core.StatusFailed
				phase.Error = pr.err.Error()
				result.Success = false

				// Skip transitive dependents and count them toward completion
				// so the event loop terminates even when phases are skipped.
				completed += e.skipDependents(ctx, pr.phaseID, pending)

				if e.config.Verbose {
					fmt.Printf("[engine] phase %s failed: %v\n", phase.Name, pr.err)
				}
			} else {
				phase.Status = core.StatusCompleted
				phase.Output = summarize(pr.output, 500)

				outputMu.Lock()
				phaseOutputs[pr.phaseID] = pr.output
				outputMu.Unlock()

				if e.config.Verbose {
					fmt.Printf("[engine] phase %s completed\n", phase.Name)
				}

				e.commitPhaseWork(phase)
			}

			e.saveCheckpoint(ctx)

			// Dispatch newly-unblocked phases
			dispatch()
		}
	}

	// Combine all outputs
	var allOutputs []string
	for _, p := range plan.Phases {
		if out, ok := phaseOutputs[p.ID]; ok {
			allOutputs = append(allOutputs, out)
		}
	}

	// Detect file overlaps across all parallel phases that completed successfully.
	e.detectFileOverlaps(ctx, plan.Phases)

	result.Output = strings.Join(allOutputs, "\n\n---\n\n")
	result.Duration = time.Since(start)
	result.Artifacts = e.collectAllArtifacts()
	return result, nil
}

func (e *Engine) executePhase(ctx context.Context, phase *core.Phase, priorContext string) (string, error) {
	phase.Status = core.StatusRunning
	now := time.Now()
	phase.StartTime = &now
	e.updateLockPhase(phase.Name)
	phaseID := phaseRuntimeID(phase)

	// Inherit workspace-level target dir when the phase has not specified its own.
	// This lets the orchestrator set one target for all phases without requiring
	// every PHASE line to repeat the WORKDIR field.
	if phase.TargetDir == "" && e.workspace.TargetDir != "" {
		phase.TargetDir = e.workspace.TargetDir
	}

	// Pre-flight: validate runtime contract for this phase's role.
	if err := e.checkContract(ctx, phase); err != nil {
		e.emit(ctx, event.PhaseFailed, phaseID, "", map[string]any{
			"error": fmt.Sprintf("contract: %v", err),
		})
		return "", fmt.Errorf("contract check: %w", err)
	}

	// Quota gate: check 5h token utilization before spawning a worker.
	// Block/skip actions are resolved here, before phase.started is emitted,
	// so skipped phases never appear as "started" in the event log.
	throttle, util := e.checkQuotaGate(phase)
	switch throttle {
	case throttleBlock:
		msg := fmt.Sprintf("quota gate: 5h utilization %.1f%% >= 95%% — all phases blocked", util*100)
		phase.Status = core.StatusFailed
		phase.Error = msg
		endNow := time.Now()
		phase.EndTime = &endNow
		e.emit(ctx, event.PhaseFailed, phaseID, "", map[string]any{
			"error": msg,
		})
		return "", fmt.Errorf("%s", msg)
	case throttleSkip:
		msg := fmt.Sprintf("quota gate: non-P0 phase skipped at %.1f%% 5h utilization", util*100)
		phase.Status = core.StatusSkipped
		phase.Error = msg
		endNow := time.Now()
		phase.EndTime = &endNow
		e.emit(ctx, event.PhaseSkipped, phaseID, "", map[string]any{
			"reason": msg,
		})
		return "", errQuotaGateSkip
	}

	// Emit point 3: phase.started
	phaseStartData := map[string]any{
		"name":    phase.Name,
		"persona": phase.Persona,
		"model":   phase.ModelTier,
		"skills":  phase.Skills,
	}
	if phase.Role != "" {
		phaseStartData["role"] = string(phase.Role)
	}
	if phase.TargetDir != "" {
		phaseStartData["target_dir"] = phase.TargetDir
	}
	if phase.Runtime != "" {
		phaseStartData["runtime"] = string(phase.Runtime)
	}
	if phase.RuntimePolicyApplied {
		phaseStartData["runtime_policy_applied"] = true
	}
	e.emit(ctx, event.PhaseStarted, phaseID, "", phaseStartData)

	if e.config.Verbose {
		fmt.Printf("[engine] starting phase %s (%s)\n", phase.Name, phase.Persona)
	}

	// Get persona focus areas for learning retrieval boost
	focusAreas := persona.GetLearningFocus(phase.Persona)

	// Fetch relevant learnings (with persona focus boost if available)
	var learningsText string
	if e.learningDB != nil && !e.config.DisableLearnings {
		learnings, err := e.learningDB.FindRelevant(ctx, phase.Objective, e.workspace.Domain, learningsLimitForPhase(phase), e.embedder, focusAreas)
		if err == nil && len(learnings) > 0 {
			phase.LearningsRetrieved = len(learnings)
			var parts []string
			for _, l := range learnings {
				parts = append(parts, fmt.Sprintf("- [%s] %s", l.Type, truncateContent(l.Content, 500)))
			}
			learningsText = strings.Join(parts, "\n")

			// Track injected learnings for post-mission compliance scan.
			e.mu.Lock()
			e.injectedLearnings = append(e.injectedLearnings, learnings...)
			e.mu.Unlock()

			// Record injection events (best-effort — don't fail the phase).
			ids := make([]string, len(learnings))
			for i, l := range learnings {
				ids[i] = l.ID
			}
			e.learningDB.RecordInjections(ctx, ids) //nolint:errcheck
		}
	}

	// Build handoff records for dependency phases with different roles.
	// This gives the worker explicit awareness of role transitions in the
	// plan/implement/review lifecycle.
	handoffs := e.buildHandoffs(phase)

	// Emit role.handoff events for each cross-role transition.
	for _, h := range handoffs {
		e.emit(ctx, event.RoleHandoff, phaseID, "", map[string]any{
			"from_phase":   h.FromPhaseID,
			"from_role":    string(h.FromRole),
			"to_role":      string(h.ToRole),
			"from_persona": h.FromPersona,
			"to_persona":   h.ToPersona,
		})
	}

	// Collect scratch notes from completed dependency phases.
	priorScratch := e.collectPriorScratch(phase)

	// Build context bundle
	bundle := core.ContextBundle{
		Objective:      phase.Objective,
		MissionContext: extractMissionContext(e.plan.Task),
		SkillIndex:     e.skillIndex,
		Constraints:    phase.Constraints,
		PriorContext:   priorContext,
		Learnings:      learningsText,
		Domain:         e.workspace.Domain,
		WorkspaceID:    e.workspace.ID,
		PhaseID:        phaseID,
		TargetDir:      phase.TargetDir,
		Handoffs:       handoffs,
		Role:           phase.Role,
		Runtime:        phase.Runtime,
		PriorScratch:   priorScratch,
		// WorkerDir and ScratchDir are populated by worker.Spawn after the directory is created.
	}

	// Guard: OBJECTIVE must not exceed 8192 bytes to prevent oversized prompts.
	if len(phase.Objective) > maxObjectiveBytes {
		e.emit(ctx, event.PhaseFailed, phaseID, "", map[string]any{
			"error": fmt.Sprintf("objective too large: %d bytes (limit %d)", len(phase.Objective), maxObjectiveBytes),
		})
		return "", fmt.Errorf("phase %q objective is %d bytes, exceeds %d-byte limit", phase.Name, len(phase.Objective), maxObjectiveBytes)
	}

	// Spawn worker
	config, err := worker.Spawn(e.workspace.Path, phase, bundle)
	if err != nil {
		// Emit point 4: phase.failed (spawn error)
		e.emit(ctx, event.PhaseFailed, phaseID, "", map[string]any{
			"error": fmt.Sprintf("spawn: %v", err),
		})
		return "", fmt.Errorf("spawn worker: %w", err)
	}

	// Override model: quota gate downgrade takes priority, then --model flag.
	if throttle == throttleForceSonnet {
		config.Model = "sonnet"
	} else if e.config.ForcedModel != "" {
		config.Model = e.config.ForcedModel
	}

	// Apply max-turns guardrail so workers can't run indefinitely.
	// The plumbing in transport.go passes --max-turns N when MaxTurns > 0.
	if config.MaxTurns <= 0 {
		config.MaxTurns = resolveMaxTurns(e.config.MaxTurns, phase.Persona)
	}

	// Apply stall timeout: per-phase TIMEOUT: takes precedence over global --stall-timeout.
	config.StallTimeout = phase.StallTimeout
	if config.StallTimeout == 0 && e.config.StallTimeout > 0 {
		config.StallTimeout = e.config.StallTimeout
	}

	// No per-phase timeout — let agents run to completion
	phaseCtx := ctx

	// Execute with retry (up to 2 retries with exponential backoff)
	var output string
	const maxAttempts = 3
	backoff := time.Second
	for attempt := 1; attempt <= maxAttempts; attempt++ {
		// Pass the saved session ID so Claude can resume a prior conversation.
		// On the first attempt this is populated from a checkpoint; on retries it
		// holds the session ID captured from the previous (failed) attempt.
		config.ResumeSessionID = phase.SessionID

		var sessionID string
		var phaseCost *sdk.CostInfo
		captureEmitter := newPhaseToolCaptureEmitter(phaseID, e.emitter)
		output, sessionID, phaseCost, err = e.resolveExecutor(phase.Runtime).Execute(phaseCtx, config, captureEmitter, e.config.Verbose)
		if sessionID != "" {
			phase.SessionID = sessionID
		}
		if parsedSkills := ParseSkillInvocations(captureEmitter.Transcript()); len(parsedSkills) > 0 {
			phase.ParsedSkills = appendRetryParsedSkills(phase.ParsedSkills, parsedSkills)
		}
		if phaseCost != nil {
			phase.TokensIn += phaseCost.InputTokens
			phase.TokensOut += phaseCost.OutputTokens
			phase.TokensCacheCreation += phaseCost.CacheCreationTokens
			phase.TokensCacheRead += phaseCost.CacheReadTokens
			phase.CostUSD += phaseCost.TotalCostUSD
			if phase.Model == "" {
				phase.Model = config.Model
			}
		}
		if err == nil {
			break
		}
		if attempt == maxAttempts {
			// Emit point 4: phase.failed (terminal — no more retries)
			e.emit(ctx, event.PhaseFailed, phaseID, config.Name, map[string]any{
				"error":   err.Error(),
				"attempt": attempt,
			})
			return output, err
		}
		phase.Retries++

		// Emit phase.failed for every worker failure so dashboards and silent-failure
		// detectors see an event immediately, not only after retries are exhausted.
		// The subsequent phase.retrying event signals that execution will continue.
		e.emit(ctx, event.PhaseFailed, phaseID, config.Name, map[string]any{
			"error":    err.Error(),
			"attempt":  attempt,
			"retrying": true,
		})

		// Emit point: phase.retrying
		e.emit(ctx, event.PhaseRetrying, phaseID, config.Name, map[string]any{
			"attempt": attempt,
			"backoff": backoff.String(),
			"error":   err.Error(),
		})

		if e.config.Verbose {
			fmt.Printf("[engine] phase %s attempt %d failed: %v — retrying in %s\n", phase.Name, attempt, err, backoff)
		}
		select {
		case <-phaseCtx.Done():
			e.emit(ctx, event.PhaseFailed, phaseID, config.Name, map[string]any{
				"error": phaseCtx.Err().Error(),
			})
			return output, phaseCtx.Err()
		case <-time.After(backoff):
			backoff *= 2
		}
	}

	// Quality gate check: warn or block depending on GateMode.
	// Default (zero value) is block so code missions fail-fast on bad output.
	gate := CheckGate(output, parseExpectedPaths(phase.Expected, e.plan.Task))
	phase.GatePassed = gate.Passed
	if !gate.Passed {
		if e.config.GateMode == core.GateModeWarn {
			fmt.Printf("[engine] phase %s gate warning: %s (output_len=%d, retries=%d)\n",
				phase.Name, gate.Reason, len(output), phase.Retries)
		} else {
			// GateModeBlock (default): fail the phase so dependents are skipped.
			if e.config.Verbose {
				fmt.Printf("[engine] phase %s gate failed: %s (output_len=%d, retries=%d, model=%s, session=%s)\n",
					phase.Name, gate.Reason, len(output), phase.Retries, phase.Model, phase.SessionID)
			}
			e.emit(ctx, event.PhaseFailed, phaseID, config.Name, map[string]any{
				"error":      fmt.Sprintf("gate: %s", gate.Reason),
				"output_len": len(output),
				"retries":    phase.Retries,
				"model":      phase.Model,
				"session_id": phase.SessionID,
			})
			return output, fmt.Errorf("gate: %s (output_len=%d, retries=%d)", gate.Reason, len(output), phase.Retries)
		}
	}

	// Read and apply completion signal from worker directory.
	sig, sigErr := core.ReadSignalFile(config.WorkerDir)
	if sigErr != nil {
		if e.config.Verbose {
			fmt.Printf("[engine] phase %s: signal read error: %v\n", phase.Name, sigErr)
		}
	} else if sig.Kind != core.SignalOK {
		output, err = e.processSignal(ctx, sig, phase, phaseID, config.Name, output)
		if err != nil {
			return output, err
		}
	}

	// Track output length
	phase.OutputLen = len(output)

	// Create phase artifact directory and merge.
	// Failures here are non-fatal: log and emit a warning so the operator is
	// informed, but the phase is still considered complete.
	e.handleArtifactMerge(ctx, phase, config)

	// Capture learnings from this phase's output in a background goroutine.
	// extractMu ensures at most one extraction runs per mission (rate limiter),
	// and extractWG tracks outstanding goroutines for graceful shutdown.
	// Errors are non-fatal: logged but never propagate to the caller.
	if e.learningDB != nil {
		outputSnap := output
		workerName := config.Name
		domain := e.workspace.Domain
		wsID := e.workspace.ID
		personaName := phase.Persona
		phaseName := phase.Name

		e.extractWG.Add(1)
		go func() {
			defer e.extractWG.Done()

			// Max-1-per-mission: acquire mutex before any extraction work.
			e.extractMu.Lock()
			defer e.extractMu.Unlock()

			extractCtx := context.Background()

			// Marker-based extraction (same as `orchestrator learn`).
			captured := learning.CaptureFromText(outputSnap, workerName, domain, wsID)

			// LLM-based extraction guided by persona focus areas.
			if focusAreas := persona.GetLearningFocus(personaName); len(focusAreas) > 0 {
				focused := learning.CaptureWithFocus(extractCtx, outputSnap, focusAreas, workerName, domain, wsID)
				captured = append(captured, focused...)
			}

			stored := 0
			for _, l := range captured {
				if insErr := e.learningDB.Insert(extractCtx, l, e.embedder); insErr != nil {
					if e.config.Verbose {
						fmt.Printf("[engine] background learning insert failed (phase %s): %v\n", phaseName, insErr)
					}
					continue // non-fatal
				}
				stored++
				e.emit(extractCtx, event.LearningStored, "", workerName, map[string]any{
					"learning_id":   l.ID,
					"learning_type": string(l.Type),
					"content":       l.Content,
					"phase":         phaseName,
					"domain":        domain,
				})
			}

			if e.config.Verbose && stored > 0 {
				fmt.Printf("[engine] background: stored %d learnings from phase %s\n", stored, phaseName)
			}
		}()
	}

	phase.Status = core.StatusCompleted
	endNow := time.Now()
	phase.EndTime = &endNow
	phase.Output = summarize(output, 500)
	if err := recordPhaseSkillsDB(e.workspace, e.plan, phase, e.startTime); err != nil {
		fmt.Fprintf(os.Stderr, "warning: phase skill metrics write failed: %v\n", err)
	}

	// Record which files this phase changed relative to the base branch.
	// Used later by detectFileOverlaps to surface cross-phase conflicts.
	e.recordChangedFiles(phase)

	// Validate persona output contract (advisory only — never blocks completion).
	if phase.Persona != "" && phase.Role != "" {
		if warnings := persona.ValidateOutput(phase.Persona, string(phase.Role), output); len(warnings) > 0 {
			for _, w := range warnings {
				if e.config.Verbose {
					fmt.Printf("[engine] persona contract warning: %s\n", w)
				}
				e.emit(ctx, event.PersonaContractViolation, phaseID, config.Name, map[string]any{
					"persona": phase.Persona,
					"role":    string(phase.Role),
					"pattern": w.Pattern,
					"message": w.Message,
				})
			}
		}
	}

	// Emit point 5: phase.completed
	completedData := map[string]any{
		"output_len":  len(output),
		"gate_passed": gate.Passed,
		"retries":     phase.Retries,
	}
	if phase.Role != "" {
		completedData["role"] = string(phase.Role)
	}
	e.emit(ctx, event.PhaseCompleted, phaseID, config.Name, completedData)

	// Extract scratch blocks from phase output and persist them.
	e.extractScratch(phase, output)

	// Prefer the worker's output.md artifact for the prior context payload.
	// worker.Execute writes the SDK text there; agents may also write their own
	// curated version. Reading from the file ensures the caller always gets the
	// artifact content rather than the raw streaming output.
	if artifact, readErr := os.ReadFile(filepath.Join(config.WorkerDir, "output.md")); readErr == nil && len(artifact) > 0 {
		return string(artifact), nil
	}
	return output, nil
}

// processSignal applies the completion signal policy to a phase.
// For partial: stores remainder in phase metadata and appends to output.
// For dependency_missing: returns an error so the caller fails the phase.
// For scope_expansion/replan_required/human_decision_needed: emits typed
// events for dashboard/channels and logs a warning.
func (e *Engine) processSignal(ctx context.Context, sig core.CompletionSignal, phase *core.Phase, phaseID, workerName, output string) (string, error) {
	switch sig.Kind {
	case core.SignalPartial:
		phase.SignalRemainder = sig.Remainder
		if sig.Remainder != "" {
			output += "\n\n## Remaining Work\n\n" + sig.Remainder
		}
		fmt.Printf("[engine] phase %s: partial completion — remainder stored for dependents\n", phase.Name)

	case core.SignalDependencyMissing:
		missing := strings.Join(sig.MissingInput, ", ")
		failErr := fmt.Errorf("signal: dependency missing: %s", missing)
		e.emit(ctx, event.PhaseFailed, phaseID, workerName, map[string]any{
			"error":         failErr.Error(),
			"signal":        string(sig.Kind),
			"missing_input": sig.MissingInput,
		})
		return output, failErr

	case core.SignalScopeExpansion:
		e.emit(ctx, event.SignalScopeExpansion, phaseID, workerName, map[string]any{
			"summary":          sig.Summary,
			"suggested_phases": sig.SuggestedPhases,
		})
		fmt.Printf("[engine] phase %s: scope expansion — %s\n", phase.Name, sig.Summary)

	case core.SignalReplanRequired:
		e.emit(ctx, event.SignalReplanRequired, phaseID, workerName, map[string]any{
			"summary":          sig.Summary,
			"suggested_phases": sig.SuggestedPhases,
		})
		fmt.Printf("[engine] phase %s: replan required — %s\n", phase.Name, sig.Summary)

	case core.SignalHumanDecisionNeeded:
		e.emit(ctx, event.SignalHumanDecisionNeeded, phaseID, workerName, map[string]any{
			"summary": sig.Summary,
		})
		fmt.Printf("[engine] phase %s: human decision needed — %s\n", phase.Name, sig.Summary)
	}

	return output, nil
}

// buildHandoffs creates HandoffRecord entries for dependency phases whose role
// differs from the current phase's role. This captures the structured context
// of role transitions (planner→implementer, implementer→reviewer, etc.) and
// makes it available to the worker via CLAUDE.md injection.
func (e *Engine) buildHandoffs(phase *core.Phase) []core.HandoffRecord {
	if phase.Role == "" || len(phase.Dependencies) == 0 {
		return nil
	}

	var handoffs []core.HandoffRecord
	for _, depID := range phase.Dependencies {
		dep, ok := e.phases[depID]
		if !ok || dep.Status != core.StatusCompleted {
			continue
		}
		// Only create handoff records for cross-role transitions.
		if dep.Role == "" || dep.Role == phase.Role {
			continue
		}

		summary := summarize(dep.Output, 300)
		expectations := core.RoleTransitionExpectations(dep.Role, phase.Role)

		handoffs = append(handoffs, core.HandoffRecord{
			FromPhaseID:  dep.ID,
			ToPhaseID:    phase.ID,
			FromRole:     dep.Role,
			ToRole:       phase.Role,
			FromPersona:  dep.Persona,
			ToPersona:    phase.Persona,
			Summary:      summary,
			Expectations: expectations,
		})
	}
	return handoffs
}

func (e *Engine) depsCompleted(phase *core.Phase) bool {
	for _, depID := range phase.Dependencies {
		dep, ok := e.phases[depID]
		if !ok {
			continue
		}
		if dep.Status != core.StatusCompleted {
			return false
		}
	}
	return true
}

func (e *Engine) skipDependents(ctx context.Context, failedID string, pending map[string]bool) int {
	skipped := 0
	for id := range pending {
		phase := e.phases[id]
		// Never overwrite a phase that has already reached a terminal state
		// (completed, failed, or skipped from a prior run / checkpoint).
		// A terminal phase must not be re-labelled by transitive skip propagation.
		if phase.Status.IsTerminal() {
			delete(pending, id)
			continue
		}
		for _, depID := range phase.Dependencies {
			if depID == failedID {
				phase.Status = core.StatusSkipped
				phase.Error = fmt.Sprintf("skipped: dependency %s failed", failedID)
				delete(pending, id)
				skipped++

				// Emit phase.skipped
				e.emit(ctx, event.PhaseSkipped, id, "", map[string]any{
					"reason": phase.Error,
				})

				// Recursively skip dependents of this phase too
				skipped += e.skipDependents(ctx, id, pending)
				break
			}
		}
	}
	return skipped
}

func (e *Engine) collectAllArtifacts() []string {
	mergedDir := core.MergedArtifactsDir(e.workspace.Path)
	var artifacts []string

	filepath.WalkDir(mergedDir, func(path string, d os.DirEntry, err error) error {
		if err == nil && !d.IsDir() {
			artifacts = append(artifacts, path)
		}
		return nil
	})

	return artifacts
}

// handleArtifactMerge creates the phase artifact directory and merges worker
// output into it and the merged artifacts directory. Errors are non-fatal:
// they are logged and emitted as system.error warning events so the operator
// is informed without failing the phase.
func (e *Engine) handleArtifactMerge(ctx context.Context, phase *core.Phase, config *core.WorkerConfig) {
	phaseArtifactDir, err := core.CreatePhaseArtifactDir(e.workspace.Path, phase.ID)
	if err != nil {
		fmt.Printf("[engine] artifact dir creation failed for phase %s: %v\n", phase.Name, err)
		e.emit(ctx, event.SystemError, phase.ID, config.Name, map[string]any{
			"error":   fmt.Sprintf("creating artifact dir: %v", err),
			"warning": true,
		})
		return
	}

	confidence := "medium"
	if phase.GatePassed {
		confidence = "high"
	}
	meta := worker.ArtifactMeta{
		ProducedBy: config.Bundle.PersonaName,
		Role:       string(phase.Role),
		Phase:      phase.Name,
		Workspace:  e.workspace.ID,
		CreatedAt:  time.Now(),
		Confidence: confidence,
		DependsOn:  phase.Dependencies,
	}

	mergedDir := core.MergedArtifactsDir(e.workspace.Path)
	if mergeErr := worker.MergeArtifactsWithMeta(config.WorkerDir, phaseArtifactDir, mergedDir, meta); mergeErr != nil {
		fmt.Printf("[engine] artifact merge failed for phase %s: %v\n", phase.Name, mergeErr)
		e.emit(ctx, event.SystemError, phase.ID, config.Name, map[string]any{
			"error":   fmt.Sprintf("merging artifacts: %v", mergeErr),
			"warning": true,
		})
	}
}

// recordChangedFiles runs git diff --name-only against the base branch in the
// phase's target directory and stores the result in phase.ChangedFiles.
// No-op when the phase has no TargetDir or the workspace has no BaseBranch.
func (e *Engine) recordChangedFiles(phase *core.Phase) {
	targetDir := phase.TargetDir
	if targetDir == "" {
		return
	}
	base := e.workspace.BaseBranch
	if base == "" {
		return
	}
	files, err := git.DiffNameOnly(targetDir, base)
	if err != nil {
		if e.config.Verbose {
			fmt.Printf("[engine] phase %s: git diff failed: %v\n", phase.Name, err)
		}
		return
	}
	phase.ChangedFiles = files
}

// detectFileOverlaps compares ChangedFiles across all completed phases and
// emits a file_overlap.detected event for each file that appears in more than
// one phase. Severity is "high" when the file was already tracked by git
// (modified by both phases) and "medium" when it is a newly added file.
// Logs a warning to the terminal for each overlap detected.
func (e *Engine) detectFileOverlaps(ctx context.Context, phases []*core.Phase) {
	// Collect only completed phases that have ChangedFiles data.
	type phaseEntry struct {
		id   string
		name string
	}
	fileToPhases := make(map[string][]phaseEntry)
	for _, p := range phases {
		if p.Status != core.StatusCompleted || len(p.ChangedFiles) == 0 {
			continue
		}
		for _, f := range p.ChangedFiles {
			fileToPhases[f] = append(fileToPhases[f], phaseEntry{p.ID, p.Name})
		}
	}

	// Fetch diff status once for severity classification (best-effort).
	// We need a TargetDir and BaseBranch; use the first completed phase with one.
	var statusMap map[string]string
	if e.workspace.BaseBranch != "" {
		for _, p := range phases {
			if p.Status == core.StatusCompleted && p.TargetDir != "" {
				statusMap, _ = git.DiffNameStatus(p.TargetDir, e.workspace.BaseBranch)
				break
			}
		}
	}

	for file, entries := range fileToPhases {
		if len(entries) < 2 {
			continue
		}

		phaseIDs := make([]string, len(entries))
		phaseNames := make([]string, len(entries))
		for i, e := range entries {
			phaseIDs[i] = e.id
			phaseNames[i] = e.name
		}

		severity := "medium"
		if status, ok := statusMap[file]; ok && status == "M" {
			severity = "high"
		}

		e.emit(ctx, event.FileOverlapDetected, "", "", map[string]any{
			"file":     file,
			"phases":   phaseIDs,
			"severity": severity,
		})

		fmt.Printf("[engine] WARNING: file_overlap.detected — %s modified by phases %s (severity: %s)\n",
			file, strings.Join(phaseNames, ", "), severity)
	}
}

// skipSequentialDependents mirrors the dependency-aware failure handling used by
// executeParallel. It scans only future phases in the sequential plan, marks
// transitive dependents of failedID as skipped, and returns the number skipped.
func (e *Engine) skipSequentialDependents(ctx context.Context, plan *core.Plan, currentIdx int, failedID string) int {
	pending := make(map[string]bool)
	for i := currentIdx + 1; i < len(plan.Phases); i++ {
		if plan.Phases[i].Status == core.StatusPending {
			pending[plan.Phases[i].ID] = true
		}
	}
	if len(pending) == 0 {
		return 0
	}
	return e.skipDependents(ctx, failedID, pending)
}

// skipRemainingSequential marks every non-terminal phase from fromIdx onward
// as skipped and emits phase.skipped for each. It is called exclusively from
// the sequential executor's cancellation path, so it uses context.Background()
// to ensure the terminal events write even though ctx is already cancelled.
func (e *Engine) skipRemainingSequential(ctx context.Context, plan *core.Plan, fromIdx int) {
	emitCtx := context.Background()
	for i := fromIdx; i < len(plan.Phases); i++ {
		phase := plan.Phases[i]
		if phase.Status.IsTerminal() {
			continue
		}
		phase.Status = core.StatusSkipped
		phase.Error = "skipped: mission cancelled"
		e.emit(emitCtx, event.PhaseSkipped, phase.ID, "", map[string]any{
			"reason": "mission cancelled",
		})
	}
}

// handleReviewLoop checks whether a completed phase is a review gate and
// drives the bounded autonomous remediation loop:
//  1. Always emits review.findings_emitted so warnings and unresolved blockers
//     are never silently discarded from the event log.
//  2. When blockers exist and the loop bound allows it, injects a fix phase
//     followed immediately by a re-review gate, creating the chain:
//     review → fix → re-review → (fix → re-review) × N until max loops.
//  3. When the loop is exhausted the final unresolved blockers are still
//     visible via the findings event emitted in step 1.
//
// When phase.Runtime == RuntimeBoth the review is executed by both Claude
// (already completed — output is the Claude result) and CodexExecutor
// (executed here via runSecondaryExecutor). Findings from both are merged
// via mergeReviewFindings before driving the fix loop.
//
// priorContext is variadic so existing callers (tests) remain compatible.
// Production callers should pass the phase's dependency output string.
//
// Only called from executeSequential — the parallel executor does not support
// dynamic phase injection in v1.
func (e *Engine) handleReviewLoop(ctx context.Context, phase *core.Phase, output string, priorContext ...string) *core.Phase {
	if !e.IsReviewGate(phase) {
		return nil
	}
	findings := ParseReviewFindings(output)
	if reviewOutputLooksMalformed(output, findings) {
		e.emit(ctx, event.SystemError, phase.ID, "", map[string]any{
			"warning": true,
			"error":   fmt.Sprintf("review output for %s was non-empty but unstructured; triggering re-review retry", phase.Name),
		})
		retryPhase := e.injectRetryReviewPhase(phase)
		if retryPhase != nil {
			e.phases[retryPhase.ID] = retryPhase
			if e.config.Verbose {
				fmt.Printf("[engine] review loop: malformed output, injected retry review phase %s\n", retryPhase.ID)
			}
			return retryPhase
		}
		// Loop exhausted — no retry available; fall through as pass-open.
		return nil
	}

	// For RuntimeBoth review phases, run CodexExecutor as a second reviewer
	// and merge its findings with the Claude findings already parsed above.
	if phase.Runtime == core.RuntimeBoth {
		pc := ""
		if len(priorContext) > 0 {
			pc = priorContext[0]
		}
		codexOutput, err := e.runSecondaryExecutor(ctx, phase, pc, e.resolveExecutor(core.RuntimeCodex))
		if err != nil {
			if e.config.Verbose {
				fmt.Printf("[engine] review loop: codex secondary executor failed for %s: %v — using Claude findings only\n", phase.Name, err)
			}
		} else {
			findings = mergeReviewFindings(findings, ParseReviewFindings(codexOutput))
			if e.config.Verbose {
				fmt.Printf("[engine] review loop: merged findings from both executors for %s: %d blocker(s), %d warning(s)\n",
					phase.Name, len(findings.Blockers), len(findings.Warnings))
			}
		}
	}

	// Always emit findings — warnings and unresolved blockers must not be lost.
	e.emitReviewFindings(ctx, phase, findings)

	if findings.Passed() {
		if e.config.Verbose {
			fmt.Printf("[engine] review gate %s passed (0 blockers)\n", phase.Name)
		}
		return nil
	}

	fixPhase := e.injectFixPhase(phase, findings)
	if fixPhase == nil {
		// Loop exhausted: blockers remain but max iterations reached.
		// The findings were already emitted above; nothing more to inject.
		if e.config.Verbose {
			fmt.Printf("[engine] review loop exhausted for %s: %d blocker(s) unresolved\n",
				phase.Name, len(findings.Blockers))
		}
		return nil
	}

	// Inject a re-review gate so the fix is re-evaluated, continuing the loop.
	reReview := e.injectReReviewPhase(phase, fixPhase)
	if reReview != nil {
		e.phases[reReview.ID] = reReview
		if e.config.Verbose {
			fmt.Printf("[engine] review loop: injected re-review phase %s (iter %d) after fix %s\n",
				reReview.Name, reReview.ReviewIteration, fixPhase.Name)
		}
	}

	return fixPhase
}

// runSecondaryExecutor spawns a lightweight worker and executes it with ex,
// bypassing quota gating, retries, and phase-state mutations. It is used
// exclusively for the CodexExecutor leg of RuntimeBoth review phases.
//
// A phase clone with a "-codex" suffix is used so worker.Spawn creates a
// distinct worker directory and resolves Codex-appropriate model names
// without mutating the original phase.
func (e *Engine) runSecondaryExecutor(ctx context.Context, phase *core.Phase, priorContext string, ex PhaseExecutor) (string, error) {
	clone := *phase
	clone.ID = phase.ID + "-codex"
	clone.Runtime = core.RuntimeCodex

	bundle := core.ContextBundle{
		Objective:      phase.Objective,
		MissionContext: extractMissionContext(e.plan.Task),
		SkillIndex:     e.skillIndex,
		Constraints:    phase.Constraints,
		PriorContext:   priorContext,
		Domain:         e.workspace.Domain,
		WorkspaceID:    e.workspace.ID,
		PhaseID:        clone.ID,
		TargetDir:      phase.TargetDir,
		Handoffs:       e.buildHandoffs(phase),
		Role:           phase.Role,
		Runtime:        core.RuntimeCodex,
	}

	// Guard: OBJECTIVE must not exceed 8192 bytes to prevent oversized prompts.
	if len(phase.Objective) > maxObjectiveBytes {
		return "", fmt.Errorf("phase %q objective is %d bytes, exceeds %d-byte limit", phase.Name, len(phase.Objective), maxObjectiveBytes)
	}

	config, err := worker.Spawn(e.workspace.Path, &clone, bundle)
	if err != nil {
		return "", fmt.Errorf("spawn secondary worker: %w", err)
	}

	if e.config.ForcedModel != "" {
		config.Model = e.config.ForcedModel
	}
	if config.MaxTurns <= 0 {
		config.MaxTurns = resolveMaxTurns(e.config.MaxTurns, clone.Persona)
	}
	config.StallTimeout = phase.StallTimeout
	if config.StallTimeout == 0 && e.config.StallTimeout > 0 {
		config.StallTimeout = e.config.StallTimeout
	}

	output, _, _, err := ex.Execute(ctx, config, e.emitter, e.config.Verbose)
	if err != nil {
		return "", fmt.Errorf("secondary executor: %w", err)
	}
	return output, nil
}

// saveCheckpoint persists plan state and emits system.checkpoint_saved.
// Emit point 7: system.checkpoint_saved
func (e *Engine) saveCheckpoint(ctx context.Context) {
	if err := core.SaveCheckpoint(e.workspace.Path, e.plan, e.config.Domain, "in_progress", e.startTime); err != nil {
		if e.config.Verbose {
			fmt.Printf("[engine] checkpoint save failed: %v\n", err)
		}
		e.emit(ctx, event.SystemError, "", "", map[string]any{
			"op":    "checkpoint",
			"error": err.Error(),
		})
		return
	}
	e.emit(ctx, event.SystemCheckpointSaved, "", "", map[string]any{
		"path": e.workspace.Path,
	})
}

// checkContract validates that the runtime executor for this phase satisfies the
// ownership contract implied by the phase's role. When the executor implements
// RuntimeDescriber, its declared capabilities are checked against ContractForRole.
// Unknown executors (no descriptor) receive a warning but are allowed to proceed
// for backward compatibility. Returns an error only when a described runtime
// is missing required capabilities — this hard-fails the phase before dispatch.
func (e *Engine) checkContract(ctx context.Context, phase *core.Phase) error {
	contract := core.ContractForRole(phase.Role)
	rt := phase.Runtime.Effective()

	desc, known := e.executors.describe(rt)
	if !known {
		// Executor doesn't declare capabilities — warn, don't block.
		if e.config.Verbose {
			fmt.Printf("[engine] contract: runtime %q has no capability descriptor; skipping validation for phase %s\n", rt, phase.Name)
		}
		e.emit(ctx, event.ContractValidated, phase.ID, "", map[string]any{
			"runtime": string(rt),
			"role":    string(phase.Role),
			"outcome": "skipped",
			"reason":  "no descriptor",
		})
		return nil
	}

	result := contract.Validate(desc)

	// Emit warnings for missing preferred capabilities.
	for _, w := range result.Warnings {
		if e.config.Verbose {
			fmt.Printf("[engine] contract warning: %s\n", w)
		}
	}

	if result.Satisfied {
		e.emit(ctx, event.ContractValidated, phase.ID, "", map[string]any{
			"runtime":  string(rt),
			"role":     string(phase.Role),
			"outcome":  "satisfied",
			"warnings": len(result.Warnings),
		})
		return nil
	}

	// Hard failure: runtime cannot fulfill the role's contract.
	missing := make([]string, len(result.Missing))
	for i, c := range result.Missing {
		missing[i] = string(c)
	}
	e.emit(ctx, event.ContractViolated, phase.ID, "", map[string]any{
		"runtime": string(rt),
		"role":    string(phase.Role),
		"missing": missing,
	})
	return fmt.Errorf("runtime %q cannot fulfill %s contract: %s", rt, phase.Role, result.ErrorMessage())
}

// truncateContent caps content at maxLen characters using sentence-boundary logic.
// It finds the last '. ', '! ', or '? ' within the limit; falls back to hard truncation.
func truncateContent(content string, maxLen int) string {
	if len(content) <= maxLen {
		return content
	}
	window := content[:maxLen]
	for i := len(window) - 1; i > 0; i-- {
		if window[i] == ' ' && (window[i-1] == '.' || window[i-1] == '!' || window[i-1] == '?') {
			return window[:i]
		}
	}
	return window + "..."
}

func summarize(text string, maxLen int) string {
	if len(text) <= maxLen {
		return text
	}
	return text[:maxLen] + "..."
}

// extractMissionContext parses key-value pairs from the mission task header.
// It looks for lines matching Target:, Image target:, Type:, Illustration staging:
// and formats them as a markdown bullet list.
func extractMissionContext(taskHeader string) string {
	prefixes := []string{"Target:", "Image target:", "Type:", "Illustration staging:"}
	var bullets []string
	for _, line := range strings.Split(taskHeader, "\n") {
		trimmed := strings.TrimSpace(line)
		for _, prefix := range prefixes {
			if strings.HasPrefix(trimmed, prefix) {
				value := strings.TrimSpace(strings.TrimPrefix(trimmed, prefix))
				if value != "" {
					bullets = append(bullets, fmt.Sprintf("- **%s** %s", strings.TrimSuffix(prefix, ":"), value))
				}
				break
			}
		}
	}
	return strings.Join(bullets, "\n")
}

// personaMaxTurns maps persona keys to their default max-turns cap.
// Personas not listed here fall back to defaultMaxTurns (50).
// Architect phases are planning-oriented and need fewer turns than implementation phases.
var personaMaxTurns = map[string]int{
	"architect": 30,
}

const defaultMaxTurns = 50

// resolveMaxTurns returns the max-turns value for a worker. Priority order:
//  1. Global --max-turns flag (engineMaxTurns > 0)
//  2. Per-persona tier (personaMaxTurns lookup)
//  3. Default (50)
func resolveMaxTurns(engineMaxTurns int, persona string) int {
	if engineMaxTurns > 0 {
		return engineMaxTurns
	}
	if limit, ok := personaMaxTurns[strings.ToLower(persona)]; ok {
		return limit
	}
	return defaultMaxTurns
}

// learningsLimitForPhase returns the number of relevant learnings to inject
// based on how knowledge-intensive the phase is.
func learningsLimitForPhase(phase *core.Phase) int {
	name := strings.ToLower(phase.Name)
	if strings.Contains(name, "research") || strings.Contains(name, "write") {
		return 5
	}
	if strings.Contains(name, "image") || strings.Contains(name, "illustrate") || strings.Contains(name, "social") {
		return 2
	}
	return 3
}

// parseExpectedPaths splits a comma-separated expected field into glob patterns,
// expanding ~ to $HOME and substituting {slug} derived from the task header.
// Returns nil when expected is empty (preserves backward compatibility).
func parseExpectedPaths(expected, taskHeader string) []string {
	if expected == "" {
		return nil
	}
	home := os.Getenv("HOME")
	slug := extractSlugFromTask(taskHeader)
	parts := strings.Split(expected, ",")
	paths := make([]string, 0, len(parts))
	for _, p := range parts {
		p = strings.TrimSpace(p)
		if p == "" {
			continue
		}
		if home != "" {
			p = strings.Replace(p, "~", home, 1)
		}
		if slug != "" {
			p = strings.ReplaceAll(p, "{slug}", slug)
		}
		paths = append(paths, p)
	}
	return paths
}

// extractSlugFromTask finds the slug from the task header.
// Looks for an explicit "Slug:" line first, then falls back to deriving it
// from the filename in the "Target:" line (stripping the extension).
func extractSlugFromTask(taskHeader string) string {
	for _, line := range strings.Split(taskHeader, "\n") {
		line = strings.TrimSpace(line)
		if after, ok := strings.CutPrefix(line, "Slug:"); ok {
			return strings.TrimSpace(after)
		}
	}
	for _, line := range strings.Split(taskHeader, "\n") {
		line = strings.TrimSpace(line)
		if after, ok := strings.CutPrefix(line, "Target:"); ok {
			target := strings.TrimSpace(after)
			base := filepath.Base(target)
			if ext := filepath.Ext(base); ext != "" {
				return strings.TrimSuffix(base, ext)
			}
			return base
		}
	}
	return ""
}

// updateLockPhase updates the phase field in the worktree lock file when a phase starts.
// No-op when git isolation is not active.
func (e *Engine) updateLockPhase(phaseName string) {
	if e.workspace.WorktreePath == "" {
		return
	}
	if err := git.UpdateLockFilePhase(e.workspace.WorktreePath, phaseName); err != nil && e.config.Verbose {
		fmt.Printf("[engine] lock file update failed: %v\n", err)
	}
}

// commitPhaseWork commits all changes in the workspace worktree after a phase
// completes successfully. It is a no-op when git isolation is not active.
// Nothing-to-commit is silently ignored; other errors are logged but do not
// fail the phase.
func (e *Engine) commitPhaseWork(phase *core.Phase) {
	if e.workspace.WorktreePath == "" {
		return
	}
	obj := phase.Objective
	if len(obj) > 72 {
		obj = obj[:72]
	}
	msg := fmt.Sprintf("phase %s: %s", phase.Name, obj)

	hasChanges, err := git.HasUncommittedChanges(e.workspace.WorktreePath)
	if err != nil {
		if e.config.Verbose {
			fmt.Printf("[engine] phase %s: git status check failed: %v\n", phase.Name, err)
		}
		return
	}
	if !hasChanges {
		return
	}

	if err := git.CommitAll(e.workspace.WorktreePath, msg); err != nil {
		if e.config.Verbose {
			fmt.Printf("[engine] phase %s: commit failed: %v\n", phase.Name, err)
		}
		return
	}

	hash, err := git.HeadSHA(e.workspace.WorktreePath)
	if err == nil && len(hash) >= 8 {
		fmt.Printf("[engine] phase %s: committed %s\n", phase.Name, hash[:8])
	}
}
