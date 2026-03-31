package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// AddOutput represents the JSON output format for the add command.
type AddOutput struct {
	TransactionID string `json:"transaction_id"`
	Date          string `json:"date"`
	Amount        int64  `json:"amount"`
	AmountDisplay string `json:"amount_display"`
	Payee         string `json:"payee"`
	Category      string `json:"category,omitempty"`
	Account       string `json:"account"`
	Memo          string `json:"memo,omitempty"`
}

// AddCmd creates a new transaction.
//
// Parameters:
//   - amount: Dollar amount as string (e.g., "50.00", "25", "-100.50")
//   - payee: Payee name (required)
//   - category: Category name (optional - can be empty for uncategorized)
//   - account: Account name (optional - uses first on-budget account if empty)
//   - date: ISO date YYYY-MM-DD (optional - uses today if empty)
//   - memo: Transaction memo (optional)
//   - jsonOutput: If true, outputs JSON instead of human-readable format
//
// Amount handling:
//   - Positive amounts are inflows (income)
//   - Negative amounts are outflows (expenses)
//   - For expenses, you can use either "-50" or "50" (defaults to expense)
func AddCmd(client *api.Client, amount, payee, category, account, date, memo string, jsonOutput bool) error {
	// Validate required parameters
	if amount == "" {
		return fmt.Errorf("amount is required")
	}
	if payee == "" {
		return fmt.Errorf("payee is required")
	}

	// Parse amount from dollars to milliunits
	amountFloat, err := strconv.ParseFloat(amount, 64)
	if err != nil {
		return fmt.Errorf("invalid amount: %s (expected decimal number like 50.00)", amount)
	}

	// Convert to milliunits
	amountMilliunits := transform.DollarsToMilliunits(amountFloat)

	// Default to expense (negative) if positive amount is given
	// Users typically think "I spent $50" not "I spent -$50"
	if amountMilliunits > 0 && !strings.HasPrefix(amount, "+") {
		amountMilliunits = -amountMilliunits
	}

	// Get default budget ID
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	// If no date provided, use today
	if date == "" {
		date = transform.FormatDate(time.Now())
	}

	// Validate date format
	parsedDate := transform.ParseDate(date)
	if parsedDate.IsZero() {
		return fmt.Errorf("invalid date format: %s (expected YYYY-MM-DD)", date)
	}

	// Find account by name or use default
	accountID, accountName, err := findAccount(client, budgetID, account)
	if err != nil {
		return err
	}

	// Find category by name (if provided)
	var categoryID string
	var categoryName string
	if category != "" {
		categoryID, categoryName, err = findCategory(client, budgetID, category)
		if err != nil {
			return err
		}
	}

	// Create transaction request
	txnReq := &api.TransactionRequest{
		BudgetID:  budgetID,
		AccountID: accountID,
		Date:      date,
		Amount:    amountMilliunits,
		PayeeName: payee,
		Memo:      memo,
		Cleared:   "uncleared",
		Approved:  true,
	}

	if categoryID != "" {
		txnReq.CategoryID = categoryID
	}

	// Create the transaction
	txn, err := client.CreateTransaction(txnReq)
	if err != nil {
		return fmt.Errorf("failed to create transaction: %w", err)
	}

	// If JSON output requested, marshal and print
	if jsonOutput {
		output := AddOutput{
			TransactionID: txn.ID,
			Date:          txn.Date,
			Amount:        txn.Amount,
			AmountDisplay: transform.FormatCurrency(txn.Amount),
			Payee:         txn.PayeeName,
			Account:       accountName,
			Memo:          txn.Memo,
		}

		if categoryName != "" {
			output.Category = categoryName
		}

		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		if err := encoder.Encode(output); err != nil {
			return fmt.Errorf("failed to encode JSON: %w", err)
		}
		return nil
	}

	// Human-readable output
	fmt.Printf("Transaction created successfully!\n\n")
	fmt.Printf("Date:     %s\n", formatDateHuman(txn.Date))
	fmt.Printf("Amount:   %s\n", transform.FormatCurrency(txn.Amount))
	fmt.Printf("Payee:    %s\n", txn.PayeeName)

	if categoryName != "" {
		fmt.Printf("Category: %s\n", categoryName)
	} else {
		fmt.Printf("Category: Uncategorized\n")
	}

	fmt.Printf("Account:  %s\n", accountName)

	if txn.Memo != "" {
		fmt.Printf("Memo:     %s\n", txn.Memo)
	}

	fmt.Printf("\nTransaction ID: %s\n", txn.ID)

	return nil
}

// findAccount finds an account by name (case-insensitive partial match).
// If accountName is empty, returns the first on-budget account.
func findAccount(client *api.Client, budgetID, accountName string) (string, string, error) {
	accounts, err := client.GetAccounts(budgetID)
	if err != nil {
		return "", "", fmt.Errorf("failed to get accounts: %w", err)
	}

	// Filter to on-budget, open accounts
	var validAccounts []*api.Account
	for _, acc := range accounts {
		if acc.OnBudget && !acc.Closed && !acc.Deleted {
			validAccounts = append(validAccounts, acc)
		}
	}

	if len(validAccounts) == 0 {
		return "", "", fmt.Errorf("no on-budget accounts found")
	}

	// If no account name specified, use first on-budget account
	if accountName == "" {
		return validAccounts[0].ID, validAccounts[0].Name, nil
	}

	// Try to find account by name (case-insensitive partial match)
	accountNameLower := strings.ToLower(accountName)
	var matches []*api.Account

	// First pass: exact match
	for _, acc := range validAccounts {
		if strings.ToLower(acc.Name) == accountNameLower {
			return acc.ID, acc.Name, nil
		}
	}

	// Second pass: partial match
	for _, acc := range validAccounts {
		if strings.Contains(strings.ToLower(acc.Name), accountNameLower) {
			matches = append(matches, acc)
		}
	}

	if len(matches) == 0 {
		// List available accounts
		var accountNames []string
		for _, acc := range validAccounts {
			accountNames = append(accountNames, acc.Name)
		}
		return "", "", fmt.Errorf("account not found: %s\nAvailable accounts: %s",
			accountName, strings.Join(accountNames, ", "))
	}

	if len(matches) > 1 {
		// List matching accounts
		var matchNames []string
		for _, acc := range matches {
			matchNames = append(matchNames, acc.Name)
		}
		return "", "", fmt.Errorf("multiple accounts match '%s': %s\nPlease be more specific",
			accountName, strings.Join(matchNames, ", "))
	}

	// Single match found
	return matches[0].ID, matches[0].Name, nil
}

// findCategory finds a category by name (case-insensitive partial match).
func findCategory(client *api.Client, budgetID, categoryName string) (string, string, error) {
	categoryGroups, err := client.GetCategories(budgetID)
	if err != nil {
		return "", "", fmt.Errorf("failed to get categories: %w", err)
	}

	categoryNameLower := strings.ToLower(categoryName)
	var matches []*api.Category

	// Build list of all valid categories
	var validCategories []*api.Category
	for _, group := range categoryGroups {
		if group.Hidden || group.Deleted {
			continue
		}
		for _, cat := range group.Categories {
			if !cat.Hidden && !cat.Deleted {
				validCategories = append(validCategories, cat)
			}
		}
	}

	// First pass: exact match
	for _, cat := range validCategories {
		if strings.ToLower(cat.Name) == categoryNameLower {
			return cat.ID, cat.Name, nil
		}
	}

	// Second pass: partial match
	for _, cat := range validCategories {
		if strings.Contains(strings.ToLower(cat.Name), categoryNameLower) {
			matches = append(matches, cat)
		}
	}

	if len(matches) == 0 {
		// List some available categories (limit to 10 for readability)
		var categoryNames []string
		for i, cat := range validCategories {
			if i >= 10 {
				categoryNames = append(categoryNames, "...")
				break
			}
			categoryNames = append(categoryNames, cat.Name)
		}
		return "", "", fmt.Errorf("category not found: %s\nSome available categories: %s",
			categoryName, strings.Join(categoryNames, ", "))
	}

	if len(matches) > 1 {
		// List matching categories
		var matchNames []string
		for _, cat := range matches {
			matchNames = append(matchNames, cat.Name)
		}
		return "", "", fmt.Errorf("multiple categories match '%s': %s\nPlease be more specific",
			categoryName, strings.Join(matchNames, ", "))
	}

	// Single match found
	return matches[0].ID, matches[0].Name, nil
}

// formatDateHuman formats a date string for human-readable output.
func formatDateHuman(dateStr string) string {
	t := transform.ParseDate(dateStr)
	if t.IsZero() {
		return dateStr
	}
	return transform.FormatDate(t)
}
