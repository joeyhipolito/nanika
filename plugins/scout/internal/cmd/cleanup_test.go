package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// writeIntelFile creates a fake intel JSON file at the given path.
func writeIntelFile(t *testing.T, path string, mtime time.Time) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	content := `{"topic":"test","gathered_at":"2020-01-01T00:00:00Z","source":"hackernews","items":[]}`
	if err := os.WriteFile(path, []byte(content), 0600); err != nil {
		t.Fatalf("write file: %v", err)
	}
	if err := os.Chtimes(path, mtime, mtime); err != nil {
		t.Fatalf("chtimes: %v", err)
	}
}

// intelPath constructs a path under the temp home's .scout/intel directory.
func intelPath(home, topic, filename string) string {
	return filepath.Join(home, ".scout", "intel", topic, filename)
}

// ─── findOldIntelFiles ────────────────────────────────────────────────────────

func TestFindOldIntelFiles_EmptyDir(t *testing.T) {
	home := setupTempHome(t)
	intelDir := filepath.Join(home, ".scout", "intel")
	if err := os.MkdirAll(intelDir, 0700); err != nil {
		t.Fatal(err)
	}

	cutoff := time.Now().Add(-7 * 24 * time.Hour)
	today := time.Now().Truncate(24 * time.Hour)

	candidates, err := findOldIntelFiles(intelDir, cutoff, today)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(candidates) != 0 {
		t.Errorf("expected 0 candidates, got %d", len(candidates))
	}
}

func TestFindOldIntelFiles_NonexistentDir(t *testing.T) {
	candidates, err := findOldIntelFiles("/nonexistent/path", time.Now(), time.Now())
	if err != nil {
		t.Fatalf("expected no error for missing dir, got: %v", err)
	}
	if len(candidates) != 0 {
		t.Errorf("expected 0 candidates for missing dir, got %d", len(candidates))
	}
}

func TestFindOldIntelFiles_OldFilesSelected(t *testing.T) {
	home := setupTempHome(t)
	intelDir := filepath.Join(home, ".scout", "intel")

	old := time.Now().Add(-40 * 24 * time.Hour)
	writeIntelFile(t, intelPath(home, "ai-models", "2020-01-01T000000_hackernews.json"), old)
	writeIntelFile(t, intelPath(home, "ai-models", "2020-01-02T000000_reddit.json"), old)

	cutoff := time.Now().Add(-30 * 24 * time.Hour)
	today := time.Now().Truncate(24 * time.Hour)

	candidates, err := findOldIntelFiles(intelDir, cutoff, today)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(candidates) != 2 {
		t.Errorf("expected 2 candidates, got %d", len(candidates))
	}
	for _, c := range candidates {
		if c.topic != "ai-models" {
			t.Errorf("expected topic ai-models, got %s", c.topic)
		}
	}
}

func TestFindOldIntelFiles_RecentFilesNotSelected(t *testing.T) {
	home := setupTempHome(t)
	intelDir := filepath.Join(home, ".scout", "intel")

	recent := time.Now().Add(-2 * 24 * time.Hour)
	writeIntelFile(t, intelPath(home, "ai-models", "2020-01-01T000000_hackernews.json"), recent)

	cutoff := time.Now().Add(-30 * 24 * time.Hour)
	today := time.Now().Truncate(24 * time.Hour)

	candidates, err := findOldIntelFiles(intelDir, cutoff, today)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(candidates) != 0 {
		t.Errorf("expected 0 candidates for recent files, got %d", len(candidates))
	}
}

func TestFindOldIntelFiles_TodayFilesNeverSelected(t *testing.T) {
	home := setupTempHome(t)
	intelDir := filepath.Join(home, ".scout", "intel")

	// File from today, even with a very long TTL
	now := time.Now()
	writeIntelFile(t, intelPath(home, "ai-models", "today_hackernews.json"), now)

	cutoff := time.Now().Add(-1 * time.Second) // 1 second ago — nearly everything qualifies
	today := time.Now().Truncate(24 * time.Hour)

	candidates, err := findOldIntelFiles(intelDir, cutoff, today)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(candidates) != 0 {
		t.Errorf("today's files must never be selected, got %d candidates", len(candidates))
	}
}

func TestFindOldIntelFiles_MultipleTopics(t *testing.T) {
	home := setupTempHome(t)
	intelDir := filepath.Join(home, ".scout", "intel")

	old := time.Now().Add(-40 * 24 * time.Hour)
	recent := time.Now().Add(-5 * 24 * time.Hour)

	writeIntelFile(t, intelPath(home, "ai-models", "old_hackernews.json"), old)
	writeIntelFile(t, intelPath(home, "go-lang", "old_reddit.json"), old)
	writeIntelFile(t, intelPath(home, "ai-models", "recent_devto.json"), recent)

	cutoff := time.Now().Add(-30 * 24 * time.Hour)
	today := time.Now().Truncate(24 * time.Hour)

	candidates, err := findOldIntelFiles(intelDir, cutoff, today)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(candidates) != 2 {
		t.Errorf("expected 2 old candidates, got %d", len(candidates))
	}
}

func TestFindOldIntelFiles_NonJSONFilesIgnored(t *testing.T) {
	home := setupTempHome(t)
	intelDir := filepath.Join(home, ".scout", "intel")

	old := time.Now().Add(-40 * 24 * time.Hour)
	topicDir := filepath.Join(intelDir, "ai-models")
	if err := os.MkdirAll(topicDir, 0700); err != nil {
		t.Fatal(err)
	}

	// Write a non-JSON file with old mtime
	txtPath := filepath.Join(topicDir, "notes.txt")
	if err := os.WriteFile(txtPath, []byte("notes"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chtimes(txtPath, old, old); err != nil {
		t.Fatal(err)
	}

	cutoff := time.Now().Add(-30 * 24 * time.Hour)
	today := time.Now().Truncate(24 * time.Hour)

	candidates, err := findOldIntelFiles(intelDir, cutoff, today)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(candidates) != 0 {
		t.Errorf("non-JSON files must be ignored, got %d candidates", len(candidates))
	}
}

// ─── CleanupCmd ──────────────────────────────────────────────────────────────

func TestCleanupCmd_DefaultIsDryRun(t *testing.T) {
	home := setupTempHome(t)

	old := time.Now().Add(-40 * 24 * time.Hour)
	filePath := intelPath(home, "ai-models", "old_hackernews.json")
	writeIntelFile(t, filePath, old)

	// No --older flag → dry-run by default, no files deleted
	if err := CleanupCmd([]string{}, false); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if _, err := os.Stat(filePath); os.IsNotExist(err) {
		t.Error("default (no --older) should be dry-run and must not delete files")
	}
}

func TestCleanupCmd_DryRunFlag(t *testing.T) {
	home := setupTempHome(t)

	old := time.Now().Add(-40 * 24 * time.Hour)
	filePath := intelPath(home, "ai-models", "old_hackernews.json")
	writeIntelFile(t, filePath, old)

	if err := CleanupCmd([]string{"--older", "30d", "--dry-run"}, false); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if _, err := os.Stat(filePath); os.IsNotExist(err) {
		t.Error("--dry-run must not delete files")
	}
}

func TestCleanupCmd_DeletesOldFiles(t *testing.T) {
	home := setupTempHome(t)

	old := time.Now().Add(-40 * 24 * time.Hour)
	filePath := intelPath(home, "ai-models", "old_hackernews.json")
	writeIntelFile(t, filePath, old)

	if err := CleanupCmd([]string{"--older", "30d"}, false); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if _, err := os.Stat(filePath); !os.IsNotExist(err) {
		t.Error("expected old file to be deleted")
	}
}

func TestCleanupCmd_PreservesRecentFiles(t *testing.T) {
	home := setupTempHome(t)

	recent := time.Now().Add(-5 * 24 * time.Hour)
	filePath := intelPath(home, "ai-models", "recent_hackernews.json")
	writeIntelFile(t, filePath, recent)

	if err := CleanupCmd([]string{"--older", "30d"}, false); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if _, err := os.Stat(filePath); os.IsNotExist(err) {
		t.Error("recent files must not be deleted")
	}
}

func TestCleanupCmd_NeverDeletesToday(t *testing.T) {
	home := setupTempHome(t)

	// File created right now
	filePath := intelPath(home, "ai-models", "today_hackernews.json")
	writeIntelFile(t, filePath, time.Now())

	// Use 1 second TTL — nearly every file would qualify, but today's must be spared
	if err := CleanupCmd([]string{"--older", "1s"}, false); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	if _, err := os.Stat(filePath); os.IsNotExist(err) {
		t.Error("today's files must never be deleted")
	}
}

func TestCleanupCmd_NoIntelDir(t *testing.T) {
	setupTempHome(t)
	// No intel files at all — should succeed silently
	if err := CleanupCmd([]string{"--older", "7d"}, false); err != nil {
		t.Fatalf("unexpected error with empty intel dir: %v", err)
	}
}

func TestCleanupCmd_UnknownFlag(t *testing.T) {
	setupTempHome(t)
	err := CleanupCmd([]string{"--bogus"}, false)
	if err == nil {
		t.Fatal("expected error for unknown flag")
	}
	if !strings.Contains(err.Error(), "--bogus") {
		t.Errorf("error should mention the unknown flag, got: %v", err)
	}
}

func TestCleanupCmd_InvalidOlderValue(t *testing.T) {
	setupTempHome(t)
	err := CleanupCmd([]string{"--older", "notaduration"}, false)
	if err == nil {
		t.Fatal("expected error for invalid duration")
	}
}

func TestCleanupCmd_OlderMissingArgument(t *testing.T) {
	setupTempHome(t)
	err := CleanupCmd([]string{"--older"}, false)
	if err == nil {
		t.Fatal("expected error when --older has no argument")
	}
}

func TestCleanupCmd_JSONOutputDryRun(t *testing.T) {
	home := setupTempHome(t)

	old := time.Now().Add(-40 * 24 * time.Hour)
	writeIntelFile(t, intelPath(home, "ai-models", "old.json"), old)

	// Capture stdout
	r, w, _ := os.Pipe()
	old2 := os.Stdout
	os.Stdout = w

	err := CleanupCmd([]string{"--older", "30d", "--dry-run"}, true)
	w.Close()
	os.Stdout = old2

	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	output := string(buf[:n])

	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, `"dry_run"`) {
		t.Errorf("JSON output should contain dry_run field, got: %s", output)
	}
	if !strings.Contains(output, `"files_found"`) {
		t.Errorf("JSON output should contain files_found field, got: %s", output)
	}
}

func TestCleanupCmd_JSONOutputDelete(t *testing.T) {
	home := setupTempHome(t)

	old := time.Now().Add(-40 * 24 * time.Hour)
	writeIntelFile(t, intelPath(home, "ai-models", "old.json"), old)

	r, w, _ := os.Pipe()
	old2 := os.Stdout
	os.Stdout = w

	err := CleanupCmd([]string{"--older", "30d"}, true)
	w.Close()
	os.Stdout = old2

	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	output := string(buf[:n])

	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, `"files_removed"`) {
		t.Errorf("JSON output should contain files_removed field, got: %s", output)
	}
	if !strings.Contains(output, `"bytes_freed"`) {
		t.Errorf("JSON output should contain bytes_freed field, got: %s", output)
	}
}

// ─── formatBytes ─────────────────────────────────────────────────────────────

func TestFormatBytes(t *testing.T) {
	tests := []struct {
		input int64
		want  string
	}{
		{0, "0 B"},
		{512, "512 B"},
		{1023, "1023 B"},
		{1024, "1.0 KB"},
		{1536, "1.5 KB"},
		{1024 * 1024, "1.0 MB"},
		{1024 * 1024 * 1024, "1.0 GB"},
	}
	for _, tc := range tests {
		got := formatBytes(tc.input)
		if got != tc.want {
			t.Errorf("formatBytes(%d) = %q, want %q", tc.input, got, tc.want)
		}
	}
}
