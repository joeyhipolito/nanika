package cmd

import (
	"fmt"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/ingest"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
)

// IngestOutput is the combined JSON output for the ingest command.
type IngestOutput struct {
	Source  string   `json:"source"`
	Created []string `json:"created"`
	Skipped []string `json:"skipped"`
	Errors  []string `json:"errors"`
}

// IngestOptions holds flags for the ingest command.
type IngestOptions struct {
	Source     string
	Topic      string
	Domain     string
	Since      string
	DryRun     bool
	JSONOutput bool
}

// IngestCmd imports data from an external source into the Obsidian vault.
func IngestCmd(vaultPath string, opts IngestOptions) error {
	if opts.Source == "" {
		return fmt.Errorf("--source is required\n\nUsage: obsidian ingest --source scout|learnings")
	}

	// Parse --since duration
	since, err := parseSinceDuration(opts.Since)
	if err != nil {
		return fmt.Errorf("invalid --since value %q: %w", opts.Since, err)
	}

	// Load ingest state
	state, err := ingest.LoadState()
	if err != nil {
		return fmt.Errorf("cannot load ingest state: %w", err)
	}

	var result IngestOutput
	result.Source = opts.Source

	switch opts.Source {
	case "scout":
		scoutResult, err := ingest.IngestScout(vaultPath, ingest.ScoutIngestOptions{
			Topic:  opts.Topic,
			Since:  since,
			DryRun: opts.DryRun,
		}, state)
		if err != nil {
			return err
		}
		result.Created = scoutResult.Created
		result.Skipped = scoutResult.Skipped
		result.Errors = scoutResult.Errors

	case "learnings":
		learningsResult, err := ingest.IngestLearnings(vaultPath, ingest.LearningsIngestOptions{
			Domain: opts.Domain,
			Since:  since,
			DryRun: opts.DryRun,
		}, state)
		if err != nil {
			return err
		}
		result.Created = learningsResult.Created
		result.Skipped = learningsResult.Skipped
		result.Errors = learningsResult.Errors

	default:
		return fmt.Errorf("unknown source %q\n\nValid sources: scout, learnings", opts.Source)
	}

	// Save state (skip if dry run — nothing was actually written)
	if !opts.DryRun {
		if err := state.Save(); err != nil {
			// Non-fatal: warn but don't fail
			fmt.Printf("Warning: cannot save ingest state: %v\n", err)
		}
	}

	if opts.JSONOutput {
		return output.JSON(result)
	}

	printIngestReport(result, opts.DryRun)
	return nil
}

// parseSinceDuration converts strings like "7d", "24h", "30d" into time.Duration.
// Returns 0 if the input is empty.
func parseSinceDuration(s string) (time.Duration, error) {
	if s == "" {
		return 0, nil
	}

	s = strings.TrimSpace(s)
	if len(s) < 2 {
		return 0, fmt.Errorf("expected format like '7d' or '24h'")
	}

	unit := s[len(s)-1]
	valueStr := s[:len(s)-1]

	// Parse numeric value
	value := 0
	for _, c := range valueStr {
		if c < '0' || c > '9' {
			return 0, fmt.Errorf("expected format like '7d' or '24h'")
		}
		value = value*10 + int(c-'0')
	}

	switch unit {
	case 'h':
		return time.Duration(value) * time.Hour, nil
	case 'd':
		return time.Duration(value) * 24 * time.Hour, nil
	case 'w':
		return time.Duration(value) * 7 * 24 * time.Hour, nil
	default:
		return 0, fmt.Errorf("unknown unit %q — use h (hours), d (days), or w (weeks)", string(unit))
	}
}

// printIngestReport prints a human-readable ingest summary.
func printIngestReport(result IngestOutput, dryRun bool) {
	header := fmt.Sprintf("Ingest: %s", result.Source)
	if dryRun {
		header += " (dry run)"
	}
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))

	if len(result.Created) > 0 {
		if dryRun {
			fmt.Printf("\nWould create (%d):\n", len(result.Created))
		} else {
			fmt.Printf("\nCreated (%d):\n", len(result.Created))
		}
		for _, p := range result.Created {
			fmt.Printf("  + %s\n", p)
		}
	}

	if len(result.Skipped) > 0 {
		fmt.Printf("\nSkipped (%d already ingested)\n", len(result.Skipped))
	}

	if len(result.Errors) > 0 {
		fmt.Printf("\nErrors (%d):\n", len(result.Errors))
		for _, e := range result.Errors {
			fmt.Printf("  ! %s\n", e)
		}
	}

	fmt.Printf("\nSummary: %d created, %d skipped, %d errors\n",
		len(result.Created), len(result.Skipped), len(result.Errors))
}
