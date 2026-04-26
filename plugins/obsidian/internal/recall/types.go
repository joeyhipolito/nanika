package recall

// Document is a vault note with metadata used for scoring and filtering.
type Document struct {
	Path    string
	Title   string
	Body    string
	Tags    []string
	Folder  string
	ModTime int64
}

// ScoredDoc pairs a document path with its computed relevance score.
type ScoredDoc struct {
	Path  string
	Score float64
}

// Request is the recall query used by the RPC callback.
type Request struct {
	Seed    string
	MaxHops int
	Limit   int
}

// WalkResult is a single scored note path returned by a recall query.
type WalkResult struct {
	Path  string
	Score float64
}
