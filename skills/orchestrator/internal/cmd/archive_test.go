package cmd

import (
	"bytes"
	"context"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

// TestRunArchive_ApplyRecomputesFirst verifies that runArchive with --apply
// runs UpdateQualityScores before ArchiveDeadWeight.
func TestRunArchive_ApplyRecomputesFirst(t *testing.T) {
	// Write a temp DB file — in-memory cannot be shared across Open calls.
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")

	// Seed the DB with 3 rows of different types, quality_score=0.0.
	seedDB, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open seed DB: %v", err)
	}
	ctx := context.Background()
	rows := []learning.Learning{
		{
			ID:           "learn_001",
			Type:         learning.TypeInsight,
			Content:      "insight learning for recompute test",
			Domain:       "dev",
			CreatedAt:    time.Now(),
			QualityScore: 0.0,
		},
		{
			ID:           "learn_002",
			Type:         learning.TypePattern,
			Content:      "pattern learning for recompute test",
			Domain:       "dev",
			CreatedAt:    time.Now(),
			QualityScore: 0.0,
		},
		{
			ID:           "learn_003",
			Type:         learning.TypeSource,
			Content:      "source learning for recompute test",
			Domain:       "dev",
			CreatedAt:    time.Now(),
			QualityScore: 0.0,
		},
	}
	for _, row := range rows {
		if err := seedDB.Insert(ctx, row, nil); err != nil {
			t.Fatalf("insert %s: %v", row.ID, err)
		}
	}
	seedDB.Close()

	// Build a synthetic cobra.Command with the same flags as archiveCmd.
	cmd := &cobra.Command{Use: "archive", RunE: runArchive}
	cmd.Flags().Bool("apply", false, "")
	cmd.Flags().String("domain", "", "")
	cmd.Flags().String("db", "", "")
	_ = cmd.Flags().MarkHidden("db")
	if err := cmd.Flags().Set("apply", "true"); err != nil {
		t.Fatalf("set apply flag: %v", err)
	}
	if err := cmd.Flags().Set("db", dbPath); err != nil {
		t.Fatalf("set db flag: %v", err)
	}

	// Capture stdout — runArchive uses fmt.Printf directly.
	origStdout := os.Stdout
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("os.Pipe: %v", err)
	}
	os.Stdout = w

	runErr := cmd.Execute()

	w.Close()
	os.Stdout = origStdout

	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("read captured output: %v", err)
	}
	out := buf.String()

	// (a) runArchive must succeed.
	if runErr != nil {
		t.Fatalf("runArchive returned error: %v\noutput: %s", runErr, out)
	}

	// (b) stdout must contain "recomputed" and a positive count.
	if !strings.Contains(out, "recomputed") {
		t.Errorf("stdout does not contain 'recomputed'\ngot: %s", out)
	}
	if !strings.Contains(out, "3") {
		t.Errorf("stdout does not contain the count '3'\ngot: %s", out)
	}

	// (a) quality_score must be > 0.0 for all three rows after the run.
	verifyDB, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open verify DB: %v", err)
	}
	defer verifyDB.Close()

	got, err := verifyDB.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("FindTopByQuality: %v", err)
	}
	if len(got) < 3 {
		t.Fatalf("expected 3 non-archived learnings, got %d", len(got))
	}
	for _, l := range got {
		if l.QualityScore <= 0.0 {
			t.Errorf("learning %s has quality_score=%.4f, want > 0.0", l.ID, l.QualityScore)
		}
	}
}

// TestRunArchive_DryRunSkipsRecompute verifies that without --apply the command
// prints the dry-run notice and does NOT update quality scores.
func TestRunArchive_DryRunSkipsRecompute(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")

	seedDB, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open seed DB: %v", err)
	}
	ctx := context.Background()
	if err := seedDB.Insert(ctx, learning.Learning{
		ID:           "learn_dr1",
		Type:         learning.TypeInsight,
		Content:      "dry-run recompute test",
		Domain:       "dev",
		CreatedAt:    time.Now(),
		QualityScore: 0.0,
	}, nil); err != nil {
		t.Fatalf("insert: %v", err)
	}
	seedDB.Close()

	cmd := &cobra.Command{Use: "archive", RunE: runArchive}
	cmd.Flags().Bool("apply", false, "")
	cmd.Flags().String("domain", "", "")
	cmd.Flags().String("db", "", "")
	_ = cmd.Flags().MarkHidden("db")
	if err := cmd.Flags().Set("db", dbPath); err != nil {
		t.Fatalf("set db flag: %v", err)
	}

	origStdout := os.Stdout
	r, w, _ := os.Pipe()
	os.Stdout = w
	runErr := cmd.Execute()
	w.Close()
	os.Stdout = origStdout

	var buf bytes.Buffer
	io.Copy(&buf, r) //nolint:errcheck
	out := buf.String()

	if runErr != nil {
		t.Fatalf("runArchive dry-run error: %v\noutput: %s", runErr, out)
	}
	if !strings.Contains(out, "dry-run: would recompute quality scores") {
		t.Errorf("expected dry-run notice\ngot: %s", out)
	}

	// Score must remain 0.0 — recompute must NOT have run.
	verifyDB, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open verify DB: %v", err)
	}
	defer verifyDB.Close()

	got, err := verifyDB.FindTopByQuality("dev", 10)
	if err != nil {
		t.Fatalf("FindTopByQuality: %v", err)
	}
	for _, l := range got {
		if l.QualityScore != 0.0 {
			t.Errorf("dry-run must not update quality_score: learning %s has %.4f", l.ID, l.QualityScore)
		}
	}
}
