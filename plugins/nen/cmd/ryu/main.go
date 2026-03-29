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
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
)

const (
	ryuAbility    = "cost-analysis"
	window7d      = -7 * 24 * time.Hour
	minOutputLen  = 200
)

func main() {
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

	var opusCost, sonnetCost float64
	var opusCount, sonnetCount, opusSuccess, sonnetSuccess int
	var opusModels, sonnetModels []string

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
		}
	}

	if opusCount == 0 || sonnetCount == 0 {
		return nil, nil
	}

	opusCPP := opusCost / float64(opusCount)
	sonnetCPP := sonnetCost / float64(sonnetCount)

	if sonnetCPP == 0 || opusCPP/sonnetCPP < 2.0 {
		return nil, nil
	}

	multiplier := opusCPP / sonnetCPP
	opusSuccessRate := float64(opusSuccess) / float64(opusCount) * 100
	sonnetSuccessRate := float64(sonnetSuccess) / float64(sonnetCount) * 100

	sev := scan.SeverityMedium
	if multiplier > 10 {
		sev = scan.SeverityHigh
	}

	return &scan.Finding{
		ID:       ryuFindingID(),
		Ability:  ryuAbility,
		Category: "model-efficiency",
		Severity: sev,
		Title:    fmt.Sprintf("Opus phases cost %.1fx more per phase than Sonnet (7d)", multiplier),
		Description: fmt.Sprintf(
			"Opus: $%.4f/phase (%d phases, %.0f%% success) vs Sonnet: $%.4f/phase (%d phases, %.0f%% success). "+
				"A %.1fx cost multiplier suggests lower-complexity phases could run on Sonnet. "+
				"Check persona assignments and routing rules.",
			opusCPP, opusCount, opusSuccessRate,
			sonnetCPP, sonnetCount, sonnetSuccessRate,
			multiplier,
		),
		Scope:  scan.Scope{Kind: "metrics", Value: "phases"},
		Source: "ryu",
		Evidence: []scan.Evidence{{
			Kind: "metric",
			Raw: fmt.Sprintf(
				"opus_models=%s opus_phases=%d opus_cost=$%.4f opus_cpp=$%.4f opus_success=%.0f%% | "+
					"sonnet_models=%s sonnet_phases=%d sonnet_cost=$%.4f sonnet_cpp=$%.4f sonnet_success=%.0f%%",
				strings.Join(opusModels, ","), opusCount, opusCost, opusCPP, opusSuccessRate,
				strings.Join(sonnetModels, ","), sonnetCount, sonnetCost, sonnetCPP, sonnetSuccessRate,
			),
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

func scanOutputWaste(ctx context.Context, db *sql.DB) ([]scan.Finding, error) {
	since := time.Now().UTC().Add(window7d)

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
			"%d phases produced <%d chars output while consuming $%.4f (7d)",
			len(wastes), minOutputLen, totalWasteCost,
		),
		Description: fmt.Sprintf(
			"%d completed phases consumed $%.4f but produced fewer than %d characters of output. "+
				"These phases may have empty objectives, hit timeouts, or lack clear deliverables. "+
				"Review their objectives and gate criteria.",
			len(wastes), totalWasteCost, minOutputLen,
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
		count   int
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
		info, err := entry.Info()
		if err != nil {
			continue
		}
		if info.ModTime().Before(cutoff) {
			continue
		}

		path := filepath.Join(eventsDir, entry.Name())
		count, err := scan.CountPhaseRetryingEvents(ctx, path, cutoff)
		if err != nil || count == 0 {
			continue
		}

		missionID := strings.TrimSuffix(entry.Name(), ".jsonl")
		perMission[missionID] = &missionRetries{count: count, modTime: info.ModTime()}
		totalRetries += count
	}

	if totalRetries == 0 {
		return nil, nil
	}

	evidence := make([]scan.Evidence, 0, len(perMission))
	for missionID, mr := range perMission {
		evidence = append(evidence, scan.Evidence{
			Kind:       "event_log",
			Raw:        fmt.Sprintf("mission=%s retry_events=%d", missionID, mr.count),
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
				"Each retry represents re-execution cost. Check the missions with the most retries "+
				"for unstable phases or under-specified objectives.",
			totalRetries, len(perMission),
		),
		Scope:    scan.Scope{Kind: "events", Value: eventsDir},
		Source:   "ryu",
		Evidence: evidence,
		FoundAt:  time.Now().UTC(),
	}, nil
}

func ryuFindingID() string {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err != nil {
		return fmt.Sprintf("ryu-%d", time.Now().UnixNano())
	}
	return "ryu-" + hex.EncodeToString(b)
}
