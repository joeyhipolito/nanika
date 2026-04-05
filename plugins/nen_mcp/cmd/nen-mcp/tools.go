package main

import (
	"bufio"
	"context"
	"database/sql"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	_ "modernc.org/sqlite"
)

// ---------------------------------------------------------------------------
// MCP tool/call protocol types
// ---------------------------------------------------------------------------

type toolCallParams struct {
	Name      string          `json:"name"`
	Arguments json.RawMessage `json:"arguments"`
}

type toolCallResult struct {
	Content []contentBlock `json:"content"`
	IsError bool           `json:"isError,omitempty"`
}

type contentBlock struct {
	Type string `json:"type"`
	Text string `json:"text"`
}

// ---------------------------------------------------------------------------
// Tool definitions
// ---------------------------------------------------------------------------

func listTools() toolsListResult {
	schema := func(props map[string]any, required ...string) map[string]any {
		s := map[string]any{"type": "object", "properties": props}
		if len(required) > 0 {
			s["required"] = required
		}
		return s
	}
	str := func(desc string) map[string]any { return map[string]any{"type": "string", "description": desc} }
	num := func(desc string) map[string]any { return map[string]any{"type": "integer", "description": desc} }
	boo := func(desc string) map[string]any { return map[string]any{"type": "boolean", "description": desc} }

	return toolsListResult{Tools: []tool{
		{
			Name:        "nanika_findings",
			Description: "List active nen observer findings from findings.db. Returns security, drift, and anomaly findings.",
			InputSchema: schema(map[string]any{
				"ability":     str("Filter by ability name (e.g. gyo, en, ryu)"),
				"severity":    str("Filter by severity (e.g. high, medium, low)"),
				"category":    str("Filter by category"),
				"active_only": boo("Only return active findings (not superseded or expired). Default: true"),
				"limit":       num("Max results (default 20, max 100)"),
			}),
		},
		{
			Name:        "nanika_proposals",
			Description: "List shu improvement proposals from proposals.db.",
			InputSchema: schema(map[string]any{
				"ability": str("Filter by ability name"),
				"limit":   num("Max results (default 20, max 100)"),
			}),
		},
		{
			Name:        "nanika_ko_verdicts",
			Description: "List ko eval run verdicts from ko-history.db.",
			InputSchema: schema(map[string]any{
				"config": str("Filter by config_path substring"),
				"limit":  num("Max results (default 20, max 100)"),
			}),
		},
		{
			Name:        "nanika_scheduler_jobs",
			Description: "List scheduler jobs from scheduler.db.",
			InputSchema: schema(map[string]any{
				"enabled_only": boo("Only return enabled jobs. Default: false"),
				"limit":        num("Max results (default 50, max 200)"),
			}),
		},
		{
			Name:        "nanika_tracker_issues",
			Description: "List tracker issues from tracker.db.",
			InputSchema: schema(map[string]any{
				"status":   str("Filter by status (e.g. open, closed, in-progress)"),
				"priority": str("Filter by priority (e.g. P0, P1, P2)"),
				"limit":    num("Max results (default 50, max 200)"),
			}),
		},
		{
			Name:        "nanika_mission",
			Description: "Get one mission by ID or list recent missions from metrics.db.",
			InputSchema: schema(map[string]any{
				"mission_id": str("Return a specific mission by its ID"),
				"status":     str("Filter by status: success, failure, or partial"),
				"limit":      num("Max results when listing (default 20, max 100)"),
			}),
		},
		{
			Name:        "nanika_events",
			Description: "Read mission events from the JSONL event log.",
			InputSchema: schema(map[string]any{
				"mission_id": str("Mission ID whose event log to read"),
				"event_type": str("Filter by event type (e.g. phase.started, mission.completed)"),
				"limit":      num("Max events (default 100, max 500)"),
			}, "mission_id"),
		},
		{
			Name:        "nanika_learnings",
			Description: "List learnings from learnings.db, ordered by quality score descending.",
			InputSchema: schema(map[string]any{
				"domain":   str("Filter by domain (e.g. dev, personal)"),
				"type":     str("Filter by learning type (e.g. insight, pattern, error)"),
				"archived": boo("Include archived learnings. Default: false (exclude archived)"),
				"limit":    num("Max results (default 20, max 100)"),
			}),
		},
	}}
}

// ---------------------------------------------------------------------------
// tools/call dispatch
// ---------------------------------------------------------------------------

func callTool(req rpcRequest) rpcResponse {
	var params toolCallParams
	if err := json.Unmarshal(req.Params, &params); err != nil {
		return rpcResponse{
			JSONRPC: "2.0",
			ID:      req.ID,
			Error:   &rpcError{Code: -32602, Message: "invalid params: " + err.Error()},
		}
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	result, err := dispatchTool(ctx, params.Name, params.Arguments)
	if err != nil {
		return rpcResponse{
			JSONRPC: "2.0",
			ID:      req.ID,
			Result: toolCallResult{
				Content: []contentBlock{{Type: "text", Text: err.Error()}},
				IsError: true,
			},
		}
	}

	text, _ := json.MarshalIndent(result, "", "  ")
	return rpcResponse{
		JSONRPC: "2.0",
		ID:      req.ID,
		Result: toolCallResult{
			Content: []contentBlock{{Type: "text", Text: string(text)}},
		},
	}
}

func dispatchTool(ctx context.Context, name string, args json.RawMessage) (any, error) {
	switch name {
	case "nanika_findings":
		return handleFindings(ctx, args)
	case "nanika_proposals":
		return handleProposals(ctx, args)
	case "nanika_ko_verdicts":
		return handleKOVerdicts(ctx, args)
	case "nanika_scheduler_jobs":
		return handleSchedulerJobs(ctx, args)
	case "nanika_tracker_issues":
		return handleTrackerIssues(ctx, args)
	case "nanika_mission":
		return handleMission(ctx, args)
	case "nanika_events":
		return handleEvents(ctx, args)
	case "nanika_learnings":
		return handleLearnings(ctx, args)
	default:
		return nil, fmt.Errorf("unknown tool: %s", name)
	}
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

// orchestratorConfigDir mirrors skills/orchestrator/internal/config.Dir().
// Priority: ORCHESTRATOR_CONFIG_DIR > ALLUKA_HOME > VIA_HOME/orchestrator >
//
//	~/.alluka > ~/.via
func orchestratorConfigDir() string {
	if d := os.Getenv("ORCHESTRATOR_CONFIG_DIR"); d != "" {
		return d
	}
	if d := os.Getenv("ALLUKA_HOME"); d != "" {
		return d
	}
	if d := os.Getenv("VIA_HOME"); d != "" {
		return filepath.Join(d, "orchestrator")
	}
	home, _ := os.UserHomeDir()
	alluka := filepath.Join(home, ".alluka")
	if _, err := os.Stat(alluka); err == nil {
		return alluka
	}
	return filepath.Join(home, ".via")
}

// schedulerDBPath mirrors plugins/scheduler/internal/config.Default().
// Priority: SCHEDULER_CONFIG_DIR > ~/.alluka/scheduler/
func schedulerDBPath() string {
	if d := os.Getenv("SCHEDULER_CONFIG_DIR"); d != "" {
		return filepath.Join(d, "scheduler.db")
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "scheduler", "scheduler.db")
}

// trackerDBPath mirrors plugins/tracker/src/db.rs.
// Priority: TRACKER_DB > ~/.alluka/tracker.db
func trackerDBPath() string {
	if p := os.Getenv("TRACKER_DB"); p != "" {
		return p
	}
	home, _ := os.UserHomeDir()
	return filepath.Join(home, ".alluka", "tracker.db")
}

// openReadOnly opens a SQLite DB at path in read-only mode.
// Returns an error if the file does not exist.
func openReadOnly(path string) (*sql.DB, error) {
	if _, err := os.Stat(path); err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("backing store not found: %s", path)
		}
		return nil, fmt.Errorf("stat %s: %w", path, err)
	}
	db, err := sql.Open("sqlite", "file:"+path+"?mode=ro&_busy_timeout=5000")
	if err != nil {
		return nil, fmt.Errorf("opening %s: %w", path, err)
	}
	db.SetMaxOpenConns(1)
	return db, nil
}

// clampLimit returns v clamped to [1, ceiling]; uses def when v <= 0.
func clampLimit(v, def, ceiling int) int {
	if v <= 0 {
		return def
	}
	if v > ceiling {
		return ceiling
	}
	return v
}

// ns converts a sql.NullString to a plain string (empty string when NULL).
func ns(v sql.NullString) string { return v.String }

// ---------------------------------------------------------------------------
// Handler: nanika_findings
// ---------------------------------------------------------------------------

func handleFindings(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		Ability    string `json:"ability"`
		Severity   string `json:"severity"`
		Category   string `json:"category"`
		ActiveOnly *bool  `json:"active_only"`
		Limit      int    `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}

	activeOnly := args.ActiveOnly == nil || *args.ActiveOnly // default true
	limit := clampLimit(args.Limit, 20, 100)

	dbPath := filepath.Join(orchestratorConfigDir(), "nen", "findings.db")
	db, err := openReadOnly(dbPath)
	if err != nil {
		return nil, err
	}
	defer db.Close()

	var whereClauses []string
	var queryArgs []any

	if activeOnly {
		whereClauses = append(whereClauses, "superseded_by = ''")
		// B1 fix: wrap expires_at in datetime() so the comparison normalises both sides
		// to SQLite's canonical form. Raw string comparison between RFC3339 ('2026-04-05T10:00:00Z')
		// and datetime('now') ('2026-04-05 19:06:42') is wrong because 'T' > ' ' in ASCII,
		// making expired findings incorrectly "win" the comparison.
		whereClauses = append(whereClauses, "(expires_at IS NULL OR datetime(expires_at) > datetime('now'))")
	}
	if args.Ability != "" {
		whereClauses = append(whereClauses, "ability = ?")
		queryArgs = append(queryArgs, args.Ability)
	}
	if args.Severity != "" {
		whereClauses = append(whereClauses, "severity = ?")
		queryArgs = append(queryArgs, args.Severity)
	}
	if args.Category != "" {
		whereClauses = append(whereClauses, "category = ?")
		queryArgs = append(queryArgs, args.Category)
	}

	q := `SELECT id, ability, category, severity, title, description, scope_kind, scope_value, evidence, source, found_at, expires_at, superseded_by, created_at FROM findings`
	if len(whereClauses) > 0 {
		q += " WHERE " + strings.Join(whereClauses, " AND ")
	}
	q += " ORDER BY found_at DESC LIMIT ?"
	queryArgs = append(queryArgs, limit)

	rows, err := db.QueryContext(ctx, q, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("querying findings: %w", err)
	}
	defer rows.Close()

	var results []map[string]any
	for rows.Next() {
		var (
			id, ability, category, severity, title, description string
			scopeKind, scopeValue, evidence, source, foundAt    string
			expiresAt, supersededBy, createdAt                  sql.NullString
		)
		if err := rows.Scan(&id, &ability, &category, &severity, &title, &description,
			&scopeKind, &scopeValue, &evidence, &source, &foundAt,
			&expiresAt, &supersededBy, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning finding: %w", err)
		}
		results = append(results, map[string]any{
			"id":            id,
			"ability":       ability,
			"category":      category,
			"severity":      severity,
			"title":         title,
			"description":   description,
			"scope_kind":    scopeKind,
			"scope_value":   scopeValue,
			"evidence":      evidence,
			"source":        source,
			"found_at":      foundAt,
			"expires_at":    ns(expiresAt),
			"superseded_by": ns(supersededBy),
			"created_at":    ns(createdAt),
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating findings: %w", err)
	}

	return map[string]any{"findings": results, "count": len(results)}, nil
}

// ---------------------------------------------------------------------------
// Handler: nanika_proposals
// ---------------------------------------------------------------------------

func handleProposals(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		Ability string `json:"ability"`
		Limit   int    `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}

	limit := clampLimit(args.Limit, 20, 100)

	dbPath := filepath.Join(orchestratorConfigDir(), "nen", "proposals.db")
	db, err := openReadOnly(dbPath)
	if err != nil {
		return nil, err
	}
	defer db.Close()

	var whereClauses []string
	var queryArgs []any

	if args.Ability != "" {
		whereClauses = append(whereClauses, "ability = ?")
		queryArgs = append(queryArgs, args.Ability)
	}

	q := `SELECT dedup_key, last_proposed_at, ability, category, tracker_issue FROM proposals`
	if len(whereClauses) > 0 {
		q += " WHERE " + strings.Join(whereClauses, " AND ")
	}
	q += " ORDER BY last_proposed_at DESC LIMIT ?"
	queryArgs = append(queryArgs, limit)

	rows, err := db.QueryContext(ctx, q, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("querying proposals: %w", err)
	}
	defer rows.Close()

	var results []map[string]any
	for rows.Next() {
		var (
			dedupKey, lastProposedAt    string
			ability, category, trackerIssue sql.NullString
		)
		if err := rows.Scan(&dedupKey, &lastProposedAt, &ability, &category, &trackerIssue); err != nil {
			return nil, fmt.Errorf("scanning proposal: %w", err)
		}
		results = append(results, map[string]any{
			"dedup_key":        dedupKey,
			"last_proposed_at": lastProposedAt,
			"ability":          ns(ability),
			"category":         ns(category),
			"tracker_issue":    ns(trackerIssue),
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating proposals: %w", err)
	}

	return map[string]any{"proposals": results, "count": len(results)}, nil
}

// ---------------------------------------------------------------------------
// Handler: nanika_ko_verdicts
// ---------------------------------------------------------------------------

func handleKOVerdicts(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		Config string `json:"config"`
		Limit  int    `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}

	limit := clampLimit(args.Limit, 20, 100)

	dbPath := filepath.Join(orchestratorConfigDir(), "ko-history.db")
	db, err := openReadOnly(dbPath)
	if err != nil {
		return nil, err
	}
	defer db.Close()

	var whereClauses []string
	var queryArgs []any

	if args.Config != "" {
		whereClauses = append(whereClauses, "config_path LIKE ?")
		queryArgs = append(queryArgs, "%"+args.Config+"%")
	}

	q := `SELECT id, config_path, description, model, started_at, finished_at, total, passed, failed, input_tokens, output_tokens, cost_usd FROM eval_runs`
	if len(whereClauses) > 0 {
		q += " WHERE " + strings.Join(whereClauses, " AND ")
	}
	q += " ORDER BY started_at DESC LIMIT ?"
	queryArgs = append(queryArgs, limit)

	rows, err := db.QueryContext(ctx, q, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("querying ko verdicts: %w", err)
	}
	defer rows.Close()

	var results []map[string]any
	for rows.Next() {
		var (
			id, configPath, description, model, startedAt string
			finishedAt                                     sql.NullString
			total, passed, failed, inputTokens, outputTokens int
			costUSD                                         float64
		)
		if err := rows.Scan(&id, &configPath, &description, &model, &startedAt, &finishedAt,
			&total, &passed, &failed, &inputTokens, &outputTokens, &costUSD); err != nil {
			return nil, fmt.Errorf("scanning ko verdict: %w", err)
		}
		results = append(results, map[string]any{
			"id":            id,
			"config_path":   configPath,
			"description":   description,
			"model":         model,
			"started_at":    startedAt,
			"finished_at":   ns(finishedAt),
			"total":         total,
			"passed":        passed,
			"failed":        failed,
			"input_tokens":  inputTokens,
			"output_tokens": outputTokens,
			"cost_usd":      costUSD,
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating ko verdicts: %w", err)
	}

	return map[string]any{"verdicts": results, "count": len(results)}, nil
}

// ---------------------------------------------------------------------------
// Handler: nanika_scheduler_jobs
// ---------------------------------------------------------------------------

func handleSchedulerJobs(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		EnabledOnly bool `json:"enabled_only"`
		Limit       int  `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}

	limit := clampLimit(args.Limit, 50, 200)

	db, err := openReadOnly(schedulerDBPath())
	if err != nil {
		return nil, err
	}
	defer db.Close()

	var whereClauses []string
	var queryArgs []any

	if args.EnabledOnly {
		whereClauses = append(whereClauses, "enabled = 1")
	}

	q := `SELECT id, name, command, schedule, schedule_type, enabled, priority, timeout_sec, last_run_at, next_run_at, created_at FROM jobs`
	if len(whereClauses) > 0 {
		q += " WHERE " + strings.Join(whereClauses, " AND ")
	}
	q += " ORDER BY name ASC LIMIT ?"
	queryArgs = append(queryArgs, limit)

	rows, err := db.QueryContext(ctx, q, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("querying scheduler jobs: %w", err)
	}
	defer rows.Close()

	var results []map[string]any
	for rows.Next() {
		var (
			id, name, command, schedule, scheduleType, priority, createdAt string
			enabled, timeoutSec                                             int
			lastRunAt, nextRunAt                                            sql.NullString
		)
		if err := rows.Scan(&id, &name, &command, &schedule, &scheduleType, &enabled,
			&priority, &timeoutSec, &lastRunAt, &nextRunAt, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning scheduler job: %w", err)
		}
		results = append(results, map[string]any{
			"id":            id,
			"name":          name,
			"command":       command,
			"schedule":      schedule,
			"schedule_type": scheduleType,
			"enabled":       enabled == 1,
			"priority":      priority,
			"timeout_sec":   timeoutSec,
			"last_run_at":   ns(lastRunAt),
			"next_run_at":   ns(nextRunAt),
			"created_at":    createdAt,
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating scheduler jobs: %w", err)
	}

	return map[string]any{"jobs": results, "count": len(results)}, nil
}

// ---------------------------------------------------------------------------
// Handler: nanika_tracker_issues
// ---------------------------------------------------------------------------

func handleTrackerIssues(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		Status   string `json:"status"`
		Priority string `json:"priority"`
		Limit    int    `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}

	limit := clampLimit(args.Limit, 50, 200)

	db, err := openReadOnly(trackerDBPath())
	if err != nil {
		return nil, err
	}
	defer db.Close()

	var whereClauses []string
	var queryArgs []any

	if args.Status != "" {
		whereClauses = append(whereClauses, "status = ?")
		queryArgs = append(queryArgs, args.Status)
	}
	if args.Priority != "" {
		whereClauses = append(whereClauses, "priority = ?")
		queryArgs = append(queryArgs, args.Priority)
	}

	q := `SELECT id, title, description, status, priority, labels, assignee, created_at, updated_at FROM issues`
	if len(whereClauses) > 0 {
		q += " WHERE " + strings.Join(whereClauses, " AND ")
	}
	q += " ORDER BY created_at DESC LIMIT ?"
	queryArgs = append(queryArgs, limit)

	rows, err := db.QueryContext(ctx, q, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("querying tracker issues: %w", err)
	}
	defer rows.Close()

	var results []map[string]any
	for rows.Next() {
		var (
			id, title, status, createdAt, updatedAt    string
			description, priority, labels, assignee sql.NullString
		)
		if err := rows.Scan(&id, &title, &description, &status, &priority,
			&labels, &assignee, &createdAt, &updatedAt); err != nil {
			return nil, fmt.Errorf("scanning tracker issue: %w", err)
		}
		results = append(results, map[string]any{
			"id":          id,
			"title":       title,
			"description": ns(description),
			"status":      status,
			"priority":    ns(priority),
			"labels":      ns(labels),
			"assignee":    ns(assignee),
			"created_at":  createdAt,
			"updated_at":  updatedAt,
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating tracker issues: %w", err)
	}

	return map[string]any{"issues": results, "count": len(results)}, nil
}

// ---------------------------------------------------------------------------
// Handler: nanika_mission
// ---------------------------------------------------------------------------

func handleMission(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		MissionID string `json:"mission_id"`
		Status    string `json:"status"`
		Limit     int    `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}

	dbPath := filepath.Join(orchestratorConfigDir(), "metrics.db")
	db, err := openReadOnly(dbPath)
	if err != nil {
		return nil, err
	}
	defer db.Close()

	// Single mission lookup by ID.
	if args.MissionID != "" {
		row := db.QueryRowContext(ctx,
			`SELECT id, domain, task, started_at, finished_at, status, phases_total, phases_completed, phases_failed, phases_skipped, retries_total, cost_usd_total, decomp_source FROM missions WHERE id = ?`,
			args.MissionID)
		var (
			id, domain, task, startedAt, status, decompSource string
			finishedAt                                         sql.NullString
			phasesTotal, phasesCompleted, phasesFailed, phasesSkipped, retriesTotal int
			costUSD                                                                  float64
		)
		if err := row.Scan(&id, &domain, &task, &startedAt, &finishedAt, &status,
			&phasesTotal, &phasesCompleted, &phasesFailed, &phasesSkipped,
			&retriesTotal, &costUSD, &decompSource); err != nil {
			if err == sql.ErrNoRows {
				return nil, fmt.Errorf("mission not found: %s", args.MissionID)
			}
			return nil, fmt.Errorf("querying mission %s: %w", args.MissionID, err)
		}
		return map[string]any{
			"id":               id,
			"domain":           domain,
			"task":             task,
			"started_at":       startedAt,
			"finished_at":      ns(finishedAt),
			"status":           status,
			"phases_total":     phasesTotal,
			"phases_completed": phasesCompleted,
			"phases_failed":    phasesFailed,
			"phases_skipped":   phasesSkipped,
			"retries_total":    retriesTotal,
			"cost_usd_total":   costUSD,
			"decomp_source":    decompSource,
		}, nil
	}

	// List missions with optional status filter.
	limit := clampLimit(args.Limit, 20, 100)

	var whereClauses []string
	var queryArgs []any

	if args.Status != "" {
		whereClauses = append(whereClauses, "status = ?")
		queryArgs = append(queryArgs, args.Status)
	}

	q := `SELECT id, domain, task, started_at, finished_at, status, phases_total, phases_completed, phases_failed, phases_skipped, retries_total, cost_usd_total, decomp_source FROM missions`
	if len(whereClauses) > 0 {
		q += " WHERE " + strings.Join(whereClauses, " AND ")
	}
	q += " ORDER BY started_at DESC LIMIT ?"
	queryArgs = append(queryArgs, limit)

	rows, err := db.QueryContext(ctx, q, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("querying missions: %w", err)
	}
	defer rows.Close()

	var results []map[string]any
	for rows.Next() {
		var (
			id, domain, task, startedAt, status, decompSource string
			finishedAt                                         sql.NullString
			phasesTotal, phasesCompleted, phasesFailed, phasesSkipped, retriesTotal int
			costUSD                                                                  float64
		)
		if err := rows.Scan(&id, &domain, &task, &startedAt, &finishedAt, &status,
			&phasesTotal, &phasesCompleted, &phasesFailed, &phasesSkipped,
			&retriesTotal, &costUSD, &decompSource); err != nil {
			return nil, fmt.Errorf("scanning mission: %w", err)
		}
		results = append(results, map[string]any{
			"id":               id,
			"domain":           domain,
			"task":             task,
			"started_at":       startedAt,
			"finished_at":      ns(finishedAt),
			"status":           status,
			"phases_total":     phasesTotal,
			"phases_completed": phasesCompleted,
			"phases_failed":    phasesFailed,
			"phases_skipped":   phasesSkipped,
			"retries_total":    retriesTotal,
			"cost_usd_total":   costUSD,
			"decomp_source":    decompSource,
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating missions: %w", err)
	}

	return map[string]any{"missions": results, "count": len(results)}, nil
}

// ---------------------------------------------------------------------------
// Handler: nanika_events
// ---------------------------------------------------------------------------

func handleEvents(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		MissionID string `json:"mission_id"`
		EventType string `json:"event_type"`
		Limit     int    `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}
	if args.MissionID == "" {
		return nil, fmt.Errorf("mission_id is required")
	}

	limit := clampLimit(args.Limit, 100, 500)

	eventsPath := filepath.Join(orchestratorConfigDir(), "events", args.MissionID+".jsonl")
	if _, err := os.Stat(eventsPath); err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("no events found for mission %s", args.MissionID)
		}
		return nil, fmt.Errorf("stat events file: %w", err)
	}

	f, err := os.Open(eventsPath)
	if err != nil {
		return nil, fmt.Errorf("opening events file: %w", err)
	}
	defer f.Close()

	var events []map[string]any
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1*1024*1024), 1*1024*1024)

	for scanner.Scan() {
		if ctx.Err() != nil {
			break
		}
		line := scanner.Bytes()
		if len(line) == 0 {
			continue
		}
		var event map[string]any
		if err := json.Unmarshal(line, &event); err != nil {
			continue // skip malformed lines; don't fail the whole request
		}
		if args.EventType != "" {
			t, _ := event["type"].(string)
			if t != args.EventType {
				continue
			}
		}
		events = append(events, event)
		if len(events) >= limit {
			break
		}
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading events file: %w", err)
	}

	return map[string]any{"events": events, "count": len(events), "mission_id": args.MissionID}, nil
}

// ---------------------------------------------------------------------------
// Handler: nanika_learnings
// ---------------------------------------------------------------------------

func handleLearnings(ctx context.Context, rawArgs json.RawMessage) (any, error) {
	var args struct {
		Domain   string `json:"domain"`
		Type     string `json:"type"`
		Archived *bool  `json:"archived"`
		Limit    int    `json:"limit"`
	}
	if len(rawArgs) > 0 {
		if err := json.Unmarshal(rawArgs, &args); err != nil {
			return nil, fmt.Errorf("parsing arguments: %w", err)
		}
	}

	// Default: exclude archived.
	includeArchived := args.Archived != nil && *args.Archived
	limit := clampLimit(args.Limit, 20, 100)

	dbPath := filepath.Join(orchestratorConfigDir(), "learnings.db")
	db, err := openReadOnly(dbPath)
	if err != nil {
		return nil, err
	}
	defer db.Close()

	var whereClauses []string
	var queryArgs []any

	if !includeArchived {
		whereClauses = append(whereClauses, "archived = 0")
	}
	if args.Domain != "" {
		whereClauses = append(whereClauses, "domain = ?")
		queryArgs = append(queryArgs, args.Domain)
	}
	if args.Type != "" {
		whereClauses = append(whereClauses, "type = ?")
		queryArgs = append(queryArgs, args.Type)
	}

	q := `SELECT id, type, content, context, domain, worker_name, workspace_id, tags, seen_count, used_count, quality_score, created_at, archived FROM learnings`
	if len(whereClauses) > 0 {
		q += " WHERE " + strings.Join(whereClauses, " AND ")
	}
	q += " ORDER BY quality_score DESC, created_at DESC LIMIT ?"
	queryArgs = append(queryArgs, limit)

	rows, err := db.QueryContext(ctx, q, queryArgs...)
	if err != nil {
		return nil, fmt.Errorf("querying learnings: %w", err)
	}
	defer rows.Close()

	var results []map[string]any
	for rows.Next() {
		var (
			id, typ, content, domain, createdAt       string
			context_, workerName, workspaceID, tags sql.NullString
			seenCount, usedCount, archived           int
			qualityScore                             float64
		)
		if err := rows.Scan(&id, &typ, &content, &context_, &domain, &workerName,
			&workspaceID, &tags, &seenCount, &usedCount, &qualityScore,
			&createdAt, &archived); err != nil {
			return nil, fmt.Errorf("scanning learning: %w", err)
		}
		results = append(results, map[string]any{
			"id":           id,
			"type":         typ,
			"content":      content,
			"context":      ns(context_),
			"domain":       domain,
			"worker_name":  ns(workerName),
			"workspace_id": ns(workspaceID),
			"tags":         ns(tags),
			"seen_count":   seenCount,
			"used_count":   usedCount,
			"quality_score": qualityScore,
			"created_at":   createdAt,
			"archived":     archived == 1,
		})
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating learnings: %w", err)
	}

	return map[string]any{"learnings": results, "count": len(results)}, nil
}
