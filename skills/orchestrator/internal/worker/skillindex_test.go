package worker

import (
	"testing"
)

func TestParseSkillNames(t *testing.T) {
	index := `[Nanika Skills Index][root: .claude/skills]IMPORTANT: Prefer retrieval-led reasoning.

|contentkit — Generates code screenshots:{.claude/skills/contentkit/SKILL.md}|` + "`contentkit ray main.go`" + `|
|obsidian — Reads vault notes:{.claude/skills/obsidian/SKILL.md}|` + "`obsidian read \"note\"`" + `|
|missions — Executes tasks:{.claude/skills/missions/SKILL.md}|` + "`orchestrator run \"task\"`" + `|`

	names := ParseSkillNames(index)

	want := []string{"contentkit", "obsidian", "missions"}
	if len(names) != len(want) {
		t.Fatalf("ParseSkillNames() returned %d names, want %d: %v", len(names), len(want), names)
	}
	for i, got := range names {
		if got != want[i] {
			t.Errorf("name[%d] = %q, want %q", i, got, want[i])
		}
	}
}

func TestParseSkillNames_Empty(t *testing.T) {
	names := ParseSkillNames("")
	if len(names) != 0 {
		t.Errorf("ParseSkillNames(\"\") = %v, want empty", names)
	}
}

