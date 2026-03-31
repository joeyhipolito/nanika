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

type ynabQueryStatusOutput struct {
	BudgetName    string                  `json:"budget_name"`
	Month         string                  `json:"month"`
	TotalBudgeted int64                   `json:"total_budgeted_milliunits"`
	TotalActivity int64                   `json:"total_activity_milliunits"`
	TotalBalance  int64                   `json:"total_balance_milliunits"`
	Categories    []ynabCategoryStatus    `json:"categories"`
	Transactions  []ynabTransactionStatus `json:"transactions"`
	Accounts      []ynabAccountStatus     `json:"accounts"`
}

type ynabCategoryStatus struct {
	CategoryGroup string `json:"category_group"`
	Category      string `json:"category"`
	Budgeted      int64  `json:"budgeted_milliunits"`
	Activity      int64  `json:"activity_milliunits"`
	Balance       int64  `json:"balance_milliunits"`
}

type ynabTransactionStatus struct {
	Date         string `json:"date"`
	PayeeName    string `json:"payee_name"`
	CategoryName string `json:"category_name"`
	Amount       int64  `json:"amount_milliunits"`
}

type ynabAccountStatus struct {
	Name     string `json:"name"`
	Type     string `json:"type"`
	Balance  int64  `json:"balance_milliunits"`
	OnBudget bool   `json:"on_budget"`
}

type ynabOverspentItem struct {
	CategoryGroup string `json:"category_group"`
	Category      string `json:"category"`
	Balance       int64  `json:"balance_milliunits"`
}

type ynabQueryItemsOutput struct {
	Items []ynabOverspentItem `json:"items"`
	Count int                 `json:"count"`
}

type ynabQueryAction struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type ynabQueryActionsOutput struct {
	Actions []ynabQueryAction `json:"actions"`
}

func QueryCmd(client *api.Client, subcommand string, jsonOutput bool) error {
	switch subcommand {
	case "status":
		return ynabQueryStatus(client, jsonOutput)
	case "items":
		return ynabQueryItems(client, jsonOutput)
	case "actions":
		return ynabQueryActions(jsonOutput)
	default:
		return fmt.Errorf("unknown query subcommand %q — use status, items, or actions", subcommand)
	}
}

func ynabQueryStatus(client *api.Client, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	budgets, err := client.GetBudgets()
	if err != nil {
		return fmt.Errorf("fetching budgets: %w", err)
	}

	budgetName := budgetID
	for _, b := range budgets {
		if b.ID == budgetID {
			budgetName = b.Name
			break
		}
	}

	categoryGroups, err := client.GetCategories(budgetID)
	if err != nil {
		return fmt.Errorf("fetching categories: %w", err)
	}

	now := time.Now()
	month := transform.FormatMonth(now.Year(), int(now.Month()))

	var totalBudgeted, totalActivity, totalBalance int64
	var categories []ynabCategoryStatus
	for _, group := range categoryGroups {
		if group.Hidden || group.Deleted || group.Name == "Internal Master Category" {
			continue
		}
		for _, cat := range group.Categories {
			if cat.Hidden || cat.Deleted {
				continue
			}
			totalBudgeted += cat.Budgeted
			totalActivity += cat.Activity
			totalBalance += cat.Balance
			if cat.Budgeted != 0 || cat.Activity != 0 {
				categories = append(categories, ynabCategoryStatus{
					CategoryGroup: group.Name,
					Category:      cat.Name,
					Budgeted:      cat.Budgeted,
					Activity:      cat.Activity,
					Balance:       cat.Balance,
				})
			}
		}
	}

	// Fetch on-budget accounts (best-effort; non-fatal on error)
	var accounts []ynabAccountStatus
	if accts, aErr := client.GetAccounts(budgetID); aErr == nil {
		for _, a := range accts {
			if !a.Deleted && !a.Closed && a.OnBudget {
				accounts = append(accounts, ynabAccountStatus{
					Name:     a.Name,
					Type:     a.Type,
					Balance:  a.Balance,
					OnBudget: a.OnBudget,
				})
			}
		}
	}

	// Fetch recent transactions — last 14 days, most-recent 10 (best-effort)
	var transactions []ynabTransactionStatus
	since := now.AddDate(0, 0, -14).Format("2006-01-02")
	if txns, tErr := client.GetTransactions(budgetID, since); tErr == nil {
		// YNAB returns transactions oldest-first; reverse to get newest first
		limit := 10
		for i := len(txns) - 1; i >= 0 && len(transactions) < limit; i-- {
			t := txns[i]
			if t.Deleted {
				continue
			}
			transactions = append(transactions, ynabTransactionStatus{
				Date:         t.Date,
				PayeeName:    t.PayeeName,
				CategoryName: t.CategoryName,
				Amount:       t.Amount,
			})
		}
	}

	out := ynabQueryStatusOutput{
		BudgetName:    budgetName,
		Month:         month,
		TotalBudgeted: totalBudgeted,
		TotalActivity: totalActivity,
		TotalBalance:  totalBalance,
		Categories:    categories,
		Transactions:  transactions,
		Accounts:      accounts,
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Println("YNAB Budget Summary")
	fmt.Println(strings.Repeat("=", 30))
	fmt.Printf("  Budget:     %s\n", out.BudgetName)
	fmt.Printf("  Month:      %s\n", out.Month)
	fmt.Printf("  Budgeted:   %s\n", transform.FormatCurrency(out.TotalBudgeted))
	fmt.Printf("  Activity:   %s\n", transform.FormatCurrency(out.TotalActivity))
	fmt.Printf("  Balance:    %s\n", transform.FormatCurrency(out.TotalBalance))
	return nil
}

func ynabQueryItems(client *api.Client, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	categoryGroups, err := client.GetCategories(budgetID)
	if err != nil {
		return fmt.Errorf("fetching categories: %w", err)
	}

	var overspent []ynabOverspentItem
	for _, group := range categoryGroups {
		if group.Hidden || group.Deleted || group.Name == "Internal Master Category" {
			continue
		}
		for _, cat := range group.Categories {
			if cat.Hidden || cat.Deleted || cat.Balance >= 0 {
				continue
			}
			overspent = append(overspent, ynabOverspentItem{
				CategoryGroup: group.Name,
				Category:      cat.Name,
				Balance:       cat.Balance,
			})
		}
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(ynabQueryItemsOutput{Items: overspent, Count: len(overspent)})
	}

	if len(overspent) == 0 {
		fmt.Println("No overspent categories.")
		return nil
	}
	fmt.Printf("Overspent categories (%d):\n", len(overspent))
	for _, item := range overspent {
		fmt.Printf("  %-30s %-25s %s\n", item.CategoryGroup, item.Category, transform.FormatCurrency(item.Balance))
	}
	return nil
}

func ynabQueryActions(jsonOutput bool) error {
	actions := []ynabQueryAction{
		{Name: "status", Command: "ynab status", Description: "Show budget status and metadata"},
		{Name: "balance", Command: "ynab balance", Description: "Show account balances"},
		{Name: "budget", Command: "ynab budget", Description: "Show current month's budget"},
		{Name: "transactions", Command: "ynab transactions", Description: "List recent transactions"},
		{Name: "add", Command: "ynab add <amount> <payee> [category]", Description: "Add a new transaction"},
		{Name: "edit", Command: "ynab edit <id>", Description: "Edit an existing transaction"},
		{Name: "move", Command: "ynab move <amount> --from <cat> --to <cat>", Description: "Move money between categories"},
		{Name: "categories", Command: "ynab categories", Description: "List all categories with IDs"},
	}
	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(ynabQueryActionsOutput{Actions: actions})
	}
	fmt.Println("Available actions:")
	for _, a := range actions {
		fmt.Printf("  %-45s  %s\n", a.Command, a.Description)
	}
	return nil
}
