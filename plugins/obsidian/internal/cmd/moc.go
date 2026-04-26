package cmd

import (
	"context"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

const (
	mocModel       = "claude-haiku-4-5-20251001"
	mocMaxTokens   = 2000
	mocMaxZettels  = 8   // in-prompt cap; remainder cited by path only
	mocBodyPreview = 200 // chars per zettel body in the prompt
	mocMinZettels  = 5   // concept threshold
	mocDryRunHead  = 500 // chars of dry-run draft preview
)

// MOCOptions holds flags for the moc generate command.
type MOCOptions struct {
	Kind    vault.VaultKind // vault kind; zero value = KindNanika
	DryRun  bool
	Drafter Drafter // optional override; nil means use NewMOCDrafter()
}

// Concept is a candidate cluster of zettels sharing one tag.
type Concept struct {
	Slug    string   // slugified tag, used for filename + ordering
	Tag     string   // original tag as written in frontmatter
	Zettels []string // vault-relative paths, deduplicated, sorted
}

// Drafter generates a MOC body for a Concept.
type Drafter interface {
	Draft(ctx context.Context, c Concept, zettels []zettelDigest) (string, error)
}

// claudeCLIDrafter generates a MOC body for a Concept using the Claude CLI.
type claudeCLIDrafter struct {
	cliPath string
}

// NewMOCDrafter returns a drafter, or nil when the `claude` CLI is not found on PATH.
// Returns nil so callers can use nil as a "no LLM" sentinel.
func NewMOCDrafter() Drafter {
	path, err := exec.LookPath("claude")
	if err != nil {
		return nil
	}
	return &claudeCLIDrafter{cliPath: path}
}

// Draft asks Claude to synthesize the MOC body for a concept via the CLI.
// Returns the model body verbatim (whitespace-trimmed, no frontmatter).
func (d *claudeCLIDrafter) Draft(ctx context.Context, c Concept, zettels []zettelDigest) (string, error) {
	prompt := buildMOCPrompt(c, zettels)
	cmd := exec.CommandContext(ctx, d.cliPath, "-p", prompt)
	output, err := cmd.Output()
	if err != nil {
		return "", fmt.Errorf("claude CLI: %w", err)
	}
	return strings.TrimSpace(string(output)), nil
}

// zettelDigest carries the in-prompt summary of a single contributing zettel.
type zettelDigest struct {
	RelPath     string
	Frontmatter string   // raw frontmatter block (between --- delimiters), trimmed
	BodyHead    string   // first mocBodyPreview chars of the body, trimmed
	Stem        string   // filename stem (basename without .md extension)
	Slug        string   // frontmatter slug field, or "" when absent
	Tags        []string // parsed tag list from frontmatter
}

var (
	slugTokenSplitRe = regexp.MustCompile(`[^a-zA-Z0-9]+`)
	slugDigitOnlyRe  = regexp.MustCompile(`^\d+$`)
	slugYearRe       = regexp.MustCompile(`^(19|20)\d{2}$`)
	slugTRKIDRe      = regexp.MustCompile(`^trk\d*$`)
	slugDatePrefixRe = regexp.MustCompile(`^\d{4}-\d{2}-\d{2}-`)
)

// slugStopwords is the 13 workflow verbs that appear frequently in zettel
// slugs but carry no concept signal. Workflow-state words ("polish", "chat",
// "dashboard") are intentionally NOT stopped — those are real concepts.
var slugStopwords = map[string]bool{
	"phase":     true,
	"fix":       true,
	"impl":      true,
	"implement": true,
	"add":       true,
	"remove":    true,
	"update":    true,
	"test":      true,
	"tests":     true,
	"review":    true,
	"verify":    true,
	"wip":       true,
	"tmp":       true,
}

// extractSlugTokens splits a zettel's slug into concept-bearing tokens.
// Source: frontmatter slug field if present and non-empty, otherwise the
// filename stem with leading YYYY-MM-DD- date prefix stripped.
// Rules applied in order: split on non-alphanumeric, lowercase, drop len<3,
// drop pure-digit strings, drop year-like tokens (19xx/20xx), drop TRK-IDs
// (trk\d*), drop workflow-verb stopwords.
func extractSlugTokens(z zettelDigest) []string {
	source := z.Slug
	if source == "" {
		source = slugDatePrefixRe.ReplaceAllString(z.Stem, "")
	}
	parts := slugTokenSplitRe.Split(source, -1)
	out := make([]string, 0, len(parts))
	for _, p := range parts {
		p = strings.ToLower(p)
		if len(p) < 3 {
			continue
		}
		if slugDigitOnlyRe.MatchString(p) {
			continue
		}
		if slugYearRe.MatchString(p) {
			continue
		}
		if slugTRKIDRe.MatchString(p) {
			continue
		}
		if slugStopwords[p] {
			continue
		}
		out = append(out, p)
	}
	return out
}

// MOCStats tracks the per-run outcome counts for the stdout summary.
type MOCStats struct {
	Candidates       int
	Written          int
	SkippedTombstone int
	Failed           int
}

// MOCCmd implements `obsidian moc generate`.
// Walks the configured zettel roots, clusters by tag, drops tombstoned
// concepts, asks Haiku for a draft, and writes mocs/<slug>.md atomically.
// Per-concept failures are logged and the run continues. Setup failures
// (vault unreadable, log unopenable) propagate so the scheduler marks the
// job failed.
func MOCCmd(vaultPath string, opts MOCOptions) error {
	if vaultPath == "" {
		return fmt.Errorf("vault path is empty")
	}
	if _, err := os.Stat(vaultPath); err != nil {
		return fmt.Errorf("stat vault: %w", err)
	}

	logger, closeLog, err := openMOCLogger(vaultPath)
	if err != nil {
		return err
	}
	defer closeLog()

	schema := vault.SchemaFor(opts.Kind)

	concepts, err := detectConcepts(vaultPath, schema)
	if err != nil {
		return fmt.Errorf("detect concepts: %w", err)
	}
	stats := MOCStats{Candidates: len(concepts)}

	if opts.DryRun {
		runMOCDryRun(vaultPath, concepts, schema)
		fmt.Printf("auto-moc: candidates=%d written=0 skipped_tombstoned=0 failed=0 (dry-run)\n", stats.Candidates)
		return nil
	}

	// drafter is nil when claude CLI is not on PATH; the loop falls back to
	// stubDraftBody so a structural MOC is still written for review.
	drafter := opts.Drafter
	if drafter == nil {
		drafter = NewMOCDrafter()
	}

	if err := os.MkdirAll(filepath.Join(vaultPath, schema.MOCs), 0o755); err != nil {
		return fmt.Errorf("mkdir %s: %w", schema.MOCs, err)
	}

	ctx := context.Background()
	now := time.Now().UTC()
	for _, c := range concepts {
		tombstoned, terr := isTombstoned(vaultPath, c.Slug, schema)
		if terr != nil {
			logger.Error("tombstone check failed", "slug", c.Slug, "err", terr)
			stats.Failed++
			continue
		}
		if tombstoned {
			logger.Info("skipped (tombstoned)", "slug", c.Slug)
			stats.SkippedTombstone++
			continue
		}

		var body string
		if drafter != nil {
			digests, derr := buildZettelDigests(vaultPath, c.Zettels)
			if derr != nil {
				logger.Error("digest build failed", "slug", c.Slug, "err", derr)
				stats.Failed++
				continue
			}
			var err error
			body, err = drafter.Draft(ctx, c, digests)
			if err != nil {
				logger.Error("haiku draft failed", "slug", c.Slug, "tag", c.Tag, "zettels", len(c.Zettels), "err", err)
				stats.Failed++
				continue
			}
		} else {
			body = stubDraftBody(c)
			logger.Info("wrote stub draft (claude CLI not available)", "slug", c.Slug)
		}

		if err := writeMOC(vaultPath, c, body, now, schema); err != nil {
			logger.Error("write moc failed", "slug", c.Slug, "err", err)
			stats.Failed++
			continue
		}
		logger.Info("wrote moc", "slug", c.Slug, "zettels", len(c.Zettels))
		stats.Written++
	}

	fmt.Printf("auto-moc: candidates=%d written=%d skipped_tombstoned=%d failed=%d\n",
		stats.Candidates, stats.Written, stats.SkippedTombstone, stats.Failed)
	return nil
}

// stubDraftBody returns a structural MOC body without calling an LLM.
// Used when claude CLI is not available so the scheduler job still writes a
// reviewable file rather than failing the run entirely.
func stubDraftBody(c Concept) string {
	var b strings.Builder
	fmt.Fprintf(&b, "## Summary\n\n*Stub draft — no `claude` CLI available at generation time. Promote to active after editing.*\n\nThis MOC covers the concept **%s**, synthesised from %d contributing zettels.\n\n", conceptDisplayName(c.Tag), len(c.Zettels))
	b.WriteString("## Key Threads\n\n")
	for _, p := range c.Zettels {
		base := strings.TrimSuffix(filepath.Base(p), ".md")
		fmt.Fprintf(&b, "- [[%s]]\n", base)
	}
	b.WriteString("\n## Lessons\n\n- *(to be filled after review)*\n\n")
	b.WriteString("## Narrative\n\n*(to be filled after review)*\n")
	return b.String()
}

// runMOCDryRun prints candidate concepts and a draft preview without calling
// Haiku or writing anything. Used by the verify phase to enumerate candidates
// cheaply.
func runMOCDryRun(vaultPath string, concepts []Concept, schemas ...vault.Schema) {
	schema := vault.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	for _, c := range concepts {
		fmt.Printf("[dry-run] concept=%s tag=%s zettels=%d\n", c.Slug, c.Tag, len(c.Zettels))
		head := c.Zettels
		if len(head) > mocMaxZettels {
			head = head[:mocMaxZettels]
		}
		fmt.Println("  paths (first 8):")
		for _, p := range head {
			fmt.Printf("    - %s\n", p)
		}
		fmt.Printf("  would write: %s\n", filepath.Join(vaultPath, schema.MOCs, c.Slug+".md"))

		digests, err := buildZettelDigests(vaultPath, c.Zettels)
		if err != nil {
			fmt.Printf("  draft preview: <digest error: %v>\n", err)
			continue
		}
		preview := buildMOCPrompt(c, digests)
		if len(preview) > mocDryRunHead {
			preview = preview[:mocDryRunHead]
		}
		fmt.Println("  draft preview (first 500 chars of prompt):")
		for _, line := range strings.Split(preview, "\n") {
			fmt.Printf("    %s\n", line)
		}
	}
}

// detectConcepts walks the scan roots and returns the union of tag-based and
// slug-token-based concepts that meet mocMinZettels. Tag-based wins on slug
// collision. The result is capped at 20 by cluster size, then sorted by slug.
func detectConcepts(vaultPath string, schemas ...vault.Schema) ([]Concept, error) {
	schema := vault.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	zettels, err := walkVaultZettels(vaultPath, schema)
	if err != nil {
		return nil, err
	}

	tagConcepts := detectConceptsByTag(zettels, mocMinZettels)
	slugConcepts := detectConceptsBySlugToken(zettels, mocMinZettels)

	// union with dedup; tag-based wins ties on identical slug
	seen := make(map[string]struct{}, len(tagConcepts))
	merged := make([]Concept, 0, len(tagConcepts)+len(slugConcepts))
	for _, c := range tagConcepts {
		seen[c.Slug] = struct{}{}
		merged = append(merged, c)
	}
	for _, c := range slugConcepts {
		if _, ok := seen[c.Slug]; ok {
			continue
		}
		seen[c.Slug] = struct{}{}
		merged = append(merged, c)
	}

	// top-20 by cluster size; stable on equal size using slug as tiebreak
	sort.Slice(merged, func(i, j int) bool {
		if len(merged[i].Zettels) != len(merged[j].Zettels) {
			return len(merged[i].Zettels) > len(merged[j].Zettels)
		}
		return merged[i].Slug < merged[j].Slug
	})
	if len(merged) > 20 {
		merged = merged[:20]
	}

	// re-sort by slug for deterministic caller output
	sort.Slice(merged, func(i, j int) bool { return merged[i].Slug < merged[j].Slug })
	return merged, nil
}

// walkVaultZettels walks schema.ScanRoots and returns a lightweight zettelDigest
// (RelPath, Stem, Tags) for every .md file found. Frontmatter and BodyHead
// are left empty — those are populated by buildZettelDigests for prompt use.
func walkVaultZettels(vaultPath string, schemas ...vault.Schema) ([]zettelDigest, error) {
	schema := vault.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	var zettels []zettelDigest
	for _, root := range schema.ScanRoots {
		rootDir := filepath.Join(vaultPath, root)
		if _, err := os.Stat(rootDir); errors.Is(err, fs.ErrNotExist) {
			continue
		}
		err := filepath.WalkDir(rootDir, func(path string, d fs.DirEntry, walkErr error) error {
			if walkErr != nil {
				return nil // skip unreadable entries; do not abort
			}
			if d.IsDir() {
				if strings.HasPrefix(d.Name(), ".") {
					return filepath.SkipDir
				}
				return nil
			}
			if strings.HasPrefix(d.Name(), ".") || !strings.HasSuffix(d.Name(), ".md") {
				return nil
			}
			data, err := os.ReadFile(path)
			if err != nil {
				return nil
			}
			note := vault.ParseNote(string(data))
			rel, rerr := filepath.Rel(vaultPath, path)
			if rerr != nil {
				return nil
			}
			stem := strings.TrimSuffix(filepath.Base(rel), ".md")
			zettels = append(zettels, zettelDigest{
				RelPath: rel,
				Stem:    stem,
				Slug:    frontmatterSlug(note.Frontmatter),
				Tags:    frontmatterTagList(note.Frontmatter),
			})
			return nil
		})
		if err != nil {
			return nil, fmt.Errorf("walk %s: %w", root, err)
		}
	}
	return zettels, nil
}

// detectConceptsByTag groups zettels by frontmatter tag and returns concepts
// that reach minZettels distinct files, sorted by slug.
func detectConceptsByTag(zettels []zettelDigest, minZettels int) []Concept {
	tagToPaths := map[string]map[string]struct{}{}
	tagOriginal := map[string]string{}

	for _, z := range zettels {
		for _, tag := range z.Tags {
			tag = strings.TrimSpace(tag)
			if tag == "" {
				continue
			}
			slug := slugify(tag)
			if slug == "" || slug == "untitled" {
				continue
			}
			bucket, ok := tagToPaths[slug]
			if !ok {
				bucket = map[string]struct{}{}
				tagToPaths[slug] = bucket
				tagOriginal[slug] = tag
			}
			bucket[z.RelPath] = struct{}{}
		}
	}

	concepts := make([]Concept, 0, len(tagToPaths))
	for slug, paths := range tagToPaths {
		if len(paths) < minZettels {
			continue
		}
		sorted := make([]string, 0, len(paths))
		for p := range paths {
			sorted = append(sorted, p)
		}
		sort.Strings(sorted)
		concepts = append(concepts, Concept{
			Slug:    slug,
			Tag:     tagOriginal[slug],
			Zettels: sorted,
		})
	}
	sort.Slice(concepts, func(i, j int) bool { return concepts[i].Slug < concepts[j].Slug })
	return concepts
}

// detectConceptsBySlugToken groups zettels by tokens extracted from their
// filename stem and returns concepts that reach minZettels distinct files,
// sorted by slug.
func detectConceptsBySlugToken(zettels []zettelDigest, minZettels int) []Concept {
	tokenToPaths := map[string]map[string]struct{}{}

	for _, z := range zettels {
		for _, tok := range extractSlugTokens(z) {
			bucket, ok := tokenToPaths[tok]
			if !ok {
				bucket = map[string]struct{}{}
				tokenToPaths[tok] = bucket
			}
			bucket[z.RelPath] = struct{}{}
		}
	}

	concepts := make([]Concept, 0, len(tokenToPaths))
	for tok, paths := range tokenToPaths {
		if len(paths) < minZettels {
			continue
		}
		sorted := make([]string, 0, len(paths))
		for p := range paths {
			sorted = append(sorted, p)
		}
		sort.Strings(sorted)
		concepts = append(concepts, Concept{
			Slug:    tok,
			Tag:     tok,
			Zettels: sorted,
		})
	}
	sort.Slice(concepts, func(i, j int) bool { return concepts[i].Slug < concepts[j].Slug })
	return concepts
}

// frontmatterSlug extracts the slug field as a trimmed string, or "" when absent.
func frontmatterSlug(fm map[string]any) string {
	v, ok := fm["slug"]
	if !ok {
		return ""
	}
	s, ok := v.(string)
	if !ok {
		return ""
	}
	return strings.TrimSpace(s)
}

// frontmatterTagList extracts the tags field as a flat string slice,
// accepting []string, []any, and bare-string shapes that the lightweight
// frontmatter parser may emit.
func frontmatterTagList(fm map[string]any) []string {
	v, ok := fm["tags"]
	if !ok {
		return nil
	}
	switch vv := v.(type) {
	case []string:
		return vv
	case []any:
		out := make([]string, 0, len(vv))
		for _, item := range vv {
			out = append(out, fmt.Sprint(item))
		}
		return out
	case string:
		return []string{vv}
	}
	return nil
}

// isTombstoned reports whether mocs/<slug>.md already exists with any frontmatter.
// "Any status: key present" tombstones; "any frontmatter at all" also skips the
// concept (defensive guard for hand-authored MOCs without a status field).
// Read errors fail closed: an unreadable existing MOC is treated as tombstoned
// so the run never overwrites partial state.
func isTombstoned(vaultPath, slug string, schemas ...vault.Schema) (bool, error) {
	schema := vault.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	mocPath := filepath.Join(vaultPath, schema.MOCs, slug+".md")
	raw, err := os.ReadFile(mocPath)
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return true, fmt.Errorf("read %s: %w", mocPath, err)
	}
	note := vault.ParseNote(string(raw))
	// Any `status:` key (even empty value, even a future value the user
	// invents) tombstones — symmetric over future state, no allowlist.
	if _, ok := note.Frontmatter["status"]; ok {
		return true, nil
	}
	// File exists without status — still skip. Defensive guard for
	// hand-authored MOCs; users opt back in by adding status: draft.
	return true, nil
}

// buildZettelDigests reads each zettel and returns its raw frontmatter block
// plus a body preview, bounded at mocMaxZettels for prompt-cost predictability.
func buildZettelDigests(vaultPath string, zettels []string) ([]zettelDigest, error) {
	limit := len(zettels)
	if limit > mocMaxZettels {
		limit = mocMaxZettels
	}
	digests := make([]zettelDigest, 0, limit)
	for _, rel := range zettels[:limit] {
		full := filepath.Join(vaultPath, rel)
		data, err := os.ReadFile(full)
		if err != nil {
			return nil, fmt.Errorf("read %s: %w", rel, err)
		}
		fmBlock, body := splitRawFrontmatter(string(data))
		body = strings.TrimSpace(body)
		if len(body) > mocBodyPreview {
			body = body[:mocBodyPreview]
		}
		digests = append(digests, zettelDigest{
			RelPath:     rel,
			Frontmatter: strings.TrimSpace(fmBlock),
			BodyHead:    body,
		})
	}
	return digests, nil
}

// splitRawFrontmatter returns the raw YAML between --- delimiters and the
// remaining body. Mirrors vault.splitFrontmatter but exposes the raw block
// (vault.ParseNote returns parsed map only).
func splitRawFrontmatter(content string) (string, string) {
	if !strings.HasPrefix(content, "---\n") && !strings.HasPrefix(content, "---\r\n") {
		return "", content
	}
	rest := content[3:]
	if strings.HasPrefix(rest, "\r\n") {
		rest = rest[2:]
	} else if strings.HasPrefix(rest, "\n") {
		rest = rest[1:]
	}
	idx := strings.Index(rest, "\n---\n")
	if idx == -1 {
		idx = strings.Index(rest, "\r\n---\r\n")
		if idx == -1 {
			return "", content
		}
	}
	fm := rest[:idx]
	body := rest[idx:]
	if nlIdx := strings.Index(body[1:], "\n"); nlIdx != -1 {
		body = body[nlIdx+2:]
	} else {
		body = ""
	}
	return fm, body
}

// buildMOCPrompt renders the user-message text sent to Haiku. The system
// instructions are inlined at the top — the existing callAnthropicMessages
// helper does not plumb a separate system field, and inlining matches the
// pattern used by canonicalize.go.
func buildMOCPrompt(c Concept, zettels []zettelDigest) string {
	var sb strings.Builder
	sb.WriteString(mocSystemPrompt)
	sb.WriteString("\n\n")
	fmt.Fprintf(&sb, "Concept: %s\n", conceptDisplayName(c.Tag))
	fmt.Fprintf(&sb, "Tag: %s\n", c.Tag)
	fmt.Fprintf(&sb, "Zettel count: %d\n", len(c.Zettels))
	fmt.Fprintf(&sb, "In-prompt zettels: %d\n\n", len(zettels))
	sb.WriteString("Contributing zettels (first 8):\n\n")
	for _, z := range zettels {
		fmt.Fprintf(&sb, "--- %s ---\n", z.RelPath)
		if z.Frontmatter != "" {
			sb.WriteString(z.Frontmatter)
			sb.WriteString("\n")
		}
		sb.WriteString(z.BodyHead)
		sb.WriteString("\n\n")
	}
	if len(c.Zettels) > len(zettels) {
		sb.WriteString("Additional zettels (cite by path, no summary included):\n")
		for _, p := range c.Zettels[len(zettels):] {
			fmt.Fprintf(&sb, "- %s\n", p)
		}
	}
	return sb.String()
}

const mocSystemPrompt = `You are drafting a Map-of-Content (MOC) note for a personal Obsidian vault.
The MOC consolidates multiple zettels that share a concept. The user will review
this draft, promote it to status: active, mark it status: rejected, or delete it.

Write in plain markdown. Do NOT include frontmatter — the caller prepends it.
Structure the body EXACTLY with these four second-level headings, in order:

## Summary
One paragraph (3-5 sentences) naming the concept and the shared thread.

## Key Threads
Bullet list. Each bullet = one distinct sub-topic across the zettels, with a
[[wikilink]] to the zettel(s) that introduced it.

## Lessons
Bullet list. Extract durable insights, decisions, gotchas, or patterns. Prefer
concrete, attributable lines over generic advice.

## Narrative
One to three paragraphs threading the zettels chronologically. Cite zettels as
[[filename-without-extension]] when their specific content is referenced.

Rules:
- Never invent details not present in the provided summaries.
- If the zettels disagree, surface the disagreement in Narrative — do not resolve it.
- Keep the whole document under 2000 output tokens.`

// conceptDisplayName turns a kebab-case tag into Title Case.
func conceptDisplayName(tag string) string {
	parts := strings.Split(tag, "-")
	for i, p := range parts {
		if p == "" {
			continue
		}
		parts[i] = strings.ToUpper(p[:1]) + p[1:]
	}
	return strings.Join(parts, " ")
}

// writeMOC assembles frontmatter + body and writes mocs/<slug>.md atomically
// via tmpfile + rename. A half-written MOC carrying a `status:` key would
// tombstone the concept permanently, so atomicity is non-negotiable.
func writeMOC(vaultPath string, c Concept, body string, now time.Time, schemas ...vault.Schema) error {
	schema := vault.NanikaSchema
	if len(schemas) > 0 {
		schema = schemas[0]
	}
	mocPath := filepath.Join(vaultPath, schema.MOCs, c.Slug+".md")
	doc := buildMOCDoc(c, body, now)

	tmp := mocPath + ".tmp"
	if err := os.WriteFile(tmp, []byte(doc), 0o644); err != nil {
		return fmt.Errorf("write tmp: %w", err)
	}
	if err := os.Rename(tmp, mocPath); err != nil {
		_ = os.Remove(tmp)
		return fmt.Errorf("rename tmp -> moc: %w", err)
	}
	return nil
}

// buildMOCDoc assembles the on-disk MOC document with deterministic
// frontmatter ordering. status is ALWAYS draft on the auto path; promotion
// is a manual user action.
func buildMOCDoc(c Concept, body string, now time.Time) string {
	var b strings.Builder
	b.WriteString("---\n")
	b.WriteString("type: moc\n")
	b.WriteString("status: draft\n")
	fmt.Fprintf(&b, "title: %s\n", conceptDisplayName(c.Tag))
	b.WriteString("tags:\n")
	fmt.Fprintf(&b, "  - %s\n", c.Slug)
	b.WriteString("  - auto-drafted\n")
	fmt.Fprintf(&b, "generated: %s\n", now.Format("2006-01-02"))
	b.WriteString("contributing_zettels:\n")
	for _, p := range c.Zettels {
		fmt.Fprintf(&b, "  - %s\n", p)
	}
	b.WriteString("---\n\n")
	b.WriteString(strings.TrimSpace(body))
	b.WriteString("\n")
	return b.String()
}

// openMOCLogger opens the auto-moc log under the workspace shared dir when
// run by the orchestrator, or under <vault>/.nanika/ for ad-hoc invocations.
// Both paths must work — the verify phase reads the workspace copy, the
// scheduled run writes the vault-local copy.
func openMOCLogger(vaultPath string) (*slog.Logger, func(), error) {
	var logPath string
	if shared := os.Getenv("NANIKA_WORKSPACE_SHARED"); shared != "" {
		logPath = filepath.Join(shared, "artifacts", "auto-moc.log")
	} else {
		logPath = filepath.Join(vaultPath, ".nanika", "auto-moc.log")
	}
	if err := os.MkdirAll(filepath.Dir(logPath), 0o755); err != nil {
		return nil, func() {}, fmt.Errorf("mkdir log dir: %w", err)
	}
	logFile, err := os.OpenFile(logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o644)
	if err != nil {
		return nil, func() {}, fmt.Errorf("open auto-moc log: %w", err)
	}
	logger := slog.New(slog.NewTextHandler(logFile, nil))
	return logger, func() { _ = logFile.Close() }, nil
}
