// Package frontmatter provides YAML frontmatter parsing for MDX files.
package frontmatter

import "strings"

// Split separates YAML frontmatter (between --- delimiters) from the body.
// Returns empty frontmatter if no valid delimiters are found.
func Split(content string) (frontmatter, body string) {
	if !strings.HasPrefix(content, "---\n") {
		return "", content
	}
	rest := content[4:]
	idx := strings.Index(rest, "\n---")
	if idx == -1 {
		return "", content
	}
	frontmatter = rest[:idx]
	body = rest[idx+4:]
	body = strings.TrimPrefix(body, "\n")
	return frontmatter, body
}

// Field extracts a simple key: value field from YAML-like frontmatter.
// Strips surrounding double quotes from the value.
func Field(fm, key string) string {
	prefix := key + ":"
	for _, line := range strings.Split(fm, "\n") {
		line = strings.TrimSpace(line)
		if strings.HasPrefix(line, prefix) {
			value := strings.TrimSpace(line[len(prefix):])
			if len(value) >= 2 && value[0] == '"' && value[len(value)-1] == '"' {
				value = value[1 : len(value)-1]
			}
			return value
		}
	}
	return ""
}
