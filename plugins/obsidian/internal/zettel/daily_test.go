package zettel

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// T1.4 — §10.4 Phase 1
// Asserts: three missions completing in sequence produce a daily note whose
// ## Missions section lists them in completion order with resolving wikilinks.
func TestDailyNote_AppendOrder(t *testing.T) {
	vaultPath := t.TempDir()

	referenceDate := time.Date(2026, 4, 20, 12, 0, 0, 0, time.UTC)

	// Append three missions in order
	err := AppendMissionToDaily(vaultPath, referenceDate, "mission-001")
	if err != nil {
		t.Fatalf("first AppendMissionToDaily failed: %v", err)
	}

	err = AppendMissionToDaily(vaultPath, referenceDate, "mission-002")
	if err != nil {
		t.Fatalf("second AppendMissionToDaily failed: %v", err)
	}

	err = AppendMissionToDaily(vaultPath, referenceDate, "mission-003")
	if err != nil {
		t.Fatalf("third AppendMissionToDaily failed: %v", err)
	}

	// Read the daily note
	dailyPath := filepath.Join(vaultPath, "daily", "2026-04-20.md")
	data, err := os.ReadFile(dailyPath)
	if err != nil {
		t.Fatalf("cannot read daily note: %v", err)
	}
	content := string(data)

	// Verify all three wikilinks are present
	for _, mission := range []string{"mission-001", "mission-002", "mission-003"} {
		if !strings.Contains(content, "[["+mission+"]]") {
			t.Errorf("wikilink %q not found in daily note", mission)
		}
	}

	// Verify ordering: mission-001 should appear before mission-002 before mission-003
	idx1 := strings.Index(content, "[[mission-001]]")
	idx2 := strings.Index(content, "[[mission-002]]")
	idx3 := strings.Index(content, "[[mission-003]]")

	if idx1 < 0 || idx2 < 0 || idx3 < 0 {
		t.Errorf("some missions not found")
	} else if !(idx1 < idx2 && idx2 < idx3) {
		t.Errorf("missions not in correct order: idx1=%d, idx2=%d, idx3=%d", idx1, idx2, idx3)
	}

	// Append again to same day: should append, not overwrite
	err = AppendMissionToDaily(vaultPath, referenceDate, "mission-004")
	if err != nil {
		t.Fatalf("fourth AppendMissionToDaily failed: %v", err)
	}

	data2, _ := os.ReadFile(dailyPath)
	content2 := string(data2)

	// Verify all four missions are still there and original three are unchanged
	for _, mission := range []string{"mission-001", "mission-002", "mission-003", "mission-004"} {
		if !strings.Contains(content2, "[["+mission+"]]") {
			t.Errorf("wikilink %q not found after second write", mission)
		}
	}

	// Verify original order is preserved
	idx1b := strings.Index(content2, "[[mission-001]]")
	idx2b := strings.Index(content2, "[[mission-002]]")
	idx3b := strings.Index(content2, "[[mission-003]]")
	idx4b := strings.Index(content2, "[[mission-004]]")

	if !(idx1b < idx2b && idx2b < idx3b && idx3b < idx4b) {
		t.Errorf("missions not in correct order after append")
	}
}
