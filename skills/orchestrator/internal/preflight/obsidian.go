package preflight

import (
	"bufio"
	"context"
	"fmt"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"time"
	"unicode/utf8"
)

const (
	obsidianBlockTitle   = "Narrative Context"
	obsidianSectionName  = "obsidian"
	obsidianPriority     = 25
	obsidianDefaultBytes = 1024
	obsidianMOCLimit     = 3
	obsidianMOCWindow    = 48 * time.Hour

	// headingReadCap limits how many bytes firstHeading will scan from a file
	// while looking for the first `# ` heading (ADR risk R7).
	headingReadCap = 4 * 1024
)

// dailyNameRE matches the spec filename shape `YYYY-MM-DD.md` for the
// daily-note fallback scan (ADR §"Read strategy" #1).
var dailyNameRE = regexp.MustCompile(`^\d{4}-\d{2}-\d{2}\.md$`)

// NOTE: ADR §"Go interface" specifies `func init() { Register(&obsidianSection{}) }`
// with a zero-value section. We register via NewObsidianSection(nil) so the
// exported constructor remains the sole seam for tests that need to pin
// time (T2.1 golden, truncation) without monkey-patching package state.
// Behavior is equivalent — nowTime() falls back to time.Now when now is nil.
func init() { Register(NewObsidianSection(nil)) }

// NewObsidianSection returns an obsidian preflight Section. If now is nil,
// time.Now is used. Tests pass a fixed func for deterministic date assertions.
func NewObsidianSection(now func() time.Time) Section {
	return &obsidianSection{now: now}
}

type obsidianSection struct {
	now func() time.Time
}

func (o *obsidianSection) Name() string  { return obsidianSectionName }
func (o *obsidianSection) Priority() int { return obsidianPriority }

func (o *obsidianSection) nowTime() time.Time {
	if o.now != nil {
		return o.now()
	}
	return time.Now()
}

// Fetch returns the narrative-context block. Per ADR §"Fallback matrix",
// Fetch never returns a non-nil error: every missing-state path yields an
// empty Block so the renderer omits the section.
func (o *obsidianSection) Fetch(ctx context.Context) (Block, error) {
	// W5: bail immediately on cancelled/timed-out context.
	if err := ctx.Err(); err != nil {
		return Block{Title: obsidianBlockTitle}, nil
	}

	if v := os.Getenv("NANIKA_OBSIDIAN_CONTEXT"); strings.EqualFold(v, "off") || v == "0" {
		return Block{Title: obsidianBlockTitle}, nil
	}

	vault := resolveObsidianVault()
	if vault == "" {
		return Block{Title: obsidianBlockTitle}, nil
	}
	fi, err := os.Stat(vault)
	if err != nil {
		if !os.IsNotExist(err) {
			// Non-ENOENT stat errors (e.g. EACCES) indicate misconfiguration,
			// not missing state. Log to stderr per ADR line 268 guidance
			// ("prefer logging to stderr"); still return empty Block so the
			// hook degrades gracefully.
			fmt.Fprintf(os.Stderr, "obsidian preflight: stat vault %q: %v\n", vault, err)
		}
		return Block{Title: obsidianBlockTitle}, nil
	}
	if !fi.IsDir() {
		return Block{Title: obsidianBlockTitle}, nil
	}

	now := o.nowTime()

	// Short-circuit on fresh cache. Skip when NANIKA_OBSIDIAN_NO_CACHE=1.
	noCache := os.Getenv("NANIKA_OBSIDIAN_NO_CACHE") == "1"
	if !noCache {
		if cached := readObsidianCacheIfFresh(vault, now); cached != "" {
			return Block{Title: obsidianBlockTitle, Body: cached}, nil
		}
	}

	// W5: check context before the ReadDir-heavy read phase.
	if err := ctx.Err(); err != nil {
		return Block{Title: obsidianBlockTitle}, nil
	}

	daily := readDailyNote(vault, now)
	mocs := readRecentMOCs(vault, now, obsidianMOCWindow, obsidianMOCLimit)
	session := readSessionSnapshot(vault, currentWorkingDir())

	body := formatObsidianBlock(daily, mocs, session)
	if body == "" {
		return Block{Title: obsidianBlockTitle}, nil
	}
	body = truncatePreservingWikilinks(body, obsidianByteBudget())

	// Write cache best-effort; never blocks or fails the preflight.
	if !noCache && body != "" {
		writeObsidianCache(vault, body)
	}

	return Block{Title: obsidianBlockTitle, Body: body}, nil
}

// readObsidianCacheIfFresh returns the content of <vault>/.cache/preflight.md
// when the cache file exists and no source file under daily/, mocs/, or
// sessions/ has a newer mtime. Returns "" on cache miss, staleness, or any
// I/O error — the caller falls back to the live build path.
func readObsidianCacheIfFresh(vault string, _ time.Time) string {
	cachePath := filepath.Join(vault, ".cache", "preflight.md")
	cfi, err := os.Stat(cachePath)
	if err != nil {
		return ""
	}
	cacheMtime := cfi.ModTime()

	// Any source file newer than the cache makes it stale.
	if maxSourceMtime(vault).After(cacheMtime) {
		return ""
	}

	data, err := os.ReadFile(cachePath)
	if err != nil || len(data) == 0 {
		return ""
	}
	return string(data)
}

// maxSourceMtime returns the most-recent mtime among all .md files under
// <vault>/daily/, <vault>/mocs/, and <vault>/sessions/.
// Returns the zero Time when no files are found or all reads fail.
func maxSourceMtime(vault string) time.Time {
	var max time.Time
	for _, sub := range []string{"daily", "mocs", "sessions"} {
		entries, err := os.ReadDir(filepath.Join(vault, sub))
		if err != nil {
			continue
		}
		for _, e := range entries {
			if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
				continue
			}
			info, err := e.Info()
			if err != nil {
				continue
			}
			if info.ModTime().After(max) {
				max = info.ModTime()
			}
		}
	}
	return max
}

// writeObsidianCache writes body to <vault>/.cache/preflight.md.
// All errors are silently dropped — cache writes are best-effort.
func writeObsidianCache(vault, body string) {
	dir := filepath.Join(vault, ".cache")
	if err := os.MkdirAll(dir, 0700); err != nil {
		return
	}
	_ = os.WriteFile(filepath.Join(dir, "preflight.md"), []byte(body), 0600)
}

// noteRef is a rendered reference to a single note: vault-relative path + H1.
type noteRef struct {
	relPath string // vault-relative (e.g. "daily/2026-04-20.md")
	heading string // first `# ` heading (without the `# ` prefix)
}

func (n noteRef) render() string {
	if n.heading == "" {
		return fmt.Sprintf(`%s — ""`, n.relPath)
	}
	return fmt.Sprintf(`%s — %q`, n.relPath, n.heading)
}

// formatObsidianBlock assembles the spec body shape (ADR §"Output format"):
//
//	today: <rel> — "<H1>"
//	mocs (48h):
//	- <rel> — "<H1>"
//	session: <rel> — "<H1>"
//
// A missing component is omitted entirely (no placeholder line).
func formatObsidianBlock(daily noteRef, mocs []noteRef, session noteRef) string {
	var parts []string
	if daily.relPath != "" {
		parts = append(parts, "today: "+daily.render())
	}
	if len(mocs) > 0 {
		var sb strings.Builder
		sb.WriteString("mocs (48h):")
		for _, m := range mocs {
			sb.WriteString("\n- ")
			sb.WriteString(m.render())
		}
		parts = append(parts, sb.String())
	}
	if session.relPath != "" {
		parts = append(parts, "session: "+session.render())
	}
	return strings.Join(parts, "\n")
}

// readDailyNote surfaces today's `<vault>/daily/<YYYY-MM-DD>.md` or the most
// recent `YYYY-MM-DD.md` file with mtime within the 48h window (ADR §"Read
// strategy" #1 / blocker B4).
func readDailyNote(vault string, now time.Time) noteRef {
	dailyDir := filepath.Join(vault, "daily")
	today := now.Format("2006-01-02") + ".md"

	// Fast path: today's note.
	if fi, err := os.Stat(filepath.Join(dailyDir, today)); err == nil && !fi.IsDir() {
		return makeNoteRef(vault, filepath.Join(dailyDir, today))
	}

	// Fallback: scan daily/ for `YYYY-MM-DD.md` files within 48h.
	entries, err := os.ReadDir(dailyDir)
	if err != nil {
		return noteRef{}
	}
	type cand struct {
		path  string
		mtime time.Time
	}
	var cands []cand
	for _, e := range entries {
		name := e.Name()
		// W8: skip dotfiles; match date pattern case-insensitively for the
		// extension (e.g. 2026-04-20.MD).
		if e.IsDir() || strings.HasPrefix(name, ".") ||
			!strings.EqualFold(filepath.Ext(name), ".md") ||
			!dailyNameRE.MatchString(strings.ToLower(name)) {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		age := now.Sub(info.ModTime())
		if age < 0 || age > obsidianMOCWindow {
			continue
		}
		cands = append(cands, cand{
			path:  filepath.Join(dailyDir, e.Name()),
			mtime: info.ModTime(),
		})
	}
	if len(cands) == 0 {
		return noteRef{}
	}
	sort.SliceStable(cands, func(i, j int) bool {
		return cands[i].mtime.After(cands[j].mtime)
	})
	return makeNoteRef(vault, cands[0].path)
}

// readRecentMOCs returns up to `limit` most-recent MOC files whose mtime is
// within `window` (ADR §"Read strategy" #2 / blocker B3). Output is sorted
// descending by mtime; ties are broken by filename for determinism.
func readRecentMOCs(vault string, now time.Time, window time.Duration, limit int) []noteRef {
	mocsDir := filepath.Join(vault, "mocs")
	entries, err := os.ReadDir(mocsDir)
	if err != nil {
		return nil
	}
	type cand struct {
		path  string
		mtime time.Time
	}
	cands := make([]cand, 0, len(entries))
	for _, e := range entries {
		name := e.Name()
		// W8: skip dotfiles; accept .md case-insensitively (e.g. .MD).
		if e.IsDir() || strings.HasPrefix(name, ".") || !strings.EqualFold(filepath.Ext(name), ".md") {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		age := now.Sub(info.ModTime())
		if age < 0 || age > window {
			continue
		}
		cands = append(cands, cand{
			path:  filepath.Join(mocsDir, e.Name()),
			mtime: info.ModTime(),
		})
	}
	sort.SliceStable(cands, func(i, j int) bool {
		if !cands[i].mtime.Equal(cands[j].mtime) {
			return cands[i].mtime.After(cands[j].mtime)
		}
		return cands[i].path < cands[j].path
	})
	if limit > 0 && len(cands) > limit {
		cands = cands[:limit]
	}
	out := make([]noteRef, 0, len(cands))
	for _, c := range cands {
		out = append(out, makeNoteRef(vault, c.path))
	}
	return out
}

// readSessionSnapshot locates a `<vault>/sessions/*.md` file whose frontmatter
// `cwd:` field matches the supplied cwd. Returns the most-recent match by
// mtime. Empty cwd or missing sessions dir yields an empty ref (ADR §"Read
// strategy" #3 / blocker B1 / risk R5).
func readSessionSnapshot(vault, cwd string) noteRef {
	if cwd == "" {
		return noteRef{}
	}
	sessionsDir := filepath.Join(vault, "sessions")
	entries, err := os.ReadDir(sessionsDir)
	if err != nil {
		return noteRef{}
	}
	type cand struct {
		path  string
		mtime time.Time
	}
	var cands []cand
	for _, e := range entries {
		if e.IsDir() || !strings.HasSuffix(e.Name(), ".md") {
			continue
		}
		p := filepath.Join(sessionsDir, e.Name())
		fmCwd := readFrontmatterCwd(p)
		if fmCwd == "" || fmCwd != cwd {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		cands = append(cands, cand{path: p, mtime: info.ModTime()})
	}
	if len(cands) == 0 {
		return noteRef{}
	}
	sort.SliceStable(cands, func(i, j int) bool {
		return cands[i].mtime.After(cands[j].mtime)
	})
	return makeNoteRef(vault, cands[0].path)
}

// readFrontmatterCwd extracts the `cwd:` field from a leading `---`-fenced
// YAML frontmatter block. Returns "" if the file has no frontmatter or no
// cwd key. Only the frontmatter header is scanned — never loads the whole file.
func readFrontmatterCwd(path string) string {
	f, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 0, 8*1024), 64*1024)

	if !scanner.Scan() || strings.TrimSpace(scanner.Text()) != "---" {
		return ""
	}
	for scanner.Scan() {
		line := scanner.Text()
		trimmed := strings.TrimSpace(line)
		if trimmed == "---" {
			return ""
		}
		if strings.HasPrefix(trimmed, "cwd:") {
			v := strings.TrimSpace(strings.TrimPrefix(trimmed, "cwd:"))
			v = strings.Trim(v, `"'`)
			return v
		}
	}
	return ""
}

// makeNoteRef builds a noteRef from an absolute path inside the vault.
func makeNoteRef(vault, absPath string) noteRef {
	rel, err := filepath.Rel(vault, absPath)
	if err != nil {
		rel = filepath.Base(absPath)
	}
	return noteRef{relPath: filepath.ToSlash(rel), heading: firstHeading(absPath)}
}

// firstHeading returns the text of the first `# ` heading in the file at
// path, skipping any leading YAML frontmatter. Reads at most headingReadCap
// bytes (ADR risk R3/R7). Returns "" on any error or if no heading exists.
func firstHeading(path string) string {
	f, err := os.Open(path)
	if err != nil {
		return ""
	}
	defer f.Close()

	lr := &limitedLineReader{r: bufio.NewReader(f), remaining: headingReadCap}
	inFrontmatter := false
	first := true
	for {
		line, ok := lr.readLine()
		if !ok {
			return ""
		}
		if first {
			first = false
			if strings.TrimSpace(line) == "---" {
				inFrontmatter = true
				continue
			}
		}
		if inFrontmatter {
			if strings.TrimSpace(line) == "---" {
				inFrontmatter = false
			}
			continue
		}
		if strings.HasPrefix(line, "# ") {
			return strings.TrimSpace(strings.TrimPrefix(line, "# "))
		}
	}
}

type limitedLineReader struct {
	r         *bufio.Reader
	remaining int
}

func (l *limitedLineReader) readLine() (string, bool) {
	if l.remaining <= 0 {
		return "", false
	}
	line, err := l.r.ReadString('\n')
	l.remaining -= len(line)
	if err != nil {
		if line == "" {
			return "", false
		}
		return strings.TrimRight(line, "\r\n"), true
	}
	return strings.TrimRight(line, "\r\n"), true
}

// truncatePreservingWikilinks cuts body to at most maxBytes, avoids cutting
// mid-wikilink, and appends the "…" marker on a line boundary.
// On a budget too small to hold the marker + a single line, returns "".
func truncatePreservingWikilinks(body string, maxBytes int) string {
	const marker = "…" // 3 UTF-8 bytes
	if maxBytes <= 0 {
		return body
	}
	if len(body) <= maxBytes {
		return body
	}
	budget := maxBytes - len(marker) - 1 // -1 reserves room for the newline before the marker
	// NOTE: warning W2 — for absurdly small budgets we return empty rather
	// than a stranded bare marker. 16 bytes is enough for any single-line
	// "today:" fragment plus newline+marker; below that we emit nothing.
	if budget < 16 {
		return ""
	}

	cut := body[:budget]

	// W6: back up to a valid UTF-8 rune boundary if the budget split a
	// multi-byte rune.
	for !utf8.ValidString(cut) && len(cut) > 0 {
		cut = cut[:len(cut)-1]
	}

	// W7: only back up past an unclosed wikilink when an opening marker
	// actually exists (openIdx >= 0 guards the -1 sentinel from LastIndex).
	openIdx := strings.LastIndex(cut, "[[")
	closeIdx := strings.LastIndex(cut, "]]")
	if openIdx >= 0 && openIdx > closeIdx {
		cut = cut[:openIdx]
	}

	// Trim to the last newline so the marker starts on a fresh line.
	if idx := strings.LastIndex(cut, "\n"); idx >= 0 {
		cut = cut[:idx]
	}

	return strings.TrimRight(cut, " \t") + "\n" + marker
}

// resolveObsidianVault returns the vault root per ADR §"Vault-root resolution":
//  1. OBSIDIAN_VAULT_PATH env var
//  2. vault_path from the obsidian plugin's config file (~/.obsidian/config)
//  3. "" — unconfigured
//
// NOTE: ADR DECISION (line 296) says "prefer importing
// plugins/obsidian/internal/config.ResolveVaultPath(); inline the 20-line
// equivalent if the module graph rejects it." plugins/obsidian is a separate
// Go module (github.com/joeyhipolito/nanika-obsidian) not required by
// skills/orchestrator/go.mod — adding a replace directive just for this
// resolver is more invasive than the inlined copy below, which is a faithful
// subset of config.ResolveVaultPath + config.Load (plugins/obsidian/internal/config/config.go).
func resolveObsidianVault() string {
	if v := os.Getenv("OBSIDIAN_VAULT_PATH"); v != "" {
		return v
	}
	return readVaultPathFromObsidianConfig()
}

// readVaultPathFromObsidianConfig parses `~/.obsidian/config` (or
// $OBSIDIAN_CONFIG_DIR/config) and returns the `vault_path=` value.
// Mirrors plugins/obsidian/internal/config.{Load, ResolveVaultPath}.
func readVaultPathFromObsidianConfig() string {
	dir := os.Getenv("OBSIDIAN_CONFIG_DIR")
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}
		dir = filepath.Join(home, ".obsidian")
	}
	f, err := os.Open(filepath.Join(dir, "config"))
	if err != nil {
		return ""
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 {
			continue
		}
		if strings.TrimSpace(parts[0]) == "vault_path" {
			return strings.TrimSpace(parts[1])
		}
	}
	return ""
}

// currentWorkingDir returns os.Getwd() or "" on error. Injection seam for
// session-snapshot matching — tests set OBSIDIAN_VAULT_PATH to a tmpdir and
// chdir into whatever cwd the test expects the session reader to match.
var currentWorkingDir = func() string {
	cwd, err := os.Getwd()
	if err != nil {
		return ""
	}
	return cwd
}

// obsidianByteBudget returns the per-block byte budget.
// Reads NANIKA_OBSIDIAN_MAX_BYTES; defaults to obsidianDefaultBytes.
func obsidianByteBudget() int {
	if v := os.Getenv("NANIKA_OBSIDIAN_MAX_BYTES"); v != "" {
		if n, err := strconv.Atoi(v); err == nil && n > 0 {
			return n
		}
	}
	return obsidianDefaultBytes
}
