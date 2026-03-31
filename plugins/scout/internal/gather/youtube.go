package gather

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"
)

// YouTubeGatherer fetches videos from YouTube channel Atom RSS feeds,
// with a Google News RSS fallback for search-term discovery.
type YouTubeGatherer struct {
	Channels []string
	Client   *http.Client
	news     *GoogleNewsGatherer
}

// NewYouTubeGatherer creates a new YouTube gatherer for the given channel IDs.
func NewYouTubeGatherer(channels []string) *YouTubeGatherer {
	client := newHTTPClient()
	return &YouTubeGatherer{
		Channels: channels,
		Client:   client,
		news:     &GoogleNewsGatherer{Client: client},
	}
}

// Name returns the canonical source identifier.
func (g *YouTubeGatherer) Name() string { return "youtube" }

// Gather fetches videos from configured YouTube channel RSS feeds, then uses
// Google News RSS as a fallback to find YouTube content matching search terms.
// Applies 2s rate limiting between channel fetches.
func (g *YouTubeGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	seen := make(map[string]bool)
	var all []IntelItem

	for i, channelID := range g.Channels {
		if i > 0 {
			select {
			case <-time.After(2 * time.Second):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		items, err := g.fetchChannel(ctx, channelID)
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: youtube/channel/%s: %v\n", channelID, err)
			continue
		}
		for j := range items {
			items[j].Tags = append(items[j].Tags, "youtube")
		}
		all = appendUnique(seen, all, items)
	}

	if len(searchTerms) > 0 {
		if len(g.Channels) > 0 {
			select {
			case <-time.After(2 * time.Second):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}
		items, err := g.news.fetchGoogleNews(ctx, "youtube "+strings.Join(searchTerms, " OR "))
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: youtube/googlenews: %v\n", err)
		} else {
			for j := range items {
				items[j].Tags = append(items[j].Tags, "youtube")
			}
			all = appendUnique(seen, all, items)
		}
	}

	if len(searchTerms) == 0 {
		return all, nil
	}
	return filterByTerms(all, searchTerms), nil
}

// fetchChannel fetches the Atom RSS feed for a YouTube channel ID.
func (g *YouTubeGatherer) fetchChannel(ctx context.Context, channelID string) ([]IntelItem, error) {
	feedURL := fmt.Sprintf("https://www.youtube.com/feeds/videos.xml?channel_id=%s", channelID)

	req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch channel feed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from YouTube RSS", resp.StatusCode)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	return parseFeed(body)
}
