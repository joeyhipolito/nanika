package engine

// TestMetricsIntegration verifies the full pipeline:
//
//	buildMetrics → toMissionRecord → metricsdb.RecordMission → QueryMissions
//
// The test uses real core.Plan / core.Phase structs with known terminal state,
// calls the same functions the engine calls after a real mission, then queries
// the SQLite database and asserts the stored values match expectations.
//
// No network calls, no Claude worker spawns — this is a pure SQLite integration test.

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	metricsdb "github.com/joeyhipolito/orchestrator-cli/internal/metrics"
	"github.com/joeyhipolito/orchestrator-cli/internal/sdk"
)

func TestMetricsIntegration(t *testing.T) {
	t.Parallel()

	// --- 1. Build a plan with known terminal state ----------------------------
	//
	// 3 phases:
	//  phase-1: architect/llm, completed, gate passed, 0 retries, 200 output chars
	//  phase-2: backend-engineer/keyword, completed, gate passed, 1 retry, 800 output chars
	//  phase-3: analyst/llm, failed, 2 retries, 0 output chars
	//
	// Expected buildMetrics result:
	//   PhasesCompleted=2, PhasesFailed=1, PhasesSkipped=0
	//   RetriesTotal=3 (0+1+2), GateFailures=0
	//   Status="partial" (not all succeeded, but some completed)

	start := time.Now().Add(-5 * time.Minute)
	t0 := start
	t1 := t0.Add(60 * time.Second)
	t2 := t1.Add(90 * time.Second)
	t3 := t2.Add(120 * time.Second)
	t4 := t3.Add(30 * time.Second)

	plan := &core.Plan{
		ID:           "plan-integration-test",
		Task:         "integration test task",
		DecompSource: core.DecompLLM,
		Phases: []*core.Phase{
			{
				ID:                     "phase-1",
				Name:                   "research",
				Persona:                "architect",
				PersonaSelectionMethod: "llm",
				Status:                 core.StatusCompleted,
				StartTime:              &t0,
				EndTime:                &t1,
				GatePassed:             true,
				OutputLen:              200,
				Retries:                0,
				LearningsRetrieved:     3,
			},
			{
				ID:                     "phase-2",
				Name:                   "implement",
				Persona:                "backend-engineer",
				PersonaSelectionMethod: "keyword",
				Status:                 core.StatusCompleted,
				StartTime:              &t1,
				EndTime:                &t2,
				GatePassed:             true,
				OutputLen:              800,
				Retries:                1,
				LearningsRetrieved:     1,
			},
			{
				ID:                     "phase-3",
				Name:                   "validate",
				Persona:                "analyst",
				PersonaSelectionMethod: "llm",
				Status:                 core.StatusFailed,
				StartTime:              &t3,
				EndTime:                &t4,
				GatePassed:             false,
				OutputLen:              0,
				Retries:                2,
				LearningsRetrieved:     0,
			},
		},
	}

	ws := &core.Workspace{
		ID:     "ws-integration-test-001",
		Domain: "dev",
	}

	result := &core.ExecutionResult{
		Plan:    plan,
		Success: false, // phase-3 failed → not full success
	}

	// --- 2. Call buildMetrics (same function the engine calls after Execute) ---

	m := buildMetrics(ws, plan, result, start)

	// Verify the computed fields before touching the DB.
	tests := []struct {
		name string
		got  any
		want any
	}{
		{"WorkspaceID", m.WorkspaceID, "ws-integration-test-001"},
		{"Domain", m.Domain, "dev"},
		{"Task", m.Task, "integration test task"},
		{"PhasesTotal", m.PhasesTotal, 3},
		{"PhasesCompleted", m.PhasesCompleted, 2},
		{"PhasesFailed", m.PhasesFailed, 1},
		{"PhasesSkipped", m.PhasesSkipped, 0},
		{"RetriesTotal", m.RetriesTotal, 3},
		{"GateFailures", m.GateFailures, 0}, // failed phases don't count as gate failures
		{"LearningsRetrieved", m.LearningsRetrieved, 4},
		{"OutputLenTotal", m.OutputLenTotal, 1000},
		{"Status", m.Status, "partial"},
		{"DecompSource", m.DecompSource, core.DecompLLM},
		{"phases count", len(m.Phases), 3},
	}
	for _, tt := range tests {
		t.Run("buildMetrics/"+tt.name, func(t *testing.T) {
			if tt.got != tt.want {
				t.Errorf("got %v, want %v", tt.got, tt.want)
			}
		})
	}

	// Verify per-phase SelectionMethod flows through buildMetrics.
	if m.Phases[0].PersonaSelectionMethod != "llm" {
		t.Errorf("phase[0] SelectionMethod: got %q, want %q", m.Phases[0].PersonaSelectionMethod, "llm")
	}
	if m.Phases[1].PersonaSelectionMethod != "keyword" {
		t.Errorf("phase[1] SelectionMethod: got %q, want %q", m.Phases[1].PersonaSelectionMethod, "keyword")
	}

	// --- 3. Convert and store in SQLite via toMissionRecord + RecordMission ---

	record := toMissionRecord(m)

	// Verify toMissionRecord maps SelectionMethod correctly.
	if record.Phases[1].SelectionMethod != "keyword" {
		t.Errorf("toMissionRecord phases[1] SelectionMethod: got %q, want %q", record.Phases[1].SelectionMethod, "keyword")
	}

	tmpDir := t.TempDir()
	db, err := metricsdb.InitDB(filepath.Join(tmpDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	ctx := context.Background()
	if err := db.RecordMission(ctx, record); err != nil {
		t.Fatalf("RecordMission: %v", err)
	}

	// --- 4. Query back and verify the data arrived intact ---------------------

	rows, err := db.QueryMissions(ctx, 10, "", 0, "", "")
	if err != nil {
		t.Fatalf("QueryMissions: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("want 1 mission row, got %d", len(rows))
	}

	got := rows[0]
	if got.WorkspaceID != "ws-integration-test-001" {
		t.Errorf("WorkspaceID: got %q", got.WorkspaceID)
	}
	if got.Domain != "dev" {
		t.Errorf("Domain: got %q", got.Domain)
	}
	if got.Status != "partial" {
		t.Errorf("Status: got %q, want partial", got.Status)
	}
	if got.PhasesTotal != 3 {
		t.Errorf("PhasesTotal: got %d, want 3", got.PhasesTotal)
	}
	if got.PhasesCompleted != 2 {
		t.Errorf("PhasesCompleted: got %d, want 2", got.PhasesCompleted)
	}
	if got.PhasesFailed != 1 {
		t.Errorf("PhasesFailed: got %d, want 1", got.PhasesFailed)
	}
	// TopPersona is the first non-empty persona assigned — should be "architect".
	if got.TopPersona != "architect" {
		t.Errorf("TopPersona: got %q, want architect", got.TopPersona)
	}
	if got.DecompSource != core.DecompLLM {
		t.Errorf("DecompSource: got %q, want %q", got.DecompSource, core.DecompLLM)
	}

	// --- 5. Verify persona metrics aggregate correctly -----------------------

	personaMetrics, err := db.QueryPersonaMetrics(ctx)
	if err != nil {
		t.Fatalf("QueryPersonaMetrics: %v", err)
	}
	// 3 distinct personas: architect (1 phase), backend-engineer (1 phase), analyst (1 phase).
	if len(personaMetrics) != 3 {
		t.Fatalf("want 3 persona rows, got %d", len(personaMetrics))
	}
	// Find analyst (the failed one).
	var analystFound bool
	for _, pm := range personaMetrics {
		if pm.Persona == "analyst" {
			analystFound = true
			if pm.FailureRate != 100 {
				t.Errorf("analyst failure_rate: got %.1f, want 100", pm.FailureRate)
			}
		}
	}
	if !analystFound {
		t.Error("analyst persona not found in QueryPersonaMetrics results")
	}
}

func TestBuildMetrics_PopulatesParsedSkillsFromPhaseOutput(t *testing.T) {
	t.Parallel()

	now := time.Now()
	t0 := now.Add(-60 * time.Second)
	t1 := now

	plan := &core.Plan{
		ID:   "plan-parsed-skills",
		Task: "test parsed skills",
		Phases: []*core.Phase{
			{
				ID:        "phase-1",
				Name:      "implement",
				Persona:   "backend-engineer",
				Status:    core.StatusCompleted,
				Output:    "⏺ Bash(scout gather \"golang\")\n⏺ Bash(obsidian capture \"note\")\n⏺ Bash(go test ./...)\n",
				StartTime: &t0,
				EndTime:   &t1,
			},
		},
	}

	ws := &core.Workspace{ID: "ws-parsed-skills-test", Domain: "dev"}
	result := &core.ExecutionResult{Plan: plan, Success: true}

	m := buildMetrics(ws, plan, result, now.Add(-60*time.Second))

	if len(m.Phases) != 1 {
		t.Fatalf("want 1 phase, got %d", len(m.Phases))
	}
	got := m.Phases[0].ParsedSkills
	want := []string{"scout", "obsidian"}
	if len(got) != len(want) {
		t.Fatalf("ParsedSkills = %v, want %v", got, want)
	}
	for i, s := range want {
		if got[i] != s {
			t.Errorf("ParsedSkills[%d] = %q, want %q", i, got[i], s)
		}
	}
}

func TestBuildMetrics_PrefersPhaseParsedSkills(t *testing.T) {
	t.Parallel()

	now := time.Now()
	t0 := now.Add(-60 * time.Second)
	t1 := now

	plan := &core.Plan{
		ID:   "plan-parsed-skills-live",
		Task: "test live parsed skills",
		Phases: []*core.Phase{
			{
				ID:           "phase-1",
				Name:         "implement",
				Persona:      "backend-engineer",
				Status:       core.StatusCompleted,
				Output:       "plain text output only",
				ParsedSkills: []string{"scout", "obsidian"},
				StartTime:    &t0,
				EndTime:      &t1,
			},
		},
	}

	ws := &core.Workspace{ID: "ws-parsed-skills-live", Domain: "dev"}
	result := &core.ExecutionResult{Plan: plan, Success: true}

	m := buildMetrics(ws, plan, result, now.Add(-60*time.Second))
	got := m.Phases[0].ParsedSkills
	want := []string{"scout", "obsidian"}
	if len(got) != len(want) {
		t.Fatalf("ParsedSkills = %v, want %v", got, want)
	}
	for i := range want {
		if got[i] != want[i] {
			t.Fatalf("ParsedSkills[%d] = %q, want %q", i, got[i], want[i])
		}
	}
}

type toolEventExecutor struct{}

func (toolEventExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	emitter.Emit(ctx, event.New(event.WorkerOutput, config.Bundle.WorkspaceID, config.Bundle.PhaseID, config.Name, map[string]any{
		"chunk":      `[tool: Bash scout gather "golang"]` + "\n",
		"event_kind": "tool_use",
		"tool_name":  "Bash",
		"streaming":  true,
	}))
	return "phase completed", "sess-tools", nil, nil
}

func TestExecutePhase_PersistsSkillInvocationsBeforeMissionCompletion(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	wsPath := t.TempDir()
	ws := &core.Workspace{
		ID:     "ws-phase-snapshot",
		Path:   wsPath,
		Domain: "dev",
	}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil, "")
	e.RegisterExecutor(core.Runtime("tool-capture"), toolEventExecutor{})

	phase := &core.Phase{
		ID:        "phase-1",
		Name:      "implement",
		Objective: "persist live skill metrics",
		Persona:   "backend-engineer",
		ModelTier: "work",
		Runtime:   core.Runtime("tool-capture"),
		Skills:    []string{"golang-pro"},
		Status:    core.StatusPending,
	}
	e.plan = &core.Plan{
		ID:           "plan-phase-snapshot",
		Task:         "persist live skill metrics",
		DecompSource: core.DecompLLM,
		Phases:       []*core.Phase{phase},
	}
	e.phases[phase.ID] = phase
	e.startTime = time.Now().Add(-time.Minute)

	if _, err := e.executePhase(context.Background(), phase, ""); err != nil {
		t.Fatalf("executePhase: %v", err)
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	usage, err := db.QuerySkillUsage(context.Background())
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 skill rows, got %d: %+v", len(usage), usage)
	}

	got := make(map[string]metricsdb.SkillUsage, len(usage))
	for _, row := range usage {
		got[row.SkillName] = row
	}
	if row, ok := got["golang-pro"]; !ok {
		t.Fatal("missing declared skill row for golang-pro")
	} else if row.Source != metricsdb.SkillSourceDeclared {
		t.Fatalf("golang-pro source = %q, want %q", row.Source, metricsdb.SkillSourceDeclared)
	}
	if row, ok := got["scout"]; !ok {
		t.Fatal("missing parsed skill row for scout")
	} else if row.Source != metricsdb.SkillSourceOutputParse {
		t.Fatalf("scout source = %q, want %q", row.Source, metricsdb.SkillSourceOutputParse)
	}
}

type retrySkillExecutor struct {
	attempts int
}

func (e *retrySkillExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	e.attempts++
	if e.attempts == 1 {
		emitter.Emit(ctx, event.New(event.WorkerOutput, config.Bundle.WorkspaceID, config.Bundle.PhaseID, config.Name, map[string]any{
			"chunk":      `[tool: Bash scout gather "golang"]` + "\n",
			"event_kind": "tool_use",
			"tool_name":  "Bash",
			"streaming":  true,
		}))
		return "first attempt failed", "sess-retry-1", nil, errors.New("transient failure")
	}
	return "second attempt succeeded", "sess-retry-2", nil, nil
}

func TestExecutePhase_RetryPreservesParsedSkillsFromEarlierAttempts(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	wsPath := t.TempDir()
	ws := &core.Workspace{
		ID:     "ws-retry-skills",
		Path:   wsPath,
		Domain: "dev",
	}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil, "")
	retryExec := &retrySkillExecutor{}
	e.RegisterExecutor(core.Runtime("retry-tool-capture"), retryExec)

	phase := &core.Phase{
		ID:        "phase-1",
		Name:      "implement",
		Objective: "persist retry skill metrics",
		Persona:   "backend-engineer",
		ModelTier: "work",
		Runtime:   core.Runtime("retry-tool-capture"),
		Status:    core.StatusPending,
	}
	e.plan = &core.Plan{
		ID:           "plan-retry-skills",
		Task:         "persist retry skill metrics",
		DecompSource: core.DecompLLM,
		Phases:       []*core.Phase{phase},
	}
	e.phases[phase.ID] = phase
	e.startTime = time.Now().Add(-time.Minute)

	if _, err := e.executePhase(context.Background(), phase, ""); err != nil {
		t.Fatalf("executePhase: %v", err)
	}

	if retryExec.attempts != 2 {
		t.Fatalf("attempts = %d, want 2", retryExec.attempts)
	}
	if len(phase.ParsedSkills) != 1 || phase.ParsedSkills[0] != "scout" {
		t.Fatalf("phase.ParsedSkills = %v, want [scout]", phase.ParsedSkills)
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	usage, err := db.QuerySkillUsage(context.Background())
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 1 {
		t.Fatalf("want 1 skill row, got %d: %+v", len(usage), usage)
	}
	if usage[0].SkillName != "scout" || usage[0].Source != metricsdb.SkillSourceOutputParse {
		t.Fatalf("usage[0] = %+v, want scout/output_parse", usage[0])
	}
}

type retryDuplicateSkillExecutor struct {
	attempts int
}

func (e *retryDuplicateSkillExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	e.attempts++
	emitter.Emit(ctx, event.New(event.WorkerOutput, config.Bundle.WorkspaceID, config.Bundle.PhaseID, config.Name, map[string]any{
		"chunk":      `[tool: Bash scout gather "golang"]` + "\n",
		"event_kind": "tool_use",
		"tool_name":  "Bash",
		"streaming":  true,
	}))
	if e.attempts == 1 {
		return "first attempt failed", "sess-retry-dup-1", nil, errors.New("transient failure")
	}
	return "second attempt succeeded", "sess-retry-dup-2", nil, nil
}

func TestExecutePhase_RetryDoesNotDoubleCountRepeatedSkillsAcrossAttempts(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	wsPath := t.TempDir()
	ws := &core.Workspace{
		ID:     "ws-retry-dup-skills",
		Path:   wsPath,
		Domain: "dev",
	}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil, "")
	retryExec := &retryDuplicateSkillExecutor{}
	e.RegisterExecutor(core.Runtime("retry-dup-tool-capture"), retryExec)

	phase := &core.Phase{
		ID:        "phase-1",
		Name:      "implement",
		Objective: "dedupe retry skill metrics",
		Persona:   "backend-engineer",
		ModelTier: "work",
		Runtime:   core.Runtime("retry-dup-tool-capture"),
		Status:    core.StatusPending,
	}
	e.plan = &core.Plan{
		ID:           "plan-retry-dup-skills",
		Task:         "dedupe retry skill metrics",
		DecompSource: core.DecompLLM,
		Phases:       []*core.Phase{phase},
	}
	e.phases[phase.ID] = phase
	e.startTime = time.Now().Add(-time.Minute)

	if _, err := e.executePhase(context.Background(), phase, ""); err != nil {
		t.Fatalf("executePhase: %v", err)
	}

	if retryExec.attempts != 2 {
		t.Fatalf("attempts = %d, want 2", retryExec.attempts)
	}
	if len(phase.ParsedSkills) != 1 || phase.ParsedSkills[0] != "scout" {
		t.Fatalf("phase.ParsedSkills = %v, want [scout]", phase.ParsedSkills)
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	usage, err := db.QuerySkillUsage(context.Background())
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 1 {
		t.Fatalf("want 1 skill row, got %d: %+v", len(usage), usage)
	}
	if usage[0].SkillName != "scout" || usage[0].Invocations != 1 {
		t.Fatalf("usage[0] = %+v, want scout with 1 invocation", usage[0])
	}
}

type legacyPhaseIDExecutor struct{}

func (legacyPhaseIDExecutor) Execute(ctx context.Context, config *core.WorkerConfig, emitter event.Emitter, verbose bool) (string, string, *sdk.CostInfo, error) {
	emitter.Emit(ctx, event.New(event.WorkerOutput, config.Bundle.WorkspaceID, "", config.Name, map[string]any{
		"chunk":      `[tool: Bash scout gather "golang"]` + "\n",
		"event_kind": "tool_use",
		"tool_name":  "Bash",
		"streaming":  true,
	}))
	emitter.Emit(ctx, event.New(event.WorkerOutput, config.Bundle.WorkspaceID, config.Bundle.PhaseID, config.Name, map[string]any{
		"chunk":      `[tool: Bash scheduler post list]` + "\n",
		"event_kind": "tool_use",
		"tool_name":  "Bash",
		"streaming":  true,
	}))
	return "phase completed", "sess-legacy-phase-id", nil, nil
}

func TestExecutePhase_EmptyLegacyPhaseIDUsesStableFallbackForToolCapture(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	wsPath := t.TempDir()
	ws := &core.Workspace{
		ID:     "ws-empty-phase-id",
		Path:   wsPath,
		Domain: "dev",
	}
	e := New(ws, &core.OrchestratorConfig{}, nil, nil, "")
	e.RegisterExecutor(core.Runtime("legacy-phase-id-tool-capture"), legacyPhaseIDExecutor{})

	phase := &core.Phase{
		ID:        "",
		Name:      "implement",
		Objective: "capture phase-scoped tool output",
		Persona:   "backend-engineer",
		ModelTier: "work",
		Runtime:   core.Runtime("legacy-phase-id-tool-capture"),
		Status:    core.StatusPending,
	}
	e.plan = &core.Plan{
		ID:           "plan-empty-phase-id",
		Task:         "capture legacy phase ids",
		DecompSource: core.DecompLLM,
		Phases:       []*core.Phase{phase},
	}
	e.phases[phase.ID] = phase
	e.startTime = time.Now().Add(-time.Minute)

	if _, err := e.executePhase(context.Background(), phase, ""); err != nil {
		t.Fatalf("executePhase: %v", err)
	}

	if len(phase.ParsedSkills) != 1 || phase.ParsedSkills[0] != "scheduler" {
		t.Fatalf("phase.ParsedSkills = %v, want [scheduler]", phase.ParsedSkills)
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	usage, err := db.QuerySkillUsage(context.Background())
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 1 {
		t.Fatalf("want 1 skill row, got %d: %+v", len(usage), usage)
	}
	if usage[0].SkillName != "scheduler" {
		t.Fatalf("usage[0] = %+v, want scheduler only", usage[0])
	}
}

func TestRecordMetrics_StoresDeclaredSkillsAfterMissionInsert(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	now := time.Now().UTC().Truncate(time.Second)
	m := MissionMetrics{
		WorkspaceID: "ws-skill-recording",
		Domain:      "dev",
		Task:        "verify declared skill persistence",
		StartedAt:   now.Add(-time.Minute),
		FinishedAt:  now,
		DurationSec: 60,
		PhasesTotal: 1,
		Status:      "success",
		Phases: []PhaseMetric{
			{
				ID:        "phase-1",
				Name:      "implement",
				Persona:   "senior-backend-engineer",
				Skills:    []string{"golang-pro", "watermark"},
				Status:    "completed",
				DurationS: 60,
			},
		},
	}

	if err := RecordMetrics(m); err != nil {
		t.Fatalf("RecordMetrics: %v", err)
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	ctx := context.Background()
	usage, err := db.QuerySkillUsage(ctx)
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 skill rows, got %d: %+v", len(usage), usage)
	}

	got := make(map[string]metricsdb.SkillUsage, len(usage))
	for _, row := range usage {
		got[row.SkillName] = row
	}
	for _, skill := range []string{"golang-pro", "watermark"} {
		row, ok := got[skill]
		if !ok {
			t.Fatalf("missing skill row for %s", skill)
		}
		if row.Phase != "implement" {
			t.Errorf("%s phase = %q, want implement", skill, row.Phase)
		}
		if row.Persona != "senior-backend-engineer" {
			t.Errorf("%s persona = %q, want senior-backend-engineer", skill, row.Persona)
		}
		if row.Source != metricsdb.SkillSourceDeclared {
			t.Errorf("%s source = %q, want %q", skill, row.Source, metricsdb.SkillSourceDeclared)
		}
		if row.Invocations != 1 {
			t.Errorf("%s invocations = %d, want 1", skill, row.Invocations)
		}
	}

	jsonlPath := filepath.Join(cfgDir, "metrics.jsonl")
	if _, err := os.Stat(jsonlPath); err != nil {
		t.Fatalf("metrics.jsonl not written: %v", err)
	}
}

func TestRecordPhaseSkillsDB_FinalCompletedPhaseStoresSuccessStatus(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	now := time.Now().UTC().Truncate(time.Second)
	start := now.Add(-time.Minute)
	end := now
	phase := &core.Phase{
		ID:                     "phase-1",
		Name:                   "implement",
		Persona:                "backend-engineer",
		PersonaSelectionMethod: "llm",
		Status:                 core.StatusCompleted,
		StartTime:              &start,
		EndTime:                &end,
		OutputLen:              120,
		ParsedSkills:           []string{"scout"},
	}
	plan := &core.Plan{
		ID:           "plan-phase-snapshot-success",
		Task:         "verify snapshot status",
		DecompSource: core.DecompLLM,
		Phases:       []*core.Phase{phase},
	}
	ws := &core.Workspace{
		ID:     "ws-phase-snapshot-success",
		Path:   t.TempDir(),
		Domain: "dev",
	}

	if err := recordPhaseSkillsDB(ws, plan, phase, start); err != nil {
		t.Fatalf("recordPhaseSkillsDB: %v", err)
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	rows, err := db.QueryMissions(context.Background(), 10, "", 0, "", "")
	if err != nil {
		t.Fatalf("QueryMissions: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("want 1 mission row, got %d", len(rows))
	}
	if rows[0].Status != "success" {
		t.Fatalf("status = %q, want success", rows[0].Status)
	}
}

func TestRecordMetrics_BackfillsSkillsWhenMissionAlreadyExists(t *testing.T) {
	cfgDir := t.TempDir()
	t.Setenv("ORCHESTRATOR_CONFIG_DIR", cfgDir)

	now := time.Now().UTC().Truncate(time.Second)
	m := MissionMetrics{
		WorkspaceID: "ws-existing-mission",
		Domain:      "dev",
		Task:        "verify skill backfill on existing mission rows",
		StartedAt:   now.Add(-time.Minute),
		FinishedAt:  now,
		DurationSec: 60,
		PhasesTotal: 1,
		Status:      "success",
		Phases: []PhaseMetric{
			{
				ID:           "phase-1",
				Name:         "implement",
				Persona:      "senior-backend-engineer",
				Skills:       []string{"watermark"},
				ParsedSkills: []string{"scout"},
				Status:       "completed",
				DurationS:    60,
			},
		},
	}

	db, err := metricsdb.InitDB(filepath.Join(cfgDir, "metrics.db"))
	if err != nil {
		t.Fatalf("InitDB: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	ctx := context.Background()
	if err := db.RecordMission(ctx, toMissionRecord(m)); err != nil {
		t.Fatalf("RecordMission: %v", err)
	}

	if err := RecordMetrics(m); err != nil {
		t.Fatalf("RecordMetrics: %v", err)
	}

	usage, err := db.QuerySkillUsage(ctx)
	if err != nil {
		t.Fatalf("QuerySkillUsage: %v", err)
	}
	if len(usage) != 2 {
		t.Fatalf("want 2 skill rows, got %d: %+v", len(usage), usage)
	}

	bySkill := make(map[string]metricsdb.SkillUsage, len(usage))
	for _, row := range usage {
		bySkill[row.SkillName] = row
	}
	if bySkill["watermark"].Source != metricsdb.SkillSourceDeclared {
		t.Errorf("watermark source = %q, want %q", bySkill["watermark"].Source, metricsdb.SkillSourceDeclared)
	}
	if bySkill["scout"].Source != metricsdb.SkillSourceOutputParse {
		t.Errorf("scout source = %q, want %q", bySkill["scout"].Source, metricsdb.SkillSourceOutputParse)
	}
}
