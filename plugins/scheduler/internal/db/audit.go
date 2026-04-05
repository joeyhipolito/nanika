// Package db — job audit trail.
//
// Every admin write to the jobs table (create, enable/disable, priority, delete)
// records a row in job_audit with before/after JSON snapshots plus an actor
// string derived from the environment.
//
// Background: 2026-04-06 persona drift investigation. A cron job that
// silently ran with the wrong persona for 11 days could not be traced to its
// author because the jobs table had no write history — once the job was
// removed, its created_at and full command vanished. This package closes
// that forensic gap.
//
// Daemon housekeeping writes (SetNextRunAt, last_run_at bookkeeping) are
// NOT audited: they fire on every tick and would flood the log without
// adding signal. Only changes a human or mission would make are recorded.
package db

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"time"
)

// JobAudit is one row of the job_audit table.
type JobAudit struct {
	ID         int64
	JobID      int64
	Op         string // "create" | "update" | "delete"
	BeforeJSON string // JSON-encoded *Job; empty for create
	AfterJSON  string // JSON-encoded *Job; empty for delete
	Actor      string
	TS         time.Time
}

// auditableJob is the serializable projection of a Job used for before/after
// snapshots. It intentionally excludes fields that change on every daemon
// tick (LastRunAt, NextRunAt, UpdatedAt) so audit rows stay semantically
// meaningful — they capture "what an admin set" not "what the daemon saw".
type auditableJob struct {
	ID           int64  `json:"id"`
	Name         string `json:"name"`
	Command      string `json:"command"`
	Schedule     string `json:"schedule"`
	Shell        string `json:"shell"`
	Enabled      bool   `json:"enabled"`
	TimeoutSec   int    `json:"timeout_sec"`
	ScheduleType string `json:"schedule_type"`
	RandomWindow string `json:"random_window,omitempty"`
	Priority     string `json:"priority"`
	CreatedAt    string `json:"created_at,omitempty"`
}

func snapshotJob(j *Job) string {
	if j == nil {
		return ""
	}
	created := ""
	if !j.CreatedAt.IsZero() {
		created = j.CreatedAt.UTC().Format(time.RFC3339)
	}
	snap := auditableJob{
		ID:           j.ID,
		Name:         j.Name,
		Command:      j.Command,
		Schedule:     j.Schedule,
		Shell:        j.Shell,
		Enabled:      j.Enabled,
		TimeoutSec:   j.TimeoutSec,
		ScheduleType: j.ScheduleType,
		RandomWindow: j.RandomWindow,
		Priority:     j.Priority,
		CreatedAt:    created,
	}
	b, err := json.Marshal(snap)
	if err != nil {
		// Marshalling a fixed shape cannot fail in practice; degrade to an
		// empty snapshot rather than breaking the caller's write path.
		return ""
	}
	return string(b)
}

// resolveActor returns a short string identifying who performed the write.
// Preference order:
//  1. CLAUDE_CODE_SESSION_ID (set by Claude Code worker sessions)
//  2. USER@HOSTNAME (interactive shell)
//  3. "unknown"
func resolveActor() string {
	if id := os.Getenv("CLAUDE_CODE_SESSION_ID"); id != "" {
		return "claude-session:" + id
	}
	user := os.Getenv("USER")
	if user == "" {
		user = "unknown"
	}
	host, _ := os.Hostname()
	if host == "" {
		host = "unknown"
	}
	return user + "@" + host
}

// recordJobAudit writes one audit row. Either before or after (or both)
// may be empty strings. Callers should invoke this inside the same
// transaction as the jobs-table write so the audit and the state change
// commit atomically.
func recordJobAudit(ctx context.Context, tx *sql.Tx, op string, jobID int64, before, after string) error {
	var beforeArg, afterArg any
	if before != "" {
		beforeArg = before
	}
	if after != "" {
		afterArg = after
	}
	_, err := tx.ExecContext(ctx,
		`INSERT INTO job_audit (job_id, op, before_json, after_json, actor)
		 VALUES (?, ?, ?, ?, ?)`,
		jobID, op, beforeArg, afterArg, resolveActor(),
	)
	if err != nil {
		return fmt.Errorf("writing job_audit row: %w", err)
	}
	return nil
}

// ListJobAudit returns audit rows for a single job, newest first. Pass
// jobID=0 to list audit rows across all jobs (useful for forensic sweeps
// after a job has been deleted — audit rows are not cascaded).
func (d *DB) ListJobAudit(ctx context.Context, jobID int64, limit int) ([]JobAudit, error) {
	if limit <= 0 {
		limit = 50
	}
	var rows *sql.Rows
	var err error
	if jobID > 0 {
		rows, err = d.db.QueryContext(ctx,
			`SELECT id, job_id, op, COALESCE(before_json,''), COALESCE(after_json,''), actor, ts
			 FROM job_audit WHERE job_id = ? ORDER BY id DESC LIMIT ?`,
			jobID, limit)
	} else {
		rows, err = d.db.QueryContext(ctx,
			`SELECT id, job_id, op, COALESCE(before_json,''), COALESCE(after_json,''), actor, ts
			 FROM job_audit ORDER BY id DESC LIMIT ?`,
			limit)
	}
	if err != nil {
		return nil, fmt.Errorf("listing job_audit: %w", err)
	}
	defer rows.Close()

	var out []JobAudit
	for rows.Next() {
		var a JobAudit
		var ts string
		if err := rows.Scan(&a.ID, &a.JobID, &a.Op, &a.BeforeJSON, &a.AfterJSON, &a.Actor, &ts); err != nil {
			return nil, fmt.Errorf("scanning job_audit row: %w", err)
		}
		if t, perr := time.Parse(time.RFC3339, ts); perr == nil {
			a.TS = t
		}
		out = append(out, a)
	}
	return out, rows.Err()
}
