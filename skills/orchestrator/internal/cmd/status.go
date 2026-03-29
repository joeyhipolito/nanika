package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
)

var projectFromLog = event.ProjectFromLog

func init() {
	statusCmd := &cobra.Command{
		Use:   "status",
		Short: "Show recent workspace status",
		RunE:  showStatus,
	}

	rootCmd.AddCommand(statusCmd)
}

func showStatus(cmd *cobra.Command, args []string) error {
	if err := showRunningMissions(cmd); err != nil {
		fmt.Fprintf(cmd.ErrOrStderr(), "warning: could not scan running missions: %v\n", err)
	}

	workspaces, err := core.ListWorkspaces()
	if err != nil {
		return err
	}

	if len(workspaces) == 0 {
		fmt.Println("no workspaces found")
		return nil
	}

	// Show last 5
	limit := 5
	if len(workspaces) < limit {
		limit = len(workspaces)
	}

	for _, wsPath := range workspaces[:limit] {
		cp, err := core.LoadCheckpoint(wsPath)
		if err != nil {
			continue
		}

		// Read mission
		mission, _ := os.ReadFile(filepath.Join(wsPath, "mission.md"))
		taskSummary := strings.TrimSpace(string(mission))
		if len(taskSummary) > 80 {
			taskSummary = taskSummary[:80] + "..."
		}

		total := len(cp.Plan.Phases)

		// Prefer event-derived status for running missions; fall back to the
		// checkpoint when no event log exists (historical). Manual checkpoint
		// overrides still win for the displayed mission status, but the event
		// log remains the freshest source for completed phase counts.
		status := cp.Status
		completed := 0

		snap, err := projectFromLog(cp.WorkspaceID)
		if err != nil {
			fmt.Fprintf(cmd.ErrOrStderr(), "warning: live status unavailable for %s: %v\n", filepath.Base(wsPath), err)
		} else if snap != nil {
			if cp.Status == "" || cp.Status == "in_progress" {
				status = snap.Status
			}
			for _, ph := range snap.Phases {
				if ph.Status == "completed" {
					completed++
				}
			}
		} else {
			// No event log — fall back to checkpoint-derived phase counts.
			for _, p := range cp.Plan.Phases {
				if p.Status == core.StatusCompleted {
					completed++
				}
			}
		}
		if err != nil {
			for _, p := range cp.Plan.Phases {
				if p.Status == core.StatusCompleted {
					completed++
				}
			}
		}

		// Show issue link when present (from frontmatter sidecar).
		issueTag := ""
		if cp.LinearIssueID != "" {
			issueTag = fmt.Sprintf(" (%s)", cp.LinearIssueID)
		}

		fmt.Fprintf(cmd.OutOrStdout(), "%s [%s] %d/%d phases%s — %s\n",
			filepath.Base(wsPath), status, completed, total, issueTag, taskSummary)

		// Show PR URL when available (written by createMissionPR after PR creation).
		if prURL, err := os.ReadFile(filepath.Join(wsPath, "pr_url")); err == nil {
			if url := strings.TrimSpace(string(prURL)); url != "" {
				fmt.Fprintf(cmd.OutOrStdout(), "  PR: %s\n", url)
			}
		}
	}

	return nil
}

// showRunningMissions scans ~/.alluka/events/ for missions that have
// mission.started but no mission.completed, mission.failed, or
// mission.cancelled, and prints them with start time, current phase, and
// elapsed duration.
func showRunningMissions(cmd *cobra.Command) error {
	dir, err := event.EventLogsDir()
	if err != nil {
		return fmt.Errorf("getting events dir: %w", err)
	}

	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading events dir: %w", err)
	}

	type runningMission struct {
		missionID    string
		startedAt    time.Time
		currentPhase string
	}

	var running []runningMission
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
			continue
		}
		missionID := strings.TrimSuffix(entry.Name(), ".jsonl")
		snap, err := projectFromLog(missionID)
		if err != nil || snap == nil {
			continue
		}
		if snap.Status != "in_progress" {
			continue
		}
		running = append(running, runningMission{
			missionID:    missionID,
			startedAt:    snap.StartedAt,
			currentPhase: currentPhaseName(snap),
		})
	}

	if len(running) == 0 {
		return nil
	}

	sort.Slice(running, func(i, j int) bool {
		return running[i].startedAt.Before(running[j].startedAt)
	})

	fmt.Fprintln(cmd.OutOrStdout(), "Running missions:")
	for _, r := range running {
		elapsed := time.Since(r.startedAt).Truncate(time.Second)
		phase := r.currentPhase
		if phase == "" {
			phase = "(no active phase)"
		}
		fmt.Fprintf(cmd.OutOrStdout(), "  %s  started %s  phase: %s  elapsed: %s\n",
			r.missionID,
			r.startedAt.Local().Format("2006-01-02 15:04:05"),
			phase,
			elapsed,
		)
	}
	fmt.Fprintln(cmd.OutOrStdout())
	return nil
}

// currentPhaseName returns the name of the currently active phase in snap.
// It prefers phases with status "running" or "retrying"; among ties it picks
// the one with the latest StartedAt. Falls back to the most recently started
// phase when none is explicitly running.
func currentPhaseName(snap *event.MissionSnap) string {
	var latest *event.PhaseSnap
	for _, ph := range snap.Phases {
		if ph.Status == "running" || ph.Status == "retrying" {
			if latest == nil || ph.StartedAt.After(latest.StartedAt) {
				latest = ph
			}
		}
	}
	if latest != nil {
		return latest.Name
	}
	// No explicitly running phase — return the most recently started one.
	for _, ph := range snap.Phases {
		if latest == nil || ph.StartedAt.After(latest.StartedAt) {
			latest = ph
		}
	}
	if latest != nil {
		return latest.Name
	}
	return ""
}
