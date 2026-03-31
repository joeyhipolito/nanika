package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/website"
)

// SyncOutput represents the JSON output format for the sync command.
type SyncOutput struct {
	Created   []string `json:"created"`
	Updated   []string `json:"updated"`
	Unchanged []string `json:"unchanged"`
	Skipped   []string `json:"skipped"`
	Source    string   `json:"source"`
	Target    string   `json:"target"`
}

// SyncCmd syncs website MDX metadata into Obsidian vault as note stubs.
// websiteURL is the base URL of the website (e.g. "https://example.com").
func SyncCmd(vaultPath, websitePath, websiteURL string, dryRun, force, jsonOutput bool) error {
	items, err := website.Scan(websitePath)
	if err != nil {
		return fmt.Errorf("failed to scan website: %w", err)
	}

	targetBase := filepath.Join(vaultPath, "Projects", "Website")
	stats := SyncOutput{
		Source: filepath.Join(websitePath, "content"),
		Target: targetBase,
	}

	for _, item := range items {
		if !item.Published && !force {
			stats.Skipped = append(stats.Skipped, item.Slug)
			continue
		}

		notePath := syncNotePath(item)
		fullPath := filepath.Join(targetBase, notePath)

		// Check if note exists and if website file changed
		if info, err := os.Stat(fullPath); err == nil && !force {
			if item.ModTime <= info.ModTime().Unix() {
				stats.Unchanged = append(stats.Unchanged, notePath)
				continue
			}
		}

		content := buildSyncNote(item, websiteURL)

		if dryRun {
			if _, err := os.Stat(fullPath); err == nil {
				stats.Updated = append(stats.Updated, notePath)
			} else {
				stats.Created = append(stats.Created, notePath)
			}
			continue
		}

		// Create parent directories
		dir := filepath.Dir(fullPath)
		if err := os.MkdirAll(dir, 0755); err != nil {
			return fmt.Errorf("cannot create directory %s: %w", dir, err)
		}

		// Determine if create or update
		if _, err := os.Stat(fullPath); err == nil {
			stats.Updated = append(stats.Updated, notePath)
		} else {
			stats.Created = append(stats.Created, notePath)
		}

		// Write directly (vault.WriteNote refuses existing files)
		if err := os.WriteFile(fullPath, []byte(content), 0644); err != nil {
			return fmt.Errorf("cannot write %s: %w", notePath, err)
		}
	}

	if jsonOutput {
		return output.JSON(stats)
	}

	printSyncReport(stats, dryRun)
	return nil
}

// syncNotePath returns the vault-relative path for a content item.
func syncNotePath(item website.ContentItem) string {
	switch item.ContentType {
	case "blog":
		return filepath.Join("Blog", item.Slug+".md")
	case "story":
		return filepath.Join("Stories", item.Slug+".md")
	case "project":
		return filepath.Join("Projects", item.Slug+".md")
	default:
		return item.Slug + ".md"
	}
}

// buildSyncNote generates the markdown content for a synced website note.
func buildSyncNote(item website.ContentItem, baseURL string) string {
	var b strings.Builder

	// Frontmatter
	b.WriteString("---\n")
	fmt.Fprintf(&b, "id: website-%s-%s\n", item.ContentType, item.Slug)
	b.WriteString("type: website-content\n")
	fmt.Fprintf(&b, "content-type: %s\n", item.ContentType)
	fmt.Fprintf(&b, "title: \"%s\"\n", strings.ReplaceAll(item.Title, "\"", "\\\""))
	fmt.Fprintf(&b, "date: %s\n", item.Date)
	fmt.Fprintf(&b, "published: %v\n", item.Published)
	if len(item.Tags) > 0 {
		fmt.Fprintf(&b, "tags: [%s]\n", strings.Join(item.Tags, ", "))
	} else {
		b.WriteString("tags: []\n")
	}
	url := contentURL(item, baseURL)
	fmt.Fprintf(&b, "url: \"%s\"\n", url)
	fmt.Fprintf(&b, "synced: %s\n", time.Now().Format(time.RFC3339))
	b.WriteString("---\n\n")

	// Body
	fmt.Fprintf(&b, "# %s\n\n", item.Title)
	if item.Description != "" {
		fmt.Fprintf(&b, "%s\n\n", item.Description)
	}

	// Metadata section
	b.WriteString("## Metadata\n")
	fmt.Fprintf(&b, "- **Type**: %s\n", item.ContentType)
	fmt.Fprintf(&b, "- **Published**: %s\n", item.Date)
	if len(item.Tags) > 0 {
		fmt.Fprintf(&b, "- **Tags**: %s\n", strings.Join(item.Tags, ", "))
	}
	fmt.Fprintf(&b, "- **URL**: [website](%s)\n", url)

	// Type-specific metadata
	if item.ContentType == "story" {
		if item.Role != "" {
			fmt.Fprintf(&b, "- **Role**: %s", item.Role)
			if item.Company != "" {
				fmt.Fprintf(&b, " at %s", item.Company)
			}
			if item.Duration != "" {
				fmt.Fprintf(&b, " (%s)", item.Duration)
			}
			b.WriteString("\n")
		}
	}
	if len(item.TechStack) > 0 {
		fmt.Fprintf(&b, "- **Tech Stack**: %s\n", strings.Join(item.TechStack, ", "))
	}
	if item.Series != "" {
		fmt.Fprintf(&b, "- **Series**: %s\n", item.Series)
	}

	b.WriteString("\n## Related Notes\n")
	b.WriteString("<!-- Add wikilinks to related vault notes -->\n")

	return b.String()
}

// contentURL returns the website URL for a content item.
// baseURL is the website base URL (e.g. "https://example.com").
func contentURL(item website.ContentItem, baseURL string) string {
	base := strings.TrimRight(baseURL, "/")
	switch item.ContentType {
	case "blog":
		return base + "/logs/" + item.Slug
	case "story":
		return base + "/stories/" + item.Slug
	case "project":
		return base + "/projects/" + item.Slug
	default:
		return base + "/" + item.Slug
	}
}

func printSyncReport(stats SyncOutput, dryRun bool) {
	if dryRun {
		fmt.Println("Website → Obsidian Sync (dry run)")
	} else {
		fmt.Println("Website → Obsidian Sync")
	}
	fmt.Println(strings.Repeat("=", 40))
	fmt.Printf("\nSource: %s\nTarget: %s\n\n", stats.Source, stats.Target)

	if len(stats.Created) > 0 {
		fmt.Println("Created:")
		for _, p := range stats.Created {
			fmt.Printf("  + %s\n", p)
		}
	}
	if len(stats.Updated) > 0 {
		fmt.Println("Updated:")
		for _, p := range stats.Updated {
			fmt.Printf("  ~ %s\n", p)
		}
	}
	if len(stats.Unchanged) > 0 {
		fmt.Println("Unchanged:")
		for _, p := range stats.Unchanged {
			fmt.Printf("  = %s\n", p)
		}
	}
	if len(stats.Skipped) > 0 {
		fmt.Println("Skipped (unpublished):")
		for _, p := range stats.Skipped {
			fmt.Printf("  - %s\n", p)
		}
	}

	fmt.Printf("\nSummary: %d created, %d updated, %d unchanged, %d skipped\n",
		len(stats.Created), len(stats.Updated), len(stats.Unchanged), len(stats.Skipped))
}
