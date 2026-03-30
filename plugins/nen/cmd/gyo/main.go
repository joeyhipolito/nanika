// gyo — orchestrator-metrics scanner + audit evaluator.
//
// Scanner mode (default): detects mission metric anomalies (z-score analysis)
// and silent worker failures (worker.failed without phase.failed in event logs).
//
// Audit subcommands:
//   - gyo evaluate [workspace-id-or-path]  — evaluate a completed mission
//   - gyo report [workspace-id]            — display a saved audit report
//
// Usage:
//
//	gyo --scope <JSON>                          (scanner mode)
//	gyo evaluate [workspace-id] [--format ...]  (audit evaluate)
//	gyo report [workspace-id] [--format ...]    (audit report)
package main

import (
	"context"
	"crypto/rand"
	"database/sql"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"time"

	"github.com/joeyhipolito/nen/internal/audit"
	"github.com/joeyhipolito/nen/internal/scan"
)

const (
	gyoAbility               = "orchestrator-metrics"
	gyoZThresholdMedium      = 2.5 // z >= 2.5 → medium
	gyoZThresholdHigh        = 3.0 // z >= 3.0 → high
	gyoZThresholdCritical    = 4.0 // z >= 4.0 → critical
	gyoMinMissions           = 10
	gyoMinMissionsFailureRate = 10 // failure-rate requires its own baseline floor
	gyoBaselineDays          = 7
)

func main() {
	if len(os.Args) >= 2 {
		switch os.Args[1] {
		case "evaluate":
			if err := cmdEvaluate(os.Args[2:]); err != nil {
				fmt.Fprintf(os.Stderr, "error: %v\n", err)
				os.Exit(1)
			}
			return
		case "report":
			if err := cmdReport(os.Args[2:]); err != nil {
				fmt.Fprintf(os.Stderr, "error: %v\n", err)
				os.Exit(1)
			}
			return
		}
	}

	// Default: scanner mode
	runScanner()
}

func runScanner() {
	var scopeJSON string
	flag.StringVar(&scopeJSON, "scope", "{}", "JSON-encoded scan scope")
	flag.Parse()

	var scope scan.Scope
	if err := json.Unmarshal([]byte(scopeJSON), &scope); err != nil {
		fmt.Fprintf(os.Stderr, "gyo: invalid --scope JSON: %v\n", err)
		os.Exit(1)
	}

	findings, scanErr := gyoScan(context.Background(), scope)
	var warnings []string
	if scanErr != nil {
		warnings = append(warnings, fmt.Sprintf("gyo: %v", scanErr))
	}

	envelope := scan.NewEnvelope("gyo", gyoAbility, findings, warnings)
	out, err := json.Marshal(envelope)
	if err != nil {
		fmt.Fprintf(os.Stderr, "gyo: marshalling envelope: %v\n", err)
		os.Exit(1)
	}
	fmt.Println(string(out))
}

// ── Audit evaluate subcommand ────────────────────────────────────────────────

func cmdEvaluate(args []string) error {
	fs := flag.NewFlagSet("evaluate", flag.ContinueOnError)
	format := fs.String("format", "text", "output format: text, json, markdown")
	model := fs.String("model", "", "override evaluation model (default: opus)")
	last := fs.Int("last", 1, "evaluate the Nth most recent mission")
	verbose := fs.Bool("verbose", false, "verbose output")
	if err := fs.Parse(args); err != nil {
		return err
	}

	var idOrPath string
	if fs.NArg() > 0 {
		idOrPath = fs.Arg(0)
	}

	wsPath, err := audit.ResolveWorkspace(idOrPath, *last)
	if err != nil {
		return err
	}

	if *verbose {
		fmt.Fprintf(os.Stderr, "[gyo] evaluating workspace: %s\n", wsPath)
	}

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()

	report, err := audit.EvaluateMission(ctx, audit.EvaluateOptions{
		WorkspacePath: wsPath,
		Model:         *model,
		Verbose:       *verbose,
	})
	if err != nil {
		return fmt.Errorf("audit failed: %w", err)
	}

	// Save report for trend tracking
	if err := audit.SaveReport(report); err != nil {
		fmt.Fprintf(os.Stderr, "[gyo] warning: failed to save report: %v\n", err)
	} else if *verbose {
		fmt.Fprintf(os.Stderr, "[gyo] report saved to audits.jsonl\n")
	}

	switch *format {
	case "json":
		out, fmtErr := audit.FormatJSON(report)
		if fmtErr != nil {
			return fmtErr
		}
		fmt.Println(out)
	case "markdown":
		fmt.Print(audit.FormatMarkdown(report))
	default:
		fmt.Print(audit.FormatText(report))
	}

	return nil
}

// ── Audit report subcommand ──────────────────────────────────────────────────

func cmdReport(args []string) error {
	fs := flag.NewFlagSet("report", flag.ContinueOnError)
	format := fs.String("format", "text", "output format: text, json, markdown")
	if err := fs.Parse(args); err != nil {
		return err
	}

	reports, err := audit.LoadReports()
	if err != nil {
		return fmt.Errorf("loading audit reports: %w", err)
	}

	if len(reports) == 0 {
		fmt.Println("No audit reports found. Run `gyo evaluate` first.")
		return nil
	}

	var report *audit.AuditReport
	if fs.NArg() > 0 {
		wsID := fs.Arg(0)
		for i := len(reports) - 1; i >= 0; i-- {
			if reports[i].WorkspaceID == wsID {
				report = &reports[i]
				break
			}
		}
		if report == nil {
			return fmt.Errorf("no audit report found for workspace %s", wsID)
		}
	} else {
		report = &reports[len(reports)-1]
	}

	switch *format {
	case "json":
		out, fmtErr := audit.FormatJSON(report)
		if fmtErr != nil {
			return fmtErr
		}
		fmt.Println(out)
	case "markdown":
		fmt.Print(audit.FormatMarkdown(report))
	default:
		fmt.Print(audit.FormatText(report))
	}

	return nil
}

// ── Scanner implementation ───────────────────────────────────────────────────

func gyoScan(ctx context.Context, _ scan.Scope) ([]scan.Finding, error) {
	dbPath, err := scan.MetricsDBPath()
	if err != nil {
		return nil, fmt.Errorf("metrics db path: %w", err)
	}
	db, err := gyoOpenDB(dbPath)
	if err != nil {
		return nil, fmt.Errorf("opening metrics db: %w", err)
	}
	defer db.Close()

	missions, err := gyoQueryMissions(ctx, db, gyoBaselineDays)
	if err != nil {
		return nil, fmt.Errorf("querying missions: %w", err)
	}

	if len(missions) < gyoMinMissions {
		return nil, nil // not enough history to establish a baseline
	}

	findings := gyoDetectMetricAnomalies(missions)

	configBase, err := scan.Dir()
	if err != nil {
		return findings, fmt.Errorf("resolving config dir: %w", err)
	}
	eventsDir := filepath.Join(configBase, "events")

	silentFindings, err := gyoDetectSilentFailures(missions, eventsDir)
	if err != nil {
		// Non-fatal: return whatever we found so far.
		return findings, fmt.Errorf("scanning event logs: %w", err)
	}
	findings = append(findings, silentFindings...)
	return findings, nil
}

type gyoMissionRow struct {
	ID           string
	DurationSec  int
	PhasesFailed int
	PhasesTotal  int
	RetriesTotal int
	CostUSD      float64
	FailureRate  float64
}

func gyoOpenDB(path string) (*sql.DB, error) {
	db, err := sql.Open("sqlite", path+"?_busy_timeout=5000")
	if err != nil {
		return nil, fmt.Errorf("open metrics db at %s: %w", path, err)
	}
	return db, nil
}

func gyoQueryMissions(ctx context.Context, db *sql.DB, days int) ([]gyoMissionRow, error) {
	rows, err := db.QueryContext(ctx, `
		SELECT id, duration_s, phases_failed, phases_total, retries_total, cost_usd_total
		FROM missions
		WHERE started_at >= datetime('now', '-' || ? || ' days')
		  AND id NOT LIKE 'ws-%'
		  AND phases_total > 0
		ORDER BY started_at DESC
	`, days)
	if err != nil {
		return nil, fmt.Errorf("querying missions: %w", err)
	}
	defer rows.Close()

	var out []gyoMissionRow
	for rows.Next() {
		var m gyoMissionRow
		if err := rows.Scan(
			&m.ID, &m.DurationSec, &m.PhasesFailed,
			&m.PhasesTotal, &m.RetriesTotal, &m.CostUSD,
		); err != nil {
			return nil, fmt.Errorf("scanning mission row: %w", err)
		}
		if m.PhasesTotal > 0 {
			m.FailureRate = float64(m.PhasesFailed) / float64(m.PhasesTotal)
		}
		out = append(out, m)
	}
	return out, rows.Err()
}

type gyoStats struct {
	mean   float64
	stddev float64
}

func gyoComputeStats(vals []float64) gyoStats {
	if len(vals) == 0 {
		return gyoStats{}
	}
	var sum float64
	for _, v := range vals {
		sum += v
	}
	mean := sum / float64(len(vals))
	if len(vals) < 2 {
		return gyoStats{mean: mean}
	}
	var variance float64
	for _, v := range vals {
		d := v - mean
		variance += d * d
	}
	variance /= float64(len(vals) - 1)
	return gyoStats{mean: mean, stddev: math.Sqrt(variance)}
}

func gyoZScore(value float64, st gyoStats) float64 {
	if st.stddev < 1e-9 {
		return 0
	}
	return (value - st.mean) / st.stddev
}

func gyoDetectMetricAnomalies(missions []gyoMissionRow) []scan.Finding {
	n := len(missions)
	costs := make([]float64, n)
	durPerPhase := make([]float64, n) // duration normalized per phase
	failureRates := make([]float64, n)
	retries := make([]float64, n)

	for i, m := range missions {
		costs[i] = m.CostUSD
		durPerPhase[i] = float64(m.DurationSec) / float64(m.PhasesTotal)
		failureRates[i] = m.FailureRate
		retries[i] = float64(m.RetriesTotal)
	}

	costSt := gyoComputeStats(costs)
	durSt := gyoComputeStats(durPerPhase)
	retrySt := gyoComputeStats(retries)

	// Failure-rate baseline requires its own minimum sample floor.
	var failSt gyoStats
	hasFailBaseline := len(missions) >= gyoMinMissionsFailureRate
	if hasFailBaseline {
		failSt = gyoComputeStats(failureRates)
	}

	now := time.Now().UTC()
	var findings []scan.Finding

	for i, m := range missions {
		type check struct {
			metric string
			value  float64
			st     gyoStats
			cat    string
			skip   bool
		}
		checks := []check{
			{"cost_usd", m.CostUSD, costSt, "cost-anomaly", false},
			{"duration_s_per_phase", durPerPhase[i], durSt, "duration-anomaly", false},
			{"failure_rate", m.FailureRate, failSt, "failure-rate-anomaly", !hasFailBaseline},
			{"retries", float64(m.RetriesTotal), retrySt, "retry-anomaly", false},
		}
		for _, c := range checks {
			if c.skip {
				continue
			}
			z := gyoZScore(c.value, c.st)
			if z < gyoZThresholdMedium {
				continue
			}
			sev := gyoSeverity(z)
			findings = append(findings, scan.Finding{
				ID:       gyoFindingID(),
				Ability:  gyoAbility,
				Category: c.cat,
				Severity: sev,
				Title:    fmt.Sprintf("Mission %s: %s anomaly (z=%.2f)", m.ID, c.metric, z),
				Description: fmt.Sprintf(
					"Mission %s has an anomalous %s value of %.4g "+
						"(7d baseline mean=%.4g, stddev=%.4g, z=%.2f).",
					m.ID, c.metric, c.value, c.st.mean, c.st.stddev, z,
				),
				Scope:  scan.Scope{Kind: "mission", Value: m.ID},
				Source: "gyo",
				Evidence: []scan.Evidence{{
					Kind:       "metric",
					Raw:        fmt.Sprintf("%s=%.4g z=%.2f mean=%.4g stddev=%.4g", c.metric, c.value, z, c.st.mean, c.st.stddev),
					Source:     "metrics.db",
					CapturedAt: now,
				}},
				FoundAt: now,
			})
		}
	}
	return findings
}

// gyoSeverity maps a z-score to a severity level.
// z=2.5-3.0 → medium, z=3.0-4.0 → high, z>4.0 → critical.
func gyoSeverity(z float64) scan.Severity {
	switch {
	case z >= gyoZThresholdCritical:
		return scan.SeverityCritical
	case z >= gyoZThresholdHigh:
		return scan.SeverityHigh
	default:
		return scan.SeverityMedium
	}
}

func gyoDetectSilentFailures(missions []gyoMissionRow, eventsDir string) ([]scan.Finding, error) {
	now := time.Now().UTC()
	var findings []scan.Finding

	for _, m := range missions {
		logPath := filepath.Join(eventsDir, m.ID+".jsonl")
		silentPhases, err := scan.FindSilentFailures(logPath)
		if err != nil {
			// Missing or unreadable log is best-effort; keep scanning.
			continue
		}
		for _, phaseID := range silentPhases {
			findings = append(findings, scan.Finding{
				ID:       gyoFindingID(),
				Ability:  gyoAbility,
				Category: "silent-failure",
				Severity: scan.SeverityHigh,
				Title:    fmt.Sprintf("Mission %s phase %s: silent worker failure", m.ID, phaseID),
				Description: fmt.Sprintf(
					"Phase %s in mission %s has a worker.failed event but no corresponding "+
						"phase.failed event. The worker failure was not propagated to the phase "+
						"lifecycle, masking the failure from metrics and alerts.",
					phaseID, m.ID,
				),
				Scope:  scan.Scope{Kind: "mission", Value: m.ID},
				Source: "gyo",
				Evidence: []scan.Evidence{{
					Kind:       "log_line",
					Raw:        fmt.Sprintf("worker.failed without phase.failed for phase_id=%s", phaseID),
					Source:     logPath,
					CapturedAt: now,
				}},
				FoundAt: now,
			})
		}
	}
	return findings, nil
}

func gyoFindingID() string {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err != nil {
		return fmt.Sprintf("gyo-%d", time.Now().UnixNano())
	}
	return "gyo_" + hex.EncodeToString(b)
}
