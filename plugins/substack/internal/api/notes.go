package api

import (
	"bytes"
	"encoding/json"
	"fmt"
	"net/http"
)

// Notes API Endpoints:
//
//   POST   /api/v1/comment/feed                              — Create note or reply
//   GET    /api/v1/reader/comment/{id}/replies?comment_id={id} — Get note + replies
//   DELETE /api/v1/comment/{id}                               — Delete note
//   GET    /api/v1/reader/feed/profile/{uid}?type=note&limit=N — List user's notes
//
// Note URLs: https://substack.com/@{username}/note/c-{id}

// dashboardFeedResponse wraps the /api/v1/reader/feed?tab=for-you&type=base response.
type dashboardFeedResponse struct {
	Items      []dashboardFeedItem `json:"items"`
	NextCursor string              `json:"nextCursor"`
}

type dashboardFeedItem struct {
	EntityKey   string          `json:"entity_key"`
	Type        string          `json:"type"`
	Comment     json.RawMessage `json:"comment"`
	Post        json.RawMessage `json:"post"`
	Publication json.RawMessage `json:"publication"`
	CanReply    bool            `json:"canReply"`
}

type dashboardPublication struct {
	Name string `json:"name"`
}

// GetDashboard returns a reader feed with cursor-based pagination.
// tab can be "for-you" or "subscribed". Fetches multiple pages until limit is reached.
func (c *Client) GetDashboard(limit int, tab string) ([]DashboardItem, error) {
	if tab == "" {
		tab = "for-you"
	}

	const maxPages = 10 // safety cap to avoid infinite loops
	var items []DashboardItem
	cursor := ""

	for page := 0; page < maxPages; page++ {
		path := fmt.Sprintf("/api/v1/reader/feed?tab=%s&type=base", tab)
		if cursor != "" {
			path += "&cursor=" + cursor
		}

		resp, err := c.doGlobal("GET", path, nil)
		if err != nil {
			return nil, fmt.Errorf("fetching dashboard: %w", err)
		}

		if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
			resp.Body.Close()
			return nil, fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
		}
		if resp.StatusCode != http.StatusOK {
			resp.Body.Close()
			return nil, fmt.Errorf("fetching dashboard: HTTP %d", resp.StatusCode)
		}

		var feedResp dashboardFeedResponse
		if err := json.NewDecoder(resp.Body).Decode(&feedResp); err != nil {
			resp.Body.Close()
			return nil, fmt.Errorf("decoding dashboard: %w", err)
		}
		resp.Body.Close()

		for _, raw := range feedResp.Items {
			if raw.Type != "comment" && raw.Type != "post" {
				continue
			}

			item := DashboardItem{
				EntityKey: raw.EntityKey,
				Type:      raw.Type,
				CanReply:  raw.CanReply,
			}

			// Extract publication name
			if raw.Publication != nil {
				var pub dashboardPublication
				if json.Unmarshal(raw.Publication, &pub) == nil {
					item.Publication = pub.Name
				}
			}

			if raw.Type == "comment" && raw.Comment != nil {
				var note Note
				if json.Unmarshal(raw.Comment, &note) == nil {
					item.Note = &note
				}
			}

			if raw.Type == "post" && raw.Post != nil {
				var post Post
				if json.Unmarshal(raw.Post, &post) == nil {
					item.Post = &post
				}
			}

			items = append(items, item)
			if limit > 0 && len(items) >= limit {
				return items, nil
			}
		}

		// No more pages
		if feedResp.NextCursor == "" {
			break
		}
		cursor = feedResp.NextCursor
	}

	return items, nil
}

// ReactToNote adds a ❤ reaction to a note.
func (c *Client) ReactToNote(noteID int) error {
	payload := map[string]any{
		"reaction":       "❤",
		"publication_id": nil,
		"tabId":          "for-you",
	}

	b, err := json.Marshal(payload)
	if err != nil {
		return fmt.Errorf("encoding reaction: %w", err)
	}

	path := fmt.Sprintf("/api/v1/comment/%d/reaction", noteID)
	resp, err := c.doGlobal("POST", path, bytes.NewReader(b))
	if err != nil {
		return fmt.Errorf("reacting to note: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return fmt.Errorf("reacting to note: HTTP %d", resp.StatusCode)
	}

	return nil
}

// notesFeedResponse wraps the /api/v1/reader/feed/profile/{id}?type=note response.
type notesFeedResponse struct {
	Items      []notesFeedItem `json:"items"`
	NextCursor string          `json:"nextCursor"`
}

// notesFeedItem is a single note item from the reader feed.
type notesFeedItem struct {
	EntityKey string          `json:"entity_key"`
	Type      string          `json:"type"`
	Comment   json.RawMessage `json:"comment"`
}

// ListNotes returns notes created by the authenticated user.
func (c *Client) ListNotes(limit int) ([]Note, error) {
	if c.UserID == 0 {
		if _, err := c.GetProfile(); err != nil {
			return nil, fmt.Errorf("getting profile for notes: %w", err)
		}
	}

	path := fmt.Sprintf("/api/v1/reader/feed/profile/%d?type=note&limit=%d", c.UserID, limit)
	resp, err := c.doGlobal("GET", path, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching notes: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching notes: HTTP %d", resp.StatusCode)
	}

	var notesResp notesFeedResponse
	if err := json.NewDecoder(resp.Body).Decode(&notesResp); err != nil {
		return nil, fmt.Errorf("decoding notes: %w", err)
	}

	var notes []Note
	for _, raw := range notesResp.Items {
		if raw.Type != "note" {
			continue
		}
		if raw.Comment == nil {
			continue
		}

		var note Note
		if err := json.Unmarshal(raw.Comment, &note); err != nil {
			continue
		}

		notes = append(notes, note)
	}

	return notes, nil
}

// AttachmentResponse represents the response from the comment/attachment endpoint.
type AttachmentResponse struct {
	ID string `json:"id"`
}

// CreateAttachment registers an uploaded image URL as a note attachment.
// Returns the attachment ID to include in attachmentIds when posting.
func (c *Client) CreateAttachment(imageURL string) (string, error) {
	payload := map[string]any{
		"url":  imageURL,
		"type": "image",
	}

	b, err := json.Marshal(payload)
	if err != nil {
		return "", fmt.Errorf("encoding attachment payload: %w", err)
	}

	resp, err := c.doGlobal("POST", "/api/v1/comment/attachment", bytes.NewReader(b))
	if err != nil {
		return "", fmt.Errorf("creating attachment: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return "", fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return "", fmt.Errorf("creating attachment: HTTP %d", resp.StatusCode)
	}

	var result AttachmentResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return "", fmt.Errorf("decoding attachment response: %w", err)
	}

	if result.ID == "" {
		return "", fmt.Errorf("attachment returned empty ID")
	}

	return result.ID, nil
}

// CreateNote publishes a note to Substack. bodyJSON is a Tiptap/ProseMirror JSON string.
func (c *Client) CreateNote(bodyJSON string, attachmentIDs ...string) (*Note, error) {
	return c.createComment(bodyJSON, 0, attachmentIDs)
}

// ReplyToNote posts a reply to an existing note. parentID is the note ID to reply to.
func (c *Client) ReplyToNote(bodyJSON string, parentID int, attachmentIDs ...string) (*Note, error) {
	return c.createComment(bodyJSON, parentID, attachmentIDs)
}

// createComment is the shared implementation for creating notes and replies.
// Both use POST /api/v1/comment/feed — replies include parent_id.
func (c *Client) createComment(bodyJSON string, parentID int, attachmentIDs []string) (*Note, error) {
	var bodyDoc any
	if err := json.Unmarshal([]byte(bodyJSON), &bodyDoc); err != nil {
		return nil, fmt.Errorf("parsing note body JSON: %w", err)
	}

	payload := map[string]any{
		"bodyJson":         bodyDoc,
		"tabId":            "for-you",
		"replyMinimumRole": "everyone",
	}

	if parentID > 0 {
		payload["parent_id"] = parentID
		payload["surface"] = "permalink"
	} else {
		payload["surface"] = "feed"
	}

	if len(attachmentIDs) > 0 {
		payload["attachmentIds"] = attachmentIDs
	}

	b, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("encoding note payload: %w", err)
	}

	resp, err := c.doGlobal("POST", "/api/v1/comment/feed", bytes.NewReader(b))
	if err != nil {
		return nil, fmt.Errorf("posting note: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusCreated {
		return nil, fmt.Errorf("posting note: HTTP %d", resp.StatusCode)
	}

	var note Note
	if err := json.NewDecoder(resp.Body).Decode(&note); err != nil {
		return nil, fmt.Errorf("decoding note response: %w", err)
	}

	return &note, nil
}

// GetNoteReplies fetches a note and its replies.
func (c *Client) GetNoteReplies(noteID int) (*NoteRepliesResponse, error) {
	path := fmt.Sprintf("/api/v1/reader/comment/%d/replies?comment_id=%d", noteID, noteID)
	resp, err := c.doGlobal("GET", path, nil)
	if err != nil {
		return nil, fmt.Errorf("fetching note replies: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return nil, fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode == http.StatusNotFound {
		return nil, fmt.Errorf("note not found: %d", noteID)
	}
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("fetching note replies: HTTP %d", resp.StatusCode)
	}

	var result NoteRepliesResponse
	if err := json.NewDecoder(resp.Body).Decode(&result); err != nil {
		return nil, fmt.Errorf("decoding note replies: %w", err)
	}

	return &result, nil
}

// DeleteNote deletes a note by ID.
func (c *Client) DeleteNote(noteID int) error {
	path := fmt.Sprintf("/api/v1/comment/%d", noteID)
	resp, err := c.doGlobal("DELETE", path, bytes.NewReader([]byte("{}")))
	if err != nil {
		return fmt.Errorf("deleting note: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode == http.StatusUnauthorized || resp.StatusCode == http.StatusForbidden {
		return fmt.Errorf("session expired. Run 'substack configure --from-browser chrome' to refresh")
	}
	if resp.StatusCode == http.StatusNotFound {
		return fmt.Errorf("note not found: %d", noteID)
	}
	if resp.StatusCode != http.StatusOK && resp.StatusCode != http.StatusNoContent {
		return fmt.Errorf("deleting note: HTTP %d", resp.StatusCode)
	}

	return nil
}
