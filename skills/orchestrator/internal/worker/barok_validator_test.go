package worker

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

// TestValidateStructuralYAML covers HR-filtering of yamlBlockRE matches: the
// opening or closing --- of a matched block that is a CommonMark horizontal
// rule (blank-bounded) must cause the block to be discarded so it is not
// counted against bareMarkers.
func TestValidateStructuralYAML(t *testing.T) {
	hr := "\n---\n" // blank-bounded HR helper: surround with \n to form a full HR context

	cases := []struct {
		name    string
		content string
		wantErr bool
	}{
		{
			// (a) Frontmatter + 2 HRs → 1 block, 2 bare markers, passes.
			name: "(a) frontmatter plus 2 HRs passes",
			content: strings.Join([]string{
				"---",
				"title: Foo",
				"---",
				"",
				"Prose A.",
				hr,
				"Prose B.",
				hr,
				"End.",
			}, "\n"),
		},
		{
			// (b) Frontmatter + 5 HRs, reproducing obsidian-vaultkind-review.md
			// pattern (7 total --- lines: 2 frontmatter + 5 HRs). Before the fix,
			// yamlBlockRE matched fake blocks between adjacent HRs, causing
			// bareMarkers(2) ≠ 2*blocks(3+).
			name: "(b) frontmatter plus 5 HRs (obsidian-vaultkind-review pattern) passes",
			content: "---\nproduced_by: staff-code-reviewer\nphase: phase-8\n---\n\n## Section 1\n\nSome content.\n\n---\n\n## Section 2\n\nMore content.\n\n---\n\n## Section 3\n\nEven more.\n\n---\n\n## Section 4\n\nAlmost done.\n\n---\n\n## Section 5\n\nFinal section.\n\n---\n\nDone.",
		},
		{
			// (c) Zero frontmatter + 3 HRs → 0 blocks, 0 bare markers, passes.
			name: "(c) no frontmatter, 3 HRs only, passes",
			content: "Prose A.\n\n---\n\nProse B.\n\n---\n\nProse C.\n\n---\n\nEnd.",
		},
		{
			// (d) Truly unbalanced frontmatter (odd bare count, none HR) → fails.
			// The orphan --- is NOT blank-bounded so isHRLine returns false; it
			// increments bareMarkers to 3, which cannot equal 2*blocks(1).
			name:    "(d) unbalanced frontmatter with non-HR orphan fails",
			content: "---\ntitle: ok\n---\n\nSome content.\n\n---\norphan",
			wantErr: true,
		},
		{
			// (e) Fenced code block containing --- inside is still excluded
			// (TRK-605-era case). Combined with frontmatter to confirm both
			// exclusion paths cooperate.
			name:    "(e) fenced code block with --- is still excluded",
			content: "---\ntitle: Real\n---\n\nExample:\n\n```yaml\n---\nexample: true\n---\n```",
		},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := validateStructuralYAML(tc.content)
			if tc.wantErr && err == nil {
				t.Error("expected error; got nil")
			}
			if !tc.wantErr && err != nil {
				t.Errorf("expected no error; got %v", err)
			}
		})
	}
}

// TestValidateStructuralYAML_FixtureFile feeds the real obsidian-vaultkind-review.md
// artifact (1 frontmatter block + 5 CommonMark HRs = 7 total --- lines) through
// the validator and asserts nil error. Skipped when the file is absent so the
// test is portable across machines (does not assume a specific home directory).
func TestValidateStructuralYAML_FixtureFile(t *testing.T) {
	// Resolve path relative to $HOME so the test works on any machine that has
	// a local clone of the repository at ~/nanika.
	home, err := os.UserHomeDir()
	if err != nil {
		t.Skipf("cannot resolve home directory: %v", err)
	}
	fixturePath := filepath.Join(home, "nanika", "shared", "artifacts", "obsidian-vaultkind-review.md")
	data, err := os.ReadFile(fixturePath)
	if os.IsNotExist(err) {
		t.Skipf("fixture not present: %s", fixturePath)
	}
	if err != nil {
		t.Fatalf("reading fixture: %v", err)
	}
	if err := validateStructuralYAML(string(data)); err != nil {
		t.Errorf("obsidian-vaultkind-review.md: unexpected error: %v", err)
	}
}

// TestBarok_ValidateStructuralYAML_FencedCodeBlocks is the regression suite for
// TRK-547: validateStructuralYAML was producing false-positive "unbalanced ---"
// errors when an artifact contained --- markers inside fenced code blocks.
func TestBarok_ValidateStructuralYAML_FencedCodeBlocks(t *testing.T) {
	cases := []struct {
		name    string
		content string
		wantErr bool
	}{
		{
			// (a) A bare --- inside a fenced code block must not be counted as a
			// YAML fence marker. Before the fix this inflated bareMarkers by 1.
			name:    "(a) bare --- inside fenced block is not counted as YAML marker",
			content: "Some prose.\n\n```yaml\n---\nkey: value\n```\n\nMore prose.",
		},
		{
			// (b) A complete ---...--- pair inside a fenced code block must not
			// be matched by yamlBlockRE. Before the fix the pair was counted as
			// one block but bareMarkers was also incremented twice, keeping the
			// equality — but the real regression manifests when the fenced pair
			// causes an off-by-one on a document that also has real frontmatter.
			name:    "(b) complete ---...--- pair inside fenced block is not a YAML block",
			content: "```yaml\n---\nkey: value\n---\n```",
		},
		{
			// (c) Real YAML frontmatter combined with a fenced code block that
			// itself contains a bare ---. The real frontmatter contributes
			// bareMarkers=2 / blocks=1; the fenced --- must be excluded from
			// both counters so the invariant 2*blocks==bareMarkers holds.
			name:    "(c) real YAML frontmatter plus fenced --- inside does not false-positive",
			content: "---\ntitle: Real\n---\n\nExample:\n\n```yaml\n---\nexample: true\n---\n```",
		},
		{
			// (d) An orphaned --- OUTSIDE a fenced block must still be detected.
			// The fenced pair inside must be excluded, leaving the orphan to
			// violate the 2*blocks==bareMarkers invariant.
			name:    "(d) unbalanced --- outside fenced block still fails",
			content: "---\ntitle: ok\n---\n\n```yaml\n---\nfaked\n---\n```\n\n---\norphan\n",
			wantErr: true,
		},
		{
			// (e) Multiple fenced code blocks each containing a ---...--- pair.
			// All markers are inside fences; bareMarkers and blocks both land
			// at 0 after filtering.
			name:    "(e) multiple fenced blocks with ---...--- pairs each",
			content: "```yaml\n---\na: 1\n---\n```\n\n```yaml\n---\nb: 2\n---\n```",
		},
		{
			// (f) Real YAML frontmatter + fenced code block with --- inside +
			// a CommonMark HR (blank-bounded ---) in the prose body. All three
			// must be handled correctly: real frontmatter counted once, fenced
			// markers excluded, HR skipped by the blank-neighbour rule. The HR
			// is placed AFTER the fenced block so it cannot pair with the fenced
			// --- via the regex (cross-boundary false match).
			name:    "(f) real YAML frontmatter plus fenced ---...--- plus HR does not false-positive",
			content: "---\ntitle: Real\n---\n\n```yaml\n---\nexample: true\n---\n```\n\nProse.\n\n---\n\nMore.",
		},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			err := ValidateArtifactStructure([]byte(tc.content), "architect")
			if tc.wantErr && err == nil {
				t.Error("expected error; got nil")
			}
			if !tc.wantErr && err != nil {
				t.Errorf("expected no error; got %v", err)
			}
		})
	}
}
