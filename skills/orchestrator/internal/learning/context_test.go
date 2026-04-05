package learning

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// ---------------------------------------------------------------------------
// InjectContext
// ---------------------------------------------------------------------------

func TestInjectContext_EmptyDB(t *testing.T) {
	db := newTestDB(t)
	content, err := InjectContext(context.Background(), db, nil, "goroutine leaks", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if content != "" {
		t.Errorf("expected empty string for empty DB, got %q", content)
	}
}

func TestInjectContext_WithMatchingLearnings(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "learn_001",
		Type:         TypeInsight,
		Content:      "Always use context cancellation to avoid goroutine leaks.",
		Domain:       "dev",
		QualityScore: 0.8,
		CreatedAt:    time.Now(),
	})

	content, err := InjectContext(context.Background(), db, nil, "goroutine leaks", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if content == "" {
		t.Fatal("expected non-empty context for matching learning")
	}
	if !strings.Contains(content, "## Relevant Learnings") {
		t.Errorf("expected heading in output, got: %q", content)
	}
	if !strings.Contains(content, "goroutine leaks") {
		t.Errorf("expected learning content in output, got: %q", content)
	}
	if !strings.HasPrefix(content, "## Relevant Learnings") {
		t.Errorf("output should start with heading, got: %q", content)
	}
	if !strings.HasSuffix(content, "\n") {
		t.Errorf("output should end with newline")
	}
}

func TestInjectContext_FormatsTypeInBold(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "learn_type",
		Type:         TypePattern,
		Content:      "Use parameterized tests to cover all edge cases thoroughly.",
		Domain:       "dev",
		QualityScore: 0.8,
		CreatedAt:    time.Now(),
	})

	content, err := InjectContext(context.Background(), db, nil, "parameterized tests edge cases", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(content, "**[pattern]**") {
		t.Errorf("expected bold type label in output, got: %q", content)
	}
}

func TestInjectContext_WrongDomain(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "learn_002",
		Type:         TypeInsight,
		Content:      "Always use context cancellation to avoid goroutine leaks.",
		Domain:       "work",
		QualityScore: 0.8,
		CreatedAt:    time.Now(),
	})

	content, err := InjectContext(context.Background(), db, nil, "goroutine leaks", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if content != "" {
		t.Errorf("expected empty string for wrong domain, got %q", content)
	}
}

func TestInjectContext_ZeroLimitDefaultsTen(t *testing.T) {
	db := newTestDB(t)
	for i := 0; i < 12; i++ {
		insertLearning(t, db, Learning{
			ID:           fmt.Sprintf("learn_%03d", i),
			Type:         TypeInsight,
			Content:      fmt.Sprintf("Use goroutine pool pattern number %d to limit concurrency.", i),
			Domain:       "dev",
			QualityScore: 0.8,
			CreatedAt:    time.Now(),
		})
	}

	content, err := InjectContext(context.Background(), db, nil, "goroutine pool pattern", "dev", 0)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	// Default limit is 10: count bullet points
	count := strings.Count(content, "\n- ")
	if count > 10 {
		t.Errorf("expected at most 10 results with zero limit (defaults to 10), got %d", count)
	}
}

func TestInjectContext_RecordsInjections(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "learn_inj",
		Type:         TypeInsight,
		Content:      "Always use context cancellation to avoid goroutine leaks.",
		Domain:       "dev",
		QualityScore: 0.8,
		CreatedAt:    time.Now(),
	})

	_, err := InjectContext(context.Background(), db, nil, "goroutine leaks", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var injCount int
	if err := db.db.QueryRow("SELECT injection_count FROM learnings WHERE id = 'learn_inj'").Scan(&injCount); err != nil {
		t.Fatalf("querying injection_count: %v", err)
	}
	if injCount != 1 {
		t.Errorf("injection_count = %d; want 1", injCount)
	}
}

// ---------------------------------------------------------------------------
// InjectContext — cold-start (empty query)
// ---------------------------------------------------------------------------

func TestInjectContext_EmptyQuery_EmptyDB(t *testing.T) {
	db := newTestDB(t)
	content, err := InjectContext(context.Background(), db, nil, "", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if content != "" {
		t.Errorf("expected empty string for empty DB, got %q", content)
	}
}

func TestInjectContext_EmptyQuery_ReturnsBestLearnings(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "best",
		Type:         TypePattern,
		Content:      "Best learning with high quality.",
		Domain:       "dev",
		QualityScore: 0.9,
		CreatedAt:    time.Now(),
	})
	insertLearning(t, db, Learning{
		ID:           "worse",
		Type:         TypeInsight,
		Content:      "Lower quality learning.",
		Domain:       "dev",
		QualityScore: 0.3,
		CreatedAt:    time.Now(),
	})

	content, err := InjectContext(context.Background(), db, nil, "", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if content == "" {
		t.Fatal("expected non-empty content from cold-start injection")
	}
	if !strings.Contains(content, "## Relevant Learnings") {
		t.Errorf("expected heading in cold-start output, got: %q", content)
	}
	if !strings.Contains(content, "Best learning") {
		t.Errorf("expected best learning in output, got: %q", content)
	}
}

func TestInjectContext_EmptyQuery_WrongDomain(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "work-only",
		Type:         TypeInsight,
		Content:      "Work domain only.",
		Domain:       "work",
		QualityScore: 0.9,
		CreatedAt:    time.Now(),
	})

	content, err := InjectContext(context.Background(), db, nil, "", "dev", 10)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if content != "" {
		t.Errorf("expected empty string for wrong domain, got %q", content)
	}
}

func TestInjectContext_EmptyQuery_RespectsLimit(t *testing.T) {
	db := newTestDB(t)
	for i := 0; i < 10; i++ {
		insertLearning(t, db, Learning{
			ID:           fmt.Sprintf("csl-%02d", i),
			Type:         TypeInsight,
			Content:      fmt.Sprintf("Cold start learning %d.", i),
			Domain:       "dev",
			QualityScore: 0.8,
			CreatedAt:    time.Now(),
		})
	}

	content, err := InjectContext(context.Background(), db, nil, "", "dev", 3)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	count := strings.Count(content, "\n- ")
	if count > 3 {
		t.Errorf("expected at most 3 results with limit=3, got %d", count)
	}
}

// ---------------------------------------------------------------------------
// FlushContext
// ---------------------------------------------------------------------------

func TestFlushContext_WritesFileWithContent(t *testing.T) {
	db := newTestDB(t)
	insertLearning(t, db, Learning{
		ID:           "learn_flush",
		Type:         TypeInsight,
		Content:      "Always use context cancellation to avoid goroutine leaks.",
		Domain:       "dev",
		QualityScore: 0.8,
		CreatedAt:    time.Now(),
	})

	outPath := filepath.Join(t.TempDir(), "context.md")
	if err := FlushContext(context.Background(), db, nil, "goroutine leaks", "dev", 10, outPath); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	data, err := os.ReadFile(outPath)
	if err != nil {
		t.Fatalf("reading output file: %v", err)
	}
	if !strings.Contains(string(data), "## Relevant Learnings") {
		t.Errorf("expected heading in file, got: %q", string(data))
	}
}

func TestFlushContext_CreatesParentDirectory(t *testing.T) {
	db := newTestDB(t)
	outPath := filepath.Join(t.TempDir(), "nested", "deep", "context.md")

	if err := FlushContext(context.Background(), db, nil, "goroutine", "dev", 10, outPath); err != nil {
		t.Fatalf("unexpected error creating nested dirs: %v", err)
	}

	if _, err := os.Stat(outPath); err != nil {
		t.Errorf("output file should exist after FlushContext: %v", err)
	}
}

func TestFlushContext_EmptyContentWritesEmptyFile(t *testing.T) {
	db := newTestDB(t)
	outPath := filepath.Join(t.TempDir(), "context.md")

	if err := FlushContext(context.Background(), db, nil, "completely unmatched query xyz", "dev", 10, outPath); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	data, err := os.ReadFile(outPath)
	if err != nil {
		t.Fatalf("reading output file: %v", err)
	}
	if len(data) != 0 {
		t.Errorf("expected empty file when no learnings match, got %q", string(data))
	}
}

func TestFlushContext_OverwritesExistingFile(t *testing.T) {
	db := newTestDB(t)
	outPath := filepath.Join(t.TempDir(), "context.md")

	// Pre-write stale content
	if err := os.WriteFile(outPath, []byte("stale content"), 0600); err != nil {
		t.Fatalf("pre-write: %v", err)
	}

	if err := FlushContext(context.Background(), db, nil, "query with no matches", "dev", 10, outPath); err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	data, err := os.ReadFile(outPath)
	if err != nil {
		t.Fatalf("reading output file: %v", err)
	}
	if string(data) == "stale content" {
		t.Error("FlushContext should overwrite existing file")
	}
}

// ---------------------------------------------------------------------------
// SnapshotSession
// ---------------------------------------------------------------------------

func TestSnapshotSession_MissingWorkspaceDir(t *testing.T) {
	db := newTestDB(t)
	n, err := SnapshotSession(context.Background(), db, nil, "/nonexistent/workspace/path", "dev")
	if err != nil {
		t.Fatalf("expected nil error for missing workspace, got: %v", err)
	}
	if n != 0 {
		t.Errorf("expected 0 learnings for missing workspace, got %d", n)
	}
}

func TestSnapshotSession_MissingWorkersSubdir(t *testing.T) {
	db := newTestDB(t)
	wsPath := t.TempDir() // workspace exists but has no "workers" subdir

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 0 {
		t.Errorf("expected 0 learnings for missing workers dir, got %d", n)
	}
}

func TestSnapshotSession_EmptyWorkersDir(t *testing.T) {
	db := newTestDB(t)
	wsPath := t.TempDir()
	if err := os.MkdirAll(filepath.Join(wsPath, "workers"), 0700); err != nil {
		t.Fatalf("mkdir workers: %v", err)
	}

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 0 {
		t.Errorf("expected 0 learnings for empty workers dir, got %d", n)
	}
}

func TestSnapshotSession_CapturesLearningsFromOutputMd(t *testing.T) {
	db := newTestDB(t)
	wsPath := t.TempDir()
	workerDir := filepath.Join(wsPath, "workers", "worker-1")
	if err := os.MkdirAll(workerDir, 0700); err != nil {
		t.Fatalf("mkdir worker: %v", err)
	}

	transcript := "LEARNING: Always use context cancellation to avoid goroutine leaks.\n" +
		"PATTERN: Prefer table-driven tests for comprehensive coverage of edge cases.\n"
	if err := os.WriteFile(filepath.Join(workerDir, "output.md"), []byte(transcript), 0600); err != nil {
		t.Fatalf("write output.md: %v", err)
	}

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n == 0 {
		t.Error("expected learnings captured from output.md, got 0")
	}
}

func TestSnapshotSession_SkipsWorkersWithNoOutputMd(t *testing.T) {
	db := newTestDB(t)
	wsPath := t.TempDir()

	// Worker without output.md
	if err := os.MkdirAll(filepath.Join(wsPath, "workers", "worker-no-output"), 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}

	// Worker with valid output.md
	workerWithOutput := filepath.Join(wsPath, "workers", "worker-with-output")
	if err := os.MkdirAll(workerWithOutput, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(workerWithOutput, "output.md"),
		[]byte("LEARNING: Always use context cancellation to avoid goroutine leaks.\n"), 0600); err != nil {
		t.Fatalf("write output.md: %v", err)
	}

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 1 {
		t.Errorf("expected 1 learning (one valid worker), got %d", n)
	}
}

func TestSnapshotSession_EmptyOutputMd(t *testing.T) {
	db := newTestDB(t)
	wsPath := t.TempDir()
	workerDir := filepath.Join(wsPath, "workers", "worker-1")
	if err := os.MkdirAll(workerDir, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(workerDir, "output.md"), []byte(""), 0600); err != nil {
		t.Fatalf("write output.md: %v", err)
	}

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 0 {
		t.Errorf("expected 0 learnings for empty output.md, got %d", n)
	}
}

func TestSnapshotSession_SkipsNonDirEntries(t *testing.T) {
	db := newTestDB(t)
	wsPath := t.TempDir()
	workersDir := filepath.Join(wsPath, "workers")
	if err := os.MkdirAll(workersDir, 0700); err != nil {
		t.Fatalf("mkdir workers: %v", err)
	}
	// Place a regular file in workers/ — should be ignored
	if err := os.WriteFile(filepath.Join(workersDir, "readme.txt"), []byte("not a worker"), 0600); err != nil {
		t.Fatalf("write file: %v", err)
	}

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 0 {
		t.Errorf("expected 0 learnings for file-only workers dir, got %d", n)
	}
}

func TestSnapshotSession_MultipleWorkers(t *testing.T) {
	db := newTestDB(t)
	wsPath := t.TempDir()

	workers := []struct {
		name    string
		content string
	}{
		{"alpha", "LEARNING: Always use context cancellation to avoid goroutine leaks.\n"},
		{"beta", "PATTERN: Prefer table-driven tests for comprehensive coverage of edge cases.\n"},
		{"gamma", "no valid markers here\n"},
	}

	for _, w := range workers {
		dir := filepath.Join(wsPath, "workers", w.name)
		if err := os.MkdirAll(dir, 0700); err != nil {
			t.Fatalf("mkdir %s: %v", w.name, err)
		}
		if err := os.WriteFile(filepath.Join(dir, "output.md"), []byte(w.content), 0600); err != nil {
			t.Fatalf("write output.md for %s: %v", w.name, err)
		}
	}

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 2 {
		t.Errorf("expected 2 learnings from 2 valid workers, got %d", n)
	}
}

func TestSnapshotSession_UsesWorkspaceIDFromPath(t *testing.T) {
	db := newTestDB(t)

	// wsPath base name becomes the workspace ID
	wsPath := filepath.Join(t.TempDir(), "ws-abc123")
	workerDir := filepath.Join(wsPath, "workers", "worker-1")
	if err := os.MkdirAll(workerDir, 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	if err := os.WriteFile(filepath.Join(workerDir, "output.md"),
		[]byte("LEARNING: Always use context cancellation to avoid goroutine leaks.\n"), 0600); err != nil {
		t.Fatalf("write output.md: %v", err)
	}

	n, err := SnapshotSession(context.Background(), db, nil, wsPath, "dev")
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if n != 1 {
		t.Fatalf("expected 1 learning, got %d", n)
	}

	// Verify workspace_id stored in DB matches path base
	var wsID string
	if err := db.db.QueryRow("SELECT workspace_id FROM learnings LIMIT 1").Scan(&wsID); err != nil {
		t.Fatalf("querying workspace_id: %v", err)
	}
	if wsID != "ws-abc123" {
		t.Errorf("workspace_id = %q; want %q", wsID, "ws-abc123")
	}
}
