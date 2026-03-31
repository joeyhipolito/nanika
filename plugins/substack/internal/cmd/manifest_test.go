package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestLoadManifest(t *testing.T) {
	tmpDir := t.TempDir()
	manifestPath := filepath.Join(tmpDir, "manifest.json")

	m := Manifest{
		Article: "/path/to/article.mdx",
		Created: "2026-02-18T00:00:00Z",
		Assets: []ManifestAsset{
			{Type: "code-screenshot", SourceLine: 42, Language: "go", Path: "/path/to/img.png"},
			{Type: "illustration", Section: "How It Works", Path: "/path/to/ill.png"},
			{Type: "diagram", Description: "auth flow", Path: "/path/to/diag.png"},
		},
	}

	data, _ := json.MarshalIndent(m, "", "  ")
	if err := os.WriteFile(manifestPath, data, 0644); err != nil {
		t.Fatalf("failed to write test manifest: %v", err)
	}

	loaded, err := loadManifest(manifestPath)
	if err != nil {
		t.Fatalf("loadManifest failed: %v", err)
	}

	if loaded.Article != m.Article {
		t.Errorf("Article = %q, want %q", loaded.Article, m.Article)
	}
	if len(loaded.Assets) != 3 {
		t.Fatalf("Assets count = %d, want 3", len(loaded.Assets))
	}
	if loaded.Assets[0].Type != "code-screenshot" {
		t.Errorf("Asset[0].Type = %q, want 'code-screenshot'", loaded.Assets[0].Type)
	}
	if loaded.Assets[1].Section != "How It Works" {
		t.Errorf("Asset[1].Section = %q, want 'How It Works'", loaded.Assets[1].Section)
	}
}

func TestLoadManifestMissing(t *testing.T) {
	_, err := loadManifest("/nonexistent/manifest.json")
	if err == nil {
		t.Fatal("expected error for missing file")
	}
}

func TestLoadManifestInvalidJSON(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "bad.json")
	os.WriteFile(path, []byte("not json"), 0644)

	_, err := loadManifest(path)
	if err == nil {
		t.Fatal("expected error for invalid JSON")
	}
}

func TestLoadManifestEmptyAssets(t *testing.T) {
	tmpDir := t.TempDir()
	path := filepath.Join(tmpDir, "empty.json")

	m := Manifest{Article: "test.mdx", Created: "2026-02-18T00:00:00Z"}
	data, _ := json.Marshal(m)
	os.WriteFile(path, data, 0644)

	loaded, err := loadManifest(path)
	if err != nil {
		t.Fatalf("loadManifest failed: %v", err)
	}
	if len(loaded.Assets) != 0 {
		t.Errorf("expected 0 assets, got %d", len(loaded.Assets))
	}
}

func TestMatchesSection(t *testing.T) {
	tests := []struct {
		name    string
		node    TiptapNode
		section string
		want    bool
	}{
		{
			name: "exact match",
			node: TiptapNode{
				Type:    "heading",
				Content: []TiptapNode{{Text: "How It Works"}},
			},
			section: "How It Works",
			want:    true,
		},
		{
			name: "case insensitive",
			node: TiptapNode{
				Type:    "heading",
				Content: []TiptapNode{{Text: "how it works"}},
			},
			section: "How It Works",
			want:    true,
		},
		{
			name: "no match",
			node: TiptapNode{
				Type:    "heading",
				Content: []TiptapNode{{Text: "Introduction"}},
			},
			section: "How It Works",
			want:    false,
		},
		{
			name: "nested text content",
			node: TiptapNode{
				Type: "heading",
				Content: []TiptapNode{
					{Text: "How "},
					{Text: "It Works"},
				},
			},
			section: "How It Works",
			want:    true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := matchesSection(tt.node, tt.section)
			if got != tt.want {
				t.Errorf("matchesSection = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestExtractNodeText(t *testing.T) {
	node := TiptapNode{
		Content: []TiptapNode{
			{Text: "Hello "},
			{
				Content: []TiptapNode{
					{Text: "World"},
				},
			},
		},
	}

	got := extractNodeText(node)
	if got != "Hello World" {
		t.Errorf("extractNodeText = %q, want 'Hello World'", got)
	}
}

func TestBuildManifestImageResolverNoCodeAssets(t *testing.T) {
	m := &Manifest{
		Assets: []ManifestAsset{
			{Type: "illustration", Path: "/tmp/ill.png"},
		},
	}
	resolver := buildManifestImageResolver(m, nil)
	if resolver != nil {
		t.Error("expected nil resolver when no code-screenshot assets")
	}
}

func TestManifestAssetJSON(t *testing.T) {
	asset := ManifestAsset{
		Type:       "code-screenshot",
		SourceLine: 42,
		Language:   "go",
		Path:       "/path/to/img.png",
	}

	data, err := json.Marshal(asset)
	if err != nil {
		t.Fatalf("marshal failed: %v", err)
	}

	s := string(data)
	if !strings.Contains(s, `"type":"code-screenshot"`) {
		t.Error("expected type field in JSON")
	}
	if !strings.Contains(s, `"source_line":42`) {
		t.Error("expected source_line field in JSON")
	}
	// omitempty fields should not be present
	if strings.Contains(s, `"section"`) {
		t.Error("expected section to be omitted")
	}
	if strings.Contains(s, `"prompt_file"`) {
		t.Error("expected prompt_file to be omitted")
	}
}
