package recall

import (
	"fmt"
	"testing"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// T3.6 — DanglingLink
// A broken wikilink encountered during traversal emits a DanglingLinkError and
// is skipped; valid neighbours still appear in results and Run does not panic.
func TestRecall_DanglingLink(t *testing.T) {
	links := []index.LinkRow{
		{Src: "a.md", Dst: "missing.md"}, // dangling — not in corpus
		{Src: "a.md", Dst: "b.md"},
	}
	g := graph.Build(links)
	docs := []Document{
		{Path: "a.md", ModTime: 100},
		{Path: "b.md", ModTime: 100},
		// missing.md intentionally absent from corpus
	}
	results, errs := Run("a.md", g, docs, WalkerConfig{MaxHops: 2}, 10)

	// b.md must appear; missing.md must not.
	foundB := false
	for _, r := range results {
		if r.Path == "b.md" {
			foundB = true
		}
		if r.Path == "missing.md" {
			t.Errorf("missing.md must not appear in results")
		}
	}
	if !foundB {
		t.Errorf("b.md should appear despite dangling link to missing.md")
	}

	// At least one DanglingLinkError must be reported for missing.md.
	hasDangling := false
	for _, err := range errs {
		if de, ok := err.(*DanglingLinkError); ok && de.Dst == "missing.md" {
			hasDangling = true
		}
	}
	if !hasDangling {
		t.Errorf("expected DanglingLinkError{Dst:\"missing.md\"}, got errs=%v", errs)
	}
}

// T3.7 — HubExplosion
// A seed with 500 outgoing links triggers the VisitedCap guard; the result set
// is bounded and traversal completes without timeout or panic.
func TestRecall_HubExplosion(t *testing.T) {
	const spokes = 500
	links := make([]index.LinkRow, spokes)
	docs := make([]Document, 0, spokes+1)
	docs = append(docs, Document{Path: "hub.md", ModTime: 100})
	for i := range links {
		dst := fmt.Sprintf("spoke%04d.md", i)
		links[i] = index.LinkRow{Src: "hub.md", Dst: dst}
		docs = append(docs, Document{Path: dst, ModTime: 100})
	}
	g := graph.Build(links)

	results, _ := Run("hub.md", g, docs, WalkerConfig{MaxHops: 2, VisitedCap: 256}, 100)
	if len(results) > 256 {
		t.Errorf("hub explosion: got %d results, want <= 256", len(results))
	}
}

// T3.8 — BenchmarkRecall_p99
// End-to-end Run over a 50-Zettel chain must be fast enough that p99 <20ms
// and p50 <10ms when the graph is warm.
func BenchmarkRecall_p99(b *testing.B) {
	links := make([]index.LinkRow, 0, 100)
	docs := make([]Document, 50)
	base := time.Now().Unix()
	for i := range docs {
		path := fmt.Sprintf("note%02d.md", i)
		docs[i] = Document{Path: path, ModTime: base - int64(i*86400)}
		if i > 0 {
			links = append(links, index.LinkRow{
				Src: path,
				Dst: fmt.Sprintf("note%02d.md", i-1),
			})
		}
	}
	g := graph.Build(links)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		Run("note49.md", g, docs, WalkerConfig{MaxHops: 3, VisitedCap: 256}, 20)
	}
}

// BenchmarkWalker_Walk_50Zettels
// Walker.Walk over a 50-Zettel corpus with 2-per-node fan-out.
func BenchmarkWalker_Walk_50Zettels(b *testing.B) {
	links := make([]index.LinkRow, 0, 100)
	docs := make([]Document, 50)
	base := time.Now().Unix()
	for i := range docs {
		path := fmt.Sprintf("note%02d.md", i)
		docs[i] = Document{Path: path, ModTime: base - int64(i*86400)}
		if i > 0 {
			links = append(links, index.LinkRow{
				Src: path,
				Dst: fmt.Sprintf("note%02d.md", i-1),
			})
		}
		if i > 1 {
			links = append(links, index.LinkRow{
				Src: path,
				Dst: fmt.Sprintf("note%02d.md", i-2),
			})
		}
	}
	g := graph.Build(links)
	w := NewWalker(g, docs, WalkerConfig{MaxHops: 3, VisitedCap: 256})
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		w.Walk("note49.md")
	}
}
