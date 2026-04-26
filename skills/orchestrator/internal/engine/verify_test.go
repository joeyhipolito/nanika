package engine

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// ---------------------------------------------------------------------------
// detectProjectType
// ---------------------------------------------------------------------------

func TestDetectProjectType_GoMod(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "go.mod"), []byte("module example.com/test\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if got := detectProjectType(dir); got != "go" {
		t.Errorf("detectProjectType = %q, want %q", got, "go")
	}
}

func TestDetectProjectType_PackageJSON(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "package.json"), []byte(`{"name":"test"}`), 0o644); err != nil {
		t.Fatal(err)
	}
	if got := detectProjectType(dir); got != "node" {
		t.Errorf("detectProjectType = %q, want %q", got, "node")
	}
}

func TestDetectProjectType_GoModTakesPrecedence(t *testing.T) {
	// When both go.mod and package.json exist, go is preferred (go.mod checked first).
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "go.mod"), []byte("module example.com/test\n"), 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "package.json"), []byte(`{}`), 0o644); err != nil {
		t.Fatal(err)
	}
	if got := detectProjectType(dir); got != "go" {
		t.Errorf("detectProjectType = %q, want %q", got, "go")
	}
}

func TestDetectProjectType_Unknown(t *testing.T) {
	dir := t.TempDir()
	if got := detectProjectType(dir); got != "" {
		t.Errorf("detectProjectType = %q, want empty string for unknown project", got)
	}
}

func TestDetectProjectType_EmptyDir(t *testing.T) {
	if got := detectProjectType(""); got != "" {
		t.Errorf("detectProjectType(\"\") = %q, want \"\"", got)
	}
}

// ---------------------------------------------------------------------------
// verifyAndRetry — nil runner / no target dir
// ---------------------------------------------------------------------------

func TestVerifyAndRetry_NilBuildRunner(t *testing.T) {
	e := newTestEngine()
	e.buildRunner = nil

	fixPhase := &core.Phase{ID: "phase-fix", Name: "fix", TargetDir: "/some/dir"}
	e.plan.Phases = []*core.Phase{fixPhase}
	e.phases["phase-fix"] = fixPhase

	if err := e.verifyAndRetry(context.Background(), fixPhase); err != nil {
		t.Fatalf("expected nil when buildRunner is nil, got %v", err)
	}
	if len(e.plan.Phases) != 1 {
		t.Errorf("expected plan unchanged, got %d phases", len(e.plan.Phases))
	}
}

func TestVerifyAndRetry_EmptyTargetDir(t *testing.T) {
	called := false
	e := newTestEngine()
	e.buildRunner = func(_ context.Context, _ string) error {
		called = true
		return nil
	}

	fixPhase := &core.Phase{ID: "phase-fix", Name: "fix", TargetDir: ""}
	e.plan.Phases = []*core.Phase{fixPhase}
	e.phases["phase-fix"] = fixPhase

	if err := e.verifyAndRetry(context.Background(), fixPhase); err != nil {
		t.Fatalf("expected nil when TargetDir is empty, got %v", err)
	}
	if called {
		t.Error("buildRunner should not be called when TargetDir is empty")
	}
}

// ---------------------------------------------------------------------------
// verifyAndRetry — build passes
// ---------------------------------------------------------------------------

func TestVerifyAndRetry_BuildPasses(t *testing.T) {
	e := newTestEngine()
	e.buildRunner = func(_ context.Context, _ string) error { return nil }

	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}
	fixPhase := &core.Phase{
		ID:           "phase-3",
		Name:         "fix",
		Dependencies: []string{"phase-2"},
		TargetDir:    "/some/dir",
		ReviewIteration: 1,
	}

	e.plan.Phases = []*core.Phase{implPhase, reviewPhase, fixPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase
	e.phases["phase-3"] = fixPhase

	if err := e.verifyAndRetry(context.Background(), fixPhase); err != nil {
		t.Fatalf("expected nil when build passes, got %v", err)
	}
	// No retry phases injected.
	if len(e.plan.Phases) != 3 {
		t.Errorf("expected 3 phases (no injection), got %d", len(e.plan.Phases))
	}
}

// ---------------------------------------------------------------------------
// verifyAndRetry — build fails → retry injected
// ---------------------------------------------------------------------------

func TestVerifyAndRetry_BuildFails_InjectsRetryAndReReview(t *testing.T) {
	em := &captureEmitter{}
	e := newTestEngine()
	e.emitter = em
	buildErrMsg := "exit status 1: undefined: Foo"
	e.buildRunner = func(_ context.Context, _ string) error {
		return errors.New(buildErrMsg)
	}

	implPhase := &core.Phase{
		ID:        "phase-1",
		Persona:   "senior-backend-engineer",
		Skills:    []string{"golang-pro"},
		TargetDir: "/proj",
		Status:    core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}
	fixPhase := &core.Phase{
		ID:              "phase-3",
		Name:            "fix",
		Dependencies:    []string{"phase-2"},
		TargetDir:       "/proj",
		ReviewIteration: 1,
		OriginPhaseID:   "phase-1",
	}

	e.plan.Phases = []*core.Phase{implPhase, reviewPhase, fixPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase
	e.phases["phase-3"] = fixPhase

	err := e.verifyAndRetry(context.Background(), fixPhase)
	if err == nil {
		t.Fatal("expected build error to be returned")
	}
	if !strings.Contains(err.Error(), buildErrMsg) {
		t.Errorf("error = %q, want it to contain %q", err.Error(), buildErrMsg)
	}

	// impl + review + fix(failed) + retryFix + reReview = 5 phases.
	if len(e.plan.Phases) != 5 {
		t.Fatalf("expected 5 phases after retry injection, got %d", len(e.plan.Phases))
	}

	retryFix := e.plan.Phases[3]
	if retryFix.Name != "fix" {
		t.Errorf("retryFix.Name = %q, want %q", retryFix.Name, "fix")
	}
	// Retry fix must depend on the review phase (not the failed fix).
	if len(retryFix.Dependencies) != 1 || retryFix.Dependencies[0] != "phase-2" {
		t.Errorf("retryFix.Dependencies = %v, want [phase-2]", retryFix.Dependencies)
	}
	if retryFix.ReviewIteration != fixPhase.ReviewIteration {
		t.Errorf("retryFix.ReviewIteration = %d, want %d", retryFix.ReviewIteration, fixPhase.ReviewIteration)
	}
	if retryFix.Persona != implPhase.Persona {
		t.Errorf("retryFix.Persona = %q, want %q", retryFix.Persona, implPhase.Persona)
	}
	if retryFix.TargetDir != implPhase.TargetDir {
		t.Errorf("retryFix.TargetDir = %q, want %q", retryFix.TargetDir, implPhase.TargetDir)
	}
	if !strings.Contains(retryFix.Objective, buildErrMsg) {
		t.Errorf("retryFix.Objective does not mention build error: %q", retryFix.Objective)
	}

	reReview := e.plan.Phases[4]
	if reReview.Role != core.RoleReviewer {
		t.Errorf("reReview.Role = %q, want reviewer", reReview.Role)
	}
	if len(reReview.Dependencies) != 1 || reReview.Dependencies[0] != retryFix.ID {
		t.Errorf("reReview.Dependencies = %v, want [%s]", reReview.Dependencies, retryFix.ID)
	}

	// Warning event must have been emitted.
	found := false
	for _, ev := range em.collected() {
		if ev.Type != event.SystemError {
			continue
		}
		if w, _ := ev.Data["warning"].(bool); !w {
			continue
		}
		if msg, _ := ev.Data["error"].(string); strings.Contains(msg, "build verification failed") {
			found = true
		}
	}
	if !found {
		t.Fatal("expected build verification warning event; none found")
	}
}

// ---------------------------------------------------------------------------
// verifyAndRetry — build fails at max loops → no retry
// ---------------------------------------------------------------------------

func TestVerifyAndRetry_BuildFails_MaxLoopsExhausted(t *testing.T) {
	e := newTestEngine()
	e.buildRunner = func(_ context.Context, _ string) error {
		return errors.New("build failed")
	}

	implPhase := &core.Phase{
		ID:        "phase-1",
		Persona:   "senior-backend-engineer",
		TargetDir: "/proj",
		Status:    core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}
	fixPhase := &core.Phase{
		ID:              "phase-3",
		Name:            "fix",
		Dependencies:    []string{"phase-2"},
		TargetDir:       "/proj",
		ReviewIteration: 2, // already at max (MaxReviewLoops == 2)
		OriginPhaseID:   "phase-1",
	}

	e.plan.Phases = []*core.Phase{implPhase, reviewPhase, fixPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase
	e.phases["phase-3"] = fixPhase

	err := e.verifyAndRetry(context.Background(), fixPhase)
	if err == nil {
		t.Fatal("expected build error to be returned")
	}
	// No retry phases should be injected when loop is exhausted.
	if len(e.plan.Phases) != 3 {
		t.Errorf("expected 3 phases (no retry when exhausted), got %d", len(e.plan.Phases))
	}
}

// ---------------------------------------------------------------------------
// verifyAndRetry — build fails, review phase missing from map → no panic
// ---------------------------------------------------------------------------

func TestVerifyAndRetry_BuildFails_ReviewPhaseNotInMap(t *testing.T) {
	e := newTestEngine()
	e.buildRunner = func(_ context.Context, _ string) error {
		return errors.New("build failed")
	}

	fixPhase := &core.Phase{
		ID:           "phase-3",
		Name:         "fix",
		Dependencies: []string{"phase-2"}, // phase-2 not registered in e.phases
		TargetDir:    "/proj",
	}

	e.plan.Phases = []*core.Phase{fixPhase}
	e.phases["phase-3"] = fixPhase

	err := e.verifyAndRetry(context.Background(), fixPhase)
	if err == nil {
		t.Fatal("expected build error to be returned")
	}
	// No retry injected (review phase not found), but no panic.
	if len(e.plan.Phases) != 1 {
		t.Errorf("expected plan unchanged at 1 phase, got %d", len(e.plan.Phases))
	}
}

// ---------------------------------------------------------------------------
// verifyAndRetry — fix depends on review which has no impl dependency
// ---------------------------------------------------------------------------

func TestVerifyAndRetry_BuildFails_NoImplPhase(t *testing.T) {
	e := newTestEngine()
	e.buildRunner = func(_ context.Context, _ string) error {
		return errors.New("build failed")
	}

	// Review phase has no dependencies → implPhase lookup returns nil.
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{}, // no impl phase
		MaxReviewLoops:         2,
	}
	fixPhase := &core.Phase{
		ID:           "phase-3",
		Name:         "fix",
		Dependencies: []string{"phase-2"},
		TargetDir:    "/proj",
	}

	e.plan.Phases = []*core.Phase{reviewPhase, fixPhase}
	e.phases["phase-2"] = reviewPhase
	e.phases["phase-3"] = fixPhase

	err := e.verifyAndRetry(context.Background(), fixPhase)
	if err == nil {
		t.Fatal("expected build error to be returned")
	}
	// No retry injected (no impl phase to inherit persona from), no panic.
	if len(e.plan.Phases) != 2 {
		t.Errorf("expected plan unchanged at 2 phases, got %d", len(e.plan.Phases))
	}
}

// ---------------------------------------------------------------------------
// execBuildRunner (newNodeAwareBuildRunner) — command routing via stub runner
// ---------------------------------------------------------------------------

func TestExecBuildRunner_BunNoBuildScript(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "bun.lockb"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "package.json"), []byte(`{"name":"test","scripts":{}}`), 0o644); err != nil {
		t.Fatal(err)
	}

	var called bool
	runner := newNodeAwareBuildRunner(func(_ context.Context, _ string, _ string, _ []string) error {
		called = true
		return nil
	})
	if err := runner(context.Background(), dir); err != nil {
		t.Fatalf("expected nil when build script absent, got %v", err)
	}
	if called {
		t.Error("cmdRunFn must not be called when scripts.build is absent")
	}
}

func TestExecBuildRunner_NpmWithBuildScript(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "package.json"), []byte(`{"name":"test","scripts":{"build":"echo ok"}}`), 0o644); err != nil {
		t.Fatal(err)
	}

	var gotName string
	var gotArgs []string
	runner := newNodeAwareBuildRunner(func(_ context.Context, _ string, name string, args []string) error {
		gotName = name
		gotArgs = args
		return nil
	})
	if err := runner(context.Background(), dir); err != nil {
		t.Fatalf("expected nil, got %v", err)
	}
	if gotName != "npm" {
		t.Errorf("manager = %q, want %q", gotName, "npm")
	}
	if len(gotArgs) != 2 || gotArgs[0] != "run" || gotArgs[1] != "build" {
		t.Errorf("args = %v, want [run build]", gotArgs)
	}
}

func TestExecBuildRunner_PnpmDetected(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "pnpm-lock.yaml"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "package.json"), []byte(`{"scripts":{"build":"tsc"}}`), 0o644); err != nil {
		t.Fatal(err)
	}

	var gotName string
	var gotArgs []string
	runner := newNodeAwareBuildRunner(func(_ context.Context, _ string, name string, args []string) error {
		gotName = name
		gotArgs = args
		return nil
	})
	if err := runner(context.Background(), dir); err != nil {
		t.Fatalf("expected nil, got %v", err)
	}
	if gotName != "pnpm" {
		t.Errorf("manager = %q, want %q", gotName, "pnpm")
	}
	if len(gotArgs) != 2 || gotArgs[0] != "run" || gotArgs[1] != "build" {
		t.Errorf("args = %v, want [run build]", gotArgs)
	}
}

func TestExecBuildRunner_GoPath(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "go.mod"), []byte("module example.com/test\n\ngo 1.21\n"), 0o644); err != nil {
		t.Fatal(err)
	}

	var gotName string
	var gotArgs []string
	runner := newNodeAwareBuildRunner(func(_ context.Context, _ string, name string, args []string) error {
		gotName = name
		gotArgs = args
		return nil
	})
	if err := runner(context.Background(), dir); err != nil {
		t.Fatalf("expected nil, got %v", err)
	}
	if gotName != "go" {
		t.Errorf("manager = %q, want %q", gotName, "go")
	}
	if len(gotArgs) != 2 || gotArgs[0] != "build" || gotArgs[1] != "./..." {
		t.Errorf("args = %v, want [build ./...]", gotArgs)
	}
}

func TestExecBuildRunner_YarnDetected(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "yarn.lock"), nil, 0o644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "package.json"), []byte(`{"scripts":{"build":"tsc"}}`), 0o644); err != nil {
		t.Fatal(err)
	}

	var gotName string
	var gotArgs []string
	runner := newNodeAwareBuildRunner(func(_ context.Context, _ string, name string, args []string) error {
		gotName = name
		gotArgs = args
		return nil
	})
	if err := runner(context.Background(), dir); err != nil {
		t.Fatalf("expected nil, got %v", err)
	}
	if gotName != "yarn" {
		t.Errorf("manager = %q, want %q", gotName, "yarn")
	}
	if len(gotArgs) != 2 || gotArgs[0] != "run" || gotArgs[1] != "build" {
		t.Errorf("args = %v, want [run build]", gotArgs)
	}
}

func TestExecBuildRunner_Unknown(t *testing.T) {
	dir := t.TempDir() // no go.mod, no package.json

	var called bool
	runner := newNodeAwareBuildRunner(func(_ context.Context, _ string, _ string, _ []string) error {
		called = true
		return nil
	})
	if err := runner(context.Background(), dir); err != nil {
		t.Fatalf("expected nil for unknown project, got %v", err)
	}
	if called {
		t.Error("cmdRunFn must not be called for unknown project type")
	}
}

// ---------------------------------------------------------------------------
// injectBuildRetry — skills are defensively copied
// ---------------------------------------------------------------------------

func TestInjectBuildRetry_SkillsAreCopied(t *testing.T) {
	e := newTestEngine()

	skills := []string{"golang-pro", "golang-testing"}
	implPhase := &core.Phase{
		ID:        "phase-1",
		Persona:   "senior-backend-engineer",
		Skills:    skills,
		TargetDir: "/proj",
		Status:    core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}
	failedFix := &core.Phase{
		ID:              "phase-3",
		Name:            "fix",
		Dependencies:    []string{"phase-2"},
		ReviewIteration: 1,
	}

	e.plan.Phases = []*core.Phase{implPhase, reviewPhase, failedFix}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase
	e.phases["phase-3"] = failedFix

	e.injectBuildRetry(context.Background(), reviewPhase, failedFix, errors.New("build failed"))

	if len(e.plan.Phases) < 4 {
		t.Fatal("expected retry fix to be injected")
	}
	retryFix := e.plan.Phases[3]

	// Mutate the original skills — retry fix should be unaffected.
	implPhase.Skills[0] = "mutated"
	if retryFix.Skills[0] == "mutated" {
		t.Error("retryFix.Skills shares backing array with implPhase.Skills; expected defensive copy")
	}
}
