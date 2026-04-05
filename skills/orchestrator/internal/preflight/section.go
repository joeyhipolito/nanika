// Package preflight assembles a short, structured system-state brief for
// worker sessions (scheduler jobs, open P0s, recent learnings, etc.).
//
// A preflight brief is injected into a worker's prompt before it starts so
// the worker does not have to re-discover state that the orchestrator
// already knows. Each slice of that state is a Section: it knows how to
// fetch itself on demand, carries a Priority so the brief renders in a
// stable order, and identifies itself with a short Name used by the
// `--sections` CLI filter.
//
// The preflight package owns the interface, the registry, and the
// rendering glue. Concrete sections (scheduler, tracker, nen, learnings…)
// live in their own files and register themselves via Register in init().
// The registry starts empty; a CLI invocation with no registered sections
// returns an empty brief and exits successfully.
package preflight

import "context"

// Section is a single unit of preflight context. Implementations wrap an
// underlying data source (scheduler CLI, tracker query, learning DB, …)
// and return a rendered Block on demand.
//
// Implementations MUST be safe to call concurrently — the registry may
// fetch multiple sections in parallel in future phases.
type Section interface {
	// Name is the stable identifier used by --sections filters.
	// Should be lowercase, short, and unique across the registry
	// (e.g. "scheduler", "tracker", "learnings").
	Name() string

	// Priority orders sections in the rendered brief (lower = earlier).
	// Ties are broken by registration order.
	Priority() int

	// Fetch returns the section's rendered block, or an error if the
	// underlying source is unavailable. Implementations MUST respect
	// ctx cancellation and return promptly; the hook runs with a bounded
	// time budget shared across all sections.
	//
	// A Block with an empty Body is valid and means "nothing to report";
	// the renderer omits empty blocks from the final brief.
	Fetch(ctx context.Context) (Block, error)
}

// Block is the rendered output of one Section.
//
// Block is intentionally flat: a human-readable Title for the text
// renderer, a raw Body string that the section fully owns, and a Name
// field that mirrors Section.Name() so JSON consumers can filter without
// a second round-trip. The registry sets Name automatically during
// BuildBrief — implementations only need to populate Title and Body.
type Block struct {
	Name  string `json:"name"`
	Title string `json:"title"`
	Body  string `json:"body"`
}
