package learning

import (
	"context"
	"fmt"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestIngestDocs_ChunksByHeading(t *testing.T) {
	dir := t.TempDir()
	content := "## First Section\n\nContent of the first section with sufficient length.\n\n## Second Section\n\nContent of the second section with sufficient length.\n\n## Third Section\n\nContent of the third section with sufficient length.\n"
	if err := os.WriteFile(filepath.Join(dir, "notes.md"), []byte(content), 0600); err != nil {
		t.Fatalf("write test file: %v", err)
	}

	db := newTestDB(t)
	stats, err := IngestDocs(dir, db, nil)
	if err != nil {
		t.Fatalf("IngestDocs: %v", err)
	}

	if stats.ChunksCreated != 3 {
		t.Errorf("ChunksCreated = %d, want 3", stats.ChunksCreated)
	}
	if stats.FilesScanned != 1 {
		t.Errorf("FilesScanned = %d, want 1", stats.FilesScanned)
	}

	rows, qErr := db.db.QueryContext(context.Background(), "SELECT context FROM learnings ORDER BY rowid")
	if qErr != nil {
		t.Fatalf("query contexts: %v", qErr)
	}
	defer rows.Close()

	var contexts []string
	for rows.Next() {
		var c string
		if err := rows.Scan(&c); err != nil {
			t.Fatalf("scan context: %v", err)
		}
		contexts = append(contexts, c)
	}

	if len(contexts) != 3 {
		t.Fatalf("got %d learnings in DB, want 3", len(contexts))
	}

	wantContexts := []string{
		"notes.md#first-section",
		"notes.md#second-section",
		"notes.md#third-section",
	}
	for i, got := range contexts {
		if got != wantContexts[i] {
			t.Errorf("context[%d] = %q, want %q", i, got, wantContexts[i])
		}
	}
}

func TestIngestDocs_SkipsNonMarkdown(t *testing.T) {
	dir := t.TempDir()

	files := map[string]string{
		"doc.md":    "## Section One\n\nThis is markdown content that should be ingested by the docs ingester.\n",
		"notes.txt": "plain text file — should be skipped entirely.",
		"main.go":   "package main\n\nfunc main() {}\n",
	}
	for name, body := range files {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0600); err != nil {
			t.Fatalf("write %s: %v", name, err)
		}
	}

	db := newTestDB(t)
	stats, err := IngestDocs(dir, db, nil)
	if err != nil {
		t.Fatalf("IngestDocs: %v", err)
	}

	if stats.FilesScanned != 1 {
		t.Errorf("FilesScanned = %d, want 1 (only .md files should be walked)", stats.FilesScanned)
	}
	if stats.ChunksCreated < 1 {
		t.Errorf("ChunksCreated = %d, want >= 1", stats.ChunksCreated)
	}

	// Confirm nothing from .txt or .go ended up in the DB.
	var total int
	db.db.QueryRow("SELECT COUNT(*) FROM learnings").Scan(&total) //nolint:errcheck
	if total != stats.ChunksCreated {
		t.Errorf("DB row count %d != ChunksCreated %d", total, stats.ChunksCreated)
	}
}

func TestIngestDocs_EmptyRoot(t *testing.T) {
	dir := t.TempDir()

	db := newTestDB(t)
	stats, err := IngestDocs(dir, db, nil)
	if err != nil {
		t.Errorf("IngestDocs on empty dir returned error: %v", err)
	}
	if stats.FilesScanned != 0 {
		t.Errorf("FilesScanned = %d, want 0", stats.FilesScanned)
	}
	if stats.ChunksCreated != 0 {
		t.Errorf("ChunksCreated = %d, want 0", stats.ChunksCreated)
	}
	if len(stats.Errors) != 0 {
		t.Errorf("Errors = %v, want none", stats.Errors)
	}
}

func TestIngestDocs_MalformedMarkdown_NonFatal(t *testing.T) {
	dir := t.TempDir()
	// File with no ## headings — should produce a single preamble chunk.
	content := "This file has no level-two headings at all.\n\nJust some plain paragraphs of content that should still be captured.\n"
	if err := os.WriteFile(filepath.Join(dir, "flat.md"), []byte(content), 0600); err != nil {
		t.Fatalf("write test file: %v", err)
	}

	db := newTestDB(t)
	stats, err := IngestDocs(dir, db, nil)
	if err != nil {
		t.Errorf("IngestDocs returned non-nil error for no-heading file: %v", err)
	}
	if len(stats.Errors) > 0 {
		t.Errorf("unexpected per-file errors: %v", stats.Errors)
	}
	if stats.ChunksCreated < 1 {
		t.Errorf("ChunksCreated = %d, want >= 1 (preamble chunk for file without headings)", stats.ChunksCreated)
	}

	var ctx string
	if err := db.db.QueryRow("SELECT context FROM learnings LIMIT 1").Scan(&ctx); err != nil {
		t.Fatalf("query context: %v", err)
	}
	if !strings.Contains(ctx, "intro") && !strings.Contains(ctx, "preamble") {
		t.Errorf("context %q should contain 'intro' or 'preamble' for a file without ## headings", ctx)
	}
}

func TestIngestDocs_IdempotentOnSecondRun(t *testing.T) {
	dir := t.TempDir()
	content := "## Alpha Section\n\nAlpha section body with enough words to be a meaningful chunk of documentation.\n\n## Beta Section\n\nBeta section body with enough words to be a meaningful chunk of documentation.\n"
	if err := os.WriteFile(filepath.Join(dir, "idempotent.md"), []byte(content), 0600); err != nil {
		t.Fatalf("write test file: %v", err)
	}

	db := newTestDB(t)

	stats1, err := IngestDocs(dir, db, nil)
	if err != nil {
		t.Fatalf("first IngestDocs: %v", err)
	}
	if stats1.ChunksCreated == 0 {
		t.Fatal("first run created no chunks; cannot verify idempotency")
	}

	stats2, err := IngestDocs(dir, db, nil)
	if err != nil {
		t.Fatalf("second IngestDocs: %v", err)
	}
	if stats2.ChunksSkippedDedup < stats1.ChunksCreated {
		t.Errorf("second run ChunksSkippedDedup = %d, want >= %d (all chunks from first run must be deduped on re-ingest)",
			stats2.ChunksSkippedDedup, stats1.ChunksCreated)
	}

	// DB row count must be stable after the second run.
	var count int
	db.db.QueryRow("SELECT COUNT(*) FROM learnings").Scan(&count) //nolint:errcheck
	if count != stats1.ChunksCreated {
		t.Errorf("DB row count after second run = %d, want %d (idempotent)", count, stats1.ChunksCreated)
	}
}

// TestIngestDocs_WithEmbedder verifies that when a working embedder is wired in,
// stored rows carry a non-nil embedding blob.
func TestIngestDocs_WithEmbedder(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		// Return a minimal but valid embedding vector.
		vals := make([]string, 8)
		for i := range vals {
			vals[i] = fmt.Sprintf("0.%d", i+1)
		}
		fmt.Fprintf(w, `{"embedding":{"values":[%s]}}`, strings.Join(vals, ","))
	}))
	defer srv.Close()

	embedder := &Embedder{
		apiKey:     "test-key",
		model:      "gemini-embedding-001",
		httpClient: srv.Client(),
		baseURL:    srv.URL,
	}

	dir := t.TempDir()
	content := "## Section One\n\nContent that will be chunked and embedded by the docs ingester.\n"
	if err := os.WriteFile(filepath.Join(dir, "doc.md"), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	db := newTestDB(t)
	stats, err := IngestDocs(dir, db, embedder)
	if err != nil {
		t.Fatalf("IngestDocs: %v", err)
	}
	if stats.ChunksCreated == 0 {
		t.Fatal("expected at least one chunk to be created")
	}

	// Every stored row must have a non-nil embedding blob.
	rows, err := db.db.QueryContext(context.Background(), "SELECT id, embedding FROM learnings")
	if err != nil {
		t.Fatalf("query: %v", err)
	}
	defer rows.Close()

	checked := 0
	for rows.Next() {
		var id string
		var embBlob []byte
		if err := rows.Scan(&id, &embBlob); err != nil {
			t.Fatalf("scan: %v", err)
		}
		if len(embBlob) == 0 {
			t.Errorf("row %s has nil/empty embedding blob; embedder was wired in", id)
		}
		checked++
	}
	if checked == 0 {
		t.Fatal("no rows found in DB")
	}
}
