package metrics

import (
	"context"
	"database/sql"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

func tempDB(t *testing.T) *DB {
	t.Helper()
	dir := t.TempDir()
	db, err := InitDB(filepath.Join(dir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return db
}

func TestInitDB(t *testing.T) {
	db := tempDB(t)
	if db == nil {
		t.Fatal("expected non-nil DB")
	}

	// Verify tables exist by querying them
	tables := []string{"missions", "phases", "skill_invocations"}
	for _, table := range tables {
		var name string
		err := db.db.QueryRow("SELECT name FROM sqlite_master WHERE type='table' AND name=?", table).Scan(&name)
		if err != nil {
			t.Errorf("table %q not found: %v", table, err)
		}
	}
}

func TestRecordMission(t *testing.T) {
	now := time.Now().Truncate(time.Second).UTC()
	mission := MissionRecord{
		WorkspaceID:     "ws-abc123",
		Domain:          "dev",
		Task:            "implement feature X",
		StartedAt:       now,
		FinishedAt:      now.Add(5 * time.Minute),
		DurationSec:     300,
		PhasesTotal:     2,
		PhasesCompleted: 2,
		PhasesFailed:    0,
		PhasesSkipped:   0,
		Status:          "success",
		Phases: []PhaseRecord{
			{Name: "research", Persona: "architect", DurationS: 120, Status: "completed", GatePassed: true, OutputLen: 500},
			{Name: "implement", Persona: "backend-engineer", DurationS: 180, Status: "completed", GatePassed: true, OutputLen: 1200},
		},
	}

	tests := []struct {
		name    string
		mission MissionRecord
		wantErr bool
	}{
		{
			name:    "valid mission with phases",
			mission: mission,
		},
		{
			name: "missing workspace_id",
			mission: MissionRecord{
				Domain: "dev",
				Status: "success",
			},
			wantErr: true,
		},
		{
			name:    "duplicate workspace_id returns ErrDuplicate",
			mission: mission, // same workspace_id as first insert
			wantErr: true,
		},
	}

	db := tempDB(t)
	ctx := context.Background()

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := db.RecordMission(ctx, tt.mission)
			if (err != nil) != tt.wantErr {
				t.Errorf("RecordMission() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}

	// Verify the mission was stored once (duplicate was ignored)
	var count int
	db.db.QueryRow("SELECT COUNT(*) FROM missions WHERE id = ?", mission.WorkspaceID).Scan(&count)
	if count != 1 {
		t.Errorf("expected 1 mission, got %d", count)
	}

	// Verify phases were stored
	db.db.QueryRow("SELECT COUNT(*) FROM phases WHERE mission_id = ?", mission.WorkspaceID).Scan(&count)
	if count != 2 {
		t.Errorf("expected 2 phases, got %d", count)
	}
}

func TestPhaseRecordMarshalUsesLegacySelectionMethodKey(t *testing.T) {
	b, err := json.Marshal(PhaseRecord{
		Name:            "implement",
		Persona:         "backend-engineer",
		SelectionMethod: "keyword",
	})
	if err != nil {
		t.Fatalf("json.Marshal: %v", err)
	}

	got := string(b)
	if !strings.Contains(got, `"selection_method":"keyword"`) {
		t.Fatalf("marshaled PhaseRecord = %s, want legacy selection_method key", got)
	}
	if strings.Contains(got, `"persona_selection_method"`) {
		t.Fatalf("marshaled PhaseRecord = %s, did not expect persona_selection_method key", got)
	}
}

func TestRecordSkillInvocation(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()

	// Insert a mission first (FK constraint)
	now := time.Now().UTC()
	if err := db.RecordMission(ctx, MissionRecord{
		WorkspaceID: "ws-skill-test",
		Domain:      "dev",
		StartedAt:   now,
		FinishedAt:  now.Add(time.Minute),
		Status:      "success",
	}); err != nil {
		t.Fatalf("RecordMission: %v", err)
	}

	tests := []struct {
		name      string
		missionID string
		phase     string
		persona   string
		skillName string
		source    string
		wantErr   bool
	}{
		{
			name:      "valid declared invocation",
			missionID: "ws-skill-test",
			phase:     "implement",
			persona:   "backend-engineer",
			skillName: "obsidian",
			source:    SkillSourceDeclared,
		},
		{
			name:      "valid output_parse invocation",
			missionID: "ws-skill-test",
			phase:     "implement",
			persona:   "backend-engineer",
			skillName: "scout",
			source:    SkillSourceOutputParse,
		},
		{
			name:      "empty missionID",
			missionID: "",
			phase:     "implement",
			skillName: "obsidian",
			source:    SkillSourceDeclared,
			wantErr:   true,
		},
		{
			name:      "empty skillName",
			missionID: "ws-skill-test",
			phase:     "implement",
			skillName: "",
			source:    SkillSourceDeclared,
			wantErr:   true,
		},
		{
			name:      "empty phase allowed for best-effort metrics",
			missionID: "ws-skill-test",
			phase:     "",
			skillName: "obsidian",
			source:    SkillSourceDeclared,
		},
		{
			name:      "invalid source",
			missionID: "ws-skill-test",
			phase:     "implement",
			skillName: "obsidian",
			source:    "manual",
			wantErr:   true,
		},
		{
			name:      "multiple invocations same skill",
			missionID: "ws-skill-test",
			phase:     "implement",
			persona:   "backend-engineer",
			skillName: "obsidian",
			source:    SkillSourceDeclared,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			err := db.RecordSkillInvocation(ctx, tt.missionID, tt.phase, tt.persona, tt.skillName, tt.source)
			if (err != nil) != tt.wantErr {
				t.Errorf("RecordSkillInvocation() error = %v, wantErr %v", err, tt.wantErr)
			}
		})
	}

	// Verify two obsidian invocations were stored
	var count int
	db.db.QueryRow("SELECT COUNT(*) FROM skill_invocations WHERE mission_id = ? AND skill_name = ?",
		"ws-skill-test", "obsidian").Scan(&count)
	if count != 3 {
		t.Errorf("expected 3 skill invocations, got %d", count)
	}

	// Verify output_parse invocation was stored with correct source
	var source string
	db.db.QueryRow("SELECT source FROM skill_invocations WHERE mission_id = ? AND skill_name = ?",
		"ws-skill-test", "scout").Scan(&source)
	if source != SkillSourceOutputParse {
		t.Errorf("expected source %q for scout, got %q", SkillSourceOutputParse, source)
	}
}

func TestRecordMission_PersistsPhaseSkills(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().UTC()

	err := db.RecordMission(ctx, MissionRecord{
		WorkspaceID: "ws-record-skills",
		Domain:      "dev",
		StartedAt:   now,
		FinishedAt:  now.Add(time.Minute),
		Status:      "success",
		Phases: []PhaseRecord{
			{
				ID:           "phase-1",
				Name:         "implement",
				Persona:      "backend-engineer",
				Skills:       []string{"watermark"},
				ParsedSkills: []string{"scout"},
			},
		},
	})
	if err != nil {
		t.Fatalf("RecordMission: %v", err)
	}

	usage, err := db.QuerySkillUsage(ctx)
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 skill rows, got %d: %+v", len(usage), usage)
	}

	bySkill := make(map[string]SkillUsage, len(usage))
	for _, row := range usage {
		bySkill[row.SkillName] = row
	}
	if bySkill["watermark"].Source != SkillSourceDeclared {
		t.Errorf("watermark source = %q, want %q", bySkill["watermark"].Source, SkillSourceDeclared)
	}
	if bySkill["scout"].Source != SkillSourceOutputParse {
		t.Errorf("scout source = %q, want %q", bySkill["scout"].Source, SkillSourceOutputParse)
	}
}

// TestPersonaStats verifies that the aggregate query functions return correct
// values when seeded with known data.
func TestPersonaStats(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	// Seed 3 missions spanning 2 domains.
	// Mission 1: dev, success, 2 phases (backend-engineer/llm, backend-engineer/keyword)
	// Mission 2: dev, failure, 1 phase (backend-engineer/llm — failed)
	// Mission 3: creative, success, 2 phases (architect/llm, architect/llm)
	missions := []MissionRecord{
		{
			WorkspaceID: "ws-stats-1", Domain: "dev", Status: "success",
			StartedAt: now.Add(-2 * time.Hour), FinishedAt: now.Add(-1 * time.Hour),
			DurationSec: 3600, PhasesTotal: 2, PhasesCompleted: 2,
			Phases: []PhaseRecord{
				{Name: "phase-1", Persona: "backend-engineer", SelectionMethod: "llm",
					DurationS: 60, Status: "completed", GatePassed: true, OutputLen: 500},
				{Name: "phase-2", Persona: "backend-engineer", SelectionMethod: "keyword",
					DurationS: 90, Status: "completed", GatePassed: true, OutputLen: 800},
			},
		},
		{
			WorkspaceID: "ws-stats-2", Domain: "dev", Status: "failure",
			StartedAt: now.Add(-30 * time.Minute), FinishedAt: now.Add(-20 * time.Minute),
			DurationSec: 600, PhasesTotal: 1, PhasesFailed: 1,
			Phases: []PhaseRecord{
				{Name: "phase-1", Persona: "backend-engineer", SelectionMethod: "llm",
					DurationS: 120, Status: "failed", GatePassed: false, OutputLen: 0, Retries: 2},
			},
		},
		{
			WorkspaceID: "ws-stats-3", Domain: "creative", Status: "success",
			StartedAt: now.Add(-10 * time.Minute), FinishedAt: now.Add(-5 * time.Minute),
			DurationSec: 300, PhasesTotal: 2, PhasesCompleted: 2,
			Phases: []PhaseRecord{
				{Name: "phase-1", Persona: "architect", SelectionMethod: "llm",
					DurationS: 45, Status: "completed", GatePassed: true, OutputLen: 300},
				{Name: "phase-2", Persona: "architect", SelectionMethod: "llm",
					DurationS: 55, Status: "completed", GatePassed: true, OutputLen: 400},
			},
		},
	}
	for _, m := range missions {
		if err := db.RecordMission(ctx, m); err != nil {
			t.Fatalf("RecordMission(%s): %v", m.WorkspaceID, err)
		}
	}

	// Skill invocations:
	// (obsidian, phase-1, backend-engineer) ×2, (scout, phase-1, backend-engineer) ×1,
	// (obsidian, phase-1, architect) ×1.
	skillRows := []struct{ mission, phase, persona, skill string }{
		{"ws-stats-1", "phase-1", "backend-engineer", "obsidian"},
		{"ws-stats-1", "phase-1", "backend-engineer", "obsidian"},
		{"ws-stats-2", "phase-1", "backend-engineer", "scout"},
		{"ws-stats-3", "phase-1", "architect", "obsidian"},
	}
	for _, s := range skillRows {
		if err := db.RecordSkillInvocation(ctx, s.mission, s.phase, s.persona, s.skill, SkillSourceDeclared); err != nil {
			t.Fatalf("RecordSkillInvocation: %v", err)
		}
	}

	t.Run("QueryPersonaMetrics aggregate counts", func(t *testing.T) {
		metrics, err := db.QueryPersonaMetrics(ctx)
		if err != nil {
			t.Fatalf("QueryPersonaMetrics: %v", err)
		}
		// Expect exactly 2 personas: backend-engineer (3 phases) and architect (2 phases).
		if len(metrics) != 2 {
			t.Fatalf("want 2 personas, got %d", len(metrics))
		}

		// Results are ordered by phase_count DESC, so backend-engineer is first.
		be := metrics[0]
		if be.Persona != "backend-engineer" {
			t.Errorf("want persona backend-engineer first, got %q", be.Persona)
		}
		if be.PhaseCount != 3 {
			t.Errorf("backend-engineer: want 3 phases, got %d", be.PhaseCount)
		}
		// 1 out of 3 phases failed → failure rate = 33.3…%
		wantFailRate := 1.0 / 3.0 * 100
		if diff := be.FailureRate - wantFailRate; diff > 0.1 || diff < -0.1 {
			t.Errorf("backend-engineer: want failure_rate ≈ %.2f, got %.2f", wantFailRate, be.FailureRate)
		}
		// 2 llm, 1 keyword out of 3 → llm_pct ≈ 66.7, keyword_pct ≈ 33.3
		wantLLM := 2.0 / 3.0 * 100
		if diff := be.LLMPct - wantLLM; diff > 0.1 || diff < -0.1 {
			t.Errorf("backend-engineer: want llm_pct ≈ %.2f, got %.2f", wantLLM, be.LLMPct)
		}
		// avg retries = (0 + 0 + 2) / 3 ≈ 0.667
		wantRetries := 2.0 / 3.0
		if diff := be.AvgRetries - wantRetries; diff > 0.01 || diff < -0.01 {
			t.Errorf("backend-engineer: want avg_retries ≈ %.3f, got %.3f", wantRetries, be.AvgRetries)
		}

		arch := metrics[1]
		if arch.Persona != "architect" {
			t.Errorf("want persona architect second, got %q", arch.Persona)
		}
		if arch.PhaseCount != 2 {
			t.Errorf("architect: want 2 phases, got %d", arch.PhaseCount)
		}
		if arch.FailureRate != 0 {
			t.Errorf("architect: want 0 failure rate, got %.2f", arch.FailureRate)
		}
		if arch.LLMPct != 100 {
			t.Errorf("architect: want 100%% llm_pct, got %.2f", arch.LLMPct)
		}
	})

	t.Run("QuerySkillUsage counts and ordering", func(t *testing.T) {
		usage, err := db.QuerySkillUsage(ctx)
		if err != nil {
			t.Fatalf("QuerySkillUsage: %v", err)
		}
		// 3 distinct (skill, phase, persona) groups:
		// (obsidian, phase-1, backend-engineer) ×2, (obsidian, phase-1, architect) ×1,
		// (scout, phase-1, backend-engineer) ×1.
		if len(usage) != 3 {
			t.Fatalf("want 3 skill rows, got %d", len(usage))
		}
		// Top result must be obsidian/phase-1/backend-engineer with 2 invocations.
		top := usage[0]
		if top.SkillName != "obsidian" || top.Phase != "phase-1" || top.Persona != "backend-engineer" {
			t.Errorf("want top skill obsidian/phase-1/backend-engineer, got %s/%s/%s", top.SkillName, top.Phase, top.Persona)
		}
		if top.Invocations != 2 {
			t.Errorf("want 2 invocations, got %d", top.Invocations)
		}
	})

	t.Run("QueryTrends returns today's data", func(t *testing.T) {
		trends, err := db.QueryTrends(ctx, 7)
		if err != nil {
			t.Fatalf("QueryTrends: %v", err)
		}
		// All 3 missions started today (within the last 2 hours), so we should
		// get at least one day with total=3.
		var total int
		for _, tr := range trends {
			total += tr.Total
		}
		if total != 3 {
			t.Errorf("QueryTrends: want total=3 across all days, got %d", total)
		}

		// 2 of 3 missions have status="success"
		var successes int
		for _, tr := range trends {
			successes += tr.Successes
		}
		if successes != 2 {
			t.Errorf("QueryTrends: want 2 successes, got %d", successes)
		}
	})

	t.Run("QueryMissions filters by domain", func(t *testing.T) {
		rows, err := db.QueryMissions(ctx, 20, "dev", 0, "", "", "")
		if err != nil {
			t.Fatalf("QueryMissions(dev): %v", err)
		}
		if len(rows) != 2 {
			t.Errorf("want 2 dev missions, got %d", len(rows))
		}
		for _, r := range rows {
			if r.Domain != "dev" {
				t.Errorf("want domain=dev, got %q", r.Domain)
			}
		}
	})

	t.Run("QueryMissions filters by status", func(t *testing.T) {
		rows, err := db.QueryMissions(ctx, 20, "", 0, "failure", "", "")
		if err != nil {
			t.Fatalf("QueryMissions(failure): %v", err)
		}
		if len(rows) != 1 {
			t.Errorf("want 1 failure mission, got %d", len(rows))
		}
		if rows[0].WorkspaceID != "ws-stats-2" {
			t.Errorf("want ws-stats-2, got %q", rows[0].WorkspaceID)
		}
	})

	t.Run("QueryMissions top_persona is set", func(t *testing.T) {
		rows, err := db.QueryMissions(ctx, 20, "", 0, "success", "", "")
		if err != nil {
			t.Fatalf("QueryMissions(success): %v", err)
		}
		// ws-stats-1 and ws-stats-3 are successes; both have non-empty top persona.
		for _, r := range rows {
			if r.TopPersona == "" {
				t.Errorf("mission %s: expected non-empty TopPersona", r.WorkspaceID)
			}
		}
	})
}

// TestPhaseNameCollision verifies that two phases with the same name but different
// IDs are both persisted. Before the fix, INSERT OR IGNORE on missionID+"_"+name
// silently dropped the second phase.
func TestPhaseNameCollision(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	mission := MissionRecord{
		WorkspaceID: "ws-collision",
		Domain:      "dev",
		StartedAt:   now,
		FinishedAt:  now.Add(5 * time.Minute),
		Status:      "success",
		PhasesTotal: 2,
		Phases: []PhaseRecord{
			{ID: "phase-1", Name: "implement", Persona: "backend-engineer", Status: "completed"},
			{ID: "phase-2", Name: "implement", Persona: "backend-engineer", Status: "completed"},
		},
	}

	if err := db.RecordMission(ctx, mission); err != nil {
		t.Fatalf("RecordMission: %v", err)
	}

	var count int
	db.db.QueryRow("SELECT COUNT(*) FROM phases WHERE mission_id = ?", mission.WorkspaceID).Scan(&count)
	if count != 2 {
		t.Errorf("phase name collision: want 2 phases stored, got %d (second was silently dropped)", count)
	}
}

// TestPhaseIDFallbackToName verifies that old JSONL records without an ID field
// (p.ID == "") still insert correctly by falling back to keying on p.Name.
func TestPhaseIDFallbackToName(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	mission := MissionRecord{
		WorkspaceID: "ws-fallback",
		Domain:      "dev",
		StartedAt:   now,
		FinishedAt:  now.Add(5 * time.Minute),
		Status:      "success",
		PhasesTotal: 2,
		Phases: []PhaseRecord{
			{Name: "research", Persona: "architect", Status: "completed"},         // no ID
			{Name: "implement", Persona: "backend-engineer", Status: "completed"}, // no ID
		},
	}

	if err := db.RecordMission(ctx, mission); err != nil {
		t.Fatalf("RecordMission: %v", err)
	}

	var count int
	db.db.QueryRow("SELECT COUNT(*) FROM phases WHERE mission_id = ?", mission.WorkspaceID).Scan(&count)
	if count != 2 {
		t.Errorf("fallback to name: want 2 phases stored, got %d", count)
	}
}

func TestUpsertMission_RefreshesSkillsForExistingMission(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	mission := MissionRecord{
		WorkspaceID: "ws-upsert",
		Domain:      "dev",
		Task:        "repair partial metrics state",
		StartedAt:   now,
		FinishedAt:  now.Add(time.Minute),
		DurationSec: 60,
		PhasesTotal: 1,
		Status:      "success",
		Phases: []PhaseRecord{
			{
				ID:           "phase-1",
				Name:         "implement",
				Persona:      "backend-engineer",
				Status:       "completed",
				Skills:       []string{"obsidian"},
				ParsedSkills: []string{"scout"},
			},
		},
	}

	created, err := db.UpsertMission(ctx, mission)
	if err != nil {
		t.Fatalf("UpsertMission(initial): %v", err)
	}
	if !created {
		t.Fatal("initial UpsertMission should report created=true")
	}

	if _, err := db.db.Exec(`DELETE FROM skill_invocations WHERE mission_id = ?`, mission.WorkspaceID); err != nil {
		t.Fatalf("DELETE skill_invocations: %v", err)
	}

	mission.Phases[0].Skills = []string{"watermark"}
	mission.Phases[0].ParsedSkills = []string{"scheduler"}

	created, err = db.UpsertMission(ctx, mission)
	if err != nil {
		t.Fatalf("UpsertMission(refresh): %v", err)
	}
	if created {
		t.Fatal("refresh UpsertMission should report created=false")
	}

	usage, err := db.QuerySkillUsage(ctx)
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 skill rows, got %d: %+v", len(usage), usage)
	}

	bySkill := make(map[string]SkillUsage, len(usage))
	for _, row := range usage {
		bySkill[row.SkillName] = row
	}
	if _, ok := bySkill["obsidian"]; ok {
		t.Fatal("stale skill row for obsidian remained after refresh")
	}
	if row, ok := bySkill["watermark"]; !ok {
		t.Fatal("missing refreshed declared skill row")
	} else if row.Source != SkillSourceDeclared {
		t.Errorf("watermark source = %q, want %q", row.Source, SkillSourceDeclared)
	}
	if row, ok := bySkill["scheduler"]; !ok {
		t.Fatal("missing refreshed parsed skill row")
	} else if row.Source != SkillSourceOutputParse {
		t.Errorf("scheduler source = %q, want %q", row.Source, SkillSourceOutputParse)
	}
}

func TestUpsertMission_EmptyPhasesPreservesExistingRowsAndRefreshesMissionFields(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	initial := MissionRecord{
		WorkspaceID:     "ws-upsert-preserve",
		Domain:          "dev",
		Task:            "initial task",
		StartedAt:       now,
		FinishedAt:      now.Add(time.Minute),
		DurationSec:     60,
		PhasesTotal:     1,
		PhasesCompleted: 1,
		Status:          "success",
		DecompSource:    decompSourceUnknown,
		Phases: []PhaseRecord{
			{
				ID:           "phase-1",
				Name:         "implement",
				Persona:      "backend-engineer",
				Status:       "completed",
				Skills:       []string{"watermark"},
				ParsedSkills: []string{"scout"},
			},
		},
	}

	if _, err := db.UpsertMission(ctx, initial); err != nil {
		t.Fatalf("UpsertMission(initial): %v", err)
	}

	refresh := MissionRecord{
		WorkspaceID:     initial.WorkspaceID,
		Domain:          initial.Domain,
		Task:            "refreshed task",
		StartedAt:       initial.StartedAt,
		FinishedAt:      initial.FinishedAt,
		DurationSec:     initial.DurationSec,
		PhasesTotal:     initial.PhasesTotal,
		PhasesCompleted: initial.PhasesCompleted,
		Status:          initial.Status,
		DecompSource:    "decomp.llm",
	}

	created, err := db.UpsertMission(ctx, refresh)
	if err != nil {
		t.Fatalf("UpsertMission(refresh): %v", err)
	}
	if created {
		t.Fatal("refresh UpsertMission should report created=false")
	}

	var task, decompSource string
	if err := db.db.QueryRow(`SELECT task, decomp_source FROM missions WHERE id = ?`, initial.WorkspaceID).Scan(&task, &decompSource); err != nil {
		t.Fatalf("query refreshed mission: %v", err)
	}
	if task != "refreshed task" {
		t.Fatalf("task = %q, want %q", task, "refreshed task")
	}
	if decompSource != "decomp.llm" {
		t.Fatalf("decomp_source = %q, want %q", decompSource, "decomp.llm")
	}

	var phaseCount, skillCount int
	if err := db.db.QueryRow(`SELECT COUNT(*) FROM phases WHERE mission_id = ?`, initial.WorkspaceID).Scan(&phaseCount); err != nil {
		t.Fatalf("count phases: %v", err)
	}
	if err := db.db.QueryRow(`SELECT COUNT(*) FROM skill_invocations WHERE mission_id = ?`, initial.WorkspaceID).Scan(&skillCount); err != nil {
		t.Fatalf("count skills: %v", err)
	}
	if phaseCount != 1 {
		t.Fatalf("phase count = %d, want 1", phaseCount)
	}
	if skillCount != 2 {
		t.Fatalf("skill count = %d, want 2", skillCount)
	}
}

func TestUpsertMissionPhaseSnapshot_ReplacesPhaseSkillsAtomically(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	mission := MissionRecord{
		WorkspaceID:     "ws-phase-snapshot",
		Domain:          "dev",
		Task:            "phase snapshot",
		StartedAt:       now,
		FinishedAt:      now.Add(time.Minute),
		DurationSec:     60,
		PhasesTotal:     1,
		PhasesCompleted: 1,
		Status:          "partial",
		DecompSource:    "decomp.llm",
	}
	phase := PhaseRecord{
		ID:           "phase-1",
		Name:         "implement",
		Persona:      "backend-engineer",
		Status:       "completed",
		Skills:       []string{"golang-pro"},
		ParsedSkills: []string{"scout"},
		Retries:      1,
		GatePassed:   true,
	}

	if err := db.UpsertMissionPhaseSnapshot(ctx, mission, phase); err != nil {
		t.Fatalf("UpsertMissionPhaseSnapshot(initial): %v", err)
	}

	var phaseCount int
	if err := db.db.QueryRow(`SELECT COUNT(*) FROM phases WHERE mission_id = ?`, mission.WorkspaceID).Scan(&phaseCount); err != nil {
		t.Fatalf("count phases: %v", err)
	}
	if phaseCount != 1 {
		t.Fatalf("phase count = %d, want 1", phaseCount)
	}

	usage, err := db.QuerySkillUsage(ctx)
	if err != nil {
		t.Fatalf("QuerySkillUsage(initial): %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 skill rows, got %d: %+v", len(usage), usage)
	}

	phase.Skills = []string{"watermark"}
	phase.ParsedSkills = []string{"scheduler"}
	phase.Retries = 2
	if err := db.UpsertMissionPhaseSnapshot(ctx, mission, phase); err != nil {
		t.Fatalf("UpsertMissionPhaseSnapshot(refresh): %v", err)
	}

	usage, err = db.QuerySkillUsage(ctx)
	if err != nil {
		t.Fatalf("QuerySkillUsage(refresh): %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 refreshed skill rows, got %d: %+v", len(usage), usage)
	}

	got := make(map[string]SkillUsage, len(usage))
	for _, row := range usage {
		got[row.SkillName] = row
	}
	if _, ok := got["golang-pro"]; ok {
		t.Fatal("stale declared skill remained after phase snapshot refresh")
	}
	if _, ok := got["scout"]; ok {
		t.Fatal("stale parsed skill remained after phase snapshot refresh")
	}
	if row, ok := got["watermark"]; !ok {
		t.Fatal("missing refreshed declared skill row")
	} else if row.Source != SkillSourceDeclared {
		t.Fatalf("watermark source = %q, want %q", row.Source, SkillSourceDeclared)
	}
	if row, ok := got["scheduler"]; !ok {
		t.Fatal("missing refreshed parsed skill row")
	} else if row.Source != SkillSourceOutputParse {
		t.Fatalf("scheduler source = %q, want %q", row.Source, SkillSourceOutputParse)
	}

	var retries int
	if err := db.db.QueryRow(`SELECT retries FROM phases WHERE mission_id = ? AND name = ?`, mission.WorkspaceID, phase.Name).Scan(&retries); err != nil {
		t.Fatalf("query phase retries: %v", err)
	}
	if retries != 2 {
		t.Fatalf("phase retries = %d, want 2", retries)
	}
}

func TestImportFromJSONL(t *testing.T) {
	now := time.Now().Truncate(time.Second).UTC()

	records := []MissionRecord{
		{
			WorkspaceID:     "ws-import-1",
			Domain:          "dev",
			Task:            "task one",
			StartedAt:       now,
			FinishedAt:      now.Add(time.Minute),
			DurationSec:     60,
			PhasesTotal:     1,
			PhasesCompleted: 1,
			Status:          "success",
		},
		{
			WorkspaceID:     "ws-import-2",
			Domain:          "creative",
			Task:            "task two",
			StartedAt:       now.Add(time.Hour),
			FinishedAt:      now.Add(2 * time.Hour),
			DurationSec:     3600,
			PhasesTotal:     4,
			PhasesCompleted: 3,
			PhasesFailed:    1,
			Status:          "partial",
		},
	}

	t.Run("import valid JSONL", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")

		f, err := os.Create(path)
		if err != nil {
			t.Fatalf("create: %v", err)
		}
		enc := json.NewEncoder(f)
		for _, r := range records {
			if err := enc.Encode(r); err != nil {
				t.Fatalf("encode: %v", err)
			}
		}
		f.Close()

		db := tempDB(t)
		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		if imported != 2 {
			t.Errorf("expected 2 imported, got %d", imported)
		}

		// Second import is idempotent — duplicates skipped
		imported2, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("second ImportFromJSONL: %v", err)
		}
		if imported2 != 0 {
			t.Errorf("expected 0 on re-import, got %d", imported2)
		}
	})

	t.Run("nonexistent file returns zero with no error", func(t *testing.T) {
		db := tempDB(t)
		imported, err := db.ImportFromJSONL(context.Background(), "/nonexistent/path/metrics.jsonl")
		if err != nil {
			t.Errorf("expected no error for missing file, got %v", err)
		}
		if imported != 0 {
			t.Errorf("expected 0, got %d", imported)
		}
	})

	t.Run("malformed lines are skipped", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")

		content := `{"workspace_id":"ws-good","domain":"dev","started_at":"2026-01-01T00:00:00Z","finished_at":"2026-01-01T00:01:00Z","status":"success"}
not valid json
{"workspace_id":"","domain":"dev","started_at":"2026-01-01T00:00:00Z","finished_at":"2026-01-01T00:01:00Z","status":"success"}
`
		if err := os.WriteFile(path, []byte(content), 0600); err != nil {
			t.Fatalf("write: %v", err)
		}

		db := tempDB(t)
		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		// Only the first line is valid (non-empty workspace_id, valid JSON)
		if imported != 1 {
			t.Errorf("expected 1 imported, got %d", imported)
		}
	})

	t.Run("phase skills survive import and re-import", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")

		record := MissionRecord{
			WorkspaceID: "ws-import-skills",
			Domain:      "dev",
			Task:        "import skills",
			StartedAt:   now,
			FinishedAt:  now.Add(time.Minute),
			DurationSec: 60,
			PhasesTotal: 1,
			Status:      "success",
			Phases: []PhaseRecord{
				{
					ID:           "phase-1",
					Name:         "implement",
					Persona:      "backend-engineer",
					Skills:       []string{"watermark"},
					ParsedSkills: []string{"scout"},
					Status:       "completed",
				},
			},
		}

		f, err := os.Create(path)
		if err != nil {
			t.Fatalf("create: %v", err)
		}
		if err := json.NewEncoder(f).Encode(record); err != nil {
			t.Fatalf("encode: %v", err)
		}
		if err := f.Close(); err != nil {
			t.Fatalf("close: %v", err)
		}

		db := tempDB(t)
		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		if imported != 1 {
			t.Fatalf("expected 1 imported, got %d", imported)
		}

		usage, err := db.QuerySkillUsage(context.Background())
		if err != nil {
			t.Fatalf("QuerySkillUsage: %v", err)
		}
		if len(usage) != 2 {
			t.Fatalf("want 2 skill rows after import, got %d: %+v", len(usage), usage)
		}

		if _, err := db.db.Exec(`DELETE FROM skill_invocations WHERE mission_id = ?`, record.WorkspaceID); err != nil {
			t.Fatalf("DELETE skill_invocations: %v", err)
		}

		imported, err = db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("second ImportFromJSONL: %v", err)
		}
		if imported != 0 {
			t.Fatalf("expected 0 imported on re-import, got %d", imported)
		}

		usage, err = db.QuerySkillUsage(context.Background())
		if err != nil {
			t.Fatalf("QuerySkillUsage after re-import: %v", err)
		}
		if len(usage) != 2 {
			t.Fatalf("want 2 skill rows after re-import, got %d: %+v", len(usage), usage)
		}
	})

	t.Run("duplicate import without skills does not clear existing rows", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")

		db := tempDB(t)
		record := MissionRecord{
			WorkspaceID: "ws-import-existing",
			Domain:      "dev",
			Task:        "existing mission",
			StartedAt:   now,
			FinishedAt:  now.Add(time.Minute),
			DurationSec: 60,
			PhasesTotal: 1,
			Status:      "success",
			Phases: []PhaseRecord{
				{
					ID:           "phase-1",
					Name:         "implement",
					Persona:      "backend-engineer",
					Skills:       []string{"watermark"},
					ParsedSkills: []string{"scout"},
					Status:       "completed",
				},
			},
		}
		if _, err := db.UpsertMission(context.Background(), record); err != nil {
			t.Fatalf("UpsertMission: %v", err)
		}

		legacy := record
		legacy.Phases[0].Skills = nil
		legacy.Phases[0].ParsedSkills = nil

		f, err := os.Create(path)
		if err != nil {
			t.Fatalf("create: %v", err)
		}
		if err := json.NewEncoder(f).Encode(legacy); err != nil {
			t.Fatalf("encode: %v", err)
		}
		if err := f.Close(); err != nil {
			t.Fatalf("close: %v", err)
		}

		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		if imported != 0 {
			t.Fatalf("expected 0 imported on duplicate import, got %d", imported)
		}

		usage, err := db.QuerySkillUsage(context.Background())
		if err != nil {
			t.Fatalf("QuerySkillUsage: %v", err)
		}
		if len(usage) != 2 {
			t.Fatalf("want 2 existing skill rows preserved, got %d: %+v", len(usage), usage)
		}
	})

	t.Run("duplicate import refreshes mission fields for existing rows", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")

		db := tempDB(t)
		record := MissionRecord{
			WorkspaceID:  "ws-import-refresh",
			Domain:       "dev",
			Task:         "initial task",
			StartedAt:    now,
			FinishedAt:   now.Add(time.Minute),
			DurationSec:  60,
			PhasesTotal:  1,
			Status:       "success",
			DecompSource: decompSourceUnknown,
			Phases: []PhaseRecord{
				{
					ID:           "phase-1",
					Name:         "implement",
					Persona:      "backend-engineer",
					Skills:       []string{"watermark"},
					ParsedSkills: []string{"scout"},
					Status:       "completed",
				},
			},
		}
		if _, err := db.UpsertMission(context.Background(), record); err != nil {
			t.Fatalf("UpsertMission: %v", err)
		}

		refreshed := record
		refreshed.Task = "refreshed task"
		refreshed.DecompSource = "decomp.llm"
		refreshed.Phases = nil

		f, err := os.Create(path)
		if err != nil {
			t.Fatalf("create: %v", err)
		}
		if err := json.NewEncoder(f).Encode(refreshed); err != nil {
			t.Fatalf("encode: %v", err)
		}
		if err := f.Close(); err != nil {
			t.Fatalf("close: %v", err)
		}

		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		if imported != 0 {
			t.Fatalf("expected 0 imported on duplicate import, got %d", imported)
		}

		var task, decompSource string
		if err := db.db.QueryRow(`SELECT task, decomp_source FROM missions WHERE id = ?`, record.WorkspaceID).Scan(&task, &decompSource); err != nil {
			t.Fatalf("query mission: %v", err)
		}
		if task != "refreshed task" {
			t.Fatalf("task = %q, want %q", task, "refreshed task")
		}
		if decompSource != "decomp.llm" {
			t.Fatalf("decomp_source = %q, want %q", decompSource, "decomp.llm")
		}

		usage, err := db.QuerySkillUsage(context.Background())
		if err != nil {
			t.Fatalf("QuerySkillUsage: %v", err)
		}
		if len(usage) != 2 {
			t.Fatalf("want 2 existing skill rows preserved, got %d: %+v", len(usage), usage)
		}
	})

	t.Run("duplicate import refreshes phase fields while preserving existing skills", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")

		db := tempDB(t)
		record := MissionRecord{
			WorkspaceID: "ws-import-phase-refresh",
			Domain:      "dev",
			Task:        "phase refresh",
			StartedAt:   now,
			FinishedAt:  now.Add(time.Minute),
			DurationSec: 60,
			PhasesTotal: 1,
			Status:      "success",
			Phases: []PhaseRecord{
				{
					ID:              "phase-1",
					Name:            "implement",
					Persona:         "backend-engineer",
					SelectionMethod: "llm",
					Skills:          []string{"watermark"},
					ParsedSkills:    []string{"scout"},
					DurationS:       60,
					Status:          "completed",
					OutputLen:       100,
				},
			},
		}
		if _, err := db.UpsertMission(context.Background(), record); err != nil {
			t.Fatalf("UpsertMission: %v", err)
		}

		refreshed := record
		refreshed.Phases = []PhaseRecord{
			{
				ID:              "phase-1",
				Name:            "implement",
				Persona:         "backend-engineer",
				SelectionMethod: "keyword",
				DurationS:       95,
				Status:          "failed",
				OutputLen:       321,
			},
		}

		f, err := os.Create(path)
		if err != nil {
			t.Fatalf("create: %v", err)
		}
		if err := json.NewEncoder(f).Encode(refreshed); err != nil {
			t.Fatalf("encode: %v", err)
		}
		if err := f.Close(); err != nil {
			t.Fatalf("close: %v", err)
		}

		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		if imported != 0 {
			t.Fatalf("expected 0 imported on duplicate import, got %d", imported)
		}

		var duration int
		var status, selectionMethod string
		if err := db.db.QueryRow(`
				SELECT duration_s, status, selection_method
				FROM phases
				WHERE mission_id = ? AND name = ?
			`, record.WorkspaceID, "implement").Scan(&duration, &status, &selectionMethod); err != nil {
			t.Fatalf("query phase: %v", err)
		}
		if duration != 95 {
			t.Fatalf("duration_s = %d, want 95", duration)
		}
		if status != "failed" {
			t.Fatalf("status = %q, want failed", status)
		}
		if selectionMethod != "keyword" {
			t.Fatalf("selection_method = %q, want keyword", selectionMethod)
		}

		usage, err := db.QuerySkillUsage(context.Background())
		if err != nil {
			t.Fatalf("QuerySkillUsage: %v", err)
		}
		if len(usage) != 2 {
			t.Fatalf("want 2 existing skill rows preserved, got %d: %+v", len(usage), usage)
		}
	})

	t.Run("engine JSON field persona_selection_method survives import", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")
		content := `{"workspace_id":"ws-import-selection","domain":"dev","task":"selection import","started_at":"2026-01-01T00:00:00Z","finished_at":"2026-01-01T00:01:00Z","status":"success","phases":[{"id":"phase-1","name":"implement","persona":"backend-engineer","persona_selection_method":"keyword"}]}`
		if err := os.WriteFile(path, []byte(content+"\n"), 0600); err != nil {
			t.Fatalf("write: %v", err)
		}

		db := tempDB(t)
		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		if imported != 1 {
			t.Fatalf("expected 1 imported, got %d", imported)
		}

		var method string
		err = db.db.QueryRow(`SELECT selection_method FROM phases WHERE mission_id = ?`, "ws-import-selection").Scan(&method)
		if err != nil {
			t.Fatalf("query selection_method: %v", err)
		}
		if method != "keyword" {
			t.Fatalf("selection_method = %q, want %q", method, "keyword")
		}
	})

	t.Run("legacy selection_method key still imports", func(t *testing.T) {
		dir := t.TempDir()
		path := filepath.Join(dir, "metrics.jsonl")
		content := `{"workspace_id":"ws-import-selection-legacy","domain":"dev","task":"selection import legacy","started_at":"2026-01-01T00:00:00Z","finished_at":"2026-01-01T00:01:00Z","status":"success","phases":[{"id":"phase-1","name":"implement","persona":"backend-engineer","selection_method":"llm"}]}`
		if err := os.WriteFile(path, []byte(content+"\n"), 0600); err != nil {
			t.Fatalf("write: %v", err)
		}

		db := tempDB(t)
		imported, err := db.ImportFromJSONL(context.Background(), path)
		if err != nil {
			t.Fatalf("ImportFromJSONL: %v", err)
		}
		if imported != 1 {
			t.Fatalf("expected 1 imported, got %d", imported)
		}

		var method string
		err = db.db.QueryRow(`SELECT selection_method FROM phases WHERE mission_id = ?`, "ws-import-selection-legacy").Scan(&method)
		if err != nil {
			t.Fatalf("query selection_method: %v", err)
		}
		if method != "llm" {
			t.Fatalf("selection_method = %q, want %q", method, "llm")
		}
	})
}

func TestImportMissingFromJSONL_PreservesExistingRows(t *testing.T) {
	now := time.Now().Truncate(time.Second).UTC()
	dir := t.TempDir()
	path := filepath.Join(dir, "metrics.jsonl")

	db := tempDB(t)
	existing := MissionRecord{
		WorkspaceID: "ws-import-missing-existing",
		Domain:      "dev",
		Task:        "existing task",
		StartedAt:   now,
		FinishedAt:  now.Add(time.Minute),
		DurationSec: 60,
		PhasesTotal: 1,
		Status:      "success",
		Phases: []PhaseRecord{
			{
				ID:           "phase-1",
				Name:         "implement",
				Persona:      "backend-engineer",
				Skills:       []string{"watermark"},
				ParsedSkills: []string{"scout"},
				Status:       "completed",
			},
		},
	}
	if _, err := db.UpsertMission(context.Background(), existing); err != nil {
		t.Fatalf("UpsertMission(existing): %v", err)
	}

	staleExisting := existing
	staleExisting.Task = "stale task from jsonl"
	staleExisting.Phases = []PhaseRecord{
		{
			ID:           "phase-1",
			Name:         "implement",
			Persona:      "backend-engineer",
			Skills:       []string{"scheduler"},
			ParsedSkills: []string{"obsidian"},
			Status:       "failed",
		},
	}
	missing := MissionRecord{
		WorkspaceID: "ws-import-missing-new",
		Domain:      "work",
		Task:        "new mission from jsonl",
		StartedAt:   now.Add(time.Hour),
		FinishedAt:  now.Add(time.Hour + time.Minute),
		DurationSec: 60,
		PhasesTotal: 1,
		Status:      "success",
	}

	f, err := os.Create(path)
	if err != nil {
		t.Fatalf("create: %v", err)
	}
	enc := json.NewEncoder(f)
	for _, record := range []MissionRecord{staleExisting, missing} {
		if err := enc.Encode(record); err != nil {
			t.Fatalf("encode: %v", err)
		}
	}
	if err := f.Close(); err != nil {
		t.Fatalf("close: %v", err)
	}

	imported, err := db.ImportMissingFromJSONL(context.Background(), path)
	if err != nil {
		t.Fatalf("ImportMissingFromJSONL: %v", err)
	}
	if imported != 1 {
		t.Fatalf("imported = %d, want 1", imported)
	}

	var task string
	if err := db.db.QueryRow(`SELECT task FROM missions WHERE id = ?`, existing.WorkspaceID).Scan(&task); err != nil {
		t.Fatalf("query existing mission: %v", err)
	}
	if task != "existing task" {
		t.Fatalf("existing task = %q, want %q", task, "existing task")
	}

	usage, err := db.QuerySkillUsage(context.Background())
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 preserved skill rows for existing mission, got %d: %+v", len(usage), usage)
	}

	var newTask string
	if err := db.db.QueryRow(`SELECT task FROM missions WHERE id = ?`, missing.WorkspaceID).Scan(&newTask); err != nil {
		t.Fatalf("query missing mission: %v", err)
	}
	if newTask != missing.Task {
		t.Fatalf("new task = %q, want %q", newTask, missing.Task)
	}
}

// TestDecompSource verifies that decomp_source is stored and filtered correctly.
func TestDecompSource(t *testing.T) {
	db := tempDB(t)
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	// Seed missions with different decomp_source values.
	missions := []MissionRecord{
		{
			WorkspaceID:  "ws-decomp-1",
			Domain:       "dev",
			Status:       "success",
			DecompSource: "predecomposed",
			StartedAt:    now.Add(-3 * time.Hour),
			FinishedAt:   now.Add(-2 * time.Hour),
		},
		{
			WorkspaceID:  "ws-decomp-2",
			Domain:       "dev",
			Status:       "success",
			DecompSource: "decomp.llm",
			StartedAt:    now.Add(-2 * time.Hour),
			FinishedAt:   now.Add(-1 * time.Hour),
		},
		{
			WorkspaceID:  "ws-decomp-3",
			Domain:       "dev",
			Status:       "failure",
			DecompSource: "decomp.keyword",
			StartedAt:    now.Add(-1 * time.Hour),
			FinishedAt:   now,
		},
		{
			WorkspaceID:  "ws-decomp-4",
			Domain:       "dev",
			Status:       "success",
			DecompSource: "", // empty → stored as "unknown"
			StartedAt:    now.Add(-30 * time.Minute),
			FinishedAt:   now.Add(-20 * time.Minute),
		},
	}
	for _, m := range missions {
		if err := db.RecordMission(ctx, m); err != nil {
			t.Fatalf("RecordMission(%s): %v", m.WorkspaceID, err)
		}
	}

	t.Run("decomp_source stored correctly", func(t *testing.T) {
		var src string
		db.db.QueryRow("SELECT decomp_source FROM missions WHERE id = ?", "ws-decomp-1").Scan(&src)
		if src != "predecomposed" {
			t.Errorf("ws-decomp-1: want decomp_source=predecomposed, got %q", src)
		}
		db.db.QueryRow("SELECT decomp_source FROM missions WHERE id = ?", "ws-decomp-2").Scan(&src)
		if src != "decomp.llm" {
			t.Errorf("ws-decomp-2: want decomp_source=decomp.llm, got %q", src)
		}
		db.db.QueryRow("SELECT decomp_source FROM missions WHERE id = ?", "ws-decomp-3").Scan(&src)
		if src != "decomp.keyword" {
			t.Errorf("ws-decomp-3: want decomp_source=decomp.keyword, got %q", src)
		}
		// Empty DecompSource falls back to "unknown".
		db.db.QueryRow("SELECT decomp_source FROM missions WHERE id = ?", "ws-decomp-4").Scan(&src)
		if src != "unknown" {
			t.Errorf("ws-decomp-4: want decomp_source=unknown for empty input, got %q", src)
		}
	})

	t.Run("QueryMissions filters by decomp_source", func(t *testing.T) {
		rows, err := db.QueryMissions(ctx, 20, "", 0, "", "predecomposed", "")
		if err != nil {
			t.Fatalf("QueryMissions(predecomposed): %v", err)
		}
		if len(rows) != 1 {
			t.Fatalf("want 1 predecomposed mission, got %d", len(rows))
		}
		if rows[0].WorkspaceID != "ws-decomp-1" {
			t.Errorf("want ws-decomp-1, got %q", rows[0].WorkspaceID)
		}
		if rows[0].DecompSource != "predecomposed" {
			t.Errorf("want DecompSource=predecomposed, got %q", rows[0].DecompSource)
		}
	})

	t.Run("QueryMissions filters by decomp.llm", func(t *testing.T) {
		rows, err := db.QueryMissions(ctx, 20, "", 0, "", "decomp.llm", "")
		if err != nil {
			t.Fatalf("QueryMissions(decomp.llm): %v", err)
		}
		if len(rows) != 1 {
			t.Fatalf("want 1 llm mission, got %d", len(rows))
		}
		if rows[0].WorkspaceID != "ws-decomp-2" {
			t.Errorf("want ws-decomp-2, got %q", rows[0].WorkspaceID)
		}
	})

	t.Run("QueryMissions empty decomp_source returns all", func(t *testing.T) {
		rows, err := db.QueryMissions(ctx, 20, "", 0, "", "", "")
		if err != nil {
			t.Fatalf("QueryMissions(no filter): %v", err)
		}
		if len(rows) != 4 {
			t.Errorf("want 4 missions with no filter, got %d", len(rows))
		}
	})

	t.Run("QueryMissions combines decomp_source with status filter", func(t *testing.T) {
		// Only decomp.keyword missions with status=failure — should be exactly ws-decomp-3.
		rows, err := db.QueryMissions(ctx, 20, "", 0, "failure", "decomp.keyword", "")
		if err != nil {
			t.Fatalf("QueryMissions(failure+decomp.keyword): %v", err)
		}
		if len(rows) != 1 {
			t.Fatalf("want 1 row, got %d", len(rows))
		}
		if rows[0].WorkspaceID != "ws-decomp-3" {
			t.Errorf("want ws-decomp-3, got %q", rows[0].WorkspaceID)
		}
	})

	t.Run("empty DecompSource defaults to unknown", func(t *testing.T) {
		// ws-decomp-4 was inserted with DecompSource=""; RecordMission normalises
		// empty strings to "unknown" before the INSERT.
		var src string
		db.db.QueryRow("SELECT decomp_source FROM missions WHERE id = ?", "ws-decomp-4").Scan(&src)
		if src != "unknown" {
			t.Errorf("empty DecompSource: want unknown, got %q", src)
		}
	})
}

func TestQueryRoutingMethodDistribution(t *testing.T) {
	ctx := context.Background()
	now := time.Now().Truncate(time.Second).UTC()

	t.Run("empty database returns empty slice", func(t *testing.T) {
		db := tempDB(t)
		dist, err := db.QueryRoutingMethodDistribution(ctx)
		if err != nil {
			t.Fatalf("QueryRoutingMethodDistribution: %v", err)
		}
		if len(dist) != 0 {
			t.Errorf("want empty slice, got %d rows", len(dist))
		}
	})

	t.Run("counts and percentages are correct", func(t *testing.T) {
		db := tempDB(t)
		// Seed: 4 llm, 2 keyword, 1 fallback, 1 required_review (excluded).
		// Total counted = 7; required_review excluded.
		mission := MissionRecord{
			WorkspaceID: "ws-rmd-1", Domain: "dev", Status: "success",
			StartedAt: now.Add(-1 * time.Hour), FinishedAt: now,
			DurationSec: 3600, PhasesTotal: 8, PhasesCompleted: 8,
			Phases: []PhaseRecord{
				{Name: "p1", Persona: "architect", SelectionMethod: "llm",
					DurationS: 10, Status: "completed"},
				{Name: "p2", Persona: "architect", SelectionMethod: "llm",
					DurationS: 10, Status: "completed"},
				{Name: "p3", Persona: "backend-engineer", SelectionMethod: "llm",
					DurationS: 10, Status: "completed"},
				{Name: "p4", Persona: "backend-engineer", SelectionMethod: "llm",
					DurationS: 10, Status: "completed"},
				{Name: "p5", Persona: "backend-engineer", SelectionMethod: "keyword",
					DurationS: 10, Status: "completed"},
				{Name: "p6", Persona: "backend-engineer", SelectionMethod: "keyword",
					DurationS: 10, Status: "completed"},
				{Name: "p7", Persona: "backend-engineer", SelectionMethod: "fallback",
					DurationS: 10, Status: "completed"},
				{Name: "p8", Persona: "staff-code-reviewer", SelectionMethod: "required_review",
					DurationS: 10, Status: "completed"},
			},
		}
		if err := db.RecordMission(ctx, mission); err != nil {
			t.Fatalf("RecordMission: %v", err)
		}

		dist, err := db.QueryRoutingMethodDistribution(ctx)
		if err != nil {
			t.Fatalf("QueryRoutingMethodDistribution: %v", err)
		}

		// Expect 3 rows (required_review excluded), ordered by count DESC.
		if len(dist) != 3 {
			t.Fatalf("want 3 rows, got %d: %+v", len(dist), dist)
		}

		byMethod := make(map[string]RoutingMethodDist)
		for _, r := range dist {
			byMethod[r.Method] = r
		}

		// First row must be llm with 4 phases.
		if dist[0].Method != "llm" {
			t.Errorf("want first row = llm, got %q", dist[0].Method)
		}
		if dist[0].Count != 4 {
			t.Errorf("llm count: want 4, got %d", dist[0].Count)
		}

		// llm pct = 4/7 * 100 ≈ 57.14
		wantLLMPct := 4.0 / 7.0 * 100
		if d := byMethod["llm"].Pct - wantLLMPct; d > 0.1 || d < -0.1 {
			t.Errorf("llm pct: want %.2f, got %.2f", wantLLMPct, byMethod["llm"].Pct)
		}

		// keyword pct = 2/7 * 100 ≈ 28.57
		wantKWPct := 2.0 / 7.0 * 100
		if d := byMethod["keyword"].Pct - wantKWPct; d > 0.1 || d < -0.1 {
			t.Errorf("keyword pct: want %.2f, got %.2f", wantKWPct, byMethod["keyword"].Pct)
		}

		// fallback pct = 1/7 * 100 ≈ 14.29
		wantFBPct := 1.0 / 7.0 * 100
		if d := byMethod["fallback"].Pct - wantFBPct; d > 0.1 || d < -0.1 {
			t.Errorf("fallback pct: want %.2f, got %.2f", wantFBPct, byMethod["fallback"].Pct)
		}

		// required_review must not appear.
		if _, ok := byMethod["required_review"]; ok {
			t.Error("required_review should be excluded from distribution")
		}
	})

	t.Run("required_review only phases return empty", func(t *testing.T) {
		db := tempDB(t)
		mission := MissionRecord{
			WorkspaceID: "ws-rmd-2", Domain: "dev", Status: "success",
			StartedAt: now.Add(-30 * time.Minute), FinishedAt: now,
			DurationSec: 1800, PhasesTotal: 1, PhasesCompleted: 1,
			Phases: []PhaseRecord{
				{Name: "review", Persona: "staff-code-reviewer", SelectionMethod: "required_review",
					DurationS: 60, Status: "completed"},
			},
		}
		if err := db.RecordMission(ctx, mission); err != nil {
			t.Fatalf("RecordMission: %v", err)
		}
		dist, err := db.QueryRoutingMethodDistribution(ctx)
		if err != nil {
			t.Fatalf("QueryRoutingMethodDistribution: %v", err)
		}
		if len(dist) != 0 {
			t.Errorf("want empty distribution, got %d rows", len(dist))
		}
	})
}

func TestFallbackRate(t *testing.T) {
	tests := []struct {
		name string
		dist []RoutingMethodDist
		want float64
	}{
		{
			name: "empty distribution returns zero",
			dist: nil,
			want: 0.0,
		},
		{
			name: "no fallback row returns zero",
			dist: []RoutingMethodDist{
				{Method: "llm", Count: 8, Pct: 80.0},
				{Method: "keyword", Count: 2, Pct: 20.0},
			},
			want: 0.0,
		},
		{
			name: "fallback row present returns its pct",
			dist: []RoutingMethodDist{
				{Method: "llm", Count: 5, Pct: 50.0},
				{Method: "keyword", Count: 2, Pct: 20.0},
				{Method: "fallback", Count: 3, Pct: 30.0},
			},
			want: 30.0,
		},
		{
			name: "fallback above threshold triggers alert invariant",
			dist: []RoutingMethodDist{
				{Method: "llm", Count: 3, Pct: 30.0},
				{Method: "fallback", Count: 7, Pct: 70.0},
			},
			want: 70.0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := FallbackRate(tt.dist)
			if got != tt.want {
				t.Errorf("FallbackRate() = %.2f, want %.2f", got, tt.want)
			}
		})
	}

	t.Run("alert threshold constant is 30", func(t *testing.T) {
		if FallbackAlertThreshold != 30.0 {
			t.Errorf("FallbackAlertThreshold = %.1f, want 30.0", FallbackAlertThreshold)
		}
	})
}

// TestMigrationFromLegacySchema verifies that InitDB correctly migrates a
// pre-existing database that was created before the "phase" and "source" columns
// were added to skill_invocations. This is the regression test for the bug where
// idx_skill_invocations_phase was created in the DDL block (before migrations),
// causing initSchema to fail on legacy databases because the "phase" column
// didn't exist yet. The error was silently swallowed, so no skill invocations
// were ever recorded.
func TestMigrationFromLegacySchema(t *testing.T) {
	dir := t.TempDir()
	dbPath := filepath.Join(dir, "metrics.db")

	// Step 1: Create a database with the LEGACY schema (no phase/source columns
	// on skill_invocations, no decomp_source on missions).
	raw, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open raw db: %v", err)
	}

	legacyDDL := []string{
		`CREATE TABLE missions (
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
			status              TEXT NOT NULL DEFAULT ''
		)`,
		`CREATE TABLE phases (
			id                  TEXT PRIMARY KEY,
			mission_id          TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
			name                TEXT NOT NULL DEFAULT '',
			persona             TEXT NOT NULL DEFAULT '',
			duration_s          INTEGER NOT NULL DEFAULT 0,
			status              TEXT NOT NULL DEFAULT '',
			retries             INTEGER NOT NULL DEFAULT 0,
			gate_passed         INTEGER NOT NULL DEFAULT 0,
			output_len          INTEGER NOT NULL DEFAULT 0,
			learnings_retrieved INTEGER NOT NULL DEFAULT 0
		)`,
		// Legacy skill_invocations: no "phase" or "source" columns.
		`CREATE TABLE skill_invocations (
			id         INTEGER PRIMARY KEY AUTOINCREMENT,
			mission_id TEXT NOT NULL REFERENCES missions(id) ON DELETE CASCADE,
			persona    TEXT NOT NULL DEFAULT '',
			skill_name TEXT NOT NULL DEFAULT '',
			invoked_at DATETIME NOT NULL
		)`,
		`CREATE INDEX idx_missions_domain ON missions(domain)`,
		`CREATE INDEX idx_missions_status ON missions(status)`,
		`CREATE INDEX idx_missions_started_at ON missions(started_at)`,
		`CREATE INDEX idx_phases_mission_id ON phases(mission_id)`,
		`CREATE INDEX idx_phases_persona ON phases(persona)`,
		`CREATE INDEX idx_skill_invocations_mission_id ON skill_invocations(mission_id)`,
		`CREATE INDEX idx_skill_invocations_skill_name ON skill_invocations(skill_name)`,
		`CREATE INDEX idx_skill_invocations_persona ON skill_invocations(persona)`,
		`CREATE INDEX idx_skill_invocations_persona_skill ON skill_invocations(persona, skill_name)`,
	}

	for _, ddl := range legacyDDL {
		if _, err := raw.Exec(ddl); err != nil {
			t.Fatalf("legacy DDL: %v", err)
		}
	}

	// Insert a legacy row to verify data survives migration.
	now := time.Now().UTC()
	_, err = raw.Exec(`INSERT INTO missions (id, domain, task, started_at, finished_at, status) VALUES (?, ?, ?, ?, ?, ?)`,
		"ws-legacy", "dev", "legacy task", now.Format(time.RFC3339), now.Add(time.Minute).Format(time.RFC3339), "success")
	if err != nil {
		t.Fatalf("insert legacy mission: %v", err)
	}
	raw.Close()

	// Step 2: Open with InitDB — this must successfully migrate the schema.
	db, err := InitDB(dbPath)
	if err != nil {
		t.Fatalf("InitDB on legacy database failed: %v", err)
	}
	defer db.Close()

	// Step 3: Verify that the "phase" and "source" columns now exist by
	// inserting a skill invocation that uses them.
	ctx := context.Background()
	err = db.RecordSkillInvocation(ctx, "ws-legacy", "implement", "backend-engineer", "golang-pro", SkillSourceDeclared)
	if err != nil {
		t.Fatalf("RecordSkillInvocation after migration failed: %v", err)
	}

	// Step 4: Verify the row was stored with the correct phase and source values.
	var phase, source string
	err = db.db.QueryRow("SELECT phase, source FROM skill_invocations WHERE mission_id = ?", "ws-legacy").Scan(&phase, &source)
	if err != nil {
		t.Fatalf("querying migrated skill_invocations: %v", err)
	}
	if phase != "implement" {
		t.Errorf("want phase=implement, got %q", phase)
	}
	if source != SkillSourceDeclared {
		t.Errorf("want source=%s, got %q", SkillSourceDeclared, source)
	}

	// Step 5: Verify the idx_skill_invocations_phase index was created.
	var idxName string
	err = db.db.QueryRow("SELECT name FROM sqlite_master WHERE type='index' AND name='idx_skill_invocations_phase'").Scan(&idxName)
	if err != nil {
		t.Errorf("idx_skill_invocations_phase index not found after migration: %v", err)
	}

	// Step 6: Verify legacy data survived the migration.
	var task string
	err = db.db.QueryRow("SELECT task FROM missions WHERE id = ?", "ws-legacy").Scan(&task)
	if err != nil {
		t.Fatalf("legacy mission not found after migration: %v", err)
	}
	if task != "legacy task" {
		t.Errorf("legacy mission task: want %q, got %q", "legacy task", task)
	}

}

// readPhaseCacheFields queries the raw phases table for the two cache token
// columns of the given phase (identified by missionID + "_" + phaseID).
func readPhaseCacheFields(t *testing.T, db *DB, missionID, phaseID string) (creation, read int) {
	t.Helper()
	pk := missionID + "_" + phaseID
	err := db.db.QueryRow(
		`SELECT tokens_cache_creation, tokens_cache_read FROM phases WHERE id = ?`,
		pk,
	).Scan(&creation, &read)
	if err != nil {
		t.Fatalf("reading cache fields for phase %s: %v", pk, err)
	}
	return creation, read
}

// TestPhaseCacheTokens_InsertAndUpsert covers two scenarios:
//
// (a) Insert a PhaseRecord with cache token values via RecordMission (which
// calls insertPhase), read back through the raw DB, and assert both fields
// equal the inserted values.
//
// (b) Upsert round-trip: insert once with cache values A, upsert the same
// phase ID with cache values B via UpsertMissionPhaseSnapshot, read back,
// assert B won.
func TestPhaseCacheTokens_InsertAndUpsert(t *testing.T) {
	ctx := context.Background()
	now := time.Now().UTC()

	baseMission := func(wsID string) MissionRecord {
		return MissionRecord{
			WorkspaceID: wsID,
			Domain:      "dev",
			StartedAt:   now,
			FinishedAt:  now.Add(time.Minute),
			Status:      "success",
		}
	}

	t.Run("insert preserves cache fields", func(t *testing.T) {
		db := tempDB(t)
		m := baseMission("ws-cache-insert")
		m.Phases = []PhaseRecord{
			{
				ID:                  "phase-a",
				Name:                "phase-a",
				Persona:             "backend-engineer",
				Status:              "completed",
				GatePassed:          true,
				TokensCacheCreation: 12345,
				TokensCacheRead:     67890,
			},
		}

		if err := db.RecordMission(ctx, m); err != nil {
			t.Fatalf("RecordMission: %v", err)
		}

		gotCreation, gotRead := readPhaseCacheFields(t, db, "ws-cache-insert", "phase-a")
		if gotCreation != 12345 {
			t.Errorf("tokens_cache_creation: want 12345, got %d", gotCreation)
		}
		if gotRead != 67890 {
			t.Errorf("tokens_cache_read: want 67890, got %d", gotRead)
		}
	})

	t.Run("upsert overwrites cache fields", func(t *testing.T) {
		db := tempDB(t)
		wsID := "ws-cache-upsert"

		// First write — cache values A.
		phaseA := PhaseRecord{
			ID:                  "phase-b",
			Name:                "phase-b",
			Persona:             "backend-engineer",
			Status:              "running",
			TokensCacheCreation: 111,
			TokensCacheRead:     222,
		}
		if err := db.UpsertMissionPhaseSnapshot(ctx, baseMission(wsID), phaseA); err != nil {
			t.Fatalf("UpsertMissionPhaseSnapshot (A): %v", err)
		}

		gotCreation, gotRead := readPhaseCacheFields(t, db, wsID, "phase-b")
		if gotCreation != 111 || gotRead != 222 {
			t.Fatalf("after first upsert: want (111, 222), got (%d, %d)", gotCreation, gotRead)
		}

		// Second write — cache values B must win.
		phaseB := PhaseRecord{
			ID:                  "phase-b",
			Name:                "phase-b",
			Persona:             "backend-engineer",
			Status:              "completed",
			GatePassed:          true,
			TokensCacheCreation: 9999,
			TokensCacheRead:     8888,
		}
		if err := db.UpsertMissionPhaseSnapshot(ctx, baseMission(wsID), phaseB); err != nil {
			t.Fatalf("UpsertMissionPhaseSnapshot (B): %v", err)
		}

		gotCreation, gotRead = readPhaseCacheFields(t, db, wsID, "phase-b")
		if gotCreation != 9999 {
			t.Errorf("tokens_cache_creation after upsert: want 9999, got %d", gotCreation)
		}
		if gotRead != 8888 {
			t.Errorf("tokens_cache_read after upsert: want 8888, got %d", gotRead)
		}
	})
}

func TestInsertUsageSnapshot(t *testing.T) {
	ctx := context.Background()

	t.Run("non-zero values round-trip including nullable timestamps", func(t *testing.T) {
		db := tempDB(t)

		fiveResets := time.Unix(1_700_000_100, 0).UTC().Truncate(time.Second)
		sevenResets := time.Unix(1_700_000_200, 0).UTC().Truncate(time.Second)
		sonnetResets := time.Unix(1_700_000_300, 0).UTC().Truncate(time.Second)
		capturedAt := time.Now().UTC().Truncate(time.Second)

		snap := UsageSnapshot{
			CapturedAt:             capturedAt,
			MissionID:              "ws-usage-test",
			FiveHourUtil:           0.25,
			FiveHourResetsAt:       &fiveResets,
			SevenDayUtil:           0.50,
			SevenDayResetsAt:       &sevenResets,
			SevenDaySonnetUtil:     0.75,
			SevenDaySonnetResetsAt: &sonnetResets,
			RawJSON:                `{"five_hour":{"utilization":0.25}}`,
		}

		if err := db.InsertUsageSnapshot(ctx, snap); err != nil {
			t.Fatalf("InsertUsageSnapshot: %v", err)
		}

		// Read back via raw SELECT.
		var (
			gotCapturedAt            string
			gotMissionID             string
			gotFiveHourUtil          float64
			gotFiveHourResetsAt      sql.NullString
			gotSevenDayUtil          float64
			gotSevenDayResetsAt      sql.NullString
			gotSevenDaySonnetUtil    float64
			gotSevenDaySonnetResetsAt sql.NullString
			gotRawJSON               string
		)
		err := db.db.QueryRowContext(ctx, `
			SELECT captured_at, mission_id,
			       five_hour_util, five_hour_resets_at,
			       seven_day_util, seven_day_resets_at,
			       seven_day_sonnet_util, seven_day_sonnet_resets_at,
			       raw_json
			FROM usage_snapshots
			WHERE mission_id = ?
		`, "ws-usage-test").Scan(
			&gotCapturedAt, &gotMissionID,
			&gotFiveHourUtil, &gotFiveHourResetsAt,
			&gotSevenDayUtil, &gotSevenDayResetsAt,
			&gotSevenDaySonnetUtil, &gotSevenDaySonnetResetsAt,
			&gotRawJSON,
		)
		if err != nil {
			t.Fatalf("SELECT: %v", err)
		}

		if gotMissionID != "ws-usage-test" {
			t.Errorf("mission_id: want ws-usage-test, got %s", gotMissionID)
		}
		if gotFiveHourUtil != 0.25 {
			t.Errorf("five_hour_util: want 0.25, got %v", gotFiveHourUtil)
		}
		if gotSevenDayUtil != 0.50 {
			t.Errorf("seven_day_util: want 0.50, got %v", gotSevenDayUtil)
		}
		if gotSevenDaySonnetUtil != 0.75 {
			t.Errorf("seven_day_sonnet_util: want 0.75, got %v", gotSevenDaySonnetUtil)
		}
		if gotRawJSON != `{"five_hour":{"utilization":0.25}}` {
			t.Errorf("raw_json: got %s", gotRawJSON)
		}

		// Nullable timestamps must be present and match.
		for _, tc := range []struct {
			field string
			ns    sql.NullString
			want  time.Time
		}{
			{"five_hour_resets_at", gotFiveHourResetsAt, fiveResets},
			{"seven_day_resets_at", gotSevenDayResetsAt, sevenResets},
			{"seven_day_sonnet_resets_at", gotSevenDaySonnetResetsAt, sonnetResets},
		} {
			if !tc.ns.Valid {
				t.Errorf("%s: want non-NULL, got NULL", tc.field)
				continue
			}
			parsed, err := time.Parse(time.RFC3339, tc.ns.String)
			if err != nil {
				t.Errorf("%s: parse %q: %v", tc.field, tc.ns.String, err)
				continue
			}
			if !parsed.UTC().Equal(tc.want) {
				t.Errorf("%s: want %v, got %v", tc.field, tc.want, parsed.UTC())
			}
		}

		// captured_at must round-trip.
		parsedCaptured, err := time.Parse(time.RFC3339, gotCapturedAt)
		if err != nil {
			t.Fatalf("captured_at parse: %v", err)
		}
		if !parsedCaptured.UTC().Equal(capturedAt) {
			t.Errorf("captured_at: want %v, got %v", capturedAt, parsedCaptured.UTC())
		}
	})

	t.Run("zero resets_at stores as NULL", func(t *testing.T) {
		db := tempDB(t)

		// Pass nil pointers for all three resets_at fields.
		snap := UsageSnapshot{
			CapturedAt:   time.Now().UTC(),
			MissionID:    "ws-usage-null",
			FiveHourUtil: 0.10,
			// FiveHourResetsAt nil → NULL
			SevenDayUtil: 0.20,
			// SevenDayResetsAt nil → NULL
			SevenDaySonnetUtil: 0.30,
			// SevenDaySonnetResetsAt nil → NULL
			RawJSON: `{}`,
		}

		if err := db.InsertUsageSnapshot(ctx, snap); err != nil {
			t.Fatalf("InsertUsageSnapshot: %v", err)
		}

		var (
			gotFiveHourResetsAt      sql.NullString
			gotSevenDayResetsAt      sql.NullString
			gotSevenDaySonnetResetsAt sql.NullString
		)
		err := db.db.QueryRowContext(ctx, `
			SELECT five_hour_resets_at, seven_day_resets_at, seven_day_sonnet_resets_at
			FROM usage_snapshots
			WHERE mission_id = ?
		`, "ws-usage-null").Scan(&gotFiveHourResetsAt, &gotSevenDayResetsAt, &gotSevenDaySonnetResetsAt)
		if err != nil {
			t.Fatalf("SELECT: %v", err)
		}

		for _, tc := range []struct {
			field string
			ns    sql.NullString
		}{
			{"five_hour_resets_at", gotFiveHourResetsAt},
			{"seven_day_resets_at", gotSevenDayResetsAt},
			{"seven_day_sonnet_resets_at", gotSevenDaySonnetResetsAt},
		} {
			if tc.ns.Valid {
				t.Errorf("%s: want NULL, got %q", tc.field, tc.ns.String)
			}
		}
	})
}
