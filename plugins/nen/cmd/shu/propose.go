package main

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"text/template"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
	"github.com/joeyhipolito/nen/ko"
)

// --- Types shared with review.go ---

type trackerIssue struct {
	ID          string  `json:"id"`
	SeqID       *int64  `json:"seq_id"`
	Title       string  `json:"title"`
	Description *string `json:"description"`
	Status      string  `json:"status"`
	Priority    *string `json:"priority"`
	Labels      *string `json:"labels"`
	Assignee    *string `json:"assignee"`
	ParentID    *string `json:"parent_id"`
	CreatedAt   string  `json:"created_at"`
	UpdatedAt   string  `json:"updated_at"`
}

func (t trackerIssue) displayID() string {
	if t.SeqID != nil {
		return fmt.Sprintf("TRK-%d", *t.SeqID)
	}
	return t.ID
}

func (t trackerIssue) hasLabel(label string) bool {
	if t.Labels == nil {
		return false
	}
	for _, l := range strings.Split(*t.Labels, ",") {
		if strings.TrimSpace(l) == label {
			return true
		}
	}
	return false
}

type trackerItemsResponse struct {
	Items []trackerIssue `json:"items"`
	Count int            `json:"count"`
}

func getTrackerIssues(ctx context.Context) ([]trackerIssue, error) {
	cmd := exec.CommandContext(ctx, "tracker", "query", "items", "--json")
	out, err := cmd.Output()
	if err != nil {
		return nil, fmt.Errorf("tracker query items: %w", err)
	}
	var resp trackerItemsResponse
	if err := json.Unmarshal(out, &resp); err != nil {
		return nil, fmt.Errorf("parsing tracker items: %w", err)
	}
	return resp.Items, nil
}

// --- Propose types ---

type proposableFinding struct {
	ID          string
	Ability     string
	Category    string
	Severity    string
	Title       string
	Description string
	ScopeKind   string
	ScopeValue  string
	Evidence    []evidenceItem
	Source      string
	FoundAt     time.Time
}

type evidenceItem struct {
	Kind       string `json:"kind"`
	Raw        string `json:"raw"`
	Source     string `json:"source"`
	CapturedAt string `json:"captured_at"`
}

type proposal struct {
	TrackerIssue string   `json:"tracker_issue"`
	MissionFile  string   `json:"mission_file"`
	FindingIDs   []string `json:"finding_ids"`
	Severity     string   `json:"severity"`
	Title        string   `json:"title"`
}

type skippedFinding struct {
	FindingID string `json:"finding_id"`
	Reason    string `json:"reason"`
}

type proposeOutput struct {
	Proposed []proposal       `json:"proposed"`
	Skipped  []skippedFinding `json:"skipped"`
}

const rateLimitMax = 5

// proposeItem is either a single finding or a batch of 3+ findings sharing ability+category.
type proposeItem struct {
	isBatch  bool
	findings []proposableFinding
	severity string // highest severity for priority ordering
}

// severityWeight returns a sort weight (lower = higher priority).
func severityWeight(s string) int {
	switch s {
	case "critical":
		return 0
	case "high":
		return 1
	default:
		return 2
	}
}

// buildRegularItems groups findings by ability+category. Groups of 3+ become a single batch
// item; smaller groups are kept as individual items. Items are sorted severity-first.
func buildRegularItems(findings []proposableFinding) []proposeItem {
	type key struct{ ability, category string }
	var order []key
	groups := make(map[key][]proposableFinding)
	seen := make(map[key]bool)
	for _, f := range findings {
		k := key{f.Ability, f.Category}
		if !seen[k] {
			order = append(order, k)
			seen[k] = true
		}
		groups[k] = append(groups[k], f)
	}

	var items []proposeItem
	for _, k := range order {
		fs := groups[k]
		sev := fs[0].Severity // SQL orders by severity; first is highest
		if len(fs) >= 3 {
			items = append(items, proposeItem{isBatch: true, findings: fs, severity: sev})
		} else {
			for _, f := range fs {
				items = append(items, proposeItem{isBatch: false, findings: []proposableFinding{f}, severity: f.Severity})
			}
		}
	}
	sort.Slice(items, func(i, j int) bool {
		return severityWeight(items[i].severity) < severityWeight(items[j].severity)
	})
	return items
}

// computeBatchDedupKey produces a stable key for a batch of findings sharing ability+category.
func computeBatchDedupKey(ability, category string) string {
	h := sha256.Sum256([]byte("batch:" + ability + ":" + category))
	return fmt.Sprintf("%x", h[:8])
}

// remediationMissionDir returns the directory where remediation mission files are stored.
// It honours ALLUKA_HOME (via scan.Dir) and falls back to ~/.alluka on error.
func remediationMissionDir() string {
	base, err := scan.Dir()
	if err != nil {
		home, _ := os.UserHomeDir()
		base = filepath.Join(home, ".alluka")
	}
	return filepath.Join(base, "missions", "remediation")
}

// batchMissionFilePath returns the mission file path for a batched ability+category group.
func batchMissionFilePath(ability, category string) string {
	date := time.Now().Format("2006-01-02")
	slug := slugify(fmt.Sprintf("batch-%s-%s", ability, category))
	if len(slug) > 60 {
		slug = slug[:60]
	}
	return filepath.Join(remediationMissionDir(), fmt.Sprintf("%s-%s.md", date, slug))
}

type batchFindingDetail struct {
	ID          string
	Title       string
	Severity    string
	Description string
	Evidence    string
}

type batchMissionData struct {
	TrackerIssue   string
	FindingIDs     []string
	Severity       string
	Ability        string
	Category       string
	GeneratedAt    string
	Count          int
	FindingDetails []batchFindingDetail
	SuccessCriteria []string
}

var batchMissionTemplate = template.Must(template.New("batch-mission").Parse(`---
source: shu-propose
tracker_issue: {{.TrackerIssue}}
finding_ids:{{range .FindingIDs}}
  - {{.}}{{end}}
severity: {{.Severity}}
ability: {{.Ability}}
category: {{.Category}}
generated_at: "{{.GeneratedAt}}"
domain: dev
target: repo:~/nanika
---

# Fix: {{.Count}} {{.Category}} findings in {{.Ability}}

## Summary

{{.Count}} findings in ability **{{.Ability}}**, category **{{.Category}}** require remediation.

## Findings
{{range .FindingDetails}}
### {{.ID}}: {{.Title}} ({{.Severity}})

{{.Description}}

Evidence:
{{.Evidence}}
{{end}}
PHASE: investigate | OBJECTIVE: Investigate all {{.Count}} {{.Category}} findings in {{.Ability}}. Identify root causes and common patterns. Document a remediation plan | PERSONA: senior-backend-engineer

PHASE: fix | OBJECTIVE: Fix all {{.Count}} {{.Category}} findings in {{.Ability}} identified in the investigate phase. Add test coverage for each failure scenario | PERSONA: senior-backend-engineer | DEPENDS: investigate

PHASE: verify | OBJECTIVE: Re-run the scanner for {{.Ability}} {{.Category}} to confirm all {{.Count}} findings are resolved. Run the full test suite | PERSONA: qa-engineer | DEPENDS: fix

## Success Criteria
{{range .SuccessCriteria}}
- [ ] {{.}}{{end}}
`))

func generateBatchMission(ability, category string, findings []proposableFinding, trackerID string) (string, error) {
	findingIDs := make([]string, len(findings))
	details := make([]batchFindingDetail, len(findings))
	for i, f := range findings {
		findingIDs[i] = f.ID
		details[i] = batchFindingDetail{
			ID:          f.ID,
			Title:       f.Title,
			Severity:    f.Severity,
			Description: f.Description,
			Evidence:    formatEvidenceSummary(f.Evidence),
		}
	}
	data := batchMissionData{
		TrackerIssue:   trackerID,
		FindingIDs:     findingIDs,
		Severity:       findings[0].Severity, // highest (SQL-ordered)
		Ability:        ability,
		Category:       category,
		GeneratedAt:    time.Now().UTC().Format(time.RFC3339),
		Count:          len(findings),
		FindingDetails: details,
		SuccessCriteria: []string{
			fmt.Sprintf("All %d %s findings in %s are resolved", len(findings), category, ability),
			"Scanner re-run produces no new findings for this category",
			"Test cases cover each identified failure mode",
		},
	}
	var buf strings.Builder
	if err := batchMissionTemplate.Execute(&buf, data); err != nil {
		return "", fmt.Errorf("executing batch template: %w", err)
	}
	return buf.String(), nil
}

func runPropose(args []string) error {
	fs := flag.NewFlagSet("propose", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	dryRun := fs.Bool("dry-run", false, "Show what would be proposed without creating issues or files")
	jsonOut := fs.Bool("json", false, "Output proposals as JSON")
	initFlag := fs.Bool("init", false, "Set up scheduler jobs for propose, dispatch, and weekly evaluate")
	if err := fs.Parse(args); err != nil {
		return err
	}

	if *initFlag {
		return runProposeInit()
	}

	if _, err := exec.LookPath("tracker"); err != nil {
		return fmt.Errorf("tracker plugin required for self-improvement loop (not found in PATH)")
	}

	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()

	findings, err := queryProposableFindings(ctx)
	if err != nil {
		return fmt.Errorf("querying findings: %w", err)
	}

	if len(findings) == 0 {
		if *jsonOut {
			return encodeJSON(proposeOutput{Proposed: []proposal{}, Skipped: []skippedFinding{}})
		}
		fmt.Println("No proposable findings.")
		return nil
	}

	existing, err := getTrackerIssues(ctx)
	if err != nil {
		return fmt.Errorf("querying tracker: %w", err)
	}

	proposalsDB, err := openProposalsDB()
	if err != nil {
		return fmt.Errorf("opening proposals db: %w", err)
	}
	defer proposalsDB.Close()

	qs, err := ko.NewQualityStore(proposalsDB)
	if err != nil {
		return fmt.Errorf("opening quality store: %w", err)
	}

	out := proposeOutput{Proposed: []proposal{}, Skipped: []skippedFinding{}}
	proposedKeys := make(map[string]bool)
	deferred := 0

	// Separate review-blocker findings for grouped workspace handling.
	var regularFindings, reviewBlockers []proposableFinding
	for _, f := range findings {
		if f.Category == "review-blocker" {
			reviewBlockers = append(reviewBlockers, f)
		} else {
			regularFindings = append(regularFindings, f)
		}
	}

	// Group regular findings: 3+ in same ability+category → single batched issue.
	// Items are sorted severity-first (critical → high → medium).
	for _, item := range buildRegularItems(regularFindings) {
		if len(out.Proposed) >= rateLimitMax {
			deferred += len(item.findings)
			for _, f := range item.findings {
				out.Skipped = append(out.Skipped, skippedFinding{
					FindingID: f.ID,
					Reason:    "rate limit reached — deferred to next cycle",
				})
			}
			continue
		}

		if item.isBatch {
			ability := item.findings[0].Ability
			category := item.findings[0].Category
			key := computeBatchDedupKey(ability, category)

			if proposedKeys[key] {
				for _, f := range item.findings {
					out.Skipped = append(out.Skipped, skippedFinding{FindingID: f.ID, Reason: "batch duplicate in this run"})
				}
				continue
			}
			if recent, err := wasRecentlyProposed(proposalsDB, key); err != nil {
				return fmt.Errorf("checking proposal recency for batch %s/%s: %w", ability, category, err)
			} else if recent {
				for _, f := range item.findings {
					out.Skipped = append(out.Skipped, skippedFinding{FindingID: f.ID, Reason: "proposed within the last 24 hours"})
				}
				continue
			}
			batchPolicy := defaultDedupPolicy()
			batchPolicy.qualityMultiplier = qs.LookupScore(ctx, ability, category)
			if issueID := findBlockingIssue(existing, key, batchPolicy, time.Now().UTC()); issueID != "" {
				for _, f := range item.findings {
					out.Skipped = append(out.Skipped, skippedFinding{FindingID: f.ID, Reason: fmt.Sprintf("blocking tracker issue %s covers same category", issueID)})
				}
				continue
			}

			n := len(item.findings)
			title := fmt.Sprintf("Fix: %d %s findings in %s", n, category, ability)
			missionPath := batchMissionFilePath(ability, category)
			findingIDs := make([]string, n)
			for i, f := range item.findings {
				findingIDs[i] = f.ID
			}

			if *dryRun {
				if !*jsonOut {
					fmt.Printf("--- dry-run batch: %d %s/%s findings ---\n", n, ability, category)
				}
				out.Proposed = append(out.Proposed, proposal{
					TrackerIssue: "(dry-run)",
					MissionFile:  missionPath,
					FindingIDs:   findingIDs,
					Severity:     item.severity,
					Title:        title,
				})
				proposedKeys[key] = true
				continue
			}

			priority := severityToPriority(item.severity)
			var descBuf strings.Builder
			fmt.Fprintf(&descBuf, "Mission: %s\n\nBatched Findings (%d):\n", missionPath, n)
			for _, f := range item.findings {
				fmt.Fprintf(&descBuf, "- %s: %s (%s)\n  Evidence:\n%s\n", f.ID, f.Title, f.Severity, formatEvidenceSummary(f.Evidence))
			}
			labels := fmt.Sprintf("auto,nen,%s,dedup:%s", ability, key)

			trackerID, err := createTrackerIssue(title, priority, labels, descBuf.String())
			if err != nil {
				return fmt.Errorf("creating batch tracker issue for %s/%s: %w", ability, category, err)
			}

			content, err := generateBatchMission(ability, category, item.findings, trackerID)
			if err != nil {
				return fmt.Errorf("generating batch mission for %s/%s: %w", ability, category, err)
			}
			if err := os.MkdirAll(filepath.Dir(missionPath), 0o755); err != nil {
				return fmt.Errorf("creating mission dir: %w", err)
			}
			if err := os.WriteFile(missionPath, []byte(content), 0o644); err != nil {
				return fmt.Errorf("writing batch mission file: %w", err)
			}
			if err := recordProposed(proposalsDB, key, ability, category, trackerID); err != nil {
				return fmt.Errorf("recording batch proposal for %s/%s: %w", ability, category, err)
			}

			out.Proposed = append(out.Proposed, proposal{
				TrackerIssue: trackerID,
				MissionFile:  missionPath,
				FindingIDs:   findingIDs,
				Severity:     item.severity,
				Title:        title,
			})
			proposedKeys[key] = true
		} else {
			f := item.findings[0]
			key := computeDedupKey(f)

			if proposedKeys[key] {
				out.Skipped = append(out.Skipped, skippedFinding{FindingID: f.ID, Reason: "duplicate of finding proposed in this batch"})
				continue
			}
			if recent, err := wasRecentlyProposed(proposalsDB, key); err != nil {
				return fmt.Errorf("checking proposal recency for %s: %w", f.ID, err)
			} else if recent {
				out.Skipped = append(out.Skipped, skippedFinding{FindingID: f.ID, Reason: "proposed within the last 24 hours"})
				continue
			}
			findingPolicy := defaultDedupPolicy()
			findingPolicy.qualityMultiplier = qs.LookupScore(ctx, f.Ability, f.Category)
			if issueID := findBlockingIssue(existing, key, findingPolicy, time.Now().UTC()); issueID != "" {
				out.Skipped = append(out.Skipped, skippedFinding{FindingID: f.ID, Reason: fmt.Sprintf("blocking tracker issue %s covers same category", issueID)})
				continue
			}

			missionPath := missionFilePath(f)
			if *dryRun {
				content, _ := generateMission(f, "(dry-run)")
				if !*jsonOut {
					fmt.Printf("--- dry-run: %s ---\n%s\n---\n\n", f.ID, content)
				}
				out.Proposed = append(out.Proposed, proposal{
					TrackerIssue: "(dry-run)",
					MissionFile:  missionPath,
					FindingIDs:   []string{f.ID},
					Severity:     f.Severity,
					Title:        "Fix: " + f.Title,
				})
				proposedKeys[key] = true
				continue
			}

			priority := severityToPriority(f.Severity)
			labels := fmt.Sprintf("auto,nen,%s,dedup:%s", f.Ability, key)
			desc := fmt.Sprintf("Mission: %s\n\nFindings:\n- %s: %s (%s)\n\nEvidence:\n%s",
				missionPath, f.ID, f.Title, f.Severity, formatEvidenceSummary(f.Evidence))

			trackerID, err := createTrackerIssue("Fix: "+f.Title, priority, labels, desc)
			if err != nil {
				return fmt.Errorf("creating tracker issue for %s: %w", f.ID, err)
			}

			content, err := generateMission(f, trackerID)
			if err != nil {
				return fmt.Errorf("generating mission for %s: %w", f.ID, err)
			}
			if err := os.MkdirAll(filepath.Dir(missionPath), 0o755); err != nil {
				return fmt.Errorf("creating mission dir: %w", err)
			}
			if err := os.WriteFile(missionPath, []byte(content), 0o644); err != nil {
				return fmt.Errorf("writing mission file: %w", err)
			}
			if err := recordProposed(proposalsDB, key, f.Ability, f.Category, trackerID); err != nil {
				return fmt.Errorf("recording proposal for %s: %w", f.ID, err)
			}

			out.Proposed = append(out.Proposed, proposal{
				TrackerIssue: trackerID,
				MissionFile:  missionPath,
				FindingIDs:   []string{f.ID},
				Severity:     f.Severity,
				Title:        "Fix: " + f.Title,
			})
			proposedKeys[key] = true
			notifyChannels(trackerID, f)
		}
	}

	// Process review-blocker findings grouped by workspace (scope_value).
	for workspaceID, groupFindings := range groupReviewBlockers(reviewBlockers) {
		if len(out.Proposed) >= rateLimitMax {
			deferred += len(groupFindings)
			for _, f := range groupFindings {
				out.Skipped = append(out.Skipped, skippedFinding{
					FindingID: f.ID,
					Reason:    "rate limit reached — deferred to next cycle",
				})
			}
			continue
		}

		key := computeDedupKey(groupFindings[0])
		if proposedKeys[key] {
			for _, f := range groupFindings {
				out.Skipped = append(out.Skipped, skippedFinding{
					FindingID: f.ID,
					Reason:    "duplicate review-blocker group in this batch",
				})
			}
			continue
		}

		if recent, err := wasRecentlyProposed(proposalsDB, key); err != nil {
			return fmt.Errorf("checking proposal recency for workspace %s: %w", workspaceID, err)
		} else if recent {
			for _, f := range groupFindings {
				out.Skipped = append(out.Skipped, skippedFinding{
					FindingID: f.ID,
					Reason:    "proposed within the last 24 hours",
				})
			}
			continue
		}

		p, skipped, err := proposeReviewBlockerGroup(workspaceID, groupFindings, existing, *dryRun, *jsonOut)
		if err != nil {
			return fmt.Errorf("proposing review-blockers for workspace %s: %w", workspaceID, err)
		}
		out.Skipped = append(out.Skipped, skipped...)
		if p != nil && !*dryRun {
			if err := recordProposed(proposalsDB, key, groupFindings[0].Ability, groupFindings[0].Category, p.TrackerIssue); err != nil {
				return fmt.Errorf("recording proposal for workspace %s: %w", workspaceID, err)
			}
		}
		if p != nil {
			out.Proposed = append(out.Proposed, *p)
			proposedKeys[key] = true
		}
	}

	if *jsonOut {
		return encodeJSON(out)
	}

	if deferred > 0 {
		fmt.Printf("rate limit reached, %d findings deferred to next cycle\n", deferred)
	}
	if len(out.Proposed) > 0 {
		fmt.Printf("Proposed %d remediation(s):\n", len(out.Proposed))
		for _, p := range out.Proposed {
			fmt.Printf("  %s — %s\n    Mission: %s\n", p.TrackerIssue, p.Title, p.MissionFile)
		}
	}
	if len(out.Skipped) > 0 {
		fmt.Printf("Skipped %d finding(s):\n", len(out.Skipped))
		for _, s := range out.Skipped {
			fmt.Printf("  %s — %s\n", s.FindingID, s.Reason)
		}
	}
	return nil
}

// queryProposableFindings returns active findings eligible for remediation proposals.
// Applies severity thresholds: critical (any), high (>24h old OR 2+ in same category), medium (any).
func queryProposableFindings(ctx context.Context) ([]proposableFinding, error) {
	path := findingsDBPath()
	if _, err := os.Stat(path); os.IsNotExist(err) {
		return nil, nil
	}
	db, err := sql.Open("sqlite", "file:"+path+"?mode=ro")
	if err != nil {
		return nil, fmt.Errorf("open findings.db: %w", err)
	}
	defer db.Close()

	now := time.Now().UTC()
	nowStr := now.Format(time.RFC3339)

	rows, err := db.QueryContext(ctx, `
		SELECT id, ability, category, severity, title, description,
		       scope_kind, scope_value, evidence, source, found_at
		FROM findings
		WHERE superseded_by = ''
		  AND (expires_at IS NULL OR expires_at > ?)
		  AND scope_kind IN ('mission', 'phase', 'workspace', 'eval-config')
		  AND evidence != '[]'
		  AND severity IN ('critical', 'high', 'medium')
		ORDER BY
			CASE severity
				WHEN 'critical' THEN 1
				WHEN 'high'     THEN 2
				WHEN 'medium'   THEN 3
			END,
			found_at ASC`, nowStr)
	if err != nil {
		return nil, fmt.Errorf("query findings: %w", err)
	}
	defer rows.Close()

	// categoryCounts tracks finding counts keyed by "ability:category:severity"
	// so we can count only same-or-higher severity findings at threshold checks.
	categoryCounts := make(map[string]int)
	var all []proposableFinding

	for rows.Next() {
		var f proposableFinding
		var evidenceJSON, foundAtStr string
		if err := rows.Scan(&f.ID, &f.Ability, &f.Category, &f.Severity,
			&f.Title, &f.Description, &f.ScopeKind, &f.ScopeValue,
			&evidenceJSON, &f.Source, &foundAtStr); err != nil {
			return nil, fmt.Errorf("scanning finding: %w", err)
		}
		if t, err := time.Parse(time.RFC3339, foundAtStr); err == nil {
			f.FoundAt = t
		}
		if err := json.Unmarshal([]byte(evidenceJSON), &f.Evidence); err != nil {
			continue
		}
		// Evidence must contain at least one item with a source field
		hasSource := false
		for _, e := range f.Evidence {
			if e.Source != "" {
				hasSource = true
				break
			}
		}
		if !hasSource {
			continue
		}

		categoryCounts[f.Ability+":"+f.Category+":"+f.Severity]++
		all = append(all, f)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating findings: %w", err)
	}

	// categoryCountAtLeast returns the number of findings in the same ability:category
	// at the given severity or higher (critical > high > medium).
	categoryCountAtLeast := func(ability, category, severity string) int {
		key := ability + ":" + category + ":"
		switch severity {
		case "high":
			return categoryCounts[key+"critical"] + categoryCounts[key+"high"]
		case "medium":
			return categoryCounts[key+"critical"] + categoryCounts[key+"high"] + categoryCounts[key+"medium"]
		default: // critical — only itself
			return categoryCounts[key+"critical"]
		}
	}

	// Apply severity thresholds
	var proposable []proposableFinding
	for _, f := range all {
		switch f.Severity {
		case "critical":
			proposable = append(proposable, f)
		case "high":
			// review-blocker findings bypass the 24h threshold — propose immediately.
			if f.Category == "review-blocker" {
				proposable = append(proposable, f)
				continue
			}
			age := now.Sub(f.FoundAt)
			if age > 24*time.Hour || categoryCountAtLeast(f.Ability, f.Category, "high") >= 2 {
				proposable = append(proposable, f)
			}
		case "medium":
			proposable = append(proposable, f)
		}
	}

	return proposable, nil
}

func computeDedupKey(f proposableFinding) string {
	h := sha256.Sum256([]byte(f.Ability + ":" + f.Category + ":" + f.ScopeKind + ":" + f.ScopeValue))
	return fmt.Sprintf("%x", h[:8])
}

// dedupBlockingPolicy captures how long each tracker-issue status suppresses new
// proposals with the same dedup label. `done` and `closed` issues never block —
// they are treated as resolved or rejected. `in-progress` always blocks. `open`
// and `cancelled` block only while they are within their respective TTLs.
type dedupBlockingPolicy struct {
	staleOpenTTL      time.Duration // how long an `open` issue suppresses new proposals
	cancelledTTL      time.Duration // how long a `cancelled` issue suppresses new proposals
	qualityMultiplier float64       // ko quality score in [0,1]; 0 = no data (use base TTL unchanged)
}

// effectiveOpenTTL returns the TTL applied to `open` issues after quality scaling.
// A qualityMultiplier of 0 (unset / no quality data) returns the base staleOpenTTL
// unchanged, preserving backward-compatible behaviour. Otherwise the effective TTL
// is staleOpenTTL × (2 × qualityMultiplier), which maps:
//
//	0.5 (ko.DefaultQualityScore, neutral) → 1.0× (no change from baseline)
//	>0.5 (high quality, past proposals succeeded) → longer TTL, fewer re-proposals
//	<0.5 (low quality, past proposals failed/stalled) → shorter TTL, faster retries
func (p dedupBlockingPolicy) effectiveOpenTTL() time.Duration {
	if p.qualityMultiplier == 0 {
		return p.staleOpenTTL
	}
	return time.Duration(float64(p.staleOpenTTL) * 2 * p.qualityMultiplier)
}

func defaultDedupPolicy() dedupBlockingPolicy {
	return dedupBlockingPolicy{
		staleOpenTTL: 24 * time.Hour,
		cancelledTTL: 7 * 24 * time.Hour,
	}
}

// findBlockingIssue returns the displayID of the first tracker issue that should
// suppress a new proposal with the given dedup key, respecting the policy. It
// returns "" when no issue blocks. Callers must pass `now` so tests can pin time.
//
// Policy table:
//
//	in-progress → always blocks (actively being worked)
//	open        → blocks while updated_at is within policy.staleOpenTTL
//	cancelled   → blocks while updated_at is within policy.cancelledTTL
//	done        → never blocks (recurrence means the fix did not stick; re-propose)
//	closed      → never blocks (rejected or superseded)
//	other       → never blocks (unknown statuses do not silently suppress)
//
// If updated_at is missing or unparseable for a status with a TTL, the issue is
// treated as expired (not blocking). The whole point of this policy is that
// permanent suppression by stale tracker data is the bug being fixed — broken
// data should fail open (propose), not fail closed (suppress forever).
func findBlockingIssue(issues []trackerIssue, dedupKey string, policy dedupBlockingPolicy, now time.Time) string {
	label := "dedup:" + dedupKey
	for _, issue := range issues {
		if !issue.hasLabel(label) {
			continue
		}
		switch issue.Status {
		case "in-progress":
			return issue.displayID()
		case "open":
			if withinTTL(issue.UpdatedAt, policy.effectiveOpenTTL(), now) {
				return issue.displayID()
			}
		case "cancelled":
			if withinTTL(issue.UpdatedAt, policy.cancelledTTL, now) {
				return issue.displayID()
			}
		default:
			// done, closed, or any other status — never blocks.
		}
	}
	return ""
}

// withinTTL reports whether the given RFC3339 timestamp is newer than `now - ttl`.
// Unparseable or empty timestamps return false so broken data does not permanently
// suppress proposals.
func withinTTL(updatedAt string, ttl time.Duration, now time.Time) bool {
	if updatedAt == "" {
		return false
	}
	t, err := time.Parse(time.RFC3339, updatedAt)
	if err != nil {
		return false
	}
	return now.Sub(t) < ttl
}

func proposalsDBPath() string {
	dir, err := scan.Dir()
	if err != nil {
		home, _ := os.UserHomeDir()
		return filepath.Join(home, ".alluka", "nen", "proposals.db")
	}
	return filepath.Join(dir, "nen", "proposals.db")
}

func openProposalsDB() (*sql.DB, error) {
	path := proposalsDBPath()
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return nil, fmt.Errorf("create proposals db dir: %w", err)
	}
	db, err := sql.Open("sqlite", path+"?_journal_mode=WAL&_busy_timeout=5000")
	if err != nil {
		return nil, fmt.Errorf("open proposals.db: %w", err)
	}
	if _, err := db.Exec(`CREATE TABLE IF NOT EXISTS proposals (
		dedup_key        TEXT PRIMARY KEY,
		last_proposed_at DATETIME NOT NULL
	)`); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate proposals.db: %w", err)
	}
	// Additive enrichment columns so ko evaluate-proposals can join a
	// tracker issue back to its ability+category without re-parsing labels.
	// ALTER TABLE ADD COLUMN is idempotent here because we swallow the
	// "duplicate column name" error on re-runs.
	enrichments := []string{
		`ALTER TABLE proposals ADD COLUMN ability       TEXT NOT NULL DEFAULT ''`,
		`ALTER TABLE proposals ADD COLUMN category      TEXT NOT NULL DEFAULT ''`,
		`ALTER TABLE proposals ADD COLUMN tracker_issue TEXT NOT NULL DEFAULT ''`,
	}
	for _, stmt := range enrichments {
		if _, err := db.Exec(stmt); err != nil {
			if !strings.Contains(err.Error(), "duplicate column name") {
				db.Close()
				return nil, fmt.Errorf("migrate proposals.db (enrichment): %w", err)
			}
		}
	}
	if _, err := db.Exec(`CREATE TABLE IF NOT EXISTS dispatches (
		id            INTEGER PRIMARY KEY AUTOINCREMENT,
		issue_id      TEXT     NOT NULL,
		mission_file  TEXT     NOT NULL,
		workspace_id  TEXT     NOT NULL DEFAULT '',
		started_at    DATETIME NOT NULL,
		finished_at   DATETIME,
		outcome       TEXT     NOT NULL DEFAULT ''
	)`); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate proposals.db (dispatches): %w", err)
	}
	if _, err := db.Exec(`CREATE INDEX IF NOT EXISTS idx_dispatches_started_at ON dispatches(started_at)`); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate proposals.db (dispatch started index): %w", err)
	}
	if _, err := db.Exec(`CREATE INDEX IF NOT EXISTS idx_dispatches_active ON dispatches(finished_at)`); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate proposals.db (dispatch active index): %w", err)
	}
	// Ensure the ko.QualityStore schema is in place so any subsequent
	// propose run can read quality scores back out without a separate
	// migration step.
	if _, err := ko.NewQualityStore(db); err != nil {
		db.Close()
		return nil, fmt.Errorf("migrate proposals.db (quality): %w", err)
	}
	return db, nil
}

func wasRecentlyProposed(db *sql.DB, dedupKey string) (bool, error) {
	var lastAt string
	err := db.QueryRow(`SELECT last_proposed_at FROM proposals WHERE dedup_key = ?`, dedupKey).Scan(&lastAt)
	if err == sql.ErrNoRows {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("querying proposal for %s: %w", dedupKey, err)
	}
	t, err := time.Parse(time.RFC3339, lastAt)
	if err != nil {
		return false, fmt.Errorf("parsing last_proposed_at for %s: %w", dedupKey, err)
	}
	return time.Since(t) < 24*time.Hour, nil
}

// recordProposed upserts a proposal into proposals.db with the enrichment
// columns (ability, category, tracker_issue) ko evaluate-proposals needs.
// A call with empty ability/category/trackerIssue still succeeds — the dry-run
// path and legacy callers fall into that shape — so downstream ko evaluation
// just skips rows where the columns are blank.
func recordProposed(db *sql.DB, dedupKey, ability, category, trackerIssue string) error {
	_, err := db.Exec(`INSERT INTO proposals (dedup_key, last_proposed_at, ability, category, tracker_issue)
		VALUES (?, ?, ?, ?, ?)
		ON CONFLICT(dedup_key) DO UPDATE SET
			last_proposed_at = excluded.last_proposed_at,
			ability          = CASE WHEN excluded.ability       != '' THEN excluded.ability       ELSE proposals.ability       END,
			category         = CASE WHEN excluded.category      != '' THEN excluded.category      ELSE proposals.category      END,
			tracker_issue    = CASE WHEN excluded.tracker_issue != '' THEN excluded.tracker_issue ELSE proposals.tracker_issue END`,
		dedupKey, time.Now().UTC().Format(time.RFC3339), ability, category, trackerIssue)
	if err != nil {
		return fmt.Errorf("recording proposal for %s: %w", dedupKey, err)
	}
	return nil
}

func severityToPriority(severity string) string {
	switch severity {
	case "critical":
		return "P0"
	case "high":
		return "P1"
	default:
		return "P2"
	}
}

func missionFilePath(f proposableFinding) string {
	date := time.Now().Format("2006-01-02")
	slug := slugify(f.Title)
	if len(slug) > 60 {
		slug = slug[:60]
	}
	return filepath.Join(remediationMissionDir(), fmt.Sprintf("%s-%s.md", date, slug))
}

var slugRe = regexp.MustCompile(`[^a-z0-9]+`)

func slugify(s string) string {
	s = strings.ToLower(s)
	s = slugRe.ReplaceAllString(s, "-")
	return strings.Trim(s, "-")
}

// --- Mission template ---

type missionData struct {
	TrackerIssue         string
	FindingIDs           []string
	Severity             string
	Ability              string
	Category             string
	GeneratedAt          string
	Title                string
	Context              string
	EvidenceLines        []string
	AffectedFiles        []affectedFile
	InvestigateObjective string
	FixObjective         string
	ScannerName          string
	SuccessCriteria      []string
}

type affectedFile struct {
	Path   string
	Reason string
}

var missionTemplate = template.Must(template.New("mission").Parse(`---
source: shu-propose
tracker_issue: {{.TrackerIssue}}
finding_ids:{{range .FindingIDs}}
  - {{.}}{{end}}
severity: {{.Severity}}
ability: {{.Ability}}
category: {{.Category}}
generated_at: "{{.GeneratedAt}}"
domain: dev
target: repo:~/nanika
---

# Fix: {{.Title}}

## Context

{{.Context}}

## Evidence
{{range .EvidenceLines}}
- {{.}}{{end}}

## Affected Files
{{range .AffectedFiles}}
- {{.Path}} — {{.Reason}}{{end}}

PHASE: investigate | OBJECTIVE: {{.InvestigateObjective}} | PERSONA: senior-backend-engineer

PHASE: fix | OBJECTIVE: {{.FixObjective}} | PERSONA: senior-backend-engineer | DEPENDS: investigate

PHASE: verify | OBJECTIVE: Run Ko evals for the affected component. Re-run the {{.ScannerName}} scanner to confirm the finding no longer reproduces. Report pass/fail | PERSONA: qa-engineer | DEPENDS: fix

## Success Criteria
{{range .SuccessCriteria}}
- [ ] {{.}}{{end}}
`))

func generateMission(f proposableFinding, trackerID string) (string, error) {
	var evidenceLines []string
	var affected []affectedFile
	seenPaths := make(map[string]bool)

	for _, e := range f.Evidence {
		line := e.Raw
		if e.Source != "" {
			line = fmt.Sprintf("%s (source: %s)", e.Raw, e.Source)
		}
		evidenceLines = append(evidenceLines, line)

		if e.Source != "" && strings.Contains(e.Source, "/") && !seenPaths[e.Source] {
			affected = append(affected, affectedFile{Path: e.Source, Reason: "referenced in finding evidence"})
			seenPaths[e.Source] = true
		}
	}
	if len(evidenceLines) == 0 {
		evidenceLines = []string{"(see finding description)"}
	}
	if len(affected) == 0 {
		affected = []affectedFile{{Path: "(identify from investigation)", Reason: "root cause location TBD"}}
	}

	data := missionData{
		TrackerIssue:         trackerID,
		FindingIDs:           []string{f.ID},
		Severity:             f.Severity,
		Ability:              f.Ability,
		Category:             f.Category,
		GeneratedAt:          time.Now().UTC().Format(time.RFC3339),
		Title:                f.Title,
		Context:              f.Description,
		EvidenceLines:        evidenceLines,
		AffectedFiles:        affected,
		InvestigateObjective: fmt.Sprintf("Investigate: %s. Examine the affected code and evidence to identify the root cause. Document findings", f.Title),
		FixObjective:         fmt.Sprintf("Fix: %s. Implement the fix and add test coverage for the failure scenario", f.Title),
		ScannerName:          f.Source,
		SuccessCriteria: []string{
			"Ko eval suite passes for the affected component",
			fmt.Sprintf("%s scanner re-run produces no new findings for %s category", f.Source, f.Category),
			"Test case covers the identified failure mode",
		},
	}

	var buf strings.Builder
	if err := missionTemplate.Execute(&buf, data); err != nil {
		return "", fmt.Errorf("executing template: %w", err)
	}
	return buf.String(), nil
}

var trkIDPattern = regexp.MustCompile(`TRK-\d+|trk-[a-z0-9]+`)

func createTrackerIssue(title, priority, labels, description string) (string, error) {
	out, err := exec.Command("tracker", "create", title,
		"--priority", priority,
		"--labels", labels,
		"--description", description,
	).Output()
	if err != nil {
		if exitErr, ok := err.(*exec.ExitError); ok {
			return "", fmt.Errorf("tracker create failed: %s", string(exitErr.Stderr))
		}
		return "", fmt.Errorf("tracker create: %w", err)
	}
	// Parse "created TRK-42" from first line
	lines := strings.SplitN(string(out), "\n", 2)
	if match := trkIDPattern.FindString(lines[0]); match != "" {
		return match, nil
	}
	parts := strings.Fields(lines[0])
	if len(parts) >= 2 {
		return parts[1], nil
	}
	return "", fmt.Errorf("could not parse tracker issue ID from: %q", lines[0])
}

func formatEvidenceSummary(evidence []evidenceItem) string {
	var lines []string
	for _, e := range evidence {
		line := "- " + e.Raw
		if e.Source != "" {
			line += " (source: " + e.Source + ")"
		}
		lines = append(lines, line)
	}
	if len(lines) == 0 {
		return "(no evidence)"
	}
	return strings.Join(lines, "\n")
}

// notifyChannels sends best-effort notifications to configured channels.
// Beta: no-op. Post-beta will add proper notification routing via configured channel IDs.
func notifyChannels(_ string, _ proposableFinding) {}

func schedulerJobExists(name string) (bool, error) {
	out, err := exec.Command("scheduler", "query", "items", "--json").Output()
	if err != nil {
		return false, fmt.Errorf("scheduler query items: %w", err)
	}
	var resp struct {
		Items []struct {
			Name string `json:"name"`
		} `json:"items"`
	}
	if err := json.Unmarshal(out, &resp); err != nil {
		return false, fmt.Errorf("parsing scheduler items: %w", err)
	}
	for _, item := range resp.Items {
		if item.Name == name {
			return true, nil
		}
	}
	return false, nil
}

func runProposeInit() error {
	if _, err := exec.LookPath("scheduler"); err != nil {
		return fmt.Errorf("scheduler plugin required (not found in PATH)")
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("getting home dir: %w", err)
	}

	scriptDir := filepath.Join(home, ".alluka", "scripts")
	if err := os.MkdirAll(scriptDir, 0o755); err != nil {
		return fmt.Errorf("creating scripts dir: %w", err)
	}
	scriptPath := filepath.Join(scriptDir, "dispatch-approved.sh")
	if _, err := os.Stat(scriptPath); os.IsNotExist(err) {
		if err := os.WriteFile(scriptPath, []byte(dispatchScript), 0o755); err != nil {
			return fmt.Errorf("writing dispatch-approved.sh: %w", err)
		}
		fmt.Printf("Created %s\n", scriptPath)
	}

	exists, err := schedulerJobExists("propose-remediations")
	if err != nil {
		return fmt.Errorf("checking propose-remediations job: %w", err)
	}
	if exists {
		fmt.Println("Scheduler job already exists: propose-remediations (skipping)")
	} else {
		if out, err := exec.Command("scheduler", "jobs", "add",
			"--name", "propose-remediations",
			"--cron", "0 */4 * * *",
			"--command", "shu propose --json",
		).CombinedOutput(); err != nil {
			return fmt.Errorf("adding propose job: %s: %w", strings.TrimSpace(string(out)), err)
		}
		fmt.Println("Added scheduler job: propose-remediations (every 4h)")
	}

	exists, err = schedulerJobExists("dispatch-approved")
	if err != nil {
		return fmt.Errorf("checking dispatch-approved job: %w", err)
	}
	if exists {
		fmt.Println("Scheduler job already exists: dispatch-approved (skipping)")
	} else {
		if out, err := exec.Command("scheduler", "jobs", "add",
			"--name", "dispatch-approved",
			"--cron", "*/15 * * * *",
			"--command", "shu dispatch --max-concurrent 1 --max-per-hour 6",
		).CombinedOutput(); err != nil {
			return fmt.Errorf("adding dispatch job: %s: %w", strings.TrimSpace(string(out)), err)
		}
		fmt.Println("Added scheduler job: dispatch-approved (every 15m)")
	}

	exists, err = schedulerJobExists("evaluate-weekly")
	if err != nil {
		return fmt.Errorf("checking evaluate-weekly job: %w", err)
	}
	if exists {
		fmt.Println("Scheduler job already exists: evaluate-weekly (skipping)")
	} else {
		if out, err := exec.Command("scheduler", "jobs", "add",
			"--name", "evaluate-weekly",
			"--cron", "0 10 * * 1",
			"--command", "shu evaluate --json",
		).CombinedOutput(); err != nil {
			return fmt.Errorf("adding evaluate-weekly job: %s: %w", strings.TrimSpace(string(out)), err)
		}
		fmt.Println("Added scheduler job: evaluate-weekly (Mondays 10am)")
	}

	exists, err = schedulerJobExists("close-sweep")
	if err != nil {
		return fmt.Errorf("checking close-sweep job: %w", err)
	}
	if exists {
		fmt.Println("Scheduler job already exists: close-sweep (skipping)")
	} else {
		if out, err := exec.Command("scheduler", "jobs", "add",
			"--name", "close-sweep",
			"--cron", "*/15 * * * *",
			"--command", "shu close --sweep --json",
		).CombinedOutput(); err != nil {
			return fmt.Errorf("adding close-sweep job: %s: %w", strings.TrimSpace(string(out)), err)
		}
		fmt.Println("Added scheduler job: close-sweep (every 15m)")
	}

	remDir := filepath.Join(home, ".alluka", "missions", "remediation")
	if err := os.MkdirAll(remDir, 0o755); err != nil {
		return fmt.Errorf("creating remediation missions dir: %w", err)
	}

	fmt.Println("Self-improvement loop initialized.")
	return nil
}

const dispatchScript = `#!/usr/bin/env bash
# dispatch-approved.sh — Run the next approved (in-progress+auto) shu mission.
set -euo pipefail

LOCK_FILE="${HOME}/.alluka/dispatch-approved.pid"
LOG_PREFIX="dispatch-approved"

log() { echo "${LOG_PREFIX}: $*"; }
die() { echo "${LOG_PREFIX}: $*" >&2; exit 1; }

acquire_lock() {
    if [[ -f "$LOCK_FILE" ]]; then
        local old_pid
        old_pid=$(cat "$LOCK_FILE" 2>/dev/null || true)
        if [[ -n "$old_pid" ]] && kill -0 "$old_pid" 2>/dev/null; then
            log "already running (pid ${old_pid}), exiting"
            exit 0
        fi
        rm -f "$LOCK_FILE"
    fi
    echo $$ > "$LOCK_FILE"
}

release_lock() { rm -f "$LOCK_FILE"; }
trap release_lock EXIT
acquire_lock

command -v jq &>/dev/null     || die "jq is required"
command -v tracker &>/dev/null || die "tracker is required"
command -v orchestrator &>/dev/null || die "orchestrator is required"
command -v shu &>/dev/null    || die "shu is required"

ITEMS_JSON=$(tracker query items --json 2>&1) || die "tracker query items failed: ${ITEMS_JSON}"

ISSUE=$(printf '%s\n' "$ITEMS_JSON" | jq -c '
    [.items[]
    | select(.status == "in-progress")
    | select((.labels // "") | split(",") | map(ltrimstr(" ") | rtrimstr(" ")) | any(. == "auto"))
    ] | first // empty
' 2>/dev/null)

if [[ -z "$ISSUE" || "$ISSUE" == "null" ]]; then
    log "no in-progress auto issues found"
    exit 0
fi

ISSUE_ID=$(printf '%s\n' "$ISSUE" | jq -r '.id')
DESCRIPTION=$(printf '%s\n' "$ISSUE" | jq -r '.description // ""')

MISSION_FILE=$(printf '%s\n' "$DESCRIPTION" | grep -m1 '^Mission: ' | sed 's/^Mission: //' | xargs 2>/dev/null || true)

if [[ -z "$MISSION_FILE" || ! -f "$MISSION_FILE" ]]; then
    log "mission file not found for ${ISSUE_ID} — reverting to open"
    tracker comment "$ISSUE_ID" "dispatch-approved: mission file not found" 2>/dev/null || true
    tracker update "$ISSUE_ID" --status open 2>/dev/null || true
    exit 1
fi

FINDING_IDS=$(printf '%s\n' "$DESCRIPTION" | grep '^- [^ ]*: ' | sed 's/^- \([^:]*\):.*/\1/' | tr '\n' ',' | sed 's/,$//')

log "dispatching ${ISSUE_ID} — mission: ${MISSION_FILE}"

set +e
ORCH_OUTPUT=$(orchestrator run "$MISSION_FILE" 2>&1)
ORCH_EXIT=$?
set -e
printf '%s\n' "$ORCH_OUTPUT"

WORKSPACE_ID=$(printf '%s\n' "$ORCH_OUTPUT" | grep -oE '[0-9]{8}-[a-f0-9]{8}' | head -1 || true)

if [[ $ORCH_EXIT -eq 0 ]]; then
    log "mission succeeded for ${ISSUE_ID}"
    if [[ -n "$FINDING_IDS" ]]; then
        CLOSE_ARGS=(--tracker-issue "$ISSUE_ID" --finding-ids "$FINDING_IDS")
        [[ -n "$WORKSPACE_ID" ]] && CLOSE_ARGS+=(--workspace "$WORKSPACE_ID")
        shu close "${CLOSE_ARGS[@]}" || {
            log "warning: shu close failed — closing tracker issue directly"
            tracker update "$ISSUE_ID" --status done 2>/dev/null || true
        }
    else
        tracker update "$ISSUE_ID" --status done 2>/dev/null || true
    fi
else
    log "mission failed for ${ISSUE_ID} (exit ${ORCH_EXIT}) — reverting to open"
    tracker comment "$ISSUE_ID" "dispatch-approved: mission failed (exit ${ORCH_EXIT})" 2>/dev/null || true
    tracker update "$ISSUE_ID" --status open 2>/dev/null || true
    exit 1
fi
`
