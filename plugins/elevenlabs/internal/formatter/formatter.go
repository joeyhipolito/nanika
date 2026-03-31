// Package formatter parses narration scripts and formats them for ElevenLabs TTS.
package formatter

import (
	"regexp"
	"strings"
)

var (
	// sceneLineRe matches a line that is only a [SCENE: ...] block.
	sceneLineRe = regexp.MustCompile(`(?i)^\s*\[SCENE:[^\]]*\]\s*$`)
	// sceneInlineRe strips inline [SCENE: ...] fragments left in narration lines.
	sceneInlineRe = regexp.MustCompile(`(?i)\[SCENE:[^\]]*\]`)
	// hrRe matches horizontal rules (--- or *** etc.).
	hrRe = regexp.MustCompile(`^[-*_]{3,}\s*$`)
	// boldMetaRe matches front-matter-style bold lines:
	//   **Key:** value   — key outside bold span followed by value
	//   **Key: value**   — entire key-value inside bold span (trailing part optional)
	boldMetaRe = regexp.MustCompile(`^\*\*[A-Za-z][^*]*\*\*([:\s].*)?$`)
	// clipHeaderRe matches clip header lines in two formats:
	//   Legacy:  **[0–8] Clip 1**  or  **[0–8] Clip 1** — VISUAL ONLY
	//   Current: **[0:00–0:08] | Narrated**  or  **[0:00–0:08] | VISUAL ONLY**
	clipHeaderRe = regexp.MustCompile(`(?i)^\*\*\[[\d:–\-]+\]\s*(Clip\s+\d+|\|\s*(Narrated|VISUAL\s+ONLY))`)
	// visualOnlyRe matches VISUAL ONLY markers in both legacy and current formats.
	//   Legacy:  **[N] Clip N** — VISUAL ONLY  or  **VISUAL-ONLY SECTION**
	//   Current: **[0:00–0:08] | VISUAL ONLY**
	visualOnlyRe = regexp.MustCompile(`(?i)(\*\*\s*VISUAL[-\s]ONLY\s+SECTION|[—|]\s*VISUAL\s+ONLY)`)
	// parenOnlyRe matches lines that are entirely a parenthetical note.
	parenOnlyRe = regexp.MustCompile(`^\s*\(.*\)\s*$`)
	// parenStartRe matches the opening of a multi-line paren block.
	parenStartRe = regexp.MustCompile(`^\s*\(`)
	// boldItalicRe, boldRe, italicRe strip inline markdown emphasis.
	boldItalicRe = regexp.MustCompile(`\*\*\*([^*]+)\*\*\*`)
	boldRe       = regexp.MustCompile(`\*\*([^*]+)\*\*`)
	italicRe     = regexp.MustCompile(`\*([^*]+)\*`)
	// dquoteRe strips ASCII and curly double-quote characters.
	dquoteRe = regexp.MustCompile(`["\x{201C}\x{201D}]`)
	// multiSpaceRe collapses runs of spaces.
	multiSpaceRe = regexp.MustCompile(` {2,}`)
	// checkmarkLineRe matches script summary checklist lines (✓ or ✗ prefix).
	checkmarkLineRe = regexp.MustCompile(`^[✓✗✔✘]\s`)
)

// metadataHeaders are known section titles to strip entirely (header + content).
// Comparison is case-insensitive substring match on the title after stripping #/[/].
var metadataHeaders = []string{
	"self-check",
	"clip budget",
	"narrator notes",
	"notes for editor",
	"scene markers",
	"timing reference",
	"learning",
	"sources",
	"script metadata",
	"narrative techniques",
	"delivery notes",
	"script summary",
	"script characteristics",
	"script notes",
}

// isMetadataHeader reports whether a section title belongs to a metadata-only section.
func isMetadataHeader(title string) bool {
	h := strings.ToLower(strings.Trim(title, " []"))
	for _, m := range metadataHeaders {
		if strings.Contains(h, m) {
			return true
		}
	}
	return false
}

// Format parses a narration script markdown file and returns text ready for
// ElevenLabs TTS with v3 audio tags, normalized numbers, and normalized abbreviations.
// When clip headers are present, it also returns a ClipManifest describing the
// clip structure and silence requirements for downstream assembly.
//
// The formatter:
//   - Strips [SCENE: ...] blocks, clip headers, section headers, metadata sections
//   - Strips delivery notes (parenthetical lines) and quotation marks
//   - Strips VISUAL-ONLY blocks (no TTS output for visual-only content)
//   - Adds v3 audio tags: [documentary style], [pause], [long pause]
//   - Normalizes numbers (16 million → sixteen million)
//   - Normalizes abbreviations (AI → A.I., R&D → R and D)
func Format(src []byte) ([]byte, *ClipManifest, error) {
	lines := strings.Split(string(src), "\n")
	lines = stripMetadataSections(lines)
	text, manifest := processLines(lines)
	text = normalizeNumbers(text)
	text = normalizeAbbreviations(text)
	text = cleanupText(text)
	return []byte(text), manifest, nil
}

// stripMetadataSections removes known metadata sections (header + all content)
// and bare section-header lines for narration sections (keeps content).
func stripMetadataSections(lines []string) []string {
	out := make([]string, 0, len(lines))
	skipSection := false

	for _, line := range lines {
		trimmed := strings.TrimSpace(line)

		if strings.HasPrefix(trimmed, "#") {
			level := 0
			for _, c := range trimmed {
				if c == '#' {
					level++
				} else {
					break
				}
			}
			title := strings.TrimSpace(trimmed[level:])

			if level == 1 {
				// H1 is the script title — drop it but end any skip.
				skipSection = false
				continue
			}
			if level >= 2 {
				if isMetadataHeader(title) {
					skipSection = true
					continue
				}
				// Narration section header: end skip, drop the header line itself.
				skipSection = false
				continue
			}
		}

		if skipSection {
			continue
		}
		out = append(out, line)
	}
	return out
}

// processLines converts filtered lines to narration paragraphs with v3 audio tags.
// When clip headers are present, it tracks clip structure and returns a manifest.
func processLines(lines []string) (string, *ClipManifest) {
	type paragraph struct {
		tag  string // optional leading tag ([pause], [long pause])
		text string
	}

	var paragraphs []paragraph
	var buf []string
	pendingTag := ""
	inVisualOnly := false
	inParenBlock := false
	firstParagraph := true

	// Clip tracking.
	var allClips []clipEntry
	var segments []ManifestEntry
	clipGridSecs := 0
	hasClipHeaders := false
	currentClipNum := 0

	flush := func() {
		joined := strings.Join(buf, " ")
		joined = strings.TrimSpace(joined)
		if joined == "" {
			buf = nil
			return
		}
		p := paragraph{tag: pendingTag, text: joined}
		pendingTag = ""
		paragraphs = append(paragraphs, p)
		buf = nil
	}

	for _, line := range lines {
		trimmed := strings.TrimSpace(line)

		// ── multi-line parenthetical delivery note ────────────────────────────
		if inParenBlock {
			if strings.HasSuffix(trimmed, ")") {
				inParenBlock = false
			}
			continue
		}

		// ── VISUAL-ONLY section exit ─────────────────────────────────────────
		if inVisualOnly {
			if clipHeaderRe.MatchString(trimmed) {
				// Any clip header exits visual-only; fall through to clip handler.
				inVisualOnly = false
			} else if hrRe.MatchString(trimmed) {
				inVisualOnly = false
				// In legacy mode (no clip headers), preserve [long pause] for the
				// transition out of visual-only. With clip headers, the HR is just
				// a visual separator — no audio tag emitted.
				if !hasClipHeaders && len(paragraphs) > 0 {
					pendingTag = "[long pause]"
				}
				continue
			} else {
				continue
			}
		}

		// ── clip header lines ────────────────────────────────────────────────
		if clipHeaderRe.MatchString(trimmed) {
			hasClipHeaders = true
			currentClipNum++
			if clipGridSecs == 0 {
				clipGridSecs = parseClipGridSeconds(trimmed)
			}
			flush()
			if visualOnlyRe.MatchString(trimmed) {
				inVisualOnly = true
				allClips = append(allClips, clipEntry{number: currentClipNum, kind: clipVisualOnly})
			} else {
				allClips = append(allClips, clipEntry{number: currentClipNum, kind: clipNarrated})
				segments = append(segments, ManifestEntry{Index: len(segments), Clip: currentClipNum})
				if len(paragraphs) > 0 {
					pendingTag = "[pause]"
				}
			}
			continue
		}

		// ── standalone VISUAL-ONLY section (no clip header format) ───────────
		if visualOnlyRe.MatchString(trimmed) {
			inVisualOnly = true
			flush()
			continue
		}

		// ── horizontal rule ──────────────────────────────────────────────────
		// With clip headers: HR is a visual separator only (no audio tag).
		// Without clip headers: HR produces [long pause] for backward compat.
		if hrRe.MatchString(trimmed) {
			flush()
			if !hasClipHeaders && len(paragraphs) > 0 {
				pendingTag = "[long pause]"
			}
			continue
		}

		// ── [SCENE: ...] standalone line → pause ─────────────────────────────
		if sceneLineRe.MatchString(trimmed) {
			flush()
			if len(paragraphs) > 0 {
				pendingTag = "[pause]"
			}
			continue
		}

		// ── empty line → paragraph break ─────────────────────────────────────
		if trimmed == "" {
			flush()
			continue
		}

		// ── bold key-value metadata (front matter) ────────────────────────────
		if boldMetaRe.MatchString(trimmed) {
			continue
		}

		// ── checklist / summary lines (✓ ✗ prefixed) ─────────────────────────
		if checkmarkLineRe.MatchString(trimmed) {
			continue
		}

		// ── single-line parenthetical delivery note ───────────────────────────
		if parenOnlyRe.MatchString(trimmed) {
			continue
		}
		// multi-line paren start (no closing ) on this line)
		if parenStartRe.MatchString(trimmed) && !strings.Contains(trimmed, ")") {
			inParenBlock = true
			continue
		}

		// ── strip inline markdown and stray [SCENE: ...] ─────────────────────
		clean := stripInlineMarkdown(trimmed)
		clean = sceneInlineRe.ReplaceAllString(clean, "")
		clean = strings.TrimSpace(clean)
		if clean == "" {
			continue
		}

		buf = append(buf, clean)
	}
	flush()

	if len(paragraphs) == 0 {
		if hasClipHeaders {
			return "", buildManifest(allClips, segments, clipGridSecs)
		}
		return "", nil
	}

	var sb strings.Builder
	for i, p := range paragraphs {
		if i > 0 {
			sb.WriteString("\n\n")
		}
		if i == 0 && firstParagraph {
			// Prepend documentary style tag to first paragraph.
			if p.tag != "" {
				sb.WriteString(p.tag)
				sb.WriteString("\n\n")
			}
			sb.WriteString("[documentary style] ")
			sb.WriteString(p.text)
			firstParagraph = false
			continue
		}
		if p.tag != "" {
			sb.WriteString(p.tag)
			sb.WriteString("\n\n")
		}
		sb.WriteString(p.text)
	}

	if !hasClipHeaders {
		return sb.String(), nil
	}
	return sb.String(), buildManifest(allClips, segments, clipGridSecs)
}

// stripInlineMarkdown removes ***bold italic***, **bold**, and *italic* markers.
func stripInlineMarkdown(s string) string {
	s = boldItalicRe.ReplaceAllString(s, "$1")
	s = boldRe.ReplaceAllString(s, "$1")
	s = italicRe.ReplaceAllString(s, "$1")
	return s
}

// cleanupText strips double-quotes (speech marks), collapses double-periods from
// abbreviation expansion, and normalises whitespace.
func cleanupText(s string) string {
	s = dquoteRe.ReplaceAllString(s, "")
	// Collapse ".." → "." but preserve "..." ellipsis.
	s = strings.ReplaceAll(s, "...", "\x00ELLIPSIS\x00")
	s = strings.ReplaceAll(s, "..", ".")
	s = strings.ReplaceAll(s, "\x00ELLIPSIS\x00", "...")
	// Clean up spaces created by quote removal.
	s = multiSpaceRe.ReplaceAllString(s, " ")
	// Trim spaces at start of each paragraph.
	parts := strings.Split(s, "\n\n")
	for i, p := range parts {
		parts[i] = strings.TrimSpace(p)
	}
	return strings.TrimSpace(strings.Join(parts, "\n\n"))
}
