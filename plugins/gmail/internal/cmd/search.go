package cmd

import (
	"fmt"
	"sync"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// SearchCmd handles "gmail search <query> [--limit N] [--account alias] [--json]".
// If --account is set, search only that account.
// Otherwise, search ALL accounts and merge results.
func SearchCmd(cfg *config.Config, query, account string, limit int, jsonOutput bool) error {
	if query == "" {
		return fmt.Errorf("search query is required")
	}
	if limit <= 0 {
		limit = 25
	}

	var allSummaries []api.ThreadSummary

	if account != "" {
		// Single account mode.
		client, err := api.NewClient(account, cfg)
		if err != nil {
			return fmt.Errorf("connect to account %q: %w", account, err)
		}

		summaries, err := client.Search(query, limit)
		if err != nil {
			return fmt.Errorf("search %q for %q: %w", account, query, err)
		}
		allSummaries = summaries
	} else {
		// All accounts mode — search concurrently.
		clients, err := api.NewClientAll(cfg)
		if err != nil {
			return err
		}

		var mu sync.Mutex
		var wg sync.WaitGroup
		var firstErr error

		for _, c := range clients {
			wg.Add(1)
			go func(client *api.Client) {
				defer wg.Done()

				summaries, err := client.Search(query, limit)
				if err != nil {
					mu.Lock()
					if firstErr == nil {
						firstErr = fmt.Errorf("search %q for %q: %w", client.Alias(), query, err)
					}
					mu.Unlock()
					return
				}

				mu.Lock()
				allSummaries = append(allSummaries, summaries...)
				mu.Unlock()
			}(c)
		}

		wg.Wait()

		if firstErr != nil && len(allSummaries) == 0 {
			return firstErr
		}
	}

	// Sort: unread first, then by date descending.
	sortThreadSummaries(allSummaries)

	if jsonOutput {
		return printThreadSummariesJSON(allSummaries)
	}

	printThreadSummaries(allSummaries)
	return nil
}
