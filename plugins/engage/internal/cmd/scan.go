package cmd

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-engage/internal/enrich"
)

// ScanCmd scans all platforms (or a subset) for engagement opportunities.
// Each opportunity is fully enriched (body, comments, transcript) in one pass.
func ScanCmd(args []string, jsonOutput bool) error {
	fs := flag.NewFlagSet("scan", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	platformsFlag := fs.String("platform", "", "comma-separated platforms to scan (youtube,linkedin,reddit,substack,x)")
	limit := fs.Int("limit", 20, "max results per platform")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: engage scan [options]

Scan platforms for engagement opportunities.

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	ctx := context.Background()
	enrichers := selectEnrichers(*platformsFlag)
	if len(enrichers) == 0 {
		return fmt.Errorf("no matching platforms: %s", *platformsFlag)
	}

	var all []enrich.EnrichedOpportunity
	for _, e := range enrichers {
		items, err := e.Scan(ctx, *limit)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: %s scan failed: %v\n", e.Platform(), err)
			continue
		}
		for _, item := range items {
			enriched, err := e.Enrich(ctx, item.ID)
			if err != nil {
				fmt.Fprintf(os.Stderr, "Warning: %s enrich %s failed: %v\n", e.Platform(), item.ID, err)
				all = append(all, item)
				continue
			}
			all = append(all, *enriched)
		}
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(all)
	}

	if len(all) == 0 {
		fmt.Println("No opportunities found.")
		return nil
	}

	for _, opp := range all {
		printEnriched(&opp)
		fmt.Println()
	}
	return nil
}

// selectEnrichers returns enrichers matching the comma-separated platform string.
// An empty string returns all enrichers.
func selectEnrichers(platformsFlag string) []enrich.Enricher {
	if platformsFlag == "" {
		return enrich.All()
	}
	var selected []enrich.Enricher
	for _, name := range strings.Split(platformsFlag, ",") {
		name = strings.TrimSpace(name)
		if e := enrich.ByPlatform(name); e != nil {
			selected = append(selected, e)
		}
	}
	return selected
}

// printEnriched formats and prints a single EnrichedOpportunity to stdout.
func printEnriched(opp *enrich.EnrichedOpportunity) {
	fmt.Printf("[%s] %s\n", opp.Platform, opp.Title)
	fmt.Printf("URL: %s\n", opp.URL)
	if opp.Author != "" {
		fmt.Printf("Author: %s\n", opp.Author)
	}
	if !opp.CreatedAt.IsZero() {
		fmt.Printf("Published: %s\n", opp.CreatedAt.Format("2006-01-02"))
	}
	fmt.Printf("Likes: %d  Comments: %d", opp.Metrics.Likes, opp.Metrics.Comments)
	if opp.Metrics.Reposts > 0 {
		fmt.Printf("  Reposts: %d", opp.Metrics.Reposts)
	}
	fmt.Println()

	if opp.Body != "" {
		body := opp.Body
		if len(body) > 500 {
			body = body[:497] + "..."
		}
		fmt.Printf("\n%s\n", body)
	}

	if opp.Transcript != "" {
		fmt.Printf("\nTranscript (%d chars):\n", len(opp.Transcript))
		excerpt := opp.Transcript
		if len(excerpt) > 400 {
			excerpt = excerpt[:397] + "..."
		}
		fmt.Println(excerpt)
	}

	if len(opp.Comments) > 0 {
		shown := len(opp.Comments)
		if shown > 5 {
			shown = 5
		}
		fmt.Printf("\nTop %d comments:\n", shown)
		for i := 0; i < shown; i++ {
			c := opp.Comments[i]
			fmt.Printf("  %s: %s\n", c.Author, truncate(c.Text, 200))
		}
		if len(opp.Comments) > 5 {
			fmt.Printf("  ... and %d more\n", len(opp.Comments)-5)
		}
	}
}

// truncate shortens s to at most n bytes, appending "..." if truncated.
func truncate(s string, n int) string {
	if len(s) <= n {
		return s
	}
	return s[:n-3] + "..."
}
