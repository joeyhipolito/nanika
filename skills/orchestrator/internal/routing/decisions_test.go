package routing

import (
	"context"
	"path/filepath"
	"testing"
	"time"
)

// openTestDB opens a temporary routing DB for tests.
func openTestDB(t *testing.T) *RoutingDB {
	t.Helper()
	path := filepath.Join(t.TempDir(), "learnings.db")
	rdb, err := OpenDB(path)
	if err != nil {
		t.Fatalf("OpenDB: %v", err)
	}
	t.Cleanup(func() { rdb.Close() })
	return rdb
}

// ─── RecordRoutingDecision ────────────────────────────────────────────────────

func TestRecordRoutingDecision_InsertsRow(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	err := rdb.RecordRoutingDecision(ctx, RoutingDecision{
		MissionID:     "mission-1",
		PhaseID:       "phase-1",
		PhaseName:     "implement",
		Persona:       "senior-backend-engineer",
		Confidence:    0.8,
		RoutingMethod: "llm",
	})
	if err != nil {
		t.Fatalf("RecordRoutingDecision: %v", err)
	}

	// Verify the row is present via GetRecentPersonaFailures (outcome is pending,
	// not failure, so it won't appear there — use stats instead).
	stats, err := rdb.GetPersonaRoutingStats(ctx)
	if err != nil {
		t.Fatalf("GetPersonaRoutingStats: %v", err)
	}
	if len(stats) != 1 {
		t.Fatalf("len(stats) = %d, want 1", len(stats))
	}
	s := stats[0]
	if s.Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q, want senior-backend-engineer", s.Persona)
	}
	if s.Total != 1 {
		t.Errorf("Total = %d, want 1", s.Total)
	}
	if s.Pending != 1 {
		t.Errorf("Pending = %d, want 1 (outcome defaults to pending)", s.Pending)
	}
}

func TestRecordRoutingDecision_IdempotentOnDuplicate(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	d := RoutingDecision{
		MissionID: "mission-1",
		PhaseID:   "phase-1",
		Persona:   "golang-engineer",
	}
	if err := rdb.RecordRoutingDecision(ctx, d); err != nil {
		t.Fatalf("first insert: %v", err)
	}
	// Second insert for same (mission, phase) must be silently ignored.
	if err := rdb.RecordRoutingDecision(ctx, d); err != nil {
		t.Fatalf("duplicate insert: %v", err)
	}

	stats, err := rdb.GetPersonaRoutingStats(ctx)
	if err != nil {
		t.Fatalf("GetPersonaRoutingStats: %v", err)
	}
	if len(stats) != 1 || stats[0].Total != 1 {
		t.Errorf("expected 1 row, got %d rows with total=%d", len(stats), stats[0].Total)
	}
}

func TestRecordRoutingDecision_RejectsMissingFields(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	cases := []struct {
		name string
		d    RoutingDecision
	}{
		{"missing_mission_id", RoutingDecision{PhaseID: "p1", Persona: "x"}},
		{"missing_phase_id", RoutingDecision{MissionID: "m1", Persona: "x"}},
		{"missing_persona", RoutingDecision{MissionID: "m1", PhaseID: "p1"}},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			if err := rdb.RecordRoutingDecision(ctx, tc.d); err == nil {
				t.Errorf("expected error for %s, got nil", tc.name)
			}
		})
	}
}

// ─── UpdateRoutingOutcome ─────────────────────────────────────────────────────

func TestUpdateRoutingOutcome_SetsOutcomeAndReason(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordRoutingDecision(ctx, RoutingDecision{
		MissionID: "m1", PhaseID: "p1", Persona: "senior-backend-engineer",
	}); err != nil {
		t.Fatalf("RecordRoutingDecision: %v", err)
	}

	if err := rdb.UpdateRoutingOutcome(ctx, "m1", "p1", "failure", "context window exceeded"); err != nil {
		t.Fatalf("UpdateRoutingOutcome: %v", err)
	}

	stats, err := rdb.GetPersonaRoutingStats(ctx)
	if err != nil {
		t.Fatalf("GetPersonaRoutingStats: %v", err)
	}
	if len(stats) != 1 {
		t.Fatalf("len(stats) = %d, want 1", len(stats))
	}
	if stats[0].Failures != 1 {
		t.Errorf("Failures = %d, want 1", stats[0].Failures)
	}
	if stats[0].Pending != 0 {
		t.Errorf("Pending = %d, want 0 (should be resolved)", stats[0].Pending)
	}
}

func TestUpdateRoutingOutcome_SuccessPath(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordRoutingDecision(ctx, RoutingDecision{
		MissionID: "m1", PhaseID: "p1", Persona: "golang-engineer",
	}); err != nil {
		t.Fatalf("RecordRoutingDecision: %v", err)
	}
	if err := rdb.UpdateRoutingOutcome(ctx, "m1", "p1", "success", ""); err != nil {
		t.Fatalf("UpdateRoutingOutcome: %v", err)
	}

	stats, err := rdb.GetPersonaRoutingStats(ctx)
	if err != nil {
		t.Fatalf("GetPersonaRoutingStats: %v", err)
	}
	if stats[0].Successes != 1 || stats[0].Failures != 0 {
		t.Errorf("successes=%d failures=%d, want 1/0", stats[0].Successes, stats[0].Failures)
	}
	if stats[0].SuccessRate != 1.0 {
		t.Errorf("SuccessRate = %.2f, want 1.0", stats[0].SuccessRate)
	}
}

func TestUpdateRoutingOutcome_NoOpOnMissingRow(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()
	// No row inserted — update should be a no-op, not an error.
	if err := rdb.UpdateRoutingOutcome(ctx, "m1", "p1", "success", ""); err != nil {
		t.Fatalf("UpdateRoutingOutcome on missing row: %v", err)
	}
}

func TestUpdateRoutingOutcome_RejectsMissingFields(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	if err := rdb.UpdateRoutingOutcome(ctx, "", "p1", "success", ""); err == nil {
		t.Error("expected error for empty mission_id, got nil")
	}
	if err := rdb.UpdateRoutingOutcome(ctx, "m1", "", "success", ""); err == nil {
		t.Error("expected error for empty phase_id, got nil")
	}
}

// ─── GetPersonaRoutingStats ───────────────────────────────────────────────────

func TestGetPersonaRoutingStats_EmptyDB(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	stats, err := rdb.GetPersonaRoutingStats(ctx)
	if err != nil {
		t.Fatalf("GetPersonaRoutingStats: %v", err)
	}
	if len(stats) != 0 {
		t.Errorf("expected empty stats, got %d entries", len(stats))
	}
}

func TestGetPersonaRoutingStats_MultiplePersonas(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	decisions := []RoutingDecision{
		{MissionID: "m1", PhaseID: "p1", Persona: "senior-backend-engineer"},
		{MissionID: "m1", PhaseID: "p2", Persona: "senior-backend-engineer"},
		{MissionID: "m1", PhaseID: "p3", Persona: "golang-engineer"},
	}
	for _, d := range decisions {
		if err := rdb.RecordRoutingDecision(ctx, d); err != nil {
			t.Fatalf("RecordRoutingDecision: %v", err)
		}
	}
	_ = rdb.UpdateRoutingOutcome(ctx, "m1", "p1", "success", "")
	_ = rdb.UpdateRoutingOutcome(ctx, "m1", "p2", "failure", "timeout")
	_ = rdb.UpdateRoutingOutcome(ctx, "m1", "p3", "success", "")

	stats, err := rdb.GetPersonaRoutingStats(ctx)
	if err != nil {
		t.Fatalf("GetPersonaRoutingStats: %v", err)
	}
	if len(stats) != 2 {
		t.Fatalf("len(stats) = %d, want 2", len(stats))
	}

	// First entry should be the persona with more decisions (senior-backend-engineer = 2).
	s := stats[0]
	if s.Persona != "senior-backend-engineer" {
		t.Errorf("first persona = %q, want senior-backend-engineer", s.Persona)
	}
	if s.Total != 2 || s.Successes != 1 || s.Failures != 1 {
		t.Errorf("senior: total=%d successes=%d failures=%d, want 2/1/1", s.Total, s.Successes, s.Failures)
	}
	wantRate := 0.5
	if s.SuccessRate != wantRate {
		t.Errorf("SuccessRate = %.2f, want %.2f", s.SuccessRate, wantRate)
	}
}

// ─── GetRecentPersonaFailures ─────────────────────────────────────────────────

func TestGetRecentPersonaFailures_ReturnsFailuresOnly(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	decisions := []RoutingDecision{
		{MissionID: "m1", PhaseID: "p1", Persona: "senior-backend-engineer", PhaseName: "implement"},
		{MissionID: "m1", PhaseID: "p2", Persona: "senior-backend-engineer", PhaseName: "review"},
	}
	for _, d := range decisions {
		if err := rdb.RecordRoutingDecision(ctx, d); err != nil {
			t.Fatalf("RecordRoutingDecision: %v", err)
		}
	}
	_ = rdb.UpdateRoutingOutcome(ctx, "m1", "p1", "failure", "timeout")
	_ = rdb.UpdateRoutingOutcome(ctx, "m1", "p2", "success", "")

	failures, err := rdb.GetRecentPersonaFailures(ctx, "senior-backend-engineer", 30, 10)
	if err != nil {
		t.Fatalf("GetRecentPersonaFailures: %v", err)
	}
	if len(failures) != 1 {
		t.Fatalf("len(failures) = %d, want 1 (only failure rows)", len(failures))
	}
	if failures[0].PhaseName != "implement" {
		t.Errorf("PhaseName = %q, want implement", failures[0].PhaseName)
	}
	if failures[0].FailureReason != "timeout" {
		t.Errorf("FailureReason = %q, want timeout", failures[0].FailureReason)
	}
}

func TestGetRecentPersonaFailures_RespectsLimit(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	for i := 0; i < 5; i++ {
		phaseID := "p" + string(rune('0'+i))
		if err := rdb.RecordRoutingDecision(ctx, RoutingDecision{
			MissionID: "m1", PhaseID: phaseID, Persona: "golang-engineer",
		}); err != nil {
			t.Fatalf("RecordRoutingDecision: %v", err)
		}
		_ = rdb.UpdateRoutingOutcome(ctx, "m1", phaseID, "failure", "err")
	}

	failures, err := rdb.GetRecentPersonaFailures(ctx, "golang-engineer", 30, 3)
	if err != nil {
		t.Fatalf("GetRecentPersonaFailures: %v", err)
	}
	if len(failures) != 3 {
		t.Errorf("len(failures) = %d, want 3 (limit respected)", len(failures))
	}
}

func TestGetRecentPersonaFailures_RespectsLookbackWindow(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	// Insert a row with an old updated_at by manipulating created_at via direct SQL.
	// We insert via RecordRoutingDecision then update outcome + backdated timestamp.
	if err := rdb.RecordRoutingDecision(ctx, RoutingDecision{
		MissionID: "m1", PhaseID: "p1", Persona: "golang-engineer",
	}); err != nil {
		t.Fatalf("RecordRoutingDecision: %v", err)
	}
	// Backdating: set created_at to 60 days ago so a 30-day window misses it.
	old := time.Now().UTC().AddDate(0, 0, -60).Format(time.RFC3339)
	_, err := rdb.db.Exec(
		`UPDATE routing_decisions SET outcome='failure', failure_reason='old', created_at=? WHERE mission_id='m1' AND phase_id='p1'`,
		old,
	)
	if err != nil {
		t.Fatalf("backdating created_at: %v", err)
	}

	failures, err := rdb.GetRecentPersonaFailures(ctx, "golang-engineer", 30, 10)
	if err != nil {
		t.Fatalf("GetRecentPersonaFailures: %v", err)
	}
	if len(failures) != 0 {
		t.Errorf("expected 0 failures (outside 30-day window), got %d", len(failures))
	}

	// Within a 90-day window it should appear.
	failures, err = rdb.GetRecentPersonaFailures(ctx, "golang-engineer", 90, 10)
	if err != nil {
		t.Fatalf("GetRecentPersonaFailures (90d): %v", err)
	}
	if len(failures) != 1 {
		t.Errorf("expected 1 failure within 90-day window, got %d", len(failures))
	}
}

func TestGetRecentPersonaFailures_EmptyPersona(t *testing.T) {
	rdb := openTestDB(t)
	ctx := context.Background()

	_, err := rdb.GetRecentPersonaFailures(ctx, "", 30, 10)
	if err == nil {
		t.Error("expected error for empty persona, got nil")
	}
}
