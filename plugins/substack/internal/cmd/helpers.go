package cmd

// Helper functions used by tiptap.go and other converters.

func splitLines(s string) []string {
	var lines []string
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == '\n' {
			lines = append(lines, s[start:i])
			start = i + 1
		}
	}
	if start < len(s) {
		lines = append(lines, s[start:])
	}
	return lines
}

func joinLines(lines []string) string {
	if len(lines) == 0 {
		return ""
	}
	result := lines[0]
	for i := 1; i < len(lines); i++ {
		result += "\n" + lines[i]
	}
	return result
}

func trimSpace(s string) string {
	start := 0
	for start < len(s) && (s[start] == ' ' || s[start] == '\t' || s[start] == '\r') {
		start++
	}
	end := len(s)
	for end > start && (s[end-1] == ' ' || s[end-1] == '\t' || s[end-1] == '\r') {
		end--
	}
	return s[start:end]
}

func splitBy(s string, sep byte) []string {
	var parts []string
	start := 0
	for i := 0; i < len(s); i++ {
		if s[i] == sep {
			parts = append(parts, s[start:i])
			start = i + 1
		}
	}
	parts = append(parts, s[start:])
	return parts
}

func splitTableRow(line string) []string {
	line = trimSpace(line)
	if len(line) > 0 && line[0] == '|' {
		line = line[1:]
	}
	if len(line) > 0 && line[len(line)-1] == '|' {
		line = line[:len(line)-1]
	}
	return splitBy(line, '|')
}

func isOrderedListItem(line string) bool {
	for i := 0; i < len(line); i++ {
		if line[i] >= '0' && line[i] <= '9' {
			continue
		}
		if line[i] == '.' && i > 0 && i+1 < len(line) && line[i+1] == ' ' {
			return true
		}
		return false
	}
	return false
}

func extractOrderedListText(line string) string {
	for i := 0; i < len(line); i++ {
		if line[i] == '.' && i+2 <= len(line) {
			return line[i+2:]
		}
	}
	return line
}
