package cmd

import "testing"

// ---------------------------------------------------------------------------
// missionSkipsGit — type field detection
// ---------------------------------------------------------------------------

func TestMissionSkipsGit_TypeResearch(t *testing.T) {
	fm := missionFrontmatter{Type: "research"}
	if !missionSkipsGit(fm) {
		t.Error("type=research should skip git")
	}
}

func TestMissionSkipsGit_TypeEvaluation(t *testing.T) {
	fm := missionFrontmatter{Type: "evaluation"}
	if !missionSkipsGit(fm) {
		t.Error("type=evaluation should skip git")
	}
}

func TestMissionSkipsGit_TypeReview(t *testing.T) {
	fm := missionFrontmatter{Type: "review"}
	if !missionSkipsGit(fm) {
		t.Error("type=review should skip git")
	}
}

func TestMissionSkipsGit_TypeCaseInsensitive(t *testing.T) {
	for _, typ := range []string{"Research", "EVALUATION", "Review"} {
		fm := missionFrontmatter{Type: typ}
		if !missionSkipsGit(fm) {
			t.Errorf("type=%q should skip git (case-insensitive)", typ)
		}
	}
}

func TestMissionSkipsGit_TypeDevelopment(t *testing.T) {
	fm := missionFrontmatter{Type: "development"}
	if missionSkipsGit(fm) {
		t.Error("type=development should not skip git")
	}
}

func TestMissionSkipsGit_TypeEmpty(t *testing.T) {
	fm := missionFrontmatter{}
	if missionSkipsGit(fm) {
		t.Error("empty type/domain should not skip git")
	}
}

// ---------------------------------------------------------------------------
// missionSkipsGit — domain field detection
// ---------------------------------------------------------------------------

func TestMissionSkipsGit_DomainDev(t *testing.T) {
	for _, d := range []string{"dev", "development", "engineering", "code", "coding"} {
		fm := missionFrontmatter{Domain: d}
		if missionSkipsGit(fm) {
			t.Errorf("domain=%q should not skip git", d)
		}
	}
}

func TestMissionSkipsGit_DomainDevCaseInsensitive(t *testing.T) {
	for _, d := range []string{"Dev", "DEV", "Engineering", "CODE"} {
		fm := missionFrontmatter{Domain: d}
		if missionSkipsGit(fm) {
			t.Errorf("domain=%q should not skip git (case-insensitive)", d)
		}
	}
}

func TestMissionSkipsGit_DomainPersonal(t *testing.T) {
	fm := missionFrontmatter{Domain: "personal"}
	if !missionSkipsGit(fm) {
		t.Error("domain=personal should skip git")
	}
}

func TestMissionSkipsGit_DomainWork(t *testing.T) {
	fm := missionFrontmatter{Domain: "work"}
	if !missionSkipsGit(fm) {
		t.Error("domain=work should skip git")
	}
}

func TestMissionSkipsGit_DomainCreative(t *testing.T) {
	fm := missionFrontmatter{Domain: "creative"}
	if !missionSkipsGit(fm) {
		t.Error("domain=creative should skip git")
	}
}

func TestMissionSkipsGit_DomainAcademic(t *testing.T) {
	fm := missionFrontmatter{Domain: "academic"}
	if !missionSkipsGit(fm) {
		t.Error("domain=academic should skip git")
	}
}

func TestMissionSkipsGit_DomainEmpty(t *testing.T) {
	fm := missionFrontmatter{Domain: ""}
	if missionSkipsGit(fm) {
		t.Error("empty domain should not skip git")
	}
}

// ---------------------------------------------------------------------------
// parseMissionFrontmatter — type and domain fields
// ---------------------------------------------------------------------------

func TestParseFrontmatter_TypeField(t *testing.T) {
	task := "---\ntype: research\n---\n\nDo some reading."
	fm := parseMissionFrontmatter(task)
	if fm.Type != "research" {
		t.Errorf("Type = %q, want research", fm.Type)
	}
}

func TestParseFrontmatter_DomainField(t *testing.T) {
	task := "---\ndomain: academic\n---\n\nWrite a paper."
	fm := parseMissionFrontmatter(task)
	if fm.Domain != "academic" {
		t.Errorf("Domain = %q, want academic", fm.Domain)
	}
}

func TestParseFrontmatter_TypeAndDomainTogether(t *testing.T) {
	task := "---\ntype: evaluation\ndomain: dev\n---\n\nRun evals."
	fm := parseMissionFrontmatter(task)
	if fm.Type != "evaluation" {
		t.Errorf("Type = %q, want evaluation", fm.Type)
	}
	if fm.Domain != "dev" {
		t.Errorf("Domain = %q, want dev", fm.Domain)
	}
}

func TestParseFrontmatter_TypeQuoted(t *testing.T) {
	task := "---\ntype: \"review\"\n---\n\nReview the PR."
	fm := parseMissionFrontmatter(task)
	if fm.Type != "review" {
		t.Errorf("Type = %q, want review (unquoted)", fm.Type)
	}
}

func TestParseFrontmatter_NoFrontmatter_ZeroValues(t *testing.T) {
	task := "Just do the task.\n"
	fm := parseMissionFrontmatter(task)
	if fm.Type != "" || fm.Domain != "" {
		t.Errorf("expected empty Type/Domain for task without frontmatter, got Type=%q Domain=%q", fm.Type, fm.Domain)
	}
}
