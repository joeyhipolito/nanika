package preflight

import (
	"sort"
	"sync"
)

// registry holds the process-wide list of registered sections. Concrete
// sections register themselves from init() and are looked up by Name or
// listed in priority order by BuildBrief.
var registry struct {
	mu       sync.RWMutex
	sections []Section
}

// Register adds a Section to the global registry. Safe to call from
// init() and from tests. If a section with the same Name is already
// registered, the new one replaces it (so tests can swap in fakes and
// plugins can override defaults).
func Register(s Section) {
	if s == nil {
		return
	}
	registry.mu.Lock()
	defer registry.mu.Unlock()
	for i, existing := range registry.sections {
		if existing.Name() == s.Name() {
			registry.sections[i] = s
			return
		}
	}
	registry.sections = append(registry.sections, s)
}

// Reset clears the registry. Tests only — production code should never
// need to unregister sections.
func Reset() {
	registry.mu.Lock()
	defer registry.mu.Unlock()
	registry.sections = nil
}

// List returns registered sections sorted ascending by Priority. Ties are
// broken by registration order (stable sort). The returned slice is a
// copy; callers may mutate it freely.
func List() []Section {
	registry.mu.RLock()
	defer registry.mu.RUnlock()
	out := make([]Section, len(registry.sections))
	copy(out, registry.sections)
	sort.SliceStable(out, func(i, j int) bool {
		return out[i].Priority() < out[j].Priority()
	})
	return out
}
