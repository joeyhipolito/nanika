package audit

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
)

// ApplyOptions configures the apply engine.
type ApplyOptions struct {
	ReportID string // workspace ID to look up the report
	DryRun   bool
	Model    string
	Verbose  bool
	Confirm  func(summary string) bool // nil = auto-confirm (for testing)
}

// ApplyResult captures what happened during apply.
type ApplyResult struct {
	ReportID    string       `json:"report_id"`
	AppliedAt   time.Time    `json:"applied_at"`
	DryRun      bool         `json:"dry_run"`
	FileChanges []FileChange `json:"file_changes"`
	Skipped     []string     `json:"skipped,omitempty"`
	Error       string       `json:"error,omitempty"`
}

// FileChange records a single file modification with before/after snapshots.
type FileChange struct {
	Path      string `json:"path"`
	Type      string `json:"type"`
	Action    string `json:"action"`
	Before    string `json:"before,omitempty"`
	After     string `json:"after"`
	DiffLines int    `json:"diff_lines"`
}

// FileDiff is what the LLM produces: a proposed change to a single file.
type FileDiff struct {
	Path       string `json:"path"`
	Type       string `json:"type"`
	Action     string `json:"action"`
	Rationale  string `json:"rationale"`
	NewContent string `json:"new_content"`
}

// ApplyPlan is the LLM's complete set of proposed changes.
type ApplyPlan struct {
	Summary string     `json:"summary"`
	Diffs   []FileDiff `json:"diffs"`
}

// ApplyRecommendations reads an audit report's recommendations, generates diffs,
// prompts for confirmation, applies changes, and records them.
func ApplyRecommendations(ctx context.Context, opts ApplyOptions) (*ApplyResult, error) {
	// 1. Find the report
	report, err := findReport(opts.ReportID)
	if err != nil {
		return nil, fmt.Errorf("finding report: %w", err)
	}

	if len(report.Evaluation.Recommendations) == 0 {
		return nil, fmt.Errorf("report %s has no recommendations to apply", opts.ReportID)
	}

	if opts.Verbose {
		fmt.Fprintf(os.Stderr, "[apply] found report for %s with %d recommendations\n",
			report.WorkspaceID, len(report.Evaluation.Recommendations))
	}

	// 2. Load current file contents for context
	fileContents, err := loadTargetFiles(opts.Verbose)
	if err != nil {
		return nil, fmt.Errorf("loading target files: %w", err)
	}

	// 3. Call LLM to generate diffs
	model := opts.Model
	if model == "" {
		model = defaultEvalModel
	}

	prompt := buildApplyPrompt(report, fileContents)

	if opts.Verbose {
		fmt.Fprintf(os.Stderr, "[apply] calling %s to generate diffs (%d chars prompt)\n", model, len(prompt))
	}

	raw, err := queryLLM(ctx, model, prompt)
	if err != nil {
		return nil, fmt.Errorf("LLM diff generation: %w", err)
	}

	if opts.Verbose {
		fmt.Fprintf(os.Stderr, "[apply] received %d chars response\n", len(raw))
	}

	// 4. Parse the apply plan
	plan, err := parseApplyPlan(raw)
	if err != nil {
		return nil, fmt.Errorf("parsing apply plan: %w", err)
	}

	if len(plan.Diffs) == 0 {
		return &ApplyResult{
			ReportID:  opts.ReportID,
			AppliedAt: time.Now(),
			DryRun:    opts.DryRun,
			Skipped:   []string{"LLM produced no file changes"},
		}, nil
	}

	// 5. Show summary and confirm
	summary := formatApplySummary(plan, opts.DryRun)
	fmt.Print(summary)

	if !opts.DryRun {
		if opts.Confirm != nil {
			if !opts.Confirm(summary) {
				return &ApplyResult{
					ReportID:  opts.ReportID,
					AppliedAt: time.Now(),
					Skipped:   []string{"user declined"},
				}, nil
			}
		}
	}

	// 6. Build changes with before snapshots
	var changes []FileChange
	var skipped []string
	for _, diff := range plan.Diffs {
		resolved := resolveFilePath(diff.Path, diff.Type)
		if resolved == "" {
			skipped = append(skipped, fmt.Sprintf("cannot resolve path for %s (%s)", diff.Path, diff.Type))
			continue
		}

		var before string
		existing, err := os.ReadFile(resolved)
		if err == nil {
			before = string(existing)
		}

		action := "modified"
		if before == "" {
			action = "created"
		}

		changes = append(changes, FileChange{
			Path:      resolved,
			Type:      diff.Type,
			Action:    action,
			Before:    before,
			After:     diff.NewContent,
			DiffLines: countDiffLines(before, diff.NewContent),
		})
	}

	result := &ApplyResult{
		ReportID:    opts.ReportID,
		AppliedAt:   time.Now(),
		DryRun:      opts.DryRun,
		FileChanges: changes,
		Skipped:     skipped,
	}

	if opts.DryRun {
		return result, nil
	}

	// 7. Apply the changes
	for i, change := range changes {
		if err := os.MkdirAll(filepath.Dir(change.Path), 0700); err != nil {
			result.Error = fmt.Sprintf("creating dir for %s: %v", change.Path, err)
			return result, fmt.Errorf("creating directory: %w", err)
		}

		if err := os.WriteFile(change.Path, []byte(change.After), 0600); err != nil {
			result.Error = fmt.Sprintf("writing %s: %v", change.Path, err)
			return result, fmt.Errorf("writing %s: %w", change.Path, err)
		}

		if opts.Verbose {
			fmt.Fprintf(os.Stderr, "[apply] wrote %s (%d lines changed)\n",
				changes[i].Path, changes[i].DiffLines)
		}
	}

	// 8. Record the change
	if err := saveChangeRecord(result, report); err != nil {
		fmt.Fprintf(os.Stderr, "[apply] warning: failed to save change record: %v\n", err)
	}

	return result, nil
}

// findReport looks up an audit report by workspace ID.
func findReport(wsID string) (*AuditReport, error) {
	reports, err := LoadReports()
	if err != nil {
		return nil, err
	}
	if len(reports) == 0 {
		return nil, fmt.Errorf("no audit reports found")
	}

	for i := len(reports) - 1; i >= 0; i-- {
		if reports[i].WorkspaceID == wsID {
			return &reports[i], nil
		}
	}
	return nil, fmt.Errorf("no report found for workspace %s", wsID)
}

// targetFile represents a file that can be modified by the apply engine.
type targetFile struct {
	Path    string
	Type    string
	Content string
}

// loadTargetFiles loads persona files, CLAUDE.md, and decomposer SKILL.md.
func loadTargetFiles(verbose bool) ([]targetFile, error) {
	var files []targetFile

	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("cannot determine home directory: %w", err)
	}

	// Persona files
	personaDir := filepath.Join(home, "nanika", "personas")
	if dir := os.Getenv("ORCHESTRATOR_PERSONAS_DIR"); dir != "" {
		personaDir = dir
	}

	entries, err := os.ReadDir(personaDir)
	if err != nil {
		if !os.IsNotExist(err) {
			return nil, fmt.Errorf("reading personas dir: %w", err)
		}
	} else {
		for _, e := range entries {
			if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
				continue
			}
			p := filepath.Join(personaDir, e.Name())
			data, err := os.ReadFile(p)
			if err != nil {
				continue
			}
			files = append(files, targetFile{
				Path:    p,
				Type:    "persona",
				Content: string(data),
			})
		}
	}

	// Decomposer SKILL.md
	base, _ := scan.Dir()
	skillPaths := []string{
		decomposerSkillEnv(),
		filepath.Join(home, "skills", "decomposer", ".claude", "skills", "decomposer", "SKILL.md"),
		filepath.Join(base, ".claude", "skills", "decomposer", "SKILL.md"),
	}
	for _, p := range skillPaths {
		if p == "" {
			continue
		}
		data, err := os.ReadFile(p)
		if err != nil {
			continue
		}
		files = append(files, targetFile{
			Path:    p,
			Type:    "skill_md",
			Content: string(data),
		})
		break
	}

	if verbose {
		fmt.Fprintf(os.Stderr, "[apply] loaded %d target files\n", len(files))
	}

	return files, nil
}

// resolveFilePath maps an LLM-suggested path to an absolute path on disk.
func resolveFilePath(path, fileType string) string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}

	switch fileType {
	case "persona":
		name := filepath.Base(path)
		if !strings.HasSuffix(name, ".md") {
			name += ".md"
		}
		personaDir := filepath.Join(home, "nanika", "personas")
		if dir := os.Getenv("ORCHESTRATOR_PERSONAS_DIR"); dir != "" {
			personaDir = dir
		}
		return filepath.Join(personaDir, name)

	case "skill_md":
		base, _ := scan.Dir()
		candidates := []string{
			decomposerSkillEnv(),
			filepath.Join(home, "skills", "decomposer", ".claude", "skills", "decomposer", "SKILL.md"),
			filepath.Join(base, ".claude", "skills", "decomposer", "SKILL.md"),
		}
		for _, c := range candidates {
			if c == "" {
				continue
			}
			if _, err := os.Stat(c); err == nil {
				return c
			}
		}
		for _, c := range candidates {
			if c != "" {
				return c
			}
		}
		return ""

	default:
		if filepath.IsAbs(path) {
			return path
		}
		return ""
	}
}

func countDiffLines(before, after string) int {
	if before == "" {
		return strings.Count(after, "\n") + 1
	}
	beforeLines := strings.Split(before, "\n")
	afterLines := strings.Split(after, "\n")

	changed := 0
	maxLen := len(beforeLines)
	if len(afterLines) > maxLen {
		maxLen = len(afterLines)
	}
	for i := 0; i < maxLen; i++ {
		var bLine, aLine string
		if i < len(beforeLines) {
			bLine = beforeLines[i]
		}
		if i < len(afterLines) {
			aLine = afterLines[i]
		}
		if bLine != aLine {
			changed++
		}
	}
	return changed
}

var applyPlanFencePattern = regexp.MustCompile("(?s)```json\\s*\n(.*?)\n\\s*```")

func parseApplyPlan(raw string) (*ApplyPlan, error) {
	matches := applyPlanFencePattern.FindStringSubmatch(raw)
	var jsonStr string
	if len(matches) > 1 {
		jsonStr = matches[1]
	} else {
		jsonStr = strings.TrimSpace(raw)
	}

	var plan ApplyPlan
	if err := json.Unmarshal([]byte(jsonStr), &plan); err != nil {
		return nil, fmt.Errorf("parsing apply plan JSON: %w", err)
	}
	return &plan, nil
}

func formatApplySummary(plan *ApplyPlan, dryRun bool) string {
	var b strings.Builder

	if dryRun {
		b.WriteString("DRY RUN — no changes will be written\n")
	}

	b.WriteString("\nProposed Changes\n")
	b.WriteString(strings.Repeat("=", 50))
	b.WriteString("\n\n")

	if plan.Summary != "" {
		b.WriteString(plan.Summary)
		b.WriteString("\n\n")
	}

	for i, diff := range plan.Diffs {
		b.WriteString(fmt.Sprintf("%d. [%s] %s (%s)\n", i+1, diff.Type, diff.Path, diff.Action))
		b.WriteString(fmt.Sprintf("   %s\n\n", diff.Rationale))
	}

	b.WriteString(fmt.Sprintf("Total: %d file(s)\n\n", len(plan.Diffs)))
	return b.String()
}

// changeManifest is what gets saved to ~/.alluka/audits/changes/<report-id>.json.
type changeManifest struct {
	ReportID    string       `json:"report_id"`
	AppliedAt   time.Time    `json:"applied_at"`
	ReportHash  string       `json:"report_hash"`
	FileChanges []FileChange `json:"file_changes"`
}

func saveChangeRecord(result *ApplyResult, report *AuditReport) error {
	base, err := scan.Dir()
	if err != nil {
		return err
	}
	dir := filepath.Join(base, "audits", "changes")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("creating changes dir: %w", err)
	}

	reportJSON, _ := json.Marshal(report)
	reportHash := fmt.Sprintf("%x", sha256.Sum256(reportJSON))

	manifest := changeManifest{
		ReportID:    result.ReportID,
		AppliedAt:   result.AppliedAt,
		ReportHash:  reportHash,
		FileChanges: result.FileChanges,
	}

	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling change manifest: %w", err)
	}

	filename := fmt.Sprintf("%s-%s.json",
		result.ReportID,
		result.AppliedAt.Format("20060102-150405"))
	path := filepath.Join(dir, filename)

	if err := os.WriteFile(path, data, 0600); err != nil {
		return fmt.Errorf("writing change manifest: %w", err)
	}

	return nil
}

// FormatApplyResult renders the apply result for terminal output.
func FormatApplyResult(result *ApplyResult) string {
	var b strings.Builder

	if result.DryRun {
		b.WriteString("DRY RUN — no files were modified\n\n")
	} else {
		b.WriteString("Changes Applied\n")
		b.WriteString(strings.Repeat("=", 40))
		b.WriteString("\n\n")
	}

	b.WriteString(fmt.Sprintf("Report: %s\n", result.ReportID))
	b.WriteString(fmt.Sprintf("Time:   %s\n\n", result.AppliedAt.Format("2006-01-02 15:04:05")))

	if len(result.FileChanges) > 0 {
		b.WriteString(fmt.Sprintf("Files (%d):\n", len(result.FileChanges)))
		for _, c := range result.FileChanges {
			b.WriteString(fmt.Sprintf("  %s %-10s %s (%d lines)\n", c.Action, "["+c.Type+"]", c.Path, c.DiffLines))
		}
		b.WriteString("\n")
	}

	if len(result.Skipped) > 0 {
		b.WriteString(fmt.Sprintf("Skipped (%d):\n", len(result.Skipped)))
		for _, s := range result.Skipped {
			b.WriteString(fmt.Sprintf("  - %s\n", s))
		}
		b.WriteString("\n")
	}

	if result.Error != "" {
		b.WriteString(fmt.Sprintf("Error: %s\n", result.Error))
	}

	if !result.DryRun && len(result.FileChanges) > 0 {
		for _, c := range result.FileChanges {
			if c.Type == "skill_md" {
				b.WriteString("Note: SKILL.md was modified. Changes will propagate to the decomposer\n")
				b.WriteString("on next mission run (it reads SKILL.md at runtime).\n")
				break
			}
		}
	}

	return b.String()
}

// FormatApplyResultJSON renders the apply result as JSON.
func FormatApplyResultJSON(result *ApplyResult) (string, error) {
	data, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return "", fmt.Errorf("marshaling apply result: %w", err)
	}
	return string(data), nil
}

func decomposerSkillEnv() string {
	if v := os.Getenv("NANIKA_DECOMPOSER_SKILL"); v != "" {
		return v
	}
	return os.Getenv("VIA_DECOMPOSER_SKILL")
}
