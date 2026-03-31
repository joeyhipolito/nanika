package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/health"
)

// HealthCmd shows per-source gather health status.
func HealthCmd(args []string, jsonOutput bool) error {
	var resetSource string
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--reset":
			if i+1 >= len(args) {
				return fmt.Errorf("--reset requires a source name")
			}
			i++
			resetSource = args[i]
		default:
			return fmt.Errorf("unknown flag: %s", args[i])
		}
	}

	store, err := health.Load(config.HealthFile())
	if err != nil {
		return fmt.Errorf("failed to load health data: %w", err)
	}

	if resetSource != "" {
		delete(store.Sources, resetSource)
		if err := store.Save(); err != nil {
			return fmt.Errorf("failed to save health data: %w", err)
		}
		if !jsonOutput {
			fmt.Printf("Reset health data for source: %s\n", resetSource)
		}
		return nil
	}

	if jsonOutput {
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(store)
	}

	if len(store.Sources) == 0 {
		fmt.Println("No health data found. Run 'scout gather' to collect metrics.")
		return nil
	}

	sources := make([]string, 0, len(store.Sources))
	for src := range store.Sources {
		sources = append(sources, src)
	}
	sort.Strings(sources)

	fmt.Printf("Source health (%d sources tracked)\n\n", len(sources))
	fmt.Printf("%-14s  %-8s  %-18s  %-18s  %9s  %8s  %11s\n",
		"SOURCE", "STATUS", "LAST SUCCESS", "LAST FAILURE", "SUCCESSES", "FAILURES", "AVG LATENCY")
	fmt.Printf("%-14s  %-8s  %-18s  %-18s  %9s  %8s  %11s\n",
		"──────────────", "────────", "──────────────────", "──────────────────",
		"─────────", "────────", "───────────")

	for _, src := range sources {
		h := store.Sources[src]
		avgLatency := "—"
		if h.CallCount > 0 {
			ms := h.AvgLatencyMs()
			if ms >= 1000 {
				avgLatency = fmt.Sprintf("%.1fs", ms/1000)
			} else {
				avgLatency = fmt.Sprintf("%.0fms", ms)
			}
		}
		fmt.Printf("%-14s  %-8s  %-18s  %-18s  %9d  %8d  %11s\n",
			src, h.Status(),
			formatTimeAgo(h.LastSuccess), formatTimeAgo(h.LastFailure),
			h.SuccessCount, h.FailureCount, avgLatency)
	}

	if !store.UpdatedAt.IsZero() {
		fmt.Printf("\nLast updated: %s\n", store.UpdatedAt.Local().Format("2006-01-02 15:04:05"))
	}
	return nil
}

// formatTimeAgo returns a compact relative time string, or "—" for nil.
func formatTimeAgo(t *time.Time) string {
	if t == nil {
		return "—"
	}
	d := time.Since(*t)
	switch {
	case d < time.Minute:
		return "just now"
	case d < time.Hour:
		return fmt.Sprintf("%dm ago", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%dh ago", int(d.Hours()))
	default:
		return fmt.Sprintf("%dd ago", int(d.Hours()/24))
	}
}
