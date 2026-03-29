package routing

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"unicode/utf8"

	"github.com/joeyhipolito/orchestrator-cli/internal/audit"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

func TestExtractAuditCorrections_BasicMismatch(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "20260301-abc123",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement feature",
					PersonaAssigned: "senior-frontend-engineer",
					PersonaIdeal:    "senior-backend-engineer",
					PersonaCorrect:  false,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}

	c := corrections[0]
	if c.TargetID != "workspace:20260301-abc123" {
		t.Errorf("TargetID = %q, want workspace:20260301-abc123", c.TargetID)
	}
	if c.AssignedPersona != "senior-frontend-engineer" {
		t.Errorf("AssignedPersona = %q", c.AssignedPersona)
	}
	if c.IdealPersona != "senior-backend-engineer" {
		t.Errorf("IdealPersona = %q", c.IdealPersona)
	}
	if c.TaskHint != "implement feature" {
		t.Errorf("TaskHint = %q", c.TaskHint)
	}
	if c.Source != SourceAudit {
		t.Errorf("Source = %q, want %q", c.Source, SourceAudit)
	}
}

func TestExtractAuditCorrections_SkipsCorrectPhases(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-a",
					PersonaAssigned: "senior-backend-engineer",
					PersonaIdeal:    "senior-backend-engineer",
					PersonaCorrect:  true,
				},
				{
					PhaseName:       "phase-b",
					PersonaAssigned: "wrong",
					PersonaIdeal:    "right",
					PersonaCorrect:  false,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}
	if corrections[0].TaskHint != "phase-b" {
		t.Errorf("TaskHint = %q, want phase-b", corrections[0].TaskHint)
	}
}

func TestExtractAuditCorrections_SkipsEmptyIdeal(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-a",
					PersonaAssigned: "wrong",
					PersonaIdeal:    "", // no ideal specified — low signal
					PersonaCorrect:  false,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 0 {
		t.Errorf("len(corrections) = %d, want 0 (empty ideal is low signal)", len(corrections))
	}
}

func TestExtractAuditCorrections_SkipsEmptyAssigned(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-a",
					PersonaAssigned: "",
					PersonaIdeal:    "right",
					PersonaCorrect:  false,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 0 {
		t.Errorf("len(corrections) = %d, want 0 (empty assigned)", len(corrections))
	}
}

func TestExtractAuditCorrections_DeduplicatesWithinReport(t *testing.T) {
	// Same mismatch appearing twice in the same report (e.g. repeated phase names).
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "wrong",
					PersonaIdeal:    "right",
					PersonaCorrect:  false,
				},
				{
					PhaseName:       "implement",
					PersonaAssigned: "wrong",
					PersonaIdeal:    "right",
					PersonaCorrect:  false,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 1 {
		t.Errorf("len(corrections) = %d, want 1 (deduplication)", len(corrections))
	}
}

func TestExtractAuditCorrections_MultipleReports(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-a",
					PersonaAssigned: "wrong-1",
					PersonaIdeal:    "right-1",
					PersonaCorrect:  false,
				},
			},
		},
		{
			WorkspaceID: "ws2",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-b",
					PersonaAssigned: "wrong-2",
					PersonaIdeal:    "right-2",
					PersonaCorrect:  false,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 2 {
		t.Fatalf("len(corrections) = %d, want 2", len(corrections))
	}
	// nil resolver → both use workspace:<id> fallback
	if corrections[0].TargetID != "workspace:ws1" {
		t.Errorf("corrections[0].TargetID = %q", corrections[0].TargetID)
	}
	if corrections[1].TargetID != "workspace:ws2" {
		t.Errorf("corrections[1].TargetID = %q", corrections[1].TargetID)
	}
}

func TestExtractAuditCorrections_EmptyReports(t *testing.T) {
	corrections := ExtractAuditCorrections(nil, nil)
	if len(corrections) != 0 {
		t.Errorf("len(corrections) = %d, want 0", len(corrections))
	}
}

func TestExtractAuditCorrections_NoMismatches(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-a",
					PersonaAssigned: "right",
					PersonaIdeal:    "right",
					PersonaCorrect:  true,
				},
				{
					PhaseName:       "phase-b",
					PersonaAssigned: "also-right",
					PersonaIdeal:    "also-right",
					PersonaCorrect:  true,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 0 {
		t.Errorf("len(corrections) = %d, want 0", len(corrections))
	}
}

func TestExtractAuditCorrections_DifferentMismatchesSameReport(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-1",
					PersonaAssigned: "wrong-a",
					PersonaIdeal:    "right-a",
					PersonaCorrect:  false,
				},
				{
					PhaseName:       "phase-2",
					PersonaAssigned: "wrong-b",
					PersonaIdeal:    "right-b",
					PersonaCorrect:  false,
				},
			},
		},
	}

	corrections := ExtractAuditCorrections(reports, nil)
	if len(corrections) != 2 {
		t.Fatalf("len(corrections) = %d, want 2 (distinct mismatches)", len(corrections))
	}
}

func TestExtractAuditCorrections_ResolverOverridesTarget(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "20260301-abc123",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement auth",
					PersonaAssigned: "senior-frontend-engineer",
					PersonaIdeal:    "senior-backend-engineer",
					PersonaCorrect:  false,
				},
			},
		},
	}

	// Resolver returns a repo target for this workspace.
	resolver := func(wsID string) string {
		if wsID == "20260301-abc123" {
			return "repo:~/skills/orchestrator"
		}
		return ""
	}

	corrections := ExtractAuditCorrections(reports, resolver)
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}
	if corrections[0].TargetID != "repo:~/skills/orchestrator" {
		t.Errorf("TargetID = %q, want repo:~/skills/orchestrator", corrections[0].TargetID)
	}
}

func TestExtractAuditCorrections_ResolverReturnsEmptyFallsBack(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-unknown",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "phase-x",
					PersonaAssigned: "wrong",
					PersonaIdeal:    "right",
					PersonaCorrect:  false,
				},
			},
		},
	}

	// Resolver returns "" → fallback to workspace:<id>
	resolver := func(wsID string) string { return "" }

	corrections := ExtractAuditCorrections(reports, resolver)
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}
	if corrections[0].TargetID != "workspace:ws-unknown" {
		t.Errorf("TargetID = %q, want workspace:ws-unknown", corrections[0].TargetID)
	}
}

func TestResolveWorkspaceTarget_ReadsTargetFile(t *testing.T) {
	// Create a temp directory structure mimicking ~/.alluka/workspaces/<id>/target_id.
	tmpDir := t.TempDir()
	wsID := "20260301-test1234"
	wsDir := filepath.Join(tmpDir, "workspaces", wsID)
	if err := os.MkdirAll(wsDir, 0700); err != nil {
		t.Fatal(err)
	}

	targetID := "repo:~/skills/orchestrator"
	if err := os.WriteFile(filepath.Join(wsDir, "target_id"), []byte(targetID), 0600); err != nil {
		t.Fatal(err)
	}

	// Override config dir via env var so ResolveWorkspaceTarget reads from tmpDir.
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)

	got := ResolveWorkspaceTarget(wsID)
	if got != targetID {
		t.Errorf("ResolveWorkspaceTarget(%q) = %q, want %q", wsID, got, targetID)
	}
}

func TestResolveWorkspaceTarget_MissingFile(t *testing.T) {
	tmpDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)

	got := ResolveWorkspaceTarget("nonexistent-ws")
	if got != "" {
		t.Errorf("ResolveWorkspaceTarget for missing workspace = %q, want empty", got)
	}
}

func TestResolveWorkspaceTarget_EmptyFile(t *testing.T) {
	tmpDir := t.TempDir()
	wsID := "20260301-empty"
	wsDir := filepath.Join(tmpDir, "workspaces", wsID)
	if err := os.MkdirAll(wsDir, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(wsDir, "target_id"), []byte("  \n"), 0600); err != nil {
		t.Fatal(err)
	}

	t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)

	got := ResolveWorkspaceTarget(wsID)
	if got != "" {
		t.Errorf("ResolveWorkspaceTarget for empty file = %q, want empty", got)
	}
}

// TestCorrectionForRepoTarget_AppearsInDecomposerContext is the end-to-end test:
// store a correction for repo:~/skills/orchestrator via the DB, verify it's
// retrievable for that target (simulating what buildTargetContext does).
func TestCorrectionForRepoTarget_AppearsInDecomposerContext(t *testing.T) {
	// Create temp DB.
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	ctx := context.Background()
	targetID := "repo:~/skills/orchestrator"

	// Insert a correction for the repo target (as audit ingestion would after resolution).
	err = rdb.InsertRoutingCorrection(ctx, RoutingCorrection{
		TargetID:        targetID,
		AssignedPersona: "senior-frontend-engineer",
		IdealPersona:    "senior-backend-engineer",
		TaskHint:        "implement feature",
		Source:          SourceAudit,
	})
	if err != nil {
		t.Fatalf("InsertRoutingCorrection: %v", err)
	}

	// Fetch corrections for the same target (as buildTargetContext does).
	corrections, err := rdb.GetRoutingCorrections(ctx, targetID)
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}

	c := corrections[0]
	if c.TargetID != targetID {
		t.Errorf("TargetID = %q, want %q", c.TargetID, targetID)
	}
	if c.AssignedPersona != "senior-frontend-engineer" {
		t.Errorf("AssignedPersona = %q", c.AssignedPersona)
	}
	if c.IdealPersona != "senior-backend-engineer" {
		t.Errorf("IdealPersona = %q", c.IdealPersona)
	}

	// Verify workspace-scoped query does NOT return the repo correction.
	wsCorrections, err := rdb.GetRoutingCorrections(ctx, "workspace:20260301-abc123")
	if err != nil {
		t.Fatalf("GetRoutingCorrections for workspace: %v", err)
	}
	if len(wsCorrections) != 0 {
		t.Errorf("workspace query returned %d corrections, want 0 (isolation)", len(wsCorrections))
	}
}

// TestExtractAuditCorrections_ResolverMapsToRepoTarget verifies the full flow:
// audit report with workspace ID → resolver maps to repo target → correction
// stored with repo target → retrievable by that target.
func TestExtractAuditCorrections_ResolverMapsToRepoTarget(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	reports := []audit.AuditReport{
		{
			WorkspaceID: "20260301-abc123",
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement auth",
					PersonaAssigned: "senior-frontend-engineer",
					PersonaIdeal:    "senior-backend-engineer",
					PersonaCorrect:  false,
				},
			},
		},
	}

	repoTarget := "repo:~/skills/orchestrator"
	resolver := func(wsID string) string {
		if wsID == "20260301-abc123" {
			return repoTarget
		}
		return ""
	}

	corrections := ExtractAuditCorrections(reports, resolver)
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}

	// Insert into DB and verify retrieval by repo target.
	ctx := context.Background()
	inserted, err := rdb.InsertRoutingCorrections(ctx, corrections)
	if err != nil {
		t.Fatalf("InsertRoutingCorrections: %v", err)
	}
	if inserted != 1 {
		t.Fatalf("inserted = %d, want 1", inserted)
	}

	// Query by repo target — should find the correction.
	got, err := rdb.GetRoutingCorrections(ctx, repoTarget)
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(got) != 1 {
		t.Fatalf("len(got) = %d, want 1", len(got))
	}
	if got[0].AssignedPersona != "senior-frontend-engineer" {
		t.Errorf("AssignedPersona = %q", got[0].AssignedPersona)
	}

	// Query by workspace — should NOT find the correction.
	wsGot, err := rdb.GetRoutingCorrections(ctx, "workspace:20260301-abc123")
	if err != nil {
		t.Fatalf("GetRoutingCorrections for workspace: %v", err)
	}
	if len(wsGot) != 0 {
		t.Errorf("workspace query returned %d, want 0", len(wsGot))
	}
}

// TestFormatRoutingCorrections verifies the prompt section rendering.
func TestFormatRoutingCorrections(t *testing.T) {
	// This test lives here rather than in decompose_test.go because it's
	// testing the corrections→prompt integration and doesn't need LLM mocks.
	// The actual formatRoutingCorrections is in the decompose package, but we
	// can verify the data flow: corrections stored with a repo target flow
	// into CorrectionHint structs that format correctly.
	//
	// We just verify the DB round-trip produces the right data for CorrectionHint.
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	ctx := context.Background()
	target := "repo:~/skills/orchestrator"

	err = rdb.InsertRoutingCorrection(ctx, RoutingCorrection{
		TargetID:        target,
		AssignedPersona: "senior-frontend-engineer",
		IdealPersona:    "senior-backend-engineer",
		TaskHint:        "implement feature",
		Source:          SourceAudit,
	})
	if err != nil {
		t.Fatal(err)
	}

	corrections, err := rdb.GetRoutingCorrections(ctx, target)
	if err != nil {
		t.Fatal(err)
	}

	if len(corrections) != 1 {
		t.Fatalf("got %d corrections, want 1", len(corrections))
	}

	c := corrections[0]
	// Verify fields that would populate CorrectionHint.
	if c.AssignedPersona != "senior-frontend-engineer" {
		t.Errorf("AssignedPersona = %q", c.AssignedPersona)
	}
	if c.IdealPersona != "senior-backend-engineer" {
		t.Errorf("IdealPersona = %q", c.IdealPersona)
	}
	if c.TaskHint != "implement feature" {
		t.Errorf("TaskHint = %q", c.TaskHint)
	}
	if c.Source != SourceAudit {
		t.Errorf("Source = %q", c.Source)
	}
}

// TestExtractAuditCorrections_MultipleReportsWithResolver verifies that the
// resolver is called per-report, and different workspaces can resolve to
// different repo targets.
func TestExtractAuditCorrections_MultipleReportsWithResolver(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-orch",
			Phases: []audit.PhaseEvaluation{{
				PhaseName:       "implement",
				PersonaAssigned: "wrong-1",
				PersonaIdeal:    "right-1",
				PersonaCorrect:  false,
			}},
		},
		{
			WorkspaceID: "ws-engage",
			Phases: []audit.PhaseEvaluation{{
				PhaseName:       "review",
				PersonaAssigned: "wrong-2",
				PersonaIdeal:    "right-2",
				PersonaCorrect:  false,
			}},
		},
		{
			WorkspaceID: "ws-unknown",
			Phases: []audit.PhaseEvaluation{{
				PhaseName:       "deploy",
				PersonaAssigned: "wrong-3",
				PersonaIdeal:    "right-3",
				PersonaCorrect:  false,
			}},
		},
	}

	resolver := func(wsID string) string {
		switch wsID {
		case "ws-orch":
			return "repo:~/skills/orchestrator"
		case "ws-engage":
			return "repo:~/skills/via-engage"
		default:
			return "" // unknown → fallback
		}
	}

	corrections := ExtractAuditCorrections(reports, resolver)
	if len(corrections) != 3 {
		t.Fatalf("len(corrections) = %d, want 3", len(corrections))
	}

	targets := make(map[string]string) // taskHint → targetID
	for _, c := range corrections {
		targets[c.TaskHint] = c.TargetID
	}

	if targets["implement"] != "repo:~/skills/orchestrator" {
		t.Errorf("implement target = %q, want repo:~/skills/orchestrator", targets["implement"])
	}
	if targets["review"] != "repo:~/skills/via-engage" {
		t.Errorf("review target = %q, want repo:~/skills/via-engage", targets["review"])
	}
	if !strings.HasPrefix(targets["deploy"], "workspace:") {
		t.Errorf("deploy target = %q, want workspace: prefix (fallback)", targets["deploy"])
	}
}

// --- ExtractDecompFindings tests ---

func TestExtractDecompFindings_MissingPhases(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"testing phase", "documentation phase"},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	if len(findings) != 2 {
		t.Fatalf("len(findings) = %d, want 2", len(findings))
	}
	for _, f := range findings {
		if f.FindingType != audit.FindingMissingPhase {
			t.Errorf("FindingType = %q, want %q", f.FindingType, audit.FindingMissingPhase)
		}
		if f.AuditScore != 4 {
			t.Errorf("AuditScore = %d, want 4", f.AuditScore)
		}
	}
}

func TestExtractDecompFindings_RedundantWork(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 3},
			Convergence: audit.ConvergenceStatus{
				RedundantWork: []string{"scaffold"},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}
	if findings[0].FindingType != audit.FindingRedundantPhase {
		t.Errorf("FindingType = %q, want %q", findings[0].FindingType, audit.FindingRedundantPhase)
	}
	if findings[0].PhaseName != "scaffold" {
		t.Errorf("PhaseName = %q, want scaffold", findings[0].PhaseName)
	}
}

func TestExtractDecompFindings_DriftPhases(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				DriftPhases: []string{"implement"},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}
	if findings[0].FindingType != audit.FindingPhaseDrift {
		t.Errorf("FindingType = %q, want %q", findings[0].FindingType, audit.FindingPhaseDrift)
	}
}

func TestExtractDecompFindings_WrongPersona(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "frontend",
					PersonaIdeal:    "backend",
					PersonaCorrect:  false,
					Score:           3,
				},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}
	if findings[0].FindingType != audit.FindingWrongPersona {
		t.Errorf("FindingType = %q, want %q", findings[0].FindingType, audit.FindingWrongPersona)
	}
	if !strings.Contains(findings[0].Detail, "frontend") {
		t.Errorf("Detail should mention assigned persona: %q", findings[0].Detail)
	}
}

func TestExtractDecompFindings_LowPhaseScore(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 3},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:      "scaffold",
					PersonaCorrect: true,
					Score:          1,
				},
				{
					PhaseName:      "implement",
					PersonaCorrect: true,
					Score:          4, // good score, should not generate finding
				},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}
	if findings[0].PhaseName != "scaffold" {
		t.Errorf("PhaseName = %q, want scaffold", findings[0].PhaseName)
	}
	if findings[0].FindingType != audit.FindingLowPhaseScore {
		t.Errorf("FindingType = %q, want %q", findings[0].FindingType, audit.FindingLowPhaseScore)
	}
}

func TestExtractDecompFindings_SkipsLowScoringReports(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 2}, // below minScore=3
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"testing phase"},
			},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "wrong",
					PersonaIdeal:    "right",
					PersonaCorrect:  false,
					Score:           1,
				},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	if len(findings) != 0 {
		t.Errorf("len(findings) = %d, want 0 (report below minScore)", len(findings))
	}
}

func TestExtractDecompFindings_EmptyReports(t *testing.T) {
	findings := ExtractDecompFindings(nil, nil, nil, 3)
	if len(findings) != 0 {
		t.Errorf("len(findings) = %d, want 0", len(findings))
	}
}

func TestExtractDecompFindings_ResolverOverridesTarget(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-orch",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"test phase"},
			},
		},
	}

	resolver := func(wsID string) string {
		if wsID == "ws-orch" {
			return "repo:~/skills/orchestrator"
		}
		return ""
	}

	findings := ExtractDecompFindings(reports, resolver, nil, 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}
	if findings[0].TargetID != "repo:~/skills/orchestrator" {
		t.Errorf("TargetID = %q, want repo:~/skills/orchestrator", findings[0].TargetID)
	}
}

func TestExtractDecompFindings_MixedFindingTypes(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"testing"},
				RedundantWork: []string{"scaffold"},
				DriftPhases:   []string{"implement"},
			},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "review",
					PersonaAssigned: "wrong",
					PersonaIdeal:    "right",
					PersonaCorrect:  false,
					Score:           2, // low score + wrong persona = 2 findings
				},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	// 1 missing + 1 redundant + 1 drift + 1 wrong_persona + 1 low_score = 5
	if len(findings) != 5 {
		t.Fatalf("len(findings) = %d, want 5", len(findings))
	}

	typeCounts := make(map[string]int)
	for _, f := range findings {
		typeCounts[f.FindingType]++
	}
	want := map[string]int{
		audit.FindingMissingPhase:   1,
		audit.FindingRedundantPhase: 1,
		audit.FindingPhaseDrift:     1,
		audit.FindingWrongPersona:   1,
		audit.FindingLowPhaseScore:  1,
	}
	for ft, wantCount := range want {
		if typeCounts[ft] != wantCount {
			t.Errorf("finding type %q: got %d, want %d", ft, typeCounts[ft], wantCount)
		}
	}
}

// --- DB round-trip tests for decomposition_findings ---

func TestDecompFindings_InsertAndRetrieve(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	ctx := context.Background()
	target := "repo:~/skills/orchestrator"

	rows := []DecompFindingRow{
		{
			TargetID:    target,
			WorkspaceID: "ws-001",
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: audit.FindingMissingPhase,
				Detail:      "testing phase",
				AuditScore:  4,
			},
		},
		{
			TargetID:    target,
			WorkspaceID: "ws-001",
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: audit.FindingWrongPersona,
				PhaseName:   "implement",
				Detail:      "assigned frontend, ideal backend",
				AuditScore:  4,
			},
		},
	}

	inserted, err := rdb.InsertDecompFindings(ctx, rows)
	if err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}
	if inserted != 2 {
		t.Fatalf("inserted = %d, want 2", inserted)
	}

	// Retrieve by target.
	findings, err := rdb.GetDecompFindings(ctx, target)
	if err != nil {
		t.Fatalf("GetDecompFindings: %v", err)
	}
	if len(findings) != 2 {
		t.Fatalf("len(findings) = %d, want 2", len(findings))
	}

	// Verify newest first (higher ID first).
	if findings[0].FindingType != audit.FindingWrongPersona {
		t.Errorf("findings[0].FindingType = %q, want wrong_persona (newest first)", findings[0].FindingType)
	}

	// Retrieve by workspace.
	wFindings, err := rdb.GetDecompFindingsByWorkspace(ctx, "ws-001")
	if err != nil {
		t.Fatalf("GetDecompFindingsByWorkspace: %v", err)
	}
	if len(wFindings) != 2 {
		t.Fatalf("len(wFindings) = %d, want 2", len(wFindings))
	}
}

func TestDecompFindings_DifferentTargetsIsolated(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	ctx := context.Background()

	rows := []DecompFindingRow{
		{
			TargetID:    "repo:~/a",
			WorkspaceID: "ws-a",
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: audit.FindingMissingPhase,
				Detail:      "test phase",
				AuditScore:  4,
			},
		},
		{
			TargetID:    "repo:~/b",
			WorkspaceID: "ws-b",
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: audit.FindingPhaseDrift,
				PhaseName:   "impl",
				Detail:      "drifted",
				AuditScore:  3,
			},
		},
	}

	_, err = rdb.InsertDecompFindings(ctx, rows)
	if err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}

	aFindings, err := rdb.GetDecompFindings(ctx, "repo:~/a")
	if err != nil {
		t.Fatalf("GetDecompFindings(a): %v", err)
	}
	if len(aFindings) != 1 {
		t.Fatalf("target a: len(findings) = %d, want 1", len(aFindings))
	}

	bFindings, err := rdb.GetDecompFindings(ctx, "repo:~/b")
	if err != nil {
		t.Fatalf("GetDecompFindings(b): %v", err)
	}
	if len(bFindings) != 1 {
		t.Fatalf("target b: len(findings) = %d, want 1", len(bFindings))
	}
}

func TestDecompFindings_SkipsInvalidRows(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	ctx := context.Background()

	rows := []DecompFindingRow{
		{
			TargetID:    "", // invalid: empty target
			WorkspaceID: "ws-001",
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: audit.FindingMissingPhase,
				Detail:      "should be skipped",
			},
		},
		{
			TargetID:    "repo:~/a",
			WorkspaceID: "ws-001",
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: "", // invalid: empty type
				Detail:      "should be skipped",
			},
		},
		{
			TargetID:    "repo:~/a",
			WorkspaceID: "ws-001",
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: audit.FindingMissingPhase,
				Detail:      "should be inserted",
				AuditScore:  3,
			},
		},
	}

	inserted, err := rdb.InsertDecompFindings(ctx, rows)
	if err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}
	if inserted != 1 {
		t.Errorf("inserted = %d, want 1 (2 invalid rows skipped)", inserted)
	}
}

func TestDecompFindings_EmptySlice(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	inserted, err := rdb.InsertDecompFindings(context.Background(), nil)
	if err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}
	if inserted != 0 {
		t.Errorf("inserted = %d, want 0", inserted)
	}
}

// --- ExtractDecompExamples tests ---

// stubPlanLoader returns a WorkspacePlanLoader that returns a fixed plan for
// known workspace IDs, or an error for unknown ones.
func stubPlanLoader(plans map[string]*core.Plan) WorkspacePlanLoader {
	return func(workspaceID string) (*core.Plan, error) {
		p, ok := plans[workspaceID]
		if !ok {
			return nil, fmt.Errorf("workspace %s not found", workspaceID)
		}
		return p, nil
	}
}

func TestExtractDecompExamples_BasicExtraction(t *testing.T) {
	plans := map[string]*core.Plan{
		"ws-good": {
			Task:          "implement a REST API with CRUD endpoints for user management",
			DecompSource:  "predecomposed",
			ExecutionMode: "sequential",
			Phases: []*core.Phase{
				{Name: "research", Objective: "research REST patterns", Persona: "architect"},
				{Name: "implement", Objective: "build endpoints", Persona: "senior-backend-engineer"},
				{Name: "review", Objective: "code review", Persona: "staff-code-reviewer"},
			},
		},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-good",
			Scorecard: audit.Scorecard{
				Overall:              4,
				DecompositionQuality: 4,
				PersonaFit:           5,
			},
		},
	}

	exResult := ExtractDecompExamples(reports, nil, stubPlanLoader(plans), 3)
	if len(exResult.Examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1", len(exResult.Examples))
	}

	ex := exResult.Examples[0]
	if ex.PhaseCount != 3 {
		t.Errorf("PhaseCount = %d, want 3", ex.PhaseCount)
	}
	if ex.DecompSource != "predecomposed" {
		t.Errorf("DecompSource = %q, want predecomposed", ex.DecompSource)
	}
	if ex.AuditScore != 4 {
		t.Errorf("AuditScore = %d, want 4", ex.AuditScore)
	}
	if ex.DecompQuality != 4 {
		t.Errorf("DecompQuality = %d, want 4", ex.DecompQuality)
	}
	if !strings.Contains(ex.PhasesJSON, "research") {
		t.Errorf("PhasesJSON should contain 'research': %s", ex.PhasesJSON)
	}
	if !strings.Contains(ex.TaskSummary, "REST API") {
		t.Errorf("TaskSummary should contain 'REST API': %s", ex.TaskSummary)
	}
}

func TestExtractDecompExamples_ScoreGateFiltersLow(t *testing.T) {
	plans := map[string]*core.Plan{
		"ws-low-overall": {Task: "low overall", DecompSource: "llm", Phases: []*core.Phase{{Name: "a"}}},
		"ws-low-decomp":  {Task: "low decomp", DecompSource: "llm", Phases: []*core.Phase{{Name: "b"}}},
		"ws-good":        {Task: "good", DecompSource: "llm", Phases: []*core.Phase{{Name: "c"}}},
	}

	reports := []audit.AuditReport{
		{WorkspaceID: "ws-low-overall", Scorecard: audit.Scorecard{Overall: 2, DecompositionQuality: 4}},
		{WorkspaceID: "ws-low-decomp", Scorecard: audit.Scorecard{Overall: 4, DecompositionQuality: 2}},
		{WorkspaceID: "ws-good", Scorecard: audit.Scorecard{Overall: 3, DecompositionQuality: 3}},
	}

	exResult := ExtractDecompExamples(reports, nil, stubPlanLoader(plans), 3)
	if len(exResult.Examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1 (only ws-good passes both gates)", len(exResult.Examples))
	}
	if exResult.Examples[0].WorkspaceID != "ws-good" {
		t.Errorf("WorkspaceID = %q, want ws-good", exResult.Examples[0].WorkspaceID)
	}
}

func TestExtractDecompExamples_MissingWorkspaceSkipped(t *testing.T) {
	// Loader returns error for unknown workspaces.
	reports := []audit.AuditReport{
		{WorkspaceID: "ws-missing", Scorecard: audit.Scorecard{Overall: 5, DecompositionQuality: 5}},
	}

	exResult := ExtractDecompExamples(reports, nil, stubPlanLoader(nil), 3)
	if len(exResult.Examples) != 0 {
		t.Errorf("len(examples) = %d, want 0 (missing workspace)", len(exResult.Examples))
	}
	if exResult.Skipped != 1 {
		t.Errorf("Skipped = %d, want 1", exResult.Skipped)
	}
}

func TestExtractDecompExamples_ResolverMapsTarget(t *testing.T) {
	plans := map[string]*core.Plan{
		"ws-1": {Task: "task", DecompSource: "llm", Phases: []*core.Phase{{Name: "do"}}},
	}

	reports := []audit.AuditReport{
		{WorkspaceID: "ws-1", Scorecard: audit.Scorecard{Overall: 4, DecompositionQuality: 4}},
	}

	resolver := func(wsID string) string {
		if wsID == "ws-1" {
			return "repo:~/skills/orchestrator"
		}
		return ""
	}

	exResult := ExtractDecompExamples(reports, resolver, stubPlanLoader(plans), 3)
	if len(exResult.Examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1", len(exResult.Examples))
	}
	if exResult.Examples[0].TargetID != "repo:~/skills/orchestrator" {
		t.Errorf("TargetID = %q, want repo:~/skills/orchestrator", exResult.Examples[0].TargetID)
	}
}

func TestExtractDecompExamples_TaskSummaryTruncated(t *testing.T) {
	longTask := strings.Repeat("x", 300)
	plans := map[string]*core.Plan{
		"ws-1": {Task: longTask, DecompSource: "llm", Phases: []*core.Phase{{Name: "do"}}},
	}

	reports := []audit.AuditReport{
		{WorkspaceID: "ws-1", Scorecard: audit.Scorecard{Overall: 4, DecompositionQuality: 4}},
	}

	exResult := ExtractDecompExamples(reports, nil, stubPlanLoader(plans), 3)
	if len(exResult.Examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1", len(exResult.Examples))
	}
	if len(exResult.Examples[0].TaskSummary) != 200 {
		t.Errorf("TaskSummary length = %d, want 200 (truncated)", len(exResult.Examples[0].TaskSummary))
	}
}

func TestExtractDecompExamples_EmptyReports(t *testing.T) {
	exResult := ExtractDecompExamples(nil, nil, stubPlanLoader(nil), 3)
	if len(exResult.Examples) != 0 {
		t.Errorf("len(examples) = %d, want 0", len(exResult.Examples))
	}
}

func TestExtractDecompExamples_PredecomposedContributes(t *testing.T) {
	// High-signal predecomposed missions should contribute to the store.
	plans := map[string]*core.Plan{
		"ws-predecomp": {
			Task:          "migrate database schema with zero downtime",
			DecompSource:  "predecomposed",
			ExecutionMode: "sequential",
			Phases: []*core.Phase{
				{Name: "plan-migration", Objective: "design migration steps", Persona: "architect"},
				{Name: "implement-migration", Objective: "write migration code", Persona: "senior-backend-engineer"},
				{Name: "test-migration", Objective: "run migration in staging", Persona: "qa-engineer"},
				{Name: "review", Objective: "final review", Persona: "staff-code-reviewer"},
			},
		},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-predecomp",
			Scorecard: audit.Scorecard{
				Overall:              5,
				DecompositionQuality: 5,
				PersonaFit:           4,
			},
		},
	}

	exResult := ExtractDecompExamples(reports, nil, stubPlanLoader(plans), 3)
	if len(exResult.Examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1", len(exResult.Examples))
	}
	if exResult.Examples[0].DecompSource != "predecomposed" {
		t.Errorf("DecompSource = %q, want predecomposed", exResult.Examples[0].DecompSource)
	}
	if exResult.Examples[0].PhaseCount != 4 {
		t.Errorf("PhaseCount = %d, want 4", exResult.Examples[0].PhaseCount)
	}
}

// --- decomp_source preservation in findings ---

func TestExtractDecompFindings_PreservesDecompSource(t *testing.T) {
	plans := map[string]*core.Plan{
		"ws-llm": {
			Task:         "build feature",
			DecompSource: "llm",
			Phases:       []*core.Phase{{Name: "implement"}},
		},
		"ws-predecomp": {
			Task:         "migrate schema",
			DecompSource: "predecomposed",
			Phases:       []*core.Phase{{Name: "plan"}, {Name: "execute"}},
		},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-llm",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"testing phase"},
			},
		},
		{
			WorkspaceID: "ws-predecomp",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				RedundantWork: []string{"plan"},
			},
		},
	}

	loader := stubPlanLoader(plans)
	findings := ExtractDecompFindings(reports, nil, loader, 3)
	if len(findings) != 2 {
		t.Fatalf("len(findings) = %d, want 2", len(findings))
	}

	// First finding should have decomp_source from ws-llm's plan.
	if findings[0].DecompSource != "llm" {
		t.Errorf("findings[0].DecompSource = %q, want llm", findings[0].DecompSource)
	}
	// Second finding should have decomp_source from ws-predecomp's plan.
	if findings[1].DecompSource != "predecomposed" {
		t.Errorf("findings[1].DecompSource = %q, want predecomposed", findings[1].DecompSource)
	}
}

func TestExtractDecompFindings_NilLoaderLeavesDecompSourceEmpty(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"testing"},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, nil, 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}
	if findings[0].DecompSource != "" {
		t.Errorf("DecompSource = %q, want empty (nil loader)", findings[0].DecompSource)
	}
}

func TestExtractDecompFindings_LoaderErrorLeavesDecompSourceEmpty(t *testing.T) {
	// Loader fails for this workspace — decomp_source should be empty, not error.
	loader := func(wsID string) (*core.Plan, error) {
		return nil, fmt.Errorf("workspace %s not found", wsID)
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-missing",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:      "implement",
					PersonaCorrect: true,
					Score:          1, // low score → generates finding
				},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, loader, 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}
	if findings[0].DecompSource != "" {
		t.Errorf("DecompSource = %q, want empty (loader error)", findings[0].DecompSource)
	}
}

func TestExtractDecompFindings_DecompSourcePersistedToDB(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "learnings.db")
	rdb, err := OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	defer rdb.Close()

	plans := map[string]*core.Plan{
		"ws-src": {Task: "task", DecompSource: "predecomposed", Phases: []*core.Phase{{Name: "do"}}},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-src",
			Scorecard:   audit.Scorecard{Overall: 4},
			Convergence: audit.ConvergenceStatus{
				DriftPhases: []string{"do"},
			},
		},
	}

	findings := ExtractDecompFindings(reports, nil, stubPlanLoader(plans), 3)
	if len(findings) != 1 {
		t.Fatalf("len(findings) = %d, want 1", len(findings))
	}

	ctx := context.Background()
	inserted, err := rdb.InsertDecompFindings(ctx, findings)
	if err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}
	if inserted != 1 {
		t.Fatalf("inserted = %d, want 1", inserted)
	}

	// Read back and verify decomp_source survived the round-trip.
	got, err := rdb.GetDecompFindingsByWorkspace(ctx, "ws-src")
	if err != nil {
		t.Fatalf("GetDecompFindingsByWorkspace: %v", err)
	}
	if len(got) != 1 {
		t.Fatalf("len(got) = %d, want 1", len(got))
	}
	if got[0].DecompSource != "predecomposed" {
		t.Errorf("DecompSource after round-trip = %q, want predecomposed", got[0].DecompSource)
	}
}

// --- IngestAuditReports (shared ingestion) tests ---

func TestIngestAuditReports_EmptyReports(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	result, err := IngestAuditReports(ctx, rdb, nil, nil, nil)
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}
	if result.Corrections != 0 || result.Findings != 0 || result.Examples != 0 {
		t.Errorf("expected all zeros, got %+v", result)
	}
}

func TestIngestAuditReports_IngestsAllThreeTypes(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plans := map[string]*core.Plan{
		"ws-good": {
			Task:          "implement feature",
			DecompSource:  "llm",
			ExecutionMode: "sequential",
			Phases: []*core.Phase{
				{Name: "implement", Objective: "build it", Persona: "senior-backend-engineer"},
				{Name: "review", Objective: "review it", Persona: "staff-code-reviewer"},
			},
		},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-good",
			Scorecard: audit.Scorecard{
				Overall:              4,
				DecompositionQuality: 4,
				PersonaFit:           4,
			},
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"testing phase"},
			},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "senior-frontend-engineer",
					PersonaIdeal:    "senior-backend-engineer",
					PersonaCorrect:  false,
					Score:           3,
				},
			},
		},
	}

	result, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(plans))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	// Should have: 1 correction (persona mismatch).
	if result.Corrections != 1 {
		t.Errorf("Corrections = %d, want 1", result.Corrections)
	}

	// Should have: 2 findings (1 missing_phase + 1 wrong_persona).
	if result.Findings != 2 {
		t.Errorf("Findings = %d, want 2", result.Findings)
	}

	// Should have: 1 example (passes quality gate).
	if result.Examples != 1 {
		t.Errorf("Examples = %d, want 1", result.Examples)
	}

	// Verify findings have decomp_source set.
	findings, err := rdb.GetDecompFindings(ctx, "workspace:ws-good")
	if err != nil {
		t.Fatalf("GetDecompFindings: %v", err)
	}
	for _, f := range findings {
		if f.DecompSource != "llm" {
			t.Errorf("finding %q DecompSource = %q, want llm", f.FindingType, f.DecompSource)
		}
	}

	// Verify example has decomp_source set.
	examples, err := rdb.GetDecompExamples(ctx, "workspace:ws-good", 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples: %v", err)
	}
	if len(examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1", len(examples))
	}
	if examples[0].DecompSource != "llm" {
		t.Errorf("example DecompSource = %q, want llm", examples[0].DecompSource)
	}
}

func TestIngestAuditReports_Idempotent(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plans := map[string]*core.Plan{
		"ws-1": {Task: "task", DecompSource: "llm", ExecutionMode: "sequential", Phases: []*core.Phase{{Name: "do", Objective: "obj", Persona: "p"}}},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-1",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 4, PersonaFit: 4},
			Convergence: audit.ConvergenceStatus{MissingPhases: []string{"test"}},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "do", PersonaAssigned: "wrong", PersonaIdeal: "right", PersonaCorrect: false, Score: 3},
			},
		},
	}

	loader := stubPlanLoader(plans)

	// First ingest.
	r1, err := IngestAuditReports(ctx, rdb, reports, nil, loader)
	if err != nil {
		t.Fatalf("first ingest: %v", err)
	}
	if r1.Corrections == 0 {
		t.Error("first ingest: expected at least 1 correction")
	}

	// Second ingest — duplicates should be skipped.
	r2, err := IngestAuditReports(ctx, rdb, reports, nil, loader)
	if err != nil {
		t.Fatalf("second ingest: %v", err)
	}

	// Corrections should be 0 (all duplicates).
	if r2.Corrections != 0 {
		t.Errorf("second ingest: Corrections = %d, want 0 (duplicates)", r2.Corrections)
	}
	if r2.CorrectionsDupes == 0 {
		t.Error("second ingest: expected CorrectionsDupes > 0")
	}
}

func TestIngestAuditReports_LowScoreSkipsFindingsAndExamples(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plans := map[string]*core.Plan{
		"ws-low": {Task: "bad task", DecompSource: "llm", Phases: []*core.Phase{{Name: "do"}}},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-low",
			Scorecard:   audit.Scorecard{Overall: 2, DecompositionQuality: 2},
			Convergence: audit.ConvergenceStatus{MissingPhases: []string{"test"}},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "do", PersonaAssigned: "wrong", PersonaIdeal: "right", PersonaCorrect: false, Score: 1},
			},
		},
	}

	result, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(plans))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	// Corrections have no score gate — persona mismatch should still be ingested.
	if result.Corrections != 1 {
		t.Errorf("Corrections = %d, want 1 (no score gate for corrections)", result.Corrections)
	}

	// Findings and examples should be 0 (below DefaultMinAuditScore=3).
	if result.Findings != 0 {
		t.Errorf("Findings = %d, want 0 (below score gate)", result.Findings)
	}
	if result.Examples != 0 {
		t.Errorf("Examples = %d, want 0 (below score gate)", result.Examples)
	}
}

func TestIngestAuditReports_ResolverMapsTarget(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plans := map[string]*core.Plan{
		"ws-orch": {Task: "fix bug", DecompSource: "llm", ExecutionMode: "sequential", Phases: []*core.Phase{{Name: "fix", Objective: "fix it", Persona: "backend"}}},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-orch",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 4, PersonaFit: 4},
			Convergence: audit.ConvergenceStatus{MissingPhases: []string{"test"}},
		},
	}

	resolver := func(wsID string) string {
		if wsID == "ws-orch" {
			return "repo:~/skills/orchestrator"
		}
		return ""
	}

	result, err := IngestAuditReports(ctx, rdb, reports, resolver, stubPlanLoader(plans))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	if result.Findings != 1 {
		t.Fatalf("Findings = %d, want 1", result.Findings)
	}

	// Verify the finding was stored under the resolved repo target, not workspace target.
	findings, err := rdb.GetDecompFindings(ctx, "repo:~/skills/orchestrator")
	if err != nil {
		t.Fatalf("GetDecompFindings: %v", err)
	}
	if len(findings) != 1 {
		t.Fatalf("findings for repo target: len = %d, want 1", len(findings))
	}

	// Workspace target should have nothing.
	wFindings, err := rdb.GetDecompFindings(ctx, "workspace:ws-orch")
	if err != nil {
		t.Fatalf("GetDecompFindings(workspace): %v", err)
	}
	if len(wFindings) != 0 {
		t.Errorf("workspace target has %d findings, want 0 (resolved to repo)", len(wFindings))
	}
}

// --- Regression tests: timeout decoupling and missing-workspace behavior ---

func TestIngestAuditReports_CancelledContextFailsIngestion(t *testing.T) {
	// Regression: proves that a cancelled context (simulating exhausted audit
	// timeout) causes ingestion to fail, motivating the decoupled-context fix.
	rdb := newTestDB(t)

	ctx, cancel := context.WithCancel(context.Background())
	cancel() // cancel immediately — simulates exhausted evaluation timeout

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-cancelled",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "do", PersonaAssigned: "wrong", PersonaIdeal: "right", PersonaCorrect: false},
			},
		},
	}

	_, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(nil))
	if err == nil {
		t.Fatal("expected error from cancelled context, got nil")
	}
	if !strings.Contains(err.Error(), "context canceled") {
		t.Errorf("expected context canceled error, got: %v", err)
	}
}

func TestIngestAuditReports_FreshContextSucceedsAfterCancel(t *testing.T) {
	// Regression: proves the decoupling fix works — a fresh context succeeds
	// even though a hypothetical prior context was cancelled.
	rdb := newTestDB(t)

	// Simulate: evaluation context is exhausted.
	evalCtx, evalCancel := context.WithCancel(context.Background())
	evalCancel()

	// Verify the eval context is indeed dead.
	if evalCtx.Err() == nil {
		t.Fatal("evalCtx should be cancelled")
	}

	// Decoupled ingest context — fresh, independent.
	ingestCtx := context.Background()

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-decoupled",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "do", PersonaAssigned: "wrong", PersonaIdeal: "right", PersonaCorrect: false},
			},
		},
	}

	result, err := IngestAuditReports(ingestCtx, rdb, reports, nil, stubPlanLoader(nil))
	if err != nil {
		t.Fatalf("IngestAuditReports with fresh context: %v", err)
	}

	// Correction should succeed (no workspace needed for corrections).
	if result.Corrections != 1 {
		t.Errorf("Corrections = %d, want 1", result.Corrections)
	}
}

func TestIngestAuditReports_MissingWorkspaceSkipsExamplesNotCorrections(t *testing.T) {
	// Regression: when workspaces have been cleaned up, corrections and
	// findings from the audit report itself should still be ingested.
	// Only examples (which need the checkpoint) should be skipped.
	rdb := newTestDB(t)
	ctx := context.Background()

	// No plans provided — all workspace loads will fail.
	emptyLoader := stubPlanLoader(nil)

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-cleaned",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 4, PersonaFit: 4},
			Convergence: audit.ConvergenceStatus{
				MissingPhases: []string{"testing"},
			},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "impl", PersonaAssigned: "frontend", PersonaIdeal: "backend", PersonaCorrect: false, Score: 3},
			},
		},
	}

	result, err := IngestAuditReports(ctx, rdb, reports, nil, emptyLoader)
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	// Corrections: 1 (persona mismatch — no workspace needed).
	if result.Corrections != 1 {
		t.Errorf("Corrections = %d, want 1", result.Corrections)
	}

	// Findings: 2 (missing_phase + wrong_persona — workspace only needed for decomp_source).
	if result.Findings != 2 {
		t.Errorf("Findings = %d, want 2", result.Findings)
	}

	// Examples: 0 (workspace missing).
	if result.Examples != 0 {
		t.Errorf("Examples = %d, want 0 (workspace missing)", result.Examples)
	}

	// ExamplesSkipped: 1 (report passed quality gate but workspace was missing).
	if result.ExamplesSkipped != 1 {
		t.Errorf("ExamplesSkipped = %d, want 1", result.ExamplesSkipped)
	}
}

func TestIngestAuditReports_MixedAvailableAndMissingWorkspaces(t *testing.T) {
	// Regression: backfill with a mix of available and cleaned-up workspaces
	// should ingest examples from available ones and report skipped count for missing ones.
	rdb := newTestDB(t)
	ctx := context.Background()

	plans := map[string]*core.Plan{
		"ws-available": {
			Task:          "available task",
			DecompSource:  "llm",
			ExecutionMode: "sequential",
			Phases:        []*core.Phase{{Name: "do", Objective: "obj", Persona: "backend"}},
		},
		// ws-gone is NOT in the map — simulates cleaned-up workspace
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-available",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 4, PersonaFit: 4},
		},
		{
			WorkspaceID: "ws-gone",
			Scorecard:   audit.Scorecard{Overall: 5, DecompositionQuality: 5, PersonaFit: 5},
		},
	}

	result, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(plans))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	if result.Examples != 1 {
		t.Errorf("Examples = %d, want 1 (ws-available)", result.Examples)
	}
	if result.ExamplesSkipped != 1 {
		t.Errorf("ExamplesSkipped = %d, want 1 (ws-gone)", result.ExamplesSkipped)
	}
}

// --- Targeted regression tests for the two robustness fixes ---

// TestExtractDecompExamples_TaskSummaryTruncated_Unicode is the targeted
// regression test for the UTF-8 truncation fix.
//
// Before the fix, ExtractDecompExamples used byte-level slicing:
//
//	taskSummary = taskSummary[:200]
//
// Japanese hiragana 'あ' is 3 bytes in UTF-8. A 300-rune string is 900 bytes.
// Slicing at byte 200 falls inside the 67th character (byte 198..200), which
// produces an invalid UTF-8 sequence. The fix uses rune-level slicing:
//
//	taskSummary = string([]rune(taskSummary)[:200])
//
// This test fails on the old code (invalid UTF-8, wrong rune count) and passes
// on the new code (valid UTF-8, exactly 200 runes, 600 bytes).
func TestExtractDecompExamples_TaskSummaryTruncated_Unicode(t *testing.T) {
	// あ is U+3042, encoded as 0xE3 0x81 0x82 (3 bytes in UTF-8).
	// 300 repetitions = 900 bytes, well above the 200-rune limit.
	longTask := strings.Repeat("あ", 300)
	plans := map[string]*core.Plan{
		"ws-unicode": {Task: longTask, DecompSource: "llm", Phases: []*core.Phase{{Name: "do"}}},
	}

	reports := []audit.AuditReport{
		{WorkspaceID: "ws-unicode", Scorecard: audit.Scorecard{Overall: 4, DecompositionQuality: 4}},
	}

	exResult := ExtractDecompExamples(reports, nil, stubPlanLoader(plans), 3)
	if len(exResult.Examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1", len(exResult.Examples))
	}

	summary := exResult.Examples[0].TaskSummary

	// Must be valid UTF-8 — byte-level truncation would split a 3-byte sequence.
	if !utf8.ValidString(summary) {
		t.Error("truncated TaskSummary is invalid UTF-8: byte-level truncation bug is present")
	}

	// Must be exactly 200 runes (not 200 bytes).
	runeCount := utf8.RuneCountInString(summary)
	if runeCount != 200 {
		t.Errorf("rune count = %d, want 200 (rune-safe truncation)", runeCount)
	}

	// Byte length must be 600 (200 runes × 3 bytes each for hiragana).
	if len(summary) != 600 {
		t.Errorf("byte length = %d, want 600 (200 × 3-byte hiragana chars)", len(summary))
	}
}

// TestIngestAuditReports_CancelledContextDuringExampleInsert is the targeted
// regression test for the context-cancellation propagation fix in the example
// insertion loop.
//
// Before the fix, the loop used a bare 'continue' for all errors:
//
//	if err := rdb.InsertDecompExample(ctx, ex); err != nil {
//	    continue // silent swallow — context cancellation disappeared here
//	}
//
// After the fix, context errors propagate:
//
//	if ctx.Err() != nil {
//	    return result, fmt.Errorf("inserting decomposition example: %w", err)
//	}
//
// To isolate the example loop, this test produces a report with:
//   - PersonaCorrect=true (no corrections → InsertRoutingCorrections is skipped)
//   - No convergence issues, Score=4 (no findings → InsertDecompFindings is skipped)
//   - Overall=4, DecompQuality=4 (passes example quality gate)
//   - Plan available (ExtractDecompExamples returns an example to insert)
//
// With a pre-cancelled context, InsertDecompExample fails and the error must
// propagate rather than being swallowed.
func TestIngestAuditReports_CancelledContextDuringExampleInsert(t *testing.T) {
	rdb := newTestDB(t)

	ctx, cancel := context.WithCancel(context.Background())
	cancel() // cancelled before any DB call

	plans := map[string]*core.Plan{
		"ws-good": {
			Task:          "implement REST API endpoints",
			DecompSource:  "llm",
			ExecutionMode: "sequential",
			Phases: []*core.Phase{
				{Name: "implement", Objective: "build it", Persona: "senior-backend-engineer"},
			},
		},
	}

	// PersonaCorrect=true → no correction extracted.
	// No convergence issues, Score=4 → no findings extracted.
	// Overall=4, DecompQuality=4 → passes example quality gate.
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-good",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 4, PersonaFit: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement", PersonaCorrect: true, Score: 4},
			},
		},
	}

	_, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(plans))
	if err == nil {
		t.Fatal("expected error from cancelled context during example insert, got nil")
	}
	if !strings.Contains(err.Error(), "context canceled") {
		t.Errorf("expected context canceled error, got: %v", err)
	}
}

// --- ExtractRoutingPatterns tests ---

func TestExtractRoutingPatterns_CorrectPhasesExtracted(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "senior-backend-engineer",
					PersonaCorrect:  true,
				},
				{
					PhaseName:       "review",
					PersonaAssigned: "staff-code-reviewer",
					PersonaCorrect:  true,
				},
			},
		},
	}

	patterns := ExtractRoutingPatterns(reports, nil, 3)
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2", len(patterns))
	}
	if patterns[0].Persona != "senior-backend-engineer" {
		t.Errorf("patterns[0].Persona = %q", patterns[0].Persona)
	}
	if patterns[0].TaskHint != "implement" {
		t.Errorf("patterns[0].TaskHint = %q", patterns[0].TaskHint)
	}
	if patterns[1].Persona != "staff-code-reviewer" {
		t.Errorf("patterns[1].Persona = %q", patterns[1].Persona)
	}
}

func TestExtractRoutingPatterns_SkipsIncorrectPhases(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "wrong-persona",
					PersonaIdeal:    "right-persona",
					PersonaCorrect:  false,
				},
				{
					PhaseName:       "review",
					PersonaAssigned: "staff-code-reviewer",
					PersonaCorrect:  true,
				},
			},
		},
	}

	patterns := ExtractRoutingPatterns(reports, nil, 3)
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (incorrect phase skipped)", len(patterns))
	}
	if patterns[0].Persona != "staff-code-reviewer" {
		t.Errorf("patterns[0].Persona = %q", patterns[0].Persona)
	}
}

func TestExtractRoutingPatterns_SkipsLowScoringReports(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 2},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "senior-backend-engineer",
					PersonaCorrect:  true,
				},
			},
		},
	}

	patterns := ExtractRoutingPatterns(reports, nil, 3)
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0 (low score)", len(patterns))
	}
}

func TestExtractRoutingPatterns_SkipsEmptyPersona(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{
					PhaseName:       "implement",
					PersonaAssigned: "",
					PersonaCorrect:  true,
				},
			},
		},
	}

	patterns := ExtractRoutingPatterns(reports, nil, 3)
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0 (empty persona)", len(patterns))
	}
}

func TestExtractRoutingPatterns_Deduplicates(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractRoutingPatterns(reports, nil, 3)
	if len(patterns) != 1 {
		t.Errorf("len(patterns) = %d, want 1 (deduplicated)", len(patterns))
	}
}

func TestExtractRoutingPatterns_ResolverMapsTarget(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-orch",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
			},
		},
	}

	resolver := func(wsID string) string {
		if wsID == "ws-orch" {
			return "repo:~/skills/orchestrator"
		}
		return ""
	}

	patterns := ExtractRoutingPatterns(reports, resolver, 3)
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].TargetID != "repo:~/skills/orchestrator" {
		t.Errorf("TargetID = %q, want repo:~/skills/orchestrator", patterns[0].TargetID)
	}
}

func TestExtractRoutingPatterns_EmptyReports(t *testing.T) {
	patterns := ExtractRoutingPatterns(nil, nil, 3)
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0", len(patterns))
	}
}

// --- ExtractHandoffPatterns tests ---

func TestExtractHandoffPatterns_ConsecutiveTransitions(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: "senior-backend-engineer", PersonaCorrect: true},
				{PhaseName: "review", PersonaAssigned: "staff-code-reviewer", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractHandoffPatterns(reports, nil, 3)
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2", len(patterns))
	}

	// architect → senior-backend-engineer
	if patterns[0].FromPersona != "architect" || patterns[0].ToPersona != "senior-backend-engineer" {
		t.Errorf("patterns[0] = %q->%q, want architect->senior-backend-engineer",
			patterns[0].FromPersona, patterns[0].ToPersona)
	}
	if patterns[0].TaskHint != "implement" {
		t.Errorf("patterns[0].TaskHint = %q, want implement", patterns[0].TaskHint)
	}

	// senior-backend-engineer → staff-code-reviewer
	if patterns[1].FromPersona != "senior-backend-engineer" || patterns[1].ToPersona != "staff-code-reviewer" {
		t.Errorf("patterns[1] = %q->%q, want senior-backend-engineer->staff-code-reviewer",
			patterns[1].FromPersona, patterns[1].ToPersona)
	}
}

func TestExtractHandoffPatterns_SkipsSelfTransitions(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement-1", PersonaAssigned: "backend", PersonaCorrect: true},
				{PhaseName: "implement-2", PersonaAssigned: "backend", PersonaCorrect: true}, // same persona
				{PhaseName: "review", PersonaAssigned: "reviewer", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractHandoffPatterns(reports, nil, 3)
	// Only backend→reviewer (implement-2→review), not backend→backend
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (self-transition skipped)", len(patterns))
	}
	if patterns[0].FromPersona != "backend" || patterns[0].ToPersona != "reviewer" {
		t.Errorf("patterns[0] = %q->%q", patterns[0].FromPersona, patterns[0].ToPersona)
	}
}

func TestExtractHandoffPatterns_SkipsEmptyPersona(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: ""}, // empty
				{PhaseName: "review", PersonaAssigned: "reviewer", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractHandoffPatterns(reports, nil, 3)
	// architect→"" skipped, ""→reviewer skipped → no patterns
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0 (empty persona gaps)", len(patterns))
	}
}

func TestExtractHandoffPatterns_SkipsIncorrectPersonaTransitions(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: "senior-frontend-engineer", PersonaCorrect: false},
				{PhaseName: "review", PersonaAssigned: "staff-code-reviewer", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractHandoffPatterns(reports, nil, 3)
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0 when transition passes through wrong persona", len(patterns))
	}
}

func TestExtractHandoffPatterns_SinglePhaseNoHandoff(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractHandoffPatterns(reports, nil, 3)
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0 (single phase)", len(patterns))
	}
}

func TestExtractHandoffPatterns_SkipsLowScoringReports(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 2},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractHandoffPatterns(reports, nil, 3)
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0 (low score)", len(patterns))
	}
}

func TestExtractHandoffPatterns_Deduplicates(t *testing.T) {
	// Same transition appears in two reports for the same target.
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
			},
		},
		{
			WorkspaceID: "ws1", // same workspace → same target
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
			},
		},
	}

	patterns := ExtractHandoffPatterns(reports, nil, 3)
	if len(patterns) != 1 {
		t.Errorf("len(patterns) = %d, want 1 (deduplicated)", len(patterns))
	}
}

func TestExtractHandoffPatterns_EmptyReports(t *testing.T) {
	patterns := ExtractHandoffPatterns(nil, nil, 3)
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0", len(patterns))
	}
}

func TestExtractHandoffPatterns_ResolverMapsTarget(t *testing.T) {
	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-orch",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true},
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true},
			},
		},
	}

	resolver := func(wsID string) string {
		if wsID == "ws-orch" {
			return "repo:~/skills/orchestrator"
		}
		return ""
	}

	patterns := ExtractHandoffPatterns(reports, resolver, 3)
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].TargetID != "repo:~/skills/orchestrator" {
		t.Errorf("TargetID = %q, want repo:~/skills/orchestrator", patterns[0].TargetID)
	}
}

// --- IngestAuditReports: routing & handoff pattern integration ---

func TestIngestAuditReports_IngestsRoutingPatterns(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 2},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement", PersonaAssigned: "senior-backend-engineer", PersonaCorrect: true, Score: 4},
				{PhaseName: "review", PersonaAssigned: "staff-code-reviewer", PersonaCorrect: true, Score: 4},
			},
		},
	}

	result, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(nil))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	if result.RoutingPatterns != 2 {
		t.Errorf("RoutingPatterns = %d, want 2", result.RoutingPatterns)
	}

	// Verify patterns are persisted and retrievable.
	patterns, err := rdb.GetRoutingPatterns(ctx, "workspace:ws1")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2", len(patterns))
	}

	// Verify confidence starts at 0.2 for first observation.
	for _, p := range patterns {
		if p.Confidence < 0.19 || p.Confidence > 0.21 {
			t.Errorf("pattern %q confidence = %f, want ~0.2", p.Persona, p.Confidence)
		}
		if p.SeenCount != 1 {
			t.Errorf("pattern %q seen_count = %d, want 1", p.Persona, p.SeenCount)
		}
	}
}

func TestIngestAuditReports_IngestsHandoffPatterns(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4, DecompositionQuality: 2},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true, Score: 4},
				{PhaseName: "implement", PersonaAssigned: "senior-backend-engineer", PersonaCorrect: true, Score: 4},
				{PhaseName: "review", PersonaAssigned: "staff-code-reviewer", PersonaCorrect: true, Score: 4},
			},
		},
	}

	result, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(nil))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	if result.HandoffPatterns != 2 {
		t.Errorf("HandoffPatterns = %d, want 2", result.HandoffPatterns)
	}

	// Verify patterns are persisted.
	handoffs, err := rdb.GetHandoffPatterns(ctx, "workspace:ws1")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(handoffs) != 2 {
		t.Fatalf("len(handoffs) = %d, want 2", len(handoffs))
	}
}

func TestIngestAuditReports_PatternsIdempotentWithConfidenceGrowth(t *testing.T) {
	// Regression: ingesting the same report twice should increment seen_count
	// and grow confidence, not create duplicates.
	rdb := newTestDB(t)
	ctx := context.Background()

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true, Score: 4},
			},
		},
	}

	// First ingest.
	r1, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(nil))
	if err != nil {
		t.Fatalf("first ingest: %v", err)
	}
	if r1.RoutingPatterns != 1 {
		t.Errorf("first ingest: RoutingPatterns = %d, want 1", r1.RoutingPatterns)
	}

	// Second ingest — same pattern, should upsert.
	r2, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(nil))
	if err != nil {
		t.Fatalf("second ingest: %v", err)
	}
	if r2.RoutingPatterns != 1 {
		t.Errorf("second ingest: RoutingPatterns = %d, want 1 (upsert, not skip)", r2.RoutingPatterns)
	}

	// Verify: still one row, but seen_count=2 and confidence=0.4.
	patterns, err := rdb.GetRoutingPatterns(ctx, "workspace:ws1")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (deduplicated in DB)", len(patterns))
	}
	if patterns[0].SeenCount != 2 {
		t.Errorf("seen_count = %d, want 2", patterns[0].SeenCount)
	}
	if patterns[0].Confidence < 0.39 || patterns[0].Confidence > 0.41 {
		t.Errorf("confidence = %f, want ~0.4 (2 observations)", patterns[0].Confidence)
	}
}

func TestIngestAuditReports_LowScoreSkipsPatternsNotCorrections(t *testing.T) {
	// Regression: patterns are score-gated but corrections are not.
	rdb := newTestDB(t)
	ctx := context.Background()

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-low",
			Scorecard:   audit.Scorecard{Overall: 2},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "impl", PersonaAssigned: "backend", PersonaCorrect: true, Score: 4},
				{PhaseName: "review", PersonaAssigned: "wrong", PersonaIdeal: "right", PersonaCorrect: false, Score: 2},
			},
		},
	}

	result, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(nil))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	// Corrections have no score gate.
	if result.Corrections != 1 {
		t.Errorf("Corrections = %d, want 1 (no score gate)", result.Corrections)
	}

	// Routing patterns ARE score-gated.
	if result.RoutingPatterns != 0 {
		t.Errorf("RoutingPatterns = %d, want 0 (below score gate)", result.RoutingPatterns)
	}

	// Handoff patterns ARE score-gated.
	if result.HandoffPatterns != 0 {
		t.Errorf("HandoffPatterns = %d, want 0 (below score gate)", result.HandoffPatterns)
	}
}

func TestIngestAuditReports_CancelledContextFailsPatternRecording(t *testing.T) {
	// Regression: cancelled context during pattern recording must propagate.
	rdb := newTestDB(t)

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws1",
			Scorecard:   audit.Scorecard{Overall: 4},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "implement", PersonaAssigned: "backend", PersonaCorrect: true, Score: 4},
			},
		},
	}

	_, err := IngestAuditReports(ctx, rdb, reports, nil, stubPlanLoader(nil))
	if err == nil {
		t.Fatal("expected error from cancelled context during pattern recording, got nil")
	}
	if !strings.Contains(err.Error(), "context canceled") {
		t.Errorf("expected context canceled error, got: %v", err)
	}
}

// TestIngestAuditReports_FullLoop_PatternsAndCorrectionsCoexist verifies the
// end-to-end flow: a report with both correct and incorrect personas produces
// routing patterns (positive signal) AND corrections (negative signal), and
// both are retrievable from the same target.
func TestIngestAuditReports_FullLoop_PatternsAndCorrectionsCoexist(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	plans := map[string]*core.Plan{
		"ws-mixed": {
			Task:          "build feature with review",
			DecompSource:  "llm",
			ExecutionMode: "sequential",
			Phases: []*core.Phase{
				{Name: "plan", Objective: "design", Persona: "architect"},
				{Name: "implement", Objective: "build", Persona: "senior-backend-engineer"},
				{Name: "review", Objective: "review", Persona: "staff-code-reviewer"},
			},
		},
	}

	reports := []audit.AuditReport{
		{
			WorkspaceID: "ws-mixed",
			Scorecard: audit.Scorecard{
				Overall:              4,
				DecompositionQuality: 4,
				PersonaFit:           3,
			},
			Phases: []audit.PhaseEvaluation{
				{PhaseName: "plan", PersonaAssigned: "architect", PersonaCorrect: true, Score: 4},
				{PhaseName: "implement", PersonaAssigned: "senior-frontend-engineer", PersonaIdeal: "senior-backend-engineer", PersonaCorrect: false, Score: 3},
				{PhaseName: "review", PersonaAssigned: "staff-code-reviewer", PersonaCorrect: true, Score: 5},
			},
		},
	}

	resolver := func(wsID string) string {
		if wsID == "ws-mixed" {
			return "repo:~/skills/orchestrator"
		}
		return ""
	}

	result, err := IngestAuditReports(ctx, rdb, reports, resolver, stubPlanLoader(plans))
	if err != nil {
		t.Fatalf("IngestAuditReports: %v", err)
	}

	target := "repo:~/skills/orchestrator"

	// 1 correction (implement: frontend→backend).
	if result.Corrections != 1 {
		t.Errorf("Corrections = %d, want 1", result.Corrections)
	}
	corrections, err := rdb.GetRoutingCorrections(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}

	// 2 routing patterns (plan=architect correct, review=staff-code-reviewer correct).
	if result.RoutingPatterns != 2 {
		t.Errorf("RoutingPatterns = %d, want 2", result.RoutingPatterns)
	}
	routingPats, err := rdb.GetRoutingPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(routingPats) != 2 {
		t.Fatalf("len(routingPats) = %d, want 2", len(routingPats))
	}

	// No handoff patterns: the only transition path runs through an audited-wrong persona.
	if result.HandoffPatterns != 0 {
		t.Errorf("HandoffPatterns = %d, want 0", result.HandoffPatterns)
	}
	handoffPats, err := rdb.GetHandoffPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(handoffPats) != 0 {
		t.Fatalf("len(handoffPats) = %d, want 0", len(handoffPats))
	}

	// 1 example (passes quality gate).
	if result.Examples != 1 {
		t.Errorf("Examples = %d, want 1", result.Examples)
	}

	// 1 finding (wrong_persona for implement).
	if result.Findings != 1 {
		t.Errorf("Findings = %d, want 1", result.Findings)
	}
}
