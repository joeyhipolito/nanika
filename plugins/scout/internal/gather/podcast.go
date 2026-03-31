package gather

import (
	"context"
	"encoding/xml"
	"fmt"
	"io"
	"net/http"
	"strings"
)

// Podcast RSS 2.0 XML structures with iTunes namespace extensions.
type podcastFeed struct {
	XMLName xml.Name       `xml:"rss"`
	Channel podcastChannel `xml:"channel"`
}

type podcastChannel struct {
	Title  string          `xml:"title"`
	Items  []podcastItem   `xml:"item"`
}

type podcastItem struct {
	Title       string          `xml:"title"`
	Link        string          `xml:"link"`
	Description string          `xml:"description"`
	Author      string          `xml:"author"`
	PubDate     string          `xml:"pubDate"`
	GUID        string          `xml:"guid"`
	Enclosure   podcastEnclosure `xml:"enclosure"`
	// iTunes namespace extensions
	ItunesTitle   string `xml:"http://www.itunes.com/dtds/podcast-1.0.dtd title"`
	ItunesSummary string `xml:"http://www.itunes.com/dtds/podcast-1.0.dtd summary"`
	ItunesAuthor  string `xml:"http://www.itunes.com/dtds/podcast-1.0.dtd author"`
}

type podcastEnclosure struct {
	URL string `xml:"url,attr"`
}

// PodcastGatherer fetches episode metadata from podcast RSS feeds.
type PodcastGatherer struct {
	FeedURLs []string
	Client   *http.Client
}

// NewPodcastGatherer creates a new podcast gatherer for the given feed URLs.
func NewPodcastGatherer(feedURLs []string) *PodcastGatherer {
	return &PodcastGatherer{FeedURLs: feedURLs, Client: newHTTPClient()}
}

// Name returns the canonical source identifier.
func (g *PodcastGatherer) Name() string { return "podcast" }

// Gather fetches all configured podcast feeds and returns intel items.
func (g *PodcastGatherer) Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error) {
	return collectFromList(g.FeedURLs, "podcast", func(feedURL string) ([]IntelItem, error) {
		return g.fetchFeed(ctx, feedURL)
	}, searchTerms)
}

func (g *PodcastGatherer) fetchFeed(ctx context.Context, feedURL string) ([]IntelItem, error) {
	req, err := http.NewRequestWithContext(ctx, "GET", feedURL, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header.Set("User-Agent", userAgent)

	resp, err := g.Client.Do(req)
	if err != nil {
		return nil, fmt.Errorf("failed to fetch %s: %w", feedURL, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusNotModified {
		return nil, nil
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("HTTP %d from %s", resp.StatusCode, feedURL)
	}

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("failed to read response: %w", err)
	}

	return parsePodcastFeed(body)
}

// parsePodcastFeed parses RSS 2.0 feed bytes into IntelItems.
func parsePodcastFeed(data []byte) ([]IntelItem, error) {
	var feed podcastFeed
	if err := xml.Unmarshal(data, &feed); err != nil {
		return nil, fmt.Errorf("failed to parse podcast feed: %w", err)
	}
	if len(feed.Channel.Items) == 0 {
		return nil, nil
	}

	podcastName := cleanText(feed.Channel.Title)
	var items []IntelItem
	for _, item := range feed.Channel.Items {
		items = append(items, podcastItemToIntel(item, podcastName))
	}
	return items, nil
}

// podcastItemToIntel converts a podcast RSS item to an IntelItem.
func podcastItemToIntel(item podcastItem, podcastName string) IntelItem {
	// Prefer iTunes extensions, fall back to standard RSS fields
	title := item.ItunesTitle
	if title == "" {
		title = item.Title
	}

	content := item.ItunesSummary
	if content == "" {
		content = item.Description
	}

	author := item.ItunesAuthor
	if author == "" {
		author = item.Author
	}
	if author == "" {
		author = podcastName
	}

	// Prefer <link>, fall back to enclosure URL
	link := strings.TrimSpace(item.Link)
	if link == "" {
		link = strings.TrimSpace(item.Enclosure.URL)
	}

	// Stable ID: prefer GUID, fall back to link
	idSource := item.GUID
	if idSource == "" {
		idSource = link
	}

	content = cleanText(content)
	if len(content) > 500 {
		content = content[:500] + "..."
	}

	return IntelItem{
		ID:        generateID(idSource),
		Title:     cleanText(title),
		Content:   content,
		SourceURL: link,
		Author:    cleanText(author),
		Timestamp: parseDate(item.PubDate),
		Tags:      []string{"podcast", podcastName},
	}
}
