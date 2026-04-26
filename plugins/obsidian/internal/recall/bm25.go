package recall

import (
	"math"
	"sort"
	"strings"
)

const (
	bm25K1 = 1.5
	bm25B  = 0.75
)

// BM25 is an inverted index that scores documents against free-text queries
// using the standard BM25 ranking function (k1=1.5, b=0.75).
type BM25 struct {
	docs  []Document
	tf    []map[string]int // per-doc term frequency
	df    map[string]int   // document frequency per term
	dl    []int            // document length in tokens
	avgdl float64
	n     int
}

// NewBM25 builds a BM25 index from docs. A nil or empty slice returns a
// valid, empty index whose TopK always returns nil.
func NewBM25(docs []Document) *BM25 {
	bm := &BM25{
		docs: docs,
		tf:   make([]map[string]int, len(docs)),
		df:   make(map[string]int),
		dl:   make([]int, len(docs)),
		n:    len(docs),
	}

	var totalLen int
	for i, doc := range docs {
		tokens := tokenize(doc.Title + " " + doc.Body)
		bm.dl[i] = len(tokens)
		totalLen += len(tokens)

		freq := make(map[string]int, len(tokens))
		for _, tok := range tokens {
			freq[tok]++
		}
		bm.tf[i] = freq

		for tok := range freq {
			bm.df[tok]++
		}
	}

	if bm.n > 0 {
		bm.avgdl = float64(totalLen) / float64(bm.n)
	}

	return bm
}

// Score returns the BM25 relevance of document docIdx for query.
func (bm *BM25) Score(query string, docIdx int) float64 {
	if bm.n == 0 || bm.avgdl == 0 {
		return 0
	}
	terms := tokenize(query)
	dl := float64(bm.dl[docIdx])

	var score float64
	for _, term := range terms {
		df := float64(bm.df[term])
		if df == 0 {
			continue
		}
		tf := float64(bm.tf[docIdx][term])
		idf := math.Log((float64(bm.n)-df+0.5)/(df+0.5) + 1)
		num := tf * (bm25K1 + 1)
		den := tf + bm25K1*(1-bm25B+bm25B*dl/bm.avgdl)
		score += idf * num / den
	}
	return score
}

// TopK returns the k highest-scoring documents for query. Ties are broken
// lexicographically by path so results are fully deterministic.
func (bm *BM25) TopK(query string, k int) []ScoredDoc {
	if bm.n == 0 || k <= 0 {
		return nil
	}

	scored := make([]ScoredDoc, 0, bm.n)
	for i, doc := range bm.docs {
		if s := bm.Score(query, i); s > 0 {
			scored = append(scored, ScoredDoc{Path: doc.Path, Score: s})
		}
	}
	if len(scored) == 0 {
		return nil
	}

	sort.Slice(scored, func(i, j int) bool {
		if scored[i].Score != scored[j].Score {
			return scored[i].Score > scored[j].Score
		}
		return scored[i].Path < scored[j].Path
	})

	if k < len(scored) {
		scored = scored[:k]
	}
	return scored
}

func tokenize(s string) []string {
	return strings.Fields(strings.ToLower(s))
}
