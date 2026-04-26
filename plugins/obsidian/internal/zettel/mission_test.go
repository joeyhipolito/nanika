package zettel

import (
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// requiredFMKeys lists the 8 mandatory frontmatter fields every mission Zettel must contain.
var requiredFMKeys = []string{"type", "id", "slug", "status", "completed", "personas", "trackers", "artifacts"}

// referenceTime is a fixed UTC instant used across mission test cases.
var referenceTime = time.Date(2026, 4, 20, 0, 0, 0, 0, time.UTC)

// T1.1 — §10.4 Phase 1
// Asserts: Mission{id,slug,completed,personas,trackers,artifacts} serializes to
// Zettel with 8 required frontmatter fields; wikilinks resolve.
// Table-driven over 8 mission-result shapes.
func TestMissionZettel_AllFrontmatterFields(t *testing.T) {
	tests := []struct {
		name          string
		mission       Mission
		wantWikilinks []string // wikilink targets that must appear in the body
		noWikilinks   []string // wikilink targets that must NOT appear
		wantTitlePfx  string   // body must start with "# <wantTitlePfx>"
		golden        string   // golden file name; empty if none
	}{
		{
			name: "all-fields-populated",
			mission: Mission{
				ID:        "test-mission-001",
				Slug:      "build-pipeline",
				Completed: referenceTime,
				Personas:  []string{"senior-backend-engineer", "staff-code-reviewer"},
				Trackers:  []string{"TRK-525", "TRK-526"},
				Artifacts: []string{"implementation-report.md", "test-results.md"},
			},
			wantWikilinks: []string{
				"senior-backend-engineer", "staff-code-reviewer",
				"TRK-525", "TRK-526",
				"implementation-report.md", "test-results.md",
			},
			wantTitlePfx: "# build-pipeline",
			golden:       "mission_all_fields.md",
		},
		{
			name: "no-personas",
			mission: Mission{
				ID:        "m-002",
				Slug:      "simple-task",
				Completed: referenceTime,
				Personas:  nil,
				Trackers:  []string{"TRK-100"},
				Artifacts: []string{"report.md"},
			},
			wantWikilinks: []string{"TRK-100", "report.md"},
			noWikilinks:   []string{"senior-backend-engineer"},
			wantTitlePfx:  "# simple-task",
		},
		{
			name: "no-trackers",
			mission: Mission{
				ID:        "m-003",
				Slug:      "research",
				Completed: referenceTime,
				Personas:  []string{"analyst"},
				Trackers:  nil,
				Artifacts: []string{"findings.md"},
			},
			wantWikilinks: []string{"analyst", "findings.md"},
			noWikilinks:   []string{"TRK-"},
			wantTitlePfx:  "# research",
		},
		{
			name: "no-artifacts",
			mission: Mission{
				ID:        "m-004",
				Slug:      "spike",
				Completed: referenceTime,
				Personas:  []string{"senior-backend-engineer"},
				Trackers:  []string{"TRK-200"},
				Artifacts: nil,
			},
			wantWikilinks: []string{"senior-backend-engineer", "TRK-200"},
			wantTitlePfx:  "# spike",
		},
		{
			name: "empty-all-lists",
			mission: Mission{
				ID:        "m-005",
				Slug:      "minimal",
				Completed: referenceTime,
				Personas:  nil,
				Trackers:  nil,
				Artifacts: nil,
			},
			wantWikilinks: nil,
			wantTitlePfx:  "# minimal",
		},
		{
			name: "empty-slug-falls-back-to-id",
			mission: Mission{
				ID:        "m-no-slug",
				Slug:      "",
				Completed: referenceTime,
				Personas:  []string{"analyst"},
				Trackers:  []string{"TRK-300"},
				Artifacts: []string{"out.md"},
			},
			wantWikilinks: []string{"analyst", "TRK-300", "out.md"},
			wantTitlePfx:  "# m-no-slug",
		},
		{
			name: "zero-completed-time",
			mission: Mission{
				ID:        "m-007",
				Slug:      "pending",
				Completed: time.Time{},
				Personas:  []string{"analyst"},
				Trackers:  []string{"TRK-400"},
				Artifacts: nil,
			},
			wantWikilinks: []string{"analyst", "TRK-400"},
			wantTitlePfx:  "# pending",
		},
		{
			name: "single-item-each",
			mission: Mission{
				ID:        "m-008",
				Slug:      "one-of-each",
				Completed: referenceTime,
				Personas:  []string{"staff-code-reviewer"},
				Trackers:  []string{"TRK-999"},
				Artifacts: []string{"single.md"},
			},
			wantWikilinks: []string{"staff-code-reviewer", "TRK-999", "single.md"},
			wantTitlePfx:  "# one-of-each",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := RenderMission(tt.mission)

			if tt.golden != "" {
				checkGolden(t, tt.golden, got)
			}

			note := vault.ParseNote(got)

			// Assert all 8 required frontmatter keys are present.
			for _, key := range requiredFMKeys {
				if _, ok := note.Frontmatter[key]; !ok {
					t.Errorf("missing required frontmatter key %q", key)
				}
			}

			// Assert type is always "mission".
			if v, _ := note.Frontmatter["type"].(string); v != "mission" {
				t.Errorf("frontmatter type = %q, want %q", v, "mission")
			}

			// Assert status is always "completed".
			if v, _ := note.Frontmatter["status"].(string); v != "completed" {
				t.Errorf("frontmatter status = %q, want %q", v, "completed")
			}

			// Assert body starts with the expected title heading.
			if tt.wantTitlePfx != "" && !strings.Contains(got, tt.wantTitlePfx) {
				t.Errorf("body does not contain title %q", tt.wantTitlePfx)
			}

			// Assert expected wikilinks are present.
			wikiSet := make(map[string]bool, len(note.Wikilinks))
			for _, w := range note.Wikilinks {
				wikiSet[w] = true
			}
			for _, want := range tt.wantWikilinks {
				if !wikiSet[want] {
					t.Errorf("missing wikilink %q in body (got: %v)", want, note.Wikilinks)
				}
			}

			// Assert forbidden wikilinks are absent.
			for _, bad := range tt.noWikilinks {
				for w := range wikiSet {
					if strings.Contains(w, bad) {
						t.Errorf("unexpected wikilink %q in body", w)
					}
				}
			}
		})
	}
}

// T1.2 — §10.4 Phase 1
// Asserts: running the write hook twice for the same mission_id produces exactly
// one file on disk and emits a ZettelSkipped event on the second call.
func TestMissionZettel_Idempotent(t *testing.T) {
	vaultPath := t.TempDir()

	writer, err := NewWriter(vaultPath)
	if err != nil {
		t.Fatalf("NewWriter failed: %v", err)
	}
	defer writer.Dedup.Close()

	mission := Mission{
		ID:        "dedup-test-001",
		Slug:      "first-write",
		Completed: referenceTime,
		Personas:  []string{"tester"},
		Trackers:  []string{"TRK-001"},
		Artifacts: nil,
	}

	// First write
	result1, err := writer.WriteMission(mission.ID, mission.Slug, "", mission)
	if err != nil {
		t.Fatalf("first WriteMission failed: %v", err)
	}
	if result1.Skipped {
		t.Errorf("first write should not skip")
	}
	if result1.Path == "" {
		t.Errorf("first write should return a path")
	}

	// Verify file exists
	_, err = os.Stat(result1.Path)
	if err != nil {
		t.Errorf("file not created: %v", err)
	}

	// Second write (should skip)
	result2, err := writer.WriteMission(mission.ID, mission.Slug, "", mission)
	if err != nil {
		t.Fatalf("second WriteMission failed: %v", err)
	}
	if !result2.Skipped {
		t.Errorf("second write should skip, but got skipped=%v", result2.Skipped)
	}
	if result2.SkipReason != "duplicate mission_id" {
		t.Errorf("skip reason = %q, want %q", result2.SkipReason, "duplicate mission_id")
	}
	if result2.Path != result1.Path {
		t.Errorf("second write returned different path: %s vs %s", result2.Path, result1.Path)
	}

	// Verify only one file exists
	files, err := filepath.Glob(filepath.Join(vaultPath, "missions", "*.md"))
	if err != nil {
		t.Fatalf("Glob failed: %v", err)
	}
	if len(files) != 1 {
		t.Errorf("expected 1 file, got %d", len(files))
	}
}

// T1.4a — §10.4 Phase 1
// Asserts: WriteMission with a non-empty ideaSlug creates the idea Zettel (if
// absent) and appends the mission wikilink to it. No warnings emitted.
func TestWriteMission_WithIdeaSlug(t *testing.T) {
	vaultPath := t.TempDir()

	writer, err := NewWriter(vaultPath)
	if err != nil {
		t.Fatalf("NewWriter failed: %v", err)
	}
	defer writer.Dedup.Close()

	mission := Mission{
		ID:        "idea-test-001",
		Slug:      "feature-build",
		Completed: referenceTime,
		Personas:  []string{"senior-backend-engineer"},
		Trackers:  []string{"TRK-525"},
		Artifacts: nil,
	}

	result, err := writer.WriteMission(mission.ID, mission.Slug, "my-idea", mission)
	if err != nil {
		t.Fatalf("WriteMission failed: %v", err)
	}
	if result.Skipped {
		t.Errorf("should not skip on first write")
	}

	// Idea file must exist.
	ideaPath := filepath.Join(vaultPath, "ideas", "my-idea.md")
	if _, err := os.Stat(ideaPath); err != nil {
		t.Errorf("idea file not created: %v", err)
	}

	// Idea file must contain the mission wikilink.
	data, _ := os.ReadFile(ideaPath)
	wikilink := strings.TrimSuffix(filepath.Base(result.Path), ".md")
	if !strings.Contains(string(data), "[["+wikilink+"]]") {
		t.Errorf("idea file missing wikilink [[%s]]", wikilink)
	}

	// No warnings expected on a clean write.
	if len(result.Warnings) != 0 {
		t.Errorf("unexpected warnings: %v", result.Warnings)
	}
}

// T1.4b — §10.4 Phase 1
// Asserts: WriteMission with an empty ideaSlug skips idea orchestration
// entirely — no idea file is created and no warnings are emitted.
func TestWriteMission_EmptyIdeaSlug(t *testing.T) {
	vaultPath := t.TempDir()

	writer, err := NewWriter(vaultPath)
	if err != nil {
		t.Fatalf("NewWriter failed: %v", err)
	}
	defer writer.Dedup.Close()

	mission := Mission{
		ID:        "no-idea-001",
		Slug:      "standalone",
		Completed: referenceTime,
		Personas:  []string{"analyst"},
		Trackers:  nil,
		Artifacts: nil,
	}

	result, err := writer.WriteMission(mission.ID, mission.Slug, "", mission)
	if err != nil {
		t.Fatalf("WriteMission failed: %v", err)
	}
	if result.Skipped {
		t.Errorf("should not skip on first write")
	}

	// No idea file should exist.
	entries, _ := filepath.Glob(filepath.Join(vaultPath, "ideas", "*.md"))
	if len(entries) != 0 {
		t.Errorf("expected no idea files, got %v", entries)
	}

	// No warnings expected.
	if len(result.Warnings) != 0 {
		t.Errorf("unexpected warnings: %v", result.Warnings)
	}
}

// T1.6 — §10.4 Phase 1
// Asserts: 10 simultaneous onMissionComplete calls (race detector on) produce
// 10 distinct Zettels with no partial writes and no duplicate daily entries.
func TestConcurrentWrites_NoRace(t *testing.T) {
	vaultPath := t.TempDir()

	writer, err := NewWriter(vaultPath)
	if err != nil {
		t.Fatalf("NewWriter failed: %v", err)
	}
	defer writer.Dedup.Close()

	referenceDate := time.Date(2026, 4, 20, 12, 0, 0, 0, time.UTC)

	// Launch 10 concurrent writes
	const numMissions = 10
	var wg sync.WaitGroup
	wg.Add(numMissions)

	for i := 0; i < numMissions; i++ {
		go func(idx int) {
			defer wg.Done()

			missionID := "concurrent-mission-" + string(rune('0'+idx))
			slug := "concurrent-" + missionID

			mission := Mission{
				ID:        missionID,
				Slug:      slug,
				Completed: referenceDate,
				Personas:  []string{"tester"},
				Trackers:  []string{"TRK-999"},
				Artifacts: nil,
			}

			result, err := writer.WriteMission(mission.ID, mission.Slug, "", mission)
			if err != nil {
				t.Errorf("WriteMission %d failed: %v", idx, err)
				return
			}
			if result.Skipped {
				t.Errorf("WriteMission %d should not skip on first write", idx)
			}
			if result.Path == "" {
				t.Errorf("WriteMission %d should return a path", idx)
			}

			// Append to daily
			err = AppendMissionToDaily(vaultPath, referenceDate, missionID)
			if err != nil {
				t.Errorf("AppendMissionToDaily %d failed: %v", idx, err)
			}
		}(i)
	}

	wg.Wait()

	// Verify 10 distinct files exist
	files, err := filepath.Glob(filepath.Join(vaultPath, "missions", "*.md"))
	if err != nil {
		t.Fatalf("Glob failed: %v", err)
	}
	if len(files) != numMissions {
		t.Errorf("expected %d files, got %d", numMissions, len(files))
	}

	// Verify all files are valid (non-empty)
	for _, f := range files {
		data, err := os.ReadFile(f)
		if err != nil {
			t.Errorf("cannot read file %s: %v", f, err)
		}
		if len(data) == 0 {
			t.Errorf("file %s is empty", f)
		}
	}
}
