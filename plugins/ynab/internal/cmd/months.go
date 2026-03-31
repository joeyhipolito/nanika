package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// MonthsListOutput represents the JSON output for the months list.
type MonthsListOutput struct {
	Months []MonthSummary `json:"months"`
}

// MonthSummary represents a single month summary.
type MonthSummary struct {
	Month        string `json:"month"`
	Income       int64  `json:"income"`
	Budgeted     int64  `json:"budgeted"`
	Activity     int64  `json:"activity"`
	ToBeBudgeted int64  `json:"to_be_budgeted"`
	AgeOfMoney   int    `json:"age_of_money,omitempty"`
}

// MonthDetailOutput represents the JSON output for a single month detail.
type MonthDetailOutput struct {
	Month        string               `json:"month"`
	Income       int64                `json:"income"`
	Budgeted     int64                `json:"budgeted"`
	Activity     int64                `json:"activity"`
	ToBeBudgeted int64                `json:"to_be_budgeted"`
	Categories   []MonthCategoryItem  `json:"categories,omitempty"`
}

// MonthCategoryItem represents a category within a month.
type MonthCategoryItem struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Budgeted int64  `json:"budgeted"`
	Activity int64  `json:"activity"`
	Balance  int64  `json:"balance"`
}

// MonthsCmd lists all budget months or shows detail for a specific month.
func MonthsCmd(client *api.Client, monthArg string, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// If a specific month is requested, show detail
	if monthArg != "" {
		return monthDetailCmd(client, budgetID, monthArg, jsonOutput)
	}

	// List all months
	months, err := client.GetMonths(budgetID)
	if err != nil {
		return fmt.Errorf("failed to get months: %w", err)
	}

	if jsonOutput {
		output := MonthsListOutput{
			Months: make([]MonthSummary, 0, len(months)),
		}
		for _, m := range months {
			if m.Deleted {
				continue
			}
			output.Months = append(output.Months, MonthSummary{
				Month:        m.Month,
				Income:       m.Income,
				Budgeted:     m.Budgeted,
				Activity:     m.Activity,
				ToBeBudgeted: m.ToBeBudgeted,
				AgeOfMoney:   m.AgeOfMoney,
			})
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Printf("Budget Months:\n\n")
	fmt.Printf("%-12s  %12s  %12s  %12s  %12s\n",
		"Month", "Income", "Budgeted", "Activity", "TBB")
	fmt.Printf("%s\n", strings.Repeat("-", 64))

	for _, m := range months {
		if m.Deleted {
			continue
		}
		fmt.Printf("%-12s  %12s  %12s  %12s  %12s\n",
			m.Month[:7], // YYYY-MM
			transform.FormatCurrency(m.Income),
			transform.FormatCurrency(m.Budgeted),
			transform.FormatCurrency(m.Activity),
			transform.FormatCurrency(m.ToBeBudgeted))
	}

	return nil
}

func monthDetailCmd(client *api.Client, budgetID, monthArg string, jsonOutput bool) error {
	// Normalize month format: YYYY-MM -> YYYY-MM-01
	if len(monthArg) == 7 {
		monthArg += "-01"
	}

	month, err := client.GetMonth(budgetID, monthArg)
	if err != nil {
		return fmt.Errorf("failed to get month: %w", err)
	}

	if jsonOutput {
		output := MonthDetailOutput{
			Month:        month.Month,
			Income:       month.Income,
			Budgeted:     month.Budgeted,
			Activity:     month.Activity,
			ToBeBudgeted: month.ToBeBudgeted,
			Categories:   make([]MonthCategoryItem, 0),
		}
		for _, c := range month.Categories {
			if c.Hidden || c.Deleted {
				continue
			}
			output.Categories = append(output.Categories, MonthCategoryItem{
				ID:       c.ID,
				Name:     c.Name,
				Budgeted: c.Budgeted,
				Activity: c.Activity,
				Balance:  c.Balance,
			})
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Printf("Month: %s\n\n", month.Month[:7])
	fmt.Printf("Income:         %s\n", transform.FormatCurrency(month.Income))
	fmt.Printf("Budgeted:       %s\n", transform.FormatCurrency(month.Budgeted))
	fmt.Printf("Activity:       %s\n", transform.FormatCurrency(month.Activity))
	fmt.Printf("To Be Budgeted: %s\n", transform.FormatCurrency(month.ToBeBudgeted))

	if month.Categories != nil && len(month.Categories) > 0 {
		fmt.Printf("\nCategories:\n\n")

		maxName := 15
		for _, c := range month.Categories {
			if !c.Hidden && !c.Deleted && len(c.Name) > maxName && len(c.Name) <= 25 {
				maxName = len(c.Name)
			}
		}

		fmt.Printf("%-*s  %12s  %12s  %12s\n", maxName, "Category", "Budgeted", "Activity", "Balance")
		fmt.Printf("%s\n", strings.Repeat("-", maxName+12+12+12+6))

		for _, c := range month.Categories {
			if c.Hidden || c.Deleted {
				continue
			}
			fmt.Printf("%-*s  %12s  %12s  %12s\n",
				maxName, c.Name,
				transform.FormatCurrency(c.Budgeted),
				transform.FormatCurrency(c.Activity),
				transform.FormatCurrency(c.Balance))
		}
	}

	return nil
}
