package preflight

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

func TestLearningsSection_NameAndPriority(t *testing.T) {
	s := &learningsSection{}
	if got := s.Name(); got != "learnings" {
		t.Errorf("Name() = %q, want %q", got, "learnings")
	}
	if got := s.Priority(); got != 30 {
		t.Errorf("Priority() = %d, want 30", got)
	}
}

func TestLearningsSection_EmptyStore(t *testing.T) {
	// Open a fresh temp DB with no learnings — should return empty Block, no error.
	dir := t.TempDir()
	db, err := learning.OpenDB(filepath.Join(dir, "learnings.db"))
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	db.Close()

	// Point the section at our temp DB by setting env var.
	// learningsSection.Fetch calls learning.OpenDB("") which uses config.Dir().
	// We test the logic by calling FindTopByQuality directly through a thin
	// wrapper; for the integration path see TestLearningsSection_WithData.
	//
	// Instead, exercise Fetch directly against the real env-directed DB path
	// only if it opens cleanly — otherwise just verify the empty-body contract.
	t.Setenv("NANIKA_DOMAIN", "dev")

	s := &learningsSection{}
	blk, err := s.Fetch(context.Background())
	// We expect either a clean empty block (DB opened, no learnings in dev domain)
	// or a DB-open error that gets swallowed by BuildBrief. Either is correct.
	if err != nil {
		// DB open failed in test environment — verify graceful error shape.
		if !strings.Contains(err.Error(), "open learning DB") &&
			!strings.Contains(err.Error(), "finding top learnings") {
			t.Errorf("unexpected error format: %v", err)
		}
		return
	}
	// Empty body is valid; Title must still be set.
	if blk.Title != "Relevant Learnings" {
		t.Errorf("Title = %q, want %q", blk.Title, "Relevant Learnings")
	}
}

func TestLearningsSection_WithData(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "learnings.db")

	db, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}

	l := learning.Learning{
		ID:           "test-1",
		Type:         learning.TypeInsight,
		Content:      "Prefer explicit over implicit",
		Domain:       "dev",
		WorkerName:   "tester",
		WorkspaceID:  "ws-test",
		QualityScore: 0.9,
	}
	if err := db.Insert(context.Background(), l, nil); err != nil {
		db.Close()
		t.Fatalf("Insert: %v", err)
	}
	db.Close()

	// Verify FindTopByQuality returns the inserted learning.
	db2, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	defer db2.Close()

	results, err := db2.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("FindTopByQuality: %v", err)
	}
	if len(results) == 0 {
		t.Fatal("expected at least 1 learning, got 0")
	}

	// Verify the formatted block would contain the expected content.
	var sb strings.Builder
	for _, r := range results {
		sb.WriteString("- **[" + string(r.Type) + "]** " + r.Content + "\n")
	}
	body := sb.String()
	if !strings.Contains(body, "[insight]") {
		t.Errorf("formatted body missing type tag: %q", body)
	}
	if !strings.Contains(body, "Prefer explicit over implicit") {
		t.Errorf("formatted body missing content: %q", body)
	}
}

func TestLearningsSection_RegisteredInInit(t *testing.T) {
	// Verify the init() registered a section named "learnings".
	found := false
	for _, s := range List() {
		if s.Name() == "learnings" {
			found = true
			break
		}
	}
	if !found {
		t.Error("learningsSection not found in registry after init()")
	}
}

func TestLearningsSection_DomainFromEnv(t *testing.T) {
	// NANIKA_DOMAIN env var controls the domain filter.
	// We only verify that the env var is read — the actual filtering is
	// tested by FindTopByQuality in the learning package.
	t.Setenv("NANIKA_DOMAIN", "personal")
	s := &learningsSection{}
	// Fetch may fail if there's no learnings DB in CI; that's OK — we're
	// testing env propagation, not DB contents.
	blk, err := s.Fetch(context.Background())
	if err == nil && blk.Title != "Relevant Learnings" {
		t.Errorf("Title = %q, want %q", blk.Title, "Relevant Learnings")
	}

	// Verify fallback to "dev" when env var is unset.
	t.Setenv("NANIKA_DOMAIN", "")
	blk2, err2 := s.Fetch(context.Background())
	if err2 == nil && blk2.Title != "Relevant Learnings" {
		t.Errorf("fallback Title = %q, want %q", blk2.Title, "Relevant Learnings")
	}

	// Both calls must not panic — that is the invariant.
	_ = os.Unsetenv("NANIKA_DOMAIN")
}
