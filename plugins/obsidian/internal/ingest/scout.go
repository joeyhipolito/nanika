package ingest

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
	"unicode"
)

// ScoutIntelFile represents the top-level structure of a scout intel JSON file.
type ScoutIntelFile struct {
	Topic      string      `json:"topic"`
	GatheredAt string      `json:"gathered_at"`
	Source     string      `json:"source"`
	Items      []ScoutItem `json:"items"`
}

// ScoutItem represents a single intel item from a scout file.
type ScoutItem struct {
	ID        string   `json:"id"`
	Title     string   `json:"title"`
	Content   string   `json:"content"`
	SourceURL string   `json:"source_url"`
	Author    string   `json:"author"`
	Timestamp string   `json:"timestamp"`
	Tags      []string `json:"tags"`
	Score     float64  `json:"score"`
}

// ScoutIngestOptions controls the scout ingest operation.
type ScoutIngestOptions struct {
	Topic  string
	Since  time.Duration // 0 means all
	DryRun bool
}

// ScoutIngestResult holds the results of a scout ingest.
type ScoutIngestResult struct {
	Created  []string `json:"created"`
	Skipped  []string `json:"skipped"`
	Errors   []string `json:"errors"`
	Source   string   `json:"source"`
}

// IngestScout reads scout intel files and creates vault notes.
func IngestScout(vaultPath string, opts ScoutIngestOptions, state *State) (*ScoutIngestResult, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return nil, fmt.Errorf("cannot determine home directory: %w", err)
	}

	intelBase := filepath.Join(home, ".scout", "intel")
	result := &ScoutIngestResult{Source: "scout"}

	// Determine which topics to scan
	topics, err := topicsToScan(intelBase, opts.Topic)
	if err != nil {
		return nil, err
	}

	// Cutoff time for --since filter
	var cutoff time.Time
	if opts.Since > 0 {
		cutoff = time.Now().Add(-opts.Since)
	}

	for _, topic := range topics {
		topicDir := filepath.Join(intelBase, topic)
		entries, err := os.ReadDir(topicDir)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Sprintf("cannot read topic %s: %v", topic, err))
			continue
		}

		for _, entry := range entries {
			if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
				continue
			}

			// Apply --since filter based on file name (timestamp prefix)
			if !cutoff.IsZero() {
				fileTime, err := parseFileTimestamp(entry.Name())
				if err == nil && fileTime.Before(cutoff) {
					continue
				}
			}

			filePath := filepath.Join(topicDir, entry.Name())
			intelFile, err := readScoutFile(filePath)
			if err != nil {
				result.Errors = append(result.Errors, fmt.Sprintf("cannot read %s: %v", entry.Name(), err))
				continue
			}

			for _, item := range intelFile.Items {
				if item.Title == "" || item.SourceURL == "" {
					continue
				}

				// State key = topic/item_id
				stateKey := topic + "/" + item.ID

				if state.HasScout(stateKey) {
					result.Skipped = append(result.Skipped, item.Title)
					continue
				}

				notePath := scoutNotePath(topic, item)
				content := buildScoutNote(topic, item, intelFile.Source)

				if opts.DryRun {
					result.Created = append(result.Created, notePath)
					continue
				}

				if err := writeNote(vaultPath, notePath, content); err != nil {
					if strings.Contains(err.Error(), "already exists") {
						result.Skipped = append(result.Skipped, notePath)
						state.MarkScout(stateKey)
						continue
					}
					result.Errors = append(result.Errors, fmt.Sprintf("cannot write %s: %v", notePath, err))
					continue
				}

				result.Created = append(result.Created, notePath)
				state.MarkScout(stateKey)
			}
		}
	}

	return result, nil
}

// topicsToScan returns the list of topics to scan. If topic is set, returns just that topic.
func topicsToScan(intelBase, topic string) ([]string, error) {
	if topic != "" {
		// Validate topic directory exists
		topicDir := filepath.Join(intelBase, topic)
		if _, err := os.Stat(topicDir); err != nil {
			return nil, fmt.Errorf("topic %q not found at %s", topic, topicDir)
		}
		return []string{topic}, nil
	}

	entries, err := os.ReadDir(intelBase)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, fmt.Errorf("scout intel directory not found at %s\n\nRun 'scout gather' first", intelBase)
		}
		return nil, fmt.Errorf("cannot read intel directory: %w", err)
	}

	var topics []string
	for _, e := range entries {
		if e.IsDir() && !strings.HasPrefix(e.Name(), ".") {
			topics = append(topics, e.Name())
		}
	}

	if len(topics) == 0 {
		return nil, fmt.Errorf("no topics found in %s\n\nRun 'scout gather' first", intelBase)
	}

	return topics, nil
}

// parseFileTimestamp extracts the timestamp from a file name like "2026-02-16T035342_web.json".
func parseFileTimestamp(name string) (time.Time, error) {
	// Extract the timestamp prefix before the first underscore
	idx := strings.Index(name, "_")
	if idx == -1 {
		return time.Time{}, fmt.Errorf("no underscore in filename: %s", name)
	}
	ts := name[:idx]

	// Format: "2026-02-16T035342"
	t, err := time.Parse("2006-01-02T150405", ts)
	if err != nil {
		return time.Time{}, fmt.Errorf("cannot parse timestamp %q: %w", ts, err)
	}
	return t, nil
}

// readScoutFile reads and parses a scout intel JSON file.
func readScoutFile(path string) (*ScoutIntelFile, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var f ScoutIntelFile
	if err := json.Unmarshal(data, &f); err != nil {
		return nil, fmt.Errorf("invalid JSON: %w", err)
	}

	return &f, nil
}

// scoutNotePath returns the vault-relative path for a scout item.
// Format: Intel/{topic}/{slug}.md
func scoutNotePath(topic string, item ScoutItem) string {
	slug := titleToSlug(item.Title)
	if slug == "" {
		slug = item.ID
	}
	return filepath.Join("Intel", topic, slug+".md")
}

// buildScoutNote generates markdown content for a scout intel note.
func buildScoutNote(topic string, item ScoutItem, source string) string {
	var b strings.Builder

	// Parse date from timestamp
	date := ""
	if item.Timestamp != "" {
		t, err := time.Parse(time.RFC3339, item.Timestamp)
		if err == nil {
			date = t.Format("2006-01-02")
		}
	}

	// Build tags list
	tags := []string{"intel", "scout", topic, source}
	for _, t := range item.Tags {
		if t != "" {
			tags = append(tags, t)
		}
	}

	// Frontmatter
	b.WriteString("---\n")
	b.WriteString("type: intel\n")
	fmt.Fprintf(&b, "source: %s\n", source)
	fmt.Fprintf(&b, "topic: %s\n", topic)
	if item.SourceURL != "" {
		fmt.Fprintf(&b, "url: \"%s\"\n", strings.ReplaceAll(item.SourceURL, "\"", "\\\""))
	}
	if date != "" {
		fmt.Fprintf(&b, "date: %s\n", date)
	}
	if item.Score > 0 {
		fmt.Fprintf(&b, "score: %.1f\n", item.Score)
	}
	fmt.Fprintf(&b, "tags: [%s]\n", strings.Join(tags, ", "))
	fmt.Fprintf(&b, "ingested: %s\n", time.Now().Format("2006-01-02"))
	b.WriteString("---\n\n")

	// Title
	title := item.Title
	if title == "" {
		title = item.ID
	}
	fmt.Fprintf(&b, "# %s\n\n", title)

	// Description / content (if distinct from title)
	if item.Content != "" && item.Content != item.Title {
		// Clean up content (sometimes it's just the title + site name)
		content := strings.TrimSpace(item.Content)
		if content != title {
			fmt.Fprintf(&b, "%s\n\n", content)
		}
	}

	// Metadata section
	b.WriteString("## Source\n\n")
	if item.SourceURL != "" {
		fmt.Fprintf(&b, "- **Link**: [%s](%s)\n", source, item.SourceURL)
	}
	if item.Author != "" {
		fmt.Fprintf(&b, "- **Author**: %s\n", item.Author)
	}
	if date != "" {
		fmt.Fprintf(&b, "- **Date**: %s\n", date)
	}
	if item.Score > 0 {
		fmt.Fprintf(&b, "- **Score**: %.1f\n", item.Score)
	}
	fmt.Fprintf(&b, "- **Topic**: %s\n", topic)

	b.WriteString("\n## Notes\n\n")
	b.WriteString("<!-- Add your notes and analysis here -->\n")

	return b.String()
}

// titleToSlug converts a title to a URL-friendly slug.
func titleToSlug(title string) string {
	// Lowercase
	s := strings.ToLower(title)

	// Remove trailing site name suffix (e.g., " - VentureBeat" or " | TechCrunch")
	// Only strip if there's meaningful content before the separator (> 10 chars)
	for _, sep := range []string{" - ", " | ", " — "} {
		if idx := strings.Index(s, sep); idx > 10 {
			// Only strip the last occurrence to avoid cutting mid-title
			if lastIdx := strings.LastIndex(s, sep); lastIdx > 10 {
				s = s[:lastIdx]
			}
			break
		}
	}

	// Replace non-alphanumeric with hyphens
	var b strings.Builder
	prevHyphen := false
	for _, r := range s {
		if unicode.IsLetter(r) || unicode.IsDigit(r) {
			b.WriteRune(r)
			prevHyphen = false
		} else if !prevHyphen && b.Len() > 0 {
			b.WriteRune('-')
			prevHyphen = true
		}
	}

	slug := strings.TrimRight(b.String(), "-")

	// Truncate to 80 chars
	if len(slug) > 80 {
		slug = slug[:80]
		// Trim trailing hyphen
		slug = strings.TrimRight(slug, "-")
	}

	return slug
}

// writeNote writes a note to the vault, creating directories as needed.
// Returns an error if the file already exists.
func writeNote(vaultPath, notePath, content string) error {
	if !strings.HasSuffix(notePath, ".md") {
		notePath += ".md"
	}
	fullPath := filepath.Join(vaultPath, notePath)

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

