package cmd

import (
	"fmt"
	"sync"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// InboxCmd handles "gmail inbox [--limit N] [--unread] [--account alias] [--json]".
// If --account is set, show only that account's inbox.
// Otherwise, fetch from ALL accounts concurrently and merge.
// Sort: unread first, then by date desc.
func InboxCmd(cfg *config.Config, account string, limit int, unreadOnly bool, jsonOutput bool) error {
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

		summaries, err := client.ListInbox(limit, unreadOnly)
		if err != nil {
			return fmt.Errorf("fetch inbox for %q: %w", account, err)
		}
		allSummaries = summaries
	} else {
		// All accounts mode — fetch concurrently.
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

				summaries, err := client.ListInbox(limit, unreadOnly)
				if err != nil {
					mu.Lock()
					if firstErr == nil {
						firstErr = fmt.Errorf("fetch inbox for %q: %w", client.Alias(), err)
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
