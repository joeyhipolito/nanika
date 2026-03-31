package suggest

import (
	"math"
	"sort"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-scout/internal/gather"
)

// Analyze accepts pre-loaded intel files and returns ranked content suggestions.
// It clusters items by shared title keywords, scores each cluster on trend signal,
// engagement, recency, and cross-topic breadth, then returns up to limit results.
func Analyze(files []gather.IntelFile, now time.Time, limit int) []Suggestion {
	items := flattenAndAnnotate(files, now)
	if len(items) == 0 {
		return nil
	}

	clusters := clusterByKeywords(items)
	if len(clusters) == 0 {
		return nil
	}

	scored := scoreClusters(clusters, now)

	sort.Slice(scored, func(i, j int) bool {
		return scored[i].Score > scored[j].Score
	})

	if limit > 0 && len(scored) > limit {
		scored = scored[:limit]
	}

	return scored
}

// flattenAndAnnotate extracts all items from intel files, annotating each with
// its originating topic and pre-computed keyword set.
func flattenAndAnnotate(files []gather.IntelFile, now time.Time) []*annotatedItem {
	var items []*annotatedItem
	for _, f := range files {
		for _, item := range f.Items {
			kw := extractKeywords(item.Title)
			if len(kw) == 0 {
				continue
			}
			items = append(items, &annotatedItem{
				title:      item.Title,
				sourceURL:  item.SourceURL,
				author:     item.Author,
				score:      item.Score,
				engagement: item.Engagement,
				timestamp:  item.Timestamp.Unix(),
				topic:      f.Topic,
				source:     f.Source,
				keywords:   kw,
			})
		}
	}
	return items
}

// extractKeywords splits a title into meaningful lowercase words, stripping
// punctuation and filtering stop words and short tokens.
func extractKeywords(title string) map[string]bool {
	kw := make(map[string]bool)
	for _, w := range strings.Fields(strings.ToLower(title)) {
		w = strings.Trim(w, ".,;:!?\"'()-[]{}")
		if len(w) <= 3 || isStopWord(w) {
			continue
		}
		kw[w] = true
	}
	return kw
}

// clusterByKeywords groups items sharing 2+ title keywords.
// Builds a keyword→items index, then for every high-frequency keyword pair
// that co-occurs across items, forms a cluster. Each item joins at most one cluster.
func clusterByKeywords(items []*annotatedItem) []cluster {
	// keyword → indices into items slice
	kwIndex := make(map[string][]int)
	for i, item := range items {
		for kw := range item.keywords {
			kwIndex[kw] = append(kwIndex[kw], i)
		}
	}

	// Find keyword pairs with co-occurring items (2+ items share both keywords)
	type kwPair struct {
		a, b  string
		items []int // deduplicated item indices
	}

	// Only consider keywords appearing in 2+ items
	var frequentKW []string
	for kw, idxs := range kwIndex {
		if len(idxs) >= 2 {
			frequentKW = append(frequentKW, kw)
		}
	}
	sort.Strings(frequentKW) // deterministic ordering

	var pairs []kwPair
	for i := 0; i < len(frequentKW); i++ {
		for j := i + 1; j < len(frequentKW); j++ {
			a, b := frequentKW[i], frequentKW[j]
			// Find items containing both keywords
			shared := intersect(kwIndex[a], kwIndex[b])
			if len(shared) >= 2 {
				pairs = append(pairs, kwPair{a: a, b: b, items: shared})
			}
		}
	}

	// Sort pairs by number of shared items descending — greedily assign
	sort.Slice(pairs, func(i, j int) bool {
		return len(pairs[i].items) > len(pairs[j].items)
	})

	assigned := make(map[int]bool)
	var clusters []cluster

	for _, p := range pairs {
		var clusterItems []*annotatedItem
		for _, idx := range p.items {
			if !assigned[idx] {
				assigned[idx] = true
				clusterItems = append(clusterItems, items[idx])
			}
		}
		if len(clusterItems) >= 2 {
			clusters = append(clusters, cluster{
				keywords: []string{p.a, p.b},
				items:    clusterItems,
			})
		}
	}

	// Fallback: items not yet in any cluster — group by single high-frequency keyword
	// This catches trending items that don't share exact keyword pairs
	for kw, idxs := range kwIndex {
		if len(idxs) < 3 {
			continue
		}
		var clusterItems []*annotatedItem
		for _, idx := range idxs {
			if !assigned[idx] {
				assigned[idx] = true
				clusterItems = append(clusterItems, items[idx])
			}
		}
		if len(clusterItems) >= 3 {
			clusters = append(clusters, cluster{
				keywords: []string{kw},
				items:    clusterItems,
			})
		}
	}

	return clusters
}

// intersect returns sorted indices present in both a and b.
func intersect(a, b []int) []int {
	set := make(map[int]bool, len(a))
	for _, v := range a {
		set[v] = true
	}
	var result []int
	for _, v := range b {
		if set[v] {
			result = append(result, v)
		}
	}
	return result
}

// scoreClusters computes a 0-100 score for each cluster and builds Suggestion structs.
func scoreClusters(clusters []cluster, now time.Time) []Suggestion {
	nowUnix := now.Unix()
	suggestions := make([]Suggestion, 0, len(clusters))

	for _, c := range clusters {
		// --- Trend signal: distinct gatherer sources covering this cluster ---
		sourceSet := make(map[string]bool)
		for _, item := range c.items {
			sourceSet[item.source] = true
		}
		trendScore := math.Min(float64(len(sourceSet))/5.0, 1.0) // normalize: 5+ sources = max

		// --- Engagement: sum of item scores, log-scaled ---
		var totalScore float64
		for _, item := range c.items {
			totalScore += item.score
		}
		engScore := math.Min(math.Log1p(totalScore)/math.Log1p(500), 1.0) // 500+ total = max

		// --- Recency: average age in hours, decayed ---
		var totalAge float64
		for _, item := range c.items {
			ageHours := float64(nowUnix-item.timestamp) / 3600.0
			if ageHours < 0 {
				ageHours = 0
			}
			totalAge += ageHours
		}
		avgAgeHours := totalAge / float64(len(c.items))
		// Linear decay over 7 days (168h). Items at 0h = 1.0, at 168h = 0.0
		recencyScore := math.Max(1.0-avgAgeHours/168.0, 0.0)

		// --- Cross-topic: distinct topics represented ---
		topicSet := make(map[string]bool)
		for _, item := range c.items {
			topicSet[item.topic] = true
		}
		crossTopicScore := math.Min(float64(len(topicSet))/3.0, 1.0) // 3+ topics = max

		// Weighted combination
		raw := trendScore*25 + engScore*30 + recencyScore*25 + crossTopicScore*20
		score := int(math.Round(raw))
		if score > 100 {
			score = 100
		}

		// Build suggestion
		title := buildTitle(c.keywords, c.items)
		contentType := inferContentType(trendScore, engScore, crossTopicScore, len(c.items))

		// Collect unique topics
		topics := make([]string, 0, len(topicSet))
		for t := range topicSet {
			topics = append(topics, t)
		}
		sort.Strings(topics)

		// Build sources — top items by score
		sources := buildSources(c.items)

		suggestions = append(suggestions, Suggestion{
			Title:       title,
			Angle:       buildAngle(c.items, c.keywords),
			ContentType: contentType,
			Score:       score,
			Topics:      topics,
			Sources:     sources,
		})
	}

	return suggestions
}

// buildTitle generates a suggestion title from cluster keywords and top item.
func buildTitle(keywords []string, items []*annotatedItem) string {
	// Use the highest-scored item's title as the basis
	best := items[0]
	for _, item := range items[1:] {
		if item.score > best.score {
			best = item
		}
	}

	// Capitalize keywords for the cluster label
	parts := make([]string, len(keywords))
	for i, kw := range keywords {
		parts[i] = capitalize(kw)
	}
	label := strings.Join(parts, " ")

	// If the best title is short enough, use it directly
	if len(best.title) <= 80 {
		return best.title
	}
	return label + ": " + truncate(best.title, 70)
}

// buildAngle derives a content angle from cluster items and keywords.
func buildAngle(items []*annotatedItem, keywords []string) string {
	if len(items) < 2 {
		return ""
	}

	// Count distinct sources for angle framing
	sourceSet := make(map[string]bool)
	topicSet := make(map[string]bool)
	for _, item := range items {
		sourceSet[item.source] = true
		topicSet[item.topic] = true
	}

	parts := make([]string, len(keywords))
	for i, kw := range keywords {
		parts[i] = capitalize(kw)
	}
	label := strings.Join(parts, " ")

	if len(topicSet) > 1 {
		return "Cross-topic analysis of " + label + " trends across " + joinKeys(topicSet)
	}
	if len(sourceSet) >= 3 {
		return "Roundup of " + label + " coverage from multiple sources"
	}
	if len(items) >= 4 {
		return "Deep dive into " + label + " — " + pluralize(len(items), "source") + " analyzed"
	}
	return "Compare perspectives on " + label
}

// inferContentType picks a content type based on signal strengths.
func inferContentType(trend, engagement, crossTopic float64, itemCount int) string {
	// Thread: high trend signal (many sources covering it), fast-moving
	if trend >= 0.6 && itemCount >= 3 {
		return "thread"
	}
	// Video: visual/tutorial topics benefit from video format
	if crossTopic >= 0.5 && engagement >= 0.4 {
		return "video"
	}
	// Blog: default for deep, engaged, or cross-topic content
	return "blog"
}

// buildSources returns the top 5 sources from a cluster, sorted by score descending.
func buildSources(items []*annotatedItem) []Source {
	sorted := make([]*annotatedItem, len(items))
	copy(sorted, items)
	sort.Slice(sorted, func(i, j int) bool {
		return sorted[i].score > sorted[j].score
	})

	limit := 5
	if len(sorted) < limit {
		limit = len(sorted)
	}

	sources := make([]Source, limit)
	for i := 0; i < limit; i++ {
		item := sorted[i]
		sources[i] = Source{
			Title:  item.title,
			URL:    item.sourceURL,
			Source: domainFromURL(item.sourceURL),
			Score:  int(item.score),
		}
	}
	return sources
}

// domainFromURL extracts a short domain label from a URL.
func domainFromURL(u string) string {
	if u == "" {
		return "unknown"
	}
	if idx := strings.Index(u, "://"); idx >= 0 {
		u = u[idx+3:]
	}
	if idx := strings.Index(u, "/"); idx >= 0 {
		u = u[:idx]
	}
	return u
}

// truncate shortens s to maxLen characters, appending "..." if truncated.
func truncate(s string, maxLen int) string {
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen] + "..."
}

// joinKeys joins map keys with ", ".
func joinKeys(m map[string]bool) string {
	keys := make([]string, 0, len(m))
	for k := range m {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return strings.Join(keys, ", ")
}

// pluralize returns "N item" or "N items".
func pluralize(n int, word string) string {
	if n == 1 {
		return "1 " + word
	}
	return strings.Join([]string{itoa(n), " ", word, "s"}, "")
}

// itoa converts int to string without importing strconv.
func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	neg := false
	if n < 0 {
		neg = true
		n = -n
	}
	var buf [20]byte
	i := len(buf)
	for n > 0 {
		i--
		buf[i] = byte('0' + n%10)
		n /= 10
	}
	if neg {
		i--
		buf[i] = '-'
	}
	return string(buf[i:])
}

// capitalize uppercases the first letter of a string.
func capitalize(s string) string {
	if s == "" {
		return s
	}
	if s[0] >= 'a' && s[0] <= 'z' {
		return string(s[0]-32) + s[1:]
	}
	return s
}

// isStopWord checks if a word is a common stop word.
// Mirrors the stop word list in cmd/brief.go.
func isStopWord(w string) bool {
	switch w {
	case "the", "and", "for", "are", "but", "not", "you", "all", "can", "had",
		"her", "was", "one", "our", "out", "has", "have", "been", "from", "with",
		"they", "this", "that", "what", "will", "into", "about", "than", "them",
		"then", "some", "when", "more", "also", "how", "new", "its",
		"your", "just", "like", "best", "most", "very", "does", "would", "could",
		"should", "which", "there", "their", "these", "those", "other", "using",
		"here", "where":
		return true
	}
	return false
}
