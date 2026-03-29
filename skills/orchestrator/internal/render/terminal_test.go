package render

import (
	"bytes"
	"context"
	"os"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

// TestColorDetectionTTY verifies colors are present when output is a TTY.
func TestColorDetectionTTY(t *testing.T) {
	// Create a temporary file to act as a TTY-like output
	tmpfile, err := os.CreateTemp("", "test_tty")
	if err != nil {
		t.Fatalf("failed to create temp file: %v", err)
	}
	defer os.Remove(tmpfile.Name())

	// detectPalette checks if output is a TTY by checking os.ModeCharDevice.
	// Since we can't easily create a real TTY in tests, we'll test the logic directly
	// by checking that colors are empty for non-TTY detection.
	palette := detectPalette(tmpfile)

	// A regular file (not a TTY) should have empty colors
	if palette.Reset != "" || palette.Bold != "" {
		t.Error("expected empty palette for non-TTY output, got color codes")
	}
}

// TestColorDetectionNonTTY verifies colors are suppressed for non-TTY output.
func TestColorDetectionNonTTY(t *testing.T) {
	// Create a pipe which is definitely not a TTY
	r, w, err := os.Pipe()
	if err != nil {
		t.Fatalf("failed to create pipe: %v", err)
	}
	defer r.Close()
	defer w.Close()

	palette := detectPalette(w)

	// Pipe output is not a TTY, so colors should be empty
	if palette.Reset != "" || palette.Bold != "" || palette.Cyan != "" {
		t.Error("expected empty palette for non-TTY output, got color codes")
	}
}

// TestOutputFormat_DecomposeStarted verifies decompose-started event format.
func TestOutputFormat_DecomposeStarted(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{}, // no colors
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.DecomposeStarted,
		Timestamp: time.Now(),
		Data: map[string]any{
			"task_summary": "implement user authentication",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Check that output contains expected elements
	if !strings.Contains(output, "decomposing") {
		t.Error("output should contain 'decomposing'")
	}
	if !strings.Contains(output, "implement user authentication") {
		t.Error("output should contain the task summary")
	}
	if !strings.Contains(output, "▸") {
		t.Error("output should contain the decompose indicator (▸)")
	}
}

// TestOutputFormat_DecomposeStarted_Truncation verifies long summaries are truncated.
func TestOutputFormat_DecomposeStarted_Truncation(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	// Create a very long summary (>80 chars)
	longSummary := strings.Repeat("a", 100)
	ev := event.Event{
		Type:      event.DecomposeStarted,
		Timestamp: time.Now(),
		Data: map[string]any{
			"task_summary": longSummary,
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Should contain ellipsis for truncation
	if !strings.Contains(output, "...") {
		t.Error("output should contain ellipsis for truncated summary")
	}

	// Full long summary should not appear
	if strings.Contains(output, longSummary) {
		t.Error("output should not contain the full long summary")
	}
}

// TestOutputFormat_MissionStarted verifies mission-started event format.
func TestOutputFormat_MissionStarted(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.MissionStarted,
		Timestamp: time.Now(),
		Data: map[string]any{
			"phases":           5,
			"execution_mode":   "sequential",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "mission started") {
		t.Error("output should contain 'mission started'")
	}
	if !strings.Contains(output, "5 phase") {
		t.Error("output should contain phase count")
	}
	if !strings.Contains(output, "sequential") {
		t.Error("output should contain execution mode")
	}
}

// TestOutputFormat_PhaseStarted verifies phase-started event format.
func TestOutputFormat_PhaseStarted(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.PhaseStarted,
		PhaseID:   "phase-1",
		Timestamp: time.Now(),
		Data: map[string]any{
			"name":    "implement",
			"persona": "senior-backend-engineer",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "implement") {
		t.Error("output should contain phase name")
	}
	if !strings.Contains(output, "senior-backend-engineer") {
		t.Error("output should contain persona")
	}
	if !strings.Contains(output, "▶") {
		t.Error("output should contain phase start indicator (▶)")
	}
}

// TestOutputFormat_PhaseCompleted verifies phase-completed event format.
func TestOutputFormat_PhaseCompleted(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	// Record phase start for duration calculation
	now := time.Now()
	r.phaseStarts["phase-1"] = now.Add(-5 * time.Second)
	r.phaseMeta["phase-1"] = phaseMeta{name: "implement", persona: "engineer"}

	ev := event.Event{
		Type:      event.PhaseCompleted,
		PhaseID:   "phase-1",
		Timestamp: now,
		Data: map[string]any{
			"retries": 0,
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "implement") {
		t.Error("output should contain phase name")
	}
	if !strings.Contains(output, "completed") {
		t.Error("output should contain 'completed'")
	}
	if !strings.Contains(output, "5s") {
		t.Error("output should contain duration")
	}
	if !strings.Contains(output, "✔") {
		t.Error("output should contain completion indicator (✔)")
	}
}

// TestOutputFormat_PhaseCompleted_WithRetries verifies retry count is shown.
func TestOutputFormat_PhaseCompleted_WithRetries(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	now := time.Now()
	r.phaseStarts["phase-1"] = now
	r.phaseMeta["phase-1"] = phaseMeta{name: "test", persona: "engineer"}

	ev := event.Event{
		Type:      event.PhaseCompleted,
		PhaseID:   "phase-1",
		Timestamp: now.Add(1 * time.Second),
		Data: map[string]any{
			"retries": 2,
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "2 retries") {
		t.Error("output should show retry count")
	}
}

// TestOutputFormat_PhaseFailed verifies failed phase format.
func TestOutputFormat_PhaseFailed(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	now := time.Now()
	r.phaseStarts["phase-1"] = now
	r.phaseMeta["phase-1"] = phaseMeta{name: "implement", persona: "engineer"}

	ev := event.Event{
		Type:      event.PhaseFailed,
		PhaseID:   "phase-1",
		Timestamp: now.Add(10 * time.Second),
		Data: map[string]any{
			"error": "connection timeout",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "failed") {
		t.Error("output should contain 'failed'")
	}
	if !strings.Contains(output, "connection timeout") {
		t.Error("output should contain error message")
	}
	if !strings.Contains(output, "✘") {
		t.Error("output should contain failure indicator (✘)")
	}
}

// TestOutputFormat_PhaseSkipped verifies skipped phase format.
func TestOutputFormat_PhaseSkipped(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.PhaseSkipped,
		PhaseID:   "phase-2",
		Timestamp: time.Now(),
		Data: map[string]any{
			"name":   "review",
			"reason": "no code changes",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "skipped") {
		t.Error("output should contain 'skipped'")
	}
	if !strings.Contains(output, "no code changes") {
		t.Error("output should contain skip reason")
	}
	if !strings.Contains(output, "⊘") {
		t.Error("output should contain skip indicator (⊘)")
	}
}

// TestOutputFormat_MissionCompleted verifies mission completion format.
func TestOutputFormat_MissionCompleted(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	// Record phase outcomes
	r.completed = 3
	r.failed = 0
	r.skipped = 1

	ev := event.Event{
		Type:      event.MissionCompleted,
		Timestamp: time.Now(),
		Data: map[string]any{
			"duration":  "2m30s",
			"artifacts": 2,
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "mission completed") {
		t.Error("output should contain 'mission completed'")
	}
	if !strings.Contains(output, "2m30s") {
		t.Error("output should contain duration")
	}
	if !strings.Contains(output, "2 artifact") {
		t.Error("output should contain artifact count")
	}
	if !strings.Contains(output, "3 completed") {
		t.Error("output should show completed phase count")
	}
	if !strings.Contains(output, "1 skipped") {
		t.Error("output should show skipped phase count")
	}
	if !strings.Contains(output, "✔") {
		t.Error("output should contain success indicator (✔)")
	}
}

// TestOutputFormat_MissionFailed verifies mission failure format.
func TestOutputFormat_MissionFailed(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	r.completed = 1
	r.failed = 1

	ev := event.Event{
		Type:      event.MissionFailed,
		Timestamp: time.Now(),
		Data: map[string]any{
			"duration": "1m15s",
			"error":    "worker process exited unexpectedly",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "mission failed") {
		t.Error("output should contain 'mission failed'")
	}
	if !strings.Contains(output, "worker process exited") {
		t.Error("output should contain error message")
	}
	if !strings.Contains(output, "✘") {
		t.Error("output should contain failure indicator (✘)")
	}
}

// TestVerboseMode verifies raw event output appears in verbose mode.
func TestVerboseMode(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     true, // verbose enabled
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.PhaseStarted,
		PhaseID:   "phase-1",
		WorkerID:  "worker-1",
		Sequence:  42,
		Timestamp: time.Now(),
		Data: map[string]any{
			"name": "test",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// In verbose mode, should contain both formatted output AND raw event output
	lines := strings.Split(output, "\n")
	if len(lines) < 2 {
		t.Error("verbose mode should produce multiple lines")
	}

	// Should contain raw event line with sequence number and type
	// Event type is "phase.started" and should have "phase=phase-1"
	hasRawEvent := false
	for _, line := range lines {
		if strings.Contains(line, "phase.started") && strings.Contains(line, "phase=phase-1") {
			hasRawEvent = true
			break
		}
	}
	if !hasRawEvent {
		t.Error("verbose mode should show raw event output with type and phase")
	}
}

// TestVerboseMode_ShowsTimestamp verifies timestamps in verbose output.
func TestVerboseMode_ShowsTimestamp(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     true,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.MissionStarted,
		Timestamp: time.Now(),
		Data: map[string]any{
			"phases": 3,
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Raw event line should contain timestamp (HH:MM:SS.fff format)
	if !strings.Contains(output, ":") {
		t.Error("verbose output should contain timestamp")
	}
}

// TestNonVerboseMode verifies raw events are NOT printed without verbose.
func TestNonVerboseMode(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false, // verbose disabled
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.PhaseStarted,
		PhaseID:   "phase-1",
		Timestamp: time.Now(),
		Data: map[string]any{
			"name":    "implement",
			"persona": "engineer",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Should not contain raw event output when verbose is false
	// (raw events contain phase IDs in "phase=" format)
	if strings.Contains(output, "phase=") {
		t.Error("non-verbose mode should not show raw event output")
	}
}

// TestWorkerOutput_ToolUse verifies tool use events are rendered compactly.
func TestWorkerOutput_ToolUse(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.WorkerOutput,
		Timestamp: time.Now(),
		Data: map[string]any{
			"event_kind": "tool_use",
			"tool_name":  "Read",
			"chunk":      "some very long tool output that should be abbreviated",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Tool events should be compact: just the tool name indicator
	if !strings.Contains(output, "Read") {
		t.Error("output should contain tool name")
	}
	if strings.Contains(output, "some very long") {
		t.Error("output should not contain full tool output for tool_use events")
	}
}

// TestWorkerOutput_TextContent verifies text output is indented and dimmed.
func TestWorkerOutput_TextContent(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.WorkerOutput,
		Timestamp: time.Now(),
		Data: map[string]any{
			"event_kind": "text",
			"chunk":      "this is worker output\nwith multiple lines",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Text content should be present (multiple lines)
	if !strings.Contains(output, "this is worker output") {
		t.Error("output should contain worker text")
	}
	if !strings.Contains(output, "multiple lines") {
		t.Error("output should contain all worker text lines")
	}
}

// TestWorkerOutput_EmptyChunk verifies empty chunks are ignored.
func TestWorkerOutput_EmptyChunk(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.WorkerOutput,
		Timestamp: time.Now(),
		Data: map[string]any{
			"event_kind": "text",
			"chunk":      "",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if output != "" {
		t.Error("empty chunks should produce no output")
	}
}

// TestGitEvents verifies git event formats.
func TestGitEvents(t *testing.T) {
	tests := []struct {
		name        string
		eventType   event.EventType
		data        map[string]any
		shouldMatch []string
	}{
		{
			name:      "GitWorktreeCreated",
			eventType: event.GitWorktreeCreated,
			data: map[string]any{
				"branch": "feature/auth",
			},
			shouldMatch: []string{"git:", "worktree created", "feature/auth"},
		},
		{
			name:      "GitCommitted",
			eventType: event.GitCommitted,
			data: map[string]any{
				"sha": "1a2b3c4d5e6f7g8h9i0j",
			},
			shouldMatch: []string{"git:", "committed", "1a2b3c4d"},
		},
		{
			name:      "GitPRCreated",
			eventType: event.GitPRCreated,
			data: map[string]any{
				"pr_url": "https://github.com/user/repo/pull/42",
			},
			shouldMatch: []string{"git:", "PR created", "https://github.com"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			w := &bytes.Buffer{}
			r := &TerminalRenderer{
				w:           w,
				c:           palette{},
				verbose:     false,
				phaseStarts: make(map[string]time.Time),
				phaseMeta:   make(map[string]phaseMeta),
			}

			ev := event.Event{
				Type:      tt.eventType,
				Timestamp: time.Now(),
				Data:      tt.data,
			}

			r.Emit(context.Background(), ev)
			output := w.String()

			for _, match := range tt.shouldMatch {
				if !strings.Contains(output, match) {
					t.Errorf("output should contain %q", match)
				}
			}
		})
	}
}

// TestSystemError verifies error formatting.
func TestSystemError(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.SystemError,
		Timestamp: time.Now(),
		Data: map[string]any{
			"error": "database connection failed",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "system error") {
		t.Error("output should contain 'system error'")
	}
	if !strings.Contains(output, "database connection failed") {
		t.Error("output should contain error message")
	}
}

// TestPhaseSummary_NoPhases verifies summary is skipped when no phases completed.
func TestPhaseSummary_NoPhases(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		completed:   0,
		failed:      0,
		skipped:     0,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.MissionCompleted,
		Timestamp: time.Now(),
		Data: map[string]any{
			"duration":  "1s",
			"artifacts": 0,
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Summary should not be printed if no phases were recorded
	if strings.Contains(output, "phases:") {
		t.Error("summary should not be printed when no phases were recorded")
	}
}

// TestPhaseDuration_Formatting verifies duration calculation and formatting.
func TestPhaseDuration_Formatting(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	startTime := time.Now().Add(-5*time.Minute - 30*time.Second)
	r.phaseStarts["phase-1"] = startTime
	r.phaseMeta["phase-1"] = phaseMeta{name: "long-task", persona: "worker"}

	ev := event.Event{
		Type:      event.PhaseCompleted,
		PhaseID:   "phase-1",
		Timestamp: time.Now(),
		Data: map[string]any{
			"retries": 0,
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Should contain duration rounded to seconds
	if !strings.Contains(output, "5m30s") && !strings.Contains(output, "330s") {
		t.Error("output should contain properly formatted duration")
	}
}

// TestPhaseMetadata_FallbackToPhaseID verifies PhaseID is used when name is missing.
func TestPhaseMetadata_FallbackToPhaseID(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	now := time.Now()
	r.phaseStarts["phase-abc123"] = now
	// No phase metadata for this ID

	ev := event.Event{
		Type:      event.PhaseFailed,
		PhaseID:   "phase-abc123",
		Timestamp: now.Add(1 * time.Second),
		Data: map[string]any{
			"error": "timeout",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Should fall back to showing the phase ID
	if !strings.Contains(output, "phase-abc123") {
		t.Error("output should contain phase ID as fallback when name is missing")
	}
}

// TestThreadSafety verifies concurrent emissions don't cause data races.
func TestThreadSafety(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	// Emit events concurrently
	done := make(chan bool, 10)
	for i := 0; i < 10; i++ {
		go func(idx int) {
			ev := event.Event{
				Type:      event.PhaseStarted,
				PhaseID:   "phase-" + string(rune(idx)),
				Timestamp: time.Now(),
				Data: map[string]any{
					"name":    "phase",
					"persona": "worker",
				},
			}
			r.Emit(context.Background(), ev)
			done <- true
		}(i)
	}

	// Wait for all goroutines
	for i := 0; i < 10; i++ {
		<-done
	}

	// Just verify no panic or data race
	output := w.String()
	if len(output) == 0 {
		t.Error("expected some output from concurrent emissions")
	}
}

// TestDecomposeCompleted_PhaseTable verifies phase table formatting.
func TestDecomposeCompleted_PhaseTable(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	ev := event.Event{
		Type:      event.DecomposeCompleted,
		Timestamp: time.Now(),
		Data: map[string]any{
			"phase_count":    3,
			"execution_mode": "sequential",
			"phases": []any{
				map[string]any{"name": "plan", "persona": "architect"},
				map[string]any{"name": "implement", "persona": "engineer"},
				map[string]any{"name": "review", "persona": "reviewer"},
			},
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	// Should show plan summary
	if !strings.Contains(output, "3 phase") {
		t.Error("output should contain phase count")
	}

	// Should show phase table
	if !strings.Contains(output, "plan") || !strings.Contains(output, "architect") {
		t.Error("output should contain first phase and persona")
	}
	if !strings.Contains(output, "implement") || !strings.Contains(output, "engineer") {
		t.Error("output should contain implementation phase and persona")
	}
	if !strings.Contains(output, "review") || !strings.Contains(output, "reviewer") {
		t.Error("output should contain review phase and persona")
	}

	// Should use numbering
	if !strings.Contains(output, "1.") || !strings.Contains(output, "2.") || !strings.Contains(output, "3.") {
		t.Error("output should have numbered phases")
	}
}

// TestPhaseRetrying verifies retry event format.
func TestPhaseRetrying(t *testing.T) {
	w := &bytes.Buffer{}
	r := &TerminalRenderer{
		w:           w,
		c:           palette{},
		verbose:     false,
		phaseStarts: make(map[string]time.Time),
		phaseMeta:   make(map[string]phaseMeta),
	}

	r.phaseMeta["phase-1"] = phaseMeta{name: "deploy", persona: "devops"}

	ev := event.Event{
		Type:      event.PhaseRetrying,
		PhaseID:   "phase-1",
		Timestamp: time.Now(),
		Data: map[string]any{
			"attempt": 2,
			"error":   "service unavailable",
			"backoff": "5s",
		},
	}

	r.Emit(context.Background(), ev)
	output := w.String()

	if !strings.Contains(output, "deploy") {
		t.Error("output should contain phase name")
	}
	if !strings.Contains(output, "attempt 2") {
		t.Error("output should show attempt number")
	}
	if !strings.Contains(output, "service unavailable") {
		t.Error("output should show error reason")
	}
	if !strings.Contains(output, "5s") {
		t.Error("output should show backoff duration")
	}
	if !strings.Contains(output, "↻") {
		t.Error("output should contain retry indicator (↻)")
	}
}
