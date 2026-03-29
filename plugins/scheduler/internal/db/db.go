// Package db manages the SQLite database for the scheduler.
// It handles connection setup, schema migrations, and provides
// typed query helpers for job and run data.
package db

import (
	"context"
	"database/sql"
	"fmt"
	"regexp"
	"time"

	_ "modernc.org/sqlite"
)

var urlRe = regexp.MustCompile(`https?://\S+`)

// DB wraps a *sql.DB and owns the scheduler schema.
type DB struct {
	db *sql.DB
}

// Open opens the SQLite database at path, creating it if it doesn't exist,
// and runs schema migrations.
func Open(path string) (*DB, error) {
	conn, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("opening database %s: %w", path, err)
	}

	// SQLite works best single-writer; set a sane timeout.
	conn.SetMaxOpenConns(1)

	d := &DB{db: conn}
	if err := d.migrate(); err != nil {
		conn.Close()
		return nil, fmt.Errorf("running migrations: %w", err)
	}
	return d, nil
}

// Close releases the database connection.
func (d *DB) Close() error {
	return d.db.Close()
}

// Ping verifies the connection is live.
func (d *DB) Ping(ctx context.Context) error {
	return d.db.PingContext(ctx)
}

// migrate applies the schema and seeds data.
func (d *DB) migrate() error {
	if _, err := d.db.Exec(schema); err != nil {
		return fmt.Errorf("applying schema: %w", err)
	}
	if _, err := d.db.Exec(seedSQL); err != nil {
		return fmt.Errorf("seeding optimal_times: %w", err)
	}

	// Add interval column if missing (v2 → v3 migration).
	if err := d.migrateIntervalColumn(); err != nil {
		return fmt.Errorf("migrating interval column: %w", err)
	}

	// Add published_url column if missing.
	if err := d.migratePublishedURLColumn(); err != nil {
		return fmt.Errorf("migrating published_url column: %w", err)
	}

	// Add random_window column to jobs if missing.
	if err := d.migrateRandomWindowColumn(); err != nil {
		return fmt.Errorf("migrating random_window column: %w", err)
	}

	// Seed substack_engage optimal times if missing.
	if _, err := d.db.Exec(seedSubstackEngageSQL); err != nil {
		return fmt.Errorf("seeding substack_engage times: %w", err)
	}

	// Seed X optimal times if missing.
	if _, err := d.db.Exec(seedXSQL); err != nil {
		return fmt.Errorf("seeding x times: %w", err)
	}

	return nil
}

// migratePublishedURLColumn adds the published_url column to posts if it doesn't exist.
func (d *DB) migratePublishedURLColumn() error {
	rows, err := d.db.Query(`PRAGMA table_info(posts)`)
	if err != nil {
		return err
	}
	defer rows.Close()

	for rows.Next() {
		var cid int
		var name, colType string
		var notNull int
		var dfltValue sql.NullString
		var pk int
		if err := rows.Scan(&cid, &name, &colType, &notNull, &dfltValue, &pk); err != nil {
			return err
		}
		if name == "published_url" {
			return nil // already migrated
		}
	}

	_, err = d.db.Exec(`ALTER TABLE posts ADD COLUMN published_url TEXT NOT NULL DEFAULT ''`)
	return err
}

// migrateRandomWindowColumn adds the random_window column to jobs if it doesn't exist.
func (d *DB) migrateRandomWindowColumn() error {
	rows, err := d.db.Query(`PRAGMA table_info(jobs)`)
	if err != nil {
		return err
	}
	defer rows.Close()

	for rows.Next() {
		var cid int
		var name, colType string
		var notNull int
		var dfltValue sql.NullString
		var pk int
		if err := rows.Scan(&cid, &name, &colType, &notNull, &dfltValue, &pk); err != nil {
			return err
		}
		if name == "random_window" {
			return nil // already migrated
		}
	}

	_, err = d.db.Exec(`ALTER TABLE jobs ADD COLUMN random_window TEXT`)
	return err
}

// migrateIntervalColumn adds the interval column to posts if it doesn't exist.
func (d *DB) migrateIntervalColumn() error {
	rows, err := d.db.Query(`PRAGMA table_info(posts)`)
	if err != nil {
		return err
	}
	defer rows.Close()

	for rows.Next() {
		var cid int
		var name, colType string
		var notNull int
		var dfltValue sql.NullString
		var pk int
		if err := rows.Scan(&cid, &name, &colType, &notNull, &dfltValue, &pk); err != nil {
			return err
		}
		if name == "interval" {
			return nil // already migrated
		}
	}

	_, err = d.db.Exec(`ALTER TABLE posts ADD COLUMN interval TEXT NOT NULL DEFAULT ''`)
	return err
}

// schema is the complete SQLite DDL for the scheduler.
// All tables use IF NOT EXISTS so it's safe to run on every startup.
const schema = `
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- jobs stores scheduled task definitions.
CREATE TABLE IF NOT EXISTS jobs (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL UNIQUE,
    command       TEXT    NOT NULL,
    schedule      TEXT    NOT NULL DEFAULT '',  -- cron expression, e.g. "*/5 * * * *"; empty for random-daily
    shell         TEXT    NOT NULL DEFAULT '/bin/sh',
    enabled       INTEGER NOT NULL DEFAULT 1,  -- 1=enabled, 0=disabled
    timeout_sec   INTEGER NOT NULL DEFAULT 0,  -- 0=no timeout
    random_window TEXT,   -- "H:MM-H:MM" for random-daily jobs, NULL for cron jobs
    last_run_at   TEXT,   -- ISO 8601, nullable
    next_run_at   TEXT,   -- ISO 8601, nullable
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- runs records each execution of a job.
CREATE TABLE IF NOT EXISTS runs (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id      INTEGER NOT NULL REFERENCES jobs(id) ON DELETE CASCADE,
    status      TEXT    NOT NULL DEFAULT 'pending', -- pending, running, success, failure, timeout
    exit_code   INTEGER,           -- NULL until finished
    stdout      TEXT    NOT NULL DEFAULT '',
    stderr      TEXT    NOT NULL DEFAULT '',
    started_at  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    finished_at TEXT,              -- NULL until finished
    duration_ms INTEGER            -- NULL until finished
);

CREATE INDEX IF NOT EXISTS idx_runs_job_id     ON runs(job_id);
CREATE INDEX IF NOT EXISTS idx_runs_started_at ON runs(started_at DESC);
CREATE INDEX IF NOT EXISTS idx_jobs_enabled    ON jobs(enabled);

-- Social content posts queue.
CREATE TABLE IF NOT EXISTS posts (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    platform     TEXT    NOT NULL,
    content      TEXT    NOT NULL,
    args         TEXT    NOT NULL DEFAULT '',
    scheduled_at TEXT    NOT NULL,
    status       TEXT    NOT NULL DEFAULT 'pending',
    run_output     TEXT    NOT NULL DEFAULT '',
    published_url  TEXT    NOT NULL DEFAULT '',
    created_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    updated_at     TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_posts_status       ON posts(status);
CREATE INDEX IF NOT EXISTS idx_posts_scheduled_at ON posts(scheduled_at);
CREATE INDEX IF NOT EXISTS idx_posts_platform     ON posts(platform);

-- Optimal posting windows per platform (seed data).
CREATE TABLE IF NOT EXISTS optimal_times (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    platform TEXT    NOT NULL,
    day      TEXT    NOT NULL,
    hour     INTEGER NOT NULL,
    minute   INTEGER NOT NULL,
    label    TEXT    NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_optimal_times_platform ON optimal_times(platform);
`

// seedSQL populates optimal_times if the table is empty.
// Runs as a separate statement after schema creation.
const seedSQL = `
INSERT INTO optimal_times (platform, day, hour, minute, label)
SELECT * FROM (
    VALUES
    ('substack_note', 'Monday',    16,  0, 'After-work reading'),
    ('substack_note', 'Monday',    20,  0, 'Evening wind-down'),
    ('substack_note', 'Tuesday',   16,  0, 'After-work reading'),
    ('substack_note', 'Tuesday',   20,  0, 'Evening wind-down'),
    ('substack_note', 'Wednesday', 16,  0, 'After-work reading'),
    ('substack_note', 'Wednesday', 20,  0, 'Evening wind-down'),
    ('substack_note', 'Thursday',  16,  0, 'After-work reading'),
    ('substack_note', 'Thursday',  20,  0, 'Evening wind-down'),
    ('substack_note', 'Friday',    16,  0, 'After-work reading'),
    ('substack_note', 'Friday',    20,  0, 'Evening wind-down'),
    ('substack_note', 'Saturday',   8,  0, 'Weekend morning read'),
    ('substack_note', 'Saturday',  10,  0, 'Late morning browse'),
    ('substack_note', 'Saturday',  17,  0, 'Evening wind-down'),
    ('substack_note', 'Saturday',  20,  0, 'Night scroll'),
    ('substack_note', 'Sunday',     8,  0, 'Weekend morning read'),
    ('substack_note', 'Sunday',    10,  0, 'Late morning browse'),
    ('substack_note', 'Sunday',    17,  0, 'Evening wind-down'),
    ('substack_note', 'Sunday',    20,  0, 'Night scroll'),
    ('linkedin', 'Tuesday',    8,  0, 'Tuesday morning commute'),
    ('linkedin', 'Tuesday',    9,  0, 'Mid-morning focus'),
    ('linkedin', 'Tuesday',   10,  0, 'Late morning peak'),
    ('linkedin', 'Wednesday',  8,  0, 'Wednesday morning'),
    ('linkedin', 'Wednesday',  9,  0, 'Mid-morning focus'),
    ('linkedin', 'Wednesday', 10,  0, 'Late morning peak'),
    ('linkedin', 'Thursday',   8,  0, 'Thursday morning'),
    ('linkedin', 'Thursday',   9,  0, 'Mid-morning focus'),
    ('linkedin', 'Thursday',  10,  0, 'Late morning peak'),
    ('reddit', 'Monday',     6,  0, 'Early morning US East'),
    ('reddit', 'Monday',     8,  0, 'Morning commute'),
    ('reddit', 'Monday',    10,  0, 'Mid-morning peak'),
    ('reddit', 'Tuesday',    6,  0, 'Early morning'),
    ('reddit', 'Tuesday',    8,  0, 'Morning commute'),
    ('reddit', 'Tuesday',   10,  0, 'Mid-morning peak'),
    ('reddit', 'Wednesday',  6,  0, 'Early morning'),
    ('reddit', 'Wednesday',  8,  0, 'Morning commute'),
    ('reddit', 'Wednesday', 10,  0, 'Mid-morning peak'),
    ('reddit', 'Thursday',   6,  0, 'Early morning'),
    ('reddit', 'Thursday',   8,  0, 'Morning commute'),
    ('reddit', 'Thursday',  10,  0, 'Mid-morning peak'),
    ('reddit', 'Friday',     6,  0, 'Early morning'),
    ('reddit', 'Friday',     8,  0, 'Morning commute'),
    ('reddit', 'Friday',    10,  0, 'Friday morning push'),
    ('reddit', 'Saturday',   8,  0, 'Weekend tech browsing'),
    ('reddit', 'Saturday',  10,  0, 'Late weekend morning'),
    ('reddit', 'Sunday',     8,  0, 'Weekend tech browsing'),
    ('reddit', 'Sunday',    10,  0, 'Late weekend morning'),
    ('substack_engage', 'Monday',    14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Tuesday',   14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Wednesday', 14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Thursday',  14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Friday',    14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Saturday',  11,  0, 'Late morning engagement'),
    ('substack_engage', 'Sunday',    11,  0, 'Late morning engagement')
) WHERE NOT EXISTS (SELECT 1 FROM optimal_times LIMIT 1);
`

// seedXSQL adds X (Twitter) optimal times if not already present.
// X engagement peaks weekday mornings and lunchtimes (US Eastern).
const seedXSQL = `
INSERT INTO optimal_times (platform, day, hour, minute, label)
SELECT * FROM (
    VALUES
    ('x', 'Monday',     8,  0, 'Monday morning scroll'),
    ('x', 'Monday',    12,  0, 'Lunch break'),
    ('x', 'Tuesday',    8,  0, 'Tuesday morning scroll'),
    ('x', 'Tuesday',   12,  0, 'Lunch break'),
    ('x', 'Tuesday',   17,  0, 'Commute home'),
    ('x', 'Wednesday',  8,  0, 'Wednesday morning'),
    ('x', 'Wednesday', 12,  0, 'Lunch break'),
    ('x', 'Wednesday', 17,  0, 'Commute home'),
    ('x', 'Thursday',   8,  0, 'Thursday morning'),
    ('x', 'Thursday',  12,  0, 'Lunch break'),
    ('x', 'Thursday',  17,  0, 'Commute home'),
    ('x', 'Friday',     8,  0, 'Friday morning'),
    ('x', 'Friday',    12,  0, 'Lunch break'),
    ('x', 'Saturday',   9,  0, 'Weekend morning'),
    ('x', 'Sunday',     9,  0, 'Weekend morning')
) WHERE NOT EXISTS (SELECT 1 FROM optimal_times WHERE platform = 'x' LIMIT 1);
`

// seedSubstackEngageSQL adds substack_engage optimal times if not already present.
const seedSubstackEngageSQL = `
INSERT INTO optimal_times (platform, day, hour, minute, label)
SELECT * FROM (
    VALUES
    ('substack_engage', 'Monday',    14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Tuesday',   14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Wednesday', 14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Thursday',  14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Friday',    14,  0, 'Afternoon engagement'),
    ('substack_engage', 'Saturday',  11,  0, 'Late morning engagement'),
    ('substack_engage', 'Sunday',    11,  0, 'Late morning engagement')
) WHERE NOT EXISTS (SELECT 1 FROM optimal_times WHERE platform = 'substack_engage' LIMIT 1);
`

// --- Job model ---

// Job represents a scheduled task definition.
type Job struct {
	ID           int64
	Name         string
	Command      string
	Schedule     string
	Shell        string
	Enabled      bool
	TimeoutSec   int
	RandomWindow string // "H:MM-H:MM" for random-daily jobs; empty for cron jobs
	LastRunAt    *time.Time
	NextRunAt    *time.Time
	CreatedAt    time.Time
	UpdatedAt    time.Time
}

// CreateJob inserts a new job record and returns its ID.
// For cron jobs, schedule is a cron expression and randomWindow is empty.
// For random-daily jobs, schedule is empty and randomWindow is "H:MM-H:MM".
func (d *DB) CreateJob(ctx context.Context, name, command, schedule, shell, randomWindow string, timeoutSec int) (int64, error) {
	if name == "" {
		return 0, fmt.Errorf("job name is required")
	}
	if command == "" {
		return 0, fmt.Errorf("job command is required")
	}
	if schedule == "" && randomWindow == "" {
		return 0, fmt.Errorf("either schedule or random_window is required")
	}
	if shell == "" {
		shell = "/bin/sh"
	}

	var randWin any
	if randomWindow != "" {
		randWin = randomWindow
	}

	res, err := d.db.ExecContext(ctx,
		`INSERT INTO jobs (name, command, schedule, shell, timeout_sec, random_window)
		 VALUES (?, ?, ?, ?, ?, ?)`,
		name, command, schedule, shell, timeoutSec, randWin,
	)
	if err != nil {
		return 0, fmt.Errorf("inserting job %q: %w", name, err)
	}
	id, err := res.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("getting job ID: %w", err)
	}
	return id, nil
}

// GetJob returns the job with the given ID.
func (d *DB) GetJob(ctx context.Context, id int64) (*Job, error) {
	row := d.db.QueryRowContext(ctx,
		`SELECT id, name, command, schedule, shell, enabled, timeout_sec,
		        random_window, last_run_at, next_run_at, created_at, updated_at
		 FROM jobs WHERE id = ?`, id)
	return scanJob(row)
}

// GetJobByName returns the job with the given name.
func (d *DB) GetJobByName(ctx context.Context, name string) (*Job, error) {
	row := d.db.QueryRowContext(ctx,
		`SELECT id, name, command, schedule, shell, enabled, timeout_sec,
		        random_window, last_run_at, next_run_at, created_at, updated_at
		 FROM jobs WHERE name = ?`, name)
	return scanJob(row)
}

// ListJobs returns all jobs ordered by name.
func (d *DB) ListJobs(ctx context.Context) ([]Job, error) {
	rows, err := d.db.QueryContext(ctx,
		`SELECT id, name, command, schedule, shell, enabled, timeout_sec,
		        random_window, last_run_at, next_run_at, created_at, updated_at
		 FROM jobs ORDER BY name`)
	if err != nil {
		return nil, fmt.Errorf("listing jobs: %w", err)
	}
	defer rows.Close()

	var jobs []Job
	for rows.Next() {
		j, err := scanJob(rows)
		if err != nil {
			return nil, err
		}
		jobs = append(jobs, *j)
	}
	return jobs, rows.Err()
}

// EnableJob sets a job's enabled flag.
func (d *DB) EnableJob(ctx context.Context, id int64, enabled bool) error {
	flag := 0
	if enabled {
		flag = 1
	}
	_, err := d.db.ExecContext(ctx,
		`UPDATE jobs SET enabled = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?`,
		flag, id)
	if err != nil {
		return fmt.Errorf("updating job %d enabled=%v: %w", id, enabled, err)
	}
	return nil
}

// SetNextRunAt updates a job's next_run_at field.
func (d *DB) SetNextRunAt(ctx context.Context, id int64, t *time.Time) error {
	var val any
	if t != nil {
		val = t.UTC().Format(time.RFC3339)
	}
	_, err := d.db.ExecContext(ctx,
		`UPDATE jobs SET next_run_at = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now') WHERE id = ?`,
		val, id)
	if err != nil {
		return fmt.Errorf("setting next_run_at for job %d: %w", id, err)
	}
	return nil
}

// DeleteJob removes a job and cascades to its runs.
func (d *DB) DeleteJob(ctx context.Context, id int64) error {
	_, err := d.db.ExecContext(ctx, `DELETE FROM jobs WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("deleting job %d: %w", id, err)
	}
	return nil
}

// scanner is satisfied by both *sql.Row and *sql.Rows.
type scanner interface {
	Scan(dest ...any) error
}

func scanJob(s scanner) (*Job, error) {
	var j Job
	var enabled int
	var randomWindow, lastRunAt, nextRunAt, createdAt, updatedAt sql.NullString

	if err := s.Scan(
		&j.ID, &j.Name, &j.Command, &j.Schedule, &j.Shell,
		&enabled, &j.TimeoutSec,
		&randomWindow, &lastRunAt, &nextRunAt, &createdAt, &updatedAt,
	); err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("job not found")
		}
		return nil, fmt.Errorf("scanning job row: %w", err)
	}

	j.Enabled = enabled == 1
	if randomWindow.Valid {
		j.RandomWindow = randomWindow.String
	}

	if lastRunAt.Valid {
		t, err := time.Parse(time.RFC3339, lastRunAt.String)
		if err == nil {
			j.LastRunAt = &t
		}
	}
	if nextRunAt.Valid {
		t, err := time.Parse(time.RFC3339, nextRunAt.String)
		if err == nil {
			j.NextRunAt = &t
		}
	}
	if createdAt.Valid {
		j.CreatedAt, _ = time.Parse(time.RFC3339, createdAt.String)
	}
	if updatedAt.Valid {
		j.UpdatedAt, _ = time.Parse(time.RFC3339, updatedAt.String)
	}
	return &j, nil
}

// --- Run model ---

// Run represents a single job execution record.
type Run struct {
	ID         int64
	JobID      int64
	Status     string
	ExitCode   *int
	Stdout     string
	Stderr     string
	StartedAt  time.Time
	FinishedAt *time.Time
	DurationMs *int64
}

// CreateRun inserts a new run record with status "running" and returns its ID.
func (d *DB) CreateRun(ctx context.Context, jobID int64) (int64, error) {
	res, err := d.db.ExecContext(ctx,
		`INSERT INTO runs (job_id, status) VALUES (?, 'running')`, jobID)
	if err != nil {
		return 0, fmt.Errorf("creating run for job %d: %w", jobID, err)
	}
	id, err := res.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("getting run ID: %w", err)
	}
	return id, nil
}

// FinishRun marks a run as complete with captured output and duration.
func (d *DB) FinishRun(ctx context.Context, id int64, status string, exitCode int, stdout, stderr string, durationMs int64) error {
	_, err := d.db.ExecContext(ctx,
		`UPDATE runs
		 SET status = ?, exit_code = ?, stdout = ?, stderr = ?,
		     finished_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
		     duration_ms = ?
		 WHERE id = ?`,
		status, exitCode, stdout, stderr, durationMs, id,
	)
	if err != nil {
		return fmt.Errorf("finishing run %d: %w", id, err)
	}
	return nil
}

// ListRuns returns the most recent runs for a job, newest first.
func (d *DB) ListRuns(ctx context.Context, jobID int64, limit int) ([]Run, error) {
	if limit <= 0 {
		limit = 20
	}
	rows, err := d.db.QueryContext(ctx,
		`SELECT id, job_id, status, exit_code, stdout, stderr, started_at, finished_at, duration_ms
		 FROM runs WHERE job_id = ? ORDER BY started_at DESC LIMIT ?`,
		jobID, limit)
	if err != nil {
		return nil, fmt.Errorf("listing runs for job %d: %w", jobID, err)
	}
	defer rows.Close()

	var runs []Run
	for rows.Next() {
		var r Run
		var exitCode sql.NullInt64
		var startedAt, finishedAt sql.NullString
		var durationMs sql.NullInt64

		if err := rows.Scan(
			&r.ID, &r.JobID, &r.Status, &exitCode,
			&r.Stdout, &r.Stderr, &startedAt, &finishedAt, &durationMs,
		); err != nil {
			return nil, fmt.Errorf("scanning run row: %w", err)
		}
		if exitCode.Valid {
			v := int(exitCode.Int64)
			r.ExitCode = &v
		}
		if startedAt.Valid {
			r.StartedAt, _ = time.Parse(time.RFC3339, startedAt.String)
		}
		if finishedAt.Valid {
			t, err := time.Parse(time.RFC3339, finishedAt.String)
			if err == nil {
				r.FinishedAt = &t
			}
		}
		if durationMs.Valid {
			r.DurationMs = &durationMs.Int64
		}
		runs = append(runs, r)
	}
	return runs, rows.Err()
}

// RunWithJobName extends Run with the job's display name, used for cross-table queries.
type RunWithJobName struct {
	Run
	JobName string
}

// ListRunsInRange returns runs that started within [start, end), with their job names.
func (d *DB) ListRunsInRange(ctx context.Context, start, end time.Time) ([]RunWithJobName, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT r.id, r.job_id, j.name, r.status, r.exit_code,
		       r.started_at, r.finished_at, r.duration_ms
		FROM runs r
		JOIN jobs j ON j.id = r.job_id
		WHERE r.started_at >= ? AND r.started_at < ?
		ORDER BY r.started_at ASC`,
		start.UTC().Format(time.RFC3339),
		end.UTC().Format(time.RFC3339),
	)
	if err != nil {
		return nil, fmt.Errorf("listing runs in range: %w", err)
	}
	defer rows.Close()

	var result []RunWithJobName
	for rows.Next() {
		var rj RunWithJobName
		var exitCode sql.NullInt64
		var startedAt, finishedAt sql.NullString
		var durationMs sql.NullInt64

		if err := rows.Scan(
			&rj.ID, &rj.JobID, &rj.JobName, &rj.Status, &exitCode,
			&startedAt, &finishedAt, &durationMs,
		); err != nil {
			return nil, fmt.Errorf("scanning run with job name: %w", err)
		}
		if exitCode.Valid {
			v := int(exitCode.Int64)
			rj.ExitCode = &v
		}
		if startedAt.Valid {
			rj.StartedAt, _ = time.Parse(time.RFC3339, startedAt.String)
		}
		if finishedAt.Valid {
			t, _ := time.Parse(time.RFC3339, finishedAt.String)
			rj.FinishedAt = &t
		}
		if durationMs.Valid {
			rj.DurationMs = &durationMs.Int64
		}
		result = append(result, rj)
	}
	return result, rows.Err()
}

// JobWithLastExitCode extends Job with the exit code from the most recent completed run.
type JobWithLastExitCode struct {
	Job
	LastExitCode *int
}

// ListJobsWithLastExitCode returns all jobs ordered by name, each annotated with
// the exit code from its most recent run (nil if the job has never run).
func (d *DB) ListJobsWithLastExitCode(ctx context.Context) ([]JobWithLastExitCode, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT j.id, j.name, j.command, j.schedule, j.shell, j.enabled, j.timeout_sec,
		       j.random_window, j.last_run_at, j.next_run_at, j.created_at, j.updated_at,
		       (SELECT r.exit_code FROM runs r WHERE r.job_id = j.id ORDER BY r.started_at DESC LIMIT 1) AS last_exit_code
		FROM jobs j
		ORDER BY j.name`)
	if err != nil {
		return nil, fmt.Errorf("listing jobs with last exit code: %w", err)
	}
	defer rows.Close()

	var jobs []JobWithLastExitCode
	for rows.Next() {
		var j Job
		var enabled int
		var randomWindow, lastRunAt, nextRunAt, createdAt, updatedAt sql.NullString
		var lastExitCode sql.NullInt64

		if err := rows.Scan(
			&j.ID, &j.Name, &j.Command, &j.Schedule, &j.Shell,
			&enabled, &j.TimeoutSec,
			&randomWindow, &lastRunAt, &nextRunAt, &createdAt, &updatedAt,
			&lastExitCode,
		); err != nil {
			return nil, fmt.Errorf("scanning job row: %w", err)
		}

		j.Enabled = enabled == 1
		if randomWindow.Valid {
			j.RandomWindow = randomWindow.String
		}
		if lastRunAt.Valid {
			t, _ := time.Parse(time.RFC3339, lastRunAt.String)
			j.LastRunAt = &t
		}
		if nextRunAt.Valid {
			t, _ := time.Parse(time.RFC3339, nextRunAt.String)
			j.NextRunAt = &t
		}
		if createdAt.Valid {
			j.CreatedAt, _ = time.Parse(time.RFC3339, createdAt.String)
		}
		if updatedAt.Valid {
			j.UpdatedAt, _ = time.Parse(time.RFC3339, updatedAt.String)
		}

		entry := JobWithLastExitCode{Job: j}
		if lastExitCode.Valid {
			v := int(lastExitCode.Int64)
			entry.LastExitCode = &v
		}
		jobs = append(jobs, entry)
	}
	return jobs, rows.Err()
}

// ListJobsMissingNextRun returns enabled jobs that have no next_run_at set.
func (d *DB) ListJobsMissingNextRun(ctx context.Context) ([]Job, error) {
	rows, err := d.db.QueryContext(ctx,
		`SELECT id, name, command, schedule, shell, enabled, timeout_sec,
		        random_window, last_run_at, next_run_at, created_at, updated_at
		 FROM jobs WHERE enabled = 1 AND next_run_at IS NULL`)
	if err != nil {
		return nil, fmt.Errorf("listing jobs missing next_run_at: %w", err)
	}
	defer rows.Close()

	var jobs []Job
	for rows.Next() {
		j, err := scanJob(rows)
		if err != nil {
			return nil, err
		}
		jobs = append(jobs, *j)
	}
	return jobs, rows.Err()
}

// ListUpcomingJobs returns enabled jobs with a next_run_at, ordered soonest first.
func (d *DB) ListUpcomingJobs(ctx context.Context, limit int) ([]Job, error) {
	if limit <= 0 {
		limit = 20
	}
	rows, err := d.db.QueryContext(ctx, `
		SELECT id, name, command, schedule, shell, enabled, timeout_sec,
		       random_window, last_run_at, next_run_at, created_at, updated_at
		FROM jobs
		WHERE enabled = 1 AND next_run_at IS NOT NULL
		ORDER BY next_run_at ASC
		LIMIT ?`, limit)
	if err != nil {
		return nil, fmt.Errorf("listing upcoming jobs: %w", err)
	}
	defer rows.Close()

	var jobs []Job
	for rows.Next() {
		j, err := scanJob(rows)
		if err != nil {
			return nil, err
		}
		jobs = append(jobs, *j)
	}
	return jobs, rows.Err()
}

// ListRecentRuns returns the most recent N runs across all jobs, newest first.
func (d *DB) ListRecentRuns(ctx context.Context, limit int) ([]RunWithJobName, error) {
	if limit <= 0 {
		limit = 20
	}
	rows, err := d.db.QueryContext(ctx, `
		SELECT r.id, r.job_id, j.name, r.status, r.exit_code,
		       r.started_at, r.finished_at, r.duration_ms
		FROM runs r
		JOIN jobs j ON j.id = r.job_id
		ORDER BY r.started_at DESC
		LIMIT ?`, limit)
	if err != nil {
		return nil, fmt.Errorf("listing recent runs: %w", err)
	}
	defer rows.Close()

	var result []RunWithJobName
	for rows.Next() {
		var rj RunWithJobName
		var exitCode sql.NullInt64
		var startedAt, finishedAt sql.NullString
		var durationMs sql.NullInt64

		if err := rows.Scan(
			&rj.ID, &rj.JobID, &rj.JobName, &rj.Status, &exitCode,
			&startedAt, &finishedAt, &durationMs,
		); err != nil {
			return nil, fmt.Errorf("scanning recent run: %w", err)
		}
		if exitCode.Valid {
			v := int(exitCode.Int64)
			rj.ExitCode = &v
		}
		if startedAt.Valid {
			rj.StartedAt, _ = time.Parse(time.RFC3339, startedAt.String)
		}
		if finishedAt.Valid {
			t, _ := time.Parse(time.RFC3339, finishedAt.String)
			rj.FinishedAt = &t
		}
		if durationMs.Valid {
			rj.DurationMs = &durationMs.Int64
		}
		result = append(result, rj)
	}
	return result, rows.Err()
}

// Stats returns aggregate counts for the doctor command.
type Stats struct {
	TotalJobs    int
	EnabledJobs  int
	TotalRuns    int
	PendingRuns  int
	RunningRuns  int
	// Post stats.
	TotalPosts   int
	PendingPosts int
	DonePosts    int
	FailedPosts  int
}

// GetStats returns a quick summary of the database state.
func (d *DB) GetStats(ctx context.Context) (Stats, error) {
	var s Stats

	row := d.db.QueryRowContext(ctx,
		`SELECT COUNT(*), COALESCE(SUM(CASE WHEN enabled = 1 THEN 1 ELSE 0 END), 0) FROM jobs`)
	if err := row.Scan(&s.TotalJobs, &s.EnabledJobs); err != nil {
		return s, fmt.Errorf("querying job stats: %w", err)
	}

	row = d.db.QueryRowContext(ctx,
		`SELECT COUNT(*),
		        COALESCE(SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END), 0),
		        COALESCE(SUM(CASE WHEN status = 'running' THEN 1 ELSE 0 END), 0)
		 FROM runs`)
	if err := row.Scan(&s.TotalRuns, &s.PendingRuns, &s.RunningRuns); err != nil {
		return s, fmt.Errorf("querying run stats: %w", err)
	}

	row = d.db.QueryRowContext(ctx,
		`SELECT COUNT(*),
		        COALESCE(SUM(CASE WHEN status = 'pending' THEN 1 ELSE 0 END), 0),
		        COALESCE(SUM(CASE WHEN status = 'done' THEN 1 ELSE 0 END), 0),
		        COALESCE(SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END), 0)
		 FROM posts`)
	if err := row.Scan(&s.TotalPosts, &s.PendingPosts, &s.DonePosts, &s.FailedPosts); err != nil {
		return s, fmt.Errorf("querying post stats: %w", err)
	}

	return s, nil
}

// --- Post model ---

// Post is a scheduled social content post.
type Post struct {
	ID          int64
	Platform    string
	Content     string
	Args        string
	ScheduledAt time.Time
	Status      string
	RunOutput    string
	PublishedURL string
	Interval     string // Go duration string for recurrence, e.g. "24h"
	CreatedAt    time.Time
	UpdatedAt    time.Time
}

// OptimalTime represents a preferred posting window for a platform.
type OptimalTime struct {
	ID       int64
	Platform string
	Day      string
	Hour     int
	Minute   int
	Label    string
}

const postColumns = `id, platform, content, args, scheduled_at, status, run_output, published_url, interval, created_at, updated_at`

func scanPost(s scanner) (*Post, error) {
	var p Post
	var scheduledAt, createdAt, updatedAt string

	if err := s.Scan(
		&p.ID, &p.Platform, &p.Content, &p.Args,
		&scheduledAt, &p.Status, &p.RunOutput, &p.PublishedURL, &p.Interval,
		&createdAt, &updatedAt,
	); err != nil {
		if err == sql.ErrNoRows {
			return nil, fmt.Errorf("post not found")
		}
		return nil, fmt.Errorf("scanning post row: %w", err)
	}

	var parseErr error
	p.ScheduledAt, parseErr = time.Parse(time.RFC3339, scheduledAt)
	if parseErr != nil {
		return nil, fmt.Errorf("parsing scheduled_at %q: %w", scheduledAt, parseErr)
	}
	p.CreatedAt, _ = time.Parse(time.RFC3339, createdAt)
	p.UpdatedAt, _ = time.Parse(time.RFC3339, updatedAt)

	return &p, nil
}

// CreatePost inserts a new post and returns its ID.
func (d *DB) CreatePost(ctx context.Context, platform, content, args, scheduledAt, interval string) (int64, error) {
	if platform == "" {
		return 0, fmt.Errorf("platform is required")
	}
	if content == "" {
		return 0, fmt.Errorf("content is required")
	}
	if scheduledAt == "" {
		return 0, fmt.Errorf("scheduled_at is required")
	}

	res, err := d.db.ExecContext(ctx,
		`INSERT INTO posts (platform, content, args, scheduled_at, interval) VALUES (?, ?, ?, ?, ?)`,
		platform, content, args, scheduledAt, interval,
	)
	if err != nil {
		return 0, fmt.Errorf("inserting post: %w", err)
	}
	id, err := res.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("getting post ID: %w", err)
	}
	return id, nil
}

// GetPost returns the post with the given ID.
func (d *DB) GetPost(ctx context.Context, id int64) (*Post, error) {
	row := d.db.QueryRowContext(ctx,
		`SELECT `+postColumns+` FROM posts WHERE id = ?`, id)
	return scanPost(row)
}

// ListPosts returns posts, optionally filtered by status. Empty status returns all.
func (d *DB) ListPosts(ctx context.Context, status string) ([]Post, error) {
	var query string
	var queryArgs []any
	if status != "" {
		query = `SELECT ` + postColumns + ` FROM posts WHERE status = ? ORDER BY scheduled_at ASC`
		queryArgs = append(queryArgs, status)
	} else {
		query = `SELECT ` + postColumns + ` FROM posts ORDER BY scheduled_at ASC`
	}

	rows, err := d.db.QueryContext(ctx, query, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("listing posts: %w", err)
	}
	defer rows.Close()

	var posts []Post
	for rows.Next() {
		p, err := scanPost(rows)
		if err != nil {
			return nil, err
		}
		posts = append(posts, *p)
	}
	return posts, rows.Err()
}

// CancelPost sets a pending post's status to cancelled.
func (d *DB) CancelPost(ctx context.Context, id int64) error {
	res, err := d.db.ExecContext(ctx,
		`UPDATE posts SET status = 'cancelled', updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		 WHERE id = ? AND status = 'pending'`, id)
	if err != nil {
		return fmt.Errorf("cancelling post %d: %w", id, err)
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		return fmt.Errorf("post %d is not pending (or does not exist)", id)
	}
	return nil
}

// DeletePost removes a post regardless of status.
func (d *DB) DeletePost(ctx context.Context, id int64) error {
	res, err := d.db.ExecContext(ctx, `DELETE FROM posts WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("deleting post %d: %w", id, err)
	}
	n, _ := res.RowsAffected()
	if n == 0 {
		return fmt.Errorf("post %d not found", id)
	}
	return nil
}

// SetPostScheduledAt updates a post's scheduled_at timestamp.
func (d *DB) SetPostScheduledAt(ctx context.Context, id int64, scheduledAt string) error {
	_, err := d.db.ExecContext(ctx,
		`UPDATE posts SET scheduled_at = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		 WHERE id = ?`, scheduledAt, id)
	if err != nil {
		return fmt.Errorf("setting post %d scheduled_at: %w", id, err)
	}
	return nil
}

// extractPublishedURL returns the last URL found in CLI output.
// Published URLs are typically the last line containing a URL.
func extractPublishedURL(output string) string {
	matches := urlRe.FindAllString(output, -1)
	if len(matches) == 0 {
		return ""
	}
	return matches[len(matches)-1]
}

// SetPostStatus updates a post's status, run_output, and published_url.
func (d *DB) SetPostStatus(ctx context.Context, id int64, status, output string) error {
	url := extractPublishedURL(output)
	_, err := d.db.ExecContext(ctx,
		`UPDATE posts SET status = ?, run_output = ?, published_url = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		 WHERE id = ?`, status, output, url, id)
	if err != nil {
		return fmt.Errorf("setting post %d status to %q: %w", id, status, err)
	}
	return nil
}

// PendingDue returns pending posts whose scheduled_at is at or before now.
// Uses Go time comparison instead of SQLite string comparison to handle timezone offsets correctly.
func (d *DB) PendingDue(ctx context.Context) ([]Post, error) {
	rows, err := d.db.QueryContext(ctx,
		`SELECT `+postColumns+` FROM posts
		 WHERE status = 'pending'
		 ORDER BY scheduled_at ASC`)
	if err != nil {
		return nil, fmt.Errorf("querying due posts: %w", err)
	}
	defer rows.Close()

	now := time.Now()
	var posts []Post
	for rows.Next() {
		p, err := scanPost(rows)
		if err != nil {
			return nil, err
		}
		if !p.ScheduledAt.After(now) {
			posts = append(posts, *p)
		}
	}
	return posts, rows.Err()
}

// ListOptimalTimes returns optimal posting windows for a platform. Empty platform returns all.
func (d *DB) ListOptimalTimes(ctx context.Context, platform string) ([]OptimalTime, error) {
	var query string
	var args []any
	if platform != "" {
		query = `SELECT id, platform, day, hour, minute, label FROM optimal_times WHERE platform = ? ORDER BY id`
		args = append(args, platform)
	} else {
		query = `SELECT id, platform, day, hour, minute, label FROM optimal_times ORDER BY platform, id`
	}

	rows, err := d.db.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, fmt.Errorf("listing optimal times: %w", err)
	}
	defer rows.Close()

	var times []OptimalTime
	for rows.Next() {
		var ot OptimalTime
		if err := rows.Scan(&ot.ID, &ot.Platform, &ot.Day, &ot.Hour, &ot.Minute, &ot.Label); err != nil {
			return nil, fmt.Errorf("scanning optimal_time row: %w", err)
		}
		times = append(times, ot)
	}
	return times, rows.Err()
}

// ListPostsInRange returns posts scheduled within [start, end).
func (d *DB) ListPostsInRange(ctx context.Context, start, end time.Time) ([]Post, error) {
	rows, err := d.db.QueryContext(ctx,
		`SELECT `+postColumns+` FROM posts
		 WHERE scheduled_at >= ? AND scheduled_at < ?
		 ORDER BY scheduled_at ASC`,
		start.UTC().Format(time.RFC3339),
		end.UTC().Format(time.RFC3339),
	)
	if err != nil {
		return nil, fmt.Errorf("listing posts in range: %w", err)
	}
	defer rows.Close()

	var posts []Post
	for rows.Next() {
		p, err := scanPost(rows)
		if err != nil {
			return nil, err
		}
		posts = append(posts, *p)
	}
	return posts, rows.Err()
}

// ListPendingPosts returns posts with pending status, ordered by scheduled_at.
func (d *DB) ListPendingPosts(ctx context.Context, limit int) ([]Post, error) {
	if limit <= 0 {
		limit = 50
	}
	rows, err := d.db.QueryContext(ctx,
		`SELECT `+postColumns+` FROM posts WHERE status = 'pending' ORDER BY scheduled_at ASC LIMIT ?`,
		limit)
	if err != nil {
		return nil, fmt.Errorf("listing pending posts: %w", err)
	}
	defer rows.Close()

	var posts []Post
	for rows.Next() {
		p, err := scanPost(rows)
		if err != nil {
			return nil, err
		}
		posts = append(posts, *p)
	}
	return posts, rows.Err()
}

// BeginTx starts a transaction.
func (d *DB) BeginTx(ctx context.Context) (*sql.Tx, error) {
	return d.db.BeginTx(ctx, nil)
}

// CreatePostTx inserts a post within a transaction.
func (d *DB) CreatePostTx(tx *sql.Tx, platform, content, args, scheduledAt, interval string) (int64, error) {
	res, err := tx.Exec(
		`INSERT INTO posts (platform, content, args, scheduled_at, interval) VALUES (?, ?, ?, ?, ?)`,
		platform, content, args, scheduledAt, interval,
	)
	if err != nil {
		return 0, fmt.Errorf("inserting post in tx: %w", err)
	}
	id, err := res.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("getting post ID: %w", err)
	}
	return id, nil
}

// SchemaVersion returns 2 if the social content tables (posts, optimal_times) exist, 1 otherwise.
func (d *DB) SchemaVersion(ctx context.Context) (int, error) {
	var count int
	err := d.db.QueryRowContext(ctx,
		`SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='posts'`,
	).Scan(&count)
	if err != nil {
		return 0, fmt.Errorf("querying schema version: %w", err)
	}
	if count > 0 {
		return 2, nil
	}
	return 1, nil
}
