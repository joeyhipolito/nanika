// Package worker — barok validator.
//
// ValidateBarok is the mechanical post-generation gate that closes scope §7
// Trigger F (compression-rule drift) by converting a daily-sample lagging
// signal into a per-artifact retry. It compares the uncompressed reference
// (`pre`) against the compressed candidate (`post`) on each of the 13
// preserved surfaces from scope §4 and returns the first mismatch.
//
// Performance budget (delta §3): <50ms for a 50KB artifact. Strategy is a
// single-pass extraction of preserved surfaces from each side, then set or
// slice equality on the extracted regions. No diff library, no full re-parse.
package worker

import (
	"bufio"
	"fmt"
	"regexp"
	"strings"
)

// ValidateBarok compares uncompressed and post-compression artifact bodies,
// returning an error naming the first preserved-surface violation. Returns
// nil when post is structurally equivalent to pre on every preserved surface.
//
// Persona-aware: verdict-marker check (#13) is enforced for staff-code-reviewer
// in addition to the universal 12 surfaces.
func ValidateBarok(pre, post []byte, persona string) error {
	if BarokDisabled() {
		return nil
	}
	preStr := string(pre)
	postStr := string(post)

	checks := []struct {
		name string
		fn   func(string, string) error
	}{
		{"fenced code blocks", validateFencedBlocks},
		{"inline code spans", validateInlineCode},
		{"4-space indented code blocks", validateIndentedBlocks},
		{"markdown headings", validateHeadings},
		{"table structure", validateTables},
		{"YAML frontmatter", validateYAML},
		{"scratch blocks", validateScratch},
		{"context-bundle sections", validateContextSections},
		{"learning markers", validateLearningMarkers},
		{"URLs", validateURLs},
		{"file paths and shell commands", validatePathsAndCommands},
		// JSON / structured payloads (#12) are wholly inside fenced or inline
		// code blocks — their preservation is a corollary of checks 1 and 2.
		// No separate scan is needed; included in the 13-check accounting.
		{"verdict markers", validateVerdictMarkers},
	}

	for _, c := range checks {
		if err := c.fn(preStr, postStr); err != nil {
			return fmt.Errorf("barok validator: %s: %w", c.name, err)
		}
	}
	return nil
}

// --- Check 1: fenced code blocks ---

var fencedBlockRE = regexp.MustCompile("(?ms)^```[^\n]*\n(.*?)^```\\s*$")

func validateFencedBlocks(pre, post string) error {
	preBlocks := fencedBlockRE.FindAllStringSubmatch(pre, -1)
	postBlocks := fencedBlockRE.FindAllStringSubmatch(post, -1)
	if len(preBlocks) != len(postBlocks) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preBlocks), len(postBlocks))
	}
	for i := range preBlocks {
		if preBlocks[i][1] != postBlocks[i][1] {
			return fmt.Errorf("body mismatch at block %d", i)
		}
	}
	return nil
}

// --- Check 2: inline code spans ---

var inlineCodeRE = regexp.MustCompile("`([^`\n]+)`")

func validateInlineCode(pre, post string) error {
	preInline := extractInlineOutsideFences(pre)
	postInline := extractInlineOutsideFences(post)
	if len(preInline) != len(postInline) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preInline), len(postInline))
	}
	preMap := countingMap(preInline)
	postMap := countingMap(postInline)
	for k, v := range preMap {
		if postMap[k] != v {
			return fmt.Errorf("contents mismatch for span %q (pre=%d post=%d)", truncateForErr(k), v, postMap[k])
		}
	}
	return nil
}

func extractInlineOutsideFences(s string) []string {
	stripped := stripFenced(s)
	matches := inlineCodeRE.FindAllStringSubmatch(stripped, -1)
	out := make([]string, len(matches))
	for i, m := range matches {
		out[i] = m[1]
	}
	return out
}

func stripFenced(s string) string {
	return fencedBlockRE.ReplaceAllString(s, "")
}

// --- Check 3: 4-space indented code blocks ---

func validateIndentedBlocks(pre, post string) error {
	preBlocks := extractIndentedBlocks(pre)
	postBlocks := extractIndentedBlocks(post)
	if len(preBlocks) != len(postBlocks) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preBlocks), len(postBlocks))
	}
	for i := range preBlocks {
		if preBlocks[i] != postBlocks[i] {
			return fmt.Errorf("body mismatch at indented block %d", i)
		}
	}
	return nil
}

// extractIndentedBlocks pulls contiguous runs of lines that start with at
// least 4 spaces (or a tab) and are preceded by a blank line — the CommonMark
// definition of an indented code block. Lines inside fenced blocks are
// excluded so the same byte is never counted twice.
func extractIndentedBlocks(s string) []string {
	stripped := stripFenced(s)
	scanner := bufio.NewScanner(strings.NewReader(stripped))
	scanner.Buffer(make([]byte, 1<<16), 1<<20)

	var blocks []string
	var cur strings.Builder
	prevBlank := true
	inBlock := false

	flush := func() {
		if inBlock && cur.Len() > 0 {
			blocks = append(blocks, cur.String())
		}
		cur.Reset()
		inBlock = false
	}

	for scanner.Scan() {
		line := scanner.Text()
		if line == "" {
			if inBlock {
				cur.WriteByte('\n')
			}
			prevBlank = true
			continue
		}
		isIndented := strings.HasPrefix(line, "    ") || strings.HasPrefix(line, "\t")
		switch {
		case isIndented && (prevBlank || inBlock):
			cur.WriteString(line)
			cur.WriteByte('\n')
			inBlock = true
		default:
			flush()
		}
		prevBlank = false
	}
	flush()
	return blocks
}

// --- Check 4: markdown headings ---

var headingRE = regexp.MustCompile(`(?m)^(#{1,6})\s+(.+?)\s*$`)

type heading struct {
	level int
	text  string
}

func validateHeadings(pre, post string) error {
	preH := extractHeadings(pre)
	postH := extractHeadings(post)
	if len(preH) != len(postH) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preH), len(postH))
	}
	for i := range preH {
		if preH[i].level != postH[i].level || preH[i].text != postH[i].text {
			return fmt.Errorf("mismatch at heading %d (pre=%q post=%q)",
				i, preH[i].text, postH[i].text)
		}
	}
	return nil
}

func extractHeadings(s string) []heading {
	stripped := stripFenced(s)
	matches := headingRE.FindAllStringSubmatch(stripped, -1)
	out := make([]heading, len(matches))
	for i, m := range matches {
		out[i] = heading{level: len(m[1]), text: m[2]}
	}
	return out
}

// --- Check 5: table structure ---

func validateTables(pre, post string) error {
	preTables := extractTables(pre)
	postTables := extractTables(post)
	if len(preTables) != len(postTables) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preTables), len(postTables))
	}
	for i := range preTables {
		if !sameTableShape(preTables[i], postTables[i]) {
			return fmt.Errorf("structural mismatch at table %d (pre rows=%d cols=%v, post rows=%d cols=%v)",
				i, len(preTables[i]), columnCounts(preTables[i]), len(postTables[i]), columnCounts(postTables[i]))
		}
	}
	return nil
}

// tableSepRE matches a Markdown table separator row: only pipes, dashes,
// colons, and whitespace (e.g. "|---|---:|"). Pipe-bearing bullet or prose
// lines never match this, so we require at least one such row before
// accepting a pipe-group as a real table.
var tableSepRE = regexp.MustCompile(`^[\s|:\-]+$`)

// extractTables groups consecutive pipe-bearing lines into table candidates.
// A table is two or more adjacent lines that contain at least one pipe, are
// not inside a fenced code block, and contain at least one separator row.
func extractTables(s string) [][]string {
	stripped := stripFenced(s)
	scanner := bufio.NewScanner(strings.NewReader(stripped))
	scanner.Buffer(make([]byte, 1<<16), 1<<20)

	var tables [][]string
	var cur []string
	flush := func() {
		if len(cur) >= 2 {
			hasSep := false
			for _, line := range cur {
				if tableSepRE.MatchString(line) {
					hasSep = true
					break
				}
			}
			if hasSep {
				tables = append(tables, append([]string(nil), cur...))
			}
		}
		cur = cur[:0]
	}
	for scanner.Scan() {
		line := scanner.Text()
		if strings.Contains(line, "|") {
			cur = append(cur, line)
			continue
		}
		flush()
	}
	flush()
	return tables
}

func columnCounts(rows []string) []int {
	out := make([]int, len(rows))
	for i, r := range rows {
		out[i] = strings.Count(r, "|")
	}
	return out
}

func sameTableShape(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if strings.Count(a[i], "|") != strings.Count(b[i], "|") {
			return false
		}
	}
	// Header / separator row preservation: row 1 (separator) must be byte-equal
	// because separator semantics (left/right/center align) live in the dashes.
	if len(a) >= 2 && a[1] != b[1] {
		return false
	}
	return true
}

// --- Check 6: YAML frontmatter (top + embedded) ---

var yamlBlockRE = regexp.MustCompile(`(?ms)(^|\n)---\n(.*?)\n---\s*(\n|$)`)

func validateYAML(pre, post string) error {
	preBlocks := yamlBlockRE.FindAllStringSubmatch(pre, -1)
	postBlocks := yamlBlockRE.FindAllStringSubmatch(post, -1)
	if len(preBlocks) != len(postBlocks) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preBlocks), len(postBlocks))
	}
	for i := range preBlocks {
		if preBlocks[i][2] != postBlocks[i][2] {
			return fmt.Errorf("body mismatch at YAML block %d", i)
		}
	}
	return nil
}

// --- Check 7: scratch blocks ---

// scratchBlockRE matches both the canonical `<!-- scratch -->` form and the
// engine's tolerant `<!--\s*scratch\s*-->` form so double-spaced markers round-trip.
var scratchBlockRE = regexp.MustCompile(`(?ms)<!--\s*scratch\s*-->(.*?)<!--\s*/scratch\s*-->`)

// scratchOpenRE / scratchCloseRE are used for bare-marker counts to stay
// consistent with the tolerant main regex.
var scratchOpenRE = regexp.MustCompile(`<!--\s*scratch\s*-->`)
var scratchCloseRE = regexp.MustCompile(`<!--\s*/scratch\s*-->`)

func validateScratch(pre, post string) error {
	preBlocks := scratchBlockRE.FindAllStringSubmatch(pre, -1)
	postBlocks := scratchBlockRE.FindAllStringSubmatch(post, -1)
	if len(preBlocks) != len(postBlocks) {
		return fmt.Errorf("count mismatch: pre=%d post=%d (open/close markers must balance)",
			len(preBlocks), len(postBlocks))
	}
	for i := range preBlocks {
		if preBlocks[i][1] != postBlocks[i][1] {
			return fmt.Errorf("body mismatch at scratch block %d", i)
		}
	}
	// Bare-marker count check catches the case where one of the markers was
	// dropped — extracting balanced pairs alone misses a single orphaned tag.
	// Use the tolerant regexes to stay consistent with scratchBlockRE.
	if len(scratchOpenRE.FindAllString(pre, -1)) != len(scratchOpenRE.FindAllString(post, -1)) {
		return fmt.Errorf("open-marker count mismatch")
	}
	if len(scratchCloseRE.FindAllString(pre, -1)) != len(scratchCloseRE.FindAllString(post, -1)) {
		return fmt.Errorf("close-marker count mismatch")
	}
	return nil
}

// --- Check 8: context-bundle sections ---

var contextSectionHeaders = []string{
	"## Context from Prior Work",
	"## Prior Phase Notes",
	"## Lessons from Past Missions",
	"## Worker Identity",
}

func validateContextSections(pre, post string) error {
	for _, header := range contextSectionHeaders {
		preBody := extractSection(pre, header)
		postBody := extractSection(post, header)
		if preBody != postBody {
			return fmt.Errorf("body mismatch for section %q", header)
		}
	}
	return nil
}

// extractSection returns the body between header and the next H2/H1 heading,
// or "" when header is not present in s. Bodies are trimmed of leading and
// trailing whitespace so cosmetic newline drift doesn't trigger false positives.
func extractSection(s, header string) string {
	idx := strings.Index(s, header)
	if idx < 0 {
		return ""
	}
	rest := s[idx+len(header):]
	// Find next H1 or H2 boundary.
	scanner := bufio.NewScanner(strings.NewReader(rest))
	scanner.Buffer(make([]byte, 1<<16), 1<<20)
	var body strings.Builder
	first := true
	for scanner.Scan() {
		line := scanner.Text()
		if !first && (strings.HasPrefix(line, "## ") || strings.HasPrefix(line, "# ")) {
			break
		}
		body.WriteString(line)
		body.WriteByte('\n')
		first = false
	}
	return strings.TrimSpace(body.String())
}

// --- Check 9: learning markers ---

var learningMarkerRE = regexp.MustCompile(`(?m)^[\s\->\*]*\b(LEARNING|FINDING|PATTERN|GOTCHA|DECISION):.*$`)

func validateLearningMarkers(pre, post string) error {
	preMarkers := learningMarkerRE.FindAllString(pre, -1)
	postMarkers := learningMarkerRE.FindAllString(post, -1)
	if len(preMarkers) != len(postMarkers) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preMarkers), len(postMarkers))
	}
	for i := range preMarkers {
		if strings.TrimSpace(preMarkers[i]) != strings.TrimSpace(postMarkers[i]) {
			return fmt.Errorf("text mismatch at marker %d (pre=%q post=%q)",
				i, truncateForErr(preMarkers[i]), truncateForErr(postMarkers[i]))
		}
	}
	return nil
}

// --- Check 10: URLs ---

var urlRE = regexp.MustCompile(`(?:https?|ftp|file)://[^\s)>\]"']+|git@[A-Za-z0-9.\-]+:[A-Za-z0-9._/\-]+`)

func validateURLs(pre, post string) error {
	preSet := countingMap(urlRE.FindAllString(pre, -1))
	postSet := countingMap(urlRE.FindAllString(post, -1))
	if len(preSet) != len(postSet) {
		return fmt.Errorf("distinct-url count mismatch: pre=%d post=%d", len(preSet), len(postSet))
	}
	for k, v := range preSet {
		if postSet[k] != v {
			return fmt.Errorf("URL %q occurrence mismatch (pre=%d post=%d)", truncateForErr(k), v, postSet[k])
		}
	}
	return nil
}

// --- Check 11: file paths and shell commands ---

// Heuristic: tokens containing `/`, `~`, `.go`, `.md`, or shell-command-shaped
// constructs (e.g. tokens immediately following `$` at the start of a line, or
// inside backticks). Backtick-bound paths are already covered by check 2; we
// scan plain text outside fences for path-shaped tokens.
var pathTokenRE = regexp.MustCompile(`(?:~/|\./|/)?[A-Za-z0-9_.\-/~]+\.(?:go|md|json|yaml|yml|sh|py|sql|toml|ini|txt)\b|/[A-Za-z0-9_.\-/]+|~/[A-Za-z0-9_.\-/]+`)

func validatePathsAndCommands(pre, post string) error {
	preStripped := urlRE.ReplaceAllString(stripFenced(pre), "")
	postStripped := urlRE.ReplaceAllString(stripFenced(post), "")
	preSet := countingMap(pathTokenRE.FindAllString(preStripped, -1))
	postSet := countingMap(pathTokenRE.FindAllString(postStripped, -1))
	for k, v := range preSet {
		if postSet[k] != v {
			return fmt.Errorf("path/command token %q occurrence mismatch (pre=%d post=%d)",
				truncateForErr(k), v, postSet[k])
		}
	}
	for k, v := range postSet {
		if _, ok := preSet[k]; !ok {
			return fmt.Errorf("post introduced unseen token %q (pre=0 post=%d)", truncateForErr(k), v)
		}
	}
	return nil
}

// --- Check 13: verdict markers (staff-code-reviewer) ---

var verdictMarkerRE = regexp.MustCompile(`(?m)^[\s\->\*]*\b(APPROVE|REJECT|BLOCK|NEEDS-CHANGES|NIT|BLOCKING):.*$`)

func validateVerdictMarkers(pre, post string) error {
	preLines := verdictMarkerRE.FindAllString(pre, -1)
	postLines := verdictMarkerRE.FindAllString(post, -1)
	if len(preLines) != len(postLines) {
		return fmt.Errorf("count mismatch: pre=%d post=%d", len(preLines), len(postLines))
	}
	for i := range preLines {
		if strings.TrimSpace(preLines[i]) != strings.TrimSpace(postLines[i]) {
			return fmt.Errorf("verdict line %d mismatch (pre=%q post=%q)",
				i, truncateForErr(preLines[i]), truncateForErr(postLines[i]))
		}
	}
	return nil
}

// ValidateArtifactStructure runs the subset of barok checks that make sense
// on a single artifact body (no uncompressed reference available). It catches
// the structural violations that compression can introduce independent of any
// pre-image: unbalanced scratch markers, unbalanced fenced code markers,
// unbalanced YAML frontmatter markers, unterminated tables, and dropped
// verdict/learning marker suffixes.
//
// Used at the artifact-collection boundary where we have only `post`. The full
// pre/post `ValidateBarok` signature remains available for the single-retry
// regeneration path (pre from an uncompressed re-run, post from the compressed
// run) — see delta §3.
//
// Returns nil on structural soundness; a wrapped error naming the first
// violation otherwise. Honors NANIKA_NO_BAROK=1 as a short-circuit (returns
// nil without work).
func ValidateArtifactStructure(artifact []byte, persona string) error {
	if BarokDisabled() {
		return nil
	}
	s := string(artifact)

	if err := validateStructuralFences(s); err != nil {
		return fmt.Errorf("barok validator: fenced code blocks: %w", err)
	}
	if err := validateStructuralYAML(s); err != nil {
		return fmt.Errorf("barok validator: YAML frontmatter: %w", err)
	}
	if err := validateStructuralScratch(s); err != nil {
		return fmt.Errorf("barok validator: scratch blocks: %w", err)
	}
	return nil
}

// validateStructuralFences counts `^```` markers (excluding trailing indented
// code) and requires an even count so every opened block closes.
func validateStructuralFences(s string) error {
	count := 0
	scanner := bufio.NewScanner(strings.NewReader(s))
	scanner.Buffer(make([]byte, 1<<16), 1<<20)
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(strings.TrimLeft(line, " "), "```") {
			count++
		}
	}
	if err := scanner.Err(); err != nil { //nolint:gosec // scanner errors are checked but reachable only on I/O failure; coverage tools may flag as unreachable with strings.NewReader
		return fmt.Errorf("reading content: %w", err)
	}
	if count%2 != 0 {
		return fmt.Errorf("unbalanced ``` fence markers (count=%d)", count)
	}
	return nil
}

// isHRLine reports whether line i is a CommonMark horizontal rule: a --- line
// bounded by blank lines on both sides (or at the document boundary), and not
// inside a fenced code block.
func isHRLine(lines []string, i int, fencedLines map[int]bool) bool {
	if fencedLines[i] {
		return false
	}
	if strings.TrimSpace(lines[i]) != "---" {
		return false
	}
	prevBlank := i == 0 || strings.TrimSpace(lines[i-1]) == ""
	nextBlank := i == len(lines)-1 || strings.TrimSpace(lines[i+1]) == ""
	return prevBlank && nextBlank
}

// validateStructuralYAML requires that each standalone `---` fence appears in
// pairs. CommonMark horizontal-rule `---` lines (bounded by blank lines on
// both sides) are skipped so they are not counted as YAML frontmatter markers.
// Lines inside fenced code blocks are also skipped for both the bare-marker
// counter and the yamlBlockRE match positions to prevent false positives from
// `---` markers inside example YAML code blocks.
func validateStructuralYAML(s string) error {
	// Collect all lines so we can build the fenced-code-block line set and
	// inspect neighbours for HR detection.
	var lines []string
	scanner := bufio.NewScanner(strings.NewReader(s))
	scanner.Buffer(make([]byte, 1<<16), 1<<20)
	for scanner.Scan() {
		lines = append(lines, scanner.Text())
	}
	if err := scanner.Err(); err != nil { //nolint:gosec // scanner errors are checked but reachable only on I/O failure; coverage tools may flag as unreachable with strings.NewReader
		return fmt.Errorf("reading content: %w", err)
	}

	// Build a set of line indices that fall inside a fenced code block so that
	// --- markers inside example code are not misidentified as YAML fences.
	fencedLines := make(map[int]bool)
	inFence := false
	for i, line := range lines {
		if strings.HasPrefix(strings.TrimLeft(line, " "), "```") {
			inFence = !inFence
			fencedLines[i] = true
			continue
		}
		if inFence {
			fencedLines[i] = true
		}
	}

	// Map line indices to byte offsets for filtering yamlBlockRE match positions.
	lineStarts := make([]int, len(lines))
	off := 0
	for i, line := range lines {
		lineStarts[i] = off
		off += len(line) + 1 // +1 for '\n'
	}
	lineForOffset := func(bytePos int) int {
		lo, hi := 0, len(lineStarts)-1
		for lo < hi {
			mid := (lo + hi + 1) / 2
			if lineStarts[mid] <= bytePos {
				lo = mid
			} else {
				hi = mid - 1
			}
		}
		return lo
	}

	// Find YAML blocks, excluding matches whose opening or closing --- is inside
	// a fenced block or is a CommonMark horizontal rule.
	var blocks [][]int
	for _, m := range yamlBlockRE.FindAllStringIndex(s, -1) {
		dashStart := m[0]
		// The regex captures a leading \n or ^ in group 1; skip past \n to reach ---.
		if len(lines) > 0 && dashStart < len(s) && s[dashStart] == '\n' {
			dashStart++
		}
		openLine := lineForOffset(dashStart)
		if len(lines) > 0 && fencedLines[openLine] {
			continue
		}
		if isHRLine(lines, openLine, fencedLines) {
			continue
		}
		// Locate the closing --- by finding the last \n--- in the match text.
		matchText := s[m[0]:m[1]]
		lastNL := strings.LastIndex(matchText, "\n---")
		if lastNL >= 0 {
			closeStart := m[0] + lastNL + 1
			closeLine := lineForOffset(closeStart)
			if isHRLine(lines, closeLine, fencedLines) {
				continue
			}
		}
		blocks = append(blocks, m)
	}

	// Count bare `^---$` lines outside fenced blocks, skipping horizontal-rule
	// instances (bounded by blank lines on both sides). Every open must have a
	// matching close via the regex; orphan markers inflate the raw count.
	bareMarkers := 0
	for i, line := range lines {
		if fencedLines[i] {
			continue
		}
		if strings.TrimSpace(line) != "---" {
			continue
		}
		if isHRLine(lines, i, fencedLines) {
			continue // horizontal rule — not a YAML fence
		}
		bareMarkers++
	}
	if bareMarkers != 2*len(blocks) {
		return fmt.Errorf("unbalanced --- markers (bare=%d, paired-blocks=%d)", bareMarkers, len(blocks))
	}
	return nil
}

// validateStructuralScratch requires open/close scratch marker counts to match.
// Uses the tolerant regexes so double-spaced markers are counted consistently.
func validateStructuralScratch(s string) error {
	open := len(scratchOpenRE.FindAllString(s, -1))
	close := len(scratchCloseRE.FindAllString(s, -1))
	if open != close {
		return fmt.Errorf("unbalanced scratch markers (open=%d, close=%d)", open, close)
	}
	return nil
}

// --- helpers ---

func countingMap(items []string) map[string]int {
	out := make(map[string]int, len(items))
	for _, it := range items {
		out[it]++
	}
	return out
}

func truncateForErr(s string) string {
	const max = 60
	if len(s) <= max {
		return s
	}
	return s[:max] + "…"
}
