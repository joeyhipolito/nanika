package main

import (
	"context"
	"database/sql"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nen/ko"
)

// ── ko evaluate-proposals ───────────────────────────────────────────────────
//
// Reads the shu proposals table + current tracker issue state, classifies
// each proposal's outcome (success, failure, stall, pending), and upserts
// the counts into the proposal_quality table that shu's ranking consults.
//
// A proposal is identified by (ability, category, tracker_issue) which shu
// records alongside the existing dedup_key when it fires. Proposals missing
// any of those fields (older rows before the schema extension) are skipped.

// defaultProposalsDBPath mirrors the canonical location shu writes to.
// ALLUKA_HOME (from scan.Dir) takes precedence, then $HOME/.alluka/nen.
func defaultProposalsDBPath() string {
	if dir := os.Getenv("ALLUKA_HOME"); dir != "" {
		return filepath.Join(dir, "nen", "proposals.db")
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "nen", "proposals.db")
}

// proposalRow is a single row from the shu proposals table, keyed on
// dedup_key, with the ability/category/tracker_issue enrichment columns
// required for quality scoring.
type proposalRow struct {
	DedupKey     string
	Ability      string
	Category     string
	TrackerIssue string
}

// loadProposals returns every row from the proposals table that has the
// enrichment columns populated. Rows missing ability, category, or
// tracker_issue are skipped in the caller since there is no way to score
// them.
func loadProposals(ctx context.Context, db *sql.DB) ([]proposalRow, error) {
	rows, err := db.QueryContext(ctx, `
		SELECT dedup_key, ability, category, tracker_issue
		FROM proposals`)
	if err != nil {
		return nil, fmt.Errorf("query proposals: %w", err)
	}
	defer rows.Close()

	var out []proposalRow
	for rows.Next() {
		var p proposalRow
		if err := rows.Scan(&p.DedupKey, &p.Ability, &p.Category, &p.TrackerIssue); err != nil {
			return nil, fmt.Errorf("scan proposal row: %w", err)
		}
		out = append(out, p)
	}
	return out, rows.Err()
}

// trackerItem is the minimal slice of the tracker CLI JSON shape we need.
// We re-declare it here to avoid a cmd/shu ↔ cmd/ko circular dep.
type trackerItem struct {
	ID        string  `json:"id"`
	SeqID     *int64  `json:"seq_id"`
	Status    string  `json:"status"`
	UpdatedAt string  `json:"updated_at"`
	CreatedAt string  `json:"created_at"`
	Labels    *string `json:"labels"`
}

type trackerItemsResponse struct {
	Items []trackerItem `json:"items"`
}

// fetchTrackerIssues shells out to `tracker query items --json`, matching
// what shu propose does. If the tracker binary is missing, the caller
// surfaces a clear error — ko quality evaluation is pointless without it.
func fetchTrackerIssues(ctx context.Context) ([]trackerItem, error) {
	if _, err := exec.LookPath("tracker"); err != nil {
		return nil, fmt.Errorf("tracker plugin not found in PATH: %w", err)
	}
	cmd := exec.CommandContext(ctx, "tracker", "query", "items", "--json")
	out, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("tracker query items: %w", err)
	}
	var resp trackerItemsResponse
	if err := json.Unmarshal(out, &resp); err != nil {
		return nil, fmt.Errorf("parse tracker items: %w", err)
	}
	return resp.Items, nil
}

// buildTrackerIndex returns a map keyed by both the raw ID (trk-abc) and
// the display ID (TRK-42) so shu's tracker_issue values match on either
// form. shu records the displayID in most paths but may record the raw ID
// via the bash dispatch shim, so we index both.
func buildTrackerIndex(items []trackerItem) map[string]trackerItem {
	idx := make(map[string]trackerItem, len(items)*2)
	for _, it := range items {
		if it.ID != "" {
			idx[it.ID] = it
		}
		if it.SeqID != nil {
			idx[fmt.Sprintf("TRK-%d", *it.SeqID)] = it
		}
	}
	return idx
}

// trackerUpdateAge returns the duration since the issue was last updated.
// Falls back to created_at if updated_at is missing, and to zero if both
// are missing — a zero age keeps a recent-looking issue in the pending
// bucket rather than incorrectly marking it as a stall.
func trackerUpdateAge(it trackerItem, now time.Time) time.Duration {
	ts := it.UpdatedAt
	if ts == "" {
		ts = it.CreatedAt
	}
	if ts == "" {
		return 0
	}
	t, err := time.Parse(time.RFC3339, ts)
	if err != nil {
		return 0
	}
	return now.Sub(t)
}

// qualityKey indexes in-memory aggregation by (ability, category).
type qualityKey struct{ ability, category string }

// evaluateProposals aggregates proposal outcomes from proposals+tracker state
// in-memory, then atomically replaces the proposal_quality table with fresh
// counts. Calling it twice with the same inputs produces identical counts —
// there is no accumulation across runs.
func evaluateProposals(ctx context.Context, qs *ko.QualityStore, proposals []proposalRow, items []trackerItem, stallThreshold time.Duration) (ko.QualityEvalSummary, error) {
	index := buildTrackerIndex(items)
	now := time.Now().UTC()

	type counts struct{ success, failure, stall int }
	agg := map[qualityKey]*counts{}
	summary := ko.QualityEvalSummary{}

	for _, p := range proposals {
		summary.Processed++
		if p.Ability == "" || p.Category == "" || p.TrackerIssue == "" {
			summary.Skipped++
			continue
		}
		it, ok := index[p.TrackerIssue]
		if !ok {
			summary.Skipped++
			continue
		}
		outcome := ko.ClassifyOutcome(it.Status, trackerUpdateAge(it, now), stallThreshold)
		summary.Increment(outcome)
		if outcome == ko.OutcomePending {
			continue
		}
		k := qualityKey{p.Ability, p.Category}
		if agg[k] == nil {
			agg[k] = &counts{}
		}
		switch outcome {
		case ko.OutcomeSuccess:
			agg[k].success++
		case ko.OutcomeFailure:
			agg[k].failure++
		case ko.OutcomeStall:
			agg[k].stall++
		}
	}

	rows := make([]ko.ProposalQuality, 0, len(agg))
	for k, c := range agg {
		rows = append(rows, ko.ProposalQuality{
			Ability:      k.ability,
			Category:     k.category,
			SuccessCount: c.success,
			FailureCount: c.failure,
			StallCount:   c.stall,
			TotalCount:   c.success + c.failure + c.stall,
			LastUpdated:  now,
		})
	}
	if err := qs.Replace(ctx, rows); err != nil {
		return summary, fmt.Errorf("replace quality scores: %w", err)
	}
	return summary, nil
}

func cmdEvaluateProposals(ctx context.Context, args []string) error {
	fs := flag.NewFlagSet("evaluate-proposals", flag.ContinueOnError)
	dbPath := fs.String("proposals-db", defaultProposalsDBPath(), "path to proposals.db")
	stallThreshold := fs.Duration("stall-threshold", ko.DefaultStallThreshold,
		"open/in-progress age past which a proposal counts as a stall")
	showList := fs.Bool("show", false, "print all quality rows after the update")
	jsonOut := fs.Bool("json", false, "JSON output")
	if err := fs.Parse(args); err != nil {
		return err
	}

	if _, err := os.Stat(*dbPath); os.IsNotExist(err) {
		return fmt.Errorf("proposals.db not found at %s — run `shu propose` first", *dbPath)
	}

	// Open with WAL + busy timeout so we play nicely with any concurrent
	// shu propose run that might hold the DB briefly.
	db, err := sql.Open("sqlite", *dbPath+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return fmt.Errorf("open proposals.db: %w", err)
	}
	defer db.Close()

	qs, err := ko.NewQualityStore(db)
	if err != nil {
		return fmt.Errorf("init quality store: %w", err)
	}

	proposals, err := loadProposals(ctx, db)
	if err != nil {
		// Surface a clearer message when the enrichment columns are missing
		// — this is the most common first-run failure before the schema
		// migration rolls out on the shu side.
		if strings.Contains(err.Error(), "no such column") {
			return fmt.Errorf("proposals table missing ability/category/tracker_issue columns — update shu first: %w", err)
		}
		return err
	}

	items, err := fetchTrackerIssues(ctx)
	if err != nil {
		return err
	}

	summary, err := evaluateProposals(ctx, qs, proposals, items, *stallThreshold)
	if err != nil {
		return err
	}

	if *jsonOut {
		type output struct {
			Summary ko.QualityEvalSummary `json:"summary"`
			Scores  []ko.ProposalQuality  `json:"scores,omitempty"`
		}
		out := output{Summary: summary}
		if *showList {
			scores, err := qs.List(ctx)
			if err != nil {
				return fmt.Errorf("list scores: %w", err)
			}
			out.Scores = scores
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Printf("Processed %d proposal(s): success=%d failure=%d stall=%d pending=%d skipped=%d\n",
		summary.Processed, summary.Success, summary.Failure, summary.Stall, summary.Pending, summary.Skipped)

	if *showList {
		scores, err := qs.List(ctx)
		if err != nil {
			return fmt.Errorf("list scores: %w", err)
		}
		if len(scores) == 0 {
			fmt.Println("No quality data yet.")
			return nil
		}
		fmt.Printf("\n%-20s  %-25s  %5s  %5s  %5s  %5s  %6s\n",
			"ABILITY", "CATEGORY", "OK", "FAIL", "STALL", "TOTAL", "SCORE")
		fmt.Println(strings.Repeat("-", 80))
		for _, q := range scores {
			fmt.Printf("%-20s  %-25s  %5d  %5d  %5d  %5d  %6.3f\n",
				q.Ability, q.Category, q.SuccessCount, q.FailureCount, q.StallCount, q.TotalCount, q.Score)
		}
	}
	return nil
}
