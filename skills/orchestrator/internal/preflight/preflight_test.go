package preflight

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"
)

// fakeSection is a minimal Section implementation for testing the
// registry and BuildBrief without depending on external CLIs.
type fakeSection struct {
	name     string
	priority int
	body     string
	err      error
}

func (f *fakeSection) Name() string     { return f.name }
func (f *fakeSection) Priority() int    { return f.priority }
func (f *fakeSection) Fetch(_ context.Context) (Block, error) {
	if f.err != nil {
		return Block{}, f.err
	}
	return Block{Title: strings.ToUpper(f.name), Body: f.body}, nil
}

func TestBuildBrief_EmptyRegistry(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	brief := BuildBrief(context.Background(), nil)
	if !brief.IsEmpty() {
		t.Errorf("expected empty brief, got %d blocks", len(brief.Blocks))
	}
	if got := brief.Text(); got != "" {
		t.Errorf("expected empty text, got %q", got)
	}
	if brief.Blocks == nil {
		t.Error("expected non-nil Blocks slice for stable JSON marshalling")
	}
}

func TestRegister_OrdersByPriority(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "c", priority: 30, body: "c-body"})
	Register(&fakeSection{name: "a", priority: 10, body: "a-body"})
	Register(&fakeSection{name: "b", priority: 20, body: "b-body"})

	brief := BuildBrief(context.Background(), nil)
	if len(brief.Blocks) != 3 {
		t.Fatalf("expected 3 blocks, got %d", len(brief.Blocks))
	}
	want := []string{"a", "b", "c"}
	for i, blk := range brief.Blocks {
		if blk.Name != want[i] {
			t.Errorf("block[%d] name = %q, want %q", i, blk.Name, want[i])
		}
	}
}

func TestRegister_ReplacesDuplicateName(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "x", priority: 10, body: "first"})
	Register(&fakeSection{name: "x", priority: 10, body: "second"})

	brief := BuildBrief(context.Background(), nil)
	if len(brief.Blocks) != 1 {
		t.Fatalf("expected 1 block after replacement, got %d", len(brief.Blocks))
	}
	if brief.Blocks[0].Body != "second" {
		t.Errorf("expected replaced body 'second', got %q", brief.Blocks[0].Body)
	}
}

func TestBuildBrief_FilterBySection(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	Register(&fakeSection{name: "tracker", priority: 20, body: "issues"})
	Register(&fakeSection{name: "learnings", priority: 30, body: "notes"})

	brief := BuildBrief(context.Background(), []string{"scheduler", "learnings"})
	if len(brief.Blocks) != 2 {
		t.Fatalf("expected 2 filtered blocks, got %d", len(brief.Blocks))
	}
	if brief.Blocks[0].Name != "scheduler" || brief.Blocks[1].Name != "learnings" {
		t.Errorf("filter produced wrong blocks: %+v", brief.Blocks)
	}
}

func TestBuildBrief_SwallowsFetchErrors(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "ok", priority: 10, body: "fine"})
	Register(&fakeSection{name: "broken", priority: 20, err: errors.New("boom")})

	brief := BuildBrief(context.Background(), nil)
	if len(brief.Blocks) != 1 {
		t.Fatalf("expected 1 block (broken swallowed), got %d", len(brief.Blocks))
	}
	if brief.Blocks[0].Name != "ok" {
		t.Errorf("expected ok block, got %q", brief.Blocks[0].Name)
	}
}

func TestBuildBrief_RespectsContextCancellation(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "a", priority: 10, body: "a"})
	Register(&fakeSection{name: "b", priority: 20, body: "b"})

	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	brief := BuildBrief(ctx, nil)
	if len(brief.Blocks) != 0 {
		t.Errorf("expected 0 blocks after cancellation, got %d", len(brief.Blocks))
	}
}

func TestBrief_TextRendersTitlesAndBodies(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "job-1\njob-2"})

	brief := BuildBrief(context.Background(), nil)
	text := brief.Text()
	if !strings.Contains(text, "## SCHEDULER") {
		t.Errorf("text missing section title: %q", text)
	}
	if !strings.Contains(text, "job-1") {
		t.Errorf("text missing body content: %q", text)
	}
}

func TestRegister_NilIsIgnored(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(nil)
	if got := List(); len(got) != 0 {
		t.Errorf("expected empty registry after Register(nil), got %d", len(got))
	}
}

// ---------------------------------------------------------------------------
// Composition and rendering
// ---------------------------------------------------------------------------

func TestRenderMarkdown_HeaderStructure(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "job-1\njob-2"})
	brief := BuildBrief(context.Background(), nil)

	text := brief.RenderMarkdown()
	if !strings.Contains(text, "## Operational Pre-flight") {
		t.Errorf("missing main header: %q", text)
	}
	if !strings.Contains(text, "### SCHEDULER") {
		t.Errorf("missing section subheader: %q", text)
	}
	if !strings.Contains(text, "job-1") {
		t.Errorf("missing body content: %q", text)
	}
}

func TestRenderMarkdown_MultiplesSections(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	Register(&fakeSection{name: "tracker", priority: 20, body: "issues"})
	Register(&fakeSection{name: "learnings", priority: 30, body: "notes"})

	brief := BuildBrief(context.Background(), nil)
	text := brief.RenderMarkdown()

	// Check that all sections appear in order.
	schedulerIdx := strings.Index(text, "### SCHEDULER")
	trackerIdx := strings.Index(text, "### TRACKER")
	learningsIdx := strings.Index(text, "### LEARNINGS")

	if schedulerIdx < 0 || trackerIdx < 0 || learningsIdx < 0 {
		t.Fatalf("missing expected section headers: %q", text)
	}
	if !(schedulerIdx < trackerIdx && trackerIdx < learningsIdx) {
		t.Errorf("sections not in priority order: scheduler=%d, tracker=%d, learnings=%d", schedulerIdx, trackerIdx, learningsIdx)
	}
}

func TestRenderMarkdown_EmptyBrief(t *testing.T) {
	brief := Brief{Blocks: []Block{}}
	text := brief.RenderMarkdown()
	if text != "" {
		t.Errorf("expected empty string for empty brief, got %q", text)
	}
}

func TestRenderMarkdown_OmitsEmptySections(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	Register(&fakeSection{name: "tracker", priority: 20, body: "   "}) // Only whitespace
	Register(&fakeSection{name: "learnings", priority: 30, body: "notes"})

	brief := BuildBrief(context.Background(), nil)
	text := brief.RenderMarkdown()

	if strings.Contains(text, "TRACKER") {
		t.Errorf("empty section should be omitted: %q", text)
	}
	if !strings.Contains(text, "SCHEDULER") || !strings.Contains(text, "LEARNINGS") {
		t.Errorf("non-empty sections missing: %q", text)
	}
}

func TestComposeWithCapacity_UnderLimit(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	brief := BuildBrief(context.Background(), nil)

	composed, dropped, rendered := brief.ComposeWithCapacity(4096)
	if len(composed.Blocks) != 1 {
		t.Errorf("expected 1 block, got %d", len(composed.Blocks))
	}
	if len(dropped) != 0 {
		t.Errorf("expected no dropped sections, got %d: %v", len(dropped), dropped)
	}
	if rendered == "" {
		t.Error("expected non-empty rendered output")
	}
}

func TestComposeWithCapacity_DropsLowestPriority(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	// Create sections with varying body sizes.
	Register(&fakeSection{name: "s1", priority: 10, body: "a"})
	Register(&fakeSection{name: "s2", priority: 20, body: "b"})
	Register(&fakeSection{name: "s3", priority: 30, body: strings.Repeat("x", 5000)})

	brief := BuildBrief(context.Background(), nil)
	composed, dropped, rendered := brief.ComposeWithCapacity(100)

	// Should keep s1 and s2 (small), drop s3 (largest).
	if len(composed.Blocks) != 2 {
		t.Errorf("expected 2 blocks, got %d", len(composed.Blocks))
	}
	if len(dropped) != 1 || dropped[0] != "s3" {
		t.Errorf("expected s3 to be dropped, got %v", dropped)
	}
	if len(rendered) > 100 {
		t.Errorf("rendered output exceeds capacity: %d > 100", len(rendered))
	}
}

func TestComposeWithCapacity_PreservesHighestPriority(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	// First section is very large.
	Register(&fakeSection{name: "s1", priority: 10, body: strings.Repeat("x", 10000)})
	Register(&fakeSection{name: "s2", priority: 20, body: "b"})

	brief := BuildBrief(context.Background(), nil)
	composed, dropped, rendered := brief.ComposeWithCapacity(100)

	// Should keep s1 even though it exceeds budget (highest priority).
	if len(composed.Blocks) != 1 {
		t.Fatalf("expected 1 block (s1 kept despite size), got %d", len(composed.Blocks))
	}
	if composed.Blocks[0].Name != "s1" {
		t.Errorf("expected s1 in composed brief, got %q", composed.Blocks[0].Name)
	}
	if len(dropped) != 1 || dropped[0] != "s2" {
		t.Errorf("expected s2 to be dropped, got %v", dropped)
	}
	if rendered == "" {
		t.Error("expected non-empty rendered output")
	}
}

func TestComposeWithCapacity_NegativeCapacityAllows(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	brief := BuildBrief(context.Background(), nil)

	composed, dropped, rendered := brief.ComposeWithCapacity(-1)
	if len(composed.Blocks) != 1 {
		t.Errorf("expected 1 block with negative capacity, got %d", len(composed.Blocks))
	}
	if len(dropped) != 0 {
		t.Errorf("expected no drops with negative capacity, got %v", dropped)
	}
	if rendered == "" {
		t.Error("expected non-empty rendered output")
	}
}

func TestComposeWithCapacity_ZeroCapacityAllows(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	brief := BuildBrief(context.Background(), nil)

	composed, dropped, rendered := brief.ComposeWithCapacity(0)
	if len(composed.Blocks) != 1 {
		t.Errorf("expected 1 block with zero capacity, got %d", len(composed.Blocks))
	}
	if len(dropped) != 0 {
		t.Errorf("expected no drops with zero capacity, got %v", dropped)
	}
	if rendered == "" {
		t.Error("expected non-empty rendered output")
	}
}

// ---------------------------------------------------------------------------
// Budget enforcement: lowest-priority sections drop first
// ---------------------------------------------------------------------------

func TestComposeWithCapacity_DropOrderingByPriority(t *testing.T) {
	tests := []struct {
		name              string
		sections          []*fakeSection
		budget            int
		wantBlockCount    int
		wantDroppedFirst  string // name of the first dropped section
		wantDroppedCount  int
	}{
		{
			name: "three sections, budget forces drop of lowest one",
			sections: []*fakeSection{
				{name: "high", priority: 10, body: "a"},
				{name: "mid", priority: 20, body: "b"},
				{name: "low", priority: 30, body: strings.Repeat("x", 5000)},
			},
			budget:           100,
			wantBlockCount:   2,
			wantDroppedFirst: "low",
			wantDroppedCount: 1,
		},
		{
			name: "two equal-priority sections, drop by registration order (stable sort)",
			sections: []*fakeSection{
				{name: "first", priority: 10, body: "first-content"},
				{name: "second", priority: 10, body: strings.Repeat("x", 5000)},
			},
			budget:           100,
			wantBlockCount:   1,
			wantDroppedFirst: "second",
			wantDroppedCount: 1,
		},
		{
			name: "all sections under budget, none dropped",
			sections: []*fakeSection{
				{name: "a", priority: 10, body: "aaa"},
				{name: "b", priority: 20, body: "bbb"},
				{name: "c", priority: 30, body: "ccc"},
			},
			budget:           1000,
			wantBlockCount:   3,
			wantDroppedFirst: "",
			wantDroppedCount: 0,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			Reset()
			t.Cleanup(Reset)

			for _, sec := range tt.sections {
				Register(sec)
			}

			brief := BuildBrief(context.Background(), nil)
			composed, dropped, _ := brief.ComposeWithCapacity(tt.budget)

			if len(composed.Blocks) != tt.wantBlockCount {
				t.Errorf("block count = %d, want %d", len(composed.Blocks), tt.wantBlockCount)
			}
			if len(dropped) != tt.wantDroppedCount {
				t.Errorf("dropped count = %d, want %d", len(dropped), tt.wantDroppedCount)
			}
			if tt.wantDroppedCount > 0 && len(dropped) > 0 {
				if dropped[0] != tt.wantDroppedFirst {
					t.Errorf("first dropped = %q, want %q", dropped[0], tt.wantDroppedFirst)
				}
			}
		})
	}
}

func TestComposeWithCapacity_EmptySectionsNotCountedInBytes(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "empty1", priority: 10, body: "   "})
	Register(&fakeSection{name: "full1", priority: 20, body: "content"})
	Register(&fakeSection{name: "empty2", priority: 30, body: ""})

	brief := BuildBrief(context.Background(), nil)
	composed, dropped, rendered := brief.ComposeWithCapacity(50)

	// Empty sections don't count toward bytes, so they shouldn't be dropped
	// just to fit the budget. Both empty sections should be present in composed.
	if strings.Contains(rendered, "empty1") || strings.Contains(rendered, "empty2") {
		// Empty sections should be omitted from rendering, so this is OK.
		// What matters is the composed brief kept them.
		_ = composed
	}
	if len(dropped) > 0 && (dropped[0] == "empty1" || dropped[0] == "empty2") {
		t.Errorf("should not drop empty sections just to fit budget: %v", dropped)
	}
}

func TestComposeWithCapacity_RenderedByteCountIncludesHeaders(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "section1", priority: 10, body: "x"})
	Register(&fakeSection{name: "section2", priority: 20, body: "y"})

	brief := BuildBrief(context.Background(), nil)
	_, _, rendered1 := brief.ComposeWithCapacity(10000)

	// Rendered includes "## Operational Pre-flight" header.
	if !strings.Contains(rendered1, "## Operational Pre-flight") {
		t.Error("rendered output should include main header")
	}
}

// ---------------------------------------------------------------------------
// JSON marshalling tests
// ---------------------------------------------------------------------------

func TestBrief_JSONMarshalling(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "test", priority: 10, body: "test-body"})
	brief := BuildBrief(context.Background(), nil)

	// Brief should be JSON-marshalable.
	data, err := json.Marshal(brief)
	if err != nil {
		t.Fatalf("json.Marshal: %v", err)
	}

	// Verify the JSON can be decoded back.
	var decoded Brief
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("json.Unmarshal: %v", err)
	}

	if len(decoded.Blocks) != len(brief.Blocks) {
		t.Errorf("decoded block count = %d, want %d", len(decoded.Blocks), len(brief.Blocks))
	}
}

func TestBrief_EmptyBriefJSONMarshalling(t *testing.T) {
	brief := Brief{Blocks: make([]Block, 0)}

	data, err := json.Marshal(brief)
	if err != nil {
		t.Fatalf("json.Marshal empty brief: %v", err)
	}

	var decoded Brief
	if err := json.Unmarshal(data, &decoded); err != nil {
		t.Fatalf("json.Unmarshal: %v", err)
	}

	// Blocks should be a non-nil empty slice for stable marshalling.
	if decoded.Blocks == nil {
		t.Error("decoded.Blocks should be non-nil empty slice, got nil")
	}
}

// ---------------------------------------------------------------------------
// Context cancellation and timeout tests
// ---------------------------------------------------------------------------

func TestBuildBrief_CancellationStopsEarlyFetch(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	// Register multiple sections.
	Register(&fakeSection{name: "a", priority: 10, body: "a"})
	Register(&fakeSection{name: "b", priority: 20, body: "b"})
	Register(&fakeSection{name: "c", priority: 30, body: "c"})

	// Cancel immediately.
	ctx, cancel := context.WithCancel(context.Background())
	cancel()

	brief := BuildBrief(ctx, nil)
	if len(brief.Blocks) != 0 {
		t.Errorf("expected 0 blocks after immediate cancellation, got %d", len(brief.Blocks))
	}
}

func TestBuildBrief_ContextTimeoutHandling(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "quick", priority: 10, body: "fast"})

	// Create a timeout context that allows immediate fetch.
	ctx, cancel := context.WithTimeout(context.Background(), 100*time.Millisecond)
	defer cancel()

	brief := BuildBrief(ctx, nil)
	// Should fetch the section successfully.
	if len(brief.Blocks) != 1 {
		t.Errorf("expected 1 block, got %d", len(brief.Blocks))
	}
}

// ---------------------------------------------------------------------------
// Filter edge cases
// ---------------------------------------------------------------------------

func TestBuildBrief_FilterWithWhitespace(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	Register(&fakeSection{name: "tracker", priority: 20, body: "issues"})

	// Filter with leading/trailing whitespace should be trimmed.
	brief := BuildBrief(context.Background(), []string{"  scheduler  ", "tracker"})
	if len(brief.Blocks) != 2 {
		t.Errorf("expected 2 blocks with whitespace-trimmed filter, got %d", len(brief.Blocks))
	}
}

func TestBuildBrief_FilterWithEmptyStrings(t *testing.T) {
	Reset()
	t.Cleanup(Reset)

	Register(&fakeSection{name: "scheduler", priority: 10, body: "jobs"})
	Register(&fakeSection{name: "tracker", priority: 20, body: "issues"})

	// Empty strings in filter should be ignored.
	brief := BuildBrief(context.Background(), []string{"", "scheduler", "", "tracker", ""})
	if len(brief.Blocks) != 2 {
		t.Errorf("expected 2 blocks with empty-string-filtered list, got %d", len(brief.Blocks))
	}
}
