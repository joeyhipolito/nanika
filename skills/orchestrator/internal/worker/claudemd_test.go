package worker

import (
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

func TestBuildCLAUDEmd_MinimalBundle(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Backend Engineer\n\nYou are a backend engineer.",
		PersonaName: "backend-engineer",
		Objective:   "Implement the user registration endpoint",
		Domain:      "dev",
		WorkspaceID: "ws-123",
		PhaseID:     "phase-1",
	}

	md := BuildCLAUDEmd(bundle)

	// Must contain persona
	if !strings.Contains(md, "Backend Engineer") {
		t.Error("CLAUDE.md missing persona content")
	}

	// Must contain objective
	if !strings.Contains(md, "## Your Task") {
		t.Error("CLAUDE.md missing task section")
	}
	if !strings.Contains(md, "Implement the user registration endpoint") {
		t.Error("CLAUDE.md missing task objective")
	}

	// Must contain workspace info
	if !strings.Contains(md, "ws-123") {
		t.Error("CLAUDE.md missing workspace ID")
	}
	if !strings.Contains(md, "phase-1") {
		t.Error("CLAUDE.md missing phase ID")
	}

	// Must contain completion signal section
	if !strings.Contains(md, "## Completion Signal") {
		t.Error("CLAUDE.md missing completion signal section")
	}
	if !strings.Contains(md, "orchestrator.signal.json") {
		t.Error("CLAUDE.md missing signal file name")
	}

	// Must contain learning capture markers
	if !strings.Contains(md, "LEARNING:") {
		t.Error("CLAUDE.md missing learning capture markers")
	}

	// Constraints section is always present (git no-op policy is hardcoded)
	if !strings.Contains(md, "## Constraints") {
		t.Error("CLAUDE.md must always have constraints section")
	}
	if !strings.Contains(md, "Do NOT create git branches") {
		t.Error("CLAUDE.md missing git no-op constraint")
	}

	// Must NOT contain optional sections when empty
	if strings.Contains(md, "## Available Tools") {
		t.Error("CLAUDE.md should not have tools section when no skill index")
	}
	if strings.Contains(md, "## Primary Tools") {
		t.Error("CLAUDE.md should not have primary tools when no skills")
	}
	if strings.Contains(md, "## Context from Prior Work") {
		t.Error("CLAUDE.md should not have prior context when empty")
	}
	if strings.Contains(md, "## Lessons from Past Missions") {
		t.Error("CLAUDE.md should not have learnings when empty")
	}
}

func TestBuildCLAUDEmd_FullBundle(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Researcher\n\nYou are a researcher.",
		PersonaName: "researcher",
		Objective:   "Research Go error handling best practices",
		SkillIndex:  "|obsidian — vault notes|scout — intel gathering|todoist — tasks|",
		Skills: []core.Skill{
			{Name: "scout", CommandReference: "scout gather\nscout intel"},
			{Name: "obsidian", CommandReference: "obsidian search\nobsidian read"},
		},
		Constraints:  []string{"Use only standard library", "No external dependencies"},
		PriorContext: "Previous phase found that Go 1.13+ error wrapping is preferred.",
		Learnings:    "- [PATTERN] Always wrap errors with context\n- [GOTCHA] Don't use pkg/errors",
		Domain:       "dev",
		WorkspaceID:  "ws-456",
		PhaseID:      "phase-2",
		Handoffs: []core.HandoffRecord{
			{
				FromRole:     core.RolePlanner,
				ToRole:       core.RoleImplementer,
				FromPersona:  "architect",
				Summary:      "Defined the rollout plan and constraints.",
				Expectations: []string{"Follow the plan"},
			},
		},
	}

	md := BuildCLAUDEmd(bundle)

	// Skill index (all tools)
	if !strings.Contains(md, "## Available Tools") {
		t.Error("CLAUDE.md missing skill index section")
	}
	if !strings.Contains(md, "obsidian — vault notes") {
		t.Error("CLAUDE.md missing skill index content")
	}

	// Phase-specific skills (detailed)
	if !strings.Contains(md, "## Primary Tools for This Phase") {
		t.Error("CLAUDE.md missing primary tools section")
	}
	if !strings.Contains(md, "### scout") {
		t.Error("CLAUDE.md missing scout skill detail")
	}
	if !strings.Contains(md, "### obsidian") {
		t.Error("CLAUDE.md missing obsidian skill detail")
	}

	// Constraints
	if !strings.Contains(md, "## Constraints") {
		t.Error("CLAUDE.md missing constraints section")
	}
	if !strings.Contains(md, "Use only standard library") {
		t.Error("CLAUDE.md missing constraint content")
	}

	// Prior context
	if !strings.Contains(md, "## Context from Prior Work") {
		t.Error("CLAUDE.md missing prior context section")
	}
	if !strings.Contains(md, "Go 1.13+ error wrapping") {
		t.Error("CLAUDE.md missing prior context content")
	}

	// Learnings
	if !strings.Contains(md, "## Lessons from Past Missions") {
		t.Error("CLAUDE.md missing learnings section")
	}
	if !strings.Contains(md, "Always wrap errors") {
		t.Error("CLAUDE.md missing learning content")
	}
	if !strings.Contains(md, "## Role Handoffs") {
		t.Error("CLAUDE.md missing role handoffs section")
	}
	if !strings.Contains(md, "Defined the rollout plan and constraints.") {
		t.Error("CLAUDE.md missing role handoff summary")
	}
}

func TestBuildCLAUDEmd_SectionOrder(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:      "# Test Persona",
		PersonaName:  "test",
		Objective:    "Do something",
		SkillIndex:   "|tools index here|",
		Skills:       []core.Skill{{Name: "scout", CommandReference: "scout gather"}},
		Constraints:  []string{"Be fast"},
		PriorContext: "Prior work exists",
		Learnings:    "Lesson learned",
		Domain:       "dev",
		WorkspaceID:  "ws-test",
		PhaseID:      "phase-1",
	}

	md := BuildCLAUDEmd(bundle)

	// Verify sections appear in correct order
	sections := []string{
		"# Test Persona",                  // 1. persona
		"## Your Task",                    // 2. task objective
		"## Context from Prior Work",      // 3. prior context (primacy zone)
		"## Lessons from Past Missions",   // 7. learnings
		"## Available Tools",              // 8. skill index
		"## Primary Tools for This Phase", // 9. phase-specific skills
		"## Constraints",                  // 10. constraints
		"## Workspace",                    // 12. workspace
		"## Output",                       // 13. output instructions
		"## Completion Signal",            // 14. signal instructions
		"## Learning Capture",             // 15. capture instructions
	}

	lastIdx := -1
	for _, section := range sections {
		idx := strings.Index(md, section)
		if idx < 0 {
			t.Errorf("CLAUDE.md missing section %q", section)
			continue
		}
		if idx <= lastIdx {
			t.Errorf("section %q appears out of order (at %d, previous at %d)", section, idx, lastIdx)
		}
		lastIdx = idx
	}
}

func TestLoadSkillIndex(t *testing.T) {
	idx := LoadSkillIndex()
	if idx == "" {
		t.Skip("no ~/nanika/CLAUDE.md found — skipping skill index test")
	}

	// Should contain at least some known skills
	if !strings.Contains(idx, "obsidian") {
		t.Error("skill index missing obsidian")
	}
	if !strings.Contains(idx, "scout") {
		t.Error("skill index missing scout")
	}
	if !strings.Contains(idx, "todoist") {
		t.Error("skill index missing todoist")
	}
}

func TestExtractAgentsMD(t *testing.T) {
	content := `# Some Header

stuff before

<!-- NANIKA-AGENTS-MD-START -->
|obsidian — notes|scout — intel|todoist — tasks|
<!-- NANIKA-AGENTS-MD-END -->

stuff after`

	result := extractAgentsMD(content)
	if result != "|obsidian — notes|scout — intel|todoist — tasks|" {
		t.Errorf("extractAgentsMD got %q", result)
	}
}

func TestExtractAgentsMD_Missing(t *testing.T) {
	result := extractAgentsMD("no markers here")
	if result != "" {
		t.Errorf("expected empty string for missing markers, got %q", result)
	}
}

func TestBuildCLAUDEmd_MissionContext_Populated(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:        "# Test Persona",
		PersonaName:    "test",
		Objective:      "Do something",
		MissionContext: "- **Target** ~/blog/posts/my-article.mdx\n- **Type** article",
		Domain:         "dev",
		WorkspaceID:    "ws-ctx",
		PhaseID:        "phase-1",
	}

	md := BuildCLAUDEmd(bundle)

	if !strings.Contains(md, "## Mission Context") {
		t.Error("CLAUDE.md missing Mission Context section when populated")
	}
	if !strings.Contains(md, "- **Target** ~/blog/posts/my-article.mdx") {
		t.Error("CLAUDE.md missing mission context content")
	}

	// Verify ordering: Mission Context after task/prior context/role, before tools
	personaIdx := strings.Index(md, "# Test Persona")
	taskIdx := strings.Index(md, "## Your Task")
	ctxIdx := strings.Index(md, "## Mission Context")
	if personaIdx >= taskIdx {
		t.Errorf("Persona (%d) should appear before Your Task (%d)", personaIdx, taskIdx)
	}
	if taskIdx >= ctxIdx {
		t.Errorf("Your Task (%d) should appear before Mission Context (%d)", taskIdx, ctxIdx)
	}
}

// TestBuildCLAUDEmdLearningsPlacement verifies learnings appear after the
// task objective and before the tools section (position 3 in the document).
func TestBuildCLAUDEmdLearningsPlacement(t *testing.T) {
	tests := []struct {
		name      string
		learnings string
		hasTools  bool
	}{
		{
			name:      "learnings with skill index: appears between task and tools",
			learnings: "- [pattern] Always wrap errors with context",
			hasTools:  true,
		},
		{
			name:      "learnings without skill index: appears after task",
			learnings: "- [gotcha] Never ignore returned errors",
			hasTools:  false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			bundle := core.ContextBundle{
				Persona:     "# Test Persona",
				PersonaName: "test",
				Objective:   "Do the thing",
				Learnings:   tt.learnings,
				Domain:      "dev",
				WorkspaceID: "ws-placement",
				PhaseID:     "phase-1",
			}
			if tt.hasTools {
				bundle.SkillIndex = "|scout — intel|obsidian — notes|"
			}

			md := BuildCLAUDEmd(bundle)

			taskIdx := strings.Index(md, "## Your Task")
			learningsIdx := strings.Index(md, "## Lessons from Past Missions")

			if taskIdx < 0 {
				t.Fatal("missing '## Your Task' section")
			}
			if learningsIdx < 0 {
				t.Fatal("missing '## Lessons from Past Missions' section")
			}
			if learningsIdx <= taskIdx {
				t.Errorf("learnings section (at %d) must appear after '## Your Task' (at %d)",
					learningsIdx, taskIdx)
			}

			if tt.hasTools {
				toolsIdx := strings.Index(md, "## Available Tools")
				if toolsIdx < 0 {
					t.Fatal("missing '## Available Tools' section")
				}
				if learningsIdx >= toolsIdx {
					t.Errorf("learnings section (at %d) must appear before '## Available Tools' (at %d)",
						learningsIdx, toolsIdx)
				}
			}

			// Learnings content must be present
			if !strings.Contains(md, tt.learnings) {
				t.Errorf("learnings content %q not found in output", tt.learnings)
			}
		})
	}
}

// TestBuildCLAUDEmdNoLearningsWhenEmpty verifies that the learnings section
// is omitted entirely when no learnings are provided.
func TestBuildCLAUDEmdNoLearningsWhenEmpty(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Test Persona",
		PersonaName: "test",
		Objective:   "Do the thing",
		// Learnings: "" (empty)
		Domain:      "dev",
		WorkspaceID: "ws-empty",
		PhaseID:     "phase-1",
	}

	md := BuildCLAUDEmd(bundle)

	if strings.Contains(md, "## Lessons from Past Missions") {
		t.Error("learnings section must not appear when Learnings is empty")
	}
}

func TestBuildCLAUDEmd_MissionContext_Empty(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Test Persona",
		PersonaName: "test",
		Objective:   "Do something",
		Domain:      "dev",
		WorkspaceID: "ws-ctx",
		PhaseID:     "phase-1",
	}

	md := BuildCLAUDEmd(bundle)

	if strings.Contains(md, "## Mission Context") {
		t.Error("CLAUDE.md should not have Mission Context section when empty")
	}
}

// ---------------------------------------------------------------------------
// BuildFrontmatter: generates YAML frontmatter block
// ---------------------------------------------------------------------------

func TestBuildFrontmatter_AllFields(t *testing.T) {
	meta := ArtifactMeta{
		ProducedBy:    "senior-backend-engineer",
		Phase:         "artifact-metadata",
		Workspace:     "ws-20260311",
		CreatedAt:     mustParseTime("2026-03-11T14:00:00Z"),
		Confidence:    "high",
		DependsOn:     []string{"phase-3", "phase-5"},
		TokenEstimate: 1200,
	}

	fm := BuildFrontmatter(meta)

	checks := []string{
		"---\n",
		"produced_by: senior-backend-engineer",
		"phase: artifact-metadata",
		"workspace: ws-20260311",
		`created_at: "2026-03-11T14:00:00Z"`,
		"confidence: high",
		"  - phase-3",
		"  - phase-5",
		"token_estimate: 1200",
	}
	for _, want := range checks {
		if !strings.Contains(fm, want) {
			t.Errorf("BuildFrontmatter missing %q; got:\n%s", want, fm)
		}
	}

	// Must end with "---\n\n" (two newlines after closing delimiter)
	if !strings.HasSuffix(fm, "---\n\n") {
		t.Errorf("BuildFrontmatter must end with '---\\n\\n'; got:\n%s", fm)
	}
}

func TestBuildFrontmatter_EmptyDependsOn(t *testing.T) {
	meta := ArtifactMeta{
		ProducedBy: "researcher",
		Phase:      "research",
		Workspace:  "ws-abc",
		CreatedAt:  mustParseTime("2026-01-01T00:00:00Z"),
		Confidence: "medium",
		DependsOn:  nil,
	}

	fm := BuildFrontmatter(meta)

	if !strings.Contains(fm, "depends_on:\n  []") {
		t.Errorf("empty DependsOn should produce '  []'; got:\n%s", fm)
	}
}

// ---------------------------------------------------------------------------
// InjectFrontmatterIfMissing: prepends frontmatter to files without it
// ---------------------------------------------------------------------------

func TestInjectFrontmatterIfMissing_NoFrontmatter(t *testing.T) {
	meta := ArtifactMeta{
		ProducedBy: "senior-backend-engineer",
		Phase:      "phase-7",
		Workspace:  "ws-test",
		CreatedAt:  mustParseTime("2026-03-11T14:00:00Z"),
		Confidence: "high",
	}

	data := []byte("# Report\n\nSome content here.\n")
	result := InjectFrontmatterIfMissing(data, meta)

	if !strings.HasPrefix(string(result), "---\n") {
		t.Error("result should start with '---\\n'")
	}
	if !strings.Contains(string(result), "produced_by: senior-backend-engineer") {
		t.Error("result missing produced_by field")
	}
	if !strings.Contains(string(result), "# Report\n\nSome content here.") {
		t.Error("result missing original content")
	}
}

func TestInjectFrontmatterIfMissing_AlreadyHasFrontmatter(t *testing.T) {
	meta := ArtifactMeta{
		ProducedBy: "researcher",
		Phase:      "phase-1",
		Workspace:  "ws-test",
		CreatedAt:  mustParseTime("2026-03-11T14:00:00Z"),
		Confidence: "high",
	}

	original := []byte("---\nproduced_by: existing-persona\n---\n\n# Content\n")
	result := InjectFrontmatterIfMissing(original, meta)

	if string(result) != string(original) {
		t.Errorf("data with existing frontmatter must be returned unchanged;\ngot: %q\nwant: %q", result, original)
	}
}

func TestInjectFrontmatterIfMissing_TokenEstimateFromFileSize(t *testing.T) {
	meta := ArtifactMeta{
		ProducedBy: "senior-backend-engineer",
		Phase:      "phase-7",
		Workspace:  "ws-test",
		CreatedAt:  mustParseTime("2026-03-11T14:00:00Z"),
		Confidence: "high",
		// TokenEstimate intentionally zero — should be derived from data length
	}

	data := []byte(strings.Repeat("x", 400)) // 400 bytes → ~100 tokens
	result := InjectFrontmatterIfMissing(data, meta)

	// token_estimate should be 400/4 = 100
	if !strings.Contains(string(result), "token_estimate: 100") {
		t.Errorf("token_estimate should be 100 (400 bytes / 4); got:\n%s", string(result)[:200])
	}
}

func TestInjectFrontmatterIfMissing_PreservedWhenExplicitlySet(t *testing.T) {
	meta := ArtifactMeta{
		ProducedBy:    "researcher",
		Phase:         "phase-1",
		Workspace:     "ws-test",
		CreatedAt:     mustParseTime("2026-03-11T14:00:00Z"),
		Confidence:    "medium",
		TokenEstimate: 999,
	}

	data := []byte("# My report\n")
	result := InjectFrontmatterIfMissing(data, meta)

	if !strings.Contains(string(result), "token_estimate: 999") {
		t.Errorf("explicit TokenEstimate should be preserved; got:\n%s", result)
	}
}

// ---------------------------------------------------------------------------
// BuildCLAUDEmd: Output section includes frontmatter instructions
// ---------------------------------------------------------------------------

func TestBuildCLAUDEmd_OutputIncludesFrontmatterInstructions(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Test Persona",
		PersonaName: "senior-backend-engineer",
		Objective:   "Do the task",
		Domain:      "dev",
		WorkspaceID: "ws-abc123",
		PhaseID:     "phase-7",
	}

	md := BuildCLAUDEmd(bundle)

	// Frontmatter instructions must appear in the Output section
	if !strings.Contains(md, "produced_by: senior-backend-engineer") {
		t.Error("CLAUDE.md Output section missing produced_by pre-filled value")
	}
	if !strings.Contains(md, "phase: phase-7") {
		t.Error("CLAUDE.md Output section missing phase pre-filled value")
	}
	if !strings.Contains(md, "workspace: ws-abc123") {
		t.Error("CLAUDE.md Output section missing workspace pre-filled value")
	}
	if !strings.Contains(md, "confidence:") {
		t.Error("CLAUDE.md Output section missing confidence field")
	}
	if !strings.Contains(md, "depends_on:") {
		t.Error("CLAUDE.md Output section missing depends_on field")
	}
	if !strings.Contains(md, "token_estimate:") {
		t.Error("CLAUDE.md Output section missing token_estimate field")
	}
}

// ---------------------------------------------------------------------------
// BuildCLAUDEmd: TargetDir-aware output instruction
// ---------------------------------------------------------------------------

func TestBuildCLAUDEmd_OutputWithTargetDir(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Test Persona",
		PersonaName: "test",
		Objective:   "Do something",
		Domain:      "dev",
		WorkspaceID: "ws-tgt",
		PhaseID:     "phase-1",
		TargetDir:   "/Users/joey/skills/orchestrator",
		WorkerDir:   "/Users/joey/.via/workspaces/abc/workers/test-phase-1",
	}

	md := BuildCLAUDEmd(bundle)

	outputIdx := strings.Index(md, "## Output")
	if outputIdx < 0 {
		t.Fatal("CLAUDE.md missing Output section")
	}
	outputSection := md[outputIdx:]

	if !strings.Contains(outputSection, "target repository") {
		t.Error("Output section must mention 'target repository' when TargetDir is set")
	}
	if !strings.Contains(outputSection, bundle.TargetDir) {
		t.Errorf("Output section must contain TargetDir %q", bundle.TargetDir)
	}
	if !strings.Contains(outputSection, bundle.WorkerDir) {
		t.Errorf("Output section must contain WorkerDir %q", bundle.WorkerDir)
	}
	// Must NOT say "current directory" when TargetDir is set (that would be wrong)
	if strings.Contains(outputSection, "current directory") {
		t.Error("Output section must not say 'current directory' when TargetDir is set")
	}
}

func TestBuildCLAUDEmd_OutputWithoutTargetDir(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Test Persona",
		PersonaName: "test",
		Objective:   "Do something",
		Domain:      "dev",
		WorkspaceID: "ws-notgt",
		PhaseID:     "phase-1",
		// TargetDir and WorkerDir empty — legacy behaviour
	}

	md := BuildCLAUDEmd(bundle)

	outputIdx := strings.Index(md, "## Output")
	if outputIdx < 0 {
		t.Fatal("CLAUDE.md missing Output section")
	}
	outputSection := md[outputIdx:]

	if !strings.Contains(outputSection, "current directory") {
		t.Error("Output section must say 'current directory' when TargetDir is not set")
	}
}

// ---------------------------------------------------------------------------
// BuildCLAUDEmd: Role contract rendering
// ---------------------------------------------------------------------------

func TestBuildCLAUDEmd_RoleContract_Implementer(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Backend Engineer",
		PersonaName: "senior-backend-engineer",
		Objective:   "Implement the feature",
		Domain:      "dev",
		WorkspaceID: "ws-role",
		PhaseID:     "phase-1",
		Role:        core.RoleImplementer,
		Runtime:     core.RuntimeClaude,
	}

	md := BuildCLAUDEmd(bundle)

	if !strings.Contains(md, "## Your Role Contract") {
		t.Error("CLAUDE.md missing role contract section")
	}
	if !strings.Contains(md, "**implementer**") {
		t.Error("CLAUDE.md role contract missing implementer designation")
	}
	if !strings.Contains(md, "Produce working code") {
		t.Error("CLAUDE.md role contract missing implementer guidance")
	}
	// Workspace section should include role and runtime
	if !strings.Contains(md, "- **Role**: implementer") {
		t.Error("CLAUDE.md workspace section missing role")
	}
	if !strings.Contains(md, "- **Runtime**: claude") {
		t.Error("CLAUDE.md workspace section missing runtime")
	}
}

func TestBuildCLAUDEmd_RoleContract_Reviewer(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Code Reviewer",
		PersonaName: "staff-code-reviewer",
		Objective:   "Review the implementation",
		Domain:      "dev",
		WorkspaceID: "ws-role",
		PhaseID:     "phase-2",
		Role:        core.RoleReviewer,
	}

	md := BuildCLAUDEmd(bundle)

	if !strings.Contains(md, "**reviewer**") {
		t.Error("CLAUDE.md role contract missing reviewer designation")
	}
	if !strings.Contains(md, "### Blockers") {
		t.Error("CLAUDE.md reviewer contract missing structured findings guidance")
	}
}

func TestBuildCLAUDEmd_RoleContract_Planner(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Architect",
		PersonaName: "architect",
		Objective:   "Design the system",
		Domain:      "dev",
		WorkspaceID: "ws-role",
		PhaseID:     "phase-1",
		Role:        core.RolePlanner,
	}

	md := BuildCLAUDEmd(bundle)

	if !strings.Contains(md, "**planner**") {
		t.Error("CLAUDE.md role contract missing planner designation")
	}
	if !strings.Contains(md, "design, architecture, or research") {
		t.Error("CLAUDE.md planner contract missing guidance")
	}
}

func TestBuildCLAUDEmd_NoRoleContract_WhenRoleEmpty(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Test",
		PersonaName: "test",
		Objective:   "Do something",
		Domain:      "dev",
		WorkspaceID: "ws-norole",
		PhaseID:     "phase-1",
	}

	md := BuildCLAUDEmd(bundle)

	if strings.Contains(md, "## Your Role Contract") {
		t.Error("CLAUDE.md should not have role contract when Role is empty")
	}
	if strings.Contains(md, "- **Role**:") {
		t.Error("CLAUDE.md workspace section should not have Role when empty")
	}
}

func TestBuildCLAUDEmd_SectionOrder_WithRoleContract(t *testing.T) {
	bundle := core.ContextBundle{
		Persona:     "# Test Persona",
		PersonaName: "test",
		Objective:   "Do something",
		Domain:      "dev",
		WorkspaceID: "ws-order",
		PhaseID:     "phase-1",
		Role:        core.RoleImplementer,
		Runtime:     core.RuntimeClaude,
	}

	md := BuildCLAUDEmd(bundle)

	taskIdx := strings.Index(md, "## Your Task")
	contractIdx := strings.Index(md, "## Your Role Contract")
	wsIdx := strings.Index(md, "## Workspace")
	outputIdx := strings.Index(md, "## Output")

	if taskIdx < 0 || contractIdx < 0 || wsIdx < 0 || outputIdx < 0 {
		t.Fatalf("missing section(s): task=%d contract=%d ws=%d output=%d", taskIdx, contractIdx, wsIdx, outputIdx)
	}
	if contractIdx <= taskIdx {
		t.Error("role contract should appear after task")
	}
	if wsIdx <= contractIdx {
		t.Error("workspace should appear after role contract")
	}
	if outputIdx <= wsIdx {
		t.Error("output should appear after workspace")
	}
}

// ---------------------------------------------------------------------------
// BuildCLAUDEmd: git no-op constraint is always injected
// ---------------------------------------------------------------------------

// TestBuildCLAUDEmd_GitConstraint_AlwaysPresent verifies that the worker
// CLAUDE.md always contains the git no-op constraint, regardless of whether
// bundle.Constraints is populated. Workers must never create branches, push,
// or open PRs — the orchestrator owns all git operations.
func TestBuildCLAUDEmd_GitConstraint_AlwaysPresent(t *testing.T) {
	tests := []struct {
		name        string
		constraints []string
	}{
		{name: "no bundle constraints", constraints: nil},
		{name: "with bundle constraints", constraints: []string{"Use only standard library", "No external deps"}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			bundle := core.ContextBundle{
				Persona:     "# Test Persona",
				PersonaName: "test",
				Objective:   "Do something",
				Constraints: tt.constraints,
				Domain:      "dev",
				WorkspaceID: "ws-git-constraint",
				PhaseID:     "phase-1",
			}

			md := BuildCLAUDEmd(bundle)

			if !strings.Contains(md, "## Constraints") {
				t.Error("CLAUDE.md must always have Constraints section")
			}
			if !strings.Contains(md, "Do NOT create git branches") {
				t.Errorf("CLAUDE.md missing git no-op constraint; got:\n%s", md)
			}
			for _, c := range tt.constraints {
				if !strings.Contains(md, c) {
					t.Errorf("CLAUDE.md missing bundle constraint %q", c)
				}
			}
		})
	}
}

// mustParseTime parses an RFC3339 timestamp for use in tests.
func mustParseTime(s string) time.Time {
	t, err := time.Parse(time.RFC3339, s)
	if err != nil {
		panic(err)
	}
	return t
}
