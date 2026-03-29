// Package executor runs scheduled job commands and captures their output.
// It manages concurrency limits, timeouts, and writes results back to the DB.
package executor

import (
	"bytes"
	"context"
	"fmt"
	"os/exec"
	"sync"
	"time"

	"github.com/joeyhipolito/nanika-scheduler/internal/db"
)

// Executor runs jobs with a bounded concurrency limit.
type Executor struct {
	db      *db.DB
	shell   string
	sem     chan struct{} // semaphore for max concurrency
	mu      sync.Mutex
	running map[int64]context.CancelFunc // jobID -> cancel
}

// New creates an Executor. maxConcurrent=0 means unlimited.
func New(d *db.DB, shell string, maxConcurrent int) *Executor {
	var sem chan struct{}
	if maxConcurrent > 0 {
		sem = make(chan struct{}, maxConcurrent)
	}
	return &Executor{
		db:      d,
		shell:   shell,
		sem:     sem,
		running: make(map[int64]context.CancelFunc),
	}
}

// Result holds the outcome of a single job execution.
type Result struct {
	RunID      int64
	JobID      int64
	Status     string
	ExitCode   int
	Stdout     string
	Stderr     string
	DurationMs int64
	Err        error
}

// Run executes job in a new goroutine and returns a channel that receives the result.
// The caller must receive from the returned channel to avoid goroutine leaks.
func (e *Executor) Run(ctx context.Context, job db.Job) <-chan Result {
	ch := make(chan Result, 1)

	go func() {
		ch <- e.run(ctx, job)
	}()

	return ch
}

// run executes a single job synchronously and returns the Result.
func (e *Executor) run(ctx context.Context, job db.Job) Result {
	// Acquire semaphore slot.
	if e.sem != nil {
		select {
		case e.sem <- struct{}{}:
			defer func() { <-e.sem }()
		case <-ctx.Done():
			return Result{
				JobID:  job.ID,
				Status: "failure",
				Err:    fmt.Errorf("context cancelled before acquiring execution slot: %w", ctx.Err()),
			}
		}
	}

	// Apply per-job timeout on top of the parent context.
	runCtx := ctx
	var cancel context.CancelFunc
	if job.TimeoutSec > 0 {
		runCtx, cancel = context.WithTimeout(ctx, time.Duration(job.TimeoutSec)*time.Second)
		defer cancel()
	}

	// Register cancel for Stop().
	e.mu.Lock()
	e.running[job.ID] = func() {
		if cancel != nil {
			cancel()
		}
	}
	e.mu.Unlock()
	defer func() {
		e.mu.Lock()
		delete(e.running, job.ID)
		e.mu.Unlock()
	}()

	// Create the run record.
	runID, err := e.db.CreateRun(runCtx, job.ID)
	if err != nil {
		return Result{
			JobID:  job.ID,
			Status: "failure",
			Err:    fmt.Errorf("creating run record for job %d: %w", job.ID, err),
		}
	}

	shell := job.Shell
	if shell == "" {
		shell = e.shell
	}
	if shell == "" {
		shell = "/bin/sh"
	}

	start := time.Now()
	var stdout, stderr bytes.Buffer

	cmd := exec.CommandContext(runCtx, shell, "-c", job.Command)
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	runErr := cmd.Run()

	durationMs := time.Since(start).Milliseconds()
	exitCode := 0
	status := "success"

	if runErr != nil {
		if runCtx.Err() == context.DeadlineExceeded {
			status = "timeout"
		} else {
			status = "failure"
		}
		if exitErr, ok := runErr.(*exec.ExitError); ok {
			exitCode = exitErr.ExitCode()
		} else {
			exitCode = -1
		}
	}

	// Persist result — use a background context so DB write survives a cancelled runCtx.
	bgCtx := context.Background()
	if dbErr := e.db.FinishRun(bgCtx, runID, status, exitCode, stdout.String(), stderr.String(), durationMs); dbErr != nil {
		return Result{
			RunID:      runID,
			JobID:      job.ID,
			Status:     status,
			ExitCode:   exitCode,
			Stdout:     stdout.String(),
			Stderr:     stderr.String(),
			DurationMs: durationMs,
			Err:        fmt.Errorf("persisting run result: %w", dbErr),
		}
	}

	return Result{
		RunID:      runID,
		JobID:      job.ID,
		Status:     status,
		ExitCode:   exitCode,
		Stdout:     stdout.String(),
		Stderr:     stderr.String(),
		DurationMs: durationMs,
	}
}

// Stop cancels any in-progress execution for the given job ID.
func (e *Executor) Stop(jobID int64) {
	e.mu.Lock()
	defer e.mu.Unlock()
	if cancel, ok := e.running[jobID]; ok {
		cancel()
		delete(e.running, jobID)
	}
}

// RunningJobs returns the IDs of currently executing jobs.
func (e *Executor) RunningJobs() []int64 {
	e.mu.Lock()
	defer e.mu.Unlock()
	ids := make([]int64, 0, len(e.running))
	for id := range e.running {
		ids = append(ids, id)
	}
	return ids
}
