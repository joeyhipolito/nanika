package metrics

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	_ "modernc.org/sqlite"
)

// ErrDuplicate is returned by RecordMission when the workspace_id already exists.
var ErrDuplicate = errors.New("metrics: duplicate workspace_id")

// SkillSource values distinguish how a skill invocation was detected.
const (
	SkillSourceDeclared    = "declared"     // listed in the PHASE SKILL block
	SkillSourceOutputParse = "output_parse" // detected in the worker output transcript
)

// decompSourceUnknown is the fallback stored when a mission record carries no DecompSource.
// Matches the DEFAULT value in the DDL and migration SQL.
const decompSourceUnknown = "unknown"

// DB wraps a SQLite database for metrics storage.
type DB struct {
	db *sql.DB
}

const upsertMissionSQL = `
	INSERT INTO missions (
		id, domain, task, started_at, finished_at, duration_s,
		phases_total, phases_completed, phases_failed, phases_skipped,
		learnings_retrieved, retries_total, gate_failures, output_len_total, status,
		decomp_source, tokens_in_total, tokens_out_total, cost_usd_total
	) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	ON CONFLICT(id) DO UPDATE SET
		domain = excluded.domain,
		task = excluded.task,
		started_at = excluded.started_at,
		finished_at = excluded.finished_at,
		duration_s = excluded.duration_s,
		phases_total = excluded.phases_total,
		phases_completed = excluded.phases_completed,
		phases_failed = excluded.phases_failed,
		phases_skipped = excluded.phases_skipped,
		learnings_retrieved = excluded.learnings_retrieved,
		retries_total = excluded.retries_total,
		gate_failures = excluded.gate_failures,
		output_len_total = excluded.output_len_total,
		status = excluded.status,
		decomp_source = CASE
			WHEN excluded.decomp_source = ? AND missions.decomp_source != ? THEN missions.decomp_source
			ELSE excluded.decomp_source
		END,
		tokens_in_total = excluded.tokens_in_total,
		tokens_out_total = excluded.tokens_out_total,
		cost_usd_total = excluded.cost_usd_total
`

// MissionRecord mirrors engine.MissionMetrics for storage.
// Declared here to avoid an import cycle (engine imports internal/metrics).
type MissionRecord struct {
	WorkspaceID        string        `json:"workspace_id"`
	Domain             string        `json:"domain"`
	Task               string        `json:"task,omitempty"`
	StartedAt          time.Time     `json:"started_at"`
	FinishedAt         time.Time     `json:"finished_at"`
	DurationSec        int           `json:"duration_s"`
	PhasesTotal        int           `json:"phases_total"`
	PhasesCompleted    int           `json:"phases_completed"`
	PhasesFailed       int           `json:"phases_failed"`
	PhasesSkipped      int           `json:"phases_skipped"`
	LearningsRetrieved int           `json:"learnings_retrieved"`
	RetriesTotal       int           `json:"retries_total"`
	GateFailures       int           `json:"gate_failures"`
	OutputLenTotal     int           `json:"output_len_total"`
	Status             string        `json:"status"`
	DecompSource       string        `json:"decomp_source,omitempty"` // "predecomposed", "decomp.llm", "decomp.keyword", "template"
	Phases             []PhaseRecord `json:"phases,omitempty"`
	// Mission-level cost rollups
	TokensInTotal  int     `json:"tokens_in_total,omitempty"`
	TokensOutTotal int     `json:"tokens_out_total,omitempty"`
	CostUSDTotal   float64 `json:"cost_usd_total,omitempty"`
}

// PhaseRecord mirrors engine.PhaseMetric for storage.
type PhaseRecord struct {
	// ID is the stable plan phase identifier (e.g. "fix-dag-deadlock"). It forms
	// the second component of the database primary key: workspaceID + "_" + ID.
	// Empty on legacy JSONL records written before this field existed; RecordMission
	// falls back to Name in that case.
	ID                 string   `json:"id,omitempty"`
	Name               string   `json:"name"`
	Persona            string   `json:"persona"`
	Skills             []string `json:"skills,omitempty"`
	ParsedSkills       []string `json:"parsed_skills,omitempty"`
	SelectionMethod    string   `json:"selection_method,omitempty"` // "llm" or "keyword"
	DurationS          int      `json:"duration_s"`
	Status             string   `json:"status"`
	Retries            int      `json:"retries,omitempty"`
	GatePassed         bool     `json:"gate_passed"`
	OutputLen          int      `json:"output_len"`
	LearningsRetrieved int      `json:"learnings_retrieved,omitempty"`
	// Cost attribution
	Provider  string  `json:"provider,omitempty"`   // always "anthropic"
	Model     string  `json:"model,omitempty"`      // resolved model ID
	TokensIn  int     `json:"tokens_in,omitempty"`  // input tokens
	TokensOut int     `json:"tokens_out,omitempty"` // output tokens
	CostUSD   float64 `json:"cost_usd,omitempty"`   // cost in USD
}

func (p *PhaseRecord) UnmarshalJSON(data []byte) error {
	type phaseRecordJSON struct {
		ID                    string   `json:"id,omitempty"`
		Name                  string   `json:"name"`
		Persona               string   `json:"persona"`
		Skills                []string `json:"skills,omitempty"`
		ParsedSkills          []string `json:"parsed_skills,omitempty"`
		SelectionMethod       string   `json:"persona_selection_method,omitempty"`
		LegacySelectionMethod string   `json:"selection_method,omitempty"`
		DurationS             int      `json:"duration_s"`
		Status                string   `json:"status"`
		Retries               int      `json:"retries,omitempty"`
		GatePassed            bool     `json:"gate_passed"`
		OutputLen             int      `json:"output_len"`
		LearningsRetrieved    int      `json:"learnings_retrieved,omitempty"`
		Provider              string   `json:"provider,omitempty"`
		Model                 string   `json:"model,omitempty"`
		TokensIn              int      `json:"tokens_in,omitempty"`
		TokensOut             int      `json:"tokens_out,omitempty"`
		CostUSD               float64  `json:"cost_usd,omitempty"`
	}

	var aux phaseRecordJSON
	if err := json.Unmarshal(data, &aux); err != nil {
		return err
	}

	p.ID = aux.ID
	p.Name = aux.Name
	p.Persona = aux.Persona
	p.Skills = aux.Skills
	p.ParsedSkills = aux.ParsedSkills
	p.SelectionMethod = aux.SelectionMethod
	if p.SelectionMethod == "" {
		p.SelectionMethod = aux.LegacySelectionMethod
	}
	p.DurationS = aux.DurationS
	p.Status = aux.Status
	p.Retries = aux.Retries
	p.GatePassed = aux.GatePassed
	p.OutputLen = aux.OutputLen
	p.LearningsRetrieved = aux.LearningsRetrieved
	p.Provider = aux.Provider
	p.Model = aux.Model
	p.TokensIn = aux.TokensIn
	p.TokensOut = aux.TokensOut
	p.CostUSD = aux.CostUSD
	return nil
}

// InitDB opens or creates the metrics database and auto-migrates the schema.
func InitDB(path string) (*DB, error) {
	if path == "" {
		base, err := config.Dir()
		if err != nil {
			return nil, fmt.Errorf("config dir: %w", err)
		}
		path = filepath.Join(base, "metrics.db")
	}

	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		return nil, fmt.Errorf("creating metrics dir: %w", err)
	}

	db, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("opening metrics db: %w", err)
	}

	pragmas := []string{
		"PRAGMA journal_mode=WAL",
		"PRAGMA busy_timeout=5000",
		"PRAGMA foreign_keys=ON", // enforce FK constraints and ON DELETE CASCADE
	}
	for _, p := range pragmas {
		if _, err := db.Exec(p); err != nil {
			db.Close()
			return nil, fmt.Errorf("setting pragma %q: %w", p, err)
		}
	}

	mdb := &DB{db: db}
	if err := mdb.initSchema(); err != nil {
		db.Close()
		return nil, fmt.Errorf("initializing schema: %w", err)
	}

	return mdb, nil
}

// Close closes the database connection.
func (d *DB) Close() error {
	return d.db.Close()
}

// RawDB returns the underlying *sql.DB for callers that need custom queries
// beyond the structured API (e.g. the dashboard API server).
func (d *DB) RawDB() *sql.DB {
	return d.db
}

// initSchema creates all tables and indexes in a single transaction so the schema
// is either fully applied or fully rolled back — no partial state on a crash.
func (d *DB) initSchema() error {
	tx, err := d.db.Begin()
	if err != nil {
		return fmt.Errorf("beginning schema transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	ddl := []struct {
		name string
		sql  string
	}{
		{"missions table", `
			CREATE TABLE IF NOT EXISTS missions (
				id                  TEXT PRIMARY KEY,
				domain              TEXT NOT NULL DEFAULT '',
				task                TEXT NOT NULL DEFAULT '',
				started_at          DATETIME NOT NULL,
				finished_at         DATETIME NOT NULL,
				duration_s          INTEGER NOT NULL DEFAULT 0,
				phases_total        INTEGER NOT NULL DEFAULT 0,
				phases_completed    INTEGER NOT NULL DEFAULT 0,
				phases_failed       INTEGER NOT NULL DEFAULT 0,
				phases_skipped      INTEGER NOT NULL DEFAULT 0,
				learnings_retrieved INTEGER NOT NULL DEFAULT 0,
				retries_total       INTEGER NOT NULL DEFAULT 0,
				gate_failures       INTEGER NOT NULL DEFAULT 0,
				output_len_total    INTEGER NOT NULL DEFAULT 0,
				status              TEXT NOT NULL DEFAULT '',
				decomp_source       TEXT NOT NULL DEFAULT 'unknown',
				tokens_in_total     INTEGER NOT NULL DEFAULT 0,
				tokens_out_total    INTEGER NOT NULL DEFAULT 0,
				cost_usd_total      REAL NOT NULL DEFAULT 0
			)`},
		{"phases table", `
			CREATE TABLE IF NOT EXISTS phases (
				id                  TEXT PRIMARY KEY,
				mission_id          TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
				name                TEXT NOT NULL DEFAULT '',
				persona             TEXT NOT NULL DEFAULT '',
				selection_method    TEXT NOT NULL DEFAULT '',
				duration_s          INTEGER NOT NULL DEFAULT 0,
				status              TEXT NOT NULL DEFAULT '',
				retries             INTEGER NOT NULL DEFAULT 0,
				gate_passed         INTEGER NOT NULL DEFAULT 0,
				output_len          INTEGER NOT NULL DEFAULT 0,
				learnings_retrieved INTEGER NOT NULL DEFAULT 0,
				provider            TEXT NOT NULL DEFAULT '',
				model               TEXT NOT NULL DEFAULT '',
				tokens_in           INTEGER NOT NULL DEFAULT 0,
				tokens_out          INTEGER NOT NULL DEFAULT 0,
				cost_usd            REAL NOT NULL DEFAULT 0,
				parsed_skills       TEXT NOT NULL DEFAULT ''
			)`},
		{"skill_invocations table", `
			CREATE TABLE IF NOT EXISTS skill_invocations (
				id         INTEGER PRIMARY KEY AUTOINCREMENT,
				mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
				phase      TEXT NOT NULL DEFAULT '',
				persona    TEXT NOT NULL DEFAULT '',
				skill_name TEXT NOT NULL DEFAULT '',
				source     TEXT NOT NULL DEFAULT 'declared',
				invoked_at DATETIME NOT NULL
			)`},
		{"idx_missions_domain", `CREATE INDEX IF NOT EXISTS idx_missions_domain ON missions(domain)`},
		{"idx_missions_status", `CREATE INDEX IF NOT EXISTS idx_missions_status ON missions(status)`},
		{"idx_missions_started_at", `CREATE INDEX IF NOT EXISTS idx_missions_started_at ON missions(started_at)`},
		{"idx_phases_mission_id", `CREATE INDEX IF NOT EXISTS idx_phases_mission_id ON phases(mission_id)`},
		{"idx_phases_persona", `CREATE INDEX IF NOT EXISTS idx_phases_persona ON phases(persona)`},
		{"idx_skill_invocations_mission_id", `CREATE INDEX IF NOT EXISTS idx_skill_invocations_mission_id ON skill_invocations(mission_id)`},
		{"idx_skill_invocations_skill_name", `CREATE INDEX IF NOT EXISTS idx_skill_invocations_skill_name ON skill_invocations(skill_name)`},
		{"idx_skill_invocations_persona", `CREATE INDEX IF NOT EXISTS idx_skill_invocations_persona ON skill_invocations(persona)`},
		{"idx_skill_invocations_persona_skill", `CREATE INDEX IF NOT EXISTS idx_skill_invocations_persona_skill ON skill_invocations(persona, skill_name)`},
		// NOTE: idx_skill_invocations_phase is created AFTER migrations below,
		// because the "phase" column may not exist yet on pre-existing databases.
	}

	for _, stmt := range ddl {
		if _, err := tx.Exec(stmt.sql); err != nil {
			return fmt.Errorf("creating %s: %w", stmt.name, err)
		}
	}

	// Additive migrations — idempotent via "already has a column named" check.
	// These handle databases created before the column was added to the DDL above.
	migrations := []struct{ name, sql string }{
		{"add phases.selection_method", `ALTER TABLE phases ADD COLUMN selection_method TEXT NOT NULL DEFAULT ''`},
		{"add phases.provider", `ALTER TABLE phases ADD COLUMN provider TEXT NOT NULL DEFAULT ''`},
		{"add phases.model", `ALTER TABLE phases ADD COLUMN model TEXT NOT NULL DEFAULT ''`},
		{"add phases.tokens_in", `ALTER TABLE phases ADD COLUMN tokens_in INTEGER NOT NULL DEFAULT 0`},
		{"add phases.tokens_out", `ALTER TABLE phases ADD COLUMN tokens_out INTEGER NOT NULL DEFAULT 0`},
		{"add phases.cost_usd", `ALTER TABLE phases ADD COLUMN cost_usd REAL NOT NULL DEFAULT 0`},
		{"add missions.tokens_in_total", `ALTER TABLE missions ADD COLUMN tokens_in_total INTEGER NOT NULL DEFAULT 0`},
		{"add missions.tokens_out_total", `ALTER TABLE missions ADD COLUMN tokens_out_total INTEGER NOT NULL DEFAULT 0`},
		{"add missions.cost_usd_total", `ALTER TABLE missions ADD COLUMN cost_usd_total REAL NOT NULL DEFAULT 0`},
		{"add skill_invocations.phase", `ALTER TABLE skill_invocations ADD COLUMN phase TEXT NOT NULL DEFAULT ''`},
		{"add skill_invocations.source", `ALTER TABLE skill_invocations ADD COLUMN source TEXT NOT NULL DEFAULT 'declared'`},
		{"add missions.decomp_source", `ALTER TABLE missions ADD COLUMN decomp_source TEXT NOT NULL DEFAULT 'unknown'`},
		{"add phases.parsed_skills", `ALTER TABLE phases ADD COLUMN parsed_skills TEXT NOT NULL DEFAULT ''`},
	}
	for _, m := range migrations {
		if _, err := tx.Exec(m.sql); err != nil {
			// modernc.org/sqlite returns "duplicate column name: <col>" when the column exists.
			if !strings.Contains(err.Error(), "duplicate column name") {
				return fmt.Errorf("migration %s: %w", m.name, err)
			}
		}
	}

	// Post-migration indexes — these reference columns that may have been added
	// by the migrations above, so they MUST run after the ALTER TABLE statements.
	// On a fresh database the columns exist from the CREATE TABLE, so these are
	// no-ops via IF NOT EXISTS. On a migrated database the column was just added.
	postMigrationIndexes := []struct{ name, sql string }{
		{"idx_skill_invocations_phase", `CREATE INDEX IF NOT EXISTS idx_skill_invocations_phase ON skill_invocations(phase)`},
	}
	for _, idx := range postMigrationIndexes {
		if _, err := tx.Exec(idx.sql); err != nil {
			return fmt.Errorf("creating %s: %w", idx.name, err)
		}
	}

	return tx.Commit()
}

// RecordMission inserts a mission and its phases into the database.
// Uses INSERT OR IGNORE — duplicate workspace_id is silently skipped (idempotent).
func (d *DB) RecordMission(ctx context.Context, m MissionRecord) error {
	if m.WorkspaceID == "" {
		return fmt.Errorf("workspace_id is required")
	}

	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	decompSource := m.DecompSource
	if decompSource == "" {
		decompSource = decompSourceUnknown
	}

	res, err := tx.ExecContext(ctx, `
		INSERT OR IGNORE INTO missions (
			id, domain, task, started_at, finished_at, duration_s,
			phases_total, phases_completed, phases_failed, phases_skipped,
			learnings_retrieved, retries_total, gate_failures, output_len_total, status,
			decomp_source, tokens_in_total, tokens_out_total, cost_usd_total
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	`,
		m.WorkspaceID, m.Domain, m.Task,
		m.StartedAt.UTC().Format(time.RFC3339),
		m.FinishedAt.UTC().Format(time.RFC3339),
		m.DurationSec,
		m.PhasesTotal, m.PhasesCompleted, m.PhasesFailed, m.PhasesSkipped,
		m.LearningsRetrieved, m.RetriesTotal, m.GateFailures, m.OutputLenTotal,
		m.Status, decompSource,
		m.TokensInTotal, m.TokensOutTotal, m.CostUSDTotal,
	)
	if err != nil {
		return fmt.Errorf("inserting mission %s: %w", m.WorkspaceID, err)
	}
	if n, _ := res.RowsAffected(); n == 0 {
		return ErrDuplicate
	}

	recordedAt := time.Now().UTC().Format(time.RFC3339)
	for _, p := range m.Phases {
		if err := insertPhase(ctx, tx, m.WorkspaceID, p); err != nil {
			return err
		}
		for _, skill := range p.Skills {
			if err := insertSkillInvocation(ctx, tx, m.WorkspaceID, p.Name, p.Persona, skill, SkillSourceDeclared, recordedAt); err != nil {
				return err
			}
		}
		for _, skill := range p.ParsedSkills {
			if err := insertSkillInvocation(ctx, tx, m.WorkspaceID, p.Name, p.Persona, skill, SkillSourceOutputParse, recordedAt); err != nil {
				return err
			}
		}
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("committing mission %s: %w", m.WorkspaceID, err)
	}
	return nil
}

func insertMissionRowIfMissing(ctx context.Context, execer execContexter, m MissionRecord) (bool, error) {
	decompSource := normalizeDecompSource(m.DecompSource)

	res, err := execer.ExecContext(ctx, `
		INSERT OR IGNORE INTO missions (
			id, domain, task, started_at, finished_at, duration_s,
			phases_total, phases_completed, phases_failed, phases_skipped,
			learnings_retrieved, retries_total, gate_failures, output_len_total, status,
			decomp_source, tokens_in_total, tokens_out_total, cost_usd_total
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	`,
		m.WorkspaceID, m.Domain, m.Task,
		m.StartedAt.UTC().Format(time.RFC3339),
		m.FinishedAt.UTC().Format(time.RFC3339),
		m.DurationSec,
		m.PhasesTotal, m.PhasesCompleted, m.PhasesFailed, m.PhasesSkipped,
		m.LearningsRetrieved, m.RetriesTotal, m.GateFailures, m.OutputLenTotal,
		m.Status, decompSource,
		m.TokensInTotal, m.TokensOutTotal, m.CostUSDTotal,
	)
	if err != nil {
		return false, fmt.Errorf("inserting mission %s: %w", m.WorkspaceID, err)
	}
	n, _ := res.RowsAffected()
	return n > 0, nil
}

type queryRowContexter interface {
	QueryRowContext(ctx context.Context, query string, args ...any) *sql.Row
}

func missionExists(ctx context.Context, queryer queryRowContexter, missionID string) (bool, error) {
	var existing string
	err := queryer.QueryRowContext(ctx, `SELECT id FROM missions WHERE id = ? LIMIT 1`, missionID).Scan(&existing)
	if errors.Is(err, sql.ErrNoRows) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	return true, nil
}

func normalizeDecompSource(src string) string {
	if src == "" {
		return decompSourceUnknown
	}
	return src
}

func missionUpsertArgs(m MissionRecord) []any {
	decompSource := normalizeDecompSource(m.DecompSource)
	return []any{
		m.WorkspaceID, m.Domain, m.Task,
		m.StartedAt.UTC().Format(time.RFC3339),
		m.FinishedAt.UTC().Format(time.RFC3339),
		m.DurationSec,
		m.PhasesTotal, m.PhasesCompleted, m.PhasesFailed, m.PhasesSkipped,
		m.LearningsRetrieved, m.RetriesTotal, m.GateFailures, m.OutputLenTotal,
		m.Status, decompSource,
		m.TokensInTotal, m.TokensOutTotal, m.CostUSDTotal,
		decompSourceUnknown, decompSourceUnknown,
	}
}

func upsertMissionRow(ctx context.Context, execer execContexter, m MissionRecord) error {
	if _, err := execer.ExecContext(ctx, upsertMissionSQL, missionUpsertArgs(m)...); err != nil {
		return fmt.Errorf("upserting mission %s: %w", m.WorkspaceID, err)
	}
	return nil
}

// marshalSkillsJSON serializes a skill slice to a JSON array string for storage.
// Returns "" for nil or empty slices.
func marshalSkillsJSON(skills []string) string {
	if len(skills) == 0 {
		return ""
	}
	b, err := json.Marshal(skills)
	if err != nil {
		return ""
	}
	return string(b)
}

// unmarshalSkillsJSON deserializes a stored JSON array string back to a slice.
// Returns nil for empty or invalid input.
func unmarshalSkillsJSON(s string) []string {
	if s == "" {
		return nil
	}
	var skills []string
	if err := json.Unmarshal([]byte(s), &skills); err != nil {
		return nil
	}
	return skills
}

func insertPhase(ctx context.Context, tx *sql.Tx, missionID string, p PhaseRecord) error {
	phaseKey := p.ID
	if phaseKey == "" {
		phaseKey = p.Name
	}
	phaseID := missionID + "_" + phaseKey
	gatePassed := 0
	if p.GatePassed {
		gatePassed = 1
	}

	_, err := tx.ExecContext(ctx, `
		INSERT OR IGNORE INTO phases (
			id, mission_id, name, persona, selection_method, duration_s,
			status, retries, gate_passed, output_len, learnings_retrieved,
			provider, model, tokens_in, tokens_out, cost_usd, parsed_skills
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
	`,
		phaseID, missionID, p.Name, p.Persona, p.SelectionMethod, p.DurationS,
		p.Status, p.Retries, gatePassed, p.OutputLen, p.LearningsRetrieved,
		p.Provider, p.Model, p.TokensIn, p.TokensOut, p.CostUSD,
		marshalSkillsJSON(p.ParsedSkills),
	)
	if err != nil {
		return fmt.Errorf("inserting phase %s: %w", phaseID, err)
	}
	return nil
}

func upsertPhase(ctx context.Context, execer execContexter, missionID string, p PhaseRecord) error {
	phaseKey := p.ID
	if phaseKey == "" {
		phaseKey = p.Name
	}
	phaseID := missionID + "_" + phaseKey
	gatePassed := 0
	if p.GatePassed {
		gatePassed = 1
	}

	_, err := execer.ExecContext(ctx, `
		INSERT INTO phases (
			id, mission_id, name, persona, selection_method, duration_s,
			status, retries, gate_passed, output_len, learnings_retrieved,
			provider, model, tokens_in, tokens_out, cost_usd, parsed_skills
		) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT(id) DO UPDATE SET
			mission_id = excluded.mission_id,
			name = excluded.name,
			persona = excluded.persona,
			selection_method = excluded.selection_method,
			duration_s = excluded.duration_s,
			status = excluded.status,
			retries = excluded.retries,
			gate_passed = excluded.gate_passed,
			output_len = excluded.output_len,
			learnings_retrieved = excluded.learnings_retrieved,
			provider = excluded.provider,
			model = excluded.model,
			tokens_in = excluded.tokens_in,
			tokens_out = excluded.tokens_out,
			cost_usd = excluded.cost_usd,
			parsed_skills = excluded.parsed_skills
	`,
		phaseID, missionID, p.Name, p.Persona, p.SelectionMethod, p.DurationS,
		p.Status, p.Retries, gatePassed, p.OutputLen, p.LearningsRetrieved,
		p.Provider, p.Model, p.TokensIn, p.TokensOut, p.CostUSD,
		marshalSkillsJSON(p.ParsedSkills),
	)
	if err != nil {
		return fmt.Errorf("upserting phase %s: %w", phaseID, err)
	}
	return nil
}

type execContexter interface {
	ExecContext(ctx context.Context, query string, args ...any) (sql.Result, error)
}

func insertSkillInvocation(ctx context.Context, execer execContexter, missionID, phase, persona, skillName, source, invokedAt string) error {
	if missionID == "" {
		return fmt.Errorf("missionID is required")
	}
	if skillName == "" {
		return fmt.Errorf("skillName is required")
	}
	switch source {
	case SkillSourceDeclared, SkillSourceOutputParse:
	default:
		return fmt.Errorf("invalid skill source %q", source)
	}

	_, err := execer.ExecContext(ctx, `
		INSERT INTO skill_invocations (mission_id, phase, persona, skill_name, source, invoked_at)
		VALUES (?, ?, ?, ?, ?, ?)
	`, missionID, phase, persona, skillName, source, invokedAt)
	if err != nil {
		return fmt.Errorf("recording skill invocation %s/%s for mission %s phase %s: %w", persona, skillName, missionID, phase, err)
	}
	return nil
}

func missionHasSkillData(m MissionRecord) bool {
	for _, p := range m.Phases {
		if len(p.Skills) > 0 || len(p.ParsedSkills) > 0 {
			return true
		}
	}
	return false
}

func hydrateImportedPhaseSkills(ctx context.Context, tx *sql.Tx, m *MissionRecord) error {
	if m == nil || len(m.Phases) == 0 {
		return nil
	}

	type phaseKey struct {
		name    string
		persona string
	}

	rows, err := tx.QueryContext(ctx, `
		SELECT phase, persona, skill_name, source
		FROM skill_invocations
		WHERE mission_id = ?
	`, m.WorkspaceID)
	if err != nil {
		return fmt.Errorf("loading existing skill invocations for mission %s: %w", m.WorkspaceID, err)
	}
	defer rows.Close()

	declared := make(map[phaseKey][]string)
	parsed := make(map[phaseKey][]string)
	for rows.Next() {
		var phaseName, persona, skillName, source string
		if err := rows.Scan(&phaseName, &persona, &skillName, &source); err != nil {
			return fmt.Errorf("scanning existing skill invocation for mission %s: %w", m.WorkspaceID, err)
		}
		key := phaseKey{name: phaseName, persona: persona}
		switch source {
		case SkillSourceDeclared:
			declared[key] = append(declared[key], skillName)
		case SkillSourceOutputParse:
			parsed[key] = append(parsed[key], skillName)
		}
	}
	if err := rows.Err(); err != nil {
		return fmt.Errorf("iterating existing skill invocations for mission %s: %w", m.WorkspaceID, err)
	}

	for i := range m.Phases {
		key := phaseKey{name: m.Phases[i].Name, persona: m.Phases[i].Persona}
		if len(m.Phases[i].Skills) == 0 {
			m.Phases[i].Skills = append([]string(nil), declared[key]...)
		}
		if len(m.Phases[i].ParsedSkills) == 0 {
			m.Phases[i].ParsedSkills = append([]string(nil), parsed[key]...)
		}
	}
	return nil
}

func replaceMissionPhaseData(ctx context.Context, tx *sql.Tx, m MissionRecord) error {
	if _, err := tx.ExecContext(ctx, `DELETE FROM skill_invocations WHERE mission_id = ?`, m.WorkspaceID); err != nil {
		return fmt.Errorf("clearing skill invocations for mission %s: %w", m.WorkspaceID, err)
	}
	if _, err := tx.ExecContext(ctx, `DELETE FROM phases WHERE mission_id = ?`, m.WorkspaceID); err != nil {
		return fmt.Errorf("clearing phases for mission %s: %w", m.WorkspaceID, err)
	}

	recordedAt := time.Now().UTC().Format(time.RFC3339)
	for _, p := range m.Phases {
		if err := insertPhase(ctx, tx, m.WorkspaceID, p); err != nil {
			return err
		}
		for _, skill := range p.Skills {
			if err := insertSkillInvocation(ctx, tx, m.WorkspaceID, p.Name, p.Persona, skill, SkillSourceDeclared, recordedAt); err != nil {
				return err
			}
		}
		for _, skill := range p.ParsedSkills {
			if err := insertSkillInvocation(ctx, tx, m.WorkspaceID, p.Name, p.Persona, skill, SkillSourceOutputParse, recordedAt); err != nil {
				return err
			}
		}
	}
	return nil
}

// UpsertMission stores the full mission snapshot in one transaction. Existing
// missions are refreshed so retries and JSONL re-import can repair missing
// phase or skill rows instead of preserving partial state. When the incoming
// snapshot omits phases, the mission row is still refreshed but existing phase
// and skill rows are preserved.
// The returned bool reports whether the mission row was newly created.
func (d *DB) UpsertMission(ctx context.Context, m MissionRecord) (bool, error) {
	if m.WorkspaceID == "" {
		return false, fmt.Errorf("workspace_id is required")
	}

	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return false, fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	exists, err := missionExists(ctx, tx, m.WorkspaceID)
	if err != nil {
		return false, fmt.Errorf("checking mission %s: %w", m.WorkspaceID, err)
	}

	if err := upsertMissionRow(ctx, tx, m); err != nil {
		return false, err
	}

	if len(m.Phases) == 0 {
		if err := tx.Commit(); err != nil {
			return false, fmt.Errorf("committing mission %s: %w", m.WorkspaceID, err)
		}
		return !exists, nil
	}

	if err := replaceMissionPhaseData(ctx, tx, m); err != nil {
		return false, err
	}

	if err := tx.Commit(); err != nil {
		return false, fmt.Errorf("committing mission %s: %w", m.WorkspaceID, err)
	}
	return !exists, nil
}

// UpsertMissionPhaseSnapshot updates mission summary state and replaces the
// recorded skills for a single completed phase in one transaction.
func (d *DB) UpsertMissionPhaseSnapshot(ctx context.Context, mission MissionRecord, phase PhaseRecord) error {
	if mission.WorkspaceID == "" {
		return fmt.Errorf("workspace_id is required")
	}

	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	if err := upsertMissionRow(ctx, tx, mission); err != nil {
		return err
	}
	if err := upsertPhase(ctx, tx, mission.WorkspaceID, phase); err != nil {
		return err
	}
	if _, err := tx.ExecContext(ctx, `
		DELETE FROM skill_invocations
		WHERE mission_id = ? AND phase = ?
	`, mission.WorkspaceID, phase.Name); err != nil {
		return fmt.Errorf("clearing skill invocations for mission %s phase %s: %w", mission.WorkspaceID, phase.Name, err)
	}

	recordedAt := time.Now().UTC().Format(time.RFC3339)
	for _, skill := range phase.Skills {
		if err := insertSkillInvocation(ctx, tx, mission.WorkspaceID, phase.Name, phase.Persona, skill, SkillSourceDeclared, recordedAt); err != nil {
			return err
		}
	}
	for _, skill := range phase.ParsedSkills {
		if err := insertSkillInvocation(ctx, tx, mission.WorkspaceID, phase.Name, phase.Persona, skill, SkillSourceOutputParse, recordedAt); err != nil {
			return err
		}
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("committing mission phase snapshot %s/%s: %w", mission.WorkspaceID, phase.Name, err)
	}
	return nil
}

// RecordSkillInvocation records a single skill invocation for a phase.
// phase is the phase name (e.g. "implement"); persona is the assigned persona.
// source is "declared" for skills listed in the PHASE SKILL block, or
// "output_parse" for skills detected in the worker output transcript.
func (d *DB) RecordSkillInvocation(ctx context.Context, missionID, phase, persona, skillName, source string) error {
	return insertSkillInvocation(ctx, d.db, missionID, phase, persona, skillName, source, time.Now().UTC().Format(time.RFC3339))
}

// ImportFromJSONL reads an existing metrics.jsonl file and imports all records
// into the database. Existing missions are refreshed without increasing the
// imported count, so repeated imports are safe and can repair stale mission
// fields in addition to missing phase or skill rows.
// Returns the number of newly created mission rows imported.
func (d *DB) ImportFromJSONL(ctx context.Context, path string) (int, error) {
	path, err := resolveImportPath(path)
	if err != nil {
		return 0, err
	}

	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil // nothing to import
		}
		return 0, fmt.Errorf("opening %s: %w", path, err)
	}
	defer f.Close()

	// 1MB buffer handles large task strings (multi-phase mission descriptions can be long)
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1<<20), 1<<20)

	var imported int
	for scanner.Scan() {
		raw := scanner.Bytes()
		if len(raw) == 0 {
			continue
		}

		var m MissionRecord
		if err := json.Unmarshal(raw, &m); err != nil {
			continue // skip malformed lines
		}
		if m.WorkspaceID == "" {
			continue
		}

		created, err := d.upsertImportedMission(ctx, m)
		if err != nil {
			return imported, fmt.Errorf("importing record %s: %w", m.WorkspaceID, err)
		}
		if created {
			imported++
		}
	}

	if err := scanner.Err(); err != nil {
		return imported, fmt.Errorf("scanning %s: %w", path, err)
	}
	return imported, nil
}

// ImportMissingFromJSONL backfills only missions that are absent from SQLite.
// Unlike ImportFromJSONL, it never refreshes existing rows, so it is safe for
// read-only command paths that should not rewrite richer live phase snapshots.
func (d *DB) ImportMissingFromJSONL(ctx context.Context, path string) (int, error) {
	path, err := resolveImportPath(path)
	if err != nil {
		return 0, err
	}

	f, err := os.Open(path)
	if err != nil {
		if os.IsNotExist(err) {
			return 0, nil // nothing to import
		}
		return 0, fmt.Errorf("opening %s: %w", path, err)
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1<<20), 1<<20)

	var imported int
	for scanner.Scan() {
		raw := scanner.Bytes()
		if len(raw) == 0 {
			continue
		}

		var m MissionRecord
		if err := json.Unmarshal(raw, &m); err != nil {
			continue
		}
		if m.WorkspaceID == "" {
			continue
		}

		created, err := d.insertImportedMissionIfMissing(ctx, m)
		if err != nil {
			return imported, fmt.Errorf("importing missing record %s: %w", m.WorkspaceID, err)
		}
		if created {
			imported++
		}
	}

	if err := scanner.Err(); err != nil {
		return imported, fmt.Errorf("scanning %s: %w", path, err)
	}
	return imported, nil
}

func resolveImportPath(path string) (string, error) {
	if path == "" {
		base, err := config.Dir()
		if err != nil {
			return "", fmt.Errorf("config dir: %w", err)
		}
		path = filepath.Join(base, "metrics.jsonl")
	}
	return path, nil
}

func (d *DB) upsertImportedMission(ctx context.Context, m MissionRecord) (bool, error) {
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return false, fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	exists, err := missionExists(ctx, tx, m.WorkspaceID)
	if err != nil {
		return false, fmt.Errorf("checking mission %s: %w", m.WorkspaceID, err)
	}

	record := m
	if exists && len(record.Phases) > 0 && !missionHasSkillData(record) {
		if err := hydrateImportedPhaseSkills(ctx, tx, &record); err != nil {
			return false, err
		}
	}

	if err := upsertMissionRow(ctx, tx, record); err != nil {
		return false, err
	}
	if len(record.Phases) > 0 {
		if err := replaceMissionPhaseData(ctx, tx, record); err != nil {
			return false, err
		}
	}

	if err := tx.Commit(); err != nil {
		return false, fmt.Errorf("committing imported mission %s: %w", m.WorkspaceID, err)
	}
	return !exists, nil
}

func (d *DB) insertImportedMissionIfMissing(ctx context.Context, m MissionRecord) (bool, error) {
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return false, fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	created, err := insertMissionRowIfMissing(ctx, tx, m)
	if err != nil {
		return false, err
	}
	if !created {
		if err := tx.Commit(); err != nil {
			return false, fmt.Errorf("committing skipped import %s: %w", m.WorkspaceID, err)
		}
		return false, nil
	}

	recordedAt := time.Now().UTC().Format(time.RFC3339)
	for _, p := range m.Phases {
		if err := insertPhase(ctx, tx, m.WorkspaceID, p); err != nil {
			return false, err
		}
		for _, skill := range p.Skills {
			if err := insertSkillInvocation(ctx, tx, m.WorkspaceID, p.Name, p.Persona, skill, SkillSourceDeclared, recordedAt); err != nil {
				return false, err
			}
		}
		for _, skill := range p.ParsedSkills {
			if err := insertSkillInvocation(ctx, tx, m.WorkspaceID, p.Name, p.Persona, skill, SkillSourceOutputParse, recordedAt); err != nil {
				return false, err
			}
		}
	}

	if err := tx.Commit(); err != nil {
		return false, fmt.Errorf("committing imported mission %s: %w", m.WorkspaceID, err)
	}
	return true, nil
}

// MissionSummary is returned by QueryMissions for display in the metrics table.
type MissionSummary struct {
	WorkspaceID     string
	Domain          string
	Status          string
	DecompSource    string // how the plan was decomposed
	Task            string
	TopPersona      string // first persona assigned in the mission (may be empty)
	DurationSec     int
	PhasesTotal     int
	PhasesCompleted int
	PhasesFailed    int
	StartedAt       time.Time
}

// PersonaMetric is returned by QueryPersonaMetrics.
type PersonaMetric struct {
	Persona        string
	PhaseCount     int
	AvgDurationSec float64
	FailureRate    float64 // 0-100
	AvgRetries     float64
	LLMPct         float64 // percent selected by LLM
	KeywordPct     float64 // percent selected by keyword fallback
}

// RoutingMethodDist is one row returned by QueryRoutingMethodDistribution.
type RoutingMethodDist struct {
	Method string
	Count  int
	Pct    float64 // 0–100, rounded to one decimal
}

// FallbackAlertThreshold is the fallback-routing percentage above which an
// alert should be surfaced to the operator.
const FallbackAlertThreshold = 30.0

// FallbackRate returns the fallback percentage (0–100) from a distribution
// slice, or 0.0 if "fallback" is not present.
func FallbackRate(dist []RoutingMethodDist) float64 {
	for _, d := range dist {
		if d.Method == "fallback" {
			return d.Pct
		}
	}
	return 0.0
}

// QueryRoutingMethodDistribution returns counts and percentages for each
// persona-selection method stored in the phases table.  "required_review"
// phases are excluded because they represent auto-injected gates, not organic
// routing decisions.  Rows are ordered by count descending.
func (d *DB) QueryRoutingMethodDistribution(ctx context.Context) ([]RoutingMethodDist, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT
		    COALESCE(NULLIF(selection_method, ''), 'unknown') AS method,
		    COUNT(*) AS cnt,
		    COALESCE(
		        COUNT(*) * 100.0 / NULLIF((
		            SELECT COUNT(*) FROM phases
		            WHERE persona != ''
		              AND COALESCE(selection_method, '') != 'required_review'
		        ), 0),
		    0.0) AS pct
		FROM phases
		WHERE persona != ''
		  AND COALESCE(selection_method, '') != 'required_review'
		GROUP BY method
		ORDER BY cnt DESC
	`)
	if err != nil {
		return nil, fmt.Errorf("querying routing method distribution: %w", err)
	}
	defer rows.Close()

	var out []RoutingMethodDist
	for rows.Next() {
		var r RoutingMethodDist
		if err := rows.Scan(&r.Method, &r.Count, &r.Pct); err != nil {
			return nil, fmt.Errorf("scanning routing method row: %w", err)
		}
		out = append(out, r)
	}
	return out, rows.Err()
}

// SkillUsage is returned by QuerySkillUsage.
type SkillUsage struct {
	SkillName   string
	Phase       string
	Persona     string
	Source      string // "declared" or "output_parse"
	Invocations int
}

// DayTrend is returned by QueryTrends.
type DayTrend struct {
	Day         string // "2026-03-05"
	Total       int
	Successes   int
	AvgDuration float64
}

// QueryMissions returns a list of missions ordered newest first, applying optional filters.
// domain="" skips domain filtering; days=0 skips date filtering; decompSource="" skips decomp_source filtering.
func (d *DB) QueryMissions(ctx context.Context, limit int, domain string, days int, status string, decompSource string) ([]MissionSummary, error) {
	if limit <= 0 {
		limit = 20
	}
	rows, err := d.db.QueryContext(ctx, `
		SELECT m.id, m.domain, m.status, m.decomp_source, m.task, m.duration_s,
		       m.phases_total, m.phases_completed, m.phases_failed, m.started_at,
		       COALESCE((
		           SELECT p.persona FROM phases p
		           WHERE p.mission_id = m.id AND p.persona != ''
		           ORDER BY rowid LIMIT 1
		       ), '') AS top_persona
		FROM missions m
		WHERE (? = '' OR m.domain = ?)
		  AND (? = 0 OR m.started_at >= datetime('now', '-' || ? || ' days'))
		  AND (? = '' OR m.status = ?)
		  AND (? = '' OR m.decomp_source = ?)
		ORDER BY m.started_at DESC
		LIMIT ?
	`, domain, domain, days, days, status, status, decompSource, decompSource, limit)
	if err != nil {
		return nil, fmt.Errorf("querying missions: %w", err)
	}
	defer rows.Close()

	var out []MissionSummary
	for rows.Next() {
		var s MissionSummary
		var startedAt string
		if err := rows.Scan(
			&s.WorkspaceID, &s.Domain, &s.Status, &s.DecompSource, &s.Task, &s.DurationSec,
			&s.PhasesTotal, &s.PhasesCompleted, &s.PhasesFailed, &startedAt,
			&s.TopPersona,
		); err != nil {
			return nil, fmt.Errorf("scanning mission row: %w", err)
		}
		var parseErr error
		if s.StartedAt, parseErr = time.Parse(time.RFC3339, startedAt); parseErr != nil {
			return nil, fmt.Errorf("parsing started_at %q: %w", startedAt, parseErr)
		}
		out = append(out, s)
	}
	return out, rows.Err()
}

// QueryPersonaMetrics returns per-persona aggregates from the phases table.
func (d *DB) QueryPersonaMetrics(ctx context.Context) ([]PersonaMetric, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT
		    persona,
		    COUNT(*) AS phase_count,
		    AVG(CAST(duration_s AS REAL)) AS avg_duration,
		    SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS failure_rate,
		    AVG(CAST(retries AS REAL)) AS avg_retries,
		    SUM(CASE WHEN selection_method = 'llm' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS llm_pct,
		    SUM(CASE WHEN selection_method = 'keyword' THEN 1 ELSE 0 END) * 100.0 / COUNT(*) AS keyword_pct
		FROM phases
		WHERE persona != ''
		GROUP BY persona
		ORDER BY phase_count DESC
	`)
	if err != nil {
		return nil, fmt.Errorf("querying persona metrics: %w", err)
	}
	defer rows.Close()

	var out []PersonaMetric
	for rows.Next() {
		var p PersonaMetric
		if err := rows.Scan(
			&p.Persona, &p.PhaseCount, &p.AvgDurationSec,
			&p.FailureRate, &p.AvgRetries, &p.LLMPct, &p.KeywordPct,
		); err != nil {
			return nil, fmt.Errorf("scanning persona row: %w", err)
		}
		out = append(out, p)
	}
	return out, rows.Err()
}

// QuerySkillUsage returns skill invocation counts grouped by skill, phase, persona, and source.
func (d *DB) QuerySkillUsage(ctx context.Context) ([]SkillUsage, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT skill_name, phase, persona, source, COUNT(*) AS invocations
		FROM skill_invocations
		GROUP BY skill_name, phase, persona, source
		ORDER BY invocations DESC, skill_name, phase
		LIMIT 100
	`)
	if err != nil {
		return nil, fmt.Errorf("querying skill usage: %w", err)
	}
	defer rows.Close()

	var out []SkillUsage
	for rows.Next() {
		var s SkillUsage
		if err := rows.Scan(&s.SkillName, &s.Phase, &s.Persona, &s.Source, &s.Invocations); err != nil {
			return nil, fmt.Errorf("scanning skill row: %w", err)
		}
		out = append(out, s)
	}
	return out, rows.Err()
}

// QueryTrends returns daily mission aggregates for the last N days.
func (d *DB) QueryTrends(ctx context.Context, days int) ([]DayTrend, error) {
	if days <= 0 {
		days = 30
	}
	rows, err := d.db.QueryContext(ctx, `
		SELECT
		    date(started_at) AS day,
		    COUNT(*) AS total,
		    SUM(CASE WHEN status = 'success' THEN 1 ELSE 0 END) AS successes,
		    AVG(CAST(duration_s AS REAL)) AS avg_duration
		FROM missions
		WHERE started_at >= datetime('now', '-' || ? || ' days')
		GROUP BY day
		ORDER BY day DESC
	`, days)
	if err != nil {
		return nil, fmt.Errorf("querying trends for %d days: %w", days, err)
	}
	defer rows.Close()

	var out []DayTrend
	for rows.Next() {
		var t DayTrend
		if err := rows.Scan(&t.Day, &t.Total, &t.Successes, &t.AvgDuration); err != nil {
			return nil, fmt.Errorf("scanning trend row: %w", err)
		}
		out = append(out, t)
	}
	return out, rows.Err()
}

// PhaseRow is returned by QueryPhases for per-phase display.
type PhaseRow struct {
	Name         string
	Persona      string
	Status       string
	DurationS    int
	ParsedSkills []string
}

// QueryPhases returns all phases for a given mission, ordered by rowid.
func (d *DB) QueryPhases(ctx context.Context, missionID string) ([]PhaseRow, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT name, persona, status, duration_s, COALESCE(parsed_skills, '')
		FROM phases
		WHERE mission_id = ?
		ORDER BY rowid
	`, missionID)
	if err != nil {
		return nil, fmt.Errorf("querying phases for mission %s: %w", missionID, err)
	}
	defer rows.Close()

	var out []PhaseRow
	for rows.Next() {
		var r PhaseRow
		var parsedSkillsJSON string
		if err := rows.Scan(&r.Name, &r.Persona, &r.Status, &r.DurationS, &parsedSkillsJSON); err != nil {
			return nil, fmt.Errorf("scanning phase row: %w", err)
		}
		r.ParsedSkills = unmarshalSkillsJSON(parsedSkillsJSON)
		out = append(out, r)
	}
	return out, rows.Err()
}

// CostSummary is returned by QueryCostSummary.
type CostSummary struct {
	TotalMissions  int
	TokensInTotal  int
	TokensOutTotal int
	CostUSDTotal   float64
	ByDomain       []DomainCost
	ByModel        []ModelCost
}

// DomainCost holds per-domain cost aggregates.
type DomainCost struct {
	Domain         string
	Missions       int
	TokensInTotal  int
	TokensOutTotal int
	CostUSDTotal   float64
}

// ModelCost holds per-model cost aggregates across all phases.
type ModelCost struct {
	Model          string
	Provider       string
	Phases         int
	TokensInTotal  int
	TokensOutTotal int
	CostUSDTotal   float64
}

// QueryCostSummary returns mission-level and phase-level cost aggregates,
// optionally filtered to the last N days (days=0 means no date filter).
func (d *DB) QueryCostSummary(ctx context.Context, days int) (*CostSummary, error) {
	// Mission-level totals and per-domain breakdown.
	missionRows, err := d.db.QueryContext(ctx, `
		SELECT domain,
		       COUNT(*) AS missions,
		       COALESCE(SUM(tokens_in_total), 0)  AS tokens_in,
		       COALESCE(SUM(tokens_out_total), 0) AS tokens_out,
		       COALESCE(SUM(cost_usd_total), 0)   AS cost_usd
		FROM missions
		WHERE (? = 0 OR started_at >= datetime('now', '-' || ? || ' days'))
		GROUP BY domain
		ORDER BY cost_usd DESC
	`, days, days)
	if err != nil {
		return nil, fmt.Errorf("querying mission costs: %w", err)
	}
	defer missionRows.Close()

	s := &CostSummary{}
	for missionRows.Next() {
		var dc DomainCost
		if err := missionRows.Scan(&dc.Domain, &dc.Missions, &dc.TokensInTotal, &dc.TokensOutTotal, &dc.CostUSDTotal); err != nil {
			return nil, fmt.Errorf("scanning domain cost row: %w", err)
		}
		s.TotalMissions += dc.Missions
		s.TokensInTotal += dc.TokensInTotal
		s.TokensOutTotal += dc.TokensOutTotal
		s.CostUSDTotal += dc.CostUSDTotal
		s.ByDomain = append(s.ByDomain, dc)
	}
	if err := missionRows.Err(); err != nil {
		return nil, fmt.Errorf("iterating mission cost rows: %w", err)
	}

	// Phase-level per-model breakdown.
	phaseRows, err := d.db.QueryContext(ctx, `
		SELECT p.model,
		       p.provider,
		       COUNT(*) AS phases,
		       COALESCE(SUM(p.tokens_in), 0)  AS tokens_in,
		       COALESCE(SUM(p.tokens_out), 0) AS tokens_out,
		       COALESCE(SUM(p.cost_usd), 0)   AS cost_usd
		FROM phases p
		JOIN missions m ON m.id = p.mission_id
		WHERE p.model != ''
		  AND (? = 0 OR m.started_at >= datetime('now', '-' || ? || ' days'))
		GROUP BY p.model, p.provider
		ORDER BY cost_usd DESC
	`, days, days)
	if err != nil {
		return nil, fmt.Errorf("querying phase model costs: %w", err)
	}
	defer phaseRows.Close()

	for phaseRows.Next() {
		var mc ModelCost
		if err := phaseRows.Scan(&mc.Model, &mc.Provider, &mc.Phases, &mc.TokensInTotal, &mc.TokensOutTotal, &mc.CostUSDTotal); err != nil {
			return nil, fmt.Errorf("scanning model cost row: %w", err)
		}
		s.ByModel = append(s.ByModel, mc)
	}
	return s, phaseRows.Err()
}
