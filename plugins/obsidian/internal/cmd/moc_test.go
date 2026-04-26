package cmd

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// fakeDrafter is a test mock implementing the Drafter interface.
type fakeDrafter struct {
	draft string
	err   error
}

func (f *fakeDrafter) Draft(ctx context.Context, c Concept, zettels []zettelDigest) (string, error) {
	return f.draft, f.err
}

// fixtureNote is a minimal vault file written to the test vault.
type fixtureNote struct {
	relPath  string
	contents string
}

func writeFixtureVault(t *testing.T, notes []fixtureNote) string {
	t.Helper()
	root := t.TempDir()
	for _, n := range notes {
		full := filepath.Join(root, n.relPath)
		if err := os.MkdirAll(filepath.Dir(full), 0o755); err != nil {
			t.Fatalf("mkdir %s: %v", full, err)
		}
		if err := os.WriteFile(full, []byte(n.contents), 0o644); err != nil {
			t.Fatalf("write %s: %v", full, err)
		}
	}
	return root
}

// taggedZettel returns a frontmatter-only fixture with the given tags.
func taggedZettel(tags ...string) string {
	if len(tags) == 0 {
		return "---\n---\n\nbody\n"
	}
	tagList := "[" + strings.Join(tags, ", ") + "]"
	return "---\ntags: " + tagList + "\n---\n\nbody about " + tags[0] + "\n"
}

func TestDetectConcepts(t *testing.T) {
	tests := []struct {
		name  string
		notes []fixtureNote
		want  []Concept // expected concepts in slug-sorted order
	}{
		{
			name: "single concept above threshold",
			notes: []fixtureNote{
				{"missions/m1.md", taggedZettel("memory-layer")},
				{"missions/m2.md", taggedZettel("memory-layer")},
				{"daily/d1.md", taggedZettel("memory-layer", "other")},
				{"daily/d2.md", taggedZettel("memory-layer")},
				{"sessions/s1.md", taggedZettel("memory-layer")},
			},
			want: []Concept{{
				Slug: "memory-layer",
				Tag:  "memory-layer",
				Zettels: []string{
					"daily/d1.md", "daily/d2.md",
					"missions/m1.md", "missions/m2.md",
					"sessions/s1.md",
				},
			}},
		},
		{
			name: "below threshold drops the concept",
			notes: []fixtureNote{
				{"missions/m1.md", taggedZettel("rare")},
				{"missions/m2.md", taggedZettel("rare")},
				{"daily/d1.md", taggedZettel("rare")},
				{"daily/d2.md", taggedZettel("rare")},
				// only 4 — below mocMinZettels=5
			},
			want: nil,
		},
		{
			name: "duplicate tag entries on one zettel count once",
			notes: []fixtureNote{
				{"missions/m1.md", taggedZettel("dup", "dup", "dup")}, // 3 entries, one zettel
				{"missions/m2.md", taggedZettel("dup")},
				{"daily/d1.md", taggedZettel("dup")},
				{"daily/d2.md", taggedZettel("dup")},
				// total distinct zettels = 4 → below threshold despite 6 tag occurrences
			},
			want: nil,
		},
		{
			name: "multiple concepts sorted by slug",
			notes: []fixtureNote{
				{"missions/a1.md", taggedZettel("zeta")},
				{"missions/a2.md", taggedZettel("zeta")},
				{"missions/a3.md", taggedZettel("zeta")},
				{"missions/a4.md", taggedZettel("zeta")},
				{"missions/a5.md", taggedZettel("zeta")},
				{"daily/b1.md", taggedZettel("alpha")},
				{"daily/b2.md", taggedZettel("alpha")},
				{"daily/b3.md", taggedZettel("alpha")},
				{"daily/b4.md", taggedZettel("alpha")},
				{"daily/b5.md", taggedZettel("alpha")},
			},
			want: []Concept{
				{Slug: "alpha", Tag: "alpha"},
				{Slug: "zeta", Tag: "zeta"},
			},
		},
		{
			name: "mocs/ folder is not scanned",
			notes: []fixtureNote{
				// 5 mocs/ files with the same tag must NOT count.
				{"mocs/x1.md", taggedZettel("feedback-loop")},
				{"mocs/x2.md", taggedZettel("feedback-loop")},
				{"mocs/x3.md", taggedZettel("feedback-loop")},
				{"mocs/x4.md", taggedZettel("feedback-loop")},
				{"mocs/x5.md", taggedZettel("feedback-loop")},
			},
			want: nil,
		},
		{
			name: "hidden directory and dotfile skipped",
			notes: []fixtureNote{
				{"missions/.trash/old.md", taggedZettel("ghost")},
				{"missions/.hidden.md", taggedZettel("ghost")},
				{"missions/m1.md", taggedZettel("ghost")},
				{"missions/m2.md", taggedZettel("ghost")},
				{"missions/m3.md", taggedZettel("ghost")},
				{"missions/m4.md", taggedZettel("ghost")},
				// only 4 visible — below threshold
			},
			want: nil,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vault := writeFixtureVault(t, tt.notes)
			got, err := detectConcepts(vault)
			if err != nil {
				t.Fatalf("detectConcepts: %v", err)
			}
			if len(got) != len(tt.want) {
				t.Fatalf("got %d concepts, want %d (got=%+v)", len(got), len(tt.want), got)
			}
			for i, w := range tt.want {
				if got[i].Slug != w.Slug {
					t.Errorf("concept[%d] slug = %q, want %q", i, got[i].Slug, w.Slug)
				}
				if got[i].Tag != w.Tag {
					t.Errorf("concept[%d] tag = %q, want %q", i, got[i].Tag, w.Tag)
				}
				if w.Zettels != nil {
					if !equalSorted(got[i].Zettels, w.Zettels) {
						t.Errorf("concept[%d] zettels = %v, want %v", i, got[i].Zettels, w.Zettels)
					}
				}
			}
		})
	}
}

func equalSorted(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}

func TestIsTombstoned(t *testing.T) {
	tests := []struct {
		name      string
		mocFile   string // contents of mocs/foo.md; empty = file does not exist
		want      bool
		wantErr   bool
	}{
		{
			name:    "no moc file present",
			mocFile: "",
			want:    false,
		},
		{
			name:    "moc file with status: rejected suppresses concept",
			mocFile: "---\nstatus: rejected\ntitle: Foo\n---\n\nbody\n",
			want:    true,
		},
		{
			name:    "moc file with status: active suppresses concept",
			mocFile: "---\nstatus: active\n---\n",
			want:    true,
		},
		{
			name:    "moc file with status: draft suppresses concept (any value)",
			mocFile: "---\nstatus: draft\n---\n",
			want:    true,
		},
		{
			name:    "moc file without status field still skipped (defensive)",
			mocFile: "---\ntitle: Hand-Authored\n---\n\nbody\n",
			want:    true,
		},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			vaultDir := t.TempDir()
			if tt.mocFile != "" {
				if err := os.MkdirAll(filepath.Join(vaultDir, "mocs"), 0o755); err != nil {
					t.Fatalf("mkdir: %v", err)
				}
				if err := os.WriteFile(filepath.Join(vaultDir, "mocs", "foo.md"), []byte(tt.mocFile), 0o644); err != nil {
					t.Fatalf("write: %v", err)
				}
			}
			got, err := isTombstoned(vaultDir, "foo")
			if (err != nil) != tt.wantErr {
				t.Fatalf("err = %v, wantErr = %v", err, tt.wantErr)
			}
			if got != tt.want {
				t.Errorf("isTombstoned = %v, want %v", got, tt.want)
			}
		})
	}
}

func TestTombstoneSuppressesInDryRun(t *testing.T) {
	// 5 zettels tagged `feedback-loop` — would normally produce a concept.
	vault := writeFixtureVault(t, []fixtureNote{
		{"missions/m1.md", taggedZettel("feedback-loop")},
		{"missions/m2.md", taggedZettel("feedback-loop")},
		{"missions/m3.md", taggedZettel("feedback-loop")},
		{"daily/d1.md", taggedZettel("feedback-loop")},
		{"daily/d2.md", taggedZettel("feedback-loop")},
		// existing tombstoned MOC for the concept
		{"mocs/feedback-loop.md", "---\nstatus: rejected\n---\n\nuser said no.\n"},
	})

	concepts, err := detectConcepts(vault)
	if err != nil {
		t.Fatalf("detectConcepts: %v", err)
	}
	if len(concepts) != 1 || concepts[0].Slug != "feedback-loop" {
		t.Fatalf("expected one feedback-loop concept, got %+v", concepts)
	}

	tombstoned, err := isTombstoned(vault, "feedback-loop")
	if err != nil {
		t.Fatalf("isTombstoned: %v", err)
	}
	if !tombstoned {
		t.Fatalf("expected tombstoned=true for status:rejected MOC")
	}
}

// TestMOCDryRun_NoHaiku exercises the full MOCCmd dry-run path end-to-end.
// It MUST NOT call the Anthropic API or write any MOC files.
func TestMOCDryRun_NoHaiku(t *testing.T) {
	t.Setenv("ANTHROPIC_API_KEY", "") // prove no network call is required
	t.Setenv("NANIKA_WORKSPACE_SHARED", "")

	vault := writeFixtureVault(t, []fixtureNote{
		{"missions/m1.md", taggedZettel("memory-layer")},
		{"missions/m2.md", taggedZettel("memory-layer")},
		{"daily/d1.md", taggedZettel("memory-layer")},
		{"daily/d2.md", taggedZettel("memory-layer")},
		{"sessions/s1.md", taggedZettel("memory-layer")},
		// a tombstoned concept that should be candidate-listed but visibly skipped
		// (dry-run prints all candidates including would-be tombstoned, which is
		// intentional — the verify phase wants the full list)
	})

	stdout := captureStdout(t, func() {
		if err := MOCCmd(vault, MOCOptions{DryRun: true}); err != nil {
			t.Fatalf("MOCCmd dry-run: %v", err)
		}
	})

	// Candidate list must be present.
	if !strings.Contains(stdout, "[dry-run] concept=memory-layer") {
		t.Errorf("dry-run output missing candidate line: %q", stdout)
	}
	// Summary line at end.
	if !strings.Contains(stdout, "candidates=1") || !strings.Contains(stdout, "(dry-run)") {
		t.Errorf("dry-run output missing summary: %q", stdout)
	}
	// Nothing was written.
	if _, err := os.Stat(filepath.Join(vault, "mocs", "memory-layer.md")); !os.IsNotExist(err) {
		t.Errorf("dry-run wrote a MOC file: err=%v", err)
	}
}

func TestWriteMOC_Frontmatter(t *testing.T) {
	vault := t.TempDir()
	if err := os.MkdirAll(filepath.Join(vault, "mocs"), 0o755); err != nil {
		t.Fatalf("mkdir: %v", err)
	}
	c := Concept{
		Slug:    "chat-stack",
		Tag:     "chat-stack",
		Zettels: []string{"daily/d1.md", "missions/m1.md"},
	}
	now := time.Date(2026, 4, 22, 12, 0, 0, 0, time.UTC)
	if err := writeMOC(vault, c, "## Summary\nbody.\n", now); err != nil {
		t.Fatalf("writeMOC: %v", err)
	}
	raw, err := os.ReadFile(filepath.Join(vault, "mocs", "chat-stack.md"))
	if err != nil {
		t.Fatalf("read: %v", err)
	}
	got := string(raw)

	wantSubs := []string{
		"type: moc",
		"status: draft",
		"title: Chat Stack",
		"  - chat-stack",
		"  - auto-drafted",
		"generated: 2026-04-22",
		"contributing_zettels:",
		"  - daily/d1.md",
		"  - missions/m1.md",
		"## Summary",
	}
	for _, sub := range wantSubs {
		if !strings.Contains(got, sub) {
			t.Errorf("MOC missing %q. got:\n%s", sub, got)
		}
	}
}

func TestConceptDisplayName(t *testing.T) {
	cases := map[string]string{
		"chat-stack":      "Chat Stack",
		"memory-layer":    "Memory Layer",
		"single":          "Single",
		"a-b-c":           "A B C",
	}
	for in, want := range cases {
		if got := conceptDisplayName(in); got != want {
			t.Errorf("conceptDisplayName(%q) = %q, want %q", in, got, want)
		}
	}
}

func TestNewMOCDrafter_NotOnPath(t *testing.T) {
	t.Setenv("PATH", t.TempDir())
	drafter := NewMOCDrafter()
	if drafter != nil {
		t.Errorf("NewMOCDrafter should return nil when claude CLI is not on PATH, got %v", drafter)
	}
}

// TestMOCCmd_WithFakeDrafter exercises the live (non-dry-run) MOCCmd path end-to-end
// using fakeDrafter so no claude CLI or network call is required.
func TestMOCCmd_WithFakeDrafter(t *testing.T) {
	t.Setenv("NANIKA_WORKSPACE_SHARED", "")

	vaultDir := writeFixtureVault(t, []fixtureNote{
		{"missions/m1.md", taggedZettel("chat-stack")},
		{"missions/m2.md", taggedZettel("chat-stack")},
		{"daily/d1.md", taggedZettel("chat-stack")},
		{"daily/d2.md", taggedZettel("chat-stack")},
		{"sessions/s1.md", taggedZettel("chat-stack")},
	})

	fd := &fakeDrafter{draft: "## Summary\nfake body injected by test.\n"}
	if err := MOCCmd(vaultDir, MOCOptions{Drafter: fd}); err != nil {
		t.Fatalf("MOCCmd: %v", err)
	}

	raw, err := os.ReadFile(filepath.Join(vaultDir, "mocs", "chat-stack.md"))
	if err != nil {
		t.Fatalf("read moc: %v", err)
	}
	got := string(raw)
	for _, want := range []string{"status: draft", "type: moc", "fake body injected by test"} {
		if !strings.Contains(got, want) {
			t.Errorf("MOC missing %q:\n%s", want, got)
		}
	}
}

// TestMOCCmd_Drafter_Error verifies that a per-concept drafter failure increments
// the failed counter and does not write a MOC file.
func TestMOCCmd_Drafter_Error(t *testing.T) {
	t.Setenv("NANIKA_WORKSPACE_SHARED", "")

	vaultDir := writeFixtureVault(t, []fixtureNote{
		{"missions/m1.md", taggedZettel("error-concept")},
		{"missions/m2.md", taggedZettel("error-concept")},
		{"daily/d1.md", taggedZettel("error-concept")},
		{"daily/d2.md", taggedZettel("error-concept")},
		{"sessions/s1.md", taggedZettel("error-concept")},
	})

	fd := &fakeDrafter{err: fmt.Errorf("claude CLI exploded")}

	stdout := captureStdout(t, func() {
		if err := MOCCmd(vaultDir, MOCOptions{Drafter: fd}); err != nil {
			t.Fatalf("MOCCmd: %v", err)
		}
	})

	if !strings.Contains(stdout, "failed=1") {
		t.Errorf("expected failed=1 in summary, got: %q", stdout)
	}
	if _, err := os.Stat(filepath.Join(vaultDir, "mocs", "error-concept.md")); !os.IsNotExist(err) {
		t.Errorf("MOC file should not exist after drafter error: err=%v", err)
	}
}

func TestExtractSlugTokens(t *testing.T) {
	tests := []struct {
		stem string
		want []string
	}{
		// date prefix stripped (pure digit token dropped)
		{stem: "20260422-memory-layer", want: []string{"memory", "layer"}},
		// year-like token dropped
		{stem: "2026-planning-session", want: []string{"planning", "session"}},
		// trk-id tokens dropped (trk alone and trk+digits)
		{stem: "trk-419-design-doc", want: []string{"design", "doc"}},
		{stem: "trk419-voice-drift", want: []string{"voice", "drift"}},
		// short tokens (len<3) dropped
		{stem: "a-bc-def-long", want: []string{"def", "long"}},
		// workflow-verb stopwords dropped; concept words retained
		{stem: "phase-implement-dashboard-polish", want: []string{"dashboard", "polish"}},
		{stem: "fix-review-verify-chat-stack", want: []string{"chat", "stack"}},
		// "polish" is NOT a stopword — it's a real concept signal
		{stem: "polish-refactoring-guide", want: []string{"polish", "refactoring", "guide"}},
		// all-digit stem yields empty
		{stem: "123-456-789", want: []string{}},
		// mixed: date + trk + valid tokens
		{stem: "20260422-trk419-voice-drift", want: []string{"voice", "drift"}},
		// lowercase normalisation
		{stem: "Memory-LAYER", want: []string{"memory", "layer"}},
	}

	// Frontmatter slug override: when Slug is set, Stem is ignored.
	t.Run("frontmatter_slug_overrides_stem", func(t *testing.T) {
		z := zettelDigest{Stem: "2026-04-22-messy-filename", Slug: "chat-ux-polish"}
		got := extractSlugTokens(z)
		want := []string{"chat", "polish"}
		if len(got) != len(want) {
			t.Fatalf("got %v, want %v", got, want)
		}
		for i := range want {
			if got[i] != want[i] {
				t.Errorf("token[%d]: got %q, want %q", i, got[i], want[i])
			}
		}
	})

	for _, tt := range tests {
		t.Run(tt.stem, func(t *testing.T) {
			z := zettelDigest{Stem: tt.stem}
			got := extractSlugTokens(z)
			if len(got) != len(tt.want) {
				t.Fatalf("extractSlugTokens(%q) = %v, want %v", tt.stem, got, tt.want)
			}
			for i := range tt.want {
				if got[i] != tt.want[i] {
					t.Errorf("token[%d]: got %q, want %q", i, got[i], tt.want[i])
				}
			}
		})
	}
}

// polishZettel returns a fixture .md note whose filename stem contains "polish".
func polishZettel(n int) fixtureNote {
	return fixtureNote{
		relPath:  fmt.Sprintf("missions/polish-session-%02d.md", n),
		contents: "---\n---\n\nbody\n",
	}
}

func TestDetectConceptsBySlugToken(t *testing.T) {
	t.Run("five zettels sharing polish token produces one concept", func(t *testing.T) {
		// Build zettelDigests with stems that all include "polish".
		zettels := []zettelDigest{
			{RelPath: "missions/polish-session-01.md", Stem: "polish-session-01"},
			{RelPath: "missions/polish-session-02.md", Stem: "polish-session-02"},
			{RelPath: "missions/polish-session-03.md", Stem: "polish-session-03"},
			{RelPath: "daily/polish-notes-day1.md", Stem: "polish-notes-day1"},
			{RelPath: "daily/polish-notes-day2.md", Stem: "polish-notes-day2"},
		}
		concepts := detectConceptsBySlugToken(zettels, 5)
		var found *Concept
		for i := range concepts {
			if concepts[i].Slug == "polish" {
				found = &concepts[i]
				break
			}
		}
		if found == nil {
			t.Fatalf("expected concept with slug 'polish', got %+v", concepts)
		}
		if len(found.Zettels) != 5 {
			t.Errorf("polish concept has %d zettels, want 5", len(found.Zettels))
		}
	})

	t.Run("fewer than five zettels sharing polish token produces no concept", func(t *testing.T) {
		zettels := []zettelDigest{
			{RelPath: "missions/polish-session-01.md", Stem: "polish-session-01"},
			{RelPath: "missions/polish-session-02.md", Stem: "polish-session-02"},
			{RelPath: "missions/polish-session-03.md", Stem: "polish-session-03"},
			{RelPath: "daily/polish-notes-day1.md", Stem: "polish-notes-day1"},
			// only 4
		}
		concepts := detectConceptsBySlugToken(zettels, 5)
		for _, c := range concepts {
			if c.Slug == "polish" {
				t.Errorf("unexpected concept 'polish' with only 4 zettels: %+v", c)
			}
		}
	})
}

func TestDetectConcepts_UnionDedup(t *testing.T) {
	// "polish" appears both as a frontmatter tag AND as a slug token.
	// The union must emit exactly one concept; the tag-based entry wins
	// (Tag field = original tag, not the token string).
	notes := []fixtureNote{
		{"missions/polish-session-01.md", taggedZettel("polish")},
		{"missions/polish-session-02.md", taggedZettel("polish")},
		{"missions/polish-session-03.md", taggedZettel("polish")},
		{"daily/polish-notes-01.md", taggedZettel("polish")},
		{"daily/polish-notes-02.md", taggedZettel("polish")},
	}
	vault := writeFixtureVault(t, notes)
	concepts, err := detectConcepts(vault)
	if err != nil {
		t.Fatalf("detectConcepts: %v", err)
	}
	count := 0
	for _, c := range concepts {
		if c.Slug == "polish" {
			count++
			// tag-based wins: Tag must equal the original frontmatter tag
			if c.Tag != "polish" {
				t.Errorf("dedup winner Tag = %q, want %q", c.Tag, "polish")
			}
		}
	}
	if count != 1 {
		t.Errorf("expected exactly 1 'polish' concept after dedup, got %d (all: %+v)", count, concepts)
	}
}

