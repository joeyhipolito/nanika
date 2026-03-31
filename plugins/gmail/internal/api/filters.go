package api

import (
	"fmt"
	"strings"

	"google.golang.org/api/gmail/v1"
)

// ListFilters returns all filters for this account, with label IDs resolved to names.
func (c *Client) ListFilters() ([]Filter, error) {
	var resp *gmail.ListFiltersResponse
	if err := withRetry(func() error {
		var e error
		resp, e = c.svc.Users.Settings.Filters.List(gmailUserID).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("failed to list filters: %w", err)
	}

	// Build label ID→name map for resolving action labels.
	labelMap, err := c.labelIDMap()
	if err != nil {
		return nil, err
	}

	filters := make([]Filter, 0, len(resp.Filter))
	for _, f := range resp.Filter {
		filters = append(filters, convertFilter(f, labelMap))
	}

	return filters, nil
}

// GetFilter returns a single filter by ID, with label IDs resolved to names.
func (c *Client) GetFilter(id string) (*Filter, error) {
	var f *gmail.Filter
	if err := withRetry(func() error {
		var e error
		f, e = c.svc.Users.Settings.Filters.Get(gmailUserID, id).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("failed to get filter %s: %w", id, err)
	}

	labelMap, err := c.labelIDMap()
	if err != nil {
		return nil, err
	}

	filter := convertFilter(f, labelMap)
	return &filter, nil
}

// CreateFilter creates a new filter. Label names in the action are resolved to IDs
// (creating labels as needed). Boolean actions (Archive, Star, MarkRead, etc.) are
// translated to the appropriate addLabelIds/removeLabelIds entries.
func (c *Client) CreateFilter(criteria FilterCriteria, action FilterAction) (*Filter, error) {
	gf := &gmail.Filter{
		Criteria: &gmail.FilterCriteria{
			From:          criteria.From,
			To:            criteria.To,
			Subject:       criteria.Subject,
			Query:         criteria.Query,
			NegatedQuery:  criteria.NegatedQuery,
			HasAttachment: criteria.HasAttachment,
		},
		Action: &gmail.FilterAction{},
	}

	// Resolve label names → IDs.
	var addIDs, removeIDs []string

	for _, name := range action.AddLabelIDs {
		id, err := c.EnsureLabel(name)
		if err != nil {
			return nil, fmt.Errorf("resolve label %q: %w", name, err)
		}
		addIDs = append(addIDs, id)
	}

	// Translate boolean actions to label operations.
	if action.Star {
		addIDs = append(addIDs, "STARRED")
	}
	if action.MarkImportant {
		addIDs = append(addIDs, "IMPORTANT")
	}
	if action.Trash {
		addIDs = append(addIDs, "TRASH")
	}
	if action.Archive {
		removeIDs = append(removeIDs, labelInbox)
	}
	if action.MarkRead {
		removeIDs = append(removeIDs, labelUnread)
	}
	if action.NeverSpam {
		removeIDs = append(removeIDs, "SPAM")
	}

	for _, name := range action.RemoveLabelIDs {
		id, err := c.GetLabelID(name)
		if err != nil {
			return nil, fmt.Errorf("resolve label %q for removal: %w", name, err)
		}
		removeIDs = append(removeIDs, id)
	}

	gf.Action.AddLabelIds = addIDs
	gf.Action.RemoveLabelIds = removeIDs
	if action.Forward != "" {
		gf.Action.Forward = action.Forward
	}

	var created *gmail.Filter
	if err := withRetry(func() error {
		var e error
		created, e = c.svc.Users.Settings.Filters.Create(gmailUserID, gf).Do()
		return e
	}); err != nil {
		return nil, fmt.Errorf("failed to create filter: %w", err)
	}

	labelMap, err := c.labelIDMap()
	if err != nil {
		return nil, err
	}

	filter := convertFilter(created, labelMap)
	return &filter, nil
}

// DeleteFilter removes a filter by ID.
func (c *Client) DeleteFilter(id string) error {
	if err := withRetry(func() error {
		return c.svc.Users.Settings.Filters.Delete(gmailUserID, id).Do()
	}); err != nil {
		return fmt.Errorf("failed to delete filter %s: %w", id, err)
	}
	return nil
}

// labelIDMap builds a map from label ID to label name.
func (c *Client) labelIDMap() (map[string]string, error) {
	labels, err := c.ListLabels()
	if err != nil {
		return nil, fmt.Errorf("list labels for ID resolution: %w", err)
	}
	m := make(map[string]string, len(labels))
	for _, l := range labels {
		m[l.ID] = l.Name
	}
	return m, nil
}

// convertFilter translates a Gmail API filter into our Filter type,
// resolving label IDs to human-readable names.
func convertFilter(gf *gmail.Filter, labelMap map[string]string) Filter {
	f := Filter{ID: gf.Id}

	if gf.Criteria != nil {
		f.Criteria = FilterCriteria{
			From:          gf.Criteria.From,
			To:            gf.Criteria.To,
			Subject:       gf.Criteria.Subject,
			Query:         gf.Criteria.Query,
			NegatedQuery:  gf.Criteria.NegatedQuery,
			HasAttachment: gf.Criteria.HasAttachment,
		}
	}

	if gf.Action != nil {
		// Resolve add label IDs, separating system actions from user labels.
		for _, id := range gf.Action.AddLabelIds {
			switch id {
			case "STARRED":
				f.Action.Star = true
			case "IMPORTANT":
				f.Action.MarkImportant = true
			case "TRASH":
				f.Action.Trash = true
			default:
				name := resolveLabel(id, labelMap)
				f.Action.AddLabelIDs = append(f.Action.AddLabelIDs, name)
			}
		}

		// Resolve remove label IDs, separating system actions from user labels.
		for _, id := range gf.Action.RemoveLabelIds {
			switch id {
			case labelInbox:
				f.Action.Archive = true
			case labelUnread:
				f.Action.MarkRead = true
			case "SPAM":
				f.Action.NeverSpam = true
			default:
				name := resolveLabel(id, labelMap)
				f.Action.RemoveLabelIDs = append(f.Action.RemoveLabelIDs, name)
			}
		}

		f.Action.Forward = gf.Action.Forward
	}

	return f
}

// resolveLabel returns the label name for an ID, falling back to the raw ID.
func resolveLabel(id string, labelMap map[string]string) string {
	if name, ok := labelMap[id]; ok {
		return name
	}
	return id
}

// FormatFilterSummary returns a one-line human-readable summary of a filter.
func FormatFilterSummary(f Filter) string {
	var parts []string

	// Criteria
	if f.Criteria.From != "" {
		parts = append(parts, fmt.Sprintf("from:%s", f.Criteria.From))
	}
	if f.Criteria.To != "" {
		parts = append(parts, fmt.Sprintf("to:%s", f.Criteria.To))
	}
	if f.Criteria.Subject != "" {
		parts = append(parts, fmt.Sprintf("subject:%q", f.Criteria.Subject))
	}
	if f.Criteria.Query != "" {
		parts = append(parts, fmt.Sprintf("query:%q", f.Criteria.Query))
	}
	if f.Criteria.HasAttachment {
		parts = append(parts, "has:attachment")
	}

	criteria := strings.Join(parts, " ")

	// Actions
	var actions []string
	for _, l := range f.Action.AddLabelIDs {
		actions = append(actions, fmt.Sprintf("+label:%s", l))
	}
	if f.Action.Star {
		actions = append(actions, "star")
	}
	if f.Action.Archive {
		actions = append(actions, "archive")
	}
	if f.Action.MarkRead {
		actions = append(actions, "mark-read")
	}
	if f.Action.MarkImportant {
		actions = append(actions, "important")
	}
	if f.Action.Trash {
		actions = append(actions, "trash")
	}
	if f.Action.NeverSpam {
		actions = append(actions, "never-spam")
	}
	if f.Action.Forward != "" {
		actions = append(actions, fmt.Sprintf("forward:%s", f.Action.Forward))
	}
	for _, l := range f.Action.RemoveLabelIDs {
		actions = append(actions, fmt.Sprintf("-label:%s", l))
	}

	actionStr := strings.Join(actions, ", ")

	return fmt.Sprintf("%s → %s", criteria, actionStr)
}
