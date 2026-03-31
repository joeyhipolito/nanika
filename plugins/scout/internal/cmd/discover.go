package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/gather"
	"github.com/joeyhipolito/nanika-scout/internal/gemini"
)

// DiscoverCmd generates topic suggestions from recent intel and optionally applies them.
func DiscoverCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	// Parse flags first
	var since string
	auto := false
	dryRun := false

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`Usage: scout discover [options]

Options:
  --since <duration>  Analyze only recent intel, e.g. 7d, 24h
  --dry-run           Preview recommendations without applying
  --auto              Apply recommendations automatically
  --json              JSON output
  --help, -h          Show this help

Note: --dry-run and --auto are mutually exclusive.

Examples:
  scout discover
  scout discover --since 7d --dry-run
  scout discover --auto
`)
			return nil
		case "--since":
			if i+1 >= len(args) {
				return fmt.Errorf("--since requires a value (e.g. 24h, 7d, 2w)")
			}
			i++
			since = args[i]
		case "--auto":
			auto = true
		case "--dry-run":
			dryRun = true
		default:
			return fmt.Errorf("unknown flag: %s\n\nRun 'scout discover --help' for usage", args[i])
		}
	}

	if dryRun && auto {
		return fmt.Errorf("--dry-run and --auto are mutually exclusive")
	}

	// Load config to get Gemini API key
	cfg, err := config.Load()
	if err != nil {
		return err
	}

	geminiClient := gemini.NewClient(cfg.GeminiAPIKey)
	if !geminiClient.IsAvailable() {
		return fmt.Errorf("Gemini API key not configured\n\nRun: scout configure")
	}

	// Parse --since duration
	var sinceTime time.Time
	if since != "" {
		dur, err := parseDuration(since)
		if err != nil {
			return fmt.Errorf("invalid --since value %q: %w", since, err)
		}
		sinceTime = time.Now().Add(-dur)
	}

	// Load recent intel
	intelFiles, err := loadAllIntelFiles("", sinceTime)
	if err != nil {
		return err
	}

	if len(intelFiles) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No intel found to analyze.")
		fmt.Println("Run: scout gather")
		return nil
	}

	// Load existing topics
	existingTopics, err := loadAllTopics()
	if err != nil {
		return fmt.Errorf("loading existing topics: %w", err)
	}

	// Generate suggestions from Gemini
	suggestions, err := generateSuggestions(context.Background(), geminiClient, intelFiles, existingTopics)
	if err != nil {
		return fmt.Errorf("generating suggestions: %w", err)
	}

	if len(suggestions.Recommendations) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No recommendations generated. Try gathering more diverse intel.")
		return nil
	}

	// Output results
	if jsonOutput {
		return discoverJSON(suggestions)
	}

	if auto || dryRun {
		return discoverAutoApply(suggestions, auto, dryRun, existingTopics)
	}

	return discoverHuman(suggestions)
}

// DiscoverRecommendation represents a single topic improvement suggestion.
type DiscoverRecommendation struct {
	Action      string   `json:"action"`                // "create", "enhance", "add_sources"
	Topic       string   `json:"topic"`                 // topic name
	Description string   `json:"description,omitempty"` // for new topics
	SearchTerms []string `json:"search_terms,omitempty"`
	AddSources  []string `json:"add_sources,omitempty"`
	AddFeeds    []string `json:"add_feeds,omitempty"`
	Rationale   string   `json:"rationale"` // why this suggestion
}

// DiscoverSuggestions is the envelope for Gemini-generated suggestions.
type DiscoverSuggestions struct {
	GeneratedAt    string                   `json:"generated_at"`
	IntelSummary   string                   `json:"intel_summary"`   // what topics/themes were detected
	Recommendations []DiscoverRecommendation `json:"recommendations"`
}

// generateSuggestions calls Gemini to analyze intel and recommend topics.
func generateSuggestions(ctx context.Context, c *gemini.Client, intelFiles []gather.IntelFile, existingTopics []gather.TopicConfig) (*DiscoverSuggestions, error) {
	// Summarize intel for the prompt
	summary := summarizeIntel(intelFiles)

	// List existing topics
	existingNames := make([]string, len(existingTopics))
	for i, t := range existingTopics {
		existingNames[i] = t.Name
	}

	prompt := buildDiscoverPrompt(summary, existingNames)

	var suggestions DiscoverSuggestions
	if err := c.GenerateInto(ctx, prompt, &suggestions); err != nil {
		return nil, err
	}

	suggestions.GeneratedAt = time.Now().UTC().Format(time.RFC3339)
	return &suggestions, nil
}

// buildDiscoverPrompt constructs the Gemini prompt for topic discovery.
func buildDiscoverPrompt(summary string, existingTopics []string) string {
	return fmt.Sprintf(`You are an intelligence analyst. Analyze the following intel summary and recommend improvements to the topic configuration.

INTEL SUMMARY:
%s

EXISTING TOPICS:
%s

Based on the intel patterns, generate a JSON response with topic recommendations. For each recommendation:
- "action": "create" (new topic), "enhance" (improve existing), or "add_sources" (add gatherer sources)
- "topic": the topic name
- "description": (only for "create") brief description
- "search_terms": keywords to track (for "create" or "enhance")
- "add_sources": gatherer types to add (e.g., "substack", "youtube")
- "add_feeds": RSS/Atom feeds to add
- "rationale": why this recommendation

Respond ONLY with valid JSON in this format:
{
  "intel_summary": "High-level summary of detected themes and trends",
  "recommendations": [
    {
      "action": "create" | "enhance" | "add_sources",
      "topic": "topic-name",
      "description": "...",
      "search_terms": [...],
      "add_sources": [...],
      "add_feeds": [...],
      "rationale": "..."
    }
  ]
}

Be concise and practical. Only recommend topics/changes with clear evidence in the intel.
Limit recommendations to 3-5 high-value suggestions.`, summary, strings.Join(existingTopics, ", "))
}

// summarizeIntel creates a brief summary of gathered intel for the prompt.
func summarizeIntel(files []gather.IntelFile) string {
	type topicStats struct {
		name      string
		count     int
		sources   map[string]int
		keywords  map[string]int
	}

	stats := make(map[string]*topicStats)

	// Gather statistics
	for _, f := range files {
		if stats[f.Topic] == nil {
			stats[f.Topic] = &topicStats{
				name:     f.Topic,
				sources:  make(map[string]int),
				keywords: make(map[string]int),
			}
		}
		ts := stats[f.Topic]
		ts.count += len(f.Items)
		ts.sources[f.Source]++

		// Extract keywords from titles
		for _, item := range f.Items {
			words := strings.Fields(strings.ToLower(item.Title))
			for _, w := range words {
				w = strings.Trim(w, ".,;:!?\"'()-[]{}:")
				if len(w) > 4 && !isStopWord(w) {
					ts.keywords[w]++
				}
			}
		}
	}

	// Sort topics by item count
	var topicList []*topicStats
	for _, ts := range stats {
		topicList = append(topicList, ts)
	}
	sort.Slice(topicList, func(i, j int) bool {
		return topicList[i].count > topicList[j].count
	})

	// Format summary
	var b strings.Builder
	for _, ts := range topicList {
		b.WriteString(fmt.Sprintf("- %s: %d items from ", ts.name, ts.count))

		// List sources
		var srcList []string
		for src, count := range ts.sources {
			srcList = append(srcList, fmt.Sprintf("%s (%d)", src, count))
		}
		sort.Strings(srcList)
		b.WriteString(strings.Join(srcList, ", "))

		// Top keywords
		var kwList []string
		for kw := range ts.keywords {
			kwList = append(kwList, kw)
		}
		sort.Slice(kwList, func(i, j int) bool {
			return ts.keywords[kwList[i]] > ts.keywords[kwList[j]]
		})
		if len(kwList) > 10 {
			kwList = kwList[:10]
		}
		if len(kwList) > 0 {
			b.WriteString("; topics: ")
			b.WriteString(strings.Join(kwList, ", "))
		}
		b.WriteString("\n")
	}

	return b.String()
}

// discoverHuman renders recommendations in human-readable format.
func discoverHuman(sugg *DiscoverSuggestions) error {
	fmt.Printf("Topic Recommendations (%s)\n", sugg.GeneratedAt)
	fmt.Println(strings.Repeat("=", 60))
	fmt.Println()

	fmt.Println("Intel Analysis:")
	fmt.Println(sugg.IntelSummary)
	fmt.Println()

	fmt.Printf("Recommendations (%d)\n", len(sugg.Recommendations))
	fmt.Println(strings.Repeat("-", 60))

	for i, rec := range sugg.Recommendations {
		fmt.Printf("%d. [%s] %s\n", i+1, rec.Action, rec.Topic)
		if rec.Description != "" {
			fmt.Printf("   Description: %s\n", rec.Description)
		}
		fmt.Printf("   Rationale: %s\n", rec.Rationale)

		if len(rec.SearchTerms) > 0 {
			fmt.Printf("   Search Terms: %s\n", strings.Join(rec.SearchTerms, ", "))
		}
		if len(rec.AddSources) > 0 {
			fmt.Printf("   Add Sources: %s\n", strings.Join(rec.AddSources, ", "))
		}
		if len(rec.AddFeeds) > 0 {
			fmt.Printf("   Add Feeds:\n")
			for _, feed := range rec.AddFeeds {
				fmt.Printf("     - %s\n", feed)
			}
		}
		fmt.Println()
	}

	fmt.Println("Apply recommendations:")
	fmt.Println("  scout discover --auto")
	return nil
}

// discoverJSON outputs recommendations as JSON.
func discoverJSON(sugg *DiscoverSuggestions) error {
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(sugg)
}

// discoverAutoApply applies recommendations to topic configs.
func discoverAutoApply(sugg *DiscoverSuggestions, autoApply, dryRun bool, existingTopics []gather.TopicConfig) error {
	// Load existing topics into a map for easy lookup
	topicMap := make(map[string]*gather.TopicConfig)
	for i := range existingTopics {
		topicMap[existingTopics[i].Name] = &existingTopics[i]
	}

	var created []string
	var updated []string
	var skipped []string

	for _, rec := range sugg.Recommendations {
		switch rec.Action {
		case "create":
			if _, exists := topicMap[rec.Topic]; exists {
				skipped = append(skipped, fmt.Sprintf("%s (already exists)", rec.Topic))
				continue
			}

			newTopic := gather.TopicConfig{
				Name:        rec.Topic,
				Description: rec.Description,
				Sources:     rec.AddSources,
				SearchTerms: rec.SearchTerms,
				Feeds:       rec.AddFeeds,
				GatherInterval: "6h",
			}

			if !dryRun {
				if err := saveTopic(&newTopic); err != nil {
					return fmt.Errorf("saving topic %q: %w", rec.Topic, err)
				}
			}
			created = append(created, rec.Topic)

		case "enhance", "add_sources":
			t, exists := topicMap[rec.Topic]
			if !exists {
				skipped = append(skipped, fmt.Sprintf("%s (topic not found)", rec.Topic))
				continue
			}

			// Append search terms (deduped)
			termSet := make(map[string]bool)
			for _, term := range t.SearchTerms {
				termSet[term] = true
			}
			for _, term := range rec.SearchTerms {
				if !termSet[term] {
					t.SearchTerms = append(t.SearchTerms, term)
				}
			}

			// Append sources (deduped)
			srcSet := make(map[string]bool)
			for _, src := range t.Sources {
				srcSet[src] = true
			}
			for _, src := range rec.AddSources {
				if !srcSet[src] {
					t.Sources = append(t.Sources, src)
				}
			}

			// Append feeds (deduped)
			feedSet := make(map[string]bool)
			for _, feed := range t.Feeds {
				feedSet[feed] = true
			}
			for _, feed := range rec.AddFeeds {
				if !feedSet[feed] {
					t.Feeds = append(t.Feeds, feed)
				}
			}

			if !dryRun {
				if err := saveTopic(t); err != nil {
					return fmt.Errorf("saving topic %q: %w", rec.Topic, err)
				}
			}
			updated = append(updated, rec.Topic)
		}
	}

	// Report results
	mode := "Applied"
	if dryRun {
		mode = "Would apply"
	}

	fmt.Printf("%s %d recommendations\n", mode, len(sugg.Recommendations)-len(skipped))
	fmt.Println()

	if len(created) > 0 {
		fmt.Printf("Created: %s\n", strings.Join(created, ", "))
	}
	if len(updated) > 0 {
		fmt.Printf("Enhanced: %s\n", strings.Join(updated, ", "))
	}
	if len(skipped) > 0 {
		fmt.Printf("Skipped: %s\n", strings.Join(skipped, ", "))
	}

	if dryRun {
		fmt.Println()
		fmt.Println("Run with --auto to apply these changes.")
	}

	return nil
}
