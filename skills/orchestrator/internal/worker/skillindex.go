package worker

import (
	"os"
	"path/filepath"
	"strings"
)

// overrideNanikaDir allows configuring the nanika directory at runtime.
var overrideNanikaDir string

// SetNanikaDir overrides the default nanika directory (~/nanika).
func SetNanikaDir(dir string) {
	overrideNanikaDir = dir
}

// LoadSkillIndex extracts the AGENTS-MD skill routing block from ~/nanika/CLAUDE.md.
// This gives every worker visibility into all available tools.
func LoadSkillIndex() string {
	claudemd := filepath.Join(nanikaDir(), "CLAUDE.md")
	data, err := os.ReadFile(claudemd)
	if err != nil {
		return ""
	}

	return extractAgentsMD(string(data))
}

// extractAgentsMD pulls the content between NANIKA-AGENTS-MD-START and NANIKA-AGENTS-MD-END markers.
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

// ParseSkillNames extracts skill names from the AGENTS-MD routing index.
// Each line is like: |name — description:{path}|`cmd1`|`cmd2`|
// Returns the name portion before " — ".
func ParseSkillNames(index string) []string {
	var names []string
	for _, line := range strings.Split(index, "\n") {
		line = strings.TrimSpace(line)
		if !strings.HasPrefix(line, "|") {
			continue
		}
		// Strip leading pipe
		line = line[1:]
		// Name is everything before " — "
		if idx := strings.Index(line, " — "); idx > 0 {
			names = append(names, strings.TrimSpace(line[:idx]))
		}
	}
	return names
}

// FormatSkillsForDecomposer produces a concise summary of skills with descriptions
// for the decomposer prompt. Each line is: "- name: description"
// Extracts from AGENTS-MD lines like: |name — description:{path}|`cmd1`|...
func FormatSkillsForDecomposer(index string) string {
	var b strings.Builder
	for _, line := range strings.Split(index, "\n") {
		line = strings.TrimSpace(line)
		if !strings.HasPrefix(line, "|") {
			continue
		}
		// Strip leading pipe
		line = line[1:]

		// Extract name (before " — ") and description (between " — " and ":{")
		dashIdx := strings.Index(line, " — ")
		if dashIdx <= 0 {
			continue
		}
		name := strings.TrimSpace(line[:dashIdx])
		rest := line[dashIdx+len(" — "):]

		// Description ends at ":{" (path reference)
		desc := rest
		if braceIdx := strings.Index(rest, ":{"); braceIdx > 0 {
			desc = rest[:braceIdx]
		} else if pipeIdx := strings.Index(rest, "|"); pipeIdx > 0 {
			desc = rest[:pipeIdx]
		}
		desc = strings.TrimSpace(desc)

		b.WriteString("- **")
		b.WriteString(name)
		b.WriteString("**: ")
		b.WriteString(desc)
		b.WriteString("\n")
	}
	return b.String()
}

func nanikaDir() string {
	if overrideNanikaDir != "" {
		return overrideNanikaDir
	}
	if dir := os.Getenv("ORCHESTRATOR_NANIKA_DIR"); dir != "" {
		return dir
	}
	if dir := os.Getenv("ORCHESTRATOR_VIA_DIR"); dir != "" { // legacy
		return dir
	}
	// Default: the directory the orchestrator was called from
	cwd, err := os.Getwd()
	if err != nil {
		return "."
	}
	return cwd
}
