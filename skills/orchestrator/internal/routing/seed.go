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
