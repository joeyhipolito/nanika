package cmd

import (
	"context"
	"flag"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-engage/internal/history"
	"github.com/joeyhipolito/nanika-engage/internal/post"
	"github.com/joeyhipolito/nanika-engage/internal/queue"
)

// PostCmd posts approved drafts via platform CLIs.
// With no arguments, posts all approved drafts.
// With a draft ID argument, posts that specific draft.
func PostCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("post", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	dryRun := fs.Bool("dry-run", false, "print what would be posted without sending")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: engage post [options] [draft-id]

Post approved drafts via platform CLIs.
Without a draft-id, posts all approved drafts in the queue.

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

	if fs.NArg() >= 1 {
		return postOne(ctx, qstore, hstore, fs.Arg(0), *dryRun)
	}

	drafts, err := qstore.List(queue.StateApproved)
	if err != nil {
		return fmt.Errorf("listing approved drafts: %w", err)
	}
	if len(drafts) == 0 {
		fmt.Println("No approved drafts to post.")
		return nil
	}

	var posted, failed int
	for _, d := range drafts {
		if err := postOne(ctx, qstore, hstore, d.ID, *dryRun); err != nil {
			fmt.Fprintf(os.Stderr, "warn: %s: %v\n", d.ID, err)
			failed++
		} else {
			posted++
		}
	}
	fmt.Printf("\n%d posted, %d failed.\n", posted, failed)
	return nil
}

func postOne(ctx context.Context, qstore *queue.Store, hstore *history.Store, id string, dryRun bool) error {
	d, err := qstore.Load(id)
	if err != nil {
		return err
	}
	if d.State != queue.StateApproved {
		return fmt.Errorf("draft %s is %s, not approved", id, d.State)
	}

	if dryRun {
		fmt.Printf("[dry-run] %s [%s]\n  URL: %s\n\n%s\n", d.ID, d.Platform, d.Opportunity.URL, d.Comment)
		return nil
	}

	if _, err := post.Post(ctx, d); err != nil {
		return err
	}

	// Transition queue item to posted — carries the PostedAt timestamp.
	updated, transErr := qstore.Transition(id, queue.StatePosted, "")
	if transErr != nil {
		fmt.Fprintf(os.Stderr, "warn: updating queue state for %s: %v\n", id, transErr)
	}

	// Record the engagement in history.
	rec := &history.Record{
		ID:       history.GenerateID(d.Platform, d.ID),
		Platform: d.Platform,
		PostURL:  d.Opportunity.URL,
		Comment:  d.Comment,
	}
	if updated != nil && updated.PostedAt != nil {
		rec.PostedAt = *updated.PostedAt
	} else {
		// Fallback: use current time if the transition didn't set it.
		rec.PostedAt = d.CreatedAt
	}

	if saveErr := hstore.Save(rec); saveErr != nil {
		fmt.Fprintf(os.Stderr, "warn: saving history for %s: %v\n", id, saveErr)
	}

	fmt.Printf("posted: %s [%s]\n  URL: %s\n", d.ID, d.Platform, d.Opportunity.URL)
	return nil
}
