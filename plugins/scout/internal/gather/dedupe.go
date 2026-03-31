package gather

import (
	"sort"
	"strings"
)

const dedupeThreshold = 0.6

// DedupeItems removes duplicate items based on ID match or Jaccard title similarity.
// When duplicates are found, the higher-scored item is kept.
func DedupeItems(items []IntelItem) []IntelItem {
	var result []IntelItem

	for _, item := range items {
		dupIdx := findDuplicate(result, item)
		if dupIdx >= 0 {
			// Keep the higher-scored item; tiebreak by engagement
			existing := result[dupIdx]
			if item.Score > existing.Score || (item.Score == existing.Score && item.Engagement > existing.Engagement) {
				result[dupIdx] = item
			}
		} else {
			result = append(result, item)
		}
	}

	return result
}

// findDuplicate returns the index of a duplicate in results, or -1 if none found.
// Checks: exact ID, normalized URL match, then Jaccard title similarity.
func findDuplicate(results []IntelItem, item IntelItem) int {
	itemURL := normalizeURL(item.SourceURL)
	for i, existing := range results {
		if existing.ID == item.ID {
			return i
		}
		if item.SourceURL != "" && existing.SourceURL != "" &&
			normalizeURL(existing.SourceURL) == itemURL {
			return i
		}
		if jaccardSimilarity(existing.Title, item.Title) >= dedupeThreshold {
			return i
		}
	}
	return -1
}

// jaccardSimilarity computes Jaccard similarity between two strings based on unique words.
func jaccardSimilarity(a, b string) float64 {
	wordsA := uniqueWords(a)
	wordsB := uniqueWords(b)

	if len(wordsA) == 0 && len(wordsB) == 0 {
		return 1.0
	}
	if len(wordsA) == 0 || len(wordsB) == 0 {
		return 0.0
	}

	// Count intersection
	intersection := 0
	for w := range wordsA {
		if wordsB[w] {
			intersection++
		}
	}

	// Union = |A| + |B| - |intersection|
	union := len(wordsA) + len(wordsB) - intersection
	if union == 0 {
		return 0.0
	}

	return float64(intersection) / float64(union)
}

// CapItems returns at most maxItems items, keeping the highest-scored ones.
// Items are assumed to already be scored. Returns the original slice if
// len(items) <= maxItems.
func CapItems(items []IntelItem, maxItems int) []IntelItem {
	if len(items) <= maxItems {
		return items
	}
	sort.Slice(items, func(i, j int) bool {
		if items[i].Score != items[j].Score {
			return items[i].Score > items[j].Score
		}
		return items[i].Engagement > items[j].Engagement
	})
	return items[:maxItems]
}

// DedupeByURL removes items whose normalized URL has already been seen.
// The seenURLs map is shared across sources within a topic so that the same
// article from different sources is only kept once (highest-scored wins).
func DedupeByURL(items []IntelItem, seenURLs map[string]IntelItem) []IntelItem {
	var result []IntelItem
	for _, item := range items {
		if item.SourceURL == "" {
			result = append(result, item)
			continue
		}
		norm := normalizeURL(item.SourceURL)
		if existing, ok := seenURLs[norm]; ok {
			if item.Score > existing.Score || (item.Score == existing.Score && item.Engagement > existing.Engagement) {
				seenURLs[norm] = item
				// Replace in result — find and swap
				for i, r := range result {
					if normalizeURL(r.SourceURL) == norm {
						result[i] = item
						break
					}
				}
			}
			continue
		}
		seenURLs[norm] = item
		result = append(result, item)
	}
	return result
}

// uniqueWords extracts unique lowercase words (>2 chars) from a string.
func uniqueWords(s string) map[string]bool {
	words := make(map[string]bool)
	for _, w := range strings.Fields(strings.ToLower(s)) {
		if len(w) > 2 {
			words[w] = true
		}
	}
	return words
}
