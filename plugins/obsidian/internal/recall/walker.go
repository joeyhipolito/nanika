package recall

import (
	"math"
	"sort"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
)

// WalkerConfig controls graph traversal behaviour.
type WalkerConfig struct {
	// MaxHops is the maximum BFS depth from the seed (inclusive). Nodes at
	// depth MaxHops+1 or beyond are not visited.
	MaxHops int

	// VisitedCap is the maximum number of result nodes collected before
	// traversal is stopped. 0 means no cap.
	VisitedCap int
}

// Walker performs a scored BFS from a seed document through the link graph.
type Walker struct {
	g      *graph.Graph
	byPath map[string]Document
	cfg    WalkerConfig
}

// NewWalker returns a Walker for the given graph and document corpus.
func NewWalker(g *graph.Graph, docs []Document, cfg WalkerConfig) *Walker {
	byPath := make(map[string]Document, len(docs))
	for _, d := range docs {
		byPath[d.Path] = d
	}
	return &Walker{g: g, byPath: byPath, cfg: cfg}
}

// Walk performs a BFS from seed and returns scored neighbours, excluding the
// seed itself. Nodes not present in the corpus (dangling links) are silently
// skipped. Results are sorted by score descending, then path ascending.
func (w *Walker) Walk(seed string) []ScoredDoc {
	maxVisit := w.cfg.VisitedCap
	if maxVisit <= 0 {
		maxVisit = math.MaxInt
	}

	seedDoc := w.byPath[seed] // zero value if seed not in corpus, handled gracefully

	type entry struct {
		path string
		hop  int
	}

	visited := map[string]bool{seed: true}
	queue := []entry{{seed, 0}}
	var results []ScoredDoc

outer:
	for len(queue) > 0 {
		cur := queue[0]
		queue = queue[1:]

		for _, nb := range w.g.Neighbours(cur.path) {
			if visited[nb] {
				continue
			}
			visited[nb] = true

			nextHop := cur.hop + 1
			if nextHop > w.cfg.MaxHops {
				continue
			}

			doc, ok := w.byPath[nb]
			if !ok {
				continue // dangling link — walker skips silently
			}

			results = append(results, ScoredDoc{Path: nb, Score: walkerScore(seedDoc, doc, nextHop)})
			if len(results) >= maxVisit {
				break outer
			}

			queue = append(queue, entry{nb, nextHop})
		}
	}

	sort.Slice(results, func(i, j int) bool {
		if results[i].Score != results[j].Score {
			return results[i].Score > results[j].Score
		}
		return results[i].Path < results[j].Path
	})
	return results
}

// walkerScore combines three signals into a single relevance score:
//
//	1/hop — proximity: closer nodes score higher
//	1/(1+age_days) — recency: recently modified nodes score higher
//	+1.0 if doc is in the same folder as seed (folder prior)
func walkerScore(seed, doc Document, hop int) float64 {
	dist := 1.0 / float64(hop+1)

	ageSecs := float64(seed.ModTime - doc.ModTime)
	if ageSecs < 0 {
		ageSecs = 0
	}
	recency := 1.0 / (1.0 + ageSecs/86400.0)

	var folderPrior float64
	if seed.Folder != "" && seed.Folder == doc.Folder {
		folderPrior = 1.0
	}

	return dist + recency + folderPrior
}
