package enrich

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	"time"
)

// LinkedInEnricher gathers LinkedIn feed opportunities via the linkedin CLI.
type LinkedInEnricher struct{}

// NewLinkedInEnricher creates a new LinkedInEnricher.
func NewLinkedInEnricher() *LinkedInEnricher {
	return &LinkedInEnricher{}
}

func (e *LinkedInEnricher) Platform() string { return "linkedin" }

// linkedinFeedItem mirrors the FeedItem struct from `linkedin feed --json`.
type linkedinFeedItem struct {
	ActivityURN    string `json:"activity_urn"`
	AuthorName     string `json:"author_name"`
	AuthorHeadline string `json:"author_headline,omitempty"`
	Text           string `json:"text"`
	Timestamp      string `json:"timestamp"`
	ReactionCount  int    `json:"reaction_count"`
	CommentCount   int    `json:"comment_count"`
	RepostCount    int    `json:"repost_count"`
}

// linkedinComment mirrors the Comment struct from `linkedin comments <urn> --json`.
type linkedinComment struct {
	AuthorName string `json:"author_name"`
	Text       string `json:"text"`
	Timestamp  string `json:"timestamp"`
	Reactions  int    `json:"reactions"`
}

// Scan calls `linkedin feed --json` and returns metadata-only opportunities.
func (e *LinkedInEnricher) Scan(ctx context.Context, limit int) ([]EnrichedOpportunity, error) {
	args := []string{"feed", "--json"}
	if limit > 0 {
		args = append(args, "--limit", fmt.Sprintf("%d", limit))
	}
	out, err := runCLI(ctx, "linkedin", args...)
	if err != nil {
		if errors.Is(err, exec.ErrNotFound) {
			return nil, nil
		}
		return nil, fmt.Errorf("linkedin feed: %w", err)
	}

	var items []linkedinFeedItem
	if err := json.Unmarshal(out, &items); err != nil {
		return nil, fmt.Errorf("linkedin feed: parsing output: %w", err)
	}

	result := make([]EnrichedOpportunity, 0, len(items))
	for _, item := range items {
		result = append(result, EnrichedOpportunity{
			ID:       item.ActivityURN,
			Platform: "linkedin",
			URL:      linkedinPostURL(item.ActivityURN),
			Title:    item.AuthorName,
			Body:     item.Text,
			Author:   item.AuthorName,
			Comments: []Comment{},
			Metrics: EngagementMetrics{
				Likes:    item.ReactionCount,
				Comments: item.CommentCount,
				Reposts:  item.RepostCount,
			},
		})
	}
	return result, nil
}

// Enrich fetches the post and its comment thread via the linkedin CLI.
func (e *LinkedInEnricher) Enrich(ctx context.Context, id string) (*EnrichedOpportunity, error) {
	if id == "" {
		return nil, fmt.Errorf("linkedin enrich: activity URN is required")
	}

	// Scan feed to find the item for metadata.
	items, err := e.Scan(ctx, 50)
	if err != nil {
		return nil, fmt.Errorf("linkedin enrich: scanning feed: %w", err)
	}

	var opp *EnrichedOpportunity
	for i := range items {
		if items[i].ID == id {
			opp = &items[i]
			break
		}
	}
	if opp == nil {
		opp = &EnrichedOpportunity{
			ID:        id,
			Platform:  "linkedin",
			URL:       linkedinPostURL(id),
			Comments:  []Comment{},
			CreatedAt: time.Now(),
		}
	}

	// Fetch comment thread.
	comments, err := fetchLinkedInComments(ctx, id)
	if err == nil {
		opp.Comments = comments
		opp.Metrics.Comments = len(comments)
	}

	return opp, nil
}

// fetchLinkedInComments calls `linkedin comments <urn> --json`.
func fetchLinkedInComments(ctx context.Context, urn string) ([]Comment, error) {
	out, err := runCLI(ctx, "linkedin", "comments", urn, "--json")
	if err != nil {
		return nil, fmt.Errorf("linkedin comments: %w", err)
	}

	var raw []linkedinComment
	if err := json.Unmarshal(out, &raw); err != nil {
		return nil, fmt.Errorf("linkedin comments: parsing output: %w", err)
	}

	comments := make([]Comment, 0, len(raw))
	for _, c := range raw {
		comments = append(comments, Comment{
			Author:    c.AuthorName,
			Text:      c.Text,
			Reactions: c.Reactions,
			CreatedAt: c.Timestamp,
		})
	}
	return comments, nil
}

// linkedinPostURL constructs a LinkedIn post URL from an activity URN.
func linkedinPostURL(urn string) string {
	if urn == "" {
		return ""
	}
	return fmt.Sprintf("https://www.linkedin.com/feed/update/%s/", urn)
}
