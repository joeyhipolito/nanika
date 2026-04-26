package vault

import (
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// NoteInfo contains metadata about a note file.
type NoteInfo struct {
	Path    string `json:"path"`    // Relative path within vault
	Name    string `json:"name"`    // Filename without extension
	ModTime int64  `json:"mod_time"` // Unix timestamp of last modification
	Size    int64  `json:"size"`    // File size in bytes
}

// ReadNote reads and parses a note from the vault.
// notePath is relative to vaultPath (e.g., "daily/2026-02-07.md").
func ReadNote(vaultPath, notePath string) (*Note, error) {
	fullPath := resolvePath(vaultPath, notePath)

	data, err := os.ReadFile(fullPath)
	if err != nil {
		return nil, fmt.Errorf("cannot read note: %w", err)
	}

	note := ParseNote(string(data))
	return note, nil
}

// WriteNote creates a new note file. Returns an error if the file already exists.
func WriteNote(vaultPath, notePath, content string) error {
	fullPath := resolvePath(vaultPath, notePath)

	// Check if file already exists
	if _, err := os.Stat(fullPath); err == nil {
		return fmt.Errorf("note already exists: %s", notePath)
	}

	// Create parent directories
	dir := filepath.Dir(fullPath)
	if err := os.MkdirAll(dir, 0755); err != nil {
		return fmt.Errorf("cannot create directory: %w", err)
	}

	if err := os.WriteFile(fullPath, []byte(content), 0644); err != nil {
		return fmt.Errorf("cannot write note: %w", err)
	}

	return nil
}

// AppendToNote appends text to an existing note.
// When section is non-empty, text is inserted at the end of that section
// (before the next heading of equal or shallower depth, or end of file).
// When section is empty, text is appended to the end of the file.
func AppendToNote(vaultPath, notePath, text, section string) error {
	fullPath := resolvePath(vaultPath, notePath)

	if _, err := os.Stat(fullPath); err != nil {
		return fmt.Errorf("note not found: %s", notePath)
	}

	if section == "" {
		return appendToEOF(fullPath, text)
	}
	return appendToSection(fullPath, notePath, text, section)
}

// appendToEOF appends text to the end of the file, ensuring a leading newline
// if the file does not already end with one.
func appendToEOF(fullPath, text string) error {
	f, err := os.OpenFile(fullPath, os.O_APPEND|os.O_WRONLY, 0644)
	if err != nil {
		return fmt.Errorf("cannot open note: %w", err)
	}
	defer f.Close()

	// Ensure text starts on a new line
	info, _ := f.Stat()
	if info.Size() > 0 {
		existing, _ := os.ReadFile(fullPath)
		if len(existing) > 0 && existing[len(existing)-1] != '\n' {
			text = "\n" + text
		}
	}

	if !strings.HasSuffix(text, "\n") {
		text += "\n"
	}

	if _, err := f.WriteString(text); err != nil {
		return fmt.Errorf("cannot append to note: %w", err)
	}

	return nil
}

// appendToSection inserts text at the end of the named section.
// The section string must match a heading line exactly (e.g. "## Capture").
// Text is inserted before the next heading of equal or shallower depth,
// or at end of file if no such heading exists.
func appendToSection(fullPath, notePath, text, section string) error {
	existing, err := os.ReadFile(fullPath)
	if err != nil {
		return fmt.Errorf("cannot read note: %w", err)
	}
	content := string(existing)

	// Build a list of lines with their byte offsets.
	type lineSpan struct {
		start int
		text  string
	}
	var lines []lineSpan
	i := 0
	for i < len(content) {
		start := i
		nl := strings.IndexByte(content[i:], '\n')
		if nl == -1 {
			lines = append(lines, lineSpan{start, content[i:]})
			break
		}
		lines = append(lines, lineSpan{start, content[i : i+nl]})
		i += nl + 1
	}

	// Find the section heading.
	normalized := strings.TrimRight(section, " \t")
	sectionIdx := -1
	for idx, l := range lines {
		if strings.TrimRight(l.text, " \t") == normalized {
			sectionIdx = idx
			break
		}
	}
	if sectionIdx == -1 {
		return fmt.Errorf("section %q not found in %s", section, notePath)
	}

	// Determine the heading depth.
	sectionLevel := 0
	for _, c := range section {
		if c != '#' {
			break
		}
		sectionLevel++
	}

	// Find the byte offset at which to insert: the start of the next heading
	// at equal or shallower depth (fewer #'s), or end of file.
	insertOffset := len(content)
	for idx := sectionIdx + 1; idx < len(lines); idx++ {
		line := strings.TrimRight(lines[idx].text, " \t")
		if !strings.HasPrefix(line, "#") {
			continue
		}
		level := 0
		for _, c := range line {
			if c != '#' {
				break
			}
			level++
		}
		// A valid heading requires a space after the # markers.
		if level > 0 && level < len(line) && line[level] == ' ' && level <= sectionLevel {
			insertOffset = lines[idx].start
			break
		}
	}

	if !strings.HasSuffix(text, "\n") {
		text += "\n"
	}
	newContent := content[:insertOffset] + text + content[insertOffset:]

	if err := os.WriteFile(fullPath, []byte(newContent), 0644); err != nil {
		return fmt.Errorf("cannot write note: %w", err)
	}

	return nil
}

// ListNotes lists all .md files in a vault directory.
// dir is relative to vaultPath; empty string lists the entire vault.
func ListNotes(vaultPath, dir string) ([]NoteInfo, error) {
	searchPath := vaultPath
	if dir != "" {
		searchPath = filepath.Join(vaultPath, dir)
	}

	// Verify directory exists
	info, err := os.Stat(searchPath)
	if err != nil {
		return nil, fmt.Errorf("directory not found: %s", dir)
	}
	if !info.IsDir() {
		return nil, fmt.Errorf("not a directory: %s", dir)
	}

	var notes []NoteInfo
	err = filepath.WalkDir(searchPath, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil // skip inaccessible entries
		}

		// Skip hidden directories (like .obsidian, .git)
		if d.IsDir() && strings.HasPrefix(d.Name(), ".") {
			return filepath.SkipDir
		}

		// Only include .md files
		if d.IsDir() || !strings.HasSuffix(d.Name(), ".md") {
			return nil
		}

		relPath, _ := filepath.Rel(vaultPath, path)

		info, err := d.Info()
		if err != nil {
			return nil // skip files we can't stat
		}

		name := strings.TrimSuffix(d.Name(), ".md")
		notes = append(notes, NoteInfo{
			Path:    relPath,
			Name:    name,
			ModTime: info.ModTime().Unix(),
			Size:    info.Size(),
		})

		return nil
	})

	if err != nil {
		return nil, fmt.Errorf("cannot list notes: %w", err)
	}

	return notes, nil
}

// resolvePath joins vault path with note path, adding .md extension if needed.
func resolvePath(vaultPath, notePath string) string {
	if !strings.HasSuffix(notePath, ".md") {
		notePath += ".md"
	}
	return filepath.Join(vaultPath, notePath)
}
