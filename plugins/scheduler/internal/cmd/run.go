package cmd

import (
	"context"
	"fmt"
	"os/signal"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
	"github.com/joeyhipolito/nanika-scheduler/internal/executor"
)

func newRunCmd() *cobra.Command {
	return &cobra.Command{
		Use:     "run <job-id>",
		Short:   "Execute a job immediately (ignores schedule)",
		Args:    cobra.ExactArgs(1),
		Example: "  scheduler run 3",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runJobNow(args[0])
		},
	}
}

func runJobNow(idStr string) error {
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

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()

	exec := executor.New(d, cfg.Shell, 1)

	fmt.Printf("running job %d (%s)...\n", job.ID, job.Name)
	start := time.Now()

	result := <-exec.Run(ctx, *job)

	elapsed := time.Since(start).Round(time.Millisecond)

	if result.Stdout != "" {
		fmt.Print(result.Stdout)
	}
	if result.Stderr != "" {
		fmt.Print(result.Stderr)
	}

	fmt.Printf("finished in %s (exit %d, status: %s)\n", elapsed, result.ExitCode, result.Status)

	if result.Err != nil {
		return fmt.Errorf("run error: %w", result.Err)
	}
	if result.ExitCode != 0 {
		return fmt.Errorf("job exited with code %d", result.ExitCode)
	}
	return nil
}
