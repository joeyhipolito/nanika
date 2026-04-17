package worker

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

// setupIdentityHome sets HOME to a temp dir so tests don't touch ~/.alluka.
func setupIdentityHome(t *testing.T) string {
	t.Helper()
	tmpHome := t.TempDir()
	t.Setenv("HOME", tmpHome)
	return tmpHome
}

// TestLoadIdentity_Bootstrap verifies that LoadIdentity creates the worker
// directory and all required files when called for the first time.
func TestLoadIdentity_Bootstrap(t *testing.T) {
	tmpHome := setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	if wi.Name != "alpha" {
		t.Errorf("Name = %q, want %q", wi.Name, "alpha")
	}
	if wi.CreatedAt.IsZero() {
		t.Error("CreatedAt should be set after bootstrap")
	}
	if wi.PhasesCompleted != 0 {
		t.Errorf("PhasesCompleted = %d, want 0", wi.PhasesCompleted)
	}
	if len(wi.Domains) != 0 {
		t.Errorf("Domains = %v, want empty", wi.Domains)
	}
	if len(wi.Entries) != 0 {
		t.Errorf("Entries = %d, want 0", len(wi.Entries))
	}

	workerDir := filepath.Join(tmpHome, ".alluka", "workers", "alpha")

	// identity.md must exist and contain the bootstrap text.
	identityContent, err := os.ReadFile(filepath.Join(workerDir, "identity.md"))
	if err != nil {
		t.Fatalf("identity.md not created: %v", err)
	}
	if !strings.Contains(string(identityContent), "persistent worker") {
		t.Errorf("identity.md does not contain bootstrap text: %q", string(identityContent))
	}

	// stats.json must exist.
	if _, err := os.Stat(filepath.Join(workerDir, "stats.json")); err != nil {
		t.Fatalf("stats.json not created: %v", err)
	}

	// memory.md must exist.
	if _, err := os.Stat(filepath.Join(workerDir, "memory.md")); err != nil {
		t.Fatalf("memory.md not created: %v", err)
	}
}

// TestLoadSaveRoundTrip verifies that all fields survive a Save → Load cycle.
func TestLoadSaveRoundTrip(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("testworker")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	// Mutate fields.
	wi.PhasesCompleted = 7
	wi.Domains = map[string]int{"dev": 5, "personal": 2}
	wi.TotalCost = 1.23
	wi.LastActive = time.Date(2026, 4, 10, 12, 0, 0, 0, time.UTC)

	entry := MemoryEntry{
		Content: "always wrap errors with context",
		Filed:   time.Date(2026, 4, 1, 0, 0, 0, 0, time.UTC),
		By:      "testworker",
		Type:    "feedback",
	}
	wi.AddMemoryEntry(entry)

	if err := wi.SaveIdentity(); err != nil {
		t.Fatalf("SaveIdentity: %v", err)
	}

	// Load fresh.
	wi2, err := LoadIdentity("testworker")
	if err != nil {
		t.Fatalf("LoadIdentity after save: %v", err)
	}

	if wi2.PhasesCompleted != 7 {
		t.Errorf("PhasesCompleted = %d, want 7", wi2.PhasesCompleted)
	}
	if wi2.Domains["dev"] != 5 {
		t.Errorf("Domains[dev] = %d, want 5", wi2.Domains["dev"])
	}
	if wi2.Domains["personal"] != 2 {
		t.Errorf("Domains[personal] = %d, want 2", wi2.Domains["personal"])
	}
	if wi2.TotalCost != 1.23 {
		t.Errorf("TotalCost = %f, want 1.23", wi2.TotalCost)
	}
	if !wi2.LastActive.Equal(wi.LastActive) {
		t.Errorf("LastActive = %v, want %v", wi2.LastActive, wi.LastActive)
	}
	if len(wi2.Entries) != 1 {
		t.Fatalf("Entries = %d, want 1", len(wi2.Entries))
	}
	if wi2.Entries[0].Content != "always wrap errors with context" {
		t.Errorf("Entries[0].Content = %q, want %q", wi2.Entries[0].Content, "always wrap errors with context")
	}
	if wi2.Entries[0].By != "testworker" {
		t.Errorf("Entries[0].By = %q, want %q", wi2.Entries[0].By, "testworker")
	}
	if wi2.Entries[0].Type != "feedback" {
		t.Errorf("Entries[0].Type = %q, want %q", wi2.Entries[0].Type, "feedback")
	}
}

// TestRecordPhase verifies that RecordPhase increments stats correctly.
func TestRecordPhase(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	before := time.Now()
	wi.RecordPhase("dev", 0.05)
	after := time.Now()

	if wi.PhasesCompleted != 1 {
		t.Errorf("PhasesCompleted = %d, want 1", wi.PhasesCompleted)
	}
	if wi.Domains["dev"] != 1 {
		t.Errorf("Domains[dev] = %d, want 1", wi.Domains["dev"])
	}
	if wi.TotalCost != 0.05 {
		t.Errorf("TotalCost = %f, want 0.05", wi.TotalCost)
	}
	if wi.LastActive.Before(before) || wi.LastActive.After(after) {
		t.Errorf("LastActive %v not within [%v, %v]", wi.LastActive, before, after)
	}

	// Second call accumulates.
	wi.RecordPhase("dev", 0.10)
	wi.RecordPhase("personal", 0.02)

	if wi.PhasesCompleted != 3 {
		t.Errorf("PhasesCompleted = %d, want 3", wi.PhasesCompleted)
	}
	if wi.Domains["dev"] != 2 {
		t.Errorf("Domains[dev] = %d, want 2", wi.Domains["dev"])
	}
	if wi.Domains["personal"] != 1 {
		t.Errorf("Domains[personal] = %d, want 1", wi.Domains["personal"])
	}
	if wi.TotalCost != 0.17 {
		// Use approximate comparison for float arithmetic.
		if wi.TotalCost < 0.169 || wi.TotalCost > 0.171 {
			t.Errorf("TotalCost = %f, want ~0.17", wi.TotalCost)
		}
	}
}

// TestRecordPhase_EmptyDomain verifies that empty domain strings are ignored.
func TestRecordPhase_EmptyDomain(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	wi.RecordPhase("", 0.0)

	if wi.PhasesCompleted != 1 {
		t.Errorf("PhasesCompleted = %d, want 1", wi.PhasesCompleted)
	}
	if len(wi.Domains) != 0 {
		t.Errorf("Domains should be empty, got %v", wi.Domains)
	}
}

// TestAddMemoryEntry_Dedup verifies that duplicate entries are silently dropped.
func TestAddMemoryEntry_Dedup(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	entry := MemoryEntry{Content: "use table-driven tests in Go"}
	wi.AddMemoryEntry(entry)
	wi.AddMemoryEntry(entry) // exact duplicate
	wi.AddMemoryEntry(MemoryEntry{Content: "USE TABLE-DRIVEN TESTS IN GO"}) // normalized dup

	if len(wi.Entries) != 1 {
		t.Errorf("Entries = %d, want 1 (duplicates should be dropped)", len(wi.Entries))
	}
}

// TestAddMemoryEntry_DifferentEntries verifies that distinct entries are all kept.
func TestAddMemoryEntry_DifferentEntries(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	wi.AddMemoryEntry(MemoryEntry{Content: "entry one"})
	wi.AddMemoryEntry(MemoryEntry{Content: "entry two"})
	wi.AddMemoryEntry(MemoryEntry{Content: "entry three"})

	if len(wi.Entries) != 3 {
		t.Errorf("Entries = %d, want 3", len(wi.Entries))
	}
}

// TestAddMemoryEntry_SupersededNotConsideredDuplicate verifies that a new entry
// matching a superseded entry's content is still appended (superseded entries
// are inactive and should not block new additions).
func TestAddMemoryEntry_SupersededNotConsideredDuplicate(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	superseded := MemoryEntry{Content: "old advice", SupersededBy: "abc123"}
	wi.Entries = append(wi.Entries, &superseded) // inject directly as superseded

	wi.AddMemoryEntry(MemoryEntry{Content: "old advice"}) // same content but active

	if len(wi.Entries) != 2 {
		t.Errorf("Entries = %d, want 2 (superseded should not block new entry)", len(wi.Entries))
	}
}

// TestBudgetedMemory_Respectsbudget verifies that the total byte count of returned
// entries stays within the budget.
func TestBudgetedMemory_Respectsbudget(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	// Add several entries.
	for i := 0; i < 10; i++ {
		wi.AddMemoryEntry(MemoryEntry{
			Content: strings.Repeat("x", 50),
			Filed:   time.Now(),
		})
	}

	budget := 100 // fits at most 1-2 entries of length 50
	result := wi.BudgetedMemory(nil, budget)

	total := 0
	for _, e := range result {
		total += len(e.String()) + 1
	}
	if total > budget {
		t.Errorf("total bytes %d exceeds budget %d", total, budget)
	}
}

// TestBudgetedMemory_ScoresByKeywordOverlap verifies that entries matching the
// given keywords are ranked ahead of unrelated entries.
func TestBudgetedMemory_ScoresByKeywordOverlap(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	recent := time.Now()
	wi.AddMemoryEntry(MemoryEntry{Content: "always use context cancellation in goroutines", Filed: recent})
	wi.AddMemoryEntry(MemoryEntry{Content: "goroutine leaks cause memory pressure", Filed: recent})
	wi.AddMemoryEntry(MemoryEntry{Content: "prefer flat package layout in Go", Filed: recent})
	wi.AddMemoryEntry(MemoryEntry{Content: "SQL indexes speed up range queries", Filed: recent})

	keywords := []string{"goroutine", "context", "cancellation"}
	result := wi.BudgetedMemory(keywords, 4096)

	if len(result) == 0 {
		t.Fatal("expected entries, got none")
	}

	// The first two results should mention goroutine or context, not SQL or packages.
	topContents := make([]string, 0, len(result))
	for _, e := range result {
		topContents = append(topContents, e.Content)
	}

	if !strings.Contains(topContents[0], "goroutine") && !strings.Contains(topContents[0], "context") {
		t.Errorf("top result %q should be goroutine/context related", topContents[0])
	}
}

// TestBudgetedMemory_FallsBackToRecency verifies that when no keyword overlap
// exists, entries are still returned (sorted by recency).
func TestBudgetedMemory_FallsBackToRecency(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	older := time.Now().Add(-48 * time.Hour)
	newer := time.Now().Add(-1 * time.Hour)
	wi.AddMemoryEntry(MemoryEntry{Content: "older entry about databases", Filed: older})
	wi.AddMemoryEntry(MemoryEntry{Content: "newer entry about databases", Filed: newer})

	// Keywords that match nothing.
	result := wi.BudgetedMemory([]string{"typescript", "react", "nextjs"}, 4096)

	if len(result) < 2 {
		t.Fatalf("expected 2 entries, got %d", len(result))
	}
	// Newer should come first (recency fallback).
	if result[0].Content != "newer entry about databases" {
		t.Errorf("expected newer entry first, got %q", result[0].Content)
	}
}

// TestBudgetedMemory_EmptyWorker verifies that an empty Entries slice returns nil.
func TestBudgetedMemory_EmptyWorker(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	result := wi.BudgetedMemory([]string{"go", "errors"}, 4096)
	if result != nil {
		t.Errorf("expected nil for empty worker, got %v", result)
	}
}

// TestBudgetedMemory_ZeroBudget verifies that a zero budget returns nil.
func TestBudgetedMemory_ZeroBudget(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	wi.AddMemoryEntry(MemoryEntry{Content: "some entry"})

	result := wi.BudgetedMemory([]string{"entry"}, 0)
	if result != nil {
		t.Errorf("expected nil for zero budget, got %v", result)
	}
}

// TestLoadMemory_TrimsOversizedFile verifies that when a memory.md file on disk
// exceeds workerMemoryCeiling (100), loadMemory trims entries down to the ceiling.
// This ensures the ceiling converges even when the file was populated outside the
// normal AddMemoryEntry path (e.g., manual editing or legacy code that didn't enforce limits).
func TestLoadMemory_TrimsOversizedFile(t *testing.T) {
	tmpHome := setupIdentityHome(t)

	// Bootstrap the worker so the directory structure exists.
	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity (bootstrap): %v", err)
	}

	// Write 110 entries directly to memory.md (bypassing AddMemoryEntry ceiling).
	var buf strings.Builder
	for i := 0; i < 110; i++ {
		entry := MemoryEntry{
			Content: fmt.Sprintf("oversized entry number %d with enough content", i),
			Filed:   time.Now().Add(-time.Duration(i) * time.Hour),
			By:      "alpha",
			Type:    "feedback",
		}
		buf.WriteString(entry.String())
		buf.WriteByte('\n')
	}
	memPath := filepath.Join(tmpHome, ".alluka", "workers", "alpha", "memory.md")
	if err := os.WriteFile(memPath, []byte(buf.String()), 0600); err != nil {
		t.Fatalf("writing oversized memory.md: %v", err)
	}

	// Reload — loadMemory should trim to workerMemoryCeiling.
	wi, err = LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity (reload): %v", err)
	}

	if len(wi.Entries) != workerMemoryCeiling {
		t.Errorf("after load: len(Entries) = %d, want %d (ceiling)", len(wi.Entries), workerMemoryCeiling)
	}

	// Now add 5 more entries via AddMemoryEntry — ceiling should still hold.
	for i := 0; i < 5; i++ {
		wi.AddMemoryEntry(MemoryEntry{
			Content: fmt.Sprintf("new entry after trim %d with enough text", i),
			Filed:   time.Now(),
			By:      "alpha",
			Type:    "feedback",
		})
	}

	if len(wi.Entries) != workerMemoryCeiling {
		t.Errorf("after adding 5 more: len(Entries) = %d, want %d (ceiling)", len(wi.Entries), workerMemoryCeiling)
	}
}

// TestSaveIdentity_Idempotent verifies that multiple saves produce consistent results.
func TestSaveIdentity_Idempotent(t *testing.T) {
	setupIdentityHome(t)

	wi, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity: %v", err)
	}

	wi.RecordPhase("dev", 0.01)
	wi.AddMemoryEntry(MemoryEntry{Content: "idempotent test entry"})

	for i := 0; i < 3; i++ {
		if err := wi.SaveIdentity(); err != nil {
			t.Fatalf("SaveIdentity call %d: %v", i+1, err)
		}
	}

	wi2, err := LoadIdentity("alpha")
	if err != nil {
		t.Fatalf("LoadIdentity after multiple saves: %v", err)
	}
	if wi2.PhasesCompleted != 1 {
		t.Errorf("PhasesCompleted = %d, want 1", wi2.PhasesCompleted)
	}
	if len(wi2.Entries) != 1 {
		t.Errorf("Entries = %d, want 1", len(wi2.Entries))
	}
}
