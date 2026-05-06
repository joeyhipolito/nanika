package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode"
)

// FileEditTool replaces old_string with new_string in a file.
// Match order: (1) exact, (2) whitespace-normalized.
// Write is atomic: temp file + rename.
type FileEditTool struct{}

// NewFileEditTool returns a FileEditTool.
func NewFileEditTool() Tool { return &FileEditTool{} }

func (t *FileEditTool) Name() string        { return "file_edit" }
func (t *FileEditTool) Risk() RiskTier      { return RiskMedium }
func (t *FileEditTool) Description() string {
	return "Replace old_string with new_string in a file. " +
		"Tries exact match first, then whitespace-normalized match. " +
		"Fails if old_string does not appear exactly once."
}

func (t *FileEditTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"path":       String("Path to the file to edit"),
		"old_string": String("Exact (or whitespace-normalized) text to find and replace"),
		"new_string": String("Replacement text"),
	}, []string{"path", "old_string", "new_string"})
}

func (t *FileEditTool) Execute(_ context.Context, args map[string]any) (ToolResult, error) {
	path, err := requireString(args, "path")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	oldStr, err := requireString(args, "old_string")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	newStr, ok := args["new_string"]
	if !ok {
		return ToolResult{IsError: true, Content: "missing required argument: new_string"}, nil
	}
	newString, ok := newStr.(string)
	if !ok {
		return ToolResult{IsError: true, Content: "argument new_string must be a string"}, nil
	}

	data, err := os.ReadFile(path)
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_edit: reading %s: %v", path, err)}, nil
	}
	original := string(data)

	updated, err := applyEdit(original, oldStr, newString)
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_edit: %v", err)}, nil
	}

	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, ".file_edit_*")
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_edit: creating temp: %v", err)}, nil
	}
	tmpName := tmp.Name()
	if _, err := tmp.WriteString(updated); err != nil {
		tmp.Close()
		os.Remove(tmpName)
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_edit: writing: %v", err)}, nil
	}
	if err := tmp.Close(); err != nil {
		os.Remove(tmpName)
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_edit: closing: %v", err)}, nil
	}
	if err := os.Rename(tmpName, path); err != nil {
		os.Remove(tmpName)
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_edit: rename: %v", err)}, nil
	}

	return ToolResult{Content: fmt.Sprintf("edited %s", path)}, nil
}

// applyEdit finds oldStr in content and replaces it with newStr exactly once.
// Tries exact match first, then whitespace-normalized match.
func applyEdit(content, oldStr, newStr string) (string, error) {
	// 1. Exact match
	count := strings.Count(content, oldStr)
	if count == 1 {
		return strings.Replace(content, oldStr, newStr, 1), nil
	}
	if count > 1 {
		return "", fmt.Errorf("old_string appears %d times (must appear exactly once)", count)
	}

	// 2. Whitespace-normalized match: normalize both sides, find offset in original
	normContent := normalizeWS(content)
	normOld := normalizeWS(oldStr)
	normCount := strings.Count(normContent, normOld)

	if normCount == 0 {
		return "", fmt.Errorf("old_string not found in file (tried exact and whitespace-normalized)")
	}
	if normCount > 1 {
		return "", fmt.Errorf("old_string appears %d times after whitespace normalization (must appear exactly once)", normCount)
	}

	// Rebuild by matching lines in the original against the normalized old_string's lines.
	return replaceByLines(content, oldStr, newStr)
}

// normalizeWS collapses all runs of whitespace (including newlines) to a single space.
func normalizeWS(s string) string {
	var b strings.Builder
	inSpace := false
	for _, r := range s {
		if unicode.IsSpace(r) {
			if !inSpace {
				b.WriteByte(' ')
				inSpace = true
			}
		} else {
			b.WriteRune(r)
			inSpace = false
		}
	}
	return strings.TrimSpace(b.String())
}

// replaceByLines finds the block of lines in content that match oldStr (when both
// are whitespace-trimmed line by line) and replaces them with newStr.
func replaceByLines(content, oldStr, newStr string) (string, error) {
	contentLines := strings.Split(content, "\n")
	oldLines := strings.Split(strings.TrimRight(oldStr, "\n"), "\n")

	// Trim trailing empty line from oldLines if present.
	if len(oldLines) > 0 && strings.TrimSpace(oldLines[len(oldLines)-1]) == "" {
		oldLines = oldLines[:len(oldLines)-1]
	}
	if len(oldLines) == 0 {
		return "", fmt.Errorf("old_string is effectively empty")
	}

	matchStart := -1
	for i := 0; i <= len(contentLines)-len(oldLines); i++ {
		if linesMatch(contentLines[i:i+len(oldLines)], oldLines) {
			if matchStart >= 0 {
				return "", fmt.Errorf("old_string matches multiple positions in file")
			}
			matchStart = i
		}
	}
	if matchStart < 0 {
		return "", fmt.Errorf("old_string not found in file (tried exact and whitespace-normalized)")
	}

	var out strings.Builder
	for _, line := range contentLines[:matchStart] {
		out.WriteString(line)
		out.WriteByte('\n')
	}
	out.WriteString(newStr)
	if !strings.HasSuffix(newStr, "\n") && matchStart+len(oldLines) < len(contentLines) {
		out.WriteByte('\n')
	}
	for _, line := range contentLines[matchStart+len(oldLines):] {
		out.WriteString(line)
		out.WriteByte('\n')
	}
	result := out.String()
	// Remove trailing newline added for the last line if the original didn't end with one.
	if !strings.HasSuffix(content, "\n") && strings.HasSuffix(result, "\n") {
		result = result[:len(result)-1]
	}
	return result, nil
}

// linesMatch compares two slices of lines with trimmed whitespace.
func linesMatch(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if strings.TrimSpace(a[i]) != strings.TrimSpace(b[i]) {
			return false
		}
	}
	return true
}
