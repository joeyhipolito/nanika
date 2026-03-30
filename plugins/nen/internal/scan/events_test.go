package scan

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// writeEventLog writes a JSONL event log to a temp file and returns the path.
func writeEventLog(t *testing.T, lines []string) string {
	t.Helper()
	dir := t.TempDir()
	path := filepath.Join(dir, "mission-abc.jsonl")
	content := strings.Join(lines, "\n") + "\n"
	if err := os.WriteFile(path, []byte(content), 0600); err != nil {
		t.Fatalf("write event log: %v", err)
	}
	return path
}

func ts(offset time.Duration) string {
	return time.Now().UTC().Add(offset).Format(time.RFC3339)
}

func TestCollectMissionRetryInfo_BasicRetry(t *testing.T) {
	path := writeEventLog(t, []string{
		`{"type":"phase.retrying","timestamp":"` + ts(-1*time.Hour) + `","phase_id":"phase-123","worker_id":"implement","data":{"error":"timeout","attempt":1}}`,
	})

	since := time.Now().UTC().Add(-2 * time.Hour)
	info, err := CollectMissionRetryInfo(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(info.Events) != 1 {
		t.Fatalf("want 1 event, got %d", len(info.Events))
	}
	ev := info.Events[0]
	if ev.PhaseID != "phase-123" {
		t.Errorf("PhaseID: want %q, got %q", "phase-123", ev.PhaseID)
	}
	if ev.WorkerID != "implement" {
		t.Errorf("WorkerID (phase name): want %q, got %q", "implement", ev.WorkerID)
	}
	if ev.Error != "timeout" {
		t.Errorf("Error: want %q, got %q", "timeout", ev.Error)
	}
	if ev.Attempt != 1 {
		t.Errorf("Attempt: want 1, got %d", ev.Attempt)
	}
}

func TestCollectMissionRetryInfo_PhaseNameInWorkerID(t *testing.T) {
	// The orchestrator emits phase name as worker_id in phase.retrying events.
	// Verify both phase ID (UUID) and phase name (WorkerID) are captured.
	path := writeEventLog(t, []string{
		`{"type":"phase.retrying","timestamp":"` + ts(-30*time.Minute) + `","phase_id":"ph-uuid-456","worker_id":"research","data":{"error":"gate failed","attempt":2}}`,
	})

	since := time.Now().UTC().Add(-1 * time.Hour)
	info, err := CollectMissionRetryInfo(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(info.Events) != 1 {
		t.Fatalf("want 1 event, got %d", len(info.Events))
	}
	ev := info.Events[0]
	if ev.PhaseID != "ph-uuid-456" {
		t.Errorf("PhaseID: want %q, got %q", "ph-uuid-456", ev.PhaseID)
	}
	if ev.WorkerID != "research" {
		t.Errorf("WorkerID (phase name): want %q, got %q", "research", ev.WorkerID)
	}
	if ev.Attempt != 2 {
		t.Errorf("Attempt: want 2, got %d", ev.Attempt)
	}
}

func TestCollectMissionRetryInfo_SkipsOldEvents(t *testing.T) {
	path := writeEventLog(t, []string{
		// older than since — should be skipped
		`{"type":"phase.retrying","timestamp":"` + ts(-48*time.Hour) + `","phase_id":"old-phase","worker_id":"implement","data":{"error":"x","attempt":1}}`,
		// within window — should be included
		`{"type":"phase.retrying","timestamp":"` + ts(-1*time.Hour) + `","phase_id":"new-phase","worker_id":"implement","data":{"error":"y","attempt":1}}`,
	})

	since := time.Now().UTC().Add(-24 * time.Hour)
	info, err := CollectMissionRetryInfo(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(info.Events) != 1 {
		t.Fatalf("want 1 event (old skipped), got %d", len(info.Events))
	}
	if info.Events[0].PhaseID != "new-phase" {
		t.Errorf("want new-phase, got %q", info.Events[0].PhaseID)
	}
}

func TestCollectMissionRetryInfo_ExtractsMissionTitle(t *testing.T) {
	path := writeEventLog(t, []string{
		`{"type":"mission.started","data":{"task":"# Build Authentication System\n\nImplement OAuth."}}`,
		`{"type":"phase.retrying","timestamp":"` + ts(-1*time.Hour) + `","phase_id":"ph1","worker_id":"implement","data":{"error":"err","attempt":1}}`,
	})

	since := time.Now().UTC().Add(-2 * time.Hour)
	info, err := CollectMissionRetryInfo(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if info.Task != "Build Authentication System" {
		t.Errorf("Task: want %q, got %q", "Build Authentication System", info.Task)
	}
}

func TestCollectMissionRetryInfo_EmptyFile(t *testing.T) {
	path := writeEventLog(t, nil)
	since := time.Now().UTC().Add(-24 * time.Hour)
	info, err := CollectMissionRetryInfo(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(info.Events) != 0 {
		t.Errorf("want 0 events, got %d", len(info.Events))
	}
}

func TestCollectMissionRetryInfo_SkipsNonRetryEvents(t *testing.T) {
	path := writeEventLog(t, []string{
		`{"type":"phase.completed","timestamp":"` + ts(-1*time.Hour) + `","phase_id":"ph1","worker_id":"implement","data":{}}`,
		`{"type":"mission.started","data":{"task":"some task"}}`,
	})

	since := time.Now().UTC().Add(-2 * time.Hour)
	info, err := CollectMissionRetryInfo(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(info.Events) != 0 {
		t.Errorf("want 0 events, got %d", len(info.Events))
	}
}

func TestCollectMissionRetryInfo_CorruptLinesSkipped(t *testing.T) {
	path := writeEventLog(t, []string{
		`not valid json at all {{{`,
		`{"type":"phase.retrying","timestamp":"` + ts(-1*time.Hour) + `","phase_id":"ph1","worker_id":"implement","data":{"error":"ok","attempt":1}}`,
		`{"broken`,
	})

	since := time.Now().UTC().Add(-2 * time.Hour)
	info, err := CollectMissionRetryInfo(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(info.Events) != 1 {
		t.Fatalf("want 1 event (corrupt lines skipped), got %d", len(info.Events))
	}
}

func TestMissionTitle_MarkdownHeading(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "h1 heading",
			input: "# Build Auth System\n\nsome body",
			want:  "Build Auth System",
		},
		{
			name:  "no heading falls back to first line",
			input: "Plain task description",
			want:  "Plain task description",
		},
		{
			name:  "yaml frontmatter skipped",
			input: "---\nversion: 1\n---\n# Real Title\nBody",
			want:  "Real Title",
		},
		{
			name:  "empty string",
			input: "",
			want:  "",
		},
		{
			name:  "only whitespace lines then heading",
			input: "\n\n# Title Here\n",
			want:  "Title Here",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := missionTitle(tt.input)
			if got != tt.want {
				t.Errorf("missionTitle(%q) = %q, want %q", tt.input, got, tt.want)
			}
		})
	}
}

func TestCountPhaseRetryingEvents_DelegatesToCollect(t *testing.T) {
	path := writeEventLog(t, []string{
		`{"type":"phase.retrying","timestamp":"` + ts(-1*time.Hour) + `","phase_id":"ph1","worker_id":"implement","data":{"error":"x","attempt":1}}`,
		`{"type":"phase.retrying","timestamp":"` + ts(-2*time.Hour) + `","phase_id":"ph2","worker_id":"review","data":{"error":"y","attempt":1}}`,
	})

	since := time.Now().UTC().Add(-3 * time.Hour)
	count, err := CountPhaseRetryingEvents(context.Background(), path, since)
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if count != 2 {
		t.Errorf("want 2, got %d", count)
	}
}
