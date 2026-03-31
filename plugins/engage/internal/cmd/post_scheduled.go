package cmd

import (
	"context"
	"flag"
	"fmt"
	"os"
	"sort"

	"github.com/joeyhipolito/nanika-engage/internal/history"
	"github.com/joeyhipolito/nanika-engage/internal/queue"
)

// PostScheduledCmd posts up to N oldest approved drafts.
// Exits with code 2 when no approved drafts are found, 0 on success, 1 on error.
func PostScheduledCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("commit", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	count := fs.Int("count", 3, "number of approved drafts to post (oldest first)")
	reschedule := fs.Bool("reschedule", false, "if approved drafts remain after posting, schedule the next commit run for tomorrow")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: engage commit [options]

Post up to N oldest approved drafts via platform CLIs.

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	qstore, err := queue.NewStore(queue.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening queue: %w", err)
	}
	hstore, err := history.NewStore(history.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening history: %w", err)
	}

	ctx := context.Background()

	drafts, err := qstore.List(queue.StateApproved)
	if err != nil {
		return fmt.Errorf("listing approved drafts: %w", err)
	}
	if len(drafts) == 0 {
		fmt.Println("no approved drafts to post")
		os.Exit(2)
	}

	// Sort oldest first so we always post in creation order.
	sort.Slice(drafts, func(i, j int) bool {
		return drafts[i].CreatedAt.Before(drafts[j].CreatedAt)
	})

	batch := drafts
	if len(batch) > *count {
		batch = batch[:*count]
	}

	var posted, failed int
	for _, d := range batch {
		if err := postOne(ctx, qstore, hstore, d.ID, false); err != nil {
			fmt.Fprintf(os.Stderr, "warn: %s: %v\n", d.ID, err)
			failed++
		} else {
			posted++
		}
	}
	fmt.Printf("%d posted, %d failed\n", posted, failed)

	if *reschedule {
		remaining, err := qstore.List(queue.StateApproved)
		if err != nil {
			fmt.Fprintf(os.Stderr, "warn: checking remaining drafts: %v\n", err)
		} else if len(remaining) > 0 {
			fmt.Printf("%d approved draft(s) remain in queue\n", len(remaining))
			if err := scheduleCommitRun(); err != nil {
				fmt.Fprintf(os.Stderr, "warn: reschedule: %v\n", err)
			}
		} else {
			fmt.Println("queue empty — not rescheduling")
		}
	}
	return nil
}
