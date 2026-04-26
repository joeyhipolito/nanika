// Package scratch provides helpers for extracting <!-- scratch --> blocks
// from phase output. It is a leaf package with no dependencies on engine or
// worker so both packages can import it without creating an import cycle.
package scratch

import (
	"regexp"
	"strings"
)

// blockRE matches <!-- scratch --> ... <!-- /scratch --> blocks in phase
// output. The content between the markers is captured as group 1.
// DOTALL: the (?s) flag makes . match newlines.
// Tolerant: \s* around the tag names accepts variants like <!-- scratch -->.
var blockRE = regexp.MustCompile(`(?s)<!--\s*scratch\s*-->\s*(.*?)\s*<!--\s*/scratch\s*-->`)

// ExtractBlock returns the concatenated content of all
// <!-- scratch --> ... <!-- /scratch --> blocks found in output.
// Returns "" when no blocks are present.
func ExtractBlock(output string) string {
	matches := blockRE.FindAllStringSubmatch(output, -1)
	if len(matches) == 0 {
		return ""
	}
	var parts []string
	for _, m := range matches {
		content := strings.TrimSpace(m[1])
		if content != "" {
			parts = append(parts, content)
		}
	}
	return strings.Join(parts, "\n\n")
}
