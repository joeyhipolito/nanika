package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// ---------------------------------------------------------------------------
// execTracker — binary-not-found handling
// ---------------------------------------------------------------------------

// TestExecTracker_BinaryNotFound verifies that execTracker returns a descriptive
// error when the tracker binary cannot be found on PATH.
//
// Strategy: set HOME to a temp dir (so enrichedEnv adds non-existent ~/bin
// paths) and set PATH to empty, ensuring tracker is unreachable.
func TestExecTracker_BinaryNotFound(t *testing.T) {
	tmpDir := t.TempDir()
	t.Setenv("HOME", tmpDir)
	t.Setenv("PATH", "")

	_, err := execTracker(3*time.Second, "query", "items", "--json")
	if err == nil {
		t.Fatal("expected error when tracker binary is not on PATH, got nil")
	}
	if !strings.Contains(err.Error(), "tracker") {
		t.Errorf("error message should mention 'tracker', got: %v", err)
	}
}

// ---------------------------------------------------------------------------
// TrackerUpdate — input validation (no binary required)
// ---------------------------------------------------------------------------

func TestTrackerUpdate_ValidationErrors(t *testing.T) {
	a := &App{}

	tests := []struct {
		name    string
		reqJSON string
		wantErr string
	}{
		{
			name:    "missing id returns error",
			reqJSON: `{"status":"open"}`,
			wantErr: "id is required",
		},
		{
			name:    "path-traversal slash in id rejected",
			reqJSON: `{"id":"../etc/passwd","status":"open"}`,
			wantErr: "invalid id",
		},
		{
			name:    "path-traversal backslash in id rejected",
			reqJSON: `{"id":"foo\\bar","status":"open"}`,
			wantErr: "invalid id",
		},
		{
			name:    "dotdot in id rejected",
			reqJSON: `{"id":"foo..bar","status":"open"}`,
			wantErr: "invalid id",
		},
		{
			name:    "no fields besides id returns error",
			reqJSON: `{"id":"trk-0001"}`,
			wantErr: "at least one field",
		},
		{
			name:    "malformed JSON returns error",
			reqJSON: `not-json`,
			wantErr: "parsing tracker update request",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			_, err := a.TrackerUpdate(tt.reqJSON)
			if err == nil {
				t.Fatalf("want error containing %q, got nil", tt.wantErr)
			}
			if !strings.Contains(err.Error(), tt.wantErr) {
				t.Errorf("want error containing %q, got: %v", tt.wantErr, err)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// TrackerItems — error propagation when binary absent
// ---------------------------------------------------------------------------

func TestTrackerItems_ErrorWhenBinaryAbsent(t *testing.T) {
	tmpDir := t.TempDir()
	t.Setenv("HOME", tmpDir)
	t.Setenv("PATH", "")

	a := &App{}
	result, err := a.TrackerItems()
	if err == nil {
		t.Fatalf("expected error when tracker binary is not on PATH, got result: %q", result)
	}
	if !strings.Contains(err.Error(), "tracker") {
		t.Errorf("error message should reference 'tracker', got: %v", err)
	}
}

// ---------------------------------------------------------------------------
// TrackerStats — error propagation when binary absent
// ---------------------------------------------------------------------------

func TestTrackerStats_ErrorWhenBinaryAbsent(t *testing.T) {
	tmpDir := t.TempDir()
	t.Setenv("HOME", tmpDir)
	t.Setenv("PATH", "")

	a := &App{}
	result, err := a.TrackerStats()
	if err == nil {
		t.Fatalf("expected error when tracker binary is not on PATH, got result: %q", result)
	}
	if !strings.Contains(err.Error(), "tracker") {
		t.Errorf("error message should reference 'tracker', got: %v", err)
	}
}

// ---------------------------------------------------------------------------
// TrackerItems — happy path: parses items envelope correctly
// ---------------------------------------------------------------------------

// TestTrackerItems_ParsesEnvelope verifies that TrackerItems correctly unwraps
// the {"items":[...]} envelope returned by the CLI.
//
// We create a fake 'tracker' binary (a shell script) in a temp dir and put it
// on PATH so execTracker picks it up.
func TestTrackerItems_ParsesEnvelope(t *testing.T) {
	fakeOutput := `{"items":[{"id":"trk-0001","title":"Fix bug","status":"open","priority":"P0"}]}`
	fakeTracker := createFakeTrackerScript(t, 0, fakeOutput)
	prependToPath(t, filepath.Dir(fakeTracker))

	a := &App{}
	result, err := a.TrackerItems()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// Result should be the items array, not the envelope
	var items []map[string]interface{}
	if err := json.Unmarshal([]byte(result), &items); err != nil {
		t.Fatalf("result is not a JSON array: %v (got: %s)", err, result)
	}
	if len(items) != 1 {
		t.Errorf("want 1 item, got %d", len(items))
	}
	if items[0]["id"] != "trk-0001" {
		t.Errorf("want id trk-0001, got %v", items[0]["id"])
	}
}

// TestTrackerItems_EmptyItemsArray verifies that an empty items array returns "[]".
func TestTrackerItems_EmptyItemsArray(t *testing.T) {
	fakeOutput := `{"items":[]}`
	fakeTracker := createFakeTrackerScript(t, 0, fakeOutput)
	prependToPath(t, filepath.Dir(fakeTracker))

	a := &App{}
	result, err := a.TrackerItems()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if result != "[]" {
		t.Errorf("want [], got %s", result)
	}
}

// ---------------------------------------------------------------------------
// TrackerStats — happy path: computes counts from items
// ---------------------------------------------------------------------------

func TestTrackerStats_ComputesCounts(t *testing.T) {
	fakeOutput := `{"items":[
		{"id":"1","status":"open","priority":"P0"},
		{"id":"2","status":"open","priority":"P1"},
		{"id":"3","status":"done","priority":"P0"}
	]}`
	fakeTracker := createFakeTrackerScript(t, 0, fakeOutput)
	prependToPath(t, filepath.Dir(fakeTracker))

	a := &App{}
	raw, err := a.TrackerStats()
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var stats struct {
		Total      int            `json:"total"`
		ByStatus   map[string]int `json:"by_status"`
		ByPriority map[string]int `json:"by_priority"`
	}
	if err := json.Unmarshal([]byte(raw), &stats); err != nil {
		t.Fatalf("failed to parse stats JSON: %v (got: %s)", err, raw)
	}

	if stats.Total != 3 {
		t.Errorf("want total=3, got %d", stats.Total)
	}
	if stats.ByStatus["open"] != 2 {
		t.Errorf("want by_status.open=2, got %d", stats.ByStatus["open"])
	}
	if stats.ByStatus["done"] != 1 {
		t.Errorf("want by_status.done=1, got %d", stats.ByStatus["done"])
	}
	if stats.ByPriority["P0"] != 2 {
		t.Errorf("want by_priority.P0=2, got %d", stats.ByPriority["P0"])
	}
}

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

// createFakeTrackerScript writes a shell script named 'tracker' to a temp dir
// that exits with the given code and prints the given output to stdout.
func createFakeTrackerScript(t *testing.T, exitCode int, stdout string) string {
	t.Helper()
	dir := t.TempDir()
	scriptPath := filepath.Join(dir, "tracker")
	script := "#!/bin/sh\n"
	if stdout != "" {
		// Use printf to avoid shell interpretation of the JSON payload.
		// Write to a temp file and cat it to avoid quoting nightmares.
		payloadFile := filepath.Join(dir, "payload.json")
		if err := os.WriteFile(payloadFile, []byte(stdout), 0644); err != nil {
			t.Fatalf("write payload: %v", err)
		}
		script += "cat " + payloadFile + "\n"
	}
	if exitCode != 0 {
		script += "exit " + string(rune('0'+exitCode)) + "\n"
	}
	if err := os.WriteFile(scriptPath, []byte(script), 0755); err != nil {
		t.Fatalf("write fake tracker script: %v", err)
	}
	return scriptPath
}

// prependToPath prepends dir to the process PATH so that binaries in dir are
// found before any others. Uses t.Setenv so the original PATH is restored.
func prependToPath(t *testing.T, dir string) {
	t.Helper()
	current := os.Getenv("PATH")
	t.Setenv("PATH", dir+":"+current)
}
