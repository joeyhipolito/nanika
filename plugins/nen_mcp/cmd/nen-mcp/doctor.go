package main

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// doctorCheck represents a single backing-store or config health check.
type doctorCheck struct {
	Name   string `json:"name"`
	Path   string `json:"path"`
	OK     bool   `json:"ok"`
	Detail string `json:"detail,omitempty"`
}

// doctorResult is the full doctor output.
type doctorResult struct {
	Version  string        `json:"version"`
	Protocol string        `json:"protocol"`
	Checks   []doctorCheck `json:"checks"`
	Tools    []toolSummary `json:"tools"`
	Healthy  bool          `json:"healthy"`
}

// toolSummary is a compact view of a tool for doctor output.
type toolSummary struct {
	Name        string `json:"name"`
	Description string `json:"description"`
}

// runDoctor performs all health checks and prints the result.
// jsonMode=true emits JSON; otherwise human-readable.
func runDoctor(jsonMode bool) {
	cfgDir := orchestratorConfigDir()
	home, _ := os.UserHomeDir()

	type spec struct {
		name string
		path string
	}
	specs := []spec{
		{"learnings.db", filepath.Join(cfgDir, "learnings.db")},
		{"metrics.db", filepath.Join(cfgDir, "metrics.db")},
		{"nen/findings.db", filepath.Join(cfgDir, "nen", "findings.db")},
		{"nen/proposals.db", filepath.Join(cfgDir, "nen", "proposals.db")},
		{"ko-history.db", filepath.Join(cfgDir, "ko-history.db")},
		{"scheduler.db", schedulerDBPath()},
		{"tracker.db", trackerDBPath()},
		{"events/", filepath.Join(cfgDir, "events")},
		{"~/.claude/settings.json", filepath.Join(home, ".claude", "settings.json")},
	}

	result := doctorResult{
		Version:  serverVersion,
		Protocol: protocolVersion,
		Healthy:  true,
	}

	for _, s := range specs {
		check := doctorCheck{Name: s.name, Path: s.path}

		info, err := os.Stat(s.path)
		if err != nil {
			if os.IsNotExist(err) {
				check.Detail = "not found"
			} else {
				check.Detail = err.Error()
			}
			check.OK = false
			result.Healthy = false
			result.Checks = append(result.Checks, check)
			continue
		}

		if filepath.Ext(s.path) == ".db" {
			db, err := openReadOnly(s.path)
			if err != nil {
				check.OK = false
				check.Detail = err.Error()
				result.Healthy = false
				result.Checks = append(result.Checks, check)
				continue
			}
			var dummy int
			if serr := db.QueryRow("SELECT 1").Scan(&dummy); serr != nil {
				check.OK = false
				check.Detail = "ping failed: " + serr.Error()
				result.Healthy = false
			} else {
				check.OK = true
				check.Detail = fmt.Sprintf("readable, %d bytes", info.Size())
			}
			db.Close()
		} else if info.IsDir() {
			check.OK = true
			check.Detail = "directory present"
		} else {
			check.OK = true
			check.Detail = fmt.Sprintf("%d bytes", info.Size())
		}

		result.Checks = append(result.Checks, check)
	}

	// Collect tool summaries from the canonical listTools().
	for _, t := range listTools().Tools {
		result.Tools = append(result.Tools, toolSummary{
			Name:        t.Name,
			Description: t.Description,
		})
	}

	if jsonMode {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		_ = enc.Encode(result)
		return
	}

	// Human-readable output.
	fmt.Printf("nen-mcp v%s  (MCP protocol %s)\n\n", result.Version, result.Protocol)
	fmt.Println("Backing stores:")
	for _, c := range result.Checks {
		mark := "✓"
		if !c.OK {
			mark = "✗"
		}
		fmt.Printf("  %s  %-32s  %s\n", mark, c.Name, c.Detail)
	}

	fmt.Printf("\nTools (%d):\n", len(result.Tools))
	for _, t := range result.Tools {
		fmt.Printf("  • %-30s  %s\n", t.Name, t.Description)
	}

	fmt.Println()
	if result.Healthy {
		fmt.Println("Status: healthy")
	} else {
		fmt.Fprintln(os.Stderr, "Status: degraded (some backing stores unreachable)")
		os.Exit(1)
	}
}
