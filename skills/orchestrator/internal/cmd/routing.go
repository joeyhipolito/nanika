package cmd

import (
	"context"
	"fmt"
	"os"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/audit"
	"github.com/joeyhipolito/orchestrator-cli/internal/routing"
)

func init() {
	routingCmd := &cobra.Command{
		Use:   "routing",
		Short: "Manage routing memory (target profiles and patterns)",
	}

	seedCmd := &cobra.Command{
		Use:   "seed",
		Short: "Seed initial target profiles into the routing database",
		Long: `Inserts a minimal set of known target profiles into the routing
database. Existing profiles for the same target are overwritten.

Currently seeds only the orchestrator repo target with Go language
markers and preferred personas (senior-backend-engineer,
staff-code-reviewer, security-auditor).`,
		RunE: runRoutingSeed,
	}

	correctCmd := &cobra.Command{
		Use:   "correct",
		Short: "Record a manual routing correction",
		Long: `Records an explicit correction that a persona assignment was wrong.
The correction is stored in the routing database and used to bias
future persona selection away from the assigned persona toward the
ideal one for the given target.

Examples:
  orchestrator routing correct --target repo:~/myapp --assigned senior-frontend-engineer --ideal senior-backend-engineer
  orchestrator routing correct --target repo:~/myapp --assigned senior-frontend-engineer --ideal senior-backend-engineer --task-hint "database migration"`,
		RunE: runRoutingCorrect,
	}
	correctCmd.Flags().String("target", "", "target ID (e.g. repo:~/myapp)")
	correctCmd.Flags().String("assigned", "", "persona that was incorrectly assigned")
	correctCmd.Flags().String("ideal", "", "persona that should have been assigned")
	correctCmd.Flags().String("task-hint", "", "optional task descriptor for context")
	correctCmd.MarkFlagRequired("target")
	correctCmd.MarkFlagRequired("assigned")
	correctCmd.MarkFlagRequired("ideal")

	correctionsCmd := &cobra.Command{
		Use:   "corrections [target-id]",
		Short: "List routing corrections for a target",
		Long: `Displays all recorded routing corrections for the given target,
newest first. If --target is omitted, uses the current git repo.

Examples:
  orchestrator routing corrections                          # corrections for current repo
  orchestrator routing corrections repo:~/skills/orchestrator  # by explicit target ID`,
		Args: cobra.MaximumNArgs(1),
		RunE: runRoutingCorrections,
	}

	ingestAuditCmd := &cobra.Command{
		Use:   "ingest-audit",
		Short: "Re-ingest routing data from saved audit reports",
		Long: `Reads saved audit reports from ~/.alluka/audits.jsonl and extracts
routing corrections, decomposition findings, and decomposition
examples into the routing database.

This is normally unnecessary — 'orchestrator audit' auto-ingests
after each evaluation. Use this command to backfill from older
reports or to re-ingest after a schema change.

Backfill limitations:
  Decomposition examples require the workspace checkpoint to still exist
  on disk (under ~/.alluka/workspaces/<id>/checkpoint.json). Workspaces that
  have been cleaned up with 'orchestrator cleanup' will be skipped for
  example extraction. Routing corrections still ingest fully from the audit
  report itself. Decomposition findings still ingest too, but when the
  workspace/checkpoint is gone they lose decomp_source enrichment because
  that metadata comes from the saved plan, not the audit report.

Examples:
  orchestrator routing ingest-audit                                # process all audits
  orchestrator routing ingest-audit --workspace 20260228-f12991ff  # single workspace`,
		RunE: runRoutingIngestAudit,
	}
	ingestAuditCmd.Flags().String("workspace", "", "limit to a specific workspace ID")

	routingCmd.AddCommand(seedCmd)
	routingCmd.AddCommand(correctCmd)
	routingCmd.AddCommand(correctionsCmd)
	routingCmd.AddCommand(ingestAuditCmd)
	rootCmd.AddCommand(routingCmd)
}

func runRoutingSeed(cmd *cobra.Command, args []string) error {
	rdb, err := routing.OpenDB("")
	if err != nil {
		return fmt.Errorf("open routing DB: %w", err)
	}
	defer rdb.Close()

	ctx := context.Background()
	profiles := routing.DefaultSeeds()

	n, err := routing.Seed(ctx, rdb, profiles)
	if err != nil {
		fmt.Printf("warning: %v\n", err)
	}

	fmt.Printf("seeded %d target profile(s)\n", n)

	if verbose {
		for _, sp := range profiles {
			fmt.Printf("  %s (lang=%s, runtime=%s, personas=%v)\n",
				sp.TargetID, sp.Language, sp.Runtime, sp.PreferredPersonas)
		}
	}

	return nil
}

func runRoutingCorrect(cmd *cobra.Command, args []string) error {
	target, _ := cmd.Flags().GetString("target")
	assigned, _ := cmd.Flags().GetString("assigned")
	ideal, _ := cmd.Flags().GetString("ideal")
	taskHint, _ := cmd.Flags().GetString("task-hint")

	rdb, err := routing.OpenDB("")
	if err != nil {
		return fmt.Errorf("open routing DB: %w", err)
	}
	defer rdb.Close()

	c := routing.RoutingCorrection{
		TargetID:        target,
		AssignedPersona: assigned,
		IdealPersona:    ideal,
		TaskHint:        taskHint,
		Source:          routing.SourceManual,
	}
	if err := rdb.InsertRoutingCorrection(context.Background(), c); err != nil {
		return fmt.Errorf("recording correction: %w", err)
	}

	fmt.Printf("recorded correction: %s -> %s (target=%s)\n", assigned, ideal, target)
	if taskHint != "" {
		fmt.Printf("  task-hint: %s\n", taskHint)
	}
	return nil
}

func runRoutingCorrections(cmd *cobra.Command, args []string) error {
	var targetID string
	if len(args) > 0 {
		targetID = args[0]
	} else {
		cwd, _ := os.Getwd()
		targetID = resolveTarget(cwd, "", "")
		if targetID == "" {
			return fmt.Errorf("no target specified and could not resolve from current directory")
		}
	}

	rdb, err := routing.OpenDB("")
	if err != nil {
		return fmt.Errorf("open routing DB: %w", err)
	}
	defer rdb.Close()

	corrections, err := rdb.GetRoutingCorrections(context.Background(), targetID)
	if err != nil {
		return fmt.Errorf("fetching corrections: %w", err)
	}

	if len(corrections) == 0 {
		fmt.Printf("no corrections for %s\n", targetID)
		return nil
	}

	fmt.Printf("corrections for %s (%d total):\n", targetID, len(corrections))
	for _, c := range corrections {
		hint := ""
		if c.TaskHint != "" {
			hint = fmt.Sprintf(" [%s]", c.TaskHint)
		}
		fmt.Printf("  #%d %s -> %s (%s)%s  %s\n",
			c.ID, c.AssignedPersona, c.IdealPersona, c.Source, hint,
			c.CreatedAt.Format("2006-01-02 15:04"))
	}
	return nil
}

func runRoutingIngestAudit(cmd *cobra.Command, args []string) error {
	wsFilter, _ := cmd.Flags().GetString("workspace")

	reports, err := audit.LoadReports()
	if err != nil {
		return fmt.Errorf("loading audit reports: %w", err)
	}
	if len(reports) == 0 {
		fmt.Println("no audit reports found")
		return nil
	}

	// Filter to specific workspace if requested.
	if wsFilter != "" {
		var filtered []audit.AuditReport
		for _, r := range reports {
			if r.WorkspaceID == wsFilter {
				filtered = append(filtered, r)
			}
		}
		reports = filtered
		if len(reports) == 0 {
			return fmt.Errorf("no audit reports for workspace %s", wsFilter)
		}
	}

	rdb, err := routing.OpenDB("")
	if err != nil {
		return fmt.Errorf("open routing DB: %w", err)
	}
	defer rdb.Close()

	ctx := context.Background()

	result, err := routing.IngestAuditReports(ctx, rdb, reports, routing.ResolveWorkspaceTarget, routing.LoadWorkspacePlan)
	if err != nil {
		return fmt.Errorf("ingesting audit reports: %w", err)
	}

	total := result.Corrections + result.Findings + result.Examples + result.RoutingPatterns + result.HandoffPatterns
	if total == 0 && result.ExamplesSkipped == 0 {
		fmt.Printf("no corrections, findings, or examples extracted from %d audit report(s)\n", len(reports))
		return nil
	}

	fmt.Printf("ingested from %d audit report(s):\n", len(reports))
	if result.Corrections > 0 || result.CorrectionsDupes > 0 {
		fmt.Printf("  %d correction(s)", result.Corrections)
		if result.CorrectionsDupes > 0 {
			fmt.Printf(" (%d duplicates skipped)", result.CorrectionsDupes)
		}
		fmt.Println()
	}
	if result.RoutingPatterns > 0 {
		fmt.Printf("  %d routing pattern(s)\n", result.RoutingPatterns)
	}
	if result.HandoffPatterns > 0 {
		fmt.Printf("  %d handoff pattern(s)\n", result.HandoffPatterns)
	}
	if result.Findings > 0 {
		fmt.Printf("  %d decomposition finding(s)\n", result.Findings)
	}
	if result.Examples > 0 {
		fmt.Printf("  %d decomposition example(s)\n", result.Examples)
	}
	if result.ExamplesSkipped > 0 {
		fmt.Printf("  %d example(s) skipped (workspace/checkpoint missing)\n", result.ExamplesSkipped)
	}
	return nil
}
