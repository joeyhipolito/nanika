package ko

// Eval history storage.
//
// DB backs the `ko evaluate <config.yaml>` subcommand. It owns the two
// tables that record LLM-as-judge verdicts produced by running YAML eval
// configs through ko.Runner:
//
//	eval_runs      — one row per `ko evaluate` invocation (config, model, totals, cost)
//	eval_results   — one row per test case within a run (passed, output, assertions, tokens)
//
// Canonical location is ~/.alluka/ko-history.db (DefaultDBPath). The
// nen-subdir stub at ~/.alluka/nen/ko-history.db is a historical zero-byte
// orphan from an earlier layout — readers should target the top-level path.
//
// # eval_results is NOT proposal_quality
//
// The `proposal_quality` table lives in a different DB file
// (~/.alluka/nen/proposals.db) and is owned by ko.QualityStore in
// quality.go. It is populated by a different subcommand
// (`ko evaluate-proposals`) from a different input set
// (shu proposals ⋈ tracker issues). There is no data flow from
// eval_results into proposal_quality. An empty proposal_quality table
// does not imply a wiring bug in this file. See the package-level
// comment in quality.go for the full split.

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

// DB wraps a SQLite connection for eval history storage.
type DB struct {
	db *sql.DB
}

// DefaultDBPath returns ~/.alluka/ko-history.db.
func DefaultDBPath() string {
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "ko-history.db")
}

// OpenDB opens (or creates) the SQLite database at path.
// A leading ~/ is expanded to the user home directory.
func OpenDB(path string) (*DB, error) {
	if strings.HasPrefix(path, "~/") {
		home, _ := os.UserHomeDir()
		path = filepath.Join(home, path[2:])
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return nil, fmt.Errorf("create db dir: %w", err)
	}
	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("open db: %w", err)
	}
	if err := migrateKoDB(db); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate: %w", err)
	}
	return &DB{db: db}, nil
}

func migrateKoDB(db *sql.DB) error {
	_, err := db.Exec(`
		CREATE TABLE IF NOT EXISTS eval_runs (
			id          TEXT    PRIMARY KEY,
			config_path TEXT    NOT NULL,
			description TEXT    NOT NULL DEFAULT '',
			model       TEXT    NOT NULL DEFAULT '',
			started_at  TEXT    NOT NULL,
			finished_at TEXT,
			total       INTEGER NOT NULL DEFAULT 0,
			passed      INTEGER NOT NULL DEFAULT 0,
			failed      INTEGER NOT NULL DEFAULT 0
		);
		CREATE TABLE IF NOT EXISTS eval_results (
			id               INTEGER PRIMARY KEY AUTOINCREMENT,
			run_id           TEXT    NOT NULL REFERENCES eval_runs(id),
			test_description TEXT    NOT NULL DEFAULT '',
			passed           INTEGER NOT NULL,
			output           TEXT    NOT NULL DEFAULT '',
			error            TEXT    NOT NULL DEFAULT '',
			duration_ms      INTEGER NOT NULL DEFAULT 0,
			assertions_json  TEXT    NOT NULL DEFAULT '[]',
			created_at       TEXT    NOT NULL
		);
		CREATE INDEX IF NOT EXISTS idx_eval_results_run ON eval_results(run_id);
		CREATE INDEX IF NOT EXISTS idx_eval_runs_config  ON eval_runs(config_path, started_at);
	`)
	if err != nil {
		return err
	}
	return migrateKoDBv2(db)
}

// migrateKoDBv2 adds cost/token columns to existing tables.
// ALTER TABLE ADD COLUMN is idempotent: we ignore "duplicate column name" errors.
func migrateKoDBv2(db *sql.DB) error {
	cols := []string{
		`ALTER TABLE eval_results ADD COLUMN input_tokens  INTEGER NOT NULL DEFAULT 0`,
		`ALTER TABLE eval_results ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0`,
		`ALTER TABLE eval_results ADD COLUMN cost_usd      REAL    NOT NULL DEFAULT 0`,
		`ALTER TABLE eval_results ADD COLUMN cache_hit     INTEGER NOT NULL DEFAULT 0`,
		`ALTER TABLE eval_runs    ADD COLUMN input_tokens  INTEGER NOT NULL DEFAULT 0`,
		`ALTER TABLE eval_runs    ADD COLUMN output_tokens INTEGER NOT NULL DEFAULT 0`,
		`ALTER TABLE eval_runs    ADD COLUMN cost_usd      REAL    NOT NULL DEFAULT 0`,
	}
	for _, stmt := range cols {
		if _, err := db.Exec(stmt); err != nil {
			if !strings.Contains(err.Error(), "duplicate column name") {
				return fmt.Errorf("migrate v2: %w", err)
			}
		}
	}
	return nil
}

// CreateRun inserts a new run record before tests start.
func (d *DB) CreateRun(ctx context.Context, runID, configPath, description, model string) error {
	_, err := d.db.ExecContext(ctx, `
		INSERT INTO eval_runs (id, config_path, description, model, started_at)
		VALUES (?, ?, ?, ?, ?)`,
		runID, configPath, description, model,
		time.Now().UTC().Format(time.RFC3339),
	)
	return err
}

// InsertResult persists a single test result immediately after it completes.
func (d *DB) InsertResult(ctx context.Context, runID string, r *TestResult) error {
	assertionsJSON, _ := json.Marshal(r.Assertions)
	passed := 0
	if r.Passed {
		passed = 1
	}
	cacheHit := 0
	if r.CacheHit {
		cacheHit = 1
	}
	_, err := d.db.ExecContext(ctx, `
		INSERT INTO eval_results
			(run_id, test_description, passed, output, error, duration_ms,
			 assertions_json, created_at, input_tokens, output_tokens, cost_usd, cache_hit)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
		runID, r.Description, passed, r.Output, r.Error,
		r.DurationMs, string(assertionsJSON),
		time.Now().UTC().Format(time.RFC3339),
		r.InputTokens, r.OutputTokens, r.CostUSD, cacheHit,
	)
	return err
}

// FinishRun updates the run record with final aggregate counts and cost.
func (d *DB) FinishRun(ctx context.Context, runID string, total, passed, failed int, inputTokens, outputTokens int, costUSD float64) error {
	_, err := d.db.ExecContext(ctx, `
		UPDATE eval_runs
		SET finished_at = ?, total = ?, passed = ?, failed = ?,
		    input_tokens = ?, output_tokens = ?, cost_usd = ?
		WHERE id = ?`,
		time.Now().UTC().Format(time.RFC3339), total, passed, failed,
		inputTokens, outputTokens, costUSD, runID,
	)
	return err
}

// EvalRunSummary is a flattened row from eval_runs.
type EvalRunSummary struct {
	ID           string
	ConfigPath   string
	Description  string
	Model        string
	StartedAt    string
	FinishedAt   string
	Total        int
	Passed       int
	Failed       int
	InputTokens  int
	OutputTokens int
	CostUSD      float64
}

// ListRuns returns recent eval runs ordered by start time descending.
// If configPath is non-empty the results are filtered to that config.
func (d *DB) ListRuns(ctx context.Context, configPath string, limit int) ([]EvalRunSummary, error) {
	var (
		rows *sql.Rows
		err  error
	)
	if configPath != "" {
		rows, err = d.db.QueryContext(ctx, `
			SELECT id, config_path, description, model,
			       started_at, COALESCE(finished_at,''), total, passed, failed,
			       input_tokens, output_tokens, cost_usd
			FROM eval_runs WHERE config_path = ?
			ORDER BY started_at DESC LIMIT ?`, configPath, limit)
	} else {
		rows, err = d.db.QueryContext(ctx, `
			SELECT id, config_path, description, model,
			       started_at, COALESCE(finished_at,''), total, passed, failed,
			       input_tokens, output_tokens, cost_usd
			FROM eval_runs ORDER BY started_at DESC LIMIT ?`, limit)
	}
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var runs []EvalRunSummary
	for rows.Next() {
		var r EvalRunSummary
		if err := rows.Scan(&r.ID, &r.ConfigPath, &r.Description, &r.Model,
			&r.StartedAt, &r.FinishedAt, &r.Total, &r.Passed, &r.Failed,
			&r.InputTokens, &r.OutputTokens, &r.CostUSD); err != nil {
			return nil, err
		}
		runs = append(runs, r)
	}
	return runs, rows.Err()
}

// GetRunResults returns all test results for a given run in insertion order.
func (d *DB) GetRunResults(ctx context.Context, runID string) ([]TestResult, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT test_description, passed, output, error, duration_ms, assertions_json,
		       input_tokens, output_tokens, cost_usd, cache_hit
		FROM eval_results WHERE run_id = ? ORDER BY id`, runID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var results []TestResult
	for rows.Next() {
		var r TestResult
		var passed, cacheHit int
		var assertionsJSON string
		if err := rows.Scan(&r.Description, &passed, &r.Output, &r.Error,
			&r.DurationMs, &assertionsJSON,
			&r.InputTokens, &r.OutputTokens, &r.CostUSD, &cacheHit); err != nil {
			return nil, err
		}
		r.Passed = passed == 1
		r.CacheHit = cacheHit == 1
		_ = json.Unmarshal([]byte(assertionsJSON), &r.Assertions)
		results = append(results, r)
	}
	return results, rows.Err()
}

// GetLastRunForConfig returns the most recent run for the given config path,
// or nil if no runs exist for that config.
func (d *DB) GetLastRunForConfig(ctx context.Context, configPath string) (*EvalRunSummary, error) {
	var r EvalRunSummary
	err := d.db.QueryRowContext(ctx, `
		SELECT id, config_path, description, model,
		       started_at, COALESCE(finished_at,''), total, passed, failed,
		       input_tokens, output_tokens, cost_usd
		FROM eval_runs WHERE config_path = ?
		ORDER BY started_at DESC LIMIT 1`, configPath,
	).Scan(&r.ID, &r.ConfigPath, &r.Description, &r.Model,
		&r.StartedAt, &r.FinishedAt, &r.Total, &r.Passed, &r.Failed,
		&r.InputTokens, &r.OutputTokens, &r.CostUSD)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, err
	}
	return &r, nil
}

// Close closes the underlying database connection.
func (d *DB) Close() error {
	return d.db.Close()
}
