package main

// Review-blocker mission generation and escalation logic.
//
// Review-blocker findings (category='review-blocker', scope_kind='workspace') are
// grouped by workspace and handled as a batch: one fix mission per workspace, or a
// P0 escalation tracker issue if the workspace has already iterated >= 3 times.

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"text/template"
	"time"
)

// groupReviewBlockers groups review-blocker findings by scope_value (workspace ID).
func groupReviewBlockers(findings []proposableFinding) map[string][]proposableFinding {
	groups := make(map[string][]proposableFinding)
	for _, f := range findings {
		groups[f.ScopeValue] = append(groups[f.ScopeValue], f)
	}
	return groups
}

// readWorkspaceIteration reads the iteration field from the workspace mission's YAML frontmatter.
// Returns 0 if the file does not exist or the field is absent.
func readWorkspaceIteration(workspaceID string) (int, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return 0, fmt.Errorf("getting home dir: %w", err)
	}
	missionPath := filepath.Join(home, ".alluka", "workspaces", workspaceID, "mission.md")
	f, err := os.Open(missionPath)
	if os.IsNotExist(err) {
		return 0, nil
	}
	if err != nil {
		return 0, fmt.Errorf("opening mission file %s: %w", missionPath, err)
	}
	defer f.Close()

	inFrontmatter := false
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := scanner.Text()
		if line == "---" {
			if !inFrontmatter {
				inFrontmatter = true
				continue
			}
			break
		}
		if !inFrontmatter {
			continue
		}
		if strings.HasPrefix(line, "iteration:") {
			val := strings.TrimSpace(strings.TrimPrefix(line, "iteration:"))
			n, err := strconv.Atoi(val)
			if err != nil {
				return 0, fmt.Errorf("parsing iteration %q in %s: %w", val, missionPath, err)
			}
			return n, nil
		}
	}
	return 0, scanner.Err()
}

// reviewBlockerMissionFilePath returns the mission file path for a workspace's review-blocker fix.
func reviewBlockerMissionFilePath(workspaceID string) string {
	home, _ := os.UserHomeDir()
	date := time.Now().Format("2006-01-02")
	return filepath.Join(home, ".alluka", "missions", "remediation",
		fmt.Sprintf("%s-review-blocker-%s.md", date, workspaceID))
}

type reviewBlockerMissionData struct {
	TrackerIssue   string
	FindingIDs     []string
	WorkspaceID    string
	Iteration      int
	GeneratedAt    string
	Title          string
	BlockerDetails []string
	FixObjective   string
}

var reviewBlockerMissionTemplate = template.Must(template.New("review-blocker").Parse(`---
source: shu-propose
tracker_issue: {{.TrackerIssue}}
finding_ids:{{range .FindingIDs}}
  - {{.}}{{end}}
severity: high
category: review-blocker
origin_workspace: {{.WorkspaceID}}
iteration: {{.Iteration}}
generated_at: "{{.GeneratedAt}}"
domain: dev
target: repo:~/nanika
---

# Fix Review Blockers: {{.Title}}

## Blockers
{{range .BlockerDetails}}
- {{.}}{{end}}

PHASE: fix | OBJECTIVE: {{.FixObjective}} | PERSONA: senior-backend-engineer

PHASE: test | OBJECTIVE: Write or update tests covering the fixed blocker scenarios. Confirm go test ./... -race passes | PERSONA: qa-engineer | DEPENDS: fix

PHASE: review | OBJECTIVE: Review the fix and tests for correctness, completeness, and coverage of all blocker file:line locations | PERSONA: staff-code-reviewer | DEPENDS: test
`))

// generateReviewBlockerMission renders the mission content for a workspace's review-blocker group.
func generateReviewBlockerMission(workspaceID string, findings []proposableFinding, iteration int, trackerID string) (string, error) {
	var findingIDs, blockerDetails []string
	seen := make(map[string]bool)
	for _, f := range findings {
		findingIDs = append(findingIDs, f.ID)
		for _, e := range f.Evidence {
			detail := e.Raw
			if e.Source != "" {
				detail = fmt.Sprintf("%s: %s", e.Source, e.Raw)
			}
			if !seen[detail] {
				seen[detail] = true
				blockerDetails = append(blockerDetails, detail)
			}
		}
	}
	if len(blockerDetails) == 0 {
		for _, f := range findings {
			if !seen[f.Title] {
				seen[f.Title] = true
				blockerDetails = append(blockerDetails, f.Title)
			}
		}
	}

	title := workspaceID
	if len(findings) > 0 && findings[0].Title != "" {
		title = findings[0].Title
		if len(findings) > 1 {
			title = fmt.Sprintf("%s (and %d more)", title, len(findings)-1)
		}
	}

	fixObjective := fmt.Sprintf("Fix all review-blocker findings in workspace %s. Address each blocker: %s",
		workspaceID, strings.Join(blockerDetails, "; "))
	if len(fixObjective) > 300 {
		fixObjective = fixObjective[:300]
	}

	data := reviewBlockerMissionData{
		TrackerIssue:   trackerID,
		FindingIDs:     findingIDs,
		WorkspaceID:    workspaceID,
		Iteration:      iteration,
		GeneratedAt:    time.Now().UTC().Format(time.RFC3339),
		Title:          title,
		BlockerDetails: blockerDetails,
		FixObjective:   fixObjective,
	}

	var buf strings.Builder
	if err := reviewBlockerMissionTemplate.Execute(&buf, data); err != nil {
		return "", fmt.Errorf("executing review-blocker template: %w", err)
	}
	return buf.String(), nil
}

// buildEscalationDescription returns a tracker issue description for an escalated workspace.
func buildEscalationDescription(workspaceID string, iteration int, findings []proposableFinding) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Workspace %s has failed review-blocker remediation %d time(s).\n\nBlockers:\n", workspaceID, iteration)
	for _, f := range findings {
		fmt.Fprintf(&b, "- %s: %s\n", f.ID, f.Title)
	}
	return b.String()
}

// proposeReviewBlockerGroup handles one workspace's review-blocker group.
// Escalates to P0 if iteration >= 3; otherwise writes a fix mission.
func proposeReviewBlockerGroup(workspaceID string, findings []proposableFinding, existing []trackerIssue, dryRun bool, jsonOut bool) (*proposal, []skippedFinding, error) {
	if len(findings) == 0 {
		return nil, nil, nil
	}

	key := computeDedupKey(findings[0])
	if issueID := findExistingIssue(existing, key); issueID != "" {
		var skipped []skippedFinding
		for _, f := range findings {
			skipped = append(skipped, skippedFinding{
				FindingID: f.ID,
				Reason:    fmt.Sprintf("existing tracker issue %s covers workspace %s review-blockers", issueID, workspaceID),
			})
		}
		return nil, skipped, nil
	}

	iteration, err := readWorkspaceIteration(workspaceID)
	if err != nil {
		return nil, nil, fmt.Errorf("reading iteration for workspace %s: %w", workspaceID, err)
	}

	var findingIDs []string
	for _, f := range findings {
		findingIDs = append(findingIDs, f.ID)
	}

	if iteration >= 3 {
		escTitle := fmt.Sprintf("Escalation: review-blocker loop in workspace %s (iter %d)", workspaceID, iteration)
		labels := fmt.Sprintf("auto,nen,review-blocker,origin:%s,iter:%d,dedup:%s", workspaceID, iteration, key)
		if dryRun {
			if !jsonOut {
				fmt.Printf("--- dry-run escalation: workspace %s (iter %d) ---\n%s\n---\n\n",
					workspaceID, iteration, buildEscalationDescription(workspaceID, iteration, findings))
			}
			return &proposal{TrackerIssue: "(dry-run escalation)", MissionFile: "", FindingIDs: findingIDs, Severity: "critical", Title: escTitle}, nil, nil
		}
		trackerID, err := createTrackerIssue(escTitle, "P0", labels, buildEscalationDescription(workspaceID, iteration, findings))
		if err != nil {
			return nil, nil, fmt.Errorf("creating escalation issue for workspace %s: %w", workspaceID, err)
		}
		return &proposal{TrackerIssue: trackerID, MissionFile: "", FindingIDs: findingIDs, Severity: "critical", Title: escTitle}, nil, nil
	}

	missionPath := reviewBlockerMissionFilePath(workspaceID)
	title := fmt.Sprintf("Fix review-blockers in workspace %s", workspaceID)
	if findings[0].Title != "" {
		title = fmt.Sprintf("Fix review-blockers: %s (workspace %s)", findings[0].Title, workspaceID)
	}
	labels := fmt.Sprintf("auto,nen,review-blocker,origin:%s,iter:%d,dedup:%s", workspaceID, iteration, key)

	if dryRun {
		content, _ := generateReviewBlockerMission(workspaceID, findings, iteration, "(dry-run)")
		if !jsonOut {
			fmt.Printf("--- dry-run review-blocker: workspace %s ---\n%s\n---\n\n", workspaceID, content)
		}
		return &proposal{TrackerIssue: "(dry-run)", MissionFile: missionPath, FindingIDs: findingIDs, Severity: "high", Title: title}, nil, nil
	}

	desc := fmt.Sprintf("Mission: %s\n\n%s", missionPath, buildEscalationDescription(workspaceID, iteration, findings))
	trackerID, err := createTrackerIssue(title, "P1", labels, desc)
	if err != nil {
		return nil, nil, fmt.Errorf("creating tracker issue for workspace %s: %w", workspaceID, err)
	}

	content, err := generateReviewBlockerMission(workspaceID, findings, iteration, trackerID)
	if err != nil {
		return nil, nil, fmt.Errorf("generating review-blocker mission for workspace %s: %w", workspaceID, err)
	}
	if err := os.MkdirAll(filepath.Dir(missionPath), 0o755); err != nil {
		return nil, nil, fmt.Errorf("creating mission dir: %w", err)
	}
	if err := os.WriteFile(missionPath, []byte(content), 0o644); err != nil {
		return nil, nil, fmt.Errorf("writing review-blocker mission for workspace %s: %w", workspaceID, err)
	}

	return &proposal{TrackerIssue: trackerID, MissionFile: missionPath, FindingIDs: findingIDs, Severity: "high", Title: title}, nil, nil
}
