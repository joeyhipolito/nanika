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
