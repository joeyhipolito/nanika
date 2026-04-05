package preflight

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/orchestrator-cli/internal/learning"
)

const (
	learningsSectionName     = "learnings"
	learningsSectionPriority = 30
	learningsLimit           = 10
	learningsDefaultDomain   = "dev"
)

func init() {
	Register(&learningsSection{})
}

// learningsSection is a preflight.Section that surfaces the top 10
// learnings ranked by quality × recency (cold-start path from inject-context).
type learningsSection struct{}

func (s *learningsSection) Name() string  { return learningsSectionName }
func (s *learningsSection) Priority() int { return learningsSectionPriority }

// Fetch opens the learning DB, queries the top learnings by quality+recency,
// and returns a formatted Block. An empty store is not an error — the returned
// Block will have an empty Body and the brief renderer will skip it.
func (s *learningsSection) Fetch(_ context.Context) (Block, error) {
	domain := os.Getenv("NANIKA_DOMAIN")
	if domain == "" {
		domain = learningsDefaultDomain
	}

	db, err := learning.OpenDB("")
	if err != nil {
		return Block{}, fmt.Errorf("open learning DB: %w", err)
	}
	defer db.Close()

	learnings, err := db.FindTopByQuality(domain, learningsLimit)
	if err != nil {
		return Block{}, fmt.Errorf("finding top learnings: %w", err)
	}
	if len(learnings) == 0 {
		// Empty store is valid — return an empty Block so the brief
		// renderer omits this section rather than showing a blank header.
		return Block{Title: "Relevant Learnings"}, nil
	}

	var sb strings.Builder
	for _, l := range learnings {
		fmt.Fprintf(&sb, "- **[%s]** %s\n", l.Type, l.Content)
	}

	return Block{
		Title: "Relevant Learnings",
		Body:  sb.String(),
	}, nil
}
