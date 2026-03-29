package worker

import (
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

// LoadSkill reads a SKILL.md file and extracts the command reference section.
func LoadSkill(name string) (core.Skill, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return core.Skill{}, err
	}

	root := nanikaDir()

	// Search paths in priority order:
	//   1. .claude/skills:    ~/nanika/.claude/skills/{name}/SKILL.md  (primary — symlinks to CLI repos)
	//   2. .agents/skills:    ~/nanika/.agents/skills/{name}/SKILL.md  (knowledge skills)
	//   3. CLI skills:        ~/skills/{name}/.claude/skills/{name}/SKILL.md  (direct fallback)
	paths := []string{
		filepath.Join(root, ".claude", "skills", name, "SKILL.md"),
		filepath.Join(root, ".agents", "skills", name, "SKILL.md"),
		filepath.Join(home, "skills", name, ".claude", "skills", name, "SKILL.md"),
	}

	for _, path := range paths {
		data, err := os.ReadFile(path)
		if err != nil {
			continue
		}

		return core.Skill{
			Name:             name,
			CommandReference: extractCommands(string(data)),
		}, nil
	}

	return core.Skill{}, os.ErrNotExist
}

// LoadSkills loads multiple skills by name, skipping any that can't be found.
func LoadSkills(names []string) []core.Skill {
	var skills []core.Skill
	for _, name := range names {
		skill, err := LoadSkill(name)
		if err != nil {
			continue
		}
		skills = append(skills, skill)
	}
	return skills
}

// extractCommands extracts the Commands section from a SKILL.md file.
// Returns everything after "## Commands" until the next top-level heading or EOF.
func extractCommands(content string) string {
	lines := strings.Split(content, "\n")
	var result []string
	inCommands := false
	inFrontmatter := false

	for _, line := range lines {
		// Skip YAML frontmatter
		if line == "---" {
			inFrontmatter = !inFrontmatter
			continue
		}
		if inFrontmatter {
			continue
		}

		// Start capturing at "## Commands" or "## Setup"
		if strings.HasPrefix(line, "## Commands") || strings.HasPrefix(line, "## Setup") {
			inCommands = true
			continue
		}

		// Stop at next top-level section (but not subsections)
		if inCommands && strings.HasPrefix(line, "## ") &&
			!strings.HasPrefix(line, "### ") {
			// Check if it's a section we want to include
			if !strings.HasPrefix(line, "## Commands") &&
				!strings.HasPrefix(line, "## ID Formats") {
				break
			}
		}

		if inCommands {
			result = append(result, line)
		}
	}

	// If we didn't find a Commands section, return everything after frontmatter
	if len(result) == 0 {
		inFrontmatter = false
		pastFrontmatter := false
		for _, line := range lines {
			if line == "---" {
				if !pastFrontmatter {
					inFrontmatter = !inFrontmatter
					if !inFrontmatter {
						pastFrontmatter = true
					}
					continue
				}
			}
			if inFrontmatter {
				continue
			}
			if pastFrontmatter || !strings.HasPrefix(line, "---") {
				result = append(result, line)
			}
		}
	}

	return strings.TrimSpace(strings.Join(result, "\n"))
}
