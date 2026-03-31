package index

import (
	"os"
	"path/filepath"
	"testing"
)

func TestOpenAndClose(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")

	store, err := Open(dbPath)
	if err != nil {
		t.Fatalf("Open failed: %v", err)
	}
	defer store.Close()

	// Verify database file was created
	if _, err := os.Stat(dbPath); err != nil {
		t.Fatalf("database file not created: %v", err)
	}
}

func TestUpsertAndGetModTime(t *testing.T) {
	store := openTestStore(t)
	defer store.Close()

	row := &NoteRow{
		Path:    "test/note.md",
		Title:   "Test Note",
		Tags:    "tag1, tag2",
		Body:    "This is the body of the test note.",
		ModTime: 1234567890,
	}

	if err := store.UpsertNote(row); err != nil {
		t.Fatalf("UpsertNote failed: %v", err)
	}

	// Check mod_time
	mtime, err := store.GetModTime("test/note.md")
	if err != nil {
		t.Fatalf("GetModTime failed: %v", err)
	}
	if mtime != 1234567890 {
		t.Errorf("got mod_time %d, want 1234567890", mtime)
	}

	// Update same note
	row.ModTime = 9999999999
	row.Body = "Updated body."
	if err := store.UpsertNote(row); err != nil {
		t.Fatalf("UpsertNote update failed: %v", err)
	}

	mtime, err = store.GetModTime("test/note.md")
	if err != nil {
		t.Fatalf("GetModTime after update failed: %v", err)
	}
	if mtime != 9999999999 {
		t.Errorf("got mod_time %d after update, want 9999999999", mtime)
	}
}

func TestGetModTimeNotFound(t *testing.T) {
	store := openTestStore(t)
	defer store.Close()

	mtime, err := store.GetModTime("nonexistent.md")
	if err != nil {
		t.Fatalf("GetModTime failed: %v", err)
	}
	if mtime != 0 {
		t.Errorf("got mod_time %d for nonexistent note, want 0", mtime)
	}
}

func TestDeleteNote(t *testing.T) {
	store := openTestStore(t)
	defer store.Close()

	row := &NoteRow{Path: "delete-me.md", Title: "Delete Me", Body: "gone", ModTime: 100}
	store.UpsertNote(row)

	if err := store.DeleteNote("delete-me.md"); err != nil {
		t.Fatalf("DeleteNote failed: %v", err)
	}

	mtime, _ := store.GetModTime("delete-me.md")
	if mtime != 0 {
		t.Errorf("note still exists after delete")
	}
}

func TestNoteCount(t *testing.T) {
	store := openTestStore(t)
	defer store.Close()

	store.UpsertNote(&NoteRow{Path: "a.md", Body: "a", ModTime: 1})
	store.UpsertNote(&NoteRow{Path: "b.md", Body: "b", ModTime: 2})
	store.UpsertNote(&NoteRow{Path: "c.md", Body: "c", ModTime: 3})

	count, err := store.NoteCount()
	if err != nil {
		t.Fatalf("NoteCount failed: %v", err)
	}
	if count != 3 {
		t.Errorf("got count %d, want 3", count)
	}
}

func TestGetAllPaths(t *testing.T) {
	store := openTestStore(t)
	defer store.Close()

	store.UpsertNote(&NoteRow{Path: "a.md", Body: "a", ModTime: 1})
	store.UpsertNote(&NoteRow{Path: "dir/b.md", Body: "b", ModTime: 2})

	paths, err := store.GetAllPaths()
	if err != nil {
		t.Fatalf("GetAllPaths failed: %v", err)
	}
	if len(paths) != 2 {
		t.Errorf("got %d paths, want 2", len(paths))
	}
	if !paths["a.md"] || !paths["dir/b.md"] {
		t.Errorf("missing expected paths: %v", paths)
	}
}

func TestSearchKeyword(t *testing.T) {
	store := openTestStore(t)
	defer store.Close()

	store.UpsertNote(&NoteRow{
		Path:  "golang.md",
		Title: "Go Programming",
		Body:  "Go is a statically typed programming language designed at Google.",
		Tags:  "golang, programming",
		ModTime: 1,
	})
	store.UpsertNote(&NoteRow{
		Path:  "python.md",
		Title: "Python Programming",
		Body:  "Python is a dynamically typed language popular for data science.",
		Tags:  "python, programming",
		ModTime: 2,
	})
	store.UpsertNote(&NoteRow{
		Path:    "cooking.md",
		Title:   "Pasta Recipe",
		Body:    "Boil water, add pasta, cook for 10 minutes.",
		ModTime: 3,
	})

	results, err := store.SearchKeyword("programming", 10)
	if err != nil {
		t.Fatalf("SearchKeyword failed: %v", err)
	}
	if len(results) != 2 {
		t.Errorf("got %d results, want 2", len(results))
	}

	// Search for something only in one note
	results, err = store.SearchKeyword("Google", 10)
	if err != nil {
		t.Fatalf("SearchKeyword failed: %v", err)
	}
	if len(results) != 1 {
		t.Errorf("got %d results for 'Google', want 1", len(results))
	}
	if len(results) > 0 && results[0].Path != "golang.md" {
		t.Errorf("got path %s, want golang.md", results[0].Path)
	}
}

func TestSearchSemantic(t *testing.T) {
	store := openTestStore(t)
	defer store.Close()

	// Create fake embeddings — just enough to test cosine similarity
	embA := make([]float32, 4)
	embA[0] = 1.0 // pointing in x direction

	embB := make([]float32, 4)
	embB[1] = 1.0 // pointing in y direction

	store.UpsertNote(&NoteRow{Path: "a.md", Title: "Note A", Body: "a", ModTime: 1, Embedding: embA})
	store.UpsertNote(&NoteRow{Path: "b.md", Title: "Note B", Body: "b", ModTime: 2, Embedding: embB})

	// Query close to embA
	query := make([]float32, 4)
	query[0] = 0.9
	query[1] = 0.1

	results, err := store.SearchSemantic(query, 10)
	if err != nil {
		t.Fatalf("SearchSemantic failed: %v", err)
	}
	if len(results) != 2 {
		t.Fatalf("got %d results, want 2", len(results))
	}
	// Note A should rank higher (closer to query)
	if results[0].Path != "a.md" {
		t.Errorf("got path %s as top result, want a.md", results[0].Path)
	}
}

func TestEmbeddingEncodeDecode(t *testing.T) {
	original := []float32{1.0, -0.5, 0.25, 3.14159}

	encoded := encodeEmbedding(original)
	decoded := decodeEmbedding(encoded)

	if len(decoded) != len(original) {
		t.Fatalf("decoded length %d != original length %d", len(decoded), len(original))
	}

	for i := range original {
		if decoded[i] != original[i] {
			t.Errorf("decoded[%d] = %f, want %f", i, decoded[i], original[i])
		}
	}
}

func TestCosineSimilarity(t *testing.T) {
	a := []float32{1, 0, 0}
	b := []float32{1, 0, 0}
	sim := CosineSimilarity(a, b)
	if sim < 0.99 || sim > 1.01 {
		t.Errorf("identical vectors got similarity %f, want ~1.0", sim)
	}

	c := []float32{0, 1, 0}
	sim = CosineSimilarity(a, c)
	if sim < -0.01 || sim > 0.01 {
		t.Errorf("orthogonal vectors got similarity %f, want ~0.0", sim)
	}
}

func TestBuildSearchText(t *testing.T) {
	text := BuildSearchText("My Title", "tag1, tag2", "Heading One\nHeading Two", "Body content here")
	if text == "" {
		t.Error("BuildSearchText returned empty string")
	}
	// Should contain all parts
	for _, want := range []string{"My Title", "tag1, tag2", "Heading One", "Body content here"} {
		if !contains(text, want) {
			t.Errorf("BuildSearchText missing %q", want)
		}
	}
}

func TestBuildSearchTextTruncatesBody(t *testing.T) {
	longBody := string(make([]byte, 10000))
	text := BuildSearchText("", "", "", longBody)
	if len(text) > 8100 {
		t.Errorf("BuildSearchText should truncate long body, got length %d", len(text))
	}
}

func contains(s, substr string) bool {
	return len(s) >= len(substr) && (s == substr || len(s) > 0 && containsHelper(s, substr))
}

func containsHelper(s, substr string) bool {
	for i := 0; i <= len(s)-len(substr); i++ {
		if s[i:i+len(substr)] == substr {
			return true
		}
	}
	return false
}

func openTestStore(t *testing.T) *Store {
	t.Helper()
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")
	store, err := Open(dbPath)
	if err != nil {
		t.Fatalf("Open failed: %v", err)
	}
	return store
}
