package enrich

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os/exec"
	"time"
)

// XEnricher gathers X (formerly Twitter) opportunities via scout's x-browser intelligence.
// Requires scout to be configured with an x-browser topic or for x-browser intel to exist.
type XEnricher struct{}

// NewXEnricher creates a new XEnricher.
func NewXEnricher() *XEnricher {
	return &XEnricher{}
}

func (e *XEnricher) Platform() string { return "x" }

// scoutIntelItem mirrors IntelItem from the scout CLI JSON output.
type scoutIntelItem struct {
	ID         string    `json:"id"`
	Title      string    `json:"title"`
	Content    string    `json:"content"`
	SourceURL  string    `json:"source_url"`
	Author     string    `json:"author"`
	Timestamp  time.Time `json:"timestamp"`
	Tags       []string  `json:"tags"`
	Engagement int       `json:"engagement,omitempty"`
}

// Scan fetches recent X posts from scout's x-browser intel store.
// Returns an empty slice without error if scout is unavailable or has no x-browser intel.
func (e *XEnricher) Scan(ctx context.Context, limit int) ([]EnrichedOpportunity, error) {
	items, err := fetchScoutXItems(ctx)
	if err != nil {
		if errors.Is(err, exec.ErrNotFound) {
			return nil, nil
		}
		return nil, fmt.Errorf("x scan: %w", err)
	}

	if limit > 0 && len(items) > limit {
		items = items[:limit]
	}

	result := make([]EnrichedOpportunity, 0, len(items))
	for _, item := range items {
		result = append(result, EnrichedOpportunity{
			ID:        item.ID,
			Platform:  "x",
			URL:       item.SourceURL,
			Title:     item.Title,
			Body:      item.Content,
			Author:    item.Author,
			CreatedAt: item.Timestamp,
			Comments:  []Comment{},
			Metrics: EngagementMetrics{
				Likes: item.Engagement,
			},
		})
	}
	return result, nil
}

// Enrich returns the X post from scout intel.
// X does not expose a public comments API, so only post text and metadata are available.
func (e *XEnricher) Enrich(ctx context.Context, id string) (*EnrichedOpportunity, error) {
	if id == "" {
		return nil, fmt.Errorf("x enrich: post ID is required")
	}

	items, err := fetchScoutXItems(ctx)
	if err != nil {
		return nil, fmt.Errorf("x enrich: %w", err)
	}

	for _, item := range items {
		if item.ID == id {
			return &EnrichedOpportunity{
				ID:        item.ID,
				Platform:  "x",
				URL:       item.SourceURL,
				Title:     item.Title,
				Body:      item.Content,
				Author:    item.Author,
				CreatedAt: item.Timestamp,
				Comments:  []Comment{}, // X does not expose a public comments API
				Metrics: EngagementMetrics{
					Likes: item.Engagement,
				},
			}, nil
		}
	}

	return nil, fmt.Errorf("x enrich: post %q not found in scout intel", id)
}

// fetchScoutXItems retrieves x-browser items from scout's intelligence store.
// Calls `scout intel --json` with no topic and filters for x-browser tags.
func fetchScoutXItems(ctx context.Context) ([]scoutIntelItem, error) {
	out, err := runCLI(ctx, "scout", "intel", "--json")
	if err != nil {
		return nil, err
	}

	var all []scoutIntelItem
	if err := json.Unmarshal(out, &all); err != nil {
		return nil, fmt.Errorf("parsing scout intel output: %w", err)
	}

	var xItems []scoutIntelItem
	for _, item := range all {
		if hasTag(item.Tags, "x-browser") {
			xItems = append(xItems, item)
		}
	}
	return xItems, nil
}

// hasTag returns true if tags contains the target tag.
func hasTag(tags []string, tag string) bool {
	for _, t := range tags {
		if t == tag {
			return true
		}
	}
	return false
}
