package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// AccountsCmd handles "gmail accounts [--json]".
// Lists all configured accounts with alias, email, and added_at.
func AccountsCmd(jsonOutput bool) error {
	accounts, err := config.LoadAccounts()
	if err != nil {
		return fmt.Errorf("failed to load accounts: %w", err)
	}

	if len(accounts) == 0 {
		if jsonOutput {
			fmt.Println("[]")
			return nil
		}
		fmt.Println("No accounts configured.")
		fmt.Println("Run 'gmail configure <alias>' to add one.")
		return nil
	}

	if jsonOutput {
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(accounts)
	}

	fmt.Printf("Configured accounts (%d):\n\n", len(accounts))
	for _, a := range accounts {
		fmt.Printf("  %-15s %s\n", a.Alias, a.Email)
		fmt.Printf("  %15s added %s\n", "", a.AddedAt)
		fmt.Println()
	}

	return nil
}

// AccountsRemoveCmd handles "gmail accounts remove <alias>".
// Removes the account and its associated token file.
func AccountsRemoveCmd(alias string) error {
	if alias == "" {
		return fmt.Errorf("usage: gmail accounts remove <alias>")
	}

	// Verify account exists before removing
	acct, err := config.GetAccount(alias)
	if err != nil {
		return err
	}

	if err := config.RemoveAccount(alias); err != nil {
		return fmt.Errorf("failed to remove account: %w", err)
	}

	fmt.Printf("Removed account %q (%s)\n", alias, acct.Email)
	return nil
}
