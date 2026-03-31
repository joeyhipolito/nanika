package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// AccountOutput represents the JSON output for account creation.
type AccountOutput struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	Type    string `json:"type"`
	Balance int64  `json:"balance"`
	BalanceDisplay string `json:"balance_display"`
}

// AddAccountCmd creates a new account in the budget.
func AddAccountCmd(client *api.Client, name, accountType string, balanceMilliunits int64, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Validate account type
	validTypes := map[string]bool{
		"checking": true, "savings": true, "creditCard": true,
		"cash": true, "lineOfCredit": true, "otherAsset": true, "otherLiability": true,
	}
	if !validTypes[accountType] {
		return fmt.Errorf("invalid account type '%s'\n\nValid types: checking, savings, creditCard, cash, lineOfCredit, otherAsset, otherLiability", accountType)
	}

	account, err := client.CreateAccount(budgetID, name, accountType, balanceMilliunits)
	if err != nil {
		return fmt.Errorf("failed to create account: %w", err)
	}

	if jsonOutput {
		output := AccountOutput{
			ID:             account.ID,
			Name:           account.Name,
			Type:           account.Type,
			Balance:        account.Balance,
			BalanceDisplay: transform.FormatCurrency(account.Balance),
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Println("Account created!")
	fmt.Println()
	fmt.Printf("Name:    %s\n", account.Name)
	fmt.Printf("Type:    %s\n", formatAccountType(account.Type))
	fmt.Printf("Balance: %s\n", transform.FormatCurrency(account.Balance))
	fmt.Printf("ID:      %s\n", account.ID)

	return nil
}
