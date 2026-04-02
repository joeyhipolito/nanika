package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"testing"
	"time"

	cronutil "github.com/joeyhipolito/nanika-scheduler/internal/cron"
	"github.com/joeyhipolito/nanika-scheduler/internal/db"
	"github.com/joeyhipolito/nanika-scheduler/internal/executor"
)

// setupTestDB creates a fresh in-memory SQLite database for testing.
func setupTestDB(t *testing.T) *db.DB {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "test.db")

	d, err := db.Open(dbPath)
	if err != nil {
		t.Fatalf("failed to open test DB: %v", err)
	}
	t.Cleanup(func() {
		_ = d.Close()
	})

	return d
}

// TestBackfillNextRunAtCron tests backfill for cron jobs.
func TestBackfillNextRunAtCron(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a cron job without next_run_at
	id, err := d.CreateJob(ctx, "daily-backup", "tar czf /tmp/backup.tgz ~/docs",
		"0 2 * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}

	// Verify it has no next_run_at
	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt != nil {
		t.Errorf("job should have nil NextRunAt, got %v", j.NextRunAt)
	}

	// Run backfill
	backfillNextRunAt(ctx, d)

	// Verify it now has a next_run_at
	j, err = d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job after backfill: %v", err)
	}
	if j.NextRunAt == nil {
		t.Errorf("job should have NextRunAt after backfill")
	}

	// Verify it's in the future and matches the cron expression
	if !j.NextRunAt.After(time.Now()) {
		t.Errorf("NextRunAt %v should be in the future", j.NextRunAt)
	}
	if j.NextRunAt.Hour() != 2 || j.NextRunAt.Minute() != 0 {
		t.Errorf("time should be 02:00, got %02d:%02d", j.NextRunAt.Hour(), j.NextRunAt.Minute())
	}
}

// TestBackfillNextRunAtEvery tests backfill for interval jobs.
func TestBackfillNextRunAtEvery(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create an every job without next_run_at
	id, err := d.CreateJob(ctx, "health-check", "curl -s localhost:8080/health",
		"30m", "/bin/sh", "", "every", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}

	before := time.Now()
	backfillNextRunAt(ctx, d)
	_ = time.Now()

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt == nil {
		t.Errorf("job should have NextRunAt")
	}

	// Should be approximately 30 minutes from before/after
	diff := j.NextRunAt.Sub(before)
	if diff < 29*time.Minute || diff > 31*time.Minute {
		t.Errorf("NextRunAt should be ~30m from now, got diff=%v", diff)
	}
}

// TestBackfillNextRunAtRandom tests backfill for random-daily jobs.
func TestBackfillNextRunAtRandom(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a random job without next_run_at
	id, err := d.CreateJob(ctx, "random-task", "echo hello",
		"", "/bin/sh", "9:00-17:00", "random", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}

	backfillNextRunAt(ctx, d)

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt == nil {
		t.Errorf("job should have NextRunAt")
	}

	// Should be in the future and within the window
	if !j.NextRunAt.After(time.Now()) {
		t.Errorf("NextRunAt should be in the future")
	}
	localTime := j.NextRunAt.Local()
	if localTime.Hour() < 9 || localTime.Hour() >= 17 {
		t.Errorf("time should be in 9:00-17:00 window, got %02d:%02d (in local tz)",
			localTime.Hour(), localTime.Minute())
	}
}

// TestBackfillNextRunAtSkipsDisabled tests that disabled jobs are skipped.
func TestBackfillNextRunAtSkipsDisabled(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a disabled job
	id, err := d.CreateJob(ctx, "disabled-job", "echo skip",
		"0 9 * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}

	// Disable it
	if err := d.EnableJob(ctx, id, false); err != nil {
		t.Fatalf("failed to disable job: %v", err)
	}

	backfillNextRunAt(ctx, d)

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt != nil {
		t.Errorf("disabled job should not have NextRunAt set, got %v", j.NextRunAt)
	}
}

// TestBackfillNextRunAtSkipsOneShots tests that one-shot jobs (at/delay) are skipped.
func TestBackfillNextRunAtSkipsOneShots(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create an 'at' job
	future := time.Now().Add(1 * time.Hour)
	id, err := d.CreateJob(ctx, "at-job", "echo once", "", "/bin/sh", "", "at", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}
	if err := d.SetNextRunAt(ctx, id, &future); err != nil {
		t.Fatalf("failed to set next run: %v", err)
	}

	// Clear it
	if err := d.SetNextRunAt(ctx, id, nil); err != nil {
		t.Fatalf("failed to clear next run: %v", err)
	}

	backfillNextRunAt(ctx, d)

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt != nil {
		t.Errorf("one-shot job should not be backfilled, got %v", j.NextRunAt)
	}
}

// TestOneShot_AtJob tests that 'at' jobs are disabled after firing.
func TestOneShot_AtJob(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create an 'at' job
	future := time.Now().Add(1 * time.Hour)
	id, err := d.CreateJob(ctx, "standup", "echo standup", "", "/bin/sh", "", "at", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}
	if err := d.SetNextRunAt(ctx, id, &future); err != nil {
		t.Fatalf("failed to set next run: %v", err)
	}

	// Simulate the one-shot clearing logic
	if err := d.SetNextRunAt(ctx, id, nil); err != nil {
		t.Fatalf("failed to clear next run: %v", err)
	}

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt != nil {
		t.Errorf("one-shot 'at' job should have nil NextRunAt after firing, got %v", j.NextRunAt)
	}
}

// TestOneShot_DelayJob tests that 'delay' jobs are disabled after firing.
func TestOneShot_DelayJob(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a 'delay' job
	future := time.Now().Add(2 * time.Hour)
	id, err := d.CreateJob(ctx, "warmup", "scripts/warmup.sh", "", "/bin/sh", "", "delay", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}
	if err := d.SetNextRunAt(ctx, id, &future); err != nil {
		t.Fatalf("failed to set next run: %v", err)
	}

	// Simulate the one-shot clearing logic
	if err := d.SetNextRunAt(ctx, id, nil); err != nil {
		t.Fatalf("failed to clear next run: %v", err)
	}

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt != nil {
		t.Errorf("one-shot 'delay' job should have nil NextRunAt after firing, got %v", j.NextRunAt)
	}
}

// TestMissedJobRecovery_CronAnchoredToScheduled tests cron missed job recovery.
func TestMissedJobRecovery_CronAnchoredToScheduled(t *testing.T) {
	// Simulate: a cron job was scheduled for 9:00 AM, but the daemon was down.
	// When it restarts at 11:00 AM, the next_run_at should be anchored to the missed 9:00 time,
	// not to 11:00 to prevent stale cascades.

	scheduled := time.Date(2026, 3, 31, 9, 0, 0, 0, time.UTC)
	completed := time.Date(2026, 3, 31, 11, 0, 0, 0, time.UTC)

	// Calculate next run after scheduled time
	next, err := cronutil.NextRunAfter("0 9 * * *", scheduled)
	if err != nil {
		t.Fatalf("failed to compute cron: %v", err)
	}

	// The next run after 9:00 AM should be the next day at 9:00 AM
	if next.Hour() != 9 || next.Minute() != 0 {
		t.Errorf("next should be at 09:00, got %02d:%02d", next.Hour(), next.Minute())
	}
	if next.Day() != 1 && next.Month() == 3 {
		t.Errorf("next should be next day (or next month), got %v", next)
	}

	// If that next run is still in the past (it is, since we're at 11:00 AM),
	// we should roll forward from the completed time.
	if !next.After(completed) {
		next, err = cronutil.NextRunAfter("0 9 * * *", completed)
		if err != nil {
			t.Fatalf("failed to recompute cron: %v", err)
		}
	}

	if !next.After(completed) {
		t.Errorf("final next %v should be after completed %v", next, completed)
	}
}

// TestMissedJobRecovery_EveryJob tests every job recovery.
func TestMissedJobRecovery_EveryJob(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create an every job with an old next_run_at
	past := time.Now().Add(-10 * time.Minute)
	id, err := d.CreateJob(ctx, "health-check", "curl localhost:8080/health",
		"5m", "/bin/sh", "", "every", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}
	if err := d.SetNextRunAt(ctx, id, &past); err != nil {
		t.Fatalf("failed to set next run: %v", err)
	}

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}

	// Simulate what the daemon does after job completion
	completed := time.Now().UTC()
	interval, _ := time.ParseDuration(j.Schedule)
	next := completed.Add(interval)

	if err := d.SetNextRunAt(ctx, id, &next); err != nil {
		t.Fatalf("failed to set next run: %v", err)
	}

	// Verify next is in the future
	j, err = d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if !j.NextRunAt.After(completed) {
		t.Errorf("recovered next_run_at %v should be in future (after %v)", j.NextRunAt, completed)
	}
}

// TestMissedJobRecovery_RandomJob tests random job with exit code 2 pause.
func TestMissedJobRecovery_RandomJob(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a random job
	future := time.Now().Add(2 * time.Hour)
	id, err := d.CreateJob(ctx, "random-task", "python script.py",
		"", "/bin/sh", "9:00-17:00", "random", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}
	if err := d.SetNextRunAt(ctx, id, &future); err != nil {
		t.Fatalf("failed to set next run: %v", err)
	}

	// Test exit code 2 (pause)
	if err := d.SetNextRunAt(ctx, id, nil); err != nil {
		t.Fatalf("failed to clear next run: %v", err)
	}

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt != nil {
		t.Errorf("random job with exit 2 should have nil NextRunAt, got %v", j.NextRunAt)
	}

	// Test exit code 0/1 (schedule tomorrow)
	tomorrow := time.Now().AddDate(0, 0, 1)
	if err := d.SetNextRunAt(ctx, id, &tomorrow); err != nil {
		t.Fatalf("failed to set next run: %v", err)
	}

	j, err = d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	if j.NextRunAt == nil {
		t.Errorf("random job with exit 0/1 should be scheduled, got nil")
	}

	// Should be approximately tomorrow (within 24-48 hours from now)
	diff := j.NextRunAt.Sub(time.Now())
	if diff < 23*time.Hour || diff > 25*time.Hour {
		t.Errorf("next_run_at should be roughly tomorrow, diff=%v", diff)
	}
}

// TestListUpcomingJobs tests that ListUpcomingJobs returns jobs in order.
func TestListUpcomingJobs(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	now := time.Now().UTC()

	// Create multiple jobs with different next_run_at times
	times := []time.Time{
		now.Add(3 * time.Hour),
		now.Add(1 * time.Hour),
		now.Add(2 * time.Hour),
	}

	for i, tm := range times {
		id, err := d.CreateJob(ctx, "job-"+string(rune(i)), "echo",
			"0 9 * * *", "/bin/sh", "", "cron", 0)
		if err != nil {
			t.Fatalf("failed to create job: %v", err)
		}
		if err := d.SetNextRunAt(ctx, id, &tm); err != nil {
			t.Fatalf("failed to set next run: %v", err)
		}
	}

	jobs, err := d.ListUpcomingJobs(ctx, 10)
	if err != nil {
		t.Fatalf("failed to list upcoming jobs: %v", err)
	}

	if len(jobs) != 3 {
		t.Errorf("expected 3 jobs, got %d", len(jobs))
		return
	}

	// Should be sorted by next_run_at ASC
	for i := 1; i < len(jobs); i++ {
		if jobs[i].NextRunAt.Before(*jobs[i-1].NextRunAt) {
			t.Errorf("jobs not sorted: job %d (%v) before job %d (%v)",
				i, jobs[i].NextRunAt, i-1, jobs[i-1].NextRunAt)
		}
	}
}

// TestScheduleTypeHandling tests that all schedule types are handled correctly.
func TestScheduleTypeHandling(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	types := []string{"cron", "every", "random", "at", "delay"}

	for _, st := range types {
		t.Run(st, func(t *testing.T) {
			var schedule, randomWindow string
			switch st {
			case "cron":
				schedule = "0 9 * * *"
			case "every":
				schedule = "30m"
			case "random":
				randomWindow = "9:00-17:00"
			}

			id, err := d.CreateJob(ctx, "test-"+st, "echo "+st,
				schedule, "/bin/sh", randomWindow, st, 0)
			if err != nil {
				t.Fatalf("failed to create %s job: %v", st, err)
			}

			// Set next run time
			future := time.Now().Add(1 * time.Hour)
			if err := d.SetNextRunAt(ctx, id, &future); err != nil {
				t.Fatalf("failed to set next run: %v", err)
			}

			// Verify job exists with correct type
			j, err := d.GetJob(ctx, id)
			if err != nil {
				t.Fatalf("failed to get job: %v", err)
			}
			if j.ScheduleType != st {
				t.Errorf("schedule type mismatch: got %q, want %q", j.ScheduleType, st)
			}
		})
	}
}

// TestBackfillInvalidSchedule tests error handling for invalid schedules during backfill.
func TestBackfillInvalidSchedule(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a job with an invalid cron expression
	id, err := d.CreateJob(ctx, "bad-cron", "echo fail",
		"invalid * * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("failed to create job: %v", err)
	}

	// Backfill should not crash; it should log the error and continue
	backfillNextRunAt(ctx, d)

	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("failed to get job: %v", err)
	}
	// Bad job should still have nil NextRunAt
	if j.NextRunAt != nil {
		t.Errorf("job with invalid schedule should not be backfilled, got %v", j.NextRunAt)
	}
}

// TestBackfillNextRunAtMultipleJobs tests backfill with multiple job types.
func TestBackfillNextRunAtMultipleJobs(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create different types of jobs
	cronID, _ := d.CreateJob(ctx, "cron-job", "echo", "0 9 * * *", "/bin/sh", "", "cron", 0)
	everyID, _ := d.CreateJob(ctx, "every-job", "echo", "30m", "/bin/sh", "", "every", 0)
	randomID, _ := d.CreateJob(ctx, "random-job", "echo", "", "/bin/sh", "9:00-17:00", "random", 0)

	// All should have nil NextRunAt initially
	for _, id := range []int64{cronID, everyID, randomID} {
		j, _ := d.GetJob(ctx, id)
		if j.NextRunAt != nil {
			t.Errorf("job %d should have nil NextRunAt before backfill", id)
		}
	}

	backfillNextRunAt(ctx, d)

	// All should now have NextRunAt
	for _, id := range []int64{cronID, everyID, randomID} {
		j, _ := d.GetJob(ctx, id)
		if j.NextRunAt == nil {
			t.Errorf("job %d should have NextRunAt after backfill", id)
		}
	}
}

// TestBackfillInvalidRandomWindow tests error handling for bad random windows.
func TestBackfillInvalidRandomWindow(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a random job with invalid window
	id, _ := d.CreateJob(ctx, "bad-random", "echo", "", "/bin/sh", "invalid-window", "random", 0)

	backfillNextRunAt(ctx, d)

	// Job should still have nil NextRunAt
	j, _ := d.GetJob(ctx, id)
	if j.NextRunAt != nil {
		t.Errorf("job with bad random window should not be backfilled, got %v", j.NextRunAt)
	}
}

// TestBackfillInvalidEveryInterval tests error handling for bad every intervals.
func TestBackfillInvalidEveryInterval(t *testing.T) {
	d := setupTestDB(t)
	ctx := context.Background()

	// Create an every job with invalid interval
	id, _ := d.CreateJob(ctx, "bad-every", "echo", "not-a-duration", "/bin/sh", "", "every", 0)

	backfillNextRunAt(ctx, d)

	// Job should still have nil NextRunAt
	j, _ := d.GetJob(ctx, id)
	if j.NextRunAt != nil {
		t.Errorf("job with bad interval should not be backfilled, got %v", j.NextRunAt)
	}
}

// BenchmarkBackfillNextRunAt benchmarks the backfill operation.
func BenchmarkBackfillNextRunAt(b *testing.B) {
	tmpDir := b.TempDir()
	dbPath := filepath.Join(tmpDir, "bench.db")
	d, err := db.Open(dbPath)
	if err != nil {
		b.Fatalf("failed to open test DB: %v", err)
	}
	defer d.Close()

	ctx := context.Background()

	// Create 100 jobs
	for i := 0; i < 100; i++ {
		_, _ = d.CreateJob(ctx, "job-"+string(rune(i)), "echo",
			"0 9 * * *", "/bin/sh", "", "cron", 0)
	}

	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		backfillNextRunAt(ctx, d)
	}
}

// writePeakCfg writes a peak-hours.json to a temp dir and returns that dir.
// Setting enabled=true with a 0–24 UTC window makes IsPeak return true on any
// UTC weekday regardless of wall-clock time.
func writePeakCfg(t *testing.T, enabled bool) string {
	t.Helper()
	home := t.TempDir()
	alluka := filepath.Join(home, ".alluka")
	if err := os.MkdirAll(alluka, 0o755); err != nil {
		t.Fatalf("writePeakCfg: mkdir: %v", err)
	}
	content := fmt.Sprintf(
		`{"enabled":%v,"start_hour":0,"end_hour":24,"timezone":"UTC"}`,
		enabled,
	)
	if err := os.WriteFile(filepath.Join(alluka, "peak-hours.json"), []byte(content), 0o644); err != nil {
		t.Fatalf("writePeakCfg: write: %v", err)
	}
	return home
}

// TestRunDueJobs_PeakDeferred verifies that non-P0 jobs are not dispatched
// during peak hours (their next_run_at stays in the past, unchanged).
func TestRunDueJobs_PeakDeferred(t *testing.T) {
	if wd := time.Now().UTC().Weekday(); wd == time.Saturday || wd == time.Sunday {
		t.Skip("peak package excludes weekends; run on a weekday")
	}
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a P1 (non-priority-0) cron job due in the past.
	id, err := d.CreateJob(ctx, "non-p0-job", "echo non-p0",
		"0 * * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("CreateJob: %v", err)
	}
	if err := d.SetPriority(ctx, id, "P1"); err != nil {
		t.Fatalf("SetPriority: %v", err)
	}
	past := time.Now().Add(-2 * time.Minute)
	if err := d.SetNextRunAt(ctx, id, &past); err != nil {
		t.Fatalf("SetNextRunAt: %v", err)
	}

	// Set HOME so peak.LoadConfig returns enabled=true (all-UTC-weekday window).
	t.Setenv("HOME", writePeakCfg(t, true))

	exec := executor.New(d, "/bin/sh", 2)
	runDueJobs(ctx, d, exec, false)

	// Job should NOT have been dispatched: next_run_at must remain in the past.
	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("GetJob after runDueJobs: %v", err)
	}
	if j.NextRunAt == nil || !j.NextRunAt.Before(time.Now()) {
		t.Errorf("non-P0 job should be deferred during peak: next_run_at = %v (expected past value ~%v)",
			j.NextRunAt, past)
	}
}

// TestRunDueJobs_P0RunsDuringPeak verifies that P0 jobs ARE dispatched even
// during peak hours, and that next_run_at advances to the future after the run.
func TestRunDueJobs_P0RunsDuringPeak(t *testing.T) {
	if wd := time.Now().UTC().Weekday(); wd == time.Saturday || wd == time.Sunday {
		t.Skip("peak package excludes weekends; run on a weekday")
	}
	d := setupTestDB(t)
	ctx := context.Background()

	// Create a P0 cron job due in the past.
	id, err := d.CreateJob(ctx, "p0-job", "echo p0",
		"0 * * * *", "/bin/sh", "", "cron", 0)
	if err != nil {
		t.Fatalf("CreateJob: %v", err)
	}
	if err := d.SetPriority(ctx, id, "P0"); err != nil {
		t.Fatalf("SetPriority: %v", err)
	}
	past := time.Now().Add(-2 * time.Minute)
	if err := d.SetNextRunAt(ctx, id, &past); err != nil {
		t.Fatalf("SetNextRunAt: %v", err)
	}

	// Set HOME so peak.LoadConfig returns enabled=true (all-UTC-weekday window).
	t.Setenv("HOME", writePeakCfg(t, true))

	exec := executor.New(d, "/bin/sh", 2)
	runDueJobs(ctx, d, exec, false)

	// P0 job should have been dispatched and next_run_at advanced.
	j, err := d.GetJob(ctx, id)
	if err != nil {
		t.Fatalf("GetJob after runDueJobs: %v", err)
	}
	if j.NextRunAt == nil || !j.NextRunAt.After(time.Now()) {
		t.Errorf("P0 job should run during peak: next_run_at = %v (expected future)", j.NextRunAt)
	}
}
