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
	"strings"
	"text/template"
	"time"
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

func runPropose(args []string) error {
	fs := flag.NewFlagSet("propose", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	dryRun := fs.Bool("dry-run", false, "Show what would be proposed without creating issues or files")
	jsonOut := fs.Bool("json", false, "Output proposals as JSON")
	initFlag := fs.Bool("init", false, "Set up scheduler jobs for propose and dispatch")
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

	out := proposeOutput{Proposed: []proposal{}, Skipped: []skippedFinding{}}
	proposedKeys := make(map[string]bool)

	for _, f := range findings {
		key := computeDedupKey(f)

		if proposedKeys[key] {
			out.Skipped = append(out.Skipped, skippedFinding{
				FindingID: f.ID,
				Reason:    "duplicate of finding proposed in this batch",
			})
			continue
		}

		if issueID := findExistingIssue(existing, key); issueID != "" {
			out.Skipped = append(out.Skipped, skippedFinding{
				FindingID: f.ID,
				Reason:    fmt.Sprintf("existing tracker issue %s covers same category", issueID),
			})
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

		// Create tracker issue first to get the ID for mission frontmatter
		priority := severityToPriority(f.Severity)
		labels := fmt.Sprintf("auto,nen,%s,dedup:%s", f.Ability, key)
		desc := fmt.Sprintf("Mission: %s\n\nFindings:\n- %s: %s (%s)\n\nEvidence:\n%s",
			missionPath, f.ID, f.Title, f.Severity, formatEvidenceSummary(f.Evidence))

		trackerID, err := createTrackerIssue("Fix: "+f.Title, priority, labels, desc)
		if err != nil {
			return fmt.Errorf("creating tracker issue for %s: %w", f.ID, err)
		}

		// Generate and write mission file with tracker ID
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

	if *jsonOut {
		return encodeJSON(out)
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
		  AND scope_kind IN ('mission', 'phase')
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

		categoryCounts[f.Ability+":"+f.Category]++
		all = append(all, f)
	}
	if err := rows.Err(); err != nil {
		return nil, fmt.Errorf("iterating findings: %w", err)
	}

	// Apply severity thresholds
	var proposable []proposableFinding
	for _, f := range all {
		switch f.Severity {
		case "critical":
			proposable = append(proposable, f)
		case "high":
			age := now.Sub(f.FoundAt)
			if age > 24*time.Hour || categoryCounts[f.Ability+":"+f.Category] >= 2 {
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
	return fmt.Sprintf("%x", h[:4])
}

func findExistingIssue(issues []trackerIssue, dedupKey string) string {
	label := "dedup:" + dedupKey
	for _, issue := range issues {
		if (issue.Status == "open" || issue.Status == "in-progress") && issue.hasLabel(label) {
			return issue.displayID()
		}
	}
	return ""
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
	home, _ := os.UserHomeDir()
	date := time.Now().Format("2006-01-02")
	slug := slugify(f.Title)
	if len(slug) > 60 {
		slug = slug[:60]
	}
	return filepath.Join(home, ".alluka", "missions", "remediation", fmt.Sprintf("%s-%s.md", date, slug))
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

func runProposeInit() error {
	if _, err := exec.LookPath("scheduler"); err != nil {
		return fmt.Errorf("scheduler plugin required (not found in PATH)")
	}

	home, err := os.UserHomeDir()
	if err != nil {
		return fmt.Errorf("getting home dir: %w", err)
	}

	scriptPath := filepath.Join(home, ".alluka", "scripts", "dispatch-approved.sh")
	if _, err := os.Stat(scriptPath); os.IsNotExist(err) {
		return fmt.Errorf("dispatch-approved.sh not found at %s — install it first", scriptPath)
	}

	if out, err := exec.Command("scheduler", "jobs", "add",
		"--name", "propose-remediations",
		"--cron", "0 */4 * * *",
		"--command", "shu propose --json",
	).CombinedOutput(); err != nil {
		return fmt.Errorf("adding propose job: %s: %w", strings.TrimSpace(string(out)), err)
	}
	fmt.Println("Added scheduler job: propose-remediations (every 4h)")

	if out, err := exec.Command("scheduler", "jobs", "add",
		"--name", "dispatch-approved",
		"--cron", "*/15 * * * *",
		"--command", fmt.Sprintf("bash %s", scriptPath),
	).CombinedOutput(); err != nil {
		return fmt.Errorf("adding dispatch job: %s: %w", strings.TrimSpace(string(out)), err)
	}
	fmt.Println("Added scheduler job: dispatch-approved (every 15m)")

	remDir := filepath.Join(home, ".alluka", "missions", "remediation")
	if err := os.MkdirAll(remDir, 0o755); err != nil {
		return fmt.Errorf("creating remediation missions dir: %w", err)
	}

	fmt.Println("Self-improvement loop initialized.")
	return nil
}
