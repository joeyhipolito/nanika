package main

import (
	"bufio"
	"bytes"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"

	_ "modernc.org/sqlite"
)

// ---------------------------------------------------------------------------
// DB schema strings (mirror the production handlers exactly)
// ---------------------------------------------------------------------------

const (
	findingsSchema = `CREATE TABLE findings (
		id TEXT, ability TEXT, category TEXT, severity TEXT,
		title TEXT, description TEXT, scope_kind TEXT, scope_value TEXT,
		evidence TEXT, source TEXT, found_at TEXT, expires_at TEXT,
		superseded_by TEXT, created_at TEXT
	)`

	proposalsSchema = `CREATE TABLE proposals (
		dedup_key TEXT, last_proposed_at TEXT,
		ability TEXT, category TEXT, tracker_issue TEXT
	)`

	koSchema = `CREATE TABLE eval_runs (
		id TEXT, config_path TEXT, description TEXT, model TEXT,
		started_at TEXT, finished_at TEXT,
		total INTEGER, passed INTEGER, failed INTEGER,
		input_tokens INTEGER, output_tokens INTEGER, cost_usd REAL
	)`

	schedulerSchema = `CREATE TABLE jobs (
		id TEXT, name TEXT, command TEXT, schedule TEXT, schedule_type TEXT,
		enabled INTEGER, priority TEXT, timeout_sec INTEGER,
		last_run_at TEXT, next_run_at TEXT, created_at TEXT
	)`

	trackerSchema = `CREATE TABLE issues (
		id TEXT, title TEXT, description TEXT, status TEXT, priority TEXT,
		labels TEXT, assignee TEXT, created_at TEXT, updated_at TEXT
	)`

	metricsSchema = `CREATE TABLE missions (
		id TEXT, domain TEXT, task TEXT, started_at TEXT, finished_at TEXT,
		status TEXT, phases_total INTEGER, phases_completed INTEGER,
		phases_failed INTEGER, phases_skipped INTEGER, retries_total INTEGER,
		cost_usd_total REAL, decomp_source TEXT
	)`

	learningsSchema = `CREATE TABLE learnings (
		id TEXT, type TEXT, content TEXT, context TEXT, domain TEXT,
		worker_name TEXT, workspace_id TEXT, tags TEXT,
		seen_count INTEGER, used_count INTEGER, quality_score REAL,
		created_at TEXT, archived INTEGER
	)`
)

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

// makeDB creates a SQLite DB at path, applies schema, returns the open *sql.DB.
// The caller must close it when done seeding.
func makeDB(t *testing.T, path string, schema string) *sql.DB {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	db, err := sql.Open("sqlite", "file:"+path)
	if err != nil {
		t.Fatalf("open %s: %v", path, err)
	}
	if _, err := db.Exec(schema); err != nil {
		db.Close()
		t.Fatalf("create schema at %s: %v", path, err)
	}
	return db
}

// exec fails the test immediately if the statement fails.
func exec(t *testing.T, db *sql.DB, q string, args ...any) {
	t.Helper()
	if _, err := db.Exec(q, args...); err != nil {
		t.Fatalf("exec %q %v: %v", q, args, err)
	}
}

// resultCount extracts the "count" key from a map[string]any result.
func resultCount(t *testing.T, result any) int {
	t.Helper()
	m, ok := result.(map[string]any)
	if !ok {
		t.Fatalf("result is %T, want map[string]any", result)
	}
	n, ok := m["count"].(int)
	if !ok {
		t.Fatalf("result[\"count\"] is %T, want int", m["count"])
	}
	return n
}

// ---------------------------------------------------------------------------
// Regression guard: exact tool count and names
// ---------------------------------------------------------------------------

func TestToolListRegression(t *testing.T) {
	want := []string{
		"nanika_findings",
		"nanika_proposals",
		"nanika_ko_verdicts",
		"nanika_scheduler_jobs",
		"nanika_tracker_issues",
		"nanika_mission",
		"nanika_events",
		"nanika_learnings",
	}

	got := listTools()

	if len(got.Tools) != len(want) {
		t.Errorf("tool count = %d, want %d", len(got.Tools), len(want))
	}

	names := make([]string, len(got.Tools))
	for i, tool := range got.Tools {
		names[i] = tool.Name
	}

	for i, w := range want {
		if i >= len(names) {
			t.Errorf("tool[%d]: missing, want %q", i, w)
			continue
		}
		if names[i] != w {
			t.Errorf("tool[%d]: got %q, want %q", i, names[i], w)
		}
	}

	// Ensure every tool has a non-empty description and an input schema.
	for _, tool := range got.Tools {
		if tool.Description == "" {
			t.Errorf("tool %q has empty description", tool.Name)
		}
		if tool.InputSchema == nil {
			t.Errorf("tool %q has nil InputSchema", tool.Name)
		}
	}
}

// ---------------------------------------------------------------------------
// clampLimit helper
// ---------------------------------------------------------------------------

func TestClampLimit(t *testing.T) {
	tests := []struct {
		name         string
		v, def, ceil int
		want         int
	}{
		{"zero uses default", 0, 20, 100, 20},
		{"negative uses default", -1, 20, 100, 20},
		{"in-range returned as-is", 50, 20, 100, 50},
		{"at ceiling returned as-is", 100, 20, 100, 100},
		{"above ceiling clamped", 9999, 20, 100, 100},
		{"one returned as-is", 1, 20, 100, 1},
		{"default equals ceiling", 100, 100, 100, 100},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := clampLimit(tt.v, tt.def, tt.ceil); got != tt.want {
				t.Errorf("clampLimit(%d, %d, %d) = %d, want %d",
					tt.v, tt.def, tt.ceil, got, tt.want)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_findings
// ---------------------------------------------------------------------------

func TestHandleFindings(t *testing.T) {
	type tc struct {
		name      string
		setup     func(t *testing.T, dir string)
		args      string
		wantCount int
		wantErr   string
	}

	tests := []tc{
		{
			name:    "missing DB returns backing store error",
			args:    `{}`,
			wantErr: "backing store not found",
		},
		{
			name: "empty DB returns zero results",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "findings.db"), findingsSchema)
				db.Close()
			},
			args:      `{}`,
			wantCount: 0,
		},
		{
			name: "active_only default filters out superseded findings",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "findings.db"), findingsSchema)
				// 2 active (superseded_by=''), 1 superseded
				exec(t, db, `INSERT INTO findings VALUES ('f1','gyo','drift','high','t1','d1','file','p1','ev','src','2024-01-01',NULL,'','2024-01-01')`)
				exec(t, db, `INSERT INTO findings VALUES ('f2','en','anomaly','low','t2','d2','cmd','p2','ev','src','2024-01-02',NULL,'','2024-01-02')`)
				exec(t, db, `INSERT INTO findings VALUES ('f3','ryu','drift','medium','t3','d3','cmd','p3','ev','src','2024-01-03',NULL,'f1','2024-01-03')`)
				db.Close()
			},
			args:      `{}`,
			wantCount: 2,
		},
		{
			name: "active_only false includes superseded",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "findings.db"), findingsSchema)
				exec(t, db, `INSERT INTO findings VALUES ('f1','gyo','drift','high','t1','d1','file','p1','ev','src','2024-01-01',NULL,'','2024-01-01')`)
				exec(t, db, `INSERT INTO findings VALUES ('f2','gyo','drift','high','t2','d2','file','p2','ev','src','2024-01-02',NULL,'f1','2024-01-02')`)
				db.Close()
			},
			args:      `{"active_only":false}`,
			wantCount: 2,
		},
		{
			name: "filter by ability",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "findings.db"), findingsSchema)
				exec(t, db, `INSERT INTO findings VALUES ('f1','gyo','drift','high','t1','d1','file','p1','ev','src','2024-01-01',NULL,'','2024-01-01')`)
				exec(t, db, `INSERT INTO findings VALUES ('f2','en','drift','high','t2','d2','file','p2','ev','src','2024-01-02',NULL,'','2024-01-02')`)
				db.Close()
			},
			args:      `{"ability":"gyo","active_only":false}`,
			wantCount: 1,
		},
		{
			name: "filter by severity",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "findings.db"), findingsSchema)
				exec(t, db, `INSERT INTO findings VALUES ('f1','gyo','drift','high','t1','d1','file','p1','ev','src','2024-01-01',NULL,'','2024-01-01')`)
				exec(t, db, `INSERT INTO findings VALUES ('f2','en','drift','low','t2','d2','file','p2','ev','src','2024-01-02',NULL,'','2024-01-02')`)
				db.Close()
			},
			args:      `{"severity":"high","active_only":false}`,
			wantCount: 1,
		},
		{
			name: "filter by category",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "findings.db"), findingsSchema)
				exec(t, db, `INSERT INTO findings VALUES ('f1','gyo','drift','high','t1','d1','file','p1','ev','src','2024-01-01',NULL,'','2024-01-01')`)
				exec(t, db, `INSERT INTO findings VALUES ('f2','en','anomaly','low','t2','d2','file','p2','ev','src','2024-01-02',NULL,'','2024-01-02')`)
				db.Close()
			},
			args:      `{"category":"anomaly","active_only":false}`,
			wantCount: 1,
		},
		{
			name: "limit clamped to ceiling of 100",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "findings.db"), findingsSchema)
				for i := 0; i < 5; i++ {
					exec(t, db, `INSERT INTO findings VALUES (?,?,?,?,?,?,?,?,?,?,?,NULL,'','')`,
						fmt.Sprintf("f%d", i), "gyo", "drift", "low",
						fmt.Sprintf("t%d", i), "d", "file", "p",
						"ev", "src", fmt.Sprintf("2024-01-%02d", i+1))
				}
				db.Close()
			},
			args:      `{"limit":9999,"active_only":false}`,
			wantCount: 5, // only 5 rows exist; ceiling=100 doesn't cut them
		},
		{
			name:    "invalid JSON args returns parse error",
			args:    `{not valid json`,
			wantErr: "parsing arguments",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
			if tt.setup != nil {
				tt.setup(t, tmpDir)
			}

			result, err := handleFindings(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_proposals
// ---------------------------------------------------------------------------

func TestHandleProposals(t *testing.T) {
	tests := []struct {
		name      string
		setup     func(t *testing.T, dir string)
		args      string
		wantCount int
		wantErr   string
	}{
		{
			name:    "missing DB returns backing store error",
			args:    `{}`,
			wantErr: "backing store not found",
		},
		{
			name: "empty DB returns zero results",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "proposals.db"), proposalsSchema)
				db.Close()
			},
			args:      `{}`,
			wantCount: 0,
		},
		{
			name: "returns all proposals without filter",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "proposals.db"), proposalsSchema)
				exec(t, db, `INSERT INTO proposals VALUES ('k1','2024-01-01','shu','prompt',NULL)`)
				exec(t, db, `INSERT INTO proposals VALUES ('k2','2024-01-02','ko','eval',NULL)`)
				db.Close()
			},
			args:      `{}`,
			wantCount: 2,
		},
		{
			name: "filter by ability",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "proposals.db"), proposalsSchema)
				exec(t, db, `INSERT INTO proposals VALUES ('k1','2024-01-01','shu','prompt',NULL)`)
				exec(t, db, `INSERT INTO proposals VALUES ('k2','2024-01-02','ko','eval',NULL)`)
				db.Close()
			},
			args:      `{"ability":"shu"}`,
			wantCount: 1,
		},
		{
			name: "limit zero uses default of 20",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "nen", "proposals.db"), proposalsSchema)
				for i := 0; i < 3; i++ {
					exec(t, db, `INSERT INTO proposals VALUES (?,?,'shu','prompt',NULL)`,
						fmt.Sprintf("k%d", i), fmt.Sprintf("2024-01-%02d", i+1))
				}
				db.Close()
			},
			args:      `{"limit":0}`,
			wantCount: 3,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
			if tt.setup != nil {
				tt.setup(t, tmpDir)
			}

			result, err := handleProposals(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_ko_verdicts
// ---------------------------------------------------------------------------

func TestHandleKOVerdicts(t *testing.T) {
	tests := []struct {
		name      string
		setup     func(t *testing.T, dir string)
		args      string
		wantCount int
		wantErr   string
	}{
		{
			name:    "missing DB returns backing store error",
			args:    `{}`,
			wantErr: "backing store not found",
		},
		{
			name: "empty DB returns zero results",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "ko-history.db"), koSchema)
				db.Close()
			},
			args:      `{}`,
			wantCount: 0,
		},
		{
			name: "returns all verdicts without filter",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "ko-history.db"), koSchema)
				exec(t, db, `INSERT INTO eval_runs VALUES ('r1','/path/to/a.yaml','desc','claude','2024-01-01',NULL,10,8,2,100,200,0.05)`)
				exec(t, db, `INSERT INTO eval_runs VALUES ('r2','/path/to/b.yaml','desc','claude','2024-01-02',NULL,5,5,0,50,100,0.02)`)
				db.Close()
			},
			args:      `{}`,
			wantCount: 2,
		},
		{
			name: "filter by config path substring",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "ko-history.db"), koSchema)
				exec(t, db, `INSERT INTO eval_runs VALUES ('r1','/path/to/alpha.yaml','desc','claude','2024-01-01',NULL,10,8,2,100,200,0.05)`)
				exec(t, db, `INSERT INTO eval_runs VALUES ('r2','/path/to/beta.yaml','desc','claude','2024-01-02',NULL,5,5,0,50,100,0.02)`)
				db.Close()
			},
			args:      `{"config":"alpha"}`,
			wantCount: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
			if tt.setup != nil {
				tt.setup(t, tmpDir)
			}

			result, err := handleKOVerdicts(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_scheduler_jobs
// ---------------------------------------------------------------------------

func TestHandleSchedulerJobs(t *testing.T) {
	tests := []struct {
		name      string
		setup     func(t *testing.T, dir string)
		args      string
		wantCount int
		wantErr   string
	}{
		{
			name:    "missing DB returns backing store error",
			args:    `{}`,
			wantErr: "backing store not found",
		},
		{
			name: "returns all jobs without filter",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "scheduler.db"), schedulerSchema)
				exec(t, db, `INSERT INTO jobs VALUES ('j1','job-a','cmd a','0 * * * *','cron',1,'normal',60,NULL,NULL,'2024-01-01')`)
				exec(t, db, `INSERT INTO jobs VALUES ('j2','job-b','cmd b','0 2 * * *','cron',0,'low',120,NULL,NULL,'2024-01-02')`)
				db.Close()
			},
			args:      `{}`,
			wantCount: 2,
		},
		{
			name: "enabled_only filters disabled jobs",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "scheduler.db"), schedulerSchema)
				exec(t, db, `INSERT INTO jobs VALUES ('j1','job-a','cmd a','0 * * * *','cron',1,'normal',60,NULL,NULL,'2024-01-01')`)
				exec(t, db, `INSERT INTO jobs VALUES ('j2','job-b','cmd b','0 2 * * *','cron',0,'low',120,NULL,NULL,'2024-01-02')`)
				db.Close()
			},
			args:      `{"enabled_only":true}`,
			wantCount: 1,
		},
		{
			name: "empty DB returns zero results",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "scheduler.db"), schedulerSchema)
				db.Close()
			},
			args:      `{}`,
			wantCount: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			t.Setenv("SCHEDULER_CONFIG_DIR", tmpDir)
			if tt.setup != nil {
				tt.setup(t, tmpDir)
			}

			result, err := handleSchedulerJobs(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_tracker_issues
// ---------------------------------------------------------------------------

func TestHandleTrackerIssues(t *testing.T) {
	tests := []struct {
		name      string
		setup     func(t *testing.T, dbPath string)
		args      string
		wantCount int
		wantErr   string
	}{
		{
			name:    "missing DB returns backing store error",
			args:    `{}`,
			wantErr: "backing store not found",
		},
		{
			name: "returns all issues without filter",
			setup: func(t *testing.T, dbPath string) {
				db := makeDB(t, dbPath, trackerSchema)
				exec(t, db, `INSERT INTO issues VALUES ('i1','Bug A',NULL,'open','P0',NULL,NULL,'2024-01-01','2024-01-01')`)
				exec(t, db, `INSERT INTO issues VALUES ('i2','Task B',NULL,'closed','P2',NULL,NULL,'2024-01-02','2024-01-02')`)
				db.Close()
			},
			args:      `{}`,
			wantCount: 2,
		},
		{
			name: "filter by status",
			setup: func(t *testing.T, dbPath string) {
				db := makeDB(t, dbPath, trackerSchema)
				exec(t, db, `INSERT INTO issues VALUES ('i1','Bug A',NULL,'open','P0',NULL,NULL,'2024-01-01','2024-01-01')`)
				exec(t, db, `INSERT INTO issues VALUES ('i2','Task B',NULL,'closed','P2',NULL,NULL,'2024-01-02','2024-01-02')`)
				db.Close()
			},
			args:      `{"status":"open"}`,
			wantCount: 1,
		},
		{
			name: "filter by priority",
			setup: func(t *testing.T, dbPath string) {
				db := makeDB(t, dbPath, trackerSchema)
				exec(t, db, `INSERT INTO issues VALUES ('i1','Bug A',NULL,'open','P0',NULL,NULL,'2024-01-01','2024-01-01')`)
				exec(t, db, `INSERT INTO issues VALUES ('i2','Task B',NULL,'open','P2',NULL,NULL,'2024-01-02','2024-01-02')`)
				db.Close()
			},
			args:      `{"priority":"P0"}`,
			wantCount: 1,
		},
		{
			name: "filter by status and priority combined",
			setup: func(t *testing.T, dbPath string) {
				db := makeDB(t, dbPath, trackerSchema)
				exec(t, db, `INSERT INTO issues VALUES ('i1','Bug A',NULL,'open','P0',NULL,NULL,'2024-01-01','2024-01-01')`)
				exec(t, db, `INSERT INTO issues VALUES ('i2','Task B',NULL,'open','P2',NULL,NULL,'2024-01-02','2024-01-02')`)
				exec(t, db, `INSERT INTO issues VALUES ('i3','Task C',NULL,'closed','P0',NULL,NULL,'2024-01-03','2024-01-03')`)
				db.Close()
			},
			args:      `{"status":"open","priority":"P0"}`,
			wantCount: 1,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			dbPath := filepath.Join(tmpDir, "tracker.db")
			t.Setenv("TRACKER_DB", dbPath)
			if tt.setup != nil {
				tt.setup(t, dbPath)
			}

			result, err := handleTrackerIssues(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_mission
// ---------------------------------------------------------------------------

func TestHandleMission(t *testing.T) {
	seedMissions := func(t *testing.T, dir string) {
		t.Helper()
		db := makeDB(t, filepath.Join(dir, "metrics.db"), metricsSchema)
		exec(t, db, `INSERT INTO missions VALUES ('m1','dev','task one','2024-01-01','2024-01-01','completed',3,3,0,0,0,0.10,'file')`)
		exec(t, db, `INSERT INTO missions VALUES ('m2','dev','task two','2024-01-02','2024-01-02','failed',2,1,1,0,0,0.05,'llm')`)
		exec(t, db, `INSERT INTO missions VALUES ('m3','personal','task three','2024-01-03',NULL,'running',1,0,0,0,0,0.00,'file')`)
		db.Close()
	}

	tests := []struct {
		name      string
		setup     func(t *testing.T, dir string)
		args      string
		wantCount int  // used when listing
		wantErr   string
		singleID  bool // expect single-mission map (not list)
	}{
		{
			name:    "missing DB returns backing store error",
			args:    `{}`,
			wantErr: "backing store not found",
		},
		{
			name:      "list all missions",
			setup:     seedMissions,
			args:      `{}`,
			wantCount: 3,
		},
		{
			name:      "list with status filter",
			setup:     seedMissions,
			args:      `{"status":"completed"}`,
			wantCount: 1,
		},
		{
			name:     "single lookup by ID",
			setup:    seedMissions,
			args:     `{"mission_id":"m2"}`,
			singleID: true,
		},
		{
			name:    "single lookup missing ID returns error",
			setup:   seedMissions,
			args:    `{"mission_id":"nonexistent"}`,
			wantErr: "mission not found",
		},
		{
			name:      "empty DB list returns zero results",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "metrics.db"), metricsSchema)
				db.Close()
			},
			args:      `{}`,
			wantCount: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
			if tt.setup != nil {
				tt.setup(t, tmpDir)
			}

			result, err := handleMission(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}

			if tt.singleID {
				// Single-mission response is a flat map, not a list wrapper.
				m, ok := result.(map[string]any)
				if !ok {
					t.Fatalf("single-mission result is %T, want map[string]any", result)
				}
				if m["id"] != "m2" {
					t.Errorf("id = %v, want m2", m["id"])
				}
				return
			}

			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_events
// ---------------------------------------------------------------------------

func writeJSONL(t *testing.T, path string, lines []string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	var buf bytes.Buffer
	for _, l := range lines {
		buf.WriteString(l)
		buf.WriteByte('\n')
	}
	if err := os.WriteFile(path, buf.Bytes(), 0o644); err != nil {
		t.Fatalf("write %s: %v", path, err)
	}
}

func TestHandleEvents(t *testing.T) {
	tests := []struct {
		name      string
		setup     func(t *testing.T, dir string)
		args      string
		wantCount int
		wantErr   string
	}{
		{
			name:    "missing mission_id returns required error",
			args:    `{}`,
			wantErr: "mission_id is required",
		},
		{
			name:    "empty mission_id returns required error",
			args:    `{"mission_id":""}`,
			wantErr: "mission_id is required",
		},
		{
			name:    "events file not found returns descriptive error",
			args:    `{"mission_id":"no-such-mission"}`,
			wantErr: "no events found for mission no-such-mission",
		},
		{
			name: "returns all events from JSONL file",
			setup: func(t *testing.T, dir string) {
				writeJSONL(t, filepath.Join(dir, "events", "m42.jsonl"), []string{
					`{"type":"mission.started","mission_id":"m42","ts":"2024-01-01T00:00:00Z"}`,
					`{"type":"phase.started","mission_id":"m42","phase":"impl","ts":"2024-01-01T00:01:00Z"}`,
					`{"type":"mission.completed","mission_id":"m42","ts":"2024-01-01T00:05:00Z"}`,
				})
			},
			args:      `{"mission_id":"m42"}`,
			wantCount: 3,
		},
		{
			name: "filter by event_type",
			setup: func(t *testing.T, dir string) {
				writeJSONL(t, filepath.Join(dir, "events", "m43.jsonl"), []string{
					`{"type":"mission.started","mission_id":"m43"}`,
					`{"type":"phase.started","mission_id":"m43"}`,
					`{"type":"phase.started","mission_id":"m43"}`,
					`{"type":"mission.completed","mission_id":"m43"}`,
				})
			},
			args:      `{"mission_id":"m43","event_type":"phase.started"}`,
			wantCount: 2,
		},
		{
			name: "malformed JSONL lines are skipped",
			setup: func(t *testing.T, dir string) {
				writeJSONL(t, filepath.Join(dir, "events", "m44.jsonl"), []string{
					`{"type":"mission.started","mission_id":"m44"}`,
					`not valid json {{`,
					``,
					`{"type":"mission.completed","mission_id":"m44"}`,
				})
			},
			args:      `{"mission_id":"m44"}`,
			wantCount: 2,
		},
		{
			name: "limit applied correctly",
			setup: func(t *testing.T, dir string) {
				lines := make([]string, 10)
				for i := range lines {
					lines[i] = fmt.Sprintf(`{"type":"tick","seq":%d}`, i)
				}
				writeJSONL(t, filepath.Join(dir, "events", "m45.jsonl"), lines)
			},
			args:      `{"mission_id":"m45","limit":3}`,
			wantCount: 3,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
			if tt.setup != nil {
				tt.setup(t, tmpDir)
			}

			result, err := handleEvents(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// nanika_learnings
// ---------------------------------------------------------------------------

func TestHandleLearnings(t *testing.T) {
	seedLearnings := func(t *testing.T, dir string) {
		t.Helper()
		db := makeDB(t, filepath.Join(dir, "learnings.db"), learningsSchema)
		// 2 dev learnings (1 archived), 1 personal
		exec(t, db, `INSERT INTO learnings VALUES ('l1','insight','content A','ctx','dev',NULL,NULL,NULL,5,2,0.8,'2024-01-01',0)`)
		exec(t, db, `INSERT INTO learnings VALUES ('l2','pattern','content B','ctx','dev',NULL,NULL,NULL,1,0,0.3,'2024-01-02',1)`)
		exec(t, db, `INSERT INTO learnings VALUES ('l3','error','content C','ctx','personal',NULL,NULL,NULL,2,1,0.5,'2024-01-03',0)`)
		db.Close()
	}

	tests := []struct {
		name      string
		setup     func(t *testing.T, dir string)
		args      string
		wantCount int
		wantErr   string
	}{
		{
			name:    "missing DB returns backing store error",
			args:    `{}`,
			wantErr: "backing store not found",
		},
		{
			name:      "default excludes archived learnings",
			setup:     seedLearnings,
			args:      `{}`,
			wantCount: 2, // l2 is archived
		},
		{
			name:      "archived true includes all learnings",
			setup:     seedLearnings,
			args:      `{"archived":true}`,
			wantCount: 3,
		},
		{
			name:      "archived false explicitly excludes archived",
			setup:     seedLearnings,
			args:      `{"archived":false}`,
			wantCount: 2,
		},
		{
			name:      "filter by domain",
			setup:     seedLearnings,
			args:      `{"domain":"dev"}`,
			wantCount: 1, // l1 only (l2 is archived, excluded by default)
		},
		{
			name:      "filter by domain with archived true",
			setup:     seedLearnings,
			args:      `{"domain":"dev","archived":true}`,
			wantCount: 2,
		},
		{
			name:      "filter by type",
			setup:     seedLearnings,
			args:      `{"type":"error"}`,
			wantCount: 1,
		},
		{
			name: "limit clamped to ceiling of 100",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "learnings.db"), learningsSchema)
				for i := 0; i < 5; i++ {
					exec(t, db, `INSERT INTO learnings VALUES (?,?,?,?,?,NULL,NULL,NULL,0,0,0.0,?,0)`,
						fmt.Sprintf("l%d", i), "insight",
						fmt.Sprintf("content %d", i), "ctx", "dev",
						fmt.Sprintf("2024-01-%02d", i+1))
				}
				db.Close()
			},
			args:      `{"limit":9999}`,
			wantCount: 5,
		},
		{
			name: "empty DB returns zero results",
			setup: func(t *testing.T, dir string) {
				db := makeDB(t, filepath.Join(dir, "learnings.db"), learningsSchema)
				db.Close()
			},
			args:      `{}`,
			wantCount: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			tmpDir := t.TempDir()
			t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
			if tt.setup != nil {
				tt.setup(t, tmpDir)
			}

			result, err := handleLearnings(context.Background(), json.RawMessage(tt.args))
			if tt.wantErr != "" {
				if err == nil {
					t.Fatalf("expected error containing %q, got nil", tt.wantErr)
				}
				if !strings.Contains(err.Error(), tt.wantErr) {
					t.Errorf("error = %q, want substring %q", err.Error(), tt.wantErr)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got := resultCount(t, result); got != tt.wantCount {
				t.Errorf("count = %d, want %d", got, tt.wantCount)
			}
		})
	}
}

// ---------------------------------------------------------------------------
// Integration test: full JSON-RPC loop over mock stdio
// ---------------------------------------------------------------------------

// TestJSONRPCLoop sends a realistic MCP session through runLoop using in-memory
// buffers and verifies protocol-level correctness end-to-end.
func TestJSONRPCLoop(t *testing.T) {
	// Set up a temp orchestrator config dir with a learnings DB so the
	// tools/call request succeeds rather than returning a backing-store error.
	tmpDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", tmpDir)
	t.Setenv("SCHEDULER_CONFIG_DIR", tmpDir) // isolate from real scheduler.db
	t.Setenv("TRACKER_DB", filepath.Join(tmpDir, "tracker.db"))

	db := makeDB(t, filepath.Join(tmpDir, "learnings.db"), learningsSchema)
	exec(t, db, `INSERT INTO learnings VALUES ('l1','insight','test content','ctx','dev',NULL,NULL,NULL,1,0,0.5,'2024-01-01',0)`)
	db.Close()

	// Build the input: 5 JSON-RPC messages, one is a notification (no response expected).
	callParamsJSON, err := json.Marshal(map[string]any{
		"name":      "nanika_learnings",
		"arguments": map[string]any{"limit": 5},
	})
	if err != nil {
		t.Fatal(err)
	}

	inputs := []string{
		`{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"0.0"}}}`,
		`{"jsonrpc":"2.0","method":"notifications/initialized"}`, // notification — no response
		`{"jsonrpc":"2.0","id":2,"method":"tools/list"}`,
		fmt.Sprintf(`{"jsonrpc":"2.0","id":3,"method":"tools/call","params":%s}`, callParamsJSON),
		`{"jsonrpc":"2.0","id":4,"method":"no_such_method"}`,
		`not valid json {`, // parse error — response sent with null id
	}

	var inputBuf bytes.Buffer
	for _, s := range inputs {
		inputBuf.WriteString(s)
		inputBuf.WriteByte('\n')
	}

	var outputBuf bytes.Buffer
	if err := runLoop(&inputBuf, &outputBuf); err != nil {
		t.Fatalf("runLoop returned error: %v", err)
	}

	// Parse all responses from the output buffer.
	type response struct {
		JSONRPC string          `json:"jsonrpc"`
		ID      json.RawMessage `json:"id"`
		Result  json.RawMessage `json:"result"`
		Error   *struct {
			Code    int    `json:"code"`
			Message string `json:"message"`
		} `json:"error"`
	}

	var responses []response
	scanner := bufio.NewScanner(&outputBuf)
	for scanner.Scan() {
		var resp response
		if err := json.Unmarshal(scanner.Bytes(), &resp); err != nil {
			t.Fatalf("unmarshal response line %q: %v", scanner.Bytes(), err)
		}
		responses = append(responses, resp)
	}

	// Expect 5 responses: initialize + tools/list + tools/call + method-not-found + parse-error.
	// The notification produces no response.
	const wantResponses = 5
	if len(responses) != wantResponses {
		t.Fatalf("response count = %d, want %d", len(responses), wantResponses)
	}

	// Response 0: initialize — must echo protocolVersion.
	var initResult map[string]any
	if err := json.Unmarshal(responses[0].Result, &initResult); err != nil {
		t.Fatalf("unmarshal initialize result: %v", err)
	}
	if pv, _ := initResult["protocolVersion"].(string); pv != protocolVersion {
		t.Errorf("protocolVersion = %q, want %q", pv, protocolVersion)
	}
	if responses[0].Error != nil {
		t.Errorf("initialize: unexpected error %+v", responses[0].Error)
	}

	// Response 1: tools/list — must contain exactly 8 tools.
	var listResult map[string]any
	if err := json.Unmarshal(responses[1].Result, &listResult); err != nil {
		t.Fatalf("unmarshal tools/list result: %v", err)
	}
	tools, ok := listResult["tools"].([]any)
	if !ok {
		t.Fatalf("tools/list result[\"tools\"] is %T, want []any", listResult["tools"])
	}
	if len(tools) != 8 {
		t.Errorf("tools/list count = %d, want 8", len(tools))
	}

	// Response 2: tools/call nanika_learnings — must succeed with content.
	if responses[2].Error != nil {
		t.Errorf("tools/call: unexpected RPC error %+v", responses[2].Error)
	}
	var callResult map[string]any
	if err := json.Unmarshal(responses[2].Result, &callResult); err != nil {
		t.Fatalf("unmarshal tools/call result: %v", err)
	}
	content, _ := callResult["content"].([]any)
	if len(content) == 0 {
		t.Error("tools/call result has empty content")
	}
	if isErr, _ := callResult["isError"].(bool); isErr {
		t.Error("tools/call result has isError=true")
	}

	// Response 3: unknown method — must be -32601.
	if responses[3].Error == nil {
		t.Error("no_such_method: expected error, got nil")
	} else if responses[3].Error.Code != -32601 {
		t.Errorf("no_such_method error code = %d, want -32601", responses[3].Error.Code)
	}

	// Response 4: parse error — must be -32700 with null ID.
	if responses[4].Error == nil {
		t.Error("parse error response: expected error, got nil")
	} else if responses[4].Error.Code != -32700 {
		t.Errorf("parse error code = %d, want -32700", responses[4].Error.Code)
	}
	if string(responses[4].ID) != "null" {
		t.Errorf("parse error response ID = %s, want null", responses[4].ID)
	}
}
