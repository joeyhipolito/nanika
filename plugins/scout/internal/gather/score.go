package gather

import (
	"math"
	"strings"
	"time"
)

// ScoreItems scores all items based on relevance, recency, and engagement.
// It modifies items in-place and returns the slice.
func ScoreItems(items []IntelItem, searchTerms []string, now time.Time) []IntelItem {
	for i := range items {
		ScoreItem(&items[i], searchTerms, now)
	}
	return items
}

// ScoreItem calculates and sets the score for a single item.
func ScoreItem(item *IntelItem, searchTerms []string, now time.Time) {
	item.Score = calculateScore(*item, searchTerms, now)
}

// calculateScore computes a 0-100 score based on:
//   - relevance (0.4): matched search terms / total terms
//   - recency (0.3): linear decay over 30 days
//   - engagement (0.3): log scale, caps at 10,000
//
// Sources without engagement data (RSS, web, substack, medium, googlenews,
// lobsters) have their engagement weight redistributed to relevance and
// recency so they aren't structurally capped at 70.
func calculateScore(item IntelItem, searchTerms []string, now time.Time) float64 {
	// Relevance: what fraction of search terms appear in the item
	var relevance float64
	if len(searchTerms) == 0 {
		relevance = 1.0
	} else {
		matches := countTermMatches(item, searchTerms)
		relevance = float64(matches) / float64(len(searchTerms))
	}

	// Recency: linear decay from 1.0 to 0.0 over 720 hours (30 days)
	hoursSince := now.Sub(item.Timestamp).Hours()
	recency := math.Max(0, 1-hoursSince/720)

	// Engagement: logarithmic scale, capped at 10,000 (log10(10001)/4 ≈ 1.0)
	engagement := math.Min(1, math.Log10(float64(item.Engagement)+1)/4)

	var score float64
	if item.Engagement == 0 {
		// No engagement data — redistribute weight to relevance (57%) and recency (43%)
		score = (relevance*0.57 + recency*0.43) * 100
	} else {
		score = (relevance*0.4 + recency*0.3 + engagement*0.3) * 100
	}
	return math.Round(score*10) / 10 // Round to 1 decimal
}

// countTermMatches counts how many search terms appear in an item's text.
func countTermMatches(item IntelItem, terms []string) int {
	text := strings.ToLower(item.Title + " " + item.Content + " " + item.Author)
	matches := 0
	for _, term := range terms {
		if strings.Contains(text, strings.ToLower(term)) {
			matches++
		}
	}
	return matches
}
