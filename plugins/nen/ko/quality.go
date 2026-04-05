package ko

// Proposal quality scoring.
//
// Ko evaluates the quality of past proposals shu has emitted and stores a
// per (ability, category) score that shu consults when ranking future
// proposals within the same severity tier. Signal sources:
//
//   success  → tracker issue status = "done"   (mission completed, findings superseded)
//   failure  → tracker issue status = "cancelled" or "closed" (human rejected or superseded)
//   stall    → tracker issue still "open"/"in-progress" past StallThreshold
//   pending  → still within the wait window, no signal recorded
//
// Scores are persisted in the same proposals.db shu already owns; ko opens
// it read/write through NewQualityStore, adds the table via idempotent
// CREATE TABLE IF NOT EXISTS, and upserts counts via INSERT…ON CONFLICT.
//
// # proposal_quality vs eval_results — not the same table, not the same data
//
// `proposal_quality` (this file, in proposals.db) and `eval_results`
// (db.go, in ko-history.db) are orthogonal. They live in different
// SQLite files, are written by different ko subcommands, and are built
// from different inputs:
//
//	proposal_quality   ← `ko evaluate-proposals`
//	                     input:  proposals rows ⋈ `tracker query items`
//	                     shape:  per-(ability, category) aggregate counts
//	                     target: ~/.alluka/nen/proposals.db
//
//	eval_results       ← `ko evaluate <config.yaml>`
//	                     input:  YAML test case + LLM response
//	                     shape:  per-test LLM-as-judge verdict rows
//	                     target: ~/.alluka/ko-history.db
//
// proposal_quality is NOT a projected view over eval_results — there is
// no data flow from one to the other. An empty proposal_quality table
// alongside a populated eval_results table means `ko evaluate-proposals`
// simply has not run yet (or ran against proposals with no enriched
// columns / no matching tracker issue). It does not mean ko's writer is
// pointing at the wrong DB. See also the resolution to memory-system
// audit open-question #4.

import (
	"context"
	"database/sql"
	"fmt"
	"time"
)

// DefaultQualityScore is the neutral score returned when no history exists
// for an (ability, category) pair or before MinSamplesForConfidence has
// accumulated. Mid-point so it neither boosts nor suppresses ranking.
const DefaultQualityScore = 0.5

// MinSamplesForConfidence is the minimum number of outcomes required before
// ComputeScore reports anything other than DefaultQualityScore. Below this
// threshold, there is not enough data to distinguish signal from noise.
const MinSamplesForConfidence = 3

// StallWeight is how much a stalled proposal contributes to the score.
// Stalls are weaker evidence than either success or failure — they may
// resolve either way — so we count them as 0.25 of a success.
const StallWeight = 0.25

// DefaultStallThreshold is the age past which an open or in-progress
// proposal is treated as stalled. 48 h gives one operator cycle to triage
// before the proposal starts dragging down its category's score.
const DefaultStallThreshold = 48 * time.Hour

// ProposalOutcome classifies the final state of a proposal for scoring.
type ProposalOutcome int

const (
	// OutcomePending means not enough time has passed to decide — no signal.
	OutcomePending ProposalOutcome = iota
	// OutcomeSuccess means the proposal was dispatched and resolved cleanly.
	OutcomeSuccess
	// OutcomeFailure means the proposal was rejected, cancelled, or closed without fix.
	OutcomeFailure
	// OutcomeStall means the proposal sat open past the stall threshold.
	OutcomeStall
)

func (o ProposalOutcome) String() string {
	switch o {
	case OutcomeSuccess:
		return "success"
	case OutcomeFailure:
		return "failure"
	case OutcomeStall:
		return "stall"
	case OutcomePending:
		return "pending"
	default:
		return "unknown"
	}
}

// ComputeScore returns a quality score in [0, 1] from raw counts.
//
// Below MinSamplesForConfidence the score is neutral (DefaultQualityScore),
// so new (ability, category) pairs do not get boosted or suppressed just
// because their single data point looks extreme.
//
// Above the threshold the score is Laplace-smoothed:
//
//	(weightedSuccesses + 1) / (total + 2)
//
// where weightedSuccesses = successes + StallWeight*stalls. The +1/+2 prior
// pulls scores toward 0.5, so a 3-for-3 category lands at 4/5 = 0.8 rather
// than 1.0, keeping ranking responsive to subsequent failures.
func ComputeScore(successes, failures, stalls int) float64 {
	total := successes + failures + stalls
	if total < MinSamplesForConfidence {
		return DefaultQualityScore
	}
	weighted := float64(successes) + StallWeight*float64(stalls)
	return (weighted + 1.0) / (float64(total) + 2.0)
}

// ClassifyOutcome maps a tracker issue status and age to a ProposalOutcome.
//
// The stallThreshold parameter lets operators tune how long an open proposal
// can sit before counting as a stall (DefaultStallThreshold is the sane
// default). Unknown statuses return OutcomePending — we prefer no signal
// over wrong signal.
func ClassifyOutcome(status string, age time.Duration, stallThreshold time.Duration) ProposalOutcome {
	switch status {
	case "done":
		return OutcomeSuccess
	case "cancelled", "closed":
		return OutcomeFailure
	case "open", "in-progress":
		if age >= stallThreshold {
			return OutcomeStall
		}
		return OutcomePending
	default:
		return OutcomePending
	}
}

// ProposalQuality is the persisted and computed view of a single
// (ability, category) row in the proposal_quality table.
type ProposalQuality struct {
	Ability      string
	Category     string
	SuccessCount int
	FailureCount int
	StallCount   int
	TotalCount   int
	// Score is ComputeScore(SuccessCount, FailureCount, StallCount). Not a
	// stored column — recomputed on every Lookup to avoid drift between the
	// counts and the cached score.
	Score       float64
	LastUpdated time.Time
}

// QualityStore reads and writes proposal quality scores, backed by the
// same *sql.DB shu uses for proposals.db. The store does not own the
// connection — Close is a no-op so callers can share the DB handle.
type QualityStore struct {
	db *sql.DB
}

// NewQualityStore wraps an open *sql.DB and runs the idempotent schema
// migration. The DB is not closed by the store; the caller retains
// ownership of the connection lifecycle.
func NewQualityStore(db *sql.DB) (*QualityStore, error) {
	if db == nil {
		return nil, fmt.Errorf("quality store: db is nil")
	}
	qs := &QualityStore{db: db}
	if err := qs.migrate(); err != nil {
		return nil, fmt.Errorf("quality store migrate: %w", err)
	}
	return qs, nil
}

func (qs *QualityStore) migrate() error {
	_, err := qs.db.Exec(`CREATE TABLE IF NOT EXISTS proposal_quality (
		ability         TEXT    NOT NULL,
		category        TEXT    NOT NULL,
		success_count   INTEGER NOT NULL DEFAULT 0,
		failure_count   INTEGER NOT NULL DEFAULT 0,
		stall_count     INTEGER NOT NULL DEFAULT 0,
		total_count     INTEGER NOT NULL DEFAULT 0,
		last_updated    TEXT    NOT NULL,
		PRIMARY KEY (ability, category)
	)`)
	if err != nil {
		return fmt.Errorf("create proposal_quality: %w", err)
	}
	return nil
}

// Record applies a single outcome to the (ability, category) row. Pending
// outcomes are a no-op because they carry no signal. Success/failure/stall
// increment the corresponding counter plus total_count via upsert.
func (qs *QualityStore) Record(ctx context.Context, ability, category string, outcome ProposalOutcome) error {
	if ability == "" || category == "" {
		return fmt.Errorf("record quality: ability and category required")
	}
	if outcome == OutcomePending {
		return nil
	}
	var successDelta, failureDelta, stallDelta int
	switch outcome {
	case OutcomeSuccess:
		successDelta = 1
	case OutcomeFailure:
		failureDelta = 1
	case OutcomeStall:
		stallDelta = 1
	default:
		return fmt.Errorf("record quality: unknown outcome %d", outcome)
	}
	now := time.Now().UTC().Format(time.RFC3339)
	_, err := qs.db.ExecContext(ctx, `INSERT INTO proposal_quality
		(ability, category, success_count, failure_count, stall_count, total_count, last_updated)
		VALUES (?, ?, ?, ?, ?, ?, ?)
		ON CONFLICT (ability, category) DO UPDATE SET
			success_count = success_count + excluded.success_count,
			failure_count = failure_count + excluded.failure_count,
			stall_count   = stall_count   + excluded.stall_count,
			total_count   = total_count   + excluded.total_count,
			last_updated  = excluded.last_updated`,
		ability, category, successDelta, failureDelta, stallDelta, 1, now,
	)
	if err != nil {
		return fmt.Errorf("upsert proposal_quality %s/%s: %w", ability, category, err)
	}
	return nil
}

// Lookup returns the stored quality row for (ability, category). If no row
// exists, Lookup returns a zero ProposalQuality with the default neutral
// score — callers do not need to special-case missing data.
func (qs *QualityStore) Lookup(ctx context.Context, ability, category string) (ProposalQuality, error) {
	var (
		q       ProposalQuality
		updated string
	)
	err := qs.db.QueryRowContext(ctx, `
		SELECT ability, category, success_count, failure_count, stall_count, total_count, last_updated
		FROM proposal_quality WHERE ability = ? AND category = ?`,
		ability, category,
	).Scan(&q.Ability, &q.Category, &q.SuccessCount, &q.FailureCount, &q.StallCount, &q.TotalCount, &updated)
	if err == sql.ErrNoRows {
		return ProposalQuality{
			Ability:  ability,
			Category: category,
			Score:    DefaultQualityScore,
		}, nil
	}
	if err != nil {
		return ProposalQuality{}, fmt.Errorf("lookup proposal_quality %s/%s: %w", ability, category, err)
	}
	if t, parseErr := time.Parse(time.RFC3339, updated); parseErr == nil {
		q.LastUpdated = t
	}
	q.Score = ComputeScore(q.SuccessCount, q.FailureCount, q.StallCount)
	return q, nil
}

// LookupScore is the fast-path shu ranking uses. Errors collapse to the
// neutral default rather than fail-stop the ranking pass, because a missing
// quality signal is not worth aborting a propose cycle over.
func (qs *QualityStore) LookupScore(ctx context.Context, ability, category string) float64 {
	q, err := qs.Lookup(ctx, ability, category)
	if err != nil {
		return DefaultQualityScore
	}
	return q.Score
}

// List returns every stored quality row, ordered by ability then category.
// Used by `ko evaluate-proposals --show` and by tests.
func (qs *QualityStore) List(ctx context.Context) ([]ProposalQuality, error) {
	rows, err := qs.db.QueryContext(ctx, `
		SELECT ability, category, success_count, failure_count, stall_count, total_count, last_updated
		FROM proposal_quality
		ORDER BY ability, category`)
	if err != nil {
		return nil, fmt.Errorf("list proposal_quality: %w", err)
	}
	defer rows.Close()

	var out []ProposalQuality
	for rows.Next() {
		var q ProposalQuality
		var updated string
		if err := rows.Scan(&q.Ability, &q.Category, &q.SuccessCount, &q.FailureCount, &q.StallCount, &q.TotalCount, &updated); err != nil {
			return nil, fmt.Errorf("scan proposal_quality: %w", err)
		}
		if t, parseErr := time.Parse(time.RFC3339, updated); parseErr == nil {
			q.LastUpdated = t
		}
		q.Score = ComputeScore(q.SuccessCount, q.FailureCount, q.StallCount)
		out = append(out, q)
	}
	return out, rows.Err()
}

// Replace atomically replaces the entire proposal_quality table with the
// provided rows. DELETE FROM proposal_quality and all INSERT statements
// execute inside a single transaction — if any INSERT fails the DELETE is
// rolled back and the original data is preserved.
//
// Replace is designed for stateless re-evaluation runs: the caller
// aggregates fresh counts in-memory from the current proposals and tracker
// state, then calls Replace once so repeated runs are idempotent.
func (qs *QualityStore) Replace(ctx context.Context, rows []ProposalQuality) error {
	tx, err := qs.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("replace quality: begin transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck — intentional best-effort rollback

	if _, err := tx.ExecContext(ctx, `DELETE FROM proposal_quality`); err != nil {
		return fmt.Errorf("replace quality: delete: %w", err)
	}

	now := time.Now().UTC().Format(time.RFC3339)
	for _, r := range rows {
		if r.Ability == "" || r.Category == "" {
			return fmt.Errorf("replace quality: ability and category required (got %q/%q)", r.Ability, r.Category)
		}
		ts := now
		if !r.LastUpdated.IsZero() {
			ts = r.LastUpdated.UTC().Format(time.RFC3339)
		}
		if _, err := tx.ExecContext(ctx, `INSERT INTO proposal_quality
			(ability, category, success_count, failure_count, stall_count, total_count, last_updated)
			VALUES (?, ?, ?, ?, ?, ?, ?)`,
			r.Ability, r.Category, r.SuccessCount, r.FailureCount, r.StallCount, r.TotalCount, ts,
		); err != nil {
			return fmt.Errorf("replace quality: insert %s/%s: %w", r.Ability, r.Category, err)
		}
	}

	if err := tx.Commit(); err != nil {
		return fmt.Errorf("replace quality: commit: %w", err)
	}
	return nil
}

// QualityEvalSummary is the aggregate counts ko evaluate-proposals returns.
type QualityEvalSummary struct {
	Processed int `json:"processed"`
	Success   int `json:"success"`
	Failure   int `json:"failure"`
	Stall     int `json:"stall"`
	Pending   int `json:"pending"`
	Skipped   int `json:"skipped"`
}

// Increment bumps the counter matching the outcome. Skipped and Processed
// are maintained by the caller since they depend on the iteration source.
func (s *QualityEvalSummary) Increment(outcome ProposalOutcome) {
	switch outcome {
	case OutcomeSuccess:
		s.Success++
	case OutcomeFailure:
		s.Failure++
	case OutcomeStall:
		s.Stall++
	case OutcomePending:
		s.Pending++
	}
}
