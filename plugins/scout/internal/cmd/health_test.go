package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func healthFilePath(home string) string {
	return filepath.Join(home, ".scout", "health.json")
}

func writeHealthJSON(t *testing.T, home string, data interface{}) {
	t.Helper()
	path := healthFilePath(home)
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	b, err := json.MarshalIndent(data, "", "  ")
	if err != nil {
		t.Fatalf("marshal: %v", err)
	}
	if err := os.WriteFile(path, b, 0600); err != nil {
		t.Fatalf("write health file: %v", err)
	}
}

// ─── HealthCmd ───────────────────────────────────────────────────────────────

func TestHealthCmd_NoData(t *testing.T) {
	setupTempHome(t)

	r, w, _ := os.Pipe()
	old := os.Stdout
	os.Stdout = w

	err := HealthCmd([]string{}, false)
	w.Close()
	os.Stdout = old

	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	output := string(buf[:n])

	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "No health data") {
		t.Errorf("expected 'No health data' message, got: %s", output)
	}
}

func TestHealthCmd_ShowsHealthData(t *testing.T) {
	home := setupTempHome(t)

	now := time.Now().UTC()
	writeHealthJSON(t, home, map[string]interface{}{
		"sources": map[string]interface{}{
			"hackernews": map[string]interface{}{
				"last_success":     now,
				"success_count":    10,
				"failure_count":    0,
				"total_latency_ms": 3000,
				"call_count":       10,
			},
			"reddit": map[string]interface{}{
				"last_failure":     now,
				"success_count":    5,
				"failure_count":    3,
				"total_latency_ms": 2000,
				"call_count":       8,
			},
		},
	})

	r, w, _ := os.Pipe()
	old := os.Stdout
	os.Stdout = w

	err := HealthCmd([]string{}, false)
	w.Close()
	os.Stdout = old

	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	output := string(buf[:n])

	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "hackernews") {
		t.Errorf("expected hackernews in output, got: %s", output)
	}
	if !strings.Contains(output, "reddit") {
		t.Errorf("expected reddit in output, got: %s", output)
	}
	if !strings.Contains(output, "healthy") {
		t.Errorf("expected 'healthy' status in output, got: %s", output)
	}
	if !strings.Contains(output, "2 sources tracked") {
		t.Errorf("expected source count in output, got: %s", output)
	}
}

func TestHealthCmd_JSONOutput(t *testing.T) {
	home := setupTempHome(t)

	now := time.Now().UTC()
	writeHealthJSON(t, home, map[string]interface{}{
		"sources": map[string]interface{}{
			"hackernews": map[string]interface{}{
				"last_success":     now,
				"success_count":    5,
				"failure_count":    0,
				"total_latency_ms": 1500,
				"call_count":       5,
			},
		},
	})

	r, w, _ := os.Pipe()
	old := os.Stdout
	os.Stdout = w

	err := HealthCmd([]string{}, true)
	w.Close()
	os.Stdout = old

	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	output := string(buf[:n])

	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	var result map[string]interface{}
	if err := json.Unmarshal([]byte(output), &result); err != nil {
		t.Fatalf("JSON output is not valid JSON: %v\nOutput: %s", err, output)
	}
	if _, ok := result["sources"]; !ok {
		t.Errorf("expected 'sources' field in JSON output, got: %s", output)
	}
}

func TestHealthCmd_Reset(t *testing.T) {
	home := setupTempHome(t)

	now := time.Now().UTC()
	writeHealthJSON(t, home, map[string]interface{}{
		"sources": map[string]interface{}{
			"hackernews": map[string]interface{}{
				"last_success":  now,
				"success_count": 5,
			},
			"reddit": map[string]interface{}{
				"last_success":  now,
				"success_count": 3,
			},
		},
	})

	r, w, _ := os.Pipe()
	old := os.Stdout
	os.Stdout = w

	err := HealthCmd([]string{"--reset", "hackernews"}, false)
	w.Close()
	os.Stdout = old

	buf := make([]byte, 4096)
	n, _ := r.Read(buf)
	output := string(buf[:n])

	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(output, "hackernews") {
		t.Errorf("expected confirmation message, got: %s", output)
	}

	// Verify the health file no longer contains hackernews
	data, err := os.ReadFile(healthFilePath(home))
	if err != nil {
		t.Fatalf("read health file: %v", err)
	}
	if strings.Contains(string(data), "hackernews") {
		t.Error("expected hackernews to be removed from health file")
	}
	if !strings.Contains(string(data), "reddit") {
		t.Error("expected reddit to remain in health file")
	}
}

func TestHealthCmd_UnknownFlag(t *testing.T) {
	setupTempHome(t)
	err := HealthCmd([]string{"--bogus"}, false)
	if err == nil {
		t.Fatal("expected error for unknown flag")
	}
	if !strings.Contains(err.Error(), "--bogus") {
		t.Errorf("error should mention the flag, got: %v", err)
	}
}

func TestHealthCmd_ResetMissingArg(t *testing.T) {
	setupTempHome(t)
	err := HealthCmd([]string{"--reset"}, false)
	if err == nil {
		t.Fatal("expected error when --reset has no argument")
	}
}

// ─── formatTimeAgo ───────────────────────────────────────────────────────────

func TestFormatTimeAgo(t *testing.T) {
	tests := []struct {
		d    time.Duration
		want string
	}{
		{30 * time.Second, "just now"},
		{5 * time.Minute, "5m ago"},
		{3 * time.Hour, "3h ago"},
		{48 * time.Hour, "2d ago"},
	}
	for _, tc := range tests {
		t.Run(tc.want, func(t *testing.T) {
			ts := time.Now().Add(-tc.d)
			got := formatTimeAgo(&ts)
			if got != tc.want {
				t.Errorf("formatTimeAgo(%v) = %q, want %q", tc.d, got, tc.want)
			}
		})
	}
}

func TestFormatTimeAgo_Nil(t *testing.T) {
	if got := formatTimeAgo(nil); got != "—" {
		t.Errorf("formatTimeAgo(nil) = %q, want %q", got, "—")
	}
}
