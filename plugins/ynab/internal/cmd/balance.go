package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// BalanceOutput represents the JSON output format for the balance command.
type BalanceOutput struct {
	Accounts []AccountBalance `json:"accounts"`
}

// AccountBalance represents a single account's balance information.
type AccountBalance struct {
	ID               string `json:"id"`
	Name             string `json:"name"`
	Type             string `json:"type"`
	Balance          int64  `json:"balance"`
	ClearedBalance   int64  `json:"cleared_balance"`
	UnclearedBalance int64  `json:"uncleared_balance"`
	OnBudget         bool   `json:"on_budget"`
	Closed           bool   `json:"closed"`
}

// BalanceCmd retrieves and displays account balances.
// If filter is provided, only accounts matching the filter (case-insensitive) are shown.
// If jsonOutput is true, outputs JSON instead of human-readable format.
func BalanceCmd(client *api.Client, filter string, jsonOutput bool) error {
	// Get default budget ID
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Get all accounts
	accounts, err := client.GetAccounts(budgetID)
	if err != nil {
		return fmt.Errorf("failed to get accounts: %w", err)
	}

	// Filter accounts
	var filtered []*api.Account
	filterLower := strings.ToLower(filter)
	for _, account := range accounts {
		// Skip deleted accounts
		if account.Deleted {
			continue
		}

		// Apply name filter if provided
		if filter != "" {
			if !strings.Contains(strings.ToLower(account.Name), filterLower) {
				continue
			}
		}

		filtered = append(filtered, account)
	}

	if len(filtered) == 0 {
		if filter != "" {
			return fmt.Errorf("no accounts found matching '%s'", filter)
		}
		return fmt.Errorf("no accounts found")
	}

	// If JSON output requested, marshal and print
	if jsonOutput {
		output := BalanceOutput{
			Accounts: make([]AccountBalance, 0, len(filtered)),
		}

		for _, account := range filtered {
			output.Accounts = append(output.Accounts, AccountBalance{
				ID:               account.ID,
				Name:             account.Name,
				Type:             account.Type,
				Balance:          account.Balance,
				ClearedBalance:   account.ClearedBalance,
				UnclearedBalance: account.UnclearedBalance,
				OnBudget:         account.OnBudget,
				Closed:           account.Closed,
			})
		}

		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		if err := encoder.Encode(output); err != nil {
			return fmt.Errorf("failed to encode JSON: %w", err)
		}
		return nil
	}

	// Human-readable output
	fmt.Printf("Account Balances:\n\n")

	// Calculate column widths
	maxNameLen := 15 // Minimum width
	for _, account := range filtered {
		if len(account.Name) > maxNameLen {
			maxNameLen = len(account.Name)
		}
	}

	// Print header
	nameHeader := "Account"
	typeHeader := "Type"
	balanceHeader := "Balance"
	clearedHeader := "Cleared"
	unclearedHeader := "Uncleared"

	fmt.Printf("%-*s  %-12s  %15s  %15s  %15s\n",
		maxNameLen, nameHeader, typeHeader, balanceHeader, clearedHeader, unclearedHeader)
	fmt.Printf("%s\n", strings.Repeat("-", maxNameLen+12+15+15+15+8))

	// Print accounts
	var totalBalance int64
	var totalCleared int64
	var totalUncleared int64
	onBudgetCount := 0

	for _, account := range filtered {
		// Format type nicely
		displayType := formatAccountType(account.Type)

		// Format account name with status indicators
		displayName := account.Name
		if account.Closed {
			displayName += " [CLOSED]"
		}
		if !account.OnBudget {
			displayName += " (off-budget)"
		}

		fmt.Printf("%-*s  %-12s  %15s  %15s  %15s\n",
			maxNameLen, displayName, displayType,
			transform.FormatCurrency(account.Balance),
			transform.FormatCurrency(account.ClearedBalance),
			transform.FormatCurrency(account.UnclearedBalance))

		// Track totals for on-budget accounts only
		if account.OnBudget && !account.Closed {
			totalBalance += account.Balance
			totalCleared += account.ClearedBalance
			totalUncleared += account.UnclearedBalance
			onBudgetCount++
		}
	}

	// Print totals if we have multiple on-budget accounts
	if onBudgetCount > 1 {
		fmt.Printf("%s\n", strings.Repeat("-", maxNameLen+12+15+15+15+8))
		fmt.Printf("%-*s  %-12s  %15s  %15s  %15s\n",
			maxNameLen, "Total (on-budget)", "",
			transform.FormatCurrency(totalBalance),
			transform.FormatCurrency(totalCleared),
			transform.FormatCurrency(totalUncleared))
	}

	return nil
}

// formatAccountType formats the account type for display.
func formatAccountType(accountType string) string {
	switch accountType {
	case "checking":
		return "Checking"
	case "savings":
		return "Savings"
	case "creditCard":
		return "Credit Card"
	case "cash":
		return "Cash"
	case "lineOfCredit":
		return "Line of Credit"
	case "otherAsset":
		return "Other Asset"
	case "otherLiability":
		return "Other Liability"
	default:
		return accountType
	}
}
