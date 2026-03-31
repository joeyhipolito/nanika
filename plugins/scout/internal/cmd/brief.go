package cmd

import (
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

// BriefCmd generates a synthesized brief for a topic.
func BriefCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	// Parse flags
	var topicName string
	var since string
	topN := 10
	var filteredArgs []string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`Usage: scout brief <topic> [options]

Options:
  --since <duration>  Filter to recent items, e.g. 24h, 7d, 2w
  --top <n>           Limit to top N items (default: 10)
  --json              JSON output
  --help, -h          Show this help

Examples:
  scout brief "ai-models"
  scout brief "ai-models" --since 7d --top 20
`)
			return nil
		case "--since":
			if i+1 >= len(args) {
				return fmt.Errorf("--since requires a value (e.g. 24h, 7d, 2w)")
			}
			i++
			since = args[i]
		case "--top":
			if i+1 >= len(args) {
				return fmt.Errorf("--top requires a numeric value (e.g. --top 20)")
			}
			i++
			n, err := strconv.Atoi(args[i])
			if err != nil {
				return fmt.Errorf("--top requires a positive integer, got %q", args[i])
			}
			if n <= 0 {
				return fmt.Errorf("--top must be a positive integer, got %d", n)
			}
			topN = n
		default:
			filteredArgs = append(filteredArgs, args[i])
		}
	}

	if len(filteredArgs) == 0 {
		return fmt.Errorf("topic name required\n\nUsage: scout brief <topic> [--since 7d] [--top 10] [--json]")
	}
	topicName = filteredArgs[0]

	// Parse since duration
	var sinceTime time.Time
	if since != "" {
		dur, err := parseDuration(since)
		if err != nil {
			return fmt.Errorf("invalid --since value %q: %w", since, err)
		}
		sinceTime = time.Now().Add(-dur)
	}

	// Load topic config for search terms
	topic, err := LoadTopicByName(topicName)
	if err != nil {
		return fmt.Errorf("topic %q not found: %w", topicName, err)
	}

	// Load intel files
	topicDir := filepath.Join(config.IntelDir(), topicName)
	if _, err := os.Stat(topicDir); os.IsNotExist(err) {
		return fmt.Errorf("no intel found for topic %q\n\nGather first: scout gather %q", topicName, topicName)
	}

	intelFiles, err := loadIntelFiles(topicDir, sinceTime)
	if err != nil {
		return err
	}

	// Collect all items
	var allItems []gather.IntelItem
	for _, f := range intelFiles {
		allItems = append(allItems, f.Items...)
	}

	if len(allItems) == 0 {
		if jsonOutput {
			fmt.Println("{}")
			return nil
		}
		fmt.Printf("No intel items found for %q\n", topicName)
		return nil
	}

	// Re-score items with current time
	now := time.Now().UTC()
	gather.ScoreItems(allItems, topic.SearchTerms, now)

	// Deduplicate
	allItems = gather.DedupeItems(allItems)

	// Sort by score descending
	sort.Slice(allItems, func(i, j int) bool {
		if allItems[i].Score != allItems[j].Score {
			return allItems[i].Score > allItems[j].Score
		}
		return allItems[i].Timestamp.After(allItems[j].Timestamp)
	})

	// Limit to topN
	topItems := allItems
	if len(topItems) > topN {
		topItems = topItems[:topN]
	}

	// Detect themes
	themes := detectThemes(topItems)

	// Find recent items (last 24h)
	var recentItems []gather.IntelItem
	cutoff := now.Add(-24 * time.Hour)
	for _, item := range allItems {
		if item.Timestamp.After(cutoff) {
			recentItems = append(recentItems, item)
		}
	}

	// Count sources
	sourceSet := make(map[string]bool)
	for _, f := range intelFiles {
		sourceSet[f.Source] = true
	}

	if jsonOutput {
		return briefJSON(topicName, topItems, themes, recentItems, len(sourceSet), now)
	}

	return briefHuman(topicName, topItems, themes, recentItems, len(sourceSet), now)
}

// briefHuman renders a human-readable brief.
func briefHuman(topicName string, items []gather.IntelItem, themes []theme,
	recentItems []gather.IntelItem, sourceCount int, now time.Time) error {

	fmt.Printf("Scout Brief: %s (%s)\n", topicName, now.Format("Jan 2, 2006"))
	fmt.Println(strings.Repeat("=", 50))
	fmt.Println()

	// Top Stories (by score)
	fmt.Println("Top Stories")
	fmt.Println(strings.Repeat("-", 30))
	for i, item := range items {
		source := sourceLabel(item)
		engLabel := ""
		if item.Engagement > 0 {
			engLabel = fmt.Sprintf(" — %d engagement", item.Engagement)
		}
		fmt.Printf("  %d. %s%s\n", i+1, item.Title, engLabel)
		if item.Content != "" {
			content := item.Content
			if len(content) > 150 {
				content = content[:150] + "..."
			}
			fmt.Printf("     %s\n", content)
		}
		fmt.Printf("     Source: %s (score: %.0f)\n", source, item.Score)
		fmt.Println()
	}

	// Trending Themes
	if len(themes) > 0 {
		fmt.Println("Trending Themes")
		fmt.Println(strings.Repeat("-", 30))
		for _, t := range themes {
			fmt.Printf("  - %s: %d items\n", t.Label, t.Count)
		}
		fmt.Println()
	}

	// Recent (last 24h)
	if len(recentItems) > 0 {
		fmt.Println("Recent (last 24h)")
		fmt.Println(strings.Repeat("-", 30))
		for _, item := range recentItems {
			fmt.Printf("  - %s — %s\n", item.Title, sourceLabel(item))
		}
		fmt.Println()
	}

	fmt.Printf("Total: %d items from %d source(s)\n", len(items), sourceCount)
	return nil
}

// briefJSON output types.
type briefOutput struct {
	Topic       string      `json:"topic"`
	GeneratedAt string      `json:"generated_at"`
	TopItems    []briefItem `json:"top_items"`
	Themes      []theme     `json:"themes"`
	RecentItems []briefItem `json:"recent_items"`
	TotalItems  int         `json:"total_items"`
	Sources     int         `json:"sources"`
}

type briefItem struct {
	Title      string  `json:"title"`
	Score      float64 `json:"score"`
	Engagement int     `json:"engagement,omitempty"`
	Source     string  `json:"source"`
	URL        string  `json:"url"`
	Timestamp  string  `json:"timestamp"`
}

func briefJSON(topicName string, items []gather.IntelItem, themes []theme,
	recentItems []gather.IntelItem, sourceCount int, now time.Time) error {

	var topItems []briefItem
	for _, item := range items {
		topItems = append(topItems, briefItem{
			Title:      item.Title,
			Score:      item.Score,
			Engagement: item.Engagement,
			Source:     sourceLabel(item),
			URL:        item.SourceURL,
			Timestamp:  item.Timestamp.Format(time.RFC3339),
		})
	}

	var recent []briefItem
	for _, item := range recentItems {
		recent = append(recent, briefItem{
			Title:      item.Title,
			Score:      item.Score,
			Engagement: item.Engagement,
			Source:     sourceLabel(item),
			URL:        item.SourceURL,
			Timestamp:  item.Timestamp.Format(time.RFC3339),
		})
	}

	output := briefOutput{
		Topic:       topicName,
		GeneratedAt: now.Format(time.RFC3339),
		TopItems:    topItems,
		Themes:      themes,
		RecentItems: recent,
		TotalItems:  len(items),
		Sources:     sourceCount,
	}

	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(output)
}

// --- Shared helpers used by both brief.go and context.go ---

// theme represents a detected trending theme.
type theme struct {
	Label string `json:"label"`
	Count int    `json:"count"`
}

// detectThemes groups items by overlapping title words.
func detectThemes(items []gather.IntelItem) []theme {
	wordCount := make(map[string]int)
	wordItems := make(map[string][]int)

	for i, item := range items {
		seen := make(map[string]bool)
		for _, w := range strings.Fields(strings.ToLower(item.Title)) {
			w = strings.Trim(w, ".,;:!?\"'()-[]{}")
			if len(w) <= 3 || isStopWord(w) {
				continue
			}
			if !seen[w] {
				seen[w] = true
				wordCount[w]++
				wordItems[w] = append(wordItems[w], i)
			}
		}
	}

	type wordFreq struct {
		word  string
		count int
	}
	var ranked []wordFreq
	for w, c := range wordCount {
		if c >= 2 {
			ranked = append(ranked, wordFreq{w, c})
		}
	}
	sort.Slice(ranked, func(i, j int) bool {
		return ranked[i].count > ranked[j].count
	})

	var themes []theme
	assigned := make(map[int]bool)

	for _, wf := range ranked {
		if len(themes) >= 5 {
			break
		}
		count := 0
		for _, idx := range wordItems[wf.word] {
			if !assigned[idx] {
				count++
			}
		}
		if count >= 2 {
			for _, idx := range wordItems[wf.word] {
				assigned[idx] = true
			}
			themes = append(themes, theme{
				Label: wf.word,
				Count: count,
			})
		}
	}

	return themes
}

// sourceLabel extracts a short source label from an item.
func sourceLabel(item gather.IntelItem) string {
	if item.SourceURL == "" {
		return "unknown"
	}
	u := item.SourceURL
	if idx := strings.Index(u, "://"); idx >= 0 {
		u = u[idx+3:]
	}
	if idx := strings.Index(u, "/"); idx >= 0 {
		u = u[:idx]
	}
	return u
}

// isStopWord checks if a word is a common stop word.
func isStopWord(w string) bool {
	stops := map[string]bool{
		"the": true, "and": true, "for": true, "are": true, "but": true,
		"not": true, "you": true, "all": true, "can": true, "had": true,
		"her": true, "was": true, "one": true, "our": true, "out": true,
		"has": true, "have": true, "been": true, "from": true, "with": true,
		"they": true, "this": true, "that": true, "what": true, "will": true,
		"into": true, "about": true, "than": true, "them": true, "then": true,
		"some": true, "when": true, "more": true, "also": true, "how": true,
		"new": true, "its": true,
	}
	return stops[w]
}

// sourceSummary describes a source type and its details.
type sourceSummary struct {
	Type   string `json:"type"`
	Detail string `json:"detail"`
}

// buildSourceSummary builds a summary of sources for a topic.
func buildSourceSummary(topic *gather.TopicConfig, files []gather.IntelFile) []sourceSummary {
	var summaries []sourceSummary

	sourceFiles := make(map[string]int)
	for _, f := range files {
		sourceFiles[f.Source]++
	}

	for _, src := range topic.Sources {
		switch src {
		case "rss":
			summaries = append(summaries, sourceSummary{
				Type:   "RSS",
				Detail: fmt.Sprintf("%d feeds (%d files)", len(topic.Feeds), sourceFiles["rss"]),
			})
		case "github":
			summaries = append(summaries, sourceSummary{
				Type:   "GitHub",
				Detail: fmt.Sprintf("%d queries (%d files)", len(topic.GitHubQueries), sourceFiles["github"]),
			})
		case "reddit":
			summaries = append(summaries, sourceSummary{
				Type:   "Reddit",
				Detail: fmt.Sprintf("%d subreddits (%d files)", len(topic.RedditSubs), sourceFiles["reddit"]),
			})
		case "x":
			summaries = append(summaries, sourceSummary{
				Type:   "X",
				Detail: fmt.Sprintf("Bird CLI (%d files)", sourceFiles["x"]),
			})
		}
	}

	return summaries
}
