package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// EditCmd updates an existing transaction.
func EditCmd(client *api.Client, transactionID string, amount *int64, payee, category, memo, date string, cleared bool, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Fetch the existing transaction
	existing, err := client.GetTransaction(budgetID, transactionID)
	if err != nil {
		return fmt.Errorf("failed to get transaction: %w", err)
	}

	// Build update map with only changed fields
	updates := map[string]interface{}{
		"account_id": existing.AccountID,
		"date":       existing.Date,
		"amount":     existing.Amount,
		"approved":   true,
	}

	if amount != nil {
		updates["amount"] = *amount
	}
	if payee != "" {
		updates["payee_name"] = payee
	}
	if date != "" {
		updates["date"] = date
	}
	if memo != "" {
		updates["memo"] = memo
	}
	if cleared {
		updates["cleared"] = "cleared"
	}

	// Resolve category if provided
	if category != "" {
		groups, err := client.GetCategories(budgetID)
		if err != nil {
			return fmt.Errorf("failed to get categories: %w", err)
		}
		catID := findCategoryID(groups, category)
		if catID == "" {
			return fmt.Errorf("no category found matching '%s'", category)
		}
		updates["category_id"] = catID
	}

	// Perform update
	updated, err := client.UpdateTransaction(budgetID, transactionID, updates)
	if err != nil {
		return fmt.Errorf("failed to update transaction: %w", err)
	}

	if jsonOutput {
		output := TransactionItem{
			ID:            updated.ID,
			Date:          updated.Date,
			Amount:        updated.Amount,
			AmountDisplay: transform.FormatCurrency(updated.Amount),
			PayeeName:     updated.PayeeName,
			CategoryName:  updated.CategoryName,
			AccountName:   updated.AccountName,
			Memo:          updated.Memo,
			Cleared:       updated.Cleared,
			Approved:      updated.Approved,
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Println("Transaction updated!")
	fmt.Println()
	fmt.Printf("Date:     %s\n", updated.Date)
	fmt.Printf("Amount:   %s\n", transform.FormatCurrency(updated.Amount))
	fmt.Printf("Payee:    %s\n", updated.PayeeName)
	fmt.Printf("Category: %s\n", updated.CategoryName)
	fmt.Printf("Account:  %s\n", updated.AccountName)
	if updated.Memo != "" {
		fmt.Printf("Memo:     %s\n", updated.Memo)
	}
	fmt.Printf("Cleared:  %s\n", updated.Cleared)

	return nil
}
