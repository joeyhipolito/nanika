package audit

import (
	"fmt"
	"strings"
)

// maxOutputPerPhase is the character limit for worker output in the prompt.
const maxOutputPerPhase = 3000

// buildEvaluationPrompt assembles the full LLM evaluation prompt.
func buildEvaluationPrompt(
	task string,
	plan *Plan,
	outputs map[string]string,
	personaCatalog string,
	skillIndex string,
	decompositionRules string,
) string {
	var b strings.Builder

	b.WriteString(`You are a mission auditor for an AI orchestration system. Your job is to
critically evaluate a completed mission — identifying what went wrong, what
could be improved, and whether the decomposition and execution were sound.

Be harsh. The goal is to surface problems, not to be encouraging. A score
of 3/5 means "acceptable." 4/5 means "good." 5/5 is rare and means
"could not be improved." 1-2/5 means serious problems.

## Evaluation Criteria

### Persona Catalog
For each phase, check whether the assigned persona was the best match.
The persona's "When to Use" and "When NOT to Use" sections define the
boundaries. A senior-backend-engineer doing architecture work is a misassignment.
A technical-writer doing code review is a misassignment.

`)
	b.WriteString(personaCatalog)

	b.WriteString("\n### Decomposition Rules (from SKILL.md)\n")
	b.WriteString("These are the canonical rules the decomposer should have followed.\n")
	b.WriteString("Check whether the plan violated any of them.\n\n")
	b.WriteString(decompositionRules)

	b.WriteString("\n\n### Available Skills\n")
	b.WriteString("Check whether phases that could have benefited from a skill were assigned it,\n")
	b.WriteString("and whether phases were assigned skills they didn't need.\n\n")
	if skillIndex != "" {
		b.WriteString(skillIndex)
	} else {
		b.WriteString("(no skills available)\n")
	}

	b.WriteString("\n## Mission Under Audit\n\n")
	b.WriteString("### Original Task\n")
	b.WriteString(task)
	b.WriteString("\n\n")

	// Plan summary
	b.WriteString("### Decomposed Plan\n")
	b.WriteString(fmt.Sprintf("Execution mode: %s | Phases: %d\n\n", plan.ExecutionMode, len(plan.Phases)))

	for _, p := range plan.Phases {
		b.WriteString(fmt.Sprintf("- **%s** (id=%s, persona=%s, tier=%s",
			p.Name, p.ID, p.Persona, p.ModelTier))
		if len(p.Skills) > 0 {
			b.WriteString(fmt.Sprintf(", skills=%s", strings.Join(p.Skills, ",")))
		}
		if len(p.Dependencies) > 0 {
			b.WriteString(fmt.Sprintf(", depends=%s", strings.Join(p.Dependencies, ",")))
		}
		b.WriteString(fmt.Sprintf(", status=%s", p.Status))
		b.WriteString(")\n")
		b.WriteString(fmt.Sprintf("  Objective: %s\n", p.Objective))
	}

	// Phase outputs
	b.WriteString("\n### Phase Outputs\n")
	for _, p := range plan.Phases {
		b.WriteString(fmt.Sprintf("\n#### Phase: %s (persona: %s, status: %s)\n",
			p.Name, p.Persona, p.Status))

		output := findOutput(p, outputs)
		if output == "" {
			b.WriteString("(no output captured)\n")
			continue
		}

		origLen := len(output)
		if origLen > maxOutputPerPhase {
			output = output[:maxOutputPerPhase]
			b.WriteString(fmt.Sprintf("[truncated from %d chars]\n", origLen))
		}
		b.WriteString(output)
		b.WriteString("\n")
	}

	// JSON schema
	b.WriteString("\n## Output Format\n\n")
	b.WriteString("Respond with a JSON object between ```json and ``` fences. The JSON must\n")
	b.WriteString("match this exact structure:\n\n")
	b.WriteString("```json\n")
	b.WriteString(jsonSchema())
	b.WriteString("\n```\n\n")

	b.WriteString(`### Scoring Guide
- decomposition_quality: Did the decomposition follow SKILL.md rules?
  Correct number of phases? Good boundaries? Proper dependency chains?
- persona_fit: Were personas correctly matched to phase objectives?
  Check against WhenToUse/WhenNotToUse.
- skill_utilization: Were available skills assigned when beneficial?
  Were unnecessary skills assigned?
- output_quality: Did phase outputs actually accomplish their objectives?
  Were outputs substantive or thin?
- rule_compliance: Did the decomposition follow the specific rules in
  the Decomposition Rules section?

### Convergence Assessment
For the convergence field, evaluate:
- Did each phase's output match its stated objective, or did workers drift?
- Was there work done that should have been planned as a separate phase?
- Did multiple phases duplicate effort?

Be specific. Name phases. Quote rules that were violated. Give concrete
recommendations, not vague advice like "improve decomposition."
`)

	return b.String()
}

// findOutput locates worker output for a phase.
func findOutput(phase *Phase, outputs map[string]string) string {
	for dirName, text := range outputs {
		if strings.Contains(dirName, phase.Name) {
			return text
		}
	}
	for dirName, text := range outputs {
		if strings.Contains(dirName, phase.ID) {
			return text
		}
	}
	if phase.Output != "" {
		return phase.Output
	}
	return ""
}

func jsonSchema() string {
	return `{
  "scorecard": {
    "decomposition_quality": 3,
    "persona_fit": 4,
    "skill_utilization": 2,
    "output_quality": 4,
    "rule_compliance": 3
  },
  "evaluation": {
    "summary": "2-3 sentence verdict on the mission",
    "strengths": ["what went well"],
    "weaknesses": ["what went wrong"],
    "recommendations": [
      {
        "category": "decomposition|persona|skill|process",
        "priority": "high|medium|low",
        "summary": "one-line description",
        "detail": "how to fix it"
      }
    ]
  },
  "phases": [
    {
      "phase_id": "phase-1",
      "phase_name": "name",
      "persona_assigned": "senior-backend-engineer",
      "persona_ideal": "senior-backend-engineer",
      "persona_correct": true,
      "objective_met": true,
      "issues": ["specific problem"],
      "score": 4
    }
  ],
  "convergence": {
    "converged": false,
    "drift_phases": ["phase names where output diverged from objective"],
    "missing_phases": ["work that should have been a separate phase"],
    "redundant_work": ["overlapping work between phases"],
    "assessment": "narrative on convergence"
  }
}`
}
