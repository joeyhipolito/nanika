package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// ─── clusterID ───────────────────────────────────────────────────────────────

func TestClusterID_Stable(t *testing.T) {
	paths := []string{"Inbox/b.md", "Inbox/a.md", "Inbox/c.md"}
	id1 := clusterID(paths)
	id2 := clusterID(paths)
	if id1 != id2 {
		t.Errorf("clusterID() is not stable: %q != %q", id1, id2)
	}
}

func TestClusterID_OrderIndependent(t *testing.T) {
	a := clusterID([]string{"Inbox/a.md", "Inbox/b.md", "Inbox/c.md"})
	b := clusterID([]string{"Inbox/c.md", "Inbox/a.md", "Inbox/b.md"})
	if a != b {
		t.Errorf("clusterID() should be order-independent: %q != %q", a, b)
	}
}

func TestClusterID_DifferentPaths(t *testing.T) {
	a := clusterID([]string{"Inbox/a.md", "Inbox/b.md", "Inbox/c.md"})
	b := clusterID([]string{"Inbox/x.md", "Inbox/y.md", "Inbox/z.md"})
	if a == b {
		t.Errorf("clusterID() should differ for different paths, both = %q", a)
	}
}

func TestClusterID_Format(t *testing.T) {
	id := clusterID([]string{"Inbox/a.md"})
	if len(id) != 16 {
		t.Errorf("clusterID() should be 16 hex chars, got %q (len %d)", id, len(id))
	}
}

// ─── avgPairwiseSimilarity ───────────────────────────────────────────────────

func TestAvgPairwiseSimilarity_IdenticalVectors(t *testing.T) {
	vec := []float32{1, 0, 0}
	notes := []*promoteNoteInfo{
		{Embedding: vec},
		{Embedding: vec},
		{Embedding: vec},
	}
	got := avgPairwiseSimilarity(notes)
	if got < 0.999 {
		t.Errorf("avgPairwiseSimilarity() = %.4f, want ~1.0 for identical vectors", got)
	}
}

func TestAvgPairwiseSimilarity_OrthogonalVectors(t *testing.T) {
	notes := []*promoteNoteInfo{
		{Embedding: []float32{1, 0, 0}},
		{Embedding: []float32{0, 1, 0}},
		{Embedding: []float32{0, 0, 1}},
	}
	got := avgPairwiseSimilarity(notes)
	if got > 0.01 {
		t.Errorf("avgPairwiseSimilarity() = %.4f, want ~0.0 for orthogonal vectors", got)
	}
}

func TestAvgPairwiseSimilarity_SingleNote(t *testing.T) {
	notes := []*promoteNoteInfo{{Embedding: []float32{1, 0}}}
	got := avgPairwiseSimilarity(notes)
	if got != 0 {
		t.Errorf("avgPairwiseSimilarity() single note = %.4f, want 0", got)
	}
}

// ─── detectSemanticClusters ──────────────────────────────────────────────────

func TestDetectSemanticClusters_FormsSingleCluster(t *testing.T) {
	vec := []float32{1, 0, 0}
	notes := []*promoteNoteInfo{
		{Path: "Inbox/a.md", Embedding: vec},
		{Path: "Inbox/b.md", Embedding: vec},
		{Path: "Inbox/c.md", Embedding: vec},
		// Orthogonal — should not join the cluster.
		{Path: "Inbox/d.md", Embedding: []float32{0, 1, 0}},
	}
	clusters := detectSemanticClusters(notes, 0.70, 3)
	if len(clusters) != 1 {
		t.Fatalf("detectSemanticClusters() found %d clusters, want 1", len(clusters))
	}
	if clusters[0].Size != 3 {
		t.Errorf("cluster size = %d, want 3", clusters[0].Size)
	}
	if clusters[0].AvgSimilarity < 0.999 {
		t.Errorf("avg similarity = %.4f, want ~1.0 for identical vectors", clusters[0].AvgSimilarity)
	}
}

func TestDetectSemanticClusters_BelowMinSize(t *testing.T) {
	vec := []float32{1, 0, 0}
	notes := []*promoteNoteInfo{
		{Path: "Inbox/a.md", Embedding: vec},
		{Path: "Inbox/b.md", Embedding: vec},
		// Only two similar notes — below min=3.
	}
	clusters := detectSemanticClusters(notes, 0.70, 3)
	if len(clusters) != 0 {
		t.Errorf("detectSemanticClusters() found %d clusters, want 0", len(clusters))
	}
}

func TestDetectSemanticClusters_EmptyInput(t *testing.T) {
	clusters := detectSemanticClusters(nil, 0.70, 3)
	if len(clusters) != 0 {
		t.Errorf("detectSemanticClusters(nil) = %d clusters, want 0", len(clusters))
	}
}

func TestDetectSemanticClusters_ClusterIDPresent(t *testing.T) {
	vec := []float32{1, 0, 0}
	notes := []*promoteNoteInfo{
		{Path: "Inbox/a.md", Embedding: vec},
		{Path: "Inbox/b.md", Embedding: vec},
		{Path: "Inbox/c.md", Embedding: vec},
	}
	clusters := detectSemanticClusters(notes, 0.70, 3)
	if len(clusters) == 0 {
		t.Fatal("expected at least one cluster")
	}
	if len(clusters[0].ClusterID) != 16 {
		t.Errorf("cluster_id should be 16 hex chars, got %q", clusters[0].ClusterID)
	}
}

// ─── loadUntaggedCaptures ────────────────────────────────────────────────────

func TestLoadUntaggedCaptures_IncludesInboxNotes(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "Inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	// Untagged fleeting capture — should be included.
	content := "---\ntype: fleeting\ncreated: 2026-03-18\n---\nSome rough idea.\n"
	if err := os.WriteFile(filepath.Join(inboxDir, "20260318-120000.md"), []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	captures, err := loadUntaggedCaptures(vaultDir)
	if err != nil {
		t.Fatalf("loadUntaggedCaptures() error: %v", err)
	}
	if len(captures) != 1 {
		t.Errorf("expected 1 capture, got %d", len(captures))
	}
}

func TestLoadUntaggedCaptures_ExcludesTaggedCaptures(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "Inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	// Tagged capture — concept is already named, should be excluded.
	tagged := "---\ntype: fleeting\ntags:\n  - golang\n---\nTagged idea.\n"
	if err := os.WriteFile(filepath.Join(inboxDir, "tagged.md"), []byte(tagged), 0644); err != nil {
		t.Fatal(err)
	}

	captures, err := loadUntaggedCaptures(vaultDir)
	if err != nil {
		t.Fatalf("loadUntaggedCaptures() error: %v", err)
	}
	if len(captures) != 0 {
		t.Errorf("expected 0 captures (tagged), got %d", len(captures))
	}
}

func TestLoadUntaggedCaptures_ExcludesPromotedNotes(t *testing.T) {
	vaultDir := t.TempDir()
	inboxDir := filepath.Join(vaultDir, "Inbox")
	if err := os.MkdirAll(inboxDir, 0755); err != nil {
		t.Fatal(err)
	}
	promoted := "---\ntype: fleeting\npromoted-to: '[[some-note]]'\n---\nAlready promoted.\n"
	if err := os.WriteFile(filepath.Join(inboxDir, "promoted.md"), []byte(promoted), 0644); err != nil {
		t.Fatal(err)
	}

	captures, err := loadUntaggedCaptures(vaultDir)
	if err != nil {
		t.Fatalf("loadUntaggedCaptures() error: %v", err)
	}
	if len(captures) != 0 {
		t.Errorf("expected 0 captures (promoted), got %d", len(captures))
	}
}

func TestLoadUntaggedCaptures_IncludesFleetingOutsideInbox(t *testing.T) {
	vaultDir := t.TempDir()
	// A fleeting note outside Inbox/ should still be included.
	content := "---\ntype: fleeting\ncreated: 2026-03-18\n---\nIdea outside inbox.\n"
	if err := os.WriteFile(filepath.Join(vaultDir, "stray-fleeting.md"), []byte(content), 0644); err != nil {
		t.Fatal(err)
	}

	captures, err := loadUntaggedCaptures(vaultDir)
	if err != nil {
		t.Fatalf("loadUntaggedCaptures() error: %v", err)
	}
	if len(captures) != 1 {
		t.Errorf("expected 1 capture (type: fleeting outside Inbox), got %d", len(captures))
	}
}

// ─── persistClusters / GetAllClusters (integration) ──────────────────────────

func TestPersistClusters_RoundTrip(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "search.db")
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("index.Open() error: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	clusters := []DetectedCluster{
		{
			ClusterID:     "abc1234500000001",
			MemberPaths:   []string{"Inbox/a.md", "Inbox/b.md", "Inbox/c.md"},
			AvgSimilarity: 0.85,
			Size:          3,
		},
	}

	if err := persistClusters(store, clusters); err != nil {
		t.Fatalf("persistClusters() error: %v", err)
	}

	rows, err := store.GetAllClusters()
	if err != nil {
		t.Fatalf("GetAllClusters() error: %v", err)
	}
	if len(rows) != 1 {
		t.Fatalf("expected 1 stored cluster, got %d", len(rows))
	}

	row := rows[0]
	if row.ClusterID != "abc1234500000001" {
		t.Errorf("cluster_id = %q, want %q", row.ClusterID, "abc1234500000001")
	}
	if row.Size != 3 {
		t.Errorf("size = %d, want 3", row.Size)
	}
	if row.AvgSimilarity < 0.849 || row.AvgSimilarity > 0.851 {
		t.Errorf("avg_similarity = %.4f, want 0.85", row.AvgSimilarity)
	}

	var paths []string
	if err := json.Unmarshal([]byte(row.MemberPaths), &paths); err != nil {
		t.Fatalf("unmarshal member_paths: %v", err)
	}
	if len(paths) != 3 {
		t.Errorf("member_paths len = %d, want 3", len(paths))
	}
}

func TestPersistClusters_ReplacesOnRerun(t *testing.T) {
	tmpDir := t.TempDir()
	dbPath := filepath.Join(tmpDir, "search.db")
	store, err := index.Open(dbPath)
	if err != nil {
		t.Fatalf("index.Open() error: %v", err)
	}
	t.Cleanup(func() { store.Close() })

	first := []DetectedCluster{{ClusterID: "aaa", MemberPaths: []string{"a.md", "b.md", "c.md"}, AvgSimilarity: 0.8, Size: 3}}
	second := []DetectedCluster{
		{ClusterID: "bbb", MemberPaths: []string{"x.md", "y.md", "z.md"}, AvgSimilarity: 0.9, Size: 3},
		{ClusterID: "ccc", MemberPaths: []string{"p.md", "q.md", "r.md"}, AvgSimilarity: 0.75, Size: 3},
	}

	if err := persistClusters(store, first); err != nil {
		t.Fatalf("first persistClusters() error: %v", err)
	}
	if err := persistClusters(store, second); err != nil {
		t.Fatalf("second persistClusters() error: %v", err)
	}

	rows, err := store.GetAllClusters()
	if err != nil {
		t.Fatalf("GetAllClusters() error: %v", err)
	}
	// Second run should completely replace first run.
	if len(rows) != 2 {
		t.Errorf("expected 2 clusters after second run, got %d", len(rows))
	}
	for _, r := range rows {
		if r.ClusterID == "aaa" {
			t.Errorf("stale cluster from first run still present")
		}
	}
}
