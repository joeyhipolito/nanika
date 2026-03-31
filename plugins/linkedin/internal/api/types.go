package api

// Post represents a LinkedIn post from the Official API.
type Post struct {
	ID                        string       `json:"id"`
	Author                    string       `json:"author"`
	Commentary                string       `json:"commentary"`
	Visibility                string       `json:"visibility"`
	LifecycleState            string       `json:"lifecycleState"`
	CreatedAt                 int64        `json:"createdAt"`
	LastModifiedAt            int64        `json:"lastModifiedAt"`
	PublishedAt               int64        `json:"publishedAt"`
	Distribution              Distribution `json:"distribution"`
	Content                   *Content     `json:"content,omitempty"`
	IsReshareDisabledByAuthor bool         `json:"isReshareDisabledByAuthor"`
}

// Distribution controls how a post is distributed.
type Distribution struct {
	FeedDistribution               string `json:"feedDistribution"`
	TargetEntities                 []any  `json:"targetEntities,omitempty"`
	ThirdPartyDistributionChannels []any  `json:"thirdPartyDistributionChannels,omitempty"`
}

// Content holds optional media content for a post.
type Content struct {
	Media *MediaContent `json:"media,omitempty"`
}

// MediaContent holds media metadata for a post.
type MediaContent struct {
	Title string `json:"title,omitempty"`
	ID    string `json:"id"`
}

// PostsResponse wraps the paginated list of posts.
type PostsResponse struct {
	Paging   Paging `json:"paging"`
	Elements []Post `json:"elements"`
}

// Paging holds pagination metadata.
type Paging struct {
	Start int    `json:"start"`
	Count int    `json:"count"`
	Links []Link `json:"links,omitempty"`
}

// Link represents a pagination link.
type Link struct {
	Rel  string `json:"rel"`
	Href string `json:"href"`
	Type string `json:"type"`
}

// CreatePostRequest for POST /rest/posts.
type CreatePostRequest struct {
	Author                    string       `json:"author"`
	Commentary                string       `json:"commentary"`
	Visibility                string       `json:"visibility"`
	Distribution              Distribution `json:"distribution"`
	LifecycleState            string       `json:"lifecycleState"`
	IsReshareDisabledByAuthor bool         `json:"isReshareDisabledByAuthor"`
	Content                   *Content     `json:"content,omitempty"`
}

// CreateCommentRequest for POST /rest/socialActions/{urn}/comments.
type CreateCommentRequest struct {
	Actor   string         `json:"actor"`
	Message CommentMessage `json:"message"`
}

// CommentMessage holds the text of a comment.
type CommentMessage struct {
	Text string `json:"text"`
}

// CreateReactionRequest for POST /rest/reactions.
type CreateReactionRequest struct {
	Root         string `json:"root"`
	ReactionType string `json:"reactionType"`
	Actor        string `json:"actor"`
}

// InitializeImageUploadRequest for POST /rest/images?action=initializeUpload.
type InitializeImageUploadRequest struct {
	InitializeUploadRequest ImageUploadInit `json:"initializeUploadRequest"`
}

// ImageUploadInit holds the owner for an image upload.
type ImageUploadInit struct {
	Owner string `json:"owner"`
}

// InitializeImageUploadResponse is the response from image upload initialization.
type InitializeImageUploadResponse struct {
	Value ImageUploadValue `json:"value"`
}

// ImageUploadValue holds the upload URL and image URN.
type ImageUploadValue struct {
	UploadURL string `json:"uploadUrl"`
	Image     string `json:"image"`
}

// UserInfo represents the response from GET /v2/userinfo (OpenID Connect).
type UserInfo struct {
	Sub     string `json:"sub"`
	Name    string `json:"name"`
	Email   string `json:"email"`
	Picture string `json:"picture"`
}

// OAuthTokenResponse is the response from the token exchange endpoint.
type OAuthTokenResponse struct {
	AccessToken string `json:"access_token"`
	ExpiresIn   int    `json:"expires_in"`
	Scope       string `json:"scope"`
}

// FeedItem represents a parsed feed entry (extracted via CDP from LinkedIn's DOM).
type FeedItem struct {
	ActivityURN    string `json:"activity_urn"`
	AuthorName     string `json:"author_name"`
	AuthorHeadline string `json:"author_headline,omitempty"`
	Text           string `json:"text"`
	Timestamp      string `json:"timestamp"`
	ReactionCount  int    `json:"reaction_count"`
	CommentCount   int    `json:"comment_count"`
	RepostCount    int    `json:"repost_count"`
}

// Comment represents a comment on a LinkedIn post.
type Comment struct {
	AuthorName string `json:"author_name"`
	Text       string `json:"text"`
	Timestamp  string `json:"timestamp"`
	Reactions  int    `json:"reactions"`
}
