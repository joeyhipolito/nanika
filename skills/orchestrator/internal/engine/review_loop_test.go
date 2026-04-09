package engine

import (
	"context"
	"fmt"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
)

// ---------------------------------------------------------------------------
// ParseReviewFindings
// ---------------------------------------------------------------------------

func TestParseReviewFindings_WithBlockers(t *testing.T) {
	input := `
## Summary

Two blockers and one warning.

### Blockers
- **[store.go:42]** Missing error check on db.Query.
  Fix: Wrap and return the error.
- **[handler.go:88]** Nil pointer dereference on empty response.
  Fix: Guard with a nil check.

### Warnings
- **[util.go:10]** Exported function missing godoc.
`
	f := ParseReviewFindings(input)
	if f.Passed() {
		t.Fatal("expected Passed()==false, got true")
	}
	if len(f.Blockers) != 2 {
		t.Fatalf("expected 2 blockers, got %d: %v", len(f.Blockers), f.Blockers)
	}
	if len(f.Warnings) != 1 {
		t.Fatalf("expected 1 warning, got %d", len(f.Warnings))
	}
	if f.Blockers[0].Location != "store.go:42" {
		t.Errorf("blocker[0].Location = %q, want %q", f.Blockers[0].Location, "store.go:42")
	}
	if !strings.Contains(f.Blockers[0].Description, "Missing error check") {
		t.Errorf("blocker[0].Description missing expected text: %q", f.Blockers[0].Description)
	}
	if f.Blockers[1].Location != "handler.go:88" {
		t.Errorf("blocker[1].Location = %q, want %q", f.Blockers[1].Location, "handler.go:88")
	}
}

func TestParseReviewFindings_NoBlockers(t *testing.T) {
	input := `
### Warnings
- **[main.go:5]** Unused import.

### Suggestions
- Consider adding a README.
`
	f := ParseReviewFindings(input)
	if !f.Passed() {
		t.Fatal("expected Passed()==true, got false")
	}
	if len(f.Blockers) != 0 {
		t.Errorf("expected 0 blockers, got %d", len(f.Blockers))
	}
	if len(f.Warnings) != 1 {
		t.Errorf("expected 1 warning, got %d", len(f.Warnings))
	}
}

func TestParseReviewFindings_EmptyOutput(t *testing.T) {
	f := ParseReviewFindings("")
	if !f.Passed() {
		t.Fatal("expected Passed()==true for empty output")
	}
	if len(f.Blockers) != 0 {
		t.Errorf("expected 0 blockers, got %d", len(f.Blockers))
	}
}

func TestParseReviewFindings_MalformedOutput(t *testing.T) {
	// No standard ### headers — parser returns empty, fail-open.
	input := `This review has no standard sections. Everything looks fine.`
	f := ParseReviewFindings(input)
	if !f.Passed() {
		t.Fatal("expected Passed()==true for malformed output (fail-open)")
	}
}

func TestParseReviewFindings_MultilineBlocker(t *testing.T) {
	input := `
### Blockers
- **[engine.go:150]** Race condition in concurrent map access.
  Why: Multiple goroutines write to the map without a lock.
  Fix: Use sync.Mutex or sync.Map.
`
	f := ParseReviewFindings(input)
	if f.Passed() {
		t.Fatal("expected Passed()==false")
	}
	if len(f.Blockers) != 1 {
		t.Fatalf("expected 1 blocker, got %d", len(f.Blockers))
	}
	desc := f.Blockers[0].Description
	if !strings.Contains(desc, "Race condition") {
		t.Errorf("description missing 'Race condition': %q", desc)
	}
	if !strings.Contains(desc, "Fix:") {
		t.Errorf("description missing Fix continuation: %q", desc)
	}
}

func TestParseReviewFindings_BlockerWithoutLocation(t *testing.T) {
	input := `
### Blockers
- **[]** No location provided but still a blocker.
`
	f := ParseReviewFindings(input)
	if f.Passed() {
		t.Fatal("expected Passed()==false")
	}
	// Location is empty string (between **[** and **]**)
	if f.Blockers[0].Location != "" {
		t.Errorf("expected empty location, got %q", f.Blockers[0].Location)
	}
}

// TestParseReviewFindings_FollowsObjectiveFormat verifies that review output
// following the format specified in the objective (as injected by ensureCodeReviewPhase)
// is correctly parsed. This test ensures the hardened prompt guidance works end-to-end.
func TestParseReviewFindings_FollowsObjectiveFormat(t *testing.T) {
	// Simulates output from a reviewer following the format guidance:
	// "Produce structured output with ### Blockers and ### Warnings sections,
	// listing each finding as a '- **[location]** description' item"
	input := `
## Summary
Implementation looks mostly solid but has a few issues to address.

### Blockers
- **[file.go:42]** Missing error check on database connection.
- **[service.go:18]** Potential nil pointer dereference.

### Warnings
- **[main.go:5]** Inefficient loop that could be optimized with a map.
`
	f := ParseReviewFindings(input)
	if f.Passed() {
		t.Fatal("expected Passed()==false due to blockers")
	}
	if len(f.Blockers) != 2 {
		t.Fatalf("expected 2 blockers, got %d", len(f.Blockers))
	}
	if len(f.Warnings) != 1 {
		t.Fatalf("expected 1 warning, got %d", len(f.Warnings))
	}
	if f.Blockers[0].Location != "file.go:42" {
		t.Errorf("blocker[0] location = %q, want file.go:42", f.Blockers[0].Location)
	}
	if !strings.Contains(f.Blockers[0].Description, "Missing error check") {
		t.Errorf("blocker[0] description missing expected text: %q", f.Blockers[0].Description)
	}
}

// ---------------------------------------------------------------------------
// IsReviewGate
// ---------------------------------------------------------------------------

func TestIsReviewGate_RequiredReview(t *testing.T) {
	e := newTestEngine()
	p := &core.Phase{
		Persona:                "staff-code-reviewer",
		PersonaSelectionMethod: core.SelectionRequiredReview,
	}
	if !e.IsReviewGate(p) {
		t.Fatal("expected IsReviewGate==true")
	}
}

func TestIsReviewGate_RegularPhase(t *testing.T) {
	e := newTestEngine()
	p := &core.Phase{
		Persona:                "staff-code-reviewer",
		PersonaSelectionMethod: "llm",
		Role:                   core.RoleReviewer,
	}
	if e.IsReviewGate(p) {
		t.Fatal("expected IsReviewGate==false without implementer dependency")
	}
}

func TestIsReviewGate_ExplicitReviewerPhaseOverImplementation(t *testing.T) {
	e := newTestEngine()
	impl := &core.Phase{
		ID:      "phase-1",
		Persona: "senior-backend-engineer",
		Role:    core.RoleImplementer,
		Status:  core.StatusCompleted,
	}
	review := &core.Phase{
		ID:           "phase-2",
		Persona:      "staff-code-reviewer",
		Role:         core.RoleReviewer,
		Dependencies: []string{"phase-1"},
	}
	e.phases[impl.ID] = impl
	e.phases[review.ID] = review
	if !e.IsReviewGate(review) {
		t.Fatal("expected explicit reviewer phase over implementer output to trigger review loop")
	}
}

func TestIsReviewGate_QAReviewerPhaseDoesNotTriggerLoop(t *testing.T) {
	e := newTestEngine()
	impl := &core.Phase{
		ID:      "phase-1",
		Persona: "senior-backend-engineer",
		Role:    core.RoleImplementer,
		Status:  core.StatusCompleted,
	}
	review := &core.Phase{
		ID:           "phase-2",
		Persona:      "qa-engineer",
		Role:         core.RoleReviewer,
		Dependencies: []string{"phase-1"},
	}
	e.phases[impl.ID] = impl
	e.phases[review.ID] = review
	if e.IsReviewGate(review) {
		t.Fatal("expected qa-engineer review phase to stay outside the autonomous fix loop")
	}
}

func TestIsReviewGate_SecurityReviewerPhaseDoesNotTriggerLoop(t *testing.T) {
	e := newTestEngine()
	impl := &core.Phase{
		ID:      "phase-1",
		Persona: "senior-backend-engineer",
		Role:    core.RoleImplementer,
		Status:  core.StatusCompleted,
	}
	review := &core.Phase{
		ID:           "phase-2",
		Persona:      "security-auditor",
		Role:         core.RoleReviewer,
		Dependencies: []string{"phase-1"},
	}
	e.phases[impl.ID] = impl
	e.phases[review.ID] = review
	if e.IsReviewGate(review) {
		t.Fatal("expected security-auditor review phase to stay outside the autonomous fix loop")
	}
}

func TestIsReviewGate_NilPhase(t *testing.T) {
	e := newTestEngine()
	if e.IsReviewGate(nil) {
		t.Fatal("expected IsReviewGate==false for nil phase")
	}
}

// ---------------------------------------------------------------------------
// injectFixPhase
// ---------------------------------------------------------------------------

func newTestEngine() *Engine {
	return &Engine{
		config:  &core.OrchestratorConfig{},
		phases:  make(map[string]*core.Phase),
		plan:    &core.Plan{},
		emitter: &captureEmitter{},
		workspace: &core.Workspace{
			ID:   "test-ws",
			Path: tTempDirCompat(),
		},
	}
}

func tTempDirCompat() string {
	// The review-loop helpers only need a non-empty workspace path for event
	// emission and other lightweight engine helpers; tests do not write to it.
	return "."
}

func Test_injectFixPhase_WithBlockers(t *testing.T) {
	implPhase := &core.Phase{
		ID:        "phase-1",
		Name:      "implement",
		Persona:   "senior-backend-engineer",
		ModelTier: "work",
		Runtime:   core.Runtime("codex"),
		Skills:    []string{"golang-pro"},
		TargetDir: "/tmp/myproject",
		Status:    core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		Persona:                "staff-code-reviewer",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
		Status:                 core.StatusCompleted,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	findings := ReviewFindings{
		Blockers: []ReviewItem{
			{Location: "store.go:10", Description: "Missing nil check"},
			{Location: "api.go:55", Description: "Unhandled error"},
		},
	}

	fixPhase := e.injectFixPhase(reviewPhase, findings)
	if fixPhase == nil {
		t.Fatal("expected fix phase, got nil")
	}

	// Plan must have 3 phases now
	if len(e.plan.Phases) != 3 {
		t.Fatalf("expected 3 phases, got %d", len(e.plan.Phases))
	}

	// Fix phase fields
	if fixPhase.Persona != implPhase.Persona {
		t.Errorf("Persona = %q, want %q", fixPhase.Persona, implPhase.Persona)
	}
	if fixPhase.TargetDir != implPhase.TargetDir {
		t.Errorf("TargetDir = %q, want %q", fixPhase.TargetDir, implPhase.TargetDir)
	}
	if fixPhase.Runtime != implPhase.Runtime {
		t.Errorf("Runtime = %q, want %q", fixPhase.Runtime, implPhase.Runtime)
	}
	if len(fixPhase.Dependencies) != 1 || fixPhase.Dependencies[0] != reviewPhase.ID {
		t.Errorf("Dependencies = %v, want [%s]", fixPhase.Dependencies, reviewPhase.ID)
	}
	if fixPhase.OriginPhaseID != implPhase.ID {
		t.Errorf("OriginPhaseID = %q, want %q", fixPhase.OriginPhaseID, implPhase.ID)
	}
	if fixPhase.ReviewIteration != 1 {
		t.Errorf("ReviewIteration = %d, want 1", fixPhase.ReviewIteration)
	}
	if fixPhase.Status != core.StatusPending {
		t.Errorf("Status = %q, want pending", fixPhase.Status)
	}
}

func Test_injectFixPhase_NoBlockers(t *testing.T) {
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}
	e := newTestEngine()
	e.plan.Phases = []*core.Phase{reviewPhase}
	e.phases["phase-2"] = reviewPhase

	fixPhase := e.injectFixPhase(reviewPhase, ReviewFindings{})
	if fixPhase != nil {
		t.Fatal("expected nil fix phase when no blockers")
	}
	if len(e.plan.Phases) != 1 {
		t.Fatalf("expected plan unchanged (1 phase), got %d", len(e.plan.Phases))
	}
}

func Test_injectFixPhase_MaxIterationReached(t *testing.T) {
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
		ReviewIteration:        1, // already at max
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	findings := ReviewFindings{Blockers: []ReviewItem{{Description: "some blocker"}}}
	fixPhase := e.injectFixPhase(reviewPhase, findings)
	if fixPhase != nil {
		t.Fatal("expected nil fix phase when max iterations reached")
	}
}

func Test_injectFixPhase_ObjectiveIsConcise(t *testing.T) {
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	longBlockerDesc := strings.Repeat("very long blocker description text ", 50)
	findings := ReviewFindings{
		Blockers: []ReviewItem{{Location: "file.go:1", Description: longBlockerDesc}},
	}

	fixPhase := e.injectFixPhase(reviewPhase, findings)
	if fixPhase == nil {
		t.Fatal("expected fix phase")
	}

	// Objective should reference the blockers but not duplicate a multi-KB raw review.
	if !strings.Contains(fixPhase.Objective, "blocker") {
		t.Errorf("objective should reference blockers: %q", fixPhase.Objective)
	}
	// Objective should not contain the full long description verbatim (summaries are truncated by intent).
	if len(fixPhase.Objective) > 2000 {
		t.Errorf("objective is too long (%d chars); should be a concise summary", len(fixPhase.Objective))
	}
}

func Test_injectFixPhase_ObjectiveIsCappedAcrossManyBlockers(t *testing.T) {
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	var blockers []ReviewItem
	for i := 0; i < 10; i++ {
		blockers = append(blockers, ReviewItem{
			Location:    "file.go:1",
			Description: strings.Repeat("very long blocker description text ", 20),
		})
	}

	fixPhase := e.injectFixPhase(reviewPhase, ReviewFindings{Blockers: blockers})
	if fixPhase == nil {
		t.Fatal("expected fix phase")
	}
	if len(fixPhase.Objective) > 1600 {
		t.Fatalf("objective too long: %d chars", len(fixPhase.Objective))
	}
	if !strings.HasSuffix(fixPhase.Objective, "...") {
		t.Fatalf("expected truncated objective to end with ellipsis: %q", fixPhase.Objective)
	}
}

func Test_injectFixPhase_MultipleImplementationDeps(t *testing.T) {
	impl1 := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	impl2 := &core.Phase{ID: "phase-2", Persona: "senior-frontend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-3",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1", "phase-2"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{impl1, impl2, reviewPhase}
	e.phases["phase-1"] = impl1
	e.phases["phase-2"] = impl2
	e.phases["phase-3"] = reviewPhase

	findings := ReviewFindings{Blockers: []ReviewItem{{Description: "a blocker"}}}
	fixPhase := e.injectFixPhase(reviewPhase, findings)
	if fixPhase == nil {
		t.Fatal("expected fix phase")
	}

	// Fix phase uses the FIRST dependency's persona.
	if fixPhase.Persona != impl1.Persona {
		t.Errorf("Persona = %q, want first dep persona %q", fixPhase.Persona, impl1.Persona)
	}
	if fixPhase.OriginPhaseID != impl1.ID {
		t.Errorf("OriginPhaseID = %q, want %q", fixPhase.OriginPhaseID, impl1.ID)
	}
}

func Test_injectFixPhase_DefaultMaxLoops(t *testing.T) {
	// MaxReviewLoops == 0 → engine uses defaultMaxReviewLoops (2)
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         0, // zero → use default
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	findings := ReviewFindings{Blockers: []ReviewItem{{Description: "blocker"}}}

	// First call: ReviewIteration=0, should inject.
	fix := e.injectFixPhase(reviewPhase, findings)
	if fix == nil {
		t.Fatal("expected fix phase on first call")
	}

	// Second call: ReviewIteration=2 (==defaultMaxReviewLoops), should not inject.
	reviewPhase.ReviewIteration = 2
	fix2 := e.injectFixPhase(reviewPhase, findings)
	if fix2 != nil {
		t.Fatal("expected nil after reaching default max loops")
	}
}

func Test_injectFixPhase_NoDependencies(t *testing.T) {
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{}, // no deps → implPhase will be nil
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{reviewPhase}
	e.phases["phase-2"] = reviewPhase

	findings := ReviewFindings{Blockers: []ReviewItem{{Description: "a blocker"}}}
	fixPhase := e.injectFixPhase(reviewPhase, findings)
	if fixPhase != nil {
		t.Fatal("expected nil when review phase has no dependencies")
	}
	if len(e.plan.Phases) != 1 {
		t.Fatalf("expected plan unchanged at 1 phase, got %d", len(e.plan.Phases))
	}
}

func Test_injectFixPhase_DepNotInPhasesMap(t *testing.T) {
	// Dep is listed in Dependencies but absent from e.phases map → implPhase nil.
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{reviewPhase}
	e.phases["phase-2"] = reviewPhase
	// Intentionally NOT setting e.phases["phase-1"]

	findings := ReviewFindings{Blockers: []ReviewItem{{Description: "a blocker"}}}
	fixPhase := e.injectFixPhase(reviewPhase, findings)
	if fixPhase != nil {
		t.Fatal("expected nil when dep phase is not found in phases map")
	}
}

func Test_injectFixPhase_SkillsAreCopied(t *testing.T) {
	// Fix phase should not share the backing array with implPhase.Skills.
	skills := []string{"golang-pro", "golang-testing"}
	implPhase := &core.Phase{
		ID:      "phase-1",
		Persona: "senior-backend-engineer",
		Skills:  skills,
		Status:  core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	findings := ReviewFindings{Blockers: []ReviewItem{{Description: "a blocker"}}}
	fixPhase := e.injectFixPhase(reviewPhase, findings)
	if fixPhase == nil {
		t.Fatal("expected fix phase")
	}

	// Mutate implPhase.Skills — fix phase should be unaffected.
	implPhase.Skills[0] = "mutated"
	if fixPhase.Skills[0] == "mutated" {
		t.Error("fix phase Skills shares backing array with implPhase; expected a defensive copy")
	}
}

// ---------------------------------------------------------------------------
// handleReviewLoop
// ---------------------------------------------------------------------------

func TestHandleReviewLoop_NotAReviewGate(t *testing.T) {
	e := newTestEngine()
	phase := &core.Phase{
		ID:                     "phase-1",
		PersonaSelectionMethod: "llm", // NOT a review gate
	}
	e.plan.Phases = []*core.Phase{phase}
	e.phases["phase-1"] = phase

	// Output contains blockers, but the phase is not a gate so nothing is injected.
	output := "### Blockers\n- **[file.go:1]** A blocker.\n"
	fix := e.handleReviewLoop(context.Background(), phase, output)
	if fix != nil {
		t.Fatal("expected nil for non-review-gate phase")
	}
	if len(e.plan.Phases) != 1 {
		t.Fatalf("expected plan unchanged, got %d phases", len(e.plan.Phases))
	}
}

func TestHandleReviewLoop_ReviewGatePassed(t *testing.T) {
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	// Output has only warnings (no blockers) → Passed() == true
	output := "### Warnings\n- **[main.go:1]** Unused import.\n"
	fix := e.handleReviewLoop(context.Background(), reviewPhase, output)
	if fix != nil {
		t.Fatal("expected nil when review passes (no blockers)")
	}
	if len(e.plan.Phases) != 2 {
		t.Fatalf("expected plan unchanged at 2 phases, got %d", len(e.plan.Phases))
	}
}

func TestHandleReviewLoop_ReviewGateWithBlockers(t *testing.T) {
	implPhase := &core.Phase{
		ID:        "phase-1",
		Persona:   "senior-backend-engineer",
		Skills:    []string{"golang-pro"},
		TargetDir: "/tmp/project",
		Status:    core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	output := "### Blockers\n- **[store.go:42]** Missing error check.\n  Fix: Return the error.\n"
	fix := e.handleReviewLoop(context.Background(), reviewPhase, output)
	if fix == nil {
		t.Fatal("expected fix phase to be injected")
	}
	// impl + review + fix + re-review = 4 phases.
	if len(e.plan.Phases) != 4 {
		t.Fatalf("expected 4 phases after injection (impl, review, fix, re-review), got %d", len(e.plan.Phases))
	}
	if fix.Persona != implPhase.Persona {
		t.Errorf("fix.Persona = %q, want %q", fix.Persona, implPhase.Persona)
	}
	if fix.Dependencies[0] != reviewPhase.ID {
		t.Errorf("fix.Dependencies[0] = %q, want %q", fix.Dependencies[0], reviewPhase.ID)
	}
}

func TestHandleReviewLoop_ReviewGateAtMaxIteration(t *testing.T) {
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
		ReviewIteration:        1, // already at max → injectFixPhase returns nil
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	output := "### Blockers\n- **[file.go:1]** Still broken.\n"
	fix := e.handleReviewLoop(context.Background(), reviewPhase, output)
	if fix != nil {
		t.Fatal("expected nil when max review iteration reached")
	}
	if len(e.plan.Phases) != 2 {
		t.Fatalf("expected plan unchanged at 2 phases, got %d", len(e.plan.Phases))
	}
}

func TestHandleReviewLoop_EmptyOutputIsPassOpen(t *testing.T) {
	// Empty output from a review gate → ParseReviewFindings returns Passed()==true.
	// No fix phase should be injected (fail-open behaviour is correct here).
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	fix := e.handleReviewLoop(context.Background(), reviewPhase, "")
	if fix != nil {
		t.Fatal("expected nil for empty review output (fail-open: no spurious fix phase)")
	}
}

func TestHandleReviewLoop_MalformedOutputTriggersRetry(t *testing.T) {
	em := &captureEmitter{}
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}

	e := newTestEngine()
	e.emitter = em
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	retry := e.handleReviewLoop(context.Background(), reviewPhase, "review agent returned prose without structured sections")
	if retry == nil {
		t.Fatal("expected retry review phase to be injected for malformed review output")
	}
	// impl + review + retry = 3 phases.
	if len(e.plan.Phases) != 3 {
		t.Fatalf("expected 3 phases after retry injection, got %d", len(e.plan.Phases))
	}
	// Retry must depend on the same impl phase as original review (not a fix phase).
	if len(retry.Dependencies) != 1 || retry.Dependencies[0] != "phase-1" {
		t.Errorf("retry.Dependencies = %v, want [phase-1]", retry.Dependencies)
	}
	if retry.PersonaSelectionMethod != core.SelectionRequiredReview {
		t.Errorf("retry.PersonaSelectionMethod = %q, want %q", retry.PersonaSelectionMethod, core.SelectionRequiredReview)
	}

	// Warning event must still be emitted.
	evts := em.collected()
	found := false
	for _, ev := range evts {
		if ev.Type != event.SystemError {
			continue
		}
		if warning, _ := ev.Data["warning"].(bool); !warning {
			continue
		}
		if msg, _ := ev.Data["error"].(string); strings.Contains(msg, "re-review retry") {
			found = true
		}
	}
	if !found {
		t.Fatalf("expected malformed review warning event; events=%+v", evts)
	}
}

func TestHandleReviewLoop_MalformedOutputAtMaxLoops_PassOpen(t *testing.T) {
	// When ReviewIteration is already at the max, a malformed review cannot
	// inject a retry. Execution should continue (pass-open, no fix injected).
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
		ReviewIteration:        1, // already at max
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	result := e.handleReviewLoop(context.Background(), reviewPhase, "prose without structured sections")
	if result != nil {
		t.Fatal("expected nil when max loops reached; no retry should be injected")
	}
	if len(e.plan.Phases) != 2 {
		t.Fatalf("expected plan unchanged at 2 phases, got %d", len(e.plan.Phases))
	}
}

func TestHandleReviewLoop_StructuredEmptyBlockersIsPass(t *testing.T) {
	// A review that contains "### Blockers" but lists no items is a legitimate
	// pass — reviewOutputLooksMalformed must return false, and no retry injected.
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	// Output has the required sections but no blocker items → legitimate pass.
	output := "### Blockers\n_No blockers found._\n\n### Warnings\n_No warnings._\n"
	fix := e.handleReviewLoop(context.Background(), reviewPhase, output)
	if fix != nil {
		t.Fatalf("expected nil for structured review with no blockers (legitimate pass), got %+v", fix)
	}
	if len(e.plan.Phases) != 2 {
		t.Fatalf("expected plan unchanged at 2 phases, got %d", len(e.plan.Phases))
	}
}

// ---------------------------------------------------------------------------
// parseItemLine edge cases
// ---------------------------------------------------------------------------

func TestParseItemLine_MalformedBracket(t *testing.T) {
	// **[ present but ]** not found — parser falls through to description = line.
	loc, desc := parseItemLine("- **[unclosed bracket no description")
	if loc != "" {
		t.Errorf("expected empty location, got %q", loc)
	}
	if desc != "**[unclosed bracket no description" {
		t.Errorf("description = %q, want %q", desc, "**[unclosed bracket no description")
	}
}

func TestParseItemLine_NoLocation(t *testing.T) {
	// Line without **[ prefix → location empty, description is the line.
	loc, desc := parseItemLine("- Plain text description without location.")
	if loc != "" {
		t.Errorf("expected empty location, got %q", loc)
	}
	if desc != "Plain text description without location." {
		t.Errorf("description = %q, want %q", desc, "Plain text description without location.")
	}
}

func TestParseItemLine_WithLocation(t *testing.T) {
	loc, desc := parseItemLine("- **[engine.go:150]** Race condition.")
	if loc != "engine.go:150" {
		t.Errorf("location = %q, want %q", loc, "engine.go:150")
	}
	if desc != "Race condition." {
		t.Errorf("description = %q, want %q", desc, "Race condition.")
	}
}

// ---------------------------------------------------------------------------
// ParseReviewFindings edge cases
// ---------------------------------------------------------------------------

func TestParseReviewFindings_EmptyBlockersSection(t *testing.T) {
	// ### Blockers header present but immediately followed by next header.
	input := `### Blockers
### Warnings
- **[main.go:5]** Unused import.
`
	f := ParseReviewFindings(input)
	if !f.Passed() {
		t.Fatal("expected Passed()==true when Blockers section has no items")
	}
	if len(f.Blockers) != 0 {
		t.Errorf("expected 0 blockers in empty section, got %d", len(f.Blockers))
	}
	if len(f.Warnings) != 1 {
		t.Errorf("expected 1 warning, got %d", len(f.Warnings))
	}
}

func TestParseReviewFindings_BlockersOnlyNoWarnings(t *testing.T) {
	// Document has Blockers but no Warnings section.
	input := `### Blockers
- **[api.go:20]** Unhandled nil.
`
	f := ParseReviewFindings(input)
	if f.Passed() {
		t.Fatal("expected Passed()==false")
	}
	if len(f.Blockers) != 1 {
		t.Errorf("expected 1 blocker, got %d", len(f.Blockers))
	}
	if len(f.Warnings) != 0 {
		t.Errorf("expected 0 warnings, got %d", len(f.Warnings))
	}
}

func TestReviewOutputLooksMalformed(t *testing.T) {
	t.Run("plain text without sections", func(t *testing.T) {
		findings := ParseReviewFindings("This review has no standard sections.")
		if !reviewOutputLooksMalformed("This review has no standard sections.", findings) {
			t.Fatal("expected malformed review output to be detected")
		}
	})

	t.Run("empty output is not malformed", func(t *testing.T) {
		findings := ParseReviewFindings("")
		if reviewOutputLooksMalformed("", findings) {
			t.Fatal("empty output should not be treated as malformed")
		}
	})

	t.Run("structured warnings are not malformed", func(t *testing.T) {
		input := "### Warnings\n- **[main.go:5]** Unused import.\n"
		findings := ParseReviewFindings(input)
		if reviewOutputLooksMalformed(input, findings) {
			t.Fatal("structured warning output should not be treated as malformed")
		}
	})
}

// ---------------------------------------------------------------------------
// injectReReviewPhase
// ---------------------------------------------------------------------------

func Test_injectReReviewPhase_BasicInjection(t *testing.T) {
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		Persona:                "staff-code-reviewer",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
		ReviewIteration:        0,
		ModelTier:              "work",
	}
	fixPhase := &core.Phase{
		ID:              "phase-3",
		Name:            "fix",
		ReviewIteration: 1,
		Status:          core.StatusPending,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase, fixPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase
	e.phases["phase-3"] = fixPhase

	reReview := e.injectReReviewPhase(reviewPhase, fixPhase)
	if reReview == nil {
		t.Fatal("expected re-review phase, got nil")
	}
	if len(e.plan.Phases) != 4 {
		t.Fatalf("expected 4 phases after injection, got %d", len(e.plan.Phases))
	}
	if reReview.PersonaSelectionMethod != core.SelectionRequiredReview {
		t.Errorf("PersonaSelectionMethod = %q, want %q", reReview.PersonaSelectionMethod, core.SelectionRequiredReview)
	}
	if reReview.Persona != reviewPhase.Persona {
		t.Errorf("Persona = %q, want %q", reReview.Persona, reviewPhase.Persona)
	}
	if reReview.Role != core.RoleReviewer {
		t.Errorf("Role = %q, want %q", reReview.Role, core.RoleReviewer)
	}
	if len(reReview.Dependencies) != 1 || reReview.Dependencies[0] != fixPhase.ID {
		t.Errorf("Dependencies = %v, want [%s]", reReview.Dependencies, fixPhase.ID)
	}
	if reReview.ReviewIteration != fixPhase.ReviewIteration {
		t.Errorf("ReviewIteration = %d, want %d", reReview.ReviewIteration, fixPhase.ReviewIteration)
	}
	if reReview.MaxReviewLoops != reviewPhase.MaxReviewLoops {
		t.Errorf("MaxReviewLoops = %d, want %d", reReview.MaxReviewLoops, reviewPhase.MaxReviewLoops)
	}
	if reReview.OriginPhaseID != reviewPhase.ID {
		t.Errorf("OriginPhaseID = %q, want %q", reReview.OriginPhaseID, reviewPhase.ID)
	}
	if reReview.Status != core.StatusPending {
		t.Errorf("Status = %q, want pending", reReview.Status)
	}
}

func Test_injectReReviewPhase_ObjectiveContainsIterationInfo(t *testing.T) {
	reviewPhase := &core.Phase{
		ID:             "phase-2",
		Persona:        "staff-code-reviewer",
		MaxReviewLoops: 3,
	}
	fixPhase := &core.Phase{ID: "phase-3", ReviewIteration: 2, Status: core.StatusPending}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{reviewPhase, fixPhase}

	reReview := e.injectReReviewPhase(reviewPhase, fixPhase)
	if reReview == nil {
		t.Fatal("expected re-review phase")
	}
	if !strings.Contains(reReview.Objective, "2") {
		t.Errorf("objective should mention fix iteration 2: %q", reReview.Objective)
	}
	if !strings.Contains(reReview.Objective, "### Blockers") {
		t.Errorf("objective should reference structured output format: %q", reReview.Objective)
	}
}

func Test_injectReReviewPhase_PreservesReviewerPersona(t *testing.T) {
	reviewPhase := &core.Phase{
		ID:             "phase-2",
		Name:           "review-runtime-state",
		Objective:      "Review runtime state reliability and event degradation behavior",
		Persona:        "staff-code-reviewer",
		MaxReviewLoops: 2,
		ModelTier:      "work",
	}
	fixPhase := &core.Phase{ID: "phase-3", ReviewIteration: 1, Status: core.StatusPending}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{reviewPhase, fixPhase}

	reReview := e.injectReReviewPhase(reviewPhase, fixPhase)
	if reReview == nil {
		t.Fatal("expected re-review phase")
	}
	if reReview.Persona != "staff-code-reviewer" {
		t.Fatalf("re-review Persona = %q, want staff-code-reviewer", reReview.Persona)
	}
}

// ---------------------------------------------------------------------------
// handleReviewLoop — re-review injection
// ---------------------------------------------------------------------------

func TestHandleReviewLoop_InjectsReReviewAfterFix(t *testing.T) {
	implPhase := &core.Phase{
		ID:      "phase-1",
		Persona: "senior-backend-engineer",
		Status:  core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		Persona:                "staff-code-reviewer",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	output := "### Blockers\n- **[store.go:10]** Missing nil check.\n"
	fixPhase := e.handleReviewLoop(context.Background(), reviewPhase, output)
	if fixPhase == nil {
		t.Fatal("expected fix phase from handleReviewLoop")
	}

	// Plan must now have: impl + review + fix + re-review = 4 phases.
	if len(e.plan.Phases) != 4 {
		t.Fatalf("expected 4 phases (impl, review, fix, re-review), got %d", len(e.plan.Phases))
	}

	reReview := e.plan.Phases[3]
	if reReview.PersonaSelectionMethod != core.SelectionRequiredReview {
		t.Errorf("re-review PersonaSelectionMethod = %q, want %q", reReview.PersonaSelectionMethod, core.SelectionRequiredReview)
	}
	if len(reReview.Dependencies) != 1 || reReview.Dependencies[0] != fixPhase.ID {
		t.Errorf("re-review Dependencies = %v, want [%s]", reReview.Dependencies, fixPhase.ID)
	}
	// Re-review must be indexed so the sequential loop can dispatch it.
	if _, ok := e.phases[reReview.ID]; !ok {
		t.Errorf("re-review phase %s not found in e.phases index", reReview.ID)
	}
}

func TestHandleReviewLoop_NoReReviewWhenPassedReview(t *testing.T) {
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	// No blockers — review passes; no fix, no re-review should be injected.
	output := "### Warnings\n- **[util.go:3]** Missing godoc.\n"
	fix := e.handleReviewLoop(context.Background(), reviewPhase, output)
	if fix != nil {
		t.Fatal("expected nil fix when review passes")
	}
	if len(e.plan.Phases) != 2 {
		t.Fatalf("expected plan unchanged at 2 phases, got %d", len(e.plan.Phases))
	}
}

func TestHandleReviewLoop_NoReReviewWhenLoopExhausted(t *testing.T) {
	// At max iteration: injectFixPhase returns nil, so injectReReviewPhase is never called.
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
		ReviewIteration:        1, // already at max
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	output := "### Blockers\n- **[api.go:5]** Still broken.\n"
	fix := e.handleReviewLoop(context.Background(), reviewPhase, output)
	if fix != nil {
		t.Fatal("expected nil fix when loop exhausted")
	}
	// No extra phases should be appended.
	if len(e.plan.Phases) != 2 {
		t.Fatalf("expected plan unchanged at 2 phases, got %d", len(e.plan.Phases))
	}
}

// ---------------------------------------------------------------------------
// emitReviewFindings — non-blocking findings persistence via events
// ---------------------------------------------------------------------------

func TestHandleReviewLoop_EmitsReviewFindingsOnPass(t *testing.T) {
	em := &captureEmitter{}
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.emitter = em
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	// Only warnings, no blockers — review passes.
	output := "### Warnings\n- **[main.go:5]** Unused import.\n"
	e.handleReviewLoop(context.Background(), reviewPhase, output)

	evts := em.collected()
	var findingsEvt *event.Event
	for i := range evts {
		if evts[i].Type == event.ReviewFindingsEmitted {
			findingsEvt = &evts[i]
			break
		}
	}
	if findingsEvt == nil {
		t.Fatal("expected review.findings_emitted event, got none")
	}
	if passed, _ := findingsEvt.Data["passed"].(bool); !passed {
		t.Errorf("findings event passed = false, want true")
	}
	if wc, _ := findingsEvt.Data["warning_count"].(int); wc != 1 {
		t.Errorf("warning_count = %v, want 1", findingsEvt.Data["warning_count"])
	}
	if bc, _ := findingsEvt.Data["blocker_count"].(int); bc != 0 {
		t.Errorf("blocker_count = %v, want 0", findingsEvt.Data["blocker_count"])
	}
	if len(reviewPhase.ReviewWarnings) != 1 {
		t.Fatalf("persisted ReviewWarnings = %v, want 1 warning", reviewPhase.ReviewWarnings)
	}
	if len(reviewPhase.ReviewBlockers) != 0 {
		t.Fatalf("persisted ReviewBlockers = %v, want none", reviewPhase.ReviewBlockers)
	}
}

func TestHandleReviewLoop_EmitsReviewFindingsWithBlockersAndWarnings(t *testing.T) {
	em := &captureEmitter{}
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
	}

	e := newTestEngine()
	e.emitter = em
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	output := "### Blockers\n- **[store.go:10]** Missing nil check.\n### Warnings\n- **[util.go:3]** Godoc missing.\n"
	e.handleReviewLoop(context.Background(), reviewPhase, output)

	evts := em.collected()
	var findingsEvt *event.Event
	for i := range evts {
		if evts[i].Type == event.ReviewFindingsEmitted {
			findingsEvt = &evts[i]
			break
		}
	}
	if findingsEvt == nil {
		t.Fatal("expected review.findings_emitted event")
	}
	if bc, _ := findingsEvt.Data["blocker_count"].(int); bc != 1 {
		t.Errorf("blocker_count = %v, want 1", findingsEvt.Data["blocker_count"])
	}
	if wc, _ := findingsEvt.Data["warning_count"].(int); wc != 1 {
		t.Errorf("warning_count = %v, want 1", findingsEvt.Data["warning_count"])
	}
	if passed, _ := findingsEvt.Data["passed"].(bool); passed {
		t.Errorf("findings event passed = true, want false (blockers present)")
	}
	blockers, _ := findingsEvt.Data["blockers"].([]string)
	if len(blockers) != 1 || !strings.Contains(blockers[0], "store.go:10") {
		t.Errorf("blockers payload = %v, expected entry with store.go:10", blockers)
	}
	warnings, _ := findingsEvt.Data["warnings"].([]string)
	if len(warnings) != 1 || !strings.Contains(warnings[0], "util.go:3") {
		t.Errorf("warnings payload = %v, expected entry with util.go:3", warnings)
	}
	if len(reviewPhase.ReviewBlockers) != 1 || len(reviewPhase.ReviewWarnings) != 1 {
		t.Fatalf("persisted findings = blockers:%v warnings:%v, want 1 each", reviewPhase.ReviewBlockers, reviewPhase.ReviewWarnings)
	}
}

func TestHandleReviewLoop_EmitsUnresolvedBlockersAtLoopExhaustion(t *testing.T) {
	// At max iteration with remaining blockers: findings event must still fire
	// so unresolved blockers are not silently discarded.
	em := &captureEmitter{}
	implPhase := &core.Phase{ID: "phase-1", Persona: "senior-backend-engineer", Status: core.StatusCompleted}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         1,
		ReviewIteration:        1, // exhausted
	}

	e := newTestEngine()
	e.emitter = em
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	output := "### Blockers\n- **[api.go:42]** Unresolved race condition.\n"
	e.handleReviewLoop(context.Background(), reviewPhase, output)

	evts := em.collected()
	var findingsEvt *event.Event
	for i := range evts {
		if evts[i].Type == event.ReviewFindingsEmitted {
			findingsEvt = &evts[i]
			break
		}
	}
	if findingsEvt == nil {
		t.Fatal("expected review.findings_emitted even when loop exhausted")
	}
	if bc, _ := findingsEvt.Data["blocker_count"].(int); bc != 1 {
		t.Errorf("blocker_count = %v, want 1", findingsEvt.Data["blocker_count"])
	}
	if len(reviewPhase.ReviewBlockers) != 1 {
		t.Fatalf("persisted ReviewBlockers = %v, want unresolved blocker recorded", reviewPhase.ReviewBlockers)
	}
}

// ---------------------------------------------------------------------------
// Full bounded autonomous loop: multi-iteration end-to-end shape
// ---------------------------------------------------------------------------

func TestHandleReviewLoop_BoundedLoopShape_MaxLoops2(t *testing.T) {
	// Verify the phase-graph shape created by two successive handleReviewLoop
	// calls matches the expected bounded-loop pattern:
	//   impl → review(0) → fix(1) → re-review(1) → fix(2) → re-review(2)
	// After re-review(2) with blockers, loop is exhausted (iter 2 == max 2).

	implPhase := &core.Phase{
		ID:      "phase-1",
		Persona: "senior-backend-engineer",
		Status:  core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		Persona:                "staff-code-reviewer",
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
		ReviewIteration:        0,
	}

	e := newTestEngine()
	e.plan.Phases = []*core.Phase{implPhase, reviewPhase}
	e.phases["phase-1"] = implPhase
	e.phases["phase-2"] = reviewPhase

	blockerOutput := "### Blockers\n- **[engine.go:10]** Race condition.\n"

	// First review cycle: review(0) has blockers → inject fix(1) + re-review(1).
	fix1 := e.handleReviewLoop(context.Background(), reviewPhase, blockerOutput)
	if fix1 == nil {
		t.Fatal("cycle 1: expected fix phase")
	}
	if fix1.ReviewIteration != 1 {
		t.Errorf("fix1.ReviewIteration = %d, want 1", fix1.ReviewIteration)
	}
	e.phases[fix1.ID] = fix1 // simulate executeSequential registering the fix

	if len(e.plan.Phases) != 4 {
		t.Fatalf("after cycle 1: expected 4 phases, got %d", len(e.plan.Phases))
	}
	reReview1 := e.plan.Phases[3]
	if reReview1.PersonaSelectionMethod != core.SelectionRequiredReview {
		t.Errorf("re-review1: PersonaSelectionMethod = %q, want %q", reReview1.PersonaSelectionMethod, core.SelectionRequiredReview)
	}
	if reReview1.ReviewIteration != 1 {
		t.Errorf("re-review1.ReviewIteration = %d, want 1", reReview1.ReviewIteration)
	}

	// Second review cycle: re-review(1) has blockers → inject fix(2) + re-review(2).
	fix2 := e.handleReviewLoop(context.Background(), reReview1, blockerOutput)
	if fix2 == nil {
		t.Fatal("cycle 2: expected second fix phase")
	}
	if fix2.ReviewIteration != 2 {
		t.Errorf("fix2.ReviewIteration = %d, want 2", fix2.ReviewIteration)
	}
	e.phases[fix2.ID] = fix2

	if len(e.plan.Phases) != 6 {
		t.Fatalf("after cycle 2: expected 6 phases, got %d", len(e.plan.Phases))
	}
	reReview2 := e.plan.Phases[5]
	if reReview2.ReviewIteration != 2 {
		t.Errorf("re-review2.ReviewIteration = %d, want 2", reReview2.ReviewIteration)
	}

	// Third call: re-review(2) with blockers → loop exhausted (iter 2 == max 2).
	fix3 := e.handleReviewLoop(context.Background(), reReview2, blockerOutput)
	if fix3 != nil {
		t.Fatal("cycle 3: expected nil — loop should be exhausted at MaxReviewLoops=2")
	}
	// No additional phases should be appended.
	if len(e.plan.Phases) != 6 {
		t.Fatalf("after exhaustion: expected still 6 phases, got %d", len(e.plan.Phases))
	}
}

// ---------------------------------------------------------------------------
// Test helpers for RuntimeBoth tests
// ---------------------------------------------------------------------------

// fakeExecutor is a PhaseExecutor stub whose output is controlled by outputFn.
// If err is non-nil, Execute returns that error.
type fakeExecutor struct {
	outputFn func() string
	err      error
}

func (f *fakeExecutor) Execute(_ context.Context, _ *core.WorkerConfig, _ event.Emitter, _ bool) (string, string, *sdk.CostInfo, error) {
	if f.err != nil {
		return "", "", nil, f.err
	}
	if f.outputFn != nil {
		return f.outputFn(), "", nil, nil
	}
	return "", "", nil, nil
}

// recordingEmitter records every event emitted through it.
type recordingEmitter struct {
	fn func(event.Event)
}

func (r *recordingEmitter) Emit(_ context.Context, e event.Event) {
	if r.fn != nil {
		r.fn(e)
	}
}

func (r *recordingEmitter) Close() error { return nil }

// ---------------------------------------------------------------------------
// mergeReviewFindings
// ---------------------------------------------------------------------------

func TestMergeReviewFindings_DedupsIdenticalBlockers(t *testing.T) {
	a := ReviewFindings{
		Blockers: []ReviewItem{{Location: "store.go:42", Description: "Missing error check on db.Query."}},
	}
	b := ReviewFindings{
		Blockers: []ReviewItem{{Location: "store.go:42", Description: "Missing error check on db.Query."}},
	}
	merged := mergeReviewFindings(a, b)
	if len(merged.Blockers) != 1 {
		t.Fatalf("expected 1 blocker after dedup, got %d", len(merged.Blockers))
	}
}

func TestMergeReviewFindings_KeepsLongerDescriptionOnDup(t *testing.T) {
	a := ReviewFindings{
		Blockers: []ReviewItem{{Location: "store.go:42", Description: "Missing error check."}},
	}
	b := ReviewFindings{
		Blockers: []ReviewItem{{Location: "store.go:42", Description: "Missing error check on db.Query — wrap and return."}},
	}
	merged := mergeReviewFindings(a, b)
	if len(merged.Blockers) != 1 {
		t.Fatalf("expected 1 blocker after dedup, got %d", len(merged.Blockers))
	}
	if !strings.Contains(merged.Blockers[0].Description, "db.Query") {
		t.Errorf("expected longer description to win, got: %q", merged.Blockers[0].Description)
	}
}

func TestMergeReviewFindings_KeepsDistinctLocations(t *testing.T) {
	a := ReviewFindings{
		Blockers: []ReviewItem{{Location: "store.go:42", Description: "Missing error check."}},
	}
	b := ReviewFindings{
		Blockers: []ReviewItem{{Location: "handler.go:10", Description: "Missing error check."}},
	}
	merged := mergeReviewFindings(a, b)
	if len(merged.Blockers) != 2 {
		t.Fatalf("expected 2 blockers for distinct locations, got %d", len(merged.Blockers))
	}
}

func TestMergeReviewFindings_UnionsWarnings(t *testing.T) {
	a := ReviewFindings{
		Warnings: []ReviewItem{{Location: "foo.go:1", Description: "Exported func missing godoc."}},
	}
	b := ReviewFindings{
		Warnings: []ReviewItem{{Location: "bar.go:2", Description: "Shadowed variable."}},
	}
	merged := mergeReviewFindings(a, b)
	if len(merged.Warnings) != 2 {
		t.Fatalf("expected 2 warnings (union), got %d", len(merged.Warnings))
	}
}

func TestMergeReviewFindings_EmptyCodexPassthrough(t *testing.T) {
	a := ReviewFindings{
		Blockers: []ReviewItem{{Location: "store.go:42", Description: "Missing error check."}},
		Warnings: []ReviewItem{{Location: "foo.go:1", Description: "Godoc missing."}},
	}
	b := ReviewFindings{}
	merged := mergeReviewFindings(a, b)
	if len(merged.Blockers) != 1 {
		t.Fatalf("expected 1 blocker, got %d", len(merged.Blockers))
	}
	if len(merged.Warnings) != 1 {
		t.Fatalf("expected 1 warning, got %d", len(merged.Warnings))
	}
}

func TestMergeReviewFindings_SubstringDescriptionDedup(t *testing.T) {
	// One description is a substring of the other at the same location.
	a := ReviewFindings{
		Blockers: []ReviewItem{{Location: "cmd.go:15", Description: "Nil pointer dereference."}},
	}
	b := ReviewFindings{
		Blockers: []ReviewItem{{Location: "cmd.go:15", Description: "Nil pointer dereference on response.Body — add nil guard."}},
	}
	merged := mergeReviewFindings(a, b)
	if len(merged.Blockers) != 1 {
		t.Fatalf("expected 1 blocker (substring dedup), got %d: %v", len(merged.Blockers), merged.Blockers)
	}
}

func TestMergeReviewFindings_WordOverlapDedup(t *testing.T) {
	// Same location, descriptions share >50% non-trivial words.
	a := ReviewFindings{
		Blockers: []ReviewItem{{Location: "api.go:99", Description: "database connection not closed properly after query execution"}},
	}
	b := ReviewFindings{
		Blockers: []ReviewItem{{Location: "api.go:99", Description: "connection not closed after query — resource leak"}},
	}
	merged := mergeReviewFindings(a, b)
	if len(merged.Blockers) != 1 {
		t.Fatalf("expected 1 blocker (word overlap dedup), got %d", len(merged.Blockers))
	}
}

// ---------------------------------------------------------------------------
// handleReviewLoop — RuntimeBoth merges findings from both executors
// ---------------------------------------------------------------------------

func TestHandleReviewLoop_RuntimeBoth_MergesFindings(t *testing.T) {
	// Claude output has 1 blocker; Codex output has a different blocker.
	// Merged findings should have 2 blockers and drive fix injection.
	claudeOutput := `
### Blockers
- **[store.go:42]** Missing error check.

### Warnings
`
	codexOutput := `
### Blockers
- **[handler.go:10]** Nil pointer on empty response.

### Warnings
`

	implPhase := &core.Phase{
		ID:      "phase-1",
		Name:    "implement",
		Role:    core.RoleImplementer,
		Persona: "senior-backend-engineer",
		Status:  core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		Role:                   core.RoleReviewer,
		Persona:                "staff-code-reviewer",
		Runtime:                core.RuntimeBoth,
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}

	capturedOutput := codexOutput
	fakeCodex := &fakeExecutor{outputFn: func() string { return capturedOutput }}

	e := &Engine{
		plan:      &core.Plan{Task: "build something", Phases: []*core.Phase{implPhase, reviewPhase}},
		phases:    map[string]*core.Phase{"phase-1": implPhase, "phase-2": reviewPhase},
		emitter:   event.NoOpEmitter{},
		config:    &core.OrchestratorConfig{},
		workspace: &core.Workspace{Path: t.TempDir()},
		executors: executorRegistry{
			core.RuntimeClaude: ClaudeExecutor{},
			core.RuntimeCodex:  fakeCodex,
		},
	}

	fix := e.handleReviewLoop(context.Background(), reviewPhase, claudeOutput, "")
	if fix == nil {
		t.Fatal("expected fix phase to be injected for merged blockers")
	}
	if !strings.Contains(fix.Objective, "store.go") && !strings.Contains(fix.Objective, "handler.go") {
		t.Errorf("fix objective should reference merged blockers, got: %q", fix.Objective)
	}
}

func TestHandleReviewLoop_RuntimeBoth_DedupsBlockers(t *testing.T) {
	// Both executors find the same blocker — only one should survive dedup.
	sharedOutput := `
### Blockers
- **[store.go:42]** Missing error check on db.Query.

### Warnings
`
	implPhase := &core.Phase{
		ID:      "phase-1",
		Name:    "implement",
		Role:    core.RoleImplementer,
		Persona: "senior-backend-engineer",
		Status:  core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		Role:                   core.RoleReviewer,
		Persona:                "staff-code-reviewer",
		Runtime:                core.RuntimeBoth,
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}

	fakeCodex := &fakeExecutor{outputFn: func() string { return sharedOutput }}

	var emittedData []map[string]any
	rec := &recordingEmitter{fn: func(e event.Event) {
		emittedData = append(emittedData, e.Data)
	}}

	eng := &Engine{
		plan:      &core.Plan{Task: "build something", Phases: []*core.Phase{implPhase, reviewPhase}},
		phases:    map[string]*core.Phase{"phase-1": implPhase, "phase-2": reviewPhase},
		emitter:   rec,
		config:    &core.OrchestratorConfig{},
		workspace: &core.Workspace{Path: t.TempDir()},
		executors: executorRegistry{
			core.RuntimeClaude: ClaudeExecutor{},
			core.RuntimeCodex:  fakeCodex,
		},
	}

	eng.handleReviewLoop(context.Background(), reviewPhase, sharedOutput, "")

	// Find the findings event and verify blockers are deduped to 1.
	for _, d := range emittedData {
		if count, ok := d["blocker_count"]; ok {
			if count.(int) != 1 {
				t.Errorf("expected 1 blocker after dedup, got %v", count)
			}
			return
		}
	}
	t.Error("no blocker_count found in emitted events")
}

func TestHandleReviewLoop_RuntimeBoth_CodexFailureFallback(t *testing.T) {
	// CodexExecutor fails — handleReviewLoop should still work using Claude findings alone.
	claudeOutput := `
### Blockers
- **[store.go:42]** Missing error check.

### Warnings
`
	implPhase := &core.Phase{
		ID:      "phase-1",
		Name:    "implement",
		Role:    core.RoleImplementer,
		Persona: "senior-backend-engineer",
		Status:  core.StatusCompleted,
	}
	reviewPhase := &core.Phase{
		ID:                     "phase-2",
		Name:                   "review",
		Role:                   core.RoleReviewer,
		Persona:                "staff-code-reviewer",
		Runtime:                core.RuntimeBoth,
		PersonaSelectionMethod: core.SelectionRequiredReview,
		Dependencies:           []string{"phase-1"},
		MaxReviewLoops:         2,
	}

	fakeCodex := &fakeExecutor{err: fmt.Errorf("codex unavailable")}

	e := &Engine{
		plan:      &core.Plan{Task: "build something", Phases: []*core.Phase{implPhase, reviewPhase}},
		phases:    map[string]*core.Phase{"phase-1": implPhase, "phase-2": reviewPhase},
		emitter:   event.NoOpEmitter{},
		config:    &core.OrchestratorConfig{},
		workspace: &core.Workspace{Path: t.TempDir()},
		executors: executorRegistry{
			core.RuntimeClaude: ClaudeExecutor{},
			core.RuntimeCodex:  fakeCodex,
		},
	}

	fix := e.handleReviewLoop(context.Background(), reviewPhase, claudeOutput, "")
	if fix == nil {
		t.Fatal("expected fix phase from Claude findings when Codex fails")
	}
}
