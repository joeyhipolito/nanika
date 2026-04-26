// T3.2 benchmarks — CSR link graph (TRK-530 gate).
// Expected compile-fail until Build, Load, Graph.BFS, Graph.Neighbours, and
// Graph.WriteTo are implemented in graph.go.
package graph

import (
	"bytes"
	"fmt"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// linearLinks returns N directed link rows forming a chain:
// node0000.md → node0001.md → … → node(N-1).md
func linearLinks(n int) []index.LinkRow {
	rows := make([]index.LinkRow, n-1)
	for i := range rows {
		rows[i] = index.LinkRow{
			Src: fmt.Sprintf("node%04d.md", i),
			Dst: fmt.Sprintf("node%04d.md", i+1),
		}
	}
	return rows
}

// hubLinks returns N directed link rows from a single hub node to N leaves.
func hubLinks(n int) []index.LinkRow {
	rows := make([]index.LinkRow, n)
	for i := range rows {
		rows[i] = index.LinkRow{
			Src: "hub.md",
			Dst: fmt.Sprintf("leaf%04d.md", i),
		}
	}
	return rows
}

// BenchmarkGraph_Build measures Build throughput on a 1 000-edge linear chain.
func BenchmarkGraph_Build(b *testing.B) {
	links := linearLinks(1000)
	b.ResetTimer()
	for range b.N {
		_ = Build(links)
	}
}

// BenchmarkGraph_Neighbours measures the hot-path Neighbours lookup on a
// 1 000-leaf hub graph.
func BenchmarkGraph_Neighbours(b *testing.B) {
	g := Build(hubLinks(1000))
	b.ResetTimer()
	for range b.N {
		_ = g.Neighbours("hub.md")
	}
}

// BenchmarkGraph_BFS2Hop measures 2-hop BFS on the fixed 50-zettel fixture
// topology (fixtureLinks returns ~40 edges covering all note types).
func BenchmarkGraph_BFS2Hop(b *testing.B) {
	g := Build(fixtureLinks())
	b.ResetTimer()
	for range b.N {
		_ = g.BFS("mocs/index.md", 2)
	}
}

// BenchmarkGraph_WriteTo measures serialisation throughput on a 1 000-node graph.
func BenchmarkGraph_WriteTo(b *testing.B) {
	g := Build(linearLinks(1000))
	b.ResetTimer()
	for range b.N {
		var buf bytes.Buffer
		if _, err := g.WriteTo(&buf); err != nil {
			b.Fatalf("WriteTo: %v", err)
		}
	}
}

// BenchmarkGraph_Load measures deserialisation throughput on a 1 000-node graph.
func BenchmarkGraph_Load(b *testing.B) {
	g := Build(linearLinks(1000))
	var buf bytes.Buffer
	if _, err := g.WriteTo(&buf); err != nil {
		b.Fatalf("WriteTo: %v", err)
	}
	data := buf.Bytes()

	b.ResetTimer()
	for range b.N {
		if _, err := Load(bytes.NewReader(data)); err != nil {
			b.Fatalf("Load: %v", err)
		}
	}
}
