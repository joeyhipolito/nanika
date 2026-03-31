package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// DeleteCmd deletes a transaction by ID.
func DeleteCmd(client *api.Client, transactionID string, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Fetch before deleting so we can show what was deleted
	existing, err := client.GetTransaction(budgetID, transactionID)
	if err != nil {
		return fmt.Errorf("failed to get transaction: %w", err)
	}

	deleted, err := client.DeleteTransaction(budgetID, transactionID)
	if err != nil {
		return fmt.Errorf("failed to delete transaction: %w", err)
	}

	if jsonOutput {
		output := TransactionItem{
			ID:            deleted.ID,
			Date:          existing.Date,
			Amount:        existing.Amount,
			AmountDisplay: transform.FormatCurrency(existing.Amount),
			PayeeName:     existing.PayeeName,
			CategoryName:  existing.CategoryName,
			AccountName:   existing.AccountName,
			Memo:          existing.Memo,
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Println("Transaction deleted!")
	fmt.Println()
	fmt.Printf("Date:     %s\n", existing.Date)
	fmt.Printf("Amount:   %s\n", transform.FormatCurrency(existing.Amount))
	fmt.Printf("Payee:    %s\n", existing.PayeeName)
	fmt.Printf("Category: %s\n", existing.CategoryName)
	fmt.Printf("Account:  %s\n", existing.AccountName)

	return nil
}
