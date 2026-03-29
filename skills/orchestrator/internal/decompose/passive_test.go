package decompose

import (
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// makePhase is a test helper that creates a Phase with the given name, persona,
// and selection method.
func makePhase(name, persona, method string) *core.Phase {
	return &core.Phase{
		ID:                     name,
		Name:                   name,
		Objective:              "test objective for " + name,
		Persona:                persona,
		PersonaSelectionMethod: method,
		Status:                 core.StatusPending,
	}
}

// makePlan creates a Plan for testing.
func makePlan(task string, phases ...*core.Phase) *core.Plan {
	return &core.Plan{
		ID:            "plan-test",
		Task:          task,
		Phases:        phases,
		ExecutionMode: "sequential",
	}
}

// repoTC returns a minimal TargetContext for a repo target.
func repoTC(targetID string) *TargetContext {
	return &TargetContext{TargetID: targetID}
}

func TestHasCodeReviewPhase_RequiresCodeReviewCapability(t *testing.T) {
	if hasCodeReviewPhase([]*core.Phase{makePhase("review", "security-auditor", "llm")}) {
		t.Fatal("security-auditor should not satisfy code-review phase detection without code review capability")
	}
	if !hasCodeReviewPhase([]*core.Phase{makePhase("review", "staff-code-reviewer", "llm")}) {
		t.Fatal("staff-code-reviewer should satisfy code-review phase detection")
	}
}

func TestAuditPlan_NilPlan(t *testing.T) {
	findings := AuditPlan(nil, nil)
	if findings == nil {
		t.Error("AuditPlan(nil, nil) returned nil; want empty slice")
	}
	if len(findings) != 0 {
		t.Errorf("AuditPlan(nil, nil) = %d findings; want 0", len(findings))
	}
}

func TestAuditPlan_NilTC_NoPanic(t *testing.T) {
	plan := makePlan("do something",
		makePhase("p1", "senior-backend-engineer", "keyword"),
		makePhase("p2", "staff-code-reviewer", "llm"),
	)
	// Must not panic when tc is nil.
	findings := AuditPlan(plan, nil)
	_ = findings
}

func TestAuditPlan_LowConfidence(t *testing.T) {
	tests := []struct {
		name    string
		phases  []*core.Phase
		wantHit bool
	}{
		{
			name: "all keyword — fires",
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "keyword"),
				makePhase("p2", "staff-code-reviewer", "keyword"),
			},
			wantHit: true,
		},
		{
			name: "all fallback — fires",
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "fallback"),
			},
			wantHit: true,
		},
		{
			name: "one llm — does not fire",
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "keyword"),
				makePhase("p2", "staff-code-reviewer", "llm"),
			},
			wantHit: false,
		},
		{
			name: "target_profile method — does not fire",
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "target_profile"),
			},
			wantHit: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			plan := makePlan("implement something", tt.phases...)
			findings := AuditPlan(plan, nil)
			hit := hasType(findings, PassiveLowConfidence)
			if hit != tt.wantHit {
				t.Errorf("LowConfidence fired=%v, want %v", hit, tt.wantHit)
			}
		})
	}
}

func TestAuditPlan_PhaseCountAnomaly(t *testing.T) {
	tests := []struct {
		name    string
		tc      *TargetContext
		phases  []*core.Phase
		wantHit bool
	}{
		{
			name:    "single phase with tc — fires",
			tc:      repoTC("repo:~/skills/orchestrator"),
			phases:  []*core.Phase{makePhase("p1", "senior-backend-engineer", "llm")},
			wantHit: true,
		},
		{
			name:    "single phase without tc — does not fire",
			tc:      nil,
			phases:  []*core.Phase{makePhase("p1", "senior-backend-engineer", "llm")},
			wantHit: false,
		},
		{
			name: "11 phases — fires",
			tc:   nil,
			phases: func() []*core.Phase {
				var ps []*core.Phase
				for i := 0; i < 11; i++ {
					ps = append(ps, makePhase("p", "senior-backend-engineer", "llm"))
				}
				return ps
			}(),
			wantHit: true,
		},
		{
			name: "10 phases — does not fire",
			tc:   nil,
			phases: func() []*core.Phase {
				var ps []*core.Phase
				for i := 0; i < 10; i++ {
					ps = append(ps, makePhase("p", "senior-backend-engineer", "llm"))
				}
				return ps
			}(),
			wantHit: false,
		},
		{
			name: "4 phases — does not fire",
			tc:   repoTC("repo:~/skills/orchestrator"),
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "senior-backend-engineer", "llm"),
				makePhase("p4", "staff-code-reviewer", "llm"),
			},
			wantHit: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			plan := makePlan("build a feature", tt.phases...)
			findings := AuditPlan(plan, tt.tc)
			hit := hasType(findings, PassivePhaseCountAnomaly)
			if hit != tt.wantHit {
				t.Errorf("PhaseCountAnomaly fired=%v, want %v", hit, tt.wantHit)
			}
		})
	}
}

func TestAuditPlan_AllSamePersona(t *testing.T) {
	tests := []struct {
		name    string
		phases  []*core.Phase
		wantHit bool
	}{
		{
			name: "4 phases same persona — fires",
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "senior-backend-engineer", "llm"),
				makePhase("p4", "senior-backend-engineer", "llm"),
			},
			wantHit: true,
		},
		{
			name: "3 phases same persona — does not fire (threshold is > 3)",
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "senior-backend-engineer", "llm"),
			},
			wantHit: false,
		},
		{
			name: "4 phases mixed personas — does not fire",
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "senior-backend-engineer", "llm"),
				makePhase("p4", "staff-code-reviewer", "llm"),
			},
			wantHit: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			plan := makePlan("build something", tt.phases...)
			findings := AuditPlan(plan, nil)
			hit := hasType(findings, PassiveAllSamePersona)
			if hit != tt.wantHit {
				t.Errorf("AllSamePersona fired=%v, want %v", hit, tt.wantHit)
			}
		})
	}
}

func TestAuditPlan_MissingCodeReview(t *testing.T) {
	tests := []struct {
		name    string
		task    string
		tc      *TargetContext
		phases  []*core.Phase
		wantHit bool
	}{
		{
			name: "repo+implement+3phases+no reviewer — fires",
			task: "implement new feature",
			tc:   repoTC("repo:~/skills/orchestrator"),
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "qa-engineer", "llm"),
			},
			wantHit: true,
		},
		{
			name: "repo+implement+has code reviewer — does not fire",
			task: "implement new feature",
			tc:   repoTC("repo:~/skills/orchestrator"),
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "staff-code-reviewer", "llm"),
			},
			wantHit: false,
		},
		{
			name: "repo+implement+has only security-auditor — still fires",
			task: "build authentication system",
			tc:   repoTC("repo:~/skills/orchestrator"),
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "security-auditor", "llm"),
			},
			wantHit: true,
		},
		{
			name: "non-repo target — does not fire",
			task: "implement new feature",
			tc:   &TargetContext{TargetID: "workspace:abc123"},
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "qa-engineer", "llm"),
			},
			wantHit: false,
		},
		{
			name: "repo+only 2 phases — still fires",
			task: "implement new feature",
			tc:   repoTC("repo:~/skills/orchestrator"),
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "qa-engineer", "llm"),
			},
			wantHit: true,
		},
		{
			name: "repo+no implement keyword — does not fire",
			task: "research Go testing patterns",
			tc:   repoTC("repo:~/skills/orchestrator"),
			phases: []*core.Phase{
				makePhase("p1", "academic-researcher", "llm"),
				makePhase("p2", "academic-researcher", "llm"),
				makePhase("p3", "academic-researcher", "llm"),
			},
			wantHit: false,
		},
		{
			name: "nil tc — does not fire",
			task: "implement new feature",
			tc:   nil,
			phases: []*core.Phase{
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "qa-engineer", "llm"),
			},
			wantHit: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			plan := makePlan(tt.task, tt.phases...)
			findings := AuditPlan(plan, tt.tc)
			hit := hasType(findings, PassiveMissingCodeReview)
			if hit != tt.wantHit {
				t.Errorf("MissingCodeReview fired=%v, want %v; findings=%v", hit, tt.wantHit, findings)
			}
		})
	}
}

func TestAuditPlan_MultipleFindings(t *testing.T) {
	// A plan that triggers LowConfidence + AllSamePersona + MissingCodeReview
	// simultaneously. Verifies that rules compose and don't short-circuit.
	plan := makePlan("implement authentication",
		makePhase("p1", "senior-backend-engineer", "keyword"),
		makePhase("p2", "senior-backend-engineer", "keyword"),
		makePhase("p3", "senior-backend-engineer", "fallback"),
		makePhase("p4", "senior-backend-engineer", "keyword"),
	)
	tc := repoTC("repo:~/skills/orchestrator")

	findings := AuditPlan(plan, tc)

	want := map[string]bool{
		PassiveLowConfidence:     true,
		PassiveAllSamePersona:    true,
		PassiveMissingCodeReview: true,
	}

	got := make(map[string]bool)
	for _, f := range findings {
		got[f.FindingType] = true
	}

	for typ, expected := range want {
		if got[typ] != expected {
			t.Errorf("finding %q: got=%v, want=%v", typ, got[typ], expected)
		}
	}

	// PhaseCountAnomaly should NOT fire (4 phases is normal).
	if got[PassivePhaseCountAnomaly] {
		t.Error("PhaseCountAnomaly should not fire for 4 phases")
	}
}

func TestAuditPlan_EmptyPlan(t *testing.T) {
	plan := &core.Plan{ID: "x", Task: "do it"}
	findings := AuditPlan(plan, nil)
	if len(findings) != 0 {
		t.Errorf("empty plan produced %d findings; want 0", len(findings))
	}
}

// ─── Additional edge-case tests ──────────────────────────────────────────────

func TestAuditPlan_AllSamePersona_EmptyPersona(t *testing.T) {
	// 4 phases all with empty persona string — should NOT fire
	// because passive.go checks `all && first != ""`
	plan := makePlan("build something",
		makePhase("p1", "", "llm"),
		makePhase("p2", "", "llm"),
		makePhase("p3", "", "llm"),
		makePhase("p4", "", "llm"),
	)
	findings := AuditPlan(plan, nil)
	if hasType(findings, PassiveAllSamePersona) {
		t.Error("AllSamePersona should not fire when persona is empty")
	}
}

func TestAuditPlan_MissingCodeReview_CaseInsensitive(t *testing.T) {
	// Task has mixed case implementation keyword — should still fire
	plan := makePlan("IMPLEMENT new database schema",
		makePhase("p1", "senior-backend-engineer", "llm"),
		makePhase("p2", "senior-backend-engineer", "llm"),
		makePhase("p3", "qa-engineer", "llm"),
	)
	tc := repoTC("repo:~/skills/orchestrator")
	findings := AuditPlan(plan, tc)
	if !hasType(findings, PassiveMissingCodeReview) {
		t.Error("MissingCodeReview should fire for case-insensitive keyword match")
	}
}

func TestAuditPlan_MissingCodeReview_AllKeywords(t *testing.T) {
	// Test that all implementKeywords are recognized
	for _, kw := range implementKeywords {
		t.Run(kw, func(t *testing.T) {
			plan := makePlan("please "+kw+" the thing",
				makePhase("p1", "senior-backend-engineer", "llm"),
				makePhase("p2", "senior-backend-engineer", "llm"),
				makePhase("p3", "qa-engineer", "llm"),
			)
			tc := repoTC("repo:~/test")
			findings := AuditPlan(plan, tc)
			if !hasType(findings, PassiveMissingCodeReview) {
				t.Errorf("MissingCodeReview should fire for keyword %q", kw)
			}
		})
	}
}

func TestAuditPlan_PhaseCountAnomaly_Exactly11(t *testing.T) {
	// Boundary: exactly 11 phases triggers (> 10)
	var phases []*core.Phase
	for i := 0; i < 11; i++ {
		phases = append(phases, makePhase("p", "senior-backend-engineer", "llm"))
	}
	plan := makePlan("build everything", phases...)
	findings := AuditPlan(plan, nil)
	if !hasType(findings, PassivePhaseCountAnomaly) {
		t.Error("PhaseCountAnomaly should fire for 11 phases")
	}
}

func TestAuditPlan_LowConfidence_EmptyMethod(t *testing.T) {
	// Empty PersonaSelectionMethod counts as low-confidence (see code: m != "keyword" && m != "fallback" && m != "")
	// Empty string passes the check, so all-empty should fire
	plan := makePlan("build feature",
		makePhase("p1", "senior-backend-engineer", ""),
		makePhase("p2", "staff-code-reviewer", ""),
	)
	findings := AuditPlan(plan, nil)
	if !hasType(findings, PassiveLowConfidence) {
		t.Error("LowConfidence should fire when all phases have empty selection method")
	}
}

func TestAuditPlan_LowConfidence_MixedEmptyAndKeyword(t *testing.T) {
	// All phases have empty or keyword — should fire
	plan := makePlan("build feature",
		makePhase("p1", "senior-backend-engineer", ""),
		makePhase("p2", "staff-code-reviewer", "keyword"),
	)
	findings := AuditPlan(plan, nil)
	if !hasType(findings, PassiveLowConfidence) {
		t.Error("LowConfidence should fire for mix of empty and keyword methods")
	}
}

// ─── Formatter tests ─────────────────────────────────────────────────────────

func TestFormatDecompInsights_WithDirectives(t *testing.T) {
	tc := &TargetContext{
		DecompInsights: []DecompInsight{
			{FindingType: findingMissingPhase, Detail: "testing phase", Count: 3},
			{FindingType: findingWrongPersona, Detail: "assigned backend for frontend work", Count: 2},
			{FindingType: "unknown_type", Detail: "something new", Count: 5},
		},
	}
	output := formatDecompInsights(tc)

	// Must contain the directive text, not just the finding
	if !containsStr(output, "Add a dedicated phase") {
		t.Error("missing_phase directive not found in output")
	}
	if !containsStr(output, "ideal persona named in the detail field") {
		t.Error("wrong_persona directive not found in output")
	}
	// Unknown type should get fallback directive
	if !containsStr(output, "Avoid repeating this pattern") {
		t.Error("fallback directive for unknown type not found")
	}
	// Must contain observation counts
	if !containsStr(output, "3 missions") {
		t.Error("observation count not found")
	}
}

func TestFormatHandoffPatterns_WithAndWithoutHint(t *testing.T) {
	tc := &TargetContext{
		HandoffHints: []HandoffHint{
			{FromPersona: "backend", ToPersona: "reviewer", Confidence: 0.8, TaskHint: "code review"},
			{FromPersona: "backend", ToPersona: "qa", Confidence: 0.4},
		},
	}
	output := formatHandoffPatterns(tc)

	if !containsStr(output, "backend → reviewer") {
		t.Error("handoff pattern A not found")
	}
	if !containsStr(output, "for: code review") {
		t.Error("task hint not found")
	}
	if !containsStr(output, "backend → qa") {
		t.Error("handoff pattern B not found")
	}
	if !containsStr(output, "Observed Handoff Patterns") {
		t.Error("section header not found")
	}
}

func TestFormatPlanShapeStats_Full(t *testing.T) {
	tc := &TargetContext{
		PlanShapeStats: &PlanShapeStats{
			AvgPhaseCount:  4.5,
			MostCommonMode: "sequential",
			TopPersonas:    []string{"senior-backend-engineer", "staff-code-reviewer", "qa-engineer"},
			ExampleCount:   7,
		},
	}
	output := formatPlanShapeStats(tc)

	if !containsStr(output, "4.5") {
		t.Error("average phase count not found")
	}
	if !containsStr(output, "sequential") {
		t.Error("execution mode not found")
	}
	if !containsStr(output, "senior-backend-engineer") {
		t.Error("top persona not found")
	}
	if !containsStr(output, "7 validated examples") {
		t.Error("example count not found")
	}
}

func TestFormatPlanShapeStats_MinimalFields(t *testing.T) {
	tc := &TargetContext{
		PlanShapeStats: &PlanShapeStats{
			AvgPhaseCount: 3.0,
			ExampleCount:  2,
			// MostCommonMode empty, TopPersonas empty
		},
	}
	output := formatPlanShapeStats(tc)

	if !containsStr(output, "3.0") {
		t.Error("average phase count not found")
	}
	// Should not contain "Typical execution mode:" when empty
	if containsStr(output, "Typical execution mode:") {
		t.Error("should not show execution mode when empty")
	}
}

func TestFormatSuccessfulShapes(t *testing.T) {
	tc := &TargetContext{
		SuccessfulShapes: []PhaseShapeHint{
			{PhaseCount: 4, ExecutionMode: "sequential",
				PersonaSeq: []string{"backend", "backend", "reviewer", "qa"}, SuccessCount: 5},
			{PhaseCount: 3, ExecutionMode: "parallel",
				PersonaSeq: []string{"backend", "frontend", "reviewer"}, SuccessCount: 3},
		},
	}
	output := formatSuccessfulShapes(tc)

	if !containsStr(output, "Proven Phase Shapes") {
		t.Error("section header not found")
	}
	if !containsStr(output, "4 phases") {
		t.Error("first shape phase count not found")
	}
	if !containsStr(output, "succeeded 5 times") {
		t.Error("first shape success count not found")
	}
	if !containsStr(output, "backend → backend → reviewer → qa") {
		t.Error("persona sequence not found")
	}
}

func TestFormatTargetContext_AllFields(t *testing.T) {
	tc := &TargetContext{
		TargetID:          "repo:~/skills/orchestrator",
		Language:          "go",
		Runtime:           "go",
		PreferredPersonas: []string{"senior-backend-engineer", "staff-code-reviewer"},
		Notes:             "main orchestrator CLI",
	}
	output := formatTargetContext(tc)

	if !containsStr(output, "repo:~/skills/orchestrator") {
		t.Error("target ID not found")
	}
	if !containsStr(output, "Language: go") {
		t.Error("language not found")
	}
	if !containsStr(output, "Runtime: go") {
		t.Error("runtime not found")
	}
	if !containsStr(output, "senior-backend-engineer, staff-code-reviewer") {
		t.Error("preferred personas not found")
	}
	if !containsStr(output, "main orchestrator CLI") {
		t.Error("notes not found")
	}
}

func TestFormatTargetContext_EmptyFields(t *testing.T) {
	tc := &TargetContext{TargetID: "repo:~/app"}
	output := formatTargetContext(tc)

	if !containsStr(output, "repo:~/app") {
		t.Error("target ID not found")
	}
	// Should not contain Language/Runtime/etc lines when empty
	if containsStr(output, "Language:") {
		t.Error("should not show Language when empty")
	}
	if containsStr(output, "Runtime:") {
		t.Error("should not show Runtime when empty")
	}
}

// ─── Insight directive constant integrity ────────────────────────────────────

func TestInsightDirectives_AllFindingTypesHaveDirectives(t *testing.T) {
	// All known finding types should have entries in insightDirectives.
	expectedTypes := []string{
		findingMissingPhase,
		findingRedundantPhase,
		findingPhaseDrift,
		findingWrongPersona,
		findingLowPhaseScore,
		PassiveLowConfidence,
		PassivePhaseCountAnomaly,
		PassiveAllSamePersona,
		PassiveMissingCodeReview,
	}
	for _, ft := range expectedTypes {
		if directive, ok := insightDirectives[ft]; !ok || directive == "" {
			t.Errorf("insightDirectives missing entry for %q", ft)
		}
	}
}

func TestInsightDirectives_MatchAuditConstants(t *testing.T) {
	// The local constants in decompose.go must match the values in audit/types.go.
	// These are DB-stable strings — if they drift, the directives map will silently
	// stop matching real findings.
	if findingMissingPhase != "missing_phase" {
		t.Errorf("findingMissingPhase = %q, want missing_phase", findingMissingPhase)
	}
	if findingRedundantPhase != "redundant_phase" {
		t.Errorf("findingRedundantPhase = %q, want redundant_phase", findingRedundantPhase)
	}
	if findingPhaseDrift != "phase_drift" {
		t.Errorf("findingPhaseDrift = %q, want phase_drift", findingPhaseDrift)
	}
	if findingWrongPersona != "wrong_persona" {
		t.Errorf("findingWrongPersona = %q, want wrong_persona", findingWrongPersona)
	}
	if findingLowPhaseScore != "low_phase_score" {
		t.Errorf("findingLowPhaseScore = %q, want low_phase_score", findingLowPhaseScore)
	}
}

// ─── Under-decomposition heuristic unit tests ────────────────────────────────

func TestCountActionVerbs(t *testing.T) {
	tests := []struct {
		text string
		want int
	}{
		{"build a feature", 1},
		// "test" matches inside "tests"; "implement" and "write" also match → 3
		{"implement authentication and write tests", 3},
		// "research","design","implement","write","test"(tests),"deploy" → 6
		{"research the codebase, design the schema, implement the feature, write tests, and deploy", 6},
		// "fix","update","document"(documentation) → 3
		{"fix the bug and update the documentation", 3},
		{"", 0},
		{"just a plan", 1}, // "plan" is in actionVerbs
	}
	for _, tt := range tests {
		got := countActionVerbs(tt.text)
		if got != tt.want {
			t.Errorf("countActionVerbs(%q) = %d, want %d", tt.text, got, tt.want)
		}
	}
}

func TestCountStepConjunctions(t *testing.T) {
	tests := []struct {
		text string
		want int
	}{
		{"build a feature", 0},
		// " and then " matches once; " then " removed to avoid double-count
		{"build a feature and then deploy it", 1},
		// " and then " matches twice (", and then" contains " and then ")
		{"research, and then design, and then implement", 2},
		// "; " matches twice
		{"fix the bug; update the tests; run the linter", 2},
		{"", 0},
	}
	for _, tt := range tests {
		got := countStepConjunctions(tt.text)
		if got != tt.want {
			t.Errorf("countStepConjunctions(%q) = %d, want %d", tt.text, got, tt.want)
		}
	}
}

func TestEstimateSuggestedPhases(t *testing.T) {
	tests := []struct {
		verbCount int
		conjCount int
		want      int
	}{
		{0, 0, 2}, // minimum is always 2
		{1, 0, 2}, // below thresholds → minimum
		{3, 0, 3}, // verb-driven
		{0, 3, 4}, // conj-driven (conjCount + 1)
		{5, 4, 5}, // verb wins over conjCount+1=5 → tied at 5
		{3, 5, 6}, // conj-driven (5+1=6 > 3)
	}
	for _, tt := range tests {
		got := estimateSuggestedPhases(tt.verbCount, tt.conjCount)
		if got != tt.want {
			t.Errorf("estimateSuggestedPhases(%d, %d) = %d, want %d", tt.verbCount, tt.conjCount, got, tt.want)
		}
	}
}

// TestAuditPlan_PhaseCountAnomaly_HeuristicNoTC verifies that the heuristic
// fires for single-phase plans even without a TargetContext when the task text
// signals complexity.
func TestAuditPlan_PhaseCountAnomaly_HeuristicNoTC(t *testing.T) {
	tests := []struct {
		name    string
		task    string
		wantHit bool
	}{
		{
			name:    "simple task — does not fire",
			task:    "build a feature",
			wantHit: false,
		},
		{
			name:    "high word count — fires",
			task:    "please research the existing codebase architecture and then design a migration plan that handles all edge cases while preserving backward compatibility",
			wantHit: true,
		},
		{
			name:    "3+ action verbs — fires",
			task:    "implement the handler, write unit tests, and deploy to staging",
			wantHit: true,
		},
		{
			name:    "2+ step conjunctions — fires",
			task:    "research the API and then design the schema and then implement the endpoints",
			wantHit: true,
		},
		// Representative patterns from the 39 phase_count_anomaly findings.
		{
			name:    "multi-deliverable objective — fires",
			task:    "Add heuristic detection in the passive auditor, emit phase_count_anomaly findings, write unit tests, and update the auditor output format",
			wantHit: true,
		},
		{
			name:    "numbered steps objective — fires (high word count)",
			task:    "1. Research current decomposer logic. 2. Design new threshold config. 3. Implement changes. 4. Add tests. 5. Run go test and make build to verify correctness.",
			wantHit: true,
		},
		{
			name:    "conjunction-heavy feature request — fires",
			task:    "implement user auth; add JWT support; write integration tests; update docs",
			wantHit: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			plan := makePlan(tt.task,
				makePhase("p1", "senior-backend-engineer", "llm"),
			)
			findings := AuditPlan(plan, nil) // tc is nil — pure heuristic path
			hit := hasType(findings, PassivePhaseCountAnomaly)
			if hit != tt.wantHit {
				t.Errorf("PhaseCountAnomaly fired=%v, want %v (task=%q)", hit, tt.wantHit, tt.task)
			}
		})
	}
}

// TestAuditPlan_PhaseCountAnomaly_SeverityAndSuggestedCount verifies that the
// Severity and SuggestedPhaseCount fields are populated correctly.
func TestAuditPlan_PhaseCountAnomaly_SeverityAndSuggestedCount(t *testing.T) {
	// Task with both high verb count AND high conjunction count → "high" severity.
	task := "implement the auth handler and then write unit tests and then deploy to staging and then update the docs"
	plan := makePlan(task, makePhase("p1", "senior-backend-engineer", "llm"))
	findings := AuditPlan(plan, nil)

	var found *PassiveFinding
	for i := range findings {
		if findings[i].FindingType == PassivePhaseCountAnomaly {
			found = &findings[i]
			break
		}
	}
	if found == nil {
		t.Fatal("expected PhaseCountAnomaly finding, got none")
	}
	if found.Severity == "" {
		t.Error("Severity should be set")
	}
	if found.SuggestedPhaseCount < 2 {
		t.Errorf("SuggestedPhaseCount = %d, want >= 2", found.SuggestedPhaseCount)
	}
	if found.ObjectiveText == "" {
		t.Error("ObjectiveText should be populated")
	}
}

// TestAuditPlan_PhaseCountAnomaly_ObjectiveTextIncluded verifies that the
// flagged objective text appears in the finding for review.
func TestAuditPlan_PhaseCountAnomaly_ObjectiveTextIncluded(t *testing.T) {
	task := "research the codebase, design the schema, implement the feature, write tests, and deploy to staging"
	plan := makePlan(task, makePhase("p1", "senior-backend-engineer", "llm"))
	findings := AuditPlan(plan, repoTC("repo:~/skills/orchestrator"))

	for _, f := range findings {
		if f.FindingType == PassivePhaseCountAnomaly {
			if f.ObjectiveText == "" {
				t.Error("ObjectiveText should not be empty")
			}
			if !strings.Contains(f.Detail, "action verbs") {
				t.Errorf("Detail should mention action verbs; got: %q", f.Detail)
			}
			return
		}
	}
	t.Fatal("expected PhaseCountAnomaly finding, got none")
}

// TestAuditPlan_PhaseCountAnomaly_LowSeverityWithTCOnly verifies that when
// tc is present but heuristics do NOT fire, severity stays "low".
func TestAuditPlan_PhaseCountAnomaly_LowSeverityWithTCOnly(t *testing.T) {
	plan := makePlan("do x",
		makePhase("p1", "senior-backend-engineer", "llm"),
	)
	findings := AuditPlan(plan, repoTC("repo:~/skills/orchestrator"))

	for _, f := range findings {
		if f.FindingType == PassivePhaseCountAnomaly {
			if f.Severity != "low" {
				t.Errorf("Severity = %q, want low", f.Severity)
			}
			if f.SuggestedPhaseCount != 0 {
				t.Errorf("SuggestedPhaseCount = %d, want 0", f.SuggestedPhaseCount)
			}
			return
		}
	}
	t.Fatal("expected PhaseCountAnomaly finding for single phase with tc")
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

// hasType returns true if any finding in fs has the given FindingType.
func hasType(fs []PassiveFinding, typ string) bool {
	for _, f := range fs {
		if f.FindingType == typ {
			return true
		}
	}
	return false
}

// containsStr is a test helper for substring checks in formatter output.
func containsStr(s, substr string) bool {
	return len(s) > 0 && len(substr) > 0 && strings.Contains(s, substr)
}
