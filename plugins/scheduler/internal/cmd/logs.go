package cmd

import (
	"context"
	"fmt"

	"github.com/spf13/cobra"
)

func newLogsCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:     "logs <job-id>",
		Short:   "Show execution history for a job",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler logs 3\n  scheduler logs 3 --limit 5",
		RunE: func(cmd *cobra.Command, args []string) error {
			limit, _ := cmd.Flags().GetInt("limit")
			return runLogs(args[0], limit)
		},
	}
	cmd.Flags().Int("limit", 20, "Number of recent runs to show")
	return cmd
}

func runLogs(idStr string, limit int) error {
	var id int64
	if _, err := fmt.Sscanf(idStr, "%d", &id); err != nil {
		return fmt.Errorf("invalid job ID %q: must be an integer", idStr)
	}

	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	job, err := d.GetJob(context.Background(), id)
	if err != nil {
		return fmt.Errorf("job %d not found", id)
	}

	runs, err := d.ListRuns(context.Background(), id, limit)
	if err != nil {
		return err
	}

	fmt.Printf("logs for job %d (%s):\n\n", job.ID, job.Name)

	if len(runs) == 0 {
		fmt.Println("  no runs recorded — use 'scheduler run <job-id>' to execute now")
		return nil
	}

	for _, r := range runs {
		exitStr := "running"
		if r.ExitCode != nil {
			exitStr = fmt.Sprintf("exit %d", *r.ExitCode)
		}
		durationStr := ""
		if r.DurationMs != nil {
			durationStr = fmt.Sprintf(" (%dms)", *r.DurationMs)
		}
		finishedStr := ""
		if r.FinishedAt != nil {
			finishedStr = " → " + r.FinishedAt.Format("2006-01-02T15:04:05Z")
		}
		fmt.Printf("  [%s%s] %s%s — %s\n",
			r.StartedAt.Format("2006-01-02T15:04:05Z"), finishedStr,
			exitStr, durationStr, r.Status)
		if r.Stdout != "" {
			fmt.Printf("    stdout: %s\n", r.Stdout)
		}
		if r.Stderr != "" {
			fmt.Printf("    stderr: %s\n", r.Stderr)
		}
	}
	return nil
}
