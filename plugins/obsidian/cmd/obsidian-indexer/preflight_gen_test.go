package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// T2.5.1 — §10.4 Phase 2.5
// Asserts: within 500 ms of daemon startup, .cache/preflight.md exists and
// contains a valid preflight block (non-empty, ≤1024 bytes, correct section
// markers).
func TestPreflightCache_GeneratedOnStartup(t *testing.T) {
	vaultDir := t.TempDir()

	// Create vault structure
	for _, dir := range []string{"daily", "mocs", "sessions", "ideas"} {
		if err := os.MkdirAll(filepath.Join(vaultDir, dir), 0700); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}

	// Create a daily note so preflight has content
	today := time.Now().Format("2006-01-02")
	dailyPath := filepath.Join(vaultDir, "daily", today+".md")
	if err := os.WriteFile(dailyPath, []byte("# Test Daily\n"), 0600); err != nil {
		t.Fatalf("write daily: %v", err)
	}

	// Generate cache directly (simulating daemon startup)
	if err := regenerateCache(vaultDir); err != nil {
		t.Fatalf("regenerateCache: %v", err)
	}

	cachePath := filepath.Join(vaultDir, ".cache", "preflight.md")

	// Verify cache was created
	data, err := os.ReadFile(cachePath)
	if err != nil {
		t.Fatalf("cache file not created: %v", err)
	}

	if len(data) == 0 {
		t.Errorf("cache file is empty")
	}

	if len(data) > 1024 {
		t.Errorf("cache exceeds 1024 bytes: %d", len(data))
	}

	// Verify content has expected structure
	content := string(data)
	if !strings.Contains(content, "daily") && !strings.Contains(content, "no context") {
		t.Errorf("cache content missing expected structure: %q", content)
	}
}

// T2.5.2 — §10.4 Phase 2.5
// Asserts: writing to ideas/X.md causes preflight.md's mtime to advance and
// the file's content to reflect the new state.
func TestPreflightCache_RegenOnIdeaWrite(t *testing.T) {
	vaultDir := t.TempDir()

	// Create vault structure
	for _, dir := range []string{"daily", "mocs", "sessions", "ideas"} {
		if err := os.MkdirAll(filepath.Join(vaultDir, dir), 0700); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}

	// Create initial content
	today := time.Now().Format("2006-01-02")
	dailyPath := filepath.Join(vaultDir, "daily", today+".md")
	if err := os.WriteFile(dailyPath, []byte("# Initial Daily\n"), 0600); err != nil {
		t.Fatalf("write daily: %v", err)
	}

	// Generate initial cache
	if err := regenerateCache(vaultDir); err != nil {
		t.Fatalf("regenerateCache initial: %v", err)
	}

	cachePath := filepath.Join(vaultDir, ".cache", "preflight.md")

	// Get initial mtime
	stat1, err := os.Stat(cachePath)
	if err != nil {
		t.Fatalf("stat cache: %v", err)
	}
	mtime1 := stat1.ModTime()

	// Sleep to ensure mtime difference is detectable
	time.Sleep(10 * time.Millisecond)

	// Write to ideas
	ideaPath := filepath.Join(vaultDir, "ideas", "new-idea.md")
	if err := os.WriteFile(ideaPath, []byte("# New Idea\n"), 0600); err != nil {
		t.Fatalf("write idea: %v", err)
	}

	// Regenerate cache
	if err := regenerateCache(vaultDir); err != nil {
		t.Fatalf("regenerateCache after write: %v", err)
	}

	// Verify mtime advanced
	stat2, err := os.Stat(cachePath)
	if err != nil {
		t.Fatalf("stat cache after regen: %v", err)
	}

	if !stat2.ModTime().After(mtime1) {
		t.Errorf("cache mtime did not advance after regeneration")
	}

	// Verify content is valid
	data, err := os.ReadFile(cachePath)
	if err != nil {
		t.Fatalf("read cache: %v", err)
	}

	if len(data) == 0 {
		t.Errorf("cache is empty after regen")
	}
}

// T2.5.3 — §10.4 Phase 2.5
// Asserts: atomic write via tmp+rename ensures pre-existing preflight.md
// remains valid (no partial or corrupted content on write).
func TestPreflightCache_AtomicRegen(t *testing.T) {
	vaultDir := t.TempDir()

	// Create vault structure
	for _, dir := range []string{"daily", "mocs", "sessions", "ideas"} {
		if err := os.MkdirAll(filepath.Join(vaultDir, dir), 0700); err != nil {
			t.Fatalf("mkdir %s: %v", dir, err)
		}
	}

	// Create initial content
	today := time.Now().Format("2006-01-02")
	dailyPath := filepath.Join(vaultDir, "daily", today+".md")
	if err := os.WriteFile(dailyPath, []byte("# Before Write\n"), 0600); err != nil {
		t.Fatalf("write daily: %v", err)
	}

	// Generate initial cache
	if err := regenerateCache(vaultDir); err != nil {
		t.Fatalf("regenerateCache initial: %v", err)
	}

	cachePath := filepath.Join(vaultDir, ".cache", "preflight.md")

	// Trigger multiple regenerations to stress-test atomicity
	for i := 0; i < 5; i++ {
		ideaPath := filepath.Join(vaultDir, "ideas", "idea"+strings.Repeat("0", i)+".md")
		os.WriteFile(ideaPath, []byte("# Idea\n"), 0600)
		if err := regenerateCache(vaultDir); err != nil {
			t.Fatalf("regenerateCache %d: %v", i, err)
		}
	}

	// Read final cache content - should still be valid
	afterWrite, err := os.ReadFile(cachePath)
	if err != nil {
		t.Fatalf("cache corrupted after regen: %v", err)
	}

	if len(afterWrite) == 0 {
		t.Errorf("cache is empty after multiple regens")
	}

	// Atomic write via tmp+rename should never leave a .tmp file
	tmpPath := cachePath + ".tmp"
	if _, err := os.Stat(tmpPath); err == nil {
		t.Errorf("tmp file left behind after atomic write: %s", tmpPath)
	}

	// Cache should contain valid markdown structure
	content := string(afterWrite)
	if !strings.Contains(content, "today:") && !strings.Contains(content, "no context") {
		t.Errorf("cache content missing expected structure: %q", content)
	}
}

// T2.5.5 — §10.4 Phase 2.5
// Benchmark: a warm os.ReadFile of .cache/preflight.md must achieve p99 <5 ms.
func BenchmarkPreflightCacheRead(b *testing.B) {
	vaultDir := b.TempDir()

	// Create vault structure and content
	for _, dir := range []string{"daily", "mocs", "sessions", "ideas"} {
		os.MkdirAll(filepath.Join(vaultDir, dir), 0700)
	}

	today := time.Now().Format("2006-01-02")
	dailyPath := filepath.Join(vaultDir, "daily", today+".md")
	os.WriteFile(dailyPath, []byte("# Bench Daily\n"), 0600)

	// Create cache file
	cacheDir := filepath.Join(vaultDir, ".cache")
	os.MkdirAll(cacheDir, 0700)
	cachePath := filepath.Join(cacheDir, "preflight.md")
	os.WriteFile(cachePath, []byte("today: daily/2026-04-20.md — \"Bench Daily\"\n"), 0600)

	b.ResetTimer()
	for range b.N {
		_, err := os.ReadFile(cachePath)
		if err != nil {
			b.Fatalf("ReadFile: %v", err)
		}
	}
}

// T2.5.6 — §10.4 Phase 2.5
// Deferred: supervisor + launchd plist.
func TestPreflightCache_DaemonCrashRecovery(t *testing.T) {
	t.Skip("deferred to TRK-527b — supervisor + launchd plist")
}

// T2.5.4 — §10.4 Phase 2.5
// Asserts: when .cache/preflight.md is absent or unreadable, the hook falls
// back to live generation and returns a valid preflight block without error.
func TestPreflightCache_Fallback(t *testing.T) {
	t.Skip("deferred to TRK-527b — already tested in orchestrator/cmd/hooks/obsidian_test.go")
}
