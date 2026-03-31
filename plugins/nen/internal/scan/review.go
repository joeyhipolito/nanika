package scan

import (
	"regexp"
	"strconv"
	"strings"
)

var (
	fileRefBold     = regexp.MustCompile(`\*\*\[([^:]+):(\d+)(?:-(\d+))?\]\*\*`)
	fileRefBacktick = regexp.MustCompile("`([^:]+):(\\d+)(?:-(\\d+))?`")
)

// ReviewBlocker represents a single blocker extracted from review markdown.
type ReviewBlocker struct {
	Title       string // Primary blocker title from the list item
	Description string // Full description including any block content
	File        string // Source file path if specified
	LineStart   int    // Start line number if file reference exists
	LineEnd     int    // End line number if file reference exists
}

// ParseReviewBlockers extracts blockers from a markdown document.
// It looks for a "## Blockers" section and parses bullet (- ) or numbered (1. 2.)
// list items, extracting file references in formats like **[file:line]** or `file:line`.
// Fenced code blocks are not split across blocker items.
// Returns an empty slice if no Blockers section is found.
func ParseReviewBlockers(markdown string) []ReviewBlocker {
	lines := strings.Split(markdown, "\n")

	// Find the Blockers section header (matches ##, ###, etc. via substring)
	blockersIdx := -1
	for i, line := range lines {
		lower := strings.ToLower(line)
		if strings.Contains(lower, "# blockers") {
			blockersIdx = i
			break
		}
	}

	if blockersIdx == -1 {
		return nil
	}

	// Extract blockers starting after the header
	var blockers []ReviewBlocker
	i := blockersIdx + 1
	inCodeBlock := false
	codeBlockDelim := ""

	for i < len(lines) {
		line := lines[i]

		// Track fenced code blocks to avoid splitting inside them
		if strings.HasPrefix(strings.TrimSpace(line), "```") {
			if !inCodeBlock {
				inCodeBlock = true
				codeBlockDelim = strings.TrimSpace(line)
			} else if strings.HasPrefix(strings.TrimSpace(line), codeBlockDelim[:3]) {
				inCodeBlock = false
				codeBlockDelim = ""
			}
			i++
			continue
		}

		// Stop at next heading (indicates end of Blockers section)
		if !inCodeBlock && strings.HasPrefix(line, "#") {
			break
		}

		// Parse bullet (- ) or numbered (1. ) list items
		if !inCodeBlock && (isBulletItem(line) || isNumberedItem(line)) {
			title, file, lineStart, lineEnd := parseBlockerLine(line)
			if title != "" {
				blockers = append(blockers, ReviewBlocker{
					Title:       title,
					Description: title,
					File:        file,
					LineStart:   lineStart,
					LineEnd:     lineEnd,
				})
			}
			i++
			continue
		}

		i++
	}

	return blockers
}

// isBulletItem returns true if the line is a bullet list item (- text).
func isBulletItem(line string) bool {
	trimmed := strings.TrimLeft(line, " \t")
	return strings.HasPrefix(trimmed, "- ")
}

// isNumberedItem returns true if the line is a numbered list item (1. 2. etc.).
func isNumberedItem(line string) bool {
	trimmed := strings.TrimLeft(line, " \t")
	// Check for pattern like "1. " or "123. "
	for i := 0; i < len(trimmed); i++ {
		if !isDigit(trimmed[i]) {
			return i > 0 && strings.HasPrefix(trimmed[i:], ". ")
		}
	}
	return false
}

// isDigit returns true if the byte is a digit character.
func isDigit(b byte) bool {
	return b >= '0' && b <= '9'
}

// parseBlockerLine extracts the title and any file references from a blocker line.
func parseBlockerLine(line string) (title, file string, lineStart, lineEnd int) {
	trimmed := strings.TrimLeft(line, " \t")

	// Remove bullet or numbered list prefix
	if strings.HasPrefix(trimmed, "- ") {
		trimmed = strings.TrimPrefix(trimmed, "- ")
	} else {
		// Remove numbered prefix (e.g., "1. ")
		parts := strings.SplitN(trimmed, ". ", 2)
		if len(parts) == 2 {
			trimmed = parts[1]
		}
	}

	title = strings.TrimSpace(trimmed)

	// Extract file references in **[file:line]** or `file:line` format
	for _, re := range []*regexp.Regexp{fileRefBold, fileRefBacktick} {
		if matches := re.FindStringSubmatch(title); matches != nil {
			file = matches[1]
			lineStart, _ = strconv.Atoi(matches[2])
			if matches[3] != "" {
				lineEnd, _ = strconv.Atoi(matches[3])
			} else {
				lineEnd = lineStart
			}
			return
		}
	}

	return
}
