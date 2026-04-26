package vault

import (
	"os"
	"path/filepath"
	"testing"
)

func TestAppendToNote_EOF(t *testing.T) {
	dir := t.TempDir()

	initial := "---\ntype: daily\n---\n\n## Notes\n\nFirst entry\n"
	notePath := "test.md"
	fullPath := filepath.Join(dir, notePath)
	if err := os.WriteFile(fullPath, []byte(initial), 0644); err != nil {
		t.Fatal(err)
	}

	if err := AppendToNote(dir, notePath, "Second entry", ""); err != nil {
		t.Fatalf("AppendToNote: %v", err)
	}

	got, err := os.ReadFile(fullPath)
	if err != nil {
		t.Fatal(err)
	}
	want := initial + "Second entry\n"
	if string(got) != want {
		t.Errorf("got:\n%q\nwant:\n%q", string(got), want)
	}
}

func TestAppendToNote_Section_BeforeNextHeading(t *testing.T) {
	dir := t.TempDir()

	initial := "# Daily\n\n## Capture\n\nExisting entry\n\n## Tasks\n\n- task 1\n"
	notePath := "test.md"
	fullPath := filepath.Join(dir, notePath)
	if err := os.WriteFile(fullPath, []byte(initial), 0644); err != nil {
		t.Fatal(err)
	}

	if err := AppendToNote(dir, notePath, "New capture", "## Capture"); err != nil {
		t.Fatalf("AppendToNote: %v", err)
	}

	got, err := os.ReadFile(fullPath)
	if err != nil {
		t.Fatal(err)
	}

	// "New capture\n" should appear before "## Tasks"
	content := string(got)
	captureIdx := findIndex(content, "New capture")
	tasksIdx := findIndex(content, "## Tasks")
	existingIdx := findIndex(content, "Existing entry")

	if captureIdx == -1 {
		t.Error("new capture text not found")
	}
	if existingIdx > captureIdx {
		t.Error("new capture should appear after existing entry")
	}
	if captureIdx > tasksIdx {
		t.Errorf("new capture (at %d) should appear before ## Tasks (at %d)", captureIdx, tasksIdx)
	}
}

func TestAppendToNote_Section_AtEOF(t *testing.T) {
	dir := t.TempDir()

	initial := "# Daily\n\n## Capture\n\nExisting entry\n"
	notePath := "test.md"
	fullPath := filepath.Join(dir, notePath)
	if err := os.WriteFile(fullPath, []byte(initial), 0644); err != nil {
		t.Fatal(err)
	}

	if err := AppendToNote(dir, notePath, "New entry", "## Capture"); err != nil {
		t.Fatalf("AppendToNote: %v", err)
	}

	got, err := os.ReadFile(fullPath)
	if err != nil {
		t.Fatal(err)
	}

	content := string(got)
	if findIndex(content, "New entry") == -1 {
		t.Errorf("new entry not found in:\n%s", content)
	}
}

func TestAppendToNote_Section_NotFound(t *testing.T) {
	dir := t.TempDir()

	initial := "# Daily\n\n## Notes\n\nsome text\n"
	notePath := "test.md"
	if err := os.WriteFile(filepath.Join(dir, notePath), []byte(initial), 0644); err != nil {
		t.Fatal(err)
	}

	err := AppendToNote(dir, notePath, "text", "## Missing Section")
	if err == nil {
		t.Error("expected error for missing section, got nil")
	}
}

func findIndex(s, substr string) int {
	for i := range s {
		if i+len(substr) <= len(s) && s[i:i+len(substr)] == substr {
			return i
		}
	}
	return -1
}

// T0.1 — §10.4 Phase 0
// Asserts: vault skeleton directories and files are created on first run with
// the correct structure as defined in the T0.1 scenario.
func TestVaultSkeletonCreated(t *testing.T) {
	t.Run("nanika_skeleton", func(t *testing.T) {
		dir := t.TempDir()
		if err := InitSkeleton(dir, KindNanika); err != nil {
			t.Fatalf("InitSkeleton: %v", err)
		}
		entries, err := os.ReadDir(dir)
		if err != nil {
			t.Fatal(err)
		}
		got := make(map[string]bool)
		for _, e := range entries {
			got[e.Name()] = true
		}
		want := map[string]bool{
			"inbox": true, "ideas": true, "missions": true, "daily": true, "mocs": true,
			"trackers": true, "sessions": true, "findings": true,
			"decisions": true, "questions": true, "index.md": true,
		}
		for name := range want {
			if !got[name] {
				t.Errorf("missing entry: %s", name)
			}
		}
		for name := range got {
			if !want[name] {
				t.Errorf("unexpected entry: %s", name)
			}
		}
	})

	t.Run("second_brain_skeleton", func(t *testing.T) {
		dir := t.TempDir()
		if err := InitSkeleton(dir, KindSecondBrain); err != nil {
			t.Fatalf("InitSkeleton: %v", err)
		}
		entries, err := os.ReadDir(dir)
		if err != nil {
			t.Fatal(err)
		}
		got := make(map[string]bool)
		for _, e := range entries {
			got[e.Name()] = true
		}
		want := map[string]bool{
			"inbox": true, "ideas": true, "daily": true, "mocs": true,
			"findings": true, "decisions": true, "questions": true,
			"topics": true, "index.md": true,
		}
		for name := range want {
			if !got[name] {
				t.Errorf("missing entry: %s", name)
			}
		}
		for name := range got {
			if !want[name] {
				t.Errorf("unexpected entry: %s", name)
			}
		}
	})

	t.Run("idempotent", func(t *testing.T) {
		dir := t.TempDir()
		if err := InitSkeleton(dir, KindNanika); err != nil {
			t.Fatalf("first InitSkeleton: %v", err)
		}
		indexPath := filepath.Join(dir, "index.md")
		before, err := os.ReadFile(indexPath)
		if err != nil {
			t.Fatal(err)
		}
		if err := InitSkeleton(dir, KindNanika); err != nil {
			t.Fatalf("second InitSkeleton: %v", err)
		}
		after, err := os.ReadFile(indexPath)
		if err != nil {
			t.Fatal(err)
		}
		if string(before) != string(after) {
			t.Error("index.md content changed on second InitSkeleton call")
		}
	})
}
