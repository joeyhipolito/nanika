package cmd

import (
	"fmt"
	"strings"

	"github.com/joeyhipolito/nanika-obsidian/internal/ingest"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
)

// AutoCaptureOptions holds flags for the auto-capture command.
type AutoCaptureOptions struct {
	Since      string
	DryRun     bool
	JSONOutput bool
}

// AutoCaptureOutput is the JSON output for the auto-capture command.
type AutoCaptureOutput struct {
	Sources []IngestOutput     `json:"sources"`
	Total   AutoCaptureSummary `json:"total"`
	DryRun  bool               `json:"dry_run,omitempty"`
}

// AutoCaptureSummary holds aggregate counts across all capture sources.
type AutoCaptureSummary struct {
	Created int `json:"created"`
	Skipped int `json:"skipped"`
	Errors  int `json:"errors"`
}

// AutoCaptureCmd captures learnings, workspace artifacts, and scout intel into the vault.
// Errors from individual sources are non-fatal: the command continues with remaining
// sources and reports failures in the output.
func AutoCaptureCmd(vaultPath string, opts AutoCaptureOptions) error {
	since, err := parseSinceDuration(opts.Since)
	if err != nil {
		return fmt.Errorf("invalid --since value %q: %w", opts.Since, err)
	}

	state, err := ingest.LoadState()
	if err != nil {
		return fmt.Errorf("cannot load ingest state: %w", err)
	}

	var sources []IngestOutput

	// Source 1: learnings from ~/.alluka/learnings.db
	lr, lErr := ingest.IngestLearnings(vaultPath, ingest.LearningsIngestOptions{
		Since:  since,
		DryRun: opts.DryRun,
	}, state)
	if lErr != nil {
		sources = append(sources, IngestOutput{Source: "learnings", Errors: []string{lErr.Error()}})
	} else {
		sources = append(sources, IngestOutput{
			Source:  "learnings",
			Created: lr.Created,
			Skipped: lr.Skipped,
			Errors:  lr.Errors,
		})
	}

	// Source 2: workspace artifacts from ~/.alluka/workspaces/
	wr, wErr := ingest.IngestWorkspaces(vaultPath, ingest.WorkspaceIngestOptions{
		Since:  since,
		DryRun: opts.DryRun,
	}, state)
	if wErr != nil {
		sources = append(sources, IngestOutput{Source: "workspaces", Errors: []string{wErr.Error()}})
	} else {
		sources = append(sources, IngestOutput{
			Source:  "workspaces",
			Created: wr.Created,
			Skipped: wr.Skipped,
			Errors:  wr.Errors,
		})
	}

	// Source 3: scout intel from ~/.scout/intel/
	sr, sErr := ingest.IngestScout(vaultPath, ingest.ScoutIngestOptions{
		Since:  since,
		DryRun: opts.DryRun,
	}, state)
	if sErr != nil {
		sources = append(sources, IngestOutput{Source: "scout", Errors: []string{sErr.Error()}})
	} else {
		sources = append(sources, IngestOutput{
			Source:  "scout",
			Created: sr.Created,
			Skipped: sr.Skipped,
			Errors:  sr.Errors,
		})
	}

	// Save state (skip on dry run — nothing was written)
	if !opts.DryRun {
		if err := state.Save(); err != nil {
			fmt.Printf("Warning: cannot save capture state: %v\n", err)
		}
	}

	// Aggregate totals across all sources
	var total AutoCaptureSummary
	for _, s := range sources {
		total.Created += len(s.Created)
		total.Skipped += len(s.Skipped)
		total.Errors += len(s.Errors)
	}

	if opts.JSONOutput {
		return output.JSON(AutoCaptureOutput{
			Sources: sources,
			Total:   total,
			DryRun:  opts.DryRun,
		})
	}

	printAutoCaptureReport(sources, opts.DryRun)
	return nil
}

func printAutoCaptureReport(sources []IngestOutput, dryRun bool) {
	header := "Auto-Capture: Via Workflows"
	if dryRun {
		header += " (dry run)"
	}
	fmt.Println(header)
	fmt.Println(strings.Repeat("=", len(header)))

	var totalCreated, totalSkipped, totalErrors int

	for _, s := range sources {
		fmt.Printf("\n[%s]\n", s.Source)

		if len(s.Created) > 0 {
			if dryRun {
				fmt.Printf("  Would create (%d):\n", len(s.Created))
			} else {
				fmt.Printf("  Created (%d):\n", len(s.Created))
			}
			for _, p := range s.Created {
				fmt.Printf("    + %s\n", p)
			}
		}

		if len(s.Skipped) > 0 {
			fmt.Printf("  Skipped: %d already captured\n", len(s.Skipped))
		}

		if len(s.Errors) > 0 {
			fmt.Printf("  Errors (%d):\n", len(s.Errors))
			for _, e := range s.Errors {
				fmt.Printf("    ! %s\n", e)
			}
		}

		if len(s.Created) == 0 && len(s.Skipped) == 0 && len(s.Errors) == 0 {
			fmt.Printf("  No new items\n")
		}

		totalCreated += len(s.Created)
		totalSkipped += len(s.Skipped)
		totalErrors += len(s.Errors)
	}

	fmt.Printf("\nSummary: %d created, %d skipped, %d errors\n", totalCreated, totalSkipped, totalErrors)
}
