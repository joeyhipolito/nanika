// Package routing provides target resolution and routing-memory storage for
// the orchestrator's decomposition pipeline.
//
// Routing memory lives in four tables added to the shared ~/.alluka/learnings.db:
//   - target_profiles    — stable, authoritative facts about a target
//   - routing_patterns   — observed persona selections per target, with confidence
//   - handoff_patterns   — observed persona-to-persona handoffs per target
//   - routing_corrections — explicit corrections (audit or human) that a prior routing was wrong
//
// All tables are created with CREATE TABLE IF NOT EXISTS so this package can be
// opened alongside the existing learnings tables without conflict.
package routing

import (
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	_ "modernc.org/sqlite"
)

// TargetProfile holds stable, authoritative facts about a known routing target.
// Profiles are seeded manually or via the routing seed command and represent
// durable context that biases persona selection for recurring tasks on a target.
type TargetProfile struct {
	// TargetID is the canonical identifier, e.g. "repo:~/skills/orchestrator".
	TargetID string
	// TargetType is the target classification, e.g. "repo", "via_system", "vault".
	TargetType string
	// Language is the primary programming language; empty for non-code targets.
	Language string
	// Runtime is the build runtime, e.g. "go", "cargo", "node".
	Runtime string
	// TestCommand is the command used to run tests, e.g. "go test ./...".
	TestCommand string
	// BuildCommand is the command used to build the target, e.g. "make build".
	BuildCommand string
	// Framework is the primary framework or library, e.g. "cobra", "react", "gin".
	Framework string
	// KeyDirectories is a list of notable directories in the repo, e.g. ["cmd", "internal"].
	KeyDirectories []string
	// PreferredPersonas is an ordered list of preferred persona names.
	PreferredPersonas []string
	// Notes is free-form context about this target.
	Notes     string
	CreatedAt time.Time
	UpdatedAt time.Time
}

// RoutingPattern records how often a particular persona was selected for tasks
// in a target context. Confidence grows by 0.2 per observation, capped at 1.0,
// so five consistent observations reach full confidence.
type RoutingPattern struct {
	TargetID  string
	Persona   string
	// TaskHint is an optional descriptor for the task type, e.g. "implementation".
	TaskHint   string
	SeenCount  int
	Confidence float64 // 0.0–1.0
	CreatedAt  time.Time
	LastSeenAt time.Time
}

// HandoffPattern records observed transitions between personas for a given target.
type HandoffPattern struct {
	TargetID    string
	FromPersona string
	ToPersona   string
	TaskHint    string
	SeenCount   int
	Confidence  float64 // 0.0–1.0
	CreatedAt   time.Time
	LastSeenAt  time.Time
}

// Target type constants for TargetProfile.TargetType.
const (
	TargetTypeRepo        = "repo"        // git repository target
	TargetTypeViaSystem   = "via_system"  // VIA internal system target
	TargetTypeVault       = "vault"       // vault/secrets target
	TargetTypeWorkspace   = "workspace"   // workspace-scoped target (from audit)
	TargetTypePublication = "publication" // content publication, e.g. Substack newsletter
)

// Correction source constants for RoutingCorrection.Source.
const (
	SourceManual = "manual" // human-entered correction
	SourceAudit  = "audit"  // derived from audit output
)

// RoutingCorrection records an explicit correction: the assigned persona was wrong
// and the ideal persona is known. Source is SourceManual or SourceAudit.
type RoutingCorrection struct {
	ID              int64
	TargetID        string
	AssignedPersona string
	IdealPersona    string
	TaskHint        string
	Source          string // SourceManual or SourceAudit
	CreatedAt       time.Time
}

// RoutingDB accesses routing-memory tables in the shared learnings database.
type RoutingDB struct {
	db *sql.DB
}

// OpenDB opens (or creates) the routing tables in the learnings database.
// If path is empty, the default ~/.alluka/learnings.db is used.
// The routing tables are additive — existing learnings data is never modified.
func OpenDB(path string) (*RoutingDB, error) {
	if path == "" {
		base, err := config.Dir()
		if err != nil {
			return nil, fmt.Errorf("config dir: %w", err)
		}
		path = filepath.Join(base, "learnings.db")
	}

	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return nil, fmt.Errorf("creating database directory: %w", err)
	}

	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("opening database: %w", err)
	}

	if _, err := db.Exec("PRAGMA journal_mode=WAL"); err != nil {
		db.Close()
		return nil, fmt.Errorf("setting WAL mode: %w", err)
	}

	rdb := &RoutingDB{db: db}
	if err := rdb.initSchema(); err != nil {
		db.Close()
		return nil, fmt.Errorf("initializing schema: %w", err)
	}
	return rdb, nil
}

// Close closes the underlying database connection.
func (r *RoutingDB) Close() error {
	return r.db.Close()
}

func (r *RoutingDB) initSchema() error {
	stmts := []string{
		`CREATE TABLE IF NOT EXISTS target_profiles (
			target_id          TEXT PRIMARY KEY,
			target_type        TEXT NOT NULL DEFAULT '',
			language           TEXT NOT NULL DEFAULT '',
			runtime            TEXT NOT NULL DEFAULT '',
			test_command       TEXT NOT NULL DEFAULT '',
			build_command      TEXT NOT NULL DEFAULT '',
			framework          TEXT NOT NULL DEFAULT '',
			key_directories    TEXT NOT NULL DEFAULT '',
			preferred_personas TEXT NOT NULL DEFAULT '',
			notes              TEXT NOT NULL DEFAULT '',
			created_at         DATETIME NOT NULL,
			updated_at         DATETIME NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS routing_patterns (
			id           INTEGER PRIMARY KEY,
			target_id    TEXT NOT NULL,
			persona      TEXT NOT NULL,
			task_hint    TEXT NOT NULL DEFAULT '',
			seen_count   INTEGER NOT NULL DEFAULT 1,
			confidence   REAL NOT NULL DEFAULT 0.2,
			created_at   DATETIME NOT NULL,
			last_seen_at DATETIME NOT NULL,
			UNIQUE(target_id, persona, task_hint)
		)`,
		`CREATE TABLE IF NOT EXISTS handoff_patterns (
			id           INTEGER PRIMARY KEY,
			target_id    TEXT NOT NULL,
			from_persona TEXT NOT NULL,
			to_persona   TEXT NOT NULL,
			task_hint    TEXT NOT NULL DEFAULT '',
			seen_count   INTEGER NOT NULL DEFAULT 1,
			confidence   REAL NOT NULL DEFAULT 0.2,
			created_at   DATETIME NOT NULL,
			last_seen_at DATETIME NOT NULL,
			UNIQUE(target_id, from_persona, to_persona, task_hint)
		)`,
		`CREATE TABLE IF NOT EXISTS routing_corrections (
			id               INTEGER PRIMARY KEY,
			target_id        TEXT NOT NULL,
			assigned_persona TEXT NOT NULL,
			ideal_persona    TEXT NOT NULL,
			task_hint        TEXT NOT NULL DEFAULT '',
			source           TEXT NOT NULL DEFAULT 'manual',
			created_at       DATETIME NOT NULL
		)`,
		`CREATE TABLE IF NOT EXISTS decomposition_examples (
			id              INTEGER PRIMARY KEY,
			target_id       TEXT NOT NULL,
			workspace_id    TEXT NOT NULL,
			task_summary    TEXT NOT NULL,
			phase_count     INTEGER NOT NULL,
			execution_mode  TEXT NOT NULL,
			phases_json     TEXT NOT NULL,
			decomp_source   TEXT NOT NULL,
			audit_score     INTEGER NOT NULL DEFAULT 0,
			decomp_quality  INTEGER NOT NULL DEFAULT 0,
			persona_fit     INTEGER NOT NULL DEFAULT 0,
			created_at      DATETIME NOT NULL,
			UNIQUE(target_id, workspace_id)
		)`,
		`CREATE TABLE IF NOT EXISTS decomposition_findings (
			id            INTEGER PRIMARY KEY,
			target_id     TEXT NOT NULL,
			workspace_id  TEXT NOT NULL,
			finding_type  TEXT NOT NULL,
			phase_name    TEXT NOT NULL DEFAULT '',
			detail        TEXT NOT NULL DEFAULT '',
			decomp_source TEXT NOT NULL DEFAULT '',
			audit_score   INTEGER NOT NULL DEFAULT 0,
			created_at    DATETIME NOT NULL,
			UNIQUE(workspace_id, finding_type, phase_name, detail)
		)`,
		// phase_shape_patterns records observed phase shapes from actual mission
		// executions. Unlike decomposition_examples (which require manual audit
		// scores), these rows are written automatically after every run. The
		// outcome column ('success' or 'failure') lets the system count how
		// often a particular shape worked for a target. When a shape appears in
		// 3+ successful missions, it is surfaced to the decomposer as a proven
		// template — closing the loop from execution outcome to future decomposition
		// shape without requiring an audit review cycle.
		`CREATE TABLE IF NOT EXISTS phase_shape_patterns (
			id             INTEGER PRIMARY KEY,
			target_id      TEXT NOT NULL,
			workspace_id   TEXT NOT NULL,
			phase_count    INTEGER NOT NULL,
			execution_mode TEXT NOT NULL DEFAULT 'sequential',
			persona_seq    TEXT NOT NULL,
			outcome        TEXT NOT NULL DEFAULT 'success',
			created_at     DATETIME NOT NULL,
			UNIQUE(target_id, workspace_id)
		)`,
		`CREATE INDEX IF NOT EXISTS idx_routing_patterns_target ON routing_patterns(target_id)`,
		`CREATE INDEX IF NOT EXISTS idx_handoff_patterns_target ON handoff_patterns(target_id)`,
		`CREATE INDEX IF NOT EXISTS idx_routing_corrections_target ON routing_corrections(target_id)`,
		`CREATE UNIQUE INDEX IF NOT EXISTS idx_routing_corrections_dedup ON routing_corrections(target_id, assigned_persona, ideal_persona, task_hint, source)`,
		`CREATE INDEX IF NOT EXISTS idx_decomp_examples_target ON decomposition_examples(target_id)`,
		`CREATE INDEX IF NOT EXISTS idx_decomp_examples_score ON decomposition_examples(target_id, audit_score DESC, decomp_quality DESC)`,
		`CREATE INDEX IF NOT EXISTS idx_decomp_findings_target ON decomposition_findings(target_id)`,
		`CREATE INDEX IF NOT EXISTS idx_decomp_findings_workspace ON decomposition_findings(workspace_id)`,
		`CREATE INDEX IF NOT EXISTS idx_phase_shapes_target ON phase_shape_patterns(target_id)`,
		`CREATE INDEX IF NOT EXISTS idx_phase_shapes_outcome ON phase_shape_patterns(target_id, outcome)`,
		// role_assignments records observed role assignments (planner, implementer,
		// reviewer) for each phase in a mission. This table drives role-aware routing:
		// when the same persona consistently appears in a given role for a target, the
		// decomposer can use that signal to bias future persona selection per role.
		`CREATE TABLE IF NOT EXISTS role_assignments (
			id            INTEGER PRIMARY KEY,
			target_id     TEXT NOT NULL,
			workspace_id  TEXT NOT NULL,
			phase_id      TEXT NOT NULL,
			persona       TEXT NOT NULL,
			role          TEXT NOT NULL,
			outcome       TEXT NOT NULL DEFAULT '',
			created_at    DATETIME NOT NULL,
			UNIQUE(workspace_id, phase_id)
		)`,
		// handoff_records captures structured handoff context between phases with
		// different roles. Unlike handoff_patterns (which count persona→persona
		// transitions), handoff_records store the actual role transition and summary
		// for a specific mission execution. Used for post-hoc analysis and to improve
		// the quality of handoff context injected in future missions.
		`CREATE TABLE IF NOT EXISTS handoff_records (
			id            INTEGER PRIMARY KEY,
			target_id     TEXT NOT NULL,
			workspace_id  TEXT NOT NULL,
			from_phase_id TEXT NOT NULL,
			to_phase_id   TEXT NOT NULL,
			from_role     TEXT NOT NULL,
			to_role       TEXT NOT NULL,
			from_persona  TEXT NOT NULL,
			to_persona    TEXT NOT NULL,
			summary       TEXT NOT NULL DEFAULT '',
			created_at    DATETIME NOT NULL,
			UNIQUE(workspace_id, from_phase_id, to_phase_id)
		)`,
		`CREATE INDEX IF NOT EXISTS idx_role_assignments_target ON role_assignments(target_id)`,
		`CREATE INDEX IF NOT EXISTS idx_role_assignments_role ON role_assignments(target_id, role)`,
		`CREATE INDEX IF NOT EXISTS idx_handoff_records_target ON handoff_records(target_id)`,
		// routing_decisions records the persona selected for each phase at decomposition time,
		// then tracks whether that selection led to a successful or failed outcome.
		// This closes the red-team feedback loop: when a persona is selected for a phase and
		// the phase (or mission) fails, the failure reason is persisted so future decompositions
		// can be warned against repeating the same selection.
		`CREATE TABLE IF NOT EXISTS routing_decisions (
			id             INTEGER PRIMARY KEY,
			mission_id     TEXT NOT NULL,
			phase_id       TEXT NOT NULL,
			phase_name     TEXT NOT NULL DEFAULT '',
			persona        TEXT NOT NULL,
			confidence     REAL NOT NULL DEFAULT 0.0,
			routing_method TEXT NOT NULL DEFAULT '',
			outcome        TEXT NOT NULL DEFAULT 'pending',
			failure_reason TEXT NOT NULL DEFAULT '',
			created_at     DATETIME NOT NULL,
			updated_at     DATETIME NOT NULL,
			UNIQUE(mission_id, phase_id)
		)`,
		`CREATE INDEX IF NOT EXISTS idx_routing_decisions_mission ON routing_decisions(mission_id)`,
		`CREATE INDEX IF NOT EXISTS idx_routing_decisions_persona ON routing_decisions(persona)`,
		`CREATE INDEX IF NOT EXISTS idx_routing_decisions_persona_outcome ON routing_decisions(persona, outcome)`,
	}
	for _, stmt := range stmts {
		if _, err := r.db.Exec(stmt); err != nil {
			return fmt.Errorf("schema init (%s...): %w", stmt[:min(40, len(stmt))], err)
		}
	}

	// Migrations: add columns to existing tables without breaking older DBs.
	// Errors are intentionally ignored — "duplicate column name" is expected on
	// fresh DBs where the CREATE TABLE already includes the column.
	migrations := []string{
		"ALTER TABLE phase_shape_patterns ADD COLUMN task_type TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE target_profiles ADD COLUMN test_command TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE target_profiles ADD COLUMN build_command TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE target_profiles ADD COLUMN framework TEXT NOT NULL DEFAULT ''",
		"ALTER TABLE target_profiles ADD COLUMN key_directories TEXT NOT NULL DEFAULT ''",
	}
	for _, m := range migrations {
		r.db.Exec(m) //nolint:errcheck // column-already-exists is expected on re-init
	}

	// Index created after migration so the column is guaranteed to exist on
	// both fresh installs and upgraded existing DBs.
	r.db.Exec("CREATE INDEX IF NOT EXISTS idx_phase_shapes_task_type ON phase_shape_patterns(task_type, outcome)") //nolint:errcheck

	return nil
}

// UpsertTargetProfile inserts or replaces the profile for a target.
// If a profile already exists for the same target_id, all fields are overwritten.
func (r *RoutingDB) UpsertTargetProfile(ctx context.Context, p TargetProfile) error {
	if p.TargetID == "" {
		return fmt.Errorf("target_id is required")
	}
	if p.CreatedAt.IsZero() {
		p.CreatedAt = time.Now().UTC()
	}
	personas := strings.Join(p.PreferredPersonas, ",")
	keyDirs := strings.Join(p.KeyDirectories, ",")
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT INTO target_profiles (target_id, target_type, language, runtime, test_command, build_command, framework, key_directories, preferred_personas, notes, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(target_id) DO UPDATE SET
			target_type        = excluded.target_type,
			language           = excluded.language,
			runtime            = excluded.runtime,
			test_command       = excluded.test_command,
			build_command      = excluded.build_command,
			framework          = excluded.framework,
			key_directories    = excluded.key_directories,
			preferred_personas = excluded.preferred_personas,
			notes              = excluded.notes,
			updated_at         = excluded.updated_at
	`, p.TargetID, p.TargetType, p.Language, p.Runtime,
		p.TestCommand, p.BuildCommand, p.Framework, keyDirs,
		personas, p.Notes,
		p.CreatedAt.UTC().Format(time.RFC3339), now,
	)
	if err != nil {
		return fmt.Errorf("upserting target profile %q: %w", p.TargetID, err)
	}
	return nil
}

// GetTargetProfile retrieves the profile for targetID.
// Returns nil, nil if no profile is found.
func (r *RoutingDB) GetTargetProfile(ctx context.Context, targetID string) (*TargetProfile, error) {
	row := r.db.QueryRowContext(ctx, `
		SELECT target_id, target_type, language, runtime, test_command, build_command, framework, key_directories, preferred_personas, notes, created_at, updated_at
		FROM target_profiles WHERE target_id = ?
	`, targetID)

	var p TargetProfile
	var personas, keyDirs, createdAt, updatedAt string
	err := row.Scan(&p.TargetID, &p.TargetType, &p.Language, &p.Runtime,
		&p.TestCommand, &p.BuildCommand, &p.Framework, &keyDirs,
		&personas, &p.Notes, &createdAt, &updatedAt)
	if err == sql.ErrNoRows {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("scanning target profile %q: %w", targetID, err)
	}
	if personas != "" {
		p.PreferredPersonas = strings.Split(personas, ",")
	}
	if keyDirs != "" {
		p.KeyDirectories = strings.Split(keyDirs, ",")
	}
	p.CreatedAt = parseTime(createdAt)
	p.UpdatedAt = parseTime(updatedAt)
	return &p, nil
}

// RecordRoutingPattern records one observation of a persona being selected for
// a target. If the (target_id, persona, task_hint) tuple already exists,
// seen_count is incremented and confidence is updated via min(1.0, seen_count*0.2).
func (r *RoutingDB) RecordRoutingPattern(ctx context.Context, targetID, persona, taskHint string) error {
	if targetID == "" {
		return fmt.Errorf("target_id is required")
	}
	if persona == "" {
		return fmt.Errorf("persona is required")
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT INTO routing_patterns (target_id, persona, task_hint, seen_count, confidence, created_at, last_seen_at)
		VALUES (?, ?, ?, 1, 0.2, ?, ?)
		ON CONFLICT(target_id, persona, task_hint) DO UPDATE SET
			seen_count   = seen_count + 1,
			confidence   = MIN(1.0, CAST(seen_count + 1 AS REAL) * 0.2),
			last_seen_at = excluded.last_seen_at
	`, targetID, persona, taskHint, now, now)
	if err != nil {
		return fmt.Errorf("recording routing pattern (target=%q persona=%q): %w", targetID, persona, err)
	}
	return nil
}

// GetRoutingPatterns returns the top routing patterns for targetID, ordered by
// confidence descending then seen_count descending. Results are capped at
// maxRoutingPatterns to keep decompose prompts focused on the strongest signals.
const maxRoutingPatterns = 10

func (r *RoutingDB) GetRoutingPatterns(ctx context.Context, targetID string) ([]RoutingPattern, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT target_id, persona, task_hint, seen_count, confidence, created_at, last_seen_at
		FROM routing_patterns
		WHERE target_id = ?
		ORDER BY confidence DESC, seen_count DESC
		LIMIT ?
	`, targetID, maxRoutingPatterns)
	if err != nil {
		return nil, fmt.Errorf("querying routing patterns for %q: %w", targetID, err)
	}
	defer rows.Close()

	var patterns []RoutingPattern
	for rows.Next() {
		var p RoutingPattern
		var createdAt, lastSeenAt string
		if err := rows.Scan(&p.TargetID, &p.Persona, &p.TaskHint,
			&p.SeenCount, &p.Confidence, &createdAt, &lastSeenAt); err != nil {
			return nil, fmt.Errorf("scanning routing pattern: %w", err)
		}
		p.CreatedAt = parseTime(createdAt)
		p.LastSeenAt = parseTime(lastSeenAt)
		patterns = append(patterns, p)
	}
	return patterns, rows.Err()
}

// RecordHandoffPattern records one observation of a persona-to-persona handoff
// for a target. If the pattern already exists, seen_count is incremented and
// confidence is updated.
func (r *RoutingDB) RecordHandoffPattern(ctx context.Context, targetID, fromPersona, toPersona, taskHint string) error {
	if targetID == "" {
		return fmt.Errorf("target_id is required")
	}
	if fromPersona == "" || toPersona == "" {
		return fmt.Errorf("from_persona and to_persona are required")
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT INTO handoff_patterns (target_id, from_persona, to_persona, task_hint, seen_count, confidence, created_at, last_seen_at)
		VALUES (?, ?, ?, ?, 1, 0.2, ?, ?)
		ON CONFLICT(target_id, from_persona, to_persona, task_hint) DO UPDATE SET
			seen_count   = seen_count + 1,
			confidence   = MIN(1.0, CAST(seen_count + 1 AS REAL) * 0.2),
			last_seen_at = excluded.last_seen_at
	`, targetID, fromPersona, toPersona, taskHint, now, now)
	if err != nil {
		return fmt.Errorf("recording handoff pattern (target=%q %q->%q): %w", targetID, fromPersona, toPersona, err)
	}
	return nil
}

// GetHandoffPatterns returns all handoff patterns for targetID, ordered by
// confidence descending then seen_count descending.
func (r *RoutingDB) GetHandoffPatterns(ctx context.Context, targetID string) ([]HandoffPattern, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT target_id, from_persona, to_persona, task_hint, seen_count, confidence, created_at, last_seen_at
		FROM handoff_patterns
		WHERE target_id = ?
		ORDER BY confidence DESC, seen_count DESC
	`, targetID)
	if err != nil {
		return nil, fmt.Errorf("querying handoff patterns for %q: %w", targetID, err)
	}
	defer rows.Close()

	var patterns []HandoffPattern
	for rows.Next() {
		var p HandoffPattern
		var createdAt, lastSeenAt string
		if err := rows.Scan(&p.TargetID, &p.FromPersona, &p.ToPersona, &p.TaskHint,
			&p.SeenCount, &p.Confidence, &createdAt, &lastSeenAt); err != nil {
			return nil, fmt.Errorf("scanning handoff pattern: %w", err)
		}
		p.CreatedAt = parseTime(createdAt)
		p.LastSeenAt = parseTime(lastSeenAt)
		patterns = append(patterns, p)
	}
	return patterns, rows.Err()
}

// InsertRoutingCorrection records an explicit routing correction.
// TargetID, AssignedPersona, and IdealPersona are required.
// Source defaults to "manual" if empty.
func (r *RoutingDB) InsertRoutingCorrection(ctx context.Context, c RoutingCorrection) error {
	if c.TargetID == "" {
		return fmt.Errorf("target_id is required")
	}
	if c.AssignedPersona == "" || c.IdealPersona == "" {
		return fmt.Errorf("assigned_persona and ideal_persona are required")
	}
	if c.Source == "" {
		c.Source = SourceManual
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO routing_corrections (target_id, assigned_persona, ideal_persona, task_hint, source, created_at)
		VALUES (?, ?, ?, ?, ?, ?)
	`, c.TargetID, c.AssignedPersona, c.IdealPersona, c.TaskHint, c.Source, now)
	if err != nil {
		return fmt.Errorf("inserting routing correction for target %q: %w", c.TargetID, err)
	}
	return nil
}

// InsertRoutingCorrections inserts multiple corrections in a single transaction.
// Returns the count of rows actually inserted (duplicates are silently skipped
// via INSERT OR IGNORE). Validation errors skip the row but do not abort the batch.
func (r *RoutingDB) InsertRoutingCorrections(ctx context.Context, corrections []RoutingCorrection) (int, error) {
	if len(corrections) == 0 {
		return 0, nil
	}

	tx, err := r.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, fmt.Errorf("begin transaction: %w", err)
	}
	defer tx.Rollback()

	stmt, err := tx.PrepareContext(ctx, `
		INSERT OR IGNORE INTO routing_corrections (target_id, assigned_persona, ideal_persona, task_hint, source, created_at)
		VALUES (?, ?, ?, ?, ?, ?)
	`)
	if err != nil {
		return 0, fmt.Errorf("preparing insert statement: %w", err)
	}
	defer stmt.Close()

	now := time.Now().UTC().Format(time.RFC3339)
	inserted := 0
	for _, c := range corrections {
		if c.TargetID == "" || c.AssignedPersona == "" || c.IdealPersona == "" {
			continue
		}
		if c.Source == "" {
			c.Source = SourceManual
		}
		res, err := stmt.ExecContext(ctx, c.TargetID, c.AssignedPersona, c.IdealPersona, c.TaskHint, c.Source, now)
		if err != nil {
			continue
		}
		if n, _ := res.RowsAffected(); n > 0 {
			inserted++
		}
	}

	if err := tx.Commit(); err != nil {
		return 0, fmt.Errorf("commit transaction: %w", err)
	}
	return inserted, nil
}

// GetRoutingCorrections returns all corrections for targetID, newest first.
func (r *RoutingDB) GetRoutingCorrections(ctx context.Context, targetID string) ([]RoutingCorrection, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT id, target_id, assigned_persona, ideal_persona, task_hint, source, created_at
		FROM routing_corrections
		WHERE target_id = ?
		ORDER BY id DESC
	`, targetID)
	if err != nil {
		return nil, fmt.Errorf("querying routing corrections for %q: %w", targetID, err)
	}
	defer rows.Close()

	var corrections []RoutingCorrection
	for rows.Next() {
		var c RoutingCorrection
		var createdAt string
		if err := rows.Scan(&c.ID, &c.TargetID, &c.AssignedPersona, &c.IdealPersona,
			&c.TaskHint, &c.Source, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning routing correction: %w", err)
		}
		c.CreatedAt = parseTime(createdAt)
		corrections = append(corrections, c)
	}
	return corrections, rows.Err()
}

// DecompExample is a validated decomposition pattern for a target.
// Extracted from audited missions that pass the quality gate (audit_score >= 3).
// Stored in the decomposition_examples table.
type DecompExample struct {
	ID            int64
	TargetID      string
	WorkspaceID   string
	TaskSummary   string // first 200 chars of original task
	PhaseCount    int
	ExecutionMode string // "sequential" or "parallel"
	PhasesJSON    string // compact JSON: [{name, objective, persona, skills, depends}]
	DecompSource  string // "predecomposed", "llm", "keyword"
	AuditScore    int    // scorecard.overall (1-5)
	DecompQuality int    // scorecard.decomposition_quality (1-5)
	PersonaFit    int    // scorecard.persona_fit (1-5)
	CreatedAt     time.Time
}

// DecompFinding is a row in the decomposition_findings table.
type DecompFinding struct {
	ID           int64
	TargetID     string
	WorkspaceID  string
	FindingType  string
	PhaseName    string
	Detail       string
	DecompSource string
	AuditScore   int
	CreatedAt    time.Time
}

// InsertDecompFindings inserts multiple decomposition findings in a single
// transaction. Returns the count of rows inserted. Validation errors skip
// the row but do not abort the batch.
func (r *RoutingDB) InsertDecompFindings(ctx context.Context, findings []DecompFindingRow) (int, error) {
	if len(findings) == 0 {
		return 0, nil
	}

	tx, err := r.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, fmt.Errorf("begin transaction: %w", err)
	}
	defer tx.Rollback()

	stmt, err := tx.PrepareContext(ctx, `
		INSERT OR IGNORE INTO decomposition_findings (target_id, workspace_id, finding_type, phase_name, detail, decomp_source, audit_score, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)
	`)
	if err != nil {
		return 0, fmt.Errorf("preparing insert statement: %w", err)
	}
	defer stmt.Close()

	now := time.Now().UTC().Format(time.RFC3339)
	inserted := 0
	for _, f := range findings {
		if f.TargetID == "" || f.FindingType == "" {
			continue
		}
		res, err := stmt.ExecContext(ctx, f.TargetID, f.WorkspaceID, f.FindingType, f.PhaseName, f.Detail, f.DecompSource, f.AuditScore, now)
		if err != nil {
			continue
		}
		if n, _ := res.RowsAffected(); n > 0 {
			inserted++
		}
	}

	if err := tx.Commit(); err != nil {
		return 0, fmt.Errorf("commit transaction: %w", err)
	}
	return inserted, nil
}

// GetDecompFindings returns all decomposition findings for targetID, newest first.
func (r *RoutingDB) GetDecompFindings(ctx context.Context, targetID string) ([]DecompFinding, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT id, target_id, workspace_id, finding_type, phase_name, detail, decomp_source, audit_score, created_at
		FROM decomposition_findings
		WHERE target_id = ?
		ORDER BY id DESC
	`, targetID)
	if err != nil {
		return nil, fmt.Errorf("querying decomposition findings for %q: %w", targetID, err)
	}
	defer rows.Close()

	var findings []DecompFinding
	for rows.Next() {
		var f DecompFinding
		var createdAt string
		if err := rows.Scan(&f.ID, &f.TargetID, &f.WorkspaceID, &f.FindingType,
			&f.PhaseName, &f.Detail, &f.DecompSource, &f.AuditScore, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning decomposition finding: %w", err)
		}
		f.CreatedAt = parseTime(createdAt)
		findings = append(findings, f)
	}
	return findings, rows.Err()
}

// GetDecompFindingsByWorkspace returns all decomposition findings for a specific workspace.
func (r *RoutingDB) GetDecompFindingsByWorkspace(ctx context.Context, workspaceID string) ([]DecompFinding, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT id, target_id, workspace_id, finding_type, phase_name, detail, decomp_source, audit_score, created_at
		FROM decomposition_findings
		WHERE workspace_id = ?
		ORDER BY id DESC
	`, workspaceID)
	if err != nil {
		return nil, fmt.Errorf("querying decomposition findings for workspace %q: %w", workspaceID, err)
	}
	defer rows.Close()

	var findings []DecompFinding
	for rows.Next() {
		var f DecompFinding
		var createdAt string
		if err := rows.Scan(&f.ID, &f.TargetID, &f.WorkspaceID, &f.FindingType,
			&f.PhaseName, &f.Detail, &f.DecompSource, &f.AuditScore, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning decomposition finding: %w", err)
		}
		f.CreatedAt = parseTime(createdAt)
		findings = append(findings, f)
	}
	return findings, rows.Err()
}

// InsertDecompExample upserts a decomposition example. If a row already exists
// for the same (target_id, workspace_id), it is replaced — an audit re-run
// updates the scores without creating duplicates.
func (r *RoutingDB) InsertDecompExample(ctx context.Context, ex DecompExample) error {
	if ex.TargetID == "" {
		return fmt.Errorf("target_id is required")
	}
	if ex.WorkspaceID == "" {
		return fmt.Errorf("workspace_id is required")
	}
	if ex.PhasesJSON == "" {
		return fmt.Errorf("phases_json is required")
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT INTO decomposition_examples
			(target_id, workspace_id, task_summary, phase_count, execution_mode,
			 phases_json, decomp_source, audit_score, decomp_quality, persona_fit, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(target_id, workspace_id) DO UPDATE SET
			task_summary    = excluded.task_summary,
			phase_count     = excluded.phase_count,
			execution_mode  = excluded.execution_mode,
			phases_json     = excluded.phases_json,
			decomp_source   = excluded.decomp_source,
			audit_score     = excluded.audit_score,
			decomp_quality  = excluded.decomp_quality,
			persona_fit     = excluded.persona_fit
	`, ex.TargetID, ex.WorkspaceID, ex.TaskSummary, ex.PhaseCount, ex.ExecutionMode,
		ex.PhasesJSON, ex.DecompSource, ex.AuditScore, ex.DecompQuality, ex.PersonaFit, now)
	if err != nil {
		return fmt.Errorf("upserting decomposition example (target=%q ws=%q): %w", ex.TargetID, ex.WorkspaceID, err)
	}
	return nil
}

// maxDecompExamples caps the number of examples injected into the decomposer prompt.
const maxDecompExamples = 3

// GetDecompExamples returns validated examples for targetID, ordered by
// audit_score DESC, decomp_quality DESC. Only rows where both audit_score
// and decomp_quality >= minScore are returned. Limit caps the result set.
func (r *RoutingDB) GetDecompExamples(ctx context.Context, targetID string, minScore, limit int) ([]DecompExample, error) {
	if limit <= 0 {
		limit = maxDecompExamples
	}
	rows, err := r.db.QueryContext(ctx, `
		SELECT id, target_id, workspace_id, task_summary, phase_count, execution_mode,
		       phases_json, decomp_source, audit_score, decomp_quality, persona_fit, created_at
		FROM decomposition_examples
		WHERE target_id = ? AND audit_score >= ? AND decomp_quality >= ?
		ORDER BY audit_score DESC, decomp_quality DESC, created_at DESC
		LIMIT ?
	`, targetID, minScore, minScore, limit)
	if err != nil {
		return nil, fmt.Errorf("querying decomposition examples for %q: %w", targetID, err)
	}
	defer rows.Close()

	var examples []DecompExample
	for rows.Next() {
		var ex DecompExample
		var createdAt string
		if err := rows.Scan(&ex.ID, &ex.TargetID, &ex.WorkspaceID, &ex.TaskSummary,
			&ex.PhaseCount, &ex.ExecutionMode, &ex.PhasesJSON, &ex.DecompSource,
			&ex.AuditScore, &ex.DecompQuality, &ex.PersonaFit, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning decomposition example: %w", err)
		}
		ex.CreatedAt = parseTime(createdAt)
		examples = append(examples, ex)
	}
	return examples, rows.Err()
}

// GetDecompFindingCounts returns finding type counts for a target, grouped by
// (finding_type, detail). Only findings from missions with audit_score >= minScore
// are counted. Used by aggregate insight computation.
func (r *RoutingDB) GetDecompFindingCounts(ctx context.Context, targetID string, minScore int) (map[string]int, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT finding_type, COUNT(*) as cnt
		FROM decomposition_findings
		WHERE target_id = ? AND audit_score >= ?
		GROUP BY finding_type
		HAVING cnt >= 1
	`, targetID, minScore)
	if err != nil {
		return nil, fmt.Errorf("querying finding counts for %q: %w", targetID, err)
	}
	defer rows.Close()

	counts := make(map[string]int)
	for rows.Next() {
		var findingType string
		var cnt int
		if err := rows.Scan(&findingType, &cnt); err != nil {
			return nil, fmt.Errorf("scanning finding count: %w", err)
		}
		counts[findingType] = cnt
	}
	return counts, rows.Err()
}

// GetRepeatedFindings returns findings for a target that appear in at least
// minObservations distinct workspaces. This is the core damping mechanism:
// a single audit cannot produce an actionable insight.
func (r *RoutingDB) GetRepeatedFindings(ctx context.Context, targetID string, minScore, minObservations int) ([]DecompFinding, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT f.id, f.target_id, f.workspace_id, f.finding_type, f.phase_name,
		       f.detail, f.decomp_source, f.audit_score, f.created_at
		FROM decomposition_findings f
		INNER JOIN (
			SELECT finding_type, detail
			FROM decomposition_findings
			WHERE target_id = ? AND audit_score >= ?
			GROUP BY finding_type, detail
			HAVING COUNT(DISTINCT workspace_id) >= ?
		) repeated ON f.finding_type = repeated.finding_type AND f.detail = repeated.detail
		WHERE f.target_id = ? AND f.audit_score >= ?
		ORDER BY f.finding_type, f.detail, f.created_at DESC
	`, targetID, minScore, minObservations, targetID, minScore)
	if err != nil {
		return nil, fmt.Errorf("querying repeated findings for %q: %w", targetID, err)
	}
	defer rows.Close()

	var findings []DecompFinding
	for rows.Next() {
		var f DecompFinding
		var createdAt string
		if err := rows.Scan(&f.ID, &f.TargetID, &f.WorkspaceID, &f.FindingType,
			&f.PhaseName, &f.Detail, &f.DecompSource, &f.AuditScore, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning repeated finding: %w", err)
		}
		f.CreatedAt = parseTime(createdAt)
		findings = append(findings, f)
	}
	return findings, rows.Err()
}

// GetRepeatedPassiveFindings returns passive findings (audit_score = 0) for a
// target that appear in at least minObservations distinct workspaces. Passive
// findings are written by launchPassiveAudit after each mission execution and
// are never audited, so they are excluded from GetRepeatedFindings (which gates
// on audit_score >= minScore). This function provides a separate read path so
// the decomposer can incorporate high-frequency passive signals without needing
// an audit review cycle.
func (r *RoutingDB) GetRepeatedPassiveFindings(ctx context.Context, targetID string, minObservations int) ([]DecompFinding, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT f.id, f.target_id, f.workspace_id, f.finding_type, f.phase_name,
		       f.detail, f.decomp_source, f.audit_score, f.created_at
		FROM decomposition_findings f
		INNER JOIN (
			SELECT finding_type, detail
			FROM decomposition_findings
			WHERE target_id = ? AND audit_score = 0
			GROUP BY finding_type, detail
			HAVING COUNT(DISTINCT workspace_id) >= ?
		) repeated ON f.finding_type = repeated.finding_type AND f.detail = repeated.detail
		WHERE f.target_id = ? AND f.audit_score = 0
		ORDER BY f.finding_type, f.detail, f.created_at DESC
	`, targetID, minObservations, targetID)
	if err != nil {
		return nil, fmt.Errorf("querying repeated passive findings for %q: %w", targetID, err)
	}
	defer rows.Close()

	var findings []DecompFinding
	for rows.Next() {
		var f DecompFinding
		var createdAt string
		if err := rows.Scan(&f.ID, &f.TargetID, &f.WorkspaceID, &f.FindingType,
			&f.PhaseName, &f.Detail, &f.DecompSource, &f.AuditScore, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning repeated passive finding: %w", err)
		}
		f.CreatedAt = parseTime(createdAt)
		findings = append(findings, f)
	}
	return findings, rows.Err()
}

// PlanShapeStats summarises the historical decomposition shape for a target.
// Returned by GetPlanShapeStats; nil when insufficient examples exist.
type PlanShapeStats struct {
	AvgPhaseCount  float64  // average phase count across qualifying examples
	MostCommonMode string   // "sequential" or "parallel" (whichever appears most)
	TopPersonas    []string // top-3 most-frequently-used personas, ordered by frequency
	ExampleCount   int      // number of examples that contributed
}

// minPlanShapeExamples is the minimum number of qualifying examples required
// before stats are considered meaningful. Below this threshold, the stats would
// be noisy and could mislead the decomposer.
const minPlanShapeExamples = 2

// GetPlanShapeStats returns distilled plan-shape statistics for targetID by
// reading all qualifying decomposition_examples (audit_score >= minScore and
// decomp_quality >= minScore). Returns nil when fewer than minPlanShapeExamples
// qualifying rows exist. Malformed phases_json rows are skipped without error.
func (r *RoutingDB) GetPlanShapeStats(ctx context.Context, targetID string, minScore int) (*PlanShapeStats, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT phase_count, execution_mode, phases_json
		FROM decomposition_examples
		WHERE target_id = ? AND audit_score >= ? AND decomp_quality >= ?
	`, targetID, minScore, minScore)
	if err != nil {
		return nil, fmt.Errorf("querying plan shape for %q: %w", targetID, err)
	}
	defer rows.Close()

	var totalPhases int
	modeCounts := make(map[string]int)
	personaCounts := make(map[string]int)
	count := 0

	for rows.Next() {
		var phaseCount int
		var execMode, phasesJSON string
		if err := rows.Scan(&phaseCount, &execMode, &phasesJSON); err != nil {
			return nil, fmt.Errorf("scanning plan shape row: %w", err)
		}
		totalPhases += phaseCount
		if execMode != "" {
			modeCounts[execMode]++
		}

		// Parse phases_json to extract persona frequencies.
		// Malformed rows are silently skipped — they should not abort the query.
		var phases []struct {
			Persona string `json:"persona"`
		}
		if jsonErr := json.Unmarshal([]byte(phasesJSON), &phases); jsonErr == nil {
			for _, p := range phases {
				if p.Persona != "" {
					personaCounts[p.Persona]++
				}
			}
		}
		count++
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating plan shape rows: %w", err)
	}

	if count < minPlanShapeExamples {
		return nil, nil //nolint:nilnil // insufficient data; nil is a valid "absent" signal
	}

	// Most common execution mode.
	mostCommonMode := ""
	bestModeCount := 0
	for mode, n := range modeCounts {
		if n > bestModeCount || (n == bestModeCount && mode < mostCommonMode) {
			mostCommonMode = mode
			bestModeCount = n
		}
	}

	// Top-3 personas by frequency.
	type personaFreq struct {
		name  string
		count int
	}
	var pf []personaFreq
	for name, n := range personaCounts {
		pf = append(pf, personaFreq{name, n})
	}
	sort.Slice(pf, func(i, j int) bool {
		if pf[i].count != pf[j].count {
			return pf[i].count > pf[j].count
		}
		return pf[i].name < pf[j].name // stable tiebreak
	})
	const maxTopPersonas = 3
	var topPersonas []string
	for i := 0; i < len(pf) && i < maxTopPersonas; i++ {
		topPersonas = append(topPersonas, pf[i].name)
	}

	return &PlanShapeStats{
		AvgPhaseCount:  float64(totalPhases) / float64(count),
		MostCommonMode: mostCommonMode,
		TopPersonas:    topPersonas,
		ExampleCount:   count,
	}, nil
}

// PhaseShapePattern describes one observed phase structure from a past mission
// for a given target. SuccessCount is the number of distinct successful missions
// that used this exact shape (same phase_count, execution_mode, and persona_seq).
type PhaseShapePattern struct {
	PhaseCount    int
	ExecutionMode string
	PersonaSeq    []string // ordered list of persona names
	SuccessCount  int
}

// MinShapeSuccesses is the minimum number of distinct successful missions that
// must have used a shape before it is surfaced to the decomposer. Below this
// threshold the signal is too noisy to be directive.
const MinShapeSuccesses = 3

// RecordPhaseShape upserts a phase shape observation for (targetID, wsID).
// outcome must be "success" or "failure". taskType is the classified task type
// (see ClassifyTaskType); pass an empty string when unknown.
// The upsert rule is:
//   - New workspace: always inserts.
//   - Same workspace, outcome unchanged: no-op (idempotent).
//   - Same workspace, failure → success (resume succeeded): upgrades the row.
//   - Same workspace, success → failure: no-op (never downgrades a success).
//
// This means a resumed mission that eventually succeeds replaces its earlier
// failure record, so GetSuccessfulShapePatterns counts it as a success.
//
// personaSeq is the ordered list of personas from the plan, joined by commas.
func (r *RoutingDB) RecordPhaseShape(ctx context.Context, targetID, wsID string, phaseCount int, execMode, personaSeq, outcome, taskType string) error {
	if targetID == "" || wsID == "" {
		return fmt.Errorf("targetID and wsID are required")
	}
	if outcome != "success" && outcome != "failure" {
		return fmt.Errorf("outcome must be 'success' or 'failure', got %q", outcome)
	}
	if execMode == "" {
		execMode = "sequential"
	}
	if taskType == "" {
		taskType = string(TaskTypeUnknown)
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT INTO phase_shape_patterns
			(target_id, workspace_id, phase_count, execution_mode, persona_seq, outcome, task_type, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(target_id, workspace_id) DO UPDATE SET
			outcome       = excluded.outcome,
			phase_count   = excluded.phase_count,
			execution_mode = excluded.execution_mode,
			persona_seq   = excluded.persona_seq,
			task_type     = excluded.task_type
		WHERE phase_shape_patterns.outcome = 'failure' AND excluded.outcome = 'success'
	`, targetID, wsID, phaseCount, execMode, personaSeq, outcome, taskType, now)
	if err != nil {
		return fmt.Errorf("recording phase shape for %q/%q: %w", targetID, wsID, err)
	}
	return nil
}

// GetSuccessfulShapePatterns returns phase shapes that have appeared in
// minCount or more distinct successful missions for targetID. Shapes are
// ordered by SuccessCount descending, then PhaseCount ascending for stability.
// Returns nil (not error) when no qualifying shapes exist.
func (r *RoutingDB) GetSuccessfulShapePatterns(ctx context.Context, targetID string, minCount int) ([]PhaseShapePattern, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT phase_count, execution_mode, persona_seq, COUNT(*) AS success_count
		FROM phase_shape_patterns
		WHERE target_id = ? AND outcome = 'success'
		GROUP BY phase_count, execution_mode, persona_seq
		HAVING success_count >= ?
		ORDER BY success_count DESC, phase_count ASC
	`, targetID, minCount)
	if err != nil {
		return nil, fmt.Errorf("querying shape patterns for %q: %w", targetID, err)
	}
	defer rows.Close()

	var patterns []PhaseShapePattern
	for rows.Next() {
		var p PhaseShapePattern
		var personaSeq string
		if err := rows.Scan(&p.PhaseCount, &p.ExecutionMode, &personaSeq, &p.SuccessCount); err != nil {
			return nil, fmt.Errorf("scanning shape pattern: %w", err)
		}
		if personaSeq != "" {
			p.PersonaSeq = strings.Split(personaSeq, ",")
		}
		patterns = append(patterns, p)
	}
	return patterns, rows.Err()
}

// GetTaskTypeSuccessfulShapes returns phase shapes that have appeared in
// minCount or more distinct successful missions for ANY target with the given
// task type. This is the cross-target cold-start fallback: when a target has
// no qualifying target-specific shapes, the decomposer can draw on shapes that
// worked for other targets decomposing the same kind of task.
//
// Returns nil (not error) when no qualifying shapes exist.
// Shapes are ordered by success_count DESC, phase_count ASC for stability.
func (r *RoutingDB) GetTaskTypeSuccessfulShapes(ctx context.Context, taskType string, minCount int) ([]PhaseShapePattern, error) {
	if taskType == "" || taskType == string(TaskTypeUnknown) {
		return nil, nil //nolint:nilnil // unknown type has no cross-target signal
	}
	rows, err := r.db.QueryContext(ctx, `
		SELECT phase_count, execution_mode, persona_seq, COUNT(*) AS success_count
		FROM phase_shape_patterns
		WHERE task_type = ? AND outcome = 'success'
		GROUP BY phase_count, execution_mode, persona_seq
		HAVING success_count >= ?
		ORDER BY success_count DESC, phase_count ASC
	`, taskType, minCount)
	if err != nil {
		return nil, fmt.Errorf("querying cross-target shape patterns for task type %q: %w", taskType, err)
	}
	defer rows.Close()

	var patterns []PhaseShapePattern
	for rows.Next() {
		var p PhaseShapePattern
		var personaSeq string
		if err := rows.Scan(&p.PhaseCount, &p.ExecutionMode, &personaSeq, &p.SuccessCount); err != nil {
			return nil, fmt.Errorf("scanning cross-target shape pattern: %w", err)
		}
		if personaSeq != "" {
			p.PersonaSeq = strings.Split(personaSeq, ",")
		}
		patterns = append(patterns, p)
	}
	return patterns, rows.Err()
}

// RoleAssignment records one observation of a persona serving a specific role
// in a phase execution.
type RoleAssignment struct {
	ID          int64
	TargetID    string
	WorkspaceID string
	PhaseID     string
	Persona     string
	Role        string // "planner", "implementer", "reviewer"
	Outcome     string // "success" or "failure"
	CreatedAt   time.Time
}

// RecordRoleAssignment upserts a role assignment observation for a phase.
func (r *RoutingDB) RecordRoleAssignment(ctx context.Context, ra RoleAssignment) error {
	if ra.TargetID == "" || ra.WorkspaceID == "" || ra.PhaseID == "" {
		return fmt.Errorf("target_id, workspace_id, and phase_id are required")
	}
	if ra.Persona == "" || ra.Role == "" {
		return fmt.Errorf("persona and role are required")
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT INTO role_assignments (target_id, workspace_id, phase_id, persona, role, outcome, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(workspace_id, phase_id) DO UPDATE SET
			target_id = excluded.target_id,
			persona = excluded.persona,
			role = excluded.role,
			outcome = excluded.outcome
	`, ra.TargetID, ra.WorkspaceID, ra.PhaseID, ra.Persona, ra.Role, ra.Outcome, now)
	if err != nil {
		return fmt.Errorf("recording role assignment (target=%q phase=%q): %w", ra.TargetID, ra.PhaseID, err)
	}
	return nil
}

// RolePersonaPattern describes which personas have historically served a given
// role for a target. Used to bias the decomposer toward proven role assignments.
type RolePersonaPattern struct {
	Role       string
	Persona    string
	SeenCount  int
	SuccessRate float64 // fraction of assignments with outcome "success"
}

// GetRolePersonaPatterns returns the top persona assignments per role for a
// target, ordered by seen_count descending. Only roles with at least
// minObservations assignments are included.
func (r *RoutingDB) GetRolePersonaPatterns(ctx context.Context, targetID string, minObservations int) ([]RolePersonaPattern, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT role, persona,
		       COUNT(*) AS seen_count,
		       CAST(SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) AS REAL) / COUNT(*) AS success_rate
		FROM role_assignments
		WHERE target_id = ?
		GROUP BY role, persona
		HAVING seen_count >= ?
		ORDER BY role, seen_count DESC
	`, targetID, minObservations)
	if err != nil {
		return nil, fmt.Errorf("querying role persona patterns for %q: %w", targetID, err)
	}
	defer rows.Close()

	var patterns []RolePersonaPattern
	for rows.Next() {
		var p RolePersonaPattern
		if err := rows.Scan(&p.Role, &p.Persona, &p.SeenCount, &p.SuccessRate); err != nil {
			return nil, fmt.Errorf("scanning role persona pattern: %w", err)
		}
		patterns = append(patterns, p)
	}
	return patterns, rows.Err()
}

// RecordHandoff stores a structured handoff record for a mission execution.
func (r *RoutingDB) RecordHandoff(ctx context.Context, targetID, wsID, fromPhaseID, toPhaseID, fromRole, toRole, fromPersona, toPersona, summary string) error {
	if targetID == "" || wsID == "" {
		return fmt.Errorf("target_id and workspace_id are required")
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT INTO handoff_records (target_id, workspace_id, from_phase_id, to_phase_id, from_role, to_role, from_persona, to_persona, summary, created_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(workspace_id, from_phase_id, to_phase_id) DO UPDATE SET
			target_id = excluded.target_id,
			from_role = excluded.from_role,
			to_role = excluded.to_role,
			from_persona = excluded.from_persona,
			to_persona = excluded.to_persona,
			summary = excluded.summary
	`, targetID, wsID, fromPhaseID, toPhaseID, fromRole, toRole, fromPersona, toPersona, summary, now)
	if err != nil {
		return fmt.Errorf("recording handoff (ws=%q %s→%s): %w", wsID, fromPhaseID, toPhaseID, err)
	}
	return nil
}

// RecordPlanRoutingPatterns records one routing-pattern observation per completed
// phase in plan. Each observation associates the phase's persona with targetID
// and taskType in routing_patterns, growing the confidence score by 0.2 per
// additional observation (capped at 1.0 after five consistent observations).
//
// Only phases with StatusCompleted and a non-empty Persona are recorded; failed,
// skipped, and pending phases are silently skipped so that executor failures do
// not falsely reinforce a poor persona choice. Reviewer and QA support personas
// are excluded so they do not pollute the top-level implementation-persona
// signal; they are captured separately in the role_assignments table.
//
// taskType should be the result of ClassifyTaskType(plan.Task); an empty string
// is accepted and stored as-is (the DB column defaults to '').
//
// Returns the first error encountered; all preceding observations are already
// committed since RecordRoutingPattern runs each upsert in its own statement.
func (r *RoutingDB) RecordPlanRoutingPatterns(ctx context.Context, targetID, taskType string, plan *core.Plan) error {
	if targetID == "" {
		return fmt.Errorf("target_id is required")
	}
	if plan == nil || len(plan.Phases) == 0 {
		return nil
	}
	for _, p := range plan.Phases {
		if p == nil || p.Persona == "" || p.Status != core.StatusCompleted {
			continue
		}
		// Reviewer and QA support personas are captured via role_assignments.
		// Exclude them here so routing_patterns remains a top-level worker
		// selection signal rather than a log of every support phase that ran.
		switch p.Persona {
		case "staff-code-reviewer", "qa-engineer", "security-auditor":
			continue
		}
		if err := r.RecordRoutingPattern(ctx, targetID, p.Persona, taskType); err != nil {
			return err
		}
	}
	return nil
}

// RecordPlanHandoffPatterns walks the dependency edges of a completed plan and
// records one handoff-pattern observation for each persona-to-persona transition.
// This is the live-execution counterpart of ExtractHandoffPatterns, which
// extracts the same signal from post-hoc audit reports.
//
// Only completed phases with non-empty personas are considered. Self-transitions
// (same persona → same persona) are skipped. The receiving phase's name is used
// as the task hint, matching the convention in ExtractHandoffPatterns.
//
// Returns the first error encountered; all preceding observations are already
// committed since RecordHandoffPattern runs each upsert in its own statement.
func (r *RoutingDB) RecordPlanHandoffPatterns(ctx context.Context, targetID string, plan *core.Plan) error {
	if targetID == "" {
		return fmt.Errorf("target_id is required")
	}
	if plan == nil || len(plan.Phases) == 0 {
		return nil
	}

	// Index phases by ID for dependency lookup.
	index := make(map[string]*core.Phase, len(plan.Phases))
	for _, p := range plan.Phases {
		if p != nil {
			index[p.ID] = p
		}
	}

	for _, p := range plan.Phases {
		if p == nil || p.Status != core.StatusCompleted || p.Persona == "" {
			continue
		}
		for _, depID := range p.Dependencies {
			dep := index[depID]
			if dep == nil || dep.Status != core.StatusCompleted || dep.Persona == "" {
				continue
			}
			if dep.Persona == p.Persona {
				continue // self-transition — no handoff
			}
			if err := r.RecordHandoffPattern(ctx, targetID, dep.Persona, p.Persona, p.Name); err != nil {
				return err
			}
		}
	}
	return nil
}

// RoutingDecision records the persona selected for a specific phase at decomposition
// time along with its eventual execution outcome. Outcome starts as "pending" and is
// updated to "success", "failure", or "skipped" when the phase completes.
type RoutingDecision struct {
	ID            int64
	MissionID     string
	PhaseID       string
	PhaseName     string
	Persona       string
	Confidence    float64 // 0.0–1.0; 0.0 when not tracked
	RoutingMethod string  // "llm", "keyword", "predecomposed", or ""
	Outcome       string  // "pending", "success", "failure", "skipped"
	FailureReason string
	CreatedAt     time.Time
	UpdatedAt     time.Time
}

// PersonaRoutingSummary aggregates routing outcomes across all decisions for a persona.
type PersonaRoutingSummary struct {
	Persona        string
	Total          int
	Successes      int
	Failures       int
	Pending        int
	SuccessRate    float64 // 0.0–1.0, computed from resolved (non-pending) decisions
	RecentFailures []RoutingDecision
}

// RecordRoutingDecision inserts a routing decision for a phase. If a decision for the
// same (mission_id, phase_id) pair already exists it is silently ignored.
func (r *RoutingDB) RecordRoutingDecision(ctx context.Context, d RoutingDecision) error {
	if d.MissionID == "" {
		return fmt.Errorf("mission_id is required")
	}
	if d.PhaseID == "" {
		return fmt.Errorf("phase_id is required")
	}
	if d.Persona == "" {
		return fmt.Errorf("persona is required")
	}
	if d.Outcome == "" {
		d.Outcome = "pending"
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO routing_decisions
			(mission_id, phase_id, phase_name, persona, confidence, routing_method, outcome, failure_reason, created_at, updated_at)
		VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	`, d.MissionID, d.PhaseID, d.PhaseName, d.Persona, d.Confidence, d.RoutingMethod,
		d.Outcome, d.FailureReason, now, now)
	if err != nil {
		return fmt.Errorf("recording routing decision (mission=%q phase=%q): %w", d.MissionID, d.PhaseID, err)
	}
	return nil
}

// UpdateRoutingOutcome updates the outcome and optional failure_reason for a
// (mission_id, phase_id) routing decision. No-op when the row does not exist.
func (r *RoutingDB) UpdateRoutingOutcome(ctx context.Context, missionID, phaseID, outcome, failureReason string) error {
	if missionID == "" || phaseID == "" {
		return fmt.Errorf("mission_id and phase_id are required")
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := r.db.ExecContext(ctx, `
		UPDATE routing_decisions
		SET outcome = ?, failure_reason = ?, updated_at = ?
		WHERE mission_id = ? AND phase_id = ?
	`, outcome, failureReason, now, missionID, phaseID)
	if err != nil {
		return fmt.Errorf("updating routing outcome (mission=%q phase=%q): %w", missionID, phaseID, err)
	}
	return nil
}

// GetPersonaRoutingStats returns per-persona aggregates across all routing decisions,
// ordered by total decisions descending. Up to 3 recent failures are included per persona.
func (r *RoutingDB) GetPersonaRoutingStats(ctx context.Context) ([]PersonaRoutingSummary, error) {
	rows, err := r.db.QueryContext(ctx, `
		SELECT persona,
		       COUNT(*)                                             AS total,
		       SUM(CASE WHEN outcome = 'success' THEN 1 ELSE 0 END) AS successes,
		       SUM(CASE WHEN outcome = 'failure' THEN 1 ELSE 0 END) AS failures,
		       SUM(CASE WHEN outcome = 'pending' THEN 1 ELSE 0 END) AS pending
		FROM routing_decisions
		GROUP BY persona
		ORDER BY total DESC
	`)
	if err != nil {
		return nil, fmt.Errorf("querying persona routing stats: %w", err)
	}
	defer rows.Close()

	var summaries []PersonaRoutingSummary
	for rows.Next() {
		var s PersonaRoutingSummary
		if err := rows.Scan(&s.Persona, &s.Total, &s.Successes, &s.Failures, &s.Pending); err != nil {
			return nil, fmt.Errorf("scanning persona routing stats: %w", err)
		}
		resolved := s.Successes + s.Failures
		if resolved > 0 {
			s.SuccessRate = float64(s.Successes) / float64(resolved)
		}
		summaries = append(summaries, s)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	// Attach recent failures to each summary.
	for i := range summaries {
		failures, err := r.GetRecentPersonaFailures(ctx, summaries[i].Persona, 30, 3)
		if err == nil {
			summaries[i].RecentFailures = failures
		}
	}
	return summaries, nil
}

// GetRecentPersonaFailures returns up to limit recent failure decisions for a persona
// within the last days days, ordered by most recent first.
func (r *RoutingDB) GetRecentPersonaFailures(ctx context.Context, persona string, days, limit int) ([]RoutingDecision, error) {
	if persona == "" {
		return nil, fmt.Errorf("persona is required")
	}
	cutoff := time.Now().UTC().AddDate(0, 0, -days).Format(time.RFC3339)
	rows, err := r.db.QueryContext(ctx, `
		SELECT id, mission_id, phase_id, phase_name, persona, confidence, routing_method,
		       outcome, failure_reason, created_at, updated_at
		FROM routing_decisions
		WHERE persona = ? AND outcome = 'failure' AND created_at >= ?
		ORDER BY created_at DESC
		LIMIT ?
	`, persona, cutoff, limit)
	if err != nil {
		return nil, fmt.Errorf("querying recent failures for persona %q: %w", persona, err)
	}
	defer rows.Close()

	var decisions []RoutingDecision
	for rows.Next() {
		var d RoutingDecision
		var createdAt, updatedAt string
		if err := rows.Scan(&d.ID, &d.MissionID, &d.PhaseID, &d.PhaseName, &d.Persona,
			&d.Confidence, &d.RoutingMethod, &d.Outcome, &d.FailureReason,
			&createdAt, &updatedAt); err != nil {
			return nil, fmt.Errorf("scanning routing decision: %w", err)
		}
		d.CreatedAt = parseTime(createdAt)
		d.UpdatedAt = parseTime(updatedAt)
		decisions = append(decisions, d)
	}
	return decisions, rows.Err()
}

// parseTime parses an RFC3339 string, returning zero time on failure.
func parseTime(s string) time.Time {
	t, _ := time.Parse(time.RFC3339, s)
	return t
}
