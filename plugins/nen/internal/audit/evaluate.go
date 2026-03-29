package audit

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"github.com/joeyhipolito/nen/internal/scan"
)

const defaultEvalModel = "claude-opus-4-6"

// EvaluateOptions configures the audit evaluation.
type EvaluateOptions struct {
	WorkspacePath string // full path to workspace dir
	Model         string // override model (default: opus)
	Verbose       bool
}

// EvaluateMission loads a completed mission and produces an audit report.
func EvaluateMission(ctx context.Context, opts EvaluateOptions) (*AuditReport, error) {
	// 1. Load checkpoint
	cp, err := loadCheckpoint(opts.WorkspacePath)
	if err != nil {
		return nil, fmt.Errorf("loading checkpoint: %w", err)
	}
	if cp.Plan == nil {
		return nil, fmt.Errorf("checkpoint has no plan")
	}

	if opts.Verbose {
		fmt.Fprintf(os.Stderr, "[audit] loaded checkpoint: %s (%d phases, status=%s)\n",
			cp.WorkspaceID, len(cp.Plan.Phases), cp.Status)
	}

	// 2. Load worker outputs
	outputs, err := loadWorkerOutputs(opts.WorkspacePath)
	if err != nil {
		return nil, fmt.Errorf("loading worker outputs: %w", err)
	}

	if opts.Verbose {
		fmt.Fprintf(os.Stderr, "[audit] loaded %d worker outputs\n", len(outputs))
	}

	// 3. Load persona catalog
	personaCatalog := loadPersonaCatalog(opts.Verbose)

	// 4. Load skill index
	skillSummary := loadSkillSummary()

	// 5. Load decomposition rules and track convergence
	convergence := checkDecomposerConvergence()

	// 6. Build prompt and call LLM
	rules := convergence.rules
	if rules == "" {
		rules = "(decomposition rules not available)"
	}

	prompt := buildEvaluationPrompt(cp.Plan.Task, cp.Plan, outputs, personaCatalog, skillSummary, rules)

	model := opts.Model
	if model == "" {
		model = defaultEvalModel
	}

	if opts.Verbose {
		fmt.Fprintf(os.Stderr, "[audit] calling %s for evaluation (%d chars prompt)\n", model, len(prompt))
	}

	raw, err := queryLLM(ctx, model, prompt)
	if err != nil {
		return nil, fmt.Errorf("LLM evaluation: %w", err)
	}

	if opts.Verbose {
		fmt.Fprintf(os.Stderr, "[audit] received %d chars response\n", len(raw))
	}

	// 7. Parse report
	report, err := parseReport(raw)
	if err != nil {
		report = &AuditReport{
			WorkspaceID: cp.WorkspaceID,
			Task:        truncateTask(cp.Plan.Task),
			Domain:      cp.Domain,
			Status:      cp.Status,
			AuditedAt:   time.Now(),
			Evaluation: MissionEvaluation{
				Summary: fmt.Sprintf("(parse failed: %v)\n\nRaw LLM response:\n%s", err, raw),
			},
		}
	}

	// 8. Fill in metadata the LLM doesn't know
	report.WorkspaceID = cp.WorkspaceID
	report.Task = truncateTask(cp.Plan.Task)
	report.Domain = cp.Domain
	report.Status = cp.Status
	report.AuditedAt = time.Now()
	report.LinearIssueID = cp.LinearIssueID
	report.MissionPath = cp.MissionPath

	// 9. Compute overall score
	s := &report.Scorecard
	s.Overall = (s.DecompositionQuality + s.PersonaFit + s.SkillUtilization + s.OutputQuality + s.RuleCompliance) / 5

	// 10. Attach decomposer convergence status
	report.DecomposerConvergence = convergence.DecomposerConvergence

	// 11. Extract changes from outputs
	report.Changes = extractChanges(cp.Plan.Phases, outputs)

	return report, nil
}

// loadCheckpoint reads checkpoint.json from the workspace directory.
func loadCheckpoint(wsPath string) (*Checkpoint, error) {
	data, err := os.ReadFile(filepath.Join(wsPath, "checkpoint.json"))
	if err != nil {
		return nil, fmt.Errorf("reading checkpoint.json: %w", err)
	}

	var cp Checkpoint
	if err := json.Unmarshal(data, &cp); err != nil {
		return nil, fmt.Errorf("parsing checkpoint.json: %w", err)
	}

	// Load sidecar files (best-effort)
	if cp.LinearIssueID == "" {
		if b, err := os.ReadFile(filepath.Join(wsPath, "linear_issue_id")); err == nil {
			cp.LinearIssueID = strings.TrimSpace(string(b))
		}
	}
	if cp.MissionPath == "" {
		if b, err := os.ReadFile(filepath.Join(wsPath, "mission_path")); err == nil {
			cp.MissionPath = strings.TrimSpace(string(b))
		}
	}

	return &cp, nil
}

// loadWorkerOutputs reads output.md from each worker directory.
func loadWorkerOutputs(wsPath string) (map[string]string, error) {
	workersDir := filepath.Join(wsPath, "workers")
	entries, err := os.ReadDir(workersDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("reading workers dir: %w", err)
	}

	outputs := make(map[string]string)
	for _, e := range entries {
		if !e.IsDir() {
			continue
		}
		outPath := filepath.Join(workersDir, e.Name(), "output.md")
		data, err := os.ReadFile(outPath)
		if err != nil {
			continue
		}
		outputs[e.Name()] = string(data)
	}
	return outputs, nil
}

// loadPersonaCatalog reads persona markdown files from ~/nanika/personas/
// and formats them for the audit prompt.
func loadPersonaCatalog(verbose bool) string {
	personaDir := os.Getenv("ORCHESTRATOR_PERSONAS_DIR")
	if personaDir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "(personas directory not found)\n"
		}
		personaDir = filepath.Join(home, "nanika", "personas")
	}

	entries, err := os.ReadDir(personaDir)
	if err != nil {
		return "(personas directory not found)\n"
	}

	var b strings.Builder
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		data, err := os.ReadFile(filepath.Join(personaDir, e.Name()))
		if err != nil {
			continue
		}
		name := strings.TrimSuffix(e.Name(), ".md")
		content := string(data)

		// Extract title (first heading)
		title := name
		for _, line := range strings.Split(content, "\n") {
			if strings.HasPrefix(line, "# ") {
				title = strings.TrimPrefix(line, "# ")
				break
			}
		}

		b.WriteString("### ")
		b.WriteString(name)
		b.WriteString("\n")
		b.WriteString(title)
		b.WriteString("\n")

		// Extract WhenToUse and WhenNotUse sections
		whenToUse := extractSection(content, "## When to Use")
		if whenToUse != "" {
			b.WriteString("**When to Use:**\n")
			b.WriteString(whenToUse)
		}
		whenNotUse := extractSection(content, "## When NOT to Use")
		if whenNotUse != "" {
			b.WriteString("**When NOT to Use:**\n")
			b.WriteString(whenNotUse)
		}
		b.WriteString("\n")
	}

	if verbose {
		fmt.Fprintf(os.Stderr, "[audit] loaded personas from %s\n", personaDir)
	}

	return b.String()
}

// extractSection extracts the content of a markdown section up to the next ## heading.
func extractSection(content, heading string) string {
	idx := strings.Index(content, heading+"\n")
	if idx < 0 {
		return ""
	}
	rest := content[idx+len(heading)+1:]
	end := strings.Index(rest, "\n## ")
	if end >= 0 {
		rest = rest[:end]
	}
	return strings.TrimSpace(rest) + "\n"
}

// loadSkillSummary reads the AGENTS-MD skill routing block from ~/nanika/CLAUDE.md
// and produces a concise summary.
func loadSkillSummary() string {
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	claudeMDPath := filepath.Join(home, "nanika", "CLAUDE.md")
	data, err := os.ReadFile(claudeMDPath)
	if err != nil {
		return ""
	}

	content := string(data)
	startMarker := "<!-- NANIKA-AGENTS-MD-START -->"
	endMarker := "<!-- NANIKA-AGENTS-MD-END -->"
	startIdx := strings.Index(content, startMarker)
	endIdx := strings.Index(content, endMarker)
	if startIdx < 0 || endIdx < 0 || endIdx <= startIdx {
		return ""
	}

	agentsMD := content[startIdx+len(startMarker) : endIdx]

	// Format as concise summary
	var b strings.Builder
	for _, line := range strings.Split(agentsMD, "\n") {
		line = strings.TrimSpace(line)
		if !strings.HasPrefix(line, "|") {
			continue
		}
		// Parse: |name — description:{path}|cmd1|cmd2|
		parts := strings.SplitN(line, "—", 2)
		if len(parts) < 2 {
			continue
		}
		name := strings.TrimSpace(strings.TrimPrefix(parts[0], "|"))
		desc := parts[1]
		if idx := strings.Index(desc, ":{"); idx >= 0 {
			desc = desc[:idx]
		}
		desc = strings.TrimSpace(desc)
		if name != "" && desc != "" {
			b.WriteString(fmt.Sprintf("- **%s**: %s\n", name, desc))
		}
	}
	return b.String()
}

// decomposerCheck bundles the SKILL.md convergence check with the extracted rules.
type decomposerCheck struct {
	DecomposerConvergence
	rules string
}

// checkDecomposerConvergence determines which rules the decomposer used.
func checkDecomposerConvergence() decomposerCheck {
	result := decomposerCheck{}

	skillPath := os.Getenv("NANIKA_DECOMPOSER_SKILL")
	if skillPath == "" {
		skillPath = os.Getenv("VIA_DECOMPOSER_SKILL")
	}

	home, _ := os.UserHomeDir()
	base, _ := scan.Dir()
	paths := []string{
		skillPath,
		filepath.Join(home, ".alluka", ".claude", "skills", "decomposer", "SKILL.md"),
		filepath.Join(home, "skills", "decomposer", ".claude", "skills", "decomposer", "SKILL.md"),
		filepath.Join(base, ".claude", "skills", "decomposer", "SKILL.md"),
	}

	for _, p := range paths {
		if p == "" {
			continue
		}
		data, err := os.ReadFile(p)
		if err != nil {
			continue
		}

		hash := fmt.Sprintf("%x", sha256.Sum256(data))
		result.SKILLMDHash = hash
		result.SKILLMDPath = p
		result.PromptSource = "skill_md"

		rules := extractDecomposerRules(string(data))
		if rules != "" {
			result.RulesExtracted = true
			result.rules = rules
		} else {
			result.PromptSource = "hardcoded_fallback"
			result.rules = hardcodedFallbackRules()
		}
		return result
	}

	result.PromptSource = "hardcoded_fallback"
	result.rules = hardcodedFallbackRules()
	return result
}

// extractDecomposerRules replicates the decompose package's extraction logic.
func extractDecomposerRules(content string) string {
	lines := strings.Split(content, "\n")
	startIdx, endIdx := -1, -1
	for i, line := range lines {
		if strings.HasPrefix(line, "## Output Format") {
			startIdx = i
		}
		if startIdx != -1 && strings.HasPrefix(line, "## Worked Examples") {
			endIdx = i
			break
		}
	}
	if startIdx == -1 || endIdx == -1 {
		return ""
	}
	return strings.TrimSpace(strings.Join(lines[startIdx+1:endIdx], "\n"))
}

func hardcodedFallbackRules() string {
	return `## Core Rules

1. Each phase must have exactly ONE persona.
2. Break by USER VALUE, not by technical layer.
3. Aim for 3-8 phases. Prefer fewer phases with rich objectives over many thin ones. Maximum 12.
4. Split independent sub-tasks into separate phases so they run in parallel.
5. Pick the MOST SPECIFIC persona from the catalog. Match by WhenToUse triggers.
6. Assign SKILLS only when a phase genuinely needs a specific tool's commands.
7. Respect persona handoff chains.`
}

// extractChanges scans worker outputs for file operations.
var (
	fileWritePattern  = regexp.MustCompile(`(?i)(?:created?|wrote?|writing)\s+(?:file\s+)?[` + "`" + `"']?([^\s` + "`" + `"']+\.\w{1,6})[` + "`" + `"']?`)
	fileEditPattern   = regexp.MustCompile(`(?i)(?:edited?|modified?|updated?)\s+(?:file\s+)?[` + "`" + `"']?([^\s` + "`" + `"']+\.\w{1,6})[` + "`" + `"']?`)
	commandRunPattern = regexp.MustCompile("(?i)(?:ran|running|executed?)\\s+[`\"']?([^`\"'\\n]+)[`\"']?")
)

func extractChanges(phases []*Phase, outputs map[string]string) []ChangeRecord {
	var changes []ChangeRecord

	for _, p := range phases {
		var output string
		for dirName, text := range outputs {
			if strings.Contains(dirName, p.Name) {
				output = text
				break
			}
		}
		if output == "" {
			continue
		}

		for _, m := range fileWritePattern.FindAllStringSubmatch(output, 20) {
			changes = append(changes, ChangeRecord{
				PhaseID: p.ID, PhaseName: p.Name, Type: "file_created", Target: m[1],
			})
		}
		for _, m := range fileEditPattern.FindAllStringSubmatch(output, 20) {
			changes = append(changes, ChangeRecord{
				PhaseID: p.ID, PhaseName: p.Name, Type: "file_modified", Target: m[1],
			})
		}
		for _, m := range commandRunPattern.FindAllStringSubmatch(output, 10) {
			cmd := m[1]
			if len(cmd) > 100 {
				cmd = cmd[:100]
			}
			changes = append(changes, ChangeRecord{
				PhaseID: p.ID, PhaseName: p.Name, Type: "command_run", Target: cmd,
			})
		}
	}

	seen := make(map[string]bool)
	var deduped []ChangeRecord
	for _, c := range changes {
		key := c.PhaseID + ":" + c.Type + ":" + c.Target
		if !seen[key] {
			seen[key] = true
			deduped = append(deduped, c)
		}
	}
	return deduped
}

// jsonFencePattern matches ```json ... ``` blocks.
var jsonFencePattern = regexp.MustCompile("(?s)```json\\s*\n(.*?)\n\\s*```")

// parseReport extracts the structured report from the LLM's response.
func parseReport(raw string) (*AuditReport, error) {
	matches := jsonFencePattern.FindStringSubmatch(raw)
	var jsonStr string
	if len(matches) > 1 {
		jsonStr = matches[1]
	} else {
		jsonStr = strings.TrimSpace(raw)
	}

	var lr llmReport
	if err := json.Unmarshal([]byte(jsonStr), &lr); err != nil {
		return nil, fmt.Errorf("parsing LLM JSON: %w", err)
	}

	return &AuditReport{
		Scorecard:   lr.Scorecard,
		Evaluation:  lr.Evaluation,
		Phases:      lr.Phases,
		Convergence: lr.Convergence,
	}, nil
}

func truncateTask(task string) string {
	if strings.HasPrefix(task, "---\n") {
		if idx := strings.Index(task[4:], "---\n"); idx > 0 {
			task = strings.TrimSpace(task[idx+8:])
		}
	}
	if idx := strings.IndexByte(task, '\n'); idx > 0 && idx < 200 {
		return strings.TrimSpace(task[:idx])
	}
	if len(task) > 200 {
		return task[:197] + "..."
	}
	return task
}

// ── LLM via Claude CLI ──────────────────────────────────────────────────────

func queryLLM(ctx context.Context, model, prompt string) (string, error) {
	cmd := exec.CommandContext(ctx, "claude",
		"--model", model,
		"--print",
		"--output-format", "text",
		"--max-turns", "1",
		"--dangerously-skip-permissions",
		"-p", prompt,
	)
	out, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("claude CLI: %w", err)
	}
	return strings.TrimSpace(string(out)), nil
}

// ── Workspace resolution ─────────────────────────────────────────────────────

// ResolveWorkspace resolves a workspace path from an ID or --last index.
func ResolveWorkspace(idOrPath string, last int) (string, error) {
	if idOrPath != "" {
		if strings.Contains(idOrPath, "/") {
			if _, err := os.Stat(filepath.Join(idOrPath, "checkpoint.json")); err != nil {
				return "", fmt.Errorf("workspace %s has no checkpoint", idOrPath)
			}
			return idOrPath, nil
		}
		base, err := scan.Dir()
		if err != nil {
			return "", fmt.Errorf("cannot determine config directory: %w", err)
		}
		wsPath := filepath.Join(base, "workspaces", idOrPath)
		if _, err := os.Stat(filepath.Join(wsPath, "checkpoint.json")); err != nil {
			return "", fmt.Errorf("workspace %s has no checkpoint", idOrPath)
		}
		return wsPath, nil
	}

	base, err := scan.Dir()
	if err != nil {
		return "", fmt.Errorf("cannot determine config directory: %w", err)
	}
	wsDir := filepath.Join(base, "workspaces")
	entries, err := os.ReadDir(wsDir)
	if err != nil {
		return "", fmt.Errorf("listing workspaces: %w", err)
	}

	// Filter to v2 workspaces (contain mission.md), newest first
	var paths []string
	for i := len(entries) - 1; i >= 0; i-- {
		e := entries[i]
		if !e.IsDir() {
			continue
		}
		p := filepath.Join(wsDir, e.Name())
		if _, err := os.Stat(filepath.Join(p, "mission.md")); err == nil {
			paths = append(paths, p)
		}
	}

	if len(paths) == 0 {
		return "", fmt.Errorf("no workspaces found")
	}

	idx := last - 1
	if idx < 0 {
		idx = 0
	}
	if idx >= len(paths) {
		return "", fmt.Errorf("only %d workspaces exist, --last %d is out of range", len(paths), last)
	}

	wsPath := paths[idx]
	if _, err := os.Stat(filepath.Join(wsPath, "checkpoint.json")); err != nil {
		return "", fmt.Errorf("workspace %s has no checkpoint", filepath.Base(wsPath))
	}

	return wsPath, nil
}
