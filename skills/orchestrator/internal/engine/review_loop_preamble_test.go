package engine

import (
	"strings"
	"testing"
)

// TestReviewLoopPreamble covers stripLegacyVerdictLine and the legacy-preamble
// handling inside ParseReviewFindings. Each row is one scenario.
func TestReviewLoopPreamble(t *testing.T) {
	cases := []struct {
		name string
		in   string
		want string // expected return value from stripLegacyVerdictLine
	}{
		{
			name: "FAIL colon is stripped",
			in:   "FAIL: blockers found\nrest of content",
			want: "rest of content",
		},
		{
			name: "PASS colon is stripped",
			in:   "PASS: all clear\nrest of content",
			want: "rest of content",
		},
		{
			name: "PASS alone on line is stripped",
			in:   "PASS\nsome content",
			want: "some content",
		},
		{
			name: "FAIL alone on line is stripped",
			in:   "FAIL\nsome content",
			want: "some content",
		},
		{
			name: "lowercase fail is stripped",
			in:   "fail: something wrong\ncontent",
			want: "content",
		},
		{
			name: "lowercase pass is stripped",
			in:   "pass\ncontent",
			want: "content",
		},
		{
			name: "mixed case FAIL is stripped",
			in:   "Fail: mixed\ncontent",
			want: "content",
		},
		{
			name: "single-line PASS returns empty",
			in:   "PASS",
			want: "",
		},
		{
			name: "single-line FAIL returns empty",
			in:   "FAIL",
			want: "",
		},
		{
			name: "single-line FAIL with colon returns empty",
			in:   "FAIL: no content after",
			want: "",
		},
		{
			name: "empty string unchanged",
			in:   "",
			want: "",
		},
		{
			name: "regular content unchanged",
			in:   "## Summary\n\nLooks good.",
			want: "## Summary\n\nLooks good.",
		},
		{
			name: "YAML frontmatter unchanged",
			in:   "---\nproduced_by: reviewer\n---\n\n## Blockers",
			want: "---\nproduced_by: reviewer\n---\n\n## Blockers",
		},
		{
			name: "word starting with pass is not stripped",
			in:   "passing all checks\ncontent",
			want: "passing all checks\ncontent",
		},
		{
			name: "word starting with fail is not stripped",
			in:   "failure is not an option\ncontent",
			want: "failure is not an option\ncontent",
		},
		{
			name: "FAIL only strips the first line not subsequent ones",
			in:   "FAIL: line one\nFAIL: line two\nline three",
			want: "FAIL: line two\nline three",
		},
		{
			name: "non-verdict first line with FAIL elsewhere unchanged",
			in:   "## Summary\nFAIL: buried\nend",
			want: "## Summary\nFAIL: buried\nend",
		},
		{
			name: "FAIL with leading whitespace stripped after TrimSpace",
			in:   "  FAIL: indented\ncontent",
			want: "content",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got := stripLegacyVerdictLine(tc.in)
			if got != tc.want {
				t.Errorf("stripLegacyVerdictLine(%q)\n got:  %q\n want: %q", tc.in, got, tc.want)
			}
		})
	}
}

// TestParseReviewFindings_LegacyPreamble verifies that ParseReviewFindings
// handles legacy artifacts that open with a bare FAIL/PASS verdict line before
// the YAML frontmatter rule was introduced.
func TestParseReviewFindings_LegacyPreamble(t *testing.T) {
	cases := []struct {
		name          string
		input         string
		wantBlockers  int
		wantWarnings  int
		wantPassed    bool
	}{
		{
			name: "FAIL preamble with blockers parses correctly",
			input: "FAIL: needs changes\n\n### Blockers\n- **[store.go:10]** Missing nil check.\n\n### Warnings\n",
			wantBlockers: 1,
			wantWarnings: 0,
			wantPassed:   false,
		},
		{
			name: "PASS preamble with no blockers parses correctly",
			input: "PASS\n\n### Blockers\n\n### Warnings\n- **[util.go:5]** Unused var.\n",
			wantBlockers: 0,
			wantWarnings: 1,
			wantPassed:   true,
		},
		{
			name: "legacy FAIL single line only returns passed (fail-open)",
			input: "FAIL",
			wantBlockers: 0,
			wantWarnings: 0,
			wantPassed:   true,
		},
		{
			name: "no preamble standard output unchanged",
			input: "### Blockers\n- **[main.go:1]** Issue.\n\n### Warnings\n",
			wantBlockers: 1,
			wantWarnings: 0,
			wantPassed:   false,
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			f := ParseReviewFindings(tc.input)
			if len(f.Blockers) != tc.wantBlockers {
				t.Errorf("blockers: got %d, want %d (input: %q)", len(f.Blockers), tc.wantBlockers, tc.input)
			}
			if len(f.Warnings) != tc.wantWarnings {
				t.Errorf("warnings: got %d, want %d", len(f.Warnings), tc.wantWarnings)
			}
			if f.Passed() != tc.wantPassed {
				t.Errorf("Passed(): got %v, want %v", f.Passed(), tc.wantPassed)
			}
		})
	}
}

// TestStripLegacyVerdictLine_PatternPrecision verifies that the verdict pattern
// does not strip lines that merely start with substrings of "fail" or "pass".
func TestStripLegacyVerdictLine_PatternPrecision(t *testing.T) {
	// These lines must NOT be stripped — they are ordinary first lines.
	notStripped := []string{
		"failing tests are a signal",
		"failed to connect",
		"password reset",
		"passage of time",
		"fail-safe enabled",
		"pass-through configuration",
	}

	for _, line := range notStripped {
		t.Run(line, func(t *testing.T) {
			input := line + "\ncontent after"
			got := stripLegacyVerdictLine(input)
			if !strings.HasPrefix(got, line) {
				t.Errorf("stripLegacyVerdictLine incorrectly stripped %q", line)
			}
		})
	}
}
