package preflight

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	_ "modernc.org/sqlite"
)

func init() {
	Register(&schedulerSection{})
}

// schedulerSection surfaces overdue or failed scheduler jobs from the
// scheduler SQLite database. It reads the DB read-only and never writes.
type schedulerSection struct{}

func (s *schedulerSection) Name() string  { return "scheduler" }
func (s *schedulerSection) Priority() int { return 10 }

// Fetch queries the scheduler DB for enabled jobs that are overdue (next_run_at
// < now) or whose last run failed. Returns an empty Block when the DB is absent
// (fresh install) or when all jobs are healthy.
func (s *schedulerSection) Fetch(ctx context.Context) (Block, error) {
	dbPath := schedulerDBPath()

	if _, err := os.Stat(dbPath); errors.Is(err, os.ErrNotExist) {
		return Block{Title: "Scheduler Jobs"}, nil
	}

	db, err := sql.Open("sqlite", dbPath+"?mode=ro")
	if err != nil {
		return Block{}, fmt.Errorf("opening scheduler db: %w", err)
	}
	defer db.Close()

	rows, err := db.QueryContext(ctx, `
		SELECT
			j.name,
			COALESCE(j.next_run_at, '') AS next_run_at,
			COALESCE(r.status, '')      AS last_status,
			CASE WHEN j.next_run_at IS NOT NULL
			          AND j.next_run_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
			     THEN 1 ELSE 0 END      AS overdue
		FROM jobs j
		LEFT JOIN runs r ON r.id = (
			SELECT id FROM runs WHERE job_id = j.id ORDER BY started_at DESC, id DESC LIMIT 1
		)
		WHERE j.enabled = 1
		  AND (
			(j.next_run_at IS NOT NULL AND j.next_run_at < strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
			OR r.status IN ('failure', 'timeout')
		  )
		ORDER BY j.next_run_at ASC
		LIMIT 20
	`)
	if err != nil {
		return Block{}, fmt.Errorf("querying scheduler jobs: %w", err)
	}
	defer rows.Close()

	type jobRow struct {
		name       string
		nextRunAt  string
		lastStatus string
		overdue    bool
	}

	var jobs []jobRow
	for rows.Next() {
		var jr jobRow
		var overdueInt int
		if err := rows.Scan(&jr.name, &jr.nextRunAt, &jr.lastStatus, &overdueInt); err != nil {
			return Block{}, fmt.Errorf("scanning scheduler row: %w", err)
		}
		jr.overdue = overdueInt == 1
		jobs = append(jobs, jr)
	}
	if err := rows.Err(); err != nil {
		return Block{}, fmt.Errorf("iterating scheduler rows: %w", err)
	}

	if len(jobs) == 0 {
		return Block{Title: "Scheduler Jobs"}, nil
	}

	var sb strings.Builder
	for _, jr := range jobs {
		tag := jobTag(jr.overdue, jr.lastStatus)
		if jr.nextRunAt != "" {
			fmt.Fprintf(&sb, "- [%s] %s (next: %s)\n", tag, jr.name, jr.nextRunAt)
		} else {
			fmt.Fprintf(&sb, "- [%s] %s\n", tag, jr.name)
		}
	}

	return Block{
		Title: "Scheduler Jobs",
		Body:  strings.TrimRight(sb.String(), "\n"),
	}, nil
}

// jobTag returns a concise tag describing why the job was flagged.
func jobTag(overdue bool, lastStatus string) string {
	failed := lastStatus == "failure" || lastStatus == "timeout"
	switch {
	case overdue && failed:
		return "overdue+failed"
	case overdue:
		return "overdue"
	default:
		return "failed"
	}
}

// schedulerDBPath returns the path to the scheduler SQLite database.
// Resolution order:
//  1. SCHEDULER_DB env var (test override)
//  2. $ALLUKA_HOME/scheduler/scheduler.db
//  3. ~/.scheduler/scheduler.db (legacy)
func schedulerDBPath() string {
	if v := os.Getenv("SCHEDULER_DB"); v != "" {
		return v
	}
	home, err := os.UserHomeDir()
	if err != nil {
		home = os.Getenv("HOME")
	}
	if v := os.Getenv("ALLUKA_HOME"); v != "" {
		return filepath.Join(v, "scheduler", "scheduler.db")
	}
	return filepath.Join(home, ".scheduler", "scheduler.db")
}
