package routing

import (
	"context"
	"fmt"
)

// DefaultSeeds returns seed profiles for all known Nanika system repositories.
// Each profile encodes stable facts (language, runtime, preferred personas) that
// bias persona selection for recurring tasks on that target without requiring
// observed routing history first.
func DefaultSeeds() []TargetProfile {
	return []TargetProfile{
		{
			TargetID:     "repo:~/nanika/skills/orchestrator",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
				"security-auditor",
			},
			Notes: "Nanika orchestrator CLI — Go stdlib, SQLite, cobra, decompose/routing pipeline",
		},
		{
			TargetID:     "repo:~/nanika/skills/scout",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
			},
			Notes: "Nanika scout CLI — Go, web scraping, RSS/Hacker News/devto/Lobsters intel gathering",
		},
		{
			TargetID:     "repo:~/nanika/skills/obsidian",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
			},
			Notes: "Nanika obsidian CLI — Go, Obsidian vault note management, capture and triage",
		},
		{
			TargetID:     "repo:~/nanika/skills/gmail",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
				"security-auditor",
			},
			Notes: "Nanika gmail CLI — Go, Gmail API, OAuth2, multi-account inbox management",
		},
		{
			TargetID:     "repo:~/nanika/skills/todoist",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
			},
			Notes: "Nanika todoist CLI — Go, Todoist REST API, task and project management",
		},
		{
			TargetID:     "repo:~/nanika/skills/ynab",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
			},
			Notes: "Nanika ynab CLI — Go, YNAB REST API, budget and transaction tracking",
		},
		{
			TargetID:     "repo:~/nanika/skills/linkedin",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
				"security-auditor",
			},
			Notes: "Nanika linkedin CLI — Go, LinkedIn OAuth2 API, post and feed management",
		},
		{
			TargetID:     "repo:~/nanika/skills/reddit",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
			},
			Notes: "Nanika reddit CLI — Go, Reddit OAuth2 API, feed reading and post/comment management",
		},
		{
			TargetID:     "repo:~/nanika/skills/substack",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
			},
			Notes: "Nanika substack CLI — Go, Substack web automation, draft and publish management",
		},
		{
			TargetID:     "repo:~/nanika/skills/scheduler",
			TargetType:   TargetTypeRepo,
			Language:     "go",
			Runtime:      "go",
			TestCommand:  "go test ./...",
			BuildCommand: "make build",
			Framework:    "cobra",
			KeyDirectories: []string{"cmd", "internal"},
			PreferredPersonas: []string{
				"senior-backend-engineer",
				"staff-code-reviewer",
			},
			Notes: "Nanika scheduler CLI — Go, cron-based social content scheduling across platforms",
		},
		// Non-repo Nanika targets: resolved from task-text signals that are distinct
		// from CLI tool names. These accumulate routing memory for recurring
		// strategy and publication tasks that have no associated git repository.
		{
			TargetID:   "system:via",
			TargetType: TargetTypeViaSystem,
			PreferredPersonas: []string{
				"architect",
				"academic-researcher",
			},
			Notes: "Nanika orchestration system — strategy, workflow design, decomposition analysis, agent meta-tasks",
		},
		{
			TargetID:   "publication:substack",
			TargetType: TargetTypePublication,
			PreferredPersonas: []string{
				"technical-writer",
				"academic-researcher",
			},
			Notes: "Substack newsletter publication — technical explainers, article drafting, newsletter planning",
		},
	}
}

// Seed upserts the given profiles into the routing database. Existing profiles
// for the same target_id are overwritten (UpsertTargetProfile semantics).
// Returns the number of profiles successfully seeded and the first error
// encountered, continuing past individual failures so partial progress is kept.
func Seed(ctx context.Context, rdb *RoutingDB, profiles []TargetProfile) (int, error) {
	if len(profiles) == 0 {
		return 0, nil
	}

	var firstErr error
	seeded := 0
	for _, p := range profiles {
		if p.TargetID == "" {
			err := fmt.Errorf("seed profile missing target_id")
			if firstErr == nil {
				firstErr = err
			}
			continue
		}

		if err := rdb.UpsertTargetProfile(ctx, p); err != nil {
			if firstErr == nil {
				firstErr = fmt.Errorf("seeding %q: %w", p.TargetID, err)
			}
			continue
		}
		seeded++
	}
	return seeded, firstErr
}
