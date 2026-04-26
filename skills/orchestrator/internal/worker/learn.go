package worker

import (
	"context"
	"log/slog"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

// capturePhaseOutput scans the worker directory for markdown files, extracts
// LEARNING/FINDING/PATTERN/DECISION/GOTCHA markers via CaptureFromText, and
// persists them to the default learning DB. It is designed to be called as a
// fire-and-forget goroutine; all errors are logged and never returned.
func capturePhaseOutput(workerDir, workerName, domain, wsID string) {
	db, err := learning.OpenDB("")
	if err != nil {
		slog.Error("learning capture: open db", "worker", workerName, "error", err)
		return
	}
	defer db.Close()

	embedder := learning.NewEmbedder(learning.LoadAPIKey())
	capturePhaseOutputTo(context.Background(), db, embedder, workerDir, workerName, domain, wsID)
}

// capturePhaseOutputTo is the testable core: it uses the provided DB and
// embedder instead of opening new connections. Errors from individual inserts
// are logged; the function continues to the next learning rather than aborting.
func capturePhaseOutputTo(ctx context.Context, db *learning.DB, embedder *learning.Embedder, workerDir, workerName, domain, wsID string) {
	text := readMarkdownFiles(workerDir)
	if text == "" {
		return
	}

	captured := learning.CaptureFromText(text, workerName, domain, wsID)
	for _, l := range captured {
		if insErr := db.Insert(ctx, l, embedder); insErr != nil {
			slog.Error("learning capture: insert", "worker", workerName, "id", l.ID, "error", insErr)
		}
	}
}

// readMarkdownFiles walks dir and returns the concatenated text of all .md files.
// Non-markdown files and unreadable files are silently skipped.
func readMarkdownFiles(dir string) string {
	var buf strings.Builder
	_ = filepath.WalkDir(dir, func(path string, d os.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return nil
		}
		if strings.ToLower(filepath.Ext(path)) != ".md" {
			return nil
		}
		data, readErr := os.ReadFile(path)
		if readErr != nil {
			return nil
		}
		buf.Write(data)
		buf.WriteByte('\n')
		return nil
	})
	return buf.String()
}
