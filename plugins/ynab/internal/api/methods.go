package api

import (
	"bytes"
	"encoding/json"
	"fmt"
	"time"
)

// GetBudgets retrieves all budgets for the authenticated user.
func (c *Client) GetBudgets() ([]*Budget, error) {
	respBody, err := c.request("GET", "/budgets", nil)
	if err != nil {
		return nil, err
	}

	var response BudgetsResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse budgets response: %w", err)
	}

	return response.Data.Budgets, nil
}

// GetBudget retrieves a single budget by ID.
// If lastKnowledge is provided (> 0), it will request a delta update.
func (c *Client) GetBudget(budgetID string, lastKnowledge int64) (*BudgetDetail, error) {
	endpoint := fmt.Sprintf("/budgets/%s", budgetID)
	if lastKnowledge > 0 {
		endpoint += fmt.Sprintf("?last_knowledge_of_server=%d", lastKnowledge)
	}

	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response BudgetResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse budget response: %w", err)
	}

	return &BudgetDetail{
		Budget:          response.Data.Budget,
		ServerKnowledge: response.Data.ServerKnowledge,
		Accounts:        response.Data.Accounts,
		CategoryGroups:  response.Data.CategoryGroups,
		Payees:          response.Data.Payees,
		Transactions:    response.Data.Transactions,
	}, nil
}

// GetCategories retrieves all category groups for a budget.
func (c *Client) GetCategories(budgetID string) ([]*CategoryGroup, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/categories", budgetID)
	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response CategoriesResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse categories response: %w", err)
	}

	return response.Data.CategoryGroups, nil
}

// UpdateCategoryBudget updates the budgeted amount for a category in a specific month.
func (c *Client) UpdateCategoryBudget(categoryID string, budgeted int64, month string, budgetID string) (*Category, error) {
	if categoryID == "" {
		return nil, fmt.Errorf("category_id is required")
	}

	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	// Default to current month if not provided
	if month == "" {
		now := time.Now()
		month = fmt.Sprintf("%04d-%02d-01", now.Year(), now.Month())
	}

	endpoint := fmt.Sprintf("/budgets/%s/months/%s/categories/%s", budgetID, month, categoryID)

	// Prepare request body
	requestBody := map[string]interface{}{
		"category": map[string]interface{}{
			"budgeted": budgeted,
		},
	}
	bodyBytes, err := json.Marshal(requestBody)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request body: %w", err)
	}

	respBody, err := c.request("PATCH", endpoint, bytes.NewReader(bodyBytes))
	if err != nil {
		return nil, err
	}

	var response CategoryResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse category response: %w", err)
	}

	return response.Data.Category, nil
}

// GetAccounts retrieves all accounts for a budget.
func (c *Client) GetAccounts(budgetID string) ([]*Account, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/accounts", budgetID)
	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response AccountsResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse accounts response: %w", err)
	}

	return response.Data.Accounts, nil
}

// CreateTransaction creates a new transaction.
func (c *Client) CreateTransaction(req *TransactionRequest) (*Transaction, error) {
	if err := req.Validate(); err != nil {
		return nil, err
	}

	budgetID := req.BudgetID
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/transactions", budgetID)

	// Build transaction object
	txn := map[string]interface{}{
		"account_id": req.AccountID,
		"date":       req.Date,
		"amount":     req.Amount,
		"cleared":    req.Cleared,
		"approved":   req.Approved,
	}

	if req.PayeeName != "" {
		txn["payee_name"] = req.PayeeName
	}
	if req.CategoryID != "" {
		txn["category_id"] = req.CategoryID
	}
	if req.Memo != "" {
		txn["memo"] = req.Memo
	}

	requestBody := map[string]interface{}{
		"transaction": txn,
	}

	bodyBytes, err := json.Marshal(requestBody)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request body: %w", err)
	}

	respBody, err := c.request("POST", endpoint, bytes.NewReader(bodyBytes))
	if err != nil {
		return nil, err
	}

	var response TransactionResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse transaction response: %w", err)
	}

	return response.Data.Transaction, nil
}

// GetTransactions retrieves transactions for a budget.
// If sinceDate is non-empty, only transactions on or after that date are returned.
func (c *Client) GetTransactions(budgetID string, sinceDate string) ([]*Transaction, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/transactions", budgetID)
	if sinceDate != "" {
		endpoint += fmt.Sprintf("?since_date=%s", sinceDate)
	}

	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response TransactionsResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse transactions response: %w", err)
	}

	return response.Data.Transactions, nil
}

// GetTransactionsByAccount retrieves transactions for a specific account.
func (c *Client) GetTransactionsByAccount(budgetID, accountID, sinceDate string) ([]*Transaction, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/accounts/%s/transactions", budgetID, accountID)
	if sinceDate != "" {
		endpoint += fmt.Sprintf("?since_date=%s", sinceDate)
	}

	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response TransactionsResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse transactions response: %w", err)
	}

	return response.Data.Transactions, nil
}

// GetTransactionsByCategory retrieves transactions for a specific category.
func (c *Client) GetTransactionsByCategory(budgetID, categoryID, sinceDate string) ([]*Transaction, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/categories/%s/transactions", budgetID, categoryID)
	if sinceDate != "" {
		endpoint += fmt.Sprintf("?since_date=%s", sinceDate)
	}

	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response TransactionsResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse transactions response: %w", err)
	}

	return response.Data.Transactions, nil
}

// GetTransaction retrieves a single transaction by ID.
func (c *Client) GetTransaction(budgetID, transactionID string) (*Transaction, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/transactions/%s", budgetID, transactionID)
	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response TransactionResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse transaction response: %w", err)
	}

	return response.Data.Transaction, nil
}

// UpdateTransaction updates an existing transaction.
func (c *Client) UpdateTransaction(budgetID, transactionID string, txn map[string]interface{}) (*Transaction, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/transactions/%s", budgetID, transactionID)

	requestBody := map[string]interface{}{
		"transaction": txn,
	}
	bodyBytes, err := json.Marshal(requestBody)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request body: %w", err)
	}

	respBody, err := c.request("PUT", endpoint, bytes.NewReader(bodyBytes))
	if err != nil {
		return nil, err
	}

	var response TransactionResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse transaction response: %w", err)
	}

	return response.Data.Transaction, nil
}

// DeleteTransaction deletes a transaction by ID.
func (c *Client) DeleteTransaction(budgetID, transactionID string) (*Transaction, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/transactions/%s", budgetID, transactionID)
	respBody, err := c.request("DELETE", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response TransactionResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse transaction response: %w", err)
	}

	return response.Data.Transaction, nil
}

// GetPayees retrieves all payees for a budget.
func (c *Client) GetPayees(budgetID string) ([]*Payee, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/payees", budgetID)
	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response PayeesResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse payees response: %w", err)
	}

	return response.Data.Payees, nil
}

// GetMonths retrieves all budget months.
func (c *Client) GetMonths(budgetID string) ([]*Month, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/months", budgetID)
	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response MonthsResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse months response: %w", err)
	}

	return response.Data.Months, nil
}

// GetMonth retrieves a single budget month with category details.
func (c *Client) GetMonth(budgetID, month string) (*Month, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/months/%s", budgetID, month)
	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response MonthResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse month response: %w", err)
	}

	return response.Data.Month, nil
}

// GetScheduledTransactions retrieves all scheduled transactions for a budget.
func (c *Client) GetScheduledTransactions(budgetID string) ([]*ScheduledTransaction, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/scheduled_transactions", budgetID)
	respBody, err := c.request("GET", endpoint, nil)
	if err != nil {
		return nil, err
	}

	var response ScheduledTransactionsResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse scheduled transactions response: %w", err)
	}

	return response.Data.ScheduledTransactions, nil
}

// CreateAccount creates a new account in a budget.
func (c *Client) CreateAccount(budgetID string, name string, accountType string, balance int64) (*Account, error) {
	if budgetID == "" {
		var err error
		budgetID, err = c.GetDefaultBudgetID()
		if err != nil {
			return nil, err
		}
	}

	endpoint := fmt.Sprintf("/budgets/%s/accounts", budgetID)

	requestBody := map[string]interface{}{
		"account": map[string]interface{}{
			"name":    name,
			"type":    accountType,
			"balance": balance,
		},
	}
	bodyBytes, err := json.Marshal(requestBody)
	if err != nil {
		return nil, fmt.Errorf("failed to marshal request body: %w", err)
	}

	respBody, err := c.request("POST", endpoint, bytes.NewReader(bodyBytes))
	if err != nil {
		return nil, err
	}

	var response AccountResponse
	if err := json.Unmarshal(respBody, &response); err != nil {
		return nil, fmt.Errorf("failed to parse account response: %w", err)
	}

	return response.Data.Account, nil
}

// TransactionRequest represents a request to create a transaction.
type TransactionRequest struct {
	BudgetID   string
	AccountID  string
	Date       string // ISO format: YYYY-MM-DD
	Amount     int64  // Amount in milliunits (negative for outflow)
	PayeeName  string
	CategoryID string
	Memo       string
	Cleared    string // "cleared", "uncleared", "reconciled"
	Approved   bool
}

// Validate validates the transaction request.
func (r *TransactionRequest) Validate() error {
	if r.AccountID == "" {
		return fmt.Errorf("account_id is required")
	}
	if r.Date == "" {
		return fmt.Errorf("date is required")
	}
	if r.Cleared == "" {
		r.Cleared = "uncleared"
	}
	// Approved defaults to true if not set
	return nil
}
