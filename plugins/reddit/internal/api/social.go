package api

import (
	"encoding/json"
	"fmt"
	"net/url"
)

// Comments fetches the comment tree for a post.
// postID: bare ID (e.g., "abc123"), not the fullname.
func (c *RedditClient) Comments(postID string, sort string, limit, depth int) ([]PostData, []CommentData, error) {
	path := fmt.Sprintf("/comments/%s.json?sort=%s&limit=%d&depth=%d", postID, sort, limit, depth)
	data, err := c.doGet(path)
	if err != nil {
		return nil, nil, fmt.Errorf("fetching comments: %w", err)
	}

	// Reddit returns an array of two listings: [post, comments]
	var listings []Listing
	if err := json.Unmarshal(data, &listings); err != nil {
		return nil, nil, fmt.Errorf("parsing comments response: %w", err)
	}

	if len(listings) < 2 {
		return nil, nil, fmt.Errorf("unexpected response format: expected 2 listings, got %d", len(listings))
	}

	// Parse the post (first listing)
	var posts []PostData
	for _, thing := range listings[0].Data.Children {
		if thing.Kind != "t3" {
			continue
		}
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

	// Parse comments (second listing)
	comments := parseCommentTree(listings[1].Data.Children)

	return posts, comments, nil
}

func parseCommentTree(things []Thing) []CommentData {
	var comments []CommentData
	for _, thing := range things {
		if thing.Kind != "t1" {
			continue
		}

		thingJSON, err := json.Marshal(thing.Data)
		if err != nil {
			continue
		}

		var comment CommentData
		if err := json.Unmarshal(thingJSON, &comment); err != nil {
			continue
		}
		comments = append(comments, comment)

		// Recursively parse replies
		if repliesRaw, ok := thing.Data["replies"]; ok {
			if repliesMap, ok := repliesRaw.(map[string]interface{}); ok {
				if dataMap, ok := repliesMap["data"].(map[string]interface{}); ok {
					if childrenRaw, ok := dataMap["children"].([]interface{}); ok {
						var replyThings []Thing
						replyJSON, _ := json.Marshal(childrenRaw)
						if json.Unmarshal(replyJSON, &replyThings) == nil {
							comments = append(comments, parseCommentTree(replyThings)...)
						}
					}
				}
			}
		}
	}
	return comments
}

// Comment posts a reply to a post or comment.
// parentFullname: the fullname of the parent (t3_xxx for post, t1_xxx for comment).
func (c *RedditClient) Comment(parentFullname, text string) (*CommentResponse, error) {
	form := url.Values{
		"thing_id": {parentFullname},
		"text":     {text},
		"api_type": {"json"},
	}

	data, err := c.doPost("/api/comment", form)
	if err != nil {
		return nil, fmt.Errorf("posting comment: %w", err)
	}

	var resp CommentResponse
	if err := json.Unmarshal(data, &resp); err != nil {
		return nil, fmt.Errorf("parsing comment response: %w", err)
	}

	if len(resp.JSON.Errors) > 0 {
		return nil, fmt.Errorf("Reddit error: %v", resp.JSON.Errors)
	}

	return &resp, nil
}

// Vote votes on a post or comment.
// fullname: the fullname (t3_xxx or t1_xxx).
// dir: 1 for upvote, -1 for downvote, 0 for unvote.
func (c *RedditClient) Vote(fullname string, dir int) error {
	form := url.Values{
		"id":  {fullname},
		"dir": {fmt.Sprintf("%d", dir)},
	}

	data, err := c.doPost("/api/vote", form)
	if err != nil {
		return fmt.Errorf("voting: %w", err)
	}

	// Vote returns empty JSON {} on success
	if len(data) > 2 {
		// Check for error response
		var errResp struct {
			JSON struct {
				Errors [][]string `json:"errors"`
			} `json:"json"`
		}
		if json.Unmarshal(data, &errResp) == nil && len(errResp.JSON.Errors) > 0 {
			return fmt.Errorf("Reddit error: %v", errResp.JSON.Errors)
		}
	}

	return nil
}
