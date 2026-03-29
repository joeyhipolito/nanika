package cmd

import (
	"context"
	"fmt"
	"log"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
	cronutil "github.com/joeyhipolito/nanika-scheduler/internal/cron"
	"github.com/joeyhipolito/nanika-scheduler/internal/db"
)

type defaultJob struct {
	name     string
	schedule string
	command  string
}

// defaultJobs defines the nanika publishing pipeline schedule.
var defaultJobs = []defaultJob{
	{
		name:     "daily-scout",
		schedule: "0 8 * * *",
		command:  "scout gather",
	},
	{
		name:     "daily-engage",
		schedule: "0 9 * * *",
		command:  "engage scan && engage draft --reschedule-post",
	},
	{
		name:     "weekly-brief",
		schedule: "0 10 * * 1",
		command:  "scout intel",
	},
}

func newInitCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "init",
		Short: "Create default nanika publishing pipeline jobs",
		Long: `Creates three default cron jobs for the nanika publishing pipeline:

  daily-scout   — 8 AM daily    scout gather
  daily-engage  — 9 AM daily    engage scan && engage draft --reschedule-post
  weekly-brief  — Monday 10 AM  scout intel

Jobs that already exist by name are skipped. Use --force to replace them.
Run 'scheduler jobs' afterward to verify.`,
		RunE: func(cmd *cobra.Command, args []string) error {
			force, _ := cmd.Flags().GetBool("force")
			return runInit(force)
		},
	}
	cmd.Flags().Bool("force", false, "Replace existing jobs with the same name")
	return cmd
}

func runInit(force bool) error {
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

	existing, err := d.ListJobs(ctx)
	if err != nil {
		return fmt.Errorf("listing existing jobs: %w", err)
	}
	existingByName := make(map[string]db.Job, len(existing))
	for _, j := range existing {
		existingByName[j.Name] = j
	}

	created, skipped := 0, 0
	for _, def := range defaultJobs {
		if j, exists := existingByName[def.name]; exists {
			if !force {
				fmt.Printf("skip  %s (ID %d already exists — use --force to replace)\n", def.name, j.ID)
				skipped++
				continue
			}
			if err := d.DeleteJob(ctx, j.ID); err != nil {
				return fmt.Errorf("removing existing job %q (ID %d): %w", def.name, j.ID, err)
			}
			fmt.Printf("removed existing %s (ID %d)\n", def.name, j.ID)
		}

		next, err := cronutil.NextRun(def.schedule)
		if err != nil {
			return fmt.Errorf("invalid schedule for %q: %w", def.name, err)
		}
		id, err := d.CreateJob(ctx, def.name, def.command, def.schedule, cfg.Shell, "", 0)
		if err != nil {
			return fmt.Errorf("creating job %q: %w", def.name, err)
		}
		if err := d.SetNextRunAt(ctx, id, &next); err != nil {
			log.Printf("warning: could not set next_run_at for %q (ID %d): %v", def.name, id, err)
		}
		fmt.Printf("created %s (ID %d)  %s  →  %s\n", def.name, id, def.schedule, def.command)
		created++
	}

	fmt.Printf("\n%d created, %d skipped\n", created, skipped)
	return nil
}
