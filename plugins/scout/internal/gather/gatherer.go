package gather

import "context"

// Gatherer is the interface implemented by every source-specific gatherer.
// Register new gatherers in registry.go; keep implementations source-specific.
type Gatherer interface {
	// Name returns the canonical source identifier used as the registry key
	// (e.g. "hackernews", "devto", "reddit").
	Name() string

	// Gather fetches intel items that match the given search terms.
	// ctx controls cancellation and deadline; callers should use appropriate timeouts.
	// Returns a nil slice (not an error) when no items were found.
	Gather(ctx context.Context, searchTerms []string) ([]IntelItem, error)
}
