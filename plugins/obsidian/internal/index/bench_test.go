// BenchmarkIndexerIncremental — T3.1 benchmark (TRK-530 gate).
// Measures the cost of one incremental Upsert against a 100-note warm store.
package index

import (
	"fmt"
	"path/filepath"
	"testing"
)

func BenchmarkIndexerIncremental(b *testing.B) {
	ix, err := OpenIndexer(filepath.Join(b.TempDir(), "bench.db"))
	if err != nil {
		b.Fatalf("OpenIndexer: %v", err)
	}
	defer ix.Close()

	// Seed a 100-note store with a linear link chain: note0→note1→…→note99.
	const seed = 100
	for i := range seed {
		path := fmt.Sprintf("note%04d.md", i)
		meta := NoteMeta{
			Title:   fmt.Sprintf("Note %d", i),
			ModTime: int64(i + 1),
		}
		var links []string
		if i+1 < seed {
			links = []string{fmt.Sprintf("note%04d.md", i+1)}
		}
		if err := ix.Upsert(path, meta, links); err != nil {
			b.Fatalf("seed Upsert %s: %v", path, err)
		}
	}

	b.ResetTimer()

	for i := range b.N {
		// Cycle through the 100-note corpus: only the note at position i%seed is
		// "modified" this iteration — simulating a single-file incremental update.
		n := i % seed
		path := fmt.Sprintf("note%04d.md", n)
		meta := NoteMeta{
			Title:   fmt.Sprintf("Note %d (rev %d)", n, i),
			ModTime: int64(seed + i + 1),
		}
		var links []string
		if n+1 < seed {
			links = []string{fmt.Sprintf("note%04d.md", n+1)}
		}
		if err := ix.Upsert(path, meta, links); err != nil {
			b.Fatalf("incremental Upsert %s: %v", path, err)
		}
	}
}
