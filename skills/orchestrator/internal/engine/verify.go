package engine

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// BuildRunner executes a build verification command in targetDir.
// Returns nil on success, non-nil when the build fails.
// Injected into Engine so tests can swap in a stub without spawning processes.
type BuildRunner func(ctx context.Context, targetDir string) error

// cmdRunFn is the internal execution primitive used by newNodeAwareBuildRunner.
// Swappable in tests to avoid spawning real subprocesses.
type cmdRunFn func(ctx context.Context, dir, name string, args []string) error

// detectProjectType identifies the build system by inspecting targetDir.
// Returns "go" when go.mod is present, "node" when package.json is present,
// or "" when neither is found or targetDir is empty.
func detectProjectType(targetDir string) string {
	if targetDir == "" {
		return ""
	}
	if _, err := os.Stat(filepath.Join(targetDir, "go.mod")); err == nil {
		return "go"
	}
	if _, err := os.Stat(filepath.Join(targetDir, "package.json")); err == nil {
		return "node"
	}
	return ""
}

// detectNodeManager returns the node package manager to use based on lockfiles
// present in targetDir. Priority: bun > pnpm > yarn > npm (fallback).
func detectNodeManager(targetDir string) string {
	for _, pair := range []struct{ file, mgr string }{
		{"bun.lockb", "bun"},
		{"pnpm-lock.yaml", "pnpm"},
		{"yarn.lock", "yarn"},
	} {
		if _, err := os.Stat(filepath.Join(targetDir, pair.file)); err == nil {
			return pair.mgr
		}
	}
	return "npm"
}

// nodeHasBuildScript reports whether package.json in targetDir contains a
// non-empty scripts.build entry.
func nodeHasBuildScript(targetDir string) (bool, error) {
	data, err := os.ReadFile(filepath.Join(targetDir, "package.json"))
	if err != nil {
		return false, fmt.Errorf("reading package.json: %w", err)
	}
	var pkg struct {
		Scripts map[string]string `json:"scripts"`
	}
	if err := json.Unmarshal(data, &pkg); err != nil {
		return false, fmt.Errorf("parsing package.json: %w", err)
	}
	return pkg.Scripts["build"] != "", nil
}

// newNodeAwareBuildRunner returns a BuildRunner backed by run. The runner:
//   - runs "go build ./..." when go.mod is present (Go path, unchanged)
//   - detects the node package manager from lockfiles and checks for a build
//     script in package.json when package.json is present; emits a warning and
//     returns nil when scripts.build is absent (skip, do not fail the phase)
//   - returns nil for unknown project types
func newNodeAwareBuildRunner(run cmdRunFn) BuildRunner {
	return func(ctx context.Context, targetDir string) error {
		switch detectProjectType(targetDir) {
		case "go":
			return run(ctx, targetDir, "go", []string{"build", "./..."})
		case "node":
			manager := detectNodeManager(targetDir)
			ok, err := nodeHasBuildScript(targetDir)
			if err != nil {
				fmt.Fprintf(os.Stderr, "[build] warning: %v; skipping build verification\n", err)
				return nil
			}
			if !ok {
				fmt.Fprintf(os.Stderr, "[build] warning: no scripts.build in package.json; skipping build verification\n")
				return nil
			}
			return run(ctx, targetDir, manager, []string{"run", "build"})
		default:
			return nil // unknown project type; skip verification
		}
	}
}

// realRunCmd is the production cmdRunFn: executes name with args in dir,
// capturing combined stdout+stderr and wrapping any error.
func realRunCmd(ctx context.Context, dir, name string, args []string) error {
	cmd := exec.CommandContext(ctx, name, args...) //nolint:gosec
	cmd.Dir = dir
	var out bytes.Buffer
	cmd.Stdout = &out
	cmd.Stderr = &out
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("%s %s: %w\n%s", name, strings.Join(args, " "), err, strings.TrimSpace(out.String()))
	}
	return nil
}

// execBuildRunner is the default BuildRunner used in production.
var execBuildRunner BuildRunner = newNodeAwareBuildRunner(realRunCmd)

// verifyAndRetry runs build verification after a fix phase succeeds.
// Returns nil when the build passes, e.buildRunner is nil, or TargetDir is empty.
//
// On build failure it:
//  1. Emits a system.error warning event.
//  2. Injects a new fix+re-review pair (bounded by MaxReviewLoops) via
//     injectBuildRetry so the agent can address the build error.
//  3. Returns the build error so the caller can mark the original fix phase
//     as failed and let skipSequentialDependents skip its stale re-review.
func (e *Engine) verifyAndRetry(ctx context.Context, fixPhase *core.Phase) error {
	if e.buildRunner == nil || fixPhase.TargetDir == "" {
		return nil
	}

	buildErr := e.buildRunner(ctx, fixPhase.TargetDir)
	if buildErr == nil {
		return nil
	}

	e.emit(ctx, event.SystemError, fixPhase.ID, "", map[string]any{
		"warning": true,
		"error":   fmt.Sprintf("build verification failed after fix %q: %v", fixPhase.Name, buildErr),
	})

	// Inject a retry if loop bound allows it.
	if len(fixPhase.Dependencies) > 0 {
		reviewPhaseID := fixPhase.Dependencies[0]
		if reviewPhase, ok := e.phases[reviewPhaseID]; ok {
			e.injectBuildRetry(ctx, reviewPhase, fixPhase, buildErr)
		}
	}

	return buildErr
}

// injectBuildRetry appends a new fix phase (depending on the original review
// phase) and a follow-up re-review gate after a build verification failure.
// Does nothing when the loop bound (MaxReviewLoops) is already exhausted or
// the review phase has no implementation dependency to inherit persona from.
func (e *Engine) injectBuildRetry(ctx context.Context, reviewPhase, failedFix *core.Phase, buildErr error) {
	maxLoops := reviewPhase.MaxReviewLoops
	if maxLoops <= 0 {
		maxLoops = defaultMaxReviewLoops
	}
	if failedFix.ReviewIteration >= maxLoops {
		if e.config.Verbose {
			fmt.Printf("[engine] build verification failed but max review loops (%d) exhausted; no retry\n", maxLoops)
		}
		return
	}

	// Inherit persona/skills/targetDir from the implementation phase.
	var implPhase *core.Phase
	for _, depID := range reviewPhase.Dependencies {
		if dep, ok := e.phases[depID]; ok {
			implPhase = dep
			break
		}
	}
	if implPhase == nil {
		return
	}

	retryFix := &core.Phase{
		ID: fmt.Sprintf("phase-%d", len(e.plan.Phases)+1),
		Name: "fix",
		Objective: fmt.Sprintf(
			"Fix build failures introduced by the previous fix attempt. "+
				"The build command failed with the following output:\n\n%s\n\n"+
				"Ensure the project builds cleanly before finishing.",
			buildErr.Error(),
		),
		Persona:         implPhase.Persona,
		ModelTier:       implPhase.ModelTier,
		Runtime:         implPhase.Runtime,
		Skills:          append([]string{}, implPhase.Skills...),
		TargetDir:       implPhase.TargetDir,
		Dependencies:    []string{reviewPhase.ID},
		Status:          core.StatusPending,
		Role:            core.RoleImplementer,
		ReviewIteration: failedFix.ReviewIteration, // same iteration count — this is a retry
		OriginPhaseID:   implPhase.ID,
		MaxReviewLoops:  reviewPhase.MaxReviewLoops,
	}
	e.plan.Phases = append(e.plan.Phases, retryFix)
	e.phases[retryFix.ID] = retryFix

	reReview := e.injectReReviewPhase(reviewPhase, retryFix)
	if reReview != nil {
		e.phases[reReview.ID] = reReview
	}

	if e.config.Verbose {
		reReviewID := ""
		if reReview != nil {
			reReviewID = reReview.ID
		}
		fmt.Printf("[engine] build verification failed: injected retry fix %s and re-review %s\n",
			retryFix.ID, reReviewID)
	}

	_ = ctx // ctx reserved for future event emission inside this helper
}
