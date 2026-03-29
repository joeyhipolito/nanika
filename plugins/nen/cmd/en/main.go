// en — system-health scanner.
// Checks orchestrator binary freshness, stale workspaces, learnings.db embedding
// coverage, daemon socket reachability, metrics.db routing quality, and mission activity.
//
// Usage: en --scope <JSON>
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
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
)

const (
	binaryAgeWarningDays = 60
	binaryAgeStaleDays   = 180

	workspaceStaleDays = 7
	workspaceHighCount = 5

	embeddingCoverageGood = 0.80
	embeddingCoverageLow  = 0.50

	deadWeightLow    = 0.10
	deadWeightMedium = 0.30

	routingFallbackThreshold = 30.0

	missionStaleDays     = 7
	missionVeryStaleDays = 30

	daemonDialTimeout = 200 * time.Millisecond
)

const (
	abilitySystemHealth = "system-health"

	categoryBinaryFreshness  = "binary-freshness"
	categoryWorkspaceHygiene = "workspace-hygiene"
	categoryEmbedding        = "embedding-coverage"
	categoryDeadWeight       = "dead-weight"
	categoryDaemonHealth     = "daemon-health"
	categoryRoutingQuality   = "routing-quality"
	categoryMissionActivity  = "mission-activity"
)

func main() {
	var scopeJSON string
	flag.StringVar(&scopeJSON, "scope", "{}", "JSON-encoded scan scope")
	flag.Parse()

	var scope scan.Scope
	if err := json.Unmarshal([]byte(scopeJSON), &scope); err != nil {
		fmt.Fprintf(os.Stderr, "en: invalid --scope JSON: %v\n", err)
		os.Exit(1)
	}

	findings, scanErr := enScan(context.Background(), scope)
	var warnings []string
	if scanErr != nil {
		warnings = append(warnings, fmt.Sprintf("en: %v", scanErr))
	}

	envelope := scan.NewEnvelope("en", abilitySystemHealth, findings, warnings)
	out, err := json.Marshal(envelope)
	if err != nil {
		fmt.Fprintf(os.Stderr, "en: marshalling envelope: %v\n", err)
		os.Exit(1)
	}
	fmt.Println(string(out))
}

func enScan(ctx context.Context, _ scan.Scope) ([]scan.Finding, error) {
	type checkResult struct {
		findings []scan.Finding
		errMsg   string
	}

	checkFns := []func(context.Context) ([]scan.Finding, string){
		checkOrchestratorBinaryAge,
		checkStaleWorkspaces,
		checkLearningsDB,
		checkDaemonSocket,
		checkMetricsDB,
	}

	ch := make(chan checkResult, len(checkFns))
	for _, fn := range checkFns {
		fn := fn
		go func() {
			ff, errMsg := fn(ctx)
			ch <- checkResult{ff, errMsg}
		}()
	}

	var findings []scan.Finding
	var errs []string
	for range checkFns {
		r := <-ch
		findings = append(findings, r.findings...)
		if r.errMsg != "" {
			errs = append(errs, r.errMsg)
		}
	}

	if len(errs) > 0 {
		return findings, fmt.Errorf("%s", strings.Join(errs, "; "))
	}
	return findings, nil
}

func newFinding(ability, category string, sev scan.Severity, title, description string, scope scan.Scope) scan.Finding {
	return scan.Finding{
		ID:          enFindingID(),
		Ability:     ability,
		Category:    category,
		Severity:    sev,
		Title:       title,
		Description: description,
		Scope:       scope,
		Source:      "en",
		FoundAt:     time.Now().UTC(),
	}
}

func checkOrchestratorBinaryAge(_ context.Context) ([]scan.Finding, string) {
	binaryPath, err := exec.LookPath("orchestrator")
	if err != nil {
		return []scan.Finding{newFinding(
			abilitySystemHealth, categoryBinaryFreshness, scan.SeverityMedium,
			"orchestrator binary not found on PATH",
			"The orchestrator binary could not be located. Ensure it is installed and available on $PATH.",
			scan.Scope{Kind: "binary", Value: "orchestrator"},
		)}, ""
	}

	info, err := os.Stat(binaryPath)
	if err != nil {
		return nil, fmt.Sprintf("stat orchestrator binary %q: %v", binaryPath, err)
	}

	ageDays := int(time.Since(info.ModTime()).Hours() / 24)

	var sev scan.Severity
	var title string
	switch {
	case ageDays > binaryAgeStaleDays:
		sev = scan.SeverityMedium
		title = fmt.Sprintf("orchestrator binary is stale (%d days old)", ageDays)
	case ageDays > binaryAgeWarningDays:
		sev = scan.SeverityLow
		title = fmt.Sprintf("orchestrator binary may need updating (%d days old)", ageDays)
	default:
		sev = scan.SeverityInfo
		title = fmt.Sprintf("orchestrator binary is recent (%d days old)", ageDays)
	}

	return []scan.Finding{newFinding(
		abilitySystemHealth, categoryBinaryFreshness, sev, title,
		fmt.Sprintf("Binary at %s was last modified %s.", binaryPath, info.ModTime().Format("2006-01-02")),
		scan.Scope{Kind: "binary", Value: binaryPath},
	)}, ""
}

func checkStaleWorkspaces(_ context.Context) ([]scan.Finding, string) {
	base, err := scan.Dir()
	if err != nil {
		return nil, fmt.Sprintf("resolving config dir: %v", err)
	}

	wsDir := filepath.Join(base, "workspaces")
	entries, err := os.ReadDir(wsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return []scan.Finding{newFinding(
				abilitySystemHealth, categoryWorkspaceHygiene, scan.SeverityInfo,
				"workspaces directory not found",
				fmt.Sprintf("No workspaces directory at %s.", wsDir),
				scan.Scope{Kind: "directory", Value: wsDir},
			)}, ""
		}
		return nil, fmt.Sprintf("reading workspaces dir %q: %v", wsDir, err)
	}

	threshold := time.Now().Add(-workspaceStaleDays * 24 * time.Hour)
	var staleCount int
	for _, entry := range entries {
		info, err := entry.Info()
		if err != nil {
			continue
		}
		if info.ModTime().Before(threshold) {
			staleCount++
		}
	}

	var sev scan.Severity
	var title string
	desc := fmt.Sprintf("%d of %d workspaces in %s have not been modified in %d+ days. Run 'orchestrator cleanup' to remove them.",
		staleCount, len(entries), wsDir, workspaceStaleDays)
	switch {
	case staleCount >= workspaceHighCount:
		sev = scan.SeverityMedium
		title = fmt.Sprintf("%d stale workspaces older than %dd", staleCount, workspaceStaleDays)
	case staleCount > 0:
		sev = scan.SeverityLow
		title = fmt.Sprintf("%d stale workspace(s) older than %dd", staleCount, workspaceStaleDays)
	default:
		sev = scan.SeverityInfo
		title = "no stale workspaces"
		desc = fmt.Sprintf("All %d workspaces in %s are active (modified within %d days).",
			len(entries), wsDir, workspaceStaleDays)
	}

	return []scan.Finding{newFinding(
		abilitySystemHealth, categoryWorkspaceHygiene, sev, title, desc,
		scan.Scope{Kind: "directory", Value: wsDir},
	)}, ""
}

func checkLearningsDB(_ context.Context) ([]scan.Finding, string) {
	dbPath, err := scan.LearningsDBPath()
	if err != nil {
		return nil, fmt.Sprintf("resolving learnings.db path: %v", err)
	}

	db, err := scan.OpenReadOnly(dbPath)
	if err != nil {
		return nil, fmt.Sprintf("opening learnings.db: %v", err)
	}
	if db == nil {
		return []scan.Finding{newFinding(
			abilitySystemHealth, categoryEmbedding, scan.SeverityInfo,
			"learnings.db not found",
			fmt.Sprintf("No learnings database at %s. Run 'orchestrator learn' to populate it.", dbPath),
			scan.Scope{Kind: "file", Value: dbPath},
		)}, ""
	}
	defer db.Close()

	findings := make([]scan.Finding, 0, 2)

	var total, withEmb int
	if err := db.QueryRow(`
		SELECT COUNT(*),
		       SUM(CASE WHEN embedding IS NOT NULL THEN 1 ELSE 0 END)
		FROM learnings WHERE COALESCE(archived, 0) = 0
	`).Scan(&total, &withEmb); err != nil {
		return nil, fmt.Sprintf("querying learnings stats: %v", err)
	}

	var covSev scan.Severity
	var covTitle string
	if total == 0 {
		covSev = scan.SeverityInfo
		covTitle = "learnings.db is empty"
	} else {
		coverage := float64(withEmb) / float64(total)
		pct := int(coverage * 100)
		switch {
		case coverage < embeddingCoverageLow:
			covSev = scan.SeverityMedium
			covTitle = fmt.Sprintf("embedding coverage is low (%d%%)", pct)
		case coverage < embeddingCoverageGood:
			covSev = scan.SeverityLow
			covTitle = fmt.Sprintf("embedding coverage is below 80%% (%d%%)", pct)
		default:
			covSev = scan.SeverityInfo
			covTitle = fmt.Sprintf("embedding coverage is good (%d%%)", pct)
		}
	}
	findings = append(findings, newFinding(
		abilitySystemHealth, categoryEmbedding, covSev, covTitle,
		fmt.Sprintf("%d of %d active learnings have embeddings in %s.", withEmb, total, dbPath),
		scan.Scope{Kind: "file", Value: dbPath},
	))

	var deadCount int
	deadErr := db.QueryRow(`
		SELECT COUNT(DISTINCT id) FROM (
			SELECT id FROM learnings
			WHERE COALESCE(archived, 0) = 0
			  AND COALESCE(injection_count, 0) = 0 AND COALESCE(used_count, 0) = 0
			  AND created_at < datetime('now', '-90 days')
			UNION
			SELECT id FROM learnings
			WHERE COALESCE(archived, 0) = 0
			  AND COALESCE(injection_count, 0) >= 5 AND COALESCE(compliance_rate, 0.0) < 0.10
			UNION
			SELECT id FROM learnings
			WHERE COALESCE(archived, 0) = 0
			  AND quality_score < 0.2 AND COALESCE(used_count, 0) = 0
			  AND created_at < datetime('now', '-60 days')
			UNION
			SELECT id FROM learnings
			WHERE COALESCE(archived, 0) = 0
			  AND COALESCE(seen_count, 0) = 1 AND embedding IS NULL
			  AND created_at < datetime('now', '-30 days')
		)
	`).Scan(&deadCount)
	if deadErr != nil {
		return findings, fmt.Sprintf("querying dead-weight candidates: %v", deadErr)
	}

	var dwSev scan.Severity
	var dwTitle string
	if total == 0 {
		dwSev = scan.SeverityInfo
		dwTitle = "no learnings to evaluate for dead-weight"
	} else {
		ratio := float64(deadCount) / float64(total)
		pct := int(ratio * 100)
		switch {
		case ratio > deadWeightMedium:
			dwSev = scan.SeverityMedium
			dwTitle = fmt.Sprintf("%d%% of learnings are dead-weight candidates (%d entries)", pct, deadCount)
		case ratio > deadWeightLow:
			dwSev = scan.SeverityLow
			dwTitle = fmt.Sprintf("%d%% of learnings are dead-weight candidates (%d entries)", pct, deadCount)
		default:
			dwSev = scan.SeverityInfo
			dwTitle = fmt.Sprintf("dead-weight ratio is healthy (%d%%, %d entries)", pct, deadCount)
		}
	}
	findings = append(findings, newFinding(
		abilitySystemHealth, categoryDeadWeight, dwSev, dwTitle,
		fmt.Sprintf("%d of %d active learnings in %s meet dead-weight criteria. Run 'orchestrator learn compact' to archive them.",
			deadCount, total, dbPath),
		scan.Scope{Kind: "file", Value: dbPath},
	))

	return findings, ""
}

func checkDaemonSocket(_ context.Context) ([]scan.Finding, string) {
	sockPath, err := scan.DaemonSocketPath()
	if err != nil {
		return nil, fmt.Sprintf("resolving daemon socket path: %v", err)
	}

	if _, err := os.Stat(sockPath); os.IsNotExist(err) {
		return []scan.Finding{newFinding(
			abilitySystemHealth, categoryDaemonHealth, scan.SeverityInfo,
			"daemon is not running",
			fmt.Sprintf("No socket at %s. Start the daemon with 'orchestrator daemon start'.", sockPath),
			scan.Scope{Kind: "socket", Value: sockPath},
		)}, ""
	}

	conn, err := net.DialTimeout("unix", sockPath, daemonDialTimeout)
	if err != nil {
		return []scan.Finding{newFinding(
			abilitySystemHealth, categoryDaemonHealth, scan.SeverityLow,
			"daemon socket is unreachable",
			fmt.Sprintf("Socket exists at %s but connection failed: %v.", sockPath, err),
			scan.Scope{Kind: "socket", Value: sockPath},
		)}, ""
	}
	conn.Close()

	return []scan.Finding{newFinding(
		abilitySystemHealth, categoryDaemonHealth, scan.SeverityInfo,
		"daemon socket is reachable",
		fmt.Sprintf("Successfully connected to daemon at %s.", sockPath),
		scan.Scope{Kind: "socket", Value: sockPath},
	)}, ""
}

func checkMetricsDB(ctx context.Context) ([]scan.Finding, string) {
	dbPath, err := scan.MetricsDBPath()
	if err != nil {
		return nil, fmt.Sprintf("resolving metrics.db path: %v", err)
	}

	db, err := scan.OpenReadOnly(dbPath)
	if err != nil {
		return nil, fmt.Sprintf("opening metrics.db: %v", err)
	}
	if db == nil {
		return []scan.Finding{newFinding(
			abilitySystemHealth, categoryRoutingQuality, scan.SeverityInfo,
			"metrics.db not found",
			fmt.Sprintf("No metrics database at %s. Metrics are recorded after missions run.", dbPath),
			scan.Scope{Kind: "file", Value: dbPath},
		)}, ""
	}
	defer db.Close()

	findings := make([]scan.Finding, 0, 2)

	fallbackFinding, errMsg := queryFallbackRate(ctx, db, dbPath)
	if errMsg != "" {
		return nil, errMsg
	}
	findings = append(findings, fallbackFinding)

	lastMissionFinding, errMsg := queryLastMissionDate(ctx, db, dbPath)
	if errMsg != "" {
		return findings, errMsg
	}
	findings = append(findings, lastMissionFinding)

	return findings, ""
}

func queryFallbackRate(ctx context.Context, db *sql.DB, dbPath string) (scan.Finding, string) {
	rows, err := db.QueryContext(ctx, `
		SELECT
		    COALESCE(NULLIF(selection_method, ''), 'unknown') AS method,
		    COUNT(*) AS cnt,
		    COALESCE(
		        COUNT(*) * 100.0 / NULLIF((
		            SELECT COUNT(*) FROM phases
		            WHERE persona != ''
		              AND COALESCE(selection_method, '') != 'required_review'
		        ), 0),
		    0.0) AS pct
		FROM phases
		WHERE persona != ''
		  AND COALESCE(selection_method, '') != 'required_review'
		GROUP BY method
		ORDER BY cnt DESC
	`)
	if err != nil {
		return scan.Finding{}, fmt.Sprintf("querying routing distribution: %v", err)
	}
	defer rows.Close()

	var fallbackPct float64
	var totalPhases int
	for rows.Next() {
		var method string
		var cnt int
		var pct float64
		if err := rows.Scan(&method, &cnt, &pct); err != nil {
			return scan.Finding{}, fmt.Sprintf("scanning routing row: %v", err)
		}
		totalPhases += cnt
		if method == "fallback" {
			fallbackPct = pct
		}
	}
	if err := rows.Err(); err != nil {
		return scan.Finding{}, fmt.Sprintf("iterating routing rows: %v", err)
	}

	if totalPhases == 0 {
		return newFinding(
			abilitySystemHealth, categoryRoutingQuality, scan.SeverityInfo,
			"no phase routing data",
			fmt.Sprintf("No phase records found in %s yet.", dbPath),
			scan.Scope{Kind: "file", Value: dbPath},
		), ""
	}

	var sev scan.Severity
	var title string
	if fallbackPct >= routingFallbackThreshold {
		sev = scan.SeverityHigh
		title = fmt.Sprintf("routing fallback rate exceeds threshold (%.1f%%)", fallbackPct)
	} else {
		sev = scan.SeverityInfo
		title = fmt.Sprintf("routing fallback rate is healthy (%.1f%%)", fallbackPct)
	}

	return newFinding(
		abilitySystemHealth, categoryRoutingQuality, sev, title,
		fmt.Sprintf("%.1f%% of %d phase assignments used fallback routing in %s.", fallbackPct, totalPhases, dbPath),
		scan.Scope{Kind: "file", Value: dbPath},
	), ""
}

func queryLastMissionDate(ctx context.Context, db *sql.DB, dbPath string) (scan.Finding, string) {
	var lastAt sql.NullString
	if err := db.QueryRowContext(ctx, `SELECT MAX(started_at) FROM missions`).Scan(&lastAt); err != nil {
		return scan.Finding{}, fmt.Sprintf("querying last mission date: %v", err)
	}

	if !lastAt.Valid || lastAt.String == "" {
		return newFinding(
			abilitySystemHealth, categoryMissionActivity, scan.SeverityInfo,
			"no missions recorded",
			fmt.Sprintf("No missions found in %s.", dbPath),
			scan.Scope{Kind: "file", Value: dbPath},
		), ""
	}

	t, err := time.Parse(time.RFC3339, lastAt.String)
	if err != nil {
		return scan.Finding{}, fmt.Sprintf("parsing last mission time %q: %v", lastAt.String, err)
	}

	ageDays := int(time.Since(t).Hours() / 24)

	var sev scan.Severity
	var title string
	switch {
	case ageDays > missionVeryStaleDays:
		sev = scan.SeverityMedium
		title = fmt.Sprintf("no missions in the last %d days", ageDays)
	case ageDays > missionStaleDays:
		sev = scan.SeverityLow
		title = fmt.Sprintf("last mission was %d days ago", ageDays)
	default:
		sev = scan.SeverityInfo
		title = fmt.Sprintf("last mission was %d day(s) ago", ageDays)
	}

	return newFinding(
		abilitySystemHealth, categoryMissionActivity, sev, title,
		fmt.Sprintf("Most recent mission started at %s (%d days ago).", t.Format("2006-01-02"), ageDays),
		scan.Scope{Kind: "file", Value: dbPath},
	), ""
}

func enFindingID() string {
	b := make([]byte, 8)
	if _, err := rand.Read(b); err != nil {
		return fmt.Sprintf("en-%d", time.Now().UnixNano())
	}
	return "en-" + hex.EncodeToString(b)
}
