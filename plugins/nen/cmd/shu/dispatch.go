package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

// dispatchLimits controls the throttle for concurrent and per-hour dispatches.
type dispatchLimits struct {
	MaxConcurrent int
	MaxPerHour    int
}

func defaultDispatchLimits() dispatchLimits {
	return dispatchLimits{MaxConcurrent: 1, MaxPerHour: 6}
}

// throttleDecision is returned by checkThrottle.
type throttleDecision int

const (
	throttleAllow           throttleDecision = iota
	throttleDeferConcurrent                  // at or over max-concurrent limit
	throttleDeferRate                        // at or over max-per-hour limit
)

// dispatchRow mirrors a row in the dispatches table.
type dispatchRow struct {
	ID          int64
	IssueID     string
	MissionFile string
	WorkspaceID string
	StartedAt   time.Time
	FinishedAt  *time.Time
	Outcome     string
}

// checkThrottle inspects the dispatches table and returns whether a new dispatch
// may start right now. Does not record anything — caller must call
// recordDispatchStart on throttleAllow.
func checkThrottle(db *sql.DB, limits dispatchLimits, now time.Time) (throttleDecision, error) {
	// Count active (un-finished) dispatches. Rows past the 6 h watchdog are
	// excluded because recoverCrashedDispatches should have already cleaned them.
	var active int
	if err := db.QueryRow(`
		SELECT COUNT(*) FROM dispatches
		WHERE finished_at IS NULL`,
	).Scan(&active); err != nil {
		return throttleAllow, fmt.Errorf("counting active dispatches: %w", err)
	}
	if active >= limits.MaxConcurrent {
		return throttleDeferConcurrent, nil
	}

	// Count dispatches started within the rolling 1-hour window.
	windowStart := now.Add(-1 * time.Hour).UTC().Format(time.RFC3339)
	var recent int
	if err := db.QueryRow(`
		SELECT COUNT(*) FROM dispatches
		WHERE started_at >= ?`,
		windowStart,
	).Scan(&recent); err != nil {
		return throttleAllow, fmt.Errorf("counting recent dispatches: %w", err)
	}
	if recent >= limits.MaxPerHour {
		return throttleDeferRate, nil
	}

	return throttleAllow, nil
}

// recoverCrashedDispatches marks any dispatch row that started more than 6 h
// ago and never finished as crashed. Call at the start of runDispatch to
// prevent a crash from permanently inflating the concurrent count.
func recoverCrashedDispatches(db *sql.DB, now time.Time) error {
	cutoff := now.Add(-6 * time.Hour).UTC().Format(time.RFC3339)
	nowStr := now.UTC().Format(time.RFC3339)
	_, err := db.Exec(`
		UPDATE dispatches
		SET finished_at = ?, outcome = 'crashed'
		WHERE finished_at IS NULL AND started_at < ?`,
		nowStr, cutoff)
	if err != nil {
		return fmt.Errorf("recovering crashed dispatches: %w", err)
	}
	return nil
}

// recordDispatchStart inserts a new row with started_at=now, finished_at=NULL.
// Returns the row ID for later update via recordDispatchFinish.
func recordDispatchStart(db *sql.DB, issueID, missionFile string, now time.Time) (int64, error) {
	result, err := db.Exec(`
		INSERT INTO dispatches (issue_id, mission_file, started_at)
		VALUES (?, ?, ?)`,
		issueID, missionFile, now.UTC().Format(time.RFC3339))
	if err != nil {
		return 0, fmt.Errorf("recording dispatch start: %w", err)
	}
	id, err := result.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("getting dispatch row id: %w", err)
	}
	return id, nil
}

// recordDispatchFinish updates finished_at, outcome, and workspace_id on the
// given dispatch row.
func recordDispatchFinish(db *sql.DB, rowID int64, outcome, workspaceID string, now time.Time) error {
	_, err := db.Exec(`
		UPDATE dispatches
		SET finished_at = ?, outcome = ?, workspace_id = ?
		WHERE id = ?`,
		now.UTC().Format(time.RFC3339), outcome, workspaceID, rowID)
	if err != nil {
		return fmt.Errorf("recording dispatch finish for row %d: %w", rowID, err)
	}
	return nil
}

// selectNextDispatchable returns the oldest in-progress+auto tracker issue
// that has a readable mission file. Returns nil, "", nil if none.
func selectNextDispatchable(ctx context.Context) (*trackerIssue, string, error) {
	issues, err := getTrackerIssues(ctx)
	if err != nil {
		return nil, "", fmt.Errorf("querying tracker: %w", err)
	}

	home, _ := os.UserHomeDir()
	remDir := filepath.Join(home, ".alluka", "missions", "remediation")

	for _, issue := range issues {
		if issue.Status != "in-progress" || !issue.hasLabel("auto") {
			continue
		}
		// Find a mission file that references this issue ID.
		missionFile, err := findMissionForIssue(remDir, issue.displayID())
		if err != nil || missionFile == "" {
			continue
		}
		// Verify it's readable.
		if _, err := os.Stat(missionFile); err != nil {
			continue
		}
		issueCopy := issue
		return &issueCopy, missionFile, nil
	}
	return nil, "", nil
}

// findMissionForIssue scans the remediation mission directory for a file whose
// tracker_issue frontmatter field matches the given issue ID.
func findMissionForIssue(dir, issueID string) (string, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		if os.IsNotExist(err) {
			return "", nil
		}
		return "", fmt.Errorf("reading remediation dir: %w", err)
	}
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".md") {
			continue
		}
		path := filepath.Join(dir, entry.Name())
		meta, err := parseRemediationMissionMeta(path)
		if err != nil {
			continue
		}
		if meta.TrackerIssue == issueID {
			return path, nil
		}
	}
	return "", nil
}

// extractWorkspaceID reads the workspace ID from orchestrator stdout, which
// typically prints "workspace: <id>" or "started workspace <id>".
func extractWorkspaceID(output string) string {
	for _, line := range strings.Split(output, "\n") {
		line = strings.TrimSpace(line)
		for _, prefix := range []string{"workspace:", "workspace_id:", "started workspace"} {
			if strings.HasPrefix(strings.ToLower(line), prefix) {
				parts := strings.Fields(line)
				if len(parts) >= 2 {
					return parts[len(parts)-1]
				}
			}
		}
	}
	return ""
}

// dispatchResult is the structured output of a dispatch run.
type dispatchResult struct {
	Action      string `json:"action"`
	IssueID     string `json:"issue_id,omitempty"`
	MissionFile string `json:"mission_file,omitempty"`
	WorkspaceID string `json:"workspace_id,omitempty"`
	Outcome     string `json:"outcome,omitempty"`
	Reason      string `json:"reason,omitempty"`
}

// runDispatch is the full dispatch pipeline:
// recover crashed → check throttle → select issue → record start →
// orchestrator run → shu close or revert → record finish.
func runDispatch(args []string) error {
	fs := flag.NewFlagSet("dispatch", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	maxConcurrent := fs.Int("max-concurrent", defaultDispatchLimits().MaxConcurrent, "Maximum simultaneously running dispatches")
	maxPerHour := fs.Int("max-per-hour", defaultDispatchLimits().MaxPerHour, "Maximum dispatches started in rolling 1h window")
	dryRun := fs.Bool("dry-run", false, "Print what would be dispatched; do not run")
	jsonOut := fs.Bool("json", false, "Structured JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}

	limits := dispatchLimits{MaxConcurrent: *maxConcurrent, MaxPerHour: *maxPerHour}

	db, err := openProposalsDB()
	if err != nil {
		return fmt.Errorf("opening proposals.db: %w", err)
	}
	defer db.Close()

	now := time.Now().UTC()

	if err := recoverCrashedDispatches(db, now); err != nil {
		// Non-fatal — log and continue.
		fmt.Fprintf(os.Stderr, "warning: %v\n", err)
	}

	decision, err := checkThrottle(db, limits, now)
	if err != nil {
		return fmt.Errorf("checking throttle: %w", err)
	}

	if decision == throttleDeferConcurrent {
		return printDispatchResult(*jsonOut, dispatchResult{
			Action: "throttled",
			Reason: fmt.Sprintf("concurrent limit reached (%d)", limits.MaxConcurrent),
		})
	}
	if decision == throttleDeferRate {
		return printDispatchResult(*jsonOut, dispatchResult{
			Action: "throttled",
			Reason: fmt.Sprintf("rate limit reached (%d/hour)", limits.MaxPerHour),
		})
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	issue, missionFile, err := selectNextDispatchable(ctx)
	if err != nil {
		return fmt.Errorf("selecting dispatchable issue: %w", err)
	}
	if issue == nil {
		return printDispatchResult(*jsonOut, dispatchResult{
			Action: "no-op",
			Reason: "no eligible in-progress+auto issues with mission files",
		})
	}

	if *dryRun {
		return printDispatchResult(*jsonOut, dispatchResult{
			Action:      "dry-run",
			IssueID:     issue.displayID(),
			MissionFile: missionFile,
		})
	}

	rowID, err := recordDispatchStart(db, issue.displayID(), missionFile, now)
	if err != nil {
		return fmt.Errorf("recording dispatch start: %w", err)
	}

	workspaceID, runErr := runOrchestrator(missionFile)
	if runErr != nil {
		// Revert tracker issue to open so it can be retried.
		_ = runTrackerUpdate(issue.displayID(), "open")
		finishTime := time.Now().UTC()
		_ = recordDispatchFinish(db, rowID, "failure", workspaceID, finishTime)
		return printDispatchResult(*jsonOut, dispatchResult{
			Action:      "failure",
			IssueID:     issue.displayID(),
			MissionFile: missionFile,
			WorkspaceID: workspaceID,
			Outcome:     "failure",
			Reason:      runErr.Error(),
		})
	}

	// Parse mission finding IDs for shu close.
	meta, _ := parseRemediationMissionMeta(missionFile)
	findingIDsStr := strings.Join(meta.FindingIDs, ",")

	if findingIDsStr != "" && meta.TrackerIssue != "" {
		closeArgs := []string{
			"--tracker-issue", meta.TrackerIssue,
			"--finding-ids", findingIDsStr,
		}
		if workspaceID != "" {
			closeArgs = append(closeArgs, "--workspace", workspaceID)
		}
		if err := runClose(closeArgs); err != nil {
			// Non-fatal: the mission succeeded but close failed. Log and continue.
			fmt.Fprintf(os.Stderr, "warning: shu close failed after dispatch: %v\n", err)
		}
	}

	finishTime := time.Now().UTC()
	if err := recordDispatchFinish(db, rowID, "success", workspaceID, finishTime); err != nil {
		fmt.Fprintf(os.Stderr, "warning: recording dispatch finish: %v\n", err)
	}

	return printDispatchResult(*jsonOut, dispatchResult{
		Action:      "dispatched",
		IssueID:     issue.displayID(),
		MissionFile: missionFile,
		WorkspaceID: workspaceID,
		Outcome:     "success",
	})
}

// runOrchestrator invokes `orchestrator run <missionFile>` and returns the
// workspace ID extracted from its stdout. Returns an error if the process exits
// non-zero.
func runOrchestrator(missionFile string) (string, error) {
	cmd := exec.Command("orchestrator", "run", missionFile)
	out, err := cmd.Output()
	outStr := string(out)
	workspaceID := extractWorkspaceID(outStr)
	if err != nil {
		return workspaceID, fmt.Errorf("orchestrator run %s: %w", missionFile, err)
	}
	return workspaceID, nil
}

// runTrackerUpdate calls `tracker update <issueID> --status <status>`.
func runTrackerUpdate(issueID, status string) error {
	out, err := exec.Command("tracker", "update", issueID, "--status", status).CombinedOutput()
	if err != nil {
		return fmt.Errorf("tracker update %s --status %s: %s: %w", issueID, status, strings.TrimSpace(string(out)), err)
	}
	return nil
}

// printDispatchResult prints r as text or JSON depending on jsonOut.
func printDispatchResult(jsonOut bool, r dispatchResult) error {
	if jsonOut {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(r)
	}
	switch r.Action {
	case "throttled":
		fmt.Printf("dispatch: throttled — %s\n", r.Reason)
	case "no-op":
		fmt.Printf("dispatch: no-op — %s\n", r.Reason)
	case "dry-run":
		fmt.Printf("dispatch: dry-run — would dispatch %s (%s)\n", r.IssueID, r.MissionFile)
	case "dispatched":
		ws := r.WorkspaceID
		if ws == "" {
			ws = "(unknown)"
		}
		fmt.Printf("dispatch: success — %s workspace=%s\n", r.IssueID, ws)
	case "failure":
		fmt.Printf("dispatch: failure — %s: %s\n", r.IssueID, r.Reason)
	}
	return nil
}
