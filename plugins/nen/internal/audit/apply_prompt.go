package audit

import (
	"fmt"
	"strings"
)

// buildApplyPrompt assembles the LLM prompt for generating file diffs from
// audit recommendations.
func buildApplyPrompt(report *AuditReport, files []targetFile) string {
	var b strings.Builder

	b.WriteString(`You are an apply engine for an AI orchestration system. Given audit
recommendations and the current contents of target files, produce a JSON
plan of concrete file modifications that implement the recommendations.

## Rules

1. Only modify files where the recommendation clearly applies.
2. For persona files: adjust WhenToUse, WhenNotUse, Learning Focus, or
   other sections based on the recommendation. Preserve the existing
   structure and markdown formatting.
3. For SKILL.md (decomposer): adjust decomposition rules, constraints,
   or worked examples. Changes here propagate to the Go decomposer
   at runtime — the decomposer reads SKILL.md on every mission.
4. Do NOT invent changes beyond what the recommendations suggest.
5. Preserve all content not affected by the recommendation.
6. Each diff must contain the COMPLETE new file content, not a patch.

`)

	b.WriteString("## Audit Report\n\n")
	b.WriteString(fmt.Sprintf("**Workspace:** %s\n", report.WorkspaceID))
	b.WriteString(fmt.Sprintf("**Task:** %s\n", report.Task))
	b.WriteString(fmt.Sprintf("**Overall Score:** %d/5\n\n", report.Scorecard.Overall))

	if len(report.Evaluation.Weaknesses) > 0 {
		b.WriteString("### Weaknesses Identified\n")
		for _, w := range report.Evaluation.Weaknesses {
			b.WriteString(fmt.Sprintf("- %s\n", w))
		}
		b.WriteString("\n")
	}

	b.WriteString("### Recommendations to Implement\n\n")
	for i, r := range report.Evaluation.Recommendations {
		b.WriteString(fmt.Sprintf("%d. **[%s/%s]** %s\n",
			i+1, r.Category, r.Priority, r.Summary))
		if r.Detail != "" {
			b.WriteString(fmt.Sprintf("   Detail: %s\n", r.Detail))
		}
		b.WriteString("\n")
	}

	hasPersonaIssues := false
	for _, p := range report.Phases {
		if !p.PersonaCorrect || len(p.Issues) > 0 {
			hasPersonaIssues = true
			break
		}
	}
	if hasPersonaIssues {
		b.WriteString("### Phase Issues\n\n")
		for _, p := range report.Phases {
			if p.PersonaCorrect && len(p.Issues) == 0 {
				continue
			}
			b.WriteString(fmt.Sprintf("- **%s** (assigned: %s", p.PhaseName, p.PersonaAssigned))
			if !p.PersonaCorrect {
				b.WriteString(fmt.Sprintf(", ideal: %s", p.PersonaIdeal))
			}
			b.WriteString(")\n")
			for _, issue := range p.Issues {
				b.WriteString(fmt.Sprintf("  - %s\n", issue))
			}
		}
		b.WriteString("\n")
	}

	b.WriteString("## Current Files\n\n")
	b.WriteString("These are the files you may modify. Include ONLY files that need changes.\n\n")

	for _, f := range files {
		content := f.Content
		if len(content) > 4000 {
			content = content[:4000] + "\n... (truncated)"
		}
		b.WriteString(fmt.Sprintf("### %s [%s]\n", f.Path, f.Type))
		b.WriteString("```markdown\n")
		b.WriteString(content)
		b.WriteString("\n```\n\n")
	}

	b.WriteString("## Output Format\n\n")
	b.WriteString("Respond with a JSON object between ```json and ``` fences:\n\n")
	b.WriteString("```json\n")
	b.WriteString(applySchema())
	b.WriteString("\n```\n\n")
	b.WriteString(`Important:
- "new_content" must be the COMPLETE file content, not a diff or patch.
- "path" should match the file path shown above.
- "type" must be one of: "persona", "skill_md".
- Only include files that actually need changes.
- If no files need changes, return {"summary": "No changes needed", "diffs": []}.
`)

	return b.String()
}

func applySchema() string {
	return `{
  "summary": "Brief description of all changes being made",
  "diffs": [
    {
      "path": "/path/to/file.md",
      "type": "persona",
      "action": "modify",
      "rationale": "Why this change addresses the recommendation",
      "new_content": "Complete new file content..."
    }
  ]
}`
}
