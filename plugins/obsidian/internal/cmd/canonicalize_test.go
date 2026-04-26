package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// ─── buildFallbackSummary ──────────────────────────────────────────────────

func TestBuildFallbackSummary_ConcatenatesBodies(t *testing.T) {
	members := []*promoteNoteInfo{
		{Body: "First idea."},
		{Body: "Second idea."},
		{Body: "Third idea."},
	}
	got := buildFallbackSummary(members)
	if !strings.Contains(got, "First idea.") {
		t.Errorf("expected first body in summary, got %q", got)
	}
	if !strings.Contains(got, "Third idea.") {
		t.Errorf("expected third body in summary, got %q", got)
	}
}

func TestBuildFallbackSummary_SkipsEmptyBodies(t *testing.T) {
	members := []*promoteNoteInfo{
		{Body: ""},
		{Body: "  "},
		{Body: "Only this."},
	}
	got := buildFallbackSummary(members)
	if got != "Only this." {
		t.Errorf("buildFallbackSummary() = %q, want %q", got, "Only this.")
	}
}

func TestBuildFallbackSummary_Empty(t *testing.T) {
	got := buildFallbackSummary(nil)
	if got != "" {
		t.Errorf("buildFallbackSummary(nil) = %q, want %q", got, "")
	}
}

// ─── loadMemberNotes ──────────────────────────────────────────────────────

func TestLoadMemberNotes_ReadsFiles(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}

	contents := []struct {
		name    string
		content string
	}{
		{"a.md", "---\ntitle: Note A\ntype: fleeting\n---\nBody A.\n"},
		{"b.md", "---\ntitle: Note B\ntype: fleeting\n---\nBody B.\n"},
		{"c.md", "---\ntitle: Note C\ntype: fleeting\n---\nBody C.\n"},
	}
	var paths []string
	for _, f := range contents {
		p := filepath.Join(inboxDir, f.name)
		if err := os.WriteFile(p, []byte(f.content), 0644); err != nil {
			t.Fatal(err)
		}
		paths = append(paths, "inbox/"+f.name)
	}

	members, err := loadMemberNotes(vaultDir, paths)
	if err != nil {
		t.Fatalf("loadMemberNotes() error: %v", err)
	}
	if len(members) != 3 {
		t.Fatalf("expected 3 members, got %d", len(members))
	}
	if members[0].Title != "Note A" {
		t.Errorf("expected title %q, got %q", "Note A", members[0].Title)
	}
	if members[1].Body != "Body B.\n" {
		t.Errorf("expected body %q, got %q", "Body B.\n", members[1].Body)
	}
}

func TestLoadMemberNotes_SkipsMissingFiles(t *testing.T) {
	vaultDir := t.TempDir()
	// Two real files, one missing path.
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(inboxDir, "real.md"), []byte("---\ntitle: Real\n---\nBody.\n"), 0644); err != nil {
		t.Fatal(err)
	}

	members, err := loadMemberNotes(vaultDir, []string{"inbox/real.md", "inbox/missing.md"})
	if err != nil {
		t.Fatalf("loadMemberNotes() error: %v", err)
	}
	if len(members) != 1 {
		t.Errorf("expected 1 member (missing file skipped), got %d", len(members))
	}
}

func TestLoadMemberNotes_UsesFilenameFallbackTitle(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	// Note without a title frontmatter field.
	if err := os.WriteFile(filepath.Join(inboxDir, "my-idea.md"), []byte("---\ntype: fleeting\n---\nSome idea.\n"), 0644); err != nil {
		t.Fatal(err)
	}

	members, err := loadMemberNotes(vaultDir, []string{"inbox/my-idea.md"})
	if err != nil {
		t.Fatalf("loadMemberNotes() error: %v", err)
	}
	if len(members) != 1 {
		t.Fatalf("expected 1 member, got %d", len(members))
	}
	if members[0].Title != "my-idea" {
		t.Errorf("expected filename fallback title %q, got %q", "my-idea", members[0].Title)
	}
}

// ─── detectNearDuplicate ──────────────────────────────────────────────────

func TestDetectNearDuplicate_NoEmbeddingClient(t *testing.T) {
	// A nil (unavailable) embedding client should always return no duplicate.
	embClient := index.NewEmbeddingClient("")
	path, err := detectNearDuplicate(nil, embClient, nil, "some summary")
	if err != nil {
		t.Fatalf("detectNearDuplicate() error: %v", err)
	}
	if path != "" {
		t.Errorf("expected no duplicate without embedding client, got %q", path)
	}
}

func TestDetectNearDuplicate_EmptyNoteRows(t *testing.T) {
	embClient := index.NewEmbeddingClient("fake-key")
	path, err := detectNearDuplicate(nil, embClient, []index.NoteRow{}, "some summary")
	if err != nil {
		t.Fatalf("detectNearDuplicate() error: %v", err)
	}
	if path != "" {
		t.Errorf("expected no duplicate for empty note rows, got %q", path)
	}
}

// ─── createCanonicalNote ──────────────────────────────────────────────────

func TestCreateCanonicalNote_WritesFile(t *testing.T) {
	vaultDir := t.TempDir()
	if err := os.MkdirAll(filepath.Join(vaultDir, canonicalizeFolder), 0755); err != nil {
		t.Fatal(err)
	}

	members := []*promoteNoteInfo{
		{Path: "inbox/a.md", Title: "Note A", Body: "Content A."},
		{Path: "inbox/b.md", Title: "Note B", Body: "Content B."},
		{Path: "inbox/c.md", Title: "Note C", Body: "Content C."},
	}
	summary := "A synthesized summary of all three notes."
	title := "Note A"
	clusterID := "abc1234500000001"

	path, err := createCanonicalNote(vaultDir, members, summary, title, clusterID, time.Now())
	if err != nil {
		t.Fatalf("createCanonicalNote() error: %v", err)
	}
	if path == "" {
		t.Fatal("createCanonicalNote() returned empty path")
	}

	data, err := os.ReadFile(filepath.Join(vaultDir, path))
	if err != nil {
		t.Fatalf("reading canonical note: %v", err)
	}
	content := string(data)

	if !strings.Contains(content, "cluster_id: "+clusterID) {
		t.Errorf("canonical note missing cluster_id field")
	}
	if !strings.Contains(content, "canonicalized-from:") {
		t.Errorf("canonical note missing canonicalized-from field")
	}
	if !strings.Contains(content, "[[a]]") {
		t.Errorf("canonical note missing backlink to 'a'")
	}
	if !strings.Contains(content, summary) {
		t.Errorf("canonical note missing summary text")
	}
	if !strings.Contains(content, "## Sources") {
		t.Errorf("canonical note missing Sources section")
	}
}

func TestCreateCanonicalNote_DeconflictsExistingPath(t *testing.T) {
	vaultDir := t.TempDir()
	notesDir := filepath.Join(vaultDir, canonicalizeFolder)
	if err := os.MkdirAll(notesDir, 0755); err != nil {
		t.Fatal(err)
	}

	members := []*promoteNoteInfo{
		{Path: "inbox/a.md", Title: "My Topic", Body: "Body."},
		{Path: "inbox/b.md", Title: "My Topic", Body: "Body."},
		{Path: "inbox/c.md", Title: "My Topic", Body: "Body."},
	}

	// Pre-create the expected path so deconfliction is triggered.
	slug := slugify("My Topic")
	preExisting := filepath.Join(notesDir, slug+".md")
	if err := os.WriteFile(preExisting, []byte("existing"), 0644); err != nil {
		t.Fatal(err)
	}

	path, err := createCanonicalNote(vaultDir, members, "summary", "My Topic", "cid123", time.Now())
	if err != nil {
		t.Fatalf("createCanonicalNote() error: %v", err)
	}
	if path == canonicalizeFolder+"/"+slug+".md" {
		t.Errorf("createCanonicalNote() should have deconflicted the path")
	}
}

// ─── insertBacklink ───────────────────────────────────────────────────────

func TestInsertBacklink_AddsField(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	notePath := "inbox/note.md"
	original := "---\ntitle: My Note\ntype: fleeting\n---\nSome content.\n"
	if err := os.WriteFile(filepath.Join(vaultDir, notePath), []byte(original), 0644); err != nil {
		t.Fatal(err)
	}

	if err := insertBacklink(vaultDir, notePath, "canonical-note"); err != nil {
		t.Fatalf("insertBacklink() error: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(vaultDir, notePath))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), "canonicalized-to: '[[canonical-note]]'") {
		t.Errorf("expected canonicalized-to field, got:\n%s", data)
	}
	if !strings.Contains(string(data), "Some content.") {
		t.Errorf("body should be preserved after backlink insertion")
	}
}

func TestInsertBacklink_IdempotentIfAlreadyPresent(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	notePath := "inbox/note.md"
	original := "---\ntitle: My Note\ncanonical-to: '[[already-there]]'\n---\nBody.\n"
	// Use the key the code checks: "canonicalized-to"
	original = "---\ntitle: My Note\ncanonicalized-to: '[[already-there]]'\n---\nBody.\n"
	if err := os.WriteFile(filepath.Join(vaultDir, notePath), []byte(original), 0644); err != nil {
		t.Fatal(err)
	}

	if err := insertBacklink(vaultDir, notePath, "new-canonical"); err != nil {
		t.Fatalf("insertBacklink() error: %v", err)
	}

	data, err := os.ReadFile(filepath.Join(vaultDir, notePath))
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(data), "already-there") {
		t.Errorf("existing canonicalized-to should be preserved")
	}
	if strings.Contains(string(data), "new-canonical") {
		t.Errorf("should not overwrite existing canonicalized-to link")
	}
}

// ─── buildSourceWithBacklink ──────────────────────────────────────────────

func TestBuildSourceWithBacklink_PreservesFields(t *testing.T) {
	content := "---\ntitle: Old Note\ncreated: 2026-01-01\ntype: fleeting\nstatus: draft\n---\nBody text here.\n"
	parsed := vault.ParseNote(content)

	result := buildSourceWithBacklink(parsed, "the-canonical")

	checks := []string{
		"title: Old Note",
		"created: 2026-01-01",
		"type: fleeting",
		"status: draft",
		"canonicalized-to: '[[the-canonical]]'",
		"Body text here.",
	}
	for _, c := range checks {
		if !strings.Contains(result, c) {
			t.Errorf("buildSourceWithBacklink() missing %q in:\n%s", c, result)
		}
	}
}

// ─── canonicalizeClusters (integration with temp store) ───────────────────

func TestCanonicalizeClusters_DryRunDoesNotWrite(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}

	// Write three member notes.
	for _, name := range []string{"a.md", "b.md", "c.md"} {
		content := "---\ntitle: " + name + "\ntype: fleeting\n---\nBody.\n"
		if err := os.WriteFile(filepath.Join(inboxDir, name), []byte(content), 0644); err != nil {
			t.Fatal(err)
		}
	}

	dbPath := filepath.Join(vaultDir, ".obsidian", "search.db")
	if err := os.MkdirAll(filepath.Dir(dbPath), 0755); err != nil {
		t.Fatal(err)
	}
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("index.Open() error: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	clusters := []DetectedCluster{
		{
			ClusterID:   "abc1234500000001",
			MemberPaths: []string{"inbox/a.md", "inbox/b.md", "inbox/c.md"},
			Size:        3,
		},
	}

	opts := CanonicalizeOptions{DryRun: true}
	result, err := canonicalizeClusters(vaultDir, store, clusters, nil, index.NewEmbeddingClient(""), opts)
	if err != nil {
		t.Fatalf("canonicalizeClusters() error: %v", err)
	}

	if len(result.Created) != 1 {
		t.Errorf("expected 1 dry-run created, got %d", len(result.Created))
	}
	if result.Created[0].CanonicalPath != "" {
		t.Errorf("dry-run should not set canonical_path, got %q", result.Created[0].CanonicalPath)
	}

	// Confirm no files were written.
	notesDir := filepath.Join(vaultDir, canonicalizeFolder)
	if _, err := os.Stat(notesDir); err == nil {
		t.Errorf("dry-run should not create %s directory", canonicalizeFolder)
	}

	// Confirm no canonical record was stored.
	canonicals, err := store.GetAllCanonicals()
	if err != nil {
		t.Fatalf("GetAllCanonicals() error: %v", err)
	}
	if len(canonicals) != 0 {
		t.Errorf("dry-run should not persist canonical records, got %d", len(canonicals))
	}
}

func TestCanonicalizeClusters_SkipsAlreadyCanonicalized(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"a.md", "b.md", "c.md"} {
		if err := os.WriteFile(filepath.Join(inboxDir, name), []byte("---\ntitle: "+name+"\n---\nBody.\n"), 0644); err != nil {
			t.Fatal(err)
		}
	}

	dbPath := filepath.Join(vaultDir, ".obsidian", "search.db")
	if err := os.MkdirAll(filepath.Dir(dbPath), 0755); err != nil {
		t.Fatal(err)
	}
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("index.Open() error: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	const clusterID = "abc1234500000001"

	// Pre-record this cluster as already canonicalized.
	if err := store.UpsertCanonical(index.CanonicalRow{
		ClusterID:     clusterID,
		CanonicalPath: "Notes/existing.md",
		CreatedAt:     time.Now().Unix(),
	}); err != nil {
		t.Fatalf("UpsertCanonical() error: %v", err)
	}

	clusters := []DetectedCluster{
		{ClusterID: clusterID, MemberPaths: []string{"inbox/a.md", "inbox/b.md", "inbox/c.md"}, Size: 3},
	}
	opts := CanonicalizeOptions{}
	result, err := canonicalizeClusters(vaultDir, store, clusters, nil, index.NewEmbeddingClient(""), opts)
	if err != nil {
		t.Fatalf("canonicalizeClusters() error: %v", err)
	}

	if len(result.Skipped) != 1 {
		t.Errorf("expected 1 skipped (already canonicalized), got %d", len(result.Skipped))
	}
	if result.Skipped[0].Reason != "already canonicalized" {
		t.Errorf("unexpected skip reason: %q", result.Skipped[0].Reason)
	}
	if len(result.Created) != 0 {
		t.Errorf("expected 0 created, got %d", len(result.Created))
	}
}

func TestCanonicalizeClusters_SkipsWhenTooFewReadableMembers(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	// Only write one of the three referenced files.
	if err := os.WriteFile(filepath.Join(inboxDir, "a.md"), []byte("---\ntitle: A\n---\nBody.\n"), 0644); err != nil {
		t.Fatal(err)
	}

	dbPath := filepath.Join(vaultDir, ".obsidian", "search.db")
	if err := os.MkdirAll(filepath.Dir(dbPath), 0755); err != nil {
		t.Fatal(err)
	}
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("index.Open() error: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	clusters := []DetectedCluster{
		{
			ClusterID:   "def9876500000002",
			MemberPaths: []string{"inbox/a.md", "inbox/missing1.md", "inbox/missing2.md"},
			Size:        3,
		},
	}
	opts := CanonicalizeOptions{}
	result, err := canonicalizeClusters(vaultDir, store, clusters, nil, index.NewEmbeddingClient(""), opts)
	if err != nil {
		t.Fatalf("canonicalizeClusters() error: %v", err)
	}

	if len(result.Skipped) != 1 {
		t.Errorf("expected 1 skipped (too few readable members), got %d", len(result.Skipped))
	}
}

func TestCanonicalizeClusters_CreatesNoteAndBacklinks(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"a.md", "b.md", "c.md"} {
		content := "---\ntitle: Note " + name + "\ntype: fleeting\n---\nContent of " + name + ".\n"
		if err := os.WriteFile(filepath.Join(inboxDir, name), []byte(content), 0644); err != nil {
			t.Fatal(err)
		}
	}

	dbPath := filepath.Join(vaultDir, ".obsidian", "search.db")
	if err := os.MkdirAll(filepath.Dir(dbPath), 0755); err != nil {
		t.Fatal(err)
	}
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("index.Open() error: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	clusters := []DetectedCluster{
		{
			ClusterID:   "abc1234500000099",
			MemberPaths: []string{"inbox/a.md", "inbox/b.md", "inbox/c.md"},
			Size:        3,
		},
	}
	opts := CanonicalizeOptions{}
	result, err := canonicalizeClusters(vaultDir, store, clusters, nil, index.NewEmbeddingClient(""), opts)
	if err != nil {
		t.Fatalf("canonicalizeClusters() error: %v", err)
	}

	if len(result.Created) != 1 {
		t.Fatalf("expected 1 created, got %d; skipped: %v", len(result.Created), result.Skipped)
	}

	created := result.Created[0]
	if created.CanonicalPath == "" {
		t.Error("canonical_path should be set")
	}

	// Check canonical note file exists.
	fullCanonical := filepath.Join(vaultDir, created.CanonicalPath)
	canonicalData, err := os.ReadFile(fullCanonical)
	if err != nil {
		t.Fatalf("reading canonical note: %v", err)
	}
	if !strings.Contains(string(canonicalData), "canonicalized-from:") {
		t.Errorf("canonical note missing canonicalized-from field")
	}

	// Check each source note received a backlink.
	for _, p := range []string{"inbox/a.md", "inbox/b.md", "inbox/c.md"} {
		data, err := os.ReadFile(filepath.Join(vaultDir, p))
		if err != nil {
			t.Fatalf("reading source note %s: %v", p, err)
		}
		if !strings.Contains(string(data), "canonicalized-to:") {
			t.Errorf("source note %s missing canonicalized-to backlink", p)
		}
	}

	// Check canonical record stored in DB.
	canonicals, err := store.GetAllCanonicals()
	if err != nil {
		t.Fatalf("GetAllCanonicals() error: %v", err)
	}
	if len(canonicals) != 1 {
		t.Errorf("expected 1 canonical record, got %d", len(canonicals))
	}
	if canonicals[0].ClusterID != "abc1234500000099" {
		t.Errorf("canonical cluster_id = %q, want %q", canonicals[0].ClusterID, "abc1234500000099")
	}
}

// ─── loadStoredClusters ───────────────────────────────────────────────────

func TestLoadStoredClusters_RoundTrip(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "search.db")
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("index.Open() error: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	// Persist a cluster.
	if err := persistClusters(store, []DetectedCluster{
		{ClusterID: "abc", MemberPaths: []string{"a.md", "b.md", "c.md"}, AvgSimilarity: 0.8, Size: 3},
	}); err != nil {
		t.Fatalf("persistClusters() error: %v", err)
	}

	clusters, err := loadStoredClusters(store)
	if err != nil {
		t.Fatalf("loadStoredClusters() error: %v", err)
	}
	if len(clusters) != 1 {
		t.Fatalf("expected 1 cluster, got %d", len(clusters))
	}
	if clusters[0].ClusterID != "abc" {
		t.Errorf("cluster_id = %q, want %q", clusters[0].ClusterID, "abc")
	}
	if len(clusters[0].MemberPaths) != 3 {
		t.Errorf("member_paths len = %d, want 3", len(clusters[0].MemberPaths))
	}
}
