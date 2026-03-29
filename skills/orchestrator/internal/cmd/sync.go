package cmd

import (
	"fmt"
	"path/filepath"
	"strings"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/orchestrator-cli/internal/audit"
	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

func init() {
	syncCmd := &cobra.Command{
		Use:   "sync [issue-id]",
		Short: "Show and reconcile issue/workspace/audit links",
		Long: `Sync displays bidirectional links between Linear issues, workspaces,
mission files, and audit reports. When an issue ID is given, it shows all
workspaces associated with that issue. Without arguments, it shows links
for recent workspaces.

Use --write-back to update the status: field in source mission files
based on workspace completion state (safe for files without frontmatter).

Examples:
  orchestrator sync                     # show links for recent workspaces
  orchestrator sync V-5                 # show all workspaces for issue V-5
  orchestrator sync --write-back        # reconcile mission file statuses
  orchestrator sync V-5 --write-back    # reconcile status for issue V-5`,
		Args: cobra.MaximumNArgs(1),
		RunE: runSync,
	}
	syncCmd.Flags().Bool("write-back", false, "update mission file frontmatter status from workspace state")
	syncCmd.Flags().Int("last", 10, "number of recent workspaces to show (ignored when issue ID given)")

	rootCmd.AddCommand(syncCmd)
}

func runSync(cmd *cobra.Command, args []string) error {
	writeBack, _ := cmd.Flags().GetBool("write-back")
	last, _ := cmd.Flags().GetInt("last")

	// Load audit reports for cross-referencing.
	auditReports, _ := audit.LoadReports() // best-effort
	auditByWS := make(map[string]*audit.AuditReport, len(auditReports))
	for i := range auditReports {
		auditByWS[auditReports[i].WorkspaceID] = &auditReports[i]
	}

	var links []core.WorkspaceLink
	var err error

	if len(args) > 0 {
		// Issue-scoped view: find all workspaces for this issue.
		issueID := args[0]
		links, err = core.FindWorkspacesByIssue(issueID)
		if err != nil {
			return fmt.Errorf("finding workspaces for %s: %w", issueID, err)
		}
		if len(links) == 0 {
			fmt.Fprintf(cmd.OutOrStdout(), "no workspaces found for issue %s\n", issueID)
			return nil
		}
		fmt.Fprintf(cmd.OutOrStdout(), "issue %s: %d workspace(s)\n\n", issueID, len(links))
	} else {
		// Recent workspaces view.
		links, err = core.ListWorkspaceLinks(last)
		if err != nil {
			return fmt.Errorf("listing workspaces: %w", err)
		}
		if len(links) == 0 {
			fmt.Fprintln(cmd.OutOrStdout(), "no workspaces found")
			return nil
		}
	}

	// Display each link.
	for _, link := range links {
		status := link.Status
		if status == "" {
			status = "unknown"
		}

		issueTag := ""
		if link.LinearIssueID != "" {
			issueTag = link.LinearIssueID
		} else {
			issueTag = "(no issue)"
		}

		missionTag := ""
		if link.MissionPath != "" {
			missionTag = link.MissionPath
		} else {
			missionTag = "(ad-hoc)"
		}

		// Check for audit report.
		auditTag := "(not audited)"
		if ar, ok := auditByWS[link.WorkspaceID]; ok {
			auditTag = fmt.Sprintf("score %d/5", ar.Scorecard.Overall)
		}

		// Append a degraded marker to the status when linkage problems exist.
		statusDisplay := status
		if len(link.Degradations) > 0 {
			statusDisplay = status + " DEGRADED"
		}

		fmt.Fprintf(cmd.OutOrStdout(), "  %s [%s] %s\n", link.WorkspaceID, statusDisplay, issueTag)
		fmt.Fprintf(cmd.OutOrStdout(), "    mission: %s\n", missionTag)
		fmt.Fprintf(cmd.OutOrStdout(), "    audit:   %s\n", auditTag)

		// Surface each degradation so operators can act.
		for _, deg := range link.Degradations {
			fmt.Fprintf(cmd.OutOrStdout(), "    warn:    %s\n", deg)
		}

		// Write-back: sync status to mission file frontmatter.
		if writeBack {
			if status == "in_progress" {
				fmt.Fprintln(cmd.OutOrStdout(), "    sync:    skipped (in_progress)")
			} else {
				synced, err := core.SyncMissionStatus(link.WorkspacePath)
				if err != nil {
					fmt.Fprintf(cmd.ErrOrStderr(), "    sync:    error: %v\n", err)
				} else if synced != "" {
					fmt.Fprintf(cmd.OutOrStdout(), "    sync:    wrote status %q to %s %s\n",
						link.Status, filepath.Base(synced), missionScopeTag(synced))
				}
			}
		}
		fmt.Fprintln(cmd.OutOrStdout())
	}

	// Summary: issues with multiple workspaces (potential re-runs).
	if len(args) == 0 {
		issueCount := make(map[string]int)
		for _, l := range links {
			if l.LinearIssueID != "" {
				issueCount[l.LinearIssueID]++
			}
		}
		var multi []string
		for id, count := range issueCount {
			if count > 1 {
				multi = append(multi, fmt.Sprintf("%s (%d runs)", id, count))
			}
		}
		if len(multi) > 0 {
			fmt.Fprintf(cmd.OutOrStdout(), "issues with multiple runs: %s\n", strings.Join(multi, ", "))
		}
	}

	return nil
}

func missionScopeTag(path string) string {
	cfgDir, err := config.Dir()
	if err != nil {
		return "(scope unknown)"
	}
	absPath, err := filepath.Abs(path)
	if err != nil {
		return "(scope unknown)"
	}
	managedBase := filepath.Join(cfgDir, "missions") + string(filepath.Separator)
	if strings.HasPrefix(absPath, managedBase) {
		return "(managed)"
	}
	return "(repo-local)"
}
