package api

// Listing is the top-level Reddit API response for lists.
type Listing struct {
	Kind string      `json:"kind"` // "Listing"
	Data ListingData `json:"data"`
}

// ListingData holds the children and pagination info.
type ListingData struct {
	After    string  `json:"after"`
	Before   string  `json:"before"`
	Children []Thing `json:"children"`
}

// Thing wraps a typed Reddit object (t1=comment, t3=post, etc).
type Thing struct {
	Kind string   `json:"kind"`
	Data ThingMap `json:"data"`
}

// ThingMap is a flexible map for Reddit thing data.
type ThingMap map[string]interface{}

// PostData represents a Reddit post (t3).
type PostData struct {
	ID            string  `json:"id"`
	Fullname      string  `json:"name"` // t3_xxx
	Title         string  `json:"title"`
	Author        string  `json:"author"`
	Subreddit     string  `json:"subreddit"`
	SelfText      string  `json:"selftext"`
	URL           string  `json:"url"`
	Permalink     string  `json:"permalink"`
	Score         int     `json:"score"`
	UpvoteRatio   float64 `json:"upvote_ratio"`
	NumComments   int     `json:"num_comments"`
	CreatedUTC    float64 `json:"created_utc"`
	IsSelf        bool    `json:"is_self"`
	Over18        bool    `json:"over_18"`
	Stickied      bool    `json:"stickied"`
	Distinguished string  `json:"distinguished"`
}

// CommentData represents a Reddit comment (t1).
type CommentData struct {
	ID        string      `json:"id"`
	Fullname  string      `json:"name"` // t1_xxx
	Author    string      `json:"author"`
	Body      string      `json:"body"`
	Score     int         `json:"score"`
	CreatedUTC float64    `json:"created_utc"`
	ParentID  string      `json:"parent_id"`
	Permalink string      `json:"permalink"`
	Depth     int         `json:"depth"`
	Replies   interface{} `json:"replies"` // Can be "" or a Listing
}

// SubmitResponse is the response from POST /api/submit.
type SubmitResponse struct {
	JSON struct {
		Errors [][]string `json:"errors"`
		Data   struct {
			URL  string `json:"url"`
			ID   string `json:"id"`
			Name string `json:"name"` // fullname t3_xxx
		} `json:"data"`
	} `json:"json"`
}

// CommentResponse is the response from POST /api/comment.
type CommentResponse struct {
	JSON struct {
		Errors [][]string `json:"errors"`
		Data   struct {
			Things []Thing `json:"things"`
		} `json:"data"`
	} `json:"json"`
}
