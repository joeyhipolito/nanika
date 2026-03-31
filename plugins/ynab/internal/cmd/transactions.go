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

// TransactionsOutput represents the JSON output for the transactions command.
type TransactionsOutput struct {
	Transactions []TransactionItem `json:"transactions"`
	Count        int               `json:"count"`
}

// TransactionItem represents a single transaction in the output.
type TransactionItem struct {
	ID           string `json:"id"`
	Date         string `json:"date"`
	Amount       int64  `json:"amount"`
	AmountDisplay string `json:"amount_display"`
	PayeeName    string `json:"payee_name"`
	CategoryName string `json:"category_name"`
	AccountName  string `json:"account_name"`
	Memo         string `json:"memo,omitempty"`
	Cleared      string `json:"cleared"`
	Approved     bool   `json:"approved"`
}

// TransactionsCmd lists transactions with optional filters.
func TransactionsCmd(client *api.Client, sinceDate, accountFilter, categoryFilter, payeeFilter string, limit int, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// Default since date: 30 days ago
	if sinceDate == "" {
		sinceDate = time.Now().AddDate(0, 0, -30).Format("2006-01-02")
	}

	var transactions []*api.Transaction

	// If account filter, resolve account ID and use account-specific endpoint
	if accountFilter != "" {
		accounts, err := client.GetAccounts(budgetID)
		if err != nil {
			return fmt.Errorf("failed to get accounts: %w", err)
		}
		accountID := findAccountID(accounts, accountFilter)
		if accountID == "" {
			return fmt.Errorf("no account found matching '%s'", accountFilter)
		}
		transactions, err = client.GetTransactionsByAccount(budgetID, accountID, sinceDate)
		if err != nil {
			return fmt.Errorf("failed to get transactions: %w", err)
		}
	} else if categoryFilter != "" {
		// Resolve category ID
		groups, err := client.GetCategories(budgetID)
		if err != nil {
			return fmt.Errorf("failed to get categories: %w", err)
		}
		categoryID := findCategoryID(groups, categoryFilter)
		if categoryID == "" {
			return fmt.Errorf("no category found matching '%s'", categoryFilter)
		}
		transactions, err = client.GetTransactionsByCategory(budgetID, categoryID, sinceDate)
		if err != nil {
			return fmt.Errorf("failed to get transactions: %w", err)
		}
	} else {
		transactions, err = client.GetTransactions(budgetID, sinceDate)
		if err != nil {
			return fmt.Errorf("failed to get transactions: %w", err)
		}
	}

	// Filter deleted
	var filtered []*api.Transaction
	for _, t := range transactions {
		if t.Deleted {
			continue
		}
		// Client-side payee filter
		if payeeFilter != "" {
			if !strings.Contains(strings.ToLower(t.PayeeName), strings.ToLower(payeeFilter)) {
				continue
			}
		}
		filtered = append(filtered, t)
	}

	// Apply limit
	if limit > 0 && len(filtered) > limit {
		filtered = filtered[len(filtered)-limit:]
	}

	if jsonOutput {
		output := TransactionsOutput{
			Transactions: make([]TransactionItem, 0, len(filtered)),
			Count:        len(filtered),
		}
		for _, t := range filtered {
			output.Transactions = append(output.Transactions, TransactionItem{
				ID:            t.ID,
				Date:          t.Date,
				Amount:        t.Amount,
				AmountDisplay: transform.FormatCurrency(t.Amount),
				PayeeName:     t.PayeeName,
				CategoryName:  t.CategoryName,
				AccountName:   t.AccountName,
				Memo:          t.Memo,
				Cleared:       t.Cleared,
				Approved:      t.Approved,
			})
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	if len(filtered) == 0 {
		fmt.Println("No transactions found.")
		return nil
	}

	// Human-readable output
	fmt.Printf("Transactions (since %s):\n\n", sinceDate)

	// Calculate column widths
	maxPayee := 15
	maxCategory := 12
	maxAccount := 10
	for _, t := range filtered {
		if len(t.PayeeName) > maxPayee && len(t.PayeeName) <= 30 {
			maxPayee = len(t.PayeeName)
		}
		if len(t.CategoryName) > maxCategory && len(t.CategoryName) <= 20 {
			maxCategory = len(t.CategoryName)
		}
		if len(t.AccountName) > maxAccount && len(t.AccountName) <= 15 {
			maxAccount = len(t.AccountName)
		}
	}

	fmt.Printf("%-12s  %-*s  %-*s  %12s  %-*s\n",
		"Date", maxPayee, "Payee", maxCategory, "Category", "Amount", maxAccount, "Account")
	fmt.Printf("%s\n", strings.Repeat("-", 12+maxPayee+maxCategory+12+maxAccount+8))

	for _, t := range filtered {
		payee := t.PayeeName
		if len(payee) > maxPayee {
			payee = payee[:maxPayee-1] + "~"
		}
		cat := t.CategoryName
		if len(cat) > maxCategory {
			cat = cat[:maxCategory-1] + "~"
		}
		acct := t.AccountName
		if len(acct) > maxAccount {
			acct = acct[:maxAccount-1] + "~"
		}

		fmt.Printf("%-12s  %-*s  %-*s  %12s  %-*s\n",
			t.Date, maxPayee, payee, maxCategory, cat,
			transform.FormatCurrency(t.Amount), maxAccount, acct)
	}

	fmt.Printf("\n%d transaction(s)\n", len(filtered))
	return nil
}

// findAccountID finds an account ID by name (case-insensitive partial match).
func findAccountID(accounts []*api.Account, filter string) string {
	lower := strings.ToLower(filter)
	for _, a := range accounts {
		if strings.EqualFold(a.Name, filter) {
			return a.ID
		}
	}
	for _, a := range accounts {
		if strings.Contains(strings.ToLower(a.Name), lower) {
			return a.ID
		}
	}
	return ""
}

// findCategoryID finds a category ID by name (case-insensitive partial match).
func findCategoryID(groups []*api.CategoryGroup, filter string) string {
	lower := strings.ToLower(filter)
	// Exact match first
	for _, g := range groups {
		for _, c := range g.Categories {
			if strings.EqualFold(c.Name, filter) {
				return c.ID
			}
		}
	}
	// Partial match
	for _, g := range groups {
		for _, c := range g.Categories {
			if strings.Contains(strings.ToLower(c.Name), lower) {
				return c.ID
			}
		}
	}
	return ""
}
