package recall

import (
	"fmt"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/graph"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// TestMaxHops_Enforced: a node exactly (maxHops+1) steps from the seed must
// not appear in Walk results.
func TestMaxHops_Enforced(t *testing.T) {
	// chain: a → b → c → d  (d is 3 hops from a)
	links := []index.LinkRow{
		{Src: "a.md", Dst: "b.md"},
		{Src: "b.md", Dst: "c.md"},
		{Src: "c.md", Dst: "d.md"},
	}
	g := graph.Build(links)
	docs := []Document{
		{Path: "a.md", ModTime: 100},
		{Path: "b.md", ModTime: 100},
		{Path: "c.md", ModTime: 100},
		{Path: "d.md", ModTime: 100},
	}
	w := NewWalker(g, docs, WalkerConfig{MaxHops: 2})
	results := w.Walk("a.md")
	for _, r := range results {
		if r.Path == "d.md" {
			t.Errorf("d.md is 3 hops away but was returned with MaxHops=2")
		}
	}
	// b.md and c.md must still appear.
	paths := make(map[string]bool)
	for _, r := range results {
		paths[r.Path] = true
	}
	for _, want := range []string{"b.md", "c.md"} {
		if !paths[want] {
			t.Errorf("expected %q in results (within 2 hops), not found", want)
		}
	}
}

// TestVisitedCap: when a hub has more outgoing links than VisitedCap, the
// result count must not exceed VisitedCap.
func TestVisitedCap(t *testing.T) {
	const spokes = 500
	const cap = 256
	links := make([]index.LinkRow, spokes)
	docs := make([]Document, 0, spokes+1)
	docs = append(docs, Document{Path: "hub.md", ModTime: 100})
	for i := range links {
		dst := fmt.Sprintf("spoke%04d.md", i)
		links[i] = index.LinkRow{Src: "hub.md", Dst: dst}
		docs = append(docs, Document{Path: dst, ModTime: 100})
	}
	g := graph.Build(links)
	w := NewWalker(g, docs, WalkerConfig{MaxHops: 2, VisitedCap: cap})
	results := w.Walk("hub.md")
	if len(results) > cap {
		t.Errorf("VisitedCap=%d but Walk returned %d results", cap, len(results))
	}
}

// TestDeterministic: Walk called twice with the same seed returns identical
// paths and scores.
func TestDeterministic(t *testing.T) {
	links := []index.LinkRow{
		{Src: "seed.md", Dst: "a.md"},
		{Src: "seed.md", Dst: "b.md"},
		{Src: "a.md", Dst: "c.md"},
	}
	g := graph.Build(links)
	docs := []Document{
		{Path: "seed.md", ModTime: 200},
		{Path: "a.md", ModTime: 150},
		{Path: "b.md", ModTime: 100},
		{Path: "c.md", ModTime: 50},
	}
	w := NewWalker(g, docs, WalkerConfig{MaxHops: 3})
	first := w.Walk("seed.md")
	second := w.Walk("seed.md")
	if len(first) != len(second) {
		t.Fatalf("Walk not deterministic: len %d vs %d", len(first), len(second))
	}
	for i := range first {
		if first[i].Path != second[i].Path || first[i].Score != second[i].Score {
			t.Errorf("result[%d] differs: %+v vs %+v", i, first[i], second[i])
		}
	}
}

// TestRecencyDecay: among equidistant neighbours, the more recently modified
// document must receive a strictly higher score.
func TestRecencyDecay(t *testing.T) {
	links := []index.LinkRow{
		{Src: "seed.md", Dst: "recent.md"},
		{Src: "seed.md", Dst: "old.md"},
	}
	g := graph.Build(links)
	const now = int64(1_000_000)
	docs := []Document{
		{Path: "seed.md", ModTime: now},
		{Path: "recent.md", ModTime: now - 86400},        // 1 day old
		{Path: "old.md", ModTime: now - 86400*30},        // 30 days old
	}
	w := NewWalker(g, docs, WalkerConfig{MaxHops: 2})
	results := w.Walk("seed.md")
	if len(results) < 2 {
		t.Fatalf("expected >= 2 results, got %d", len(results))
	}
	scores := make(map[string]float64)
	for _, r := range results {
		scores[r.Path] = r.Score
	}
	if scores["recent.md"] <= scores["old.md"] {
		t.Errorf("recency decay: recent.md (%.4f) should score > old.md (%.4f)",
			scores["recent.md"], scores["old.md"])
	}
}

// TestFolderPrior: a note in the same folder as the seed must score at least
// as high as an equidistant note in a different folder.
func TestFolderPrior(t *testing.T) {
	links := []index.LinkRow{
		{Src: "notes/seed.md", Dst: "notes/sibling.md"},
		{Src: "notes/seed.md", Dst: "other/foreign.md"},
	}
	g := graph.Build(links)
	const now = int64(1_000_000)
	docs := []Document{
		{Path: "notes/seed.md", Folder: "notes", ModTime: now},
		{Path: "notes/sibling.md", Folder: "notes", ModTime: now - 100},
		{Path: "other/foreign.md", Folder: "other", ModTime: now - 100},
	}
	w := NewWalker(g, docs, WalkerConfig{MaxHops: 2})
	results := w.Walk("notes/seed.md")
	if len(results) < 2 {
		t.Fatalf("expected >= 2 results, got %d", len(results))
	}
	scores := make(map[string]float64)
	for _, r := range results {
		scores[r.Path] = r.Score
	}
	if scores["notes/sibling.md"] < scores["other/foreign.md"] {
		t.Errorf("folder prior: sibling (%.4f) should score >= foreign (%.4f)",
			scores["notes/sibling.md"], scores["other/foreign.md"])
	}
}
