package worker

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

func TestComputeDensity(t *testing.T) {
	tests := []struct {
		name               string
		input              string
		wantTotalBytes     int
		wantArticles       int
		wantLinkingVerbs   int
		wantFragmentRatio  float64
		checkFragmentRatio bool
	}{
		{
			name:           "empty input yields zero counts",
			input:          "",
			wantTotalBytes: 0,
			wantArticles:   0,
		},
		{
			name: "pure code-block input yields zero prose counts",
			// Fenced block containing prose-like tokens that must NOT be counted.
			input: "```\nthe a an is are was the cat sat on the mat\n```\n",
			// total_bytes == len(input); prose counters == 0 because regions stripped.
			wantArticles:     0,
			wantLinkingVerbs: 0,
		},
		{
			// NOTE: the literal phrase "the cat sat on the mat" contains 2 articles
			// ("the" × 2). The architect's IMPLEMENTATION-SPEC wrote "article count=3"
			// for this input, which is a miscounting of that phrase. We assert the
			// correct literal count here (2) so the regex contract remains
			// well-defined and future regressions surface cleanly.
			name:         "article-dense prose increments article count",
			input:        "the cat sat on the mat",
			wantArticles: 2,
		},
		{
			name:               "fragment-only prose yields fragment_ratio 1.0",
			input:              "Wrap in useMemo. Drop ref.",
			wantFragmentRatio:  1.0,
			checkFragmentRatio: true,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			rec := ComputeDensity([]byte(tt.input))

			if tt.name == "empty input yields zero counts" {
				if rec.TotalBytes != 0 {
					t.Errorf("TotalBytes = %d, want 0", rec.TotalBytes)
				}
				if rec.ArticleCount != 0 || rec.LinkingVerbCount != 0 {
					t.Errorf("prose counts nonzero for empty input: articles=%d linking=%d",
						rec.ArticleCount, rec.LinkingVerbCount)
				}
				if rec.AvgSentenceLen != 0 || rec.FragmentRatio != 0 {
					t.Errorf("sentence metrics nonzero for empty input: avg=%v frag=%v",
						rec.AvgSentenceLen, rec.FragmentRatio)
				}
				return
			}

			if rec.ArticleCount != tt.wantArticles {
				t.Errorf("ArticleCount = %d, want %d", rec.ArticleCount, tt.wantArticles)
			}
			if tt.wantLinkingVerbs != 0 || tt.name == "pure code-block input yields zero prose counts" {
				if rec.LinkingVerbCount != tt.wantLinkingVerbs {
					t.Errorf("LinkingVerbCount = %d, want %d",
						rec.LinkingVerbCount, tt.wantLinkingVerbs)
				}
			}
			if tt.checkFragmentRatio {
				if rec.FragmentRatio != tt.wantFragmentRatio {
					t.Errorf("FragmentRatio = %v, want %v",
						rec.FragmentRatio, tt.wantFragmentRatio)
				}
			}
		})
	}
}

func TestObserveDensityWritesJSON(t *testing.T) {
	dir := t.TempDir()
	t.Setenv(BarokDensityEnvDir, dir)

	meta := DensityMeta{
		WorkspaceID:   "ws123",
		Persona:       "architect",
		PhaseID:       "design",
		PhaseName:     "Design the thing",
		IntensityTier: "LiteSentence",
		ArtifactPath:  "/tmp/foo/DESIGN.md",
	}
	ObserveDensity([]byte("the cat sat on the mat"), meta)

	entries, err := os.ReadDir(dir)
	if err != nil {
		t.Fatalf("reading density dir: %v", err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 record file, got %d", len(entries))
	}

	data, err := os.ReadFile(filepath.Join(dir, entries[0].Name()))
	if err != nil {
		t.Fatalf("reading record: %v", err)
	}

	var rec DensityRecord
	if err := json.Unmarshal(data, &rec); err != nil {
		t.Fatalf("unmarshaling record: %v", err)
	}

	if rec.WorkspaceID != meta.WorkspaceID {
		t.Errorf("WorkspaceID = %q, want %q", rec.WorkspaceID, meta.WorkspaceID)
	}
	if rec.IntensityTier != meta.IntensityTier {
		t.Errorf("IntensityTier = %q, want %q", rec.IntensityTier, meta.IntensityTier)
	}
	if rec.ArticleCount != 2 {
		t.Errorf("ArticleCount = %d, want 2", rec.ArticleCount)
	}
	if rec.ComputedAt.IsZero() {
		t.Error("ComputedAt is zero; expected current UTC time")
	}
}

func TestObserveDensitySwallowsWriteFailure(t *testing.T) {
	// Point the env var at a path that cannot be created (a file, not a dir).
	blocker := filepath.Join(t.TempDir(), "blocker")
	if err := os.WriteFile(blocker, []byte("x"), 0o600); err != nil {
		t.Fatalf("preparing blocker file: %v", err)
	}
	// Use the blocker file as a parent dir — MkdirAll will fail.
	t.Setenv(BarokDensityEnvDir, filepath.Join(blocker, "cannot-create-here"))

	// Must not panic, must not return error (no return value).
	ObserveDensity([]byte("the cat sat on the mat"), DensityMeta{
		WorkspaceID: "ws",
		Persona:     "architect",
		PhaseID:     "p",
	})
}

func TestBarokIntensityTier(t *testing.T) {
	tests := []struct {
		persona string
		want    string
	}{
		{"technical-writer", "LiteFragment"},
		{"academic-researcher", "LiteFragment"},
		{"architect", "LiteSentence"},
		{"data-analyst", "LiteSentence"},
		{"staff-code-reviewer", "LiteNarrative"},
		{"senior-backend-engineer", ""},
		{"", ""},
	}
	for _, tt := range tests {
		t.Run(tt.persona, func(t *testing.T) {
			got := BarokIntensityTier(tt.persona)
			if got != tt.want {
				t.Errorf("BarokIntensityTier(%q) = %q, want %q", tt.persona, got, tt.want)
			}
		})
	}
}
