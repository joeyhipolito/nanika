package cmd

import (
	"fmt"
	"strconv"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// DeleteCmd handles the delete command.
func DeleteCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	if len(args) == 0 {
		return fmt.Errorf("usage: substack delete <draft-id> [<draft-id>...]")
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	for _, arg := range args {
		id, err := strconv.Atoi(arg)
		if err != nil {
			fmt.Printf("Skipping invalid ID: %s\n", arg)
			continue
		}

		fmt.Printf("Deleting draft %d...\n", id)
		if err := client.DeleteDraft(id); err != nil {
			fmt.Printf("  Failed: %v\n", err)
		} else {
			fmt.Printf("  Deleted.\n")
		}
	}

	return nil
}
