package api

import (
	"encoding/base64"
	"fmt"
	"strings"

	"google.golang.org/api/gmail/v1"
)

// ListInbox returns inbox threads for this account.
// Supports limit and unread-only filter.
func (c *Client) ListInbox(limit int, unreadOnly bool) ([]ThreadSummary, error) {
	call := c.svc.Users.Threads.List(gmailUserID).LabelIds(labelInbox)
	if unreadOnly {
		call = call.Q("is:unread")
	}
	if limit > 0 {
		call = call.MaxResults(int64(limit))
	}

	var resp *gmail.ListThreadsResponse
	if err := withRetry(func() error {
		var e error
		resp, e = call.Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("list inbox threads: %w", err)
	}

	return c.hydrateThreadSummaries(resp.Threads)
}

// GetThread returns a full thread with decoded message bodies.
func (c *Client) GetThread(threadID string) (*Thread, error) {
	var t *gmail.Thread
	if err := withRetry(func() error {
		var e error
		t, e = c.svc.Users.Threads.Get(gmailUserID, threadID).Format("full").Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("get thread %s: %w", threadID, err)
	}

	thread := &Thread{
		ID:      t.Id,
		Account: c.alias,
	}

	for _, msg := range t.Messages {
		m := convertMessage(msg)
		thread.Messages = append(thread.Messages, m)
	}

	return thread, nil
}

// Search returns threads matching a Gmail query.
func (c *Client) Search(query string, limit int) ([]ThreadSummary, error) {
	call := c.svc.Users.Threads.List(gmailUserID).Q(query)
	if limit > 0 {
		call = call.MaxResults(int64(limit))
	}

	var resp *gmail.ListThreadsResponse
	if err := withRetry(func() error {
		var e error
		resp, e = call.Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("search threads: %w", err)
	}

	return c.hydrateThreadSummaries(resp.Threads)
}

// ModifyThread adds/removes label IDs on a thread.
func (c *Client) ModifyThread(threadID string, addLabels, removeLabels []string) error {
	req := &gmail.ModifyThreadRequest{
		AddLabelIds:    addLabels,
		RemoveLabelIds: removeLabels,
	}
	if err := withRetry(func() error {
		_, e := c.svc.Users.Threads.Modify(gmailUserID, threadID, req).Do()
		return e
	}); err != nil {
		return fmt.Errorf("modify thread %s: %w", threadID, err)
	}
	return nil
}

// TrashThread moves a thread to trash.
func (c *Client) TrashThread(threadID string) error {
	if err := withRetry(func() error {
		_, e := c.svc.Users.Threads.Trash(gmailUserID, threadID).Do()
		return e
	}); err != nil {
		return fmt.Errorf("trash thread %s: %w", threadID, err)
	}
	return nil
}

// hydrateThreadSummaries takes the lightweight thread list response and fetches
// metadata for each thread to build ThreadSummary values.
func (c *Client) hydrateThreadSummaries(threads []*gmail.Thread) ([]ThreadSummary, error) {
	summaries := make([]ThreadSummary, 0, len(threads))

	for _, t := range threads {
		var full *gmail.Thread
		if err := withRetry(func() error {
			var e error
			full, e = c.svc.Users.Threads.Get(gmailUserID, t.Id).
				Format("metadata").
				MetadataHeaders("From", "Subject", "Date").
				Do()
			return e
		}); err != nil {
			return nil, fmt.Errorf("get thread metadata %s: %w", t.Id, err)
		}

		summary := ThreadSummary{
			ID:           full.Id,
			Account:      c.alias,
			Snippet:      full.Snippet,
			MessageCount: len(full.Messages),
		}

		// Use headers from the first message in the thread.
		if len(full.Messages) > 0 {
			first := full.Messages[0]
			summary.From = extractHeader(first, "From")
			summary.Subject = extractHeader(first, "Subject")
			summary.Date = extractHeader(first, "Date")
			summary.Unread = containsLabel(first.LabelIds, labelUnread)
			summary.HasAttachment = hasAttachment(first)
			summary.Labels = first.LabelIds
		}

		summaries = append(summaries, summary)
	}

	return summaries, nil
}

// convertMessage converts a Gmail API message to our Message type,
// decoding the body from the MIME tree.
func convertMessage(msg *gmail.Message) *Message {
	m := &Message{
		ID:       msg.Id,
		ThreadID: msg.ThreadId,
		Snippet:  msg.Snippet,
		Labels:   msg.LabelIds,
		Headers:  make(map[string]string),
	}

	// Extract all headers into the map and set named fields.
	if msg.Payload != nil {
		for _, h := range msg.Payload.Headers {
			m.Headers[h.Name] = h.Value
		}
	}
	m.From = m.Headers["From"]
	m.To = m.Headers["To"]
	m.Subject = m.Headers["Subject"]
	m.Date = m.Headers["Date"]

	// Decode body from MIME tree.
	m.Body = decodeBody(msg.Payload)

	return m
}

// decodeBody walks the MIME tree to find and decode the text/plain body.
// Falls back to text/html if no plain text part is found.
func decodeBody(payload *gmail.MessagePart) string {
	if payload == nil {
		return ""
	}

	// If there are no parts, the body is directly on the payload.
	if len(payload.Parts) == 0 {
		if payload.Body != nil && payload.Body.Data != "" {
			decoded, err := base64.URLEncoding.DecodeString(payload.Body.Data)
			if err != nil {
				// Gmail uses unpadded base64url; try raw encoding.
				decoded, err = base64.RawURLEncoding.DecodeString(payload.Body.Data)
				if err != nil {
					return ""
				}
			}
			return string(decoded)
		}
		return ""
	}

	// Walk parts: prefer text/plain, fall back to text/html.
	var htmlBody string
	for _, part := range payload.Parts {
		mime := strings.ToLower(part.MimeType)
		switch {
		case mime == mimeTextPlain:
			body := decodePartData(part)
			if body != "" {
				return body
			}
		case mime == mimeTextHTML:
			if htmlBody == "" {
				htmlBody = decodePartData(part)
			}
		case strings.HasPrefix(mime, mimeMultipartPrefix):
			// Recurse into nested multipart.
			body := decodeBody(part)
			if body != "" {
				return body
			}
		}
	}

	return htmlBody
}

// decodePartData decodes the base64url data from a message part.
func decodePartData(part *gmail.MessagePart) string {
	if part.Body == nil || part.Body.Data == "" {
		return ""
	}
	decoded, err := base64.URLEncoding.DecodeString(part.Body.Data)
	if err != nil {
		decoded, err = base64.RawURLEncoding.DecodeString(part.Body.Data)
		if err != nil {
			return ""
		}
	}
	return string(decoded)
}

// extractHeader returns the value of the named header from a message,
// or empty string if not found.
func extractHeader(msg *gmail.Message, name string) string {
	if msg.Payload == nil {
		return ""
	}
	for _, h := range msg.Payload.Headers {
		if strings.EqualFold(h.Name, name) {
			return h.Value
		}
	}
	return ""
}

// containsLabel checks if a label ID is present in the list.
func containsLabel(labels []string, target string) bool {
	for _, l := range labels {
		if l == target {
			return true
		}
	}
	return false
}

// hasAttachment checks if a message has any attachment parts.
func hasAttachment(msg *gmail.Message) bool {
	if msg.Payload == nil {
		return false
	}
	return checkPartsForAttachment(msg.Payload.Parts)
}

// checkPartsForAttachment recursively checks MIME parts for attachments.
func checkPartsForAttachment(parts []*gmail.MessagePart) bool {
	for _, part := range parts {
		if part.Filename != "" {
			return true
		}
		if checkPartsForAttachment(part.Parts) {
			return true
		}
	}
	return false
}
