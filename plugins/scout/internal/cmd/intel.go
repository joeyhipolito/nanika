package cmd

import (
	"encoding/csv"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

const (
	formatMarkdown = "markdown"
	formatJSON     = "json"
	formatCSV      = "csv"
)

// IntelCmd browses gathered intelligence.
func IntelCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	// Parse flags
	var topicName string
	var since string
	var format string
	var filteredArgs []string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`Usage: scout intel [<topic>] [options]

Options:
  --since <duration>  Filter items by date window, e.g. 24h, 7d, 2w
  --json              JSON output
  --help, -h          Show this help

Examples:
  scout intel                           List all topics with item counts
  scout intel "ai-models"               Show items for a topic
  scout intel "ai-models" --since 7d    Show items from last 7 days
`)
			return nil
		case "--since":
			if i+1 >= len(args) {
				return fmt.Errorf("--since requires a value (e.g. 24h, 7d, 2w)")
			}
			i++
			since = args[i]
		case "--format":
			if i+1 < len(args) {
				i++
				format = args[i]
			}
		default:
			filteredArgs = append(filteredArgs, args[i])
		}
	}

	// Determine output format: --format flag takes precedence, then --json flag, then default markdown
	if format == "" {
		if jsonOutput {
			format = formatJSON
		} else {
			format = formatMarkdown
		}
	}

	// Validate format
	if format != formatMarkdown && format != formatJSON && format != formatCSV {
		return fmt.Errorf("invalid format %q (must be 'markdown', 'json', or 'csv')", format)
	}

	if len(filteredArgs) > 0 {
		topicName = filteredArgs[0]
	}

	// Parse since duration
	var sinceTime time.Time
	if since != "" {
		dur, err := parseDuration(since)
		if err != nil {
			return fmt.Errorf("invalid --since value %q: %w", since, err)
		}
		sinceTime = time.Now().Add(-dur)
	}

	if topicName == "" {
		return intelListCmd(format)
	}

	return intelShowCmd(topicName, sinceTime, format)
}

// intelListCmd lists all topics with item counts.
func intelListCmd(format string) error {
	intelDir := config.IntelDir()
	entries, err := os.ReadDir(intelDir)
	if err != nil {
		if os.IsNotExist(err) {
			if format == formatJSON {
				fmt.Println("[]")
				return nil
			}
			if format == formatCSV {
				w := csv.NewWriter(os.Stdout)
				w.Write([]string{"name", "files", "items", "last_gather"})
				w.Flush()
				return nil
			}
			fmt.Println("No intel gathered yet.")
			fmt.Println("Run: scout gather")
			return nil
		}
		return fmt.Errorf("failed to read intel directory: %w", err)
	}

	type topicSummary struct {
		Name       string `json:"name"`
		Files      int    `json:"files"`
		Items      int    `json:"items"`
		LastGather string `json:"last_gather"`
	}

	var summaries []topicSummary

	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}

		topicDir := filepath.Join(intelDir, entry.Name())
		files, items, lastGather := countIntel(topicDir)

		summaries = append(summaries, topicSummary{
			Name:       entry.Name(),
			Files:      files,
			Items:      items,
			LastGather: lastGather,
		})
	}

	if len(summaries) == 0 {
		if format == formatJSON {
			fmt.Println("[]")
			return nil
		}
		if format == formatCSV {
			w := csv.NewWriter(os.Stdout)
			w.Write([]string{"name", "files", "items", "last_gather"})
			w.Flush()
			return nil
		}
		fmt.Println("No intel gathered yet.")
		fmt.Println("Run: scout gather")
		return nil
	}

	if format == formatJSON {
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(summaries)
	}

	if format == formatCSV {
		w := csv.NewWriter(os.Stdout)
		w.Write([]string{"name", "files", "items", "last_gather"})
		for _, s := range summaries {
			w.Write([]string{s.Name, strconv.Itoa(s.Files), strconv.Itoa(s.Items), s.LastGather})
		}
		w.Flush()
		return w.Error()
	}

	fmt.Printf("Intel (%d topics)\n", len(summaries))
	fmt.Println(strings.Repeat("-", 60))
	for _, s := range summaries {
		fmt.Printf("  %-20s  %4d items  %3d files  last: %s\n", s.Name, s.Items, s.Files, s.LastGather)
	}
	fmt.Println()
	fmt.Println("Show topic details:")
	fmt.Println("  scout intel \"topic-name\"")
	return nil
}

// intelShowCmd shows intel items for a specific topic.
func intelShowCmd(topicName string, sinceTime time.Time, format string) error {
	// Validate format
	if format != formatMarkdown && format != formatJSON && format != formatCSV {
		return fmt.Errorf("invalid format %q (must be 'markdown', 'json', or 'csv')", format)
	}

	topicDir := filepath.Join(config.IntelDir(), topicName)

	if _, err := os.Stat(topicDir); os.IsNotExist(err) {
		return fmt.Errorf("no intel found for topic %q\n\nGather first: scout gather %q", topicName, topicName)
	}

	// Load all intel files for this topic
	intelFiles, err := loadIntelFiles(topicDir, sinceTime)
	if err != nil {
		return err
	}

	if len(intelFiles) == 0 {
		if format == formatJSON {
			fmt.Println("[]")
			return nil
		}
		if format == formatCSV {
			w := csv.NewWriter(os.Stdout)
			w.Write([]string{"timestamp", "score", "title", "author", "url", "content", "tags", "engagement"})
			w.Flush()
			return nil
		}
		fmt.Printf("No intel items found for %q", topicName)
		if !sinceTime.IsZero() {
			fmt.Printf(" since %s", sinceTime.Format("2006-01-02"))
		}
		fmt.Println()
		return nil
	}

	// Collect all items across files
	var allItems []gather.IntelItem
	for _, f := range intelFiles {
		allItems = append(allItems, f.Items...)
	}

	// Deduplicate using Jaccard title similarity + exact ID match
	allItems = gather.DedupeItems(allItems)

	// Sort by score descending, then timestamp as tiebreaker
	sort.Slice(allItems, func(i, j int) bool {
		if allItems[i].Score != allItems[j].Score {
			return allItems[i].Score > allItems[j].Score
		}
		return allItems[i].Timestamp.After(allItems[j].Timestamp)
	})

	if format == formatJSON {
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(allItems)
	}

	if format == formatCSV {
		w := csv.NewWriter(os.Stdout)
		w.Write([]string{"timestamp", "score", "title", "author", "url", "content", "tags", "engagement"})
		for _, item := range allItems {
			w.Write([]string{
				item.Timestamp.Format("2006-01-02 15:04"),
				strconv.FormatFloat(item.Score, 'f', -1, 64),
				item.Title,
				item.Author,
				item.SourceURL,
				item.Content,
				strings.Join(item.Tags, ";"),
				strconv.Itoa(item.Engagement),
			})
		}
		w.Flush()
		return w.Error()
	}

	fmt.Printf("Intel: %s (%d items)\n", topicName, len(allItems))
	fmt.Println(strings.Repeat("-", 60))

	for i, item := range allItems {
		if i > 0 {
			fmt.Println()
		}

		ts := item.Timestamp.Format("2006-01-02 15:04")
		if item.Score > 0 {
			fmt.Printf("  [%s] (%.0f) %s\n", ts, item.Score, item.Title)
		} else {
			fmt.Printf("  [%s] %s\n", ts, item.Title)
		}

		if item.Author != "" {
			fmt.Printf("  By: %s\n", item.Author)
		}

		if item.SourceURL != "" {
			fmt.Printf("  URL: %s\n", item.SourceURL)
		}

		if item.Content != "" {
			// Truncate content for display
			content := item.Content
			if len(content) > 200 {
				content = content[:200] + "..."
			}
			fmt.Printf("  %s\n", content)
		}

		if len(item.Tags) > 0 {
			fmt.Printf("  Tags: %s\n", strings.Join(item.Tags, ", "))
		}

		if item.Engagement > 0 {
			fmt.Printf("  Engagement: %d\n", item.Engagement)
		}
	}

	return nil
}

// loadIntelFiles reads all intel JSON files from a topic directory.
func loadIntelFiles(topicDir string, sinceTime time.Time) ([]gather.IntelFile, error) {
	entries, err := os.ReadDir(topicDir)
	if err != nil {
		return nil, fmt.Errorf("failed to read intel directory: %w", err)
	}

	var files []gather.IntelFile
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}

		filePath := filepath.Join(topicDir, entry.Name())
		data, err := os.ReadFile(filePath)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to read %s: %v\n", entry.Name(), err)
			continue
		}

		var intelFile gather.IntelFile
		if err := json.Unmarshal(data, &intelFile); err != nil {
			fmt.Fprintf(os.Stderr, "Warning: invalid JSON in %s: %v\n", entry.Name(), err)
			continue
		}

		// Filter by since time
		if !sinceTime.IsZero() && intelFile.GatheredAt.Before(sinceTime) {
			continue
		}

		files = append(files, intelFile)
	}

	// Sort by gathered_at descending
	sort.Slice(files, func(i, j int) bool {
		return files[i].GatheredAt.After(files[j].GatheredAt)
	})

	return files, nil
}

// countIntel counts files and items in a topic intel directory.
func countIntel(topicDir string) (files int, items int, lastGather string) {
	entries, err := os.ReadDir(topicDir)
	if err != nil {
		return 0, 0, "unknown"
	}

	var latestTime time.Time
	seen := make(map[string]bool)

	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}
		files++

		filePath := filepath.Join(topicDir, entry.Name())
		data, err := os.ReadFile(filePath)
		if err != nil {
			continue
		}

		var intelFile gather.IntelFile
		if err := json.Unmarshal(data, &intelFile); err != nil {
			continue
		}

		for _, item := range intelFile.Items {
			if !seen[item.ID] {
				seen[item.ID] = true
				items++
			}
		}
		if intelFile.GatheredAt.After(latestTime) {
			latestTime = intelFile.GatheredAt
		}
	}

	if latestTime.IsZero() {
		lastGather = "never"
	} else {
		lastGather = latestTime.Format("2006-01-02 15:04")
	}

	return files, items, lastGather
}

// parseDuration parses a human-friendly duration string like "24h", "7d", "30d".
func parseDuration(s string) (time.Duration, error) {
	s = strings.TrimSpace(s)
	if s == "" {
		return 0, fmt.Errorf("empty duration")
	}

	// Handle day suffix
	if strings.HasSuffix(s, "d") {
		numStr := strings.TrimSuffix(s, "d")
		var days int
		if _, err := fmt.Sscanf(numStr, "%d", &days); err != nil {
			return 0, fmt.Errorf("invalid day duration: %s", s)
		}
		return time.Duration(days) * 24 * time.Hour, nil
	}

	// Handle week suffix
	if strings.HasSuffix(s, "w") {
		numStr := strings.TrimSuffix(s, "w")
		var weeks int
		if _, err := fmt.Sscanf(numStr, "%d", &weeks); err != nil {
			return 0, fmt.Errorf("invalid week duration: %s", s)
		}
		return time.Duration(weeks) * 7 * 24 * time.Hour, nil
	}

	// Fall back to Go's time.ParseDuration (handles h, m, s)
	return time.ParseDuration(s)
}
