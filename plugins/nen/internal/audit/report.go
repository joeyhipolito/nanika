package audit

import (
	"encoding/json"
	"fmt"
	"strings"
)

// FormatText renders the audit report as terminal output.
func FormatText(report *AuditReport) string {
	var b strings.Builder

	b.WriteString(fmt.Sprintf("Mission Audit: %s\n", report.WorkspaceID))
	b.WriteString(strings.Repeat("=", 50))
	b.WriteString("\n\n")

	b.WriteString(fmt.Sprintf("Task:   %s\n", report.Task))
	b.WriteString(fmt.Sprintf("Domain: %s | Status: %s\n", report.Domain, report.Status))
	if report.LinearIssueID != "" {
		b.WriteString(fmt.Sprintf("Issue:  %s\n", report.LinearIssueID))
	}
	if report.MissionPath != "" {
		b.WriteString(fmt.Sprintf("Source: %s\n", report.MissionPath))
	}
	b.WriteString("\n")

	b.WriteString("Scorecard\n")
	b.WriteString(strings.Repeat("-", 35))
	b.WriteString("\n")
	writeScore(&b, "Decomposition", report.Scorecard.DecompositionQuality)
	writeScore(&b, "Persona Fit", report.Scorecard.PersonaFit)
	writeScore(&b, "Skill Usage", report.Scorecard.SkillUtilization)
	writeScore(&b, "Output Quality", report.Scorecard.OutputQuality)
	writeScore(&b, "Rule Compliance", report.Scorecard.RuleCompliance)
	b.WriteString("  ")
	b.WriteString(strings.Repeat("-", 25))
	b.WriteString("\n")
	writeScore(&b, "Overall", report.Scorecard.Overall)
	b.WriteString("\n")

	b.WriteString("Summary\n")
	b.WriteString(strings.Repeat("-", 35))
	b.WriteString("\n")
	b.WriteString(report.Evaluation.Summary)
	b.WriteString("\n\n")

	if len(report.Evaluation.Strengths) > 0 {
		b.WriteString("Strengths\n")
		for _, s := range report.Evaluation.Strengths {
			b.WriteString(fmt.Sprintf("  + %s\n", s))
		}
		b.WriteString("\n")
	}

	if len(report.Evaluation.Weaknesses) > 0 {
		b.WriteString("Weaknesses\n")
		for _, w := range report.Evaluation.Weaknesses {
			b.WriteString(fmt.Sprintf("  - %s\n", w))
		}
		b.WriteString("\n")
	}

	b.WriteString("Phases\n")
	b.WriteString(strings.Repeat("-", 35))
	b.WriteString("\n")
	for _, p := range report.Phases {
		marker := "+"
		if !p.ObjectiveMet {
			marker = "x"
		}
		b.WriteString(fmt.Sprintf("  %s %s (%s)", marker, p.PhaseName, p.PersonaAssigned))
		b.WriteString(fmt.Sprintf(" %s %d/5\n", scoreBar(p.Score), p.Score))
		if !p.PersonaCorrect {
			b.WriteString(fmt.Sprintf("    -> should be: %s\n", p.PersonaIdeal))
		}
		for _, issue := range p.Issues {
			b.WriteString(fmt.Sprintf("    ! %s\n", issue))
		}
	}
	b.WriteString("\n")

	b.WriteString("Convergence: ")
	if report.Convergence.Converged {
		b.WriteString("OK\n")
	} else {
		b.WriteString("DRIFT DETECTED\n")
	}
	if report.Convergence.Assessment != "" {
		b.WriteString(fmt.Sprintf("  %s\n", report.Convergence.Assessment))
	}
	for _, d := range report.Convergence.DriftPhases {
		b.WriteString(fmt.Sprintf("  drift: %s\n", d))
	}
	for _, m := range report.Convergence.MissingPhases {
		b.WriteString(fmt.Sprintf("  missing: %s\n", m))
	}
	for _, r := range report.Convergence.RedundantWork {
		b.WriteString(fmt.Sprintf("  redundant: %s\n", r))
	}
	b.WriteString("\n")

	if len(report.Evaluation.Recommendations) > 0 {
		b.WriteString("Recommendations\n")
		b.WriteString(strings.Repeat("-", 35))
		b.WriteString("\n")
		for _, r := range report.Evaluation.Recommendations {
			pri := strings.ToUpper(r.Priority)
			b.WriteString(fmt.Sprintf("  [%s] %s\n", pri, r.Summary))
			if r.Detail != "" {
				b.WriteString(fmt.Sprintf("        %s\n", r.Detail))
			}
		}
		b.WriteString("\n")
	}

	b.WriteString("Decomposer\n")
	b.WriteString(strings.Repeat("-", 35))
	b.WriteString("\n")
	b.WriteString(fmt.Sprintf("  Source: %s\n", report.DecomposerConvergence.PromptSource))
	if report.DecomposerConvergence.SKILLMDHash != "" {
		b.WriteString(fmt.Sprintf("  SKILL.md hash: %s\n", report.DecomposerConvergence.SKILLMDHash[:16]))
	}
	if report.DecomposerConvergence.SKILLMDPath != "" {
		b.WriteString(fmt.Sprintf("  Path: %s\n", report.DecomposerConvergence.SKILLMDPath))
	}
	b.WriteString(fmt.Sprintf("  Rules extracted: %v\n\n", report.DecomposerConvergence.RulesExtracted))

	if len(report.Changes) > 0 {
		b.WriteString(fmt.Sprintf("Changes (%d detected)\n", len(report.Changes)))
		b.WriteString(strings.Repeat("-", 35))
		b.WriteString("\n")
		limit := len(report.Changes)
		if limit > 20 {
			limit = 20
		}
		for _, c := range report.Changes[:limit] {
			b.WriteString(fmt.Sprintf("  %-15s %s  %s\n", c.Type, c.PhaseName, c.Target))
		}
		if len(report.Changes) > 20 {
			b.WriteString(fmt.Sprintf("  ... and %d more\n", len(report.Changes)-20))
		}
	}

	return b.String()
}

// FormatJSON renders the audit report as pretty-printed JSON.
func FormatJSON(report *AuditReport) (string, error) {
	data, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		return "", fmt.Errorf("marshaling report: %w", err)
	}
	return string(data), nil
}

// FormatMarkdown renders the audit report as markdown.
func FormatMarkdown(report *AuditReport) string {
	var b strings.Builder

	b.WriteString(fmt.Sprintf("# Mission Audit: %s\n\n", report.WorkspaceID))
	b.WriteString(fmt.Sprintf("**Task:** %s\n", report.Task))
	b.WriteString(fmt.Sprintf("**Domain:** %s | **Status:** %s | **Audited:** %s\n",
		report.Domain, report.Status, report.AuditedAt.Format("2006-01-02 15:04")))
	if report.LinearIssueID != "" {
		b.WriteString(fmt.Sprintf("**Issue:** %s\n", report.LinearIssueID))
	}
	if report.MissionPath != "" {
		b.WriteString(fmt.Sprintf("**Source:** `%s`\n", report.MissionPath))
	}
	b.WriteString("\n")

	b.WriteString("## Scorecard\n\n")
	b.WriteString("| Axis | Score |\n|------|-------|\n")
	b.WriteString(fmt.Sprintf("| Decomposition | %d/5 |\n", report.Scorecard.DecompositionQuality))
	b.WriteString(fmt.Sprintf("| Persona Fit | %d/5 |\n", report.Scorecard.PersonaFit))
	b.WriteString(fmt.Sprintf("| Skill Usage | %d/5 |\n", report.Scorecard.SkillUtilization))
	b.WriteString(fmt.Sprintf("| Output Quality | %d/5 |\n", report.Scorecard.OutputQuality))
	b.WriteString(fmt.Sprintf("| Rule Compliance | %d/5 |\n", report.Scorecard.RuleCompliance))
	b.WriteString(fmt.Sprintf("| **Overall** | **%d/5** |\n\n", report.Scorecard.Overall))

	b.WriteString("## Summary\n\n")
	b.WriteString(report.Evaluation.Summary)
	b.WriteString("\n\n")

	if len(report.Evaluation.Strengths) > 0 {
		b.WriteString("### Strengths\n")
		for _, s := range report.Evaluation.Strengths {
			b.WriteString(fmt.Sprintf("- %s\n", s))
		}
		b.WriteString("\n")
	}

	if len(report.Evaluation.Weaknesses) > 0 {
		b.WriteString("### Weaknesses\n")
		for _, w := range report.Evaluation.Weaknesses {
			b.WriteString(fmt.Sprintf("- %s\n", w))
		}
		b.WriteString("\n")
	}

	b.WriteString("## Phase Evaluations\n\n")
	for _, p := range report.Phases {
		status := "met"
		if !p.ObjectiveMet {
			status = "NOT met"
		}
		b.WriteString(fmt.Sprintf("### %s (%d/5, objective %s)\n", p.PhaseName, p.Score, status))
		b.WriteString(fmt.Sprintf("- Persona: %s", p.PersonaAssigned))
		if !p.PersonaCorrect {
			b.WriteString(fmt.Sprintf(" (should be **%s**)", p.PersonaIdeal))
		}
		b.WriteString("\n")
		for _, issue := range p.Issues {
			b.WriteString(fmt.Sprintf("- Issue: %s\n", issue))
		}
		b.WriteString("\n")
	}

	b.WriteString("## Convergence\n\n")
	if report.Convergence.Converged {
		b.WriteString("Execution converged with the plan.\n\n")
	} else {
		b.WriteString("**Drift detected.**\n\n")
	}
	if report.Convergence.Assessment != "" {
		b.WriteString(report.Convergence.Assessment)
		b.WriteString("\n\n")
	}

	if len(report.Evaluation.Recommendations) > 0 {
		b.WriteString("## Recommendations\n\n")
		for _, r := range report.Evaluation.Recommendations {
			b.WriteString(fmt.Sprintf("- **[%s]** %s — %s\n", strings.ToUpper(r.Priority), r.Summary, r.Detail))
		}
		b.WriteString("\n")
	}

	b.WriteString("## Decomposer Convergence\n\n")
	b.WriteString(fmt.Sprintf("- **Source:** %s\n", report.DecomposerConvergence.PromptSource))
	if report.DecomposerConvergence.SKILLMDHash != "" {
		b.WriteString(fmt.Sprintf("- **SKILL.md hash:** `%s`\n", report.DecomposerConvergence.SKILLMDHash[:16]))
	}
	b.WriteString(fmt.Sprintf("- **Rules extracted:** %v\n\n", report.DecomposerConvergence.RulesExtracted))

	return b.String()
}

func writeScore(b *strings.Builder, label string, score int) {
	b.WriteString(fmt.Sprintf("  %-15s %s  %d/5\n", label, scoreBar(score), score))
}

func scoreBar(score int) string {
	if score < 1 {
		score = 1
	}
	if score > 5 {
		score = 5
	}
	filled := strings.Repeat("#", score)
	empty := strings.Repeat(".", 5-score)
	return filled + empty
}
