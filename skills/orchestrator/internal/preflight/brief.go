package preflight

import (
	"context"
	"fmt"
	"strings"
)

// Brief is the fetched, rendered collection of section blocks that the
// `orchestrator hooks preflight` command returns. An empty Brief is valid
// and renders to the empty string in text mode.
type Brief struct {
	Blocks []Block `json:"blocks"`
}

// IsEmpty reports whether the brief has any non-whitespace content.
func (b Brief) IsEmpty() bool {
	for _, blk := range b.Blocks {
		if strings.TrimSpace(blk.Body) != "" {
			return false
		}
	}
	return true
}

// Text renders the brief as a markdown string suitable for prompt
// injection. An empty brief renders to the empty string so callers can
// unconditionally print the result without worrying about blank output.
func (b Brief) Text() string {
	if b.IsEmpty() {
		return ""
	}
	var sb strings.Builder
	for _, blk := range b.Blocks {
		if strings.TrimSpace(blk.Body) == "" {
			continue
		}
		fmt.Fprintf(&sb, "## %s\n\n%s\n\n", blk.Title, strings.TrimRight(blk.Body, "\n"))
	}
	return sb.String()
}

// BuildBrief fetches every registered section that matches the filter and
// returns the assembled brief. A nil or empty filter fetches every
// registered section; a non-empty filter restricts to sections whose Name
// appears in the list.
//
// Per-section Fetch errors are swallowed: a broken section must not bring
// down the whole brief. Implementations that need to surface failures
// should log internally and return a Block describing the degraded state.
//
// BuildBrief honors ctx cancellation between section fetches. With an
// empty registry it returns a Brief with zero blocks and no error path.
func BuildBrief(ctx context.Context, filter []string) Brief {
	wanted := make(map[string]struct{}, len(filter))
	for _, name := range filter {
		trimmed := strings.TrimSpace(name)
		if trimmed != "" {
			wanted[trimmed] = struct{}{}
		}
	}

	sections := List()
	brief := Brief{Blocks: make([]Block, 0, len(sections))}
	for _, s := range sections {
		if len(wanted) > 0 {
			if _, ok := wanted[s.Name()]; !ok {
				continue
			}
		}
		if ctx.Err() != nil {
			break
		}
		blk, err := s.Fetch(ctx)
		if err != nil {
			continue
		}
		blk.Name = s.Name()
		brief.Blocks = append(brief.Blocks, blk)
	}
	return brief
}

// ComposeWithCapacity adjusts the Brief to fit within maxBytes by dropping
// lowest-priority (highest-index) sections first. Returns the adjusted Brief,
// a list of dropped section names, and the final rendered markdown.
//
// If the Brief is already under the byte limit, no sections are dropped.
// Empty sections (with blank Body) do not count toward the byte limit.
// If even the highest-priority section exceeds maxBytes alone, it is kept
// (to avoid dropping all content).
func (b Brief) ComposeWithCapacity(maxBytes int) (Brief, []string, string) {
	if maxBytes <= 0 {
		return b, nil, b.RenderMarkdown()
	}

	// Render current brief to measure bytes.
	text := b.RenderMarkdown()
	if len(text) <= maxBytes {
		return b, nil, text
	}

	// Copy blocks. Drop sections from the end (lowest priority) until we fit.
	// Collect dropped indices in reverse order, then reverse once at the end.
	blocks := make([]Block, len(b.Blocks))
	copy(blocks, b.Blocks)
	droppedIndices := make([]int, 0, len(blocks))

	for i := len(blocks) - 1; i > 0; i-- {
		// Remove section at index i.
		blocks = blocks[:i]
		adjusted := Brief{Blocks: blocks}
		text := adjusted.RenderMarkdown()
		if len(text) <= maxBytes {
			// Now within budget. Collect dropped indices.
			for j := i; j < len(b.Blocks); j++ {
				droppedIndices = append(droppedIndices, j)
			}
			// Build dropped names in order (reverse the indices we collected).
			var dropped []string
			for k := len(droppedIndices) - 1; k >= 0; k-- {
				dropped = append(dropped, b.Blocks[droppedIndices[k]].Name)
			}
			return adjusted, dropped, text
		}
	}

	// If we get here, even keeping just the first section exceeds maxBytes.
	// Truncate its body to fit but keep the header.
	if len(blocks) > 0 {
		blk := blocks[0]
		header := fmt.Sprintf("## Operational Pre-flight\n\n### %s\n\n", blk.Title)
		budgetForBody := maxBytes - len(header) - 1 // -1 for safety
		if budgetForBody > 0 && len(blk.Body) > budgetForBody {
			blk.Body = blk.Body[:budgetForBody]
			// Trim to last newline to avoid mid-line cutoff.
			if idx := strings.LastIndex(blk.Body, "\n"); idx > 0 {
				blk.Body = blk.Body[:idx]
			}
			blocks[0] = blk
		}
		// All sections except the first are dropped.
		var dropped []string
		for j := 1; j < len(b.Blocks); j++ {
			dropped = append(dropped, b.Blocks[j].Name)
		}
		// Render the final truncated brief once.
		adjusted := Brief{Blocks: blocks}
		return adjusted, dropped, adjusted.RenderMarkdown()
	}

	return b, nil, ""
}

// RenderMarkdown renders the Brief as markdown with "## Operational
// Pre-flight" as the main heading and "### Section Title" as subsection
// headings. Empty sections are omitted.
func (b Brief) RenderMarkdown() string {
	if b.IsEmpty() {
		return ""
	}
	var sb strings.Builder
	sb.WriteString("## Operational Pre-flight\n\n")
	for _, blk := range b.Blocks {
		if strings.TrimSpace(blk.Body) == "" {
			continue
		}
		fmt.Fprintf(&sb, "### %s\n\n%s\n\n", blk.Title, strings.TrimRight(blk.Body, "\n"))
	}
	return sb.String()
}
