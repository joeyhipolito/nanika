package recall

import (
	"fmt"
	"testing"
)

// TestBM25_Build_Empty: building a BM25 index with a nil corpus returns a non-nil
// index whose TopK returns an empty slice without panicking.
func TestBM25_Build_Empty(t *testing.T) {
	idx := NewBM25(nil)
	if idx == nil {
		t.Fatal("NewBM25(nil) returned nil")
	}
	results := idx.TopK("query", 5)
	if len(results) != 0 {
		t.Fatalf("TopK on empty index: want 0 results, got %d", len(results))
	}
}

// TestTopK_Deterministic: identical queries on the same corpus always return
// results in the same order.
func TestTopK_Deterministic(t *testing.T) {
	docs := []Document{
		{Path: "a.md", Title: "Alpha Note", Body: "alpha beta gamma"},
		{Path: "b.md", Title: "Beta Note", Body: "beta gamma delta"},
		{Path: "c.md", Title: "Gamma Note", Body: "gamma delta epsilon"},
	}
	idx := NewBM25(docs)
	first := idx.TopK("beta gamma", 3)
	second := idx.TopK("beta gamma", 3)
	if len(first) != len(second) {
		t.Fatalf("TopK not deterministic: len %d vs %d", len(first), len(second))
	}
	for i := range first {
		if first[i].Path != second[i].Path {
			t.Errorf("result[%d]: %q vs %q", i, first[i].Path, second[i].Path)
		}
	}
}

// TestBM25_TopK_Scored: the document with the highest term overlap ranks first
// and carries a positive score.
func TestBM25_TopK_Scored(t *testing.T) {
	docs := []Document{
		{Path: "exact.md", Body: "retrieval augmented generation rag context window"},
		{Path: "partial.md", Body: "generation language model transformer"},
		{Path: "unrelated.md", Body: "cooking recipes pasta tomato sauce"},
	}
	idx := NewBM25(docs)
	results := idx.TopK("retrieval augmented generation", 2)
	if len(results) == 0 {
		t.Fatal("expected at least one result")
	}
	if results[0].Path != "exact.md" {
		t.Errorf("top result: want exact.md, got %q", results[0].Path)
	}
	if results[0].Score <= 0 {
		t.Errorf("top score: want > 0, got %f", results[0].Score)
	}
}

// BenchmarkBM25_Build_1000: measures throughput of building a BM25 index over
// a 1000-document corpus.
func BenchmarkBM25_Build_1000(b *testing.B) {
	docs := make([]Document, 1000)
	for i := range docs {
		docs[i] = Document{
			Path: fmt.Sprintf("note%04d.md", i),
			Body: "word1 word2 word3 word4 word5 word6 word7 word8",
		}
	}
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		NewBM25(docs)
	}
}

// BenchmarkBM25_TopK: measures query latency against a warm 1000-document index.
func BenchmarkBM25_TopK(b *testing.B) {
	docs := make([]Document, 1000)
	for i := range docs {
		docs[i] = Document{
			Path: fmt.Sprintf("note%04d.md", i),
			Body: "word1 word2 word3 word4 word5 word6 word7 word8",
		}
	}
	idx := NewBM25(docs)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		idx.TopK("word1 word2", 10)
	}
}
