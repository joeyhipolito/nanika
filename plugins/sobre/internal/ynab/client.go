// Package ynab provides a minimal YNAB API client for the sobre import command.
// It reads credentials from the existing ~/.ynab/config file written by the ynab CLI.
package ynab

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"
	"time"
)

const baseURL = "https://api.youneedabudget.com/v1"

// Budget represents a YNAB budget.
type Budget struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// Account represents a YNAB account.
type Account struct {
	ID      string `json:"id"`
	Name    string `json:"name"`
	Type    string `json:"type"` // checking, savings, creditCard, cash, lineOfCredit, otherAsset, otherLiability
	Closed  bool   `json:"closed"`
	Deleted bool   `json:"deleted"`
}

// Transaction represents a YNAB transaction.
type Transaction struct {
	ID                string `json:"id"`
	Date              string `json:"date"`   // YYYY-MM-DD
	Amount            int64  `json:"amount"` // milliunits (1000 = $1.00)
	Memo              string `json:"memo,omitempty"`
	Cleared           string `json:"cleared"`
	Approved          bool   `json:"approved"`
	AccountID         string `json:"account_id"`
	AccountName       string `json:"account_name,omitempty"`
	PayeeName         string `json:"payee_name,omitempty"`
	CategoryName      string `json:"category_name,omitempty"`
	TransferAccountID string `json:"transfer_account_id,omitempty"`
	Deleted           bool   `json:"deleted"`
}

// MilliunitsToCents converts YNAB milliunits to cents.
// YNAB stores amounts as milliunits: 1 dollar = 1000 milliunits = 100 cents.
// So cents = milliunits / 10.
func MilliunitsToCents(milliunits int64) int64 {
	return milliunits / 10
}

// MapAccountType converts a YNAB account type to a sobre account type.
func MapAccountType(ynabType string) string {
	switch ynabType {
	case "creditCard", "lineOfCredit":
		return "credit_card"
	case "investment", "otherAsset", "otherLiability":
		return "investment"
	default:
		// checking, savings, cash → pass through (sobre accepts these)
		return ynabType
	}
}

// Client is a minimal YNAB API client.
type Client struct {
	token string
	http  *http.Client
}

// NewClientFromConfig creates a Client by reading credentials from ~/.ynab/config.
// Falls back to the YNAB_ACCESS_TOKEN environment variable.
func NewClientFromConfig() (*Client, error) {
	token := resolveToken()
	if token == "" {
		return nil, fmt.Errorf("YNAB access token not found; run 'ynab configure' or set YNAB_ACCESS_TOKEN")
	}
	return &Client{
		token: token,
		http:  &http.Client{Timeout: 30 * time.Second},
	}, nil
}

func resolveToken() string {
	if tok := os.Getenv("YNAB_ACCESS_TOKEN"); tok != "" {
		return tok
	}
	cfgDir := os.Getenv("YNAB_CONFIG_DIR")
	if cfgDir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return ""
		}
		cfgDir = filepath.Join(home, ".ynab")
	}
	f, err := os.Open(filepath.Join(cfgDir, "config"))
	if err != nil {
		return ""
	}
	defer f.Close()

	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if strings.HasPrefix(line, "access_token=") {
			return strings.TrimPrefix(line, "access_token=")
		}
	}
	return ""
}

func (c *Client) get(endpoint string, out any) error {
	req, err := http.NewRequest("GET", baseURL+endpoint, nil)
	if err != nil {
		return fmt.Errorf("creating request for %s: %w", endpoint, err)
	}
	req.Header.Set("Authorization", "Bearer "+c.token)
	req.Header.Set("User-Agent", "sobre/0.1.0")

	resp, err := c.http.Do(req)
	if err != nil {
		return fmt.Errorf("GET %s: %w", endpoint, err)
	}
	defer resp.Body.Close()

	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("reading response from %s: %w", endpoint, err)
	}
	if resp.StatusCode == http.StatusUnauthorized {
		return fmt.Errorf("YNAB API unauthorized: check token in ~/.ynab/config")
	}
	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("YNAB API %s returned %d: %s", endpoint, resp.StatusCode, string(body))
	}
	return json.Unmarshal(body, out)
}

// GetBudgets returns all budgets for the authenticated user.
func (c *Client) GetBudgets() ([]Budget, error) {
	var resp struct {
		Data struct {
			Budgets []Budget `json:"budgets"`
		} `json:"data"`
	}
	if err := c.get("/budgets", &resp); err != nil {
		return nil, fmt.Errorf("fetching budgets: %w", err)
	}
	return resp.Data.Budgets, nil
}

// FindBudget returns the first budget whose name contains the given substring (case-insensitive).
func (c *Client) FindBudget(nameFilter string) (*Budget, error) {
	budgets, err := c.GetBudgets()
	if err != nil {
		return nil, err
	}
	lower := strings.ToLower(nameFilter)
	// Exact match first
	for i, b := range budgets {
		if strings.EqualFold(b.Name, nameFilter) {
			return &budgets[i], nil
		}
	}
	// Partial match
	for i, b := range budgets {
		if strings.Contains(strings.ToLower(b.Name), lower) {
			return &budgets[i], nil
		}
	}
	names := make([]string, len(budgets))
	for i, b := range budgets {
		names[i] = b.Name
	}
	return nil, fmt.Errorf("no budget matching %q found (available: %s)", nameFilter, strings.Join(names, ", "))
}

// GetAccounts returns all non-deleted, non-closed accounts for a budget.
func (c *Client) GetAccounts(budgetID string) ([]Account, error) {
	var resp struct {
		Data struct {
			Accounts []Account `json:"accounts"`
		} `json:"data"`
	}
	if err := c.get("/budgets/"+budgetID+"/accounts", &resp); err != nil {
		return nil, fmt.Errorf("fetching accounts for budget %s: %w", budgetID, err)
	}
	var active []Account
	for _, a := range resp.Data.Accounts {
		if !a.Deleted && !a.Closed {
			active = append(active, a)
		}
	}
	return active, nil
}

// GetTransactions returns all non-deleted transactions for a budget, optionally since a date.
// since should be in YYYY-MM-DD format or empty for all time.
func (c *Client) GetTransactions(budgetID, since string) ([]Transaction, error) {
	endpoint := "/budgets/" + budgetID + "/transactions"
	if since != "" {
		endpoint += "?since_date=" + since
	}

	var resp struct {
		Data struct {
			Transactions []Transaction `json:"transactions"`
		} `json:"data"`
	}
	if err := c.get(endpoint, &resp); err != nil {
		return nil, fmt.Errorf("fetching transactions for budget %s: %w", budgetID, err)
	}

	var active []Transaction
	for _, t := range resp.Data.Transactions {
		if !t.Deleted {
			active = append(active, t)
		}
	}
	return active, nil
}
