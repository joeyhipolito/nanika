package api

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"time"
)

// GetPosts returns published posts.
func (c *Client) GetPosts(offset, limit int) ([]Post, error) {
	path := fmt.Sprintf("/api/v1/posts?offset=%d&limit=%d", offset, limit)
	resp, err := c.do("GET", path, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching posts: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching posts: HTTP %d", resp.StatusCode)
	}

	var posts []Post
	if err := json.NewDecoder(resp.Body).Decode(&posts); err != nil {
		return nil, fmt.Errorf("decoding posts: %w", err)
	}

	return posts, nil
}

// GetDrafts returns current drafts.
func (c *Client) GetDrafts(offset, limit int) ([]Post, error) {
	path := fmt.Sprintf("/api/v1/drafts?offset=%d&limit=%d", offset, limit)
	resp, err := c.do("GET", path, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching drafts: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching drafts: HTTP %d", resp.StatusCode)
	}

	var drafts []Post
	if err := json.NewDecoder(resp.Body).Decode(&drafts); err != nil {
		return nil, fmt.Errorf("decoding drafts: %w", err)
	}

	return drafts, nil
}

// CreateDraft creates a new draft post.
func (c *Client) CreateDraft(req *DraftRequest) (*Post, error) {
	body, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("encoding draft request: %w", err)
	}

	resp, err := c.do("POST", "/api/v1/drafts/", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("creating draft: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return nil, fmt.Errorf("creating draft: HTTP %d", resp.StatusCode)
	}

	var draft Post
	if err := json.NewDecoder(resp.Body).Decode(&draft); err != nil {
		return nil, fmt.Errorf("decoding draft response: %w", err)
	}

	return &draft, nil
}

// GetPublicationTags returns all tags for the publication.
// GET /api/v1/publication/post-tag
func (c *Client) GetPublicationTags() ([]PostTag, error) {
	resp, err := c.do("GET", "/api/v1/publication/post-tag", nil)
	if err != nil {
		return nil, fmt.Errorf("fetching tags: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching tags: HTTP %d", resp.StatusCode)
	}

	var tags []PostTag
	if err := json.NewDecoder(resp.Body).Decode(&tags); err != nil {
		return nil, fmt.Errorf("decoding tags: %w", err)
	}

	return tags, nil
}

// CreatePublicationTag creates a new tag for the publication.
// POST /api/v1/publication/post-tag with {"name":"TagName"}
func (c *Client) CreatePublicationTag(name string) (*PostTag, error) {
	payload := struct {
		Name string `json:"name"`
	}{Name: name}

	body, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("encoding tag request: %w", err)
	}

	resp, err := c.do("POST", "/api/v1/publication/post-tag", bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("creating tag: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return nil, fmt.Errorf("creating tag: HTTP %d", resp.StatusCode)
	}

	var tag PostTag
	if err := json.NewDecoder(resp.Body).Decode(&tag); err != nil {
		return nil, fmt.Errorf("decoding tag response: %w", err)
	}

	return &tag, nil
}

// DeletePublicationTag deletes a tag from the publication by ID.
// DELETE /api/v1/publication/post-tag/{id}
func (c *Client) DeletePublicationTag(id string) error {
	path := fmt.Sprintf("/api/v1/publication/post-tag/%s", id)
	resp, err := c.do("DELETE", path, nil)
	if err != nil {
		return fmt.Errorf("deleting tag: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusNoContent {
		return fmt.Errorf("deleting tag: HTTP %d", resp.StatusCode)
	}

	return nil
}

// AssignTagToPost assigns an existing tag to a post/draft.
// POST /api/v1/post/{postId}/tag/{tagId} with no body.
func (c *Client) AssignTagToPost(postID int, tagID string) error {
	path := fmt.Sprintf("/api/v1/post/%d/tag/%s", postID, tagID)
	resp, err := c.do("POST", path, nil)
	if err != nil {
		return fmt.Errorf("assigning tag: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return fmt.Errorf("assigning tag: HTTP %d", resp.StatusCode)
	}

	return nil
}

// RemoveTagFromPost removes a tag from a post/draft.
// DELETE /api/v1/post/{postId}/tag/{tagId} with no body.
func (c *Client) RemoveTagFromPost(postID int, tagID string) error {
	path := fmt.Sprintf("/api/v1/post/%d/tag/%s", postID, tagID)
	resp, err := c.do("DELETE", path, nil)
	if err != nil {
		return fmt.Errorf("removing tag: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusNoContent {
		return fmt.Errorf("removing tag: HTTP %d", resp.StatusCode)
	}

	return nil
}

// EnsureAndAssignTags creates any missing tags at the publication level,
// then assigns them all to the given post. Returns the resolved tags.
func (c *Client) EnsureAndAssignTags(postID int, tagNames []string) ([]PostTag, error) {
	if len(tagNames) == 0 {
		return nil, nil
	}

	// Fetch existing publication tags
	existing, err := c.GetPublicationTags()
	if err != nil {
		return nil, fmt.Errorf("fetching existing tags: %w", err)
	}

	// Build lookup by lowercase name
	tagByName := make(map[string]PostTag)
	for _, t := range existing {
		tagByName[ToLower(t.Name)] = t
	}

	// Resolve or create each tag
	var resolved []PostTag
	for _, name := range tagNames {
		if tag, ok := tagByName[ToLower(name)]; ok {
			resolved = append(resolved, tag)
		} else {
			// Create new tag
			tag, err := c.CreatePublicationTag(name)
			if err != nil {
				return nil, fmt.Errorf("creating tag %q: %w", name, err)
			}
			resolved = append(resolved, *tag)
		}
	}

	// Assign all tags to the post
	for _, tag := range resolved {
		if err := c.AssignTagToPost(postID, tag.ID); err != nil {
			return nil, fmt.Errorf("assigning tag %q to post %d: %w", tag.Name, postID, err)
		}
	}

	return resolved, nil
}

// ToLower is a simple ASCII lowercase helper.
func ToLower(s string) string {
	b := make([]byte, len(s))
	for i := range s {
		c := s[i]
		if c >= 'A' && c <= 'Z' {
			c += 'a' - 'A'
		}
		b[i] = c
	}
	return string(b)
}

// GetDraft fetches a single draft/post by ID.
func (c *Client) GetDraft(draftID int) (*Post, error) {
	path := fmt.Sprintf("/api/v1/drafts/%d", draftID)
	resp, err := c.do("GET", path, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching draft: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching draft: HTTP %d", resp.StatusCode)
	}

	var draft Post
	if err := json.NewDecoder(resp.Body).Decode(&draft); err != nil {
		return nil, fmt.Errorf("decoding draft: %w", err)
	}

	return &draft, nil
}

// UpdateDraft updates an existing draft/post body via PUT.
func (c *Client) UpdateDraft(draftID int, req *DraftUpdateRequest) (*Post, error) {
	body, err := json.Marshal(req)
	if err != nil {
		return nil, fmt.Errorf("encoding update request: %w", err)
	}

	path := fmt.Sprintf("/api/v1/drafts/%d", draftID)
	resp, err := c.do("PUT", path, bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("updating draft: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		respBody, _ := io.ReadAll(resp.Body)
		return nil, fmt.Errorf("updating draft: HTTP %d: %s", resp.StatusCode, string(respBody))
	}

	var draft Post
	if err := json.NewDecoder(resp.Body).Decode(&draft); err != nil {
		return nil, fmt.Errorf("decoding update response: %w", err)
	}

	return &draft, nil
}

// DeleteDraft deletes a draft by ID.
func (c *Client) DeleteDraft(draftID int) error {
	path := fmt.Sprintf("/api/v1/drafts/%d", draftID)
	resp, err := c.do("DELETE", path, nil)
	if err != nil {
		return fmt.Errorf("deleting draft: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusNoContent {
		return fmt.Errorf("deleting draft: HTTP %d", resp.StatusCode)
	}

	return nil
}

// PrePublish runs the GET prepublish validation check for a draft at the given publish date.
// It returns subscriber counts and any warnings about the post before scheduling.
// GET /api/v1/drafts/{id}/prepublish?publish_date={RFC3339}
func (c *Client) PrePublish(ctx context.Context, postID int, publishDate time.Time) (*PrePublishResult, error) {
	path := fmt.Sprintf("/api/v1/drafts/%d/prepublish?publish_date=%s", postID, publishDate.UTC().Format(time.RFC3339))
	resp, err := c.doCtx(ctx, "GET", path, nil)
	if err != nil {
		return nil, fmt.Errorf("prepublish check for draft %d: %w", postID, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("prepublish check failed: HTTP %d", resp.StatusCode)
	}

	var result PrePublishResult
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, fmt.Errorf("decoding prepublish response: %w", err)
	}

	return &result, nil
}

// ScheduleRelease schedules a draft for release at the given time with the specified audiences.
// postAudience controls who can read the post (e.g. "everyone", "only_paid").
// emailAudience controls who receives the email (e.g. "everyone", "only_paid").
// POST /api/v1/drafts/{id}/scheduled_release
func (c *Client) ScheduleRelease(ctx context.Context, postID int, publishAt time.Time, postAudience, emailAudience string) error {
	req := ScheduleReleaseRequest{
		TriggerAt:     publishAt.UTC().Format(time.RFC3339),
		PostAudience:  postAudience,
		EmailAudience: emailAudience,
	}

	body, err := json.Marshal(req)
	if err != nil {
		return fmt.Errorf("encoding schedule release request: %w", err)
	}

	path := fmt.Sprintf("/api/v1/drafts/%d/scheduled_release", postID)
	resp, err := c.doCtx(ctx, "POST", path, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("scheduling release for draft %d: %w", postID, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return fmt.Errorf("scheduling release failed: HTTP %d", resp.StatusCode)
	}

	return nil
}

// SetDraftAudience updates the audience field on an existing draft via PUT.
// Called before scheduling to ensure the correct audience is set on the draft.
func (c *Client) SetDraftAudience(ctx context.Context, draftID int, audience string) error {
	body, err := json.Marshal(struct {
		Audience string `json:"audience"`
	}{Audience: audience})
	if err != nil {
		return fmt.Errorf("encoding audience update: %w", err)
	}

	path := fmt.Sprintf("/api/v1/drafts/%d", draftID)
	resp, err := c.doCtx(ctx, "PUT", path, bytes.NewReader(body))
	if err != nil {
		return fmt.Errorf("updating draft %d audience: %w", draftID, err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("updating draft audience: HTTP %d", resp.StatusCode)
	}

	return nil
}

// Prepublish runs the prepublish step for a draft.
func (c *Client) Prepublish(draftID int) error {
	path := fmt.Sprintf("/api/v1/drafts/%d/prepublish", draftID)
	resp, err := c.do("POST", path, nil)
	if err != nil {
		return fmt.Errorf("prepublishing draft: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("prepublish failed: HTTP %d", resp.StatusCode)
	}

	return nil
}

// UnpublishPost reverts a published post back to draft state.
func (c *Client) UnpublishPost(postID int) error {
	path := fmt.Sprintf("/api/v1/drafts/%d/unpublish", postID)
	resp, err := c.do("POST", path, bytes.NewReader([]byte("{}")))
	if err != nil {
		return fmt.Errorf("unpublishing post: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("unpublish failed: HTTP %d", resp.StatusCode)
	}

	return nil
}

// Publish publishes a draft.
func (c *Client) Publish(draftID int, send bool) (*Post, error) {
	body, err := json.Marshal(PublishRequest{Send: send})
	if err != nil {
		return nil, fmt.Errorf("encoding publish request: %w", err)
	}

	path := fmt.Sprintf("/api/v1/drafts/%d/publish", draftID)
	resp, err := c.do("POST", path, bytes.NewReader(body))
	if err != nil {
		return nil, fmt.Errorf("publishing draft: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("publish failed: HTTP %d", resp.StatusCode)
	}

	var post Post
	if err := json.NewDecoder(resp.Body).Decode(&post); err != nil {
		return nil, fmt.Errorf("decoding publish response: %w", err)
	}

	return &post, nil
}
