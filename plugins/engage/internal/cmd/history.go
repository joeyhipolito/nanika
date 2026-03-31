package cmd

import (
	"flag"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-engage/internal/history"
)

// HistoryCmd displays posted engagement history.
func HistoryCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("history", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	since := fs.String("since", "", "show entries since this duration ago (e.g. 7d, 24h, 30m)")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: engage history [options]

Show a log of all posted engagements.

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	var sinceTime time.Time
	if *since != "" {
		t, err := parseSince(*since)
		if err != nil {
			return fmt.Errorf("invalid --since %q: %w", *since, err)
		}
		sinceTime = t
	}

	hstore, err := history.NewStore(history.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening history: %w", err)
	}

	records, err := hstore.List(sinceTime)
	if err != nil {
		return fmt.Errorf("listing history: %w", err)
	}

	if len(records) == 0 {
		if *since != "" {
			fmt.Printf("No engagements in the last %s.\n", *since)
		} else {
			fmt.Println("No engagements yet. Run 'engage post' after approving drafts.")
		}
		return nil
	}

	sep := strings.Repeat("─", 72)
	for _, r := range records {
		fmt.Println(sep)
		fmt.Printf("Platform: %s\n", r.Platform)
		fmt.Printf("Posted:   %s\n", r.PostedAt.Format("2006-01-02 15:04:05"))
		if r.PostURL != "" {
			fmt.Printf("URL:      %s\n", r.PostURL)
		}
		if r.Likes > 0 || r.Replies > 0 {
			fmt.Printf("Received: %d likes, %d replies\n", r.Likes, r.Replies)
		}
		comment := r.Comment
		if len([]rune(comment)) > 200 {
			comment = string([]rune(comment)[:200]) + "..."
		}
		fmt.Printf("\n%s\n", comment)
	}
	fmt.Printf("\n%d engagement(s).\n", len(records))
	return nil
}

// parseSince parses a human duration like "7d", "24h", or "30m" into a past time.Time.
// Days ("d") are not supported by Go's time.ParseDuration, so we handle them first.
func parseSince(s string) (time.Time, error) {
	s = strings.ToLower(strings.TrimSpace(s))
	if strings.HasSuffix(s, "d") {
		n, err := strconv.Atoi(strings.TrimSuffix(s, "d"))
		if err != nil || n <= 0 {
			return time.Time{}, fmt.Errorf("expected a positive integer before 'd', got %q", s)
		}
		return time.Now().Add(-time.Duration(n) * 24 * time.Hour), nil
	}
	d, err := time.ParseDuration(s)
	if err != nil {
		return time.Time{}, err
	}
	if d <= 0 {
		return time.Time{}, fmt.Errorf("duration must be positive, got %q", s)
	}
	return time.Now().Add(-d), nil
}
