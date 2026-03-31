package gather

import (
	"context"
	"encoding/xml"
	"fmt"
	"io"
	"net/http"
	"strings"
	"time"
)

// RSS 2.0 XML structures
type rssFeed struct {
	XMLName xml.Name   `xml:"rss"`
	Channel rssChannel `xml:"channel"`
}

type rssChannel struct {
	Title string    `xml:"title"`
	Items []rssItem `xml:"item"`
}

type rssItem struct {
	Title       string   `xml:"title"`
	Link        string   `xml:"link"`
	Description string   `xml:"description"`
	Author      string   `xml:"author"`
	PubDate     string   `xml:"pubDate"`
	Categories  []string `xml:"category"`
	GUID        string   `xml:"guid"`
}

// Atom 1.0 XML structures
type atomFeed struct {
	XMLName xml.Name    `xml:"feed"`
	Title   string      `xml:"title"`
	Entries []atomEntry `xml:"entry"`
}

type atomEntry struct {
	Title      string         `xml:"title"`
	Links      []atomLink     `xml:"link"`
	Summary    string         `xml:"summary"`
	Content    string         `xml:"content"`
	Author     atomAuthor     `xml:"author"`
	Published  string         `xml:"published"`
	Updated    string         `xml:"updated"`
	Categories []atomCategory `xml:"category"`
	ID         string         `xml:"id"`
}

type atomLink struct {
	Href string `xml:"href,attr"`
	Rel  string `xml:"rel,attr"`
}

type atomAuthor struct {
	Name string `xml:"name"`
}

type atomCategory struct {
	Term string `xml:"term,attr"`
}

// Date formats to try when parsing feed dates.
var dateFormats = []string{
	time.RFC1123Z,                    // "Mon, 02 Jan 2006 15:04:05 -0700"
	time.RFC1123,                     // "Mon, 02 Jan 2006 15:04:05 MST"
	time.RFC3339,                     // "2006-01-02T15:04:05Z07:00"
	"2006-01-02T15:04:05Z",           // ISO 8601 UTC
	"2006-01-02T15:04:05-07:00",      // ISO 8601 with offset
	"Mon, 2 Jan 2006 15:04:05 -0700", // RFC1123Z with single-digit day
	"Mon, 2 Jan 2006 15:04:05 MST",   // RFC1123 with single-digit day
	"2006-01-02",                     // Date only
}

// parseDate tries multiple date formats and returns the parsed time.
// Falls back to time.Now().UTC() if no format matches.
func parseDate(s string) time.Time {
	s = strings.TrimSpace(s)
	if s == "" {
		return time.Now().UTC()
	}
	for _, format := range dateFormats {
		if t, err := time.Parse(format, s); err == nil {
			return t
		}
	}
	return time.Now().UTC()
}

// parseFeed tries to parse data as RSS 2.0, then Atom 1.0.
func parseFeed(data []byte) ([]IntelItem, error) {
	// Try RSS 2.0 first
	var rss rssFeed
	if err := xml.Unmarshal(data, &rss); err == nil && len(rss.Channel.Items) > 0 {
		return rssItemsToIntel(rss.Channel.Items), nil
	}

	// Try Atom
	var atom atomFeed
	if err := xml.Unmarshal(data, &atom); err == nil && len(atom.Entries) > 0 {
		return atomEntriesToIntel(atom.Entries), nil
	}

	return nil, fmt.Errorf("unrecognized feed format")
}

func rssItemsToIntel(items []rssItem) []IntelItem {
	var result []IntelItem
	for _, item := range items {
		result = append(result, rssItemToIntel(item))
	}
	return result
}

func rssItemToIntel(item rssItem) IntelItem {
	title := cleanText(item.Title)
	link := strings.TrimSpace(item.Link)
	content := cleanText(item.Description)

	id := link
	if item.GUID != "" {
		id = item.GUID
	}

	return IntelItem{
		ID:        generateID(id),
		Title:     title,
		Content:   content,
		SourceURL: link,
		Author:    cleanText(item.Author),
		Timestamp: parseDate(item.PubDate),
		Tags:      item.Categories,
	}
}

func atomEntriesToIntel(entries []atomEntry) []IntelItem {
	var result []IntelItem
	for _, entry := range entries {
		result = append(result, atomEntryToIntel(entry))
	}
	return result
}

func atomEntryToIntel(entry atomEntry) IntelItem {
	title := cleanText(entry.Title)

	// Get link — prefer alternate, fall back to first
	var link string
	for _, l := range entry.Links {
		if l.Rel == "alternate" || l.Rel == "" {
			link = l.Href
			break
		}
	}
	if link == "" && len(entry.Links) > 0 {
		link = entry.Links[0].Href
	}

	// Prefer content over summary
	content := entry.Content
	if content == "" {
		content = entry.Summary
	}
	content = cleanText(content)

	// Truncate long content
	if len(content) > 500 {
		content = content[:500] + "..."
	}

	// Parse timestamp — prefer published, fall back to updated
	ts := entry.Published
	if ts == "" {
		ts = entry.Updated
	}

	// Collect categories
	var tags []string
	for _, c := range entry.Categories {
		if c.Term != "" {
			tags = append(tags, c.Term)
		}
	}

	id := entry.ID
	if id == "" {
		id = link
	}

	return IntelItem{
		ID:        generateID(id),
		Title:     title,
		Content:   content,
		SourceURL: link,
		Author:    entry.Author.Name,
		Timestamp: parseDate(ts),
		Tags:      tags,
	}
}

// RSSGatherer fetches and parses RSS/Atom feeds.
type RSSGatherer struct {
	FeedURLs []string
	Client   *http.Client
}

// NewRSSGatherer creates a new RSS gatherer with the given feed URLs.
func NewRSSGatherer(feedURLs []string) *RSSGatherer {
	return &RSSGatherer{FeedURLs: feedURLs, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *RSSGatherer) Name() string { return "rss" }

// Gather fetches all configured feeds and returns intel items.
func (g *RSSGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	return collectFromList(g.FeedURLs, "feed", func(feedURL string) ([]IntelItem, error) {
		return g.fetchFeed(ctx, feedURL)
	}, searchTerms)
}

func (g *RSSGatherer) fetchFeed(ctx context.Context, feedURL string) ([]IntelItem, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch %s: %w", feedURL, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from %s", resp.StatusCode, feedURL)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	return parseFeed(body)
}
