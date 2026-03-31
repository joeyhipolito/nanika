package api

// Gmail API constants for system labels, user IDs, and MIME types.
// Using constants avoids scattered magic strings and typo-related bugs.
const (
	// gmailUserID is the special user ID used in Gmail API calls to represent
	// the authenticated user.
	gmailUserID = "me"

	// labelInbox is the Gmail system label ID for inbox threads.
	labelInbox = "INBOX"

	// labelUnread is the Gmail system label ID for unread threads.
	labelUnread = "UNREAD"

	// mimeTextPlain is the MIME type for plain-text message parts.
	mimeTextPlain = "text/plain"

	// mimeTextHTML is the MIME type for HTML message parts.
	mimeTextHTML = "text/html"

	// mimeMultipartPrefix is the prefix shared by all multipart MIME types.
	mimeMultipartPrefix = "multipart/"
)
