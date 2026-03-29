package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
)

func newStatusCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "status",
		Short: "Show scheduler overview: job count, recent runs, config",
		RunE: func(cmd *cobra.Command, args []string) error {
			return runStatus()
		},
	}
}

func runStatus() error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	d, err := openDB()
	if err != nil {
		return fmt.Errorf("opening database: %w", err)
	}
	defer d.Close()

	stats, err := d.GetStats(context.Background())
	if err != nil {
		return fmt.Errorf("getting stats: %w", err)
	}

	fmt.Printf("scheduler status\n")
	fmt.Printf("  jobs (total/enabled) : %d / %d\n", stats.TotalJobs, stats.EnabledJobs)
	fmt.Printf("  runs total           : %d\n", stats.TotalRuns)
	if stats.RunningRuns > 0 {
		fmt.Printf("  runs currently running: %d\n", stats.RunningRuns)
	}
	fmt.Printf("  posts (total/pending): %d / %d\n", stats.TotalPosts, stats.PendingPosts)
	if stats.DonePosts > 0 {
		fmt.Printf("  posts done           : %d\n", stats.DonePosts)
	}
	if stats.FailedPosts > 0 {
		fmt.Printf("  posts failed         : %d\n", stats.FailedPosts)
	}

	// Daemon state
	pidPath := filepath.Join(config.Dir(), "daemon.pid")
	if pidBytes, err := os.ReadFile(pidPath); err == nil {
		pid, _ := strconv.Atoi(strings.TrimSpace(string(pidBytes)))
		if pid > 0 && processAlive(pid) {
			fmt.Printf("  daemon               : running (PID %d)\n", pid)
		} else {
			fmt.Printf("  daemon               : not running (stale PID file)\n")
		}
	} else {
		fmt.Printf("  daemon               : not running\n")
	}

	fmt.Printf("  database             : %s\n", cfg.DBPath)
	fmt.Printf("  shell                : %s\n", cfg.Shell)
	fmt.Printf("  max concurrent       : %d\n", cfg.MaxConcurrent)
	fmt.Printf("  log level            : %s\n", cfg.LogLevel)
	return nil
}
