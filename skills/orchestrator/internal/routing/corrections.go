package routing

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"

	"github.com/joeyhipolito/orchestrator-cli/internal/audit"
	"github.com/joeyhipolito/orchestrator-cli/internal/config"
	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// TargetResolver maps a workspace ID to a canonical target ID (e.g. "repo:~/skills/orchestrator").
// Returning "" signals the resolver could not determine a target; the caller falls back
// to "workspace:<id>".
type TargetResolver func(workspaceID string) string

// ResolveWorkspaceTarget reads the target_id file written by `orchestrator run`
// at workspace creation time. This file contains the canonical repo target
// (e.g. "repo:~/skills/orchestrator") resolved from the cwd's git root.
// Returns "" when the file is missing or unreadable (e.g. old workspaces
// created before target_id was recorded).
func ResolveWorkspaceTarget(workspaceID string) string {
	base, err := config.Dir()
	if err != nil {
		return ""
	}
	data, err := os.ReadFile(filepath.Join(base, "workspaces", workspaceID, "target_id"))
	if err != nil {
		return ""
	}
	target := strings.TrimSpace(string(data))
	if target == "" {
		return ""
	}
	return target
}

// resolveTargetID maps a workspace ID to a canonical target ID using the
// provided resolver. Falls back to "workspace:<id>" when the resolver is nil
// or returns "".
func resolveTargetID(workspaceID string, resolve TargetResolver) string {
	if resolve != nil {
		if id := resolve(workspaceID); id != "" {
			return id
		}
	}
	return TargetTypeWorkspace + ":" + workspaceID
}

// ExtractAuditCorrections scans audit reports for explicit persona mismatches
// and returns routing corrections with Source=SourceAudit. Only phases where
// the audit LLM explicitly flagged persona_correct=false AND provided a
// persona_ideal value are extracted — this is the high-signal path.
//
// resolve maps workspace IDs to canonical target IDs (e.g. "repo:~/skills/orchestrator").
// When resolve is nil or returns "", the fallback "workspace:<id>" is used.
// Pass ResolveWorkspaceTarget as the resolver in production.
func ExtractAuditCorrections(reports []audit.AuditReport, resolve TargetResolver) []RoutingCorrection {
	var corrections []RoutingCorrection
	seen := make(map[string]bool)

	for _, r := range reports {
		targetID := resolveTargetID(r.WorkspaceID, resolve)

		for _, phase := range r.Phases {
			if phase.PersonaCorrect {
				continue
			}
			if phase.PersonaAssigned == "" || phase.PersonaIdeal == "" {
				continue
			}
			// Same assigned→ideal for same target+hint is a duplicate.
			key := fmt.Sprintf("%s|%s|%s|%s", targetID, phase.PersonaAssigned, phase.PersonaIdeal, phase.PhaseName)
			if seen[key] {
				continue
			}
			seen[key] = true

			corrections = append(corrections, RoutingCorrection{
				TargetID:        targetID,
				AssignedPersona: phase.PersonaAssigned,
				IdealPersona:    phase.PersonaIdeal,
				TaskHint:        phase.PhaseName,
				Source:          SourceAudit,
			})
		}
	}
	return corrections
}

// DecompFindingRow is a decomposition finding ready for DB insertion,
// enriched with target and workspace context from the audit report.
type DecompFindingRow struct {
	TargetID    string
	WorkspaceID string
	audit.DecompositionFinding
}

// NewPassiveFindingRow constructs a DecompFindingRow for a passive audit finding.
// Callers do not need to import the audit package — all fields are wired here.
// AuditScore is always 0 for passive findings; they remain distinct from audited
// findings and flow back through the dedicated passive-finding read path.
func NewPassiveFindingRow(targetID, wsID, findingType, phaseName, detail string) DecompFindingRow {
	return DecompFindingRow{
		TargetID:    targetID,
		WorkspaceID: wsID,
		DecompositionFinding: audit.DecompositionFinding{
			FindingType:  findingType,
			PhaseName:    phaseName,
			Detail:       detail,
			DecompSource: "passive",
			AuditScore:   0,
		},
	}
}

// ExtractDecompFindings scans audit reports and produces structured
// decomposition findings from ConvergenceStatus and PhaseEvaluation data.
// Only reports with Overall >= minScore are processed — low-scoring audits
// are too noisy to learn from.
//
// resolve maps workspace IDs to canonical target IDs; nil or "" falls back
// to "workspace:<id>". loader reads the plan from the workspace checkpoint
// to populate DecompSource; nil loader leaves DecompSource empty.
func ExtractDecompFindings(reports []audit.AuditReport, resolve TargetResolver, loader WorkspacePlanLoader, minScore int) []DecompFindingRow {
	var findings []DecompFindingRow

	for _, r := range reports {
		if r.Scorecard.Overall < minScore {
			continue
		}

		targetID := resolveTargetID(r.WorkspaceID, resolve)

		// Load decomp_source from the workspace plan when a loader is available.
		var decompSource string
		if loader != nil {
			if plan, err := loader(r.WorkspaceID); err == nil && plan != nil {
				decompSource = plan.DecompSource
			}
		}

		base := DecompFindingRow{
			TargetID:    targetID,
			WorkspaceID: r.WorkspaceID,
		}

		// Missing phases from convergence.
		for _, name := range r.Convergence.MissingPhases {
			f := base
			f.DecompositionFinding = audit.DecompositionFinding{
				FindingType:  audit.FindingMissingPhase,
				PhaseName:    "",
				Detail:       name,
				DecompSource: decompSource,
				AuditScore:   r.Scorecard.Overall,
			}
			findings = append(findings, f)
		}

		// Redundant work from convergence.
		for _, name := range r.Convergence.RedundantWork {
			f := base
			f.DecompositionFinding = audit.DecompositionFinding{
				FindingType:  audit.FindingRedundantPhase,
				PhaseName:    name,
				Detail:       fmt.Sprintf("phase %q produced no useful output", name),
				DecompSource: decompSource,
				AuditScore:   r.Scorecard.Overall,
			}
			findings = append(findings, f)
		}

		// Drift phases from convergence.
		for _, name := range r.Convergence.DriftPhases {
			f := base
			f.DecompositionFinding = audit.DecompositionFinding{
				FindingType:  audit.FindingPhaseDrift,
				PhaseName:    name,
				Detail:       fmt.Sprintf("phase %q diverged from its objective", name),
				DecompSource: decompSource,
				AuditScore:   r.Scorecard.Overall,
			}
			findings = append(findings, f)
		}

		// Per-phase findings.
		for _, phase := range r.Phases {
			// Wrong persona.
			if !phase.PersonaCorrect && phase.PersonaAssigned != "" && phase.PersonaIdeal != "" {
				f := base
				f.DecompositionFinding = audit.DecompositionFinding{
					FindingType:  audit.FindingWrongPersona,
					PhaseName:    phase.PhaseName,
					Detail:       fmt.Sprintf("assigned %s, ideal %s", phase.PersonaAssigned, phase.PersonaIdeal),
					DecompSource: decompSource,
					AuditScore:   r.Scorecard.Overall,
				}
				findings = append(findings, f)
			}

			// Low phase score.
			if phase.Score > 0 && phase.Score <= 2 {
				f := base
				f.DecompositionFinding = audit.DecompositionFinding{
					FindingType:  audit.FindingLowPhaseScore,
					PhaseName:    phase.PhaseName,
					Detail:       fmt.Sprintf("score %d/5", phase.Score),
					DecompSource: decompSource,
					AuditScore:   r.Scorecard.Overall,
				}
				findings = append(findings, f)
			}
		}
	}
	return findings
}

// compactPhase is the minimal structure serialized into phases_json.
// Only structural fields — no runtime state, no output, no cost data.
type compactPhase struct {
	Name      string   `json:"name"`
	Objective string   `json:"objective"`
	Persona   string   `json:"persona"`
	Skills    []string `json:"skills,omitempty"`
	Depends   []string `json:"depends,omitempty"`
}

// WorkspacePlanLoader loads a plan from a workspace's checkpoint.
// In production, pass LoadWorkspacePlan. Tests can substitute a stub.
type WorkspacePlanLoader func(workspaceID string) (*core.Plan, error)

// LoadWorkspacePlan reads the checkpoint.json from a workspace and returns
// the plan. Returns an error if the workspace or checkpoint cannot be read.
func LoadWorkspacePlan(workspaceID string) (*core.Plan, error) {
	base, err := config.Dir()
	if err != nil {
		return nil, fmt.Errorf("config dir: %w", err)
	}
	wsPath := filepath.Join(base, "workspaces", workspaceID)
	cp, err := core.LoadCheckpoint(wsPath)
	if err != nil {
		return nil, fmt.Errorf("loading checkpoint for %s: %w", workspaceID, err)
	}
	return cp.Plan, nil
}

// DecompExamplesResult holds extracted examples plus metadata about skipped reports.
type DecompExamplesResult struct {
	Examples []DecompExample
	Skipped  int // reports that passed quality gate but had missing workspace/checkpoint
}

// ExtractDecompExamples scans audit reports for missions that pass the quality
// gate (both Overall and DecompositionQuality >= minScore) and extracts their
// decomposition structure as reusable examples.
//
// For each qualifying report, the plan is loaded from the workspace checkpoint
// to capture the resolved phase structure (not the raw task text). Reports
// whose workspaces are missing or unreadable are counted in Skipped.
//
// resolve maps workspace IDs to canonical target IDs; nil or "" falls back
// to "workspace:<id>". loader reads the plan from the workspace checkpoint.
func ExtractDecompExamples(reports []audit.AuditReport, resolve TargetResolver, loader WorkspacePlanLoader, minScore int) DecompExamplesResult {
	var result DecompExamplesResult
	seen := make(map[string]bool) // "target|workspace" → deduplicate

	for _, r := range reports {
		if r.Scorecard.Overall < minScore || r.Scorecard.DecompositionQuality < minScore {
			continue
		}

		targetID := resolveTargetID(r.WorkspaceID, resolve)
		key := targetID + "|" + r.WorkspaceID
		if seen[key] {
			continue
		}
		seen[key] = true

		plan, err := loader(r.WorkspaceID)
		if err != nil || plan == nil {
			result.Skipped++
			continue
		}

		phasesJSON := buildCompactPhasesJSON(plan.Phases)
		if phasesJSON == "" {
			continue
		}

		taskSummary := plan.Task
		if utf8.RuneCountInString(taskSummary) > 200 {
			taskSummary = string([]rune(taskSummary)[:200])
		}

		result.Examples = append(result.Examples, DecompExample{
			TargetID:      targetID,
			WorkspaceID:   r.WorkspaceID,
			TaskSummary:   taskSummary,
			PhaseCount:    len(plan.Phases),
			ExecutionMode: plan.ExecutionMode,
			PhasesJSON:    phasesJSON,
			DecompSource:  plan.DecompSource,
			AuditScore:    r.Scorecard.Overall,
			DecompQuality: r.Scorecard.DecompositionQuality,
			PersonaFit:    r.Scorecard.PersonaFit,
		})
	}
	return result
}

// IngestResult summarizes what was persisted during audit ingestion.
type IngestResult struct {
	Corrections       int // routing corrections inserted
	CorrectionsDupes  int // duplicate corrections skipped
	Findings          int // decomposition findings inserted
	Examples          int // decomposition examples inserted
	ExamplesSkipped   int // examples skipped (missing workspace/checkpoint)
	RoutingPatterns   int // routing patterns recorded (positive persona signals)
	HandoffPatterns   int // handoff patterns recorded (persona transitions)
}

// routingPatternEntry is a deduplicated routing pattern observation extracted
// from audit reports, ready for recording via RecordRoutingPattern.
type routingPatternEntry struct {
	TargetID string
	Persona  string
	TaskHint string
}

// ExtractRoutingPatterns scans audit reports for phases where the persona was
// confirmed correct (PersonaCorrect == true) and returns positive routing
// observations. Only reports with Overall >= minScore are processed — low-scoring
// audits are too noisy for positive signal.
//
// This is the complementary path to ExtractAuditCorrections: corrections learn
// from mistakes, routing patterns learn from successes.
func ExtractRoutingPatterns(reports []audit.AuditReport, resolve TargetResolver, minScore int) []routingPatternEntry {
	var patterns []routingPatternEntry
	seen := make(map[string]bool)

	for _, r := range reports {
		if r.Scorecard.Overall < minScore {
			continue
		}

		targetID := resolveTargetID(r.WorkspaceID, resolve)

		for _, phase := range r.Phases {
			if !phase.PersonaCorrect {
				continue
			}
			if phase.PersonaAssigned == "" {
				continue
			}
			key := targetID + "|" + phase.PersonaAssigned + "|" + phase.PhaseName
			if seen[key] {
				continue
			}
			seen[key] = true

			patterns = append(patterns, routingPatternEntry{
				TargetID: targetID,
				Persona:  phase.PersonaAssigned,
				TaskHint: phase.PhaseName,
			})
		}
	}
	return patterns
}

// handoffPatternEntry is a deduplicated handoff observation extracted from
// audit reports, ready for recording via RecordHandoffPattern.
type handoffPatternEntry struct {
	TargetID    string
	FromPersona string
	ToPersona   string
	TaskHint    string
}

// ExtractHandoffPatterns scans audit reports for consecutive phase transitions
// and returns persona-to-persona handoff observations. Only reports with
// Overall >= minScore are processed. Only phases whose assigned personas were
// judged correct are considered, and self-transitions (same persona → same
// persona) are excluded.
func ExtractHandoffPatterns(reports []audit.AuditReport, resolve TargetResolver, minScore int) []handoffPatternEntry {
	var patterns []handoffPatternEntry
	seen := make(map[string]bool)

	for _, r := range reports {
		if r.Scorecard.Overall < minScore {
			continue
		}
		if len(r.Phases) < 2 {
			continue
		}

		targetID := resolveTargetID(r.WorkspaceID, resolve)

		for i := 1; i < len(r.Phases); i++ {
			fromPhase := r.Phases[i-1]
			toPhase := r.Phases[i]
			if !fromPhase.PersonaCorrect || !toPhase.PersonaCorrect {
				continue
			}

			from := fromPhase.PersonaAssigned
			to := toPhase.PersonaAssigned
			if from == "" || to == "" || from == to {
				continue
			}

			// Use the receiving phase name as the task hint — it describes
			// what kind of work the handoff leads to.
			hint := toPhase.PhaseName
			key := targetID + "|" + from + "|" + to + "|" + hint
			if seen[key] {
				continue
			}
			seen[key] = true

			patterns = append(patterns, handoffPatternEntry{
				TargetID:    targetID,
				FromPersona: from,
				ToPersona:   to,
				TaskHint:    hint,
			})
		}
	}
	return patterns
}

// DefaultMinAuditScore is the minimum overall audit score required for
// findings and examples to be ingested. Low-scoring audits are too noisy.
const DefaultMinAuditScore = 3

// IngestAuditReports extracts routing corrections, decomposition findings,
// and decomposition examples from audit reports and persists them to the
// routing database. This is the single code path used by both the audit
// command (auto-ingest) and the routing ingest-audit command.
func IngestAuditReports(ctx context.Context, rdb *RoutingDB, reports []audit.AuditReport, resolve TargetResolver, loader WorkspacePlanLoader) (IngestResult, error) {
	var result IngestResult

	if len(reports) == 0 {
		return result, nil
	}

	// Routing corrections (no score gate — any explicit mismatch is signal).
	corrections := ExtractAuditCorrections(reports, resolve)
	if len(corrections) > 0 {
		inserted, err := rdb.InsertRoutingCorrections(ctx, corrections)
		if err != nil {
			return result, fmt.Errorf("inserting routing corrections: %w", err)
		}
		result.Corrections = inserted
		result.CorrectionsDupes = len(corrections) - inserted
	}

	// Routing patterns — positive persona signals (score-gated).
	routingPats := ExtractRoutingPatterns(reports, resolve, DefaultMinAuditScore)
	for _, rp := range routingPats {
		if err := rdb.RecordRoutingPattern(ctx, rp.TargetID, rp.Persona, rp.TaskHint); err != nil {
			if ctx.Err() != nil {
				return result, fmt.Errorf("recording routing pattern: %w", err)
			}
			continue // non-fatal DB error — skip this pattern
		}
		result.RoutingPatterns++
	}

	// Handoff patterns — persona transitions (score-gated).
	handoffPats := ExtractHandoffPatterns(reports, resolve, DefaultMinAuditScore)
	for _, hp := range handoffPats {
		if err := rdb.RecordHandoffPattern(ctx, hp.TargetID, hp.FromPersona, hp.ToPersona, hp.TaskHint); err != nil {
			if ctx.Err() != nil {
				return result, fmt.Errorf("recording handoff pattern: %w", err)
			}
			continue // non-fatal DB error — skip this pattern
		}
		result.HandoffPatterns++
	}

	// Decomposition findings (score-gated).
	findings := ExtractDecompFindings(reports, resolve, loader, DefaultMinAuditScore)
	if len(findings) > 0 {
		inserted, err := rdb.InsertDecompFindings(ctx, findings)
		if err != nil {
			return result, fmt.Errorf("inserting decomposition findings: %w", err)
		}
		result.Findings = inserted
	}

	// Decomposition examples (score-gated on both Overall and DecompQuality).
	exResult := ExtractDecompExamples(reports, resolve, loader, DefaultMinAuditScore)
	result.ExamplesSkipped = exResult.Skipped
	for _, ex := range exResult.Examples {
		if err := rdb.InsertDecompExample(ctx, ex); err != nil {
			if ctx.Err() != nil {
				return result, fmt.Errorf("inserting decomposition example: %w", err)
			}
			continue // duplicate or non-fatal DB error — skip this example
		}
		result.Examples++
	}

	return result, nil
}

// buildCompactPhasesJSON serializes plan phases into the compact JSON format
// stored in decomposition_examples.phases_json. Only structural fields are
// included — no runtime state, output, or cost data.
func buildCompactPhasesJSON(phases []*core.Phase) string {
	if len(phases) == 0 {
		return ""
	}
	compact := make([]compactPhase, len(phases))
	for i, p := range phases {
		compact[i] = compactPhase{
			Name:      p.Name,
			Objective: p.Objective,
			Persona:   p.Persona,
			Skills:    p.Skills,
			Depends:   p.Dependencies,
		}
	}
	data, err := json.Marshal(compact)
	if err != nil {
		return ""
	}
	return string(data)
}
