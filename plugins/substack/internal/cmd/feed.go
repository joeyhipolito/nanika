package cmd

import (
	"crypto/md5"
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// ScoutItem is the IntelItem-compatible format for scout pipeline integration.
type ScoutItem struct {
	ID         string    `json:"id"`
	Title      string    `json:"title"`
	Content    string    `json:"content"`
	SourceURL  string    `json:"source_url"`
	Author     string    `json:"author"`
	Timestamp  time.Time `json:"timestamp"`
	Tags       []string  `json:"tags"`
	Engagement int       `json:"engagement,omitempty"`
}

// FeedCmd shows posts from publications the user follows.
func FeedCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags
	limit := 10
	scoutOutput := false
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--limit":
			if i+1 < len(args) {
				i++
				n := parseIntArg(args[i])
				if n > 0 {
					limit = n
				}
			}
		case "--scout":
			scoutOutput = true
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	items, err := client.GetFeed(limit)
	if err != nil {
		return fmt.Errorf("fetching feed: %w", err)
	}

	if scoutOutput {
		out := feedItemsToScout(items)
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	if jsonOutput {
		type jsonItem struct {
			ID             int            `json:"id"`
			Title          string         `json:"title"`
			Subtitle       string         `json:"subtitle,omitempty"`
			Author         string         `json:"author"`
			Publication    string         `json:"publication"`
			Date           string         `json:"date"`
			URL            string         `json:"url"`
			Comments       int            `json:"comments"`
			Reactions      map[string]int `json:"reactions,omitempty"`
			TotalReactions int            `json:"total_reactions"`
		}

		out := make([]jsonItem, len(items))
		for i, item := range items {
			out[i] = jsonItem{
				ID:             item.ID,
				Title:          item.Title,
				Subtitle:       item.Subtitle,
				Author:         item.AuthorName(),
				Publication:    item.PublicationName,
				Date:           item.PostDate,
				URL:            item.CanonicalURL,
				Comments:       item.CommentCount,
				Reactions:      item.Reactions,
				TotalReactions: item.TotalReactions(),
			}
		}

		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	if len(items) == 0 {
		fmt.Println("No feed items found. Follow some publications on substack.com first.")
		return nil
	}

	for _, item := range items {
		author := item.AuthorName()
		pub := item.PublicationName
		date := formatRelativeDate(item.PostDate)
		url := item.CanonicalURL

		fmt.Println("\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501")
		if pub != "" && author != "" {
			fmt.Printf("%s \u00b7 %s\n", pub, author)
		} else if pub != "" {
			fmt.Println(pub)
		} else if author != "" {
			fmt.Println(author)
		}
		fmt.Println(item.Title)
		fmt.Printf("%s \u00b7 \U0001f4ac %d \u00b7 \u2764\ufe0f %d\n", date, item.CommentCount, item.TotalReactions())
		if url != "" {
			fmt.Println(url)
		}
	}
	fmt.Println("\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501\u2501")

	return nil
}

// formatRelativeDate returns a short human-readable date.
func formatRelativeDate(date string) string {
	if date == "" {
		return "unknown date"
	}
	if len(date) > 10 {
		return date[:10]
	}
	return date
}

// feedItemsToScout converts FeedItems to ScoutItems for scout pipeline integration.
// Outputs the IntelItem-compatible format used by the scout plugin.
func feedItemsToScout(items []api.FeedItem) []ScoutItem {
	out := make([]ScoutItem, 0, len(items))
	for _, item := range items {
		ts := parsePostDate(item.PostDate)
		content := item.Subtitle
		if content == "" {
			content = item.Title
		}
		tags := []string{"substack"}
		if item.PublicationName != "" {
			tags = append(tags, item.PublicationName)
		}
		out = append(out, ScoutItem{
			ID:         fmt.Sprintf("%x", md5.Sum([]byte(item.CanonicalURL)))[:16],
			Title:      item.Title,
			Content:    content,
			SourceURL:  item.CanonicalURL,
			Author:     item.AuthorName(),
			Timestamp:  ts,
			Tags:       tags,
			Engagement: item.CommentCount + item.TotalReactions(),
		})
	}
	return out
}

// parsePostDate parses a Substack post date string into a time.Time.
func parsePostDate(s string) time.Time {
	if s == "" {
		return time.Time{}
	}
	for _, layout := range []string{time.RFC3339, "2006-01-02T15:04:05", "2006-01-02"} {
		if t, err := time.Parse(layout, s); err == nil {
			return t
		}
	}
	return time.Time{}
}
