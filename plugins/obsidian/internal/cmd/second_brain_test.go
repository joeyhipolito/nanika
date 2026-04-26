package cmd

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// TestCapture_SecondBrain verifies that CaptureCmd places a fleeting note in
// the SecondBrain inbox folder when vault.KindSecondBrain is passed.
func TestCapture_SecondBrain(t *testing.T) {
	vaultDir := t.TempDir()
	if err := vault.InitSkeleton(vaultDir, vault.KindSecondBrain); err != nil {
		t.Fatalf("InitSkeleton: %v", err)
	}

	if err := CaptureCmd(vaultDir, "second-brain integration capture", "", false, vault.KindSecondBrain); err != nil {
		t.Fatalf("CaptureCmd: %v", err)
	}

	inboxPath := filepath.Join(vaultDir, vault.SecondBrainInbox)
	entries, err := os.ReadDir(inboxPath)
	if err != nil {
		t.Fatalf("ReadDir(%s): %v", vault.SecondBrainInbox, err)
	}
	if len(entries) != 1 {
		t.Fatalf("expected 1 note in %s/, got %d", vault.SecondBrainInbox, len(entries))
	}

	data, err := os.ReadFile(filepath.Join(inboxPath, entries[0].Name()))
	if err != nil {
		t.Fatalf("ReadFile: %v", err)
	}
	content := string(data)
	if !strings.Contains(content, "second-brain integration capture") {
		t.Error("capture body not present in note")
	}
	if !strings.Contains(content, "type: "+vault.TypeFleeting) {
		t.Errorf("type: %s not found in note", vault.TypeFleeting)
	}
}

// TestList_SecondBrain verifies that ListCmd finds notes placed in the
// SecondBrain inbox folder of a second-brain vault.
func TestList_SecondBrain(t *testing.T) {
	vaultDir := t.TempDir()
	if err := vault.InitSkeleton(vaultDir, vault.KindSecondBrain); err != nil {
		t.Fatalf("InitSkeleton: %v", err)
	}

	notePath := filepath.Join(vaultDir, vault.SecondBrainInbox, "test-note.md")
	content := "---\ntype: fleeting\ncreated: 2026-04-22\n---\nTest content for list.\n"
	if err := os.WriteFile(notePath, []byte(content), 0644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	if err := ListCmd(vaultDir, vault.SecondBrainInbox, false); err != nil {
		t.Fatalf("ListCmd: %v", err)
	}

	notes, err := vault.ListNotes(vaultDir, vault.SecondBrainInbox)
	if err != nil {
		t.Fatalf("ListNotes: %v", err)
	}
	if len(notes) != 1 {
		t.Fatalf("expected 1 note in %s/, got %d", vault.SecondBrainInbox, len(notes))
	}
	if !strings.HasPrefix(notes[0].Path, vault.SecondBrainInbox+"/") {
		t.Errorf("note path %q should be under %s/", notes[0].Path, vault.SecondBrainInbox)
	}
}

// TestDoctor_SecondBrain verifies that checkInboxStatus uses the SecondBrain
// inbox schema and correctly reports a clear inbox on an empty vault, and a
// warn status once a pending note is placed in the inbox.
func TestDoctor_SecondBrain(t *testing.T) {
	vaultDir := t.TempDir()
	if err := vault.InitSkeleton(vaultDir, vault.KindSecondBrain); err != nil {
		t.Fatalf("InitSkeleton: %v", err)
	}

	check := checkInboxStatus(vaultDir, vault.KindSecondBrain)
	if check.Status != "ok" {
		t.Errorf("empty vault: checkInboxStatus() status = %q, want %q; message: %s",
			check.Status, "ok", check.Message)
	}
	if check.Name != vault.SecondBrainInbox {
		t.Errorf("checkInboxStatus() name = %q, want %q", check.Name, vault.SecondBrainInbox)
	}

	// Place a pending (unprocessed) note in the second-brain inbox.
	pendingPath := filepath.Join(vaultDir, vault.SecondBrainInbox, "pending.md")
	pending := "---\ntype: fleeting\ncreated: 2026-04-01\n---\nPending capture.\n"
	if err := os.WriteFile(pendingPath, []byte(pending), 0644); err != nil {
		t.Fatalf("WriteFile: %v", err)
	}

	check = checkInboxStatus(vaultDir, vault.KindSecondBrain)
	if check.Status != "warn" {
		t.Errorf("with pending note: checkInboxStatus() status = %q, want %q; message: %s",
			check.Status, "warn", check.Message)
	}
}
