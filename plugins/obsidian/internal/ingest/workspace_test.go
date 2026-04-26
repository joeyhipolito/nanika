package ingest

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// ---------- readWorkspaceDir tests ----------

func TestReadWorkspaceDir_WithMissionJSON(t *testing.T) {
	tmp := t.TempDir()
	wsDir := filepath.Join(tmp, "20260318-abc123")
	if err := os.MkdirAll(wsDir, 0755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	m := WorkspaceMission{
		ID:        "abc123",
		Title:     "Test Mission",
		Status:    "done",
		Type:      "feature",
		Summary:   "A test mission summary",
		Tags:      []string{"go", "cli"},
		CreatedAt: "2026-03-18T10:00:00Z",
	}
	data, _ := json.Marshal(m)
	os.WriteFile(filepath.Join(wsDir, "mission.json"), data, 0644)
	os.WriteFile(filepath.Join(wsDir, "PLAN.md"), []byte("# Plan"), 0644)
	os.WriteFile(filepath.Join(wsDir, "output.json"), []byte("{}"), 0644)

	a, err := readWorkspaceDir(wsDir, "20260318-abc123")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if a.ID != "abc123" {
		t.Errorf("expected ID 'abc123', got %q", a.ID)
	}
	if a.Title != "Test Mission" {
		t.Errorf("expected title 'Test Mission', got %q", a.Title)
	}
	if a.Status != "done" {
		t.Errorf("expected status 'done', got %q", a.Status)
	}
	if a.Type != "feature" {
		t.Errorf("expected type 'feature', got %q", a.Type)
	}
	if a.Summary != "A test mission summary" {
		t.Errorf("expected summary, got %q", a.Summary)
	}
	if a.CreatedAt.Year() != 2026 || a.CreatedAt.Month() != 3 || a.CreatedAt.Day() != 18 {
		t.Errorf("expected 2026-03-18, got %v", a.CreatedAt)
	}
	if len(a.Files) != 2 {
		t.Errorf("expected 2 artifact files (PLAN.md + output.json), got %d: %v", len(a.Files), a.Files)
	}
}

func TestReadWorkspaceDir_NoMissionJSON(t *testing.T) {
	tmp := t.TempDir()
	wsDir := filepath.Join(tmp, "ws-nojson")
	os.MkdirAll(wsDir, 0755)

	a, err := readWorkspaceDir(wsDir, "ws-nojson")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if a.ID != "ws-nojson" {
		t.Errorf("expected ID 'ws-nojson', got %q", a.ID)
	}
	if a.Title != "ws-nojson" {
		t.Errorf("expected title to fall back to dir name, got %q", a.Title)
	}
	// CreatedAt should be non-zero (from directory mtime)
	if a.CreatedAt.IsZero() {
		t.Error("expected non-zero CreatedAt from directory mtime")
	}
}

func TestReadWorkspaceDir_MalformedMissionJSON(t *testing.T) {
	tmp := t.TempDir()
	wsDir := filepath.Join(tmp, "ws-bad")
	os.MkdirAll(wsDir, 0755)
	os.WriteFile(filepath.Join(wsDir, "mission.json"), []byte("{not valid json"), 0644)

	// Should still succeed, falling back to dir name
	a, err := readWorkspaceDir(wsDir, "ws-bad")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if a.ID != "ws-bad" {
		t.Errorf("expected fallback ID 'ws-bad', got %q", a.ID)
	}
}

func TestReadWorkspaceDir_OnlyMDExtensionsCollected(t *testing.T) {
	tmp := t.TempDir()
	wsDir := filepath.Join(tmp, "ws-files")
	os.MkdirAll(wsDir, 0755)

	// Only .md, .json, .txt should be collected
	os.WriteFile(filepath.Join(wsDir, "report.md"), []byte("# Report"), 0644)
	os.WriteFile(filepath.Join(wsDir, "data.json"), []byte("{}"), 0644)
	os.WriteFile(filepath.Join(wsDir, "notes.txt"), []byte("notes"), 0644)
	os.WriteFile(filepath.Join(wsDir, "binary.bin"), []byte{0x00, 0x01}, 0644)
	os.WriteFile(filepath.Join(wsDir, "script.sh"), []byte("#!/bin/bash"), 0644)

	a, err := readWorkspaceDir(wsDir, "ws-files")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(a.Files) != 3 {
		t.Errorf("expected 3 files (.md, .json, .txt), got %d: %v", len(a.Files), a.Files)
	}
}

// ---------- workspaceNotePath tests ----------

func TestWorkspaceNotePath_Format(t *testing.T) {
	a := &WorkspaceArtifact{ID: "20260318-abc123"}
	got := workspaceNotePath(a)

	if !strings.HasPrefix(got, filepath.Join("Captures", "Workspaces")) {
		t.Errorf("expected path under Captures/Workspaces, got %q", got)
	}
	if !strings.HasSuffix(got, ".md") {
		t.Errorf("expected .md extension, got %q", got)
	}
	if strings.Contains(got, " ") {
		t.Errorf("path should not contain spaces, got %q", got)
	}
}

func TestWorkspaceNotePath_EmptyIDFallback(t *testing.T) {
	a := &WorkspaceArtifact{ID: ""}
	got := workspaceNotePath(a)
	if !strings.Contains(got, "workspace") {
		t.Errorf("expected 'workspace' fallback in path, got %q", got)
	}
}

// ---------- buildWorkspaceNote tests ----------

func TestBuildWorkspaceNote_ContainsFrontmatter(t *testing.T) {
	a := &WorkspaceArtifact{
		ID:        "20260318-abc",
		Title:     "My Mission",
		Status:    "done",
		Type:      "feature",
		Summary:   "Did something cool",
		Tags:      []string{"go"},
		CreatedAt: time.Date(2026, 3, 18, 10, 0, 0, 0, time.UTC),
		Files:     []string{"PLAN.md", "output.json"},
	}

	content := buildWorkspaceNote(a)

	if !strings.HasPrefix(content, "---\n") {
		t.Error("expected YAML frontmatter opening")
	}
	if !strings.Contains(content, "type: capture") {
		t.Error("expected 'type: capture'")
	}
	if !strings.Contains(content, "source: via-workspace") {
		t.Error("expected 'source: via-workspace'")
	}
	if !strings.Contains(content, "workspace-id: 20260318-abc") {
		t.Error("expected workspace-id in frontmatter")
	}
	if !strings.Contains(content, "status: done") {
		t.Error("expected status in frontmatter")
	}
	if !strings.Contains(content, "date: 2026-03-18") {
		t.Error("expected date in frontmatter")
	}
	if !strings.Contains(content, "# My Mission") {
		t.Error("expected H1 title heading")
	}
	if !strings.Contains(content, "Did something cool") {
		t.Error("expected summary in body")
	}
	if !strings.Contains(content, "PLAN.md") {
		t.Error("expected artifact file listed")
	}
	if !strings.Contains(content, "output.json") {
		t.Error("expected artifact file listed")
	}
}

func TestBuildWorkspaceNote_NoSummaryNoFiles(t *testing.T) {
	a := &WorkspaceArtifact{
		ID:        "ws-minimal",
		Title:     "Minimal Workspace",
		CreatedAt: time.Now(),
	}

	content := buildWorkspaceNote(a)

	if !strings.Contains(content, "# Minimal Workspace") {
		t.Error("expected title heading")
	}
	// No Artifacts section when no files
	if strings.Contains(content, "## Artifacts") {
		t.Error("expected no Artifacts section when files list is empty")
	}
	// Notes section always present
	if !strings.Contains(content, "## Notes") {
		t.Error("expected Notes section")
	}
}

func TestBuildWorkspaceNote_TagsIncludeDefaults(t *testing.T) {
	a := &WorkspaceArtifact{
		ID:        "ws-tags",
		Title:     "Tag Test",
		Type:      "mission",
		Tags:      []string{"custom-tag"},
		CreatedAt: time.Now(),
	}

	content := buildWorkspaceNote(a)

	// Default tags: capture, workspace, via
	if !strings.Contains(content, "capture") {
		t.Error("expected 'capture' in tags")
	}
	if !strings.Contains(content, "workspace") {
		t.Error("expected 'workspace' in tags")
	}
	if !strings.Contains(content, "via") {
		t.Error("expected 'via' in tags")
	}
	if !strings.Contains(content, "mission") {
		t.Error("expected type 'mission' in tags")
	}
	if !strings.Contains(content, "custom-tag") {
		t.Error("expected 'custom-tag' in tags")
	}
}

// ---------- State.Workspaces dedup tests ----------

func TestState_WorkspaceDedup(t *testing.T) {
	s := &State{
		Scout:      make(map[string]bool),
		Learnings:  make(map[string]bool),
		Workspaces: make(map[string]bool),
	}

	if s.HasWorkspace("ws-123") {
		t.Error("expected false before marking")
	}
	s.MarkWorkspace("ws-123")
	if !s.HasWorkspace("ws-123") {
		t.Error("expected true after marking")
	}
	if s.HasWorkspace("ws-456") {
		t.Error("unrelated key should not be marked")
	}
}

// ---------- IngestWorkspaces integration test ----------

func TestIngestWorkspaces_DryRun(t *testing.T) {
	vault := t.TempDir()
	wsBase := t.TempDir()

	// Create two workspace directories
	for _, name := range []string{"ws-alpha", "ws-beta"} {
		wsDir := filepath.Join(wsBase, name)
		os.MkdirAll(wsDir, 0755)
		m := WorkspaceMission{
			ID:    name,
			Title: "Workspace " + name,
		}
		data, _ := json.Marshal(m)
		os.WriteFile(filepath.Join(wsDir, "mission.json"), data, 0644)
	}

	state := &State{
		Scout:      make(map[string]bool),
		Learnings:  make(map[string]bool),
		Workspaces: make(map[string]bool),
	}

	// Test readWorkspaceDir + workspaceNotePath + buildWorkspaceNote + writeNote
	// for each directory (simulating what IngestWorkspaces does internally)
	var created []string
	entries, _ := os.ReadDir(wsBase)
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		wsDir := filepath.Join(wsBase, entry.Name())
		a, err := readWorkspaceDir(wsDir, entry.Name())
		if err != nil {
			t.Fatalf("readWorkspaceDir: %v", err)
		}
		notePath := workspaceNotePath(a)
		content := buildWorkspaceNote(a)

		if err := writeNote(vault, notePath, content); err != nil {
			t.Fatalf("writeNote: %v", err)
		}
		state.MarkWorkspace(a.ID)
		created = append(created, notePath)
	}

	if len(created) != 2 {
		t.Errorf("expected 2 created notes, got %d", len(created))
	}

	// Verify files exist in vault
	for _, p := range created {
		full := filepath.Join(vault, p)
		if _, err := os.Stat(full); err != nil {
			t.Errorf("expected note at %s: %v", full, err)
		}
	}

	// Dedup: both should now be marked
	if !state.HasWorkspace("ws-alpha") {
		t.Error("expected ws-alpha to be marked")
	}
	if !state.HasWorkspace("ws-beta") {
		t.Error("expected ws-beta to be marked")
	}

	// Duplicate write should error
	a := &WorkspaceArtifact{ID: "ws-alpha", Title: "Workspace ws-alpha", CreatedAt: time.Now()}
	if err := writeNote(vault, workspaceNotePath(a), buildWorkspaceNote(a)); err == nil {
		t.Error("expected error on duplicate write")
	}
}

func TestIngestWorkspaces_MissingBaseDir(t *testing.T) {
	// The real function reads from ~/.via/workspaces. We test the graceful path
	// by confirming readWorkspaceDir handles non-existent workspace dirs.
	// (We can't easily override home dir in unit tests without env manipulation.)
	a, err := readWorkspaceDir("/nonexistent/path/ws-x", "ws-x")
	if err != nil {
		t.Fatalf("expected graceful handling, got: %v", err)
	}
	// Should fall back to dir name, CreatedAt will be zero since stat failed
	if a.ID != "ws-x" {
		t.Errorf("expected fallback ID, got %q", a.ID)
	}
}
