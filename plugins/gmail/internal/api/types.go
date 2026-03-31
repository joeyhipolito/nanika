package api

// ThreadSummary is the lightweight form for list operations.
type ThreadSummary struct {
	ID            string   `json:"id"`
	Account       string   `json:"account"`
	Snippet       string   `json:"snippet"`
	From          string   `json:"from"`
	Subject       string   `json:"subject"`
	Date          string   `json:"date"`
	Unread        bool     `json:"unread"`
	HasAttachment bool     `json:"has_attachment"`
	Labels        []string `json:"labels"`
	MessageCount  int      `json:"message_count"`
}

// Thread is the full thread with messages.
type Thread struct {
	ID       string     `json:"id"`
	Account  string     `json:"account"`
	Messages []*Message `json:"messages"`
}

// Message is a single email within a thread.
type Message struct {
	ID       string            `json:"id"`
	ThreadID string            `json:"thread_id"`
	From     string            `json:"from"`
	To       string            `json:"to"`
	Subject  string            `json:"subject"`
	Date     string            `json:"date"`
	Body     string            `json:"body"`
	Snippet  string            `json:"snippet"`
	Labels   []string          `json:"labels"`
	Headers  map[string]string `json:"headers"`
}

// Label represents a Gmail label.
type Label struct {
	ID            string `json:"id"`
	Name          string `json:"name"`
	Type          string `json:"type"`
	ThreadsTotal  int    `json:"threads_total"`
	ThreadsUnread int    `json:"threads_unread"`
}

// FilterCriteria defines matching rules for a Gmail filter.
type FilterCriteria struct {
	From          string `json:"from,omitempty"`
	To            string `json:"to,omitempty"`
	Subject       string `json:"subject,omitempty"`
	Query         string `json:"query,omitempty"`
	NegatedQuery  string `json:"negated_query,omitempty"`
	HasAttachment bool   `json:"has_attachment,omitempty"`
}

// FilterAction defines what a Gmail filter does when it matches.
type FilterAction struct {
	AddLabelIDs    []string `json:"add_label_ids,omitempty"`
	RemoveLabelIDs []string `json:"remove_label_ids,omitempty"`
	Forward        string   `json:"forward,omitempty"`
	Star           bool     `json:"star,omitempty"`
	Archive        bool     `json:"archive,omitempty"`
	MarkRead       bool     `json:"mark_read,omitempty"`
	MarkImportant  bool     `json:"mark_important,omitempty"`
	Trash          bool     `json:"trash,omitempty"`
	NeverSpam      bool     `json:"never_spam,omitempty"`
}

// Filter represents a Gmail filter rule.
type Filter struct {
	ID       string         `json:"id"`
	Criteria FilterCriteria `json:"criteria"`
	Action   FilterAction   `json:"action"`
}

// CalendarEvent is a single Google Calendar event.
type CalendarEvent struct {
	ID          string   `json:"id"`
	Account     string   `json:"account"`
	Summary     string   `json:"summary"`
	Description string   `json:"description,omitempty"`
	Location    string   `json:"location,omitempty"`
	Start       string   `json:"start"` // RFC3339 for timed events; date (YYYY-MM-DD) for all-day
	End         string   `json:"end"`
	AllDay      bool     `json:"all_day"`
	Attendees   []string `json:"attendees,omitempty"`
	HtmlLink    string   `json:"html_link,omitempty"`
	Status      string   `json:"status,omitempty"`
}

// CreateEventParams holds input for creating a calendar event.
type CreateEventParams struct {
	Summary     string
	Description string
	Location    string
	Start       string // RFC3339
	End         string // RFC3339
	TimeZone    string // IANA timezone name (e.g. "America/New_York"); optional
	Attendees   []string
}

// FreeBusySlot represents a single busy time range.
type FreeBusySlot struct {
	Start string `json:"start"` // RFC3339
	End   string `json:"end"`   // RFC3339
}

// FreeBusy is the result of an availability check for an account.
type FreeBusy struct {
	Account   string         `json:"account"`
	TimeMin   string         `json:"time_min"`
	TimeMax   string         `json:"time_max"`
	Available bool           `json:"available"`
	BusySlots []FreeBusySlot `json:"busy_slots"`
}

// DriveFile is a single Google Drive file.
type DriveFile struct {
	ID           string   `json:"id"`
	Account      string   `json:"account"`
	Name         string   `json:"name"`
	MimeType     string   `json:"mime_type"`
	Size         int64    `json:"size,omitempty"`
	ModifiedTime string   `json:"modified_time"`
	WebViewLink  string   `json:"web_view_link,omitempty"`
	Owners       []string `json:"owners,omitempty"`
}
