package worker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestEncodeProjectKey(t *testing.T) {
	tests := []struct {
		name string
		dir  string
		want string
	}{
		{
			name: "simple path",
			dir:  "/Users/joey/project",
			want: "-Users-joey-project",
		},
		{
			name: "path with dots",
			dir:  "/Users/joey/.via/workspaces/abc",
			want: "-Users-joey--via-workspaces-abc",
		},
		{
			name: "dots and slashes",
			dir:  "/home/user/.config/app.d/test",
			want: "-home-user--config-app-d-test",
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := encodeProjectKey(tt.dir)
			if got != tt.want {
				t.Errorf("encodeProjectKey(%q) = %q, want %q", tt.dir, got, tt.want)
			}
		})
	}
}

func TestSeedMemory_CreatesCanonicalIfAbsent(t *testing.T) {
	// Use temp dirs to avoid touching real persona files.
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	workerDir := filepath.Join(tmpHome, "worker", "test-worker")
	if err := os.MkdirAll(workerDir, 0700); err != nil {
		t.Fatal(err)
	}

	err := seedMemory("test-persona", workerDir)
	if err != nil {
		t.Fatalf("seedMemory: %v", err)
	}

	// Canonical file should have been created.
	canonical := filepath.Join(tmpHome, "nanika", "personas", "test-persona", "MEMORY.md")
	if _, err := os.Stat(canonical); err != nil {
		t.Fatalf("canonical MEMORY.md not created: %v", err)
	}

	// Worker memory should exist at the encoded path.
	key := encodeProjectKey(workerDir)
	workerMem := filepath.Join(tmpHome, ".claude", "projects", key, "memory", "MEMORY.md")
	if _, err := os.Stat(workerMem); err != nil {
		t.Fatalf("worker MEMORY.md not created: %v", err)
	}
}

func TestSeedMemory_CopiesExistingContent(t *testing.T) {
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	// Pre-create canonical with content.
	canonical := filepath.Join(tmpHome, "nanika", "personas", "test-persona", "MEMORY.md")
	if err := os.MkdirAll(filepath.Dir(canonical), 0700); err != nil {
		t.Fatal(err)
	}
	content := "- Remember to use errgroup for concurrency\n- SQLite needs WAL mode\n"
	if err := os.WriteFile(canonical, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	workerDir := filepath.Join(tmpHome, "worker", "test-worker")
	if err := os.MkdirAll(workerDir, 0700); err != nil {
		t.Fatal(err)
	}

	if err := seedMemory("test-persona", workerDir); err != nil {
		t.Fatalf("seedMemory: %v", err)
	}

	// Verify worker got the same content.
	key := encodeProjectKey(workerDir)
	workerMem := filepath.Join(tmpHome, ".claude", "projects", key, "memory", "MEMORY.md")
	got, err := os.ReadFile(workerMem)
	if err != nil {
		t.Fatalf("reading worker MEMORY.md: %v", err)
	}
	if string(got) != content {
		t.Errorf("worker MEMORY.md = %q, want %q", got, content)
	}
}

func TestMergeMemoryBack_AppendsNewLines(t *testing.T) {
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	// Set up canonical with existing content.
	canonical := filepath.Join(tmpHome, "nanika", "personas", "test-persona", "MEMORY.md")
	if err := os.MkdirAll(filepath.Dir(canonical), 0700); err != nil {
		t.Fatal(err)
	}
	existing := "- existing entry one\n- existing entry two\n"
	if err := os.WriteFile(canonical, []byte(existing), 0600); err != nil {
		t.Fatal(err)
	}

	// Set up worker memory with mix of existing and new.
	workerDir := filepath.Join(tmpHome, "worker", "test-worker")
	key := encodeProjectKey(workerDir)
	workerMemDir := filepath.Join(tmpHome, ".claude", "projects", key, "memory")
	if err := os.MkdirAll(workerMemDir, 0700); err != nil {
		t.Fatal(err)
	}
	workerContent := "- existing entry one\n- new entry from worker\n- existing entry two\n- another new entry\n"
	if err := os.WriteFile(filepath.Join(workerMemDir, "MEMORY.md"), []byte(workerContent), 0600); err != nil {
		t.Fatal(err)
	}

	if err := mergeMemoryBack("test-persona", workerDir); err != nil {
		t.Fatalf("mergeMemoryBack: %v", err)
	}

	got, err := os.ReadFile(canonical)
	if err != nil {
		t.Fatal(err)
	}

	result := string(got)
	if !strings.Contains(result, "- new entry from worker") {
		t.Errorf("merged file missing '- new entry from worker': %q", result)
	}
	if !strings.Contains(result, "- another new entry") {
		t.Errorf("merged file missing '- another new entry': %q", result)
	}

	// Existing entries should appear exactly once each.
	if strings.Count(result, "- existing entry one") != 1 {
		t.Errorf("'- existing entry one' should appear once, got: %q", result)
	}
	if strings.Count(result, "- existing entry two") != 1 {
		t.Errorf("'- existing entry two' should appear once, got: %q", result)
	}
}

func TestMergeMemoryBack_NoWorkerFile(t *testing.T) {
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	// No worker memory file exists — should return nil silently.
	workerDir := filepath.Join(tmpHome, "worker", "nonexistent")
	err := mergeMemoryBack("test-persona", workerDir)
	if err != nil {
		t.Fatalf("expected nil error for missing worker file, got: %v", err)
	}
}

func TestMergeMemoryBack_NothingNew(t *testing.T) {
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	// Canonical and worker have identical content.
	canonical := filepath.Join(tmpHome, "nanika", "personas", "test-persona", "MEMORY.md")
	if err := os.MkdirAll(filepath.Dir(canonical), 0700); err != nil {
		t.Fatal(err)
	}
	content := "- same line one\n- same line two\n"
	if err := os.WriteFile(canonical, []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	workerDir := filepath.Join(tmpHome, "worker", "test-worker")
	key := encodeProjectKey(workerDir)
	workerMemDir := filepath.Join(tmpHome, ".claude", "projects", key, "memory")
	if err := os.MkdirAll(workerMemDir, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(workerMemDir, "MEMORY.md"), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	if err := mergeMemoryBack("test-persona", workerDir); err != nil {
		t.Fatalf("mergeMemoryBack: %v", err)
	}

	got, err := os.ReadFile(canonical)
	if err != nil {
		t.Fatal(err)
	}

	// File should be unchanged — no new lines appended.
	if string(got) != content {
		t.Errorf("canonical changed when nothing was new: got %q, want %q", got, content)
	}
}

func TestMergeMemoryBack_CanonicalMissing(t *testing.T) {
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)

	// Worker has memory but no canonical exists yet.
	workerDir := filepath.Join(tmpHome, "worker", "test-worker")
	key := encodeProjectKey(workerDir)
	workerMemDir := filepath.Join(tmpHome, ".claude", "projects", key, "memory")
	if err := os.MkdirAll(workerMemDir, 0700); err != nil {
		t.Fatal(err)
	}
	workerContent := "- learned something new\n"
	if err := os.WriteFile(filepath.Join(workerMemDir, "MEMORY.md"), []byte(workerContent), 0600); err != nil {
		t.Fatal(err)
	}

	// Ensure the canonical parent dir exists (mergeMemoryBack uses O_CREATE but not MkdirAll for the file).
	canonical := filepath.Join(tmpHome, "nanika", "personas", "test-persona", "MEMORY.md")
	if err := os.MkdirAll(filepath.Dir(canonical), 0700); err != nil {
		t.Fatal(err)
	}

	if err := mergeMemoryBack("test-persona", workerDir); err != nil {
		t.Fatalf("mergeMemoryBack: %v", err)
	}

	got, err := os.ReadFile(canonical)
	if err != nil {
		t.Fatalf("reading canonical after merge: %v", err)
	}
	if !strings.Contains(string(got), "- learned something new") {
		t.Errorf("canonical should contain worker's line, got: %q", got)
	}
}
