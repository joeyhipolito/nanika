package worker

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

// ---------------------------------------------------------------------------
// readMarkdownFiles
// ---------------------------------------------------------------------------

func TestReadMarkdownFiles_ReturnsContentOfMdFiles(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte("LEARNING: Always wrap errors.\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "notes.md"), []byte("FINDING: Config reads are cached.\n"), 0600); err != nil {
		t.Fatal(err)
	}

	got := readMarkdownFiles(dir)

	if !strings.Contains(got, "Always wrap errors.") {
		t.Errorf("readMarkdownFiles: missing output.md content; got %q", got)
	}
	if !strings.Contains(got, "Config reads are cached.") {
		t.Errorf("readMarkdownFiles: missing notes.md content; got %q", got)
	}
}

func TestReadMarkdownFiles_SkipsNonMarkdown(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "main.go"), []byte("LEARNING: This should not appear.\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "report.md"), []byte("PATTERN: Use table-driven tests.\n"), 0600); err != nil {
		t.Fatal(err)
	}

	got := readMarkdownFiles(dir)

	if strings.Contains(got, "This should not appear.") {
		t.Error("readMarkdownFiles: .go file content must not be included")
	}
	if !strings.Contains(got, "Use table-driven tests.") {
		t.Errorf("readMarkdownFiles: .md file content missing; got %q", got)
	}
}

func TestReadMarkdownFiles_EmptyDirReturnsEmpty(t *testing.T) {
	dir := t.TempDir()
	got := readMarkdownFiles(dir)
	if got != "" {
		t.Errorf("readMarkdownFiles: empty dir should return empty string; got %q", got)
	}
}

func TestReadMarkdownFiles_UnreadableFileSkipped(t *testing.T) {
	dir := t.TempDir()
	readable := filepath.Join(dir, "readable.md")
	unreadable := filepath.Join(dir, "secret.md")

	if err := os.WriteFile(readable, []byte("DECISION: Use WAL mode.\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(unreadable, []byte("secret content\n"), 0600); err != nil {
		t.Fatal(err)
	}
	if err := os.Chmod(unreadable, 0000); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.Chmod(unreadable, 0600) })

	got := readMarkdownFiles(dir)

	// Must include the readable file content without panicking on the unreadable one.
	if !strings.Contains(got, "Use WAL mode.") {
		t.Errorf("readMarkdownFiles: readable content missing; got %q", got)
	}
}

func TestReadMarkdownFiles_SubdirFilesIncluded(t *testing.T) {
	dir := t.TempDir()
	subdir := filepath.Join(dir, "sub")
	if err := os.MkdirAll(subdir, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(subdir, "nested.md"), []byte("GOTCHA: Close DB after use.\n"), 0600); err != nil {
		t.Fatal(err)
	}

	got := readMarkdownFiles(dir)

	if !strings.Contains(got, "Close DB after use.") {
		t.Errorf("readMarkdownFiles: nested file content missing; got %q", got)
	}
}

// ---------------------------------------------------------------------------
// capturePhaseOutputTo: marker extraction → DB insert
// ---------------------------------------------------------------------------

func TestCapturePhaseOutputTo_ExtractsAndInsertsLearnings(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(t.TempDir(), "test.db")

	content := strings.Join([]string{
		"LEARNING: Always check returned errors in Go.",
		"FINDING: SQLite WAL mode improves concurrent read throughput.",
		"PATTERN: Table-driven tests reduce test boilerplate significantly.",
		"DECISION: Use SHA256 for content hashing consistency across packages.",
		"GOTCHA: Regex compiled inside a loop causes O(n) overhead per call.",
	}, "\n") + "\n"

	if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte(content), 0600); err != nil {
		t.Fatal(err)
	}

	db, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open test db: %v", err)
	}
	defer db.Close()

	capturePhaseOutputTo(context.Background(), db, dir, "test-worker", "dev", "ws-test")

	// Verify at least one learning was stored.
	stored, err := db.FindTopByQuality("dev", 20)
	if err != nil {
		t.Fatalf("list learnings: %v", err)
	}
	if len(stored) == 0 {
		t.Fatal("expected learnings to be stored; got none")
	}

	// All stored learnings must carry the worker metadata.
	for _, l := range stored {
		if l.WorkerName != "test-worker" {
			t.Errorf("learning %s: WorkerName = %q; want %q", l.ID, l.WorkerName, "test-worker")
		}
		if l.Domain != "dev" {
			t.Errorf("learning %s: Domain = %q; want %q", l.ID, l.Domain, "dev")
		}
		if l.WorkspaceID != "ws-test" {
			t.Errorf("learning %s: WorkspaceID = %q; want %q", l.ID, l.WorkspaceID, "ws-test")
		}
	}
}

func TestCapturePhaseOutputTo_EmptyDirIsNoOp(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(t.TempDir(), "test.db")

	db, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open test db: %v", err)
	}
	defer db.Close()

	// Must not panic or error on an empty directory.
	capturePhaseOutputTo(context.Background(), db, dir, "worker", "dev", "ws-1")

	total, _, statsErr := db.Stats()
	if statsErr != nil {
		t.Fatalf("stats: %v", statsErr)
	}
	if total != 0 {
		t.Errorf("expected no learnings for empty dir; got %d", total)
	}
}

func TestCapturePhaseOutputTo_NoMarkersIsNoOp(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(t.TempDir(), "test.db")

	// Markdown file with no recognised markers.
	if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte("This is a summary with no markers.\n"), 0600); err != nil {
		t.Fatal(err)
	}

	db, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open test db: %v", err)
	}
	defer db.Close()

	capturePhaseOutputTo(context.Background(), db, dir, "worker", "dev", "ws-2")

	total, _, statsErr := db.Stats()
	if statsErr != nil {
		t.Fatalf("stats: %v", statsErr)
	}
	if total != 0 {
		t.Errorf("expected no learnings when no markers present; got %d", total)
	}
}

// ---------------------------------------------------------------------------
// Goroutine lifecycle: capturePhaseOutput terminates within a reasonable window
// ---------------------------------------------------------------------------

func TestCapturePhaseOutput_GoroutineTerminates(t *testing.T) {
	dir := t.TempDir()

	// Write a file with valid markers so the goroutine does real work.
	if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte(
		"LEARNING: Always check returned errors in Go.\n",
	), 0600); err != nil {
		t.Fatal(err)
	}

	var wg sync.WaitGroup
	wg.Add(1)
	go func() {
		defer wg.Done()
		capturePhaseOutput(dir, "lifecycle-worker", "dev", "ws-lifecycle")
	}()

	done := make(chan struct{})
	go func() {
		wg.Wait()
		close(done)
	}()

	select {
	case <-done:
		// Goroutine completed cleanly.
	case <-time.After(10 * time.Second):
		t.Fatal("capturePhaseOutput goroutine did not terminate within 10s")
	}
}
