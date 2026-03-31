package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// MoveOutput represents the JSON output for the move command.
type MoveOutput struct {
	Amount       int64  `json:"amount"`
	AmountDisplay string `json:"amount_display"`
	Month        string `json:"month"`
	From         MoveCategoryInfo `json:"from"`
	To           MoveCategoryInfo `json:"to"`
}

// MoveCategoryInfo represents category info in a move operation.
type MoveCategoryInfo struct {
	ID              string `json:"id"`
	Name            string `json:"name"`
	BudgetedBefore  int64  `json:"budgeted_before"`
	BudgetedAfter   int64  `json:"budgeted_after"`
}

// MoveCmd moves money between budget categories.
func MoveCmd(client *api.Client, amountMilliunits int64, fromCategory, toCategory, month string, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Default to current month
	if month == "" {
		now := time.Now()
		month = fmt.Sprintf("%04d-%02d-01", now.Year(), now.Month())
	} else if len(month) == 7 {
		month += "-01"
	}

	// Resolve categories
	groups, err := client.GetCategories(budgetID)
	if err != nil {
		return fmt.Errorf("failed to get categories: %w", err)
	}

	fromID := findCategoryID(groups, fromCategory)
	if fromID == "" {
		return fmt.Errorf("no category found matching '%s'", fromCategory)
	}
	toID := findCategoryID(groups, toCategory)
	if toID == "" {
		return fmt.Errorf("no category found matching '%s'", toCategory)
	}

	fromName := findCategoryName(groups, fromID)
	toName := findCategoryName(groups, toID)

	// Get current budgeted amounts for the month
	monthData, err := client.GetMonth(budgetID, month)
	if err != nil {
		return fmt.Errorf("failed to get month data: %w", err)
	}

	var fromBudgeted, toBudgeted int64
	for _, c := range monthData.Categories {
		if c.ID == fromID {
			fromBudgeted = c.Budgeted
		}
		if c.ID == toID {
			toBudgeted = c.Budgeted
		}
	}

	// Update source (decrease)
	newFromBudgeted := fromBudgeted - amountMilliunits
	_, err = client.UpdateCategoryBudget(fromID, newFromBudgeted, month, budgetID)
	if err != nil {
		return fmt.Errorf("failed to update source category: %w", err)
	}

	// Update destination (increase)
	newToBudgeted := toBudgeted + amountMilliunits
	_, err = client.UpdateCategoryBudget(toID, newToBudgeted, month, budgetID)
	if err != nil {
		// Try to roll back source on failure
		_, _ = client.UpdateCategoryBudget(fromID, fromBudgeted, month, budgetID)
		return fmt.Errorf("failed to update destination category: %w", err)
	}

	if jsonOutput {
		output := MoveOutput{
			Amount:        amountMilliunits,
			AmountDisplay: transform.FormatCurrency(amountMilliunits),
			Month:         month[:7],
			From: MoveCategoryInfo{
				ID:             fromID,
				Name:           fromName,
				BudgetedBefore: fromBudgeted,
				BudgetedAfter:  newFromBudgeted,
			},
			To: MoveCategoryInfo{
				ID:             toID,
				Name:           toName,
				BudgetedBefore: toBudgeted,
				BudgetedAfter:  newToBudgeted,
			},
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Printf("Moved %s from '%s' to '%s' (%s)\n\n",
		transform.FormatCurrency(amountMilliunits), fromName, toName, month[:7])
	fmt.Printf("  %s: %s -> %s\n", fromName,
		transform.FormatCurrency(fromBudgeted), transform.FormatCurrency(newFromBudgeted))
	fmt.Printf("  %s: %s -> %s\n", toName,
		transform.FormatCurrency(toBudgeted), transform.FormatCurrency(newToBudgeted))

	return nil
}

// findCategoryName finds a category name by ID.
func findCategoryName(groups []*api.CategoryGroup, id string) string {
	for _, g := range groups {
		for _, c := range g.Categories {
			if c.ID == id {
				return c.Name
			}
		}
	}
	return id
}
