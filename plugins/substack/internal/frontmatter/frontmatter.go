// Package frontmatter provides YAML frontmatter parsing for MDX files.
package frontmatter

import "strings"

// Split separates YAML frontmatter (between --- delimiters) from the body.
func Split(content string) (fm, body string) {
	if !strings.HasPrefix(content, "---\n") {
		return "", content
	}
	rest := content[4:]
	idx := strings.Index(rest, "\n---")
	if idx == -1 {
		return "", content
	}
	fm = rest[:idx]
	body = rest[idx+4:]
	body = strings.TrimPrefix(body, "\n")
	return fm, body
}

// Field extracts a simple key: value field from YAML-like frontmatter.
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

// List extracts a YAML list field from frontmatter.
func List(fm, key string) []string {
	lines := strings.Split(fm, "\n")
	prefix := key + ":"

	for i, line := range lines {
		trimmed := strings.TrimSpace(line)
		if !strings.HasPrefix(trimmed, prefix) {
			continue
		}
		val := strings.TrimSpace(trimmed[len(prefix):])

		if len(val) >= 2 && val[0] == '[' && val[len(val)-1] == ']' {
			inner := val[1 : len(val)-1]
			parts := strings.Split(inner, ",")
			var result []string
			for _, p := range parts {
				p = strings.TrimSpace(p)
				p = strings.Trim(p, "\"'")
				if p != "" {
					result = append(result, p)
				}
			}
			return result
		}

		if val != "" {
			val = strings.Trim(val, "\"'")
			return []string{val}
		}

		var result []string
		for j := i + 1; j < len(lines); j++ {
			item := strings.TrimSpace(lines[j])
			if len(item) >= 2 && item[0] == '-' && item[1] == ' ' {
				v := strings.TrimSpace(item[2:])
				v = strings.Trim(v, "\"'")
				if v != "" {
					result = append(result, v)
				}
			} else {
				break
			}
		}
		return result
	}
	return nil
}
