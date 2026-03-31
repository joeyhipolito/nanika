package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

// ScheduledOutput represents the JSON output for the scheduled command.
type ScheduledOutput struct {
	Transactions []ScheduledItem `json:"scheduled_transactions"`
	Count        int             `json:"count"`
}

// ScheduledItem represents a scheduled transaction in the output.
type ScheduledItem struct {
	ID           string `json:"id"`
	DateNext     string `json:"date_next"`
	Frequency    string `json:"frequency"`
	Amount       int64  `json:"amount"`
	AmountDisplay string `json:"amount_display"`
	PayeeName    string `json:"payee_name"`
	CategoryName string `json:"category_name"`
	AccountName  string `json:"account_name"`
	Memo         string `json:"memo,omitempty"`
}

// ScheduledCmd lists all scheduled/recurring transactions.
func ScheduledCmd(client *api.Client, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	scheduled, err := client.GetScheduledTransactions(budgetID)
	if err != nil {
		return fmt.Errorf("failed to get scheduled transactions: %w", err)
	}

	// Filter deleted
	var filtered []*api.ScheduledTransaction
	for _, s := range scheduled {
		if !s.Deleted {
			filtered = append(filtered, s)
		}
	}

	if jsonOutput {
		output := ScheduledOutput{
			Transactions: make([]ScheduledItem, 0, len(filtered)),
			Count:        len(filtered),
		}
		for _, s := range filtered {
			output.Transactions = append(output.Transactions, ScheduledItem{
				ID:            s.ID,
				DateNext:      s.DateNext,
				Frequency:     s.Frequency,
				Amount:        s.Amount,
				AmountDisplay: transform.FormatCurrency(s.Amount),
				PayeeName:     s.PayeeName,
				CategoryName:  s.CategoryName,
				AccountName:   s.AccountName,
				Memo:          s.Memo,
			})
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	if len(filtered) == 0 {
		fmt.Println("No scheduled transactions.")
		return nil
	}

	fmt.Printf("Scheduled Transactions:\n\n")

	maxPayee := 15
	maxCategory := 12
	for _, s := range filtered {
		if len(s.PayeeName) > maxPayee && len(s.PayeeName) <= 25 {
			maxPayee = len(s.PayeeName)
		}
		if len(s.CategoryName) > maxCategory && len(s.CategoryName) <= 20 {
			maxCategory = len(s.CategoryName)
		}
	}

	fmt.Printf("%-12s  %-14s  %-*s  %-*s  %12s\n",
		"Next Date", "Frequency", maxPayee, "Payee", maxCategory, "Category", "Amount")
	fmt.Printf("%s\n", strings.Repeat("-", 12+14+maxPayee+maxCategory+12+8))

	for _, s := range filtered {
		fmt.Printf("%-12s  %-14s  %-*s  %-*s  %12s\n",
			s.DateNext, formatFrequency(s.Frequency),
			maxPayee, s.PayeeName, maxCategory, s.CategoryName,
			transform.FormatCurrency(s.Amount))
	}

	fmt.Printf("\n%d scheduled transaction(s)\n", len(filtered))
	return nil
}

func formatFrequency(freq string) string {
	switch freq {
	case "never":
		return "Once"
	case "daily":
		return "Daily"
	case "weekly":
		return "Weekly"
	case "everyOtherWeek":
		return "Every 2 Weeks"
	case "twiceAMonth":
		return "Twice/Month"
	case "every4Weeks":
		return "Every 4 Weeks"
	case "monthly":
		return "Monthly"
	case "everyOtherMonth":
		return "Every 2 Months"
	case "every3Months":
		return "Quarterly"
	case "every4Months":
		return "Every 4 Months"
	case "twiceAYear":
		return "Twice/Year"
	case "yearly":
		return "Yearly"
	case "everyOtherYear":
		return "Every 2 Years"
	default:
		return freq
	}
}
