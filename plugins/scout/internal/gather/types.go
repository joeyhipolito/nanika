// Package gather defines types for intelligence gathering and storage.
package gather

import (
	"time"
)

// DefaultMaxItemsPerSource is the default cap on items returned per source per topic.
const DefaultMaxItemsPerSource = 50

// TopicConfig represents a topic configuration stored in ~/.scout/topics/{name}.json.
type TopicConfig struct {
	Name              string   `json:"name"`
	Description       string   `json:"description"`
	Sources           []string `json:"sources"`
	SearchTerms       []string `json:"search_terms"`
	GatherInterval    string   `json:"gather_interval"`
	MaxItemsPerSource int      `json:"max_items_per_source,omitempty"`
	Feeds             []string `json:"feeds,omitempty"`
	GitHubQueries     []string `json:"github_queries,omitempty"`
	RedditSubs        []string `json:"reddit_subs,omitempty"`
	SubstackPubs      []string `json:"substack_pubs,omitempty"`
	MediumTags        []string `json:"medium_tags,omitempty"`
	MediumPubs        []string `json:"medium_pubs,omitempty"`
	DevToTags         []string `json:"devto_tags,omitempty"`
	LobstersTags      []string `json:"lobsters_tags,omitempty"`
	YouTubeChannels   []string `json:"youtube_channels,omitempty"`
	ArxivCategories         []string `json:"arxiv_categories,omitempty"`
	GitHubTrendingLanguages []string `json:"github_trending_languages,omitempty"`
	PodcastFeeds            []string `json:"podcast_feeds,omitempty"`
}

// EffectiveMaxItems returns MaxItemsPerSource if set, otherwise DefaultMaxItemsPerSource.
func (tc TopicConfig) EffectiveMaxItems() int {
	if tc.MaxItemsPerSource > 0 {
		return tc.MaxItemsPerSource
	}
	return DefaultMaxItemsPerSource
}

// IntelItem represents a single gathered intelligence item.
type IntelItem struct {
	ID         string    `json:"id"`
	Title      string    `json:"title"`
	Content    string    `json:"content"`
	SourceURL  string    `json:"source_url"`
	Author     string    `json:"author"`
	Timestamp  time.Time `json:"timestamp"`
	Tags       []string  `json:"tags"`
	Score      float64   `json:"score,omitempty"`
	Engagement int       `json:"engagement,omitempty"`
}

// IntelFile represents a collection of intel items gathered in one session.
type IntelFile struct {
	Topic      string      `json:"topic"`
	GatheredAt time.Time   `json:"gathered_at"`
	Source     string      `json:"source"`
	Items      []IntelItem `json:"items"`
}
