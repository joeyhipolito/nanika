package recall

import (
	"fmt"
	"path/filepath"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// Engine executes recall queries against a live graph and note index.
// Either dependency may be nil; Recall degrades gracefully when absent.
type Engine struct {
	graphFn func() *graph.Graph
	idxr    *index.Indexer
}

// NewEngine returns an Engine backed by the given graph closure and indexer.
func NewEngine(graphFn func() *graph.Graph, idxr *index.Indexer) *Engine {
	return &Engine{graphFn: graphFn, idxr: idxr}
}

// Recall runs a scored BFS from req.Seed and returns up to req.Limit WalkResults
// sorted by relevance descending. Returns an empty slice without error when the
// graph is nil or the seed is not found.
func (e *Engine) Recall(req Request) ([]WalkResult, error) {
	if e.graphFn == nil {
		return []WalkResult{}, nil
	}
	g := e.graphFn()
	if g == nil {
		return []WalkResult{}, nil
	}

	docs, err := e.loadDocs()
	if err != nil {
		return nil, fmt.Errorf("recall engine: load docs: %w", err)
	}

	maxHops := req.MaxHops
	if maxHops <= 0 {
		maxHops = 2
	}

	cfg := WalkerConfig{
		MaxHops:    maxHops,
		VisitedCap: 0,
	}
	scored, _ := Run(req.Seed, g, docs, cfg, req.Limit)

	out := make([]WalkResult, len(scored))
	for i, s := range scored {
		out[i] = WalkResult{Path: s.Path, Score: s.Score}
	}
	return out, nil
}

// loadDocs fetches path + title + mod_time from the indexer and converts them
// to the Document corpus expected by Walker. Folder is derived from the path prefix.
func (e *Engine) loadDocs() ([]Document, error) {
	if e.idxr == nil {
		return nil, nil
	}
	metas, err := e.idxr.AllNotes()
	if err != nil {
		return nil, err
	}
	docs := make([]Document, 0, len(metas))
	for path, m := range metas {
		docs = append(docs, Document{
			Path:    path,
			Title:   m.Title,
			Folder:  filepath.Dir(path),
			ModTime: m.ModTime,
		})
	}
	return docs, nil
}
