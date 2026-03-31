package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/gather"
	"github.com/joeyhipolito/nanika-scout/internal/suggest"
)

// SuggestCmd generates content suggestions from gathered intel.
func SuggestCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	// Parse flags
	var since string
	var topicFilter string
	var typeFilter string
	limit := 5

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`Usage: scout suggest [options]

Options:
  --topic <name>               Suggestions for one topic only
  --type blog|thread|video     Filter by content type
  --since <duration>           Only use recent intel, e.g. 7d, 24h
  --limit <n>                  Max suggestions (default: 5)
  --json                       JSON output
  --help, -h                   Show this help

Examples:
  scout suggest
  scout suggest --topic "ai-models" --type blog
  scout suggest --since 7d --limit 10
`)
			return nil
		case "--since":
			if i+1 >= len(args) {
				return fmt.Errorf("--since requires a value (e.g. 24h, 7d, 2w)")
			}
			i++
			since = args[i]
		case "--topic":
			if i+1 >= len(args) {
				return fmt.Errorf("--topic requires a topic name")
			}
			i++
			topicFilter = args[i]
		case "--type":
			if i+1 >= len(args) {
				return fmt.Errorf("--type requires a value: blog, thread, or video")
			}
			i++
			typeFilter = args[i]
		case "--limit":
			if i+1 >= len(args) {
				return fmt.Errorf("--limit requires a numeric value (e.g. --limit 10)")
			}
			i++
			n, err := strconv.Atoi(args[i])
			if err != nil {
				return fmt.Errorf("--limit requires a positive integer, got %q", args[i])
			}
			if n <= 0 {
				return fmt.Errorf("--limit must be a positive integer, got %d", n)
			}
			limit = n
		default:
			return fmt.Errorf("unknown flag: %s\n\nRun 'scout suggest --help' for usage", args[i])
		}
	}

	// Validate --type if provided
	if typeFilter != "" {
		switch typeFilter {
		case "blog", "thread", "video":
			// valid
		default:
			return fmt.Errorf("invalid --type %q: must be blog, thread, or video", typeFilter)
		}
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

	// Load intel files across all topics (or filtered topic)
	intelFiles, err := loadAllIntelFiles(topicFilter, sinceTime)
	if err != nil {
		return err
	}

	if len(intelFiles) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No intel found.")
		if topicFilter != "" {
			fmt.Printf("Topic %q may not exist or has no gathered data.\n", topicFilter)
		}
		fmt.Println("Run: scout gather")
		return nil
	}

	// Run the suggest engine
	now := time.Now().UTC()
	suggestions := suggest.Analyze(intelFiles, now, limit)

	// Filter by content type if requested
	if typeFilter != "" {
		suggestions = filterByType(suggestions, typeFilter)
	}

	if len(suggestions) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No suggestions generated.")
		fmt.Println("Try gathering more intel or broadening your filters.")
		return nil
	}

	if jsonOutput {
		return suggestJSON(suggestions, now)
	}
	return suggestHuman(suggestions, now)
}

// suggestHuman renders suggestions in human-readable format.
func suggestHuman(suggestions []suggest.Suggestion, now time.Time) error {
	fmt.Printf("Content Suggestions (%s)\n", now.Format("Jan 2, 2006"))
	fmt.Println(strings.Repeat("=", 50))
	fmt.Println()

	for i, s := range suggestions {
		fmt.Printf("  %d. [%s] %s (score: %d)\n", i+1, s.ContentType, s.Title, s.Score)
		if s.Angle != "" {
			fmt.Printf("     Angle: %s\n", s.Angle)
		}
		fmt.Printf("     Topics: %s\n", strings.Join(s.Topics, ", "))

		if len(s.Sources) > 0 {
			fmt.Printf("     Sources:\n")
			for _, src := range s.Sources {
				fmt.Printf("       - %s (%s)\n", src.Title, src.Source)
				if src.URL != "" {
					fmt.Printf("         %s\n", src.URL)
				}
			}
		}
		fmt.Println()
	}

	fmt.Printf("%d suggestion(s) generated\n", len(suggestions))
	return nil
}

// suggestOutput is the JSON envelope for suggest output.
type suggestOutput struct {
	GeneratedAt string               `json:"generated_at"`
	Count       int                  `json:"count"`
	Suggestions []suggest.Suggestion `json:"suggestions"`
}

// suggestJSON renders suggestions as structured JSON.
func suggestJSON(suggestions []suggest.Suggestion, now time.Time) error {
	output := suggestOutput{
		GeneratedAt: now.Format(time.RFC3339),
		Count:       len(suggestions),
		Suggestions: suggestions,
	}
	encoder := json.NewEncoder(os.Stdout)
	encoder.SetIndent("", "  ")
	return encoder.Encode(output)
}

// loadAllIntelFiles reads intel from all topic directories (or a single one).
func loadAllIntelFiles(topicFilter string, sinceTime time.Time) ([]gather.IntelFile, error) {
	intelDir := config.IntelDir()

	if topicFilter != "" {
		topicDir := filepath.Join(intelDir, topicFilter)
		if _, err := os.Stat(topicDir); os.IsNotExist(err) {
			return nil, fmt.Errorf("no intel found for topic %q\n\nGather first: scout gather %q", topicFilter, topicFilter)
		}
		return loadIntelFiles(topicDir, sinceTime)
	}

	// Load from all topic directories
	entries, err := os.ReadDir(intelDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("failed to read intel directory: %w", err)
	}

	var allFiles []gather.IntelFile
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}
		topicDir := filepath.Join(intelDir, entry.Name())
		files, err := loadIntelFiles(topicDir, sinceTime)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to load intel for %s: %v\n", entry.Name(), err)
			continue
		}
		allFiles = append(allFiles, files...)
	}

	return allFiles, nil
}

// filterByType keeps only suggestions matching the given content type.
func filterByType(suggestions []suggest.Suggestion, contentType string) []suggest.Suggestion {
	var filtered []suggest.Suggestion
	for _, s := range suggestions {
		if s.ContentType == contentType {
			filtered = append(filtered, s)
		}
	}
	return filtered
}
