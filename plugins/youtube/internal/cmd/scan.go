package cmd

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-youtube/internal/api"
)

// ScanCmd scans configured channels and topics for recent videos.
func ScanCmd(args []string, jsonOutput bool) error {
	fs := flag.NewFlagSet("scan", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	limit := fs.Int("limit", 20, "max candidates to return")
	topics := fs.String("topics", "", "comma-separated topic search queries")
	sinceStr := fs.String("since", "24h", "only return videos newer than this duration (e.g. 24h, 7d)")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: youtube scan [options]

Scan configured channels and topics for recent videos.

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	since, err := parseDuration(*sinceStr)
	if err != nil {
		return fmt.Errorf("parsing --since: %w", err)
	}

	client, err := api.NewClient()
	if err != nil {
		return fmt.Errorf("loading youtube config: %w", err)
	}

	opts := api.ScanOpts{
		Limit: *limit,
		Since: time.Now().Add(-since),
	}
	if *topics != "" {
		for _, t := range strings.Split(*topics, ",") {
			if t = strings.TrimSpace(t); t != "" {
				opts.Topics = append(opts.Topics, t)
			}
		}
	}

	ctx := context.Background()
	candidates, err := client.Scan(ctx, opts)
	if err != nil {
		return fmt.Errorf("scanning youtube: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(candidates)
	}

	if len(candidates) == 0 {
		fmt.Println("No videos found.")
		return nil
	}
	for _, c := range candidates {
		fmt.Printf("[%s] %s\n  %s\n  Channel: %s  Published: %s\n\n",
			c.Platform, c.Title, c.URL, c.Author, c.CreatedAt.Format(time.RFC3339))
	}
	fmt.Printf("Quota used this run: %d units\n", client.RunUnits())
	return nil
}

// parseDuration extends time.ParseDuration to support "7d", "30d" shorthand.
func parseDuration(s string) (time.Duration, error) {
	if strings.HasSuffix(s, "d") {
		days := strings.TrimSuffix(s, "d")
		var n int
		if _, err := fmt.Sscanf(days, "%d", &n); err != nil {
			return 0, fmt.Errorf("invalid day duration: %s", s)
		}
		return time.Duration(n) * 24 * time.Hour, nil
	}
	return time.ParseDuration(s)
}
