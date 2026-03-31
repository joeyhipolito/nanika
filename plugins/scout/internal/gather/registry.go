package gather

import "fmt"

// FactoryFunc creates a Gatherer from a TopicConfig.
// Returns an error if the source cannot be configured (e.g. required fields missing).
type FactoryFunc func(TopicConfig) (Gatherer, error)

// Registry maps canonical source names to factory functions.
// To add a new source: implement the Gatherer interface and add an entry here.
var Registry = map[string]FactoryFunc{
	"rss": func(t TopicConfig) (Gatherer, error) {
		if len(t.Feeds) == 0 {
			return nil, fmt.Errorf("no feeds configured (use --feeds when adding topic)")
		}
		return NewRSSGatherer(t.Feeds), nil
	},
	"github":     func(t TopicConfig) (Gatherer, error) { return NewGitHubGatherer(t.GitHubQueries), nil },
	"reddit":     func(t TopicConfig) (Gatherer, error) { return NewRedditGatherer(t.RedditSubs), nil },
	"substack":   func(t TopicConfig) (Gatherer, error) { return NewSubstackGatherer(t.SubstackPubs), nil },
	"medium":     func(t TopicConfig) (Gatherer, error) { return NewMediumGatherer(t.MediumTags, t.MediumPubs), nil },
	"hackernews": func(t TopicConfig) (Gatherer, error) { return NewHackerNewsGatherer(), nil },
	"googlenews": func(t TopicConfig) (Gatherer, error) { return NewGoogleNewsGatherer(), nil },
	"devto":      func(t TopicConfig) (Gatherer, error) { return NewDevToGatherer(t.DevToTags), nil },
	"lobsters":   func(t TopicConfig) (Gatherer, error) { return NewLobstersGatherer(t.LobstersTags), nil },
	"x":          func(t TopicConfig) (Gatherer, error) { return NewXGatherer(), nil },
	"youtube":     func(t TopicConfig) (Gatherer, error) { return NewYouTubeGatherer(t.YouTubeChannels), nil },
	"youtube-cli": func(t TopicConfig) (Gatherer, error) { return NewYouTubeCLIGatherer(), nil },
	"arxiv":      func(t TopicConfig) (Gatherer, error) { return NewArxivGatherer(t.ArxivCategories), nil },
	"bluesky":      func(t TopicConfig) (Gatherer, error) { return NewBlueskyGatherer(), nil },
	"discover":     func(t TopicConfig) (Gatherer, error) { return NewDiscoverGatherer(), nil },
	"producthunt":  func(t TopicConfig) (Gatherer, error) { return NewProductHuntGatherer(), nil },
	"github-trending": func(t TopicConfig) (Gatherer, error) {
		return NewGitHubTrendingGatherer(t.GitHubTrendingLanguages), nil
	},
	"podcast": func(t TopicConfig) (Gatherer, error) {
		if len(t.PodcastFeeds) == 0 {
			return nil, fmt.Errorf("no podcast feeds configured (use --podcast-feeds when adding topic)")
		}
		return NewPodcastGatherer(t.PodcastFeeds), nil
	},

	// Browser-based sources — require Chrome on localhost:9222 with --remote-debugging-port=9222.
	// Each gatherer skips gracefully (warning to stderr, no error) when Chrome is unavailable.
	"google-browser":   func(t TopicConfig) (Gatherer, error) { return NewGoogleBrowserGatherer(), nil },
	"x-browser":        func(t TopicConfig) (Gatherer, error) { return NewXBrowserGatherer(), nil },
	"linkedin-browser": func(t TopicConfig) (Gatherer, error) { return NewLinkedInBrowserGatherer(), nil },
	"substack-browser": func(t TopicConfig) (Gatherer, error) { return NewSubstackBrowserGatherer(), nil },
}
