package cmd

import (
	"flag"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-engage/internal/queue"
)

// ReviewCmd lists pending drafts with context preview.
func ReviewCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("review", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	stateFlag := fs.String("state", "pending", "filter by state: pending, approved, rejected, posted, all")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: engage review [options]

List drafts pending review with context preview.

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	store, err := queue.NewStore(queue.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening queue: %w", err)
	}

	var filterState queue.State
	if *stateFlag != "all" {
		filterState = queue.State(*stateFlag)
	}

	drafts, err := store.List(filterState)
	if err != nil {
		return fmt.Errorf("listing drafts: %w", err)
	}

	if len(drafts) == 0 {
		if *stateFlag == "all" {
			fmt.Println("No drafts in queue.")
		} else {
			fmt.Printf("No %s drafts.\n", *stateFlag)
		}
		return nil
	}

	for _, d := range drafts {
		printDraftPreview(d)
	}

	fmt.Printf("\n%d draft(s). Use 'engage approve <id>' or 'engage reject <id>'.\n", len(drafts))
	return nil
}

// ApproveCmd transitions a draft to approved state.
func ApproveCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("approve", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: engage approve <draft-id>\n")
	}
	if err := fs.Parse(args); err != nil {
		return err
	}
	if fs.NArg() < 1 {
		return fmt.Errorf("usage: engage approve <draft-id>")
	}

	store, err := queue.NewStore(queue.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening queue: %w", err)
	}

	id := fs.Arg(0)
	d, err := store.Transition(id, queue.StateApproved, "")
	if err != nil {
		return err
	}

	fmt.Printf("approved: %s [%s]\n", d.ID, d.Platform)
	fmt.Printf("comment:\n%s\n", d.Comment)
	return nil
}

// RejectCmd transitions a draft to rejected state, optionally with a note.
func RejectCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("reject", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	note := fs.String("note", "", "reason for rejection")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: engage reject <draft-id> [--note <reason>]\n")
	}
	if err := fs.Parse(args); err != nil {
		return err
	}
	if fs.NArg() < 1 {
		return fmt.Errorf("usage: engage reject <draft-id>")
	}

	store, err := queue.NewStore(queue.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening queue: %w", err)
	}

	id := fs.Arg(0)
	d, err := store.Transition(id, queue.StateRejected, *note)
	if err != nil {
		return err
	}

	fmt.Printf("rejected: %s [%s]\n", d.ID, d.Platform)
	if d.Note != "" {
		fmt.Printf("note: %s\n", d.Note)
	}
	return nil
}

// printDraftPreview prints a human-readable preview of a draft for review.
func printDraftPreview(d *queue.Draft) {
	sep := strings.Repeat("─", 72)
	fmt.Println(sep)
	fmt.Printf("ID:       %s\n", d.ID)
	fmt.Printf("State:    %s\n", d.State)
	fmt.Printf("Platform: %s\n", d.Platform)
	fmt.Printf("Created:  %s\n", d.CreatedAt.Format("2006-01-02 15:04:05"))
	if d.Persona != "" {
		fmt.Printf("Persona:  %s\n", d.Persona)
	}

	opp := d.Opportunity
	if opp.Title != "" {
		fmt.Printf("\nPost: %s\n", truncateTitle(opp.Title, 80))
	}
	if opp.Author != "" {
		fmt.Printf("By:   %s\n", opp.Author)
	}
	if opp.URL != "" {
		fmt.Printf("URL:  %s\n", opp.URL)
	}
	if opp.Metrics.Likes > 0 || opp.Metrics.Comments > 0 {
		fmt.Printf("Engagement: %d likes, %d comments\n", opp.Metrics.Likes, opp.Metrics.Comments)
	}

	// Show top 3 existing comments so reviewer can judge originality.
	if len(opp.Comments) > 0 {
		top := opp.Comments
		if len(top) > 3 {
			top = top[:3]
		}
		fmt.Println("\nTop comments (context):")
		for _, c := range top {
			txt := c.Text
			if len([]rune(txt)) > 120 {
				txt = string([]rune(txt)[:120]) + "..."
			}
			if c.Author != "" {
				fmt.Printf("  [%s] %s\n", c.Author, txt)
			} else {
				fmt.Printf("  - %s\n", txt)
			}
		}
	}

	fmt.Printf("\nDraft comment:\n%s\n", d.Comment)

	if d.Note != "" {
		fmt.Printf("\nNote: %s\n", d.Note)
	}
}
