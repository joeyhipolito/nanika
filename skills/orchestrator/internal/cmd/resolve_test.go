package cmd

import (
	"os"
	"path/filepath"
	"testing"
)

// TestResolveTarget_CWDGitRootWinsOverTaskText verifies source 1 priority:
// when cwd contains a git root, it is returned regardless of task text keywords.
func TestResolveTarget_CWDGitRootWinsOverTaskText(t *testing.T) {
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, ".git"), 0700); err != nil {
		t.Fatal(err)
	}

	got := resolveTarget(dir, "", "fix orchestrator")

	// Must NOT resolve to the orchestrator keyword target.
	if got == "repo:~/nanika/skills/orchestrator" {
		t.Errorf("cwd git root should win over task text keyword, but got orchestrator target")
	}
	// Must resolve to something (the temp dir's git root).
	if got == "" {
		t.Errorf("expected non-empty target from cwd git root, got empty")
	}
}

// TestResolveTarget_TaskTextFindsOrchestrator verifies source 3:
// when no cwd git root and no mission path, "orchestrator" in task text
// resolves to the canonical orchestrator target.
func TestResolveTarget_TaskTextFindsOrchestrator(t *testing.T) {
	// Use an empty cwd and no mission path so sources 1 and 2 are skipped.
	got := resolveTarget("", "", "fix orchestrator")
	if got != "repo:~/nanika/skills/orchestrator" {
		t.Errorf("got %q; want repo:~/nanika/skills/orchestrator", got)
	}
}

// TestResolveTarget_MissionPathUnderOrchestrator verifies source 2:
// a mission file located under ~/skills/orchestrator/ resolves via the
// mission file's git root, not task text.
func TestResolveTarget_MissionPathUnderOrchestrator(t *testing.T) {
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skip("cannot determine home dir")
	}
	orchestratorDir := filepath.Join(home, "nanika", "skills", "orchestrator")
	if _, err := os.Stat(filepath.Join(orchestratorDir, ".git")); err != nil {
		t.Skipf("~/nanika/skills/orchestrator is not a git repo: %v", err)
	}

	// The mission file doesn't need to exist — findGitRoot only checks for
	// .git in ancestor directories.
	missionPath := filepath.Join(orchestratorDir, "missions", "foo.md")

	got := resolveTarget("", missionPath, "")
	if got != "repo:~/nanika/skills/orchestrator" {
		t.Errorf("got %q; want repo:~/nanika/skills/orchestrator", got)
	}
}

// TestResolveTarget_CWDWinsOverMissionPath verifies that a cwd git root
// beats a mission file path under a different git repo.
func TestResolveTarget_CWDWinsOverMissionPath(t *testing.T) {
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, ".git"), 0700); err != nil {
		t.Fatal(err)
	}

	home, _ := os.UserHomeDir()
	missionPath := filepath.Join(home, "skills", "orchestrator", "missions", "foo.md")

	got := resolveTarget(dir, missionPath, "fix orchestrator")

	// Must NOT be the orchestrator target — cwd wins.
	if got == "repo:~/nanika/skills/orchestrator" {
		t.Errorf("cwd git root should win over mission path and task text")
	}
	if got == "" {
		t.Errorf("expected non-empty target from cwd git root")
	}
}

// TestResolveTarget_NoSourcesReturnsEmpty verifies that when all three sources
// yield nothing, resolveTarget returns "".
func TestResolveTarget_NoSourcesReturnsEmpty(t *testing.T) {
	// Empty cwd, no mission path, no known system in task.
	got := resolveTarget("", "", "write a haiku about clouds")
	if got != "" {
		t.Errorf("got %q; want empty string when no source matches", got)
	}
}

// TestResolveFromTaskText covers the known system keyword table.
func TestResolveFromTaskText(t *testing.T) {
	tests := []struct {
		task string
		want string
	}{
		{"fix orchestrator routing bug", "repo:~/nanika/skills/orchestrator"},
		// Case-insensitive.
		{"Fix ORCHESTRATOR routing", "repo:~/nanika/skills/orchestrator"},
		// Unknown system — no match.
		{"deploy the new service", ""},
		{"update scout feed parser", ""},
		{"gmail label sync", ""},
		{"", ""},
	}

	for _, tt := range tests {
		got := resolveFromTaskText(tt.task)
		if got != tt.want {
			t.Errorf("resolveFromTaskText(%q) = %q; want %q", tt.task, got, tt.want)
		}
	}
}

// TestResolveTarget_MissionPathNoGitRoot verifies that a mission path with no
// git ancestor falls through to task text (source 3).
func TestResolveTarget_MissionPathNoGitRoot(t *testing.T) {
	// Use a temp dir as the mission file's parent — no .git present.
	dir := t.TempDir()
	missionPath := filepath.Join(dir, "foo.md")

	got := resolveTarget("", missionPath, "fix orchestrator")
	// Source 2 finds nothing (temp dir has no git root), falls to source 3.
	if got != "repo:~/nanika/skills/orchestrator" {
		t.Errorf("got %q; want repo:~/nanika/skills/orchestrator (source 3 fallback)", got)
	}
}

// ─── Non-Repo Target Resolution ──────────────────────────────────────────────

// TestResolveFromTaskText_NonRepoTargets verifies that orchestration and newsletter
// signals resolve to non-repo canonical targets when no CLI tool name is present,
// and that existing repo targets are never displaced.
func TestResolveFromTaskText_NonRepoTargets(t *testing.T) {
	tests := []struct {
		task string
		want string
	}{
		// system:via — "orchestration" keyword (distinct from "orchestrator" CLI).
		{"design the orchestration strategy for Nanika", "system:via"},
		{"improve orchestration reliability across missions", "system:via"},
		{"review our orchestration approach", "system:via"},
		// publication:substack — "newsletter" keyword (distinct from "substack" CLI).
		{"plan my newsletter content for the month", "publication:substack"},
		{"write the weekly newsletter for subscribers", "publication:substack"},
		// Existing repo targets must NOT be displaced.
		// "orchestrator" is a word-boundary match that beats "orchestration".
		{"fix the orchestrator routing bug", "repo:~/nanika/skills/orchestrator"},
		// "substack" is no longer a known system; newsletter still resolves to publication:substack.
		{"fix the substack CLI draft command", ""},
		// Unrelated tasks still return empty.
		{"write a haiku about clouds", ""},
		{"deploy the new service", ""},
	}

	for _, tt := range tests {
		got := resolveFromTaskText(tt.task)
		if got != tt.want {
			t.Errorf("resolveFromTaskText(%q) = %q; want %q", tt.task, got, tt.want)
		}
	}
}

// TestResolveTarget_NonRepoTargetFromTaskText verifies that resolveTarget
// returns a non-repo target ID via source-3 when no git root is available.
func TestResolveTarget_NonRepoTargetFromTaskText(t *testing.T) {
	got := resolveTarget("", "", "design the orchestration strategy")
	if got != "system:via" {
		t.Errorf("got %q; want system:via", got)
	}
}

// TestResolveTarget_RepoWinsOverNonRepo verifies that a cwd git root (source 1)
// overrides a non-repo task-text match (source 3).
func TestResolveTarget_RepoWinsOverNonRepo(t *testing.T) {
	dir := t.TempDir()
	if err := os.Mkdir(filepath.Join(dir, ".git"), 0700); err != nil {
		t.Fatal(err)
	}
	got := resolveTarget(dir, "", "design the orchestration strategy")
	// cwd git root must win over the non-repo "orchestration" keyword.
	if got == "system:via" {
		t.Errorf("cwd git root should beat non-repo task-text match, but got system:via")
	}
	if got == "" {
		t.Errorf("expected non-empty target from cwd git root")
	}
}

// TestResolveTarget_NanikaRepoDefersToExplicitTaskTarget verifies that the ambient
// Nanika control repo does not swallow a more specific task target.
func TestResolveTarget_NanikaRepoDefersToExplicitTaskTarget(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	nanikaDir := filepath.Join(home, "nanika")
	if err := os.MkdirAll(filepath.Join(nanikaDir, ".git"), 0700); err != nil {
		t.Fatal(err)
	}

	tests := []struct {
		task string
		want string
	}{
		{"fix the orchestrator routing bug", "repo:~/nanika/skills/orchestrator"},
		{"write the weekly newsletter for subscribers", "publication:substack"},
		{"design the orchestration strategy for Nanika", "system:via"},
	}

	for _, tt := range tests {
		got := resolveTarget(nanikaDir, "", tt.task)
		if got != tt.want {
			t.Errorf("resolveTarget(nanika repo, %q) = %q; want %q", tt.task, got, tt.want)
		}
	}
}

// TestResolveTarget_NonNanikaRepoStillWins verifies that the override only applies
// to the Nanika control repo. Real code repos should still keep cwd precedence.
func TestResolveTarget_NonNanikaRepoStillWins(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	appDir := filepath.Join(home, "app")
	if err := os.MkdirAll(filepath.Join(appDir, ".git"), 0700); err != nil {
		t.Fatal(err)
	}

	got := resolveTarget(appDir, "", "write the weekly newsletter for subscribers")
	if got != "repo:~/app" {
		t.Errorf("resolveTarget(non-nanika repo, newsletter task) = %q; want repo:~/app", got)
	}
}

// TestResolveTarget_NanikaMissionPathDefersToTaskTarget verifies the same override
// for mission files stored inside the Nanika repo when no cwd git root is present.
func TestResolveTarget_NanikaMissionPathDefersToTaskTarget(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	nanikaDir := filepath.Join(home, "nanika")
	if err := os.MkdirAll(filepath.Join(nanikaDir, ".git"), 0700); err != nil {
		t.Fatal(err)
	}

	missionPath := filepath.Join(nanikaDir, "missions", "newsletter.md")
	got := resolveTarget("", missionPath, "write the weekly newsletter for subscribers")
	if got != "publication:substack" {
		t.Errorf("resolveTarget(nanika mission path, newsletter task) = %q; want publication:substack", got)
	}
}

// TestResolveTarget_ExplicitMissionTargetWins verifies that an explicit mission
// frontmatter target outranks both cwd/task-text heuristics and prevents
// misleading keywords in the mission body from hijacking the target repo.
func TestResolveTarget_ExplicitMissionTargetWins(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	targetPath := filepath.Join(home, "dev", "personal", "mywebsite")
	mission := "---\nlinear_issue_id: V-23\ntarget: repo:" + targetPath + "\nstatus: active\n---\n\n# Mission: V-23 Foundation Hardening\n\nThis mission mentions how-the-orchestrator-works and Substack sync work.\n"

	got := resolveTarget("", "", mission)
	if got != "repo:~/dev/personal/mywebsite" {
		t.Errorf("resolveTarget(explicit mission target) = %q; want repo:~/dev/personal/mywebsite", got)
	}
}

// TestResolveTarget_ExplicitMissionTargetBeatsNanikaOverride verifies that the
// explicit target still wins even when the command is launched from ~/nanika and
// the task text contains a stronger keyword match like "orchestrator".
func TestResolveTarget_ExplicitMissionTargetBeatsNanikaOverride(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	nanikaDir := filepath.Join(home, "nanika")
	if err := os.MkdirAll(filepath.Join(nanikaDir, ".git"), 0700); err != nil {
		t.Fatal(err)
	}

	targetRepo := filepath.Join(home, "dev", "personal", "mywebsite")

	mission := `---
target: repo:` + targetRepo + `
---

Fix how-the-orchestrator-works and prepare later Substack sync work.
`

	got := resolveTarget(nanikaDir, "", mission)
	if got != "repo:~/dev/personal/mywebsite" {
		t.Errorf("resolveTarget(nanika repo, explicit target mission) = %q; want repo:~/dev/personal/mywebsite", got)
	}
}

func TestExtractMissionTarget(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	absPath := filepath.Join(home, "dev", "personal", "mywebsite")

	tests := []struct {
		name string
		task string
		want string
	}{
		{
			name: "absolute repo path canonicalized",
			task: "---\ntarget: repo:" + absPath + "\n---\n# Mission\n",
			want: "repo:~/dev/personal/mywebsite",
		},
		{
			name: "already canonical repo target preserved",
			task: "---\ntarget: repo:~/nanika/skills/orchestrator\n---\n# Mission\n",
			want: "repo:~/nanika/skills/orchestrator",
		},
		{
			name: "quoted target canonicalized",
			task: "---\ntarget: \"repo:" + absPath + "\"\n---\n# Mission\n",
			want: "repo:~/dev/personal/mywebsite",
		},
		{
			name: "quoted empty target ignored",
			task: "---\ntarget: \"\"\n---\n# Mission\n",
			want: "",
		},
		{
			name: "non repo target preserved",
			task: "---\ntarget: publication:substack\n---\n# Mission\n",
			want: "publication:substack",
		},
		{
			name: "no frontmatter target",
			task: "# Mission\nNo target here\n",
			want: "",
		},
		{
			name: "unclosed frontmatter ignored",
			task: "---\ntarget: repo:/tmp/app\n# Mission\nbody target: repo:/tmp/wrong\n",
			want: "",
		},
		{
			name: "bare absolute path canonicalized as repo target",
			task: "---\ntarget: " + absPath + "\n---\n# Mission\n",
			want: "repo:~/dev/personal/mywebsite",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := extractMissionTarget(tt.task); got != tt.want {
				t.Errorf("extractMissionTarget() = %q; want %q", got, tt.want)
			}
		})
	}
}

func TestParseMissionFrontmatter_UnquotesLinkageFields(t *testing.T) {
	home := t.TempDir()
	t.Setenv("HOME", home)

	absPath := filepath.Join(home, "dev", "personal", "mywebsite")
	task := "---\nlinear_issue_id: \"V-5\"\ntarget: \"repo:" + absPath + "\"\nstatus: \"active\"\n---\n\n# Mission: Test\n"

	fm := parseMissionFrontmatter(task)
	if fm.LinearIssueID != "V-5" {
		t.Fatalf("LinearIssueID = %q; want %q", fm.LinearIssueID, "V-5")
	}
	if fm.Target != "repo:~/dev/personal/mywebsite" {
		t.Fatalf("Target = %q; want %q", fm.Target, "repo:~/dev/personal/mywebsite")
	}
	if fm.Status != "active" {
		t.Fatalf("Status = %q; want %q", fm.Status, "active")
	}
}

// TestParseMissionFrontmatter_LinkageFields exercises the three linkage-field
// states: empty (no frontmatter), default (frontmatter present but field
// omitted), and present (field explicitly set).
func TestParseMissionFrontmatter_LinkageFields(t *testing.T) {
	tests := []struct {
		name            string
		task            string
		wantIssueID     string
		wantTarget      string
		wantStatus      string
	}{
		{
			name:        "no frontmatter returns zero struct",
			task:        "# Just a plain mission\nDo something useful.\n",
			wantIssueID: "",
			wantTarget:  "",
			wantStatus:  "",
		},
		{
			name:        "empty frontmatter block returns zero struct",
			task:        "---\n---\n# Mission with empty frontmatter\n",
			wantIssueID: "",
			wantTarget:  "",
			wantStatus:  "",
		},
		{
			name:        "unclosed frontmatter returns zero struct",
			task:        "---\nlinear_issue_id: V-99\n# No closing delimiter\n",
			wantIssueID: "",
			wantTarget:  "",
			wantStatus:  "",
		},
		{
			name:        "frontmatter with only target omits issue and status",
			task:        "---\ntarget: repo:~/nanika/skills/orchestrator\n---\n# Mission\n",
			wantIssueID: "",
			wantTarget:  "repo:~/nanika/skills/orchestrator",
			wantStatus:  "",
		},
		{
			name:        "frontmatter with only issue omits target and status",
			task:        "---\nlinear_issue_id: V-73\n---\n# Mission\n",
			wantIssueID: "V-73",
			wantTarget:  "",
			wantStatus:  "",
		},
		{
			name:        "frontmatter with only status omits issue and target",
			task:        "---\nstatus: draft\n---\n# Mission\n",
			wantIssueID: "",
			wantTarget:  "",
			wantStatus:  "draft",
		},
		{
			name:        "all linkage fields present",
			task:        "---\nlinear_issue_id: V-23\ntarget: repo:~/nanika/skills/orchestrator\nstatus: active\n---\n# Mission\n",
			wantIssueID: "V-23",
			wantTarget:  "repo:~/nanika/skills/orchestrator",
			wantStatus:  "active",
		},
		{
			name:        "single-quoted values unquoted",
			task:        "---\nlinear_issue_id: 'V-42'\nstatus: 'active'\n---\n# Mission\n",
			wantIssueID: "V-42",
			wantTarget:  "",
			wantStatus:  "active",
		},
		{
			name:        "unknown fields silently ignored",
			task:        "---\ntitle: My Mission\nlinear_issue_id: V-10\npriority: high\n---\n# Mission\n",
			wantIssueID: "V-10",
			wantTarget:  "",
			wantStatus:  "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			fm := parseMissionFrontmatter(tt.task)
			if fm.LinearIssueID != tt.wantIssueID {
				t.Errorf("LinearIssueID = %q; want %q", fm.LinearIssueID, tt.wantIssueID)
			}
			if fm.Target != tt.wantTarget {
				t.Errorf("Target = %q; want %q", fm.Target, tt.wantTarget)
			}
			if fm.Status != tt.wantStatus {
				t.Errorf("Status = %q; want %q", fm.Status, tt.wantStatus)
			}
		})
	}
}
