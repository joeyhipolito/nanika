package enrich

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
)

// SubstackEnricher gathers Substack article opportunities via the substack CLI.
type SubstackEnricher struct{}

// NewSubstackEnricher creates a new SubstackEnricher.
func NewSubstackEnricher() *SubstackEnricher {
	return &SubstackEnricher{}
}

func (e *SubstackEnricher) Platform() string { return "substack" }

// substackFeedItem mirrors the JSON output of `substack feed --json`.
type substackFeedItem struct {
	ID             int            `json:"id"`
	Title          string         `json:"title"`
	Subtitle       string         `json:"subtitle,omitempty"`
	Author         string         `json:"author"`
	Publication    string         `json:"publication"`
	Date           string         `json:"date"`
	URL            string         `json:"url"`
	Comments       int            `json:"comments"`
	Reactions      map[string]int `json:"reactions,omitempty"`
	TotalReactions int            `json:"total_reactions"`
}

// substackComment mirrors the flat Comment output of `substack comments <url> --json`.
type substackComment struct {
	ID        int            `json:"id"`
	Body      string         `json:"body"`
	Name      string         `json:"name"`
	Date      string         `json:"date"`
	Reactions map[string]int `json:"reactions"`
	Children  []substackComment `json:"children"`
}

func (c *substackComment) totalReactions() int {
	n := 0
	for _, v := range c.Reactions {
		n += v
	}
	return n
}

// Scan calls `substack feed --json` and returns metadata-only opportunities.
func (e *SubstackEnricher) Scan(ctx context.Context, limit int) ([]EnrichedOpportunity, error) {
	args := []string{"feed", "--json"}
	if limit > 0 {
		args = append(args, "--limit", fmt.Sprintf("%d", limit))
	}
	out, err := runCLI(ctx, "substack", args...)
	if err != nil {
		if errors.Is(err, exec.ErrNotFound) {
			return nil, nil
		}
		return nil, fmt.Errorf("substack feed: %w", err)
	}

	var items []substackFeedItem
	if err := json.Unmarshal(out, &items); err != nil {
		return nil, fmt.Errorf("substack feed: parsing output: %w", err)
	}

	result := make([]EnrichedOpportunity, 0, len(items))
	for _, item := range items {
		result = append(result, EnrichedOpportunity{
			ID:       fmt.Sprintf("%d", item.ID),
			Platform: "substack",
			URL:      item.URL,
			Title:    item.Title,
			Body:     item.Subtitle,
			Author:   item.Author,
			Comments: []Comment{},
			Metrics: EngagementMetrics{
				Likes:    item.TotalReactions,
				Comments: item.Comments,
			},
		})
	}
	return result, nil
}

// Enrich fetches the article's full comment thread via the substack CLI.
// id is the post URL (substack comments command accepts URLs).
func (e *SubstackEnricher) Enrich(ctx context.Context, id string) (*EnrichedOpportunity, error) {
	if id == "" {
		return nil, fmt.Errorf("substack enrich: post URL is required")
	}

	out, err := runCLI(ctx, "substack", "comments", id, "--json")
	if err != nil {
		return nil, fmt.Errorf("substack enrich: %w", err)
	}

	var raw []substackComment
	if err := json.Unmarshal(out, &raw); err != nil {
		return nil, fmt.Errorf("substack enrich: parsing comments: %w", err)
	}

	opp := &EnrichedOpportunity{
		ID:       id,
		Platform: "substack",
		URL:      id, // for substack, the post URL is the canonical ID
		Comments: make([]Comment, 0, len(raw)),
	}

	for _, c := range raw {
		opp.Comments = append(opp.Comments, Comment{
			Author:    c.Name,
			Text:      c.Body,
			Reactions: c.totalReactions(),
			CreatedAt: c.Date,
		})
	}
	opp.Metrics.Comments = len(opp.Comments)

	return opp, nil
}
