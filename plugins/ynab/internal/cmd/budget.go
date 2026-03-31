package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// BudgetOutput represents the JSON output format for the budget command.
type BudgetOutput struct {
	Month          string          `json:"month"`
	CategoryGroups []CategoryGroup `json:"category_groups"`
}

// CategoryGroup represents a category group with its categories.
type CategoryGroup struct {
	ID         string           `json:"id"`
	Name       string           `json:"name"`
	Categories []CategoryBudget `json:"categories"`
	TotalBudgeted int64         `json:"total_budgeted"`
	TotalActivity int64         `json:"total_activity"`
	TotalBalance  int64         `json:"total_balance"`
}

// CategoryBudget represents a single category's budget information.
type CategoryBudget struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Budgeted int64  `json:"budgeted"`
	Activity int64  `json:"activity"`
	Balance  int64  `json:"balance"`
}

// BudgetCmd retrieves and displays category budgets for the current month.
// Categories are grouped by their category groups.
// If jsonOutput is true, outputs JSON instead of human-readable format.
func BudgetCmd(client *api.Client, jsonOutput bool) error {
	// Get default budget ID
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Get all category groups
	categoryGroups, err := client.GetCategories(budgetID)
	if err != nil {
		return fmt.Errorf("failed to get categories: %w", err)
	}

	// Determine current month in YNAB format (YYYY-MM-01)
	now := time.Now()
	currentMonth := transform.FormatMonth(now.Year(), int(now.Month())) + "-01"

	// If JSON output requested, marshal and print
	if jsonOutput {
		output := BudgetOutput{
			Month:          currentMonth,
			CategoryGroups: make([]CategoryGroup, 0),
		}

		for _, group := range categoryGroups {
			// Skip hidden and deleted groups
			if group.Hidden || group.Deleted {
				continue
			}

			// Skip internal master category
			if group.Name == "Internal Master Category" {
				continue
			}

			categoryGroup := CategoryGroup{
				ID:         group.ID,
				Name:       group.Name,
				Categories: make([]CategoryBudget, 0),
			}

			for _, category := range group.Categories {
				// Skip hidden and deleted categories
				if category.Hidden || category.Deleted {
					continue
				}

				categoryGroup.Categories = append(categoryGroup.Categories, CategoryBudget{
					ID:       category.ID,
					Name:     category.Name,
					Budgeted: category.Budgeted,
					Activity: category.Activity,
					Balance:  category.Balance,
				})

				// Add to group totals
				categoryGroup.TotalBudgeted += category.Budgeted
				categoryGroup.TotalActivity += category.Activity
				categoryGroup.TotalBalance += category.Balance
			}

			// Only include groups that have categories
			if len(categoryGroup.Categories) > 0 {
				output.CategoryGroups = append(output.CategoryGroups, categoryGroup)
			}
		}

		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		if err := encoder.Encode(output); err != nil {
			return fmt.Errorf("failed to encode JSON: %w", err)
		}
		return nil
	}

	// Human-readable output
	year, month, _ := transform.ParseMonth(currentMonth)
	fmt.Printf("Budget for %s\n\n", transform.FormatMonth(year, month))

	// Track grand totals
	var grandTotalBudgeted int64
	var grandTotalActivity int64
	var grandTotalBalance int64

	// Process each category group
	for _, group := range categoryGroups {
		// Skip hidden and deleted groups
		if group.Hidden || group.Deleted {
			continue
		}

		// Skip internal master category
		if group.Name == "Internal Master Category" {
			continue
		}

		// Filter out hidden/deleted categories
		var visibleCategories []*api.Category
		for _, category := range group.Categories {
			if !category.Hidden && !category.Deleted {
				visibleCategories = append(visibleCategories, category)
			}
		}

		// Skip groups with no visible categories
		if len(visibleCategories) == 0 {
			continue
		}

		// Print group header
		fmt.Printf("%s\n", group.Name)
		fmt.Printf("%s\n", strings.Repeat("-", len(group.Name)))

		// Calculate column width for category names
		maxNameLen := 20
		for _, category := range visibleCategories {
			if len(category.Name) > maxNameLen {
				maxNameLen = len(category.Name)
			}
		}

		// Print categories
		var groupTotalBudgeted int64
		var groupTotalActivity int64
		var groupTotalBalance int64

		for _, category := range visibleCategories {
			fmt.Printf("  %-*s  %15s  %15s  %15s\n",
				maxNameLen, category.Name,
				transform.FormatCurrency(category.Budgeted),
				transform.FormatCurrency(category.Activity),
				transform.FormatCurrency(category.Balance))

			groupTotalBudgeted += category.Budgeted
			groupTotalActivity += category.Activity
			groupTotalBalance += category.Balance
		}

		// Print group totals if there's more than one category
		if len(visibleCategories) > 1 {
			fmt.Printf("  %s\n", strings.Repeat("-", maxNameLen+15+15+15+6))
			fmt.Printf("  %-*s  %15s  %15s  %15s\n",
				maxNameLen, "Total",
				transform.FormatCurrency(groupTotalBudgeted),
				transform.FormatCurrency(groupTotalActivity),
				transform.FormatCurrency(groupTotalBalance))
		}

		fmt.Println()

		// Add to grand totals
		grandTotalBudgeted += groupTotalBudgeted
		grandTotalActivity += groupTotalActivity
		grandTotalBalance += groupTotalBalance
	}

	// Print grand totals
	fmt.Printf("Overall Totals\n")
	fmt.Printf("==============\n")
	fmt.Printf("Budgeted:  %s\n", transform.FormatCurrency(grandTotalBudgeted))
	fmt.Printf("Activity:  %s\n", transform.FormatCurrency(grandTotalActivity))
	fmt.Printf("Balance:   %s\n", transform.FormatCurrency(grandTotalBalance))

	return nil
}
