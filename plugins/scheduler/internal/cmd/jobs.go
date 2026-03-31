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
		Example: `  scheduler jobs add --name "daily-backup" --cron "0 2 * * *" --command "tar czf /tmp/backup.tgz ~/docs"
  scheduler jobs add --name "health-check" --cron "*/5 * * * *" --command "curl -s localhost:8080/health"
  scheduler jobs add --name "health-sweep" --cron "0 */4 * * *" --command "shu evaluate"
  scheduler jobs add --name "standup" --at "9:00 AM" --command "notify-send standup"
  scheduler jobs add --name "ping" --every 30m --command "curl -s localhost:8080/health"
  scheduler jobs add --name "warmup" --delay 2h --command "scripts/warmup.sh"`,
		RunE: func(cmd *cobra.Command, args []string) error {
			name, _ := cmd.Flags().GetString("name")
			schedule, _ := cmd.Flags().GetString("cron")
			randomDaily, _ := cmd.Flags().GetString("random-daily")
			atStr, _ := cmd.Flags().GetString("at")
			everyStr, _ := cmd.Flags().GetString("every")
			delayStr, _ := cmd.Flags().GetString("delay")
			command, _ := cmd.Flags().GetString("command")
			timeoutSec, _ := cmd.Flags().GetInt("timeout")
			dryRun, _ := cmd.Flags().GetBool("dry-run")

			// Count how many schedule modes were provided.
			modes := 0
			if schedule != "" {
				modes++
			}
			if randomDaily != "" {
				modes++
			}
			if atStr != "" {
				modes++
			}
			if everyStr != "" {
				modes++
			}
			if delayStr != "" {
				modes++
			}
			if modes > 1 {
				return fmt.Errorf("--cron, --random-daily, --at, --every, and --delay are mutually exclusive")
			}
			if modes == 0 {
				return fmt.Errorf("one of --cron, --random-daily, --at, --every, or --delay is required")
			}
			return runJobsAdd(name, schedule, randomDaily, atStr, everyStr, delayStr, command, timeoutSec, dryRun)
		},
	}
	addCmd.Flags().String("name", "", "Job name (required)")
	addCmd.Flags().String("cron", "", "Cron expression, e.g. '*/5 * * * *'")
	addCmd.Flags().String("random-daily", "", "Random daily window, e.g. '8:00-20:00' (mutually exclusive with --cron)")
	addCmd.Flags().String("at", "", "One-shot time, e.g. '2026-03-31T14:00' or '2:00 PM'")
	addCmd.Flags().String("every", "", "Repeating interval, e.g. '30m' or '4h'")
	addCmd.Flags().String("delay", "", "One-shot delay from now, e.g. '2h'")
	addCmd.Flags().String("command", "", "Shell command to execute (required)")
	addCmd.Flags().Int("timeout", 0, "Timeout in seconds (0 = no timeout)")
	addCmd.Flags().Bool("dry-run", false, "Validate schedule and print next fire times without creating the job")
	_ = addCmd.MarkFlagRequired("name")
	_ = addCmd.MarkFlagRequired("command")

	removeCmd := &cobra.Command{
		Use:     "remove <job-id>",
		Short:   "Remove a job by ID (cascades to run history)",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler jobs remove 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsRemove(args[0])
		},
	}

	enableCmd := &cobra.Command{
		Use:     "enable <job-id>",
		Short:   "Enable a job",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler jobs enable 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsSetEnabled(args[0], true)
		},
	}

	disableCmd := &cobra.Command{
		Use:     "disable <job-id>",
		Short:   "Disable a job (won't run until re-enabled)",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler jobs disable 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsSetEnabled(args[0], false)
		},
	}

	nextCmd := &cobra.Command{
		Use:     "next <job-id>",
		Short:   "Show next fire times for a job (5 for recurring, 1 for one-shots)",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler jobs next 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobsNext(args[0])
		},
	}

	cmd.AddCommand(addCmd, removeCmd, enableCmd, disableCmd, nextCmd)
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
		fmt.Println("no jobs — add one with: scheduler jobs add --name <name> --cron '<expr>' --command '<cmd>'")
		return nil
	}

	w := tabwriter.NewWriter(os.Stdout, 0, 0, 2, ' ', 0)
	fmt.Fprintln(w, "ID\tNAME\tSCHEDULE\tNEXT RUN\tENABLED\tCOMMAND")
	for _, j := range jobs {
		enabled := "yes"
		if !j.Enabled {
			enabled = "no"
		}
		var schedule string
		switch j.ScheduleType {
		case "random":
			schedule = "random " + j.RandomWindow
		case "at":
			if j.NextRunAt != nil {
				schedule = "at " + j.NextRunAt.Local().Format("2006-01-02 15:04")
			} else {
				schedule = "at (done)"
			}
		case "every":
			schedule = "every " + j.Schedule
		case "delay":
			if j.NextRunAt != nil {
				schedule = "delay (runs " + j.NextRunAt.Local().Format("15:04") + ")"
			} else {
				schedule = "delay (done)"
			}
		default:
			schedule = j.Schedule
		}
		var nextRun string
		if j.NextRunAt == nil {
			nextRun = "—"
		} else {
			nextRun = humanRelTime(*j.NextRunAt)
		}
		command := j.Command
		if len(command) > 40 {
			command = command[:37] + "..."
		}
		fmt.Fprintf(w, "%d\t%s\t%s\t%s\t%s\t%s\n", j.ID, j.Name, schedule, nextRun, enabled, command)
	}
	return w.Flush()
}

func runJobsAdd(name, schedule, randomDaily, atStr, everyStr, delayStr, command string, timeoutSec int, dryRun bool) error {
	// Load config once; openDB also loads it, so we inline the open to avoid a double read.
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if err := config.EnsureDir(); err != nil {
		return err
	}

	var next time.Time
	var displaySchedule string
	var scheduleType string
	var intervalStr string // for --every: stored in schedule column

	switch {
	case randomDaily != "":
		t, err := nextRandomTime(randomDaily)
		if err != nil {
			return fmt.Errorf("invalid --random-daily window %q: %w", randomDaily, err)
		}
		next = t
		displaySchedule = "random " + randomDaily
		scheduleType = "random"

	case atStr != "":
		t, err := parseAtTime(atStr)
		if err != nil {
			return fmt.Errorf("invalid --at time %q: %w", atStr, err)
		}
		if !t.After(time.Now()) {
			return fmt.Errorf("--at time %q is in the past", atStr)
		}
		next = t
		displaySchedule = "at " + t.Local().Format("2006-01-02 15:04")
		scheduleType = "at"

	case everyStr != "":
		interval, err := time.ParseDuration(everyStr)
		if err != nil {
			return fmt.Errorf("invalid --every duration %q: %w", everyStr, err)
		}
		if interval <= 0 {
			return fmt.Errorf("--every duration must be positive")
		}
		next = time.Now().Add(interval)
		intervalStr = everyStr
		displaySchedule = "every " + everyStr
		scheduleType = "every"

	case delayStr != "":
		delay, err := time.ParseDuration(delayStr)
		if err != nil {
			return fmt.Errorf("invalid --delay duration %q: %w", delayStr, err)
		}
		if delay <= 0 {
			return fmt.Errorf("--delay duration must be positive")
		}
		next = time.Now().Add(delay)
		displaySchedule = "delay " + delayStr + " (runs at " + next.Local().Format("15:04") + ")"
		scheduleType = "delay"

	default: // --cron
		t, err := cronutil.NextRun(schedule)
		if err != nil {
			return fmt.Errorf("invalid cron schedule %q: %w", schedule, err)
		}
		next = t
		displaySchedule = schedule
		scheduleType = "cron"
	}

	// For --every, store the interval string in the schedule column.
	storedSchedule := schedule
	if scheduleType == "every" {
		storedSchedule = intervalStr
	}

	if dryRun {
		fmt.Printf("dry-run: job %q (%s) — not created\n", name, displaySchedule)
		synthetic := db.Job{
			ScheduleType: scheduleType,
			Schedule:     storedSchedule,
			RandomWindow: randomDaily,
			NextRunAt:    &next,
		}
		times, err := computeNextFireTimes(synthetic, 5)
		if err != nil {
			return fmt.Errorf("computing fire times: %w", err)
		}
		printNextFireTimes(times)
		return nil
	}

	d, err := db.Open(cfg.DBPath)
	if err != nil {
		return err
	}
	defer d.Close()

	ctx := context.Background()

	id, err := d.CreateJob(ctx, name, command, storedSchedule, cfg.Shell, randomDaily, scheduleType, timeoutSec)
	if err != nil {
		return fmt.Errorf("adding job: %w", err)
	}

	if err := d.SetNextRunAt(ctx, id, &next); err != nil {
		log.Printf("warning: could not set next_run_at for job %d: %v", id, err)
	}

	fmt.Printf("added job %d: %s (%s)\n", id, name, displaySchedule)
	return nil
}

// parseAtTime parses a time string in several formats.
// Supports RFC3339, "2006-01-02T15:04", time-only formats (uses today's date).
func parseAtTime(s string) (time.Time, error) {
	// Try date+time formats first.
	dateTimeFmts := []string{
		time.RFC3339,
		"2006-01-02T15:04:05",
		"2006-01-02T15:04",
		"2006-01-02 15:04:05",
		"2006-01-02 15:04",
	}
	for _, layout := range dateTimeFmts {
		if t, err := time.ParseInLocation(layout, s, time.Local); err == nil {
			return t, nil
		}
	}

	// Try time-only formats; apply to today's date.
	timeOnlyFmts := []string{
		"3:04 PM",
		"3:04PM",
		"15:04",
		"3 PM",
		"3PM",
	}
	now := time.Now()
	for _, layout := range timeOnlyFmts {
		t, err := time.ParseInLocation(layout, s, time.Local)
		if err != nil {
			continue
		}
		// Combine today's date with the parsed clock time.
		result := time.Date(now.Year(), now.Month(), now.Day(),
			t.Hour(), t.Minute(), t.Second(), 0, time.Local)
		return result, nil
	}

	return time.Time{}, fmt.Errorf("unrecognized time format; try '2026-03-31T14:00', '2:00 PM', or '14:00'")
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

func runJobsNext(idStr string) error {
	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	var id int64
	if _, err := fmt.Sscanf(idStr, "%d", &id); err != nil {
		return fmt.Errorf("invalid job ID %q: must be an integer", idStr)
	}

	j, err := d.GetJob(context.Background(), id)
	if err != nil {
		return fmt.Errorf("job %d not found", id)
	}

	fmt.Printf("job %d: %s (%s)\n", j.ID, j.Name, j.ScheduleType)

	switch j.ScheduleType {
	case "at", "delay":
		if j.NextRunAt == nil {
			fmt.Println("  one-shot: already fired")
		} else {
			fmt.Printf("  fires once: %s  (%s)\n",
				j.NextRunAt.Local().Format("Mon, Jan 2 2006 at 3:04 PM"),
				humanRelTime(*j.NextRunAt))
		}
		return nil
	}

	times, err := computeNextFireTimes(*j, 5)
	if err != nil {
		return fmt.Errorf("computing fire times: %w", err)
	}
	if len(times) == 0 {
		fmt.Println("  no upcoming fire times")
		return nil
	}
	printNextFireTimes(times)
	return nil
}

// computeNextFireTimes returns up to count future fire times for a job.
// For cron jobs, it iterates from now. For every jobs, it starts from NextRunAt
// (or now+interval if nil) and steps by interval. For random jobs, it returns
// the single stored NextRunAt. For at/delay, callers handle the one-shot display
// directly.
func computeNextFireTimes(j db.Job, count int) ([]time.Time, error) {
	if count <= 0 {
		count = 5
	}
	switch j.ScheduleType {
	case "cron", "": // legacy empty defaults to cron
		var times []time.Time
		anchor := time.Now()
		for i := 0; i < count; i++ {
			t, err := cronutil.NextRunAfter(j.Schedule, anchor)
			if err != nil {
				return nil, fmt.Errorf("computing cron run: %w", err)
			}
			times = append(times, t)
			anchor = t
		}
		return times, nil

	case "every":
		interval, err := time.ParseDuration(j.Schedule)
		if err != nil {
			return nil, fmt.Errorf("bad every interval %q: %w", j.Schedule, err)
		}
		var anchor time.Time
		if j.NextRunAt != nil {
			anchor = *j.NextRunAt
		} else {
			anchor = time.Now().Add(interval)
		}
		times := make([]time.Time, count)
		times[0] = anchor
		for i := 1; i < count; i++ {
			times[i] = times[i-1].Add(interval)
		}
		return times, nil

	case "random":
		// Can only show the stored next run; future randoms are non-deterministic.
		if j.NextRunAt == nil {
			return nil, nil
		}
		return []time.Time{*j.NextRunAt}, nil

	case "at", "delay":
		if j.NextRunAt == nil {
			return nil, nil
		}
		return []time.Time{*j.NextRunAt}, nil

	default:
		return nil, fmt.Errorf("unknown schedule type %q", j.ScheduleType)
	}
}

// printNextFireTimes prints a numbered list of fire times with human-relative labels.
func printNextFireTimes(times []time.Time) {
	for i, t := range times {
		fmt.Printf("  %d. %s  (%s)\n", i+1,
			t.Local().Format("Mon, Jan 2 2006 at 3:04 PM"),
			humanRelTime(t))
	}
}

// humanRelTime returns a short human-readable description of when t occurs relative to now.
// Examples: "in 2h 15m", "tomorrow 8:00 AM", "Mon 3:04 PM", "Apr 5 2:00 PM".
func humanRelTime(t time.Time) string {
	now := time.Now()
	d := t.Sub(now)
	if d < 0 {
		return "overdue"
	}
	if d < time.Minute {
		return "now"
	}

	// Boundaries in local time.
	tLocal := t.Local()
	todayStart := time.Date(now.Year(), now.Month(), now.Day(), 0, 0, 0, 0, time.Local)
	tomorrowStart := todayStart.AddDate(0, 0, 1)
	dayAfterStart := todayStart.AddDate(0, 0, 2)

	switch {
	case tLocal.Before(tomorrowStart):
		// Same day: show relative duration.
		return "in " + formatRelDuration(d)
	case tLocal.Before(dayAfterStart):
		return "tomorrow " + tLocal.Format("3:04 PM")
	case d < 7*24*time.Hour:
		return tLocal.Format("Mon 3:04 PM")
	default:
		return tLocal.Format("Jan 2 3:04 PM")
	}
}

// formatRelDuration formats d as "Xh Ym" or "Xh" or "Ym".
func formatRelDuration(d time.Duration) string {
	h := int(d.Hours())
	m := int(d.Minutes()) % 60
	switch {
	case h > 0 && m > 0:
		return fmt.Sprintf("%dh %dm", h, m)
	case h > 0:
		return fmt.Sprintf("%dh", h)
	default:
		return fmt.Sprintf("%dm", m)
	}
}
