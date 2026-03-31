package api

// Budget represents a YNAB budget.
type Budget struct {
	ID             string          `json:"id"`
	Name           string          `json:"name"`
	LastModifiedOn string          `json:"last_modified_on"`
	FirstMonth     string          `json:"first_month,omitempty"`
	LastMonth      string          `json:"last_month,omitempty"`
	DateFormat     *DateFormat     `json:"date_format,omitempty"`
	CurrencyFormat *CurrencyFormat `json:"currency_format,omitempty"`
	Accounts       []*Account      `json:"accounts,omitempty"`
}

// DateFormat represents date formatting options.
type DateFormat struct {
	Format string `json:"format"`
}

// CurrencyFormat represents currency formatting options.
type CurrencyFormat struct {
	ISOCode          string `json:"iso_code"`
	ExampleFormat    string `json:"example_format"`
	DecimalDigits    int    `json:"decimal_digits"`
	DecimalSeparator string `json:"decimal_separator"`
	SymbolFirst      bool   `json:"symbol_first"`
	GroupSeparator   string `json:"group_separator"`
	CurrencySymbol   string `json:"currency_symbol"`
	DisplaySymbol    bool   `json:"display_symbol"`
}

// BudgetDetail represents detailed budget information.
type BudgetDetail struct {
	Budget          *Budget          `json:"budget"`
	ServerKnowledge int64            `json:"server_knowledge"`
	Accounts        []*Account       `json:"accounts,omitempty"`
	CategoryGroups  []*CategoryGroup `json:"category_groups,omitempty"`
	Payees          []*Payee         `json:"payees,omitempty"`
	Transactions    []*Transaction   `json:"transactions,omitempty"`
}

// CategoryGroup represents a category group.
type CategoryGroup struct {
	ID         string      `json:"id"`
	Name       string      `json:"name"`
	Hidden     bool        `json:"hidden"`
	Deleted    bool        `json:"deleted"`
	Categories []*Category `json:"categories,omitempty"`
}

// Category represents a budget category.
type Category struct {
	ID                      string `json:"id"`
	CategoryGroupID         string `json:"category_group_id"`
	Name                    string `json:"name"`
	Hidden                  bool   `json:"hidden"`
	OriginalCategoryGroupID string `json:"original_category_group_id,omitempty"`
	Note                    string `json:"note,omitempty"`
	Budgeted                int64  `json:"budgeted"` // Amount budgeted in milliunits for the month
	Activity                int64  `json:"activity"` // Activity amount in milliunits for the month
	Balance                 int64  `json:"balance"`  // Balance in milliunits
	GoalType                string `json:"goal_type,omitempty"` // Goal type (TB, TBD, MF, NEED, DEBT)
	GoalCreationMonth       string `json:"goal_creation_month,omitempty"`
	GoalTarget              int64  `json:"goal_target,omitempty"` // Target amount in milliunits
	GoalTargetMonth         string `json:"goal_target_month,omitempty"`
	GoalPercentageComplete  int    `json:"goal_percentage_complete,omitempty"`
	Deleted                 bool   `json:"deleted"`
}

// Account represents a budget account.
type Account struct {
	ID               string `json:"id"`
	Name             string `json:"name"`
	Type             string `json:"type"` // checking, savings, creditCard, cash, lineOfCredit, otherAsset, otherLiability
	OnBudget         bool   `json:"on_budget"`
	Closed           bool   `json:"closed"`
	Note             string `json:"note,omitempty"`
	Balance          int64  `json:"balance"`           // Balance in milliunits
	ClearedBalance   int64  `json:"cleared_balance"`   // Cleared balance in milliunits
	UnclearedBalance int64  `json:"uncleared_balance"` // Uncleared balance in milliunits
	TransferPayeeID  string `json:"transfer_payee_id,omitempty"`
	Deleted          bool   `json:"deleted"`
}

// Transaction represents a YNAB transaction.
type Transaction struct {
	ID                    string            `json:"id"`
	Date                  string            `json:"date"`     // ISO date format (YYYY-MM-DD)
	Amount                int64             `json:"amount"`   // Amount in milliunits (negative = outflow, positive = inflow)
	Memo                  string            `json:"memo,omitempty"`
	Cleared               string            `json:"cleared"`  // cleared, uncleared, reconciled
	Approved              bool              `json:"approved"`
	FlagColor             string            `json:"flag_color,omitempty"` // red, orange, yellow, green, blue, purple
	AccountID             string            `json:"account_id"`
	AccountName           string            `json:"account_name,omitempty"`
	PayeeID               string            `json:"payee_id,omitempty"`
	PayeeName             string            `json:"payee_name,omitempty"`
	CategoryID            string            `json:"category_id,omitempty"`
	CategoryName          string            `json:"category_name,omitempty"`
	TransferAccountID     string            `json:"transfer_account_id,omitempty"`
	TransferTransactionID string            `json:"transfer_transaction_id,omitempty"`
	MatchedTransactionID  string            `json:"matched_transaction_id,omitempty"`
	ImportID              string            `json:"import_id,omitempty"`
	Deleted               bool              `json:"deleted"`
	Subtransactions       []*SubTransaction `json:"subtransactions,omitempty"`
}

// SubTransaction represents a split transaction.
type SubTransaction struct {
	ID                    string `json:"id"`
	TransactionID         string `json:"transaction_id"`
	Amount                int64  `json:"amount"` // Amount in milliunits
	Memo                  string `json:"memo,omitempty"`
	PayeeID               string `json:"payee_id,omitempty"`
	PayeeName             string `json:"payee_name,omitempty"`
	CategoryID            string `json:"category_id,omitempty"`
	CategoryName          string `json:"category_name,omitempty"`
	TransferAccountID     string `json:"transfer_account_id,omitempty"`
	TransferTransactionID string `json:"transfer_transaction_id,omitempty"`
	Deleted               bool   `json:"deleted"`
}

// Payee represents a transaction payee.
type Payee struct {
	ID                string `json:"id"`
	Name              string `json:"name"`
	TransferAccountID string `json:"transfer_account_id,omitempty"`
	Deleted           bool   `json:"deleted"`
}

// Month represents budget data for a specific month.
type Month struct {
	Month        string      `json:"month"` // ISO date format (YYYY-MM-01)
	Note         string      `json:"note,omitempty"`
	Income       int64       `json:"income"`         // Total income in milliunits
	Budgeted     int64       `json:"budgeted"`       // Total budgeted in milliunits
	Activity     int64       `json:"activity"`       // Total activity in milliunits
	ToBeBudgeted int64       `json:"to_be_budgeted"` // Amount available to budget in milliunits
	AgeOfMoney   int         `json:"age_of_money,omitempty"`
	Deleted      bool        `json:"deleted"`
	Categories   []*Category `json:"categories,omitempty"`
}

// Response wrappers for API endpoints

// BudgetsResponse wraps the budgets list response.
type BudgetsResponse struct {
	Data struct {
		Budgets []*Budget `json:"budgets"`
	} `json:"data"`
}

// BudgetResponse wraps the budget detail response.
type BudgetResponse struct {
	Data struct {
		Budget          *Budget          `json:"budget"`
		ServerKnowledge int64            `json:"server_knowledge"`
		Accounts        []*Account       `json:"accounts,omitempty"`
		CategoryGroups  []*CategoryGroup `json:"category_groups,omitempty"`
		Payees          []*Payee         `json:"payees,omitempty"`
		Transactions    []*Transaction   `json:"transactions,omitempty"`
	} `json:"data"`
}

// CategoriesResponse wraps the categories response.
type CategoriesResponse struct {
	Data struct {
		CategoryGroups []*CategoryGroup `json:"category_groups"`
	} `json:"data"`
}

// CategoryResponse wraps a single category response.
type CategoryResponse struct {
	Data struct {
		Category *Category `json:"category"`
	} `json:"data"`
}

// AccountsResponse wraps the accounts list response.
type AccountsResponse struct {
	Data struct {
		Accounts []*Account `json:"accounts"`
	} `json:"data"`
}

// TransactionResponse wraps the transaction response.
type TransactionResponse struct {
	Data struct {
		Transaction        *Transaction   `json:"transaction"`
		Transactions       []*Transaction `json:"transactions,omitempty"`       // For bulk creates
		DuplicateImportIDs []string       `json:"duplicate_import_ids,omitempty"`
		ServerKnowledge    int64          `json:"server_knowledge"`
	} `json:"data"`
}

// TransactionsResponse wraps the transactions list response.
type TransactionsResponse struct {
	Data struct {
		Transactions    []*Transaction `json:"transactions"`
		ServerKnowledge int64          `json:"server_knowledge"`
	} `json:"data"`
}

// AccountResponse wraps a single account response.
type AccountResponse struct {
	Data struct {
		Account *Account `json:"account"`
	} `json:"data"`
}

// MonthsResponse wraps the months list response.
type MonthsResponse struct {
	Data struct {
		Months          []*Month `json:"months"`
		ServerKnowledge int64    `json:"server_knowledge"`
	} `json:"data"`
}

// MonthResponse wraps a single month response.
type MonthResponse struct {
	Data struct {
		Month *Month `json:"month"`
	} `json:"data"`
}

// ScheduledTransaction represents a scheduled/recurring transaction.
type ScheduledTransaction struct {
	ID              string `json:"id"`
	DateFirst       string `json:"date_first"`
	DateNext        string `json:"date_next"`
	Frequency       string `json:"frequency"` // never, daily, weekly, everyOtherWeek, twiceAMonth, every4Weeks, monthly, everyOtherMonth, every3Months, every4Months, twiceAYear, yearly, everyOtherYear
	Amount          int64  `json:"amount"`
	Memo            string `json:"memo,omitempty"`
	FlagColor       string `json:"flag_color,omitempty"`
	AccountID       string `json:"account_id"`
	AccountName     string `json:"account_name,omitempty"`
	PayeeID         string `json:"payee_id,omitempty"`
	PayeeName       string `json:"payee_name,omitempty"`
	CategoryID      string `json:"category_id,omitempty"`
	CategoryName    string `json:"category_name,omitempty"`
	Deleted         bool   `json:"deleted"`
}

// ScheduledTransactionsResponse wraps the scheduled transactions list response.
type ScheduledTransactionsResponse struct {
	Data struct {
		ScheduledTransactions []*ScheduledTransaction `json:"scheduled_transactions"`
		ServerKnowledge       int64                   `json:"server_knowledge"`
	} `json:"data"`
}

// PayeesResponse wraps the payees list response.
type PayeesResponse struct {
	Data struct {
		Payees          []*Payee `json:"payees"`
		ServerKnowledge int64    `json:"server_knowledge"`
	} `json:"data"`
}

// PayeeResponse wraps a single payee response.
type PayeeResponse struct {
	Data struct {
		Payee *Payee `json:"payee"`
	} `json:"data"`
}
