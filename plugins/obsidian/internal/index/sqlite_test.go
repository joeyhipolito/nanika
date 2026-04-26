// T3.1 RED test suite — Indexer with link graph (TRK-530 gate).
// These tests MUST compile-fail until sqlite.go implements Indexer, NoteMeta,
// OpenIndexer, Upsert, Delete, ReplaceLinks, and Neighbours.
package index

import (
	"fmt"
	"path/filepath"
	"sort"
	"sync"
	"testing"
)

// openIdx is a test helper that opens a fresh Indexer in a temp dir and
// registers t.Cleanup to close it.
func openIdx(t *testing.T) *Indexer {
	t.Helper()
	ix, err := OpenIndexer(filepath.Join(t.TempDir(), "idx.db"))
	if err != nil {
		t.Fatalf("OpenIndexer: %v", err)
	}
	t.Cleanup(func() { ix.Close() })
	return ix
}

// OpenCreatesSchema verifies that opening a fresh DB succeeds and the schema
// is usable — Neighbours on an empty store returns an empty slice, not an error.
func TestIndexer_OpenCreatesSchema(t *testing.T) {
	ix := openIdx(t)

	got, err := ix.Neighbours("nonexistent.md")
	if err != nil {
		t.Fatalf("Neighbours on empty store: %v", err)
	}
	if len(got) != 0 {
		t.Errorf("expected empty slice, got %v", got)
	}
}

// UpsertReplacesExisting verifies that upserting the same path twice replaces
// the prior link set atomically.
func TestIndexer_UpsertReplacesExisting(t *testing.T) {
	ix := openIdx(t)

	if err := ix.Upsert("note.md", NoteMeta{Title: "First", ModTime: 1}, []string{"a.md"}); err != nil {
		t.Fatalf("first Upsert: %v", err)
	}
	if err := ix.Upsert("note.md", NoteMeta{Title: "Second", ModTime: 2}, []string{"b.md", "c.md"}); err != nil {
		t.Fatalf("second Upsert: %v", err)
	}

	got, err := ix.Neighbours("note.md")
	if err != nil {
		t.Fatalf("Neighbours: %v", err)
	}
	sort.Strings(got)
	if len(got) != 2 || got[0] != "b.md" || got[1] != "c.md" {
		t.Errorf("expected [b.md c.md] after second Upsert, got %v", got)
	}
}

// DeleteCascadesLinks verifies that deleting a note removes all its outgoing
// link records so Neighbours returns an empty slice.
func TestIndexer_DeleteCascadesLinks(t *testing.T) {
	ix := openIdx(t)

	if err := ix.Upsert("src.md", NoteMeta{Title: "Src", ModTime: 1}, []string{"dst.md"}); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	pre, err := ix.Neighbours("src.md")
	if err != nil || len(pre) != 1 {
		t.Fatalf("pre-delete Neighbours: %v %v", pre, err)
	}

	if err := ix.Delete("src.md"); err != nil {
		t.Fatalf("Delete: %v", err)
	}

	post, err := ix.Neighbours("src.md")
	if err != nil {
		t.Fatalf("post-delete Neighbours error: %v", err)
	}
	if len(post) != 0 {
		t.Errorf("expected empty neighbours after delete, got %v", post)
	}
}

// ReplaceLinksAtomic verifies that ReplaceLinks swaps the full link set in a
// single transaction — old links are gone, new links are present.
func TestIndexer_ReplaceLinksAtomic(t *testing.T) {
	ix := openIdx(t)

	if err := ix.Upsert("n.md", NoteMeta{Title: "N", ModTime: 1}, []string{"old.md"}); err != nil {
		t.Fatalf("Upsert: %v", err)
	}
	if err := ix.ReplaceLinks("n.md", []string{"new1.md", "new2.md"}); err != nil {
		t.Fatalf("ReplaceLinks: %v", err)
	}

	got, err := ix.Neighbours("n.md")
	if err != nil {
		t.Fatalf("Neighbours after ReplaceLinks: %v", err)
	}
	sort.Strings(got)
	if len(got) != 2 || got[0] != "new1.md" || got[1] != "new2.md" {
		t.Errorf("expected [new1.md new2.md], got %v", got)
	}
}

// NeighboursEmpty verifies that Neighbours returns an empty (non-nil) slice for
// a path that has never been upserted.
func TestIndexer_NeighboursEmpty(t *testing.T) {
	ix := openIdx(t)

	got, err := ix.Neighbours("ghost.md")
	if err != nil {
		t.Fatalf("Neighbours for unknown path: %v", err)
	}
	if len(got) != 0 {
		t.Errorf("expected empty slice for unknown path, got %v", got)
	}
}

// ConcurrentUpsertSafe verifies that concurrent Upsert calls from multiple
// goroutines do not race or return errors.
func TestIndexer_ConcurrentUpsertSafe(t *testing.T) {
	ix := openIdx(t)

	const workers = 8
	const notesPerWorker = 20

	errc := make(chan error, workers*notesPerWorker)
	var wg sync.WaitGroup

	for w := range workers {
		wg.Add(1)
		go func(worker int) {
			defer wg.Done()
			for n := range notesPerWorker {
				path := fmt.Sprintf("worker%d/note%d.md", worker, n)
				meta := NoteMeta{
					Title:   fmt.Sprintf("Worker %d Note %d", worker, n),
					ModTime: int64(worker*notesPerWorker + n + 1),
				}
				if err := ix.Upsert(path, meta, nil); err != nil {
					errc <- fmt.Errorf("worker %d note %d: %w", worker, n, err)
				}
			}
		}(w)
	}

	wg.Wait()
	close(errc)

	for err := range errc {
		t.Error(err)
	}
}

// TestIndexer_Incremental is the T3.1 gate test (TRK-530).
// It verifies the full incremental indexing lifecycle:
//  1. Initial index of N notes with outgoing links.
//  2. Incremental update of one note — only its entry and links change.
//  3. Delete of a note — cascades its link rows; peers are unaffected.
func TestIndexer_Incremental(t *testing.T) {
	ix := openIdx(t)

	// --- Phase 1: initial index ---
	seed := []struct {
		path  string
		meta  NoteMeta
		links []string
	}{
		{"a.md", NoteMeta{Title: "A", ModTime: 10}, []string{"b.md"}},
		{"b.md", NoteMeta{Title: "B", ModTime: 20}, []string{"c.md"}},
		{"c.md", NoteMeta{Title: "C", ModTime: 30}, nil},
	}
	for _, n := range seed {
		if err := ix.Upsert(n.path, n.meta, n.links); err != nil {
			t.Fatalf("initial Upsert %s: %v", n.path, err)
		}
	}

	t.Run("initial_link_graph", func(t *testing.T) {
		gotA, err := ix.Neighbours("a.md")
		if err != nil || len(gotA) != 1 || gotA[0] != "b.md" {
			t.Errorf("a.md neighbours: %v %v", gotA, err)
		}
		gotB, err := ix.Neighbours("b.md")
		if err != nil || len(gotB) != 1 || gotB[0] != "c.md" {
			t.Errorf("b.md neighbours: %v %v", gotB, err)
		}
		gotC, err := ix.Neighbours("c.md")
		if err != nil || len(gotC) != 0 {
			t.Errorf("c.md neighbours: %v %v", gotC, err)
		}
	})

	// --- Phase 2: incremental update of a.md only ---
	if err := ix.Upsert("a.md", NoteMeta{Title: "A Updated", ModTime: 99}, []string{"c.md"}); err != nil {
		t.Fatalf("incremental Upsert a.md: %v", err)
	}

	t.Run("after_incremental_update", func(t *testing.T) {
		// a.md now points to c.md, not b.md.
		gotA, err := ix.Neighbours("a.md")
		if err != nil {
			t.Fatalf("Neighbours a.md: %v", err)
		}
		if len(gotA) != 1 || gotA[0] != "c.md" {
			t.Errorf("a.md expected [c.md], got %v", gotA)
		}

		// b.md is unchanged — still points to c.md.
		gotB, err := ix.Neighbours("b.md")
		if err != nil || len(gotB) != 1 || gotB[0] != "c.md" {
			t.Errorf("b.md unchanged: %v %v", gotB, err)
		}
	})

	// --- Phase 3: delete c.md; cascade only c's outgoing links ---
	if err := ix.Delete("c.md"); err != nil {
		t.Fatalf("Delete c.md: %v", err)
	}

	t.Run("after_delete_cascade", func(t *testing.T) {
		gotC, err := ix.Neighbours("c.md")
		if err != nil {
			t.Fatalf("Neighbours c.md post-delete: %v", err)
		}
		if len(gotC) != 0 {
			t.Errorf("c.md links not cascaded on delete: %v", gotC)
		}

		// a.md still exists and its outgoing link to c.md is intact
		// (the link record is about c.md as a destination, not source).
		gotA, err := ix.Neighbours("a.md")
		if err != nil {
			t.Fatalf("Neighbours a.md post-c-delete: %v", err)
		}
		if len(gotA) != 1 {
			t.Errorf("a.md should still have 1 outgoing link, got %v", gotA)
		}

		// b.md unchanged.
		gotB, err := ix.Neighbours("b.md")
		if err != nil {
			t.Fatalf("Neighbours b.md post-c-delete: %v", err)
		}
		if len(gotB) != 1 {
			t.Errorf("b.md should still have 1 outgoing link, got %v", gotB)
		}
	})
}
