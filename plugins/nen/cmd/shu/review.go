package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"strings"
	"time"
)

func runReview(args []string) error {
	fs := flag.NewFlagSet("review", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	jsonOut := fs.Bool("json", false, "Output as JSON")
	approveID := fs.String("approve", "", "Approve a proposal by tracker issue ID (open → in-progress)")
	approveReason := fs.String("reason", "", "Reason for approval (optional, used in comment)")
	if err := fs.Parse(args); err != nil {
		return err
	}

	if *approveID != "" {
		return runReviewApprove(*approveID, *approveReason)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	issues, err := getTrackerIssues(ctx)
	if err != nil {
		return fmt.Errorf("querying tracker: %w", err)
	}

	var pending []trackerIssue
	for _, issue := range issues {
		if issue.Status == "open" && issue.hasLabel("auto") {
			pending = append(pending, issue)
		}
	}

	if *jsonOut {
		if pending == nil {
			pending = []trackerIssue{}
		}
		return encodeJSON(struct {
			Pending []trackerIssue `json:"pending"`
			Count   int            `json:"count"`
		}{pending, len(pending)})
	}

	if len(pending) == 0 {
		fmt.Println("No pending proposals.")
		return nil
	}

	fmt.Printf("Pending proposals (%d):\n\n", len(pending))
	for _, issue := range pending {
		id := issue.displayID()
		priority := "(none)"
		if issue.Priority != nil {
			priority = *issue.Priority
		}
		fmt.Printf("  %s (%s) — %s\n", id, priority, issue.Title)
		if issue.Labels != nil {
			fmt.Printf("    Labels:  %s\n", *issue.Labels)
		}
		fmt.Printf("    Created: %s\n", issue.CreatedAt)
		fmt.Printf("    Approve: shu review --approve %s\n", id)
		fmt.Printf("    Reject:  tracker update %s --status cancelled\n\n", id)
	}

	return nil
}

// runReviewApprove flips a tracker issue from open → in-progress. Idempotent:
// if the issue is already in-progress, exits 0 silently.
func runReviewApprove(issueID, reason string) error {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	issues, err := getTrackerIssues(ctx)
	if err != nil {
		return fmt.Errorf("querying tracker: %w", err)
	}

	var found *trackerIssue
	for i := range issues {
		if issues[i].displayID() == issueID || issues[i].ID == issueID {
			found = &issues[i]
			break
		}
	}
	if found == nil {
		return fmt.Errorf("tracker issue %q not found", issueID)
	}

	if !found.hasLabel("auto") {
		return fmt.Errorf("tracker issue %q is not a nen-authored proposal (missing 'auto' label)", issueID)
	}

	if found.Status == "in-progress" {
		// Already approved — idempotent success.
		return nil
	}

	out, err := exec.Command("tracker", "update", issueID, "--status", "in-progress").CombinedOutput()
	if err != nil {
		return fmt.Errorf("tracker update %s --status in-progress: %s: %w",
			issueID, strings.TrimSpace(string(out)), err)
	}
	fmt.Printf("Approved %s → in-progress\n", issueID)

	comment := "approved by shu review --approve"
	if reason != "" {
		comment = reason
	}
	_ = exec.Command("tracker", "comment", issueID, comment).Run()
	return nil
}
