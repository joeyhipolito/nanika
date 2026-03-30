// ryu — cost-analysis scanner.
// Queries metrics.db and event logs to surface cost trends, model efficiency gaps,
// retry waste, and minimal-output phases.
//
// Usage: ryu --scope <JSON>
// Output: []Finding JSON on stdout.
package main

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
)

const (
	ryuAbility         = "cost-analysis"
	window7d           = -7 * 24 * time.Hour
	minOutputLen       = 200
	minOutputLenCoord  = 50
)

func main() {
	if len(os.Args) > 1 && os.Args[1] == "report" {
		if err := runReport(os.Args[2:]); err != nil {
			fmt.Fprintf(os.Stderr, "ryu report: %v\n", err)
			os.Exit(1)
		}
		return
	}

	var scopeJSON string
	flag.StringVar(&scopeJSON, "scope", "{}", "JSON-encoded scan scope")
	flag.Parse()

	var scope scan.Scope
	if err := json.Unmarshal([]byte(scopeJSON), &scope); err != nil {
		fmt.Fprintf(os.Stderr, "ryu: invalid --scope JSON: %v\n", err)
		os.Exit(1)
	}

	findings, scanErr := ryuScan(context.Background(), scope)
	var warnings []string
	if scanErr != nil {
		warnings = append(warnings, fmt.Sprintf("ryu: %v", scanErr))
	}

	envelope := scan.NewEnvelope("ryu", ryuAbility, findings, warnings)
	out, err := json.Marshal(envelope)
	if err != nil {
		fmt.Fprintf(os.Stderr, "ryu: marshalling envelope: %v\n", err)
		os.Exit(1)
	}
	fmt.Println(string(out))
}

func ryuScan(ctx context.Context, _ scan.Scope) ([]scan.Finding, error) {
	db, err := scan.OpenMetricsDB()
	if err != nil {
		return nil, fmt.Errorf("open metrics.db: %w", err)
	}
	defer db.Close()

	var findings []scan.Finding
	var errs []string

	if f, err := scanCostTrend(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("cost trend: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if f, err := scanModelEfficiency(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("model efficiency: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if f, err := scanRetryCost(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("retry cost: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if ff, err := scanOutputWaste(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("output waste: %v", err))
	} else {
		findings = append(findings, ff...)
	}

	if f, err := scanEventLogRetries(ctx); err != nil {
		errs = append(errs, fmt.Sprintf("event log retries: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if f, err := scanLimitProximity(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("limit proximity: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if f, err := scanSpendThreshold(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("spend threshold: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if f, err := scanCostAnomaly(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("cost anomaly: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if ff, err := scanModelDowngrade(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("model downgrade: %v", err))
	} else {
		findings = append(findings, ff...)
	}

	if f, err := scanCacheEfficiency(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("cache efficiency: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if f, err := scanBurnRateSpike(ctx, db); err != nil {
		errs = append(errs, fmt.Sprintf("burn rate spike: %v", err))
	} else if f != nil {
		findings = append(findings, *f)
	}

	if len(errs) > 0 {
		return findings, fmt.Errorf("%s", strings.Join(errs, "; "))
	}
	return findings, nil
}

func scanCostTrend(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	since := time.Now().UTC().Add(window7d)
	rows, err := db.QueryContext(ctx,
		`SELECT id, started_at, cost_usd_total, status FROM missions WHERE started_at >= ? ORDER BY started_at ASC`,
		since.Format(time.RFC3339),
	)
	if err != nil {
		return nil, fmt.Errorf("querying missions: %w", err)
	}
	defer rows.Close()

	type missionRow struct {
		id        string
		startedAt time.Time
		costUSD   float64
		status    string
	}

	var missions []missionRow
	for rows.Next() {
		var m missionRow
		var startedAtStr string
		if err := rows.Scan(&m.id, &startedAtStr, &m.costUSD, &m.status); err != nil {
			return nil, fmt.Errorf("scanning mission row: %w", err)
		}
		t, err := time.Parse(time.RFC3339, startedAtStr)
		if err != nil {
			return nil, fmt.Errorf("parsing started_at %q: %w", startedAtStr, err)
		}
		m.startedAt = t
		missions = append(missions, m)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	if len(missions) < 4 {
		return nil, nil
	}

	mid := len(missions) / 2
	var firstHalf, secondHalf, total float64
	for _, m := range missions[:mid] {
		firstHalf += m.costUSD
	}
	for _, m := range missions[mid:] {
		secondHalf += m.costUSD
	}
	for _, m := range missions {
		total += m.costUSD
	}

	if firstHalf == 0 || secondHalf/firstHalf < 1.5 {
		return nil, nil
	}

	growth := (secondHalf - firstHalf) / firstHalf * 100
	sev := scan.SeverityMedium
	if growth > 200 {
		sev = scan.SeverityHigh
	}

	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "cost-trend",
		Severity: sev,
		Title:    fmt.Sprintf("Mission cost trending up %.0f%% over 7 days", growth),
		Description: fmt.Sprintf(
			"Mission costs grew %.0f%% in the second half of the 7-day window "+
				"(first half $%.4f, second half $%.4f, total $%.4f across %d missions). "+
				"Review mission complexity, model choices, and retry rates.",
			growth, firstHalf, secondHalf, total, len(missions),
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "missions"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"first_half=$%.4f second_half=$%.4f total_7d=$%.4f growth=%.0f%% missions=%d",
				firstHalf, secondHalf, total, growth, len(missions),
			),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		}},
		FoundAt: time.Now().UTC(),
	}, nil
}

func scanModelEfficiency(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	since := time.Now().UTC().Add(window7d)

	rows, err := db.QueryContext(ctx, `
		SELECT p.model,
		       COUNT(*) as cnt,
		       SUM(p.cost_usd) as total_cost,
		       SUM(p.output_len) as total_output,
		       SUM(CASE WHEN p.status = 'completed' THEN 1 ELSE 0 END) as successes
		FROM phases p
		JOIN missions m ON p.mission_id = m.id
		WHERE p.model != '' AND m.started_at >= ?
		GROUP BY p.model
	`, since.Format(time.RFC3339))
	if err != nil {
		return nil, fmt.Errorf("querying phase models: %w", err)
	}
	defer rows.Close()

	type modelStats struct {
		count       int
		totalCost   float64
		totalOutput int
		successes   int
	}
	stats := make(map[string]*modelStats)

	for rows.Next() {
		var model string
		var ms modelStats
		if err := rows.Scan(&model, &ms.count, &ms.totalCost, &ms.totalOutput, &ms.successes); err != nil {
			return nil, fmt.Errorf("scanning model stats: %w", err)
		}
		stats[model] = &ms
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	var opusCost, sonnetCost, haikuCost float64
	var opusCount, sonnetCount, haikuCount int
	var opusSuccess, sonnetSuccess, haikuSuccess int
	var opusModels, sonnetModels, haikuModels []string

	for model, ms := range stats {
		lower := strings.ToLower(model)
		switch {
		case strings.Contains(lower, "opus"):
			opusCost += ms.totalCost
			opusCount += ms.count
			opusSuccess += ms.successes
			opusModels = append(opusModels, model)
		case strings.Contains(lower, "sonnet"):
			sonnetCost += ms.totalCost
			sonnetCount += ms.count
			sonnetSuccess += ms.successes
			sonnetModels = append(sonnetModels, model)
		case strings.Contains(lower, "haiku"):
			haikuCost += ms.totalCost
			haikuCount += ms.count
			haikuSuccess += ms.successes
			haikuModels = append(haikuModels, model)
		}
	}

	// Build list of active tiers (any with phase data).
	type tierStats struct {
		name      string
		cpp       float64 // cost per phase
		totalCost float64
		count     int
		success   int
		models    []string
	}
	var tiers []tierStats
	if opusCount > 0 {
		tiers = append(tiers, tierStats{"opus", opusCost / float64(opusCount), opusCost, opusCount, opusSuccess, opusModels})
	}
	if sonnetCount > 0 {
		tiers = append(tiers, tierStats{"sonnet", sonnetCost / float64(sonnetCount), sonnetCost, sonnetCount, sonnetSuccess, sonnetModels})
	}
	if haikuCount > 0 {
		tiers = append(tiers, tierStats{"haiku", haikuCost / float64(haikuCount), haikuCost, haikuCount, haikuSuccess, haikuModels})
	}

	// Need at least two tiers to compare.
	if len(tiers) < 2 {
		return nil, nil
	}

	// Find the most and least expensive active tiers.
	mostExp, leastExp := tiers[0], tiers[0]
	for _, t := range tiers[1:] {
		if t.cpp > mostExp.cpp {
			mostExp = t
		}
		if t.cpp < leastExp.cpp {
			leastExp = t
		}
	}

	if leastExp.cpp == 0 || mostExp.cpp/leastExp.cpp < 2.0 {
		return nil, nil
	}

	multiplier := mostExp.cpp / leastExp.cpp
	mostExpSuccessRate := float64(mostExp.success) / float64(mostExp.count) * 100
	leastExpSuccessRate := float64(leastExp.success) / float64(leastExp.count) * 100

	sev := scan.SeverityMedium
	if multiplier > 10 {
		sev = scan.SeverityHigh
	}

	// Build evidence — include all active tiers.
	evParts := make([]string, 0, len(tiers))
	for _, t := range tiers {
		sr := float64(t.success) / float64(t.count) * 100
		evParts = append(evParts, fmt.Sprintf(
			"%s_models=%s %s_phases=%d %s_cost=$%.4f %s_cpp=$%.4f %s_success=%.0f%%",
			t.name, strings.Join(t.models, ","),
			t.name, t.count,
			t.name, t.totalCost,
			t.name, t.cpp,
			t.name, sr,
		))
	}

	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "model-efficiency",
		Severity: sev,
		Title:    fmt.Sprintf("%s phases cost %.1fx more per phase than %s (7d)", mostExp.name, multiplier, leastExp.name),
		Description: fmt.Sprintf(
			"%s: $%.4f/phase (%d phases, %.0f%% success) vs %s: $%.4f/phase (%d phases, %.0f%% success). "+
				"A %.1fx cost multiplier suggests lower-complexity phases could run on %s. "+
				"Check persona assignments and routing rules.",
			mostExp.name, mostExp.cpp, mostExp.count, mostExpSuccessRate,
			leastExp.name, leastExp.cpp, leastExp.count, leastExpSuccessRate,
			multiplier, leastExp.name,
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "phases"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind:       "metric",
			Raw:        strings.Join(evParts, " | "),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		}},
		FoundAt: time.Now().UTC(),
	}, nil
}

func scanRetryCost(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	since := time.Now().UTC().Add(window7d)

	rows, err := db.QueryContext(ctx, `
		SELECT p.retries, p.cost_usd, m.id
		FROM phases p
		JOIN missions m ON p.mission_id = m.id
		WHERE p.retries > 0 AND m.started_at >= ?
	`, since.Format(time.RFC3339))
	if err != nil {
		return nil, fmt.Errorf("querying retry phases: %w", err)
	}
	defer rows.Close()

	var totalRetryCost float64
	var totalRetries int
	missionSet := make(map[string]struct{})

	for rows.Next() {
		var retries int
		var costUSD float64
		var missionID string
		if err := rows.Scan(&retries, &costUSD, &missionID); err != nil {
			return nil, fmt.Errorf("scanning retry row: %w", err)
		}
		totalRetryCost += costUSD * float64(retries) / float64(retries+1)
		totalRetries += retries
		missionSet[missionID] = struct{}{}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	if totalRetries == 0 || totalRetryCost < 0.0001 {
		return nil, nil
	}

	sev := scan.SeverityLow
	switch {
	case totalRetryCost > 0.50:
		sev = scan.SeverityHigh
	case totalRetryCost > 0.10:
		sev = scan.SeverityMedium
	}

	affectedMissions := len(missionSet)
	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "retry-waste",
		Severity: sev,
		Title:    fmt.Sprintf("$%.4f attributed to retries across %d missions (7d)", totalRetryCost, affectedMissions),
		Description: fmt.Sprintf(
			"%d retries across %d missions in the last 7 days consumed an estimated $%.4f. "+
				"Retries indicate flaky phases, under-specified objectives, or timeout issues. "+
				"Review gate failures and phase prompts to reduce retry rates.",
			totalRetries, affectedMissions, totalRetryCost,
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "phases"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"retry_cost=$%.4f total_retries=%d affected_missions=%d",
				totalRetryCost, totalRetries, affectedMissions,
			),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		}},
		FoundAt: time.Now().UTC(),
	}, nil
}

// isCoordinatorPhase returns true for phases that naturally produce minimal output
// and should not be held to the standard 200-char output threshold.
// Review, coordinate, and staff-code-reviewer phases are coordinators by definition.
func isCoordinatorPhase(name, persona string) bool {
	lower := strings.ToLower(name)
	if strings.Contains(lower, "review") || strings.Contains(lower, "coordinate") {
		return true
	}
	return strings.ToLower(persona) == "staff-code-reviewer"
}

func scanOutputWaste(ctx context.Context, db *sql.DB) ([]scan.Finding, error) {
	since := time.Now().UTC().Add(window7d)

	// The SQL upper bound remains minOutputLen so that coordinator phases (which
	// produce 50–200 chars) are included; Go filtering below applies the correct
	// threshold per phase type.
	rows, err := db.QueryContext(ctx, `
		SELECT p.name, p.persona, p.cost_usd, p.output_len, m.id, m.task
		FROM phases p
		JOIN missions m ON p.mission_id = m.id
		WHERE p.output_len < ?
		  AND p.cost_usd > 0.001
		  AND p.status = 'completed'
		  AND m.started_at >= ?
		ORDER BY p.cost_usd DESC
		LIMIT 20
	`, minOutputLen, since.Format(time.RFC3339))
	if err != nil {
		return nil, fmt.Errorf("querying output waste: %w", err)
	}
	defer rows.Close()

	type wasteRow struct {
		phaseName string
		persona   string
		costUSD   float64
		outputLen int
		missionID string
		task      string
	}

	var wastes []wasteRow
	for rows.Next() {
		var w wasteRow
		if err := rows.Scan(&w.phaseName, &w.persona, &w.costUSD, &w.outputLen, &w.missionID, &w.task); err != nil {
			return nil, fmt.Errorf("scanning waste row: %w", err)
		}
		wastes = append(wastes, w)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	// Exclude coordinator phases that produced above the lower threshold —
	// review and coordinate phases naturally produce little output and are not waste.
	filtered := wastes[:0]
	for _, w := range wastes {
		if isCoordinatorPhase(w.phaseName, w.persona) && w.outputLen >= minOutputLenCoord {
			continue
		}
		filtered = append(filtered, w)
	}
	wastes = filtered

	if len(wastes) == 0 {
		return nil, nil
	}

	var totalWasteCost float64
	evidence := make([]scan.Evidence, 0, len(wastes))
	for _, w := range wastes {
		totalWasteCost += w.costUSD
		evidence = append(evidence, scan.Evidence{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"mission=%s phase=%s persona=%s cost=$%.4f output_len=%d",
				w.missionID, w.phaseName, w.persona, w.costUSD, w.outputLen,
			),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		})
	}

	sev := scan.SeverityLow
	if totalWasteCost > 0.05 {
		sev = scan.SeverityMedium
	}

	return []scan.Finding{{
		ID:      ryuFindingID(),
		Ability: ryuAbility,
		Category: "output-waste",
		Severity: sev,
		Title: fmt.Sprintf(
			"%d implementation phases produced <%d chars output while consuming $%.4f (7d)",
			len(wastes), minOutputLen, totalWasteCost,
		),
		Description: fmt.Sprintf(
			"%d completed implementation phases consumed $%.4f but produced fewer than %d characters of output "+
				"(coordinator phases like review/coordinate are excluded — they use a %d-char floor). "+
				"These phases may have empty objectives, hit timeouts, or lack clear deliverables. "+
				"Review their objectives and gate criteria.",
			len(wastes), totalWasteCost, minOutputLen, minOutputLenCoord,
		),
		Scope:    scan.Scope{Kind: "metrics", Value: "phases"},
		Source:   "ryu",
		Evidence: evidence,
		FoundAt:  time.Now().UTC(),
	}}, nil
}

func scanEventLogRetries(ctx context.Context) (*scan.Finding, error) {
	eventsDir, err := scan.EventsDir()
	if err != nil {
		return nil, fmt.Errorf("events dir: %w", err)
	}

	entries, err := os.ReadDir(eventsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading events dir: %w", err)
	}

	cutoff := time.Now().UTC().Add(window7d)

	type missionRetries struct {
		info    scan.MissionRetryInfo
		modTime time.Time
	}
	perMission := make(map[string]*missionRetries)
	totalRetries := 0

	for _, entry := range entries {
		if ctx.Err() != nil {
			break
		}
		if !strings.HasSuffix(entry.Name(), ".jsonl") {
			continue
		}
		fileInfo, err := entry.Info()
		if err != nil {
			continue
		}
		if fileInfo.ModTime().Before(cutoff) {
			continue
		}

		path := filepath.Join(eventsDir, entry.Name())
		missionInfo, err := scan.CollectMissionRetryInfo(ctx, path, cutoff)
		if err != nil || len(missionInfo.Events) == 0 {
			continue
		}

		missionID := strings.TrimSuffix(entry.Name(), ".jsonl")
		perMission[missionID] = &missionRetries{info: missionInfo, modTime: fileInfo.ModTime()}
		totalRetries += len(missionInfo.Events)
	}

	if totalRetries == 0 {
		return nil, nil
	}

	evidence := make([]scan.Evidence, 0, len(perMission))
	for missionID, mr := range perMission {
		var b strings.Builder
		fmt.Fprintf(&b, "mission=%s retries=%d", missionID, len(mr.info.Events))
		if mr.info.Task != "" {
			fmt.Fprintf(&b, " task=%q", mr.info.Task)
		}
		for _, re := range mr.info.Events {
			fmt.Fprintf(&b, "\n  phase=%s", re.PhaseID)
			if re.WorkerID != "" {
				fmt.Fprintf(&b, " worker=%s", re.WorkerID)
			}
			if re.Attempt > 0 {
				fmt.Fprintf(&b, " attempt=%d", re.Attempt)
			}
			if re.Error != "" {
				fmt.Fprintf(&b, " error=%q", re.Error)
			}
		}
		evidence = append(evidence, scan.Evidence{
			Kind:       "event_log",
			Raw:        b.String(),
			Source:     filepath.Join(eventsDir, missionID+".jsonl"),
			CapturedAt: mr.modTime,
		})
	}

	sev := scan.SeverityInfo
	switch {
	case totalRetries > 30:
		sev = scan.SeverityMedium
	case totalRetries > 10:
		sev = scan.SeverityLow
	}

	return &scan.Finding{
		ID:      ryuFindingID(),
		Ability: ryuAbility,
		Category: "retry-events",
		Severity: sev,
		Title: fmt.Sprintf(
			"%d phase.retrying events across %d missions (7d event logs)",
			totalRetries, len(perMission),
		),
		Description: fmt.Sprintf(
			"Event logs recorded %d phase.retrying events across %d missions in the last 7 days. "+
				"Each retry represents re-execution cost. Evidence entries include the mission task, "+
				"the phase that retried, and the worker error message for each retry.",
			totalRetries, len(perMission),
		),
		Scope:    scan.Scope{Kind: "events", Value: eventsDir},
		Source:   "ryu",
		Evidence: evidence,
		FoundAt:  time.Now().UTC(),
	}, nil
}

// scanLimitProximity reads the most recent quota_snapshot within the last 5h
// and alerts when estimated_5h_utilization crosses 40/60/80% thresholds.
func scanLimitProximity(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	since := time.Now().UTC().Add(-5 * time.Hour).Format(time.RFC3339)
	row := db.QueryRowContext(ctx, `
		SELECT estimated_5h_utilization, window_5h_cost_usd,
		       window_5h_tokens_in, window_5h_tokens_out, captured_at
		FROM quota_snapshots
		WHERE captured_at >= ?
		ORDER BY captured_at DESC
		LIMIT 1
	`, since)

	var util, cost float64
	var tokIn, tokOut int
	var capturedAtStr string
	if err := row.Scan(&util, &cost, &tokIn, &tokOut, &capturedAtStr); err != nil {
		if err == sql.ErrNoRows {
			return nil, nil
		}
		return nil, fmt.Errorf("querying quota snapshots: %w", err)
	}

	const (
		thresh40 = 0.40
		thresh60 = 0.60
		thresh80 = 0.80
	)
	if util < thresh40 {
		return nil, nil
	}

	pct := util * 100
	sev := scan.SeverityInfo
	label := "approaching"
	switch {
	case util >= thresh80:
		sev = scan.SeverityHigh
		label = "critical"
	case util >= thresh60:
		sev = scan.SeverityMedium
		label = "warning"
	}

	capturedAt, _ := time.Parse(time.RFC3339, capturedAtStr)
	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "limit-proximity",
		Severity: sev,
		Title:    fmt.Sprintf("5h token window at %.0f%% utilization (%s)", pct, label),
		Description: fmt.Sprintf(
			"The rolling 5-hour token window is at %.1f%% utilization "+
				"(%d tokens in, %d tokens out, $%.4f cost in window). "+
				"Claude Max rate limits reset every 5 hours. "+
				"Slow down or defer non-critical missions to avoid hitting the cap.",
			pct, tokIn, tokOut, cost,
		),
		Scope:  scan.Scope{Kind: "quota", Value: "5h-window"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"utilization=%.1f%% window_5h_tokens_in=%d window_5h_tokens_out=%d window_5h_cost_usd=$%.4f",
				pct, tokIn, tokOut, cost,
			),
			Source:     "metrics.db/quota_snapshots",
			CapturedAt: capturedAt,
		}},
		FoundAt: time.Now().UTC(),
	}, nil
}

// scanSpendThreshold checks whether 24h rolling spend has exceeded
// the threshold set in RYU_DAILY_SPEND_THRESHOLD (USD float, e.g. "5.00").
// Returns nil with no finding when the env var is unset.
func scanSpendThreshold(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	threshStr := os.Getenv("RYU_DAILY_SPEND_THRESHOLD")
	if threshStr == "" {
		return nil, nil
	}
	threshold, err := strconv.ParseFloat(threshStr, 64)
	if err != nil || threshold <= 0 {
		return nil, fmt.Errorf("RYU_DAILY_SPEND_THRESHOLD=%q: must be a positive float", threshStr)
	}

	since := time.Now().UTC().Add(-24 * time.Hour).Format(time.RFC3339)
	var totalCost float64
	if err := db.QueryRowContext(ctx, `
		SELECT COALESCE(SUM(cost_usd_total), 0) FROM missions WHERE started_at >= ?
	`, since).Scan(&totalCost); err != nil {
		return nil, fmt.Errorf("querying 24h cost: %w", err)
	}

	if totalCost < threshold {
		return nil, nil
	}

	ratio := totalCost / threshold
	sev := scan.SeverityMedium
	if ratio >= 2.0 {
		sev = scan.SeverityHigh
	}

	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "spend-threshold",
		Severity: sev,
		Title:    fmt.Sprintf("24h spend $%.4f exceeds threshold $%.4f (%.1fx)", totalCost, threshold, ratio),
		Description: fmt.Sprintf(
			"The 24-hour rolling spend of $%.4f has exceeded the configured daily threshold of $%.4f "+
				"(%.1fx over limit). Review active missions or increase RYU_DAILY_SPEND_THRESHOLD if intentional.",
			totalCost, threshold, ratio,
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "missions"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind:       "metric",
			Raw:        fmt.Sprintf("spend_24h=$%.4f threshold=$%.4f ratio=%.2fx", totalCost, threshold, ratio),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		}},
		FoundAt: time.Now().UTC(),
	}, nil
}

// scanCostAnomaly computes the z-score of the most recent mission's cost
// against the 7-day baseline and alerts when it exceeds 2.5 standard deviations.
// Requires at least 5 missions in the 7d window to produce a reliable baseline.
func scanCostAnomaly(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	since := time.Now().UTC().Add(window7d).Format(time.RFC3339)
	rows, err := db.QueryContext(ctx, `
		SELECT id, cost_usd_total
		FROM missions
		WHERE started_at >= ? AND cost_usd_total > 0
		ORDER BY started_at ASC
	`, since)
	if err != nil {
		return nil, fmt.Errorf("querying mission costs: %w", err)
	}
	defer rows.Close()

	type mRow struct {
		id   string
		cost float64
	}
	var missions []mRow
	for rows.Next() {
		var m mRow
		if err := rows.Scan(&m.id, &m.cost); err != nil {
			return nil, fmt.Errorf("scanning mission row: %w", err)
		}
		missions = append(missions, m)
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	if len(missions) < 5 {
		return nil, nil
	}

	baseline := missions[:len(missions)-1]
	latest := missions[len(missions)-1]

	var sum float64
	for _, m := range baseline {
		sum += m.cost
	}
	mean := sum / float64(len(baseline))

	var variance float64
	for _, m := range baseline {
		d := m.cost - mean
		variance += d * d
	}
	stddev := math.Sqrt(variance / float64(len(baseline)))
	if stddev < 0.0001 {
		return nil, nil
	}

	zscore := (latest.cost - mean) / stddev
	const anomalyThreshold = 2.5
	if zscore < anomalyThreshold {
		return nil, nil
	}

	sev := scan.SeverityMedium
	if zscore >= 4.0 {
		sev = scan.SeverityHigh
	}

	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "cost-anomaly",
		Severity: sev,
		Title:    fmt.Sprintf("Mission %s cost $%.4f is %.1f stddevs above 7d baseline", latest.id, latest.cost, zscore),
		Description: fmt.Sprintf(
			"The most recent mission (%s) cost $%.4f, which is %.1f standard deviations above the "+
				"7-day baseline mean of $%.4f (stddev=$%.4f, %d missions in baseline). "+
				"This may indicate an unexpectedly complex mission, a runaway retry loop, or a model routing issue.",
			latest.id, latest.cost, zscore, mean, stddev, len(baseline),
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "missions"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"mission=%s cost=$%.4f mean=$%.4f stddev=$%.4f zscore=%.2f baseline_n=%d",
				latest.id, latest.cost, mean, stddev, zscore, len(baseline),
			),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		}},
		FoundAt: time.Now().UTC(),
	}, nil
}

// scanModelDowngrade finds Opus phases in the 7d window with >95% success rate
// and <500 char average output, grouped by persona. These are downgrade candidates:
// they succeed reliably with minimal output, suggesting Sonnet would suffice.
func scanModelDowngrade(ctx context.Context, db *sql.DB) ([]scan.Finding, error) {
	since := time.Now().UTC().Add(window7d).Format(time.RFC3339)
	rows, err := db.QueryContext(ctx, `
		SELECT p.persona,
		       COUNT(*) AS cnt,
		       SUM(CASE WHEN p.status = 'completed' THEN 1 ELSE 0 END) AS successes,
		       AVG(p.output_len) AS avg_output,
		       SUM(p.cost_usd) AS total_cost
		FROM phases p
		JOIN missions m ON p.mission_id = m.id
		WHERE LOWER(p.model) LIKE '%opus%'
		  AND m.started_at >= ?
		GROUP BY p.persona
		HAVING cnt >= 5
	`, since)
	if err != nil {
		return nil, fmt.Errorf("querying opus phase stats: %w", err)
	}
	defer rows.Close()

	type personaStats struct {
		persona   string
		count     int
		successes int
		avgOutput float64
		totalCost float64
	}
	var candidates []personaStats
	for rows.Next() {
		var ps personaStats
		if err := rows.Scan(&ps.persona, &ps.count, &ps.successes, &ps.avgOutput, &ps.totalCost); err != nil {
			return nil, fmt.Errorf("scanning persona row: %w", err)
		}
		if float64(ps.successes)/float64(ps.count) > 0.95 && ps.avgOutput < 500 {
			candidates = append(candidates, ps)
		}
	}
	if err := rows.Err(); err != nil {
		return nil, err
	}

	if len(candidates) == 0 {
		return nil, nil
	}

	var totalCost float64
	evidence := make([]scan.Evidence, 0, len(candidates))
	for _, ps := range candidates {
		totalCost += ps.totalCost
		sr := float64(ps.successes) / float64(ps.count) * 100
		evidence = append(evidence, scan.Evidence{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"persona=%s phases=%d success_rate=%.0f%% avg_output=%.0f chars total_cost=$%.4f",
				ps.persona, ps.count, sr, ps.avgOutput, ps.totalCost,
			),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		})
	}

	return []scan.Finding{{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "model-downgrade",
		Severity: scan.SeverityMedium,
		Title: fmt.Sprintf(
			"%d persona(s) using Opus with >95%% success + <500 char output — downgrade candidates (7d)",
			len(candidates),
		),
		Description: fmt.Sprintf(
			"%d persona(s) ran on Opus in the last 7 days with >95%% success rates and average output "+
				"under 500 characters, consuming $%.4f total. "+
				"These phases likely do not require Opus-tier reasoning and could be routed to Sonnet "+
				"at 3-5x lower cost per phase. Review persona routing rules in rules.go.",
			len(candidates), totalCost,
		),
		Scope:    scan.Scope{Kind: "metrics", Value: "phases"},
		Source:   "ryu",
		Evidence: evidence,
		FoundAt:  time.Now().UTC(),
	}}, nil
}

// scanCacheEfficiency computes the 7-day cache hit rate as
// tokens_cache_read / (tokens_in + tokens_cache_read) across all phases.
// Alerts when the rate falls below 30%, indicating prompts are not being reused.
func scanCacheEfficiency(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	since := time.Now().UTC().Add(window7d).Format(time.RFC3339)
	var totalIn, totalCacheRead int
	err := db.QueryRowContext(ctx, `
		SELECT COALESCE(SUM(p.tokens_in), 0),
		       COALESCE(SUM(p.tokens_cache_read), 0)
		FROM phases p
		JOIN missions m ON p.mission_id = m.id
		WHERE m.started_at >= ?
		  AND p.tokens_in + p.tokens_cache_read > 0
	`, since).Scan(&totalIn, &totalCacheRead)
	if err != nil {
		return nil, fmt.Errorf("querying cache efficiency: %w", err)
	}

	totalTokens := totalIn + totalCacheRead
	if totalTokens == 0 {
		return nil, nil
	}

	hitRate := float64(totalCacheRead) / float64(totalTokens) * 100
	const minEfficiencyPct = 30.0
	if hitRate >= minEfficiencyPct {
		return nil, nil
	}

	sev := scan.SeverityLow
	if hitRate < 10.0 {
		sev = scan.SeverityMedium
	}

	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "cache-efficiency",
		Severity: sev,
		Title:    fmt.Sprintf("7d cache hit rate %.1f%% is below 30%% threshold", hitRate),
		Description: fmt.Sprintf(
			"Only %.1f%% of tokens in the last 7 days were served from cache "+
				"(%d cache-read tokens out of %d total input tokens). "+
				"Low cache efficiency means prompts are not being reused effectively. "+
				"Review prompt construction: stable system prompts and context should use cache_control breakpoints.",
			hitRate, totalCacheRead, totalTokens,
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "phases"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"cache_hit_rate=%.1f%% tokens_in=%d tokens_cache_read=%d total=%d window=7d",
				hitRate, totalIn, totalCacheRead, totalTokens,
			),
			Source:     "metrics.db",
			CapturedAt: time.Now().UTC(),
		}},
		FoundAt: time.Now().UTC(),
	}, nil
}

// scanBurnRateSpike compares the 1-hour burn rate to the 7-day hourly average.
// Alerts when the 1h rate is 3x or more above the 7d average, indicating a
// sudden spike in mission activity or unexpectedly expensive phases.
func scanBurnRateSpike(ctx context.Context, db *sql.DB) (*scan.Finding, error) {
	now := time.Now().UTC()

	var cost1h float64
	if err := db.QueryRowContext(ctx, `
		SELECT COALESCE(SUM(cost_usd_total), 0) FROM missions WHERE started_at >= ?
	`, now.Add(-time.Hour).Format(time.RFC3339)).Scan(&cost1h); err != nil {
		return nil, fmt.Errorf("querying 1h cost: %w", err)
	}

	var cost7d float64
	if err := db.QueryRowContext(ctx, `
		SELECT COALESCE(SUM(cost_usd_total), 0) FROM missions WHERE started_at >= ?
	`, now.Add(window7d).Format(time.RFC3339)).Scan(&cost7d); err != nil {
		return nil, fmt.Errorf("querying 7d cost: %w", err)
	}

	hourly7d := cost7d / (7 * 24)
	if hourly7d < 0.0001 || cost1h < 0.001 {
		return nil, nil
	}

	spike := cost1h / hourly7d
	const spikeThreshold = 3.0
	if spike < spikeThreshold {
		return nil, nil
	}

	sev := scan.SeverityMedium
	if spike >= 10.0 {
		sev = scan.SeverityHigh
	}

	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "burn-rate-spike",
		Severity: sev,
		Title:    fmt.Sprintf("1h burn rate $%.4f is %.1fx the 7d hourly average ($%.4f/h)", cost1h, spike, hourly7d),
		Description: fmt.Sprintf(
			"Spend in the last hour ($%.4f) is %.1fx higher than the 7-day hourly average ($%.4f/h). "+
				"This indicates a sudden increase in mission activity or cost. "+
				"Check for runaway missions, batch jobs, or unusually expensive phases.",
			cost1h, spike, hourly7d,
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "missions"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"cost_1h=$%.4f hourly_7d_avg=$%.4f spike=%.1fx total_7d=$%.4f",
				cost1h, hourly7d, spike, cost7d,
			),
			Source:     "metrics.db",
			CapturedAt: now,
		}},
		FoundAt: now,
	}, nil
}

// runReport handles the "ryu report" subcommand and writes markdown to stdout.
func runReport(args []string) error {
	fs := flag.NewFlagSet("ryu report", flag.ContinueOnError)
	period := fs.String("period", "", "Report period: daily or weekly")
	missionID := fs.String("mission", "", "Mission ID for per-phase breakdown")
	if err := fs.Parse(args); err != nil {
		return err
	}
	if *missionID == "" && *period == "" {
		return fmt.Errorf("one of --period (daily|weekly) or --mission <id> is required")
	}
	if *missionID != "" && *period != "" {
		return fmt.Errorf("--period and --mission are mutually exclusive")
	}
	if *period != "" && *period != "daily" && *period != "weekly" {
		return fmt.Errorf("--period must be daily or weekly, got %q", *period)
	}

	db, err := scan.OpenMetricsDB()
	if err != nil {
		return fmt.Errorf("open metrics.db: %w", err)
	}
	defer db.Close()

	ctx := context.Background()
	switch {
	case *missionID != "":
		return reportMission(ctx, db, *missionID, os.Stdout)
	case *period == "daily":
		return reportPeriod(ctx, db, time.Now().UTC().Add(-24*time.Hour), "Daily (last 24h)", false, os.Stdout)
	default: // weekly
		return reportPeriod(ctx, db, time.Now().UTC().Add(window7d), "Weekly (last 7d)", true, os.Stdout)
	}
}

// reportPeriod writes a daily or weekly cost digest as markdown.
func reportPeriod(ctx context.Context, db *sql.DB, since time.Time, label string, showTrend bool, w io.Writer) error {
	sinceStr := since.Format(time.RFC3339)
	now := time.Now().UTC()

	fmt.Fprintf(w, "# Ryu Cost Report — %s\n\n", label)
	fmt.Fprintf(w, "_Generated %s_\n\n", now.Format("2006-01-02 15:04:05 UTC"))

	// Mission-level totals.
	var missionCount int
	var totalCost float64
	var tokIn, tokOut, tokCacheCreate, tokCacheRead int
	err := db.QueryRowContext(ctx, `
		SELECT COUNT(*),
		       COALESCE(SUM(cost_usd_total), 0),
		       COALESCE(SUM(tokens_in_total), 0),
		       COALESCE(SUM(tokens_out_total), 0),
		       COALESCE(SUM(tokens_cache_creation_total), 0),
		       COALESCE(SUM(tokens_cache_read_total), 0)
		FROM missions
		WHERE started_at >= ?
	`, sinceStr).Scan(&missionCount, &totalCost, &tokIn, &tokOut, &tokCacheCreate, &tokCacheRead)
	if err != nil {
		return fmt.Errorf("querying mission summary: %w", err)
	}

	cacheHitRate := 0.0
	if totalIn := tokIn + tokCacheRead; totalIn > 0 {
		cacheHitRate = float64(tokCacheRead) / float64(totalIn) * 100
	}

	fmt.Fprintf(w, "## Summary\n\n")
	fmt.Fprintf(w, "| Metric | Value |\n|--------|-------|\n")
	fmt.Fprintf(w, "| Missions | %d |\n", missionCount)
	fmt.Fprintf(w, "| Total cost | $%.4f |\n\n", totalCost)

	fmt.Fprintf(w, "## Tokens\n\n")
	fmt.Fprintf(w, "| Type | Count |\n|------|-------|\n")
	fmt.Fprintf(w, "| Input | %d |\n", tokIn)
	fmt.Fprintf(w, "| Output | %d |\n", tokOut)
	fmt.Fprintf(w, "| Cache creation | %d |\n", tokCacheCreate)
	fmt.Fprintf(w, "| Cache read | %d |\n", tokCacheRead)
	fmt.Fprintf(w, "| **Cache hit rate** | **%.1f%%** |\n\n", cacheHitRate)

	if err := writeModelCostTable(ctx, db, sinceStr, w); err != nil {
		return err
	}
	if err := writeTopMissions(ctx, db, sinceStr, w); err != nil {
		return err
	}
	if err := writeTopPhases(ctx, db, sinceStr, w); err != nil {
		return err
	}
	if err := writeBurnRate(ctx, db, since, w); err != nil {
		return err
	}
	if showTrend {
		if err := writeWeeklyTrend(ctx, db, w); err != nil {
			return err
		}
	}
	return writeQuotaUtil(ctx, db, w)
}

// writeModelCostTable writes cost by model for phases in the given window.
func writeModelCostTable(ctx context.Context, db *sql.DB, sinceStr string, w io.Writer) error {
	rows, err := db.QueryContext(ctx, `
		SELECT p.model, COUNT(*) AS phases, COALESCE(SUM(p.cost_usd), 0) AS total_cost
		FROM phases p
		JOIN missions m ON p.mission_id = m.id
		WHERE p.model != '' AND m.started_at >= ?
		GROUP BY p.model
		ORDER BY total_cost DESC
	`, sinceStr)
	if err != nil {
		return fmt.Errorf("querying model costs: %w", err)
	}
	defer rows.Close()

	fmt.Fprintf(w, "## Cost by Model\n\n")
	fmt.Fprintf(w, "| Model | Phases | Cost |\n|-------|--------|------|\n")
	for rows.Next() {
		var model string
		var phases int
		var cost float64
		if err := rows.Scan(&model, &phases, &cost); err != nil {
			return fmt.Errorf("scanning model cost row: %w", err)
		}
		fmt.Fprintf(w, "| %s | %d | $%.4f |\n", model, phases, cost)
	}
	fmt.Fprintf(w, "\n")
	return rows.Err()
}

// writeTopMissions writes the top 5 most expensive missions in the window.
func writeTopMissions(ctx context.Context, db *sql.DB, sinceStr string, w io.Writer) error {
	rows, err := db.QueryContext(ctx, `
		SELECT id, task, cost_usd_total, status
		FROM missions
		WHERE started_at >= ?
		ORDER BY cost_usd_total DESC
		LIMIT 5
	`, sinceStr)
	if err != nil {
		return fmt.Errorf("querying top missions: %w", err)
	}
	defer rows.Close()

	fmt.Fprintf(w, "## Top 5 Missions by Cost\n\n")
	fmt.Fprintf(w, "| Mission ID | Task | Cost | Status |\n|-----------|------|------|--------|\n")
	for rows.Next() {
		var id, task, status string
		var cost float64
		if err := rows.Scan(&id, &task, &cost, &status); err != nil {
			return fmt.Errorf("scanning top mission row: %w", err)
		}
		if len(task) > 60 {
			task = task[:57] + "..."
		}
		fmt.Fprintf(w, "| %s | %s | $%.4f | %s |\n", id, task, cost, status)
	}
	fmt.Fprintf(w, "\n")
	return rows.Err()
}

// writeTopPhases writes the top 5 most expensive phases in the window.
func writeTopPhases(ctx context.Context, db *sql.DB, sinceStr string, w io.Writer) error {
	rows, err := db.QueryContext(ctx, `
		SELECT p.mission_id, p.name, p.model, p.cost_usd, p.status
		FROM phases p
		JOIN missions m ON p.mission_id = m.id
		WHERE m.started_at >= ?
		ORDER BY p.cost_usd DESC
		LIMIT 5
	`, sinceStr)
	if err != nil {
		return fmt.Errorf("querying top phases: %w", err)
	}
	defer rows.Close()

	fmt.Fprintf(w, "## Top 5 Phases by Cost\n\n")
	fmt.Fprintf(w, "| Mission | Phase | Model | Cost | Status |\n|---------|-------|-------|------|--------|\n")
	for rows.Next() {
		var missionID, name, model, status string
		var cost float64
		if err := rows.Scan(&missionID, &name, &model, &cost, &status); err != nil {
			return fmt.Errorf("scanning top phase row: %w", err)
		}
		fmt.Fprintf(w, "| %s | %s | %s | $%.4f | %s |\n", missionID, name, model, cost, status)
	}
	fmt.Fprintf(w, "\n")
	return rows.Err()
}

// writeBurnRate writes the 1h spend and period hourly average.
func writeBurnRate(ctx context.Context, db *sql.DB, since time.Time, w io.Writer) error {
	now := time.Now().UTC()

	var cost1h float64
	if err := db.QueryRowContext(ctx,
		`SELECT COALESCE(SUM(cost_usd_total), 0) FROM missions WHERE started_at >= ?`,
		now.Add(-time.Hour).Format(time.RFC3339),
	).Scan(&cost1h); err != nil {
		return fmt.Errorf("querying 1h cost: %w", err)
	}

	var periodCost float64
	if err := db.QueryRowContext(ctx,
		`SELECT COALESCE(SUM(cost_usd_total), 0) FROM missions WHERE started_at >= ?`,
		since.Format(time.RFC3339),
	).Scan(&periodCost); err != nil {
		return fmt.Errorf("querying period cost: %w", err)
	}

	windowHours := now.Sub(since).Hours()
	if windowHours < 1 {
		windowHours = 1
	}
	hourlyAvg := periodCost / windowHours

	fmt.Fprintf(w, "## Burn Rate\n\n")
	fmt.Fprintf(w, "| Metric | Value |\n|--------|-------|\n")
	fmt.Fprintf(w, "| Last 1h spend | $%.4f |\n", cost1h)
	fmt.Fprintf(w, "| Period hourly avg | $%.4f/h |\n", hourlyAvg)
	if hourlyAvg > 0 {
		fmt.Fprintf(w, "| 1h vs avg ratio | %.1fx |\n", cost1h/hourlyAvg)
	}
	fmt.Fprintf(w, "\n")
	return nil
}

// writeWeeklyTrend writes a daily cost breakdown for the 7d window.
func writeWeeklyTrend(ctx context.Context, db *sql.DB, w io.Writer) error {
	rows, err := db.QueryContext(ctx, `
		SELECT date(started_at) AS day, COUNT(*) AS missions,
		       COALESCE(SUM(cost_usd_total), 0) AS cost
		FROM missions
		WHERE started_at >= ?
		GROUP BY day
		ORDER BY day ASC
	`, time.Now().UTC().Add(window7d).Format(time.RFC3339))
	if err != nil {
		return fmt.Errorf("querying weekly trend: %w", err)
	}
	defer rows.Close()

	fmt.Fprintf(w, "## Daily Trend (7d)\n\n")
	fmt.Fprintf(w, "| Date | Missions | Cost |\n|------|----------|------|\n")
	for rows.Next() {
		var day string
		var missions int
		var cost float64
		if err := rows.Scan(&day, &missions, &cost); err != nil {
			return fmt.Errorf("scanning daily trend row: %w", err)
		}
		fmt.Fprintf(w, "| %s | %d | $%.4f |\n", day, missions, cost)
	}
	fmt.Fprintf(w, "\n")
	return rows.Err()
}

// writeQuotaUtil writes the most recent 5h quota snapshot.
func writeQuotaUtil(ctx context.Context, db *sql.DB, w io.Writer) error {
	since := time.Now().UTC().Add(-5 * time.Hour).Format(time.RFC3339)
	var util, cost float64
	var tokIn, tokOut int
	var capturedAtStr string
	err := db.QueryRowContext(ctx, `
		SELECT estimated_5h_utilization, window_5h_tokens_in, window_5h_tokens_out,
		       window_5h_cost_usd, captured_at
		FROM quota_snapshots
		WHERE captured_at >= ?
		ORDER BY captured_at DESC
		LIMIT 1
	`, since).Scan(&util, &tokIn, &tokOut, &cost, &capturedAtStr)
	if err != nil {
		if err == sql.ErrNoRows {
			fmt.Fprintf(w, "## Quota Utilization (5h Window)\n\n_No quota snapshots in the last 5 hours._\n\n")
			return nil
		}
		return fmt.Errorf("querying quota snapshot: %w", err)
	}

	label := "normal"
	switch {
	case util >= 0.80:
		label = "CRITICAL"
	case util >= 0.60:
		label = "WARNING"
	case util >= 0.40:
		label = "approaching"
	}

	fmt.Fprintf(w, "## Quota Utilization (5h Window)\n\n")
	fmt.Fprintf(w, "| Metric | Value |\n|--------|-------|\n")
	fmt.Fprintf(w, "| Utilization | %.1f%% (%s) |\n", util*100, label)
	fmt.Fprintf(w, "| Window tokens in | %d |\n", tokIn)
	fmt.Fprintf(w, "| Window tokens out | %d |\n", tokOut)
	fmt.Fprintf(w, "| Window cost | $%.4f |\n", cost)
	fmt.Fprintf(w, "| Snapshot at | %s |\n\n", capturedAtStr)
	return nil
}

// reportMission writes a per-phase breakdown for a single mission.
func reportMission(ctx context.Context, db *sql.DB, missionID string, w io.Writer) error {
	var id, task, status, startedAt string
	var durationS, phasesTotal, phasesCompleted, phasesFailed, phasesSkipped int
	var costTotal float64
	var tokIn, tokOut, tokCacheCreate, tokCacheRead int
	err := db.QueryRowContext(ctx, `
		SELECT id, task, status, started_at, duration_s,
		       phases_total, phases_completed, phases_failed, phases_skipped,
		       cost_usd_total, tokens_in_total, tokens_out_total,
		       tokens_cache_creation_total, tokens_cache_read_total
		FROM missions WHERE id = ?
	`, missionID).Scan(
		&id, &task, &status, &startedAt, &durationS,
		&phasesTotal, &phasesCompleted, &phasesFailed, &phasesSkipped,
		&costTotal, &tokIn, &tokOut, &tokCacheCreate, &tokCacheRead,
	)
	if err != nil {
		if err == sql.ErrNoRows {
			return fmt.Errorf("mission %q not found", missionID)
		}
		return fmt.Errorf("querying mission: %w", err)
	}

	cacheHitRate := 0.0
	if totalIn := tokIn + tokCacheRead; totalIn > 0 {
		cacheHitRate = float64(tokCacheRead) / float64(totalIn) * 100
	}

	fmt.Fprintf(w, "# Mission Report — %s\n\n", id)
	fmt.Fprintf(w, "_Generated %s_\n\n", time.Now().UTC().Format("2006-01-02 15:04:05 UTC"))

	fmt.Fprintf(w, "## Overview\n\n")
	fmt.Fprintf(w, "| Metric | Value |\n|--------|-------|\n")
	fmt.Fprintf(w, "| Task | %s |\n", task)
	fmt.Fprintf(w, "| Status | %s |\n", status)
	fmt.Fprintf(w, "| Started at | %s |\n", startedAt)
	fmt.Fprintf(w, "| Duration | %ds |\n", durationS)
	fmt.Fprintf(w, "| Total cost | $%.4f |\n", costTotal)
	fmt.Fprintf(w, "| Phases | %d total, %d completed, %d failed, %d skipped |\n\n",
		phasesTotal, phasesCompleted, phasesFailed, phasesSkipped)

	fmt.Fprintf(w, "## Tokens\n\n")
	fmt.Fprintf(w, "| Type | Count |\n|------|-------|\n")
	fmt.Fprintf(w, "| Input | %d |\n", tokIn)
	fmt.Fprintf(w, "| Output | %d |\n", tokOut)
	fmt.Fprintf(w, "| Cache creation | %d |\n", tokCacheCreate)
	fmt.Fprintf(w, "| Cache read | %d |\n", tokCacheRead)
	fmt.Fprintf(w, "| **Cache hit rate** | **%.1f%%** |\n\n", cacheHitRate)

	// Per-phase breakdown ordered by cost descending.
	rows, err := db.QueryContext(ctx, `
		SELECT name, persona, model, cost_usd,
		       tokens_in, tokens_out, tokens_cache_read,
		       status, duration_s, retries
		FROM phases
		WHERE mission_id = ?
		ORDER BY cost_usd DESC
	`, missionID)
	if err != nil {
		return fmt.Errorf("querying phases: %w", err)
	}
	defer rows.Close()

	fmt.Fprintf(w, "## Phase Breakdown\n\n")
	fmt.Fprintf(w, "| Phase | Persona | Model | Cost | Tok In | Tok Out | Cache Read | Status | Duration | Retries |\n")
	fmt.Fprintf(w, "|-------|---------|-------|------|--------|---------|------------|--------|----------|---------|\n")
	for rows.Next() {
		var name, persona, model, phaseStatus string
		var phaseCost float64
		var pTokIn, pTokOut, pCacheRead, pDuration, pRetries int
		if err := rows.Scan(&name, &persona, &model, &phaseCost,
			&pTokIn, &pTokOut, &pCacheRead, &phaseStatus, &pDuration, &pRetries); err != nil {
			return fmt.Errorf("scanning phase row: %w", err)
		}
		fmt.Fprintf(w, "| %s | %s | %s | $%.4f | %d | %d | %d | %s | %ds | %d |\n",
			name, persona, model, phaseCost, pTokIn, pTokOut, pCacheRead, phaseStatus, pDuration, pRetries)
	}
	fmt.Fprintf(w, "\n")
	if err := rows.Err(); err != nil {
		return err
	}

	// Cost by model for this mission.
	modelRows, err := db.QueryContext(ctx, `
		SELECT model, COUNT(*) AS phases, COALESCE(SUM(cost_usd), 0) AS total_cost
		FROM phases
		WHERE mission_id = ? AND model != ''
		GROUP BY model
		ORDER BY total_cost DESC
	`, missionID)
	if err != nil {
		return fmt.Errorf("querying mission model costs: %w", err)
	}
	defer modelRows.Close()

	fmt.Fprintf(w, "## Cost by Model\n\n")
	fmt.Fprintf(w, "| Model | Phases | Cost |\n|-------|--------|------|\n")
	for modelRows.Next() {
		var model string
		var phases int
		var cost float64
		if err := modelRows.Scan(&model, &phases, &cost); err != nil {
			return fmt.Errorf("scanning mission model cost row: %w", err)
		}
		fmt.Fprintf(w, "| %s | %d | $%.4f |\n", model, phases, cost)
	}
	fmt.Fprintf(w, "\n")
	return modelRows.Err()
}

func ryuFindingID() string {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err != nil {
		return fmt.Sprintf("ryu-%d", time.Now().UnixNano())
	}
	return "ryu-" + hex.EncodeToString(b)
}
