package gather

import (
	"context"
	"encoding/xml"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"
)

// Medium RSS uses content:encoded for full HTML and dc:creator for author.
// We define Medium-specific RSS structs to capture these fields.

type mediumRSSFeed struct {
	XMLName xml.Name         `xml:"rss"`
	Channel mediumRSSChannel `xml:"channel"`
}

type mediumRSSChannel struct {
	Title string          `xml:"title"`
	Items []mediumRSSItem `xml:"item"`
}

type mediumRSSItem struct {
	Title          string   `xml:"title"`
	Link           string   `xml:"link"`
	Description    string   `xml:"description"`
	ContentEncoded string   `xml:"http://purl.org/rss/1.0/modules/content/ encoded"`
	Creator        string   `xml:"http://purl.org/dc/elements/1.1/ creator"`
	PubDate        string   `xml:"pubDate"`
	Categories     []string `xml:"category"`
	GUID           string   `xml:"guid"`
}

// MediumGatherer fetches posts from Medium tag and publication RSS feeds.
type MediumGatherer struct {
	Tags         []string
	Publications []string
	Client       *http.Client
}

// NewMediumGatherer creates a new Medium gatherer with the given tags and publications.
func NewMediumGatherer(tags, publications []string) *MediumGatherer {
	return &MediumGatherer{Tags: tags, Publications: publications, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *MediumGatherer) Name() string { return "medium" }

// Gather fetches posts from configured Medium tags and publications.
func (g *MediumGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	seen := make(map[string]bool)
	var all []IntelItem

	for _, tag := range g.Tags {
		items, err := g.fetchMediumFeed(ctx, fmt.Sprintf("https://medium.com/feed/tag/%s", tag), tag)
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: medium/tag/%s: %v\n", tag, err)
			continue
		}
		all = appendUnique(seen, all, items)
	}

	for _, pub := range g.Publications {
		items, err := g.fetchMediumFeed(ctx, fmt.Sprintf("https://medium.com/feed/%s", pub), pub)
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: medium/%s: %v\n", pub, err)
			continue
		}
		all = appendUnique(seen, all, items)
	}

	return filterByTerms(all, searchTerms), nil
}

// fetchMediumFeed fetches a Medium RSS feed with backoff on 429.
func (g *MediumGatherer) fetchMediumFeed(ctx context.Context, feedURL, label string) ([]IntelItem, error) {
	backoffs := []time.Duration{0, 1 * time.Second, 2 * time.Second, 4 * time.Second}

	for attempt, backoff := range backoffs {
		if backoff > 0 {
			select {
			case <-time.After(backoff):
			case <-ctx.Done():
				return nil, ctx.Err()
			}
		}

		req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
		if err != nil {
			return nil, fmt.Errorf("failed to create request: %w", err)
		}
		req.Header.Set("User-Agent", userAgent)

		resp, err := g.Client.Do(req)
		if err != nil {
			return nil, fmt.Errorf("failed to fetch %s: %w", feedURL, err)
		}

		if resp.StatusCode == http.StatusTooManyRequests {
			resp.Body.Close()
			if attempt < len(backoffs)-1 {
				fmt.Fprintf(os.Stderr, "    Warning: medium/%s: rate limited, retrying...\n", label)
				continue
			}
			return nil, fmt.Errorf("medium/%s: rate limited after %d attempts", label, len(backoffs))
		}

		if resp.StatusCode != http.StatusOK {
			resp.Body.Close()
			return nil, fmt.Errorf("HTTP %d from %s", resp.StatusCode, feedURL)
		}

		body, err := io.ReadAll(resp.Body)
		resp.Body.Close()
		if err != nil {
			return nil, fmt.Errorf("failed to read response: %w", err)
		}

		return g.parseMediumFeed(body, label)
	}

	return nil, fmt.Errorf("medium/%s: exhausted retries", label)
}

// parseMediumFeed parses Medium-specific RSS with content:encoded and dc:creator.
func (g *MediumGatherer) parseMediumFeed(data []byte, label string) ([]IntelItem, error) {
	var feed mediumRSSFeed
	if err := xml.Unmarshal(data, &feed); err != nil {
		// Fall back to generic parseFeed if Medium-specific parsing fails
		items, fallbackErr := parseFeed(data)
		if fallbackErr != nil {
			return nil, fmt.Errorf("failed to parse medium feed: %w", err)
		}
		for i := range items {
			items[i].Tags = append(items[i].Tags, "medium", label)
		}
		return items, nil
	}

	var items []IntelItem
	for _, item := range feed.Channel.Items {
		// Prefer content:encoded over description for richer content
		content := item.ContentEncoded
		if content == "" {
			content = item.Description
		}
		content = cleanText(content)
		if len(content) > 500 {
			content = content[:500] + "..."
		}

		author := strings.TrimSpace(item.Creator)

		link := strings.TrimSpace(item.Link)
		id := link
		if item.GUID != "" {
			id = item.GUID
		}

		tags := append(item.Categories, "medium", label)

		items = append(items, IntelItem{
			ID:        generateID(id),
			Title:     cleanText(item.Title),
			Content:   content,
			SourceURL: link,
			Author:    author,
			Timestamp: parseDate(item.PubDate),
			Tags:      tags,
		})
	}

	return items, nil
}
