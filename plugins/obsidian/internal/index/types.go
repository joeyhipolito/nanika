package index

// NoteMeta holds the lightweight metadata stored by Indexer for incremental sync.
type NoteMeta struct {
	Title   string
	ModTime int64
}

// LinkRow represents one outgoing-link record in the links table.
type LinkRow struct {
	Src string
	Dst string
}
