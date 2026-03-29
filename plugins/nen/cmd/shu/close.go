package main

import (
	"database/sql"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"strings"
)

func runClose(args []string) error {
	fs := flag.NewFlagSet("close", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	trackerIssueFlag := fs.String("tracker-issue", "", "Tracker issue ID to close (required)")
	findingIDsFlag := fs.String("finding-ids", "", "Comma-separated finding IDs to supersede (required)")
	workspaceFlag := fs.String("workspace", "", "Workspace ID that resolved the findings")
	if err := fs.Parse(args); err != nil {
		return err
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

	// Supersede findings in findings.db
	if err := supersedeFindings(ids, supersededBy); err != nil {
		return fmt.Errorf("superseding findings: %w", err)
	}
	fmt.Printf("Superseded %d finding(s) with %s\n", len(ids), supersededBy)

	// Update tracker issue to done
	if out, err := exec.Command("tracker", "update", *trackerIssueFlag,
		"--status", "done",
	).CombinedOutput(); err != nil {
		return fmt.Errorf("tracker update: %s: %w", strings.TrimSpace(string(out)), err)
	}
	fmt.Printf("Tracker issue %s → done\n", *trackerIssueFlag)

	// Add resolution comment (non-fatal)
	comment := fmt.Sprintf("Resolved by %s", supersededBy)
	if out, err := exec.Command("tracker", "comment", *trackerIssueFlag, comment,
	).CombinedOutput(); err != nil {
		fmt.Fprintf(os.Stderr, "warning: failed to add comment: %s\n", strings.TrimSpace(string(out)))
	}

	return nil
}

func supersedeFindings(ids []string, supersededBy string) error {
	path := findingsDBPath()
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return fmt.Errorf("findings.db not found at %s", path)
	}

	db, err := sql.Open("sqlite", "file:"+path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return fmt.Errorf("open findings.db: %w", err)
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
		return fmt.Errorf("updating findings: %w", err)
	}
	affected, _ := result.RowsAffected()
	if affected == 0 {
		fmt.Fprintf(os.Stderr, "warning: no findings matched the provided IDs\n")
	}
	return nil
}
