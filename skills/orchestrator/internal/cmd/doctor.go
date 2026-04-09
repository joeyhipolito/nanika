package cmd

import (
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/spf13/cobra"

	_ "modernc.org/sqlite"
)

// doctorCheck holds the result of a single doctor check.
type doctorCheck struct {
	Check   string `json:"check"`
	Status  string `json:"status"` // "ok" or "fail"
	Message string `json:"message"`
}

func init() {
	doctorCmd := &cobra.Command{
		Use:   "doctor",
		Short: "Run environment health checks",
		Long: `Check that the orchestrator's runtime dependencies are available and healthy.

Checks:
  1. runtime     — claude CLI on PATH (reports version)
  2. event-bus   — nen-daemon running (reports PID and last event)
  3. learning-db — learnings.db exists and responds (reports row count)
  4. personas    — persona files present in ~/nanika/personas/ (reports count)
  5. plugins     — shu, ko, nen-daemon, tracker present in ~/.alluka/bin/`,
		RunE: runDoctor,
	}
	doctorCmd.Flags().Bool("json", false, "output results as JSON array")

	rootCmd.AddCommand(doctorCmd)
}

func runDoctor(cmd *cobra.Command, args []string) error {
	jsonOut, _ := cmd.Flags().GetBool("json")

	ctx, cancel := context.WithTimeout(context.Background(), 500*time.Millisecond)
	defer cancel()

	checks := []doctorCheck{
		checkRuntime(ctx),
		checkEventBus(),
		checkLearningDB(),
		checkPersonas(),
		checkPlugins(),
	}

	if jsonOut {
		enc := json.NewEncoder(cmd.OutOrStdout())
		enc.SetIndent("", "  ")
		return enc.Encode(checks)
	}

	for _, c := range checks {
		prefix := "✓"
		if c.Status != "ok" {
			prefix = "✗"
		}
		fmt.Fprintf(cmd.OutOrStdout(), "%s %s: %s\n", prefix, c.Check, c.Message)
	}
	return nil
}

// checkRuntime verifies the claude CLI is on PATH and reports its version.
func checkRuntime(ctx context.Context) doctorCheck {
	path, err := exec.LookPath("claude")
	if err != nil {
		return doctorCheck{"runtime", "fail", "claude not found on PATH"}
	}

	var out bytes.Buffer
	c := exec.CommandContext(ctx, path, "--version")
	c.Stdout = &out
	c.Stderr = &out
	if err := c.Run(); err != nil {
		return doctorCheck{"runtime", "fail", fmt.Sprintf("claude found at %s but --version failed: %v", path, err)}
	}
	version := strings.TrimSpace(out.String())
	if version == "" {
		version = path
	}
	return doctorCheck{"runtime", "ok", version}
}

// checkEventBus verifies nen-daemon is running and reports its last event time.
func checkEventBus() doctorCheck {
	statsPath := nenDoctorStatsPath()
	data, err := os.ReadFile(statsPath)
	if os.IsNotExist(err) {
		return doctorCheck{"event-bus", "fail", "nen-daemon stats not found — daemon may not be running"}
	}
	if err != nil {
		return doctorCheck{"event-bus", "fail", fmt.Sprintf("reading nen stats: %v", err)}
	}

	var stats struct {
		StartedAt   time.Time `json:"started_at"`
		TotalEvents int64     `json:"total_events"`
		LastEventAt time.Time `json:"last_event_at"`
	}
	if err := json.Unmarshal(data, &stats); err != nil {
		return doctorCheck{"event-bus", "fail", fmt.Sprintf("parsing nen stats: %v", err)}
	}

	// Detect a stale file — if it hasn't been updated in 30 minutes the daemon
	// has likely crashed or been stopped.
	fi, _ := os.Stat(statsPath)
	if fi != nil && time.Since(fi.ModTime()) > 30*time.Minute {
		return doctorCheck{"event-bus", "fail", "nen-daemon stats file is stale (>30m old) — daemon may not be running"}
	}

	var lastEvt string
	if !stats.LastEventAt.IsZero() {
		ago := time.Since(stats.LastEventAt).Round(time.Second)
		lastEvt = fmt.Sprintf("%s ago", ago)
	} else {
		lastEvt = "no events recorded"
	}
	return doctorCheck{
		"event-bus", "ok",
		fmt.Sprintf("running, %d total events, last event %s", stats.TotalEvents, lastEvt),
	}
}

// checkLearningDB opens the learnings database and reports the row count.
func checkLearningDB() doctorCheck {
	dbPath := learningDBDoctorPath()
	if _, err := os.Stat(dbPath); os.IsNotExist(err) {
		return doctorCheck{"learning-db", "fail", fmt.Sprintf("learnings.db not found at %s", dbPath)}
	}

	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		return doctorCheck{"learning-db", "fail", fmt.Sprintf("open: %v", err)}
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 400*time.Millisecond)
	defer cancel()

	var count int
	if err := db.QueryRowContext(ctx, "SELECT COUNT(*) FROM learnings").Scan(&count); err != nil {
		return doctorCheck{"learning-db", "fail", fmt.Sprintf("query: %v", err)}
	}
	return doctorCheck{"learning-db", "ok", fmt.Sprintf("%d rows in learnings.db", count)}
}

// checkPersonas counts persona .md files in ~/nanika/personas/.
func checkPersonas() doctorCheck {
	dir := personasDoctorDir()
	entries, err := os.ReadDir(dir)
	if os.IsNotExist(err) {
		return doctorCheck{"personas", "fail", fmt.Sprintf("personas directory not found: %s", dir)}
	}
	if err != nil {
		return doctorCheck{"personas", "fail", fmt.Sprintf("reading %s: %v", dir, err)}
	}

	count := 0
	for _, e := range entries {
		if !e.IsDir() && strings.HasSuffix(e.Name(), ".md") {
			count++
		}
	}
	if count == 0 {
		return doctorCheck{"personas", "fail", fmt.Sprintf("no .md files in %s", dir)}
	}
	return doctorCheck{"personas", "ok", fmt.Sprintf("%d persona files in %s", count, dir)}
}

// checkPlugins spot-checks that critical plugin binaries exist in ~/.alluka/bin/.
func checkPlugins() doctorCheck {
	binDir := pluginsBinDoctorDir()
	required := []string{"shu", "ko", "nen-daemon", "tracker"}
	var missing []string
	for _, name := range required {
		p := filepath.Join(binDir, name)
		if _, err := os.Stat(p); os.IsNotExist(err) {
			missing = append(missing, name)
		}
	}
	if len(missing) > 0 {
		return doctorCheck{
			"plugins", "fail",
			fmt.Sprintf("missing in %s: %s", binDir, strings.Join(missing, ", ")),
		}
	}
	return doctorCheck{
		"plugins", "ok",
		fmt.Sprintf("shu, ko, nen-daemon, tracker found in %s", binDir),
	}
}

// path helpers — separated for testability.

func nenDoctorStatsPath() string {
	if v := os.Getenv("NEN_STATS"); v != "" {
		return v
	}
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return filepath.Join(v, "nen", "nen-daemon.stats.json")
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "nen", "nen-daemon.stats.json")
}

func learningDBDoctorPath() string {
	if v := os.Getenv("ORCHESTRATOR_CONFIG_DIR"); v != "" {
		return filepath.Join(v, "learnings.db")
	}
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return filepath.Join(v, "learnings.db")
	}
	home, _ := os.UserHomeDir()
	alluka := filepath.Join(home, ".alluka")
	if _, err := os.Stat(alluka); err == nil {
		return filepath.Join(alluka, "learnings.db")
	}
	return filepath.Join(home, ".via", "learnings.db")
}

func personasDoctorDir() string {
	if v := os.Getenv("ORCHESTRATOR_PERSONAS_DIR"); v != "" {
		return v
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, "nanika", "personas")
}

func pluginsBinDoctorDir() string {
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return filepath.Join(v, "bin")
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "bin")
}
