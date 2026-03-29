package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
	"github.com/joeyhipolito/nanika-scheduler/internal/executor"
)

// queryStatusOutput is the JSON shape for "query status".
type queryStatusOutput struct {
	DaemonRunning bool    `json:"daemon_running"`
	JobCount      int     `json:"job_count"`
	EnabledCount  int     `json:"enabled_count"`
	NextRunAt     *string `json:"next_run_at"`
}

// queryItemOutput is one job entry in "query items".
type queryItemOutput struct {
	ID           int64   `json:"id"`
	Name         string  `json:"name"`
	Schedule     string  `json:"schedule"`
	Enabled      bool    `json:"enabled"`
	LastRun      *string `json:"last_run"`
	NextRun      *string `json:"next_run"`
	LastExitCode *int    `json:"last_exit_code"`
}

// queryActionOutput is the JSON shape for "query action" commands.
type queryActionOutput struct {
	OK       bool   `json:"ok"`
	JobID    int64  `json:"job_id"`
	Action   string `json:"action"`
	Message  string `json:"message"`
	ExitCode *int   `json:"exit_code,omitempty"`
	Stdout   string `json:"stdout,omitempty"`
	Stderr   string `json:"stderr,omitempty"`
}

type queryItemsEnvelope struct {
	Items []queryItemOutput `json:"items"`
	Count int               `json:"count"`
}

type querySchedulerActionItem struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type querySchedulerActionsOutput struct {
	Actions []querySchedulerActionItem `json:"actions"`
}

func runQueryActions(jsonOutput bool) error {
	actions := []querySchedulerActionItem{
		{Name: "jobs", Command: "scheduler jobs", Description: "List all scheduled jobs"},
		{Name: "daemon", Command: "scheduler daemon", Description: "Start the scheduler daemon"},
		{Name: "run", Command: "scheduler query action run <job-id>", Description: "Run a job immediately"},
		{Name: "enable", Command: "scheduler query action enable <job-id>", Description: "Enable a job"},
		{Name: "disable", Command: "scheduler query action disable <job-id>", Description: "Disable a job"},
	}
	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(querySchedulerActionsOutput{Actions: actions})
	}
	for _, a := range actions {
		fmt.Printf("%-10s  %s\n            command: %s\n", a.Name, a.Description, a.Command)
	}
	return nil
}

func newQueryCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "query",
		Short: "Query scheduler state (JSON-native subcommands for agent use)",
	}

	statusCmd := &cobra.Command{
		Use:   "status",
		Short: "Show daemon state, job counts, and next scheduled run",
		RunE: func(cmd *cobra.Command, args []string) error {
			jsonFlag, _ := cmd.Flags().GetBool("json")
			return runQueryStatus(jsonFlag)
		},
	}
	statusCmd.Flags().Bool("json", false, "Output as JSON")

	itemsCmd := &cobra.Command{
		Use:   "items",
		Short: "List all jobs with schedule and last-run details",
		RunE: func(cmd *cobra.Command, args []string) error {
			jsonFlag, _ := cmd.Flags().GetBool("json")
			return runQueryItems(jsonFlag)
		},
	}
	itemsCmd.Flags().Bool("json", false, "Output as JSON")

	actionCmd := &cobra.Command{
		Use:   "action <run|enable|disable> <job-id>",
		Short: "Run, enable, or disable a job immediately",
		Args:  cobra.ExactArgs(2),
		RunE: func(cmd *cobra.Command, args []string) error {
			jsonFlag, _ := cmd.Flags().GetBool("json")
			return runQueryAction(args[0], args[1], jsonFlag)
		},
	}
	actionCmd.Flags().Bool("json", false, "Output as JSON")

	actionsCmd := &cobra.Command{
		Use:   "actions",
		Short: "List available scheduler actions",
		RunE: func(cmd *cobra.Command, args []string) error {
			jsonFlag, _ := cmd.Flags().GetBool("json")
			return runQueryActions(jsonFlag)
		},
	}
	actionsCmd.Flags().Bool("json", false, "Output as JSON")

	cmd.AddCommand(statusCmd, itemsCmd, actionCmd, actionsCmd)
	return cmd
}

func runQueryStatus(jsonOutput bool) error {
	d, err := openDB()
	if err != nil {
		return fmt.Errorf("opening database: %w", err)
	}
	defer d.Close()

	stats, err := d.GetStats(context.Background())
	if err != nil {
		return fmt.Errorf("getting stats: %w", err)
	}

	pidPath := filepath.Join(config.Dir(), "daemon.pid")
	daemonRunning := false
	if pidBytes, err := os.ReadFile(pidPath); err == nil {
		pid, _ := strconv.Atoi(strings.TrimSpace(string(pidBytes)))
		daemonRunning = pid > 0 && processAlive(pid)
	}

	var nextRunAt *string
	jobs, err := d.ListJobs(context.Background())
	if err == nil {
		var earliest *time.Time
		for _, j := range jobs {
			if !j.Enabled || j.NextRunAt == nil {
				continue
			}
			if earliest == nil || j.NextRunAt.Before(*earliest) {
				earliest = j.NextRunAt
			}
		}
		if earliest != nil {
			s := earliest.UTC().Format(time.RFC3339)
			nextRunAt = &s
		}
	}

	out := queryStatusOutput{
		DaemonRunning: daemonRunning,
		JobCount:      stats.TotalJobs,
		EnabledCount:  stats.EnabledJobs,
		NextRunAt:     nextRunAt,
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Printf("daemon running : %v\n", out.DaemonRunning)
	fmt.Printf("jobs           : %d total, %d enabled\n", out.JobCount, out.EnabledCount)
	if out.NextRunAt != nil {
		fmt.Printf("next run at    : %s\n", *out.NextRunAt)
	} else {
		fmt.Printf("next run at    : none\n")
	}
	return nil
}

func runQueryItems(jsonOutput bool) error {
	d, err := openDB()
	if err != nil {
		return fmt.Errorf("opening database: %w", err)
	}
	defer d.Close()

	jobsWithStats, err := d.ListJobsWithLastExitCode(context.Background())
	if err != nil {
		return fmt.Errorf("listing jobs: %w", err)
	}

	items := make([]queryItemOutput, 0, len(jobsWithStats))
	for _, j := range jobsWithStats {
		schedule := j.Schedule
		if j.RandomWindow != "" {
			schedule = "random " + j.RandomWindow
		}

		var lastRun, nextRun *string
		if j.LastRunAt != nil {
			s := j.LastRunAt.UTC().Format(time.RFC3339)
			lastRun = &s
		}
		if j.NextRunAt != nil {
			s := j.NextRunAt.UTC().Format(time.RFC3339)
			nextRun = &s
		}

		items = append(items, queryItemOutput{
			ID:           j.ID,
			Name:         j.Name,
			Schedule:     schedule,
			Enabled:      j.Enabled,
			LastRun:      lastRun,
			NextRun:      nextRun,
			LastExitCode: j.LastExitCode,
		})
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(queryItemsEnvelope{Items: items, Count: len(items)})
	}

	if len(items) == 0 {
		fmt.Println("no jobs")
		return nil
	}
	for _, item := range items {
		enabled := "yes"
		if !item.Enabled {
			enabled = "no"
		}
		lastRun := "-"
		if item.LastRun != nil {
			lastRun = *item.LastRun
		}
		nextRun := "-"
		if item.NextRun != nil {
			nextRun = *item.NextRun
		}
		exitCode := "-"
		if item.LastExitCode != nil {
			exitCode = strconv.Itoa(*item.LastExitCode)
		}
		fmt.Printf("%d  %-20s  %-30s  enabled=%-3s  last_run=%s  next_run=%s  exit=%s\n",
			item.ID, item.Name, item.Schedule, enabled, lastRun, nextRun, exitCode)
	}
	return nil
}

func runQueryAction(action, idStr string, jsonOutput bool) error {
	var id int64
	if _, err := fmt.Sscanf(idStr, "%d", &id); err != nil {
		return fmt.Errorf("invalid job ID %q: must be an integer", idStr)
	}

	switch action {
	case "run":
		return runQueryActionRun(id, jsonOutput)
	case "enable":
		return runQueryActionToggle(id, true, jsonOutput)
	case "disable":
		return runQueryActionToggle(id, false, jsonOutput)
	default:
		return fmt.Errorf("unknown action %q: must be run, enable, or disable", action)
	}
}

func runQueryActionRun(id int64, jsonOutput bool) error {
	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	job, err := d.GetJob(context.Background(), id)
	if err != nil {
		return fmt.Errorf("job %d not found", id)
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	exec := executor.New(d, cfg.Shell, 1)
	result := <-exec.Run(ctx, *job)

	exitCode := result.ExitCode
	ok := result.Err == nil && result.ExitCode == 0

	out := queryActionOutput{
		OK:       ok,
		JobID:    id,
		Action:   "run",
		Message:  fmt.Sprintf("exit %d, status: %s", result.ExitCode, result.Status),
		ExitCode: &exitCode,
		Stdout:   result.Stdout,
		Stderr:   result.Stderr,
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	if result.Stdout != "" {
		fmt.Print(result.Stdout)
	}
	if result.Stderr != "" {
		fmt.Print(result.Stderr)
	}
	fmt.Printf("exit %d (%s)\n", result.ExitCode, result.Status)
	return nil
}

func runQueryActionToggle(id int64, enabled bool, jsonOutput bool) error {
	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	if err := d.EnableJob(context.Background(), id, enabled); err != nil {
		return fmt.Errorf("toggling job %d: %w", id, err)
	}

	action := "enable"
	msg := fmt.Sprintf("job %d enabled", id)
	if !enabled {
		action = "disable"
		msg = fmt.Sprintf("job %d disabled", id)
	}

	out := queryActionOutput{
		OK:      true,
		JobID:   id,
		Action:  action,
		Message: msg,
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Println(msg)
	return nil
}
