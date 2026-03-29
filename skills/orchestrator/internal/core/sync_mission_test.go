package core

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func writeCheckpoint(t *testing.T, wsPath, status string) {
	t.Helper()
	data, _ := json.Marshal(map[string]string{"status": status})
	if err := os.WriteFile(filepath.Join(wsPath, "checkpoint.json"), data, 0600); err != nil {
		t.Fatal(err)
	}
}

func writeMissionPath(t *testing.T, wsPath, missionPath string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(wsPath, "mission_path"), []byte(missionPath), 0600); err != nil {
		t.Fatal(err)
	}
}

func writeIssueID(t *testing.T, wsPath, issueID string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(wsPath, "linear_issue_id"), []byte(issueID), 0600); err != nil {
		t.Fatal(err)
	}
}

func writeMissionFile(t *testing.T, wsPath string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(wsPath, "mission.md"), []byte("# test mission\n"), 0600); err != nil {
		t.Fatal(err)
	}
}

func TestSyncMissionStatus_UpdatesExistingStatusField(t *testing.T) {
	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "mission.md")

	if err := os.WriteFile(mf, []byte("---\ntitle: test\nstatus: in_progress\n---\n# Body\n"), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, mf)

	synced, err := SyncMissionStatus(ws)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if synced != mf {
		t.Fatalf("synced = %q; want %q", synced, mf)
	}

	body, _ := os.ReadFile(mf)
	content := string(body)
	if !strings.Contains(content, "status: completed") {
		t.Errorf("expected bare 'status: completed' in:\n%s", content)
	}
	if strings.Contains(content, `status: "completed"`) {
		t.Errorf("status must not be Go-quoted; got:\n%s", content)
	}
}

func TestSyncMissionStatus_InsertsStatusWhenAbsent(t *testing.T) {
	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "mission.md")

	if err := os.WriteFile(mf, []byte("---\ntitle: test\n---\n# Body\n"), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "failed")
	writeMissionPath(t, ws, mf)

	if _, err := SyncMissionStatus(ws); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	body, _ := os.ReadFile(mf)
	content := string(body)
	if !strings.Contains(content, "status: failed") {
		t.Errorf("expected 'status: failed' inserted; got:\n%s", content)
	}
	// Closing --- must still be present after the inserted line.
	if !strings.Contains(content, "---\n# Body") {
		t.Errorf("closing --- or body lost; got:\n%s", content)
	}
}

func TestSyncMissionStatus_NoopForInProgress(t *testing.T) {
	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "mission.md")
	original := "---\ntitle: test\nstatus: in_progress\n---\n"
	if err := os.WriteFile(mf, []byte(original), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "in_progress")
	writeMissionPath(t, ws, mf)

	synced, err := SyncMissionStatus(ws)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if synced != "" {
		t.Errorf("expected no-op (synced=\"\") for in_progress; got %q", synced)
	}
}

func TestSyncMissionStatus_NoopWithoutMissionPath(t *testing.T) {
	ws := t.TempDir()
	writeCheckpoint(t, ws, "completed")
	// no mission_path sidecar

	synced, err := SyncMissionStatus(ws)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if synced != "" {
		t.Errorf("expected no-op; got %q", synced)
	}
}

func TestSyncMissionStatus_NoopWithoutFrontmatter(t *testing.T) {
	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "mission.md")
	if err := os.WriteFile(mf, []byte("# No frontmatter here\n"), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, mf)

	synced, err := SyncMissionStatus(ws)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if synced != "" {
		t.Errorf("expected no-op for file without frontmatter; got %q", synced)
	}
}

func TestSyncMissionStatus_ErrorsOnMissionReadFailure(t *testing.T) {
	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "mission-dir")
	if err := os.MkdirAll(mf, 0700); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, mf)

	synced, err := SyncMissionStatus(ws)
	if err == nil {
		t.Fatal("expected read error, got nil")
	}
	if synced != "" {
		t.Fatalf("expected no synced path on read failure, got %q", synced)
	}
	if !strings.Contains(err.Error(), "reading mission file") {
		t.Fatalf("expected wrapped read error, got %v", err)
	}
}

func TestSyncMissionStatus_PreservesCRLF(t *testing.T) {
	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "mission.md")

	content := "---\r\ntitle: test\r\nstatus: in_progress\r\n---\r\n# Body\r\n"
	if err := os.WriteFile(mf, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, mf)

	if _, err := SyncMissionStatus(ws); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	body, _ := os.ReadFile(mf)
	got := string(body)
	if !strings.Contains(got, "status: completed\r\n") {
		t.Fatalf("expected CRLF-preserved status update, got:\n%q", got)
	}
	if strings.Contains(got, "\nstatus: completed\n") {
		t.Fatalf("expected original CRLF line endings to be preserved, got:\n%q", got)
	}
}

func TestSyncMissionStatus_DoesNotMatchStatusCode(t *testing.T) {
	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "mission.md")

	content := "---\nstatus_code: 200\n---\n# Body\n"
	if err := os.WriteFile(mf, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "failed")
	writeMissionPath(t, ws, mf)

	if _, err := SyncMissionStatus(ws); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	body, _ := os.ReadFile(mf)
	got := string(body)
	if !strings.Contains(got, "status_code: 200") {
		t.Fatalf("expected status_code field to remain intact, got:\n%s", got)
	}
	if !strings.Contains(got, "status: failed") {
		t.Fatalf("expected standalone status field to be inserted, got:\n%s", got)
	}
}

func TestSyncManagedMissionStatus_NoopOutsideManagedMissions(t *testing.T) {
	cfg := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfg)

	ws := t.TempDir()
	mf := filepath.Join(t.TempDir(), "repo-mission.md")
	original := "---\ntitle: test\nstatus: active\n---\n# Body\n"
	if err := os.WriteFile(mf, []byte(original), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, mf)

	synced, err := SyncManagedMissionStatus(ws)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if synced != "" {
		t.Fatalf("expected managed auto-sync to skip repo-local mission, got %q", synced)
	}

	body, _ := os.ReadFile(mf)
	if string(body) != original {
		t.Fatalf("repo-local mission should be untouched, got:\n%s", string(body))
	}
}

func TestSyncManagedMissionStatus_UpdatesManagedMission(t *testing.T) {
	cfg := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfg)

	missionsDir := filepath.Join(cfg, "missions")
	if err := os.MkdirAll(missionsDir, 0700); err != nil {
		t.Fatal(err)
	}

	ws := t.TempDir()
	mf := filepath.Join(missionsDir, "managed.md")
	if err := os.WriteFile(mf, []byte("---\ntitle: test\nstatus: active\n---\n"), 0600); err != nil {
		t.Fatal(err)
	}
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, mf)

	synced, err := SyncManagedMissionStatus(ws)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if synced != mf {
		t.Fatalf("synced = %q; want %q", synced, mf)
	}
}

func TestFindWorkspacesByIssue_NormalizesReturnedIssueID(t *testing.T) {
	cfg := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfg)

	ws := filepath.Join(cfg, "workspaces", "20260316-aaaa1111")
	if err := os.MkdirAll(ws, 0700); err != nil {
		t.Fatal(err)
	}
	writeMissionFile(t, ws)
	writeIssueID(t, ws, "v-5\n")
	writeCheckpoint(t, ws, "completed")
	writeMissionPath(t, ws, "/tmp/mission.md")

	links, err := FindWorkspacesByIssue("V-5")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(links) != 1 {
		t.Fatalf("expected 1 match, got %d", len(links))
	}
	if links[0].LinearIssueID != "V-5" {
		t.Fatalf("expected normalized issue ID, got %q", links[0].LinearIssueID)
	}
}

func TestListWorkspaceLinks_NormalizesIssueIDAndRespectsLimit(t *testing.T) {
	cfg := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfg)

	ws1 := filepath.Join(cfg, "workspaces", "20260316-bbbb2222")
	ws2 := filepath.Join(cfg, "workspaces", "20260316-cccc3333")
	for _, ws := range []string{ws1, ws2} {
		if err := os.MkdirAll(ws, 0700); err != nil {
			t.Fatal(err)
		}
		writeMissionFile(t, ws)
	}
	writeIssueID(t, ws1, "v-9")
	writeCheckpoint(t, ws1, "completed")
	writeIssueID(t, ws2, "v-10")
	writeCheckpoint(t, ws2, "failed")

	links, err := ListWorkspaceLinks(1)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(links) != 1 {
		t.Fatalf("expected 1 link, got %d", len(links))
	}
	if links[0].LinearIssueID != "V-10" && links[0].LinearIssueID != "V-9" {
		t.Fatalf("expected normalized issue ID, got %q", links[0].LinearIssueID)
	}
	if links[0].LinearIssueID != strings.ToUpper(links[0].LinearIssueID) {
		t.Fatalf("expected uppercase issue ID, got %q", links[0].LinearIssueID)
	}
}
