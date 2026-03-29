// Package git – PR creation helpers.
package git

import (
	"bytes"
	"fmt"
	"os/exec"
	"strings"
)

// HasGH reports whether the gh CLI is available in PATH.
func HasGH() bool {
	_, err := exec.LookPath("gh")
	return err == nil
}

// HasCodex reports whether the codex CLI is available in PATH.
func HasCodex() bool {
	_, err := exec.LookPath("codex")
	return err == nil
}

// PRMetadata holds the mission metadata used to render the PR body.
type PRMetadata struct {
	Summary    string
	MissionID  string
	PhaseCount int
	Mode       string
	Personas   []string
	CostUSD    float64
	Duration   string
	Files      []string
}

// BuildPRBody renders a Markdown PR description from the provided metadata.
func BuildPRBody(m PRMetadata) string {
	var b strings.Builder

	b.WriteString("## Summary\n\n")
	if m.Summary != "" {
		b.WriteString(m.Summary)
	} else {
		b.WriteString("_No summary provided._")
	}
	b.WriteString("\n\n")

	b.WriteString("## Mission Details\n\n")
	b.WriteString("| Field | Value |\n")
	b.WriteString("|-------|-------|\n")
	fmt.Fprintf(&b, "| Mission ID | `%s` |\n", m.MissionID)
	fmt.Fprintf(&b, "| Phases | %d |\n", m.PhaseCount)
	fmt.Fprintf(&b, "| Mode | %s |\n", m.Mode)
	if len(m.Personas) > 0 {
		fmt.Fprintf(&b, "| Personas | %s |\n", strings.Join(m.Personas, ", "))
	}
	if m.CostUSD > 0 {
		fmt.Fprintf(&b, "| Cost | $%.4f |\n", m.CostUSD)
	}
	if m.Duration != "" {
		fmt.Fprintf(&b, "| Duration | %s |\n", m.Duration)
	}

	if len(m.Files) > 0 {
		b.WriteString("\n## Files Changed\n\n")
		for _, f := range m.Files {
			fmt.Fprintf(&b, "- `%s`\n", f)
		}
	}

	b.WriteString("\n---\n*Created by [nanika orchestrator](https://github.com/joeyhipolito/nanika)*\n")

	return b.String()
}

// ChangedFiles returns the list of files that differ between base and head in
// repoRoot. Suitable for populating PRMetadata.Files before the branch is
// pushed. Returns nil (no error) when there are no differences.
func ChangedFiles(repoRoot, base, head string) ([]string, error) {
	out, err := run(repoRoot, "git", "diff", "--name-only", base+"..."+head)
	if err != nil {
		return nil, fmt.Errorf("git diff --name-only %s...%s: %w", base, head, err)
	}
	var files []string
	for _, line := range strings.Split(strings.TrimSpace(out), "\n") {
		if line != "" {
			files = append(files, line)
		}
	}
	return files, nil
}

// CommentOnPR posts a comment on an existing pull request identified by prURL.
// Uses "gh pr comment <prURL> --body <body>".
func CommentOnPR(repoRoot, prURL, body string) error {
	cmd := exec.Command("gh", "pr", "comment", prURL, "--body", body) //nolint:gosec
	cmd.Dir = repoRoot
	var errOut bytes.Buffer
	cmd.Stderr = &errOut
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("gh pr comment: %w\n%s", err, errOut.String())
	}
	return nil
}

// AddPRReviewers requests GitHub reviewers on an existing pull request via
// "gh pr edit <prURL> --add-reviewer <r1,r2,...>".
func AddPRReviewers(repoRoot, prURL string, reviewers []string) error {
	if len(reviewers) == 0 {
		return nil
	}
	cmd := exec.Command("gh", "pr", "edit", prURL, "--add-reviewer", strings.Join(reviewers, ",")) //nolint:gosec
	cmd.Dir = repoRoot
	var errOut bytes.Buffer
	cmd.Stderr = &errOut
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("gh pr edit --add-reviewer: %w\n%s", err, errOut.String())
	}
	return nil
}

// AddPRLabels adds labels to an existing pull request via
// "gh pr edit <prURL> --add-label <l1,l2,...>".
func AddPRLabels(repoRoot, prURL string, labels []string) error {
	if len(labels) == 0 {
		return nil
	}
	cmd := exec.Command("gh", "pr", "edit", prURL, "--add-label", strings.Join(labels, ",")) //nolint:gosec
	cmd.Dir = repoRoot
	var errOut bytes.Buffer
	cmd.Stderr = &errOut
	if err := cmd.Run(); err != nil {
		return fmt.Errorf("gh pr edit --add-label: %w\n%s", err, errOut.String())
	}
	return nil
}

// CreatePR invokes the gh CLI to open a pull request from head against base in
// the repository at repoRoot. Returns the PR URL printed by gh on success.
func CreatePR(repoRoot, head, base, title, body string, draft bool) (string, error) {
	args := []string{
		"pr", "create",
		"--head", head,
		"--base", base,
		"--title", title,
		"--body", body,
	}
	if draft {
		args = append(args, "--draft")
	}

	cmd := exec.Command("gh", args...) //nolint:gosec
	cmd.Dir = repoRoot
	var out, errOut bytes.Buffer
	cmd.Stdout = &out
	cmd.Stderr = &errOut
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("gh pr create: %w\n%s", err, errOut.String())
	}
	return strings.TrimSpace(out.String()), nil
}

// RunCodexReview executes "codex review" against the branch checked out in
// repoRoot. When prompt is non-empty it is passed via stdin using the "-"
// prompt sentinel, avoiding shell interpolation.
func RunCodexReview(repoRoot, baseBranch, prompt string) (string, error) {
	args := []string{"review"}
	if baseBranch != "" {
		args = append(args, "--base", baseBranch)
	}
	if strings.TrimSpace(prompt) != "" {
		args = append(args, "-")
	}

	cmd := exec.Command("codex", args...) //nolint:gosec
	cmd.Dir = repoRoot
	if strings.TrimSpace(prompt) != "" {
		cmd.Stdin = strings.NewReader(prompt)
	}

	var out, errOut bytes.Buffer
	cmd.Stdout = &out
	cmd.Stderr = &errOut
	if err := cmd.Run(); err != nil {
		return "", fmt.Errorf("codex review: %w\n%s", err, errOut.String())
	}
	return strings.TrimSpace(out.String()), nil
}
