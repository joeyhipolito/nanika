package engine

import (
	"os"
	"path/filepath"
	"testing"
)

// longOutput is a >100-char string that passes the existence and format gates.
const longOutput = "This is a sufficiently long output string that exceeds the one-hundred character minimum required by the format gate check."

func TestCheckGateArtifactPaths(t *testing.T) {
	// Create a real file for the "match" cases.
	tmpDir := t.TempDir()
	existingFile := filepath.Join(tmpDir, "artifact.txt")
	if err := os.WriteFile(existingFile, []byte("ok"), 0644); err != nil {
		t.Fatalf("creating temp file: %v", err)
	}

	tests := []struct {
		name          string
		output        string
		expectedPaths []string
		wantPassed    bool
	}{
		{
			name:          "empty paths — backward compat, gate passes",
			output:        longOutput,
			expectedPaths: nil,
			wantPassed:    true,
		},
		{
			name:          "matching glob pattern — gate passes",
			output:        longOutput,
			expectedPaths: []string{filepath.Join(tmpDir, "*.txt")},
			wantPassed:    true,
		},
		{
			name:          "non-matching glob pattern — gate fails",
			output:        longOutput,
			expectedPaths: []string{filepath.Join(tmpDir, "*.png")},
			wantPassed:    false,
		},
		{
			name:          "mix of matching and non-matching — gate fails on missing",
			output:        longOutput,
			expectedPaths: []string{filepath.Join(tmpDir, "*.txt"), filepath.Join(tmpDir, "*.png")},
			wantPassed:    false,
		},
		{
			name:          "explicit file path that exists — gate passes",
			output:        longOutput,
			expectedPaths: []string{existingFile},
			wantPassed:    true,
		},
		{
			name:          "explicit file path that does not exist — gate fails",
			output:        longOutput,
			expectedPaths: []string{filepath.Join(tmpDir, "missing.txt")},
			wantPassed:    false,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			result := CheckGate(tt.output, tt.expectedPaths)
			if result.Passed != tt.wantPassed {
				t.Errorf("Passed = %v; want %v (Reason: %q)", result.Passed, tt.wantPassed, result.Reason)
			}
			if !result.Passed && result.Reason == "" {
				t.Error("Passed=false but Reason is empty")
			}
		})
	}
}

func TestParseExpectedPaths(t *testing.T) {
	home := os.Getenv("HOME")

	tests := []struct {
		name       string
		expected   string
		taskHeader string
		want       []string
	}{
		{
			name:       "empty expected returns nil",
			expected:   "",
			taskHeader: "",
			want:       nil,
		},
		{
			name:       "single path no substitution",
			expected:   "/tmp/file.txt",
			taskHeader: "",
			want:       []string{"/tmp/file.txt"},
		},
		{
			name:       "tilde expanded to HOME",
			expected:   "~/blog/post.mdx",
			taskHeader: "",
			want:       []string{filepath.Join(home, "blog/post.mdx")},
		},
		{
			name:       "slug resolved from Slug line",
			expected:   "~/blog/{slug}.mdx",
			taskHeader: "Slug: my-article\nType: post",
			want:       []string{filepath.Join(home, "blog/my-article.mdx")},
		},
		{
			name:       "slug derived from Target filename",
			expected:   "~/output/{slug}.png",
			taskHeader: "Target: ~/blog/posts/my-post.mdx",
			want:       []string{filepath.Join(home, "output/my-post.png")},
		},
		{
			name:       "comma-separated paths split correctly",
			expected:   "/tmp/a.txt, /tmp/b.txt",
			taskHeader: "",
			want:       []string{"/tmp/a.txt", "/tmp/b.txt"},
		},
		{
			name:       "empty segments ignored",
			expected:   "/tmp/a.txt, , /tmp/b.txt",
			taskHeader: "",
			want:       []string{"/tmp/a.txt", "/tmp/b.txt"},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := parseExpectedPaths(tt.expected, tt.taskHeader)
			if len(got) != len(tt.want) {
				t.Fatalf("got %v; want %v", got, tt.want)
			}
			for i := range got {
				if got[i] != tt.want[i] {
					t.Errorf("path[%d] = %q; want %q", i, got[i], tt.want[i])
				}
			}
		})
	}
}
