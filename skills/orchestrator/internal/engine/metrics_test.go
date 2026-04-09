package engine

import (
	"testing"

	"github.com/joeyhipolito/orchestrator-cli/internal/core"
)

func TestParseSkillInvocations(t *testing.T) {
	tests := []struct {
		name   string
		output string
		want   []string
	}{
		{
			name:   "empty output",
			output: "",
			want:   nil,
		},
		{
			name:   "no Bash tool calls",
			output: "Some text output\nNo CLI invocations here\n",
			want:   nil,
		},
		{
			name: "Claude Code Bash indicator format",
			output: `⏺ Bash(orchestrator status)
  ⎿  Running.
⏺ Bash(orchestrator metrics --last 5)
  ⎿  Done.
`,
			want: []string{"orchestrator", "orchestrator"},
		},
		{
			name: "Bash() format without indicator",
			output: `Running Bash(orchestrator run "do something")
Then Bash(orchestrator status)
`,
			want: []string{"orchestrator", "orchestrator"},
		},
		{
			name: "shell prompt format",
			output: `$ orchestrator status
output: ok
$ orchestrator metrics
output: ok
`,
			want: []string{"orchestrator", "orchestrator"},
		},
		{
			name: "worker tool summary format",
			output: `[tool: Bash orchestrator status]
[tool: Bash orchestrator metrics]
`,
			want: []string{"orchestrator", "orchestrator"},
		},
		{
			name: "duplicate invocations preserved",
			output: `⏺ Bash(orchestrator status)
⏺ Bash(orchestrator status)
⏺ Bash(orchestrator metrics)
`,
			want: []string{"orchestrator", "orchestrator", "orchestrator"},
		},
		{
			name: "unknown CLI tools ignored",
			output: `⏺ Bash(grep -r "pattern" .)
⏺ Bash(go test ./...)
$ ls -la
`,
			want: nil,
		},
		{
			name: "all known CLI skills recognised",
			output: `⏺ Bash(orchestrator status)
⏺ Bash(go test ./...)
⏺ Bash(cat README.md)
`,
			want: []string{"orchestrator"},
		},
		{
			name: "mixed known and unknown commands",
			output: `⏺ Bash(go build -o bin/orchestrator .)
⏺ Bash(orchestrator run "task")
⏺ Bash(cat README.md)
`,
			want: []string{"orchestrator"},
		},
		{
			name: "quoted command string in Bash()",
			output: `⏺ Bash("orchestrator run \"task\"")
`,
			want: []string{"orchestrator"},
		},
		{
			name: "nested parens in Bash are handled",
			output: `⏺ Bash(orchestrator run "$(date)")
`,
			want: []string{"orchestrator"},
		},
		{
			name: "blockquote lines are ignored",
			output: `> scout the area for relevant files
> publish your findings below
`,
			want: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ParseSkillInvocations(tt.output)
			if len(got) != len(tt.want) {
				t.Fatalf("ParseSkillInvocations() = %v; want %v", got, tt.want)
			}
			for i := range got {
				if got[i] != tt.want[i] {
					t.Errorf("ParseSkillInvocations()[%d] = %q; want %q", i, got[i], tt.want[i])
				}
			}
		})
	}
}

func TestBashCommandFromLine(t *testing.T) {
	tests := []struct {
		name string
		line string
		want string
	}{
		{
			name: "Claude Code indicator with Bash()",
			line: "⏺ Bash(scout gather \"topic\")",
			want: `scout gather "topic"`,
		},
		{
			name: "bare Bash()",
			line: "Bash(gmail inbox --unread)",
			want: "gmail inbox --unread",
		},
		{
			name: "dollar prompt",
			line: "$ obsidian triage --auto",
			want: "obsidian triage --auto",
		},
		{
			name: "worker tool summary",
			line: `[tool: Bash scout gather "topic"]`,
			want: `scout gather "topic"`,
		},
		{
			name: "leading whitespace stripped",
			line: "   ⏺ Bash(todoist list)",
			want: "todoist list",
		},
		{
			name: "unrelated line",
			line: "Some output text here",
			want: "",
		},
		{
			name: "empty line",
			line: "",
			want: "",
		},
		{
			name: "Bash with no closing paren",
			line: `⏺ Bash(scout gather "topic"`,
			want: "",
		},
		{
			name: "Bash with nested parens",
			line: `⏺ Bash(scout gather "$(date)")`,
			want: `scout gather "$(date)"`,
		},
		{
			name: "blockquote prefix ignored",
			line: "> scout the area for relevant files",
			want: "",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := bashCommandFromLine(tt.line)
			if got != tt.want {
				t.Errorf("bashCommandFromLine(%q) = %q; want %q", tt.line, got, tt.want)
			}
		})
	}
}

func TestPlanSnapshotSucceeded(t *testing.T) {
	tests := []struct {
		name string
		plan *core.Plan
		want bool
	}{
		{
			name: "all phases completed or skipped",
			plan: &core.Plan{Phases: []*core.Phase{
				{Status: core.StatusCompleted},
				{Status: core.StatusSkipped},
			}},
			want: true,
		},
		{
			name: "pending phase keeps snapshot incomplete",
			plan: &core.Plan{Phases: []*core.Phase{
				{Status: core.StatusCompleted},
				{Status: core.StatusPending},
			}},
			want: false,
		},
		{
			name: "failed phase is not a successful snapshot",
			plan: &core.Plan{Phases: []*core.Phase{
				{Status: core.StatusCompleted},
				{Status: core.StatusFailed},
			}},
			want: false,
		},
		{
			name: "empty plan is not a successful snapshot",
			plan: &core.Plan{},
			want: false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if got := planSnapshotSucceeded(tt.plan); got != tt.want {
				t.Fatalf("planSnapshotSucceeded() = %v, want %v", got, tt.want)
			}
		})
	}
}
