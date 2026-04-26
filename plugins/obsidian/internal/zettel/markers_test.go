package zettel

import (
	"strings"
	"testing"
)

// markerInput is the canonical test input for T1.7 marker parsing tests.
const markerInput = `Some text before.
FINDING: The key insight about the system.
More text.
DECISION: Used atomic.Int64 over mutex.
Some more text.
PATTERN: Always use table-driven tests.
And more.
GOTCHA: Don't use init() functions.
Trailing text.`

// T1.7 — §10.4 Phase 1
// Asserts: ParseMarkers correctly extracts, orders, and categorises inline
// learning markers from phase output. Each subtest names one behavioural
// contract of the parser.
func TestMarkerParsing(t *testing.T) {
	tests := []struct {
		name         string
		input        string
		wantLen      int
		wantKinds    []MarkerKind // checked index-by-index; nil skips
		wantContents []string     // checked index-by-index; nil skips
		wantLines    []int        // checked index-by-index; nil skips
	}{
		{
			name:      "canonical input extracts four markers",
			input:     markerInput,
			wantLen:   4,
			wantKinds: []MarkerKind{MarkerFinding, MarkerDecision, MarkerPattern, MarkerGotcha},
		},
		{
			name:    "empty string returns no markers",
			input:   "",
			wantLen: 0,
		},
		{
			name:    "plain text without prefix returns no markers",
			input:   "Just some text\nno markers here\nplain sentences only.",
			wantLen: 0,
		},
		{
			name:         "FINDING only extracts one marker",
			input:        "FINDING: key insight",
			wantLen:      1,
			wantKinds:    []MarkerKind{MarkerFinding},
			wantContents: []string{"key insight"},
		},
		{
			name:      "DECISION only extracts one marker",
			input:     "DECISION: use atomics",
			wantLen:   1,
			wantKinds: []MarkerKind{MarkerDecision},
		},
		{
			name:      "PATTERN only extracts one marker",
			input:     "PATTERN: table-driven tests",
			wantLen:   1,
			wantKinds: []MarkerKind{MarkerPattern},
		},
		{
			name:      "GOTCHA only extracts one marker",
			input:     "GOTCHA: avoid globals",
			wantLen:   1,
			wantKinds: []MarkerKind{MarkerGotcha},
		},
		{
			name:      "LEARNING only extracts one marker",
			input:     "LEARNING: remember this",
			wantLen:   1,
			wantKinds: []MarkerKind{MarkerLearning},
		},
		{
			name:         "leading whitespace on marker line is trimmed",
			input:        "  FINDING: indented marker",
			wantLen:      1,
			wantKinds:    []MarkerKind{MarkerFinding},
			wantContents: []string{"indented marker"},
		},
		{
			name:    "lowercase prefix is not recognised as a marker",
			input:   "finding: lowercase\ndecision: also lower",
			wantLen: 0,
		},
		{
			name:      "line number reflects position in input",
			input:     "line one\nFINDING: second line",
			wantLen:   1,
			wantLines: []int{2},
		},
		{
			name:      "multiple markers preserve insertion order",
			input:     "FINDING: first\nGOTCHA: second",
			wantLen:   2,
			wantKinds: []MarkerKind{MarkerFinding, MarkerGotcha},
		},
		{
			name:         "canonical FINDING content matches verbatim",
			input:        markerInput,
			wantLen:      4,
			wantContents: []string{"The key insight about the system."},
		},
		{
			name:         "canonical GOTCHA content matches verbatim",
			input:        markerInput,
			wantLen:      4,
			wantContents: []string{"", "", "", "Don't use init() functions."},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := ParseMarkers(tt.input)

			if len(got) != tt.wantLen {
				t.Fatalf("ParseMarkers returned %d markers, want %d", len(got), tt.wantLen)
			}

			for i, wantKind := range tt.wantKinds {
				if i >= len(got) {
					break
				}
				if got[i].Kind != wantKind {
					t.Errorf("markers[%d].Kind = %q, want %q", i, got[i].Kind, wantKind)
				}
			}

			for i, wantContent := range tt.wantContents {
				if i >= len(got) || wantContent == "" {
					continue
				}
				if got[i].Content != wantContent {
					t.Errorf("markers[%d].Content = %q, want %q", i, got[i].Content, wantContent)
				}
			}

			for i, wantLine := range tt.wantLines {
				if i >= len(got) {
					break
				}
				if got[i].Line != wantLine {
					t.Errorf("markers[%d].Line = %d, want %d", i, got[i].Line, wantLine)
				}
			}
		})
	}

	// Separate subtests for behaviours that don't fit the scalar table above.

	t.Run("all content fields are non-empty in canonical input", func(t *testing.T) {
		markers := ParseMarkers(markerInput)
		for i, m := range markers {
			if strings.TrimSpace(m.Content) == "" {
				t.Errorf("markers[%d].Content is empty", i)
			}
		}
	})

	t.Run("all line numbers are positive in canonical input", func(t *testing.T) {
		markers := ParseMarkers(markerInput)
		for i, m := range markers {
			if m.Line <= 0 {
				t.Errorf("markers[%d].Line = %d, want > 0", i, m.Line)
			}
		}
	})

	t.Run("canonical output matches golden file", func(t *testing.T) {
		markers := ParseMarkers(markerInput)
		checkGolden(t, "markers_all_kinds.txt", FormatMarkers(markers))
	})

	t.Run("parsing is deterministic for identical input", func(t *testing.T) {
		m1 := ParseMarkers(markerInput)
		m2 := ParseMarkers(markerInput)
		if FormatMarkers(m1) != FormatMarkers(m2) {
			t.Error("ParseMarkers is non-deterministic: two calls with same input differ")
		}
	})
}

// T1.7 (fuzz) — §10.4 Phase 1
// Fuzz target for the marker parser: arbitrary byte sequences must not crash
// the parser and must produce deterministic output for fixed seeds.
func FuzzMarkerParser(f *testing.F) {
	// Inline seeds (supplemented by testdata/corpus/FuzzMarkerParser/).
	f.Add(markerInput)
	f.Add("")
	f.Add("FINDING: ")
	f.Add("FINDING:no space")
	f.Add("not a marker\nFINDING: real\nGOTCHA: also real")
	f.Add("FINDING: \x00\x01\x02 binary")
	f.Add(strings.Repeat("FINDING: x\n", 1000))

	f.Fuzz(func(t *testing.T, input string) {
		// Must not panic.
		markers := ParseMarkers(input)

		// FormatMarkers must not panic on any marker slice.
		out := FormatMarkers(markers)

		// Output must be idempotent: re-parsing the formatted output
		// and re-formatting must equal the first-pass output.
		markers2 := ParseMarkers(out)
		out2 := FormatMarkers(markers2)
		if out != out2 {
			t.Errorf("FormatMarkers(ParseMarkers(FormatMarkers(ParseMarkers(input)))) != FormatMarkers(ParseMarkers(input))\ninput: %q\nout:  %q\nout2: %q",
				input, out, out2)
		}
	})
}
