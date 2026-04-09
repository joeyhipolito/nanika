package store

import (
	"os"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-memory/internal/config"
)

func TestAddAndStateRoundTrip(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	entry, state, err := engine.Add(AddInput{
		Text:   "Alice works at OpenAI on platform reliability",
		Entity: "Alice",
		Slots: map[string]string{
			"employer": "OpenAI",
			"role":     "Engineer",
		},
		Tags: map[string]string{
			"project": "memory",
		},
		Source: "unit-test",
	})
	if err != nil {
		t.Fatalf("Add() error = %v", err)
	}
	if entry.ID != 1 {
		t.Fatalf("entry.ID = %d, want 1", entry.ID)
	}
	if got := state.Slots["employer"]; got != "OpenAI" {
		t.Fatalf("state employer = %q, want OpenAI", got)
	}

	reloaded, err := Open()
	if err != nil {
		t.Fatalf("Open() reload error = %v", err)
	}
	reloadedState, ok := reloaded.State("Alice")
	if !ok {
		t.Fatal("State(Alice) not found after reload")
	}
	if got := reloadedState.Slots["role"]; got != "Engineer" {
		t.Fatalf("reloaded role = %q, want Engineer", got)
	}

	results := reloaded.Find("OpenAI reliability", 5)
	if results.Count == 0 {
		t.Fatal("Find() returned no hits")
	}
	if results.Hits[0].ID != entry.ID {
		t.Fatalf("top hit ID = %d, want %d", results.Hits[0].ID, entry.ID)
	}
}

func TestRebuildFromLog(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}
	if _, _, err := engine.Add(AddInput{
		Text:   "Project Atlas deploy owner is Bob",
		Entity: "Bob",
		Slots: map[string]string{
			"role": "Owner",
		},
		Tags: map[string]string{
			"project": "Atlas",
		},
	}); err != nil {
		t.Fatalf("Add() error = %v", err)
	}

	if err := os.Remove(config.SnapshotPath()); err != nil {
		t.Fatalf("Remove(snapshot) error = %v", err)
	}

	rebuilt, err := Rebuild()
	if err != nil {
		t.Fatalf("Rebuild() error = %v", err)
	}

	state, ok := rebuilt.State("Bob")
	if !ok {
		t.Fatal("State(Bob) not found after rebuild")
	}
	if got := state.Slots["role"]; got != "Owner" {
		t.Fatalf("rebuilt role = %q, want Owner", got)
	}

	results := rebuilt.Find("project=atlas", 5)
	if results.Count != 1 {
		t.Fatalf("Find(project=atlas) count = %d, want 1", results.Count)
	}
}

func TestTrustSignal(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	entry, _, err := engine.Add(AddInput{Text: "Atlas deploy notes"})
	if err != nil {
		t.Fatalf("Add() error = %v", err)
	}

	_, err = engine.Trust(entry.ID, "helpful")
	if err != nil {
		t.Fatalf("Trust() error = %v", err)
	}

	signals := engine.TrustSignals(entry.ID)
	if len(signals) != 1 || signals[0] != "helpful" {
		t.Fatalf("TrustSignals() = %v, want [helpful]", signals)
	}

	// duplicate signal must not double-record
	_, err = engine.Trust(entry.ID, "helpful")
	if err != nil {
		t.Fatalf("Trust() duplicate error = %v", err)
	}
	signals = engine.TrustSignals(entry.ID)
	if len(signals) != 1 {
		t.Fatalf("TrustSignals() after duplicate = %v, want [helpful]", signals)
	}

	// survive a rebuild
	reloaded, err := Rebuild()
	if err != nil {
		t.Fatalf("Rebuild() error = %v", err)
	}
	signals = reloaded.TrustSignals(entry.ID)
	if len(signals) != 1 || signals[0] != "helpful" {
		t.Fatalf("TrustSignals() after rebuild = %v, want [helpful]", signals)
	}
}

func TestTrustUnknownEntry(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	_, err = engine.Trust(999, "helpful")
	if err == nil {
		t.Fatal("Trust(999) expected error for unknown entry, got nil")
	}
}

func TestTrustUnknownSignal(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}
	entry, _, err := engine.Add(AddInput{Text: "some entry"})
	if err != nil {
		t.Fatalf("Add() error = %v", err)
	}

	_, err = engine.Trust(entry.ID, "bogus")
	if err == nil {
		t.Fatal("Trust() expected error for unknown signal, got nil")
	}
}

func TestFindFacetQuery(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}
	if _, _, err := engine.Add(AddInput{
		Text:   "Carol owns the alpha rollout",
		Entity: "Carol",
		Slots: map[string]string{
			"role": "Engineer",
		},
		Tags: map[string]string{
			"project": "Alpha",
		},
	}); err != nil {
		t.Fatalf("Add(Carol) error = %v", err)
	}
	if _, _, err := engine.Add(AddInput{
		Text:   "Dave handles beta support",
		Entity: "Dave",
		Slots: map[string]string{
			"role": "Support",
		},
		Tags: map[string]string{
			"project": "Beta",
		},
	}); err != nil {
		t.Fatalf("Add(Dave) error = %v", err)
	}

	results := engine.Find("role=engineer project=alpha", 5)
	if results.Count != 1 {
		t.Fatalf("Find(role=engineer project=alpha) count = %d, want 1", results.Count)
	}
	if got := results.Hits[0].Entity; got != "Carol" {
		t.Fatalf("top hit entity = %q, want Carol", got)
	}
}

// TestTemporalDecayHelper verifies the decay formula directly.
func TestTemporalDecayHelper(t *testing.T) {
	now := time.Now().UTC()

	// Fresh entry (age=0): decay should be 1.0.
	d := temporalDecay(now, 90, now)
	if d != 1.0 {
		t.Fatalf("temporalDecay(age=0) = %f, want 1.0", d)
	}

	// Entry exactly one half-life old: decay should be 0.5.
	halfLifeAgo := now.Add(-90 * 24 * time.Hour)
	d = temporalDecay(halfLifeAgo, 90, now)
	if d < 0.499 || d > 0.501 {
		t.Fatalf("temporalDecay(age=half_life) = %f, want ~0.5", d)
	}

	// Entry two half-lives old: decay should be 0.25.
	twoHalfLivesAgo := now.Add(-180 * 24 * time.Hour)
	d = temporalDecay(twoHalfLivesAgo, 90, now)
	if d < 0.249 || d > 0.251 {
		t.Fatalf("temporalDecay(age=2*half_life) = %f, want ~0.25", d)
	}

	// Future timestamp: treated as age=0, decay must not exceed 1.0.
	d = temporalDecay(now.Add(time.Hour), 90, now)
	if d != 1.0 {
		t.Fatalf("temporalDecay(future) = %f, want 1.0", d)
	}
}

// TestTemporalDecayRanking verifies that older entries score lower than newer ones.
func TestTemporalDecayRanking(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())
	t.Setenv("MEMORY_DECAY_HALF_LIFE_DAYS", "30")

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	// Insert a fresh entry via Add() (CreatedAt = now).
	freshEntry, _, err := engine.Add(AddInput{Text: "deployment notes for the platform system"})
	if err != nil {
		t.Fatalf("Add(fresh) error = %v", err)
	}

	// Insert an old entry directly via addEntry() to backdate CreatedAt.
	oldEntry := normalizeEntry(Entry{
		ID:        engine.snapshot.NextID + 1,
		CreatedAt: time.Now().UTC().Add(-120 * 24 * time.Hour), // 120 days ago (4 half-lives)
		Text:      "deployment notes for the platform system",
	})
	engine.snapshot.addEntry(oldEntry)

	results := engine.Find("deployment platform system", 10)
	if results.Count < 2 {
		t.Fatalf("Find() returned %d hits, want at least 2", results.Count)
	}

	// Fresh entry must outrank old entry.
	var freshScore, oldScore float64
	for _, h := range results.Hits {
		switch h.ID {
		case freshEntry.ID:
			freshScore = h.Score
		case oldEntry.ID:
			oldScore = h.Score
		}
	}
	if freshScore <= oldScore {
		t.Fatalf("fresh entry score (%.4f) should be > old entry score (%.4f)", freshScore, oldScore)
	}
}

// TestJaccardSimHelper verifies Jaccard boundary conditions.
func TestJaccardSimHelper(t *testing.T) {
	// Perfect overlap.
	q := map[string]struct{}{"go": {}, "deploy": {}}
	terms := []string{"go", "deploy"}
	j := jaccardSim(q, terms)
	if j != 1.0 {
		t.Fatalf("jaccardSim(perfect) = %f, want 1.0", j)
	}

	// No overlap.
	q2 := map[string]struct{}{"rust": {}}
	j2 := jaccardSim(q2, terms)
	if j2 != 0.0 {
		t.Fatalf("jaccardSim(no overlap) = %f, want 0.0", j2)
	}

	// Partial overlap: 1 common out of 3 unique = 1/3.
	q3 := map[string]struct{}{"go": {}, "build": {}}
	j3 := jaccardSim(q3, terms) // union={go,deploy,build}=3, intersection={go}=1
	expected := 1.0 / 3.0
	if j3 < expected-0.001 || j3 > expected+0.001 {
		t.Fatalf("jaccardSim(partial) = %f, want ~%.4f", j3, expected)
	}

	// Both empty.
	j4 := jaccardSim(map[string]struct{}{}, nil)
	if j4 != 0.0 {
		t.Fatalf("jaccardSim(empty,empty) = %f, want 0.0", j4)
	}
}

// TestJaccardRanking verifies that higher word overlap produces higher scores.
func TestJaccardRanking(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	// High overlap with query "golang deployment pipeline": 3/3 query tokens present.
	highEntry, _, err := engine.Add(AddInput{Text: "golang deployment pipeline configuration notes"})
	if err != nil {
		t.Fatalf("Add(high) error = %v", err)
	}

	// Low overlap: only 1/3 query tokens present.
	lowEntry, _, err := engine.Add(AddInput{Text: "golang notes on unrelated topic"})
	if err != nil {
		t.Fatalf("Add(low) error = %v", err)
	}

	results := engine.Find("golang deployment pipeline", 10)
	if results.Count < 2 {
		t.Fatalf("Find() returned %d hits, want at least 2", results.Count)
	}

	var highScore, lowScore float64
	for _, h := range results.Hits {
		switch h.ID {
		case highEntry.ID:
			highScore = h.Score
		case lowEntry.ID:
			lowScore = h.Score
		}
	}
	if highScore <= lowScore {
		t.Fatalf("high overlap score (%.4f) should be > low overlap score (%.4f)", highScore, lowScore)
	}
}

// TestTrustMultiplierHelpful verifies that a "helpful" signal raises the entry's score.
func TestTrustMultiplierHelpful(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	entryA, _, err := engine.Add(AddInput{Text: "shared memory cache configuration notes"})
	if err != nil {
		t.Fatalf("Add(A) error = %v", err)
	}
	entryB, _, err := engine.Add(AddInput{Text: "shared memory cache configuration notes"})
	if err != nil {
		t.Fatalf("Add(B) error = %v", err)
	}

	// Mark A as helpful; B stays at default trust.
	if _, err := engine.Trust(entryA.ID, "helpful"); err != nil {
		t.Fatalf("Trust(helpful) error = %v", err)
	}

	ref, ok := engine.snapshot.Entries[entryA.ID]
	if !ok {
		t.Fatal("entry A not found in snapshot after Trust(helpful)")
	}
	if ref.Trust < 1.04 || ref.Trust > 1.06 {
		t.Fatalf("entry A Trust = %.4f after helpful, want ~1.05", ref.Trust)
	}

	results := engine.Find("shared memory cache configuration", 10)
	if results.Count < 2 {
		t.Fatalf("Find() returned %d hits, want at least 2", results.Count)
	}

	var scoreA, scoreB float64
	for _, h := range results.Hits {
		switch h.ID {
		case entryA.ID:
			scoreA = h.Score
		case entryB.ID:
			scoreB = h.Score
		}
	}
	if scoreA <= scoreB {
		t.Fatalf("helpful entry A score (%.4f) should be > default entry B score (%.4f)", scoreA, scoreB)
	}
}

// TestTrustMultiplierUnhelpful verifies "unhelpful" lowers the entry's score.
func TestTrustMultiplierUnhelpful(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	entryA, _, err := engine.Add(AddInput{Text: "shared memory cache configuration notes"})
	if err != nil {
		t.Fatalf("Add(A) error = %v", err)
	}
	entryB, _, err := engine.Add(AddInput{Text: "shared memory cache configuration notes"})
	if err != nil {
		t.Fatalf("Add(B) error = %v", err)
	}

	// Mark A as unhelpful; B stays at default trust.
	if _, err := engine.Trust(entryA.ID, "unhelpful"); err != nil {
		t.Fatalf("Trust(unhelpful) error = %v", err)
	}

	ref, ok := engine.snapshot.Entries[entryA.ID]
	if !ok {
		t.Fatal("entry A not found in snapshot after Trust(unhelpful)")
	}
	if ref.Trust < 0.89 || ref.Trust > 0.91 {
		t.Fatalf("entry A Trust = %.4f after unhelpful, want ~0.90", ref.Trust)
	}

	results := engine.Find("shared memory cache configuration", 10)
	if results.Count < 2 {
		t.Fatalf("Find() returned %d hits, want at least 2", results.Count)
	}

	var scoreA, scoreB float64
	for _, h := range results.Hits {
		switch h.ID {
		case entryA.ID:
			scoreA = h.Score
		case entryB.ID:
			scoreB = h.Score
		}
	}
	if scoreA >= scoreB {
		t.Fatalf("unhelpful entry A score (%.4f) should be < default entry B score (%.4f)", scoreA, scoreB)
	}
}

// TestTrustRebuildPopulatesField verifies that Rebuild() correctly sets the
// Trust field on entries from TrustIndex signal entries in the log.
func TestTrustRebuildPopulatesField(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	entry, _, err := engine.Add(AddInput{Text: "some deployment notes"})
	if err != nil {
		t.Fatalf("Add() error = %v", err)
	}
	if _, err := engine.Trust(entry.ID, "helpful"); err != nil {
		t.Fatalf("Trust(helpful) error = %v", err)
	}
	if _, err := engine.Trust(entry.ID, "unhelpful"); err != nil {
		t.Fatalf("Trust(unhelpful) error = %v", err)
	}

	// Net trust = 1.0 + 0.05 (helpful) - 0.10 (unhelpful) = 0.95
	rebuilt, err := Rebuild()
	if err != nil {
		t.Fatalf("Rebuild() error = %v", err)
	}

	ref, ok := rebuilt.snapshot.Entries[entry.ID]
	if !ok {
		t.Fatal("entry not found after rebuild")
	}
	if ref.Trust < 0.94 || ref.Trust > 0.96 {
		t.Fatalf("rebuilt Trust = %.4f, want ~0.95", ref.Trust)
	}
}

// TestEffectiveTrustDefault verifies backward-compat: zero Trust treated as 1.0.
func TestEffectiveTrustDefault(t *testing.T) {
	if got := effectiveTrust(0); got != 1.0 {
		t.Fatalf("effectiveTrust(0) = %f, want 1.0", got)
	}
	if got := effectiveTrust(1.05); got != 1.05 {
		t.Fatalf("effectiveTrust(1.05) = %f, want 1.05", got)
	}
	if got := effectiveTrust(0.9); got != 0.9 {
		t.Fatalf("effectiveTrust(0.9) = %f, want 0.9", got)
	}
}

// TestBackwardCompatZeroTrustInFind verifies end-to-end that an entry with
// Trust=0 (the zero value from old serialized records that lack the field)
// is treated as Trust=1.0 by Find() and produces a non-zero score.
func TestBackwardCompatZeroTrustInFind(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	// Inject an entry directly with Trust=0 (simulates an old record loaded
	// from disk before the Trust field existed).
	raw := normalizeEntry(Entry{
		ID:        engine.snapshot.NextID + 1,
		CreatedAt: time.Now().UTC(),
		Text:      "legacy cache warming notes",
		Trust:     0, // explicitly absent / zero — backward compat case
	})
	engine.snapshot.addEntry(raw)

	results := engine.Find("legacy cache warming", 5)
	if results.Count == 0 {
		t.Fatal("Find() returned no hits for legacy zero-trust entry")
	}

	hit := results.Hits[0]
	if hit.ID != raw.ID {
		t.Fatalf("top hit ID = %d, want %d", hit.ID, raw.ID)
	}
	// The SearchHit.Trust field must be 1.0 (not 0), proving effectiveTrust was applied.
	if hit.Trust != 1.0 {
		t.Fatalf("SearchHit.Trust = %f for zero-trust entry, want 1.0", hit.Trust)
	}
	// Score must be non-zero — a zero Trust would have collapsed the score to 0.
	if hit.Score <= 0 {
		t.Fatalf("SearchHit.Score = %f for zero-trust entry, want > 0", hit.Score)
	}
}

// TestCombinedScoreFormulaCorrectness verifies that the Score/FinalScore field
// on every SearchHit exactly equals (token_score×0.5 + jaccard×0.5) × decay × trust,
// i.e. the formula documented in Find().
func TestCombinedScoreFormulaCorrectness(t *testing.T) {
	t.Setenv("MEMORY_HOME", t.TempDir())

	engine, err := Open()
	if err != nil {
		t.Fatalf("Open() error = %v", err)
	}

	entries := []struct {
		text   string
		signal string // optional trust signal to apply
	}{
		{"kubernetes deployment pipeline rollout", ""},
		{"kubernetes deployment pipeline rollout", "helpful"},
		{"kubernetes deployment pipeline rollout", "unhelpful"},
	}

	ids := make([]uint64, 0, len(entries))
	for _, e := range entries {
		entry, _, addErr := engine.Add(AddInput{Text: e.text})
		if addErr != nil {
			t.Fatalf("Add() error = %v", addErr)
		}
		if e.signal != "" {
			if _, trustErr := engine.Trust(entry.ID, e.signal); trustErr != nil {
				t.Fatalf("Trust(%s) error = %v", e.signal, trustErr)
			}
		}
		ids = append(ids, entry.ID)
	}

	results := engine.Find("kubernetes deployment pipeline rollout", 10)
	if results.Count < len(ids) {
		t.Fatalf("Find() returned %d hits, want at least %d", results.Count, len(ids))
	}

	const epsilon = 1e-9
	for _, hit := range results.Hits {
		// Only check our inserted entries.
		found := false
		for _, id := range ids {
			if hit.ID == id {
				found = true
				break
			}
		}
		if !found {
			continue
		}

		expected := (hit.TokenScore*0.5 + hit.Jaccard*0.5) * hit.TemporalDecay * hit.Trust
		if diff := expected - hit.Score; diff < -epsilon || diff > epsilon {
			t.Errorf("hit %d: Score = %.10f, formula gives %.10f (diff = %e)",
				hit.ID, hit.Score, expected, diff)
		}
		// FinalScore is an alias for Score.
		if hit.FinalScore != hit.Score {
			t.Errorf("hit %d: FinalScore = %f != Score = %f", hit.ID, hit.FinalScore, hit.Score)
		}
		// Sanity: Trust must never be 0 in a returned hit.
		if hit.Trust == 0 {
			t.Errorf("hit %d: Trust = 0 in SearchHit (effectiveTrust not applied)", hit.ID)
		}
	}
}
