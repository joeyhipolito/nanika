// Package vault provides utilities for reading and manipulating Obsidian vault notes.
// It handles frontmatter YAML parsing, wikilink extraction, and heading extraction.
package vault

import (
	"bufio"
	"fmt"
	"regexp"
	"strings"
)

// Note represents a parsed Obsidian markdown note.
type Note struct {
	Frontmatter map[string]any `json:"frontmatter,omitempty"`
	Body        string         `json:"body"`
	Headings    []Heading      `json:"headings,omitempty"`
	Wikilinks   []string       `json:"wikilinks,omitempty"`
}

// Heading represents a markdown heading with its level and text.
type Heading struct {
	Level int    `json:"level"`
	Text  string `json:"text"`
}

var (
	wikilinkRe = regexp.MustCompile(`\[\[([^\]|]+)(?:\|[^\]]+)?\]\]`)
	headingRe  = regexp.MustCompile(`^(#{1,6})\s+(.+)$`)
)

// ParseNote parses a markdown string into frontmatter, body, headings, and wikilinks.
func ParseNote(content string) *Note {
	note := &Note{
		Frontmatter: make(map[string]any),
	}

	body := content
	if strings.HasPrefix(content, "---\n") || strings.HasPrefix(content, "---\r\n") {
		fm, rest, ok := splitFrontmatter(content)
		if ok {
			note.Frontmatter = parseFrontmatterYAML(fm)
			body = rest
		}
	}

	note.Body = body
	note.Headings = extractHeadings(body)
	note.Wikilinks = extractWikilinks(body)

	return note
}

// splitFrontmatter splits content at the YAML frontmatter delimiters (---).
// Returns the frontmatter content (without delimiters), the remaining body, and whether
// a valid frontmatter block was found.
func splitFrontmatter(content string) (string, string, bool) {
	// Content must start with ---
	rest := content[3:]
	// Skip the newline after opening ---
	if len(rest) > 0 && rest[0] == '\n' {
		rest = rest[1:]
	} else if len(rest) > 1 && rest[0] == '\r' && rest[1] == '\n' {
		rest = rest[2:]
	} else {
		return "", content, false
	}

	// Find closing ---
	idx := strings.Index(rest, "\n---\n")
	if idx == -1 {
		// Try with \r\n
		idx = strings.Index(rest, "\r\n---\r\n")
		if idx == -1 {
			// Check if --- is at the very end
			if strings.HasSuffix(rest, "\n---") {
				idx = len(rest) - 4
			} else if strings.HasSuffix(rest, "\r\n---") {
				idx = len(rest) - 5
			} else {
				return "", content, false
			}
		}
	}

	fm := rest[:idx]
	body := rest[idx:]
	// Skip past the closing --- line
	if nlIdx := strings.Index(body[1:], "\n"); nlIdx != -1 {
		body = body[nlIdx+2:] // skip \n---\n
	} else {
		body = ""
	}

	return fm, body, true
}

// parseFrontmatterYAML parses simple YAML key-value pairs from frontmatter.
// Supports string values, lists (- item), and inline lists [a, b].
// This is a lightweight parser for common Obsidian frontmatter patterns
// without requiring a full YAML library.
func parseFrontmatterYAML(fm string) map[string]any {
	result := make(map[string]any)
	scanner := bufio.NewScanner(strings.NewReader(fm))

	var currentKey string
	var listItems []string
	inList := false

	for scanner.Scan() {
		line := scanner.Text()

		// Check for list continuation
		trimmed := strings.TrimSpace(line)
		if inList && strings.HasPrefix(trimmed, "- ") {
			listItems = append(listItems, strings.TrimPrefix(trimmed, "- "))
			continue
		}

		// Flush any pending list
		if inList {
			result[currentKey] = listItems
			inList = false
			currentKey = ""
			listItems = nil
		}

		// Skip empty lines and comments
		if trimmed == "" || strings.HasPrefix(trimmed, "#") {
			continue
		}

		// Parse key: value
		colonIdx := strings.Index(line, ":")
		if colonIdx == -1 {
			continue
		}

		// Only parse top-level keys (no leading whitespace)
		if line[0] == ' ' || line[0] == '\t' {
			continue
		}

		key := strings.TrimSpace(line[:colonIdx])
		value := strings.TrimSpace(line[colonIdx+1:])

		if value == "" {
			// Could be start of a list
			currentKey = key
			inList = true
			listItems = nil
			continue
		}

		// Remove surrounding quotes
		wasQuoted := false
		if len(value) >= 2 {
			if (value[0] == '"' && value[len(value)-1] == '"') ||
				(value[0] == '\'' && value[len(value)-1] == '\'') {
				value = value[1 : len(value)-1]
				wasQuoted = true
			}
		}

		// Handle inline lists: [item1, item2]
		// Skip if value was explicitly quoted — quoted values are always strings.
		if !wasQuoted && strings.HasPrefix(value, "[") && strings.HasSuffix(value, "]") {
			inner := value[1 : len(value)-1]
			if inner == "" {
				result[key] = []string{}
			} else {
				parts := strings.Split(inner, ",")
				items := make([]string, 0, len(parts))
				for _, p := range parts {
					p = strings.TrimSpace(p)
					// Remove quotes from items
					if len(p) >= 2 {
						if (p[0] == '"' && p[len(p)-1] == '"') ||
							(p[0] == '\'' && p[len(p)-1] == '\'') {
							p = p[1 : len(p)-1]
						}
					}
					items = append(items, p)
				}
				result[key] = items
			}
			continue
		}

		result[key] = value
	}

	// Flush any pending list at end of frontmatter
	if inList && currentKey != "" {
		result[currentKey] = listItems
	}

	return result
}

// extractHeadings finds all markdown headings in the body.
func extractHeadings(body string) []Heading {
	var headings []Heading
	scanner := bufio.NewScanner(strings.NewReader(body))
	for scanner.Scan() {
		matches := headingRe.FindStringSubmatch(scanner.Text())
		if matches != nil {
			headings = append(headings, Heading{
				Level: len(matches[1]),
				Text:  strings.TrimSpace(matches[2]),
			})
		}
	}
	return headings
}

// extractWikilinks finds all [[wikilink]] targets in the body.
// For aliased links like [[target|alias]], only the target is returned.
// Duplicates are removed.
func extractWikilinks(body string) []string {
	matches := wikilinkRe.FindAllStringSubmatch(body, -1)
	if matches == nil {
		return nil
	}

	seen := make(map[string]bool)
	var links []string
	for _, m := range matches {
		target := strings.TrimSpace(m[1])
		if !seen[target] {
			seen[target] = true
			links = append(links, target)
		}
	}
	return links
}

// FormatFrontmatter converts a map of key-value pairs into YAML frontmatter block.
func FormatFrontmatter(fm map[string]any) string {
	if len(fm) == 0 {
		return ""
	}

	var b strings.Builder
	b.WriteString("---\n")
	for key, value := range fm {
		switch v := value.(type) {
		case []string:
			if len(v) == 0 {
				fmt.Fprintf(&b, "%s: []\n", key)
			} else {
				fmt.Fprintf(&b, "%s:\n", key)
				for _, item := range v {
					fmt.Fprintf(&b, "  - %s\n", item)
				}
			}
		case string:
			if strings.ContainsAny(v, ":{}[]#&*!|>'\"%@`") {
				fmt.Fprintf(&b, "%s: \"%s\"\n", key, strings.ReplaceAll(v, "\"", "\\\""))
			} else {
				fmt.Fprintf(&b, "%s: %s\n", key, v)
			}
		default:
			fmt.Fprintf(&b, "%s: %v\n", key, v)
		}
	}
	b.WriteString("---\n")
	return b.String()
}
