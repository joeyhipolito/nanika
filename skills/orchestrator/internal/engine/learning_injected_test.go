package engine

// learning_injected_test.go verifies the TRK-522 observability guarantee: the
// engine emits one event.LearningInjected per selected learning whenever a
// phase prompt has learnings injected, and the event carries the expected
// {learning_id, learning_type, rank, reason} shape.

import (
	"context"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	"github.com/joeyhipolito/orchestrator-cli/internal/event"
	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

// seedLearnings inserts a handful of FTS-searchable learnings into the test
// database keyed on a shared token so FindRelevant can return them without
// an embedder. Returns the inserted learnings in insertion order.
func seedLearnings(t *testing.T, db *learning.DB, domain, token string, n int) []learning.Learning {
	t.Helper()
	seeded := make([]learning.Learning, 0, n)
	for i := 0; i < n; i++ {
		l := learning.Learning{
			ID:        "learn_trk522_" + token + "_" + string(rune('a'+i)),
			Type:      learning.TypeInsight,
			Content:   token + " insight number " + string(rune('a'+i)) + " explaining retrieval.",
			Domain:    domain,
			CreatedAt: time.Now().UTC(),
		}
		if err := db.Insert(context.Background(), l, nil); err != nil {
			t.Fatalf("seed Insert %d: %v", i, err)
		}
		seeded = append(seeded, l)
	}
	return seeded
}

// TestEngine_LearningInjected_EmitsOnePerSelected verifies that when a phase
// pulls N learnings from the DB, N LearningInjected events are emitted — one
// per selected learning — with rank=1..N and reason="find_relevant".
func TestEngine_LearningInjected_EmitsOnePerSelected(t *testing.T) {
	db := openTempDB(t)
	const token = "retrievaltoken"
	seeded := seedLearnings(t, db, "test", token, 3)

	e, em := newExtractEngine(t, db, learningOutputExecutor{output: "plain output no markers"})

	phase := extractPhase("li1")
	// Use the shared token as the full objective so FTS MATCH (implicit AND
	// across query tokens) finds every seeded row.
	phase.Objective = token
	plan := &core.Plan{
		ID:            "plan-learning-injected",
		Task:          "learning injected emission test",
		ExecutionMode: "sequential",
		Phases:        []*core.Phase{phase},
	}

	if _, err := e.Execute(context.Background(), plan); err != nil {
		t.Fatalf("Execute: %v", err)
	}

	evts := em.collected()
	injected := make([]event.Event, 0)
	for _, ev := range evts {
		if ev.Type == event.LearningInjected {
			injected = append(injected, ev)
		}
	}

	if len(injected) == 0 {
		t.Fatalf("expected at least one LearningInjected event; got %d", len(injected))
	}
	if len(injected) > len(seeded) {
		t.Fatalf("expected at most %d LearningInjected events (seeded size); got %d", len(seeded), len(injected))
	}

	// Every event must carry the expected shape and monotonic rank 1..N.
	for i, ev := range injected {
		if ev.PhaseID != "li1" {
			t.Errorf("event %d: phase_id = %q, want li1", i, ev.PhaseID)
		}
		if got, _ := ev.Data["reason"].(string); got != "find_relevant" {
			t.Errorf("event %d: reason = %q, want find_relevant", i, got)
		}
		if got, _ := ev.Data["rank"].(int); got != i+1 {
			t.Errorf("event %d: rank = %d, want %d", i, got, i+1)
		}
		if id, _ := ev.Data["learning_id"].(string); id == "" {
			t.Errorf("event %d: learning_id missing", i)
		}
		if typ, _ := ev.Data["learning_type"].(string); typ == "" {
			t.Errorf("event %d: learning_type missing", i)
		}
	}
}

// TestEngine_LearningInjected_NoLearnings verifies that when FindRelevant
// returns nothing, no LearningInjected events are emitted at all.
func TestEngine_LearningInjected_NoLearnings(t *testing.T) {
	db := openTempDB(t)
	// Do not seed any learnings — FindRelevant returns an empty slice.

	e, em := newExtractEngine(t, db, learningOutputExecutor{output: "no markers here"})

	phase := extractPhase("li-empty")
	phase.Objective = "objective with no matching learnings"
	plan := &core.Plan{
		ID:            "plan-learning-injected-empty",
		Task:          "empty retrieval test",
		ExecutionMode: "sequential",
		Phases:        []*core.Phase{phase},
	}

	if _, err := e.Execute(context.Background(), plan); err != nil {
		t.Fatalf("Execute: %v", err)
	}

	for _, ev := range em.collected() {
		if ev.Type == event.LearningInjected {
			t.Fatalf("unexpected LearningInjected event when DB is empty: %+v", ev.Data)
		}
	}
}
