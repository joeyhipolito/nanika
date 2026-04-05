package main

import (
	"context"
	"database/sql"
	"path/filepath"
	"testing"
	"time"

	"github.com/joeyhipolito/nen/ko"
	_ "modernc.org/sqlite"
)

// seedProposalsDB creates proposals.db with the enrichment columns and
// inserts the provided rows so evaluateProposals has something to classify.
func seedProposalsDB(t *testing.T, dir string, rows []proposalRow) *sql.DB {
	t.Helper()
	dbPath := filepath.Join(dir, "proposals.db")
	db, err := sql.Open("sqlite", dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		t.Fatalf("open proposals.db: %v", err)
	}
	_, err = db.Exec(`CREATE TABLE IF NOT EXISTS proposals (
		dedup_key      TEXT PRIMARY KEY,
		ability        TEXT NOT NULL DEFAULT '',
		category       TEXT NOT NULL DEFAULT '',
		tracker_issue  TEXT NOT NULL DEFAULT ''
	)`)
	if err != nil {
		t.Fatalf("create proposals table: %v", err)
	}
	for _, r := range rows {
		if _, err := db.Exec(`INSERT INTO proposals (dedup_key, ability, category, tracker_issue) VALUES (?,?,?,?)`,
			r.DedupKey, r.Ability, r.Category, r.TrackerIssue); err != nil {
			t.Fatalf("insert proposal %s: %v", r.DedupKey, err)
		}
	}
	t.Cleanup(func() { db.Close() })
	return db
}

// TestCmdEvaluateProposals_PopulatesProposalQualityTable pins that a single
// evaluateProposals call with matching proposals + tracker items leaves
// proposal_quality non-empty with the expected per-(ability, category)
// counts. The sibling idempotency test only compares two runs for equality,
// which silently passes on an empty-list regression — this test closes that
// gap so a future refactor that breaks the writer, drops the Replace call,
// or regresses ClassifyOutcome fails loudly here.
func TestCmdEvaluateProposals_PopulatesProposalQualityTable(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()

	// Three proposals spanning two (ability, category) pairs, each bound
	// to a tracker issue whose status classifies as non-pending — a
	// pending-only input would leave proposal_quality empty and defeat
	// the assertion below.
	proposals := []proposalRow{
		{DedupKey: "k-success", Ability: "shu", Category: "perf", TrackerIssue: "TRK-10"},
		{DedupKey: "k-failure", Ability: "shu", Category: "perf", TrackerIssue: "TRK-11"},
		{DedupKey: "k-stall", Ability: "ko", Category: "eval", TrackerIssue: "TRK-12"},
	}
	db := seedProposalsDB(t, dir, proposals)

	qs, err := ko.NewQualityStore(db)
	if err != nil {
		t.Fatalf("NewQualityStore: %v", err)
	}

	// Past DefaultStallThreshold so open/in-progress classifies as stall.
	staleTime := time.Now().UTC().Add(-ko.DefaultStallThreshold * 2).Format(time.RFC3339)
	seq10, seq11, seq12 := int64(10), int64(11), int64(12)
	items := []trackerItem{
		{ID: "trk-10", SeqID: &seq10, Status: "done", UpdatedAt: staleTime},
		{ID: "trk-11", SeqID: &seq11, Status: "cancelled", UpdatedAt: staleTime},
		{ID: "trk-12", SeqID: &seq12, Status: "in-progress", UpdatedAt: staleTime},
	}

	summary, err := evaluateProposals(ctx, qs, proposals, items, ko.DefaultStallThreshold)
	if err != nil {
		t.Fatalf("evaluateProposals: %v", err)
	}
	if summary.Processed != len(proposals) {
		t.Errorf("summary.Processed = %d, want %d", summary.Processed, len(proposals))
	}
	if summary.Skipped != 0 {
		t.Errorf("summary.Skipped = %d, want 0 (all proposals enriched and have tracker matches)", summary.Skipped)
	}

	list, err := qs.List(ctx)
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(list) == 0 {
		t.Fatal("proposal_quality is empty after evaluateProposals — writer regression")
	}
	// Two distinct (ability, category) pairs → two aggregate rows.
	if len(list) != 2 {
		t.Errorf("proposal_quality rows = %d, want 2", len(list))
	}

	// Verify each expected aggregate row exists with the right counts.
	type key struct{ ability, category string }
	byKey := make(map[key]ko.ProposalQuality, len(list))
	for _, r := range list {
		byKey[key{r.Ability, r.Category}] = r
	}
	shuPerf, ok := byKey[key{"shu", "perf"}]
	if !ok {
		t.Fatal("missing shu/perf row")
	}
	if shuPerf.SuccessCount != 1 || shuPerf.FailureCount != 1 || shuPerf.TotalCount != 2 {
		t.Errorf("shu/perf counts = {ok:%d fail:%d total:%d}, want {1 1 2}",
			shuPerf.SuccessCount, shuPerf.FailureCount, shuPerf.TotalCount)
	}
	koEval, ok := byKey[key{"ko", "eval"}]
	if !ok {
		t.Fatal("missing ko/eval row")
	}
	if koEval.StallCount != 1 || koEval.TotalCount != 1 {
		t.Errorf("ko/eval counts = {stall:%d total:%d}, want {1 1}",
			koEval.StallCount, koEval.TotalCount)
	}
}

// TestCmdEvaluateProposals_IdempotentOnRepeatedRuns is the H1 regression guard.
// Running evaluateProposals twice against identical proposals+tracker state
// must produce identical quality counts — no score accumulation across runs.
func TestCmdEvaluateProposals_IdempotentOnRepeatedRuns(t *testing.T) {
	ctx := context.Background()
	dir := t.TempDir()

	proposals := []proposalRow{
		{DedupKey: "k1", Ability: "shu", Category: "perf", TrackerIssue: "TRK-1"},
		{DedupKey: "k2", Ability: "shu", Category: "perf", TrackerIssue: "TRK-2"},
		{DedupKey: "k3", Ability: "shu", Category: "error", TrackerIssue: "TRK-3"},
		{DedupKey: "k4", Ability: "ko", Category: "eval", TrackerIssue: "TRK-4"},
	}
	db := seedProposalsDB(t, dir, proposals)

	qs, err := ko.NewQualityStore(db)
	if err != nil {
		t.Fatalf("NewQualityStore: %v", err)
	}

	// staleTime is old enough to be a stall at DefaultStallThreshold.
	staleTime := time.Now().UTC().Add(-ko.DefaultStallThreshold * 2).Format(time.RFC3339)

	seqID1, seqID2, seqID3, seqID4 := int64(1), int64(2), int64(3), int64(4)
	items := []trackerItem{
		{ID: "trk-aaa", SeqID: &seqID1, Status: "done", UpdatedAt: staleTime},
		{ID: "trk-bbb", SeqID: &seqID2, Status: "cancelled", UpdatedAt: staleTime},
		{ID: "trk-ccc", SeqID: &seqID3, Status: "open", UpdatedAt: staleTime},    // stall
		{ID: "trk-ddd", SeqID: &seqID4, Status: "done", UpdatedAt: staleTime},
	}

	run := func(label string) []ko.ProposalQuality {
		if _, err := evaluateProposals(ctx, qs, proposals, items, ko.DefaultStallThreshold); err != nil {
			t.Fatalf("%s: evaluateProposals: %v", label, err)
		}
		list, err := qs.List(ctx)
		if err != nil {
			t.Fatalf("%s: List: %v", label, err)
		}
		return list
	}

	first := run("run1")
	second := run("run2")

	if len(first) != len(second) {
		t.Fatalf("row count changed: run1=%d run2=%d", len(first), len(second))
	}
	// Build a map for easy comparison regardless of order.
	type key struct{ ability, category string }
	toMap := func(rows []ko.ProposalQuality) map[key]ko.ProposalQuality {
		m := make(map[key]ko.ProposalQuality, len(rows))
		for _, r := range rows {
			m[key{r.Ability, r.Category}] = r
		}
		return m
	}
	m1, m2 := toMap(first), toMap(second)
	for k, r1 := range m1 {
		r2, ok := m2[k]
		if !ok {
			t.Errorf("run2 missing row %s/%s", k.ability, k.category)
			continue
		}
		if r1.SuccessCount != r2.SuccessCount || r1.FailureCount != r2.FailureCount || r1.StallCount != r2.StallCount || r1.TotalCount != r2.TotalCount {
			t.Errorf("%s/%s: run1={ok:%d fail:%d stall:%d total:%d} run2={ok:%d fail:%d stall:%d total:%d}",
				k.ability, k.category,
				r1.SuccessCount, r1.FailureCount, r1.StallCount, r1.TotalCount,
				r2.SuccessCount, r2.FailureCount, r2.StallCount, r2.TotalCount,
			)
		}
	}
}
