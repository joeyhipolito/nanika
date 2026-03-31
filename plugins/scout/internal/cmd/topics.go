package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"

	"github.com/joeyhipolito/nanika-scout/internal/config"
	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

// presets defines pre-configured topic bundles.
var presets = map[string]gather.TopicConfig{
	// --- AI presets ---
	"ai-models": {
		Name:        "ai-models",
		Description: "AI model releases and announcements from OpenAI, Anthropic, Google",
		Sources:     []string{"rss", "googlenews", "reddit", "x", "hackernews", "devto", "substack", "medium", "youtube", "arxiv", "bluesky"},
		SearchTerms: []string{"GPT", "Claude", "Gemini", "model release", "LLM", "foundation model"},
		Feeds: []string{
			"https://openai.com/blog/rss.xml",
			"https://www.technologyreview.com/feed/",
			"https://blog.google/technology/ai/rss/",
			"https://machinelearningmastery.com/feed/",
		},
		RedditSubs:   []string{"MachineLearning", "LocalLLaMA", "artificial"},
		SubstackPubs: []string{"oneusefulthing", "thealgorithmicbridge", "aisupremacy"},
		MediumTags:   []string{"artificial-intelligence", "llm", "machine-learning"},
		MediumPubs:   []string{"towards-data-science"},
		DevToTags:    []string{"ai", "llm", "machinelearning"},
		// Channel IDs: open a YouTube channel page, view source, search for "channelId"
		YouTubeChannels: []string{
			"UCH_at8rAFuYuQB6k8WeJiZA", // Andrej Karpathy
			"UCbfYPyITQ-7l4upoX8nvctg", // Two Minute Papers
			"UCYO_jab_esuFRV4b17AJtAg", // 3Blue1Brown
		},
		ArxivCategories: []string{"cs.LG", "cs.CL", "cs.AI"},
		GatherInterval:  "6h",
	},
	"ai-research": {
		Name:        "ai-research",
		Description: "AI research papers and breakthroughs from curated sources",
		Sources:     []string{"rss", "reddit", "substack", "medium", "hackernews", "devto", "arxiv", "youtube", "bluesky"},
		SearchTerms: []string{"paper", "research", "benchmark", "SOTA", "transformer", "diffusion"},
		Feeds: []string{
			"https://huggingface.co/blog/feed.xml",
			"https://bair.berkeley.edu/blog/feed.xml",
			"https://deepmind.google/blog/rss.xml",
			"https://blog.research.google/feeds/posts/default",
		},
		RedditSubs:   []string{"MachineLearning", "deeplearning", "LanguageTechnology"},
		SubstackPubs: []string{"importai", "thesequenceai", "chinaai"},
		MediumTags:   []string{"deep-learning", "nlp", "ai-research"},
		MediumPubs:   []string{"towards-ai"},
		DevToTags:    []string{"deeplearning", "ai-research"},
		YouTubeChannels: []string{
			"UCZHmQk67mSJgfCCTn7xBfew", // Yannic Kilcher
			"UCbfYPyITQ-7l4upoX8nvctg", // Two Minute Papers
		},
		ArxivCategories: []string{"cs.AI", "cs.CL", "cs.LG"},
		GatherInterval:  "6h",
	},
	"ai-tools": {
		Name:          "ai-tools",
		Description:   "Trending AI tools, frameworks, and libraries on GitHub",
		Sources:       []string{"github", "hackernews", "devto", "reddit", "substack", "medium", "youtube", "bluesky", "producthunt"},
		SearchTerms:   []string{"framework", "library", "tool", "SDK", "API", "open source AI"},
		GitHubQueries: []string{"topic:ai", "topic:llm", "topic:machine-learning", "topic:generative-ai"},
		RedditSubs:    []string{"LocalLLaMA", "MachineLearning"},
		SubstackPubs:  []string{"lsvp", "latent-space"},
		MediumTags:    []string{"developer-tools", "ai-tools"},
		MediumPubs:    []string{"better-programming"},
		DevToTags:     []string{"ai", "machinelearning", "aitools"},
		YouTubeChannels: []string{
			"UCgnrx8qi4qhmN6sBebdZenA", // Matt Wolfe (Futurepedia)
			"UCZHmQk67mSJgfCCTn7xBfew", // Yannic Kilcher
			"UCNJ1Ymd5yFuUPtn21xtRbbw", // AI Explained
		},
		GatherInterval: "6h",
	},
	"ai-industry": {
		Name:        "ai-industry",
		Description: "AI industry news, funding, and regulation",
		Sources:     []string{"rss", "reddit", "x", "hackernews", "devto", "googlenews", "substack", "medium", "youtube", "bluesky", "producthunt"},
		SearchTerms: []string{"funding", "acquisition", "startup", "regulation", "AI policy", "AI safety"},
		Feeds: []string{
			"https://techcrunch.com/category/artificial-intelligence/feed/",
			"https://www.theverge.com/rss/ai-artificial-intelligence/index.xml",
			"https://venturebeat.com/category/ai/feed/",
			"https://www.wired.com/feed/tag/artificial-intelligence/latest/rss",
		},
		RedditSubs:   []string{"artificial", "technology", "singularity"},
		SubstackPubs: []string{"platformer", "bigtechnology", "newcomer", "aisupremacy", "exponentialview"},
		MediumTags:   []string{"ai-industry", "tech-industry", "startups"},
		DevToTags:    []string{"ai", "tech-industry"},
		YouTubeChannels: []string{
			"UCSHZKyawb77ixDdsGog4iWA", // Lex Fridman
		},
		GatherInterval: "6h",
	},
	"ai-agent-skills": {
		Name:          "ai-agent-skills",
		Description:   "AI agent frameworks, autonomous systems, and multi-agent patterns",
		Sources:       []string{"github", "hackernews", "reddit", "googlenews", "bluesky", "devto"},
		SearchTerms:   []string{"AI agent", "autonomous agent", "LangChain", "LangGraph", "AutoGPT", "CrewAI", "agentic AI", "multi-agent", "agent framework"},
		GitHubQueries: []string{"topic:ai-agents stars:>50", "topic:llm-agent stars:>50", "topic:multi-agent stars:>50", "topic:agent-framework stars:>50"},
		RedditSubs:    []string{"MachineLearning", "LocalLLaMA", "artificial"},
		SubstackPubs:  []string{"latent-space", "thesequenceai"},
		DevToTags:     []string{"aiagents", "llm", "automation"},
		GatherInterval: "6h",
	},
	// --- Non-AI presets ---
	"go-development": {
		Name:        "go-development",
		Description: "Go ecosystem news, libraries, patterns, and releases",
		Sources:     []string{"googlenews", "reddit", "hackernews", "devto", "lobsters", "medium", "github", "github-trending", "rss", "youtube"},
		SearchTerms: []string{"golang", "Go programming", "Go library", "Go CLI", "Go concurrency"},
		Feeds: []string{
			"https://go.dev/blog/feed.atom",
			"https://dave.cheney.net/feed",
			"https://changelog.com/gotime/feed",
		},
		RedditSubs:              []string{"golang"},
		SubstackPubs:            []string{"gopherweekly"},
		MediumTags:              []string{"golang", "go-programming"},
		DevToTags:               []string{"go"},
		LobstersTags:            []string{"go"},
		GitHubQueries:           []string{"language:go stars:>100"},
		GitHubTrendingLanguages: []string{"go"},
		YouTubeChannels: []string{
			"UC_BzFbxG2za3bp5NRRRXJSw", // justforfunc (Francesc Campoy, ex-Go team)
			"UCx9QVEApa5BKLw9r8cnOFEA", // GopherCon (official Go conference)
			"UCVQLvAzhC6RtPJz_H8UOMOQ", // Dreams of Code (Go project builds)
		},
		GatherInterval: "6h",
	},
	"developer-tools": {
		Name:           "developer-tools",
		Description:    "CLI tools, developer experience, and productivity tooling",
		Sources:        []string{"googlenews", "hackernews", "github", "github-trending", "devto", "lobsters", "reddit", "producthunt"},
		SearchTerms:    []string{"developer tools", "CLI tool", "developer experience", "devtools", "terminal"},
		RedditSubs:     []string{"commandline", "devops"},
		DevToTags:      []string{"devtools", "cli", "productivity"},
		LobstersTags:   []string{"programming"},
		GitHubQueries:  []string{"topic:cli stars:>50"},
		GatherInterval: "6h",
	},
	"content-creation": {
		Name:        "content-creation",
		Description: "Technical writing, developer blogging, and newsletter strategy",
		Sources:     []string{"googlenews", "medium", "substack", "devto", "reddit", "youtube"},
		SearchTerms: []string{"technical writing", "developer blog", "newsletter growth", "content strategy tech"},
		RedditSubs:  []string{"technicalwriting", "Blogging"},
		MediumTags:  []string{"technical-writing", "blogging"},
		MediumPubs:  []string{"better-programming"},
		SubstackPubs: []string{"lenny", "pragmaticengineer", "bytebytego"},
		DevToTags:   []string{"writing", "career"},
		YouTubeChannels: []string{
			"UCoOae5nYA7VqaXzerajD0lg", // Ali Abdaal
			"UCG-KntY7aVnIGXYEBQvmBAQ", // Thomas Frank
			"UCkm2tAjNRTLtjkPHH21VT5Q", // Jay Clouse (Creator Science)
		},
		GatherInterval: "6h",
	},
	"software-architecture": {
		Name:           "software-architecture",
		Description:    "System design, distributed systems, and architecture patterns",
		Sources:        []string{"googlenews", "hackernews", "medium", "devto", "lobsters", "reddit"},
		SearchTerms:    []string{"software architecture", "system design", "distributed systems", "microservices patterns"},
		MediumTags:     []string{"software-architecture", "system-design"},
		MediumPubs:     []string{"better-programming"},
		RedditSubs:     []string{"softwarearchitecture", "ExperiencedDevs"},
		DevToTags:      []string{"architecture", "distributedsystems"},
		LobstersTags:   []string{"distributed"},
		GatherInterval: "6h",
	},
	"open-source": {
		Name:           "open-source",
		Description:    "Open source projects, maintainership, funding, and licensing",
		Sources:        []string{"hackernews", "github", "reddit", "lobsters", "devto"},
		SearchTerms:    []string{"open source", "OSS maintainer", "open source funding", "FOSS"},
		RedditSubs:     []string{"opensource"},
		GitHubQueries:  []string{"stars:>1000"},
		DevToTags:      []string{"opensource"},
		LobstersTags:   []string{"programming"},
		GatherInterval: "6h",
	},
}

// presetOrder defines the installation order for ai-all.
var presetOrder = []string{"ai-models", "ai-research", "ai-tools", "ai-industry", "ai-agent-skills"}

// nonAIPresetOrder defines the installation order for dev-all.
var nonAIPresetOrder = []string{"go-development", "developer-tools", "content-creation", "software-architecture", "open-source"}

// TopicsCmd lists all configured topics from ~/.scout/topics/*.json.
func TopicsCmd(args []string, jsonOutput bool) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	// Handle subcommands
	if len(args) > 0 {
		switch args[0] {
		case "add":
			return topicsAddCmd(args[1:])
		case "remove":
			return topicsRemoveCmd(args[1:])
		case "preset":
			return topicsPresetCmd(args[1:])
		}
	}

	// List topics
	topics, err := loadAllTopics()
	if err != nil {
		return err
	}

	if len(topics) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No topics configured.")
		fmt.Println()
		fmt.Println("Add a topic:")
		fmt.Println("  scout topics add \"my-topic\" --sources \"rss,web\" --feeds \"https://example.com/feed.xml\"")
		fmt.Println("  scout topics preset ai-all")
		return nil
	}

	if jsonOutput {
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(topics)
	}

	fmt.Printf("Topics (%d)\n", len(topics))
	fmt.Println(strings.Repeat("-", 40))
	for _, t := range topics {
		sources := strings.Join(t.Sources, ", ")
		termCount := len(t.SearchTerms)
		feedCount := len(t.Feeds)
		fmt.Printf("  %-20s  sources: %-15s  terms: %d  feeds: %d\n", t.Name, sources, termCount, feedCount)
		if t.Description != "" {
			fmt.Printf("  %-20s  %s\n", "", t.Description)
		}
	}
	return nil
}

// topicsAddCmd creates a new topic configuration file.
func topicsAddCmd(args []string) error {
	for _, arg := range args {
		if arg == "--help" || arg == "-h" {
			fmt.Print(`Usage: scout topics add "<name>" [options]

Options:
  --sources <list>            Comma-separated sources (default: rss,web)
  --terms <list>              Comma-separated search terms
  --feeds <list>              Comma-separated RSS feed URLs
  --github-queries <list>     GitHub search queries
  --reddit-subs <list>        Subreddit names
  --substack-pubs <list>      Substack publication slugs
  --medium-tags <list>        Medium tags
  --medium-pubs <list>        Medium publication slugs
  --devto-tags <list>         dev.to tags
  --lobsters-tags <list>      Lobste.rs tags
  --youtube-channels <list>   YouTube channel IDs
  --arxiv-categories <list>            arXiv category codes
  --github-trending-languages <list>   GitHub trending language filter (e.g. go,python)
  --podcast-feeds <list>               Comma-separated podcast RSS feed URLs
  --description <text>                 Topic description
  --help, -h                           Show this help
`)
			return nil
		}
	}

	if len(args) == 0 {
		return fmt.Errorf("topic name required\n\nUsage: scout topics add \"name\" [--sources \"rss,web\"]")
	}

	name := args[0]

	// Parse flags
	sources := []string{"rss", "googlenews"}
	var searchTerms []string
	var feeds []string
	var githubQueries []string
	var redditSubs []string
	var substackPubs []string
	var mediumTags []string
	var mediumPubs []string
	var devtoTags []string
	var lobstersTags []string
	var youtubeChannels []string
	var arxivCategories []string
	var githubTrendingLanguages []string
	var podcastFeeds []string
	description := ""

	for i := 1; i < len(args); i++ {
		switch args[i] {
		case "--sources":
			if i+1 >= len(args) {
				return fmt.Errorf("--sources requires a comma-separated list (e.g. rss,googlenews,github)")
			}
			i++
			sources = strings.Split(args[i], ",")
			for j := range sources {
				sources[j] = strings.TrimSpace(sources[j])
			}
		case "--terms":
			if i+1 >= len(args) {
				return fmt.Errorf("--terms requires a comma-separated list of search terms")
			}
			i++
			searchTerms = strings.Split(args[i], ",")
			for j := range searchTerms {
				searchTerms[j] = strings.TrimSpace(searchTerms[j])
			}
		case "--feeds":
			if i+1 >= len(args) {
				return fmt.Errorf("--feeds requires a comma-separated list of RSS/Atom URLs")
			}
			i++
			feeds = strings.Split(args[i], ",")
			for j := range feeds {
				feeds[j] = strings.TrimSpace(feeds[j])
			}
		case "--github-queries":
			if i+1 >= len(args) {
				return fmt.Errorf("--github-queries requires a comma-separated list of GitHub search queries")
			}
			i++
			githubQueries = strings.Split(args[i], ",")
			for j := range githubQueries {
				githubQueries[j] = strings.TrimSpace(githubQueries[j])
			}
		case "--reddit-subs":
			if i+1 >= len(args) {
				return fmt.Errorf("--reddit-subs requires a comma-separated list of subreddit names")
			}
			i++
			redditSubs = strings.Split(args[i], ",")
			for j := range redditSubs {
				redditSubs[j] = strings.TrimSpace(redditSubs[j])
			}
		case "--substack-pubs":
			if i+1 >= len(args) {
				return fmt.Errorf("--substack-pubs requires a comma-separated list of Substack publication slugs")
			}
			i++
			substackPubs = strings.Split(args[i], ",")
			for j := range substackPubs {
				substackPubs[j] = strings.TrimSpace(substackPubs[j])
			}
		case "--medium-tags":
			if i+1 >= len(args) {
				return fmt.Errorf("--medium-tags requires a comma-separated list of Medium tags")
			}
			i++
			mediumTags = strings.Split(args[i], ",")
			for j := range mediumTags {
				mediumTags[j] = strings.TrimSpace(mediumTags[j])
			}
		case "--medium-pubs":
			if i+1 >= len(args) {
				return fmt.Errorf("--medium-pubs requires a comma-separated list of Medium publication slugs")
			}
			i++
			mediumPubs = strings.Split(args[i], ",")
			for j := range mediumPubs {
				mediumPubs[j] = strings.TrimSpace(mediumPubs[j])
			}
		case "--devto-tags":
			if i+1 >= len(args) {
				return fmt.Errorf("--devto-tags requires a comma-separated list of dev.to tags")
			}
			i++
			devtoTags = strings.Split(args[i], ",")
			for j := range devtoTags {
				devtoTags[j] = strings.TrimSpace(devtoTags[j])
			}
		case "--lobsters-tags":
			if i+1 >= len(args) {
				return fmt.Errorf("--lobsters-tags requires a comma-separated list of Lobste.rs tags")
			}
			i++
			lobstersTags = strings.Split(args[i], ",")
			for j := range lobstersTags {
				lobstersTags[j] = strings.TrimSpace(lobstersTags[j])
			}
		case "--youtube-channels":
			if i+1 >= len(args) {
				return fmt.Errorf("--youtube-channels requires a comma-separated list of YouTube channel IDs")
			}
			i++
			youtubeChannels = strings.Split(args[i], ",")
			for j := range youtubeChannels {
				youtubeChannels[j] = strings.TrimSpace(youtubeChannels[j])
			}
		case "--arxiv-categories":
			if i+1 >= len(args) {
				return fmt.Errorf("--arxiv-categories requires a comma-separated list of arXiv category codes (e.g. cs.LG,cs.AI)")
			}
			i++
			arxivCategories = strings.Split(args[i], ",")
			for j := range arxivCategories {
				arxivCategories[j] = strings.TrimSpace(arxivCategories[j])
			}
		case "--github-trending-languages":
			if i+1 < len(args) {
				i++
				githubTrendingLanguages = strings.Split(args[i], ",")
				for j := range githubTrendingLanguages {
					githubTrendingLanguages[j] = strings.TrimSpace(githubTrendingLanguages[j])
				}
			}
		case "--podcast-feeds":
			if i+1 >= len(args) {
				return fmt.Errorf("--podcast-feeds requires a comma-separated list of podcast RSS feed URLs")
			}
			i++
			podcastFeeds = strings.Split(args[i], ",")
			for j := range podcastFeeds {
				podcastFeeds[j] = strings.TrimSpace(podcastFeeds[j])
			}
		case "--description":
			if i+1 >= len(args) {
				return fmt.Errorf("--description requires a text value")
			}
			i++
			description = args[i]
		default:
			return fmt.Errorf("unknown flag %q\n\nRun 'scout topics add --help' for usage", args[i])
		}
	}

	// Validate source names against the gatherer registry
	for _, src := range sources {
		if src == "" {
			continue
		}
		if _, ok := gather.Registry[src]; !ok {
			names := make([]string, 0, len(gather.Registry))
			for k := range gather.Registry {
				names = append(names, k)
			}
			sort.Strings(names)
			return fmt.Errorf("unknown source %q\n\nValid sources: %s", src, strings.Join(names, ", "))
		}
	}

	// Warn when rss source is requested but no feeds are provided
	for _, src := range sources {
		if src == "rss" && len(feeds) == 0 {
			fmt.Fprintf(os.Stderr, "Warning: source \"rss\" requires at least one feed URL (use --feeds \"https://example.com/feed.xml\")\n")
			fmt.Fprintf(os.Stderr, "         Gathering from \"rss\" will fail until feeds are added.\n")
			break
		}
	}

	// Warn when podcast source is requested but no feeds are provided
	for _, src := range sources {
		if src == "podcast" && len(podcastFeeds) == 0 {
			fmt.Fprintf(os.Stderr, "Warning: source \"podcast\" requires at least one feed URL (use --podcast-feeds \"https://example.com/feed.rss\")\n")
			fmt.Fprintf(os.Stderr, "         Gathering from \"podcast\" will fail until feeds are added.\n")
			break
		}
	}

	// Default search terms to the topic name
	if len(searchTerms) == 0 {
		searchTerms = []string{name}
	}

	topic := gather.TopicConfig{
		Name:            name,
		Description:     description,
		Sources:         sources,
		SearchTerms:     searchTerms,
		GatherInterval:  "6h",
		Feeds:           feeds,
		GitHubQueries:   githubQueries,
		RedditSubs:      redditSubs,
		SubstackPubs:    substackPubs,
		MediumTags:      mediumTags,
		MediumPubs:      mediumPubs,
		DevToTags:       devtoTags,
		LobstersTags:    lobstersTags,
		YouTubeChannels:         youtubeChannels,
		ArxivCategories:         arxivCategories,
		GitHubTrendingLanguages: githubTrendingLanguages,
		PodcastFeeds:            podcastFeeds,
	}

	// Check if topic already exists
	topicPath := filepath.Join(config.TopicsDir(), name+".json")
	if _, err := os.Stat(topicPath); err == nil {
		return fmt.Errorf("topic %q already exists at %s", name, topicPath)
	}

	if err := saveTopic(&topic); err != nil {
		return err
	}

	fmt.Printf("Topic %q created at %s\n", name, topicPath)
	fmt.Println()
	fmt.Println("Gather intel now:")
	fmt.Printf("  scout gather %q\n", name)
	return nil
}

// topicsRemoveCmd deletes a topic configuration file.
func topicsRemoveCmd(args []string) error {
	if len(args) == 0 {
		return fmt.Errorf("topic name required\n\nUsage: scout topics remove \"name\"")
	}

	name := args[0]
	topicPath := filepath.Join(config.TopicsDir(), name+".json")

	if _, err := os.Stat(topicPath); os.IsNotExist(err) {
		return fmt.Errorf("topic %q not found at %s", name, topicPath)
	}

	if err := os.Remove(topicPath); err != nil {
		return fmt.Errorf("failed to remove topic: %w", err)
	}

	fmt.Printf("Topic %q removed.\n", name)
	return nil
}

// topicsPresetCmd installs pre-configured topic presets.
func topicsPresetCmd(args []string) error {
	if len(args) == 0 {
		// List available presets
		fmt.Println("Available presets:")
		fmt.Println()
		fmt.Println("  AI Topics:")
		for _, name := range presetOrder {
			p := presets[name]
			fmt.Printf("    %-22s  %s\n", name, p.Description)
		}
		fmt.Println()
		fmt.Println("  Developer Topics:")
		for _, name := range nonAIPresetOrder {
			p := presets[name]
			fmt.Printf("    %-22s  %s\n", name, p.Description)
		}
		fmt.Println()
		fmt.Printf("  %-24s  Install all AI presets\n", "ai-all")
		fmt.Printf("  %-24s  Install all developer presets\n", "dev-all")
		fmt.Printf("  %-24s  Install all presets\n", "all")
		fmt.Println()
		fmt.Println("Usage: scout topics preset <name>")
		return nil
	}

	name := args[0]

	switch name {
	case "ai-all":
		return installPresetGroup("AI", presetOrder)
	case "dev-all":
		return installPresetGroup("Developer", nonAIPresetOrder)
	case "all":
		if err := installPresetGroup("AI", presetOrder); err != nil {
			return err
		}
		return installPresetGroup("Developer", nonAIPresetOrder)
	}

	preset, ok := presets[name]
	if !ok {
		return fmt.Errorf("unknown preset %q\n\nRun 'scout topics preset' to see available presets", name)
	}

	return installPreset(preset)
}

func installPreset(preset gather.TopicConfig) error {
	// Check if already exists
	topicPath := filepath.Join(config.TopicsDir(), preset.Name+".json")
	if _, err := os.Stat(topicPath); err == nil {
		fmt.Printf("  Topic %q already exists, skipping\n", preset.Name)
		return nil
	}

	if err := saveTopic(&preset); err != nil {
		return fmt.Errorf("failed to create preset %q: %w", preset.Name, err)
	}

	fmt.Printf("  Created topic %q (%d feeds, %d search terms)\n",
		preset.Name, len(preset.Feeds), len(preset.SearchTerms))
	return nil
}

func installPresetGroup(label string, order []string) error {
	fmt.Printf("Installing %s presets...\n", label)
	fmt.Println()

	for _, name := range order {
		if err := installPreset(presets[name]); err != nil {
			return err
		}
	}

	fmt.Println()
	fmt.Println("Gather intel now:")
	fmt.Println("  scout gather")
	return nil
}

// loadAllTopics reads all topic config files from ~/.scout/topics/.
func loadAllTopics() ([]gather.TopicConfig, error) {
	topicsDir := config.TopicsDir()
	entries, err := os.ReadDir(topicsDir)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, nil
		}
		return nil, fmt.Errorf("failed to read topics directory: %w", err)
	}

	var topics []gather.TopicConfig
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".json") {
			continue
		}

		topic, err := loadTopic(filepath.Join(topicsDir, entry.Name()))
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to read %s: %v\n", entry.Name(), err)
			continue
		}
		topics = append(topics, *topic)
	}

	return topics, nil
}

// loadTopic reads a single topic config file.
func loadTopic(path string) (*gather.TopicConfig, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, err
	}

	var topic gather.TopicConfig
	if err := json.Unmarshal(data, &topic); err != nil {
		return nil, fmt.Errorf("invalid JSON in %s: %w", path, err)
	}

	return &topic, nil
}

// LoadTopicByName loads a topic by name from the topics directory.
func LoadTopicByName(name string) (*gather.TopicConfig, error) {
	topicPath := filepath.Join(config.TopicsDir(), name+".json")
	return loadTopic(topicPath)
}

// saveTopic writes a topic config to ~/.scout/topics/{name}.json.
func saveTopic(topic *gather.TopicConfig) error {
	if err := config.EnsureDirs(); err != nil {
		return err
	}

	data, err := json.MarshalIndent(topic, "", "  ")
	if err != nil {
		return fmt.Errorf("failed to marshal topic: %w", err)
	}

	topicPath := filepath.Join(config.TopicsDir(), topic.Name+".json")
	if err := os.WriteFile(topicPath, data, 0600); err != nil {
		return fmt.Errorf("failed to write topic file: %w", err)
	}

	return nil
}
