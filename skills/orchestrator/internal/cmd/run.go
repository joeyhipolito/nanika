package cmd

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"os/signal"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/claims"
	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/decompose"
	"github.com/joeyhipolito/orchestrator-cli/internal/engine"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/git"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	"github.com/joeyhipolito/orchestrator-cli/internal/render"
	"github.com/joeyhipolito/orchestrator-cli/internal/routing"
	"github.com/joeyhipolito/orchestrator-cli/internal/sanitize"
	"github.com/joeyhipolito/orchestrator-cli/internal/worker"
)

var (
	resume       string
	noLearnings  bool
	noReview     bool
	gateMode     string
	templateName string
	saveTemplate string
	gitIsolate   bool
	noGit        bool
	createPR     bool
	noDraft      bool
	codexReview  bool
	noComment    bool
	stallTimeout string
)

func init() {
	runCmd := &cobra.Command{
		Use:   "run [task or mission-file]",
		Short: "Execute a task or mission",
		Long: `Run a task description or a mission file (.md).
Simple tasks get a single worker. Complex tasks are decomposed
into phases with specialized workers.

Use --template to run a saved frozen plan:
  orchestrator run --template <name> [key=value ...]
Use --save-template to freeze a plan after execution:
  orchestrator run <task> --save-template <name>`,
		Args: cobra.ArbitraryArgs,
		RunE: runTask,
	}

	runCmd.Flags().StringVar(&resume, "resume", "", "resume from workspace path")
	runCmd.Flags().BoolVar(&noLearnings, "no-learnings", false, "skip learning retrieval and injection")
	runCmd.Flags().BoolVar(&noReview, "no-review", false, "skip automatic review-phase injection after decomposition")
	runCmd.Flags().StringVar(&gateMode, "gate-mode", "block", "quality gate mode: block (fail phase on bad output) or warn (log and continue)")
	runCmd.Flags().StringVar(&templateName, "template", "", "run from a saved template (skips decomposition)")
	runCmd.Flags().StringVar(&saveTemplate, "save-template", "", "save plan as reusable template after execution")
	runCmd.Flags().BoolVar(&gitIsolate, "git-isolate", true, "execute in an isolated git worktree (auto-enabled for git-repo targets)")
	runCmd.Flags().BoolVar(&noGit, "no-git", false, "skip git isolation even when target is a git repository")
	runCmd.Flags().BoolVar(&createPR, "pr", false, "open a GitHub pull request after a successful run (requires gh CLI)")
	runCmd.Flags().BoolVar(&noDraft, "no-draft", false, "create PR as ready-for-review instead of draft (only used with --pr)")
	runCmd.Flags().BoolVar(&codexReview, "codex-review", false, "post @codex review request comment on the PR after creation (requires --pr)")
	runCmd.Flags().BoolVar(&noComment, "no-comment", false, "skip posting summary comment to Linear issue after completion")
	runCmd.Flags().StringVar(&stallTimeout, "stall-timeout", "", "watchdog stall timeout per phase (e.g. 10m, 30m); overrides ORCHESTRATOR_STALL_TIMEOUT env var")

	rootCmd.AddCommand(runCmd)
}

func runTask(cmd *cobra.Command, args []string) error {
	if templateName == "" && len(args) == 0 {
		return fmt.Errorf("provide a task or use --template <name>")
	}

	// --pr requires git isolation; --no-git or --git-isolate=false disables
	// isolation, so the combination is contradictory and must be rejected up front.
	if createPR && (noGit || !gitIsolate) {
		return fmt.Errorf("--pr requires git isolation: cannot combine with --no-git or --git-isolate=false")
	}
	// --codex-review only makes sense when a PR is being created.
	if codexReview && !createPR {
		return fmt.Errorf("--codex-review requires --pr")
	}

	var task string
	var missionPath string
	if templateName == "" {
		task = strings.Join(args, " ")
		// If argument is a file, read its contents
		if strings.HasSuffix(task, ".md") {
			// Capture absolute path before reading so buildTargetContext can
			// resolve the target from the mission file's git root (source 2).
			if abs, err := filepath.Abs(task); err == nil {
				missionPath = abs
			} else {
				missionPath = task
			}
			data, err := os.ReadFile(task)
			if err != nil {
				return fmt.Errorf("read mission file: %w", err)
			}
			task = string(data)
		}
	}

	// Setup context with signal handling
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()

	sigCh := make(chan os.Signal, 1)
	signal.Notify(sigCh, syscall.SIGINT, syscall.SIGTERM)
	go func() {
		<-sigCh
		fmt.Println("\ninterrupted, cleaning up...")
		cancel()
	}()

	// Open learning DB
	db, err := learning.OpenDB("")
	if err != nil {
		if verbose {
			fmt.Printf("warning: could not open learning DB: %v\n", err)
		}
		db = nil
	}
	if db != nil {
		defer db.Close()
	}

	// Setup embedder
	apiKey := learning.LoadAPIKey()
	embedder := learning.NewEmbedder(apiKey)

	// Handle resume
	if resume != "" {
		if err := core.ValidateWorkspacePath(resume); err != nil {
			return fmt.Errorf("invalid --resume path: %w", err)
		}
		return resumeMission(ctx, resume, db, embedder)
	}

	// Load template if specified — provides both plan and task, skipping decomposition.
	var plan *core.Plan
	if templateName != "" {
		tmpl, err := core.LoadTemplate(templateName)
		if err != nil {
			return err
		}
		params := core.ParseTemplateParams(args)
		plan, err = core.PlanFromTemplate(tmpl, params)
		if err != nil {
			return fmt.Errorf("apply template: %w", err)
		}
		plan.DecompSource = core.DecompTemplate
		task = plan.Task
		fmt.Printf("using template %q (%d phases, %s)\n", templateName, len(plan.Phases), plan.ExecutionMode)
	}

	// Capture learnings from the most recent prior workspace so they're
	// available to FindRelevant below. Without this, learnings from a
	// just-finished mission only enter the DB when `orchestrator learn`
	// is run manually — which happens after decomposition.
	if db != nil {
		capturePriorLearnings(ctx, db, embedder, domain)
	}

	// Fetch learnings for decomposer
	var learningsText string
	if db != nil && !noLearnings {
		learnings, err := db.FindRelevant(ctx, task, domain, 3, embedder)
		if err == nil && len(learnings) > 0 {
			var parts []string
			for _, l := range learnings {
				parts = append(parts, fmt.Sprintf("- [%s] %s", l.Type, l.Content))
			}
			learningsText = strings.Join(parts, "\n")
		}
	}

	// Load skill index (used by both decomposer and engine)
	skillIndex := worker.LoadSkillIndex()

	// For non-dry-run: create workspace and build emitter before decompose so
	// decompose lifecycle events are captured in the mission's event log.
	var ws *core.Workspace
	var emitter event.Emitter = event.NoOpEmitter{}
	missionID := ""

	if !dryRun {
		var wsErr error
		ws, wsErr = core.CreateWorkspace(task, domain)
		if wsErr != nil {
			return fmt.Errorf("create workspace: %w", wsErr)
		}
		missionID = ws.ID
		emitter = buildEmitter(ws.ID, verbose)
		defer emitter.Close()
		learning.SetEmitter(emitter)

		// Write PID so `orchestrator cancel` can signal this process.
		// Always warn on failure — without the PID file, cancel falls back
		// to manual cleanup and cannot signal the live process.
		if err := core.WritePID(ws.Path); err != nil {
			fmt.Fprintf(os.Stderr, "warning: could not write pid file: %v\n", err)
		}

		fmt.Printf("workspace: %s\n\n", ws.Path)
	}

	// Resolve target context from routing memory.
	tc := buildTargetContext(ctx, missionPath, task)

	// Wire --no-review flag: suppress automatic review-phase injection.
	if noReview {
		if tc == nil {
			tc = &decompose.TargetContext{}
		}
		tc.SkipReviewInjection = true
	}

	// Persist target ID to workspace so audit ingestion can map workspace→repo.
	// Also persist task_type so resumed missions use the same classification
	// without re-deriving it from the task text.
	if ws != nil && tc != nil && tc.TargetID != "" {
		if err := os.WriteFile(filepath.Join(ws.Path, "target_id"), []byte(tc.TargetID), 0600); err != nil && verbose {
			fmt.Printf("warning: could not write target_id: %v\n", err)
		}
		if tc.TaskType != "" {
			if err := os.WriteFile(filepath.Join(ws.Path, "task_type"), []byte(tc.TaskType), 0600); err != nil && verbose {
				fmt.Printf("warning: could not write task_type: %v\n", err)
			}
		}
		ws.TargetDir = core.ResolveTargetDir(tc.TargetID)
		// Only warn when a repo: target was expected to have a local directory but
		// doesn't. Non-repo targets (system:, publication:) have no directory by
		// design — emitting a warning for them would be misleading.
		if ws.TargetDir == "" && strings.HasPrefix(tc.TargetID, "repo:") && verbose {
			fmt.Printf("warning: target repository %q not found on disk, workers will run in WorkerDir\n", tc.TargetID)
		}
	}

	// Parse mission frontmatter early so type/domain can influence git isolation
	// below, and so the same parsed values are reused for sidecar writing and PR
	// options without re-scanning the task text.
	fm := parseMissionFrontmatter(task)

	// Set up git worktree isolation when enabled (default) and the target is a
	// git repository on disk. Silently skipped when not a git repo so that
	// non-repo targets work unmodified. --no-git opts out explicitly.
	//
	// Missions whose frontmatter declares a non-code type (research, evaluation,
	// review) or a non-dev domain skip isolation unless the user explicitly
	// passed --git-isolate or --pr on the command line (--pr needs a branch).
	gitIsolateExplicit := cmd.Flags().Changed("git-isolate")
	skipGitForMission := !gitIsolateExplicit && !createPR && missionSkipsGit(fm)
	if gitIsolate && !noGit && !skipGitForMission && ws != nil && ws.TargetDir != "" && git.FindRoot(ws.TargetDir) != "" {
		if err := setupGitIsolation(ws, task, emitter, missionID); err != nil {
			return fmt.Errorf("git isolation setup: %w", err)
		}
	}

	// Advisory file-claim registry: claim the repo root for this mission so
	// parallel missions targeting the same repo see a warning. Claims are
	// repo-level (not per-file) to avoid bloating the DB and decomposer prompt
	// on large repositories. Claims are released explicitly on successful
	// completion; failed missions retain them so parallel missions continue to
	// see the conflict warning. Early exits still release via the deferred flag.
	// All operations are best-effort — a DB failure never blocks the mission.
	var claimsDB *claims.DB
	releaseClaimsOnExit := false
	if ws != nil && ws.GitRepoRoot != "" {
		if cdb, err := claims.OpenDB(""); err == nil {
			claimsDB = cdb
			releaseClaimsOnExit = true
			defer func() {
				if releaseClaimsOnExit {
					if rErr := claimsDB.ReleaseAll(missionID); rErr != nil && verbose {
						fmt.Printf("warning: could not release file claims: %v\n", rErr)
					}
				}
				if err := claimsDB.Close(); err != nil && verbose {
					fmt.Printf("warning: could not close file claims DB: %v\n", err)
				}
			}()

			// Claim-first pattern: write our claim before checking for
			// conflicts to close the race window where two missions both
			// read zero conflicts before either writes.
			repoMarker := []string{"."}
			if claimErr := cdb.ClaimFiles(missionID, ws.GitRepoRoot, repoMarker); claimErr != nil && verbose {
				fmt.Printf("warning: could not register repo claim: %v\n", claimErr)
			}
			// Now check — our own claim is excluded by CheckConflicts (filters by missionID).
			// Warnings only — decomposer constraint injection deferred until
			// per-file claims are re-introduced (see Linear backlog).
			if conflicts, cErr := cdb.CheckConflicts(missionID, ws.GitRepoRoot, repoMarker); cErr == nil && len(conflicts) > 0 {
				for _, c := range conflicts {
					fmt.Printf("warning: repo %s already claimed by active mission %s\n", ws.GitRepoRoot, c.MissionID)
				}
			}
		} else if verbose {
			fmt.Printf("warning: could not open claims db: %v\n", err)
		}
	}

	// Persist linking metadata from mission frontmatter so audit can reconstruct
	// the workspace→mission→issue chain without re-parsing the task text.
	if ws != nil {
		if fm.LinearIssueID != "" {
			if err := os.WriteFile(filepath.Join(ws.Path, "linear_issue_id"), []byte(fm.LinearIssueID), 0600); err != nil && verbose {
				fmt.Printf("warning: could not write linear_issue_id: %v\n", err)
			}
		}
		if missionPath != "" {
			if err := os.WriteFile(filepath.Join(ws.Path, "mission_path"), []byte(missionPath), 0600); err != nil && verbose {
				fmt.Printf("warning: could not write mission_path: %v\n", err)
			}
		}
		warnMissingWorkspaceLinks(fm, missionPath)
	}

	// Strip orchestrator-level git workflow instructions before decomposition so
	// they never reach worker objectives. Workers must not create branches, push,
	// or open PRs — the orchestrator handles all git operations via --pr / teardown.
	task = stripGitWorkflowSection(task)

	// Sanitize task text: strip invisible Unicode characters that could carry
	// hidden prompt injection instructions from external sources (web pages,
	// emails, social posts). Log a warning and emit a security event when any
	// characters are stripped so the operator knows injection was attempted.
	if findings := sanitize.DetectInvisible(task); len(findings) > 0 {
		task = sanitize.SanitizeText(task)
		types := make([]string, 0, len(findings))
		seen := make(map[string]bool, len(findings))
		for _, f := range findings {
			if !seen[f.Description] {
				seen[f.Description] = true
				types = append(types, f.Description)
			}
		}
		fmt.Fprintf(os.Stderr, "warning: stripped %d invisible Unicode character(s) from task text (possible prompt injection): %s\n",
			len(findings), strings.Join(types, "; "))
		emitter.Emit(ctx, event.New(event.SecurityInvisibleCharsStripped, missionID, "", "", map[string]any{
			"count": len(findings),
			"types": types,
		}))
	}



	// Decompose task — skip entirely when loaded from template.
	// Decompose handles pre-decomposed PHASE lines internally and emits
	// decompose.started / decompose.completed events for the renderer.
	if plan == nil {
		plan, err = decompose.Decompose(ctx, task, learningsText, skillIndex, missionID, emitter, tc)
		if err != nil {
			return fmt.Errorf("decompose: %w", err)
		}
	}

	// Record routing decisions for each phase at decomposition time.
	// Best-effort: failures here never block the mission.
	if !dryRun && ws != nil {
		if recErr := recordRoutingDecisions(ws.ID, plan, tc); recErr != nil && verbose {
			fmt.Printf("warning: could not record routing decisions: %v\n", recErr)
		}
	}

	// Passive audit: best-effort observation of plan quality signals.
	// Persist synchronously so the process does not exit before the rows hit disk.
	if !dryRun && ws != nil && tc != nil {
		if err := launchPassiveAudit(ws.ID, tc, plan); err != nil && verbose {
			fmt.Printf("warning: could not persist passive audit findings: %v\n", err)
		}
	}

	// In dry-run mode, print the plan to stdout (no renderer is active).
	// For real runs the terminal renderer displays the plan via decompose events.
	if dryRun {
		printPlan(plan)
		if saveTemplate != "" {
			if err := core.SaveTemplate(saveTemplate, plan); err != nil {
				return fmt.Errorf("save template: %w", err)
			}
			fmt.Printf("template saved: %s\n", saveTemplate)
		}
		return nil
	}

	// Save initial checkpoint
	missionStart := time.Now()
	core.SaveCheckpointFull(ws.Path, plan, domain, "in_progress", missionStart, ws)

	// Save plan
	planData, _ := json.MarshalIndent(plan, "", "  ")
	os.WriteFile(ws.Path+"/plan.json", planData, 0600)

	// Validate and resolve --gate-mode flag.
	var resolvedGateMode core.GateMode
	switch gateMode {
	case "warn":
		resolvedGateMode = core.GateModeWarn
	case "block", "":
		resolvedGateMode = core.GateModeBlock
	default:
		return fmt.Errorf("--gate-mode must be \"warn\" or \"block\", got %q", gateMode)
	}

	// Parse --stall-timeout flag.
	var resolvedStallTimeout time.Duration
	if stallTimeout != "" {
		d, err := time.ParseDuration(stallTimeout)
		if err != nil || d <= 0 {
			return fmt.Errorf("--stall-timeout %q is not a valid duration (e.g. 10m, 30m)", stallTimeout)
		}
		resolvedStallTimeout = d
	}

	// Execute
	config := &core.OrchestratorConfig{
		MaxConcurrent:    3,
		Timeout:          15 * time.Minute,
		Verbose:          verbose,
		DryRun:           dryRun,
		ForcedModel:      model,
		ForceSequential:  sequential,
		Domain:           domain,
		MaxTurns:         maxTurns,
		DisableLearnings: noLearnings,
		GateMode:         resolvedGateMode,
		StallTimeout:     resolvedStallTimeout,
	}

	eng := engine.New(ws, config, embedder, db, skillIndex).WithEmitter(emitter)
	registerRuntimeExecutors(eng)
	result, err := eng.Execute(ctx, plan)
	result = normalizeExecutionResult(plan, result, err)
	success := missionSucceeded(result, err)

	// Save final checkpoint
	status := "completed"
	if !success {
		status = "failed"
	}
	core.SaveCheckpointFull(ws.Path, plan, domain, status, missionStart, ws)

	// Commit successful worktrees before any post-execution actions that still
	// need access to the checked-out branch. Failed runs preserve the worktree.
	worktreeReady := false
	if success {
		worktreeReady = commitGitIsolation(ws, task, emitter, missionID)
	} else {
		teardownGitIsolation(ws, false, task, emitter, missionID)
	}

	// Update file claims after commit/preserve but before a successful worktree
	// is removed. This lets preserved worktrees keep claims for staged,
	// unstaged, and untracked files.
	if claimsDB != nil {
		updatePerFileClaimsPostExecution(claimsDB, missionID, ws)
		if success {
			if rErr := claimsDB.ReleaseAll(missionID); rErr != nil && verbose {
				fmt.Printf("warning: could not release file claims: %v\n", rErr)
			}
		}
		releaseClaimsOnExit = false
	}
	// Create GitHub PR when requested and the mission succeeded.
	if createPR && success && ws != nil && ws.BranchName != "" {
		if worktreeReady {
			opts := prOptions{
				draft:       !noDraft,
				codexReview: codexReview || fm.CodexReview,
				reviewers:   fm.PRReviewers,
				labels:      fm.PRLabels,
			}
			createMissionPR(ws, plan, result, opts, emitter, missionID)
		} else {
			fmt.Println("PR creation skipped (worktree could not be committed)")
		}
	}

	if success && worktreeReady {
		removeGitWorktree(ws)
	}

	// Sync mission file frontmatter status back to runtime mission files only.
	// Best-effort: no-op for ad-hoc tasks, repo-local missions, or files without
	// frontmatter. Repo-local mission files still require explicit sync.
	if synced, syncErr := core.SyncManagedMissionStatus(ws.Path); syncErr != nil && verbose {
		fmt.Printf("warning: could not sync mission status: %v\n", syncErr)
	} else if synced != "" && verbose {
		fmt.Printf("synced status %q to %s\n", status, synced)
	}

	// Update routing decisions with per-phase outcomes.
	// Best-effort: failures here never affect the mission result.
	if ws != nil {
		if updErr := updateRoutingOutcomes(ws.ID, plan); updErr != nil && verbose {
			fmt.Printf("warning: could not update routing outcomes: %v\n", updErr)
		}
	}

	// Post-execution shape recording: best-effort, but synchronous so the
	// process does not exit before the learning row is persisted. Records the phase shape
	// (persona sequence, phase count, execution mode) with its outcome so future
	// decompositions for the same target can learn from successful structures.
	// Only meaningful when a target was resolved — anonymous tasks have no target
	// to accumulate patterns against.
	if ws != nil && tc != nil {
		outcome := "failure"
		if result.Success {
			outcome = "success"
		}
		runPostExecutionRecorders(ws.ID, tc.TargetID, plan, outcome, tc.TaskType)
	}

	// Post Linear comment with mission summary when issue ID is present.
	// Best-effort: a failure here never fails the mission.
	if fm.LinearIssueID != "" && !noComment && ws != nil {
		comment := buildLinearComment(ws, plan, missionStart, result.Success)
		if err := commentOnLinearIssue(fm.LinearIssueID, comment); err != nil {
			fmt.Printf("warning: could not post Linear comment: %v\n", err)
		} else {
			fmt.Printf("Linear comment posted to %s\n", fm.LinearIssueID)
		}
	}

	// Post-mission compliance scan: check whether injected learnings were
	// applied in the worker outputs and update compliance_rate in the DB.
	if db != nil {
		injected := eng.InjectedLearnings()
		if len(injected) > 0 {
			runComplianceScan(ctx, db, ws.Path, injected)
		}
	}

	// Save template on successful execution if requested.
	if saveTemplate != "" && result.Success {
		if saveErr := core.SaveTemplate(saveTemplate, plan); saveErr != nil {
			fmt.Printf("warning: could not save template: %v\n", saveErr)
		} else {
			fmt.Printf("template saved: %s\n", saveTemplate)
		}
	}

	// The terminal renderer prints the mission summary via events.
	// Only warn about dropped events separately.
	printEmitterDropWarning(emitter)

	if err != nil {
		return err
	}
	return nil
}

func resumeMission(ctx context.Context, wsPath string, db *learning.DB, embedder *learning.Embedder) error {
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		return fmt.Errorf("load checkpoint: %w", err)
	}

	fmt.Printf("resuming mission from %s\n", wsPath)

	// Reset failed/running phases to pending; revive their skipped descendants.
	resetPhasesForResume(cp.Plan.Phases)

	ws := &core.Workspace{
		ID:     cp.WorkspaceID,
		Path:   wsPath,
		Task:   cp.Plan.Task,
		Domain: cp.Domain,
		// Restore git isolation fields so SaveCheckpointFull re-serializes them.
		GitRepoRoot:  cp.GitRepoRoot,
		WorktreePath: cp.WorktreePath,
		BranchName:   cp.BranchName,
		BaseBranch:   cp.BaseBranch,
	}

	// Restore TargetDir so resumed phases run inside the target repo, not just
	// WorkerDir. targetID is declared in outer scope so recordPostExecutionShape
	// can use it after eng.Execute returns.
	var targetID string
	if data, err := os.ReadFile(filepath.Join(wsPath, "target_id")); err == nil {
		targetID = strings.TrimSpace(string(data))
		ws.TargetDir = core.ResolveTargetDir(targetID)
		if ws.TargetDir == "" && strings.HasPrefix(targetID, "repo:") && verbose {
			fmt.Printf("warning: target repository %q not found on disk, workers will run in WorkerDir\n", targetID)
		}
	} else if !errors.Is(err, os.ErrNotExist) && verbose {
		fmt.Printf("warning: could not read target_id: %v\n", err)
	}

	// Re-attach to the isolated worktree when the mission used git isolation.
	// If the worktree still exists, override TargetDir so resumed workers run
	// there. If it has been removed (e.g. after a successful commit on a prior
	// partial run), fall back to the regular TargetDir and clear the field so
	// teardown does not attempt another commit.
	if ws.WorktreePath != "" {
		if _, statErr := os.Stat(ws.WorktreePath); statErr == nil {
			ws.TargetDir = ws.WorktreePath
			fmt.Printf("git: re-attached to worktree %s (branch: %s)\n", ws.WorktreePath, ws.BranchName)
		} else {
			if verbose {
				fmt.Printf("warning: worktree %q no longer exists, running in target dir\n", ws.WorktreePath)
			}
			ws.WorktreePath = "" // disable teardown
		}
	}

	var resumeStallTimeout time.Duration
	if stallTimeout != "" {
		if d, err := time.ParseDuration(stallTimeout); err == nil && d > 0 {
			resumeStallTimeout = d
		}
	}
	config := &core.OrchestratorConfig{
		MaxConcurrent:   3,
		Timeout:         15 * time.Minute,
		Verbose:         verbose,
		ForcedModel:     model,
		ForceSequential: sequential,
		Domain:          cp.Domain,
		MaxTurns:        maxTurns,
		StallTimeout:    resumeStallTimeout,
	}

	// Re-attach event emitters for resumed missions (appends to existing log).
	resumeEmitter := buildEmitter(ws.ID, verbose)
	defer resumeEmitter.Close()

	eng := engine.New(ws, config, embedder, db, worker.LoadSkillIndex()).WithEmitter(resumeEmitter)
	registerRuntimeExecutors(eng)
	result, err := eng.Execute(ctx, cp.Plan)
	result = normalizeExecutionResult(cp.Plan, result, err)
	success := missionSucceeded(result, err)

	status := "completed"
	outcome := "success"
	if !success {
		status = "failed"
		outcome = "failure"
	}
	core.SaveCheckpointFull(wsPath, cp.Plan, cp.Domain, status, cp.StartedAt, ws)

	worktreeReady := false
	if success {
		worktreeReady = commitGitIsolation(ws, cp.Plan.Task, resumeEmitter, ws.ID)
	} else {
		teardownGitIsolation(ws, false, cp.Plan.Task, resumeEmitter, ws.ID)
	}

	// Update file claims after commit/preserve but before removing the worktree.
	// On success, release claims so parallel missions are unblocked.
	// On failure, retain them so the conflict warning remains visible.
	if claimsDB, cErr := claims.OpenDB(""); cErr == nil {
		defer claimsDB.Close()
		updatePerFileClaimsPostExecution(claimsDB, ws.ID, ws)
		if success {
			if rErr := claimsDB.ReleaseAll(ws.ID); rErr != nil && verbose {
				fmt.Printf("warning: could not release file claims: %v\n", rErr)
			}
		}
	} else if verbose {
		fmt.Printf("warning: could not open claims DB for post-execution update: %v\n", cErr)
	}

	if success && worktreeReady {
		removeGitWorktree(ws)
	}
	// Record phase shape for the resumed mission so the routing DB accumulates
	// an accurate success/failure signal. Mirrors the recording in runTask.
	// targetID is empty when no target_id file exists; recordPostExecutionShape
	// is a no-op in that case.
	// Use the persisted task type written at mission start; fall back to
	// re-classification for workspaces created before this sidecar was added.
	var resumeTaskType string
	if data, err := os.ReadFile(filepath.Join(wsPath, "task_type")); err == nil {
		resumeTaskType = strings.TrimSpace(string(data))
	} else {
		resumeTaskType = string(routing.ClassifyTaskType(cp.Plan.Task))
	}
	runPostExecutionRecorders(ws.ID, targetID, cp.Plan, outcome, resumeTaskType)

	// Post-mission compliance scan for resumed missions.
	if db != nil {
		if injected := eng.InjectedLearnings(); len(injected) > 0 {
			runComplianceScan(ctx, db, wsPath, injected)
		}
	}

	printResult(result)
	printEmitterDropWarning(resumeEmitter)
	return err
}

// resetPhasesForResume resets failed/running phases to pending and revives
// their transitive skipped descendants so the resumed engine re-executes them.
// Without the descendant revival, phases skipped because their upstream failed
// remain skipped forever — they are never dispatched again on resume.
func resetPhasesForResume(phases []*core.Phase) {
	// Build dependents index: phaseID → phases that directly depend on it.
	dependents := make(map[string][]*core.Phase)
	for _, p := range phases {
		for _, depID := range p.Dependencies {
			dependents[depID] = append(dependents[depID], p)
		}
	}

	// reviveSkipped recursively resets skipped descendants of id back to pending.
	var reviveSkipped func(id string)
	reviveSkipped = func(id string) {
		for _, dep := range dependents[id] {
			if dep.Status == core.StatusSkipped {
				dep.Status = core.StatusPending
				dep.Error = ""
				reviveSkipped(dep.ID)
			}
		}
	}

	for _, p := range phases {
		if p.Status == core.StatusRunning || p.Status == core.StatusFailed {
			p.Status = core.StatusPending
			p.Error = ""
			reviveSkipped(p.ID)
		}
	}
}

// buildEmitter constructs a fan-out emitter for a mission:
//   - FileEmitter writes the JSONL log to ~/.alluka/events/<id>.jsonl.
//   - UDSEmitter relays events to the daemon (silently skipped if not running).
//
// On resume, the sequence counter is primed from the last sequence in the
// existing log so emitted events continue the existing numbering.
// Returns NoOpEmitter if neither emitter can be set up.
func buildEmitter(missionID string, verbose bool) event.Emitter {
	var emitters []event.Emitter
	var lastSeq int64

	if logPath, err := event.EventLogPath(missionID); err == nil {
		// Read last sequence before opening for append so the counter
		// continues from where the previous session stopped.
		if seq, err := event.LastSequence(logPath); err == nil {
			lastSeq = seq
		} else if verbose {
			fmt.Printf("warning: could not read last event sequence: %v\n", err)
		}
		if fe, err := event.NewFileEmitter(logPath); err == nil {
			emitters = append(emitters, fe)
			if verbose {
				fmt.Printf("event log: %s\n", logPath)
			}
		} else if verbose {
			fmt.Printf("warning: could not open event log: %v\n", err)
		}
	}

	// UDSEmitter is always added; it silently drops events if the daemon is
	// not running — no error, no configuration required.
	if sockPath, err := event.DaemonSocketPath(); err == nil {
		emitters = append(emitters, event.NewUDSEmitter(sockPath))
	}

	// Terminal renderer: always present for interactive progress display.
	// In verbose mode it additionally prints the raw event stream.
	emitters = append(emitters, render.NewTerminalRenderer(verbose))

	// Always use MultiEmitter so the single authoritative counter is in one
	// place, and resume missions continue from the correct sequence.
	return event.NewMultiEmitterFromSeq(lastSeq, emitters...)
}

// setupGitIsolation creates an isolated branch and linked worktree for the
// mission. On return, ws.TargetDir points at the worktree and the git fields
// (GitRepoRoot, WorktreePath, BranchName, BaseBranch) are populated.
// Emits a git.worktree_created event on success.
func setupGitIsolation(ws *core.Workspace, task string, emitter event.Emitter, missionID string) error {
	repoRoot := git.FindRoot(ws.TargetDir)
	if repoRoot == "" {
		return fmt.Errorf("target directory %q is not inside a git repository", ws.TargetDir)
	}

	baseBranch, err := git.CurrentBranch(repoRoot)
	if err != nil {
		return fmt.Errorf("could not determine current branch in %q: %w", repoRoot, err)
	}

	branchName := git.BranchName(ws.ID, task)

	if err := git.CreateBranch(repoRoot, branchName, baseBranch); err != nil {
		return fmt.Errorf("create branch %q: %w", branchName, err)
	}

	base, err := config.Dir()
	if err != nil {
		return fmt.Errorf("get config dir: %w", err)
	}
	worktreePath := filepath.Join(base, "worktrees", ws.ID)

	if err := os.MkdirAll(filepath.Dir(worktreePath), 0700); err != nil {
		return fmt.Errorf("create worktrees dir: %w", err)
	}

	if err := git.CreateWorktree(repoRoot, worktreePath, branchName); err != nil {
		return fmt.Errorf("create worktree at %q: %w", worktreePath, err)
	}

	ws.GitRepoRoot = repoRoot
	ws.WorktreePath = worktreePath
	ws.BranchName = branchName
	ws.BaseBranch = baseBranch
	ws.TargetDir = worktreePath

	fmt.Printf("git: branch %q → worktree %s\n", branchName, worktreePath)

	emitter.Emit(context.Background(), event.New(event.GitWorktreeCreated, missionID, "", "", map[string]any{
		"branch":        branchName,
		"worktree_path": worktreePath,
		"base_branch":   baseBranch,
	}))

	return nil
}

// teardownGitIsolation handles post-execution worktree lifecycle:
//   - success: commits all changes then removes the worktree.
//   - failure: preserves the worktree so the user can inspect or resume.
//
// No-op when ws has no WorktreePath (git isolation was not used).
// Emits git.committed on a successful commit.
func teardownGitIsolation(ws *core.Workspace, success bool, task string, emitter event.Emitter, missionID string) {
	if ws == nil || ws.WorktreePath == "" {
		return
	}

	if success {
		if !commitGitIsolation(ws, task, emitter, missionID) {
			return
		}
		removeGitWorktree(ws)
	} else {
		fmt.Printf("git: worktree preserved at %s (branch: %s)\n", ws.WorktreePath, ws.BranchName)
		fmt.Printf("     resume: orchestrator run --resume %s\n", ws.Path)
	}
}

// prOptions holds the optional parameters for PR creation and post-creation
// actions. The zero value is safe to use (no codex review, no reviewers, no
// labels, draft=false).
type prOptions struct {
	draft       bool
	codexReview bool
	reviewers   []string
	labels      []string
}

const codexReviewPrompt = "Review the current branch for correctness, regressions, unsafe assumptions, and missing tests. Findings first with file references when possible."

// createMissionPR builds a PR body from mission metadata and invokes gh to
// open the pull request. Prints the PR URL on success. Best-effort: prints a
// warning and continues if gh is unavailable or the call fails.
func createMissionPR(ws *core.Workspace, plan *core.Plan, result *core.ExecutionResult, opts prOptions, emitter event.Emitter, missionID string) {
	if !git.HasGH() {
		fmt.Println("warning: --pr requested but gh CLI not found in PATH; skipping PR creation")
		return
	}

	// Collect unique personas from the plan.
	seen := make(map[string]bool)
	var personas []string
	for _, p := range plan.Phases {
		if p.Persona != "" && !seen[p.Persona] {
			seen[p.Persona] = true
			personas = append(personas, p.Persona)
		}
	}

	// Sum cost across all phases.
	var totalCost float64
	for _, p := range plan.Phases {
		totalCost += p.CostUSD
	}

	// Build summary from the first line of the task.
	summary := plan.Task
	if idx := strings.IndexByte(summary, '\n'); idx >= 0 {
		summary = summary[:idx]
	}

	// Collect changed files via git diff.
	files, _ := git.ChangedFiles(ws.GitRepoRoot, ws.BaseBranch, ws.BranchName)

	meta := git.PRMetadata{
		Summary:    summary,
		MissionID:  ws.ID,
		PhaseCount: len(plan.Phases),
		Mode:       plan.ExecutionMode,
		Personas:   personas,
		CostUSD:    totalCost,
		Duration:   result.Duration.Round(time.Second).String(),
		Files:      files,
	}

	body := git.BuildPRBody(meta)
	title := summary
	if len(title) > 72 {
		title = title[:69] + "..."
	}

	// Push branch to remote before creating PR — gh requires the head ref on the remote.
	pushPath := ws.WorktreePath
	if pushPath == "" {
		pushPath = ws.GitRepoRoot
	}
	if err := git.Push(pushPath, "origin", ws.BranchName); err != nil {
		fmt.Printf("warning: could not push branch %q: %v\n", ws.BranchName, err)
		fmt.Println("PR creation skipped (branch not pushed)")
		return
	}

	prURL, err := git.CreatePR(ws.GitRepoRoot, ws.BranchName, ws.BaseBranch, title, body, opts.draft)
	if err != nil {
		fmt.Printf("warning: could not create PR: %v\n", err)
		return
	}

	fmt.Printf("PR created: %s\n", prURL)

	// Persist PR URL to workspace sidecar so `orchestrator status` can display it.
	if err := os.WriteFile(filepath.Join(ws.Path, "pr_url"), []byte(prURL), 0600); err != nil && verbose {
		fmt.Printf("warning: could not write pr_url sidecar: %v\n", err)
	}

	emitter.Emit(context.Background(), event.New(event.GitPRCreated, missionID, "", "", map[string]any{
		"url":    prURL,
		"branch": ws.BranchName,
		"draft":  opts.draft,
	}))

	// Add reviewers if requested via frontmatter.
	if len(opts.reviewers) > 0 {
		if err := git.AddPRReviewers(ws.GitRepoRoot, prURL, opts.reviewers); err != nil {
			fmt.Printf("warning: could not add PR reviewers: %v\n", err)
		} else {
			fmt.Printf("PR reviewers requested: %s\n", strings.Join(opts.reviewers, ", "))
		}
	}

	// Add labels if requested via frontmatter.
	if len(opts.labels) > 0 {
		if err := git.AddPRLabels(ws.GitRepoRoot, prURL, opts.labels); err != nil {
			fmt.Printf("warning: could not add PR labels: %v\n", err)
		} else {
			fmt.Printf("PR labels added: %s\n", strings.Join(opts.labels, ", "))
		}
	}

	// Post internal review findings as a structured PR comment when the engine's
	// review loop produced blockers or warnings.
	if findingsComment := buildReviewFindingsComment(plan); findingsComment != "" {
		if err := git.CommentOnPR(ws.GitRepoRoot, prURL, findingsComment); err != nil {
			fmt.Printf("warning: could not post review findings comment: %v\n", err)
		} else {
			fmt.Println("Internal review findings posted as PR comment.")
		}
	}

	// Request Codex review when --codex-review flag or codex_review frontmatter is set.
	if opts.codexReview {
		requestCodexReview(ws, prURL, emitter, missionID)
	}
}

// buildReviewFindingsComment collects ReviewBlockers and ReviewWarnings from
// all review-gate phases and renders a structured Markdown comment. Returns ""
// when there are no findings (no comment needed).
func buildReviewFindingsComment(plan *core.Plan) string {
	var allBlockers, allWarnings []string
	for _, p := range plan.Phases {
		allBlockers = append(allBlockers, p.ReviewBlockers...)
		allWarnings = append(allWarnings, p.ReviewWarnings...)
	}
	if len(allBlockers) == 0 && len(allWarnings) == 0 {
		return ""
	}

	var b strings.Builder
	b.WriteString("## Internal Review Findings\n\n")
	b.WriteString("_Posted by the via orchestrator review loop._\n\n")

	if len(allBlockers) > 0 {
		b.WriteString("### Blockers (resolved)\n\n")
		for _, bl := range allBlockers {
			fmt.Fprintf(&b, "- %s\n", bl)
		}
		b.WriteString("\n")
	}
	if len(allWarnings) > 0 {
		b.WriteString("### Warnings\n\n")
		for _, w := range allWarnings {
			fmt.Fprintf(&b, "- %s\n", w)
		}
		b.WriteString("\n")
	}
	return b.String()
}

// commentOnLinearIssue posts body as a comment on the given Linear issue via
// the `linear` CLI. Returns an error if the CLI is not installed or the call
// fails; callers should warn and continue.
func commentOnLinearIssue(issueID, body string) error {
	cmd := exec.Command("linear", "issue", "comment", "add", issueID, "--body", body)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return fmt.Errorf("linear issue comment: %w (output: %s)", err, strings.TrimSpace(string(out)))
	}
	return nil
}

// buildLinearComment builds a concise Markdown summary comment for a completed
// mission. Sections: Summary, Phase Outcomes, Artifacts, Duration.
func buildLinearComment(ws *core.Workspace, plan *core.Plan, missionStart time.Time, success bool) string {
	var b strings.Builder

	// Summary
	status := "✅ Completed"
	if !success {
		status = "❌ Failed"
	}
	b.WriteString("## Mission Summary\n\n")
	fmt.Fprintf(&b, "**Status:** %s  \n", status)
	fmt.Fprintf(&b, "**Workspace:** `%s`\n\n", ws.ID)

	// PR URL (from sidecar)
	if raw, err := os.ReadFile(filepath.Join(ws.Path, "pr_url")); err == nil {
		if prURL := strings.TrimSpace(string(raw)); prURL != "" {
			fmt.Fprintf(&b, "**PR:** %s\n\n", prURL)
		}
	}

	// Phase Outcomes
	if plan != nil && len(plan.Phases) > 0 {
		b.WriteString("## Phase Outcomes\n\n")
		for _, p := range plan.Phases {
			icon := "⏭️"
			switch p.Status {
			case core.StatusCompleted:
				icon = "✅"
			case core.StatusFailed:
				icon = "❌"
			case core.StatusSkipped:
				icon = "⏭️"
			}
			dur := ""
			if p.StartTime != nil && p.EndTime != nil {
				dur = fmt.Sprintf(" (%s)", p.EndTime.Sub(*p.StartTime).Round(time.Second))
			}
			fmt.Fprintf(&b, "- %s **%s** `%s`%s\n", icon, p.Name, p.Persona, dur)
		}
		b.WriteString("\n")
	}

	// Artifacts
	artifactsDir := filepath.Join(ws.Path, "artifacts")
	var artifacts []string
	_ = fs.WalkDir(os.DirFS(artifactsDir), ".", func(path string, d fs.DirEntry, err error) error {
		if err != nil || d.IsDir() || path == "." {
			return nil
		}
		artifacts = append(artifacts, path)
		return nil
	})
	if len(artifacts) > 0 {
		b.WriteString("## Artifacts\n\n")
		for _, a := range artifacts {
			fullPath := filepath.Join(artifactsDir, a)
			info, statErr := os.Stat(fullPath)
			if statErr != nil {
				fmt.Fprintf(&b, "- `%s`\n", a)
				continue
			}
			const inlineLimit = 2 * 1024
			if info.Size() <= inlineLimit {
				content, readErr := os.ReadFile(fullPath)
				if readErr == nil {
					fmt.Fprintf(&b, "<details><summary><code>%s</code></summary>\n\n```\n%s\n```\n\n</details>\n", a, strings.TrimSpace(string(content)))
					continue
				}
			}
			fmt.Fprintf(&b, "- `%s` (%d bytes)\n", a, info.Size())
		}
		b.WriteString("\n")
	}

	// Review findings count
	if plan != nil {
		var blockerCount, warningCount int
		for _, p := range plan.Phases {
			blockerCount += len(p.ReviewBlockers)
			warningCount += len(p.ReviewWarnings)
		}
		if blockerCount > 0 || warningCount > 0 {
			b.WriteString("## Review Findings\n\n")
			if blockerCount > 0 {
				fmt.Fprintf(&b, "- %d blocker(s) (resolved)\n", blockerCount)
			}
			if warningCount > 0 {
				fmt.Fprintf(&b, "- %d warning(s)\n", warningCount)
			}
			b.WriteString("\n")
		}
	}

	// Duration
	elapsed := time.Since(missionStart).Round(time.Second)
	fmt.Fprintf(&b, "**Duration:** %s\n", elapsed)

	return b.String()
}

func printPlan(plan *core.Plan) {
	fmt.Printf("plan: %d phases (%s)\n", len(plan.Phases), plan.ExecutionMode)
	for _, p := range plan.Phases {
		deps := ""
		if len(p.Dependencies) > 0 {
			deps = fmt.Sprintf(" [depends: %s]", strings.Join(p.Dependencies, ", "))
		}
		skills := ""
		if len(p.Skills) > 0 {
			skills = fmt.Sprintf(" [skills: %s]", strings.Join(p.Skills, ", "))
		}
		fmt.Printf("  %s: %s (%s, %s)%s%s\n",
			p.ID, p.Name, p.Persona, p.ModelTier, deps, skills)
	}
	fmt.Println()
}

func printResult(result *core.ExecutionResult) {
	if result.Success {
		fmt.Printf("mission completed in %s\n", result.Duration.Round(time.Second))
	} else {
		fmt.Printf("mission failed in %s: %s\n", result.Duration.Round(time.Second), result.Error)
	}

	completed := 0
	failed := 0
	skipped := 0
	for _, p := range result.Plan.Phases {
		switch p.Status {
		case core.StatusCompleted:
			completed++
		case core.StatusFailed:
			failed++
		case core.StatusSkipped:
			skipped++
		}
	}

	fmt.Printf("  phases: %d completed, %d failed, %d skipped\n", completed, failed, skipped)

	if len(result.Artifacts) > 0 {
		fmt.Printf("  artifacts: %d files\n", len(result.Artifacts))
	}
}

func printEmitterDropWarning(emitter event.Emitter) {
	reporter, ok := emitter.(event.DropReporter)
	if !ok {
		return
	}
	stats := reporter.DropStats()
	if !stats.Any() {
		return
	}

	var parts []string
	if stats.FileDroppedWrites > 0 {
		parts = append(parts, fmt.Sprintf("file=%d", stats.FileDroppedWrites))
	}
	if stats.UDSDroppedWrites > 0 {
		parts = append(parts, fmt.Sprintf("uds=%d", stats.UDSDroppedWrites))
	}
	if stats.SubscriberDrops > 0 {
		parts = append(parts, fmt.Sprintf("subscribers=%d", stats.SubscriberDrops))
	}

	fmt.Fprintf(os.Stderr, "warning: event delivery drops observed (%s)\n", strings.Join(parts, ", "))
}

func warnMissingWorkspaceLinks(fm missionFrontmatter, missionPath string) {
	if fm.LinearIssueID == "" {
		fmt.Fprintln(os.Stderr, "warning: no linear_issue_id in mission frontmatter; workspace will not be linked to a Linear issue")
	}
	if missionPath == "" {
		fmt.Fprintln(os.Stderr, "warning: no mission file path; workspace is ad-hoc and mission sync/write-back will be unavailable")
	}
}

// runComplianceScan reads all worker output files from the workspace, then
// checks whether each injected learning's keywords appear in the combined
// output text. Results are written back to the DB via RecordCompliance.
// Runs best-effort: individual failures are logged under --verbose but do
// not propagate as errors to the caller.
func runComplianceScan(ctx context.Context, db *learning.DB, wsPath string, injected []learning.Learning) {
	workersDir := filepath.Join(wsPath, "workers")
	workers, err := os.ReadDir(workersDir)
	if err != nil {
		return
	}

	// Concatenate all worker outputs into a single blob for keyword matching.
	var sb strings.Builder
	for _, w := range workers {
		if !w.IsDir() {
			continue
		}
		data, err := os.ReadFile(filepath.Join(workersDir, w.Name(), "output.md"))
		if err != nil {
			continue
		}
		sb.Write(data)
		sb.WriteByte('\n')
	}
	combinedOutput := sb.String()
	if combinedOutput == "" {
		return
	}

	followed := learning.ComplianceScan(injected, combinedOutput)
	for id, wasFollowed := range followed {
		if err := db.RecordCompliance(ctx, id, wasFollowed); err != nil && verbose {
			fmt.Printf("warning: compliance record failed for %s: %v\n", id, err)
		}
	}
}

// capturePriorLearnings scans the most recent workspace's worker outputs for
// learnings and inserts them into the DB. This ensures learnings from a
// just-completed mission are available to FindRelevant at the start of the
// next mission — closing the same-day transfer gap.
func capturePriorLearnings(ctx context.Context, db *learning.DB, embedder *learning.Embedder, dom string) {
	workspaces, err := core.ListWorkspaces()
	if err != nil || len(workspaces) == 0 {
		return
	}

	// Only process the most recent workspace to keep startup fast.
	wsPath := workspaces[0]
	workersDir := filepath.Join(wsPath, "workers")
	wsID := filepath.Base(wsPath)

	workers, err := os.ReadDir(workersDir)
	if err != nil {
		return
	}

	for _, w := range workers {
		if !w.IsDir() {
			continue
		}
		outputPath := filepath.Join(workersDir, w.Name(), "output.md")
		data, err := os.ReadFile(outputPath)
		if err != nil {
			continue
		}
		captured := learning.CaptureFromText(string(data), w.Name(), dom, wsID)
		for _, l := range captured {
			// Insert handles dedup via cosine similarity, so re-inserting
			// already-captured learnings is a no-op.
			db.Insert(ctx, l, embedder)
		}
	}
}

// buildTargetContext resolves the target ID from cwd, mission file path, or
// task text keywords, then opens the routing DB to fetch the target profile
// and routing patterns. Returns nil when no target can be determined or when
// routing data is unavailable; callers must treat nil as a no-op.
func buildTargetContext(ctx context.Context, missionPath, taskText string) *decompose.TargetContext {
	cwd, _ := os.Getwd()
	targetID := resolveTarget(cwd, missionPath, taskText)
	if targetID == "" {
		return nil
	}

	rdb, err := routing.OpenDB("")
	if err != nil {
		if verbose {
			fmt.Printf("[routing] could not open routing DB: %v\n", err)
		}
		return nil
	}
	defer rdb.Close()

	// Classify the task type up-front so it can be stored when recording the
	// post-execution shape and used for cross-target fallback lookup below.
	taskType := routing.ClassifyTaskType(taskText)
	tc := &decompose.TargetContext{TargetID: targetID, TaskType: string(taskType)}

	// Fetch profile, patterns, corrections, examples, and repeated findings
	// concurrently — all independent reads.
	const minExampleScore = 3        // both audit_score and decomp_quality must be >= 3
	const minFindingScore = 3        // only findings from missions scoring >= 3
	const minObservations = 2        // same finding must appear in >= 2 workspaces
	const minPassiveObservations = 3 // passive findings require a higher bar (no score-based damping)

	var (
		profile         *routing.TargetProfile
		patterns        []routing.RoutingPattern
		rolePatterns    []routing.RolePersonaPattern
		corrections     []routing.RoutingCorrection
		examples        []routing.DecompExample
		repeated        []routing.DecompFinding
		passiveRepeated []routing.DecompFinding
		handoffs        []routing.HandoffPattern
		planShape       *routing.PlanShapeStats
		successShapes   []routing.PhaseShapePattern
		wg              sync.WaitGroup
	)
	wg.Add(10)
	go func() {
		defer wg.Done()
		profile, _ = rdb.GetTargetProfile(ctx, targetID)
	}()
	go func() {
		defer wg.Done()
		patterns, _ = rdb.GetRoutingPatterns(ctx, targetID)
	}()
	go func() {
		defer wg.Done()
		rolePatterns, _ = rdb.GetRolePersonaPatterns(ctx, targetID, 2)
	}()
	go func() {
		defer wg.Done()
		corrections, _ = rdb.GetRoutingCorrections(ctx, targetID)
	}()
	go func() {
		defer wg.Done()
		examples, _ = rdb.GetDecompExamples(ctx, targetID, minExampleScore, 0)
	}()
	go func() {
		defer wg.Done()
		repeated, _ = rdb.GetRepeatedFindings(ctx, targetID, minFindingScore, minObservations)
	}()
	go func() {
		defer wg.Done()
		passiveRepeated, _ = rdb.GetRepeatedPassiveFindings(ctx, targetID, minPassiveObservations)
	}()
	go func() {
		defer wg.Done()
		handoffs, _ = rdb.GetHandoffPatterns(ctx, targetID)
	}()
	go func() {
		defer wg.Done()
		planShape, _ = rdb.GetPlanShapeStats(ctx, targetID, minExampleScore)
	}()
	go func() {
		defer wg.Done()
		successShapes, _ = rdb.GetSuccessfulShapePatterns(ctx, targetID, routing.MinShapeSuccesses)
	}()
	wg.Wait()

	// Build the effective profile: start from the DB profile (or empty) then fill
	// any zero-value fields via auto-detection from the target repo on disk.
	// This lets seeded/manual profiles win while still giving the decomposer
	// useful hints for targets that have never been explicitly configured.
	var effective routing.TargetProfile
	if profile != nil {
		effective = *profile
	}
	if repoPath := targetIDToPath(targetID); repoPath != "" {
		detected := routing.DetectProfile(repoPath)
		effective = routing.MergeDetected(effective, detected)
	}

	if profile != nil || effective.Language != "" {
		tc.Language = effective.Language
		tc.Runtime = effective.Runtime
		tc.TestCommand = effective.TestCommand
		tc.BuildCommand = effective.BuildCommand
		tc.Framework = effective.Framework
		tc.KeyDirectories = effective.KeyDirectories
		tc.PreferredPersonas = effective.PreferredPersonas
		tc.Notes = effective.Notes
	}
	for _, p := range patterns {
		tc.TopPatterns = append(tc.TopPatterns, decompose.RoutingHint{
			Persona:    p.Persona,
			TaskHint:   p.TaskHint,
			Confidence: p.Confidence,
		})
	}
	for _, p := range rolePatterns {
		tc.RolePersonaHints = append(tc.RolePersonaHints, decompose.RolePersonaHint{
			Role:        p.Role,
			Persona:     p.Persona,
			SeenCount:   p.SeenCount,
			SuccessRate: p.SuccessRate,
		})
	}
	for _, c := range corrections {
		tc.RoutingCorrections = append(tc.RoutingCorrections, decompose.CorrectionHint{
			AssignedPersona: c.AssignedPersona,
			IdealPersona:    c.IdealPersona,
			TaskHint:        c.TaskHint,
			Source:          c.Source,
		})
	}
	for _, ex := range examples {
		tc.DecompExamples = append(tc.DecompExamples, decompose.DecompExampleHint{
			TaskSummary:   ex.TaskSummary,
			PhaseCount:    ex.PhaseCount,
			ExecutionMode: ex.ExecutionMode,
			PhasesJSON:    ex.PhasesJSON,
			DecompSource:  ex.DecompSource,
			AuditScore:    ex.AuditScore,
		})
	}
	for _, h := range handoffs {
		tc.HandoffHints = append(tc.HandoffHints, decompose.HandoffHint{
			FromPersona: h.FromPersona,
			ToPersona:   h.ToPersona,
			TaskHint:    h.TaskHint,
			Confidence:  h.Confidence,
		})
	}
	if planShape != nil {
		tc.PlanShapeStats = &decompose.PlanShapeStats{
			AvgPhaseCount:  planShape.AvgPhaseCount,
			MostCommonMode: planShape.MostCommonMode,
			TopPersonas:    planShape.TopPersonas,
			ExampleCount:   planShape.ExampleCount,
		}
	}
	for _, s := range successShapes {
		tc.SuccessfulShapes = append(tc.SuccessfulShapes, decompose.PhaseShapeHint{
			PhaseCount:    s.PhaseCount,
			ExecutionMode: s.ExecutionMode,
			PersonaSeq:    s.PersonaSeq,
			SuccessCount:  s.SuccessCount,
		})
	}

	tc.DecompInsights = mergeDecompInsights(repeated, passiveRepeated)

	// Cross-target shape fallback: when this target has no proven shapes yet
	// (cold start), look up shapes that succeeded for OTHER targets with the
	// same task type. This is a weaker signal — it tells the decomposer what
	// has worked elsewhere for similar tasks, not what has worked here.
	// The lookup is synchronous and best-effort; errors are intentionally silenced.
	if len(tc.SuccessfulShapes) == 0 && taskType != routing.TaskTypeUnknown {
		crossShapes, _ := rdb.GetTaskTypeSuccessfulShapes(ctx, string(taskType), routing.MinShapeSuccesses)
		for _, s := range crossShapes {
			tc.CrossTargetShapes = append(tc.CrossTargetShapes, decompose.PhaseShapeHint{
				PhaseCount:    s.PhaseCount,
				ExecutionMode: s.ExecutionMode,
				PersonaSeq:    s.PersonaSeq,
				SuccessCount:  s.SuccessCount,
			})
		}
	}

	// Feedback injection: check whether any preferred personas have recent failures
	// on this target and inject advisory warnings into the decomposer context.
	// We query the top preferred personas (from profile or patterns) to surface
	// failure signals before persona selection happens. Best-effort: errors silenced.
	{
		candidatePersonas := make(map[string]bool)
		for _, p := range tc.PreferredPersonas {
			candidatePersonas[p] = true
		}
		for _, h := range tc.TopPatterns {
			candidatePersonas[h.Persona] = true
		}
		for persona := range candidatePersonas {
			failures, ferr := rdb.GetRecentPersonaFailures(ctx, persona, 14, 3)
			if ferr != nil || len(failures) == 0 {
				continue
			}
			for _, f := range failures {
				age := int(time.Since(f.CreatedAt).Hours() / 24)
				ageStr := fmt.Sprintf("%d day(s) ago", age)
				if age == 0 {
					ageStr = "today"
				}
				reason := f.FailureReason
				if reason == "" {
					reason = "unknown reason"
				}
				phaseName := f.PhaseName
				if phaseName == "" {
					phaseName = f.PhaseID
				}
				tc.RoutingFailureWarnings = append(tc.RoutingFailureWarnings,
					fmt.Sprintf("%s failed phase %q %s: %s", persona, phaseName, ageStr, reason))
			}
		}
	}

	// No useful data: skip passing context to avoid empty-struct noise.
	if profile == nil && len(tc.TopPatterns) == 0 && len(tc.RolePersonaHints) == 0 && len(tc.RoutingCorrections) == 0 &&
		len(tc.DecompExamples) == 0 && len(tc.DecompInsights) == 0 &&
		len(tc.HandoffHints) == 0 && tc.PlanShapeStats == nil &&
		len(tc.SuccessfulShapes) == 0 && len(tc.CrossTargetShapes) == 0 &&
		taskType == routing.TaskTypeUnknown {
		return nil
	}

	return tc
}

// resolveTarget infers a canonical target identifier from four sources
// (highest priority first, with one Nanika-specific exception):
//  0. explicit mission frontmatter target — e.g. target: repo:/abs/path
//  1. cwd git root — walk up from cwd looking for .git
//  2. mission path git root — walk up from the mission file's directory
//  3. task text keywords — scan for known Nanika system names
//
// Returns "" when no target can be determined. The identifier is in
// "repo:~/<path>" format with the home directory replaced by ~.
//
// Exception: when the ambient repo is the Nanika control repo itself (`repo:~/nanika`)
// and task text resolves to a more specific target, prefer the task target.
// This prevents the control repo from swallowing explicit targets like
// `repo:~/skills/orchestrator` or `publication:substack` just because the
// command was launched from `~/nanika`.
func resolveTarget(cwd, missionPath, taskText string) string {
	if explicit := extractMissionTarget(taskText); explicit != "" {
		return explicit
	}

	taskTarget := resolveFromTaskText(taskText)

	// Source 1: cwd git root (highest priority).
	if cwd != "" {
		if root := git.FindRoot(cwd); root != "" {
			repoID := canonicalRepoID(root)
			if shouldPreferTaskTarget(repoID, taskTarget) {
				return taskTarget
			}
			return repoID
		}
	}

	// Source 2: mission file path git root.
	if missionPath != "" {
		if root := git.FindRoot(filepath.Dir(missionPath)); root != "" {
			repoID := canonicalRepoID(root)
			if shouldPreferTaskTarget(repoID, taskTarget) {
				return taskTarget
			}
			return repoID
		}
	}

	// Source 3: task text keyword scan.
	return taskTarget
}

// missionFrontmatter holds the structured metadata parsed from a mission's
// YAML frontmatter block. All fields are optional; unset fields are empty strings.
type missionFrontmatter struct {
	Target        string   // canonical target ID (e.g. "repo:~/skills/orchestrator")
	LinearIssueID string   // Linear issue identifier (e.g. "V-5")
	Status        string   // mission status hint ("active", "draft", etc.)
	Type          string   // mission type: "research", "evaluation", "review", "development", etc.
	Domain        string   // mission domain: "dev", "personal", "work", "creative", "academic", etc.
	CodexReview   bool     // codex_review: true → request @codex review on PR
	PRReviewers   []string // pr_reviewers: list of GitHub handles to request as reviewers
	PRLabels      []string // pr_labels: list of GitHub labels to add to the PR
}

// parseMissionFrontmatter reads the YAML frontmatter block at the top of a
// mission file body and returns the recognised fields. Unknown fields are
// silently ignored. Returns a zero-value struct when no valid frontmatter is
// present.
func parseMissionFrontmatter(taskText string) missionFrontmatter {
	var fm missionFrontmatter
	if !strings.HasPrefix(taskText, "---\n") {
		return fm
	}

	lines := strings.Split(taskText, "\n")
	if len(lines) == 0 || lines[0] != "---" {
		return fm
	}

	closing := -1
	for i := 1; i < len(lines); i++ {
		if strings.TrimSpace(lines[i]) == "---" {
			closing = i
			break
		}
	}
	if closing == -1 {
		return fm
	}

	// currentListKey tracks which list field we are collecting sequence items for.
	// Set when a pr_reviewers: or pr_labels: key has no inline value.
	var currentListKey string
	for i := 1; i < closing; i++ {
		line := strings.TrimSpace(lines[i])

		// YAML sequence item ("- value") belonging to the current list key.
		if currentListKey != "" && strings.HasPrefix(line, "- ") {
			item := strings.TrimSpace(strings.TrimPrefix(line, "- "))
			switch currentListKey {
			case "pr_reviewers":
				fm.PRReviewers = append(fm.PRReviewers, item)
			case "pr_labels":
				fm.PRLabels = append(fm.PRLabels, item)
			}
			continue
		}
		// Any non-list-item line resets the sequence collector.
		currentListKey = ""

		switch {
		case strings.HasPrefix(line, "target:"):
			raw := unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "target:")))
			fm.Target = canonicalExplicitTarget(raw)
		case strings.HasPrefix(line, "linear_issue_id:"):
			fm.LinearIssueID = unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "linear_issue_id:")))
		case strings.HasPrefix(line, "status:"):
			fm.Status = unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "status:")))
		case strings.HasPrefix(line, "type:"):
			fm.Type = unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "type:")))
		case strings.HasPrefix(line, "domain:"):
			fm.Domain = unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "domain:")))
		case strings.HasPrefix(line, "codex_review:"):
			val := unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "codex_review:")))
			fm.CodexReview = strings.EqualFold(val, "true")
		case strings.HasPrefix(line, "pr_reviewers:"):
			raw := unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "pr_reviewers:")))
			if raw != "" {
				fm.PRReviewers = parseFrontmatterList(raw)
			} else {
				currentListKey = "pr_reviewers"
			}
		case strings.HasPrefix(line, "pr_labels:"):
			raw := unquoteFrontmatterValue(strings.TrimSpace(strings.TrimPrefix(line, "pr_labels:")))
			if raw != "" {
				fm.PRLabels = parseFrontmatterList(raw)
			} else {
				currentListKey = "pr_labels"
			}
		}
	}
	return fm
}

// parseFrontmatterList splits a comma-separated value string into trimmed,
// non-empty entries. Used for pr_reviewers and pr_labels fields.
func parseFrontmatterList(raw string) []string {
	if raw == "" {
		return nil
	}
	parts := strings.Split(raw, ",")
	var out []string
	for _, p := range parts {
		if s := strings.TrimSpace(p); s != "" {
			out = append(out, s)
		}
	}
	return out
}

func unquoteFrontmatterValue(v string) string {
	v = strings.TrimSpace(v)
	if len(v) >= 2 {
		if (strings.HasPrefix(v, "\"") && strings.HasSuffix(v, "\"")) ||
			(strings.HasPrefix(v, "'") && strings.HasSuffix(v, "'")) {
			return strings.TrimSpace(v[1 : len(v)-1])
		}
	}
	return v
}

// missionSkipsGit returns true when the mission's frontmatter indicates it is
// not a code-modification task and git isolation should be skipped by default.
// Callers should only honour this when --git-isolate was NOT explicitly passed
// by the user; an explicit flag always overrides frontmatter-based detection.
//
// Non-code signals:
//   - type: research | evaluation | review  → read-only or analytical work
//   - domain: anything other than dev/development/engineering/code/coding
func missionSkipsGit(fm missionFrontmatter) bool {
	switch strings.ToLower(fm.Type) {
	case "research", "evaluation", "review":
		return true
	}
	if fm.Domain != "" {
		switch strings.ToLower(fm.Domain) {
		case "dev", "development", "engineering", "code", "coding":
			// Explicitly a dev domain — don't skip.
		default:
			return true
		}
	}
	return false
}

// extractMissionTarget parses a YAML frontmatter `target:` field from the top of
// a mission file body. It returns a canonical target ID or "" when no explicit
// target is present.
func extractMissionTarget(taskText string) string {
	return parseMissionFrontmatter(taskText).Target
}

func canonicalExplicitTarget(target string) string {
	target = strings.TrimSpace(target)
	if target == "" {
		return ""
	}
	if strings.HasPrefix(target, "repo:~/") || strings.HasPrefix(target, "system:") || strings.HasPrefix(target, "publication:") {
		return target
	}
	if strings.HasPrefix(target, "repo:/") {
		return canonicalRepoID(strings.TrimPrefix(target, "repo:"))
	}
	if strings.HasPrefix(target, "/") {
		return canonicalRepoID(target)
	}
	return target
}

func shouldPreferTaskTarget(repoID, taskTarget string) bool {
	if repoID != "repo:~/nanika" {
		return false
	}
	return taskTarget != ""
}

// targetIDToPath converts a canonical "repo:~/..." target ID back to an
// absolute filesystem path so the auto-detection scanner can read the repo root.
// Returns "" for non-repo targets (system:, publication:) or if resolution fails.
func targetIDToPath(targetID string) string {
	if !strings.HasPrefix(targetID, "repo:") {
		return ""
	}
	path := strings.TrimPrefix(targetID, "repo:")
	if strings.HasPrefix(path, "~/") {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}
		path = filepath.Join(home, path[2:])
	}
	return path
}

// canonicalRepoID converts a git root path to "repo:~/<path>" format,
// replacing the home directory prefix with ~ for a stable cross-machine identifier.
func canonicalRepoID(root string) string {
	home, err := os.UserHomeDir()
	if err != nil {
		return "repo:" + root
	}
	if strings.HasPrefix(root, home) {
		root = "~" + root[len(home):]
	}
	return "repo:" + root
}

// knownSystems lists Nanika system names and their canonical target IDs in
// priority order. The first match wins, so order matters when a task mentions
// multiple systems. All Nanika systems live under ~/nanika/skills/.
var knownSystems = [][2]string{
	{"orchestrator", "repo:~/nanika/skills/orchestrator"},
	{"scout", "repo:~/nanika/skills/scout"},
	{"obsidian", "repo:~/nanika/skills/obsidian"},
	{"gmail", "repo:~/nanika/skills/gmail"},
	{"todoist", "repo:~/nanika/skills/todoist"},
	{"ynab", "repo:~/nanika/skills/ynab"},
	{"linkedin", "repo:~/nanika/skills/linkedin"},
	{"reddit", "repo:~/nanika/skills/reddit"},
	{"substack", "repo:~/nanika/skills/substack"},
	{"scheduler", "repo:~/nanika/skills/scheduler"},
	{"publish", "repo:~/nanika/skills/publish"},
	{"engage", "repo:~/nanika/skills/engage"},
	{"elevenlabs", "repo:~/nanika/skills/elevenlabs"},
	{"contentkit", "repo:~/nanika/skills/contentkit"},
}

type knownSystemPattern struct {
	re     *regexp.Regexp
	target string
}

// buildPatterns compiles word-boundary regexps for a slice of (keyword, target) pairs.
// Using \b prevents substring false positives: "engaged" does not match "engage",
// "published" does not match "publish", etc.
func buildPatterns(pairs [][2]string) []knownSystemPattern {
	patterns := make([]knownSystemPattern, len(pairs))
	for i, pair := range pairs {
		patterns[i].re = regexp.MustCompile(`(?i)\b` + regexp.QuoteMeta(pair[0]) + `\b`)
		patterns[i].target = pair[1]
	}
	return patterns
}

// knownSystemPatterns holds precompiled patterns for known Nanika CLI repo targets.
var knownSystemPatterns = buildPatterns(knownSystems)

// knownNonRepoTargets maps task-text signals to non-repo canonical target IDs.
// These are checked only after knownSystemPatterns finds no match, so repo targets
// always win when a CLI tool name is present.
//
// "orchestration" (not "orchestrator") catches design/strategy meta-tasks about
// the Nanika system as a whole — e.g. "improve orchestration reliability" — without
// conflicting with "fix the orchestrator CLI" which matches the repo target.
//
// "newsletter" catches Substack publication tasks — e.g. "write my weekly newsletter"
// — that do not mention the "substack" CLI tool keyword directly.
var knownNonRepoTargets = buildPatterns([][2]string{
	// Nanika orchestration strategy — distinct from the "orchestrator" CLI repo.
	{"orchestration", "system:via"},
	// Newsletter publication tasks — distinct from the "substack" CLI repo.
	{"newsletter", "publication:substack"},
})

// resolveFromTaskText scans task for known Nanika system names and returns the
// canonical target ID of the first match. Returns "" if no known system is found.
// Matching uses word-boundary regexps (case-insensitive) so substrings of known
// names — "published", "engaged", "scouting" — do not trigger a false match.
// Iteration order is deterministic; the first match in knownSystems wins.
//
// Resolution is two-pass: repo targets (knownSystemPatterns) are tried first.
// Non-repo targets (knownNonRepoTargets) are only tried when no CLI repo matched,
// ensuring repo routing memory is never displaced by a broader target class.
func resolveFromTaskText(task string) string {
	if task == "" {
		return ""
	}
	// Pass 1: known Nanika CLI repo targets — highest priority.
	for _, p := range knownSystemPatterns {
		if p.re.MatchString(task) {
			return p.target
		}
	}
	// Pass 2: non-repo Nanika targets — only when no CLI repo was named.
	for _, p := range knownNonRepoTargets {
		if p.re.MatchString(task) {
			return p.target
		}
	}
	return ""
}

// mergeDecompInsights converts repeated audited findings plus repeated passive
// findings into prompt-ready decomposition insights.
// Audited findings always take precedence: when the same (finding_type, detail)
// key appears in both sets, only the audited workspaces count toward the
// observation total. Passive findings only fill gaps where no audited signal exists.
func mergeDecompInsights(repeated, passiveRepeated []routing.DecompFinding) []decompose.DecompInsight {
	insightKey := func(f routing.DecompFinding) string { return f.FindingType + "|" + f.Detail }
	insightWorkspaces := make(map[string]map[string]bool) // key -> set of workspace IDs
	insightFirst := make(map[string]routing.DecompFinding)
	auditedKeys := make(map[string]bool)

	for _, f := range repeated {
		k := insightKey(f)
		if insightWorkspaces[k] == nil {
			insightWorkspaces[k] = make(map[string]bool)
			insightFirst[k] = f
		}
		insightWorkspaces[k][f.WorkspaceID] = true
		auditedKeys[k] = true
	}
	for _, f := range passiveRepeated {
		k := insightKey(f)
		if auditedKeys[k] {
			continue
		}
		if insightWorkspaces[k] == nil {
			insightWorkspaces[k] = make(map[string]bool)
			insightFirst[k] = f
		}
		insightWorkspaces[k][f.WorkspaceID] = true
	}

	insights := make([]decompose.DecompInsight, 0, len(insightWorkspaces))
	for k, wsSet := range insightWorkspaces {
		f := insightFirst[k]
		insights = append(insights, decompose.DecompInsight{
			FindingType: f.FindingType,
			Detail:      f.Detail,
			Count:       len(wsSet),
		})
	}
	return insights
}

// launchPassiveAudit runs AuditPlan and persists any findings as passive
// (audit_score=0) rows in decomposition_findings.
// Returns an error if persistence fails; callers may warn and continue.
func launchPassiveAudit(wsID string, tc *decompose.TargetContext, plan *core.Plan) error {
	findings := decompose.AuditPlan(plan, tc)
	if len(findings) == 0 {
		return nil
	}

	rdb, err := routing.OpenDB("")
	if err != nil {
		return err
	}
	defer rdb.Close()

	rows := make([]routing.DecompFindingRow, 0, len(findings))
	for _, f := range findings {
		rows = append(rows, routing.NewPassiveFindingRow(tc.TargetID, wsID, f.FindingType, f.PhaseName, f.Detail))
	}
	_, err = rdb.InsertDecompFindings(context.Background(), rows)
	return err
}

// updatePerFileClaimsPostExecution replaces repo-root claim with per-file claims
// based on actual modifications detected via git diff. This is called after
// phase execution completes to record what was actually modified instead of
// claiming the entire repo.
//
// Only called when ws is non-nil and has git isolation fields populated.
// Best-effort: failures are logged when verbose but do not block mission completion.
func updatePerFileClaimsPostExecution(cdb *claims.DB, missionID string, ws *core.Workspace) {
	if cdb == nil || missionID == "" || ws == nil {
		return
	}
	if ws.GitRepoRoot == "" || ws.BranchName == "" || ws.BaseBranch == "" {
		return
	}

	// Capture both committed branch diff files and any staged, unstaged, or
	// untracked files still present in a preserved worktree.
	changedFiles, err := git.ClaimChangedFiles(ws.GitRepoRoot, ws.WorktreePath, ws.BaseBranch, ws.BranchName)
	if err != nil {
		if verbose {
			fmt.Printf("warning: could not determine changed files for claims update: %v\n", err)
		}
		return
	}

	// Update the claims: replace repo-root marker "." with per-file claims.
	if err := cdb.UpdateFileClaimsWithFiles(missionID, ws.GitRepoRoot, changedFiles); err != nil {
		if verbose {
			fmt.Printf("warning: could not update per-file claims: %v\n", err)
		}
	}
}
// runPostExecutionRecorders opens the routing DB once and runs all four
// post-execution recorders (shape, roles, routing patterns, handoff patterns)
// with best-effort failure isolation: a failure in one recorder is logged when
// verbose is set but does not prevent the remaining recorders from running.
func runPostExecutionRecorders(wsID, targetID string, plan *core.Plan, outcome, taskType string) {
	if targetID == "" || wsID == "" || plan == nil || len(plan.Phases) == 0 {
		return
	}
	rdb, err := routing.OpenDB("")
	if err != nil {
		if verbose {
			fmt.Printf("warning: could not open routing DB for post-execution recording: %v\n", err)
		}
		return
	}
	defer rdb.Close()

	if recErr := recordPostExecutionShape(rdb, wsID, targetID, plan, outcome, taskType); recErr != nil && verbose {
		fmt.Printf("warning: could not record phase shape: %v\n", recErr)
	}
	if recErr := recordPostExecutionRoles(rdb, wsID, targetID, plan); recErr != nil && verbose {
		fmt.Printf("warning: could not record role signals: %v\n", recErr)
	}
	if recErr := recordPostExecutionRoutingPatterns(rdb, wsID, targetID, plan, taskType); recErr != nil && verbose {
		fmt.Printf("warning: could not record routing patterns: %v\n", recErr)
	}
	if recErr := recordPostExecutionHandoffPatterns(rdb, wsID, targetID, plan); recErr != nil && verbose {
		fmt.Printf("warning: could not record handoff patterns: %v\n", recErr)
	}
}

// recordPostExecutionShape persists the phase shape of a completed mission in
// phase_shape_patterns.
//
// outcome is "success" or "failure". taskType is the classified task type
// (see routing.ClassifyTaskType); pass an empty string when unknown.
// The persona_seq column stores the ordered list of phase personas joined by
// commas so SQL GROUP BY can bucket identical sequences and count how often
// each one succeeded for a target or task type.
func recordPostExecutionShape(rdb *routing.RoutingDB, wsID, targetID string, plan *core.Plan, outcome, taskType string) error {
	if targetID == "" || wsID == "" || plan == nil || len(plan.Phases) == 0 {
		return nil
	}

	personas := make([]string, len(plan.Phases))
	for i, p := range plan.Phases {
		personas[i] = p.Persona
	}
	personaSeq := strings.Join(personas, ",")

	return rdb.RecordPhaseShape(context.Background(), targetID, wsID, len(plan.Phases), plan.ExecutionMode, personaSeq, outcome, taskType)
}

// recordPostExecutionRoles persists role assignments and cross-role handoffs
// from a finished mission. This closes the learning loop for the role-aware
// routing surfaces: future decompositions can bias planner/reviewer persona
// selection from observed successful role assignments on the same target.
func recordPostExecutionRoles(rdb *routing.RoutingDB, wsID, targetID string, plan *core.Plan) error {
	if targetID == "" || wsID == "" || plan == nil || len(plan.Phases) == 0 {
		return nil
	}

	ctx := context.Background()
	for _, p := range plan.Phases {
		if p == nil || p.Role == "" || p.Persona == "" {
			continue
		}
		outcome := "failure"
		if p.Status == core.StatusCompleted {
			outcome = "success"
		}
		if err := rdb.RecordRoleAssignment(ctx, routing.RoleAssignment{
			TargetID:    targetID,
			WorkspaceID: wsID,
			PhaseID:     p.ID,
			Persona:     p.Persona,
			Role:        string(p.Role),
			Outcome:     outcome,
		}); err != nil {
			return err
		}
	}

	index := make(map[string]*core.Phase, len(plan.Phases))
	for _, p := range plan.Phases {
		if p != nil {
			index[p.ID] = p
		}
	}
	for _, p := range plan.Phases {
		if p == nil || p.Role == "" {
			continue
		}
		for _, depID := range p.Dependencies {
			dep := index[depID]
			if dep == nil || dep.Role == "" || dep.Role == p.Role || dep.Persona == "" || p.Persona == "" {
				continue
			}
			if err := rdb.RecordHandoff(ctx, targetID, wsID, dep.ID, p.ID, string(dep.Role), string(p.Role), dep.Persona, p.Persona, summarizeForRouting(dep.Output, 300)); err != nil {
				return err
			}
		}
	}
	return nil
}

// recordPostExecutionRoutingPatterns persists one routing-pattern observation
// per completed phase into routing_patterns. This closes the live-execution
// learning loop: persona selection signals accumulate from real mission runs
// rather than only from post-hoc audit corrections.
//
// Only completed phases are recorded; failed/skipped phases are not persisted
// so that executor failures do not incorrectly reinforce a poor persona choice.
// The function is a no-op when targetID or wsID is empty, or when plan has no
// phases, so callers in runTask and resumeMission may call it unconditionally.
func recordPostExecutionRoutingPatterns(rdb *routing.RoutingDB, wsID, targetID string, plan *core.Plan, taskType string) error {
	if targetID == "" || wsID == "" || plan == nil || len(plan.Phases) == 0 {
		return nil
	}
	return rdb.RecordPlanRoutingPatterns(context.Background(), targetID, taskType, plan)
}

// recordPostExecutionHandoffPatterns persists handoff-pattern observations
// (persona-to-persona transitions) from a completed plan into handoff_patterns.
// This closes the live-execution learning loop for handoff sequences: transition
// confidence accumulates from real mission runs rather than only from post-hoc
// audit ingestion.
//
// The function is a no-op when targetID or wsID is empty, or when plan has no
// phases, so callers in runTask and resumeMission may call it unconditionally.
func recordPostExecutionHandoffPatterns(rdb *routing.RoutingDB, wsID, targetID string, plan *core.Plan) error {
	if targetID == "" || wsID == "" || plan == nil || len(plan.Phases) == 0 {
		return nil
	}
	return rdb.RecordPlanHandoffPatterns(context.Background(), targetID, plan)
}

// recordRoutingDecisions inserts a routing_decisions row for each phase in plan
// immediately after decomposition. Confidence is derived from the routing context
// when a matching pattern is found; otherwise 0.0 is stored.
// tc may be nil (no routing context), in which case confidence defaults to 0.0.
func recordRoutingDecisions(wsID string, plan *core.Plan, tc *decompose.TargetContext) error {
	if wsID == "" || plan == nil || len(plan.Phases) == 0 {
		return nil
	}
	rdb, err := routing.OpenDB("")
	if err != nil {
		return fmt.Errorf("opening routing DB: %w", err)
	}
	defer rdb.Close()

	// Build a confidence lookup from TopPatterns if we have routing context.
	patternConf := make(map[string]float64)
	if tc != nil {
		for _, h := range tc.TopPatterns {
			if _, exists := patternConf[h.Persona]; !exists {
				patternConf[h.Persona] = h.Confidence
			}
		}
	}

	ctx := context.Background()
	for _, p := range plan.Phases {
		if p == nil || p.Persona == "" {
			continue
		}
		conf := patternConf[p.Persona]
		if conf == 0.0 && p.PersonaSelectionMethod == "predecomposed" {
			conf = 1.0
		}
		if err := rdb.RecordRoutingDecision(ctx, routing.RoutingDecision{
			MissionID:     wsID,
			PhaseID:       p.ID,
			PhaseName:     p.Name,
			Persona:       p.Persona,
			Confidence:    conf,
			RoutingMethod: p.PersonaSelectionMethod,
		}); err != nil {
			return fmt.Errorf("recording routing decision for phase %q: %w", p.ID, err)
		}
	}
	return nil
}

// updateRoutingOutcomes updates the outcome of every routing decision for wsID
// based on the final phase status in plan. Called after engine.Execute returns.
func updateRoutingOutcomes(wsID string, plan *core.Plan) error {
	if wsID == "" || plan == nil || len(plan.Phases) == 0 {
		return nil
	}
	rdb, err := routing.OpenDB("")
	if err != nil {
		return fmt.Errorf("opening routing DB: %w", err)
	}
	defer rdb.Close()

	ctx := context.Background()
	for _, p := range plan.Phases {
		if p == nil {
			continue
		}
		var outcome, failureReason string
		switch p.Status {
		case core.StatusCompleted:
			outcome = "success"
		case core.StatusFailed:
			outcome = "failure"
			failureReason = p.Error
		case core.StatusSkipped:
			outcome = "skipped"
			failureReason = p.Error
		default:
			outcome = "pending"
		}
		if err := rdb.UpdateRoutingOutcome(ctx, wsID, p.ID, outcome, failureReason); err != nil {
			return fmt.Errorf("updating routing outcome for phase %q: %w", p.ID, err)
		}
	}
	return nil
}

func summarizeForRouting(text string, maxLen int) string {
	text = strings.TrimSpace(text)
	runes := []rune(text)
	if maxLen <= 0 || len(runes) <= maxLen {
		return text
	}
	return string(runes[:maxLen]) + "..."
}

// registerRuntimeExecutors registers all non-default runtime executors on eng.
// Called immediately after engine.New so every executor is available before
// the first Execute call. Executors whose binary is absent are registered
// anyway — the binary check happens at dispatch time, producing a clear error
// rather than a silent fallback to Claude.
func registerRuntimeExecutors(eng *engine.Engine) {
	eng.RegisterExecutor(core.RuntimeCodex, engine.NewCodexExecutor())
}

func normalizeExecutionResult(plan *core.Plan, result *core.ExecutionResult, err error) *core.ExecutionResult {
	if result == nil {
		msg := "execution returned no result"
		if err != nil {
			msg = err.Error()
		}
		return &core.ExecutionResult{
			Plan:    plan,
			Success: false,
			Error:   msg,
		}
	}
	if result.Plan == nil {
		result.Plan = plan
	}
	if result.Error == "" && err != nil {
		result.Error = err.Error()
	}
	return result
}

func missionSucceeded(result *core.ExecutionResult, err error) bool {
	return err == nil && result != nil && result.Success
}

func commitGitIsolation(ws *core.Workspace, task string, emitter event.Emitter, missionID string) bool {
	if ws == nil || ws.WorktreePath == "" {
		return true
	}

	if ws.BaseBranch != "" && ws.BranchName != "" {
		if moved, commits, bErr := git.BaseBranchMoved(ws.GitRepoRoot, ws.BaseBranch, ws.BranchName); bErr == nil && moved {
			fmt.Printf("warning: %s has %d new commit(s) since this branch was created — consider rebasing before merging\n",
				ws.BaseBranch, len(commits))
		}
	}

	summary := task
	if idx := strings.IndexByte(summary, '\n'); idx >= 0 {
		summary = summary[:idx]
	}
	if len(summary) > 72 {
		summary = summary[:69] + "..."
	}
	msg := fmt.Sprintf("via(%s): %s", ws.ID, summary)

	if err := git.CommitAll(ws.WorktreePath, msg); err != nil {
		fmt.Fprintf(os.Stderr, "warning: could not commit changes in worktree: %v\n", err)
		fmt.Printf("git: worktree preserved at %s (commit failed, branch: %s)\n", ws.WorktreePath, ws.BranchName)
		return false
	}

	emitter.Emit(context.Background(), event.New(event.GitCommitted, missionID, "", "", map[string]any{
		"message": msg,
		"branch":  ws.BranchName,
	}))
	return true
}

func removeGitWorktree(ws *core.Workspace) {
	if ws == nil || ws.WorktreePath == "" {
		return
	}
	if err := git.RemoveWorktree(ws.WorktreePath); err != nil {
		fmt.Printf("warning: could not remove worktree: %v\n", err)
	}
}

func requestCodexReview(ws *core.Workspace, prURL string, emitter event.Emitter, missionID string) {
	reviewPath := ws.GitRepoRoot
	if ws != nil && ws.WorktreePath != "" {
		reviewPath = ws.WorktreePath
	}

	if git.HasCodex() {
		reviewBody, err := git.RunCodexReview(reviewPath, ws.BaseBranch, codexReviewPrompt)
		if err == nil {
			if strings.TrimSpace(reviewBody) == "" {
				fmt.Println("Codex review completed with no findings.")
				return
			}
			if err := git.CommentOnPR(ws.GitRepoRoot, prURL, buildCodexReviewComment(reviewBody)); err != nil {
				fmt.Printf("warning: could not post local Codex review: %v\n", err)
			} else {
				fmt.Println("Codex review posted from local CLI.")
			}
			return
		}
		fmt.Printf("warning: local codex review failed: %v\n", err)
	} else {
		fmt.Println("warning: codex CLI not found; falling back to GitHub review request comment")
	}

	if err := git.CommentOnPR(ws.GitRepoRoot, prURL, "@codex please review this PR"); err != nil {
		fmt.Printf("warning: could not post Codex review request: %v\n", err)
		return
	}
	fmt.Println("Codex review requested.")
	emitter.Emit(context.Background(), event.New(event.ReviewExternalRequested, missionID, "", "", map[string]any{
		"pr_url":   prURL,
		"reviewer": "codex",
	}))
}

func buildCodexReviewComment(reviewBody string) string {
	return "## Codex CLI Review\n\n_Generated locally via `codex review`._\n\n" + strings.TrimSpace(reviewBody)
}

// stripGitWorkflowSection removes any "## Git Workflow" section (and its body)
// from a mission task string. These sections are orchestrator-level instructions
// that must not reach workers — workers must never create branches, push, or open
// PRs. The section body ends at the next "##"-level heading or at EOF.
func stripGitWorkflowSection(task string) string {
	lines := strings.Split(task, "\n")
	var out []string
	inGitSection := false
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if trimmed == "## Git Workflow" {
			inGitSection = true
			continue
		}
		if inGitSection && strings.HasPrefix(trimmed, "## ") {
			inGitSection = false
		}
		if !inGitSection {
			out = append(out, line)
		}
	}
	return strings.TrimRight(strings.Join(out, "\n"), "\n")
}
