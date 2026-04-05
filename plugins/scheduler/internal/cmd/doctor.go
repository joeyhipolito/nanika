package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
	"github.com/joeyhipolito/nanika-scheduler/internal/db"
)

// check represents a single doctor validation result.
type check struct {
	Name    string `json:"name"`
	Status  string `json:"status"`  // "ok", "warn", "fail"
	Message string `json:"message"`
}

// doctorOutput is the JSON envelope for --json output.
type doctorOutput struct {
	Checks  []check `json:"checks"`
	Summary string  `json:"summary"`
	AllOK   bool    `json:"all_ok"`
}

func newDoctorCmd() *cobra.Command {
	var jsonFlag bool

	cmd := &cobra.Command{
		Use:   "doctor",
		Short: "Validate installation and configuration",
		Long:  `Run a series of checks to verify that scheduler is set up correctly.`,
		RunE: func(cmd *cobra.Command, args []string) error {
			return runDoctor(jsonFlag)
		},
	}
	cmd.Flags().BoolVar(&jsonFlag, "json", false, "Output results as JSON")
	return cmd
}

func runDoctor(jsonOutput bool) error {
	var checks []check
	allOK := true

	addCheck := func(name, status, message string) {
		checks = append(checks, check{Name: name, Status: status, Message: message})
		if status == "fail" {
			allOK = false
		}
	}

	// 1. Binary in PATH
	if binaryPath, err := exec.LookPath("scheduler"); err != nil {
		addCheck("Binary", "warn", "scheduler not found in PATH (running from local build?)")
	} else {
		addCheck("Binary", "ok", binaryPath)
	}

	// 2. Config dir exists
	configDir := config.Dir()
	if info, err := os.Stat(configDir); err != nil {
		addCheck("Config dir", "warn", fmt.Sprintf("%s not found — run 'scheduler configure'", configDir))
	} else if !info.IsDir() {
		addCheck("Config dir", "fail", fmt.Sprintf("%s exists but is not a directory", configDir))
	} else {
		addCheck("Config dir", "ok", configDir)
	}

	// 3. Config file exists and is readable
	configPath := config.Path()
	if !config.Exists() {
		addCheck("Config file", "warn", fmt.Sprintf("%s not found — run 'scheduler configure'", configPath))
	} else {
		addCheck("Config file", "ok", configPath)

		// 4. Config file permissions (should be 0600)
		perms, err := config.Permissions()
		if err != nil {
			addCheck("Config permissions", "fail", fmt.Sprintf("cannot read permissions: %v", err))
		} else if perms != 0600 {
			addCheck("Config permissions", "warn",
				fmt.Sprintf("%04o (should be 0600) — fix: chmod 600 %s", perms, configPath))
		} else {
			addCheck("Config permissions", "ok", "0600")
		}

		// 5. Config is parseable
		cfg, err := config.Load()
		if err != nil {
			addCheck("Config parse", "fail", fmt.Sprintf("parse error: %v", err))
		} else {
			addCheck("Config parse", "ok", "valid")

			// 6. Shell exists and is executable
			if cfg.Shell == "" {
				addCheck("Shell", "warn", "shell is empty; defaulting to /bin/sh")
			} else if _, err := os.Stat(cfg.Shell); err != nil {
				addCheck("Shell", "fail", fmt.Sprintf("%s not found: %v", cfg.Shell, err))
			} else {
				addCheck("Shell", "ok", cfg.Shell)
			}

			// 7. DB directory is writable
			dbPath := cfg.DBPath
			if dbPath == "" {
				addCheck("Database", "warn", "db_path not set in config")
			} else {
				if err := checkDBConnectivity(dbPath); err != nil {
					addCheck("Database", "fail", fmt.Sprintf("cannot open %s: %v", dbPath, err))
				} else {
					addCheck("Database", "ok", dbPath)
				}
			}
		}
	}

	// 8. Go version (informational)
	if goPath, err := exec.LookPath("go"); err != nil {
		addCheck("Go toolchain", "warn", "go not found in PATH (not required at runtime)")
	} else {
		addCheck("Go toolchain", "ok", goPath)
	}

	// 9–11. Social CLIs in PATH
	for _, cli := range []string{"substack", "linkedin", "reddit"} {
		if cliPath, err := exec.LookPath(cli); err != nil {
			addCheck(cli+" CLI", "warn", fmt.Sprintf("%s not found in PATH — posts to %s will fail", cli, cli))
		} else {
			addCheck(cli+" CLI", "ok", cliPath)
		}
	}

	// 12. Daemon PID file status
	pidPath := filepath.Join(config.Dir(), "daemon.pid")
	if pidBytes, err := os.ReadFile(pidPath); err != nil {
		addCheck("Daemon", "ok", "not running (no PID file)")
	} else {
		pid, _ := strconv.Atoi(strings.TrimSpace(string(pidBytes)))
		if pid > 0 && processAlive(pid) {
			addCheck("Daemon", "ok", fmt.Sprintf("running (PID %d)", pid))
		} else {
			addCheck("Daemon", "warn", fmt.Sprintf("stale PID file (PID %d) — process not running; remove %s", pid, pidPath))
		}
	}

	// 13. DB schema version (requires DB to be reachable)
	if config.Exists() {
		if cfg, err := config.Load(); err == nil && cfg.DBPath != "" {
			if version, err := checkDBSchemaVersion(cfg.DBPath); err != nil {
				addCheck("DB schema", "warn", fmt.Sprintf("cannot check schema: %v", err))
			} else {
				addCheck("DB schema", "ok", fmt.Sprintf("v%d (posts table %s)", version,
					map[bool]string{true: "present", false: "missing"}[version >= 2]))
			}
		}
	}

	// 14. Multiple scheduler.db files
	if name, msg, status := checkMultipleDBs(); status != "" {
		addCheck(name, status, msg)
	}

	// --- Output ---
	if jsonOutput {
		counts := map[string]int{"ok": 0, "warn": 0, "fail": 0}
		for _, c := range checks {
			counts[c.Status]++
		}
		summary := fmt.Sprintf("%d ok, %d warn, %d fail",
			counts["ok"], counts["warn"], counts["fail"])

		out := doctorOutput{
			Checks:  checks,
			Summary: summary,
			AllOK:   allOK,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	// Human-readable output
	icons := map[string]string{
		"ok":   "✓",
		"warn": "⚠",
		"fail": "✗",
	}
	for _, c := range checks {
		icon := icons[c.Status]
		fmt.Printf("  %s  %-22s %s\n", icon, c.Name, c.Message)
	}

	fmt.Println()
	counts := map[string]int{"ok": 0, "warn": 0, "fail": 0}
	for _, c := range checks {
		counts[c.Status]++
	}
	fmt.Printf("  %d ok, %d warn, %d fail\n", counts["ok"], counts["warn"], counts["fail"])

	if !allOK {
		fmt.Println()
		fmt.Println("  Fix the failures above before using scheduler.")
		return fmt.Errorf("doctor: %d check(s) failed", counts["fail"])
	}
	return nil
}

// checkDBConnectivity opens the SQLite database and pings it.
func checkDBConnectivity(path string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	d, err := db.Open(path)
	if err != nil {
		return err
	}
	defer d.Close()

	return d.Ping(ctx)
}

// checkDBSchemaVersion opens the DB and returns the inferred schema version.
func checkDBSchemaVersion(path string) (int, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()

	d, err := db.Open(path)
	if err != nil {
		return 0, err
	}
	defer d.Close()

	return d.SchemaVersion(ctx)
}

// checkMultipleDBs scans the two known non-zero scheduler.db locations and
// returns a warning if more than one contains data. Zero-byte files are skipped
// (they are orphaned placeholders, not live databases).
// Returns empty strings when nothing to report.
func checkMultipleDBs() (name, message, status string) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", "", ""
	}

	// ~/.alluka/scheduler.db was a zero-byte orphan and has been removed.
	// Only the legacy path and the canonical default are checked here.
	candidates := []string{
		filepath.Join(home, ".scheduler", "scheduler.db"),
		filepath.Join(home, ".alluka", "scheduler", "scheduler.db"),
	}

	type dbInfo struct {
		path    string
		size    int64
		modTime time.Time
	}

	var found []dbInfo
	for _, p := range candidates {
		info, err := os.Stat(p)
		if err != nil || info.Size() == 0 {
			continue // skip missing or zero-byte orphans
		}
		found = append(found, dbInfo{path: p, size: info.Size(), modTime: info.ModTime()})
	}

	if len(found) <= 1 {
		return "", "", ""
	}

	var sb strings.Builder
	fmt.Fprintf(&sb, "%d scheduler.db files found — canonical default is ~/.alluka/scheduler/scheduler.db.\n", len(found))
	fmt.Fprintf(&sb, "    Active path is set by db_path in ~/.alluka/scheduler/config (run 'scheduler configure show'):\n")
	for _, f := range found {
		fmt.Fprintf(&sb, "    %s  (%d bytes, modified %s)",
			f.path, f.size, f.modTime.Format("2006-01-02 15:04:05"))
	}
	return "Multiple DBs", sb.String(), "warn"
}
