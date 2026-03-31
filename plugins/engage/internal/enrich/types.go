// Package enrich provides per-platform context gatherers for engagement enrichment.
package enrich

import (
	"context"
	"fmt"
	"os/exec"
	"strings"
	"time"
)

// Comment represents a comment on any platform post.
type Comment struct {
	Author    string `json:"author"`
	Text      string `json:"text"`
	Score     int    `json:"score,omitempty"`     // upvote score (Reddit)
	Reactions int    `json:"reactions,omitempty"` // reaction count (LinkedIn, Substack)
	CreatedAt string `json:"created_at,omitempty"`
	Depth     int    `json:"depth,omitempty"` // nesting level
}

// EngagementMetrics holds platform-agnostic engagement numbers.
type EngagementMetrics struct {
	Likes    int `json:"likes"`
	Comments int `json:"comments"`
	Reposts  int `json:"reposts,omitempty"`
	Views    int `json:"views,omitempty"`
	Score    int `json:"score,omitempty"` // Reddit upvote score
}

// EnrichedOpportunity is the fully enriched representation of a post, video, or article.
type EnrichedOpportunity struct {
	ID         string            `json:"id"`
	Platform   string            `json:"platform"`
	URL        string            `json:"url"`
	Title      string            `json:"title"`
	Body       string            `json:"body"`                 // full post/article text
	Author     string            `json:"author"`
	CreatedAt  time.Time         `json:"created_at"`
	Transcript string            `json:"transcript,omitempty"` // video transcript (YouTube)
	Comments   []Comment         `json:"comments"`
	Metrics    EngagementMetrics `json:"metrics"`
	Images     []string          `json:"images,omitempty"` // image URLs or descriptions
}

// Enricher gathers engagement opportunities from a single platform.
type Enricher interface {
	Platform() string
	// Scan returns lightly-enriched opportunities (metadata only, no comments or transcript).
	// Returns an empty slice without error if the platform CLI is unavailable or unconfigured.
	Scan(ctx context.Context, limit int) ([]EnrichedOpportunity, error)
	// Enrich returns a fully enriched opportunity for the given platform ID.
	Enrich(ctx context.Context, id string) (*EnrichedOpportunity, error)
}

// All returns all registered enrichers.
func All() []Enricher {
	return []Enricher{
		NewYouTubeEnricher(),
		NewLinkedInEnricher(),
		NewRedditEnricher(),
		NewSubstackEnricher(),
		NewXEnricher(),
	}
}

// ByPlatform returns the enricher for the given platform name, or nil if not found.
func ByPlatform(name string) Enricher {
	for _, e := range All() {
		if e.Platform() == name {
			return e
		}
	}
	return nil
}

// runCLI executes a CLI tool and returns its stdout.
// Returns a descriptive error if the binary is not found or exits non-zero.
func runCLI(ctx context.Context, name string, args ...string) ([]byte, error) {
	out, err := exec.CommandContext(ctx, name, args...).Output()
	if err != nil {
		if ee, ok := err.(*exec.ExitError); ok && len(ee.Stderr) > 0 {
			return nil, fmt.Errorf("%s: %s", name, strings.TrimSpace(string(ee.Stderr)))
		}
		return nil, fmt.Errorf("running %s %s: %w", name, strings.Join(args, " "), err)
	}
	return out, nil
}
