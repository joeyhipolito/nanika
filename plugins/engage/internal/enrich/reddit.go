package enrich

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	"time"
)

// RedditEnricher gathers Reddit post opportunities via the reddit CLI.
type RedditEnricher struct{}

// NewRedditEnricher creates a new RedditEnricher.
func NewRedditEnricher() *RedditEnricher {
	return &RedditEnricher{}
}

func (e *RedditEnricher) Platform() string { return "reddit" }

// redditPost mirrors PostData from `reddit feed --json`.
type redditPost struct {
	ID          string  `json:"id"`
	Title       string  `json:"title"`
	Author      string  `json:"author"`
	Subreddit   string  `json:"subreddit"`
	SelfText    string  `json:"selftext"`
	URL         string  `json:"url"`
	Permalink   string  `json:"permalink"`
	Score       int     `json:"score"`
	UpvoteRatio float64 `json:"upvote_ratio"`
	NumComments int     `json:"num_comments"`
	CreatedUTC  float64 `json:"created_utc"`
	IsSelf      bool    `json:"is_self"`
}

// redditComment mirrors CommentData from `reddit comments <id> --json`.
type redditComment struct {
	Author     string  `json:"author"`
	Body       string  `json:"body"`
	Score      int     `json:"score"`
	CreatedUTC float64 `json:"created_utc"`
	Depth      int     `json:"depth"`
}

// redditCommentsOutput mirrors the response envelope of `reddit comments <id> --json`.
type redditCommentsOutput struct {
	Post     []redditPost    `json:"post"`
	Comments []redditComment `json:"comments"`
}

// Scan calls `reddit feed --json` and returns metadata-only opportunities.
func (e *RedditEnricher) Scan(ctx context.Context, limit int) ([]EnrichedOpportunity, error) {
	args := []string{"feed", "--json"}
	if limit > 0 {
		args = append(args, "--limit", fmt.Sprintf("%d", limit))
	}
	out, err := runCLI(ctx, "reddit", args...)
	if err != nil {
		if errors.Is(err, exec.ErrNotFound) {
			return nil, nil
		}
		return nil, fmt.Errorf("reddit feed: %w", err)
	}

	var posts []redditPost
	if err := json.Unmarshal(out, &posts); err != nil {
		return nil, fmt.Errorf("reddit feed: parsing output: %w", err)
	}

	result := make([]EnrichedOpportunity, 0, len(posts))
	for _, p := range posts {
		result = append(result, EnrichedOpportunity{
			ID:        p.ID,
			Platform:  "reddit",
			URL:       redditPostURL(p.Permalink),
			Title:     p.Title,
			Body:      p.SelfText,
			Author:    p.Author,
			CreatedAt: time.Unix(int64(p.CreatedUTC), 0).UTC(),
			Comments:  []Comment{},
			Metrics: EngagementMetrics{
				Likes:    p.Score,
				Comments: p.NumComments,
				Score:    p.Score,
			},
		})
	}
	return result, nil
}

// Enrich fetches the post with its full comment tree via the reddit CLI.
func (e *RedditEnricher) Enrich(ctx context.Context, id string) (*EnrichedOpportunity, error) {
	if id == "" {
		return nil, fmt.Errorf("reddit enrich: post ID is required")
	}

	out, err := runCLI(ctx, "reddit", "comments", id, "--json")
	if err != nil {
		return nil, fmt.Errorf("reddit enrich: %w", err)
	}

	var data redditCommentsOutput
	if err := json.Unmarshal(out, &data); err != nil {
		return nil, fmt.Errorf("reddit enrich: parsing output: %w", err)
	}

	opp := &EnrichedOpportunity{
		ID:       id,
		Platform: "reddit",
		Comments: []Comment{},
	}

	if len(data.Post) > 0 {
		p := data.Post[0]
		opp.URL = redditPostURL(p.Permalink)
		opp.Title = p.Title
		opp.Body = p.SelfText
		opp.Author = p.Author
		opp.CreatedAt = time.Unix(int64(p.CreatedUTC), 0).UTC()
		opp.Metrics = EngagementMetrics{
			Likes:    p.Score,
			Comments: p.NumComments,
			Score:    p.Score,
		}
	}

	for _, c := range data.Comments {
		opp.Comments = append(opp.Comments, Comment{
			Author: c.Author,
			Text:   c.Body,
			Score:  c.Score,
			Depth:  c.Depth,
		})
	}

	return opp, nil
}

// redditPostURL constructs a full Reddit URL from a permalink.
func redditPostURL(permalink string) string {
	if permalink == "" {
		return ""
	}
	if len(permalink) > 0 && permalink[0] == '/' {
		return "https://www.reddit.com" + permalink
	}
	return permalink
}
