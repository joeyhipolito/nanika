package preflight

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// writeNenStats writes a nenStats value as JSON to a temp file and returns the path.
func writeNenStats(t *testing.T, stats nenStats) string {
	t.Helper()
	data, err := json.Marshal(stats)
	if err != nil {
		t.Fatalf("marshal nen stats: %v", err)
	}
	dir := t.TempDir()
	path := filepath.Join(dir, "nen-daemon.stats.json")
	if err := os.WriteFile(path, data, 0644); err != nil {
		t.Fatalf("write nen stats: %v", err)
	}
	return path
}

func TestNenSection_MissingFile(t *testing.T) {
	t.Setenv("NEN_STATS", "/nonexistent/nen-daemon.stats.json")

	sec := &nenSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("expected no error for missing stats file, got %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for missing file, got %q", blk.Body)
	}
	if blk.Title == "" {
		t.Error("expected non-empty title")
	}
}

func TestNenSection_ValidStats(t *testing.T) {
	now := time.Now().UTC()
	stats := nenStats{
		StartedAt:      now.Add(-2 * time.Hour),
		TotalEvents: 100,
		LastEventAt: now.Add(-1 * time.Minute),
		Scanners: map[string]nenScannerStat{
			"en":  {EventsRouted: 90},
			"gyo": {EventsRouted: 10},
		},
	}
	path := writeNenStats(t, stats)
	t.Setenv("NEN_STATS", path)

	sec := &nenSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body == "" {
		t.Fatal("expected non-empty body")
	}
	if blk.Title != nenBlockTitle {
		t.Errorf("expected title %q, got %q", nenBlockTitle, blk.Title)
	}
	if !strings.Contains(blk.Body, "uptime:") {
		t.Error("expected uptime in output")
	}
	if !strings.Contains(blk.Body, "observers:") {
		t.Error("expected observers in output")
	}
	if !strings.Contains(blk.Body, "total_events: 100") {
		t.Errorf("expected total_events in output, got: %q", blk.Body)
	}
}

func TestNenSection_ObserversSorted(t *testing.T) {
	now := time.Now().UTC()
	stats := nenStats{
		StartedAt:   now.Add(-1 * time.Hour),
		LastEventAt: now.Add(-30 * time.Second),
		Scanners: map[string]nenScannerStat{
			"ryu": {EventsRouted: 1},
			"en":  {EventsRouted: 50},
			"gyo": {EventsRouted: 5},
		},
	}
	path := writeNenStats(t, stats)
	t.Setenv("NEN_STATS", path)

	sec := &nenSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// en should appear before gyo, gyo before ryu (alphabetical sort)
	enIdx := strings.Index(blk.Body, "en(")
	gyoIdx := strings.Index(blk.Body, "gyo(")
	ryuIdx := strings.Index(blk.Body, "ryu(")
	if enIdx < 0 || gyoIdx < 0 || ryuIdx < 0 {
		t.Fatalf("expected all observers in output, got: %q", blk.Body)
	}
	if !(enIdx < gyoIdx && gyoIdx < ryuIdx) {
		t.Errorf("expected alphabetical observer order, got: %q", blk.Body)
	}
}

func TestNenSection_ErrorCountShown(t *testing.T) {
	now := time.Now().UTC()
	stats := nenStats{
		StartedAt:   now.Add(-1 * time.Hour),
		LastEventAt: now.Add(-1 * time.Minute),
		Scanners: map[string]nenScannerStat{
			"en": {EventsRouted: 10, ErrorCount: 3},
		},
	}
	path := writeNenStats(t, stats)
	t.Setenv("NEN_STATS", path)

	sec := &nenSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(blk.Body, "err") {
		t.Errorf("expected error count in observer line, got: %q", blk.Body)
	}
}

func TestNenSection_StaleWarning(t *testing.T) {
	now := time.Now().UTC()
	stats := nenStats{
		StartedAt:   now.Add(-3 * time.Hour),
		LastEventAt: now.Add(-30 * time.Minute), // older than nenStaleThreshold
		TotalEvents: 50,
	}
	path := writeNenStats(t, stats)
	t.Setenv("NEN_STATS", path)

	sec := &nenSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(blk.Body, "WARNING") {
		t.Errorf("expected stale warning for idle daemon, got: %q", blk.Body)
	}
}

func TestNenSection_NoStaleWarningWhenRecent(t *testing.T) {
	now := time.Now().UTC()
	stats := nenStats{
		StartedAt:   now.Add(-1 * time.Hour),
		LastEventAt: now.Add(-1 * time.Minute), // within threshold
		TotalEvents: 20,
	}
	path := writeNenStats(t, stats)
	t.Setenv("NEN_STATS", path)

	sec := &nenSection{}
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.Contains(blk.Body, "WARNING") {
		t.Errorf("unexpected stale warning for recent daemon: %q", blk.Body)
	}
}

func TestNenSection_Metadata(t *testing.T) {
	sec := &nenSection{}
	if sec.Name() != "nen" {
		t.Errorf("expected name 'nen', got %q", sec.Name())
	}
	if sec.Priority() <= 0 {
		t.Errorf("expected positive priority, got %d", sec.Priority())
	}
}

func TestFormatDuration(t *testing.T) {
	cases := []struct {
		d    time.Duration
		want string
	}{
		{30 * time.Second, "30s"},
		{90 * time.Second, "1m30s"},
		{5 * time.Minute, "5m"},
		{65 * time.Minute, "1h5m"},
		{2 * time.Hour, "2h"},
		{2*time.Hour + 30*time.Minute, "2h30m"},
	}
	for _, tc := range cases {
		got := formatDuration(tc.d)
		if got != tc.want {
			t.Errorf("formatDuration(%v) = %q, want %q", tc.d, got, tc.want)
		}
	}
}

func TestFormatNenBlock_UptimeAndObservers(t *testing.T) {
	now := time.Date(2026, 4, 6, 12, 0, 0, 0, time.UTC)
	stats := &nenStats{
		StartedAt:   now.Add(-2*time.Hour - 15*time.Minute),
		LastEventAt: now.Add(-3 * time.Minute),
		TotalEvents: 200,
		Scanners: map[string]nenScannerStat{
			"en": {EventsRouted: 200},
		},
	}

	body := formatNenBlock(stats, now)
	if !strings.Contains(body, "uptime: 2h15m") {
		t.Errorf("expected uptime 2h15m, got: %q", body)
	}
	if !strings.Contains(body, "en(200)") {
		t.Errorf("expected en(200) in observers, got: %q", body)
	}
	if !strings.Contains(body, "last_event: 3m ago") {
		t.Errorf("expected last_event: 3m ago, got: %q", body)
	}
}
