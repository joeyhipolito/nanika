package db

import "time"

// Account represents a financial account.
type Account struct {
	ID          int64
	Name        string
	Type        string    // "checking", "savings", "credit_card", "investment"
	BalanceCents int64
	Currency    string
	Archived    bool
	LastUpdated time.Time
	CreatedAt   time.Time
}

// Transaction represents a single account transaction.
type Transaction struct {
	ID               int64
	AccountID        int64
	Description      string
	AmountCents      int64     // Negative for debits
	Category         string
	TransactionDate  time.Time // When the transaction occurred
	PostedDate       time.Time // When it was recorded
	Notes            string
	CreatedAt        time.Time
}

// Budget represents a monthly budget limit for a category.
type Budget struct {
	ID         int64
	Category   string
	Month      string // YYYY-MM format
	LimitCents int64
	CreatedAt  time.Time
}

// CategoryGroup represents a named group of categories (e.g., "NZ Living").
type CategoryGroup struct {
	ID        int64
	Name      string
	Currency  string
	CreatedAt time.Time
}

// Category represents an expense/income category.
type Category struct {
	ID           int64
	Name         string
	GroupID      int64 // Foreign key to category_groups
	Archived     bool
	TargetType   string // "set_aside" or "refill"; empty = no target
	TargetCents  int64
	DueDay       *int   // 1-31 for monthly, 1-7 for weekly; nil = ongoing
	DueFrequency string // "monthly" or "weekly"; default "monthly"
	CreatedAt    time.Time
}

// TransactionRow extends Transaction with the account name resolved.
type TransactionRow struct {
	Transaction
	AccountName string
}

// BudgetSummary combines a budget row with computed spending and carryover data.
type BudgetSummary struct {
	Category       string
	Month          string
	LimitCents     int64
	SpentCents     int64  // absolute value of negative transactions for the month
	CarryoverCents int64  // previous month underspend (positive only)
	RemainCents    int64  // LimitCents + CarryoverCents - SpentCents
	Currency       string // currency code from the category's group (e.g., "NZD", "PHP")
	GroupName      string // name of the category group
	TargetType     string // "set_aside" or "refill"; empty = no target
	TargetCents    int64
	DueDay         *int   // 1-31 for monthly, 1-7 for weekly; nil = ongoing
	DueFrequency   string // "monthly" or "weekly"
}

// ExchangeRate stores the latest known rate between two currencies.
type ExchangeRate struct {
	ID           int64
	FromCurrency string
	ToCurrency   string
	Rate         float64
	RecordedAt   time.Time
}

// ScheduledTransaction is a recurring transaction template.
type ScheduledTransaction struct {
	ID          int64
	AccountID   int64
	Description string
	AmountCents int64
	Category    string
	Frequency   string    // "daily", "weekly", "monthly", "yearly"
	NextDueDate time.Time // date of next materialization
	CreatedAt   time.Time
}

// ScheduledRow extends ScheduledTransaction with the account name resolved.
type ScheduledRow struct {
	ScheduledTransaction
	AccountName string
}

// CategorySpend represents total spending for a single category in a report.
type CategorySpend struct {
	Category   string
	TotalCents int64
	Count      int
}
