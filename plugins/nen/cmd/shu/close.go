package main

import (
	"context"
	"database/sql"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

func runClose(args []string) error {
	fs := flag.NewFlagSet("close", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	trackerIssueFlag := fs.String("tracker-issue", "", "Tracker issue ID to close (required unless --sweep)")
	findingIDsFlag := fs.String("finding-ids", "", "Comma-separated finding IDs to supersede (required unless --sweep)")
	workspaceFlag := fs.String("workspace", "", "Workspace ID that resolved the findings")
	sweepFlag := fs.Bool("sweep", false, "Sweep remediation mission files and reconcile resolved tracker issues")
	dryRunFlag := fs.Bool("dry-run", false, "Print decisions without mutating tracker or findings (--sweep only)")
	jsonFlag := fs.Bool("json", false, "Structured JSON output (--sweep only)")
	if err := fs.Parse(args); err != nil {
		return err
	}

	if *sweepFlag {
		home, _ := os.UserHomeDir()
		opts := sweepOptions{
			MissionDir:       filepath.Join(home, ".alluka", "missions", "remediation"),
			DryRun:           *dryRunFlag,
			ConservativeMode: true,
		}
		ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
		defer cancel()
		report, err := runCloseSweep(ctx, opts)
		if err != nil {
			return fmt.Errorf("close sweep: %w", err)
		}
		if *jsonFlag {
			return encodeJSON(report)
		}
		fmt.Printf("swept %d mission files: %d already done, %d swept to done, %d skipped, %d invalid\n",
			report.Total, report.AlreadyDone, report.SweptToDone, report.Skipped, report.Invalid)
		for _, e := range report.Errors {
			fmt.Fprintf(os.Stderr, "sweep error: %s\n", e)
		}
		return nil
	}

	if *trackerIssueFlag == "" {
		return fmt.Errorf("--tracker-issue is required")
	}
	if *findingIDsFlag == "" {
		return fmt.Errorf("--finding-ids is required")
	}

	ids := strings.Split(*findingIDsFlag, ",")
	for i := range ids {
		ids[i] = strings.TrimSpace(ids[i])
	}

	supersededBy := "mission:unknown"
	if *workspaceFlag != "" {
		supersededBy = "mission:" + *workspaceFlag
	}

	// Supersede findings in findings.db — refuse if nothing matched.
	affected, err := supersedeFindings(ids, supersededBy)
	if err != nil {
		return fmt.Errorf("superseding findings: %w", err)
	}
	if affected == 0 {
		return fmt.Errorf("no findings matched the provided IDs (given: %s); tracker issue NOT updated",
			strings.Join(ids, ", "))
	}
	if affected < int64(len(ids)) {
		fmt.Fprintf(os.Stderr, "warning: only %d of %d finding IDs matched\n", affected, len(ids))
	}
	fmt.Printf("Superseded %d finding(s) with %s\n", affected, supersededBy)

	// Update tracker issue to done.
	if out, err := exec.Command("tracker", "update", *trackerIssueFlag,
		"--status", "done",
	).CombinedOutput(); err != nil {
		return fmt.Errorf("tracker update: %s: %w", strings.TrimSpace(string(out)), err)
	}
	fmt.Printf("Tracker issue %s → done\n", *trackerIssueFlag)

	// Add resolution comment (non-fatal).
	comment := fmt.Sprintf("Resolved by %s", supersededBy)
	if out, err := exec.Command("tracker", "comment", *trackerIssueFlag, comment,
	).CombinedOutput(); err != nil {
		fmt.Fprintf(os.Stderr, "warning: failed to add comment: %s\n", strings.TrimSpace(string(out)))
	}

	return nil
}

// supersedeFindings updates the superseded_by field for the given finding IDs.
// Returns the number of rows actually updated and any DB error.
func supersedeFindings(ids []string, supersededBy string) (int64, error) {
	path := findingsDBPath()
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return 0, fmt.Errorf("findings.db not found at %s", path)
	}

	db, err := sql.Open("sqlite", "file:"+path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return 0, fmt.Errorf("open findings.db: %w", err)
	}
	defer db.Close()

	placeholders := make([]string, len(ids))
	args := make([]any, 0, len(ids)+1)
	args = append(args, supersededBy)
	for i, id := range ids {
		placeholders[i] = "?"
		args = append(args, id)
	}

	query := fmt.Sprintf(
		"UPDATE findings SET superseded_by = ? WHERE id IN (%s)",
		strings.Join(placeholders, ","),
	)
	result, err := db.Exec(query, args...)
	if err != nil {
		return 0, fmt.Errorf("updating findings: %w", err)
	}
	affected, _ := result.RowsAffected()
	return affected, nil
}
