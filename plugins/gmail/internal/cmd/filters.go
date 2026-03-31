package cmd

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// accountFilters groups filters with their account alias for output.
type accountFilters struct {
	Account string       `json:"account"`
	Filters []api.Filter `json:"filters"`
}

// FiltersCmd handles "gmail filters [--account alias] [--json]"
func FiltersCmd(cfg *config.Config, account string, jsonOutput bool) error {
	var results []accountFilters

	if account != "" {
		client, err := api.NewClient(account, cfg)
		if err != nil {
			return fmt.Errorf("connect to account %q: %w", account, err)
		}
		filters, err := client.ListFilters()
		if err != nil {
			return fmt.Errorf("list filters for %q: %w", account, err)
		}
		results = append(results, accountFilters{Account: account, Filters: filters})
	} else {
		clients, err := api.NewClientAll(cfg)
		if err != nil {
			return err
		}
		for _, c := range clients {
			filters, err := c.ListFilters()
			if err != nil {
				fmt.Fprintf(os.Stderr, "warning: failed to list filters for %q: %v\n", c.Alias(), err)
				continue
			}
			results = append(results, accountFilters{Account: c.Alias(), Filters: filters})
		}
	}

	if jsonOutput {
		return printJSON(results)
	}

	for i, r := range results {
		if i > 0 {
			fmt.Println()
		}
		fmt.Printf("[%s] Filters (%d):\n", r.Account, len(r.Filters))
		if len(r.Filters) == 0 {
			fmt.Println("  (none)")
			continue
		}
		for _, f := range r.Filters {
			fmt.Printf("  %s  %s\n", f.ID, api.FormatFilterSummary(f))
		}
	}

	return nil
}

// FilterCreateCmd handles "gmail filter --create ... --account <alias>"
func FilterCreateCmd(cfg *config.Config, criteria api.FilterCriteria, action api.FilterAction, account string, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	filter, err := client.CreateFilter(criteria, action)
	if err != nil {
		return fmt.Errorf("create filter: %w", err)
	}

	if jsonOutput {
		return printJSON(filter)
	}

	fmt.Printf("Created filter %s on account %q\n", filter.ID, account)
	fmt.Printf("  %s\n", api.FormatFilterSummary(*filter))
	return nil
}

// FilterDeleteCmd handles "gmail filter --delete <id> --account <alias>"
func FilterDeleteCmd(cfg *config.Config, filterID, account string) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	if err := client.DeleteFilter(filterID); err != nil {
		return fmt.Errorf("delete filter %s: %w", filterID, err)
	}

	fmt.Printf("Deleted filter %s from account %q\n", filterID, account)
	return nil
}
