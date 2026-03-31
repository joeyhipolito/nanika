package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"syscall"
	"time"

	"github.com/spf13/cobra"

	"github.com/joeyhipolito/nanika-scheduler/internal/config"
	cronutil "github.com/joeyhipolito/nanika-scheduler/internal/cron"
	"github.com/joeyhipolito/nanika-scheduler/internal/db"
	"github.com/joeyhipolito/nanika-scheduler/internal/executor"
)

// schedulerEvent is written to ~/.alluka/events/scheduler.jsonl after each job run.
type schedulerEvent struct {
	Type       string `json:"type"`
	JobID      int64  `json:"job_id"`
	JobName    string `json:"job_name"`
	Command    string `json:"command"`
	DurationMs int64  `json:"duration_ms"`
	ExitCode   int    `json:"exit_code"`
	Stderr     string `json:"stderr,omitempty"`
	Ts         string `json:"ts"`
}

// eventsDir returns ~/.alluka/events.
func eventsDir() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, ".alluka", "events")
}

// writeEvent appends ev as a JSON line to ~/.alluka/events/scheduler.jsonl.
func writeEvent(ev schedulerEvent) {
	dir := eventsDir()
	if dir == "" {
		return
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		log.Printf("events: mkdir %s: %v", dir, err)
		return
	}
	data, err := json.Marshal(ev)
	if err != nil {
		log.Printf("events: marshal: %v", err)
		return
	}
	f, err := os.OpenFile(filepath.Join(dir, "scheduler.jsonl"), os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		log.Printf("events: open jsonl: %v", err)
		return
	}
	defer f.Close()
	_, _ = f.Write(append(data, '\n'))
}

// notifyOrchestratorSocket sends ev as JSON to the orchestrator UDS socket.
// It is best-effort: runs in a goroutine with a 500ms timeout and never blocks the caller.
func notifyOrchestratorSocket(ev schedulerEvent) {
	go func() {
		home, err := os.UserHomeDir()
		if err != nil {
			return
		}
		socketPath := filepath.Join(home, ".alluka", "orchestrator.sock")
		data, err := json.Marshal(ev)
		if err != nil {
			return
		}
		conn, err := net.DialTimeout("unix", socketPath, 500*time.Millisecond)
		if err != nil {
			return // socket not available; best-effort
		}
		defer conn.Close()
		_ = conn.SetWriteDeadline(time.Now().Add(500 * time.Millisecond))
		_, _ = conn.Write(append(data, '\n'))
	}()
}

// stderrSnippet returns a trimmed, truncated excerpt of stderr for event payloads.
func stderrSnippet(s string) string {
	s = strings.TrimSpace(s)
	if len(s) > 200 {
		return s[:200] + "…"
	}
	return s
}

func newDaemonCmd() *cobra.Command {
	cmd := &cobra.Command{
		Use:   "daemon",
		Short: "Start the job runner daemon (polls every 30s)",
		RunE: func(cmd *cobra.Command, args []string) error {
			stop, _ := cmd.Flags().GetBool("stop")
			if stop {
				return stopDaemon()
			}
			once, _ := cmd.Flags().GetBool("once")
			notify, _ := cmd.Flags().GetBool("notify")
			return startDaemon(once, notify)
		},
	}
	cmd.Flags().Bool("stop", false, "Stop a running daemon via PID file")
	cmd.Flags().Bool("once", false, "Run one tick and exit (for testing)")
	cmd.Flags().Bool("notify", false, "Push job events to orchestrator UDS socket (~/.alluka/orchestrator.sock) in addition to writing events JSONL")
	return cmd
}

func startDaemon(once, notify bool) error {
	pidPath := filepath.Join(config.Dir(), "daemon.pid")

	// Check for existing daemon
	if pidBytes, err := os.ReadFile(pidPath); err == nil {
		pid, _ := strconv.Atoi(strings.TrimSpace(string(pidBytes)))
		if pid > 0 && processAlive(pid) {
			return fmt.Errorf("daemon already running (PID %d)", pid)
		}
		os.Remove(pidPath) // stale PID file
	}

	// Write PID file
	if err := config.EnsureDir(); err != nil {
		return err
	}
	if err := os.WriteFile(pidPath, []byte(strconv.Itoa(os.Getpid())), 0600); err != nil {
		return fmt.Errorf("writing PID file: %w", err)
	}
	defer os.Remove(pidPath)

	d, err := openDB()
	if err != nil {
		return err
	}
	defer d.Close()

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	exec := executor.New(d, cfg.Shell, 2)

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGTERM, syscall.SIGINT)
	defer stop()

	log.Printf("daemon started (PID %d), polling every 30s", os.Getpid())

	// Backfill next_run_at for any jobs missing it.
	backfillNextRunAt(ctx, d)

	// Run once immediately
	runDueJobs(ctx, d, exec, notify)

	if once {
		return nil
	}

	ticker := time.NewTicker(30 * time.Second)
	defer ticker.Stop()

	for {
		select {
		case <-ctx.Done():
			log.Println("daemon shutting down")
			return nil
		case <-ticker.C:
			runDueJobs(ctx, d, exec, notify)
		}
	}
}

// backfillNextRunAt sets next_run_at for enabled jobs that are missing it.
// One-shot types (at/delay) are excluded by ListJobsMissingNextRun.
func backfillNextRunAt(ctx context.Context, d *db.DB) {
	jobs, err := d.ListJobsMissingNextRun(ctx)
	if err != nil {
		log.Printf("backfill: error listing jobs: %v", err)
		return
	}
	for _, j := range jobs {
		var next time.Time
		switch j.ScheduleType {
		case "random":
			t, err := nextRandomTime(j.RandomWindow)
			if err != nil {
				log.Printf("backfill: job %d (%s) bad random_window %q: %v", j.ID, j.Name, j.RandomWindow, err)
				continue
			}
			next = t
		case "every":
			interval, err := time.ParseDuration(j.Schedule)
			if err != nil {
				log.Printf("backfill: job %d (%s) bad every interval %q: %v", j.ID, j.Name, j.Schedule, err)
				continue
			}
			next = time.Now().Add(interval)
		default: // "cron" or legacy empty
			t, err := cronutil.NextRun(j.Schedule)
			if err != nil {
				log.Printf("backfill: job %d (%s) bad schedule %q: %v", j.ID, j.Name, j.Schedule, err)
				continue
			}
			next = t
		}
		if err := d.SetNextRunAt(ctx, j.ID, &next); err != nil {
			log.Printf("backfill: job %d: %v", j.ID, err)
		} else {
			log.Printf("backfill: job %d (%s) next_run_at = %s", j.ID, j.Name, next.Format(time.RFC3339))
		}
	}
}

// runDueJobs finds enabled jobs whose next_run_at has passed and executes them.
func runDueJobs(ctx context.Context, d *db.DB, exec *executor.Executor, notify bool) {
	jobs, err := d.ListUpcomingJobs(ctx, 100)
	if err != nil {
		log.Printf("error listing upcoming jobs: %v", err)
		return
	}

	// Launch all due jobs concurrently; ListUpcomingJobs is sorted ASC so we can break early.
	type pending struct {
		job db.Job
		ch  <-chan executor.Result
	}

	// Build a set of already-running job IDs to avoid re-dispatching in-flight jobs.
	// Without this, a slow job whose next_run_at hasn't been updated yet would be
	// re-dispatched on the next 30-second tick.
	runningSet := make(map[int64]bool)
	for _, id := range exec.RunningJobs() {
		runningSet[id] = true
	}

	now := time.Now().UTC()
	var queue []pending
	for _, j := range jobs {
		nextRunAt := j.NextRunAt.UTC()
		if j.NextRunAt.After(now) {
			log.Printf("[debug] job %d (%s): next_run_at=%s now=%s decision=skip (not yet due)",
				j.ID, j.Name, nextRunAt.Format(time.RFC3339), now.Format(time.RFC3339))
			break // sorted by next_run_at ASC; no subsequent job is due either
		}
		if runningSet[j.ID] {
			log.Printf("[debug] job %d (%s): next_run_at=%s now=%s decision=skip (still running)",
				j.ID, j.Name, nextRunAt.Format(time.RFC3339), now.Format(time.RFC3339))
			continue
		}
		missed := now.Sub(nextRunAt) > time.Minute
		if missed {
			log.Printf("[debug] job %d (%s): next_run_at=%s now=%s decision=fire (missed, firing immediately)",
				j.ID, j.Name, nextRunAt.Format(time.RFC3339), now.Format(time.RFC3339))
		} else {
			log.Printf("[debug] job %d (%s): next_run_at=%s now=%s decision=fire",
				j.ID, j.Name, nextRunAt.Format(time.RFC3339), now.Format(time.RFC3339))
		}
		log.Printf("running job %d (%s)", j.ID, j.Name)
		queue = append(queue, pending{j, exec.Run(ctx, j)})
	}

	// Collect results, emit events, and advance next_run_at for each completed job.
	for _, p := range queue {
		result := <-p.ch
		completedAt := time.Now().UTC()

		var evType string
		if result.Err != nil || result.ExitCode != 0 {
			log.Printf("job %d (%s) failed: exit=%d err=%v", p.job.ID, p.job.Name, result.ExitCode, result.Err)
			evType = "schedule.failed"
		} else {
			log.Printf("job %d (%s) succeeded in %dms", p.job.ID, p.job.Name, result.DurationMs)
			evType = "schedule.completed"
		}

		ev := schedulerEvent{
			Type:       evType,
			JobID:      p.job.ID,
			JobName:    p.job.Name,
			Command:    p.job.Command,
			DurationMs: result.DurationMs,
			ExitCode:   result.ExitCode,
			Stderr:     stderrSnippet(result.Stderr),
			Ts:         completedAt.Format(time.RFC3339),
		}
		writeEvent(ev)
		if notify {
			notifyOrchestratorSocket(ev)
		}

		switch p.job.ScheduleType {
		case "at", "delay":
			// One-shot: clear next_run_at so the job doesn't fire again.
			log.Printf("job %d (%s): one-shot complete, clearing next_run_at", p.job.ID, p.job.Name)
			if err := d.SetNextRunAt(ctx, p.job.ID, nil); err != nil {
				log.Printf("job %d: cannot clear next_run_at: %v", p.job.ID, err)
			}

		case "every":
			// Interval job: next = completedAt + interval.
			interval, err := time.ParseDuration(p.job.Schedule)
			if err != nil {
				log.Printf("job %d: bad every interval %q: %v", p.job.ID, p.job.Name, err)
				continue
			}
			next := completedAt.Add(interval)
			if err := d.SetNextRunAt(ctx, p.job.ID, &next); err != nil {
				log.Printf("job %d: cannot update next_run_at: %v", p.job.ID, err)
			}

		case "random":
			// Random-daily job: schedule based on exit code.
			// exit 2 → pause (next_run_at = NULL)
			// exit 0 or exit 1 → random time tomorrow
			if result.ExitCode == 2 {
				log.Printf("job %d (%s): exit 2, pausing (next_run_at = NULL)", p.job.ID, p.job.Name)
				if err := d.SetNextRunAt(ctx, p.job.ID, nil); err != nil {
					log.Printf("job %d: cannot clear next_run_at: %v", p.job.ID, err)
				}
			} else {
				next, err := randomTimeTomorrow(p.job.RandomWindow)
				if err != nil {
					log.Printf("job %d: cannot compute random next run: %v", p.job.ID, err)
					continue
				}
				if err := d.SetNextRunAt(ctx, p.job.ID, &next); err != nil {
					log.Printf("job %d: cannot update next_run_at: %v", p.job.ID, err)
				}
			}

		default: // "cron" or legacy empty
			// Anchor rescheduling to the scheduled time rather than the drain time.
			// This prevents concurrent job execution from inflating next_run_at for fast jobs
			// when a slow job earlier in the queue delays the sequential drain loop.
			anchor := *p.job.NextRunAt
			next, err := cronutil.NextRunAfter(p.job.Schedule, anchor)
			if err != nil {
				log.Printf("job %d: cannot compute next run: %v", p.job.ID, err)
				continue
			}
			// If the computed next run is still in the past (daemon was down for a long time
			// and this was a missed job), roll forward from now to prevent a re-fire cascade.
			if !next.After(completedAt) {
				next, err = cronutil.NextRunAfter(p.job.Schedule, completedAt)
				if err != nil {
					log.Printf("job %d: cannot compute next run from now: %v", p.job.ID, err)
					continue
				}
				log.Printf("job %d (%s): missed run detected, rolled next_run_at forward to %s",
					p.job.ID, p.job.Name, next.Format(time.RFC3339))
			}
			if err := d.SetNextRunAt(ctx, p.job.ID, &next); err != nil {
				log.Printf("job %d: cannot update next_run_at: %v", p.job.ID, err)
			}
		}
	}
}

func stopDaemon() error {
	pidPath := filepath.Join(config.Dir(), "daemon.pid")
	pidBytes, err := os.ReadFile(pidPath)
	if err != nil {
		return fmt.Errorf("no daemon PID file found (is it running?)")
	}
	pid, _ := strconv.Atoi(strings.TrimSpace(string(pidBytes)))
	if pid <= 0 {
		return fmt.Errorf("invalid PID in %s", pidPath)
	}
	proc, err := os.FindProcess(pid)
	if err != nil {
		return fmt.Errorf("process %d not found", pid)
	}
	if err := proc.Signal(syscall.SIGTERM); err != nil {
		return fmt.Errorf("sending SIGTERM to PID %d: %w", pid, err)
	}
	fmt.Printf("sent SIGTERM to daemon (PID %d)\n", pid)
	return nil
}

func processAlive(pid int) bool {
	proc, err := os.FindProcess(pid)
	if err != nil {
		return false
	}
	return proc.Signal(syscall.Signal(0)) == nil
}
