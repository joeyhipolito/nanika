package api

import (
	"encoding/json"
	"fmt"
	"net/url"
)

// CreateComment posts a comment on a LinkedIn post via the Official API.
func (c *OAuthClient) CreateComment(activityURN, text string) error {
	encodedURN := url.QueryEscape(activityURN)
	path := fmt.Sprintf("/socialActions/%s/comments", encodedURN)

	req := &CreateCommentRequest{
		Actor: c.PersonURN,
		Message: CommentMessage{
			Text: text,
		},
	}

	_, _, err := c.do("POST", path, req)
	return err
}

// CreateReaction reacts to a LinkedIn post via the Official API.
// reactionType should be one of: LIKE, PRAISE, EMPATHY, INTEREST, APPRECIATION, ENTERTAINMENT.
func (c *OAuthClient) CreateReaction(activityURN, reactionType string) error {
	encodedURN := url.QueryEscape(activityURN)
	path := fmt.Sprintf("/socialActions/%s/likes", encodedURN)

	// The reactions endpoint uses a specific body format
	body := map[string]interface{}{
		"actor":              c.PersonURN,
		"specificContent":    reactionType,
	}

	_, _, err := c.do("POST", path, body)
	return err
}

// GetCommentsOfficial fetches comments on a post via the Official API.
// Returns comments and nil error on success, or error if unauthorized/unavailable.
func (c *OAuthClient) GetCommentsOfficial(activityURN string, count int) ([]OfficialComment, error) {
	if count <= 0 {
		count = 10
	}

	encodedURN := url.QueryEscape(activityURN)
	path := fmt.Sprintf("/socialActions/%s/comments?count=%d&start=0", encodedURN, count)

	data, _, err := c.do("GET", path, nil)
	if err != nil {
		return nil, err
	}

	var resp OfficialCommentsResponse
	if err := json.Unmarshal(data, &resp); err != nil {
		return nil, fmt.Errorf("parsing comments response: %w", err)
	}
	return resp.Elements, nil
}

// OfficialComment represents a comment from the Official API.
type OfficialComment struct {
	Actor     string         `json:"actor"`
	Message   CommentMessage `json:"message"`
	CreatedAt int64          `json:"created"`
}

// OfficialCommentsResponse wraps the paginated list of comments from the Official API.
type OfficialCommentsResponse struct {
	Elements []OfficialComment `json:"elements"`
	Paging   Paging            `json:"paging"`
}
