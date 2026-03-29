package cmd

import (
	"context"
	"fmt"
	"log"
	"math/rand"
	"os"
	"strings"
	"text/tabwriter"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
	cronutil "github.com/joeyhipolito/nanika-scheduler/internal/cron"
	"github.com/joeyhipolito/nanika-scheduler/internal/db"
)

func newJobsCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "jobs",
		Short: "List, add, enable, disable, or remove scheduled jobs",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsList()
		},
	}

	addCmd := &cobra.Command{
		Use:   "add",
		Short: "Add a new scheduled job",
		Example: `  scheduler-cli jobs add --name "daily-backup" --cron "0 2 * * *" --command "tar czf /tmp/backup.tgz ~/docs"
  scheduler-cli jobs add --name "health-check" --cron "*/5 * * * *" --command "curl -s localhost:8080/health"
  scheduler-cli jobs add --name "health-sweep" --cron "0 */4 * * *" --command "shu evaluate"`,
		RunE: func(cmd *cobra.Command, args []string) error {
			name, _ := cmd.Flags().GetString("name")
			schedule, _ := cmd.Flags().GetString("cron")
			randomDaily, _ := cmd.Flags().GetString("random-daily")
			command, _ := cmd.Flags().GetString("command")
			timeoutSec, _ := cmd.Flags().GetInt("timeout")
			if schedule != "" && randomDaily != "" {
				return fmt.Errorf("--cron and --random-daily are mutually exclusive")
			}
			if schedule == "" && randomDaily == "" {
				return fmt.Errorf("one of --cron or --random-daily is required")
			}
			return runJobsAdd(name, schedule, randomDaily, command, timeoutSec)
		},
	}
	addCmd.Flags().String("name", "", "Job name (required)")
	addCmd.Flags().String("cron", "", "Cron expression, e.g. '*/5 * * * *'")
	addCmd.Flags().String("random-daily", "", "Random daily window, e.g. '8:00-20:00' (mutually exclusive with --cron)")
	addCmd.Flags().String("command", "", "Shell command to execute (required)")
	addCmd.Flags().Int("timeout", 0, "Timeout in seconds (0 = no timeout)")
	_ = addCmd.MarkFlagRequired("name")
	_ = addCmd.MarkFlagRequired("command")

	removeCmd := &cobra.Command{
		Use:     "remove <job-id>",
		Short:   "Remove a job by ID (cascades to run history)",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler-cli jobs remove 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsRemove(args[0])
		},
	}

	enableCmd := &cobra.Command{
		Use:     "enable <job-id>",
		Short:   "Enable a job",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler-cli jobs enable 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsSetEnabled(args[0], true)
		},
	}

	disableCmd := &cobra.Command{
		Use:     "disable <job-id>",
		Short:   "Disable a job (won't run until re-enabled)",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler-cli jobs disable 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsSetEnabled(args[0], false)
		},
	}

	cmd.AddCommand(addCmd, removeCmd, enableCmd, disableCmd)
	return cmd
}

func openDB() (*db.DB, error) {
	cfg, err := config.Load()
	if err != nil {
		return nil, fmt.Errorf("loading config: %w", err)
	}
	if err := config.EnsureDir(); err != nil {
		return nil, err
	}
	return db.Open(cfg.DBPath)
}

func runJobsList() error {
	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	jobs, err := d.ListJobs(context.Background())
	if err != nil {
		return err
	}

	if len(jobs) == 0 {
		fmt.Println("no jobs — add one with: scheduler-cli jobs add --name <name> --cron '<expr>' --command '<cmd>'")
		return nil
	}

	w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
	fmt.Fprintln(w, "ID\tNAME\tSCHEDULE\tENABLED\tCOMMAND")
	for _, j := range jobs {
		enabled := "yes"
		if !j.Enabled {
			enabled = "no"
		}
		schedule := j.Schedule
		if j.RandomWindow != "" {
			schedule = "random " + j.RandomWindow
		}
		cmd := j.Command
		if len(cmd) > 40 {
			cmd = cmd[:37] + "..."
		}
		fmt.Fprintf(w, "%d\t%s\t%s\t%s\t%s\n", j.ID, j.Name, schedule, enabled, cmd)
	}
	return w.Flush()
}

func runJobsAdd(name, schedule, randomDaily, command string, timeoutSec int) error {
	// Load config once; openDB also loads it, so we inline the open to avoid a double read.
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if err := config.EnsureDir(); err != nil {
		return err
	}
	d, err := db.Open(cfg.DBPath)
	if err != nil {
		return err
	}
	defer d.Close()

	ctx := context.Background()

	var next time.Time
	var displaySchedule string

	if randomDaily != "" {
		// Validate the window format.
		t, err := nextRandomTime(randomDaily)
		if err != nil {
			return fmt.Errorf("invalid --random-daily window %q: %w", randomDaily, err)
		}
		next = t
		displaySchedule = "random " + randomDaily
	} else {
		// Validate the cron expression before touching the DB.
		t, err := cronutil.NextRun(schedule)
		if err != nil {
			return fmt.Errorf("invalid cron schedule %q: %w", schedule, err)
		}
		next = t
		displaySchedule = schedule
	}

	id, err := d.CreateJob(ctx, name, command, schedule, cfg.Shell, randomDaily, timeoutSec)
	if err != nil {
		return fmt.Errorf("adding job: %w", err)
	}

	if err := d.SetNextRunAt(ctx, id, &next); err != nil {
		log.Printf("warning: could not set next_run_at for job %d: %v", id, err)
	}

	fmt.Printf("added job %d: %s (%s)\n", id, name, displaySchedule)
	return nil
}

// parseRandomWindow parses a "H:MM-H:MM" window string into (startH, startM, endH, endM).
func parseRandomWindow(window string) (startH, startM, endH, endM int, err error) {
	parts := strings.SplitN(window, "-", 2)
	if len(parts) != 2 {
		return 0, 0, 0, 0, fmt.Errorf("expected H:MM-H:MM format")
	}
	if _, err := fmt.Sscanf(parts[0], "%d:%d", &startH, &startM); err != nil {
		return 0, 0, 0, 0, fmt.Errorf("invalid start time %q: %w", parts[0], err)
	}
	if _, err := fmt.Sscanf(parts[1], "%d:%d", &endH, &endM); err != nil {
		return 0, 0, 0, 0, fmt.Errorf("invalid end time %q: %w", parts[1], err)
	}
	startMins := startH*60 + startM
	endMins := endH*60 + endM
	if endMins <= startMins {
		return 0, 0, 0, 0, fmt.Errorf("end time must be after start time")
	}
	return startH, startM, endH, endM, nil
}

// randomTimeInWindow returns a random time on the given day within the H:MM-H:MM window.
// The returned time uses local timezone.
func randomTimeInWindow(window string, day time.Time) (time.Time, error) {
	startH, startM, endH, endM, err := parseRandomWindow(window)
	if err != nil {
		return time.Time{}, fmt.Errorf("parsing window %q: %w", window, err)
	}
	startMins := startH*60 + startM
	endMins := endH*60 + endM
	offset := rand.Intn(endMins - startMins)
	total := startMins + offset
	return time.Date(day.Year(), day.Month(), day.Day(), total/60, total%60, 0, 0, day.Location()), nil
}

// nextRandomTime returns the next valid random time within window —
// a random time today if still in the future, otherwise a random time tomorrow.
func nextRandomTime(window string) (time.Time, error) {
	now := time.Now()
	t, err := randomTimeInWindow(window, now)
	if err != nil {
		return time.Time{}, err
	}
	if t.After(now) {
		return t, nil
	}
	return randomTimeInWindow(window, now.AddDate(0, 0, 1))
}

// randomTimeTomorrow returns a random time tomorrow within window.
func randomTimeTomorrow(window string) (time.Time, error) {
	tomorrow := time.Now().AddDate(0, 0, 1)
	return randomTimeInWindow(window, tomorrow)
}

func runJobsRemove(idStr string) error {
	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	var id int64
	if _, err := fmt.Sscanf(idStr, "%d", &id); err != nil {
		return fmt.Errorf("invalid job ID %q: must be an integer", idStr)
	}

	// Verify it exists before deleting.
	if _, err := d.GetJob(context.Background(), id); err != nil {
		return fmt.Errorf("job %d not found", id)
	}

	if err := d.DeleteJob(context.Background(), id); err != nil {
		return err
	}
	fmt.Printf("removed job %d\n", id)
	return nil
}

func runJobsSetEnabled(idStr string, enabled bool) error {
	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	var id int64
	if _, err := fmt.Sscanf(idStr, "%d", &id); err != nil {
		return fmt.Errorf("invalid job ID %q: must be an integer", idStr)
	}

	if err := d.EnableJob(context.Background(), id, enabled); err != nil {
		return err
	}
	state := "enabled"
	if !enabled {
		state = "disabled"
	}
	fmt.Printf("job %d %s\n", id, state)
	return nil
}
