package gather

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"time"
)

// LinkedInBrowserGatherer fetches the LinkedIn feed via the linkedin CLI.
// Requires the linkedin CLI to be installed and configured with Chrome CDP.
type LinkedInBrowserGatherer struct{}

// NewLinkedInBrowserGatherer creates a new LinkedIn feed gatherer.
func NewLinkedInBrowserGatherer() *LinkedInBrowserGatherer {
	return &LinkedInBrowserGatherer{}
}

func (g *LinkedInBrowserGatherer) Name() string { return "linkedin-browser" }

// linkedInFeedItem mirrors the JSON shape emitted by `linkedin feed --json`.
type linkedInFeedItem struct {
	ActivityURN   string `json:"activity_urn"`
	AuthorName    string `json:"author_name"`
	Text          string `json:"text"`
	ReactionCount int    `json:"reaction_count"`
	CommentCount  int    `json:"comment_count"`
	RepostCount   int    `json:"repost_count"`
}

// Gather runs `linkedin feed --json` and maps results to IntelItems.
// Returns nil items (not an error) if the linkedin CLI is not installed.
func (g *LinkedInBrowserGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	path, err := exec.LookPath("linkedin")
	if err != nil {
		fmt.Fprintf(os.Stderr, "    Warning: linkedin-browser: linkedin CLI not installed, skipping\n")
		return nil, nil
	}

	cmd := exec.CommandContext(ctx, path, "feed", "--json")
	var stdout, stderr bytes.Buffer
	cmd.Stdout = &stdout
	cmd.Stderr = &stderr

	if err := cmd.Run(); err != nil {
		return nil, fmt.Errorf("linkedin-browser: running linkedin feed --json: %w (stderr: %s)", err, stderr.String())
	}

	var feedItems []linkedInFeedItem
	if err := json.Unmarshal(stdout.Bytes(), &feedItems); err != nil {
		return nil, fmt.Errorf("linkedin-browser: parsing linkedin feed output: %w", err)
	}

	now := time.Now().UTC()
	seen := make(map[string]bool)
	var items []IntelItem

	for _, fi := range feedItems {
		postURL := ""
		if fi.ActivityURN != "" {
			postURL = "https://www.linkedin.com/feed/update/" + fi.ActivityURN
		}

		id := generateID(fi.ActivityURN)
		if id == "" {
			id = generateID(fi.Text[:min(len(fi.Text), 50)])
		}
		if seen[id] {
			continue
		}
		seen[id] = true

		title := fi.Text
		if len(title) > 100 {
			title = title[:100] + "..."
		}

		items = append(items, IntelItem{
			ID:         id,
			Title:      cleanText(title),
			Content:    cleanText(fi.Text),
			SourceURL:  postURL,
			Author:     cleanText(fi.AuthorName),
			Timestamp:  now,
			Tags:       []string{"linkedin-browser"},
			Engagement: fi.ReactionCount + fi.CommentCount + fi.RepostCount,
		})
	}

	// Browser sources skip search term filtering
	return items, nil
}
