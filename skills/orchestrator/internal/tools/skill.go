package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// SkillViewTool reads a skill's SKILL.md file.
type SkillViewTool struct{ nanikaDir string }

// NewSkillViewTool returns a SkillViewTool that reads skills from nanikaDir.
func NewSkillViewTool(nanikaDir string) Tool { return &SkillViewTool{nanikaDir: nanikaDir} }

func (t *SkillViewTool) Name() string        { return "skill_view" }
func (t *SkillViewTool) Risk() RiskTier      { return RiskLow }
func (t *SkillViewTool) Description() string {
	return "View the full SKILL.md reference for a named skill. " +
		"Use skill_search first to discover available skill names."
}

func (t *SkillViewTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"name": String("Skill name (e.g. 'orchestrator', 'decomposer')"),
	}, []string{"name"})
}

func (t *SkillViewTool) Execute(_ context.Context, args map[string]any) (ToolResult, error) {
	name, err := requireString(args, "name")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}

	// Try .claude/skills/<name>/SKILL.md first, then plugins/<name>/skills/SKILL.md.
	candidates := []string{
		filepath.Join(t.nanikaDir, ".claude", "skills", name, "SKILL.md"),
		filepath.Join(t.nanikaDir, "plugins", name, "skills", "SKILL.md"),
	}

	for _, path := range candidates {
		data, err := os.ReadFile(path)
		if err == nil {
			return ToolResult{Content: string(data)}, nil
		}
	}

	return ToolResult{
		IsError: true,
		Content: fmt.Sprintf("skill_view: no SKILL.md found for %q (tried %s)", name, strings.Join(candidates, ", ")),
	}, nil
}

// SkillSearchTool searches the AGENTS-MD skill index for a keyword.
type SkillSearchTool struct{ nanikaDir string }

// NewSkillSearchTool returns a SkillSearchTool.
func NewSkillSearchTool(nanikaDir string) Tool { return &SkillSearchTool{nanikaDir: nanikaDir} }

func (t *SkillSearchTool) Name() string        { return "skill_search" }
func (t *SkillSearchTool) Risk() RiskTier      { return RiskLow }
func (t *SkillSearchTool) Description() string {
	return "Search available skills by keyword. Returns matching skill names and one-line descriptions."
}

func (t *SkillSearchTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"query": String("Keyword to search for in skill names and descriptions"),
	}, []string{"query"})
}

func (t *SkillSearchTool) Execute(_ context.Context, args map[string]any) (ToolResult, error) {
	query, err := requireString(args, "query")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	query = strings.ToLower(query)

	index := loadSkillIndex(t.nanikaDir)
	if index == "" {
		return ToolResult{IsError: true, Content: "skill_search: skill index not found (CLAUDE.md missing or has no AGENTS-MD block)"}, nil
	}

	var results []string
	for _, line := range strings.Split(index, "\n") {
		if strings.Contains(strings.ToLower(line), query) {
			if entry := formatSkillLine(line); entry != "" {
				results = append(results, entry)
			}
		}
	}

	if len(results) == 0 {
		return ToolResult{Content: fmt.Sprintf("no skills matching %q", query)}, nil
	}
	return ToolResult{Content: strings.Join(results, "\n")}, nil
}

// loadSkillIndex reads the AGENTS-MD routing block from <nanikaDir>/CLAUDE.md.
func loadSkillIndex(nanikaDir string) string {
	if nanikaDir == "" {
		return ""
	}
	data, err := os.ReadFile(filepath.Join(nanikaDir, "CLAUDE.md"))
	if err != nil {
		return ""
	}
	return extractAgentsMD(string(data))
}

// extractAgentsMD pulls content between the NANIKA-AGENTS-MD markers.
func extractAgentsMD(content string) string {
	const startMarker = "<!-- NANIKA-AGENTS-MD-START -->"
	const endMarker = "<!-- NANIKA-AGENTS-MD-END -->"

	startIdx := strings.Index(content, startMarker)
	if startIdx < 0 {
		return ""
	}
	startIdx += len(startMarker)
	endIdx := strings.Index(content[startIdx:], endMarker)
	if endIdx < 0 {
		return ""
	}
	return strings.TrimSpace(content[startIdx : startIdx+endIdx])
}

// formatSkillLine converts a routing index line to "name: description".
// Line format: |name — description:{path}|`cmd`|...
func formatSkillLine(line string) string {
	line = strings.TrimSpace(line)
	if !strings.HasPrefix(line, "|") {
		return ""
	}
	line = line[1:]
	dashIdx := strings.Index(line, " — ")
	if dashIdx <= 0 {
		return ""
	}
	name := strings.TrimSpace(line[:dashIdx])
	rest := line[dashIdx+len(" — "):]
	desc := rest
	if braceIdx := strings.Index(rest, ":{"); braceIdx > 0 {
		desc = rest[:braceIdx]
	} else if pipeIdx := strings.Index(rest, "|"); pipeIdx > 0 {
		desc = rest[:pipeIdx]
	}
	return name + ": " + strings.TrimSpace(desc)
}
