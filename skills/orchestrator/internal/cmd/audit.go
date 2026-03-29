package cmd

import (
	"fmt"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/audit"
)

func init() {
	auditCmd := &cobra.Command{
		Use:   "audit",
		Short: "Audit tools for mission quality tracking",
		Long: `Audit tools for evaluating and tracking mission quality.

Evaluation and report display have moved to gyo:
  gyo evaluate [workspace-id]   — evaluate a completed mission
  gyo report [workspace-id]     — display a saved audit report

Applying recommendations has moved to ko:
  ko apply <workspace-id>       — apply audit recommendations

The scorecard subcommand remains here for trend analysis.`,
	}

	// Subcommand: audit scorecard
	scorecardCmd := &cobra.Command{
		Use:   "scorecard",
		Short: "Show trend lines and regressions across all audit reports",
		Long: `Reads all saved audit reports from ~/.alluka/audits.jsonl, computes trend lines
per metric, and detects regressions linked to specific workspaces.

Examples:
  orchestrator audit scorecard                # show all trends
  orchestrator audit scorecard --domain dev   # filter by domain
  orchestrator audit scorecard --last 10      # last 10 audits only
  orchestrator audit scorecard --format json  # machine-readable output`,
		RunE: runScorecard,
	}
	scorecardCmd.Flags().String("format", "text", "output format: text, json")
	scorecardCmd.Flags().String("domain", "", "filter audits by domain")
	scorecardCmd.Flags().Int("last", 0, "limit to last N audits (0 = all)")

	auditCmd.AddCommand(scorecardCmd)
	rootCmd.AddCommand(auditCmd)
}

func runScorecard(cmd *cobra.Command, args []string) error {
	format, _ := cmd.Flags().GetString("format")
	domainFilter, _ := cmd.Flags().GetString("domain")
	last, _ := cmd.Flags().GetInt("last")

	reports, err := audit.LoadReports()
	if err != nil {
		return fmt.Errorf("loading audit reports: %w", err)
	}

	if len(reports) == 0 {
		fmt.Println("No audit reports found. Run `gyo evaluate` to generate one.")
		return nil
	}

	// Filter by domain
	if domainFilter != "" {
		var filtered []audit.AuditReport
		for _, r := range reports {
			if r.Domain == domainFilter {
				filtered = append(filtered, r)
			}
		}
		reports = filtered
		if len(reports) == 0 {
			fmt.Printf("No audit reports found for domain %q.\n", domainFilter)
			return nil
		}
	}

	// Limit to last N
	if last > 0 && last < len(reports) {
		reports = reports[len(reports)-last:]
	}

	summary := audit.BuildScorecard(reports)

	switch format {
	case "json":
		out, err := audit.FormatScorecardJSON(summary)
		if err != nil {
			return err
		}
		fmt.Println(out)
	default:
		fmt.Print(audit.FormatScorecard(summary))
	}

	return nil
}

