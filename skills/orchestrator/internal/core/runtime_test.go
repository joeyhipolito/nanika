package core

import (
	"strings"
	"testing"
)

// ---------------------------------------------------------------------------
// RuntimeCaps
// ---------------------------------------------------------------------------

func TestRuntimeCaps_Has(t *testing.T) {
	caps := RuntimeCaps{CapToolUse: true, CapStreaming: true}

	if !caps.Has(CapToolUse) {
		t.Error("Has(CapToolUse) = false, want true")
	}
	if !caps.Has(CapStreaming) {
		t.Error("Has(CapStreaming) = false, want true")
	}
	if caps.Has(CapArtifacts) {
		t.Error("Has(CapArtifacts) = true, want false")
	}
}

func TestRuntimeCaps_Slice(t *testing.T) {
	caps := RuntimeCaps{
		CapToolUse:       true,
		CapCostReport:    true,
		CapSessionResume: true,
	}
	sl := caps.Slice()
	if len(sl) != 3 {
		t.Fatalf("Slice() len = %d, want 3", len(sl))
	}
	want := []RuntimeCap{CapToolUse, CapCostReport, CapSessionResume}
	for i := range want {
		if sl[i] != want[i] {
			t.Fatalf("Slice()[%d] = %q, want %q (full slice=%v)", i, sl[i], want[i], sl)
		}
	}
}

// ---------------------------------------------------------------------------
// ClaudeDescriptor
// ---------------------------------------------------------------------------

func TestClaudeDescriptor(t *testing.T) {
	desc := ClaudeDescriptor()
	if desc.Name != RuntimeClaude {
		t.Fatalf("ClaudeDescriptor().Name = %q, want %q", desc.Name, RuntimeClaude)
	}
	for _, cap := range []RuntimeCap{CapToolUse, CapSessionResume, CapStreaming, CapCostReport, CapArtifacts} {
		if !desc.Caps.Has(cap) {
			t.Errorf("ClaudeDescriptor missing capability %q", cap)
		}
	}
}

// ---------------------------------------------------------------------------
// CodexDescriptor
// ---------------------------------------------------------------------------

func TestCodexDescriptor(t *testing.T) {
	desc := CodexDescriptor()
	if desc.Name != RuntimeCodex {
		t.Fatalf("CodexDescriptor().Name = %q, want %q", desc.Name, RuntimeCodex)
	}
	for _, cap := range []RuntimeCap{CapToolUse, CapSessionResume, CapStreaming, CapArtifacts} {
		if !desc.Caps.Has(cap) {
			t.Errorf("CodexDescriptor missing capability %q", cap)
		}
	}
}

// TestCodexDescriptor_CapCostReport_NotInSet is a regression test: having
// CapCostReport: false as a map entry causes Slice() to include it, making
// the set report a capability the runtime does not provide. The key must be
// absent from the map entirely.
func TestCodexDescriptor_CapCostReport_NotInSet(t *testing.T) {
	desc := CodexDescriptor()
	if desc.Caps.Has(CapCostReport) {
		t.Error("CodexDescriptor: Has(CapCostReport) = true, want false")
	}
	for _, c := range desc.Caps.Slice() {
		if c == CapCostReport {
			t.Error("CodexDescriptor: Slice() contains CapCostReport; it must be absent from the set")
		}
	}
}

// ---------------------------------------------------------------------------
// ContractForRole
// ---------------------------------------------------------------------------

func TestContractForRole_Planner(t *testing.T) {
	c := ContractForRole(RolePlanner)
	if c.Role != RolePlanner {
		t.Fatalf("ContractForRole(planner).Role = %q", c.Role)
	}
	if len(c.Required) != 1 || c.Required[0] != CapToolUse {
		t.Errorf("planner required = %v, want [tool_use]", c.Required)
	}
	// Preferred should include artifacts and cost_report
	found := map[RuntimeCap]bool{}
	for _, p := range c.Preferred {
		found[p] = true
	}
	if !found[CapArtifacts] {
		t.Error("planner preferred missing artifacts")
	}
}

func TestContractForRole_Implementer(t *testing.T) {
	c := ContractForRole(RoleImplementer)
	if c.Role != RoleImplementer {
		t.Fatalf("ContractForRole(implementer).Role = %q", c.Role)
	}
	required := map[RuntimeCap]bool{}
	for _, r := range c.Required {
		required[r] = true
	}
	if !required[CapToolUse] || !required[CapArtifacts] {
		t.Errorf("implementer required = %v, want [tool_use, artifacts]", c.Required)
	}
}

func TestContractForRole_Reviewer(t *testing.T) {
	c := ContractForRole(RoleReviewer)
	if c.Role != RoleReviewer {
		t.Fatalf("ContractForRole(reviewer).Role = %q", c.Role)
	}
	required := map[RuntimeCap]bool{}
	for _, r := range c.Required {
		required[r] = true
	}
	if !required[CapToolUse] || !required[CapArtifacts] {
		t.Errorf("reviewer required = %v, want [tool_use, artifacts]", c.Required)
	}
}

func TestContractForRole_Unknown(t *testing.T) {
	c := ContractForRole(Role("custom"))
	if len(c.Required) != 1 || c.Required[0] != CapToolUse {
		t.Errorf("unknown role required = %v, want [tool_use]", c.Required)
	}
}

// ---------------------------------------------------------------------------
// PhaseContract.Validate
// ---------------------------------------------------------------------------

func TestPhaseContract_Validate_Satisfied(t *testing.T) {
	contract := ContractForRole(RoleImplementer)
	desc := ClaudeDescriptor()

	result := contract.Validate(desc)
	if !result.Satisfied {
		t.Fatalf("expected Satisfied=true for Claude + implementer; missing=%v", result.Missing)
	}
	if len(result.Missing) != 0 {
		t.Errorf("expected no missing caps; got %v", result.Missing)
	}
}

func TestPhaseContract_Validate_MissingRequired(t *testing.T) {
	contract := ContractForRole(RoleImplementer) // requires tool_use + artifacts
	desc := RuntimeDescriptor{
		Name: Runtime("limited"),
		Caps: RuntimeCaps{CapToolUse: true}, // no artifacts
	}

	result := contract.Validate(desc)
	if result.Satisfied {
		t.Fatal("expected Satisfied=false when artifacts missing")
	}
	if len(result.Missing) != 1 || result.Missing[0] != CapArtifacts {
		t.Errorf("Missing = %v, want [artifacts]", result.Missing)
	}
}

func TestPhaseContract_Validate_MissingPreferred(t *testing.T) {
	contract := ContractForRole(RoleImplementer) // prefers session_resume, cost_report
	desc := RuntimeDescriptor{
		Name: Runtime("basic"),
		Caps: RuntimeCaps{CapToolUse: true, CapArtifacts: true}, // no session_resume or cost_report
	}

	result := contract.Validate(desc)
	if !result.Satisfied {
		t.Fatal("expected Satisfied=true when only preferred caps missing")
	}
	if len(result.Warnings) == 0 {
		t.Error("expected warnings for missing preferred caps")
	}
}

func TestPhaseContract_Validate_AllRolesWithClaude(t *testing.T) {
	desc := ClaudeDescriptor()
	for _, role := range []Role{RolePlanner, RoleImplementer, RoleReviewer} {
		contract := ContractForRole(role)
		result := contract.Validate(desc)
		if !result.Satisfied {
			t.Errorf("Claude should satisfy %s contract; missing=%v", role, result.Missing)
		}
	}
}

// ---------------------------------------------------------------------------
// ContractResult.ErrorMessage
// ---------------------------------------------------------------------------

func TestContractResult_ErrorMessage_Satisfied(t *testing.T) {
	r := ContractResult{Satisfied: true}
	if r.ErrorMessage() != "" {
		t.Errorf("ErrorMessage() = %q for satisfied result, want empty", r.ErrorMessage())
	}
}

func TestContractResult_ErrorMessage_Unsatisfied(t *testing.T) {
	r := ContractResult{
		Satisfied: false,
		Missing:   []RuntimeCap{CapArtifacts, CapToolUse},
	}
	errStr := r.ErrorMessage()
	if errStr == "" {
		t.Fatal("ErrorMessage() is empty for unsatisfied result")
	}
	if !strings.Contains(errStr, "artifacts") {
		t.Errorf("ErrorMessage() = %q, missing 'artifacts'", errStr)
	}
	if !strings.Contains(errStr, "cap.tool_use") {
		t.Errorf("ErrorMessage() = %q, missing 'cap.tool_use'", errStr)
	}
}

// ---------------------------------------------------------------------------
// RuntimeBoth sentinel
// ---------------------------------------------------------------------------

// TestRuntimeBoth_Value ensures the constant is defined and distinct from the
// other two named runtimes. The sentinel value "both" is used by ParsePhases
// when the PHASE line has RUNTIME: both.
func TestRuntimeBoth_Value(t *testing.T) {
	if RuntimeBoth == "" {
		t.Fatal("RuntimeBoth must be non-empty")
	}
	if RuntimeBoth == RuntimeClaude {
		t.Fatalf("RuntimeBoth must differ from RuntimeClaude (%q)", RuntimeClaude)
	}
	if RuntimeBoth == RuntimeCodex {
		t.Fatalf("RuntimeBoth must differ from RuntimeCodex (%q)", RuntimeCodex)
	}
}

// TestRuntimeBoth_Effective verifies that RuntimeBoth.Effective() returns itself
// (not RuntimeClaude), so the engine can detect the sentinel and dispatch to
// both executors.
func TestRuntimeBoth_Effective(t *testing.T) {
	if RuntimeBoth.Effective() != RuntimeBoth {
		t.Errorf("RuntimeBoth.Effective() = %q, want %q", RuntimeBoth.Effective(), RuntimeBoth)
	}
}

// ---------------------------------------------------------------------------
// SelectRuntime — runtime-selection policy
// ---------------------------------------------------------------------------

// TestSelectRuntime_AlwaysClaude covers every path that must unconditionally
// resolve to Claude regardless of persona or task: reviewer, planner, unknown
// roles, generic implementers, ambiguous task text, and explicit non-code work.
func TestSelectRuntime_AlwaysClaude(t *testing.T) {
	cases := []struct {
		role    Role
		persona string
		task    string
		reason  string
	}{
		// Reviewer and planner always → Claude
		{RoleReviewer, "staff-code-reviewer", "review the implementation", "reviewer role"},
		{RoleReviewer, "senior-golang-engineer", "audit the code", "reviewer role overrides language persona"},
		{RolePlanner, "architect", "design the system", "planner role"},
		{RolePlanner, "senior-typescript-engineer", "plan the architecture", "planner role overrides language persona"},

		// Generic implementer personas → Claude (no language-specialist token)
		{RoleImplementer, "senior-backend-engineer", "implement the API", "generic backend persona"},
		{RoleImplementer, "senior-fullstack-engineer", "build the feature", "generic fullstack persona"},
		{RoleImplementer, "", "write code", "empty persona"},
		{RoleImplementer, "senior-javafx-engineer", "implement the renderer", "javafx should not match java token"},
		{RoleImplementer, "senior-golang-engineer", "coordinate the release checklist", "language persona but no positive code-shape cue"},
		{RoleImplementer, "senior-golang-engineer", "", "empty task text"},
		{RoleImplementer, "senior-golang-engineer", "   ", "whitespace-only task text"},
		{RoleImplementer, "senior-golang-engineer", "Prepare the client onboarding notes", "substring collision: cli inside client must not trigger Codex"},

		// Language-specialist persona but non-code task shape → Claude
		{RoleImplementer, "senior-golang-engineer", "research how to structure the cache", "non-code task: research"},
		{RoleImplementer, "senior-golang-engineer", "research how to implement the cache", "mixed signal: non-code blacklist wins over code cue"},
		{RoleImplementer, "senior-typescript-engineer", "document the API", "non-code task: docs"},
		{RoleImplementer, "senior-python-engineer", "write blog post about the release", "non-code task: writing"},
		{RoleImplementer, "senior-rust-engineer", "deploy the service to production", "non-code task: deploy"},

		// Zero/unknown roles → Claude
		{Role(""), "", "", "zero role"},
		{Role("custom"), "unknown-persona", "some task", "unknown role"},
	}
	for _, c := range cases {
		got := SelectRuntime(c.role, c.persona, c.task)
		if got != RuntimeClaude {
			t.Errorf("SelectRuntime(%q, %q, %q) = %q, want RuntimeClaude [%s]",
				c.role, c.persona, c.task, got, c.reason)
		}
	}
}

// TestSelectRuntime_NeverReturnsEmpty ensures the policy always returns a
// named, non-empty runtime so plan.json never carries an ambiguous zero value.
func TestSelectRuntime_NeverReturnsEmpty(t *testing.T) {
	roles := []Role{RolePlanner, RoleImplementer, RoleReviewer, Role("")}
	for _, r := range roles {
		got := SelectRuntime(r, "", "")
		if got == "" {
			t.Errorf("SelectRuntime(%q) returned empty runtime — policy must always return a named runtime", r)
		}
	}
}

// TestSelectRuntime_CodexDisabled verifies that Codex auto-selection is
// disabled — all cases that previously resolved to Codex now resolve to Claude.
// When re-enabling Codex, flip the expected value back to RuntimeCodex.
func TestSelectRuntime_CodexDisabled(t *testing.T) {
	cases := []struct {
		role    Role
		persona string
		task    string
	}{
		{RoleImplementer, "senior-golang-engineer", "implement the cache layer"},
		{RoleImplementer, "senior-typescript-engineer", "implement the React component"},
		{RoleImplementer, "senior-rust-engineer", "write the parser"},
		{RoleImplementer, "senior-golang-engineer", "migrate the database schema"},
	}
	for _, c := range cases {
		got := SelectRuntime(c.role, c.persona, c.task)
		if got != RuntimeClaude {
			t.Errorf("SelectRuntime(%q, %q, %q) = %q, want RuntimeClaude (Codex auto-selection disabled)",
				c.role, c.persona, c.task, got)
		}
	}
}

// TestSelectRuntime_ExplicitRuntimeNotAffected documents that SelectRuntime is
// only called when Phase.Runtime == "". The engine's applyRuntimePolicy guards
// this; explicit authored runtimes always win over the heuristic.
// This test validates the boundary by confirming the heuristic output is
// deterministic — callers can rely on the return value when guarding correctly.
func TestSelectRuntime_ExplicitRuntimeNotAffected(t *testing.T) {
	// A phase with an explicit RUNTIME: claude must never be passed to
	// SelectRuntime. We document the expectation: if a caller accidentally
	// passes a generic implementer, they still get Claude, and if they pass a
	// language-specialist implementer they get Codex — the caller must guard
	// against this by checking Phase.Runtime != "" before calling.
	got := SelectRuntime(RoleImplementer, "senior-backend-engineer", "implement the API")
	if got != RuntimeClaude {
		t.Errorf("generic implementer should resolve to Claude, got %q", got)
	}
}

func TestContainsHeuristicPhrase(t *testing.T) {
	cases := []struct {
		name   string
		text   string
		phrase string
		want   bool
	}{
		{name: "word boundary positive", text: "Implement the API client", phrase: "api", want: true},
		{name: "substring cli collision", text: "Prepare the client notes", phrase: "cli", want: false},
		{name: "substring test collision", text: "latest rollout notes", phrase: "test", want: false},
		{name: "multi-word phrase", text: "Write blog post about the release", phrase: "write blog", want: true},
		{name: "persona token boundary", text: "senior-javascript-engineer", phrase: "java", want: false},
		{name: "go boundary", text: "senior-go-engineer", phrase: "go", want: true},
		{name: "punctuation normalized", text: "Implement-the-cache-layer", phrase: "implement", want: true},
	}
	for _, tc := range cases {
		if got := containsHeuristicPhrase(tc.text, tc.phrase); got != tc.want {
			t.Errorf("%s: containsHeuristicPhrase(%q, %q) = %v, want %v",
				tc.name, tc.text, tc.phrase, got, tc.want)
		}
	}
}

func TestNormalizeHeuristicText(t *testing.T) {
	cases := []struct {
		in   string
		want string
	}{
		{in: "  Implement-the-cache-layer  ", want: "implement the cache layer"},
		{in: "senior_javascript---engineer", want: "senior javascript engineer"},
		{in: "write,   blog\tpost", want: "write blog post"},
	}
	for _, tc := range cases {
		if got := normalizeHeuristicText(tc.in); got != tc.want {
			t.Errorf("normalizeHeuristicText(%q) = %q, want %q", tc.in, got, tc.want)
		}
	}
}
