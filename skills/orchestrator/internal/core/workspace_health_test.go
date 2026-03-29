package core

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// makeWorkspace creates a minimal workspace dir (with mission.md) and returns
// its path. Additional sidecar files are written by the caller.
func makeWorkspace(t *testing.T) string {
	t.Helper()
	ws := t.TempDir()
	writeMissionFile(t, ws)
	return ws
}

// writeBlankIssueID writes a linear_issue_id sidecar whose content is all whitespace.
func writeBlankIssueID(t *testing.T, wsPath string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(wsPath, "linear_issue_id"), []byte("   \n"), 0600); err != nil {
		t.Fatal(err)
	}
}

func writeCorruptCheckpoint(t *testing.T, wsPath string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(wsPath, "checkpoint.json"), []byte("{bad json"), 0600); err != nil {
		t.Fatal(err)
	}
}

// --- CheckWorkspaceLinkHealth unit tests ---

func TestCheckWorkspaceLinkHealth_CleanWorkspace_NoSidecars(t *testing.T) {
	// Older workspace: mission.md only, no sidecars at all → not degraded.
	ws := makeWorkspace(t)
	writeCheckpoint(t, ws, "completed")

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) != 0 {
		t.Errorf("expected no degradations for clean workspace, got %v", d)
	}
}

func TestCheckWorkspaceLinkHealth_CleanWorkspace_FullSidecars(t *testing.T) {
	// Workspace with valid sidecars → not degraded.
	ws := makeWorkspace(t)
	writeCheckpoint(t, ws, "completed")

	mf := filepath.Join(t.TempDir(), "mission.md")
	if err := os.WriteFile(mf, []byte("---\ntitle: test\n---\n"), 0600); err != nil {
		t.Fatal(err)
	}
	writeIssueID(t, ws, "V-5")
	writeMissionPath(t, ws, mf)

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) != 0 {
		t.Errorf("expected no degradations, got %v", d)
	}
}

func TestCheckWorkspaceLinkHealth_BlankIssueIDSidecar(t *testing.T) {
	// linear_issue_id exists but is blank → degraded.
	ws := makeWorkspace(t)
	writeCheckpoint(t, ws, "completed")
	writeBlankIssueID(t, ws)

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) == 0 {
		t.Fatal("expected degradation for blank issue ID sidecar, got none")
	}
	if !containsSubstr(d, "issue ID sidecar is blank") {
		t.Errorf("expected 'issue ID sidecar is blank' in %v", d)
	}
}

func TestCheckWorkspaceLinkHealth_MissionFileMissing(t *testing.T) {
	// mission_path sidecar points to a deleted file → degraded.
	ws := makeWorkspace(t)
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, "/nonexistent/mission.md")

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) == 0 {
		t.Fatal("expected degradation for missing mission file, got none")
	}
	if !containsSubstr(d, "mission file missing") {
		t.Errorf("expected 'mission file missing' in %v", d)
	}
}

func TestCheckWorkspaceLinkHealth_MissionPathSidecarEmpty(t *testing.T) {
	// mission_path sidecar exists but content is blank → not degraded
	// (treated the same as absent sidecar — ad-hoc workspace).
	ws := makeWorkspace(t)
	writeCheckpoint(t, ws, "completed")
	if err := os.WriteFile(filepath.Join(ws, "mission_path"), []byte("   "), 0600); err != nil {
		t.Fatal(err)
	}

	d := CheckWorkspaceLinkHealth(ws)
	if containsSubstr(d, "mission file") {
		t.Errorf("empty mission_path content should not flag a degradation, got %v", d)
	}
}

func TestCheckWorkspaceLinkHealth_CheckpointMissing(t *testing.T) {
	// No checkpoint.json → degraded (workspace was never checkpointed).
	ws := makeWorkspace(t)
	// Deliberately no writeCheckpoint call.

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) == 0 {
		t.Fatal("expected degradation for missing checkpoint, got none")
	}
	if !containsSubstr(d, "checkpoint missing") {
		t.Errorf("expected 'checkpoint missing' in %v", d)
	}
}

func TestCheckWorkspaceLinkHealth_CheckpointCorrupt(t *testing.T) {
	// checkpoint.json present but not valid JSON → degraded.
	ws := makeWorkspace(t)
	writeCorruptCheckpoint(t, ws)

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) == 0 {
		t.Fatal("expected degradation for corrupt checkpoint, got none")
	}
	if !containsSubstr(d, "checkpoint corrupt") {
		t.Errorf("expected 'checkpoint corrupt' in %v", d)
	}
}

func TestCheckWorkspaceLinkHealth_MultipleDegradations(t *testing.T) {
	// Both blank issue ID and missing checkpoint → two degradations.
	ws := makeWorkspace(t)
	writeBlankIssueID(t, ws)
	// no checkpoint

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) < 2 {
		t.Errorf("expected at least 2 degradations, got %v", d)
	}
	if !containsSubstr(d, "issue ID sidecar is blank") {
		t.Errorf("expected 'issue ID sidecar is blank' in %v", d)
	}
	if !containsSubstr(d, "checkpoint missing") {
		t.Errorf("expected 'checkpoint missing' in %v", d)
	}
}

// --- Integration: ListWorkspaceLinks propagates degradations ---

func TestListWorkspaceLinks_PropagatesDegradations(t *testing.T) {
	cfg := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfg)

	// Workspace A: healthy.
	wsA := filepath.Join(cfg, "workspaces", "20260316-aaaa0001")
	if err := os.MkdirAll(wsA, 0700); err != nil {
		t.Fatal(err)
	}
	writeMissionFile(t, wsA)
	writeIssueID(t, wsA, "V-1")
	writeCheckpoint(t, wsA, "completed")

	// Workspace B: degraded — blank issue ID + corrupt checkpoint.
	wsB := filepath.Join(cfg, "workspaces", "20260316-bbbb0002")
	if err := os.MkdirAll(wsB, 0700); err != nil {
		t.Fatal(err)
	}
	writeMissionFile(t, wsB)
	writeBlankIssueID(t, wsB)
	writeCorruptCheckpoint(t, wsB)

	links, err := ListWorkspaceLinks(10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(links) != 2 {
		t.Fatalf("expected 2 links, got %d", len(links))
	}

	// Identify A and B by workspace ID suffix.
	var linkA, linkB *WorkspaceLink
	for i := range links {
		switch links[i].WorkspaceID {
		case "20260316-aaaa0001":
			linkA = &links[i]
		case "20260316-bbbb0002":
			linkB = &links[i]
		}
	}
	if linkA == nil || linkB == nil {
		t.Fatalf("could not identify both workspaces in links: %v", links)
	}

	if len(linkA.Degradations) != 0 {
		t.Errorf("workspace A should be clean, got %v", linkA.Degradations)
	}
	if len(linkB.Degradations) == 0 {
		t.Error("workspace B should be degraded, got no degradations")
	}
}

// --- Integration: FindWorkspacesByIssue propagates degradations ---

func TestFindWorkspacesByIssue_PropagatesDegradations(t *testing.T) {
	cfg := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfg)

	ws := filepath.Join(cfg, "workspaces", "20260316-cccc0003")
	if err := os.MkdirAll(ws, 0700); err != nil {
		t.Fatal(err)
	}
	writeMissionFile(t, ws)
	writeIssueID(t, ws, "V-7")
	// Dangling mission path + no checkpoint.
	writeMissionPath(t, ws, "/gone/mission.md")

	links, err := FindWorkspacesByIssue("V-7")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(links) != 1 {
		t.Fatalf("expected 1 link, got %d", len(links))
	}
	if len(links[0].Degradations) == 0 {
		t.Error("expected degradations to be propagated, got none")
	}
	if !containsSubstr(links[0].Degradations, "mission file missing") {
		t.Errorf("expected 'mission file missing' degradation, got %v", links[0].Degradations)
	}
	if !containsSubstr(links[0].Degradations, "checkpoint missing") {
		t.Errorf("expected 'checkpoint missing' degradation, got %v", links[0].Degradations)
	}
}

// --- Integration: older workspaces (no sidecars) are never degraded ---

func TestCheckWorkspaceLinkHealth_OlderWorkspace_NotDegraded(t *testing.T) {
	// Simulate a pre-sidecar workspace: mission.md + checkpoint, nothing else.
	// These must not be flagged as degraded to preserve backward compat.
	ws := makeWorkspace(t)
	writeCheckpoint(t, ws, "completed")

	d := CheckWorkspaceLinkHealth(ws)
	if len(d) != 0 {
		t.Errorf("older workspace without sidecars must not be degraded, got %v", d)
	}
}

// --- Helpers ---

func containsSubstr(strs []string, sub string) bool {
	for _, s := range strs {
		if strings.Contains(s, sub) {
			return true
		}
	}
	return false
}

