package gather

import (
	"fmt"
	"net/http"
	"os"
	"time"
)

// userAgent is sent with every outbound HTTP request.
const userAgent = "scout-cli/0.4 (github.com/joeyhipolito/scout-cli)"

// newHTTPClient returns an *http.Client with a standard 30-second timeout.
// All gatherers use this instead of constructing their own.
func newHTTPClient() *http.Client {
	return &http.Client{Timeout: 30 * time.Second}
}

// appendUnique appends items to dst, skipping any whose ID is already in seen.
func appendUnique(seen map[string]bool, dst []IntelItem, items []IntelItem) []IntelItem {
	for _, item := range items {
		if !seen[item.ID] {
			seen[item.ID] = true
			dst = append(dst, item)
		}
	}
	return dst
}

// collectFromList calls fetch for each entry in list, deduplicates results by
// item ID using appendUnique, then filters by searchTerms.
// Fetch errors are logged as warnings and do not abort the loop.
func collectFromList(list []string, label string, fetch func(string) ([]IntelItem, error), searchTerms []string) ([]IntelItem, error) {
	seen := make(map[string]bool)
	var all []IntelItem
	for _, item := range list {
		got, err := fetch(item)
		if err != nil {
			fmt.Fprintf(os.Stderr, "    Warning: %s/%s: %v\n", label, item, err)
			continue
		}
		all = appendUnique(seen, all, got)
	}
	if len(searchTerms) > 0 {
		all = filterByTerms(all, searchTerms)
	}
	return all, nil
}

// collectByTerms calls fetch for each search term, deduplicates by item ID,
// and returns an error only when every fetch fails.
// Partial failures are logged as warnings and collection continues.
func collectByTerms(searchTerms []string, label string, fetch func(string) ([]IntelItem, error)) ([]IntelItem, error) {
	seen := make(map[string]bool)
	var all []IntelItem
	var lastErr error
	for _, term := range searchTerms {
		got, err := fetch(term)
		if err != nil {
			lastErr = err
			fmt.Fprintf(os.Stderr, "    Warning: %s/%s: %v\n", label, term, err)
			continue
		}
		all = appendUnique(seen, all, got)
	}
	if len(all) == 0 && lastErr != nil {
		return nil, fmt.Errorf("%s: all queries failed, last: %w", label, lastErr)
	}
	return all, nil
}
