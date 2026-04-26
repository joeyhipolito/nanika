package preflight

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
	"unicode/utf8"
)

// fixedNow is the pinned clock used across obsidian unit tests.
var fixedNow = time.Date(2026, 4, 20, 10, 0, 0, 0, time.UTC)

func obsidianFixedSection() *obsidianSection {
	return &obsidianSection{now: func() time.Time { return fixedNow }}
}

// seedVaultDaily writes content to <vaultDir>/daily/<date>.md and sets its mtime.
func seedVaultDaily(t *testing.T, vaultDir, date, content string, mtime time.Time) {
	t.Helper()
	dir := filepath.Join(vaultDir, "daily")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatalf("mkdir daily: %v", err)
	}
	p := filepath.Join(dir, date+".md")
	if err := os.WriteFile(p, []byte(content), 0600); err != nil {
		t.Fatalf("write daily note: %v", err)
	}
	if !mtime.IsZero() {
		if err := os.Chtimes(p, mtime, mtime); err != nil {
			t.Fatalf("chtimes daily: %v", err)
		}
	}
}

// seedVaultMOC writes content to <vaultDir>/mocs/<name>.md and sets its mtime.
func seedVaultMOC(t *testing.T, vaultDir, name, content string, mtime time.Time) {
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

// seedVaultSession writes a session file with frontmatter containing cwd.
func seedVaultSession(t *testing.T, vaultDir, name, cwd, heading string, mtime time.Time) {
	t.Helper()
	dir := filepath.Join(vaultDir, "sessions")
	if err := os.MkdirAll(dir, 0700); err != nil {
		t.Fatalf("mkdir sessions: %v", err)
	}
	body := "---\ncwd: " + cwd + "\n---\n\n# " + heading + "\n\nSession notes.\n"
	p := filepath.Join(dir, name+".md")
	if err := os.WriteFile(p, []byte(body), 0600); err != nil {
		t.Fatalf("write session: %v", err)
	}
	if !mtime.IsZero() {
		if err := os.Chtimes(p, mtime, mtime); err != nil {
			t.Fatalf("chtimes session: %v", err)
		}
	}
}

// clearObsidianEnv resets the env vars that control the section so each
// test starts from the same baseline.
func clearObsidianEnv(t *testing.T) {
	t.Helper()
	t.Setenv("OBSIDIAN_VAULT_PATH", "")
	t.Setenv("OBSIDIAN_CONFIG_DIR", t.TempDir()) // empty dir so config lookup returns ""
	t.Setenv("NANIKA_OBSIDIAN_CONTEXT", "")
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "")
	t.Setenv("NANIKA_OBSIDIAN_NO_CACHE", "1") // disable cache writes in unit tests
}

// TestObsidianVaultUnconfigured — no vault path resolves → empty body.
func TestObsidianVaultUnconfigured(t *testing.T) {
	clearObsidianEnv(t)
	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for unconfigured vault, got %q", blk.Body)
	}
	if blk.Title != obsidianBlockTitle {
		t.Errorf("expected title %q, got %q", obsidianBlockTitle, blk.Title)
	}
}

// TestObsidianVaultMissing — env points to non-existent directory → empty body.
func TestObsidianVaultMissing(t *testing.T) {
	clearObsidianEnv(t)
	t.Setenv("OBSIDIAN_VAULT_PATH", "/nonexistent/obsidian/vault/xyz")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for missing vault, got %q", blk.Body)
	}
}

// TestObsidianVaultEmpty — vault exists but has no daily/mocs/sessions → empty
// body (ADR fallback matrix: empty Block; renderer omits the section).
func TestObsidianVaultEmpty(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body for empty vault, got %q", blk.Body)
	}
}

// TestObsidianDailyOnly — vault with just a daily note emits the today: line
// in the spec format: `today: daily/<date>.md — "<H1>"`.
func TestObsidianDailyOnly(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	seedVaultDaily(t, vaultDir, "2026-04-20",
		"# Shipping narrative layer\n\n## Tasks\n- [ ] Write tests\n",
		fixedNow.Add(-1*time.Hour))
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	want := `today: daily/2026-04-20.md — "Shipping narrative layer"`
	if blk.Body != want {
		t.Errorf("daily-only body mismatch:\n got:  %q\n want: %q", blk.Body, want)
	}
}

// TestObsidianMOCsAndDaily — MOCs appear under a `mocs (48h):` header as
// vault-relative paths sorted desc by mtime, limited to obsidianMOCLimit (=3).
func TestObsidianMOCsAndDaily(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	seedVaultDaily(t, vaultDir, "2026-04-20",
		"# Today\n\n## Tasks\n- [x] Done\n",
		fixedNow.Add(-1*time.Hour))

	// Stagger mtimes so ordering is deterministic: newest first.
	seedVaultMOC(t, vaultDir, "alpha", "# Alpha\n", fixedNow.Add(-30*time.Minute))
	seedVaultMOC(t, vaultDir, "beta", "# Beta\n", fixedNow.Add(-2*time.Hour))
	seedVaultMOC(t, vaultDir, "gamma", "# Gamma\n", fixedNow.Add(-1*time.Hour))
	// Outside the 48h window — must be filtered out.
	seedVaultMOC(t, vaultDir, "stale", "# Stale\n", fixedNow.Add(-72*time.Hour))
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}

	want := `today: daily/2026-04-20.md — "Today"
mocs (48h):
- mocs/alpha.md — "Alpha"
- mocs/gamma.md — "Gamma"
- mocs/beta.md — "Beta"`
	if blk.Body != want {
		t.Errorf("body mismatch:\n got:\n%s\n want:\n%s", blk.Body, want)
	}
	if strings.Contains(blk.Body, "stale") {
		t.Errorf("stale MOC (>48h) leaked into body: %q", blk.Body)
	}
}

// TestObsidianMOCLimit — more than 3 recent MOCs → only newest 3 emitted.
func TestObsidianMOCLimit(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	for i, name := range []string{"a", "b", "c", "d", "e"} {
		seedVaultMOC(t, vaultDir, name, "# "+strings.ToUpper(name)+"\n",
			fixedNow.Add(-time.Duration(i+1)*time.Minute))
	}
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	for _, wantName := range []string{"a.md", "b.md", "c.md"} {
		if !strings.Contains(blk.Body, wantName) {
			t.Errorf("expected %s in body (newest 3), got: %q", wantName, blk.Body)
		}
	}
	for _, dropName := range []string{"d.md", "e.md"} {
		if strings.Contains(blk.Body, dropName) {
			t.Errorf("older MOC %s leaked past limit: %q", dropName, blk.Body)
		}
	}
}

// TestObsidianDailyFallback — today's note missing, most-recent dated note
// within 48h (not strictly yesterday) is surfaced.
func TestObsidianDailyFallback(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()

	// Two-day-old note (still inside 48h window).
	twoDays := fixedNow.AddDate(0, 0, -2).Format("2006-01-02")
	// Clamp mtime to exactly -47h so it is within window.
	seedVaultDaily(t, vaultDir, twoDays,
		"# Two days ago\n\nContent.\n",
		fixedNow.Add(-47*time.Hour))
	// Outside window — must be ignored.
	old := fixedNow.AddDate(0, 0, -5).Format("2006-01-02")
	seedVaultDaily(t, vaultDir, old,
		"# Stale\n",
		fixedNow.Add(-120*time.Hour))

	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	wantPath := "daily/" + twoDays + ".md"
	if !strings.Contains(blk.Body, wantPath) {
		t.Errorf("expected fallback daily %s, got: %q", wantPath, blk.Body)
	}
	if strings.Contains(blk.Body, old) {
		t.Errorf("stale daily (>48h) leaked: %q", blk.Body)
	}
}

// TestObsidianSessionSnapshot — a sessions/ file whose frontmatter `cwd:`
// matches the current working directory appears as the `session:` line.
func TestObsidianSessionSnapshot(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	workDir := t.TempDir()
	oldCWD, _ := os.Getwd()
	if err := os.Chdir(workDir); err != nil {
		t.Fatalf("chdir: %v", err)
	}
	t.Cleanup(func() { _ = os.Chdir(oldCWD) })
	// Getwd may return a symlink-resolved path (macOS: /var → /private/var).
	// Use whatever Getwd reports so cwd-matching in the session reader works.
	resolvedCWD, err := os.Getwd()
	if err != nil {
		t.Fatalf("getwd: %v", err)
	}
	cwd := resolvedCWD

	seedVaultSession(t, vaultDir, "20260420-session", cwd, "Working session", fixedNow.Add(-5*time.Minute))
	// A matching older session — must be shadowed by the newer one.
	seedVaultSession(t, vaultDir, "20260419-session", cwd, "Older session", fixedNow.Add(-2*time.Hour))
	// A non-matching session — must not appear.
	seedVaultSession(t, vaultDir, "other-cwd", "/some/other/path", "Other", fixedNow.Add(-10*time.Minute))
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	wantLine := `session: sessions/20260420-session.md — "Working session"`
	if !strings.Contains(blk.Body, wantLine) {
		t.Errorf("expected %q in body, got: %q", wantLine, blk.Body)
	}
	if strings.Contains(blk.Body, "Older session") {
		t.Errorf("older session shadowed newer one: %q", blk.Body)
	}
	if strings.Contains(blk.Body, "Other") {
		t.Errorf("non-matching cwd session leaked: %q", blk.Body)
	}
}

// TestObsidianSessionMismatchedCWD — sessions exist but none match cwd →
// session line omitted entirely.
func TestObsidianSessionMismatchedCWD(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()

	seedVaultSession(t, vaultDir, "20260420-session", "/not/the/current/cwd", "Nope", fixedNow)
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.Contains(blk.Body, "session:") {
		t.Errorf("expected no session line when cwd doesn't match: %q", blk.Body)
	}
}

// TestObsidianDisabledOff — NANIKA_OBSIDIAN_CONTEXT=off → empty body.
func TestObsidianDisabledOff(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	seedVaultDaily(t, vaultDir, "2026-04-20", "# T\n", fixedNow)
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_CONTEXT", "off")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body with CONTEXT=off, got %q", blk.Body)
	}
}

// TestObsidianDisabledZero — NANIKA_OBSIDIAN_CONTEXT=0 → empty body (B6).
func TestObsidianDisabledZero(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	seedVaultDaily(t, vaultDir, "2026-04-20", "# T\n", fixedNow)
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_CONTEXT", "0")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body with CONTEXT=0, got %q", blk.Body)
	}
}

// TestObsidianTruncation — oversize body is cut under the byte budget with
// a trailing marker; never splits a wikilink that appears in heading text.
func TestObsidianTruncation(t *testing.T) {
	clearObsidianEnv(t)
	const budget = 256
	vaultDir := t.TempDir()
	// Lots of MOCs with long headings forces the body past the budget.
	for i := 0; i < 3; i++ {
		name := string(rune('a' + i))
		heading := "# " + strings.Repeat("big heading with [[linked word]] ", 5) + "\n"
		seedVaultMOC(t, vaultDir, name, heading,
			fixedNow.Add(-time.Duration(i+1)*time.Minute))
	}
	seedVaultDaily(t, vaultDir, "2026-04-20",
		"# "+strings.Repeat("Daily heading ", 10)+"\n",
		fixedNow.Add(-1*time.Hour))

	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "256")

	sec := obsidianFixedSection()
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
	// No split wikilink: every [[ has a matching ]].
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

// TestObsidianFirstHeadingSkipsFrontmatter — a note with leading YAML
// frontmatter does not confuse the H1 extractor (ADR risk R3).
func TestObsidianFirstHeadingSkipsFrontmatter(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	body := "---\ntitle: ignored\ntags: [a, b]\n---\n\n# Real heading\n\nContent.\n"
	seedVaultDaily(t, vaultDir, "2026-04-20", body, fixedNow.Add(-1*time.Hour))
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if !strings.Contains(blk.Body, `"Real heading"`) {
		t.Errorf("expected H1 past frontmatter, got: %q", blk.Body)
	}
}

// TestObsidianNoHeading — note without `# ` heading → rendered with empty "".
func TestObsidianNoHeading(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	seedVaultDaily(t, vaultDir, "2026-04-20", "## Only subheading\nBody.\n",
		fixedNow.Add(-1*time.Hour))
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	want := `today: daily/2026-04-20.md — ""`
	if blk.Body != want {
		t.Errorf("no-heading body mismatch:\n got:  %q\n want: %q", blk.Body, want)
	}
}

// TestReadObsidianCacheIfFresh_FreshHit — cache file is newer than all source
// files → readObsidianCacheIfFresh returns the cached body.
func TestReadObsidianCacheIfFresh_FreshHit(t *testing.T) {
	vaultDir := t.TempDir()

	// Source file written first (older mtime).
	sourceTime := fixedNow.Add(-2 * time.Hour)
	seedVaultDaily(t, vaultDir, "2026-04-20", "# Source note\n", sourceTime)

	// Cache file written after all sources (newer mtime).
	cacheTime := fixedNow.Add(-1 * time.Hour)
	cacheDir := filepath.Join(vaultDir, ".cache")
	if err := os.MkdirAll(cacheDir, 0700); err != nil {
		t.Fatalf("mkdir .cache: %v", err)
	}
	cachePath := filepath.Join(cacheDir, "preflight.md")
	const cacheBody = `today: daily/2026-04-20.md — "Source note"`
	if err := os.WriteFile(cachePath, []byte(cacheBody), 0600); err != nil {
		t.Fatalf("write cache: %v", err)
	}
	if err := os.Chtimes(cachePath, cacheTime, cacheTime); err != nil {
		t.Fatalf("chtimes cache: %v", err)
	}

	got := readObsidianCacheIfFresh(vaultDir, fixedNow)
	if got != cacheBody {
		t.Errorf("fresh-cache hit: got %q, want %q", got, cacheBody)
	}
}

// TestReadObsidianCacheIfFresh_StaleCache — a source file is newer than the
// cache → readObsidianCacheIfFresh returns "" (stale miss).
func TestReadObsidianCacheIfFresh_StaleCache(t *testing.T) {
	vaultDir := t.TempDir()

	// Cache file written first (older mtime).
	cacheTime := fixedNow.Add(-2 * time.Hour)
	cacheDir := filepath.Join(vaultDir, ".cache")
	if err := os.MkdirAll(cacheDir, 0700); err != nil {
		t.Fatalf("mkdir .cache: %v", err)
	}
	cachePath := filepath.Join(cacheDir, "preflight.md")
	if err := os.WriteFile(cachePath, []byte("stale cached body"), 0600); err != nil {
		t.Fatalf("write cache: %v", err)
	}
	if err := os.Chtimes(cachePath, cacheTime, cacheTime); err != nil {
		t.Fatalf("chtimes cache: %v", err)
	}

	// Source file written after the cache (newer mtime) → makes cache stale.
	sourceTime := fixedNow.Add(-1 * time.Hour)
	seedVaultMOC(t, vaultDir, "updated", "# Updated MOC\n", sourceTime)

	got := readObsidianCacheIfFresh(vaultDir, fixedNow)
	if got != "" {
		t.Errorf("stale cache: expected empty string, got %q", got)
	}
}

// TestObsidianCancelledContext — W5: a cancelled context causes Fetch to
// return an empty block without error.
func TestObsidianCancelledContext(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()
	seedVaultDaily(t, vaultDir, "2026-04-20", "# Cancelled\n", fixedNow.Add(-1*time.Hour))
	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)

	ctx, cancel := context.WithCancel(context.Background())
	cancel() // cancel immediately

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(ctx)
	if err != nil {
		t.Fatalf("unexpected error on cancelled ctx: %v", err)
	}
	if blk.Body != "" {
		t.Errorf("expected empty body on cancelled ctx, got %q", blk.Body)
	}
}

// TestObsidianTruncateUTF8Boundary — W6: truncation never splits a multi-byte
// UTF-8 rune; output must be valid UTF-8.
func TestObsidianTruncateUTF8Boundary(t *testing.T) {
	// Build a body that contains multi-byte runes (3-byte UTF-8: U+2026 "…")
	// so the budget byte cut lands inside one.
	base := "mocs (48h):\n- mocs/a.md — \"" + strings.Repeat("é", 60) + "\""
	// Set budget so it slices right into a 2-byte é (U+00E9).
	for budget := len(base) - 5; budget <= len(base)-1; budget++ {
		got := truncatePreservingWikilinks(base, budget)
		if got != "" && !utf8.ValidString(got) {
			t.Errorf("budget=%d: truncated output is not valid UTF-8: %q", budget, got)
		}
	}
}

// TestObsidianDotfileSkipped — W8: dotfiles in mocs/ and daily/ are not
// surfaced in the output.
func TestObsidianDotfileSkipped(t *testing.T) {
	clearObsidianEnv(t)
	vaultDir := t.TempDir()

	// Plant a regular MOC and a dotfile MOC.
	seedVaultMOC(t, vaultDir, "real", "# Real MOC\n", fixedNow.Add(-30*time.Minute))
	// Write a dotfile directly (seedVaultMOC prepends no dot).
	dotPath := filepath.Join(vaultDir, "mocs", ".hidden.md")
	if err := os.WriteFile(dotPath, []byte("# Hidden\n"), 0600); err != nil {
		t.Fatalf("write dotfile: %v", err)
	}
	if err := os.Chtimes(dotPath, fixedNow.Add(-1*time.Minute), fixedNow.Add(-1*time.Minute)); err != nil {
		t.Fatalf("chtimes dotfile: %v", err)
	}

	t.Setenv("OBSIDIAN_VAULT_PATH", vaultDir)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "4096")

	sec := obsidianFixedSection()
	blk, err := sec.Fetch(context.Background())
	if err != nil {
		t.Fatalf("unexpected error: %v", err)
	}
	if strings.Contains(blk.Body, ".hidden") {
		t.Errorf("dotfile .hidden.md leaked into output: %q", blk.Body)
	}
	if !strings.Contains(blk.Body, "real.md") {
		t.Errorf("expected real.md in output, got: %q", blk.Body)
	}
}

// TestObsidianTruncateOrphanClose — W7: a body that contains a closing ]]
// but no opening [[ must not be corrupted; the wikilink backup only fires
// when openIdx >= 0.
func TestObsidianTruncateOrphanClose(t *testing.T) {
	body := "today: daily/2026-04-20.md — \"no open wikilink]] here\"\nmocs (48h):\n- mocs/a.md — \"Alpha\""
	// Budget that cuts after the ]] so we can verify the guard.
	budget := len(body) - 10
	got := truncatePreservingWikilinks(body, budget)
	// Must not panic and must not strip content before the orphan ]].
	if strings.Contains(got, "[[") && !strings.Contains(got, "]]") {
		t.Errorf("split wikilink in output: %q", got)
	}
	if got != "" && !utf8.ValidString(got) {
		t.Errorf("invalid UTF-8 after truncation: %q", got)
	}
}

// TestObsidianEnvBudgetOverride — NANIKA_OBSIDIAN_MAX_BYTES overrides the
// default byte budget (1024).
func TestObsidianEnvBudgetOverride(t *testing.T) {
	clearObsidianEnv(t)
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "512")
	if got := obsidianByteBudget(); got != 512 {
		t.Errorf("budget override: got %d, want 512", got)
	}
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "0")
	if got := obsidianByteBudget(); got != obsidianDefaultBytes {
		t.Errorf("budget 0 should fall back to default, got %d", got)
	}
	t.Setenv("NANIKA_OBSIDIAN_MAX_BYTES", "not-a-number")
	if got := obsidianByteBudget(); got != obsidianDefaultBytes {
		t.Errorf("budget invalid should fall back to default, got %d", got)
	}
}
