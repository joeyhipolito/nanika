package recall

import (
	"sort"
	"strings"
)

// SeedFilter filters a document corpus by frontmatter attributes.
type SeedFilter struct {
	docs []Document
}

// NewSeedFilter returns a SeedFilter over docs.
func NewSeedFilter(docs []Document) *SeedFilter {
	return &SeedFilter{docs: docs}
}

// ByTag returns all documents whose Tags slice contains tag.
// Results are sorted by Path for determinism.
func (sf *SeedFilter) ByTag(tag string) []Document {
	var out []Document
	for _, d := range sf.docs {
		for _, t := range d.Tags {
			if t == tag {
				out = append(out, d)
				break
			}
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Path < out[j].Path })
	return out
}

// ByTitlePrefix returns all documents whose Title starts with prefix.
// An empty prefix matches all documents. Results are sorted by Path.
func (sf *SeedFilter) ByTitlePrefix(prefix string) []Document {
	var out []Document
	for _, d := range sf.docs {
		if strings.HasPrefix(d.Title, prefix) {
			out = append(out, d)
		}
	}
	sort.Slice(out, func(i, j int) bool { return out[i].Path < out[j].Path })
	return out
}

// SelectSeeds ranks documents by a weighted combination of three signals:
//
//	BM25 text relevance  × 0.4
//	tag overlap          × 0.3  (1.0 if any tag in tags matches, else 0)
//	title prefix match   × 0.3  (1.0 if title starts with titlePrefix, else 0)
//
// BM25 scores are normalised to [0,1] before weighting. Only documents with
// a combined score > 0 are returned. Ties are broken by Path ascending.
func SelectSeeds(query string, tags []string, titlePrefix string, docs []Document) []ScoredDoc {
	if len(docs) == 0 {
		return nil
	}

	idx := NewBM25(docs)
	raw := idx.TopK(query, len(docs))

	// normalise bm25 scores to [0,1]
	bm25Map := make(map[string]float64, len(raw))
	if len(raw) > 0 {
		maxScore := raw[0].Score // TopK returns descending
		for _, r := range raw {
			if maxScore > 0 {
				bm25Map[r.Path] = r.Score / maxScore
			}
		}
	}

	tagSet := make(map[string]bool, len(tags))
	for _, t := range tags {
		tagSet[t] = true
	}

	results := make([]ScoredDoc, 0, len(docs))
	for _, doc := range docs {
		bm25Score := bm25Map[doc.Path]

		var tagBonus float64
		for _, t := range doc.Tags {
			if tagSet[t] {
				tagBonus = 1.0
				break
			}
		}

		var titleBonus float64
		if titlePrefix != "" && strings.HasPrefix(doc.Title, titlePrefix) {
			titleBonus = 1.0
		}

		score := 0.4*bm25Score + 0.3*tagBonus + 0.3*titleBonus
		if score > 0 {
			results = append(results, ScoredDoc{Path: doc.Path, Score: score})
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
