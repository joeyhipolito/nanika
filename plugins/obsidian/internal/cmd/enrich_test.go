package cmd

import (
	"os"
	"path/filepath"
	"testing"
)

func TestApplyLinkSuggestions_InsertsBeforeNextHeading(t *testing.T) {
	dir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(dir, "notes"), 0755); err != nil {
		t.Fatal(err)
	}

	noteA := "notes/note-a.md"
	noteB := "notes/note-b.md"

	// note-a has a ## Related Notes section followed by ## See Also
	contentA := "# Note A\n\nBody text.\n\n## Related Notes\n- [[OldNote]]\n\n## See Also\n\nOther content here.\n"
	contentB := "# Note B\n\nContent.\n"

	if err := os.WriteFile(filepath.Join(dir, noteA), []byte(contentA), 0644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, noteB), []byte(contentB), 0644); err != nil {
		t.Fatal(err)
	}

	suggestions := []LinkSuggestion{
		{From: noteA, To: noteB, Similarity: 0.9},
	}

	applied := applyLinkSuggestions(dir, suggestions)
	if applied != 2 {
		t.Fatalf("expected 2 applied, got %d", applied)
	}

	gotA, err := os.ReadFile(filepath.Join(dir, noteA))
	if err != nil {
		t.Fatal(err)
	}

	// New link must appear inside ## Related Notes, before ## See Also
	wantA := "# Note A\n\nBody text.\n\n## Related Notes\n- [[OldNote]]\n- [[note-b]]\n\n## See Also\n\nOther content here.\n"
	if string(gotA) != wantA {
		t.Errorf("note A content mismatch:\ngot:  %q\nwant: %q", string(gotA), wantA)
	}

	// note-b gets a new ## Related Notes section at the end
	gotB, err := os.ReadFile(filepath.Join(dir, noteB))
	if err != nil {
		t.Fatal(err)
	}
	wantB := "# Note B\n\nContent.\n\n## Related Notes\n- [[note-a]]\n"
	if string(gotB) != wantB {
		t.Errorf("note B content mismatch:\ngot:  %q\nwant: %q", string(gotB), wantB)
	}
}
