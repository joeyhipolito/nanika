// Package cmd implements the CLI command handlers for ynab-cli.
package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// StatusOutput represents the JSON output format for the status command.
type StatusOutput struct {
	BudgetID         string `json:"budget_id"`
	BudgetName       string `json:"budget_name"`
	LastModified     string `json:"last_modified"`
	FirstMonth       string `json:"first_month,omitempty"`
	LastMonth        string `json:"last_month,omitempty"`
	CurrencyCode     string `json:"currency_code,omitempty"`
	CurrencySymbol   string `json:"currency_symbol,omitempty"`
	AccountCount     int    `json:"account_count,omitempty"`
}

// StatusCmd retrieves and displays information about the default YNAB budget.
// If jsonOutput is true, outputs JSON instead of human-readable format.
func StatusCmd(client *api.Client, jsonOutput bool) error {
	// Get default budget ID
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Get all budgets and find the default one
	budgets, err := client.GetBudgets()
	if err != nil {
		return fmt.Errorf("failed to get budgets: %w", err)
	}

	var budget *api.Budget
	for _, b := range budgets {
		if b.ID == budgetID {
			budget = b
			break
		}
	}
	if budget == nil {
		return fmt.Errorf("budget %s not found", budgetID)
	}

	// If JSON output requested, marshal and print
	if jsonOutput {
		output := StatusOutput{
			BudgetID:     budget.ID,
			BudgetName:   budget.Name,
			LastModified: budget.LastModifiedOn,
			FirstMonth:   budget.FirstMonth,
			LastMonth:    budget.LastMonth,
		}

		// Add currency info if available
		if budget.CurrencyFormat != nil {
			output.CurrencyCode = budget.CurrencyFormat.ISOCode
			output.CurrencySymbol = budget.CurrencyFormat.CurrencySymbol
		}

		// Add account count if available
		if budget.Accounts != nil {
			output.AccountCount = len(budget.Accounts)
		}

		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		if err := encoder.Encode(output); err != nil {
			return fmt.Errorf("failed to encode JSON: %w", err)
		}
		return nil
	}

	// Human-readable output
	fmt.Printf("Budget: %s\n", budget.Name)
	fmt.Printf("ID: %s\n", budget.ID)
	fmt.Printf("Last Modified: %s\n", formatLastModified(budget.LastModifiedOn))

	if budget.FirstMonth != "" {
		fmt.Printf("First Month: %s\n", formatMonth(budget.FirstMonth))
	}

	if budget.LastMonth != "" {
		fmt.Printf("Last Month: %s\n", formatMonth(budget.LastMonth))
	}

	if budget.CurrencyFormat != nil {
		fmt.Printf("Currency: %s (%s)\n",
			budget.CurrencyFormat.ISOCode,
			budget.CurrencyFormat.CurrencySymbol)
	}

	if budget.Accounts != nil {
		// Count on-budget accounts
		onBudgetCount := 0
		for _, account := range budget.Accounts {
			if account.OnBudget && !account.Closed && !account.Deleted {
				onBudgetCount++
			}
		}
		fmt.Printf("Accounts: %d total, %d on-budget\n", len(budget.Accounts), onBudgetCount)
	}

	return nil
}

// formatLastModified formats a last modified timestamp for display.
// YNAB returns ISO 8601 timestamps like "2024-01-15T10:30:00.000Z"
func formatLastModified(timestamp string) string {
	// Parse the timestamp
	t := transform.ParseDate(timestamp[:10]) // Extract YYYY-MM-DD part
	if t.IsZero() {
		return timestamp // Return original if parsing fails
	}
	return transform.FormatDate(t)
}

// formatMonth formats a month string (YYYY-MM-DD) to a more readable format.
func formatMonth(monthStr string) string {
	year, month, err := transform.ParseMonth(monthStr)
	if err != nil {
		return monthStr // Return original if parsing fails
	}
	return transform.FormatMonth(year, month)
}
