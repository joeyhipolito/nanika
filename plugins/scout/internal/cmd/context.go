package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

// ContextCmd exports a compact context block for downstream consumption.
func ContextCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	// Parse flags
	var topicName string
	var outFile string
	compact := false
	var filteredArgs []string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`Usage: scout context <topic> [options]

Options:
  --compact           Limit to top 5 items (default: top 10)
  --file <path>       Write output to file
  --json              JSON output
  --help, -h          Show this help

Examples:
  scout context "ai-models"
  scout context "ai-models" --compact --json
  scout context "ai-models" --file context.md
`)
			return nil
		case "--compact":
			compact = true
		case "--file":
			if i+1 >= len(args) {
				return fmt.Errorf("--file requires a path value")
			}
			i++
			outFile = args[i]
		default:
			filteredArgs = append(filteredArgs, args[i])
		}
	}

	if len(filteredArgs) == 0 {
		return fmt.Errorf("topic name required\n\nUsage: scout context <topic> [--compact] [--json] [--file path]")
	}
	topicName = filteredArgs[0]

	// Load topic config
	topic, err := LoadTopicByName(topicName)
	if err != nil {
		return fmt.Errorf("topic %q not found: %w", topicName, err)
	}

	// Load intel
	topicDir := filepath.Join(config.IntelDir(), topicName)
	if _, err := os.Stat(topicDir); os.IsNotExist(err) {
		return fmt.Errorf("no intel found for topic %q\n\nGather first: scout gather %q", topicName, topicName)
	}

	intelFiles, err := loadIntelFiles(topicDir, time.Time{})
	if err != nil {
		return err
	}

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

	now := time.Now().UTC()
	gather.ScoreItems(allItems, topic.SearchTerms, now)
	allItems = gather.DedupeItems(allItems)

	sort.Slice(allItems, func(i, j int) bool {
		if allItems[i].Score != allItems[j].Score {
			return allItems[i].Score > allItems[j].Score
		}
		return allItems[i].Timestamp.After(allItems[j].Timestamp)
	})

	// Limit items
	limit := 10
	if compact {
		limit = 5
	}
	if len(allItems) > limit {
		allItems = allItems[:limit]
	}

	// Collect source summary
	sources := buildSourceSummary(topic, intelFiles)

	// Detect themes
	themes := detectThemes(allItems)

	// Generate output
	if jsonOutput {
		return contextJSON(topicName, allItems, themes, sources, now, outFile)
	}

	output := contextMarkdown(topicName, allItems, themes, sources, now)

	if outFile != "" {
		if err := os.WriteFile(outFile, []byte(output), 0600); err != nil {
			return fmt.Errorf("failed to write to %s: %w", outFile, err)
		}
		fmt.Printf("Context exported to %s\n", outFile)
		return nil
	}

	fmt.Print(output)
	return nil
}

// contextMarkdown generates a markdown context block.
func contextMarkdown(topicName string, items []gather.IntelItem, themes []theme,
	sources []sourceSummary, now time.Time) string {

	var b strings.Builder

	b.WriteString(fmt.Sprintf("## Scout Context: %s (%s)\n\n", topicName, now.Format("Jan 2, 2006")))

	b.WriteString(fmt.Sprintf("### Top %d by score\n", len(items)))
	for i, item := range items {
		content := item.Content
		if len(content) > 100 {
			content = content[:100] + "..."
		}
		b.WriteString(fmt.Sprintf("%d. **%s** (score: %.0f) — %s", i+1, item.Title, item.Score, sourceLabel(item)))
		if content != "" {
			b.WriteString(fmt.Sprintf(" — %s", content))
		}
		b.WriteString("\n")
	}
	b.WriteString("\n")

	if len(themes) > 0 {
		b.WriteString("### Themes\n")
		for _, t := range themes {
			b.WriteString(fmt.Sprintf("- %s: %d items\n", t.Label, t.Count))
		}
		b.WriteString("\n")
	}

	if len(sources) > 0 {
		b.WriteString("### Sources scanned\n")
		for _, s := range sources {
			b.WriteString(fmt.Sprintf("- %s: %s\n", s.Type, s.Detail))
		}
		b.WriteString("\n")
	}

	return b.String()
}

// contextJSON output types.
type contextOutput struct {
	Topic       string          `json:"topic"`
	GeneratedAt string          `json:"generated_at"`
	Items       []contextItem   `json:"items"`
	Themes      []theme         `json:"themes"`
	Sources     []sourceSummary `json:"sources"`
}

type contextItem struct {
	Title      string  `json:"title"`
	Score      float64 `json:"score"`
	Engagement int     `json:"engagement,omitempty"`
	Source     string  `json:"source"`
	URL        string  `json:"url"`
	Summary    string  `json:"summary"`
	Timestamp  string  `json:"timestamp"`
}

func contextJSON(topicName string, items []gather.IntelItem, themes []theme,
	sources []sourceSummary, now time.Time, outFile string) error {

	var ctxItems []contextItem
	for _, item := range items {
		summary := item.Content
		if len(summary) > 200 {
			summary = summary[:200] + "..."
		}
		ctxItems = append(ctxItems, contextItem{
			Title:      item.Title,
			Score:      item.Score,
			Engagement: item.Engagement,
			Source:     sourceLabel(item),
			URL:        item.SourceURL,
			Summary:    summary,
			Timestamp:  item.Timestamp.Format(time.RFC3339),
		})
	}

	output := contextOutput{
		Topic:       topicName,
		GeneratedAt: now.Format(time.RFC3339),
		Items:       ctxItems,
		Themes:      themes,
		Sources:     sources,
	}

	if outFile != "" {
		data, err := json.MarshalIndent(output, "", "  ")
		if err != nil {
			return err
		}
		if err := os.WriteFile(outFile, data, 0600); err != nil {
			return fmt.Errorf("failed to write to %s: %w", outFile, err)
		}
		fmt.Printf("Context exported to %s\n", outFile)
		return nil
	}

	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(output)
}
