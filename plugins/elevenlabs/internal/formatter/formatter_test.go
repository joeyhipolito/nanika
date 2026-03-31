package formatter

import (
	"strings"
	"testing"
)

// ─── intToWords ───────────────────────────────────────────────────────────────

func TestIntToWords(t *testing.T) {
	cases := []struct {
		n    int
		want string
	}{
		{0, "zero"},
		{1, "one"},
		{9, "nine"},
		{10, "ten"},
		{11, "eleven"},
		{19, "nineteen"},
		{20, "twenty"},
		{21, "twenty-one"},
		{99, "ninety-nine"},
		{100, "one hundred"},
		{101, "one hundred one"},
		{329, "three hundred twenty-nine"},
		{1000, "one thousand"},
		{1001, "one thousand one"},
		{16000000, "sixteen million"},
		{780000, "seven hundred eighty thousand"},
		{1000000000, "one billion"},
	}
	for _, tc := range cases {
		t.Run(tc.want, func(t *testing.T) {
			got := intToWords(tc.n)
			if got != tc.want {
				t.Errorf("intToWords(%d) = %q, want %q", tc.n, got, tc.want)
			}
		})
	}
}

// ─── ordinalToWords ───────────────────────────────────────────────────────────

func TestOrdinalToWords(t *testing.T) {
	cases := []struct {
		n    int
		want string
	}{
		{1, "first"},
		{2, "second"},
		{3, "third"},
		{4, "fourth"},
		{5, "fifth"},
		{12, "twelfth"},
		{17, "seventeenth"},
		{20, "twentieth"},
		{21, "twenty-first"},
		{22, "twenty-second"},
		{45, "forty-fifth"},
	}
	for _, tc := range cases {
		t.Run(tc.want, func(t *testing.T) {
			got := ordinalToWords(tc.n)
			if got != tc.want {
				t.Errorf("ordinalToWords(%d) = %q, want %q", tc.n, got, tc.want)
			}
		})
	}
}

// ─── yearToWords ─────────────────────────────────────────────────────────────

func TestYearToWords(t *testing.T) {
	cases := []struct {
		y    int
		want string
	}{
		{2000, "two thousand"},
		{2001, "two thousand one"},
		{2009, "two thousand nine"},
		{2010, "twenty ten"},
		{2021, "twenty twenty-one"},
		{2026, "twenty twenty-six"},
	}
	for _, tc := range cases {
		t.Run(tc.want, func(t *testing.T) {
			got := yearToWords(tc.y)
			if got != tc.want {
				t.Errorf("yearToWords(%d) = %q, want %q", tc.y, got, tc.want)
			}
		})
	}
}

// ─── timeToWords ─────────────────────────────────────────────────────────────

func TestTimeToWords(t *testing.T) {
	cases := []struct {
		h, m  int
		ampm  string
		want  string
	}{
		{5, 1, "pm", "five oh one p.m."},
		{10, 30, "am", "ten thirty a.m."},
		{12, 0, "pm", "twelve p.m."},
		{3, 15, "AM", "three fifteen a.m."},
	}
	for _, tc := range cases {
		t.Run(tc.want, func(t *testing.T) {
			got := timeToWords(tc.h, tc.m, tc.ampm)
			if got != tc.want {
				t.Errorf("timeToWords(%d,%d,%s) = %q, want %q", tc.h, tc.m, tc.ampm, got, tc.want)
			}
		})
	}
}

// ─── normalizeNumbers ────────────────────────────────────────────────────────

func TestNormalizeNumbers(t *testing.T) {
	cases := []struct {
		input string
		want  string
	}{
		{"5:01pm", "five oh one p.m."},
		{"95%", "ninety-five percent"},
		{"76%", "seventy-six percent"},
		{"1st", "first"},
		{"21st", "twenty-first"},
		{"45th", "forty-fifth"},
		{"780,000 words", "seven hundred eighty thousand words"},
		{"16 million interactions", "sixteen million interactions"},
		{"329 total turns", "three hundred twenty-nine total turns"},
		{"February 17th, 2026", "February seventeenth, twenty twenty-six"},
		// Version numbers must be left alone.
		{"model v3.0", "model v3.0"},
		{"RSP v2.2", "R.S.P. v2.2"},
		// Plain integers.
		{"20 out of 21", "twenty out of twenty-one"},
		{"8 minutes", "eight minutes"},
	}
	for _, tc := range cases {
		t.Run(tc.input, func(t *testing.T) {
			got := normalizeNumbers(tc.input)
			// For abbreviation expansion we run normalizeAbbreviations after, but
			// we verify numbers here independently.
			if tc.input == "RSP v2.2" {
				got = normalizeAbbreviations(normalizeNumbers(tc.input))
			}
			if got != tc.want {
				t.Errorf("normalizeNumbers(%q) = %q, want %q", tc.input, got, tc.want)
			}
		})
	}
}

// ─── normalizeAbbreviations ───────────────────────────────────────────────────

func TestNormalizeAbbreviations(t *testing.T) {
	cases := []struct {
		input string
		want  string
	}{
		{"AI model", "A.I. model"},
		{"RSP version", "R.S.P. version"},
		{"R&D budget", "R and D budget"},
		// skipWords must not be dotted.
		{"AND the OR for", "AND the OR for"},
		// DoD mixed-case.
		{"the DoD contract", "the D.O.D. contract"},
		// NLE pipeline.
		{"in the NLE", "in the N.L.E."},
		// Multiple abbreviations.
		{"AI and RSP", "A.I. and R.S.P."},
	}
	for _, tc := range cases {
		t.Run(tc.input, func(t *testing.T) {
			got := normalizeAbbreviations(tc.input)
			if got != tc.want {
				t.Errorf("normalizeAbbreviations(%q) = %q, want %q", tc.input, got, tc.want)
			}
		})
	}
}

// ─── isMetadataHeader ────────────────────────────────────────────────────────

func TestIsMetadataHeader(t *testing.T) {
	cases := []struct {
		title string
		want  bool
	}{
		{"Self-Check", true},
		{"Clip Budget Notes", true},
		{"Narrator Notes for Editor", true},
		{"[SCENE MARKERS REFERENCE]", true},
		{"TIMING REFERENCE", true},
		{"LEARNING", true},
		{"SOURCES EMBEDDED", true},
		{"THE SETUP", false},
		{"THE NUMBERS", false},
		{"THE CLOSE", false},
		{"THE HONEST HEDGE", false},
	}
	for _, tc := range cases {
		t.Run(tc.title, func(t *testing.T) {
			got := isMetadataHeader(tc.title)
			if got != tc.want {
				t.Errorf("isMetadataHeader(%q) = %v, want %v", tc.title, got, tc.want)
			}
		})
	}
}

// ─── Format (integration) ────────────────────────────────────────────────────

func TestFormat_StripsScenesAndMetadata(t *testing.T) {
	input := `# Test Script

**Format**: 8-minute video

[SCENE: Pentagon exterior at dusk]

On Friday afternoon at 5:01pm, Pete Hegseth gave Anthropic three options.

[SCENE: another scene]

The AI model was involved.

---

## Self-Check

- [x] Cold open drops into the event
- [x] Promise hook

## Clip Budget Notes

60 clips × 8 seconds = 480 seconds
`
	out, _, err := Format([]byte(input))
	if err != nil {
		t.Fatalf("Format() error: %v", err)
	}
	result := string(out)

	// Must have documentary style tag.
	if !strings.HasPrefix(result, "[documentary style]") {
		t.Errorf("expected result to start with [documentary style], got: %q", result[:min(50, len(result))])
	}
	// Must contain pause between scenes.
	if !strings.Contains(result, "[pause]") {
		t.Errorf("expected [pause] tag in result")
	}
	// Must not contain [SCENE: ...].
	if strings.Contains(result, "[SCENE:") {
		t.Errorf("expected [SCENE:] to be stripped")
	}
	// Must not contain Self-Check content.
	if strings.Contains(result, "Cold open drops") {
		t.Errorf("expected self-check content to be stripped")
	}
	// Must not contain metadata.
	if strings.Contains(result, "8-minute video") {
		t.Errorf("expected front-matter metadata to be stripped")
	}
	// Must normalize numbers.
	if strings.Contains(result, "5:01pm") {
		t.Errorf("expected 5:01pm to be normalized, still present in: %s", result)
	}
	// Must normalize abbreviations.
	if strings.Contains(result, " AI ") {
		t.Errorf("expected AI to be expanded to A.I.")
	}
	// Must contain narration text.
	if !strings.Contains(result, "Pete Hegseth") {
		t.Errorf("expected narration text to be preserved")
	}
}

func TestFormat_StripsVisualOnlyBlocks(t *testing.T) {
	input := `# Test

First paragraph.

**VISUAL-ONLY SECTION — 30 seconds**

(The score ticks up slowly. Numbers appearing one by one.)

---

Second paragraph.
`
	out, _, err := Format([]byte(input))
	if err != nil {
		t.Fatalf("Format() error: %v", err)
	}
	result := string(out)

	if strings.Contains(result, "VISUAL-ONLY") {
		t.Errorf("VISUAL-ONLY header should be stripped")
	}
	if strings.Contains(result, "score ticks up") {
		t.Errorf("visual-only parenthetical content should be stripped")
	}
	if !strings.Contains(result, "First paragraph") {
		t.Errorf("first paragraph should be present")
	}
	if !strings.Contains(result, "Second paragraph") {
		t.Errorf("second paragraph should be present")
	}
}

func TestFormat_StripsQuotes(t *testing.T) {
	input := `# Test

He said: "The world is in peril."

She replied "Fully autonomous weapons would not make that distinction."
`
	out, _, err := Format([]byte(input))
	if err != nil {
		t.Fatalf("Format() error: %v", err)
	}
	result := string(out)

	if strings.Contains(result, `"`) {
		t.Errorf("double quotes should be stripped, got: %s", result)
	}
	if !strings.Contains(result, "The world is in peril") {
		t.Errorf("quote content should be preserved")
	}
}

func TestFormat_DocumentaryStyleOnFirstParagraph(t *testing.T) {
	input := `# My Script

Hello world.

---

Second section.
`
	out, _, err := Format([]byte(input))
	if err != nil {
		t.Fatalf("Format() error: %v", err)
	}
	result := string(out)

	if !strings.HasPrefix(result, "[documentary style] Hello world.") {
		t.Errorf("expected [documentary style] on first paragraph, got: %q", result[:min(60, len(result))])
	}
}

func min(a, b int) int {
	if a < b {
		return a
	}
	return b
}

// ─── Clip manifest ──────────────────────────────────────────────────────────

func TestFormat_ClipManifest(t *testing.T) {
	input := `# Test Script

**[0:00–0:08] | Narrated**
First narrated clip.

---

**[0:08–0:16] | VISUAL ONLY**

---

**[0:16–0:24] | Narrated**
Second narrated clip.

---

**[0:24–0:32] | VISUAL ONLY**

---

**[0:32–0:40] | VISUAL ONLY**

---

**[0:40–0:48] | Narrated**
Third narrated clip.
`
	out, manifest, err := Format([]byte(input))
	if err != nil {
		t.Fatalf("Format() error: %v", err)
	}
	result := string(out)

	// No break tags anywhere.
	if strings.Contains(result, "<break") {
		t.Errorf("break tags must not appear in output, got: %s", result)
	}

	// No [long pause] — suppressed when clip headers present.
	if strings.Contains(result, "[long pause]") {
		t.Errorf("[long pause] must not appear when clip headers are present, got: %s", result)
	}

	// Must have [pause] between narrated segments.
	if strings.Count(result, "[pause]") != 2 {
		t.Errorf("expected 2 [pause] tags (between 3 segments), got %d in: %s",
			strings.Count(result, "[pause]"), result)
	}

	// All narrated text present.
	for _, want := range []string{"First narrated clip", "Second narrated clip", "Third narrated clip"} {
		if !strings.Contains(result, want) {
			t.Errorf("expected %q in output", want)
		}
	}

	// Manifest must be non-nil.
	if manifest == nil {
		t.Fatal("expected non-nil manifest with clip headers")
	}

	// Verify manifest fields.
	if manifest.ClipGridSeconds != 8 {
		t.Errorf("ClipGridSeconds = %d, want 8", manifest.ClipGridSeconds)
	}
	if manifest.TotalClips != 6 {
		t.Errorf("TotalClips = %d, want 6", manifest.TotalClips)
	}
	if manifest.TotalDurationSeconds != 48 {
		t.Errorf("TotalDurationSeconds = %d, want 48", manifest.TotalDurationSeconds)
	}
	if manifest.NarratedClips != 3 {
		t.Errorf("NarratedClips = %d, want 3", manifest.NarratedClips)
	}
	if manifest.VisualOnlyClips != 3 {
		t.Errorf("VisualOnlyClips = %d, want 3", manifest.VisualOnlyClips)
	}
	if len(manifest.Segments) != 3 {
		t.Fatalf("len(Segments) = %d, want 3", len(manifest.Segments))
	}

	// Segment 0: clip 1, followed by 1 visual-only → silence_after=8
	if manifest.Segments[0].Clip != 1 || manifest.Segments[0].SilenceAfter != 8 {
		t.Errorf("Segments[0] = {Clip:%d, SilenceAfter:%d}, want {1, 8}",
			manifest.Segments[0].Clip, manifest.Segments[0].SilenceAfter)
	}
	// Segment 1: clip 3, followed by 2 visual-only → silence_after=16
	if manifest.Segments[1].Clip != 3 || manifest.Segments[1].SilenceAfter != 16 {
		t.Errorf("Segments[1] = {Clip:%d, SilenceAfter:%d}, want {3, 16}",
			manifest.Segments[1].Clip, manifest.Segments[1].SilenceAfter)
	}
	// Segment 2: clip 6, no following clips → silence_after=0
	if manifest.Segments[2].Clip != 6 || manifest.Segments[2].SilenceAfter != 0 {
		t.Errorf("Segments[2] = {Clip:%d, SilenceAfter:%d}, want {6, 0}",
			manifest.Segments[2].Clip, manifest.Segments[2].SilenceAfter)
	}
}

func TestFormat_NoManifestWithoutClipHeaders(t *testing.T) {
	input := `# Plain Script

First paragraph.

---

Second paragraph.
`
	out, manifest, err := Format([]byte(input))
	if err != nil {
		t.Fatalf("Format() error: %v", err)
	}
	result := string(out)

	if manifest != nil {
		t.Errorf("expected nil manifest without clip headers")
	}
	// [long pause] should still work without clip headers.
	if !strings.Contains(result, "[long pause]") {
		t.Errorf("expected [long pause] from HR in non-clip-header mode")
	}
	if !strings.Contains(result, "First paragraph") || !strings.Contains(result, "Second paragraph") {
		t.Errorf("expected both paragraphs in output")
	}
}

func TestFormat_VisualOnlyNoBreakTags(t *testing.T) {
	input := `# Test

**[0:00–0:08] | Narrated**
Speech here.

---

**[0:08–0:16] | VISUAL ONLY**

---

**[0:16–0:24] | Narrated**
More speech.
`
	out, _, err := Format([]byte(input))
	if err != nil {
		t.Fatalf("Format() error: %v", err)
	}
	result := string(out)

	if strings.Contains(result, "<break") {
		t.Errorf("must not contain break tags, got: %s", result)
	}
	if strings.Contains(result, "VISUAL") {
		t.Errorf("must not contain VISUAL ONLY text, got: %s", result)
	}
}

func TestParseClipGridSeconds(t *testing.T) {
	cases := []struct {
		header string
		want   int
	}{
		{"**[0:00–0:08] | Narrated**", 8},
		{"**[0:00-0:10] | Narrated**", 10},
		{"**[1:20–1:28] | VISUAL ONLY**", 8},
		{"**[0–8] Clip 1**", 8},            // legacy (no colons) → default
		{"no clip header at all", 8},        // no match → default
		{"**[0:00–0:06] | Narrated**", 6},   // 6-second grid
	}
	for _, tc := range cases {
		t.Run(tc.header, func(t *testing.T) {
			got := parseClipGridSeconds(tc.header)
			if got != tc.want {
				t.Errorf("parseClipGridSeconds(%q) = %d, want %d", tc.header, got, tc.want)
			}
		})
	}
}

func TestBuildManifest_SilenceAfter(t *testing.T) {
	clips := []clipEntry{
		{1, clipNarrated},
		{2, clipNarrated},
		{3, clipVisualOnly},
		{4, clipNarrated},
		{5, clipVisualOnly},
		{6, clipVisualOnly},
	}
	segments := []ManifestEntry{
		{Index: 0, Clip: 1},
		{Index: 1, Clip: 2},
		{Index: 2, Clip: 4},
	}
	m := buildManifest(clips, segments, 8)

	// Segment 0 (clip 1): next is clip 2 (narrated) → 0
	if m.Segments[0].SilenceAfter != 0 {
		t.Errorf("segments[0].SilenceAfter = %d, want 0", m.Segments[0].SilenceAfter)
	}
	// Segment 1 (clip 2): next is clip 3 (visual_only), then clip 4 (narrated) → 8
	if m.Segments[1].SilenceAfter != 8 {
		t.Errorf("segments[1].SilenceAfter = %d, want 8", m.Segments[1].SilenceAfter)
	}
	// Segment 2 (clip 4): next are clips 5,6 (visual_only) → 16
	if m.Segments[2].SilenceAfter != 16 {
		t.Errorf("segments[2].SilenceAfter = %d, want 16", m.Segments[2].SilenceAfter)
	}
	// Verify derived fields.
	if m.NarratedClips != 3 {
		t.Errorf("NarratedClips = %d, want 3", m.NarratedClips)
	}
	if m.VisualOnlyClips != 3 {
		t.Errorf("VisualOnlyClips = %d, want 3", m.VisualOnlyClips)
	}
	if m.TotalDurationSeconds != 48 {
		t.Errorf("TotalDurationSeconds = %d, want 48", m.TotalDurationSeconds)
	}
}
