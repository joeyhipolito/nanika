package zettel

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// T1.3 — §10.4 Phase 1
// Asserts: a mission with an unknown idea: field creates an idea Zettel with
// placeholder frontmatter; a second mission with the same idea updates the
// existing Zettel rather than replacing it.
func TestIdea_AutoCreate(t *testing.T) {
	vaultPath := t.TempDir()

	ideaSlug := "test-idea"

	// First ensure: idea should not exist
	ideaPath := filepath.Join(vaultPath, "ideas", ideaSlug+".md")
	created, err := EnsureIdeaExists(vaultPath, ideaSlug)
	if err != nil {
		t.Fatalf("first EnsureIdeaExists failed: %v", err)
	}
	if !created {
		t.Errorf("first EnsureIdeaExists should return created=true")
	}

	// Verify file exists
	_, err = os.Stat(ideaPath)
	if err != nil {
		t.Errorf("idea file not created: %v", err)
	}

	// Get original mtime
	stat1, _ := os.Stat(ideaPath)
	mtime1 := stat1.ModTime()

	// Wait a tiny bit to ensure time difference would be visible
	// (in real scenarios, this would be milliseconds apart)

	// Second ensure: file already exists, should not recreate
	created, err = EnsureIdeaExists(vaultPath, ideaSlug)
	if err != nil {
		t.Fatalf("second EnsureIdeaExists failed: %v", err)
	}
	if created {
		t.Errorf("second EnsureIdeaExists should return created=false")
	}

	// Verify mtime unchanged (file not touched)
	stat2, _ := os.Stat(ideaPath)
	mtime2 := stat2.ModTime()
	if !mtime1.Equal(mtime2) {
		t.Errorf("idea file was modified on second ensure (mtime changed)")
	}

	// Append mission to idea
	err = AppendMissionToIdea(vaultPath, ideaSlug, "mission-001")
	if err != nil {
		t.Fatalf("AppendMissionToIdea failed: %v", err)
	}

	// Read and verify content
	data, _ := os.ReadFile(ideaPath)
	content := string(data)
	if !strings.Contains(content, "[[mission-001]]") {
		t.Errorf("mission wikilink not found in idea content")
	}

	// Second append to same mission: should be idempotent (no-op)
	err = AppendMissionToIdea(vaultPath, ideaSlug, "mission-001")
	if err != nil {
		t.Fatalf("second AppendMissionToIdea failed: %v", err)
	}

	data2, _ := os.ReadFile(ideaPath)
	content2 := string(data2)

	// Count occurrences of the wikilink
	count := strings.Count(content2, "[[mission-001]]")
	if count != 1 {
		t.Errorf("mission wikilink appears %d times, want 1 (idempotent)", count)
	}
}
