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

// defaultJobs is intentionally empty: the scheduler plugin owns the cron
// execution infrastructure but does NOT own jobs that belong to other
// plugins. Each plugin registers its own jobs via its own init command —
// for example, `shu propose --init` registers the nen self-improvement
// loop jobs (propose-remediations, dispatch-approved, close-sweep,
// evaluate-weekly).
//
// An earlier version of this slice included scout/engage publishing
// pipeline jobs, but those reference commands from plugins that may not
// ship with every nanika install (the core release bundle excludes them),
// so the leaky dependency has been removed. If you want the publishing
// pipeline, install scout + engage and add the jobs via `scheduler jobs add`
// or a future `scout init` / `engage init` subcommand.
var defaultJobs = []defaultJob{}

func newInitCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "init",
		Short: "Initialize scheduler database",
		Long: `Creates the scheduler database and registers any default jobs.
Use 'scheduler jobs add' to add your own cron jobs.
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
		id, err := d.CreateJob(ctx, def.name, def.command, def.schedule, cfg.Shell, "", "cron", 0)
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
