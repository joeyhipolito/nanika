// T3.2 RED test suite — CSR link graph (TRK-530 gate).
// Expected compile-fail: Build, Load, Graph.BFS, Graph.Neighbours, and
// Graph.WriteTo are undefined until TRK-528 Phase 3 implements graph.go.
package graph

import (
	"bytes"
	"errors"
	"io"
	"os"
	"path/filepath"
	"slices"
	"testing"

	"github.com/joeyhipolito/nanika-obsidian/internal/index"
)

// fixtureVault is the 50-zettel test vault committed to testdata/.
// Tests run with cwd = plugins/obsidian/internal/graph/, so we walk up two
// levels to reach the plugin root.
const fixtureVault = "../../testdata/fixtures/vault-50-zettels"

// fixtureLinks returns the directed link rows that represent the 50-zettel
// fixture vault topology. Derived from the committed fixture files; must stay
// in sync with any changes to those files.
func fixtureLinks() []index.LinkRow {
	return []index.LinkRow{
		// mocs/index.md → 4 MOC pages
		{Src: "mocs/index.md", Dst: "mocs/ideas-moc.md"},
		{Src: "mocs/index.md", Dst: "mocs/sessions-moc.md"},
		{Src: "mocs/index.md", Dst: "mocs/findings-moc.md"},
		{Src: "mocs/index.md", Dst: "mocs/decisions-moc.md"},
		// mocs/ideas-moc.md → 14 idea notes (idea-13 absent from fixture)
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-01.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-02.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-03.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-04.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-05.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-06.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-07.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-08.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-09.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-10.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-11.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-12.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-14.md"},
		{Src: "mocs/ideas-moc.md", Dst: "ideas/idea-15.md"},
		// mocs/sessions-moc.md → 10 session notes
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-01.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-02.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-03.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-04.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-05.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-06.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-07.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-08.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-09.md"},
		{Src: "mocs/sessions-moc.md", Dst: "sessions/session-10.md"},
		// mocs/findings-moc.md → 5 finding notes
		{Src: "mocs/findings-moc.md", Dst: "findings/finding-01.md"},
		{Src: "mocs/findings-moc.md", Dst: "findings/finding-02.md"},
		{Src: "mocs/findings-moc.md", Dst: "findings/finding-03.md"},
		{Src: "mocs/findings-moc.md", Dst: "findings/finding-04.md"},
		{Src: "mocs/findings-moc.md", Dst: "findings/finding-05.md"},
		// mocs/decisions-moc.md → 5 decision notes
		{Src: "mocs/decisions-moc.md", Dst: "decisions/decision-01.md"},
		{Src: "mocs/decisions-moc.md", Dst: "decisions/decision-02.md"},
		{Src: "mocs/decisions-moc.md", Dst: "decisions/decision-03.md"},
		{Src: "mocs/decisions-moc.md", Dst: "decisions/decision-04.md"},
		{Src: "mocs/decisions-moc.md", Dst: "decisions/decision-05.md"},
		// cross-type edges within the vault
		{Src: "ideas/idea-01.md", Dst: "ideas/idea-02.md"},
		{Src: "ideas/idea-02.md", Dst: "ideas/idea-03.md"},
		{Src: "sessions/session-01.md", Dst: "daily/2026-01-01.md"},
		{Src: "sessions/session-01.md", Dst: "ideas/idea-01.md"},
	}
}

// errWriter always returns an error on the first Write call.
type errWriter struct{ err error }

func (w *errWriter) Write(_ []byte) (int, error) { return 0, w.err }

// T3.2 — §10.4 Phase 3
// Asserts: a 2-hop BFS from mocs/index.md over the fixed 50-zettel fixture
// vault returns the known 38-node set in deterministic sorted order.
func TestCSRGraph_BFS2Hop(t *testing.T) {
	// Verify the fixture vault directory is present.
	if _, err := os.Stat(filepath.Join(fixtureVault, "mocs/index.md")); err != nil {
		t.Fatalf("fixture missing: %v", err)
	}

	g := Build(fixtureLinks())

	got := g.BFS("mocs/index.md", 2)

	// Golden set: all nodes reachable within 2 hops from mocs/index.md,
	// excluding the seed, sorted lexicographically.
	want := []string{
		"decisions/decision-01.md",
		"decisions/decision-02.md",
		"decisions/decision-03.md",
		"decisions/decision-04.md",
		"decisions/decision-05.md",
		"findings/finding-01.md",
		"findings/finding-02.md",
		"findings/finding-03.md",
		"findings/finding-04.md",
		"findings/finding-05.md",
		"ideas/idea-01.md",
		"ideas/idea-02.md",
		"ideas/idea-03.md",
		"ideas/idea-04.md",
		"ideas/idea-05.md",
		"ideas/idea-06.md",
		"ideas/idea-07.md",
		"ideas/idea-08.md",
		"ideas/idea-09.md",
		"ideas/idea-10.md",
		"ideas/idea-11.md",
		"ideas/idea-12.md",
		"ideas/idea-14.md",
		"ideas/idea-15.md",
		"mocs/decisions-moc.md",
		"mocs/findings-moc.md",
		"mocs/ideas-moc.md",
		"mocs/sessions-moc.md",
		"sessions/session-01.md",
		"sessions/session-02.md",
		"sessions/session-03.md",
		"sessions/session-04.md",
		"sessions/session-05.md",
		"sessions/session-06.md",
		"sessions/session-07.md",
		"sessions/session-08.md",
		"sessions/session-09.md",
		"sessions/session-10.md",
	}

	if !slices.Equal(got, want) {
		t.Errorf("BFS(mocs/index.md, 2) returned %d nodes, want %d\ngot:  %v\nwant: %v",
			len(got), len(want), got, want)
	}
}

// TestGraph_Build_Empty verifies that Build with no links returns a non-nil
// graph with zero nodes; Neighbours and BFS on any node return nil.
func TestGraph_Build_Empty(t *testing.T) {
	g := Build(nil)
	if g == nil {
		t.Fatal("Build(nil) returned nil")
	}

	if n := g.Neighbours("any.md"); n != nil {
		t.Errorf("Neighbours on empty graph: want nil, got %v", n)
	}
	if b := g.BFS("any.md", 2); b != nil {
		t.Errorf("BFS on empty graph: want nil, got %v", b)
	}
}

// TestGraph_Build_DroppedBadLinks verifies that self-loops and links with
// empty Src or Dst are silently dropped during Build.
func TestGraph_Build_DroppedBadLinks(t *testing.T) {
	links := []index.LinkRow{
		{Src: "a.md", Dst: "b.md"}, // valid
		{Src: "a.md", Dst: "a.md"}, // self-loop — drop
		{Src: "", Dst: "b.md"},     // empty Src — drop
		{Src: "a.md", Dst: ""},     // empty Dst — drop
	}

	g := Build(links)

	got := g.Neighbours("a.md")
	if len(got) != 1 || got[0] != "b.md" {
		t.Errorf("expected [b.md] after dropping bad links, got %v", got)
	}
}

// TestGraph_Neighbours_Unknown verifies that Neighbours returns nil (not an
// error) for a node not present in the graph.
func TestGraph_Neighbours_Unknown(t *testing.T) {
	g := Build([]index.LinkRow{
		{Src: "a.md", Dst: "b.md"},
	})

	if n := g.Neighbours("ghost.md"); n != nil {
		t.Errorf("expected nil for unknown node, got %v", n)
	}
}

// TestGraph_Neighbours_Deterministic verifies that calling Neighbours twice on
// the same node returns identical sorted slices.
func TestGraph_Neighbours_Deterministic(t *testing.T) {
	links := []index.LinkRow{
		{Src: "hub.md", Dst: "c.md"},
		{Src: "hub.md", Dst: "a.md"},
		{Src: "hub.md", Dst: "b.md"},
	}
	g := Build(links)

	first := g.Neighbours("hub.md")
	second := g.Neighbours("hub.md")

	if !slices.Equal(first, second) {
		t.Errorf("Neighbours not deterministic: first=%v second=%v", first, second)
	}
	if !slices.IsSorted(first) {
		t.Errorf("Neighbours not sorted: %v", first)
	}
}

// TestGraph_WriteLoad_RoundTrip verifies that a graph serialised with WriteTo
// and deserialised with Load produces an identical Neighbours set for every
// node in the original.
func TestGraph_WriteLoad_RoundTrip(t *testing.T) {
	links := []index.LinkRow{
		{Src: "a.md", Dst: "b.md"},
		{Src: "a.md", Dst: "c.md"},
		{Src: "b.md", Dst: "c.md"},
	}
	orig := Build(links)

	var buf bytes.Buffer
	if _, err := orig.WriteTo(&buf); err != nil {
		t.Fatalf("WriteTo: %v", err)
	}

	loaded, err := Load(bytes.NewReader(buf.Bytes()))
	if err != nil {
		t.Fatalf("Load: %v", err)
	}

	for _, node := range []string{"a.md", "b.md", "c.md"} {
		got := loaded.Neighbours(node)
		want := orig.Neighbours(node)
		if !slices.Equal(got, want) {
			t.Errorf("Neighbours(%q) after round-trip: got %v, want %v", node, got, want)
		}
	}
}

// TestGraph_Load_CorruptCRC verifies that Load returns an error when the
// trailing CRC32 checksum does not match the payload.
func TestGraph_Load_CorruptCRC(t *testing.T) {
	g := Build([]index.LinkRow{{Src: "a.md", Dst: "b.md"}})

	var buf bytes.Buffer
	if _, err := g.WriteTo(&buf); err != nil {
		t.Fatalf("WriteTo: %v", err)
	}

	// Flip the last byte to corrupt the CRC.
	data := buf.Bytes()
	data[len(data)-1] ^= 0xFF

	_, err := Load(bytes.NewReader(data))
	if err == nil {
		t.Fatal("Load with corrupt CRC: expected error, got nil")
	}
}

// TestGraph_Load_TruncatedFile verifies that Load returns an error when the
// reader ends before the full graph payload is available.
func TestGraph_Load_TruncatedFile(t *testing.T) {
	g := Build([]index.LinkRow{{Src: "a.md", Dst: "b.md"}})

	var buf bytes.Buffer
	if _, err := g.WriteTo(&buf); err != nil {
		t.Fatalf("WriteTo: %v", err)
	}

	// Truncate to half the written size.
	truncated := buf.Bytes()[:buf.Len()/2]

	_, err := Load(bytes.NewReader(truncated))
	if err == nil {
		t.Fatal("Load with truncated data: expected error, got nil")
	}
	if !errors.Is(err, io.ErrUnexpectedEOF) && !errors.Is(err, io.EOF) {
		// Accept any wrapping of an EOF-family error.
		// The exact sentinel depends on implementation; we just require non-nil.
		_ = err
	}
}

// TestGraph_Load_WrongMagic verifies that Load returns an error immediately
// when the magic header bytes do not match the expected value.
func TestGraph_Load_WrongMagic(t *testing.T) {
	// Write four arbitrary bytes that cannot match any valid magic header.
	bad := bytes.NewReader([]byte{0x00, 0x00, 0x00, 0x00})

	_, err := Load(bad)
	if err == nil {
		t.Fatal("Load with wrong magic: expected error, got nil")
	}
}

// TestGraph_WriteTo_AtomicOnFailure verifies that WriteTo propagates a writer
// error cleanly and does not panic when the underlying writer rejects all data.
func TestGraph_WriteTo_AtomicOnFailure(t *testing.T) {
	g := Build([]index.LinkRow{
		{Src: "a.md", Dst: "b.md"},
		{Src: "b.md", Dst: "c.md"},
	})

	sentinel := errors.New("disk full")
	_, err := g.WriteTo(&errWriter{err: sentinel})
	if err == nil {
		t.Fatal("WriteTo with failing writer: expected error, got nil")
	}
	if !errors.Is(err, sentinel) {
		t.Errorf("WriteTo error should wrap sentinel: got %v", err)
	}
}
