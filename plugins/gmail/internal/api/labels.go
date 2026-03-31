package api

import (
	"fmt"
	"strings"

	"google.golang.org/api/gmail/v1"
)

// ListLabels returns all labels for this account.
func (c *Client) ListLabels() ([]Label, error) {
	var resp *gmail.ListLabelsResponse
	if err := withRetry(func() error {
		var e error
		resp, e = c.svc.Users.Labels.List(gmailUserID).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("failed to list labels: %w", err)
	}

	labels := make([]Label, 0, len(resp.Labels))
	for _, l := range resp.Labels {
		label := Label{
			ID:   l.Id,
			Name: l.Name,
			Type: l.Type,
		}
		if l.ThreadsTotal > 0 || l.ThreadsUnread > 0 {
			label.ThreadsTotal = int(l.ThreadsTotal)
			label.ThreadsUnread = int(l.ThreadsUnread)
		}
		labels = append(labels, label)
	}

	return labels, nil
}

// CreateLabel creates a new user label.
func (c *Client) CreateLabel(name string) (*Label, error) {
	gl := &gmail.Label{
		Name:                  name,
		LabelListVisibility:   "labelShow",
		MessageListVisibility: "show",
	}

	var created *gmail.Label
	if err := withRetry(func() error {
		var e error
		created, e = c.svc.Users.Labels.Create(gmailUserID, gl).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("failed to create label %q: %w", name, err)
	}

	return &Label{
		ID:   created.Id,
		Name: created.Name,
		Type: created.Type,
	}, nil
}

// GetLabelID returns the label ID for a given label name. Case-insensitive match.
func (c *Client) GetLabelID(name string) (string, error) {
	labels, err := c.ListLabels()
	if err != nil {
		return "", err
	}

	lower := strings.ToLower(name)
	for _, l := range labels {
		if strings.ToLower(l.Name) == lower {
			return l.ID, nil
		}
	}

	return "", fmt.Errorf("label %q not found", name)
}

// EnsureLabel returns the label ID for name, creating it if it doesn't exist.
func (c *Client) EnsureLabel(name string) (string, error) {
	id, err := c.GetLabelID(name)
	if err == nil {
		return id, nil
	}

	label, err := c.CreateLabel(name)
	if err != nil {
		return "", err
	}

	return label.ID, nil
}
