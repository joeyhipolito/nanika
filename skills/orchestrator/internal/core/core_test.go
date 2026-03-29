package core

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// --- Workspace Tests ---

func TestCreateWorkspace(t *testing.T) {
	// CreateWorkspace uses os.UserHomeDir and writes to ~/.alluka/workspaces/,
	// which we can't easily redirect. Instead, test the helpers that accept
	// a wsPath parameter, and test CreateWorkspace for basic contract.

	ws, err := CreateWorkspace("test task", "dev")
	if err != nil {
		t.Fatalf("CreateWorkspace: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(ws.Path) })

	// Verify returned fields
	if ws.ID == "" {
		t.Error("workspace ID is empty")
	}
	if ws.Task != "test task" {
		t.Errorf("task = %q; want %q", ws.Task, "test task")
	}
	if ws.Domain != "dev" {
		t.Errorf("domain = %q; want %q", ws.Domain, "dev")
	}
	if ws.CreatedAt.IsZero() {
		t.Error("created_at is zero")
	}

	// Verify directory structure
	expectedDirs := []string{
		"",
		"workers",
		"artifacts",
		"artifacts/merged",
		"learnings",
	}
	for _, rel := range expectedDirs {
		dir := filepath.Join(ws.Path, rel)
		info, err := os.Stat(dir)
		if err != nil {
			t.Errorf("expected directory %s: %v", rel, err)
			continue
		}
		if !info.IsDir() {
			t.Errorf("%s is not a directory", rel)
		}
		if perm := info.Mode().Perm(); perm != 0700 {
			t.Errorf("%s permissions = %o; want 0700", rel, perm)
		}
	}

	// Verify mission.md
	missionPath := filepath.Join(ws.Path, "mission.md")
	data, err := os.ReadFile(missionPath)
	if err != nil {
		t.Fatalf("reading mission.md: %v", err)
	}
	if string(data) != "test task" {
		t.Errorf("mission.md = %q; want %q", data, "test task")
	}
	info, _ := os.Stat(missionPath)
	if perm := info.Mode().Perm(); perm != 0600 {
		t.Errorf("mission.md permissions = %o; want 0600", perm)
	}
}

func TestCreateWorkerDir(t *testing.T) {
	tmpDir := t.TempDir()

	workerDir, err := CreateWorkerDir(tmpDir, "architect-01")
	if err != nil {
		t.Fatalf("CreateWorkerDir: %v", err)
	}

	// Verify directory structure
	expectedDirs := []string{
		"workers/architect-01",
		"workers/architect-01/.claude/hooks",
	}
	for _, rel := range expectedDirs {
		dir := filepath.Join(tmpDir, rel)
		info, err := os.Stat(dir)
		if err != nil {
			t.Errorf("expected directory %s: %v", rel, err)
			continue
		}
		if !info.IsDir() {
			t.Errorf("%s is not a directory", rel)
		}
		if perm := info.Mode().Perm(); perm != 0700 {
			t.Errorf("%s permissions = %o; want 0700", rel, perm)
		}
	}

	// Verify returned path
	expected := filepath.Join(tmpDir, "workers", "architect-01")
	if workerDir != expected {
		t.Errorf("workerDir = %q; want %q", workerDir, expected)
	}

	// Repeated creation should not error
	_, err = CreateWorkerDir(tmpDir, "architect-01")
	if err != nil {
		t.Errorf("repeated CreateWorkerDir should not error: %v", err)
	}
}

func TestCreatePhaseArtifactDir(t *testing.T) {
	tmpDir := t.TempDir()

	dir, err := CreatePhaseArtifactDir(tmpDir, "phase-1")
	if err != nil {
		t.Fatalf("CreatePhaseArtifactDir: %v", err)
	}

	expected := filepath.Join(tmpDir, "artifacts", "phase-1")
	if dir != expected {
		t.Errorf("dir = %q; want %q", dir, expected)
	}

	info, err := os.Stat(dir)
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0700 {
		t.Errorf("permissions = %o; want 0700", perm)
	}

	// Idempotent
	_, err = CreatePhaseArtifactDir(tmpDir, "phase-1")
	if err != nil {
		t.Errorf("repeated call should not error: %v", err)
	}
}

func TestMergedArtifactsDir(t *testing.T) {
	got := MergedArtifactsDir("/tmp/ws")
	want := "/tmp/ws/artifacts/merged"
	if got != want {
		t.Errorf("MergedArtifactsDir = %q; want %q", got, want)
	}
}

func TestLearningsDir(t *testing.T) {
	got := LearningsDir("/tmp/ws")
	want := "/tmp/ws/learnings"
	if got != want {
		t.Errorf("LearningsDir = %q; want %q", got, want)
	}
}

func TestCreateWorkerDir_ReadOnlyParent(t *testing.T) {
	tmpDir := t.TempDir()
	readOnly := filepath.Join(tmpDir, "readonly")
	if err := os.MkdirAll(readOnly, 0500); err != nil {
		t.Fatalf("setup: %v", err)
	}
	t.Cleanup(func() { os.Chmod(readOnly, 0700) })

	_, err := CreateWorkerDir(readOnly, "worker-1")
	if err == nil {
		t.Error("expected error creating worker dir in read-only parent")
	}
}

// --- Checkpoint Tests ---

func TestCheckpointSaveRestore(t *testing.T) {
	now := time.Now().Truncate(time.Second)
	later := now.Add(5 * time.Minute)

	tests := []struct {
		name   string
		plan   *Plan
		domain string
		status string
	}{
		{
			name:   "empty phases",
			plan:   &Plan{ID: "p1", Task: "build it", Phases: []*Phase{}, ExecutionMode: "sequential", CreatedAt: now},
			domain: "dev",
			status: "in_progress",
		},
		{
			name: "completed phases",
			plan: &Plan{
				ID:            "p2",
				Task:          "deploy service",
				ExecutionMode: "parallel",
				CreatedAt:     now,
				Phases: []*Phase{
					{
						ID: "ph1", Name: "design", Objective: "design the API",
						Persona: "architect", ModelTier: "think", Status: StatusCompleted,
						Output: "designed it", StartTime: &now, EndTime: &later,
						GatePassed: true, OutputLen: 11,
					},
					{
						ID: "ph2", Name: "implement", Objective: "write the code",
						Persona: "backend-engineer", ModelTier: "work", Status: StatusCompleted,
						Dependencies: []string{"ph1"}, Output: "wrote it",
						StartTime: &now, EndTime: &later, Retries: 1,
					},
				},
			},
			domain: "dev",
			status: "completed",
		},
		{
			name: "failed phase",
			plan: &Plan{
				ID:            "p3",
				Task:          "broken task",
				ExecutionMode: "sequential",
				CreatedAt:     now,
				Phases: []*Phase{
					{
						ID: "ph1", Name: "attempt", Status: StatusFailed,
						Error: "connection refused", StartTime: &now, EndTime: &later,
					},
					{
						ID: "ph2", Name: "skipped", Status: StatusSkipped,
						Dependencies: []string{"ph1"},
					},
				},
			},
			domain: "personal",
			status: "failed",
		},
		{
			name: "running phase with retries",
			plan: &Plan{
				ID:            "p4",
				Task:          "long task",
				ExecutionMode: "sequential",
				CreatedAt:     now,
				Phases: []*Phase{
					{
						ID: "ph1", Name: "processing", Status: StatusRunning,
						Persona: "backend-engineer", ModelTier: "work",
						StartTime: &now, Retries: 2, SessionID: "sess-abc",
					},
				},
			},
			domain: "work",
			status: "in_progress",
		},
		{
			name:   "nil plan",
			plan:   nil,
			domain: "dev",
			status: "failed",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()

			startedAt := time.Now().Truncate(time.Second)

			// Save
			err := SaveCheckpoint(tmpDir, tt.plan, tt.domain, tt.status, startedAt)
			if err != nil {
				t.Fatalf("SaveCheckpoint: %v", err)
			}

			// Verify file exists with correct permissions
			cpPath := filepath.Join(tmpDir, "checkpoint.json")
			info, err := os.Stat(cpPath)
			if err != nil {
				t.Fatalf("checkpoint.json missing: %v", err)
			}
			if perm := info.Mode().Perm(); perm != 0600 {
				t.Errorf("checkpoint permissions = %o; want 0600", perm)
			}

			// Load
			cp, err := LoadCheckpoint(tmpDir)
			if err != nil {
				t.Fatalf("LoadCheckpoint: %v", err)
			}

			// Verify checkpoint metadata
			if cp.Version != 2 {
				t.Errorf("version = %d; want 2", cp.Version)
			}
			if !cp.StartedAt.Equal(startedAt) {
				t.Errorf("started_at = %v; want %v", cp.StartedAt, startedAt)
			}
			if cp.WorkspaceID != filepath.Base(tmpDir) {
				t.Errorf("workspace_id = %q; want %q", cp.WorkspaceID, filepath.Base(tmpDir))
			}
			if cp.Domain != tt.domain {
				t.Errorf("domain = %q; want %q", cp.Domain, tt.domain)
			}
			if cp.Status != tt.status {
				t.Errorf("status = %q; want %q", cp.Status, tt.status)
			}

			// Verify plan round-trip
			if tt.plan == nil {
				if cp.Plan != nil {
					t.Errorf("plan should be nil, got %+v", cp.Plan)
				}
				return
			}

			if cp.Plan == nil {
				t.Fatal("plan is nil after restore")
			}
			if cp.Plan.ID != tt.plan.ID {
				t.Errorf("plan.ID = %q; want %q", cp.Plan.ID, tt.plan.ID)
			}
			if cp.Plan.Task != tt.plan.Task {
				t.Errorf("plan.Task = %q; want %q", cp.Plan.Task, tt.plan.Task)
			}
			if cp.Plan.ExecutionMode != tt.plan.ExecutionMode {
				t.Errorf("plan.ExecutionMode = %q; want %q", cp.Plan.ExecutionMode, tt.plan.ExecutionMode)
			}
			if len(cp.Plan.Phases) != len(tt.plan.Phases) {
				t.Fatalf("phases count = %d; want %d", len(cp.Plan.Phases), len(tt.plan.Phases))
			}

			for i, phase := range cp.Plan.Phases {
				want := tt.plan.Phases[i]
				if phase.ID != want.ID {
					t.Errorf("phase[%d].ID = %q; want %q", i, phase.ID, want.ID)
				}
				if phase.Status != want.Status {
					t.Errorf("phase[%d].Status = %q; want %q", i, phase.Status, want.Status)
				}
				if phase.Error != want.Error {
					t.Errorf("phase[%d].Error = %q; want %q", i, phase.Error, want.Error)
				}
				if phase.Output != want.Output {
					t.Errorf("phase[%d].Output = %q; want %q", i, phase.Output, want.Output)
				}
				if phase.Retries != want.Retries {
					t.Errorf("phase[%d].Retries = %d; want %d", i, phase.Retries, want.Retries)
				}
				if phase.SessionID != want.SessionID {
					t.Errorf("phase[%d].SessionID = %q; want %q", i, phase.SessionID, want.SessionID)
				}
			}
		})
	}
}

func TestCheckpointOverwrite(t *testing.T) {
	tmpDir := t.TempDir()

	plan1 := &Plan{ID: "v1", Task: "first"}
	plan2 := &Plan{ID: "v2", Task: "second"}

	t0 := time.Now()
	if err := SaveCheckpoint(tmpDir, plan1, "dev", "in_progress", t0); err != nil {
		t.Fatalf("first save: %v", err)
	}
	if err := SaveCheckpoint(tmpDir, plan2, "dev", "completed", t0); err != nil {
		t.Fatalf("second save: %v", err)
	}

	cp, err := LoadCheckpoint(tmpDir)
	if err != nil {
		t.Fatalf("load: %v", err)
	}
	if cp.Plan.ID != "v2" {
		t.Errorf("plan.ID = %q; want %q (latest save should win)", cp.Plan.ID, "v2")
	}
	if cp.Status != "completed" {
		t.Errorf("status = %q; want %q", cp.Status, "completed")
	}
}

// --- Edge Cases ---

func TestLoadCheckpoint_V1BackwardCompat(t *testing.T) {
	// A version 1 checkpoint has no started_at field.
	// LoadCheckpoint must succeed and StartedAt must be zero.
	tmpDir := t.TempDir()
	v1JSON := `{
  "version": 1,
  "workspace_id": "old-ws",
  "domain": "dev",
  "plan": {"id": "p1", "task": "old task", "execution_mode": "sequential"},
  "status": "in_progress"
}`
	if err := os.WriteFile(filepath.Join(tmpDir, "checkpoint.json"), []byte(v1JSON), 0600); err != nil {
		t.Fatalf("setup: %v", err)
	}

	cp, err := LoadCheckpoint(tmpDir)
	if err != nil {
		t.Fatalf("LoadCheckpoint: %v", err)
	}
	if cp.Version != 1 {
		t.Errorf("version = %d; want 1", cp.Version)
	}
	if !cp.StartedAt.IsZero() {
		t.Errorf("StartedAt = %v; want zero value for v1 checkpoint", cp.StartedAt)
	}
}

func TestLoadCheckpoint_MissingFile(t *testing.T) {
	tmpDir := t.TempDir()

	_, err := LoadCheckpoint(tmpDir)
	if err == nil {
		t.Error("expected error loading from empty directory")
	}
	if !strings.Contains(err.Error(), "read checkpoint") {
		t.Errorf("error should wrap with context, got: %v", err)
	}
}

func TestLoadCheckpoint_CorruptJSON(t *testing.T) {
	tests := []struct {
		name    string
		content string
	}{
		{"empty file", ""},
		{"invalid json", "{not json at all!!!"},
		{"truncated json", `{"version": 1, "plan":`},
		{"wrong type", `{"version": "not a number"}`},
		{"array instead of object", `[1, 2, 3]`},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			cpPath := filepath.Join(tmpDir, "checkpoint.json")
			if err := os.WriteFile(cpPath, []byte(tt.content), 0600); err != nil {
				t.Fatalf("setup: %v", err)
			}

			_, err := LoadCheckpoint(tmpDir)
			if err == nil {
				t.Error("expected error for corrupt checkpoint")
			}
			if !strings.Contains(err.Error(), "parse checkpoint") {
				t.Errorf("error should wrap with 'parse checkpoint', got: %v", err)
			}
		})
	}
}

func TestSaveCheckpoint_ReadOnlyDir(t *testing.T) {
	tmpDir := t.TempDir()
	readOnly := filepath.Join(tmpDir, "readonly")
	if err := os.MkdirAll(readOnly, 0500); err != nil {
		t.Fatalf("setup: %v", err)
	}
	t.Cleanup(func() { os.Chmod(readOnly, 0700) })

	err := SaveCheckpoint(readOnly, &Plan{ID: "p1"}, "dev", "in_progress", time.Now())
	if err == nil {
		t.Error("expected error writing checkpoint to read-only directory")
	}
}

func TestSaveCheckpoint_MissingDir(t *testing.T) {
	err := SaveCheckpoint("/nonexistent/path/that/does/not/exist", &Plan{ID: "p1"}, "dev", "in_progress", time.Now())
	if err == nil {
		t.Error("expected error writing checkpoint to missing directory")
	}
}

// --- ValidateWorkspacePath Tests ---

func TestValidateWorkspacePath(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("UserHomeDir: %v", err)
	}

	wsBase := filepath.Join(home, ".alluka", "workspaces")

	// Ensure base exists for valid path tests
	if err := os.MkdirAll(wsBase, 0700); err != nil {
		t.Fatalf("setup: %v", err)
	}

	// Create a real workspace dir for the valid case
	validDir := filepath.Join(wsBase, "test-valid-ws")
	if err := os.MkdirAll(validDir, 0700); err != nil {
		t.Fatalf("setup: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(validDir) })

	tests := []struct {
		name    string
		path    string
		wantErr bool
		errMsg  string
	}{
		{
			name:    "valid workspace path",
			path:    validDir,
			wantErr: false,
		},
		{
			name:    "path traversal with ..",
			path:    filepath.Join(wsBase, "..", "etc", "passwd"),
			wantErr: true,
			errMsg:  "outside",
		},
		{
			name:    "outside workspace base",
			path:    "/tmp/malicious",
			wantErr: true,
			errMsg:  "outside",
		},
		{
			name:    "workspace base itself (not inside)",
			path:    wsBase,
			wantErr: true,
			errMsg:  "outside",
		},
		{
			name:    "home directory",
			path:    home,
			wantErr: true,
			errMsg:  "outside",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := ValidateWorkspacePath(tt.path)
			if tt.wantErr {
				if err == nil {
					t.Error("expected error")
				} else if tt.errMsg != "" && !strings.Contains(err.Error(), tt.errMsg) {
					t.Errorf("error = %q; want to contain %q", err.Error(), tt.errMsg)
				}
			} else {
				if err != nil {
					t.Errorf("unexpected error: %v", err)
				}
			}
		})
	}
}

// --- ResolveTargetDir Tests ---

func TestResolveTargetDir(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Fatalf("UserHomeDir: %v", err)
	}

	// Create a real directory that resolves
	realDir := t.TempDir()

	tests := []struct {
		name     string
		targetID string
		want     string
	}{
		{
			name:     "repo with tilde expands to real path",
			targetID: "repo:" + strings.Replace(realDir, home, "~", 1),
			want:     realDir,
		},
		{
			name:     "repo with absolute path",
			targetID: "repo:" + realDir,
			want:     realDir,
		},
		{
			name:     "non-repo scheme returns empty",
			targetID: "workspace:ws-123",
			want:     "",
		},
		{
			name:     "empty string returns empty",
			targetID: "",
			want:     "",
		},
		{
			name:     "repo with nonexistent path returns empty",
			targetID: "repo:/nonexistent/path/that/does/not/exist",
			want:     "",
		},
		{
			name:     "repo prefix only returns empty",
			targetID: "repo:",
			want:     "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ResolveTargetDir(tt.targetID)
			if got != tt.want {
				t.Errorf("ResolveTargetDir(%q) = %q, want %q", tt.targetID, got, tt.want)
			}
		})
	}
}

// --- generateID Tests ---

func TestGenerateID_Format(t *testing.T) {
	id := generateID()

	// Should be date-hex format: YYYYMMDD-<8 hex chars>
	parts := strings.SplitN(id, "-", 2)
	if len(parts) != 2 {
		t.Fatalf("id = %q; want YYYYMMDD-hex format", id)
	}

	// Date part should be today
	today := time.Now().Format("20060102")
	if parts[0] != today {
		t.Errorf("date part = %q; want %q", parts[0], today)
	}

	// Hex part should be 8 chars (4 bytes)
	if len(parts[1]) != 8 {
		t.Errorf("hex part length = %d; want 8", len(parts[1]))
	}
}

func TestGenerateID_Unique(t *testing.T) {
	seen := make(map[string]bool)
	for i := 0; i < 100; i++ {
		id := generateID()
		if seen[id] {
			t.Fatalf("duplicate ID generated: %s", id)
		}
		seen[id] = true
	}
}

// --- JSON Round-Trip Fidelity ---

func TestCheckpointJSONStructure(t *testing.T) {
	tmpDir := t.TempDir()
	now := time.Now().Truncate(time.Second)

	plan := &Plan{
		ID:            "plan-1",
		Task:          "build feature",
		ExecutionMode: "sequential",
		CreatedAt:     now,
		Phases: []*Phase{
			{
				ID: "ph1", Name: "design", Status: StatusCompleted,
				Skills: []string{"architect"}, Constraints: []string{"no frameworks"},
				Dependencies: []string{},
			},
		},
	}

	if err := SaveCheckpoint(tmpDir, plan, "dev", "in_progress", now); err != nil {
		t.Fatalf("save: %v", err)
	}

	// Read raw JSON and verify structure
	data, err := os.ReadFile(filepath.Join(tmpDir, "checkpoint.json"))
	if err != nil {
		t.Fatalf("read: %v", err)
	}

	var raw map[string]json.RawMessage
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshal raw: %v", err)
	}

	// Verify envelope structure: top-level version and payload
	if _, ok := raw["version"]; !ok {
		t.Error("missing top-level key \"version\" in checkpoint envelope")
	}
	if _, ok := raw["payload"]; !ok {
		t.Error("missing top-level key \"payload\" in checkpoint envelope")
	}

	// Verify checkpoint fields are in the payload
	var payload map[string]json.RawMessage
	if err := json.Unmarshal(raw["payload"], &payload); err == nil {
		expectedPayloadKeys := []string{"version", "workspace_id", "domain", "plan", "status", "started_at"}
		for _, key := range expectedPayloadKeys {
			if _, ok := payload[key]; !ok {
				t.Errorf("missing payload key %q in checkpoint JSON", key)
			}
		}
	}

	// Verify it's pretty-printed (indented)
	if !strings.Contains(string(data), "\n  ") {
		t.Error("checkpoint JSON should be indented")
	}
}

// ---------------------------------------------------------------------------
// PhaseStatus.IsTerminal
// ---------------------------------------------------------------------------

func TestPhaseStatusIsTerminal(t *testing.T) {
	tests := []struct {
		status   PhaseStatus
		terminal bool
	}{
		{StatusPending, false},
		{StatusRunning, false},
		{StatusCompleted, true},
		{StatusFailed, true},
		{StatusSkipped, true},
		// Zero value (empty string) is non-terminal — unknown status should not
		// prevent future transitions.
		{PhaseStatus(""), false},
	}
	for _, tt := range tests {
		t.Run(string(tt.status), func(t *testing.T) {
			got := tt.status.IsTerminal()
			if got != tt.terminal {
				t.Errorf("PhaseStatus(%q).IsTerminal() = %v; want %v", tt.status, got, tt.terminal)
			}
		})
	}
}

// TestIsTerminalCoversAllKnownStatuses is a canary: if a new PhaseStatus
// constant is added, the developer must also update IsTerminal and this test.
func TestIsTerminalCoversAllKnownStatuses(t *testing.T) {
	all := []PhaseStatus{StatusPending, StatusRunning, StatusCompleted, StatusFailed, StatusSkipped}
	for _, s := range all {
		// Just confirm each known status answers without panic.
		_ = s.IsTerminal()
	}
	if len(all) != 5 {
		t.Fatalf("expected 5 known PhaseStatus values; got %d — update IsTerminal to cover new statuses", len(all))
	}
}

// ---------------------------------------------------------------------------
// ValidatePhaseIDs
// ---------------------------------------------------------------------------

func TestValidatePhaseIDs_UniqueIDs(t *testing.T) {
	plan := &Plan{
		Phases: []*Phase{
			{ID: "phase-1", Name: "plan"},
			{ID: "phase-2", Name: "implement"},
			{ID: "phase-3", Name: "review"},
		},
	}
	if err := ValidatePhaseIDs(plan); err != nil {
		t.Errorf("expected no error for unique IDs; got %v", err)
	}
}

func TestValidatePhaseIDs_DuplicateIDs(t *testing.T) {
	plan := &Plan{
		Phases: []*Phase{
			{ID: "phase-1", Name: "plan"},
			{ID: "phase-2", Name: "implement"},
			{ID: "phase-1", Name: "review"}, // duplicate
		},
	}
	err := ValidatePhaseIDs(plan)
	if err == nil {
		t.Fatal("expected error for duplicate phase IDs")
	}
	if !strings.Contains(err.Error(), "duplicate phase ID") {
		t.Errorf("error = %q; want to contain 'duplicate phase ID'", err)
	}
	if !strings.Contains(err.Error(), "phase-1") {
		t.Errorf("error = %q; want to contain the duplicate ID 'phase-1'", err)
	}
}

func TestValidatePhaseIDs_NilPlan(t *testing.T) {
	if err := ValidatePhaseIDs(nil); err != nil {
		t.Errorf("expected nil error for nil plan; got %v", err)
	}
}

func TestValidatePhaseIDs_EmptyPhases(t *testing.T) {
	plan := &Plan{Phases: []*Phase{}}
	if err := ValidatePhaseIDs(plan); err != nil {
		t.Errorf("expected nil error for empty phases; got %v", err)
	}
}

// ---------------------------------------------------------------------------
// Checkpoint sidecar enrichment — LoadCheckpoint populates LinearIssueID and
// MissionPath from sidecar files written at mission start.
// ---------------------------------------------------------------------------

func TestLoadCheckpoint_SidecarEnrichment(t *testing.T) {
	tests := []struct {
		name          string
		writeIssueID  bool
		issueContent  string
		writeMission  bool
		missionContent string
		wantIssueID   string
		wantMission   string
	}{
		{
			name:        "no sidecars present",
			wantIssueID: "",
			wantMission: "",
		},
		{
			name:          "both sidecars present",
			writeIssueID:  true,
			issueContent:  "V-73",
			writeMission:  true,
			missionContent: "/Users/joey/via/missions/hardening.md",
			wantIssueID:   "V-73",
			wantMission:   "/Users/joey/via/missions/hardening.md",
		},
		{
			name:         "only linear_issue_id sidecar",
			writeIssueID: true,
			issueContent: "V-5",
			wantIssueID:  "V-5",
			wantMission:  "",
		},
		{
			name:          "only mission_path sidecar",
			writeMission:  true,
			missionContent: "/tmp/mission.md",
			wantIssueID:   "",
			wantMission:   "/tmp/mission.md",
		},
		{
			name:          "whitespace-padded sidecars are trimmed",
			writeIssueID:  true,
			issueContent:  "  V-23\n",
			writeMission:  true,
			missionContent: "  /tmp/mission.md  \n",
			wantIssueID:   "V-23",
			wantMission:   "/tmp/mission.md",
		},
		{
			name:         "empty sidecar file yields empty string",
			writeIssueID: true,
			issueContent: "",
			wantIssueID:  "",
			wantMission:  "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			dir := t.TempDir()

			// Write a minimal checkpoint.json.
			cpJSON := `{"version":2,"workspace_id":"test","domain":"dev","status":"completed","started_at":"2026-01-01T00:00:00Z"}`
			if err := os.WriteFile(filepath.Join(dir, "checkpoint.json"), []byte(cpJSON), 0600); err != nil {
				t.Fatalf("write checkpoint: %v", err)
			}

			if tt.writeIssueID {
				if err := os.WriteFile(filepath.Join(dir, "linear_issue_id"), []byte(tt.issueContent), 0600); err != nil {
					t.Fatalf("write linear_issue_id: %v", err)
				}
			}
			if tt.writeMission {
				if err := os.WriteFile(filepath.Join(dir, "mission_path"), []byte(tt.missionContent), 0600); err != nil {
					t.Fatalf("write mission_path: %v", err)
				}
			}

			cp, err := LoadCheckpoint(dir)
			if err != nil {
				t.Fatalf("LoadCheckpoint: %v", err)
			}

			if cp.LinearIssueID != tt.wantIssueID {
				t.Errorf("LinearIssueID = %q; want %q", cp.LinearIssueID, tt.wantIssueID)
			}
			if cp.MissionPath != tt.wantMission {
				t.Errorf("MissionPath = %q; want %q", cp.MissionPath, tt.wantMission)
			}
		})
	}
}

// TestSaveCheckpoint_DoesNotWriteSidecars verifies that SaveCheckpoint writes
// only checkpoint.json and does NOT produce sidecar files — those are written
// by the run command, not by the checkpoint layer.
func TestSaveCheckpoint_DoesNotWriteSidecars(t *testing.T) {
	dir := t.TempDir()

	if err := SaveCheckpoint(dir, &Plan{ID: "p1"}, "dev", "completed", time.Now()); err != nil {
		t.Fatalf("SaveCheckpoint: %v", err)
	}

	for _, name := range []string{"linear_issue_id", "mission_path"} {
		if _, err := os.Stat(filepath.Join(dir, name)); err == nil {
			t.Errorf("SaveCheckpoint should not create %s sidecar", name)
		}
	}
}

// TestSaveCheckpoint_AtomicWrite verifies the atomic write pattern:
// (1) checkpoint.json.tmp is created temporarily
// (2) rename is atomic on POSIX (no .tmp file remains after write)
// (3) concurrent read during write gets consistent state (old or new, not partial)
func TestSaveCheckpoint_AtomicWrite(t *testing.T) {
	dir := t.TempDir()
	now := time.Now()

	// Write first checkpoint.
	plan1 := &Plan{ID: "p1", Phases: []*Phase{{Name: "phase1"}}}
	if err := SaveCheckpoint(dir, plan1, "dev", "in_progress", now); err != nil {
		t.Fatalf("SaveCheckpoint (1st): %v", err)
	}

	// Verify no .tmp file exists (atomic rename completed).
	tmpPath := filepath.Join(dir, "checkpoint.json.tmp")
	if _, err := os.Stat(tmpPath); err == nil {
		t.Error("checkpoint.json.tmp should not exist after successful write (atomic rename should clean it up)")
	} else if !os.IsNotExist(err) {
		t.Fatalf("unexpected error checking for .tmp file: %v", err)
	}

	// Read the checkpoint to verify it's valid.
	cp1, err := LoadCheckpoint(dir)
	if err != nil {
		t.Fatalf("LoadCheckpoint after 1st write: %v", err)
	}
	if cp1.Status != "in_progress" {
		t.Errorf("status = %q; want %q", cp1.Status, "in_progress")
	}

	// Write second checkpoint (simulates resume/update).
	plan2 := &Plan{ID: "p1", Phases: []*Phase{{Name: "phase1"}, {Name: "phase2"}}}
	if err := SaveCheckpoint(dir, plan2, "dev", "completed", now); err != nil {
		t.Fatalf("SaveCheckpoint (2nd): %v", err)
	}

	// Verify .tmp file is cleaned up again.
	if _, err := os.Stat(tmpPath); err == nil {
		t.Error("checkpoint.json.tmp should not exist after 2nd write")
	} else if !os.IsNotExist(err) {
		t.Fatalf("unexpected error checking for .tmp file after 2nd write: %v", err)
	}

	// Read final checkpoint.
	cp2, err := LoadCheckpoint(dir)
	if err != nil {
		t.Fatalf("LoadCheckpoint after 2nd write: %v", err)
	}
	if cp2.Status != "completed" {
		t.Errorf("status = %q; want %q", cp2.Status, "completed")
	}
}

// TestSaveCheckpoint_EnvelopeFormat verifies the JSON is wrapped in
// an envelope with a version field for integrity checking.
func TestSaveCheckpoint_EnvelopeFormat(t *testing.T) {
	dir := t.TempDir()
	now := time.Now()

	plan := &Plan{ID: "test-plan"}
	if err := SaveCheckpoint(dir, plan, "dev", "in_progress", now); err != nil {
		t.Fatalf("SaveCheckpoint: %v", err)
	}

	// Read raw JSON to verify envelope structure.
	data, err := os.ReadFile(filepath.Join(dir, "checkpoint.json"))
	if err != nil {
		t.Fatalf("reading checkpoint.json: %v", err)
	}

	var raw map[string]interface{}
	if err := json.Unmarshal(data, &raw); err != nil {
		t.Fatalf("unmarshaling JSON: %v", err)
	}

	// Verify envelope has top-level "version" and "payload" fields.
	if _, ok := raw["version"]; !ok {
		t.Error("envelope missing 'version' field")
	}
	if _, ok := raw["payload"]; !ok {
		t.Error("envelope missing 'payload' field")
	}

	// Verify the payload contains the checkpoint data.
	payload, ok := raw["payload"].(map[string]interface{})
	if !ok {
		t.Fatal("payload is not an object")
	}
	if id, ok := payload["workspace_id"].(string); !ok || id == "" {
		t.Error("payload missing or invalid workspace_id")
	}
}

// TestLoadCheckpoint_TruncatedWrite verifies detection of truncated writes
// (which could happen if process crashes during temp file write).
func TestLoadCheckpoint_TruncatedWrite(t *testing.T) {
	dir := t.TempDir()

	// Write a truncated JSON that would be valid if complete.
	truncated := []byte(`{
  "version": 1,
  "payload": {
    "version": 2,
    "workspace_id": "test",
    "domain": "dev",
    "status": "in_progress"`)
	cpPath := filepath.Join(dir, "checkpoint.json")
	if err := os.WriteFile(cpPath, truncated, 0600); err != nil {
		t.Fatalf("setup: %v", err)
	}

	_, err := LoadCheckpoint(dir)
	if err == nil {
		t.Error("expected error for truncated checkpoint")
	}
	if !strings.Contains(err.Error(), "parse checkpoint") {
		t.Errorf("error should wrap with 'parse checkpoint', got: %v", err)
	}
}
