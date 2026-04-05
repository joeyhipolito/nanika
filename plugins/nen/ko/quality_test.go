package ko

import (
	"context"
	"database/sql"
	"math"
	"path/filepath"
	"testing"
	"time"

	_ "modernc.org/sqlite"
)

func TestComputeScore_BelowMinSamplesIsNeutral(t *testing.T) {
	// Zero samples or too few → default neutral, regardless of ratio.
	cases := []struct {
		name                string
		success, fail, stall int
	}{
		{"empty", 0, 0, 0},
		{"one success", 1, 0, 0},
		{"one failure", 0, 1, 0},
		{"two successes", 2, 0, 0},
		{"mixed two", 1, 1, 0},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := ComputeScore(tc.success, tc.fail, tc.stall)
			if got != DefaultQualityScore {
				t.Errorf("ComputeScore(%d,%d,%d) = %v, want neutral %v",
					tc.success, tc.fail, tc.stall, got, DefaultQualityScore)
			}
		})
	}
}

func TestComputeScore_PerfectSuccessApproachesOne(t *testing.T) {
	// 3-for-3 hits the threshold: Laplace = (3+1)/(3+2) = 0.8
	got := ComputeScore(3, 0, 0)
	want := 0.8
	if math.Abs(got-want) > 0.0001 {
		t.Errorf("ComputeScore(3,0,0) = %v, want %v", got, want)
	}
	// 10-for-10 should be (10+1)/(10+2) ≈ 0.9167
	got = ComputeScore(10, 0, 0)
	want = 11.0 / 12.0
	if math.Abs(got-want) > 0.0001 {
		t.Errorf("ComputeScore(10,0,0) = %v, want %v", got, want)
	}
}

func TestComputeScore_PerfectFailureApproachesZero(t *testing.T) {
	// 0-for-3: (0+1)/(3+2) = 0.2
	got := ComputeScore(0, 3, 0)
	want := 0.2
	if math.Abs(got-want) > 0.0001 {
		t.Errorf("ComputeScore(0,3,0) = %v, want %v", got, want)
	}
	// 0-for-10: (0+1)/(10+2) ≈ 0.0833
	got = ComputeScore(0, 10, 0)
	want = 1.0 / 12.0
	if math.Abs(got-want) > 0.0001 {
		t.Errorf("ComputeScore(0,10,0) = %v, want %v", got, want)
	}
}

func TestComputeScore_MixedOutcomesCenterNear5(t *testing.T) {
	// Even split tends back toward 0.5 after Laplace smoothing.
	got := ComputeScore(5, 5, 0)
	want := 6.0 / 12.0 // (5+1)/(10+2) = 0.5
	if math.Abs(got-want) > 0.0001 {
		t.Errorf("ComputeScore(5,5,0) = %v, want %v", got, want)
	}
}

func TestComputeScore_StallsWeightedPartially(t *testing.T) {
	// 2 success + 2 stall: weighted = 2 + 0.25*2 = 2.5, total = 4
	// (2.5 + 1) / (4 + 2) = 3.5 / 6 ≈ 0.5833
	got := ComputeScore(2, 0, 2)
	want := 3.5 / 6.0
	if math.Abs(got-want) > 0.0001 {
		t.Errorf("ComputeScore(2,0,2) = %v, want %v", got, want)
	}

	// 0 success + 0 fail + 4 stall: weighted = 1, total = 4
	// (1 + 1) / (4 + 2) = 2/6 ≈ 0.3333 — stalls alone are worse than neutral.
	got = ComputeScore(0, 0, 4)
	want = 2.0 / 6.0
	if math.Abs(got-want) > 0.0001 {
		t.Errorf("ComputeScore(0,0,4) = %v, want %v", got, want)
	}
}

func TestComputeScore_ThresholdBoundary(t *testing.T) {
	// Below MinSamplesForConfidence — neutral.
	if got := ComputeScore(0, MinSamplesForConfidence-1, 0); got != DefaultQualityScore {
		t.Errorf("just-below threshold = %v, want neutral", got)
	}
	// Exactly at MinSamplesForConfidence — apply smoothing.
	got := ComputeScore(MinSamplesForConfidence, 0, 0)
	if got == DefaultQualityScore {
		t.Errorf("at threshold should not be neutral, got %v", got)
	}
}

func TestComputeScore_BoundedBetween0And1(t *testing.T) {
	cases := [][3]int{
		{100, 0, 0},
		{0, 100, 0},
		{0, 0, 100},
		{50, 25, 25},
	}
	for _, tc := range cases {
		got := ComputeScore(tc[0], tc[1], tc[2])
		if got < 0 || got > 1 {
			t.Errorf("ComputeScore(%v) = %v, out of [0,1] range", tc, got)
		}
	}
}

// --- ClassifyOutcome ---

func TestClassifyOutcome_DoneIsSuccess(t *testing.T) {
	got := ClassifyOutcome("done", time.Minute, DefaultStallThreshold)
	if got != OutcomeSuccess {
		t.Errorf("done → %v, want success", got)
	}
}

func TestClassifyOutcome_CancelledAndClosedAreFailures(t *testing.T) {
	for _, status := range []string{"cancelled", "closed"} {
		got := ClassifyOutcome(status, time.Minute, DefaultStallThreshold)
		if got != OutcomeFailure {
			t.Errorf("%s → %v, want failure", status, got)
		}
	}
}

func TestClassifyOutcome_RecentOpenIsPending(t *testing.T) {
	got := ClassifyOutcome("open", 1*time.Hour, DefaultStallThreshold)
	if got != OutcomePending {
		t.Errorf("recent open → %v, want pending", got)
	}
	got = ClassifyOutcome("in-progress", 1*time.Hour, DefaultStallThreshold)
	if got != OutcomePending {
		t.Errorf("recent in-progress → %v, want pending", got)
	}
}

func TestClassifyOutcome_StaleOpenIsStall(t *testing.T) {
	// Exactly at threshold → stall (>= comparison).
	got := ClassifyOutcome("open", DefaultStallThreshold, DefaultStallThreshold)
	if got != OutcomeStall {
		t.Errorf("open at exact threshold → %v, want stall", got)
	}
	// Past threshold.
	got = ClassifyOutcome("in-progress", 72*time.Hour, DefaultStallThreshold)
	if got != OutcomeStall {
		t.Errorf("72h in-progress → %v, want stall", got)
	}
}

func TestClassifyOutcome_StallThresholdIsConfigurable(t *testing.T) {
	// Custom 10-minute threshold — 15 minutes counts as stall.
	got := ClassifyOutcome("open", 15*time.Minute, 10*time.Minute)
	if got != OutcomeStall {
		t.Errorf("open 15min with 10min threshold → %v, want stall", got)
	}
	got = ClassifyOutcome("open", 5*time.Minute, 10*time.Minute)
	if got != OutcomePending {
		t.Errorf("open 5min with 10min threshold → %v, want pending", got)
	}
}

func TestClassifyOutcome_UnknownStatusIsPending(t *testing.T) {
	got := ClassifyOutcome("weird-state", 999*time.Hour, DefaultStallThreshold)
	if got != OutcomePending {
		t.Errorf("unknown status → %v, want pending (prefer no signal over wrong signal)", got)
	}
}

// --- QualityStore ---

func openTestQualityDB(t *testing.T) (*QualityStore, *sql.DB) {
	t.Helper()
	dbPath := filepath.Join(t.TempDir(), "proposals.db")
	db, err := sql.Open("sqlite", dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		t.Fatalf("open sqlite: %v", err)
	}
	qs, err := NewQualityStore(db)
	if err != nil {
		db.Close()
		t.Fatalf("NewQualityStore: %v", err)
	}
	t.Cleanup(func() { db.Close() })
	return qs, db
}

func TestQualityStore_NewMigrationIsIdempotent(t *testing.T) {
	dbPath := filepath.Join(t.TempDir(), "proposals.db")
	db, err := sql.Open("sqlite", dbPath)
	if err != nil {
		t.Fatalf("open: %v", err)
	}
	defer db.Close()
	if _, err := NewQualityStore(db); err != nil {
		t.Fatalf("first migrate: %v", err)
	}
	// Running migrate again must not fail.
	if _, err := NewQualityStore(db); err != nil {
		t.Errorf("second migrate should be idempotent: %v", err)
	}
}

func TestQualityStore_NewRejectsNilDB(t *testing.T) {
	if _, err := NewQualityStore(nil); err == nil {
		t.Error("expected error for nil DB, got nil")
	}
}

func TestQualityStore_LookupMissingReturnsNeutral(t *testing.T) {
	qs, _ := openTestQualityDB(t)
	q, err := qs.Lookup(context.Background(), "shu", "review-blocker")
	if err != nil {
		t.Fatalf("Lookup: %v", err)
	}
	if q.Score != DefaultQualityScore {
		t.Errorf("missing row score = %v, want neutral %v", q.Score, DefaultQualityScore)
	}
	if q.TotalCount != 0 {
		t.Errorf("missing row total = %d, want 0", q.TotalCount)
	}
}

func TestQualityStore_RecordAndLookupRoundtrip(t *testing.T) {
	qs, _ := openTestQualityDB(t)
	ctx := context.Background()

	// 3 successes, 1 failure, 1 stall for shu/perf.
	for i := 0; i < 3; i++ {
		if err := qs.Record(ctx, "shu", "perf", OutcomeSuccess); err != nil {
			t.Fatalf("record success: %v", err)
		}
	}
	if err := qs.Record(ctx, "shu", "perf", OutcomeFailure); err != nil {
		t.Fatalf("record failure: %v", err)
	}
	if err := qs.Record(ctx, "shu", "perf", OutcomeStall); err != nil {
		t.Fatalf("record stall: %v", err)
	}

	q, err := qs.Lookup(ctx, "shu", "perf")
	if err != nil {
		t.Fatalf("Lookup: %v", err)
	}
	if q.SuccessCount != 3 {
		t.Errorf("success count = %d, want 3", q.SuccessCount)
	}
	if q.FailureCount != 1 {
		t.Errorf("failure count = %d, want 1", q.FailureCount)
	}
	if q.StallCount != 1 {
		t.Errorf("stall count = %d, want 1", q.StallCount)
	}
	if q.TotalCount != 5 {
		t.Errorf("total = %d, want 5", q.TotalCount)
	}
	// Score must match ComputeScore exactly — no drift between counts and cached score.
	wantScore := ComputeScore(3, 1, 1)
	if math.Abs(q.Score-wantScore) > 0.0001 {
		t.Errorf("score = %v, want %v", q.Score, wantScore)
	}
}

func TestQualityStore_RecordPendingIsNoOp(t *testing.T) {
	qs, _ := openTestQualityDB(t)
	ctx := context.Background()
	if err := qs.Record(ctx, "shu", "test", OutcomePending); err != nil {
		t.Fatalf("record pending: %v", err)
	}
	q, _ := qs.Lookup(ctx, "shu", "test")
	if q.TotalCount != 0 {
		t.Errorf("pending should not increment counts, got total %d", q.TotalCount)
	}
}

func TestQualityStore_RecordRequiresAbilityAndCategory(t *testing.T) {
	qs, _ := openTestQualityDB(t)
	ctx := context.Background()
	if err := qs.Record(ctx, "", "x", OutcomeSuccess); err == nil {
		t.Error("expected error for empty ability")
	}
	if err := qs.Record(ctx, "x", "", OutcomeSuccess); err == nil {
		t.Error("expected error for empty category")
	}
}

func TestQualityStore_LookupScoreConvenience(t *testing.T) {
	qs, _ := openTestQualityDB(t)
	ctx := context.Background()
	// Missing row → neutral.
	if got := qs.LookupScore(ctx, "x", "y"); got != DefaultQualityScore {
		t.Errorf("missing LookupScore = %v, want %v", got, DefaultQualityScore)
	}
	// After 3 successes, score should be > neutral.
	for i := 0; i < 3; i++ {
		_ = qs.Record(ctx, "x", "y", OutcomeSuccess)
	}
	if got := qs.LookupScore(ctx, "x", "y"); got <= DefaultQualityScore {
		t.Errorf("after 3 successes, LookupScore = %v, want > %v", got, DefaultQualityScore)
	}
}

func TestQualityStore_ListReturnsAllOrdered(t *testing.T) {
	qs, _ := openTestQualityDB(t)
	ctx := context.Background()

	_ = qs.Record(ctx, "shu", "perf", OutcomeSuccess)
	_ = qs.Record(ctx, "shu", "error", OutcomeFailure)
	_ = qs.Record(ctx, "ko", "eval-failure", OutcomeSuccess)

	list, err := qs.List(ctx)
	if err != nil {
		t.Fatalf("List: %v", err)
	}
	if len(list) != 3 {
		t.Fatalf("list length = %d, want 3", len(list))
	}
	// Ordered: ability asc, then category asc → ko/eval-failure, shu/error, shu/perf.
	wantOrder := [][2]string{
		{"ko", "eval-failure"},
		{"shu", "error"},
		{"shu", "perf"},
	}
	for i, want := range wantOrder {
		if list[i].Ability != want[0] || list[i].Category != want[1] {
			t.Errorf("list[%d] = %s/%s, want %s/%s",
				i, list[i].Ability, list[i].Category, want[0], want[1])
		}
	}
}

func TestQualityStore_UpsertUpdatesExistingRow(t *testing.T) {
	qs, _ := openTestQualityDB(t)
	ctx := context.Background()

	_ = qs.Record(ctx, "shu", "perf", OutcomeSuccess)
	q1, _ := qs.Lookup(ctx, "shu", "perf")
	if q1.SuccessCount != 1 {
		t.Fatalf("after 1 record, success = %d", q1.SuccessCount)
	}

	// Second record for same (ability, category) must update, not insert.
	_ = qs.Record(ctx, "shu", "perf", OutcomeSuccess)
	q2, _ := qs.Lookup(ctx, "shu", "perf")
	if q2.SuccessCount != 2 {
		t.Errorf("after 2 records, success = %d, want 2", q2.SuccessCount)
	}

	list, _ := qs.List(ctx)
	if len(list) != 1 {
		t.Errorf("list should still have 1 row after upsert, got %d", len(list))
	}
}

// --- Summary helper ---

func TestQualityEvalSummary_Increment(t *testing.T) {
	var s QualityEvalSummary
	s.Increment(OutcomeSuccess)
	s.Increment(OutcomeSuccess)
	s.Increment(OutcomeFailure)
	s.Increment(OutcomeStall)
	s.Increment(OutcomePending)
	if s.Success != 2 {
		t.Errorf("Success = %d, want 2", s.Success)
	}
	if s.Failure != 1 {
		t.Errorf("Failure = %d, want 1", s.Failure)
	}
	if s.Stall != 1 {
		t.Errorf("Stall = %d, want 1", s.Stall)
	}
	if s.Pending != 1 {
		t.Errorf("Pending = %d, want 1", s.Pending)
	}
}
