package recall

import (
	"fmt"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
)

// DanglingLinkError is returned when a graph edge points to a node that has
// no corresponding Document in the corpus.
type DanglingLinkError struct {
	Dst string
}

func (e *DanglingLinkError) Error() string {
	return fmt.Sprintf("dangling link: %s", e.Dst)
}

// Run executes a scored BFS from seed, returns the top-k ScoredDocs and any
// DanglingLinkErrors encountered during traversal. Dangling targets are
// excluded from results but reported as errors. Run never panics.
func Run(seed string, g *graph.Graph, docs []Document, cfg WalkerConfig, k int) ([]ScoredDoc, []error) {
	pathSet := make(map[string]bool, len(docs))
	for _, d := range docs {
		pathSet[d.Path] = true
	}

	// Walker skips dangling links internally — use it for the scored results.
	w := NewWalker(g, docs, cfg)
	results := w.Walk(seed)
	if k > 0 && len(results) > k {
		results = results[:k]
	}

	// Separate BFS pass to detect and report dangling links.
	errs := detectDangling(seed, g, pathSet, cfg)

	return results, errs
}

// detectDangling performs a BFS limited to cfg.MaxHops and reports each
// unique link target that is absent from pathSet.
func detectDangling(seed string, g *graph.Graph, pathSet map[string]bool, cfg WalkerConfig) []error {
	type entry struct {
		path string
		hop  int
	}

	visited := map[string]bool{seed: true}
	reported := map[string]bool{}
	queue := []entry{{seed, 0}}
	var errs []error

	for len(queue) > 0 {
		cur := queue[0]
		queue = queue[1:]

		if cur.hop >= cfg.MaxHops {
			continue
		}

		for _, nb := range g.Neighbours(cur.path) {
			if visited[nb] {
				continue
			}
			visited[nb] = true

			if !pathSet[nb] {
				if !reported[nb] {
					errs = append(errs, &DanglingLinkError{Dst: nb})
					reported[nb] = true
				}
				// don't enqueue — nothing to explore from a missing node
				continue
			}

			queue = append(queue, entry{nb, cur.hop + 1})
		}
	}

	return errs
}
