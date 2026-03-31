package api

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
)

// commentsResponse wraps the API response for comments which includes a "comments" key.
type commentsResponse struct {
	Comments []Comment `json:"comments"`
}

// GetComments returns comments on a post.
func (c *Client) GetComments(subdomain string, postID int) ([]Comment, error) {
	url := fmt.Sprintf("https://%s.substack.com/api/v1/post/%d/comments?token=&all_comments=true&sort=best_first", subdomain, postID)
	resp, err := c.doURL("GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching comments: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusNotFound {
		return nil, fmt.Errorf("post not found: %d", postID)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching comments: HTTP %d", resp.StatusCode)
	}

	// Try decoding as wrapped response first, then as bare array
	var wrapped commentsResponse
	if err := json.NewDecoder(resp.Body).Decode(&wrapped); err != nil {
		return nil, fmt.Errorf("decoding comments: %w", err)
	}

	return wrapped.Comments, nil
}

// PostComment posts a comment on a post. The text is sent as Tiptap JSON (ProseMirror format).
func (c *Client) PostComment(subdomain string, postID int, text string) (*Comment, error) {
	// Build Tiptap document for the comment body
	bodyJSON := map[string]any{
		"type": "doc",
		"content": []map[string]any{
			{
				"type": "paragraph",
				"content": []map[string]any{
					{
						"type": "text",
						"text": text,
					},
				},
			},
		},
	}

	payload := map[string]any{
		"body": bodyJSON,
	}

	body, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("encoding comment: %w", err)
	}

	url := fmt.Sprintf("https://%s.substack.com/api/v1/post/%d/comment", subdomain, postID)
	resp, err := c.doURL("POST", url, bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("posting comment: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return nil, fmt.Errorf("posting comment: HTTP %d", resp.StatusCode)
	}

	var comment Comment
	if err := json.NewDecoder(resp.Body).Decode(&comment); err != nil {
		return nil, fmt.Errorf("decoding comment response: %w", err)
	}

	return &comment, nil
}

// GetPostBySlug fetches a post by its slug from a specific publication.
func (c *Client) GetPostBySlug(subdomain, slug string) (*Post, error) {
	url := fmt.Sprintf("https://%s.substack.com/api/v1/posts/%s", subdomain, slug)
	resp, err := c.doURL("GET", url, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching post by slug: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusNotFound {
		return nil, fmt.Errorf("post not found: %s on %s", slug, subdomain)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching post by slug: HTTP %d", resp.StatusCode)
	}

	var post Post
	if err := json.NewDecoder(resp.Body).Decode(&post); err != nil {
		return nil, fmt.Errorf("decoding post: %w", err)
	}

	return &post, nil
}

// PostRef holds a resolved post reference — either by subdomain+slug or by direct ID.
type PostRef struct {
	Subdomain string
	Slug      string
	PostID    int // Set if resolved from /home/post/p-{id} URL or bare ID
}

// ResolvePostURL parses a Substack post URL or bare post ID.
// Accepts formats:
//   - https://example.substack.com/p/my-post-slug
//   - example.substack.com/p/my-post-slug
//   - https://substack.com/home/post/p-188374917
//   - 188374917 (bare post ID)
func ResolvePostURL(postURL string) (*PostRef, error) {
	s := postURL

	// Check if it's a bare numeric ID
	if isNumeric(s) {
		id := parsePostID(s)
		if id > 0 {
			return &PostRef{PostID: id}, nil
		}
	}

	// Strip protocol
	if len(s) > 8 && s[:8] == "https://" {
		s = s[8:]
	} else if len(s) > 7 && s[:7] == "http://" {
		s = s[7:]
	}

	// Split host/path
	slashIdx := -1
	for i := 0; i < len(s); i++ {
		if s[i] == '/' {
			slashIdx = i
			break
		}
	}
	if slashIdx < 0 {
		return nil, fmt.Errorf("invalid post URL: %s", postURL)
	}

	host := s[:slashIdx]
	path := s[slashIdx:]

	// Handle substack.com/home/post/p-{id} format
	if host == "substack.com" || host == "www.substack.com" {
		prefix := "/home/post/p-"
		if len(path) > len(prefix) && path[:len(prefix)] == prefix {
			idStr := path[len(prefix):]
			// Strip trailing query/hash
			for i := 0; i < len(idStr); i++ {
				if idStr[i] == '?' || idStr[i] == '#' || idStr[i] == '/' {
					idStr = idStr[:i]
					break
				}
			}
			id := parsePostID(idStr)
			if id > 0 {
				return &PostRef{PostID: id}, nil
			}
		}
	}

	// Extract subdomain from host
	var subdomain string
	suffix := ".substack.com"
	if len(host) > len(suffix) && host[len(host)-len(suffix):] == suffix {
		subdomain = host[:len(host)-len(suffix)]
	} else {
		subdomain = host
	}

	// Parse /p/slug
	if len(path) < 3 || path[:3] != "/p/" {
		return nil, fmt.Errorf("invalid post URL: %s (expected /p/slug path or /home/post/p-ID)", postURL)
	}
	slug := path[3:]

	// Strip trailing slash and query params
	for i := 0; i < len(slug); i++ {
		if slug[i] == '?' || slug[i] == '#' || slug[i] == '/' {
			slug = slug[:i]
			break
		}
	}

	if slug == "" {
		return nil, fmt.Errorf("invalid post URL: %s (empty slug)", postURL)
	}

	return &PostRef{Subdomain: subdomain, Slug: slug}, nil
}

func isNumeric(s string) bool {
	if s == "" {
		return false
	}
	for _, c := range s {
		if c < '0' || c > '9' {
			return false
		}
	}
	return true
}

func parsePostID(s string) int {
	id := 0
	for _, c := range s {
		if c < '0' || c > '9' {
			return 0
		}
		id = id*10 + int(c-'0')
	}
	return id
}
