package core

import (
	"os"
	"path/filepath"
	"testing"
)

func TestWriteAndReadPID(t *testing.T) {
	tmpDir := t.TempDir()

	if err := WritePID(tmpDir); err != nil {
		t.Fatalf("WritePID: %v", err)
	}

	pid, err := ReadPID(tmpDir)
	if err != nil {
		t.Fatalf("ReadPID: %v", err)
	}

	if pid != os.Getpid() {
		t.Errorf("pid = %d; want %d", pid, os.Getpid())
	}

	// Verify file permissions
	info, err := os.Stat(filepath.Join(tmpDir, "pid"))
	if err != nil {
		t.Fatalf("stat: %v", err)
	}
	if perm := info.Mode().Perm(); perm != 0600 {
		t.Errorf("permissions = %o; want 0600", perm)
	}
}

func TestReadPID_MissingFile(t *testing.T) {
	tmpDir := t.TempDir()

	pid, err := ReadPID(tmpDir)
	if err != nil {
		t.Errorf("ReadPID on missing file should not error: %v", err)
	}
	if pid != 0 {
		t.Errorf("pid = %d; want 0 for missing file", pid)
	}
}

func TestReadPID_CorruptContent(t *testing.T) {
	tmpDir := t.TempDir()
	os.WriteFile(filepath.Join(tmpDir, "pid"), []byte("not-a-number"), 0600)

	_, err := ReadPID(tmpDir)
	if err == nil {
		t.Error("expected error for corrupt pid file")
	}
}

func TestCancelSentinel(t *testing.T) {
	tmpDir := t.TempDir()

	// Should not exist initially
	if HasCancelSentinel(tmpDir) {
		t.Error("cancel sentinel should not exist initially")
	}

	// Write sentinel
	if err := WriteCancelSentinel(tmpDir); err != nil {
		t.Fatalf("WriteCancelSentinel: %v", err)
	}

	// Should exist now
	if !HasCancelSentinel(tmpDir) {
		t.Error("cancel sentinel should exist after write")
	}
}

func TestResolveWorkspacePath_Nonexistent(t *testing.T) {
	_, err := ResolveWorkspacePath("nonexistent-workspace-id-12345678")
	if err == nil {
		t.Error("expected error for nonexistent workspace")
	}
}

func TestResolveWorkspacePath_Valid(t *testing.T) {
	// Create a real workspace to resolve
	ws, err := CreateWorkspace("test resolve", "dev")
	if err != nil {
		t.Fatalf("CreateWorkspace: %v", err)
	}
	t.Cleanup(func() { os.RemoveAll(ws.Path) })

	resolved, err := ResolveWorkspacePath(ws.ID)
	if err != nil {
		t.Fatalf("ResolveWorkspacePath: %v", err)
	}

	// Paths should match
	if resolved != ws.Path {
		t.Errorf("resolved = %q; want %q", resolved, ws.Path)
	}

	t.Logf("resolved workspace: %s", resolved)
}
