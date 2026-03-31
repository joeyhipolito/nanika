package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// ─── tagJaccard ──────────────────────────────────────────────────────────────

func TestTagJaccard_IdenticalSets(t *testing.T) {
	got := tagJaccard([]string{"go", "cli"}, []string{"go", "cli"})
	if got != 1.0 {
		t.Errorf("identical sets: got %.4f, want 1.0", got)
	}
}

func TestTagJaccard_NoOverlap(t *testing.T) {
	got := tagJaccard([]string{"go"}, []string{"rust"})
	if got != 0.0 {
		t.Errorf("disjoint sets: got %.4f, want 0.0", got)
	}
}

func TestTagJaccard_PartialOverlap(t *testing.T) {
	// {go} ∩ {go, cli} = {go}, union = {go, cli} → 1/2 = 0.5
	got := tagJaccard([]string{"go"}, []string{"go", "cli"})
	const want = 0.5
	if got != want {
		t.Errorf("partial overlap: got %.4f, want %.4f", got, want)
	}
}

func TestTagJaccard_BothEmpty(t *testing.T) {
	got := tagJaccard(nil, nil)
	if got != 0.0 {
		t.Errorf("both empty: got %.4f, want 0.0", got)
	}
}

func TestTagJaccard_OneEmpty(t *testing.T) {
	got := tagJaccard([]string{"go"}, nil)
	if got != 0.0 {
		t.Errorf("one empty: got %.4f, want 0.0", got)
	}
}

func TestTagJaccard_CaseInsensitive(t *testing.T) {
	got := tagJaccard([]string{"Go"}, []string{"go"})
	if got != 1.0 {
		t.Errorf("case insensitive: got %.4f, want 1.0", got)
	}
}

// ─── findCommonTags ──────────────────────────────────────────────────────────

func TestFindCommonTags_AllShare(t *testing.T) {
	notes := []*promoteNoteInfo{
		{Tags: []string{"go", "cli"}},
		{Tags: []string{"go", "cli", "tools"}},
		{Tags: []string{"go", "cli"}},
	}
	got := findCommonTags(notes)
	want := []string{"cli", "go"} // sorted
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Errorf("findCommonTags() = %v, want %v", got, want)
	}
}

func TestFindCommonTags_NoneShared(t *testing.T) {
	notes := []*promoteNoteInfo{
		{Tags: []string{"go"}},
		{Tags: []string{"rust"}},
		{Tags: []string{"python"}},
	}
	got := findCommonTags(notes)
	if len(got) != 0 {
		t.Errorf("findCommonTags() = %v, want empty", got)
	}
}

func TestFindCommonTags_Empty(t *testing.T) {
	got := findCommonTags(nil)
	if got != nil {
		t.Errorf("findCommonTags(nil) = %v, want nil", got)
	}
}

// ─── mergeUniqueTags ─────────────────────────────────────────────────────────

func TestMergeUniqueTags_DeduplicatesAndSorts(t *testing.T) {
	notes := []*promoteNoteInfo{
		{Tags: []string{"go", "cli"}},
		{Tags: []string{"cli", "tools"}},
		{Tags: []string{"go", "tools"}},
	}
	got := mergeUniqueTags(notes)
	want := []string{"cli", "go", "tools"}
	if strings.Join(got, ",") != strings.Join(want, ",") {
		t.Errorf("mergeUniqueTags() = %v, want %v", got, want)
	}
}

func TestMergeUniqueTags_Empty(t *testing.T) {
	got := mergeUniqueTags(nil)
	if len(got) != 0 {
		t.Errorf("mergeUniqueTags(nil) = %v, want empty", got)
	}
}

// ─── deriveClusterTitle ──────────────────────────────────────────────────────

func TestDeriveClusterTitle_MostCommonTag(t *testing.T) {
	notes := []*promoteNoteInfo{
		{Tags: []string{"go", "cli"}, Title: "Note A"},
		{Tags: []string{"go", "search"}, Title: "Note B"},
		{Tags: []string{"go"}, Title: "Note C"},
	}
	// "go" appears 3 times — should win.
	got := deriveClusterTitle(notes)
	if got != "Go" {
		t.Errorf("deriveClusterTitle() = %q, want %q", got, "Go")
	}
}

func TestDeriveClusterTitle_FallsBackToFirstTitle(t *testing.T) {
	notes := []*promoteNoteInfo{
		{Tags: nil, Title: "First Note"},
		{Tags: nil, Title: "Second Note"},
		{Tags: nil, Title: "Third Note"},
	}
	got := deriveClusterTitle(notes)
	if got != "First Note" {
		t.Errorf("deriveClusterTitle() = %q, want %q", got, "First Note")
	}
}

func TestDeriveClusterTitle_DefaultWhenNoTagsOrTitle(t *testing.T) {
	notes := []*promoteNoteInfo{
		{Tags: nil, Title: ""},
	}
	got := deriveClusterTitle(notes)
	if got != "Promoted Cluster" {
		t.Errorf("deriveClusterTitle() = %q, want %q", got, "Promoted Cluster")
	}
}

// ─── detectClusters ──────────────────────────────────────────────────────────

func TestDetectClusters_TagBased(t *testing.T) {
	// Three notes sharing the "go" tag should form one cluster.
	notes := []*promoteNoteInfo{
		{Path: "a.md", Tags: []string{"go"}},
		{Path: "b.md", Tags: []string{"go"}},
		{Path: "c.md", Tags: []string{"go"}},
		{Path: "d.md", Tags: []string{"rust"}}, // unrelated — should stay out
	}
	clusters, noteGroups := detectClusters(notes, 0.25, 0.80, 3)
	if len(clusters) != 1 {
		t.Fatalf("detectClusters() found %d clusters, want 1", len(clusters))
	}
	if len(clusters[0].Notes) != 3 {
		t.Errorf("cluster has %d notes, want 3", len(clusters[0].Notes))
	}
	if len(noteGroups) != 1 {
		t.Errorf("noteGroups len = %d, want 1", len(noteGroups))
	}
}

func TestDetectClusters_BelowMinSize(t *testing.T) {
	// Only two notes share a tag — should not form a cluster (min=3).
	notes := []*promoteNoteInfo{
		{Path: "a.md", Tags: []string{"go"}},
		{Path: "b.md", Tags: []string{"go"}},
		{Path: "c.md", Tags: []string{"rust"}},
	}
	clusters, _ := detectClusters(notes, 0.25, 0.80, 3)
	if len(clusters) != 0 {
		t.Errorf("detectClusters() found %d clusters, want 0 (below min size)", len(clusters))
	}
}

func TestDetectClusters_EmptyInput(t *testing.T) {
	clusters, groups := detectClusters(nil, 0.25, 0.80, 3)
	if len(clusters) != 0 || len(groups) != 0 {
		t.Errorf("expected empty result for nil input, got clusters=%d groups=%d", len(clusters), len(groups))
	}
}

func TestDetectClusters_SemanticBased(t *testing.T) {
	// Three notes with identical embeddings but no tags — cluster via semantics.
	vec := []float32{1, 0, 0}
	notes := []*promoteNoteInfo{
		{Path: "a.md", Embedding: vec},
		{Path: "b.md", Embedding: vec},
		{Path: "c.md", Embedding: vec},
		// Orthogonal note — should not be included.
		{Path: "d.md", Embedding: []float32{0, 1, 0}},
	}
	clusters, _ := detectClusters(notes, 0.25, 0.80, 3)
	if len(clusters) != 1 {
		t.Fatalf("detectClusters() found %d clusters, want 1", len(clusters))
	}
	if len(clusters[0].Notes) != 3 {
		t.Errorf("cluster has %d notes, want 3", len(clusters[0].Notes))
	}
}

func TestDetectClusters_MultipleClusters(t *testing.T) {
	// Two independent clusters of 3 notes each.
	notes := []*promoteNoteInfo{
		{Path: "a.md", Tags: []string{"go"}},
		{Path: "b.md", Tags: []string{"go"}},
		{Path: "c.md", Tags: []string{"go"}},
		{Path: "d.md", Tags: []string{"rust"}},
		{Path: "e.md", Tags: []string{"rust"}},
		{Path: "f.md", Tags: []string{"rust"}},
	}
	clusters, _ := detectClusters(notes, 0.25, 0.80, 3)
	if len(clusters) != 2 {
		t.Errorf("detectClusters() found %d clusters, want 2", len(clusters))
	}
}

// ─── buildCanonicalNote ──────────────────────────────────────────────────────

func TestBuildCanonicalNote_Structure(t *testing.T) {
	now := time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)
	notes := []*promoteNoteInfo{
		{Path: "Ideas/idea-one.md", Title: "Idea One", Tags: []string{"go"}, Body: "Body of idea one.\n"},
		{Path: "Ideas/idea-two.md", Title: "Idea Two", Tags: []string{"go", "cli"}, Body: "Body of idea two.\n"},
		{Path: "Ideas/idea-three.md", Title: "Idea Three", Tags: []string{"go"}, Body: "Body of idea three.\n"},
	}
	path, content := buildCanonicalNote(notes, now)

	if !strings.HasPrefix(path, "Notes/") {
		t.Errorf("canonical path should be in Notes/, got %q", path)
	}
	if !strings.HasSuffix(path, ".md") {
		t.Errorf("canonical path should end in .md, got %q", path)
	}
	if !strings.Contains(content, "promoted-from:") {
		t.Error("canonical note missing promoted-from frontmatter")
	}
	if !strings.Contains(content, "[[idea-one]]") {
		t.Error("canonical note missing wikilink to source 1")
	}
	if !strings.Contains(content, "Body of idea one.") {
		t.Error("canonical note missing body of source 1")
	}
	if !strings.Contains(content, "type: note") {
		t.Error("canonical note missing type: note")
	}
	if !strings.Contains(content, "2026-03-18") {
		t.Error("canonical note missing created date")
	}
}

// ─── buildPromotedSourceContent ──────────────────────────────────────────────

func TestBuildPromotedSourceContent_HasPromotedTo(t *testing.T) {
	now := time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)
	n := &promoteNoteInfo{
		Path:  "Ideas/my-idea.md",
		Title: "My Idea",
		Tags:  []string{"go"},
		Body:  "\nOriginal body.\n",
		Frontmatter: map[string]any{
			"title":   "My Idea",
			"type":    "idea",
			"created": "2026-03-10",
			"tags":    []string{"go"},
		},
	}
	content := buildPromotedSourceContent(n, "canonical-note", now)

	if !strings.Contains(content, "promoted-to: '[[canonical-note]]'") {
		t.Errorf("source content missing promoted-to link:\n%s", content)
	}
	if !strings.Contains(content, "archived: 2026-03-18") {
		t.Errorf("source content missing archived date:\n%s", content)
	}
	if !strings.Contains(content, "Original body.") {
		t.Errorf("source content missing original body:\n%s", content)
	}
}

// ─── archiveSourceNote (integration) ─────────────────────────────────────────

func TestArchiveSourceNote_MovesAndUpdates(t *testing.T) {
	vaultDir := t.TempDir()
	ideasDir := filepath.Join(vaultDir, "Ideas")
	if err := os.MkdirAll(ideasDir, 0755); err != nil {
		t.Fatal(err)
	}
	notePath := filepath.Join(ideasDir, "my-idea.md")
	if err := os.WriteFile(notePath, []byte("---\ntype: idea\n---\nSome idea.\n"), 0644); err != nil {
		t.Fatal(err)
	}

	n := &promoteNoteInfo{
		Path:        "Ideas/my-idea.md",
		Title:       "My Idea",
		Tags:        []string{"go"},
		Body:        "\nSome idea.\n",
		Frontmatter: map[string]any{"type": "idea"},
	}
	now := time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)

	archivePath, err := archiveSourceNote(vaultDir, n, "canonical", now)
	if err != nil {
		t.Fatalf("archiveSourceNote() error: %v", err)
	}

	// Original should be removed.
	if _, err := os.Stat(notePath); !os.IsNotExist(err) {
		t.Error("original note was not removed")
	}

	// Archive should exist with promoted-to link.
	fullArchive := filepath.Join(vaultDir, archivePath)
	data, err := os.ReadFile(fullArchive)
	if err != nil {
		t.Fatalf("archive file not found at %s: %v", archivePath, err)
	}
	if !strings.Contains(string(data), "promoted-to: '[[canonical]]'") {
		t.Errorf("archived note missing promoted-to link:\n%s", string(data))
	}
	if !strings.Contains(string(data), "archived: 2026-03-18") {
		t.Errorf("archived note missing archived date:\n%s", string(data))
	}
}

// ─── extractTagsList ─────────────────────────────────────────────────────────

func TestExtractTagsList_StringSlice(t *testing.T) {
	fm := map[string]any{"tags": []string{"go", "cli"}}
	got := extractTagsList(fm)
	if len(got) != 2 || got[0] != "go" || got[1] != "cli" {
		t.Errorf("extractTagsList() = %v, want [go cli]", got)
	}
}

func TestExtractTagsList_SingleString(t *testing.T) {
	fm := map[string]any{"tags": "go"}
	got := extractTagsList(fm)
	if len(got) != 1 || got[0] != "go" {
		t.Errorf("extractTagsList() = %v, want [go]", got)
	}
}

func TestExtractTagsList_Missing(t *testing.T) {
	got := extractTagsList(map[string]any{})
	if got != nil {
		t.Errorf("extractTagsList() = %v, want nil", got)
	}
}

func TestExtractTagsList_EmptyString(t *testing.T) {
	fm := map[string]any{"tags": ""}
	got := extractTagsList(fm)
	if got != nil {
		t.Errorf("extractTagsList() for empty string = %v, want nil", got)
	}
}

// ─── computeClusterScore ─────────────────────────────────────────────────────

func TestComputeClusterScore_TagOnly(t *testing.T) {
	// All identical tags → Jaccard = 1.0 for every pair → score = 1.0.
	notes := []*promoteNoteInfo{
		{Tags: []string{"go"}},
		{Tags: []string{"go"}},
		{Tags: []string{"go"}},
	}
	score := computeClusterScore(notes)
	if score != 1.0 {
		t.Errorf("computeClusterScore() = %.4f, want 1.0", score)
	}
}

func TestComputeClusterScore_SingleNote(t *testing.T) {
	notes := []*promoteNoteInfo{{Tags: []string{"go"}}}
	score := computeClusterScore(notes)
	if score != 0 {
		t.Errorf("computeClusterScore() single note = %.4f, want 0", score)
	}
}

// ─── promoteCluster (integration) ────────────────────────────────────────────

func TestPromoteCluster_CreatesCanonicalAndArchives(t *testing.T) {
	vaultDir := t.TempDir()
	ideasDir := filepath.Join(vaultDir, "Ideas")
	if err := os.MkdirAll(ideasDir, 0755); err != nil {
		t.Fatal(err)
	}

	// Create three source notes.
	sources := []struct {
		name, content string
	}{
		{"note-a.md", "---\ntype: idea\ntags:\n  - go\n---\nIdea A.\n"},
		{"note-b.md", "---\ntype: idea\ntags:\n  - go\n---\nIdea B.\n"},
		{"note-c.md", "---\ntype: idea\ntags:\n  - go\n---\nIdea C.\n"},
	}
	var noteInfos []*promoteNoteInfo
	for _, s := range sources {
		p := filepath.Join(ideasDir, s.name)
		if err := os.WriteFile(p, []byte(s.content), 0644); err != nil {
			t.Fatal(err)
		}
		noteInfos = append(noteInfos, &promoteNoteInfo{
			Path:        "Ideas/" + s.name,
			Title:       strings.TrimSuffix(s.name, ".md"),
			Tags:        []string{"go"},
			Body:        "Idea content.\n",
			Frontmatter: map[string]any{"type": "idea", "tags": []string{"go"}},
		})
	}

	now := time.Date(2026, 3, 18, 0, 0, 0, 0, time.UTC)
	result, err := promoteCluster(vaultDir, noteInfos, now)
	if err != nil {
		t.Fatalf("promoteCluster() error: %v", err)
	}

	// Canonical note should exist.
	fullCanonical := filepath.Join(vaultDir, result.CanonicalPath)
	data, err := os.ReadFile(fullCanonical)
	if err != nil {
		t.Fatalf("canonical note not found: %v", err)
	}
	if !strings.Contains(string(data), "promoted-from:") {
		t.Error("canonical note missing promoted-from frontmatter")
	}

	// All three sources should have been archived.
	if len(result.SourcePaths) != 3 {
		t.Errorf("expected 3 archived sources, got %d", len(result.SourcePaths))
	}

	// Original source files should be gone.
	for _, s := range sources {
		orig := filepath.Join(ideasDir, s.name)
		if _, statErr := os.Stat(orig); !os.IsNotExist(statErr) {
			t.Errorf("original file still exists: %s", orig)
		}
	}
}

// ─── collectNotesForClustering ───────────────────────────────────────────────

func TestCollectNotesForClustering_SkipsPromotedNotes(t *testing.T) {
	vaultDir := t.TempDir()
	ideasDir := filepath.Join(vaultDir, "Ideas")
	if err := os.MkdirAll(ideasDir, 0755); err != nil {
		t.Fatal(err)
	}

	// Write one normal note and one already-promoted note.
	normal := "---\ntype: idea\n---\nNormal idea.\n"
	promoted := "---\ntype: idea\npromoted-to: '[[canonical]]'\n---\nAlready done.\n"
	if err := os.WriteFile(filepath.Join(ideasDir, "normal.md"), []byte(normal), 0644); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(ideasDir, "promoted.md"), []byte(promoted), 0644); err != nil {
		t.Fatal(err)
	}

	notes, err := collectNotesForClustering(vaultDir)
	if err != nil {
		t.Fatalf("collectNotesForClustering() error: %v", err)
	}
	if len(notes) != 1 {
		t.Errorf("expected 1 note (skip promoted), got %d", len(notes))
	}
	if notes[0].Path != "Ideas/normal.md" {
		t.Errorf("unexpected note path: %s", notes[0].Path)
	}
}
