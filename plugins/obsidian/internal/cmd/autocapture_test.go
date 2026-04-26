package cmd

import (
	"bytes"
	"io"
	"os"
	"strings"
	"testing"
)

// captureStdout captures text written to os.Stdout during f().
func captureStdout(t *testing.T, f func()) string {
	t.Helper()
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("os.Pipe: %v", err)
	}
	old := os.Stdout
	os.Stdout = w

	f()

	w.Close()
	os.Stdout = old

	var buf bytes.Buffer
	if _, err := io.Copy(&buf, r); err != nil {
		t.Fatalf("io.Copy: %v", err)
	}
	return buf.String()
}

// ---------- printAutoCaptureReport ----------

func TestPrintAutoCaptureReport_NoItems(t *testing.T) {
	sources := []IngestOutput{
		{Source: "learnings"},
		{Source: "workspaces"},
		{Source: "scout"},
	}

	out := captureStdout(t, func() {
		printAutoCaptureReport(sources, false)
	})

	if out == "" {
		t.Error("expected non-empty output")
	}
	for _, s := range []string{"[learnings]", "[workspaces]", "[scout]"} {
		if !strings.Contains(out, s) {
			t.Errorf("expected %q in output", s)
		}
	}
	if !strings.Contains(out, "No new items") {
		t.Error("expected 'No new items' when all sources are empty")
	}
}

func TestPrintAutoCaptureReport_WithCreated(t *testing.T) {
	sources := []IngestOutput{
		{
			Source:  "learnings",
			Created: []string{"Learnings/dev/error-abc.md", "Learnings/dev/insight-xyz.md"},
			Skipped: []string{"learn_old"},
		},
		{Source: "workspaces"},
		{Source: "scout"},
	}

	out := captureStdout(t, func() {
		printAutoCaptureReport(sources, false)
	})

	if !strings.Contains(out, "Created (2)") {
		t.Errorf("expected 'Created (2)' in output, got:\n%s", out)
	}
	if !strings.Contains(out, "Learnings/dev/error-abc.md") {
		t.Error("expected created path in output")
	}
	if !strings.Contains(out, "Skipped: 1 already captured") {
		t.Error("expected skipped count in output")
	}
	if !strings.Contains(out, "Summary: 2 created, 1 skipped, 0 errors") {
		t.Errorf("expected summary line, got:\n%s", out)
	}
}

func TestPrintAutoCaptureReport_DryRun(t *testing.T) {
	sources := []IngestOutput{
		{
			Source:  "workspaces",
			Created: []string{"Captures/Workspaces/ws-abc.md"},
		},
	}

	out := captureStdout(t, func() {
		printAutoCaptureReport(sources, true)
	})

	if !strings.Contains(out, "(dry run)") {
		t.Errorf("expected '(dry run)' header, got:\n%s", out)
	}
	if !strings.Contains(out, "Would create (1)") {
		t.Errorf("expected 'Would create (1)' in dry-run output, got:\n%s", out)
	}
}

func TestPrintAutoCaptureReport_WithErrors(t *testing.T) {
	sources := []IngestOutput{
		{Source: "learnings"},
		{Source: "workspaces"},
		{
			Source: "scout",
			Errors: []string{"scout intel directory not found"},
		},
	}

	out := captureStdout(t, func() {
		printAutoCaptureReport(sources, false)
	})

	if !strings.Contains(out, "Errors (1)") {
		t.Errorf("expected 'Errors (1)' in output, got:\n%s", out)
	}
	if !strings.Contains(out, "scout intel directory not found") {
		t.Error("expected error message in output")
	}
	if !strings.Contains(out, "Summary: 0 created, 0 skipped, 1 errors") {
		t.Errorf("expected summary with 1 error, got:\n%s", out)
	}
}

// ---------- AutoCaptureCmd flag validation ----------

func TestAutoCaptureCmd_InvalidSince(t *testing.T) {
	vault := t.TempDir()
	err := AutoCaptureCmd(vault, AutoCaptureOptions{
		Since: "bad-value",
	})
	if err == nil {
		t.Error("expected error for invalid --since value")
	}
}

// ---------- AutoCaptureSummary aggregation ----------

func TestAutoCaptureSummary_Aggregation(t *testing.T) {
	sources := []IngestOutput{
		{Source: "learnings", Created: []string{"a", "b"}, Skipped: []string{"c"}, Errors: []string{"e1"}},
		{Source: "workspaces", Created: []string{"d"}, Skipped: []string{"f", "g"}},
		{Source: "scout", Errors: []string{"e2", "e3"}},
	}

	var total AutoCaptureSummary
	for _, s := range sources {
		total.Created += len(s.Created)
		total.Skipped += len(s.Skipped)
		total.Errors += len(s.Errors)
	}

	if total.Created != 3 {
		t.Errorf("expected Created=3, got %d", total.Created)
	}
	if total.Skipped != 3 {
		t.Errorf("expected Skipped=3, got %d", total.Skipped)
	}
	if total.Errors != 3 {
		t.Errorf("expected Errors=3, got %d", total.Errors)
	}
}
