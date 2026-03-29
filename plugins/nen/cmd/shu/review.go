package main

import (
	"context"
	"flag"
	"fmt"
	"os"
	"time"
)

func runReview(args []string) error {
	fs := flag.NewFlagSet("review", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	jsonOut := fs.Bool("json", false, "Output as JSON")
	if err := fs.Parse(args); err != nil {
		return err
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
		fmt.Printf("    Approve: tracker update %s --status in-progress\n", id)
		fmt.Printf("    Reject:  tracker update %s --status cancelled\n\n", id)
	}

	return nil
}
