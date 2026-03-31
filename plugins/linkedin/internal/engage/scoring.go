package engage

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/llm"
)

const scoringModel = "claude-haiku-4-5-20251001"

// FeedScore holds the LLM-assigned scores for a LinkedIn feed item.
type FeedScore struct {
	ActivityURN    string `json:"activity_urn"`
	Relevance      int    `json:"relevance"`
	Interest       int    `json:"interest"`
	MatchedArticle string `json:"matched_article,omitempty"` // slug of the matched substack post
	Reason         string `json:"reason,omitempty"`
}

// ScoreFeedItems calls Haiku to score each feed item on relevance (to our Substack posts) and interest.
func ScoreFeedItems(ctx context.Context, items []api.FeedItem, posts []SubstackPost) ([]FeedScore, error) {
	if len(items) == 0 {
		return nil, nil
	}

	prompt := buildScoringPrompt(items, posts)
	resp, err := llm.QueryText(ctx, prompt, scoringModel)
	if err != nil {
		return nil, fmt.Errorf("scoring LLM call: %w", err)
	}

	return parseScoringResponse(resp)
}

func buildScoringPrompt(items []api.FeedItem, posts []SubstackPost) string {
	var sb strings.Builder

	sb.WriteString(`You are a scoring engine. Score each LinkedIn feed item on two dimensions:
- relevance (0-10): How relevant is this post to one of OUR articles? 7+ means we could write a grounded comment linking our article.
- interest (0-10): How interesting/engaging is this post generally? 6+ means worth commenting on, 4+ means worth reacting to.

OUR ARTICLES (most recent Substack posts):
`)

	for i, p := range posts {
		if i >= 20 {
			break
		}
		desc := p.Description
		if len(desc) > 150 {
			desc = desc[:150] + "..."
		}
		fmt.Fprintf(&sb, "- slug=%q title=%q desc=%q\n", p.Slug, p.Title, desc)
	}

	sb.WriteString("\nFEED ITEMS TO SCORE:\n")

	for _, item := range items {
		text := item.Text
		if len(text) > 300 {
			text = text[:300] + "..."
		}
		fmt.Fprintf(&sb, "- urn=%q author=%q headline=%q reactions=%d comments=%d text=%q\n",
			item.ActivityURN, item.AuthorName, item.AuthorHeadline, item.ReactionCount, item.CommentCount, text)
	}

	sb.WriteString(`
Return ONLY a JSON array. No explanation, no markdown fences. Each element:
{"activity_urn": "<urn>", "relevance": <0-10>, "interest": <0-10>, "matched_article": "<slug or empty>", "reason": "<brief>"}
`)

	return sb.String()
}

func parseScoringResponse(resp string) ([]FeedScore, error) {
	resp = strings.TrimSpace(resp)

	// Strip markdown fences if present
	if strings.HasPrefix(resp, "```") {
		lines := strings.Split(resp, "\n")
		var cleaned []string
		for _, l := range lines {
			if strings.HasPrefix(strings.TrimSpace(l), "```") {
				continue
			}
			cleaned = append(cleaned, l)
		}
		resp = strings.Join(cleaned, "\n")
	}

	// Find JSON array boundaries
	start := strings.Index(resp, "[")
	end := strings.LastIndex(resp, "]")
	if start < 0 || end < 0 || end <= start {
		return nil, fmt.Errorf("no JSON array found in scoring response")
	}
	resp = resp[start : end+1]

	var scores []FeedScore
	if err := json.Unmarshal([]byte(resp), &scores); err != nil {
		return nil, fmt.Errorf("parsing scoring JSON: %w\nraw: %s", err, resp)
	}

	return scores, nil
}

// fallbackScores assigns scores based on reaction count when LLM scoring fails.
func fallbackScores(items []api.FeedItem) []FeedScore {
	var scores []FeedScore
	for _, item := range items {
		interest := 4 // default: react-worthy
		if item.ReactionCount >= 10 {
			interest = 6
		}
		scores = append(scores, FeedScore{
			ActivityURN: item.ActivityURN,
			Interest:    interest,
		})
	}
	return scores
}

// SubstackPost holds a Substack post used for article grounding.
// Matches the JSON output of `substack posts --json`.
type SubstackPost struct {
	ID           int    `json:"id"`
	Title        string `json:"title"`
	Subtitle     string `json:"subtitle,omitempty"`
	Slug         string `json:"slug"`
	CanonicalURL string `json:"canonical_url,omitempty"`
	Description  string `json:"description,omitempty"`
}

// loadSubstackPosts reads a JSON array of Substack posts (from `substack posts --json`).
func loadSubstackPosts(path string) ([]SubstackPost, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("reading substack posts file: %w", err)
	}

	var posts []SubstackPost
	if err := json.Unmarshal(data, &posts); err != nil {
		return nil, fmt.Errorf("parsing substack posts JSON: %w", err)
	}

	return posts, nil
}
