package hooks

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/joeyhipolito/orchestrator-cli/internal/preflight"
)

// fixedDate is the pinned clock used across T2.x tests so golden file
// comparisons are stable regardless of when the test suite runs.
var fixedDate = time.Date(2026, 4, 20, 10, 0, 0, 0, time.UTC)

// hookFakeSection is a minimal preflight.Section for priority-ordering tests.
type hookFakeSection struct {
	name string
	pri  int
}

func (f *hookFakeSection) Name() string  { return f.name }
func (f *hookFakeSection) Priority() int { return f.pri }
func (f *hookFakeSection) Fetch(_ context.Context) (preflight.Block, error) {
	return preflight.Block{Title: f.name, Body: f.name}, nil
}

// seedVaultDailyAt writes <vaultDir>/daily/<date>.md with a fixed mtime so
// the spec's 48h-window filter behaves deterministically.
func seedVaultDailyAt(t *testing.T, vaultDir, date, content string, mtime time.Time) {
	t.Helper()
	dir := filepath.Join(vaultDir, "daily")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatalf("mkdir daily: %v", err)
	}
	p := filepath.Join(dir, date+".md")
	if err := os.WriteFile(p, []byte(content), 0600); err != nil {
		t.Fatalf("write daily: %v", err)
	}
	if !mtime.IsZero() {
		if err := os.Chtimes(p, mtime, mtime); err != nil {
			t.Fatalf("chtimes daily: %v", err)
		}
	}
}

// seedVaultMOCAt writes <vaultDir>/mocs/<name>.md with a fixed mtime.
func seedVaultMOCAt(t *testing.T, vaultDir, name, content string, mtime time.Time) {
	t.Helper()
	dir := filepath.Join(vaultDir, "mocs")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatalf("mkdir mocs: %v", err)
	}
	p := filepath.Join(dir, name+".md")
	if err := os.WriteFile(p, []byte(content), 0600); err != nil {
		t.Fatalf("write moc %s: %v", name, err)
	}
	if !mtime.IsZero() {
		if err := os.Chtimes(p, mtime, mtime); err != nil {
			t.Fatalf("chtimes moc: %v", err)
		}
	}
}

// clearObsidianHookEnv resets env state before each T2.x test.
func clearObsidianHookEnv(t *testing.T) {
	t.Helper()
	t.Setenv("OBSIDIAN_VAULT_PATH", "")
	t.Setenv("OBSIDIAN_CONFIG_DIR", t.TempDir())
	t.Setenv("NANIKA_OBSIDIAN_CONTEXT", "")
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "")
	t.Setenv("NANIKA_OBSIDIAN_NO_CACHE", "1") // disable cache side-effects in hook tests
}

// TestObsidianPreflightHappyPath (T2.1)
// Seeded vault → body matches the ADR-format golden; ≤1024 bytes.
func TestObsidianPreflightHappyPath(t *testing.T) {
	clearObsidianHookEnv(t)
	vaultDir := t.TempDir()
	seedVaultDailyAt(t, vaultDir, "2026-04-20",
		"# Shipping the vault narrative layer\n\n## Tasks\n- [ ] Review alpha mission output\n",
		fixedDate.Add(-1*time.Hour))
	// Stagger mtimes so ordering is deterministic: alpha newest → beta → gamma.
	seedVaultMOCAt(t, vaultDir, "alpha", "# Alpha MOC\n", fixedDate.Add(-30*time.Minute))
	seedVaultMOCAt(t, vaultDir, "beta", "# Beta MOC\n", fixedDate.Add(-2*time.Hour))
	seedVaultMOCAt(t, vaultDir, "gamma", "# Gamma MOC\n", fixedDate.Add(-1*time.Hour))

	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "1024")

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	goldenBytes, err := os.ReadFile(filepath.Join("testdata", "golden", "preflight-block-happy.txt"))
	if err != nil {
		t.Fatalf("read golden file: %v", err)
	}
	want := strings.TrimRight(string(goldenBytes), "\n")

	if blk.Body != want {
		t.Errorf("body mismatch\ngot:\n%s\nwant:\n%s", blk.Body, want)
	}
	if len(blk.Body) > 1024 {
		t.Errorf("body length %d exceeds 1024 bytes", len(blk.Body))
	}
}

// TestObsidianPreflightEmptyVault (T2.2)
// ADR fallback matrix: an empty vault → empty Block (renderer omits section).
func TestObsidianPreflightEmptyVault(t *testing.T) {
	clearObsidianHookEnv(t)
	vaultDir := t.TempDir()
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("empty-vault body: got %q, want empty", blk.Body)
	}
}

// TestObsidianPreflightMissingDaily (T2.3)
// Today's note missing, yesterday's exists within 48h → fallback surfaces it.
func TestObsidianPreflightMissingDaily(t *testing.T) {
	clearObsidianHookEnv(t)
	vaultDir := t.TempDir()
	yesterday := fixedDate.AddDate(0, 0, -1).Format("2006-01-02")
	seedVaultDailyAt(t, vaultDir, yesterday,
		"# Yesterday's progress\n- [x] Done\n",
		fixedDate.Add(-20*time.Hour))
	seedVaultMOCAt(t, vaultDir, "projects", "# Projects\n", fixedDate.Add(-1*time.Hour))

	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	wantLine := `today: daily/` + yesterday + `.md — "Yesterday's progress"`
	if !strings.Contains(blk.Body, wantLine) {
		t.Errorf("expected fallback daily line %q, got: %q", wantLine, blk.Body)
	}
	if strings.Contains(blk.Body, "2026-04-20.md") {
		t.Errorf("today's non-existent date should not appear: %q", blk.Body)
	}
}

// TestObsidianPreflightBudgetTruncation (T2.4)
// Oversize body → stays ≤1024 bytes with "…" marker and no split wikilinks.
func TestObsidianPreflightBudgetTruncation(t *testing.T) {
	clearObsidianHookEnv(t)
	const budget = 1024
	vaultDir := t.TempDir()
	// Large daily note with a wikilink in the heading text (stresses the
	// wikilink-balance guard even after format change).
	bigHeading := "# " + strings.Repeat("[[Alpha Mission]] milestone ", 30) + "\n"
	seedVaultDailyAt(t, vaultDir, "2026-04-20", bigHeading, fixedDate.Add(-1*time.Hour))
	seedVaultMOCAt(t, vaultDir, "projects",
		"# "+strings.Repeat("Projects ", 30)+"\n",
		fixedDate.Add(-30*time.Minute))
	seedVaultMOCAt(t, vaultDir, "learning",
		"# "+strings.Repeat("Learning ", 30)+"\n",
		fixedDate.Add(-45*time.Minute))

	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "1024")

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if len(blk.Body) > budget {
		t.Errorf("truncated body %d bytes exceeds budget %d", len(blk.Body), budget)
	}
	if !strings.Contains(blk.Body, "…") {
		t.Errorf("expected trailing … marker, got: %q", blk.Body)
	}
	body := blk.Body
	for {
		openIdx := strings.Index(body, "[[")
		if openIdx < 0 {
			break
		}
		closeIdx := strings.Index(body[openIdx:], "]]")
		if closeIdx < 0 {
			t.Errorf("split wikilink in truncated body: %q", blk.Body)
			break
		}
		body = body[openIdx+closeIdx+2:]
	}
}

// TestObsidianPreflightDisabledOff (T2.5)
// NANIKA_OBSIDIAN_CONTEXT=off → empty body; BuildBrief+RenderMarkdown safe.
func TestObsidianPreflightDisabledOff(t *testing.T) {
	clearObsidianHookEnv(t)
	vaultDir := t.TempDir()
	seedVaultDailyAt(t, vaultDir, "2026-04-20", "# T\n", fixedDate)
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_CONTEXT", "off")

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body with CONTEXT=off, got: %q", blk.Body)
	}

	preflight.Reset()
	t.Cleanup(preflight.Reset)
	preflight.Register(sec)
	brief := preflight.BuildBrief(context.Background(), nil)
	_ = brief.RenderMarkdown() // must not panic
}

// TestObsidianPreflightDisabledZero (T2.5b / B6)
// NANIKA_OBSIDIAN_CONTEXT=0 → empty body.
func TestObsidianPreflightDisabledZero(t *testing.T) {
	clearObsidianHookEnv(t)
	vaultDir := t.TempDir()
	seedVaultDailyAt(t, vaultDir, "2026-04-20", "# T\n", fixedDate)
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_CONTEXT", "0")

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body with CONTEXT=0, got: %q", blk.Body)
	}
}

// TestPreflightCache_Fallback (T2.5.4)
// A stale cache (source file newer than cache) → Fetch ignores cache and
// returns live-built body.
func TestPreflightCache_Fallback(t *testing.T) {
	clearObsidianHookEnv(t)
	t.Setenv("NANIKA_OBSIDIAN_NO_CACHE", "") // enable cache for this test

	vaultDir := t.TempDir()

	// Write a stale cache first (older mtime).
	cacheDir := filepath.Join(vaultDir, ".cache")
	if err := os.MkdirAll(cacheDir, 0700); err != nil {
		t.Fatalf("mkdir .cache: %v", err)
	}
	stalePath := filepath.Join(cacheDir, "preflight.md")
	const staleBody = `today: daily/old.md — "Stale cached content"`
	if err := os.WriteFile(stalePath, []byte(staleBody), 0600); err != nil {
		t.Fatalf("write stale cache: %v", err)
	}
	staleTime := fixedDate.Add(-3 * time.Hour)
	if err := os.Chtimes(stalePath, staleTime, staleTime); err != nil {
		t.Fatalf("chtimes stale cache: %v", err)
	}

	// Source file written after the stale cache → makes it stale.
	sourceTime := fixedDate.Add(-1 * time.Hour)
	seedVaultDailyAt(t, vaultDir, "2026-04-20",
		"# Live daily note\n- [ ] real task\n",
		sourceTime)

	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	// Must contain live content, not stale cache.
	if strings.Contains(blk.Body, "Stale cached content") {
		t.Errorf("stale cache leaked into output: %q", blk.Body)
	}
	if !strings.Contains(blk.Body, "Live daily note") {
		t.Errorf("expected live-built body, got: %q", blk.Body)
	}
}

// BenchmarkPreflightCacheRead (T2.5.5)
// Measures Fetch latency when a warm, fresh cache is present.
func BenchmarkPreflightCacheRead(b *testing.B) {
	vaultDir := b.TempDir()

	// Source files (older mtime).
	dailyDir := filepath.Join(vaultDir, "daily")
	if err := os.MkdirAll(dailyDir, 0700); err != nil {
		b.Fatalf("mkdir daily: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dailyDir, "2026-04-20.md"),
		[]byte("# Bench Daily\n"), 0600); err != nil {
		b.Fatalf("write daily: %v", err)
	}
	sourceTime := fixedDate.Add(-2 * time.Hour)
	if err := os.Chtimes(filepath.Join(dailyDir, "2026-04-20.md"), sourceTime, sourceTime); err != nil {
		b.Fatalf("chtimes daily: %v", err)
	}

	// Fresh cache (newer mtime).
	cacheDir := filepath.Join(vaultDir, ".cache")
	if err := os.MkdirAll(cacheDir, 0700); err != nil {
		b.Fatalf("mkdir .cache: %v", err)
	}
	cachePath := filepath.Join(cacheDir, "preflight.md")
	const cacheBody = `today: daily/2026-04-20.md — "Bench Daily"`
	if err := os.WriteFile(cachePath, []byte(cacheBody), 0600); err != nil {
		b.Fatalf("write cache: %v", err)
	}
	cacheTime := fixedDate.Add(-1 * time.Hour)
	if err := os.Chtimes(cachePath, cacheTime, cacheTime); err != nil {
		b.Fatalf("chtimes cache: %v", err)
	}

	b.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	b.Setenv("OBSIDIAN_CONFIG_DIR", b.TempDir())
	b.Setenv("NANIKA_OBSIDIAN_CONTEXT", "")
	b.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "1024")
	b.Setenv("NANIKA_OBSIDIAN_NO_CACHE", "") // enable cache reads

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	ctx := context.Background()
	b.ResetTimer()
	for range b.N {
		blk, err := sec.Fetch(ctx)
		if err != nil {
			b.Fatalf("Fetch: %v", err)
		}
		if blk.Body != cacheBody {
			b.Fatalf("expected cached body, got: %q", blk.Body)
		}
	}
}

// TestObsidianPreflightPriorityOrdering (T2.6)
// obsidian(25) between tracker(20) and learnings(30) regardless of register order.
func TestObsidianPreflightPriorityOrdering(t *testing.T) {
	preflight.Reset()
	t.Cleanup(preflight.Reset)

	preflight.Register(&hookFakeSection{"learnings", 30})
	preflight.Register(preflight.NewObsidianSection(nil))
	preflight.Register(&hookFakeSection{"tracker", 20})

	sections := preflight.List()
	if len(sections) != 3 {
		t.Fatalf("expected 3 sections, got %d", len(sections))
	}

	names := make([]string, len(sections))
	for i, s := range sections {
		names[i] = s.Name()
	}

	if names[0] != "tracker" {
		t.Errorf("expected tracker first (priority 20), got %q", names[0])
	}
	if names[1] != "obsidian" {
		t.Errorf("expected obsidian second (priority 25), got %q", names[1])
	}
	if names[2] != "learnings" {
		t.Errorf("expected learnings third (priority 30), got %q", names[2])
	}
}

// TestObsidianPreflightBenchmark (T2.7)
// Naive read stays well under the 200ms ceiling.
func BenchmarkObsidianPreflight(b *testing.B) {
	vaultDir := b.TempDir()

	dailyDir := filepath.Join(vaultDir, "daily")
	if err := os.MkdirAll(dailyDir, 0700); err != nil {
		b.Fatalf("mkdir daily: %v", err)
	}
	if err := os.WriteFile(filepath.Join(dailyDir, "2026-04-20.md"),
		[]byte("# Bench\n- [ ] bench task\n"), 0600); err != nil {
		b.Fatalf("write daily note: %v", err)
	}

	mocsDir := filepath.Join(vaultDir, "mocs")
	if err := os.MkdirAll(mocsDir, 0700); err != nil {
		b.Fatalf("mkdir mocs: %v", err)
	}
	for _, name := range []string{"alpha", "beta", "gamma"} {
		if err := os.WriteFile(filepath.Join(mocsDir, name+".md"),
			[]byte("# "+name+"\n"), 0600); err != nil {
			b.Fatalf("write moc: %v", err)
		}
	}

	b.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	b.Setenv("OBSIDIAN_CONFIG_DIR", b.TempDir())
	b.Setenv("NANIKA_OBSIDIAN_CONTEXT", "")
	b.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "1024")

	sec := preflight.NewObsidianSection(func() time.Time { return fixedDate })
	ctx := context.Background()
	b.ResetTimer()
	for range b.N {
		if _, err := sec.Fetch(ctx); err != nil {
			b.Fatalf("Fetch: %v", err)
		}
	}
}
