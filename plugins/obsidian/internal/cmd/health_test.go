package cmd

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

func TestCountOrphans(t *testing.T) {
	notes := []vault.NoteInfo{
		{Path: "Ideas/my-idea.md", Name: "my-idea"},
		{Path: "Notes/meeting.md", Name: "meeting"},
		{Path: "References/article.md", Name: "article"},
	}

	tests := []struct {
		name          string
		inboundLinks  map[string]int
		wantOrphans   int
	}{
		{
			name:         "all orphans when no links",
			inboundLinks: map[string]int{},
			wantOrphans:  3,
		},
		{
			name: "note linked by name reduces orphan count",
			inboundLinks: map[string]int{
				"my-idea": 2,
				"article": 1,
			},
			wantOrphans: 1, // only meeting is orphaned
		},
		{
			name: "note linked by path reduces orphan count",
			inboundLinks: map[string]int{
				"notes/meeting": 1,
			},
			wantOrphans: 2, // my-idea and article are orphaned
		},
		{
			name: "all notes linked",
			inboundLinks: map[string]int{
				"my-idea": 1,
				"meeting": 1,
				"article": 1,
			},
			wantOrphans: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := countOrphans(notes, tt.inboundLinks)
			if got != tt.wantOrphans {
				t.Errorf("countOrphans() = %d, want %d", got, tt.wantOrphans)
			}
		})
	}
}

func TestNoteClassificationDist(t *testing.T) {
	tests := []struct {
		name  string
		notes []vault.NoteInfo
		want  map[string]int
	}{
		{
			name:  "empty vault",
			notes: []vault.NoteInfo{},
			want:  map[string]int{},
		},
		{
			name: "notes in multiple folders",
			notes: []vault.NoteInfo{
				{Path: "Ideas/idea-a.md"},
				{Path: "Ideas/idea-b.md"},
				{Path: "Notes/note-a.md"},
				{Path: "inbox/capture.md"},
			},
			want: map[string]int{
				"Ideas": 2,
				"Notes": 1,
				"inbox": 1,
			},
		},
		{
			name: "notes at vault root counted as Root",
			notes: []vault.NoteInfo{
				{Path: "readme.md"},
				{Path: "index.md"},
				{Path: "Notes/note.md"},
			},
			want: map[string]int{
				"Root":  2,
				"Notes": 1,
			},
		},
		{
			name: "deeply nested notes use top-level folder",
			notes: []vault.NoteInfo{
				{Path: "Daily/2026/03/2026-03-17.md"},
				{Path: "Daily/2026/03/2026-03-16.md"},
				{Path: "Projects/obsidian/health.md"},
			},
			want: map[string]int{
				"Daily":    2,
				"Projects": 1,
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := noteClassificationDist(tt.notes)
			if len(got) != len(tt.want) {
				t.Errorf("noteClassificationDist() len = %d, want %d; got %v", len(got), len(tt.want), got)
				return
			}
			for folder, wantCount := range tt.want {
				if got[folder] != wantCount {
					t.Errorf("noteClassificationDist()[%q] = %d, want %d", folder, got[folder], wantCount)
				}
			}
		})
	}
}

// runHealthJSON runs HealthCmd in JSON mode and returns the decoded output.
func runHealthJSON(t *testing.T, vaultDir string) HealthOutput {
	t.Helper()
	var cmdErr error
	raw := captureStdout(t, func() {
		cmdErr = HealthCmd(vaultDir, true)
	})
	if cmdErr != nil {
		t.Fatalf("HealthCmd: %v", cmdErr)
	}
	var out HealthOutput
	if err := json.NewDecoder(strings.NewReader(raw)).Decode(&out); err != nil {
		t.Fatalf("decode JSON: %v", err)
	}
	return out
}

func writeVaultNote(t *testing.T, vaultDir, relPath, content string) {
	t.Helper()
	fullPath := filepath.Join(vaultDir, relPath)
	if err := os.MkdirAll(filepath.Dir(fullPath), 0700); err != nil {
		t.Fatalf("mkdir %s: %v", filepath.Dir(fullPath), err)
	}
	if err := os.WriteFile(fullPath, []byte(content), 0600); err != nil {
		t.Fatalf("write %s: %v", fullPath, err)
	}
}

func TestHealthCmd(t *testing.T) {
	dir := t.TempDir()

	// Inbox note — pending, not stale (no created date means it uses modtime which is now)
	writeVaultNote(t, dir, "inbox/fresh-capture.md", `---
title: fresh capture
status: pending
---
A new capture.
`)

	// Inbox note — processed (should not count toward inbox depth)
	writeVaultNote(t, dir, "inbox/done-capture.md", `---
title: done capture
status: processed
---
Already processed.
`)

	// Ideas note with an outbound wikilink to meeting
	writeVaultNote(t, dir, "Ideas/my-idea.md", `---
title: My Idea
type: idea
---
See also [[meeting]].
`)

	// Notes note that is linked (meeting)
	writeVaultNote(t, dir, "Notes/meeting.md", `---
title: Meeting Notes
type: note
---
Discussion points.
`)

	// Notes note that is an orphan — nothing links to it
	writeVaultNote(t, dir, "Notes/orphan.md", `---
title: Orphan Note
type: note
---
Nobody links here.
`)

	t.Run("json output parses correctly", func(t *testing.T) {
		out := runHealthJSON(t, dir)

		// 5 notes total: 2 Inbox + 1 Ideas + 2 Notes.
		if out.TotalNotes != 5 {
			t.Errorf("TotalNotes = %d, want 5", out.TotalNotes)
		}

		// Only fresh-capture is pending.
		if out.InboxDepth != 1 {
			t.Errorf("inboxDepth = %d, want 1", out.InboxDepth)
		}

		// fresh-capture has no created date and was just written, so age ≈ 0 days — not stale.
		if out.StaleCaptures != 0 {
			t.Errorf("StaleCaptures = %d, want 0", out.StaleCaptures)
		}

		// orphan.md has no inbound links; fresh-capture and done-capture also have no inbound links.
		// my-idea is not linked either; only meeting is linked (from my-idea).
		// So orphans = TotalNotes - 1 = 4.
		if out.OrphanNotes != 4 {
			t.Errorf("OrphanNotes = %d, want 4", out.OrphanNotes)
		}

		// my-idea has 1 wikilink; other notes have 0. Total = 1 across 5 notes → 0.2.
		wantDensity := 1.0 / 5.0
		if out.LinkDensity != wantDensity {
			t.Errorf("LinkDensity = %f, want %f", out.LinkDensity, wantDensity)
		}

		// Classification distribution should have Inbox, Ideas, Notes entries.
		if out.ClassificationDist["inbox"] != 2 {
			t.Errorf("ClassificationDist[Inbox] = %d, want 2", out.ClassificationDist["inbox"])
		}
		if out.ClassificationDist["Ideas"] != 1 {
			t.Errorf("ClassificationDist[Ideas] = %d, want 1", out.ClassificationDist["Ideas"])
		}
		if out.ClassificationDist["Notes"] != 2 {
			t.Errorf("ClassificationDist[Notes] = %d, want 2", out.ClassificationDist["Notes"])
		}
	})

	t.Run("text output does not error", func(t *testing.T) {
		if err := HealthCmd(dir, false); err != nil {
			t.Fatalf("HealthCmd text mode: %v", err)
		}
	})
}

func TestHealthCmdStaleCaptures(t *testing.T) {
	dir := t.TempDir()

	// Write an inbox note with a created date well in the past (> 7 days).
	writeVaultNote(t, dir, "inbox/old-capture.md", `---
title: old capture
status: pending
created: 2020-01-01
---
A very old capture.
`)

	out := runHealthJSON(t, dir)

	if out.InboxDepth != 1 {
		t.Errorf("inboxDepth = %d, want 1", out.InboxDepth)
	}
	if out.StaleCaptures != 1 {
		t.Errorf("StaleCaptures = %d, want 1 (note is >7d old)", out.StaleCaptures)
	}
}

func TestHealthCmdEmptyVault(t *testing.T) {
	dir := t.TempDir()
	out := runHealthJSON(t, dir)

	if out.TotalNotes != 0 {
		t.Errorf("TotalNotes = %d, want 0", out.TotalNotes)
	}
	if out.LinkDensity != 0 {
		t.Errorf("LinkDensity = %f, want 0", out.LinkDensity)
	}
	if out.InboxDepth != 0 {
		t.Errorf("inboxDepth = %d, want 0", out.InboxDepth)
	}
}

func TestAvgWikilinkDensity(t *testing.T) {
	tests := []struct {
		name       string
		totalLinks int
		noteCount  int
		want       float64
	}{
		{
			name:       "zero notes returns zero",
			totalLinks: 0,
			noteCount:  0,
			want:       0,
		},
		{
			name:       "no links in vault",
			totalLinks: 0,
			noteCount:  10,
			want:       0,
		},
		{
			name:       "even distribution",
			totalLinks: 20,
			noteCount:  4,
			want:       5.0,
		},
		{
			name:       "fractional density",
			totalLinks: 7,
			noteCount:  3,
			want:       7.0 / 3.0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := avgWikilinkDensity(tt.totalLinks, tt.noteCount)
			if got != tt.want {
				t.Errorf("avgWikilinkDensity(%d, %d) = %v, want %v", tt.totalLinks, tt.noteCount, got, tt.want)
			}
		})
	}
}
