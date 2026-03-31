package api

import (
	"encoding/json"
	"fmt"
	"net/url"
	"strconv"
)

// Submit creates a new Reddit post (text or link).
// kind: "self" for text posts, "link" for link posts.
func (c *RedditClient) Submit(subreddit, title, text, linkURL, kind string) (*SubmitResponse, error) {
	form := url.Values{
		"sr":       {subreddit},
		"title":    {title},
		"kind":     {kind},
		"api_type": {"json"},
	}
	if kind == "self" {
		form.Set("text", text)
	} else if kind == "link" {
		form.Set("url", linkURL)
	}

	data, err := c.doPost("/api/submit", form)
	if err != nil {
		return nil, fmt.Errorf("submitting post: %w", err)
	}

	var resp SubmitResponse
	if err := json.Unmarshal(data, &resp); err != nil {
		return nil, fmt.Errorf("parsing submit response: %w", err)
	}

	if len(resp.JSON.Errors) > 0 {
		return nil, fmt.Errorf("Reddit error: %v", resp.JSON.Errors)
	}

	return &resp, nil
}

// UserPosts fetches a user's recent submitted posts.
func (c *RedditClient) UserPosts(username string, limit int) ([]PostData, error) {
	path := fmt.Sprintf("/user/%s/submitted.json?limit=%d", username, limit)
	data, err := c.doGet(path)
	if err != nil {
		return nil, fmt.Errorf("fetching user posts: %w", err)
	}

	return parsePosts(data)
}

// Feed fetches posts from the home feed or a specific subreddit.
// subreddit: empty for home feed, or a subreddit name (without r/ prefix).
// sort: "hot", "new", "top", "rising".
func (c *RedditClient) Feed(subreddit, sort string, limit int) ([]PostData, error) {
	var path string
	if subreddit != "" {
		path = fmt.Sprintf("/r/%s/%s.json?limit=%d", subreddit, sort, limit)
	} else {
		path = fmt.Sprintf("/%s.json?limit=%d", sort, limit)
	}

	data, err := c.doGet(path)
	if err != nil {
		return nil, fmt.Errorf("fetching feed: %w", err)
	}

	return parsePosts(data)
}

// Search searches Reddit posts globally or within a subreddit.
// subreddit: empty for global search, or a subreddit name (without r/ prefix).
// sort: "relevance", "new", "top", "comments".
// timeFilter: "hour", "day", "week", "month", "year", "all".
func (c *RedditClient) Search(query, subreddit, sort, timeFilter string, limit int) ([]PostData, error) {
	if query == "" {
		return nil, fmt.Errorf("query is required")
	}

	params := url.Values{}
	params.Set("q", query)
	params.Set("sort", sort)
	params.Set("t", timeFilter)
	params.Set("limit", strconv.Itoa(limit))
	params.Set("type", "link")

	var path string
	if subreddit != "" {
		params.Set("restrict_sr", "true")
		path = fmt.Sprintf("/r/%s/search.json?%s", subreddit, params.Encode())
	} else {
		path = fmt.Sprintf("/search.json?%s", params.Encode())
	}

	data, err := c.doGet(path)
	if err != nil {
		return nil, fmt.Errorf("searching Reddit: %w", err)
	}

	return parsePosts(data)
}

func parsePosts(data []byte) ([]PostData, error) {
	var listing Listing
	if err := json.Unmarshal(data, &listing); err != nil {
		return nil, fmt.Errorf("parsing listing: %w", err)
	}

	var posts []PostData
	for _, thing := range listing.Data.Children {
		if thing.Kind != "t3" {
			continue
		}

		// Re-marshal the thing data and unmarshal into PostData
		thingJSON, err := json.Marshal(thing.Data)
		if err != nil {
			continue
		}

		var post PostData
		if err := json.Unmarshal(thingJSON, &post); err != nil {
			continue
		}
		posts = append(posts, post)
	}

	return posts, nil
}
