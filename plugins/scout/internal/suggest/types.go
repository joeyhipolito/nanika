package suggest

// Suggestion represents a ranked content idea derived from intel clustering.
type Suggestion struct {
	Title       string   `json:"title"`
	Angle       string   `json:"angle"`
	ContentType string   `json:"content_type"` // "blog", "thread", "video"
	Score       int      `json:"score"`
	Topics      []string `json:"topics"`
	Sources     []Source `json:"sources"`
}

// Source references a single intel item backing a suggestion.
type Source struct {
	Title  string `json:"title"`
	URL    string `json:"url"`
	Source string `json:"source"`
	Score  int    `json:"score"`
}

// annotatedItem pairs an intel item's fields with its originating topic.
type annotatedItem struct {
	title      string
	sourceURL  string
	author     string
	score      float64
	engagement int
	timestamp  int64 // unix seconds for fast arithmetic
	topic      string
	source     string // gatherer source (e.g. "web", "reddit")
	keywords   map[string]bool
}

// cluster groups related annotated items sharing keywords.
type cluster struct {
	keywords []string
	items    []*annotatedItem
}
