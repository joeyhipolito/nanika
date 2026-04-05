package preflight

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

const (
	nenBlockTitle    = "Nen Daemon"
	nenStaleThreshold = 10 * time.Minute
)

func init() {
	Register(&nenSection{})
}

// nenSection surfaces nen-daemon health: observer counts, uptime, and a
// stale-observer warning when last_event_at is older than nenStaleThreshold.
type nenSection struct{}

func (n *nenSection) Name() string  { return "nen" }
func (n *nenSection) Priority() int { return 15 }

// nenStats mirrors the structure of ~/.alluka/nen/nen-daemon.stats.json.
type nenStats struct {
	StartedAt   time.Time                 `json:"started_at"`
	TotalEvents int64                     `json:"total_events"`
	LastEventAt time.Time                 `json:"last_event_at"`
	Scanners    map[string]nenScannerStat `json:"scanners"`
}

type nenScannerStat struct {
	EventsRouted int64 `json:"events_routed"`
	ErrorCount   int64 `json:"error_count"`
}

func (n *nenSection) Fetch(_ context.Context) (Block, error) {
	statsPath := nenStatsPath()

	data, err := os.ReadFile(statsPath)
	if errors.Is(err, os.ErrNotExist) {
		// Daemon not running or never started — normal on fresh installs.
		return Block{Title: nenBlockTitle}, nil
	}
	if err != nil {
		return Block{}, fmt.Errorf("reading nen stats: %w", err)
	}

	var stats nenStats
	if err := json.Unmarshal(data, &stats); err != nil {
		return Block{}, fmt.Errorf("parsing nen stats: %w", err)
	}

	return Block{
		Title: nenBlockTitle,
		Body:  formatNenBlock(&stats, time.Now()),
	}, nil
}

// formatNenBlock renders the nen stats block. now is injected to avoid a hard
// clock dependency in tests.
func formatNenBlock(stats *nenStats, now time.Time) string {
	var sb strings.Builder

	if !stats.StartedAt.IsZero() {
		uptime := now.Sub(stats.StartedAt).Round(time.Second)
		fmt.Fprintf(&sb, "uptime: %s\n", formatDuration(uptime))
	}

	if len(stats.Scanners) > 0 {
		type entry struct {
			name string
			stat nenScannerStat
		}
		entries := make([]entry, 0, len(stats.Scanners))
		for name, s := range stats.Scanners {
			entries = append(entries, entry{name, s})
		}
		sort.Slice(entries, func(i, j int) bool { return entries[i].name < entries[j].name })

		parts := make([]string, len(entries))
		for i, e := range entries {
			if e.stat.ErrorCount > 0 {
				parts[i] = fmt.Sprintf("%s(%d, %d err)", e.name, e.stat.EventsRouted, e.stat.ErrorCount)
			} else {
				parts[i] = fmt.Sprintf("%s(%d)", e.name, e.stat.EventsRouted)
			}
		}
		fmt.Fprintf(&sb, "observers: %s\n", strings.Join(parts, ", "))
	}

	fmt.Fprintf(&sb, "total_events: %d\n", stats.TotalEvents)

	if !stats.LastEventAt.IsZero() {
		idle := now.Sub(stats.LastEventAt).Round(time.Second)
		if idle > nenStaleThreshold {
			fmt.Fprintf(&sb, "WARNING: no events for %s (last: %s)\n",
				formatDuration(idle),
				stats.LastEventAt.UTC().Format(time.RFC3339),
			)
		} else {
			fmt.Fprintf(&sb, "last_event: %s ago\n", formatDuration(idle))
		}
	}

	return strings.TrimRight(sb.String(), "\n")
}

func formatDuration(d time.Duration) string {
	if d >= time.Hour {
		h := int(d.Hours())
		m := int(d.Minutes()) % 60
		if m == 0 {
			return fmt.Sprintf("%dh", h)
		}
		return fmt.Sprintf("%dh%dm", h, m)
	}
	if d >= time.Minute {
		m := int(d.Minutes())
		s := int(d.Seconds()) % 60
		if s == 0 {
			return fmt.Sprintf("%dm", m)
		}
		return fmt.Sprintf("%dm%ds", m, s)
	}
	return fmt.Sprintf("%ds", int(d.Seconds()))
}

// nenStatsPath returns the path to nen-daemon.stats.json.
// Resolution order:
//  1. NEN_STATS env var (test override)
//  2. $ALLUKA_HOME/nen/nen-daemon.stats.json
//  3. ~/.alluka/nen/nen-daemon.stats.json (default)
func nenStatsPath() string {
	if v := os.Getenv("NEN_STATS"); v != "" {
		return v
	}
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return filepath.Join(v, "nen", "nen-daemon.stats.json")
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "nen", "nen-daemon.stats.json")
}
