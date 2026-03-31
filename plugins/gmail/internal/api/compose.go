package api

import (
	"encoding/base64"
	"fmt"
	"strings"
	"time"

	"google.golang.org/api/gmail/v1"
)

// ComposeParams holds parameters for composing an email.
type ComposeParams struct {
	To      string
	CC      string
	BCC     string
	Subject string
	Body    string // plain text body
	HTML    string // optional HTML body; if set, sends multipart/alternative
}

// SentMessage is the result of a send operation.
type SentMessage struct {
	ID       string `json:"id"`
	ThreadID string `json:"thread_id"`
	Account  string `json:"account"`
}

// DraftSummary is a lightweight draft representation.
type DraftSummary struct {
	ID      string `json:"id"`
	Subject string `json:"subject"`
	To      string `json:"to"`
	Snippet string `json:"snippet"`
	Date    string `json:"date"`
	Account string `json:"account"`
}

// Send composes and sends a new email message.
func (c *Client) Send(p ComposeParams) (*SentMessage, error) {
	raw, err := buildMIME(p, "", "", "")
	if err != nil {
		return nil, err
	}

	msg := &gmail.Message{Raw: raw}
	var sent *gmail.Message
	if err := withRetry(func() error {
		var e error
		sent, e = c.svc.Users.Messages.Send(gmailUserID, msg).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("send message: %w", err)
	}

	return &SentMessage{ID: sent.Id, ThreadID: sent.ThreadId, Account: c.alias}, nil
}

// Reply sends a reply to an existing thread.
// It fetches the thread to populate In-Reply-To, References, and To headers automatically.
func (c *Client) Reply(threadID, body string) (*SentMessage, error) {
	thread, err := c.GetThread(threadID)
	if err != nil {
		return nil, fmt.Errorf("fetch thread for reply: %w", err)
	}
	if len(thread.Messages) == 0 {
		return nil, fmt.Errorf("thread %s has no messages", threadID)
	}

	last := thread.Messages[len(thread.Messages)-1]

	// Reply goes to the original sender.
	replyTo := last.From

	// Prepend "Re: " if not already present.
	subject := last.Subject
	if !strings.HasPrefix(strings.ToLower(subject), "re:") {
		subject = "Re: " + subject
	}

	// Extract threading headers for proper conversation threading.
	msgID := headerVal(last.Headers, "Message-ID")
	if msgID == "" {
		msgID = headerVal(last.Headers, "Message-Id")
	}
	refs := headerVal(last.Headers, "References")
	if refs == "" {
		refs = msgID
	} else if msgID != "" {
		refs = refs + " " + msgID
	}

	p := ComposeParams{
		To:      replyTo,
		Subject: subject,
		Body:    body,
	}

	raw, err := buildMIME(p, threadID, msgID, refs)
	if err != nil {
		return nil, err
	}

	msg := &gmail.Message{
		Raw:      raw,
		ThreadId: threadID,
	}

	var sent *gmail.Message
	if err := withRetry(func() error {
		var e error
		sent, e = c.svc.Users.Messages.Send(gmailUserID, msg).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("send reply: %w", err)
	}

	return &SentMessage{ID: sent.Id, ThreadID: sent.ThreadId, Account: c.alias}, nil
}

// CreateDraft creates a new draft message.
func (c *Client) CreateDraft(p ComposeParams) (*DraftSummary, error) {
	raw, err := buildMIME(p, "", "", "")
	if err != nil {
		return nil, err
	}

	draft := &gmail.Draft{
		Message: &gmail.Message{Raw: raw},
	}

	var created *gmail.Draft
	if err := withRetry(func() error {
		var e error
		created, e = c.svc.Users.Drafts.Create(gmailUserID, draft).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("create draft: %w", err)
	}

	result := &DraftSummary{
		ID:      created.Id,
		Subject: p.Subject,
		To:      p.To,
		Account: c.alias,
	}
	if created.Message != nil {
		result.Snippet = created.Message.Snippet
	}

	return result, nil
}

// ListDrafts returns all drafts for the account with metadata.
func (c *Client) ListDrafts() ([]DraftSummary, error) {
	var listResp *gmail.ListDraftsResponse
	if err := withRetry(func() error {
		var e error
		listResp, e = c.svc.Users.Drafts.List(gmailUserID).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("list drafts: %w", err)
	}

	var drafts []DraftSummary
	for _, d := range listResp.Drafts {
		var full *gmail.Draft
		if err := withRetry(func() error {
			var e error
			full, e = c.svc.Users.Drafts.Get(gmailUserID, d.Id).Format("metadata").Do()
			return e
		}); err != nil {
			// Skip drafts we can't hydrate rather than failing the entire list.
			continue
		}

		ds := DraftSummary{
			ID:      full.Id,
			Account: c.alias,
		}
		if full.Message != nil {
			ds.Snippet = full.Message.Snippet
			if full.Message.Payload != nil {
				for _, h := range full.Message.Payload.Headers {
					switch strings.ToLower(h.Name) {
					case "subject":
						ds.Subject = h.Value
					case "to":
						ds.To = h.Value
					case "date":
						ds.Date = h.Value
					}
				}
			}
		}

		drafts = append(drafts, ds)
	}

	return drafts, nil
}

// SendDraft sends an existing draft by ID.
func (c *Client) SendDraft(draftID string) (*SentMessage, error) {
	draft := &gmail.Draft{Id: draftID}
	var sent *gmail.Message
	if err := withRetry(func() error {
		var e error
		sent, e = c.svc.Users.Drafts.Send(gmailUserID, draft).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("send draft %s: %w", draftID, err)
	}

	return &SentMessage{ID: sent.Id, ThreadID: sent.ThreadId, Account: c.alias}, nil
}

// buildMIME constructs a MIME email and returns it base64url-encoded (URL-safe, padded).
// inReplyTo and references are optional threading headers used for replies.
func buildMIME(p ComposeParams, _ string, inReplyTo, references string) (string, error) {
	if p.To == "" {
		return "", fmt.Errorf("To address is required")
	}

	var sb strings.Builder

	sb.WriteString("To: " + p.To + "\r\n")
	if p.CC != "" {
		sb.WriteString("Cc: " + p.CC + "\r\n")
	}
	if p.BCC != "" {
		sb.WriteString("Bcc: " + p.BCC + "\r\n")
	}
	sb.WriteString("Subject: " + p.Subject + "\r\n")
	if inReplyTo != "" {
		sb.WriteString("In-Reply-To: " + inReplyTo + "\r\n")
	}
	if references != "" {
		sb.WriteString("References: " + references + "\r\n")
	}
	sb.WriteString("Date: " + time.Now().UTC().Format(time.RFC1123Z) + "\r\n")
	sb.WriteString("MIME-Version: 1.0\r\n")

	if p.HTML != "" {
		// multipart/alternative with both plain-text and HTML parts.
		boundary := fmt.Sprintf("----=_Part_%d", time.Now().UnixNano())
		sb.WriteString("Content-Type: multipart/alternative; boundary=\"" + boundary + "\"\r\n")
		sb.WriteString("\r\n")

		sb.WriteString("--" + boundary + "\r\n")
		sb.WriteString("Content-Type: text/plain; charset=utf-8\r\n")
		sb.WriteString("\r\n")
		sb.WriteString(p.Body + "\r\n")

		sb.WriteString("--" + boundary + "\r\n")
		sb.WriteString("Content-Type: text/html; charset=utf-8\r\n")
		sb.WriteString("\r\n")
		sb.WriteString(p.HTML + "\r\n")

		sb.WriteString("--" + boundary + "--\r\n")
	} else {
		sb.WriteString("Content-Type: text/plain; charset=utf-8\r\n")
		sb.WriteString("\r\n")
		sb.WriteString(p.Body + "\r\n")
	}

	return base64.URLEncoding.EncodeToString([]byte(sb.String())), nil
}

// headerVal performs a case-insensitive lookup in a message headers map.
func headerVal(headers map[string]string, name string) string {
	for k, v := range headers {
		if strings.EqualFold(k, name) {
			return v
		}
	}
	return ""
}
