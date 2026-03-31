package api

import "encoding/json"

// User represents the authenticated Substack user.
type User struct {
	ID    int    `json:"id"`
	Name  string `json:"name"`
	Email string `json:"email"`
}

// PostTag represents a tag attached to a Substack post.
// Tags are managed via /api/v1/publication/post-tag (create/list)
// and /api/v1/post/{postId}/tag/{tagId} (assign to post).
type PostTag struct {
	ID         string `json:"id"`
	Name       string `json:"name"`
	Slug       string `json:"slug,omitempty"`
	PostCount  int    `json:"post_count,omitempty"`
	IsInternal bool   `json:"is_internal,omitempty"`
}

// Post represents a Substack post (draft or published).
type Post struct {
	ID              int       `json:"id"`
	Title           string    `json:"title"`
	Subtitle        string    `json:"subtitle"`
	Slug            string    `json:"slug"`
	Type            string    `json:"type"`
	Audience        string    `json:"audience"`
	CanonicalURL    string    `json:"canonical_url"`
	PostDate        string    `json:"post_date"`
	IsDraft         bool      `json:"draft"`
	IsPublished     bool      `json:"is_published"`
	WordCount       int       `json:"word_count"`
	ReactionCount   int       `json:"reaction_count"`
	CommentCount    int       `json:"comment_count"`
	ChildComments   int       `json:"child_comment_count"`
	Description     string    `json:"description"`
	CoverImage      string    `json:"cover_image"`
	PublishDate     string    `json:"first_published_at"`
	EmailSentAt     string    `json:"email_sent_at"`
	WrittenAt       string    `json:"written_at"`
	PostTags        []PostTag `json:"postTags,omitempty"`
}

// UnmarshalJSON handles both published posts (title/subtitle) and drafts (draft_title/draft_subtitle).
func (p *Post) UnmarshalJSON(data []byte) error {
	type Alias Post
	aux := &struct {
		*Alias
		DraftTitle    string `json:"draft_title"`
		DraftSubtitle string `json:"draft_subtitle"`
	}{Alias: (*Alias)(p)}

	if err := json.Unmarshal(data, aux); err != nil {
		return err
	}

	if p.Title == "" && aux.DraftTitle != "" {
		p.Title = aux.DraftTitle
	}
	if p.Subtitle == "" && aux.DraftSubtitle != "" {
		p.Subtitle = aux.DraftSubtitle
	}
	return nil
}

// DraftRequest represents the payload for creating a draft.
type DraftRequest struct {
	DraftTitle    string        `json:"draft_title"`
	DraftSubtitle string        `json:"draft_subtitle,omitempty"`
	DraftBody     string        `json:"draft_body"`
	DraftBylines  []DraftByline `json:"draft_bylines"`
	Type          string        `json:"type"`
	Audience      string        `json:"audience"`
	EditorV2      bool          `json:"editor_v2"`
}

// DraftByline identifies the author of a draft.
type DraftByline struct {
	ID int `json:"id"`
}

// DraftUpdateRequest represents the payload for updating a draft/post body via PUT.
type DraftUpdateRequest struct {
	DraftTitle    string        `json:"draft_title"`
	DraftSubtitle string        `json:"draft_subtitle"`
	DraftBody     string        `json:"draft_body"`
	DraftBylines  []DraftByline `json:"draft_bylines"`
}

// PublishRequest represents the payload for publishing a draft.
type PublishRequest struct {
	Send bool `json:"send"`
}

// PrepublishResponse represents the response from the prepublish endpoint.
type PrepublishResponse struct {
	OK bool `json:"ok"`
}

// PrePublishResult holds subscriber counts and validation warnings returned by
// GET /api/v1/drafts/{id}/prepublish?publish_date=... before scheduling a release.
type PrePublishResult struct {
	FreeSubscriberCount  int      `json:"free_subscriber_count"`
	PaidSubscriberCount  int      `json:"paid_subscriber_count"`
	EmailSubscriberCount int      `json:"email_subscriber_count"`
	Warnings             []string `json:"warnings"`
}

// ScheduleReleaseRequest is the payload for POST /api/v1/drafts/{id}/scheduled_release.
type ScheduleReleaseRequest struct {
	TriggerAt     string `json:"trigger_at"`
	PostAudience  string `json:"post_audience"`
	EmailAudience string `json:"email_audience"`
}

// Subscription represents a publication the user subscribes to.
type Subscription struct {
	ID          int    `json:"id"`
	Subdomain   string `json:"custom_domain_optional"`
	Name        string `json:"name"`
	AuthorName  string `json:"author_name"`
	BaseURL     string `json:"base_url"`
	CustomDomain string `json:"custom_domain"`
}

// FeedItem represents a post from the reader feed or a publication archive.
type FeedItem struct {
	ID              int            `json:"id"`
	Title           string         `json:"title"`
	Subtitle        string         `json:"subtitle"`
	Slug            string         `json:"slug"`
	PostDate        string         `json:"post_date"`
	CanonicalURL    string         `json:"canonical_url"`
	Audience        string         `json:"audience"`
	CommentCount    int            `json:"comment_count"`
	Reactions       map[string]int `json:"reactions"`
	PublishedBylines []Byline      `json:"publishedBylines"`
	PublicationName string         `json:"-"`
}

// TotalReactions returns the sum of all reaction counts.
func (f *FeedItem) TotalReactions() int {
	total := 0
	for _, v := range f.Reactions {
		total += v
	}
	return total
}

// AuthorName returns the first byline name or empty string.
func (f *FeedItem) AuthorName() string {
	if len(f.PublishedBylines) > 0 {
		return f.PublishedBylines[0].Name
	}
	return ""
}

// Byline represents an author byline on a post.
type Byline struct {
	ID   int    `json:"id"`
	Name string `json:"name"`
}

// Comment represents a comment on a post.
type Comment struct {
	ID        int       `json:"id"`
	Body      string    `json:"body"`
	BodyJSON  any       `json:"body_json"`
	Name      string    `json:"name"`
	UserID    int       `json:"user_id"`
	Date      string    `json:"date"`
	EditedAt  string    `json:"edited_at"`
	Reactions map[string]int `json:"reactions"`
	Children  []Comment `json:"children"`
}

// TotalReactions returns the sum of all reaction counts on a comment.
func (c *Comment) TotalReactions() int {
	total := 0
	for _, v := range c.Reactions {
		total += v
	}
	return total
}

// Note represents a user note (comment on the feed).
type Note struct {
	ID               int            `json:"id"`
	Body             string         `json:"body"`
	BodyJSON         any            `json:"body_json"`
	Date             string         `json:"date"`
	Name             string         `json:"name"`
	UserID           int            `json:"user_id"`
	PhotoURL         string         `json:"photo_url"`
	ReactionCount    int            `json:"reaction_count"`
	Reactions        map[string]int `json:"reactions"`
	ChildrenCount    int            `json:"children_count"`
	AncestorPath     string         `json:"ancestor_path"`
	ReplyMinimumRole string         `json:"reply_minimum_role"`
}

// DashboardItem represents an item from the for-you feed (note or post).
type DashboardItem struct {
	EntityKey   string `json:"entity_key"`
	Type        string `json:"type"` // "comment" (note), "post", "userSuggestions"
	Note        *Note  `json:"-"`    // populated for type=comment
	Post        *Post  `json:"-"`    // populated for type=post
	Publication string `json:"-"`    // publication name
	CanReply    bool   `json:"canReply"`
}

// NoteRepliesResponse is the response from GET /api/v1/reader/comment/{id}/replies.
type NoteRepliesResponse struct {
	RootComment    Note             `json:"rootComment"`
	CommentBranches []CommentBranch `json:"commentBranches"`
	MoreBranches   int              `json:"moreBranches"`
}

// CommentBranch is a reply and its descendants.
type CommentBranch struct {
	Comment            Note   `json:"comment"`
	DescendantComments []Note `json:"descendantComments"`
}
