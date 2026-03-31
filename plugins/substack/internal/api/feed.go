package api

import (
	"encoding/json"
	"fmt"
	"net/http"
)

// readerFeedResponse wraps the /api/v1/reader/feed/profile/{id} response.
type readerFeedResponse struct {
	Items      []readerFeedItem `json:"items"`
	NextCursor string           `json:"nextCursor"`
}

// readerFeedItem is a single item from the reader feed (post or comment).
type readerFeedItem struct {
	EntityKey   string          `json:"entity_key"`
	Type        string          `json:"type"`
	Post        json.RawMessage `json:"post"`
	Publication json.RawMessage `json:"publication"`
}

// readerPublication is the publication info nested in a feed item.
type readerPublication struct {
	Name string `json:"name"`
}

// GetFeed returns recent posts from the user's reader feed (subscribed publications).
func (c *Client) GetFeed(limit int) ([]FeedItem, error) {
	if c.UserID == 0 {
		if _, err := c.GetProfile(); err != nil {
			return nil, fmt.Errorf("getting profile for feed: %w", err)
		}
	}

	path := fmt.Sprintf("/api/v1/reader/feed/profile/%d", c.UserID)
	resp, err := c.doGlobal("GET", path, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching feed: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching feed: HTTP %d", resp.StatusCode)
	}

	var feedResp readerFeedResponse
	if err := json.NewDecoder(resp.Body).Decode(&feedResp); err != nil {
		return nil, fmt.Errorf("decoding feed: %w", err)
	}

	var items []FeedItem
	for _, raw := range feedResp.Items {
		if raw.Type != "post" {
			continue
		}
		if raw.Post == nil {
			continue
		}

		var item FeedItem
		if err := json.Unmarshal(raw.Post, &item); err != nil {
			continue
		}

		// Extract publication name
		if raw.Publication != nil {
			var pub readerPublication
			if err := json.Unmarshal(raw.Publication, &pub); err == nil {
				item.PublicationName = pub.Name
			}
		}

		items = append(items, item)

		if limit > 0 && len(items) >= limit {
			break
		}
	}

	return items, nil
}
