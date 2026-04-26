package engine

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
	metricsdb "github.com/joeyhipolito/orchestrator-cli/internal/metrics"
	"github.com/joeyhipolito/orchestrator-cli/internal/usage"
)

// defaultBudgetTokens is the fallback 5h token budget (input + output, cache_read
// excluded) used for utilization estimation. Derived from ~160K tokens/min × 300min.
// Override with RYU_5H_BUDGET_TOKENS.
const defaultBudgetTokens = 50_000_000

// PhaseMetric captures per-phase execution data.
type PhaseMetric struct {
	ID                     string   `json:"id,omitempty"`
	Name                   string   `json:"name"`
	Persona                string   `json:"persona"`
	Skills                 []string `json:"skills,omitempty"`
	ParsedSkills           []string `json:"parsed_skills,omitempty"`
	PersonaSelectionMethod string   `json:"persona_selection_method,omitempty"` // "llm" or "keyword"
	DurationS              int      `json:"duration_s"`
	Status                 string   `json:"status"`
	Retries                int      `json:"retries,omitempty"`
	GatePassed             bool     `json:"gate_passed"`
	OutputLen              int      `json:"output_len"`
	LearningsRetrieved     int      `json:"learnings_retrieved,omitempty"`
	// Error classification (only set when status == "failed")
	ErrorType    string `json:"error_type,omitempty"`    // rate-limit, tool-error, gate-failure, timeout, unknown
	ErrorMessage string `json:"error_message,omitempty"` // raw error string
	// Cost attribution (populated from Claude CLI session output)
	Provider            string  `json:"provider,omitempty"`              // always "anthropic"
	Model               string  `json:"model,omitempty"`                 // resolved model ID
	TokensIn            int     `json:"tokens_in,omitempty"`             // input tokens (sum of raw + cache_creation + cache_read, accumulated across retries)
	TokensOut           int     `json:"tokens_out,omitempty"`            // output tokens (accumulated across retries)
	TokensCacheCreation int     `json:"tokens_cache_creation,omitempty"` // cache creation tokens (accumulated across retries)
	TokensCacheRead     int     `json:"tokens_cache_read,omitempty"`     // cache read tokens (accumulated across retries)
	CostUSD             float64 `json:"cost_usd,omitempty"`              // total cost in USD
	// WorkerName is the persistent worker that ran this phase (e.g. "alpha").
	// Empty means the phase ran on an ephemeral worker.
	WorkerName string `json:"worker_name,omitempty"`
	// OutputBytes is the total on-disk size of the phase's merged artifacts,
	// captured in engine.handleArtifactMerge. Used for Barok V2 density rollup.
	OutputBytes int `json:"output_bytes,omitempty"`
	// Barok output-compression telemetry, propagated to metrics DB phases table.
	BarokApplied          int `json:"barok_applied,omitempty"`            // 1 when InjectBarok returned non-empty
	BarokRetry            int `json:"barok_retry,omitempty"`              // 1 when validator rejected and a retry ran
	BarokValidatorMs      int `json:"barok_validator_ms,omitempty"`       // summed wall-clock ms inside ValidateArtifactStructure on the initial pass
	BarokRetryValidatorMs int `json:"barok_retry_validator_ms,omitempty"` // summed wall-clock ms inside ValidateArtifactStructure on the retry pass (0 when no retry)
}

// Error type constants used in PhaseMetric.ErrorType and stored in phases.error_type.
const (
	ErrorTypeRateLimit   = "rate-limit"
	ErrorTypeToolError   = "tool-error"
	ErrorTypeGateFailure = "gate-failure"
	ErrorTypeTimeout     = "timeout"
	ErrorTypeWorkerCrash = "worker-crash"
	ErrorTypeUnknown     = "unknown"
)

// ParseErrorType classifies a raw error string into one of the known error type
// categories. Rate-limit errors are parsed specifically because they are needed
// for quota calibration.
func ParseErrorType(errMsg string) string {
	if errMsg == "" {
		return ""
	}
	lower := strings.ToLower(errMsg)
	switch {
	case strings.Contains(lower, "rate limit") ||
		strings.Contains(lower, "rate_limit") ||
		strings.Contains(lower, "rate limited") ||
		strings.Contains(lower, "ratelimit") ||
		strings.Contains(lower, "429") ||
		strings.Contains(lower, "quota exceeded") ||
		strings.Contains(lower, "overloaded"):
		return ErrorTypeRateLimit
	case strings.Contains(lower, "gate:") ||
		strings.Contains(lower, "gate failed") ||
		strings.Contains(lower, "quality gate") ||
		strings.Contains(lower, "gate failure"):
		return ErrorTypeGateFailure
	case strings.Contains(lower, "context deadline exceeded") ||
		strings.Contains(lower, "deadline exceeded") ||
		strings.Contains(lower, "timed out") ||
		strings.Contains(lower, "stall timeout") ||
		strings.Contains(lower, "timeout"):
		return ErrorTypeTimeout
	case strings.Contains(lower, "tool error") ||
		strings.Contains(lower, "tool_error") ||
		strings.Contains(lower, "tool call failed") ||
		strings.Contains(lower, "tool failed") ||
		strings.Contains(lower, "tool_use_error"):
		return ErrorTypeToolError
	case strings.Contains(lower, "claude exited "):
		return ErrorTypeWorkerCrash
	default:
		return ErrorTypeUnknown
	}
}

// MissionMetrics captures observability data for a single mission execution.
type MissionMetrics struct {
	WorkspaceID        string        `json:"workspace_id"`
	Domain             string        `json:"domain"`
	Task               string        `json:"task,omitempty"`
	StartedAt          time.Time     `json:"started_at"`
	FinishedAt         time.Time     `json:"finished_at"`
	DurationSec        int           `json:"duration_s"`
	PhasesTotal        int           `json:"phases_total"`
	PhasesCompleted    int           `json:"phases_completed"`
	PhasesFailed       int           `json:"phases_failed"`
	PhasesSkipped      int           `json:"phases_skipped"`
	LearningsRetrieved int           `json:"learnings_retrieved"`
	RetriesTotal       int           `json:"retries_total"`
	GateFailures       int           `json:"gate_failures"`
	OutputLenTotal     int           `json:"output_len_total"`
	Status             string        `json:"status"`                  // success, failure, partial
	DecompSource       string        `json:"decomp_source,omitempty"` // "predecomposed", "decomp.llm", "decomp.keyword", "template"
	Phases             []PhaseMetric `json:"phases,omitempty"`
	// Mission-level cost rollups (sum of all phase costs)
	TokensInTotal            int     `json:"tokens_in_total,omitempty"`
	TokensOutTotal           int     `json:"tokens_out_total,omitempty"`
	TokensCacheCreationTotal int     `json:"tokens_cache_creation_total,omitempty"`
	TokensCacheReadTotal     int     `json:"tokens_cache_read_total,omitempty"`
	CostUSDTotal             float64 `json:"cost_usd_total,omitempty"`
}

// RecordMetrics appends a JSONL line to ~/.alluka/metrics.jsonl and writes to
// the SQLite metrics database for richer queryability. The SQLite write is
// best-effort — any error is reported to stderr so it never blocks mission
// completion while still surfacing data-loss risk to the operator.
func RecordMetrics(m MissionMetrics) error {
	if err := recordMetricsDB(m); err != nil {
		fmt.Fprintf(os.Stderr, "warning: sqlite metrics write failed: %v\n", err)
	}
	if err := recordQuotaSnapshotDB(m); err != nil {
		fmt.Fprintf(os.Stderr, "warning: quota snapshot write failed: %v\n", err)
	}

	base, err := config.Dir()
	if err != nil {
		return err
	}

	path := filepath.Join(base, "metrics.jsonl")
	os.MkdirAll(filepath.Dir(path), 0700)

	data, err := json.Marshal(m)
	if err != nil {
		return err
	}
	data = append(data, '\n')

	f, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return err
	}
	defer f.Close()

	_, err = f.Write(data)
	return err
}

// recordMetricsDB writes m to the SQLite metrics database.
// Opens a fresh connection per call — acceptable since this runs once per mission.
func recordMetricsDB(m MissionMetrics) error {
	db, err := metricsdb.InitDB("")
	if err != nil {
		return err
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	_, err = db.UpsertMission(ctx, toMissionRecord(m))
	return err
}

// recordQuotaSnapshotDB writes a quota snapshot row after mission completion.
// It queries the existing 5h window totals from the DB, adds this mission's
// tokens, then inserts a snapshot row with the combined window values.
// Opens a fresh DB connection — acceptable since this runs once per mission.
func recordQuotaSnapshotDB(m MissionMetrics) error {
	if m.WorkspaceID == "" {
		return nil
	}

	db, err := metricsdb.InitDB("")
	if err != nil {
		return err
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	// Query rolling 5h totals from prior snapshots (this mission not yet included).
	prior, err := db.Get5hWindowTotals(ctx)
	if err != nil {
		return fmt.Errorf("get 5h window totals: %w", err)
	}

	window5hIn := prior.TokensIn + m.TokensInTotal
	window5hOut := prior.TokensOut + m.TokensOutTotal
	window5hCacheRead := prior.TokensCacheRead + m.TokensCacheReadTotal
	window5hCost := prior.CostUSD + m.CostUSDTotal

	budget := defaultBudgetTokens
	if v := os.Getenv("RYU_5H_BUDGET_TOKENS"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			budget = n
		}
	}

	// Exclude cache_read tokens: they don't count toward Anthropic rate limits.
	effectiveIn := window5hIn - window5hCacheRead
	if effectiveIn < 0 {
		effectiveIn = 0
	}
	util := 0.0
	if budget > 0 {
		util = float64(effectiveIn+window5hOut) / float64(budget)
	}

	fmt.Printf("[ryu] quota snapshot: window_in=%d cache_read=%d (excluded) effective_in=%d out=%d budget=%d util=%.1f%%\n",
		window5hIn, window5hCacheRead, effectiveIn, window5hOut, budget, util*100)

	snap := metricsdb.QuotaSnapshot{
		CapturedAt:        m.FinishedAt,
		MissionID:         m.WorkspaceID,
		TokensIn:          m.TokensInTotal,
		TokensOut:         m.TokensOutTotal,
		TokensCacheRead:   m.TokensCacheReadTotal,
		CostUSD:           m.CostUSDTotal,
		Window5hTokensIn:  window5hIn,
		Window5hTokensOut: window5hOut,
		Window5hCostUSD:   window5hCost,
		Estimated5hUtil:   util,
		Model:             dominantModel(m.Phases),
	}
	if err := db.InsertQuotaSnapshot(ctx, snap); err != nil {
		return err
	}

	// Probe real plan-utilization from the Anthropic usage endpoint and persist it.
	// This is intentionally separate from the synthetic quota snapshot above — it
	// uses its own context and its own error path so a probe failure never blocks
	// mission completion.
	probeCtx, probeCancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer probeCancel()

	usageSnap, err := usage.Probe(probeCtx)
	if err != nil {
		fmt.Fprintf(os.Stderr, "[usage-probe] %v\n", err)
		return nil
	}

	usnap := metricsdb.UsageSnapshot{
		MissionID:              m.WorkspaceID,
		CapturedAt:             time.Now().UTC(),
		FiveHourUtil:           usageSnap.FiveHourUtil,
		FiveHourResetsAt:       usageSnap.FiveHourResetsAt,
		SevenDayUtil:           usageSnap.SevenDayUtil,
		SevenDayResetsAt:       usageSnap.SevenDayResetsAt,
		SevenDaySonnetUtil:     usageSnap.SevenDaySonnetUtil,
		SevenDaySonnetResetsAt: usageSnap.SevenDaySonnetResetsAt,
		RawJSON:                usageSnap.RawJSON,
	}
	// Use a fresh context for the insert — probeCtx may have exhausted its 3 s
	// budget during the HTTP probe itself (slow-but-successful probe). The DB
	// write is a local SQLite operation and does not need a network timeout.
	if err := db.InsertUsageSnapshot(context.Background(), usnap); err != nil {
		fmt.Fprintf(os.Stderr, "[usage-probe] insert: %v\n", err)
	}
	return nil
}

// dominantModel returns the model with the highest cost across phases.
// Falls back to the first non-empty model string if costs are zero.
func dominantModel(phases []PhaseMetric) string {
	best := ""
	bestCost := -1.0
	for _, p := range phases {
		if p.Model == "" {
			continue
		}
		if p.CostUSD > bestCost {
			bestCost = p.CostUSD
			best = p.Model
		}
	}
	return best
}

// recordPhaseSkillsDB persists the mission row and current phase skill usage as
// soon as a phase completes. This preserves skill metrics when a later phase or
// mission-level metrics write fails before RecordMetrics runs.
func recordPhaseSkillsDB(ws *core.Workspace, plan *core.Plan, phase *core.Phase, start time.Time) error {
	if ws == nil || plan == nil || phase == nil {
		return fmt.Errorf("workspace, plan, and phase are required")
	}
	if len(phase.Skills) == 0 && len(phase.ParsedSkills) == 0 {
		return nil
	}

	db, err := metricsdb.InitDB("")
	if err != nil {
		return err
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()

	snapshot := toMissionRecord(buildMetrics(ws, plan, &core.ExecutionResult{
		Plan:    plan,
		Success: planSnapshotSucceeded(plan),
	}, start))
	snapshot.Phases = nil
	return db.UpsertMissionPhaseSnapshot(ctx, snapshot, toPhaseRecord(phase))
}

func planSnapshotSucceeded(plan *core.Plan) bool {
	if plan == nil || len(plan.Phases) == 0 {
		return false
	}
	for _, p := range plan.Phases {
		switch p.Status {
		case core.StatusCompleted, core.StatusSkipped:
			continue
		default:
			return false
		}
	}
	return true
}

func phaseRuntimeID(p *core.Phase) string {
	if p == nil {
		return ""
	}
	if p.ID != "" {
		return p.ID
	}
	return p.Name
}

// appendRetryParsedSkills preserves repeated invocations within one attempt,
// but avoids counting the same skill again when it only reappears on a later retry.
func appendRetryParsedSkills(existing, parsed []string) []string {
	if len(parsed) == 0 {
		return existing
	}
	if len(existing) == 0 {
		return append([]string(nil), parsed...)
	}

	seen := make(map[string]struct{}, len(existing))
	for _, skill := range existing {
		seen[skill] = struct{}{}
	}

	merged := append([]string(nil), existing...)
	for _, skill := range parsed {
		if _, ok := seen[skill]; ok {
			continue
		}
		merged = append(merged, skill)
	}
	return merged
}

// toMissionRecord converts the engine-local MissionMetrics to the storage type.
// The two structs are kept separate to avoid an import cycle (engine ← internal/metrics).
func toMissionRecord(m MissionMetrics) metricsdb.MissionRecord {
	phases := make([]metricsdb.PhaseRecord, len(m.Phases))
	for i, p := range m.Phases {
		phases[i] = metricsdb.PhaseRecord{
			ID:                    p.ID,
			Name:                  p.Name,
			Persona:               p.Persona,
			Skills:                append([]string(nil), p.Skills...),
			ParsedSkills:          append([]string(nil), p.ParsedSkills...),
			SelectionMethod:       p.PersonaSelectionMethod,
			DurationS:             p.DurationS,
			Status:                p.Status,
			Retries:               p.Retries,
			GatePassed:            p.GatePassed,
			OutputLen:             p.OutputLen,
			LearningsRetrieved:    p.LearningsRetrieved,
			ErrorType:             p.ErrorType,
			ErrorMessage:          p.ErrorMessage,
			Provider:              p.Provider,
			Model:                 p.Model,
			TokensIn:              p.TokensIn,
			TokensOut:             p.TokensOut,
			TokensCacheCreation:   p.TokensCacheCreation,
			TokensCacheRead:       p.TokensCacheRead,
			CostUSD:               p.CostUSD,
			WorkerName:            p.WorkerName,
			OutputBytes:           p.OutputBytes,
			BarokApplied:          p.BarokApplied,
			BarokRetry:            p.BarokRetry,
			BarokValidatorMs:      p.BarokValidatorMs,
			BarokRetryValidatorMs: p.BarokRetryValidatorMs,
		}
	}
	return metricsdb.MissionRecord{
		WorkspaceID:              m.WorkspaceID,
		Domain:                   m.Domain,
		Task:                     m.Task,
		StartedAt:                m.StartedAt,
		FinishedAt:               m.FinishedAt,
		DurationSec:              m.DurationSec,
		PhasesTotal:              m.PhasesTotal,
		PhasesCompleted:          m.PhasesCompleted,
		PhasesFailed:             m.PhasesFailed,
		PhasesSkipped:            m.PhasesSkipped,
		LearningsRetrieved:       m.LearningsRetrieved,
		RetriesTotal:             m.RetriesTotal,
		GateFailures:             m.GateFailures,
		OutputLenTotal:           m.OutputLenTotal,
		Status:                   m.Status,
		DecompSource:             m.DecompSource,
		Phases:                   phases,
		TokensInTotal:            m.TokensInTotal,
		TokensOutTotal:           m.TokensOutTotal,
		TokensCacheCreationTotal: m.TokensCacheCreationTotal,
		TokensCacheReadTotal:     m.TokensCacheReadTotal,
		CostUSDTotal:             m.CostUSDTotal,
	}
}

// knownCLISkills is the set of CLI skill names the orchestrator recognises in worker output.
var knownCLISkills = map[string]struct{}{
	"scout": {}, "engage": {}, "gmail": {}, "linkedin": {}, "reddit": {},
	"substack": {}, "contentkit": {}, "elevenlabs": {}, "obsidian": {},
	"todoist": {}, "ynab": {}, "scheduler": {}, "publish": {}, "orchestrator": {},
	"watermark": {}, "tracker": {}, "discord": {}, "telegram": {}, "youtube": {},
}

// ParseSkillInvocations scans a worker output transcript for Bash tool calls that
// invoke known CLI skill names. Returns one entry per invocation (duplicates included).
//
// Recognised transcript formats:
//
//	"⏺ Bash(scout gather \"topic\")"   — Claude Code tool indicator
//	"Bash(scout gather \"topic\")"      — without indicator prefix
//	"$ scout gather topic"             — shell prompt
func ParseSkillInvocations(output string) []string {
	var found []string
	for _, line := range strings.Split(output, "\n") {
		cmd := bashCommandFromLine(line)
		if cmd == "" {
			continue
		}
		fields := strings.Fields(cmd)
		if len(fields) == 0 {
			continue
		}
		if _, ok := knownCLISkills[fields[0]]; ok {
			found = append(found, fields[0])
		}
	}
	return found
}

// bashCommandFromLine extracts the command string from a line that contains a Bash
// tool call or shell prompt in a Claude Code transcript. Returns "" if no match.
func bashCommandFromLine(line string) string {
	line = strings.TrimSpace(line)

	// Bash(cmd) format — e.g. "⏺ Bash(scout gather \"topic\")"
	if i := strings.Index(line, "Bash("); i >= 0 {
		rest := line[i+5:]
		if inner, ok := bashCallBody(rest); ok {
			// Strip matching outer quotes: Bash("cmd") or Bash('cmd')
			if len(inner) >= 2 && (inner[0] == '"' || inner[0] == '\'') && inner[len(inner)-1] == inner[0] {
				inner = inner[1 : len(inner)-1]
			}
			return inner
		}
		return ""
	}

	// WorkerOutput tool summary format emitted by worker.Execute:
	// "[tool: Bash scout gather \"topic\"]"
	const toolPrefix = "[tool: Bash "
	if strings.HasPrefix(line, toolPrefix) && strings.HasSuffix(line, "]") {
		cmd := strings.TrimSuffix(strings.TrimPrefix(line, toolPrefix), "]")
		if cmd != "" {
			return cmd
		}
	}

	// Shell prompt format: "$ cmd"
	if strings.HasPrefix(line, "$ ") {
		return line[2:]
	}

	return ""
}

func bashCallBody(rest string) (string, bool) {
	depth := 0
	var quote byte
	escaped := false

	for i := 0; i < len(rest); i++ {
		ch := rest[i]
		if quote != 0 {
			if escaped {
				escaped = false
				continue
			}
			if ch == '\\' {
				escaped = true
				continue
			}
			if ch == quote {
				quote = 0
			}
			continue
		}

		switch ch {
		case '"', '\'':
			quote = ch
		case '(':
			depth++
		case ')':
			if depth == 0 {
				return rest[:i], true
			}
			depth--
		}
	}

	return "", false
}

func phaseParsedSkills(p *core.Phase) []string {
	if len(p.ParsedSkills) > 0 {
		return append([]string(nil), p.ParsedSkills...)
	}
	return ParseSkillInvocations(p.Output)
}

func toPhaseRecord(p *core.Phase) metricsdb.PhaseRecord {
	durS := 0
	if p.StartTime != nil && p.EndTime != nil {
		durS = int(p.EndTime.Sub(*p.StartTime).Seconds())
	}

	pr := metricsdb.PhaseRecord{
		ID:                    phaseRuntimeID(p),
		Name:                  p.Name,
		Persona:               p.Persona,
		Skills:                append([]string(nil), p.Skills...),
		ParsedSkills:          append([]string(nil), p.ParsedSkills...),
		SelectionMethod:       p.PersonaSelectionMethod,
		DurationS:             durS,
		Status:                string(p.Status),
		Retries:               p.Retries,
		GatePassed:            p.GatePassed,
		OutputLen:             p.OutputLen,
		LearningsRetrieved:    p.LearningsRetrieved,
		Provider:              providerForPhase(p),
		Model:                 p.Model,
		TokensIn:              p.TokensIn,
		TokensOut:             p.TokensOut,
		TokensCacheCreation:   p.TokensCacheCreation,
		TokensCacheRead:       p.TokensCacheRead,
		CostUSD:               p.CostUSD,
		WorkerName:            p.Worker,
		OutputBytes:           p.OutputBytes,
		BarokApplied:          p.BarokApplied,
		BarokRetry:            p.BarokRetry,
		BarokValidatorMs:      p.BarokValidatorMs,
		BarokRetryValidatorMs: p.BarokRetryValidatorMs,
	}
	if p.Status == core.StatusFailed && p.Error != "" {
		pr.ErrorType = ParseErrorType(p.Error)
		pr.ErrorMessage = p.Error
	}
	return pr
}

func providerForPhase(p *core.Phase) string {
	if p.Model == "" && p.Runtime == "" {
		return ""
	}
	switch p.Runtime {
	case core.RuntimeCodex:
		return "openai"
	case core.RuntimeClaude, "":
		return "anthropic"
	default:
		return string(p.Runtime)
	}
}

// buildMetrics constructs a MissionMetrics from execution state.
func buildMetrics(ws *core.Workspace, plan *core.Plan, result *core.ExecutionResult, start time.Time) MissionMetrics {
	var completed, failed, skipped int
	var retriesTotal, gateFailures, learningsRetrieved, outputLenTotal int
	var tokensInTotal, tokensOutTotal int
	var tokensCacheCreationTotal, tokensCacheReadTotal int
	var costUSDTotal float64
	var phaseDetails []PhaseMetric

	for _, p := range plan.Phases {
		switch p.Status {
		case core.StatusCompleted:
			completed++
		case core.StatusFailed:
			failed++
		case core.StatusSkipped:
			skipped++
		}

		retriesTotal += p.Retries
		if !p.GatePassed && p.Status == core.StatusCompleted {
			gateFailures++
		}
		learningsRetrieved += p.LearningsRetrieved
		outputLenTotal += p.OutputLen

		// Accumulate cost rollups
		tokensInTotal += p.TokensIn
		tokensOutTotal += p.TokensOut
		tokensCacheCreationTotal += p.TokensCacheCreation
		tokensCacheReadTotal += p.TokensCacheRead
		costUSDTotal += p.CostUSD

		// Per-phase duration
		durS := 0
		if p.StartTime != nil && p.EndTime != nil {
			durS = int(p.EndTime.Sub(*p.StartTime).Seconds())
		}

		pm := PhaseMetric{
			ID:                     phaseRuntimeID(p),
			Name:                   p.Name,
			Persona:                p.Persona,
			Skills:                 append([]string(nil), p.Skills...),
			ParsedSkills:           phaseParsedSkills(p),
			PersonaSelectionMethod: p.PersonaSelectionMethod,
			DurationS:              durS,
			Status:                 string(p.Status),
			Retries:                p.Retries,
			GatePassed:             p.GatePassed,
			OutputLen:              p.OutputLen,
			LearningsRetrieved:     p.LearningsRetrieved,
			Provider:               providerForPhase(p),
			Model:                  p.Model,
			TokensIn:               p.TokensIn,
			TokensOut:              p.TokensOut,
			TokensCacheCreation:    p.TokensCacheCreation,
			TokensCacheRead:        p.TokensCacheRead,
			CostUSD:                p.CostUSD,
			WorkerName:             p.Worker,
			OutputBytes:            p.OutputBytes,
			BarokApplied:           p.BarokApplied,
			BarokRetry:             p.BarokRetry,
			BarokValidatorMs:       p.BarokValidatorMs,
			BarokRetryValidatorMs:  p.BarokRetryValidatorMs,
		}
		if p.Status == core.StatusFailed && p.Error != "" {
			pm.ErrorType = ParseErrorType(p.Error)
			pm.ErrorMessage = p.Error
		}
		phaseDetails = append(phaseDetails, pm)
	}

	status := "success"
	if !result.Success {
		if completed > 0 {
			status = "partial"
		} else {
			status = "failure"
		}
	}

	end := time.Now()
	return MissionMetrics{
		WorkspaceID:              ws.ID,
		Domain:                   ws.Domain,
		Task:                     plan.Task,
		StartedAt:                start,
		FinishedAt:               end,
		DurationSec:              int(end.Sub(start).Seconds()),
		PhasesTotal:              len(plan.Phases),
		PhasesCompleted:          completed,
		PhasesFailed:             failed,
		PhasesSkipped:            skipped,
		LearningsRetrieved:       learningsRetrieved,
		RetriesTotal:             retriesTotal,
		GateFailures:             gateFailures,
		OutputLenTotal:           outputLenTotal,
		Status:                   status,
		DecompSource:             plan.DecompSource,
		Phases:                   phaseDetails,
		TokensInTotal:            tokensInTotal,
		TokensOutTotal:           tokensOutTotal,
		TokensCacheCreationTotal: tokensCacheCreationTotal,
		TokensCacheReadTotal:     tokensCacheReadTotal,
		CostUSDTotal:             costUSDTotal,
	}
}
