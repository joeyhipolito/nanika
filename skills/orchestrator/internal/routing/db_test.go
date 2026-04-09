package routing

import (
	"context"
	"database/sql"
	"fmt"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/audit"
	_ "modernc.org/sqlite"
)

// newTestDB creates an in-memory RoutingDB for use in tests.
func newTestDB(t *testing.T) *RoutingDB {
	t.Helper()
	db, err := sql.Open("sqlite", ":memory:")
	if err != nil {
		t.Fatalf("open in-memory db: %v", err)
	}
	t.Cleanup(func() { db.Close() })

	rdb := &RoutingDB{db: db}
	if err := rdb.initSchema(); err != nil {
		t.Fatalf("initSchema: %v", err)
	}
	return rdb
}

// ─── TargetProfile ────────────────────────────────────────────────────────────

func TestUpsertAndGetTargetProfile(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	p := TargetProfile{
		TargetID:         "repo:~/skills/orchestrator",
		TargetType:       "repo",
		Language:         "go",
		Runtime:          "go",
		PreferredPersonas: []string{"senior-backend-engineer", "staff-code-reviewer"},
		Notes:            "primary orchestrator repo",
	}
	if err := rdb.UpsertTargetProfile(ctx, p); err != nil {
		t.Fatalf("UpsertTargetProfile: %v", err)
	}

	got, err := rdb.GetTargetProfile(ctx, p.TargetID)
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if got == nil {
		t.Fatal("GetTargetProfile returned nil, want profile")
	}
	if got.TargetID != p.TargetID {
		t.Errorf("TargetID = %q, want %q", got.TargetID, p.TargetID)
	}
	if got.Language != "go" {
		t.Errorf("Language = %q, want %q", got.Language, "go")
	}
	if got.Runtime != "go" {
		t.Errorf("Runtime = %q, want %q", got.Runtime, "go")
	}
	if len(got.PreferredPersonas) != 2 || got.PreferredPersonas[0] != "senior-backend-engineer" {
		t.Errorf("PreferredPersonas = %v, want [senior-backend-engineer staff-code-reviewer]", got.PreferredPersonas)
	}
	if got.Notes != p.Notes {
		t.Errorf("Notes = %q, want %q", got.Notes, p.Notes)
	}
	if got.CreatedAt.IsZero() {
		t.Error("CreatedAt is zero")
	}
	if got.UpdatedAt.IsZero() {
		t.Error("UpdatedAt is zero")
	}
}

func TestGetTargetProfile_NotFound(t *testing.T) {
	rdb := newTestDB(t)
	got, err := rdb.GetTargetProfile(context.Background(), "nonexistent")
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if got != nil {
		t.Errorf("GetTargetProfile = %+v, want nil", got)
	}
}

func TestUpsertTargetProfile_Overwrite(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	first := TargetProfile{
		TargetID: "system:orchestrator",
		Language: "go",
		Notes:    "original",
	}
	if err := rdb.UpsertTargetProfile(ctx, first); err != nil {
		t.Fatalf("first upsert: %v", err)
	}

	second := TargetProfile{
		TargetID: "system:orchestrator",
		Language: "go",
		Notes:    "updated",
		PreferredPersonas: []string{"senior-backend-engineer"},
	}
	if err := rdb.UpsertTargetProfile(ctx, second); err != nil {
		t.Fatalf("second upsert: %v", err)
	}

	got, err := rdb.GetTargetProfile(ctx, "system:orchestrator")
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if got.Notes != "updated" {
		t.Errorf("Notes = %q, want %q", got.Notes, "updated")
	}
	if len(got.PreferredPersonas) != 1 || got.PreferredPersonas[0] != "senior-backend-engineer" {
		t.Errorf("PreferredPersonas = %v after overwrite", got.PreferredPersonas)
	}
}

func TestUpsertTargetProfile_EmptyPersonas(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	p := TargetProfile{
		TargetID:   "vault",
		TargetType: "vault",
	}
	if err := rdb.UpsertTargetProfile(ctx, p); err != nil {
		t.Fatalf("UpsertTargetProfile: %v", err)
	}
	got, err := rdb.GetTargetProfile(ctx, "vault")
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if len(got.PreferredPersonas) != 0 {
		t.Errorf("PreferredPersonas = %v, want []", got.PreferredPersonas)
	}
}

func TestUpsertTargetProfile_EmptyID(t *testing.T) {
	rdb := newTestDB(t)
	err := rdb.UpsertTargetProfile(context.Background(), TargetProfile{})
	if err == nil {
		t.Error("expected error for empty target_id, got nil")
	}
}

// ─── RoutingPattern ───────────────────────────────────────────────────────────

func TestRecordRoutingPattern_FirstObservation(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordRoutingPattern(ctx, "repo:~/myapp", "senior-backend-engineer", ""); err != nil {
		t.Fatalf("RecordRoutingPattern: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	p := patterns[0]
	if p.Persona != "senior-backend-engineer" {
		t.Errorf("Persona = %q, want %q", p.Persona, "senior-backend-engineer")
	}
	if p.SeenCount != 1 {
		t.Errorf("SeenCount = %d, want 1", p.SeenCount)
	}
	if p.Confidence != 0.2 {
		t.Errorf("Confidence = %.2f, want 0.20", p.Confidence)
	}
}

func TestRecordRoutingPattern_ConfidenceGrows(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"
	persona := "senior-backend-engineer"

	// Record 3 observations.
	for i := 0; i < 3; i++ {
		if err := rdb.RecordRoutingPattern(ctx, target, persona, ""); err != nil {
			t.Fatalf("RecordRoutingPattern[%d]: %v", i, err)
		}
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	p := patterns[0]
	if p.SeenCount != 3 {
		t.Errorf("SeenCount = %d, want 3", p.SeenCount)
	}
	// confidence = min(1.0, 3 * 0.2) = 0.6
	if !approxEqual(p.Confidence, 0.6) {
		t.Errorf("Confidence = %.4f, want 0.6000", p.Confidence)
	}
}

func TestRecordRoutingPattern_ConfidenceCap(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"
	persona := "senior-backend-engineer"

	// 7 observations should cap at 1.0 (not exceed it).
	for i := 0; i < 7; i++ {
		if err := rdb.RecordRoutingPattern(ctx, target, persona, ""); err != nil {
			t.Fatalf("RecordRoutingPattern[%d]: %v", i, err)
		}
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].Confidence > 1.0 {
		t.Errorf("Confidence = %.4f exceeds 1.0", patterns[0].Confidence)
	}
	if patterns[0].Confidence != 1.0 {
		t.Errorf("Confidence = %.4f, want 1.0 after 7 observations", patterns[0].Confidence)
	}
}

func TestGetRoutingPatterns_Empty(t *testing.T) {
	rdb := newTestDB(t)
	patterns, err := rdb.GetRoutingPatterns(context.Background(), "nonexistent")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("len(patterns) = %d, want 0", len(patterns))
	}
}

func TestGetRoutingPatterns_OrderedByConfidence(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	// persona-a: 3 observations → confidence 0.6
	for i := 0; i < 3; i++ {
		if err := rdb.RecordRoutingPattern(ctx, target, "persona-a", ""); err != nil {
			t.Fatalf("RecordRoutingPattern persona-a[%d]: %v", i, err)
		}
	}
	// persona-b: 1 observation → confidence 0.2
	if err := rdb.RecordRoutingPattern(ctx, target, "persona-b", ""); err != nil {
		t.Fatalf("RecordRoutingPattern persona-b: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2", len(patterns))
	}
	if patterns[0].Persona != "persona-a" {
		t.Errorf("patterns[0].Persona = %q, want persona-a (highest confidence first)", patterns[0].Persona)
	}
	if patterns[1].Persona != "persona-b" {
		t.Errorf("patterns[1].Persona = %q, want persona-b", patterns[1].Persona)
	}
}

func TestGetRoutingPatterns_IsolatedByTarget(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordRoutingPattern(ctx, "target-a", "persona-x", ""); err != nil {
		t.Fatalf("RecordRoutingPattern target-a: %v", err)
	}
	if err := rdb.RecordRoutingPattern(ctx, "target-b", "persona-y", ""); err != nil {
		t.Fatalf("RecordRoutingPattern target-b: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, "target-a")
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	if len(patterns) != 1 || patterns[0].Persona != "persona-x" {
		t.Errorf("GetRoutingPatterns(target-a) = %v, want [persona-x]", patterns)
	}
}

func TestRecordRoutingPattern_TaskHintDistinct(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	if err := rdb.RecordRoutingPattern(ctx, target, "senior-backend-engineer", "implementation"); err != nil {
		t.Fatalf("hint=implementation: %v", err)
	}
	if err := rdb.RecordRoutingPattern(ctx, target, "senior-backend-engineer", "review"); err != nil {
		t.Fatalf("hint=review: %v", err)
	}

	patterns, err := rdb.GetRoutingPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingPatterns: %v", err)
	}
	// Different task_hints → two separate rows.
	if len(patterns) != 2 {
		t.Errorf("len(patterns) = %d, want 2 (task_hint is part of the key)", len(patterns))
	}
}

func TestRecordRoutingPattern_ValidationErrors(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordRoutingPattern(ctx, "", "persona", ""); err == nil {
		t.Error("expected error for empty target_id")
	}
	if err := rdb.RecordRoutingPattern(ctx, "target", "", ""); err == nil {
		t.Error("expected error for empty persona")
	}
}

// ─── HandoffPattern ───────────────────────────────────────────────────────────

func TestRecordHandoffPattern_FirstObservation(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	err := rdb.RecordHandoffPattern(ctx, "repo:~/myapp", "senior-backend-engineer", "staff-code-reviewer", "")
	if err != nil {
		t.Fatalf("RecordHandoffPattern: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, "repo:~/myapp")
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	p := patterns[0]
	if p.FromPersona != "senior-backend-engineer" {
		t.Errorf("FromPersona = %q, want senior-backend-engineer", p.FromPersona)
	}
	if p.ToPersona != "staff-code-reviewer" {
		t.Errorf("ToPersona = %q, want staff-code-reviewer", p.ToPersona)
	}
	if p.SeenCount != 1 {
		t.Errorf("SeenCount = %d, want 1", p.SeenCount)
	}
	if p.Confidence != 0.2 {
		t.Errorf("Confidence = %.2f, want 0.20", p.Confidence)
	}
}

func TestRecordHandoffPattern_ConfidenceGrows(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	for i := 0; i < 4; i++ {
		if err := rdb.RecordHandoffPattern(ctx, target, "from", "to", ""); err != nil {
			t.Fatalf("RecordHandoffPattern[%d]: %v", i, err)
		}
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	// confidence = min(1.0, 4 * 0.2) = 0.8
	if !approxEqual(patterns[0].Confidence, 0.8) {
		t.Errorf("Confidence = %.4f, want 0.8000", patterns[0].Confidence)
	}
}

func TestGetHandoffPatterns_OrderedByConfidence(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	// pair A: 5 obs → confidence 1.0
	for i := 0; i < 5; i++ {
		if err := rdb.RecordHandoffPattern(ctx, target, "backend", "reviewer", ""); err != nil {
			t.Fatalf("pair-A[%d]: %v", i, err)
		}
	}
	// pair B: 1 obs → confidence 0.2
	if err := rdb.RecordHandoffPattern(ctx, target, "backend", "qa", ""); err != nil {
		t.Fatalf("pair-B: %v", err)
	}

	patterns, err := rdb.GetHandoffPatterns(ctx, target)
	if err != nil {
		t.Fatalf("GetHandoffPatterns: %v", err)
	}
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2", len(patterns))
	}
	if patterns[0].ToPersona != "reviewer" {
		t.Errorf("patterns[0].ToPersona = %q, want reviewer (highest confidence first)", patterns[0].ToPersona)
	}
}

func TestRecordHandoffPattern_ValidationErrors(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordHandoffPattern(ctx, "", "from", "to", ""); err == nil {
		t.Error("expected error for empty target_id")
	}
	if err := rdb.RecordHandoffPattern(ctx, "target", "", "to", ""); err == nil {
		t.Error("expected error for empty from_persona")
	}
	if err := rdb.RecordHandoffPattern(ctx, "target", "from", "", ""); err == nil {
		t.Error("expected error for empty to_persona")
	}
}

// ─── RoutingCorrection ────────────────────────────────────────────────────────

func TestInsertRoutingCorrection_Manual(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	c := RoutingCorrection{
		TargetID:        "repo:~/myapp",
		AssignedPersona: "senior-frontend-engineer",
		IdealPersona:    "senior-backend-engineer",
		TaskHint:        "fix database query",
		Source:          SourceManual,
	}
	if err := rdb.InsertRoutingCorrection(ctx, c); err != nil {
		t.Fatalf("InsertRoutingCorrection: %v", err)
	}

	corrections, err := rdb.GetRoutingCorrections(ctx, c.TargetID)
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(corrections) != 1 {
		t.Fatalf("len(corrections) = %d, want 1", len(corrections))
	}
	got := corrections[0]
	if got.ID == 0 {
		t.Error("ID is 0, want auto-assigned integer")
	}
	if got.AssignedPersona != "senior-frontend-engineer" {
		t.Errorf("AssignedPersona = %q", got.AssignedPersona)
	}
	if got.IdealPersona != "senior-backend-engineer" {
		t.Errorf("IdealPersona = %q", got.IdealPersona)
	}
	if got.TaskHint != "fix database query" {
		t.Errorf("TaskHint = %q", got.TaskHint)
	}
	if got.Source != SourceManual {
		t.Errorf("Source = %q, want manual", got.Source)
	}
	if got.CreatedAt.IsZero() {
		t.Error("CreatedAt is zero")
	}
}

func TestInsertRoutingCorrection_AuditSource(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	c := RoutingCorrection{
		TargetID:        "repo:~/myapp",
		AssignedPersona: "wrong",
		IdealPersona:    "right",
		Source:          SourceAudit,
	}
	if err := rdb.InsertRoutingCorrection(ctx, c); err != nil {
		t.Fatalf("InsertRoutingCorrection: %v", err)
	}

	corrections, err := rdb.GetRoutingCorrections(ctx, c.TargetID)
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if corrections[0].Source != SourceAudit {
		t.Errorf("Source = %q, want audit", corrections[0].Source)
	}
}

func TestInsertRoutingCorrection_DefaultSource(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	c := RoutingCorrection{
		TargetID:        "repo:~/myapp",
		AssignedPersona: "wrong",
		IdealPersona:    "right",
		// Source intentionally empty — should default to "manual".
	}
	if err := rdb.InsertRoutingCorrection(ctx, c); err != nil {
		t.Fatalf("InsertRoutingCorrection: %v", err)
	}

	corrections, err := rdb.GetRoutingCorrections(ctx, c.TargetID)
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if corrections[0].Source != SourceManual {
		t.Errorf("Source = %q, want %q (default)", corrections[0].Source, SourceManual)
	}
}

func TestGetRoutingCorrections_NewestFirst(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	for _, hint := range []string{"first", "second", "third"} {
		c := RoutingCorrection{
			TargetID:        target,
			AssignedPersona: "wrong",
			IdealPersona:    "right",
			TaskHint:        hint,
		}
		if err := rdb.InsertRoutingCorrection(ctx, c); err != nil {
			t.Fatalf("InsertRoutingCorrection(%q): %v", hint, err)
		}
	}

	corrections, err := rdb.GetRoutingCorrections(ctx, target)
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(corrections) != 3 {
		t.Fatalf("len(corrections) = %d, want 3", len(corrections))
	}
	// Newest-first: IDs should be descending.
	if corrections[0].ID < corrections[1].ID {
		t.Errorf("corrections[0].ID=%d < corrections[1].ID=%d; want newest first",
			corrections[0].ID, corrections[1].ID)
	}
}

func TestGetRoutingCorrections_Empty(t *testing.T) {
	rdb := newTestDB(t)
	corrections, err := rdb.GetRoutingCorrections(context.Background(), "nonexistent")
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(corrections) != 0 {
		t.Errorf("len(corrections) = %d, want 0", len(corrections))
	}
}

func TestInsertRoutingCorrection_ValidationErrors(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.InsertRoutingCorrection(ctx, RoutingCorrection{
		AssignedPersona: "a", IdealPersona: "b",
	}); err == nil {
		t.Error("expected error for empty target_id")
	}
	if err := rdb.InsertRoutingCorrection(ctx, RoutingCorrection{
		TargetID: "t", IdealPersona: "b",
	}); err == nil {
		t.Error("expected error for empty assigned_persona")
	}
	if err := rdb.InsertRoutingCorrection(ctx, RoutingCorrection{
		TargetID: "t", AssignedPersona: "a",
	}); err == nil {
		t.Error("expected error for empty ideal_persona")
	}
}

// ─── InsertRoutingCorrections (batch) ─────────────────────────────────────────

func TestInsertRoutingCorrections_Batch(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	corrections := []RoutingCorrection{
		{TargetID: "t1", AssignedPersona: "wrong-a", IdealPersona: "right-a", Source: SourceAudit},
		{TargetID: "t1", AssignedPersona: "wrong-b", IdealPersona: "right-b", Source: SourceAudit},
	}
	n, err := rdb.InsertRoutingCorrections(ctx, corrections)
	if err != nil {
		t.Fatalf("InsertRoutingCorrections: %v", err)
	}
	if n != 2 {
		t.Errorf("inserted = %d, want 2", n)
	}

	got, err := rdb.GetRoutingCorrections(ctx, "t1")
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(got) != 2 {
		t.Errorf("len(got) = %d, want 2", len(got))
	}
}

func TestInsertRoutingCorrections_Idempotent(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	corrections := []RoutingCorrection{
		{TargetID: "t1", AssignedPersona: "wrong", IdealPersona: "right", Source: SourceAudit, TaskHint: "test"},
	}

	n1, err := rdb.InsertRoutingCorrections(ctx, corrections)
	if err != nil {
		t.Fatalf("first insert: %v", err)
	}
	if n1 != 1 {
		t.Errorf("first inserted = %d, want 1", n1)
	}

	// Second insert of the same data should be a no-op (INSERT OR IGNORE).
	n2, err := rdb.InsertRoutingCorrections(ctx, corrections)
	if err != nil {
		t.Fatalf("second insert: %v", err)
	}
	if n2 != 0 {
		t.Errorf("second inserted = %d, want 0 (duplicate)", n2)
	}

	got, err := rdb.GetRoutingCorrections(ctx, "t1")
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(got) != 1 {
		t.Errorf("len(got) = %d, want 1 (no duplicates)", len(got))
	}
}

func TestInsertRoutingCorrections_SkipsInvalid(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	corrections := []RoutingCorrection{
		{TargetID: "", AssignedPersona: "a", IdealPersona: "b"},     // invalid: no target
		{TargetID: "t1", AssignedPersona: "", IdealPersona: "b"},    // invalid: no assigned
		{TargetID: "t1", AssignedPersona: "a", IdealPersona: "b"},   // valid
	}
	n, err := rdb.InsertRoutingCorrections(ctx, corrections)
	if err != nil {
		t.Fatalf("InsertRoutingCorrections: %v", err)
	}
	if n != 1 {
		t.Errorf("inserted = %d, want 1 (2 invalid skipped)", n)
	}
}

func TestInsertRoutingCorrections_Empty(t *testing.T) {
	rdb := newTestDB(t)
	n, err := rdb.InsertRoutingCorrections(context.Background(), nil)
	if err != nil {
		t.Fatalf("InsertRoutingCorrections(nil): %v", err)
	}
	if n != 0 {
		t.Errorf("inserted = %d, want 0", n)
	}
}

// Also verify single InsertRoutingCorrection is idempotent with the dedup index.
func TestInsertRoutingCorrection_Idempotent(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	c := RoutingCorrection{
		TargetID:        "t1",
		AssignedPersona: "wrong",
		IdealPersona:    "right",
		Source:          SourceManual,
		TaskHint:        "fix bug",
	}
	if err := rdb.InsertRoutingCorrection(ctx, c); err != nil {
		t.Fatalf("first insert: %v", err)
	}
	// Second insert should silently skip (INSERT OR IGNORE).
	if err := rdb.InsertRoutingCorrection(ctx, c); err != nil {
		t.Fatalf("second insert: %v", err)
	}

	got, err := rdb.GetRoutingCorrections(ctx, "t1")
	if err != nil {
		t.Fatalf("GetRoutingCorrections: %v", err)
	}
	if len(got) != 1 {
		t.Errorf("len(got) = %d, want 1 (dedup)", len(got))
	}
}

// ─── Seed ─────────────────────────────────────────────────────────────────────

func TestSeed_DefaultProfiles(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	profiles := DefaultSeeds()
	n, err := Seed(ctx, rdb, profiles)
	if err != nil {
		t.Fatalf("Seed: %v", err)
	}
	if n != len(profiles) {
		t.Errorf("Seed returned %d, want %d", n, len(profiles))
	}

	// Verify the orchestrator profile was inserted correctly.
	got, err := rdb.GetTargetProfile(ctx, "repo:~/nanika/skills/orchestrator")
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if got == nil {
		t.Fatal("GetTargetProfile returned nil after seed")
	}
	if got.Language != "go" {
		t.Errorf("Language = %q, want %q", got.Language, "go")
	}
	if got.Runtime != "go" {
		t.Errorf("Runtime = %q, want %q", got.Runtime, "go")
	}
	if got.TargetType != TargetTypeRepo {
		t.Errorf("TargetType = %q, want %q", got.TargetType, TargetTypeRepo)
	}
	if len(got.PreferredPersonas) != 3 {
		t.Fatalf("PreferredPersonas len = %d, want 3", len(got.PreferredPersonas))
	}
	wantPersonas := []string{"senior-backend-engineer", "staff-code-reviewer", "security-auditor"}
	for i, want := range wantPersonas {
		if got.PreferredPersonas[i] != want {
			t.Errorf("PreferredPersonas[%d] = %q, want %q", i, got.PreferredPersonas[i], want)
		}
	}
}

func TestSeed_Idempotent(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	profiles := DefaultSeeds()

	// Seed twice — second call should overwrite, not fail.
	n1, err := Seed(ctx, rdb, profiles)
	if err != nil {
		t.Fatalf("first Seed: %v", err)
	}
	n2, err := Seed(ctx, rdb, profiles)
	if err != nil {
		t.Fatalf("second Seed: %v", err)
	}
	if n1 != n2 {
		t.Errorf("first=%d, second=%d; want equal counts", n1, n2)
	}

	// Verify profile still correct after overwrite.
	got, err := rdb.GetTargetProfile(ctx, "repo:~/nanika/skills/orchestrator")
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if got == nil {
		t.Fatal("profile nil after second seed")
	}
	if got.Language != "go" {
		t.Errorf("Language = %q after double-seed", got.Language)
	}
}

func TestSeed_EmptySlice(t *testing.T) {
	rdb := newTestDB(t)
	n, err := Seed(context.Background(), rdb, nil)
	if err != nil {
		t.Fatalf("Seed(nil): %v", err)
	}
	if n != 0 {
		t.Errorf("Seed(nil) = %d, want 0", n)
	}
}

func TestSeed_SkipsEmptyTargetID(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	profiles := []TargetProfile{
		{TargetID: "", Language: "go"}, // invalid
		{TargetID: "repo:~/valid", Language: "rust", TargetType: TargetTypeRepo},
	}
	n, err := Seed(ctx, rdb, profiles)
	if err == nil {
		t.Error("expected error for empty target_id, got nil")
	}
	if n != 1 {
		t.Errorf("Seed returned %d, want 1 (valid profile only)", n)
	}

	// The valid one should still be present.
	got, err := rdb.GetTargetProfile(ctx, "repo:~/valid")
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if got == nil {
		t.Fatal("valid profile not found after partial seed")
	}
	if got.Language != "rust" {
		t.Errorf("Language = %q, want rust", got.Language)
	}
}

func TestSeed_CustomProfiles(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	custom := []TargetProfile{
		{
			TargetID:          "system:via/orchestrator",
			TargetType:        TargetTypeViaSystem,
			Language:          "go",
			Runtime:           "go",
			PreferredPersonas: []string{"senior-backend-engineer"},
			Notes:             "system target",
		},
	}
	n, err := Seed(ctx, rdb, custom)
	if err != nil {
		t.Fatalf("Seed: %v", err)
	}
	if n != 1 {
		t.Errorf("Seed returned %d, want 1", n)
	}

	got, err := rdb.GetTargetProfile(ctx, "system:via/orchestrator")
	if err != nil {
		t.Fatalf("GetTargetProfile: %v", err)
	}
	if got == nil {
		t.Fatal("custom profile not found")
	}
	if got.TargetType != TargetTypeViaSystem {
		t.Errorf("TargetType = %q, want %q", got.TargetType, TargetTypeViaSystem)
	}
	if got.Notes != "system target" {
		t.Errorf("Notes = %q, want %q", got.Notes, "system target")
	}
}

// ─── DecompExample ────────────────────────────────────────────────────────────

func TestInsertDecompExample_Basic(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	ex := DecompExample{
		TargetID:      "repo:~/skills/orchestrator",
		WorkspaceID:   "ws-001",
		TaskSummary:   "implement user auth system",
		PhaseCount:    3,
		ExecutionMode: "sequential",
		PhasesJSON:    `[{"name":"research","persona":"architect"},{"name":"implement","persona":"senior-backend-engineer"},{"name":"review","persona":"staff-code-reviewer"}]`,
		DecompSource:  "predecomposed",
		AuditScore:    4,
		DecompQuality: 4,
		PersonaFit:    5,
	}
	if err := rdb.InsertDecompExample(ctx, ex); err != nil {
		t.Fatalf("InsertDecompExample: %v", err)
	}

	examples, err := rdb.GetDecompExamples(ctx, ex.TargetID, 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples: %v", err)
	}
	if len(examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1", len(examples))
	}
	got := examples[0]
	if got.TaskSummary != "implement user auth system" {
		t.Errorf("TaskSummary = %q", got.TaskSummary)
	}
	if got.PhaseCount != 3 {
		t.Errorf("PhaseCount = %d, want 3", got.PhaseCount)
	}
	if got.DecompSource != "predecomposed" {
		t.Errorf("DecompSource = %q, want predecomposed", got.DecompSource)
	}
	if got.AuditScore != 4 {
		t.Errorf("AuditScore = %d, want 4", got.AuditScore)
	}
}

func TestInsertDecompExample_UpsertOverwrites(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	first := DecompExample{
		TargetID:      "repo:~/app",
		WorkspaceID:   "ws-001",
		TaskSummary:   "original task",
		PhaseCount:    2,
		ExecutionMode: "sequential",
		PhasesJSON:    `[{"name":"a","persona":"x"}]`,
		DecompSource:  "llm",
		AuditScore:    3,
		DecompQuality: 3,
	}
	if err := rdb.InsertDecompExample(ctx, first); err != nil {
		t.Fatalf("first insert: %v", err)
	}

	// Re-audit with improved scores.
	second := first
	second.AuditScore = 5
	second.DecompQuality = 4
	second.TaskSummary = "updated task"
	if err := rdb.InsertDecompExample(ctx, second); err != nil {
		t.Fatalf("second insert: %v", err)
	}

	examples, err := rdb.GetDecompExamples(ctx, "repo:~/app", 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples: %v", err)
	}
	if len(examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1 (upsert)", len(examples))
	}
	if examples[0].AuditScore != 5 {
		t.Errorf("AuditScore = %d, want 5 (updated)", examples[0].AuditScore)
	}
	if examples[0].TaskSummary != "updated task" {
		t.Errorf("TaskSummary = %q, want updated task", examples[0].TaskSummary)
	}
}

func TestInsertDecompExample_UpsertPreservesOriginalRecency(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	older := DecompExample{
		TargetID:      target,
		WorkspaceID:   "ws-older",
		TaskSummary:   "older",
		PhaseCount:    2,
		ExecutionMode: "sequential",
		PhasesJSON:    `[{}]`,
		DecompSource:  "llm",
		AuditScore:    4,
		DecompQuality: 4,
	}
	if err := rdb.InsertDecompExample(ctx, older); err != nil {
		t.Fatalf("insert older: %v", err)
	}

	time.Sleep(1100 * time.Millisecond)

	newer := DecompExample{
		TargetID:      target,
		WorkspaceID:   "ws-newer",
		TaskSummary:   "newer",
		PhaseCount:    2,
		ExecutionMode: "sequential",
		PhasesJSON:    `[{}]`,
		DecompSource:  "llm",
		AuditScore:    4,
		DecompQuality: 4,
	}
	if err := rdb.InsertDecompExample(ctx, newer); err != nil {
		t.Fatalf("insert newer: %v", err)
	}

	examples, err := rdb.GetDecompExamples(ctx, target, 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples before upsert: %v", err)
	}
	if len(examples) != 2 {
		t.Fatalf("len(examples) = %d, want 2", len(examples))
	}
	if examples[0].WorkspaceID != "ws-newer" {
		t.Fatalf("before upsert examples[0].WorkspaceID = %q, want ws-newer", examples[0].WorkspaceID)
	}

	time.Sleep(1100 * time.Millisecond)

	older.TaskSummary = "older-updated"
	if err := rdb.InsertDecompExample(ctx, older); err != nil {
		t.Fatalf("upsert older: %v", err)
	}

	examples, err = rdb.GetDecompExamples(ctx, target, 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples after upsert: %v", err)
	}
	if len(examples) != 2 {
		t.Fatalf("len(examples) after upsert = %d, want 2", len(examples))
	}
	if examples[0].WorkspaceID != "ws-newer" {
		t.Fatalf("after upsert examples[0].WorkspaceID = %q, want ws-newer", examples[0].WorkspaceID)
	}
	if examples[1].TaskSummary != "older-updated" {
		t.Errorf("examples[1].TaskSummary = %q, want older-updated", examples[1].TaskSummary)
	}
}

func TestGetDecompExamples_ScoreGate(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Insert examples with varying scores.
	for _, ex := range []DecompExample{
		{TargetID: target, WorkspaceID: "ws-high", TaskSummary: "high", PhaseCount: 3, ExecutionMode: "sequential", PhasesJSON: `[{}]`, DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
		{TargetID: target, WorkspaceID: "ws-low-audit", TaskSummary: "low-audit", PhaseCount: 2, ExecutionMode: "sequential", PhasesJSON: `[{}]`, DecompSource: "llm", AuditScore: 2, DecompQuality: 4},
		{TargetID: target, WorkspaceID: "ws-low-decomp", TaskSummary: "low-decomp", PhaseCount: 2, ExecutionMode: "sequential", PhasesJSON: `[{}]`, DecompSource: "llm", AuditScore: 4, DecompQuality: 2},
		{TargetID: target, WorkspaceID: "ws-borderline", TaskSummary: "borderline", PhaseCount: 1, ExecutionMode: "sequential", PhasesJSON: `[{}]`, DecompSource: "llm", AuditScore: 3, DecompQuality: 3},
	} {
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			t.Fatalf("insert %s: %v", ex.WorkspaceID, err)
		}
	}

	// Score gate = 3: should return ws-high and ws-borderline.
	examples, err := rdb.GetDecompExamples(ctx, target, 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples: %v", err)
	}
	if len(examples) != 2 {
		t.Fatalf("len(examples) = %d, want 2 (score gate filters out low-audit and low-decomp)", len(examples))
	}
	// Highest score first.
	if examples[0].AuditScore != 4 {
		t.Errorf("examples[0].AuditScore = %d, want 4 (highest first)", examples[0].AuditScore)
	}
}

func TestGetDecompExamples_TargetIsolation(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	for _, ex := range []DecompExample{
		{TargetID: "repo:~/a", WorkspaceID: "ws-1", TaskSummary: "task a", PhaseCount: 1, ExecutionMode: "sequential", PhasesJSON: `[{}]`, DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
		{TargetID: "repo:~/b", WorkspaceID: "ws-2", TaskSummary: "task b", PhaseCount: 1, ExecutionMode: "sequential", PhasesJSON: `[{}]`, DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
	} {
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			t.Fatalf("insert: %v", err)
		}
	}

	examples, err := rdb.GetDecompExamples(ctx, "repo:~/a", 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples: %v", err)
	}
	if len(examples) != 1 {
		t.Fatalf("len(examples) = %d, want 1 (target isolation)", len(examples))
	}
	if examples[0].TaskSummary != "task a" {
		t.Errorf("got task from wrong target: %q", examples[0].TaskSummary)
	}
}

func TestInsertDecompExample_ValidationErrors(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.InsertDecompExample(ctx, DecompExample{WorkspaceID: "ws", PhasesJSON: "[]"}); err == nil {
		t.Error("expected error for empty target_id")
	}
	if err := rdb.InsertDecompExample(ctx, DecompExample{TargetID: "t", PhasesJSON: "[]"}); err == nil {
		t.Error("expected error for empty workspace_id")
	}
	if err := rdb.InsertDecompExample(ctx, DecompExample{TargetID: "t", WorkspaceID: "ws"}); err == nil {
		t.Error("expected error for empty phases_json")
	}
}

func TestGetDecompExamples_LimitCap(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Insert 5 examples.
	for i := 0; i < 5; i++ {
		ex := DecompExample{
			TargetID:      target,
			WorkspaceID:   fmt.Sprintf("ws-%d", i),
			TaskSummary:   fmt.Sprintf("task %d", i),
			PhaseCount:    2,
			ExecutionMode: "sequential",
			PhasesJSON:    `[{}]`,
			DecompSource:  "llm",
			AuditScore:    4,
			DecompQuality: 4,
		}
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			t.Fatalf("insert %d: %v", i, err)
		}
	}

	// Default limit (0) should return maxDecompExamples=3.
	examples, err := rdb.GetDecompExamples(ctx, target, 3, 0)
	if err != nil {
		t.Fatalf("GetDecompExamples: %v", err)
	}
	if len(examples) != 3 {
		t.Errorf("len(examples) = %d, want 3 (capped at maxDecompExamples)", len(examples))
	}
}

// ─── GetDecompFindingCounts ──────────────────────────────────────────────────

func TestGetDecompFindingCounts_Basic(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	rows := []DecompFindingRow{
		{TargetID: target, WorkspaceID: "ws-1", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingMissingPhase, Detail: "test", AuditScore: 4}},
		{TargetID: target, WorkspaceID: "ws-2", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingMissingPhase, Detail: "docs", AuditScore: 3}},
		{TargetID: target, WorkspaceID: "ws-3", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingWrongPersona, PhaseName: "impl", Detail: "wrong", AuditScore: 4}},
		{TargetID: target, WorkspaceID: "ws-4", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingMissingPhase, Detail: "low", AuditScore: 2}}, // below score gate
	}
	if _, err := rdb.InsertDecompFindings(ctx, rows); err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}

	counts, err := rdb.GetDecompFindingCounts(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetDecompFindingCounts: %v", err)
	}
	if counts[audit.FindingMissingPhase] != 2 {
		t.Errorf("missing_phase count = %d, want 2 (one below score gate)", counts[audit.FindingMissingPhase])
	}
	if counts[audit.FindingWrongPersona] != 1 {
		t.Errorf("wrong_persona count = %d, want 1", counts[audit.FindingWrongPersona])
	}
}

// ─── GetRepeatedFindings ─────────────────────────────────────────────────────

func TestGetRepeatedFindings_RequiresMinObservations(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Same finding in 1 workspace — should NOT appear.
	rows := []DecompFindingRow{
		{TargetID: target, WorkspaceID: "ws-1", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingMissingPhase, Detail: "testing phase", AuditScore: 4}},
	}
	if _, err := rdb.InsertDecompFindings(ctx, rows); err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}

	repeated, err := rdb.GetRepeatedFindings(ctx, target, 3, 2)
	if err != nil {
		t.Fatalf("GetRepeatedFindings: %v", err)
	}
	if len(repeated) != 0 {
		t.Errorf("len(repeated) = %d, want 0 (single observation is not repeated)", len(repeated))
	}

	// Add same finding from 2nd workspace — now it should appear.
	rows2 := []DecompFindingRow{
		{TargetID: target, WorkspaceID: "ws-2", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingMissingPhase, Detail: "testing phase", AuditScore: 4}},
	}
	if _, err := rdb.InsertDecompFindings(ctx, rows2); err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}

	repeated, err = rdb.GetRepeatedFindings(ctx, target, 3, 2)
	if err != nil {
		t.Fatalf("GetRepeatedFindings: %v", err)
	}
	if len(repeated) != 2 {
		t.Errorf("len(repeated) = %d, want 2 (same finding from 2 workspaces)", len(repeated))
	}
}

func TestGetRepeatedFindings_DifferentFindingsNotRepeated(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	rows := []DecompFindingRow{
		{TargetID: target, WorkspaceID: "ws-1", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingMissingPhase, Detail: "testing phase", AuditScore: 4}},
		{TargetID: target, WorkspaceID: "ws-2", DecompositionFinding: audit.DecompositionFinding{FindingType: audit.FindingMissingPhase, Detail: "documentation phase", AuditScore: 4}},
	}
	if _, err := rdb.InsertDecompFindings(ctx, rows); err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}

	// Different details → different findings → no repetition.
	repeated, err := rdb.GetRepeatedFindings(ctx, target, 3, 2)
	if err != nil {
		t.Fatalf("GetRepeatedFindings: %v", err)
	}
	if len(repeated) != 0 {
		t.Errorf("len(repeated) = %d, want 0 (different findings are not repeated)", len(repeated))
	}
}

// ─── Schema idempotency ───────────────────────────────────────────────────────

func TestInitSchema_Idempotent(t *testing.T) {
	rdb := newTestDB(t)
	// Calling initSchema a second time on the same DB must not fail.
	if err := rdb.initSchema(); err != nil {
		t.Errorf("second initSchema call failed: %v", err)
	}
}

// ─── GetPlanShapeStats ────────────────────────────────────────────────────────

func TestGetPlanShapeStats_Basic(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/skills/orchestrator"

	// Seed 3 examples that qualify (audit_score >= 3, decomp_quality >= 3).
	examples := []DecompExample{
		{
			TargetID: target, WorkspaceID: "ws-1", TaskSummary: "task 1",
			PhaseCount: 4, ExecutionMode: "sequential",
			PhasesJSON:    `[{"persona":"senior-backend-engineer"},{"persona":"senior-backend-engineer"},{"persona":"staff-code-reviewer"},{"persona":"qa-engineer"}]`,
			DecompSource:  "llm", AuditScore: 4, DecompQuality: 4,
		},
		{
			TargetID: target, WorkspaceID: "ws-2", TaskSummary: "task 2",
			PhaseCount: 6, ExecutionMode: "sequential",
			PhasesJSON:    `[{"persona":"senior-backend-engineer"},{"persona":"senior-backend-engineer"},{"persona":"senior-backend-engineer"},{"persona":"staff-code-reviewer"},{"persona":"qa-engineer"},{"persona":"devops-engineer"}]`,
			DecompSource:  "llm", AuditScore: 5, DecompQuality: 4,
		},
		{
			TargetID: target, WorkspaceID: "ws-3", TaskSummary: "task 3",
			PhaseCount: 5, ExecutionMode: "parallel",
			PhasesJSON:    `[{"persona":"senior-backend-engineer"},{"persona":"senior-frontend-engineer"},{"persona":"staff-code-reviewer"},{"persona":"qa-engineer"},{"persona":"devops-engineer"}]`,
			DecompSource:  "predecomposed", AuditScore: 4, DecompQuality: 3,
		},
	}
	for _, ex := range examples {
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			t.Fatalf("insert %s: %v", ex.WorkspaceID, err)
		}
	}

	stats, err := rdb.GetPlanShapeStats(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetPlanShapeStats: %v", err)
	}
	if stats == nil {
		t.Fatal("GetPlanShapeStats returned nil, want stats")
	}

	// Average: (4+6+5)/3 = 5.0
	if !approxEqual(stats.AvgPhaseCount, 5.0) {
		t.Errorf("AvgPhaseCount = %.2f, want 5.0", stats.AvgPhaseCount)
	}
	if stats.ExampleCount != 3 {
		t.Errorf("ExampleCount = %d, want 3", stats.ExampleCount)
	}
	// MostCommonMode: sequential appears 2x, parallel 1x
	if stats.MostCommonMode != "sequential" {
		t.Errorf("MostCommonMode = %q, want sequential", stats.MostCommonMode)
	}
	// TopPersonas: senior-backend-engineer (6 appearances), staff-code-reviewer (3), qa-engineer (3)
	if len(stats.TopPersonas) != 3 {
		t.Fatalf("TopPersonas len = %d, want 3", len(stats.TopPersonas))
	}
	if stats.TopPersonas[0] != "senior-backend-engineer" {
		t.Errorf("TopPersonas[0] = %q, want senior-backend-engineer", stats.TopPersonas[0])
	}
}

func TestGetPlanShapeStats_InsufficientExamples(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/myapp"

	// Only 1 qualifying example — below minPlanShapeExamples (2)
	ex := DecompExample{
		TargetID: target, WorkspaceID: "ws-1", TaskSummary: "task",
		PhaseCount: 3, ExecutionMode: "sequential",
		PhasesJSON: `[{"persona":"x"}]`, DecompSource: "llm",
		AuditScore: 4, DecompQuality: 4,
	}
	if err := rdb.InsertDecompExample(ctx, ex); err != nil {
		t.Fatalf("insert: %v", err)
	}

	stats, err := rdb.GetPlanShapeStats(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetPlanShapeStats: %v", err)
	}
	if stats != nil {
		t.Errorf("GetPlanShapeStats returned non-nil with 1 example; want nil (insufficient)")
	}
}

func TestGetPlanShapeStats_ScoreGate(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Two examples but one below score gate
	for _, ex := range []DecompExample{
		{TargetID: target, WorkspaceID: "ws-1", TaskSummary: "high", PhaseCount: 3,
			ExecutionMode: "sequential", PhasesJSON: `[{"persona":"a"}]`,
			DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
		{TargetID: target, WorkspaceID: "ws-2", TaskSummary: "low", PhaseCount: 5,
			ExecutionMode: "parallel", PhasesJSON: `[{"persona":"b"}]`,
			DecompSource: "llm", AuditScore: 2, DecompQuality: 4}, // below gate
	} {
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			t.Fatalf("insert %s: %v", ex.WorkspaceID, err)
		}
	}

	// minScore=3 → only ws-1 qualifies → 1 < minPlanShapeExamples → nil
	stats, err := rdb.GetPlanShapeStats(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetPlanShapeStats: %v", err)
	}
	if stats != nil {
		t.Errorf("want nil (only 1 example passes score gate), got %+v", stats)
	}
}

func TestGetPlanShapeStats_MalformedJSON(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Two qualifying examples: one with malformed phases_json
	for _, ex := range []DecompExample{
		{TargetID: target, WorkspaceID: "ws-1", TaskSummary: "good", PhaseCount: 3,
			ExecutionMode: "sequential", PhasesJSON: `[{"persona":"a"},{"persona":"b"}]`,
			DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
		{TargetID: target, WorkspaceID: "ws-2", TaskSummary: "bad json", PhaseCount: 2,
			ExecutionMode: "sequential", PhasesJSON: `{not valid json`,
			DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
	} {
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			t.Fatalf("insert %s: %v", ex.WorkspaceID, err)
		}
	}

	stats, err := rdb.GetPlanShapeStats(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetPlanShapeStats: %v", err)
	}
	if stats == nil {
		t.Fatal("want non-nil stats (2 qualifying rows, malformed json skipped)")
	}
	// Phase count includes both rows
	if !approxEqual(stats.AvgPhaseCount, 2.5) {
		t.Errorf("AvgPhaseCount = %.2f, want 2.5 ((3+2)/2)", stats.AvgPhaseCount)
	}
	// Persona counts only from ws-1 (ws-2 json is malformed)
	if len(stats.TopPersonas) != 2 {
		t.Errorf("TopPersonas len = %d, want 2 (from valid json only)", len(stats.TopPersonas))
	}
}

func TestGetPlanShapeStats_TargetIsolation(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	for _, ex := range []DecompExample{
		{TargetID: "repo:~/a", WorkspaceID: "ws-1", TaskSummary: "a1", PhaseCount: 3,
			ExecutionMode: "sequential", PhasesJSON: `[{"persona":"x"}]`,
			DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
		{TargetID: "repo:~/a", WorkspaceID: "ws-2", TaskSummary: "a2", PhaseCount: 5,
			ExecutionMode: "sequential", PhasesJSON: `[{"persona":"x"}]`,
			DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
		{TargetID: "repo:~/b", WorkspaceID: "ws-3", TaskSummary: "b1", PhaseCount: 10,
			ExecutionMode: "parallel", PhasesJSON: `[{"persona":"y"}]`,
			DecompSource: "llm", AuditScore: 4, DecompQuality: 4},
	} {
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			t.Fatalf("insert %s: %v", ex.WorkspaceID, err)
		}
	}

	stats, err := rdb.GetPlanShapeStats(ctx, "repo:~/a", 3)
	if err != nil {
		t.Fatalf("GetPlanShapeStats: %v", err)
	}
	if stats == nil {
		t.Fatal("want non-nil for repo:~/a")
	}
	// Only a's examples: (3+5)/2 = 4.0
	if !approxEqual(stats.AvgPhaseCount, 4.0) {
		t.Errorf("AvgPhaseCount = %.2f, want 4.0 (target isolation)", stats.AvgPhaseCount)
	}
}

func TestGetPlanShapeStats_Empty(t *testing.T) {
	rdb := newTestDB(t)
	stats, err := rdb.GetPlanShapeStats(context.Background(), "nonexistent", 3)
	if err != nil {
		t.Fatalf("GetPlanShapeStats: %v", err)
	}
	if stats != nil {
		t.Errorf("want nil for nonexistent target, got %+v", stats)
	}
}

// ─── RecordPhaseShape ─────────────────────────────────────────────────────────

func TestRecordPhaseShape_Basic(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-1", 3, "sequential",
		"senior-backend-engineer,staff-code-reviewer,qa-engineer", "success", "")
	if err != nil {
		t.Fatalf("RecordPhaseShape: %v", err)
	}

	// Verify it was recorded by reading back
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	p := patterns[0]
	if p.PhaseCount != 3 {
		t.Errorf("PhaseCount = %d, want 3", p.PhaseCount)
	}
	if p.ExecutionMode != "sequential" {
		t.Errorf("ExecutionMode = %q, want sequential", p.ExecutionMode)
	}
	if len(p.PersonaSeq) != 3 || p.PersonaSeq[0] != "senior-backend-engineer" {
		t.Errorf("PersonaSeq = %v, want [senior-backend-engineer staff-code-reviewer qa-engineer]", p.PersonaSeq)
	}
}

func TestRecordPhaseShape_Idempotent(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	for i := 0; i < 3; i++ {
		err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-1", 3, "sequential", "a,b,c", "success", "")
		if err != nil {
			t.Fatalf("RecordPhaseShape[%d]: %v", i, err)
		}
	}

	// Only one row per workspace (INSERT OR IGNORE)
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1 (idempotent)", len(patterns))
	}
	if patterns[0].SuccessCount != 1 {
		t.Errorf("SuccessCount = %d, want 1 (same workspace counted once)", patterns[0].SuccessCount)
	}
}

func TestRecordPhaseShape_ValidationErrors(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordPhaseShape(ctx, "", "ws-1", 3, "seq", "a", "success", ""); err == nil {
		t.Error("expected error for empty targetID")
	}
	if err := rdb.RecordPhaseShape(ctx, "t", "", 3, "seq", "a", "success", ""); err == nil {
		t.Error("expected error for empty wsID")
	}
	if err := rdb.RecordPhaseShape(ctx, "t", "ws", 3, "seq", "a", "invalid", ""); err == nil {
		t.Error("expected error for invalid outcome")
	}
}

func TestRecordPhaseShape_DefaultExecMode(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Empty execMode should default to "sequential"
	err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-1", 2, "", "a,b", "success", "")
	if err != nil {
		t.Fatalf("RecordPhaseShape: %v", err)
	}

	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len = %d, want 1", len(patterns))
	}
	if patterns[0].ExecutionMode != "sequential" {
		t.Errorf("ExecutionMode = %q, want sequential (default)", patterns[0].ExecutionMode)
	}
}

// ─── GetSuccessfulShapePatterns ───────────────────────────────────────────────

func TestGetSuccessfulShapePatterns_MinCount(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Record same shape from 3 different workspaces
	for i := 0; i < 3; i++ {
		err := rdb.RecordPhaseShape(ctx, target, fmt.Sprintf("ws-%d", i),
			3, "sequential", "backend,reviewer,qa", "success", "")
		if err != nil {
			t.Fatalf("RecordPhaseShape[%d]: %v", i, err)
		}
	}

	// minCount=3 → should return the pattern
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len = %d, want 1", len(patterns))
	}
	if patterns[0].SuccessCount != 3 {
		t.Errorf("SuccessCount = %d, want 3", patterns[0].SuccessCount)
	}

	// minCount=4 → should return nothing
	patterns, err = rdb.GetSuccessfulShapePatterns(ctx, target, 4)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("len = %d, want 0 (below minCount)", len(patterns))
	}
}

func TestGetSuccessfulShapePatterns_ExcludesFailures(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// 2 successes + 1 failure for same shape
	for i := 0; i < 2; i++ {
		rdb.RecordPhaseShape(ctx, target, fmt.Sprintf("ws-s%d", i), 3, "seq", "a,b,c", "success", "")
	}
	rdb.RecordPhaseShape(ctx, target, "ws-f1", 3, "seq", "a,b,c", "failure", "")

	// minCount=3 → only 2 successes, should not qualify
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("len = %d, want 0 (failures should not count)", len(patterns))
	}
}

func TestGetSuccessfulShapePatterns_DifferentShapes(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Shape A: 3 successes
	for i := 0; i < 3; i++ {
		rdb.RecordPhaseShape(ctx, target, fmt.Sprintf("ws-a%d", i), 3, "sequential", "backend,reviewer", "success", "")
	}
	// Shape B: 2 successes (below threshold)
	for i := 0; i < 2; i++ {
		rdb.RecordPhaseShape(ctx, target, fmt.Sprintf("ws-b%d", i), 4, "parallel", "backend,frontend,reviewer,qa", "success", "")
	}

	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len = %d, want 1 (only shape A qualifies)", len(patterns))
	}
	if patterns[0].PhaseCount != 3 {
		t.Errorf("PhaseCount = %d, want 3 (shape A)", patterns[0].PhaseCount)
	}
}

func TestGetSuccessfulShapePatterns_OrderedBySuccessCount(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()
	target := "repo:~/app"

	// Shape A: 5 successes
	for i := 0; i < 5; i++ {
		rdb.RecordPhaseShape(ctx, target, fmt.Sprintf("ws-a%d", i), 3, "seq", "a,b,c", "success", "")
	}
	// Shape B: 3 successes
	for i := 0; i < 3; i++ {
		rdb.RecordPhaseShape(ctx, target, fmt.Sprintf("ws-b%d", i), 2, "seq", "a,b", "success", "")
	}

	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, target, 3)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 2 {
		t.Fatalf("len = %d, want 2", len(patterns))
	}
	// Ordered by success count desc
	if patterns[0].SuccessCount < patterns[1].SuccessCount {
		t.Errorf("patterns[0].SuccessCount=%d < patterns[1].SuccessCount=%d; want desc order",
			patterns[0].SuccessCount, patterns[1].SuccessCount)
	}
}

func TestGetSuccessfulShapePatterns_Empty(t *testing.T) {
	rdb := newTestDB(t)
	patterns, err := rdb.GetSuccessfulShapePatterns(context.Background(), "nonexistent", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("len = %d, want 0", len(patterns))
	}
}

// ─── GetTaskTypeSuccessfulShapes ──────────────────────────────────────────────

// TestGetTaskTypeSuccessfulShapes_ReturnsCrossTargetShapes verifies that shapes
// recorded for different targets but the same task type are aggregated and
// returned when the success count meets the threshold.
func TestGetTaskTypeSuccessfulShapes_ReturnsCrossTargetShapes(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Record successful shapes for two different targets, same task type.
	for _, tc := range []struct{ target, ws string }{
		{"repo:~/app", "ws-1"},
		{"repo:~/api", "ws-2"},
		{"repo:~/web", "ws-3"},
	} {
		if err := rdb.RecordPhaseShape(ctx, tc.target, tc.ws, 3, "sequential", "a,b,c", "success", "implementation"); err != nil {
			t.Fatalf("RecordPhaseShape(%q, %q): %v", tc.target, tc.ws, err)
		}
	}

	patterns, err := rdb.GetTaskTypeSuccessfulShapes(ctx, "implementation", 2)
	if err != nil {
		t.Fatalf("GetTaskTypeSuccessfulShapes: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len = %d, want 1", len(patterns))
	}
	if patterns[0].PhaseCount != 3 {
		t.Errorf("PhaseCount = %d, want 3", patterns[0].PhaseCount)
	}
	if patterns[0].SuccessCount != 3 {
		t.Errorf("SuccessCount = %d, want 3 (cross-target aggregate)", patterns[0].SuccessCount)
	}
}

// TestGetTaskTypeSuccessfulShapes_BelowThreshold verifies that shapes with
// fewer successes than minCount are not returned.
func TestGetTaskTypeSuccessfulShapes_BelowThreshold(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-1", 3, "sequential", "a,b,c", "success", "bugfix"); err != nil {
		t.Fatalf("RecordPhaseShape: %v", err)
	}

	// minCount=2, only 1 success → should return nothing.
	patterns, err := rdb.GetTaskTypeSuccessfulShapes(ctx, "bugfix", 2)
	if err != nil {
		t.Fatalf("GetTaskTypeSuccessfulShapes: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("len = %d, want 0 (below minCount)", len(patterns))
	}
}

// TestGetTaskTypeSuccessfulShapes_ExcludesFailures verifies that only
// successful outcomes count toward the cross-target threshold.
func TestGetTaskTypeSuccessfulShapes_ExcludesFailures(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// One success, two failures — should not reach minCount=2.
	for _, tc := range []struct {
		ws, outcome string
	}{
		{"ws-1", "success"},
		{"ws-2", "failure"},
		{"ws-3", "failure"},
	} {
		if err := rdb.RecordPhaseShape(ctx, "repo:~/app", tc.ws, 2, "sequential", "x,y", tc.outcome, "refactor"); err != nil {
			t.Fatalf("RecordPhaseShape(%q): %v", tc.outcome, err)
		}
	}

	patterns, err := rdb.GetTaskTypeSuccessfulShapes(ctx, "refactor", 2)
	if err != nil {
		t.Fatalf("GetTaskTypeSuccessfulShapes: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("len = %d, want 0 (failures must not count)", len(patterns))
	}
}

// TestGetTaskTypeSuccessfulShapes_UnknownTypeReturnsNil verifies that
// "unknown" task type always returns nil without error (no signal).
func TestGetTaskTypeSuccessfulShapes_UnknownTypeReturnsNil(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Seed some data so the table is non-empty.
	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-1", 3, "sequential", "a,b", "success", "unknown"); err != nil {
		t.Fatalf("RecordPhaseShape: %v", err)
	}

	patterns, err := rdb.GetTaskTypeSuccessfulShapes(ctx, "unknown", 1)
	if err != nil {
		t.Fatalf("GetTaskTypeSuccessfulShapes: %v", err)
	}
	if patterns != nil {
		t.Errorf("expected nil for unknown type, got %v", patterns)
	}
}

// TestGetTaskTypeSuccessfulShapes_IsolatedByTaskType verifies that shapes from
// one task type are not returned when querying another task type.
func TestGetTaskTypeSuccessfulShapes_IsolatedByTaskType(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Two successes of "implementation", one success of "docs".
	for _, tc := range []struct {
		ws, taskType string
	}{
		{"ws-1", "implementation"},
		{"ws-2", "implementation"},
		{"ws-3", "docs"},
	} {
		if err := rdb.RecordPhaseShape(ctx, "repo:~/app", tc.ws, 3, "sequential", "a,b,c", "success", tc.taskType); err != nil {
			t.Fatalf("RecordPhaseShape(%q): %v", tc.taskType, err)
		}
	}

	// Query for "docs" at minCount=2 — only 1 docs success, must return nothing.
	patterns, err := rdb.GetTaskTypeSuccessfulShapes(ctx, "docs", 2)
	if err != nil {
		t.Fatalf("GetTaskTypeSuccessfulShapes: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("len = %d, want 0 (docs has only 1 success, implementation should not bleed in)", len(patterns))
	}
}

// ─── Seed profile completeness ────────────────────────────────────────────────

func TestSeed_AllProfilesHaveRequiredFields(t *testing.T) {
	seeds := DefaultSeeds()

	// 1 Go CLI repo profile + 2 non-repo Nanika targets (system:via, publication:substack).
	if len(seeds) != 3 {
		t.Fatalf("DefaultSeeds() has %d profiles, want 3", len(seeds))
	}

	// Go repo targets: must have Language, Runtime, and senior-backend-engineer
	// in PreferredPersonas.
	expectedGoRepoTargets := map[string]bool{
		"repo:~/nanika/skills/orchestrator": false,
	}
	// Non-repo targets: have no Language/Runtime; preferred personas differ.
	expectedNonRepoTargets := map[string]bool{
		"system:via":           false,
		"publication:substack": false,
	}

	// inMap reports whether key exists in m, regardless of its bool value.
	// Required because map values start as false (unvisited), so m[key] alone
	// cannot distinguish "key missing" from "key present but not yet marked".
	inMap := func(m map[string]bool, key string) bool {
		_, ok := m[key]
		return ok
	}

	for _, p := range seeds {
		switch {
		case inMap(expectedGoRepoTargets, p.TargetID):
			expectedGoRepoTargets[p.TargetID] = true
			if p.TargetType != TargetTypeRepo {
				t.Errorf("%s: TargetType = %q, want %q", p.TargetID, p.TargetType, TargetTypeRepo)
			}
			if p.Language != "go" {
				t.Errorf("%s: Language = %q, want go", p.TargetID, p.Language)
			}
			if p.Runtime != "go" {
				t.Errorf("%s: Runtime = %q, want go", p.TargetID, p.Runtime)
			}
			if len(p.PreferredPersonas) == 0 {
				t.Errorf("%s: PreferredPersonas is empty", p.TargetID)
			}
			if p.Notes == "" {
				t.Errorf("%s: Notes is empty", p.TargetID)
			}
			hasSBE := false
			for _, pp := range p.PreferredPersonas {
				if pp == "senior-backend-engineer" {
					hasSBE = true
				}
			}
			if !hasSBE {
				t.Errorf("%s: PreferredPersonas %v missing senior-backend-engineer", p.TargetID, p.PreferredPersonas)
			}
		case inMap(expectedNonRepoTargets, p.TargetID):
			expectedNonRepoTargets[p.TargetID] = true
			if len(p.PreferredPersonas) == 0 {
				t.Errorf("%s: PreferredPersonas is empty", p.TargetID)
			}
			if p.Notes == "" {
				t.Errorf("%s: Notes is empty", p.TargetID)
			}
		default:
			t.Errorf("unexpected seed profile: %q", p.TargetID)
		}
	}

	for target, found := range expectedGoRepoTargets {
		if !found {
			t.Errorf("missing Go repo seed profile for %q", target)
		}
	}
	for target, found := range expectedNonRepoTargets {
		if !found {
			t.Errorf("missing non-repo seed profile for %q", target)
		}
	}
}

// ─── NewPassiveFindingRow ─────────────────────────────────────────────────────

func TestNewPassiveFindingRow(t *testing.T) {
	row := NewPassiveFindingRow("repo:~/app", "ws-123", "low_confidence", "phase-1", "all keyword")
	if row.TargetID != "repo:~/app" {
		t.Errorf("TargetID = %q", row.TargetID)
	}
	if row.WorkspaceID != "ws-123" {
		t.Errorf("WorkspaceID = %q", row.WorkspaceID)
	}
	if row.FindingType != "low_confidence" {
		t.Errorf("FindingType = %q", row.FindingType)
	}
	if row.PhaseName != "phase-1" {
		t.Errorf("PhaseName = %q", row.PhaseName)
	}
	if row.Detail != "all keyword" {
		t.Errorf("Detail = %q", row.Detail)
	}
	if row.DecompSource != "passive" {
		t.Errorf("DecompSource = %q, want passive", row.DecompSource)
	}
	if row.AuditScore != 0 {
		t.Errorf("AuditScore = %d, want 0 (passive findings always zero)", row.AuditScore)
	}
}

// ─── InsertDecompFindings for passive ─────────────────────────────────────────

func TestInsertDecompFindings_PassiveRoundTrip(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	rows := []DecompFindingRow{
		NewPassiveFindingRow("repo:~/app", "ws-1", "low_confidence", "", "all keyword"),
		NewPassiveFindingRow("repo:~/app", "ws-1", "all_same_persona", "", "4 phases same persona"),
	}

	n, err := rdb.InsertDecompFindings(ctx, rows)
	if err != nil {
		t.Fatalf("InsertDecompFindings: %v", err)
	}
	if n != 2 {
		t.Errorf("inserted = %d, want 2", n)
	}

	// Passive findings have audit_score=0, which is below the minFindingScore=3
	// gate used by GetRepeatedFindings. Verify they are excluded.
	findings, err := rdb.GetRepeatedFindings(ctx, "repo:~/app", 3, 1)
	if err != nil {
		t.Fatalf("GetRepeatedFindings: %v", err)
	}
	if len(findings) != 0 {
		t.Errorf("passive findings (score=0) should be excluded by minScore=3 gate; got %d", len(findings))
	}

	// But they ARE visible when minScore=0
	findings, err = rdb.GetRepeatedFindings(ctx, "repo:~/app", 0, 1)
	if err != nil {
		t.Fatalf("GetRepeatedFindings: %v", err)
	}
	if len(findings) != 2 {
		t.Errorf("passive findings should be visible with minScore=0; got %d", len(findings))
	}
}

// TestRecordPhaseShape_FailureUpgradedOnResume verifies that when a workspace
// initially records a failure and then records a success (resume succeeded),
// the row is upgraded to success so GetSuccessfulShapePatterns counts it.
func TestRecordPhaseShape_FailureUpgradedOnResume(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// First run: failed.
	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-resume", 3, "sequential", "a,b,c", "failure", ""); err != nil {
		t.Fatalf("RecordPhaseShape (failure): %v", err)
	}

	// Verify it shows as failure (not visible in successful shapes).
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 0 {
		t.Errorf("expected no successful shapes before resume, got %d", len(patterns))
	}

	// Resume: same workspace, now succeeds.
	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-resume", 3, "sequential", "a,b,c", "success", ""); err != nil {
		t.Fatalf("RecordPhaseShape (success): %v", err)
	}

	// Now it must appear as a success.
	patterns, err = rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns after resume: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("expected 1 successful shape after resume, got %d", len(patterns))
	}
	if patterns[0].SuccessCount != 1 {
		t.Errorf("SuccessCount = %d, want 1 (one workspace)", patterns[0].SuccessCount)
	}
}

// TestRecordPhaseShape_SuccessNotDowngraded verifies that recording a failure
// for a workspace that already has a success outcome is a no-op — success is never
// overwritten by a subsequent failure.
func TestRecordPhaseShape_SuccessNotDowngraded(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// First run: succeeded.
	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-1", 3, "sequential", "a,b,c", "success", "unknown"); err != nil {
		t.Fatalf("RecordPhaseShape (success): %v", err)
	}

	// Attempt to record a failure for the same workspace.
	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-1", 3, "sequential", "a,b,c", "failure", "unknown"); err != nil {
		t.Fatalf("RecordPhaseShape (failure): %v", err)
	}

	// Outcome must remain success.
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("expected 1 successful shape, got %d (success was downgraded)", len(patterns))
	}
}

// TestRecordPhaseShape_FailureUpgradeRefreshesAllFields verifies that when a
// workspace is upgraded from failure to success the upsert also refreshes
// phase_count, execution_mode, persona_seq, and task_type to the values from
// the success call — not the stale values from the original failure insert.
//
// This is the regression test for the bug where ON CONFLICT only updated
// `outcome`, leaving the other columns frozen from the failed run.
func TestRecordPhaseShape_FailureUpgradeRefreshesAllFields(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// First run: failed with one set of values.
	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-resume", 2, "sequential", "backend,reviewer", "failure", "bugfix"); err != nil {
		t.Fatalf("RecordPhaseShape (failure): %v", err)
	}

	// Resume: same workspace, succeeds with a different plan shape.
	// The resumed plan has more phases, different personas, and a corrected task type.
	if err := rdb.RecordPhaseShape(ctx, "repo:~/app", "ws-resume", 4, "parallel", "backend,frontend,reviewer,qa", "success", "implementation"); err != nil {
		t.Fatalf("RecordPhaseShape (success): %v", err)
	}

	// The stored shape must reflect the success call, not the stale failure values.
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("expected 1 successful shape, got %d", len(patterns))
	}
	p := patterns[0]
	if p.PhaseCount != 4 {
		t.Errorf("PhaseCount = %d, want 4 (from success call)", p.PhaseCount)
	}
	if p.ExecutionMode != "parallel" {
		t.Errorf("ExecutionMode = %q, want parallel (from success call)", p.ExecutionMode)
	}
	if len(p.PersonaSeq) != 4 || p.PersonaSeq[0] != "backend" || p.PersonaSeq[3] != "qa" {
		t.Errorf("PersonaSeq = %v, want [backend frontend reviewer qa] (from success call)", p.PersonaSeq)
	}
}

// TestRecordPhaseShape_NewWorkspaceAlwaysInserted verifies that each distinct
// workspace gets its own row regardless of outcome, and that both outcomes can
// coexist in the table.
func TestRecordPhaseShape_NewWorkspaceAlwaysInserted(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	workspaces := []struct {
		wsID    string
		outcome string
	}{
		{"ws-a", "success"},
		{"ws-b", "failure"},
		{"ws-c", "success"},
	}
	for _, w := range workspaces {
		if err := rdb.RecordPhaseShape(ctx, "repo:~/app", w.wsID, 2, "sequential", "x,y", w.outcome, "unknown"); err != nil {
			t.Fatalf("RecordPhaseShape(%q, %q): %v", w.wsID, w.outcome, err)
		}
	}

	// Two successes → SuccessCount = 2 at minCount=1.
	patterns, err := rdb.GetSuccessfulShapePatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetSuccessfulShapePatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("expected 1 shape pattern, got %d", len(patterns))
	}
	if patterns[0].SuccessCount != 2 {
		t.Errorf("SuccessCount = %d, want 2 (two distinct successful workspaces)", patterns[0].SuccessCount)
	}
}

// ─── GetRepeatedPassiveFindings ───────────────────────────────────────────────

// TestGetRepeatedPassiveFindings_SurfaceAtThreshold verifies that a passive
// finding appearing in exactly minObservations distinct workspaces is returned.
func TestGetRepeatedPassiveFindings_SurfaceAtThreshold(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Insert the same finding from 3 distinct workspaces.
	for i := 1; i <= 3; i++ {
		rows := []DecompFindingRow{
			NewPassiveFindingRow("repo:~/app", fmt.Sprintf("ws-%d", i), "low_confidence", "", "all keyword"),
		}
		if _, err := rdb.InsertDecompFindings(ctx, rows); err != nil {
			t.Fatalf("InsertDecompFindings[%d]: %v", i, err)
		}
	}

	findings, err := rdb.GetRepeatedPassiveFindings(ctx, "repo:~/app", 3)
	if err != nil {
		t.Fatalf("GetRepeatedPassiveFindings: %v", err)
	}
	if len(findings) == 0 {
		t.Error("expected findings at threshold=3, got none")
	}
	for _, f := range findings {
		if f.AuditScore != 0 {
			t.Errorf("AuditScore = %d, want 0 for passive finding", f.AuditScore)
		}
	}
}

// TestGetRepeatedPassiveFindings_NotBelowThreshold verifies that a finding
// appearing in fewer workspaces than minObservations is not returned.
func TestGetRepeatedPassiveFindings_NotBelowThreshold(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Insert from only 2 workspaces; threshold is 3.
	for i := 1; i <= 2; i++ {
		rows := []DecompFindingRow{
			NewPassiveFindingRow("repo:~/app", fmt.Sprintf("ws-%d", i), "low_confidence", "", "all keyword"),
		}
		if _, err := rdb.InsertDecompFindings(ctx, rows); err != nil {
			t.Fatalf("InsertDecompFindings[%d]: %v", i, err)
		}
	}

	findings, err := rdb.GetRepeatedPassiveFindings(ctx, "repo:~/app", 3)
	if err != nil {
		t.Fatalf("GetRepeatedPassiveFindings: %v", err)
	}
	if len(findings) != 0 {
		t.Errorf("expected no findings below threshold=3, got %d", len(findings))
	}
}

// TestGetRepeatedPassiveFindings_NoBleedFromAudited verifies that rows with
// audit_score > 0 (from scored audits) are never returned by
// GetRepeatedPassiveFindings, even if they share the same finding_type/detail.
func TestGetRepeatedPassiveFindings_NoBleedFromAudited(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	// Insert 3 passive rows (audit_score=0) and 3 audited rows (audit_score=4)
	// with the same finding_type/detail key.
	for i := 1; i <= 3; i++ {
		passiveRows := []DecompFindingRow{
			NewPassiveFindingRow("repo:~/app", fmt.Sprintf("passive-ws-%d", i), "low_confidence", "", "all keyword"),
		}
		auditedRows := []DecompFindingRow{{
			TargetID:    "repo:~/app",
			WorkspaceID: fmt.Sprintf("audited-ws-%d", i),
			DecompositionFinding: audit.DecompositionFinding{
				FindingType: "low_confidence",
				Detail:      "all keyword",
				AuditScore:  4,
			},
		}}
		if _, err := rdb.InsertDecompFindings(ctx, passiveRows); err != nil {
			t.Fatalf("InsertDecompFindings passive[%d]: %v", i, err)
		}
		if _, err := rdb.InsertDecompFindings(ctx, auditedRows); err != nil {
			t.Fatalf("InsertDecompFindings audited[%d]: %v", i, err)
		}
	}

	// GetRepeatedPassiveFindings must only return audit_score=0 rows.
	findings, err := rdb.GetRepeatedPassiveFindings(ctx, "repo:~/app", 3)
	if err != nil {
		t.Fatalf("GetRepeatedPassiveFindings: %v", err)
	}
	for _, f := range findings {
		if f.AuditScore != 0 {
			t.Errorf("returned finding with AuditScore=%d (workspace %q), want 0 only", f.AuditScore, f.WorkspaceID)
		}
	}

	// GetRepeatedFindings (minScore=3) must not return the passive rows.
	scoredFindings, err := rdb.GetRepeatedFindings(ctx, "repo:~/app", 3, 3)
	if err != nil {
		t.Fatalf("GetRepeatedFindings: %v", err)
	}
	for _, f := range scoredFindings {
		if f.AuditScore == 0 {
			t.Errorf("GetRepeatedFindings returned passive row (workspace %q, score=0)", f.WorkspaceID)
		}
	}
}

func TestRecordRoleAssignmentAndGetRolePersonaPatterns(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	rows := []RoleAssignment{
		{TargetID: "repo:~/app", WorkspaceID: "ws-1", PhaseID: "p1", Persona: "architect", Role: "planner", Outcome: "success"},
		{TargetID: "repo:~/app", WorkspaceID: "ws-2", PhaseID: "p1", Persona: "architect", Role: "planner", Outcome: "success"},
		{TargetID: "repo:~/app", WorkspaceID: "ws-3", PhaseID: "p2", Persona: "staff-code-reviewer", Role: "reviewer", Outcome: "success"},
		{TargetID: "repo:~/app", WorkspaceID: "ws-4", PhaseID: "p2", Persona: "staff-code-reviewer", Role: "reviewer", Outcome: "failure"},
	}
	for _, row := range rows {
		if err := rdb.RecordRoleAssignment(ctx, row); err != nil {
			t.Fatalf("RecordRoleAssignment(%+v): %v", row, err)
		}
	}

	patterns, err := rdb.GetRolePersonaPatterns(ctx, "repo:~/app", 2)
	if err != nil {
		t.Fatalf("GetRolePersonaPatterns: %v", err)
	}
	if len(patterns) != 2 {
		t.Fatalf("len(patterns) = %d, want 2", len(patterns))
	}

	if patterns[0].Role != "planner" || patterns[0].Persona != "architect" || patterns[0].SeenCount != 2 {
		t.Fatalf("planner pattern = %+v, want architect x2", patterns[0])
	}
	if patterns[0].SuccessRate != 1.0 {
		t.Fatalf("planner success rate = %.2f, want 1.0", patterns[0].SuccessRate)
	}

	if patterns[1].Role != "reviewer" || patterns[1].Persona != "staff-code-reviewer" || patterns[1].SeenCount != 2 {
		t.Fatalf("reviewer pattern = %+v, want staff-code-reviewer x2", patterns[1])
	}
	if !approxEqual(patterns[1].SuccessRate, 0.5) {
		t.Fatalf("reviewer success rate = %.2f, want 0.5", patterns[1].SuccessRate)
	}
}

func TestRecordRoleAssignment_UpsertRefreshesFields(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordRoleAssignment(ctx, RoleAssignment{
		TargetID:    "repo:~/app",
		WorkspaceID: "ws-1",
		PhaseID:     "phase-1",
		Persona:     "architect",
		Role:        "planner",
		Outcome:     "failure",
	}); err != nil {
		t.Fatalf("RecordRoleAssignment(initial): %v", err)
	}
	if err := rdb.RecordRoleAssignment(ctx, RoleAssignment{
		TargetID:    "repo:~/app",
		WorkspaceID: "ws-1",
		PhaseID:     "phase-1",
		Persona:     "staff-code-reviewer",
		Role:        "reviewer",
		Outcome:     "success",
	}); err != nil {
		t.Fatalf("RecordRoleAssignment(update): %v", err)
	}

	patterns, err := rdb.GetRolePersonaPatterns(ctx, "repo:~/app", 1)
	if err != nil {
		t.Fatalf("GetRolePersonaPatterns: %v", err)
	}
	if len(patterns) != 1 {
		t.Fatalf("len(patterns) = %d, want 1", len(patterns))
	}
	if patterns[0].Role != "reviewer" || patterns[0].Persona != "staff-code-reviewer" {
		t.Fatalf("pattern = %+v, want refreshed reviewer/staff-code-reviewer", patterns[0])
	}
}

func TestRecordHandoff_UpsertRefreshesSummaryAndRoles(t *testing.T) {
	rdb := newTestDB(t)
	ctx := context.Background()

	if err := rdb.RecordHandoff(ctx, "repo:~/app", "ws-1", "p1", "p2", "planner", "implementer", "architect", "senior-backend-engineer", "initial plan"); err != nil {
		t.Fatalf("RecordHandoff(initial): %v", err)
	}
	if err := rdb.RecordHandoff(ctx, "repo:~/app", "ws-1", "p1", "p2", "implementer", "reviewer", "senior-backend-engineer", "staff-code-reviewer", "updated summary"); err != nil {
		t.Fatalf("RecordHandoff(update): %v", err)
	}

	var fromRole, toRole, fromPersona, toPersona, summary string
	if err := rdb.db.QueryRowContext(ctx, `
		SELECT from_role, to_role, from_persona, to_persona, summary
		FROM handoff_records
		WHERE workspace_id = ? AND from_phase_id = ? AND to_phase_id = ?
	`, "ws-1", "p1", "p2").Scan(&fromRole, &toRole, &fromPersona, &toPersona, &summary); err != nil {
		t.Fatalf("QueryRow: %v", err)
	}
	if fromRole != "implementer" || toRole != "reviewer" || fromPersona != "senior-backend-engineer" || toPersona != "staff-code-reviewer" || summary != "updated summary" {
		t.Fatalf("handoff row = (%s,%s,%s,%s,%s), want refreshed values", fromRole, toRole, fromPersona, toPersona, summary)
	}
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

// approxEqual reports whether a and b are within 1e-9 of each other.
func approxEqual(a, b float64) bool {
	d := a - b
	if d < 0 {
		d = -d
	}
	return d < 1e-9
}
