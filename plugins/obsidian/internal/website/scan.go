// Package website provides scanning of website content directories
// to extract MDX frontmatter metadata for syncing into the Obsidian vault.
package website

import (
	"io/fs"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// ContentItem represents a parsed piece of website content.
type ContentItem struct {
	Slug        string
	ContentType string // "blog", "story", "project"
	Title       string
	Description string
	Date        string
	Published   bool
	Tags        []string
	Author      string
	Series      string
	// Story-specific
	Role     string
	Company  string
	Duration string
	// Project-specific
	TechStack []string
	// Filesystem
	ModTime int64
}

// Scan walks the website content directory and returns all MDX content items.
func Scan(websitePath string) ([]ContentItem, error) {
	contentDir := filepath.Join(websitePath, "content")

	if _, err := os.Stat(contentDir); err != nil {
		return nil, err
	}

	var items []ContentItem

	err := filepath.WalkDir(contentDir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil
		}
		if d.IsDir() {
			return nil
		}
		if !strings.HasSuffix(d.Name(), ".mdx") && !strings.HasSuffix(d.Name(), ".md") {
			return nil
		}

		relPath, _ := filepath.Rel(contentDir, path)
		contentType := detectContentType(relPath)
		if contentType == "" {
			return nil // skip unknown content types
		}

		data, err := os.ReadFile(path)
		if err != nil {
			return nil
		}

		note := vault.ParseNote(string(data))
		info, _ := d.Info()

		item := ContentItem{
			Slug:        slugFromPath(relPath),
			ContentType: contentType,
			Title:       getString(note.Frontmatter, "title"),
			Description: getString(note.Frontmatter, "description"),
			Date:        getString(note.Frontmatter, "date"),
			Published:   getBool(note.Frontmatter, "published"),
			Tags:        getStringSlice(note.Frontmatter, "tags"),
			Author:      getString(note.Frontmatter, "author"),
			Series:      getString(note.Frontmatter, "series"),
			Role:        getString(note.Frontmatter, "role"),
			Company:     getString(note.Frontmatter, "company"),
			Duration:    getString(note.Frontmatter, "duration"),
			TechStack:   getStringSlice(note.Frontmatter, "techStack"),
		}
		if info != nil {
			item.ModTime = info.ModTime().Unix()
		}

		items = append(items, item)
		return nil
	})

	return items, err
}

// detectContentType determines the content type from the relative path.
func detectContentType(relPath string) string {
	parts := strings.SplitN(relPath, string(filepath.Separator), 2)
	if len(parts) == 0 {
		return ""
	}
	switch parts[0] {
	case "blog":
		return "blog"
	case "stories":
		return "story"
	case "projects":
		return "project"
	default:
		return ""
	}
}

// slugFromPath extracts the slug from a content file path.
// e.g., "blog/2026-02-13-what-claude-code-insights-told-me.mdx" -> "2026-02-13-what-claude-code-insights-told-me"
func slugFromPath(relPath string) string {
	base := filepath.Base(relPath)
	ext := filepath.Ext(base)
	return strings.TrimSuffix(base, ext)
}

func getString(fm map[string]any, key string) string {
	if v, ok := fm[key].(string); ok {
		return v
	}
	return ""
}

func getBool(fm map[string]any, key string) bool {
	switch v := fm[key].(type) {
	case bool:
		return v
	case string:
		return v == "true"
	default:
		return false
	}
}

func getStringSlice(fm map[string]any, key string) []string {
	switch v := fm[key].(type) {
	case []string:
		return v
	case string:
		if v != "" {
			return []string{v}
		}
	}
	return nil
}
