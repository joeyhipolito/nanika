package cmd

import (
	"bytes"
	"context"
	"database/sql"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
	_ "modernc.org/sqlite"
)

// TestRunBackfillEmbeddings_DryRunReportsCountAndZeroAPICalls confirms that
// without --apply the command emits a row count and never invokes the
// embedding API. We assert no API calls by configuring GEMINI_API_KEY to a
// sentinel value and pointing at a non-routable URL — if the dry-run code
// path hit the network the test would hang or fail.
func TestRunBackfillEmbeddings_DryRunReportsCountAndZeroAPICalls(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "test.db")

	seedDB, err := learning.OpenDB(dbPath)
	if err != nil {
		t.Fatalf("open seed DB: %v", err)
	}
	seedDB.Close()

	raw, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open raw seed DB: %v", err)
	}
	now := time.Now()
	for _, id := range []string{"null-1", "null-2"} {
		if _, err := raw.Exec(`
			INSERT INTO learnings (id, type, content, context, domain, created_at, embedding, archived)
			VALUES (?, 'insight', ?, '', 'dev', ?, NULL, 0)
		`, id, "needs embedding "+id, now.UTC().Format(time.RFC3339)); err != nil {
			t.Fatalf("seed null row %s: %v", id, err)
		}
	}
	if _, err := raw.Exec(`
		INSERT INTO learnings (id, type, content, context, domain, created_at, embedding, archived)
		VALUES ('has-emb', 'insight', 'already done', '', 'dev', ?, X'01020304', 0)
	`, now.UTC().Format(time.RFC3339)); err != nil {
		t.Fatalf("seed embedded row: %v", err)
	}
	raw.Close()

	// Force any embedder construction to fail by clearing the API key. The
	// dry-run code path must not reach LoadAPIKey at all — if it did, this
	// would surface as exit-code 3 or a network call.
	t.Setenv("GEMINI_API_KEY", "")
	t.Setenv("HOME", dir) // isolate from real ~/.alluka and ~/.obsidian configs

	cmd := &cobra.Command{Use: "backfill-embeddings", RunE: runBackfillEmbeddings}
	cmd.Flags().Bool("apply", false, "")
	cmd.Flags().Duration("since", 0, "")
	cmd.Flags().Int("limit", 0, "")
	cmd.Flags().Int("batch-size", 100, "")
	cmd.Flags().Int("rpm", 60, "")
	cmd.Flags().Int("max-retries", 5, "")
	cmd.Flags().String("db", "", "")
	cmd.Flags().Bool("include-archived", false, "")
	cmd.Flags().Bool("quiet", false, "")
	if err := cmd.Flags().Set("db", dbPath); err != nil {
		t.Fatalf("set db: %v", err)
	}

	out := captureStdout(t, func() {
		if err := cmd.ExecuteContext(context.Background()); err != nil {
			t.Fatalf("dry-run execute: %v", err)
		}
	})

	if !strings.Contains(out, "candidate rows: 2") {
		t.Errorf("dry-run output missing 'candidate rows: 2'\ngot: %s", out)
	}
	if !strings.Contains(out, "dry-run") {
		t.Errorf("dry-run output missing 'dry-run' marker\ngot: %s", out)
	}
	if !strings.Contains(out, "--apply") {
		t.Errorf("dry-run output missing '--apply' hint\ngot: %s", out)
	}
	// The dry-run path must not have written embeddings to either NULL row.
	verify, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("reopen: %v", err)
	}
	defer verify.Close()
	for _, id := range []string{"null-1", "null-2"} {
		var got []byte
		if err := verify.QueryRow(`SELECT embedding FROM learnings WHERE id = ?`, id).Scan(&got); err != nil {
			t.Fatalf("post-dry-run fetch %s: %v", id, err)
		}
		if len(got) != 0 {
			t.Errorf("dry-run wrote embedding to %s (len=%d), expected NULL", id, len(got))
		}
	}
}

func captureStdout(t *testing.T, fn func()) string {
	t.Helper()
	orig := os.Stdout
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("os.Pipe: %v", err)
	}
	os.Stdout = w
	defer func() { os.Stdout = orig }()

	done := make(chan struct{})
	var buf bytes.Buffer
	go func() {
		_, _ = io.Copy(&buf, r)
		close(done)
	}()

	fn()

	w.Close()
	<-done
	return buf.String()
}
