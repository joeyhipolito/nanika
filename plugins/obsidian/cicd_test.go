package obsidian_test

import (
	"bufio"
	"bytes"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
)

// repoRoot locates the repo root via git, falling back to .git/ ancestor walk.
func repoRoot(t *testing.T) string {
	t.Helper()
	out, err := exec.Command("git", "rev-parse", "--show-toplevel").Output()
	if err == nil {
		return strings.TrimSpace(string(out))
	}
	dir, err := os.Getwd()
	if err != nil {
		t.Fatalf("getwd: %v", err)
	}
	for {
		if _, statErr := os.Stat(filepath.Join(dir, ".git")); statErr == nil {
			return dir
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			t.Fatal("repoRoot: .git not found in any ancestor directory")
		}
		dir = parent
	}
}

// jobsFromWorkflowYAML extracts top-level job keys from a GitHub Actions YAML
// without requiring an external YAML parser. It scans for lines at exactly
// 2-space indentation inside the jobs: block.
func jobsFromWorkflowYAML(data []byte) []string {
	var jobs []string
	inJobs := false
	scanner := bufio.NewScanner(bytes.NewReader(data))
	for scanner.Scan() {
		line := scanner.Text()
		// Detect the jobs: top-level key.
		if strings.TrimRight(line, " \t") == "jobs:" {
			inJobs = true
			continue
		}
		if !inJobs {
			continue
		}
		// A new top-level (non-indented) key means we left jobs:.
		if len(line) > 0 && line[0] != ' ' && line[0] != '\t' && !strings.HasPrefix(line, "#") {
			break
		}
		// Job names sit at exactly 2 spaces of indent (not 3+).
		if strings.HasPrefix(line, "  ") && !strings.HasPrefix(line, "   ") {
			trimmed := strings.TrimLeft(line, " ")
			if idx := strings.Index(trimmed, ":"); idx > 0 {
				name := trimmed[:idx]
				if !strings.ContainsAny(name, " \t") {
					jobs = append(jobs, name)
				}
			}
		}
	}
	return jobs
}

func TestWorkflowYAML_Parseable(t *testing.T) {
	root := repoRoot(t)
	yamlPath := filepath.Join(root, ".github", "workflows", "obsidian.yml")

	data, err := os.ReadFile(yamlPath)
	if err != nil {
		t.Fatalf("reading %s: %v", yamlPath, err)
	}

	wantJobs := []string{
		"unit", "benchmark", "fuzz", "golden",
		"vault-doctor", "coverage", "chaos", "test-first-check",
	}

	found := jobsFromWorkflowYAML(data)
	foundSet := make(map[string]bool, len(found))
	for _, j := range found {
		foundSet[j] = true
	}

	for _, job := range wantJobs {
		if !foundSet[job] {
			t.Errorf("job %q not found in %s", job, yamlPath)
		}
	}
	if len(found) != len(wantJobs) {
		t.Errorf("expected %d jobs, got %d: %v", len(wantJobs), len(found), found)
	}
}

func TestPRTemplate_RequiredFields(t *testing.T) {
	root := repoRoot(t)
	tmplPath := filepath.Join(root, ".github", "PULL_REQUEST_TEMPLATE.md")

	data, err := os.ReadFile(tmplPath)
	if err != nil {
		t.Fatalf("reading %s: %v", tmplPath, err)
	}

	required := []string{"## Test IDs"}
	for _, field := range required {
		if !bytes.Contains(data, []byte(field)) {
			t.Errorf("PR template %s missing required field %q", tmplPath, field)
		}
	}
}

func TestChaosScript_ExitCodes(t *testing.T) {
	root := repoRoot(t)
	script := filepath.Join(root, "scripts", "chaos-obsidian.sh")

	if _, err := os.Stat(script); err != nil {
		t.Fatalf("chaos script not found at %s: %v", script, err)
	}

	scenarios := []string{"daemon-kill", "partial-write", "disk-full"}
	for _, scenario := range scenarios {
		scenario := scenario
		t.Run(scenario, func(t *testing.T) {
			cmd := exec.Command("bash", script, "--dry-run", "--only", scenario)
			out, err := cmd.CombinedOutput()
			if err != nil {
				t.Errorf("chaos-obsidian.sh --dry-run --only %s: non-zero exit: %v\noutput:\n%s",
					scenario, err, out)
			}
		})
	}
}
