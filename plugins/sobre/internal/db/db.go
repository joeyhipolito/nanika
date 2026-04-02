// Package db manages the SQLite database for the sobre personal finance plugin.
// It handles connection setup, schema migrations, and provides
// typed query helpers for transaction and account data.
package db

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	_ "modernc.org/sqlite"
)

// CategoryTransfer is the category assigned to inter-account transfer transactions.
const CategoryTransfer = "Transfer"

func parseTime(s string) (time.Time, error) {
	if s == "" {
		return time.Time{}, nil
	}
	return time.Parse("2006-01-02T15:04:05Z", s)
}

// DB wraps a *sql.DB and owns the sobre schema.
type DB struct {
	db *sql.DB
}

// Open opens the SQLite database at path, creating it if it doesn't exist,
// and runs schema migrations.
func Open(path string) (*DB, error) {
	conn, err := sql.Open("sqlite", path)
	if err != nil {
		return nil, fmt.Errorf("opening database %s: %w", path, err)
	}

	// SQLite works best single-writer; set a sane timeout.
	conn.SetMaxOpenConns(1)

	d := &DB{db: conn}
	if err := d.migrate(); err != nil {
		conn.Close()
		return nil, fmt.Errorf("running migrations: %w", err)
	}
	return d, nil
}

// Close releases the database connection.
func (d *DB) Close() error {
	return d.db.Close()
}

// Ping verifies the connection is live.
func (d *DB) Ping(ctx context.Context) error {
	return d.db.PingContext(ctx)
}

// AddAccount inserts a new account and returns its ID.
func (d *DB) AddAccount(ctx context.Context, name, accountType, currency string) (int64, error) {
	result, err := d.db.ExecContext(ctx, `
		INSERT INTO accounts (name, account_type, currency)
		VALUES (?, ?, ?)
	`, name, accountType, currency)
	if err != nil {
		return 0, fmt.Errorf("inserting account: %w", err)
	}
	return result.LastInsertId()
}

// SetAccountBalance sets the balance_cents for an account by ID.
func (d *DB) SetAccountBalance(ctx context.Context, id int64, balanceCents int64) error {
	_, err := d.db.ExecContext(ctx, `
		UPDATE accounts SET balance_cents = ?, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		WHERE id = ?
	`, balanceCents, id)
	if err != nil {
		return fmt.Errorf("setting account balance: %w", err)
	}
	return nil
}

// ListAccounts returns all non-archived accounts.
func (d *DB) ListAccounts(ctx context.Context) ([]Account, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT id, name, account_type, balance_cents, currency, archived, last_updated, created_at
		FROM accounts
		WHERE archived = 0
		ORDER BY created_at
	`)
	if err != nil {
		return nil, fmt.Errorf("querying accounts: %w", err)
	}
	defer rows.Close()

	var accounts []Account
	for rows.Next() {
		var a Account
		var lastUpdated, createdAt string
		if err := rows.Scan(&a.ID, &a.Name, &a.Type, &a.BalanceCents, &a.Currency, &a.Archived, &lastUpdated, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning account: %w", err)
		}
		if t, err := parseTime(lastUpdated); err == nil {
			a.LastUpdated = t
		}
		if t, err := parseTime(createdAt); err == nil {
			a.CreatedAt = t
		}
		accounts = append(accounts, a)
	}
	return accounts, rows.Err()
}

// FindAccount looks up an account by name (exact match).
func (d *DB) FindAccount(ctx context.Context, name string) (*Account, error) {
	var a Account
	var lastUpdated, createdAt string
	err := d.db.QueryRowContext(ctx, `
		SELECT id, name, account_type, balance_cents, currency, archived, last_updated, created_at
		FROM accounts
		WHERE name = ?
	`, name).Scan(&a.ID, &a.Name, &a.Type, &a.BalanceCents, &a.Currency, &a.Archived, &lastUpdated, &createdAt)
	if err == sql.ErrNoRows {
		return nil, fmt.Errorf("account not found: %s", name)
	}
	if err != nil {
		return nil, fmt.Errorf("finding account %s: %w", name, err)
	}
	if t, err := parseTime(lastUpdated); err == nil {
		a.LastUpdated = t
	}
	if t, err := parseTime(createdAt); err == nil {
		a.CreatedAt = t
	}
	return &a, nil
}

// CloseAccount marks an account as archived.
func (d *DB) CloseAccount(ctx context.Context, name string) error {
	result, err := d.db.ExecContext(ctx, `
		UPDATE accounts SET archived = 1, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		WHERE name = ? AND archived = 0
	`, name)
	if err != nil {
		return fmt.Errorf("closing account: %w", err)
	}
	affected, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("checking affected rows: %w", err)
	}
	if affected == 0 {
		return fmt.Errorf("account not found or already closed: %s", name)
	}
	return nil
}

// RenameAccount updates an account's name.
func (d *DB) RenameAccount(ctx context.Context, oldName, newName string) error {
	result, err := d.db.ExecContext(ctx, `
		UPDATE accounts SET name = ? WHERE name = ? AND archived = 0
	`, newName, oldName)
	if err != nil {
		return fmt.Errorf("renaming account: %w", err)
	}
	affected, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("checking affected rows: %w", err)
	}
	if affected == 0 {
		return fmt.Errorf("account not found: %s", oldName)
	}
	return nil
}

// AddCategoryGroup inserts a new category group and returns its ID.
func (d *DB) AddCategoryGroup(ctx context.Context, name, currency string) (int64, error) {
	result, err := d.db.ExecContext(ctx, `
		INSERT INTO category_groups (name, currency)
		VALUES (?, ?)
	`, name, currency)
	if err != nil {
		return 0, fmt.Errorf("inserting category group: %w", err)
	}
	return result.LastInsertId()
}

// ListCategoryGroups returns all category groups.
func (d *DB) ListCategoryGroups(ctx context.Context) ([]CategoryGroup, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT id, name, currency, created_at
		FROM category_groups
		ORDER BY created_at
	`)
	if err != nil {
		return nil, fmt.Errorf("querying category groups: %w", err)
	}
	defer rows.Close()

	var groups []CategoryGroup
	for rows.Next() {
		var g CategoryGroup
		var createdAt string
		if err := rows.Scan(&g.ID, &g.Name, &g.Currency, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning category group: %w", err)
		}
		if t, err := parseTime(createdAt); err == nil {
			g.CreatedAt = t
		}
		groups = append(groups, g)
	}
	return groups, rows.Err()
}

// FindCategoryGroup looks up a category group by name.
func (d *DB) FindCategoryGroup(ctx context.Context, name string) (*CategoryGroup, error) {
	var g CategoryGroup
	var createdAt string
	err := d.db.QueryRowContext(ctx, `
		SELECT id, name, currency, created_at
		FROM category_groups
		WHERE name = ?
	`, name).Scan(&g.ID, &g.Name, &g.Currency, &createdAt)
	if err == sql.ErrNoRows {
		return nil, fmt.Errorf("category group not found: %s", name)
	}
	if err != nil {
		return nil, fmt.Errorf("finding category group %s: %w", name, err)
	}
	if t, err := parseTime(createdAt); err == nil {
		g.CreatedAt = t
	}
	return &g, nil
}

// AddCategory inserts a new category and returns its ID.
func (d *DB) AddCategory(ctx context.Context, name string, groupID int64) (int64, error) {
	result, err := d.db.ExecContext(ctx, `
		INSERT INTO categories (name, group_id)
		VALUES (?, ?)
	`, name, groupID)
	if err != nil {
		return 0, fmt.Errorf("inserting category: %w", err)
	}
	return result.LastInsertId()
}

// ListCategories returns all non-archived categories.
func (d *DB) ListCategories(ctx context.Context) ([]Category, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT id, name, group_id, archived, created_at
		FROM categories
		WHERE archived = 0
		ORDER BY created_at
	`)
	if err != nil {
		return nil, fmt.Errorf("querying categories: %w", err)
	}
	defer rows.Close()

	var categories []Category
	for rows.Next() {
		var c Category
		var createdAt string
		if err := rows.Scan(&c.ID, &c.Name, &c.GroupID, &c.Archived, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning category: %w", err)
		}
		if t, err := parseTime(createdAt); err == nil {
			c.CreatedAt = t
		}
		categories = append(categories, c)
	}
	return categories, rows.Err()
}

// FindCategory looks up a category by name.
func (d *DB) FindCategory(ctx context.Context, name string) (*Category, error) {
	var c Category
	var createdAt string
	err := d.db.QueryRowContext(ctx, `
		SELECT id, name, group_id, archived, created_at
		FROM categories
		WHERE name = ?
	`, name).Scan(&c.ID, &c.Name, &c.GroupID, &c.Archived, &createdAt)
	if err == sql.ErrNoRows {
		return nil, fmt.Errorf("category not found: %s", name)
	}
	if err != nil {
		return nil, fmt.Errorf("finding category %s: %w", name, err)
	}
	if t, err := parseTime(createdAt); err == nil {
		c.CreatedAt = t
	}
	return &c, nil
}

// ArchiveCategory marks a category as archived.
func (d *DB) ArchiveCategory(ctx context.Context, name string) error {
	result, err := d.db.ExecContext(ctx, `
		UPDATE categories SET archived = 1
		WHERE name = ? AND archived = 0
	`, name)
	if err != nil {
		return fmt.Errorf("archiving category: %w", err)
	}
	affected, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("checking affected rows: %w", err)
	}
	if affected == 0 {
		return fmt.Errorf("category not found or already archived: %s", name)
	}
	return nil
}

// SetCategoryTarget updates the target fields on a category by name.
func (d *DB) SetCategoryTarget(ctx context.Context, categoryName, targetType string, targetCents int64, dueDay *int, dueFrequency string) error {
	result, err := d.db.ExecContext(ctx, `
		UPDATE categories
		SET target_type = ?, target_cents = ?, due_day = ?, due_frequency = ?
		WHERE name = ? AND archived = 0
	`, targetType, targetCents, dueDay, dueFrequency, categoryName)
	if err != nil {
		return fmt.Errorf("setting target on %s: %w", categoryName, err)
	}
	affected, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("checking affected rows: %w", err)
	}
	if affected == 0 {
		return fmt.Errorf("category not found or archived: %s", categoryName)
	}
	return nil
}

// GetCategoryWithTarget returns a category including its target info.
func (d *DB) GetCategoryWithTarget(ctx context.Context, name string) (*Category, error) {
	var c Category
	var createdAt string
	var targetType, dueFrequency *string
	var targetCents *int64
	var dueDay *int
	err := d.db.QueryRowContext(ctx, `
		SELECT id, name, group_id, archived,
		       target_type, target_cents, due_day, due_frequency,
		       created_at
		FROM categories
		WHERE name = ?
	`, name).Scan(&c.ID, &c.Name, &c.GroupID, &c.Archived,
		&targetType, &targetCents, &dueDay, &dueFrequency,
		&createdAt)
	if err == sql.ErrNoRows {
		return nil, fmt.Errorf("category not found: %s", name)
	}
	if err != nil {
		return nil, fmt.Errorf("finding category %s: %w", name, err)
	}
	if targetType != nil {
		c.TargetType = *targetType
	}
	if targetCents != nil {
		c.TargetCents = *targetCents
	}
	c.DueDay = dueDay
	if dueFrequency != nil {
		c.DueFrequency = *dueFrequency
	}
	if t, err := parseTime(createdAt); err == nil {
		c.CreatedAt = t
	}
	return &c, nil
}

// AddTransaction inserts a transaction and updates the account balance atomically.
func (d *DB) AddTransaction(ctx context.Context, accountID int64, description string, amountCents int64, category string, txDate time.Time) (int64, error) {
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	result, err := tx.ExecContext(ctx, `
		INSERT INTO transactions (account_id, description, amount_cents, category, transaction_date)
		VALUES (?, ?, ?, ?, ?)
	`, accountID, description, amountCents, category, txDate.Format("2006-01-02T15:04:05Z"))
	if err != nil {
		return 0, fmt.Errorf("inserting transaction: %w", err)
	}
	id, err := result.LastInsertId()
	if err != nil {
		return 0, fmt.Errorf("getting transaction id: %w", err)
	}

	_, err = tx.ExecContext(ctx, `
		UPDATE accounts SET balance_cents = balance_cents + ?, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		WHERE id = ?
	`, amountCents, accountID)
	if err != nil {
		return 0, fmt.Errorf("updating account balance: %w", err)
	}

	if err := tx.Commit(); err != nil {
		return 0, fmt.Errorf("committing transaction: %w", err)
	}
	return id, nil
}

// ListTransactions returns recent transactions with account name, most recent first.
func (d *DB) ListTransactions(ctx context.Context, limit int) ([]TransactionRow, error) {
	if limit <= 0 {
		limit = 25
	}
	rows, err := d.db.QueryContext(ctx, `
		SELECT t.id, t.account_id, a.name, t.description, t.amount_cents, t.category,
		       t.transaction_date, t.posted_date, t.notes, t.created_at
		FROM transactions t
		JOIN accounts a ON t.account_id = a.id
		ORDER BY t.transaction_date DESC, t.id DESC
		LIMIT ?
	`, limit)
	if err != nil {
		return nil, fmt.Errorf("querying transactions: %w", err)
	}
	defer rows.Close()

	var results []TransactionRow
	for rows.Next() {
		var r TransactionRow
		var txDate, postedDate, createdAt string
		var category, notes *string
		if err := rows.Scan(&r.ID, &r.AccountID, &r.AccountName, &r.Description, &r.AmountCents,
			&category, &txDate, &postedDate, &notes, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning transaction: %w", err)
		}
		if category != nil {
			r.Category = *category
		}
		if notes != nil {
			r.Notes = *notes
		}
		if t, err := parseTime(txDate); err == nil {
			r.TransactionDate = t
		}
		if t, err := parseTime(postedDate); err == nil {
			r.PostedDate = t
		}
		if t, err := parseTime(createdAt); err == nil {
			r.CreatedAt = t
		}
		results = append(results, r)
	}
	return results, rows.Err()
}

// GetTransaction retrieves a single transaction by ID.
func (d *DB) GetTransaction(ctx context.Context, id int64) (*Transaction, error) {
	var t Transaction
	var txDate, postedDate, createdAt string
	var category, notes *string
	err := d.db.QueryRowContext(ctx, `
		SELECT id, account_id, description, amount_cents, category, transaction_date, posted_date, notes, created_at
		FROM transactions WHERE id = ?
	`, id).Scan(&t.ID, &t.AccountID, &t.Description, &t.AmountCents, &category, &txDate, &postedDate, &notes, &createdAt)
	if err == sql.ErrNoRows {
		return nil, fmt.Errorf("transaction not found: %d", id)
	}
	if err != nil {
		return nil, fmt.Errorf("fetching transaction %d: %w", id, err)
	}
	if category != nil {
		t.Category = *category
	}
	if notes != nil {
		t.Notes = *notes
	}
	if tm, err := parseTime(txDate); err == nil {
		t.TransactionDate = tm
	}
	if tm, err := parseTime(postedDate); err == nil {
		t.PostedDate = tm
	}
	if tm, err := parseTime(createdAt); err == nil {
		t.CreatedAt = tm
	}
	return &t, nil
}

// UpdateTransaction updates a transaction's fields and adjusts the account balance delta.
func (d *DB) UpdateTransaction(ctx context.Context, id int64, description string, amountCents int64, category string) error {
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	var oldAmount, accountID int64
	err = tx.QueryRowContext(ctx, `SELECT amount_cents, account_id FROM transactions WHERE id = ?`, id).Scan(&oldAmount, &accountID)
	if err == sql.ErrNoRows {
		return fmt.Errorf("transaction not found: %d", id)
	}
	if err != nil {
		return fmt.Errorf("fetching transaction %d: %w", id, err)
	}

	_, err = tx.ExecContext(ctx, `
		UPDATE transactions SET description = ?, amount_cents = ?, category = ? WHERE id = ?
	`, description, amountCents, category, id)
	if err != nil {
		return fmt.Errorf("updating transaction: %w", err)
	}

	delta := amountCents - oldAmount
	if delta != 0 {
		_, err = tx.ExecContext(ctx, `
			UPDATE accounts SET balance_cents = balance_cents + ?, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
			WHERE id = ?
		`, delta, accountID)
		if err != nil {
			return fmt.Errorf("updating account balance: %w", err)
		}
	}

	return tx.Commit()
}

// DeleteTransaction removes a transaction and reverses its effect on the account balance.
func (d *DB) DeleteTransaction(ctx context.Context, id int64) error {
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	var amountCents, accountID int64
	err = tx.QueryRowContext(ctx, `SELECT amount_cents, account_id FROM transactions WHERE id = ?`, id).Scan(&amountCents, &accountID)
	if err == sql.ErrNoRows {
		return fmt.Errorf("transaction not found: %d", id)
	}
	if err != nil {
		return fmt.Errorf("fetching transaction %d: %w", id, err)
	}

	_, err = tx.ExecContext(ctx, `DELETE FROM transactions WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("deleting transaction: %w", err)
	}

	_, err = tx.ExecContext(ctx, `
		UPDATE accounts SET balance_cents = balance_cents - ?, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		WHERE id = ?
	`, amountCents, accountID)
	if err != nil {
		return fmt.Errorf("reversing account balance: %w", err)
	}

	return tx.Commit()
}

// SetBudget upserts a budget limit for the given category and month.
func (d *DB) SetBudget(ctx context.Context, category, month string, limitCents int64) error {
	_, err := d.db.ExecContext(ctx, `
		INSERT INTO budgets (category, month, limit_cents) VALUES (?, ?, ?)
		ON CONFLICT(category, month) DO UPDATE SET limit_cents = excluded.limit_cents
	`, category, month, limitCents)
	if err != nil {
		return fmt.Errorf("upserting budget for %s/%s: %w", category, month, err)
	}
	return nil
}

// budgetRow holds a budget line with its resolved group metadata.
type budgetRow struct {
	category     string
	limit        int64
	currency     string
	groupName    string
	targetType   string
	targetCents  int64
	dueDay       *int
	dueFrequency string
}

// ListBudgetSummaries returns budget rows for a month with spending, carryover,
// currency, group name, and target info computed via JOINs through categories -> category_groups.
func (d *DB) ListBudgetSummaries(ctx context.Context, month string) ([]BudgetSummary, error) {
	bRows, err := d.db.QueryContext(ctx, `
		SELECT b.category, b.limit_cents,
		       COALESCE(cg.currency, ''),
		       COALESCE(cg.name, ''),
		       COALESCE(c.target_type, ''),
		       COALESCE(c.target_cents, 0),
		       c.due_day,
		       COALESCE(c.due_frequency, 'monthly')
		FROM budgets b
		LEFT JOIN categories c ON c.name = b.category
		LEFT JOIN category_groups cg ON cg.id = c.group_id
		WHERE b.month = ?
		ORDER BY cg.currency, cg.id, b.rowid
	`, month)
	if err != nil {
		return nil, fmt.Errorf("querying budgets: %w", err)
	}
	var rows []budgetRow
	seen := map[string]bool{}
	for bRows.Next() {
		var r budgetRow
		if err := bRows.Scan(&r.category, &r.limit, &r.currency, &r.groupName,
			&r.targetType, &r.targetCents, &r.dueDay, &r.dueFrequency); err != nil {
			bRows.Close()
			return nil, fmt.Errorf("scanning budget: %w", err)
		}
		if !seen[r.category] {
			rows = append(rows, r)
			seen[r.category] = true
		}
	}
	if err := bRows.Close(); err != nil {
		return nil, fmt.Errorf("closing budget rows: %w", err)
	}

	spent, err := d.sumSpentByMonth(ctx, month)
	if err != nil {
		return nil, err
	}

	prev := prevMonthStr(month)
	prevBudgets, err := d.sumBudgetsByMonth(ctx, prev)
	if err != nil {
		return nil, err
	}
	prevSpent, err := d.sumSpentByMonth(ctx, prev)
	if err != nil {
		return nil, err
	}

	summaries := make([]BudgetSummary, 0, len(rows))
	for _, r := range rows {
		spentCents := spent[r.category]
		carryover := int64(0)
		if prevLimit, ok := prevBudgets[r.category]; ok && prevLimit > prevSpent[r.category] {
			carryover = prevLimit - prevSpent[r.category]
		}
		summaries = append(summaries, BudgetSummary{
			Category:       r.category,
			Month:          month,
			LimitCents:     r.limit,
			SpentCents:     spentCents,
			CarryoverCents: carryover,
			RemainCents:    r.limit + carryover - spentCents,
			Currency:       r.currency,
			GroupName:      r.groupName,
			TargetType:     r.targetType,
			TargetCents:    r.targetCents,
			DueDay:         r.dueDay,
			DueFrequency:   r.dueFrequency,
		})
	}
	return summaries, nil
}

// ListCategoryTargets returns all non-archived categories that have a target set,
// with their currency and group name resolved.
func (d *DB) ListCategoryTargets(ctx context.Context) ([]BudgetSummary, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT c.name,
		       COALESCE(cg.currency, ''),
		       COALESCE(cg.name, ''),
		       c.target_type, c.target_cents, c.due_day, COALESCE(c.due_frequency, 'monthly')
		FROM categories c
		LEFT JOIN category_groups cg ON cg.id = c.group_id
		WHERE c.archived = 0 AND c.target_type IS NOT NULL
		ORDER BY cg.currency, cg.id, c.name
	`)
	if err != nil {
		return nil, fmt.Errorf("querying category targets: %w", err)
	}
	defer rows.Close()

	var results []BudgetSummary
	for rows.Next() {
		var s BudgetSummary
		if err := rows.Scan(&s.Category, &s.Currency, &s.GroupName,
			&s.TargetType, &s.TargetCents, &s.DueDay, &s.DueFrequency); err != nil {
			return nil, fmt.Errorf("scanning category target: %w", err)
		}
		results = append(results, s)
	}
	return results, rows.Err()
}

// SumAccountBalancesByCurrency returns total non-archived account balances grouped by currency.
func (d *DB) SumAccountBalancesByCurrency(ctx context.Context) (map[string]int64, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT currency, SUM(balance_cents) FROM accounts WHERE archived = 0 GROUP BY currency
	`)
	if err != nil {
		return nil, fmt.Errorf("summing account balances: %w", err)
	}
	defer rows.Close()

	result := map[string]int64{}
	for rows.Next() {
		var currency string
		var total int64
		if err := rows.Scan(&currency, &total); err != nil {
			return nil, fmt.Errorf("scanning balance: %w", err)
		}
		result[currency] = total
	}
	return result, rows.Err()
}

// SumBudgetedByCurrency returns total budgeted cents per currency for a given month.
func (d *DB) SumBudgetedByCurrency(ctx context.Context, month string) (map[string]int64, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT COALESCE(cg.currency, ''), SUM(b.limit_cents)
		FROM budgets b
		LEFT JOIN categories c ON c.name = b.category
		LEFT JOIN category_groups cg ON cg.id = c.group_id
		WHERE b.month = ?
		GROUP BY cg.currency
	`, month)
	if err != nil {
		return nil, fmt.Errorf("summing budgeted by currency for %s: %w", month, err)
	}
	defer rows.Close()

	result := map[string]int64{}
	for rows.Next() {
		var currency string
		var total int64
		if err := rows.Scan(&currency, &total); err != nil {
			return nil, fmt.Errorf("scanning budgeted: %w", err)
		}
		result[currency] = total
	}
	return result, rows.Err()
}

// MoveBudget transfers amountCents from one category's budget to another within a month.
func (d *DB) MoveBudget(ctx context.Context, fromCategory, toCategory, month string, amountCents int64) error {
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return fmt.Errorf("beginning transaction: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	var fromLimit int64
	err = tx.QueryRowContext(ctx, `SELECT limit_cents FROM budgets WHERE category = ? AND month = ?`, fromCategory, month).Scan(&fromLimit)
	if err == sql.ErrNoRows {
		return fmt.Errorf("no budget for %q in %s", fromCategory, month)
	}
	if err != nil {
		return fmt.Errorf("fetching budget for %s: %w", fromCategory, err)
	}
	if fromLimit < amountCents {
		return fmt.Errorf("insufficient budget in %q: have %d cents, need %d cents", fromCategory, fromLimit, amountCents)
	}

	_, err = tx.ExecContext(ctx, `UPDATE budgets SET limit_cents = limit_cents - ? WHERE category = ? AND month = ?`, amountCents, fromCategory, month)
	if err != nil {
		return fmt.Errorf("reducing budget for %s: %w", fromCategory, err)
	}

	_, err = tx.ExecContext(ctx, `
		INSERT INTO budgets (category, month, limit_cents) VALUES (?, ?, ?)
		ON CONFLICT(category, month) DO UPDATE SET limit_cents = limit_cents + ?
	`, toCategory, month, amountCents, amountCents)
	if err != nil {
		return fmt.Errorf("increasing budget for %s: %w", toCategory, err)
	}

	return tx.Commit()
}

// sumSpentByMonth returns the absolute spending per category for negative transactions in a month.
func (d *DB) sumSpentByMonth(ctx context.Context, month string) (map[string]int64, error) {
	start, end, err := monthDateRange(month)
	if err != nil {
		return nil, err
	}
	rows, err := d.db.QueryContext(ctx, `
		SELECT category, -SUM(amount_cents)
		FROM transactions
		WHERE amount_cents < 0 AND transaction_date >= ? AND transaction_date < ?
		GROUP BY category
	`, start, end)
	if err != nil {
		return nil, fmt.Errorf("querying spending for %s: %w", month, err)
	}
	result := map[string]int64{}
	for rows.Next() {
		var cat string
		var s int64
		if err := rows.Scan(&cat, &s); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scanning spending: %w", err)
		}
		result[cat] = s
	}
	if err := rows.Close(); err != nil {
		return nil, fmt.Errorf("closing spending rows: %w", err)
	}
	return result, nil
}

// sumBudgetsByMonth returns budget limits per category for a month.
func (d *DB) sumBudgetsByMonth(ctx context.Context, month string) (map[string]int64, error) {
	rows, err := d.db.QueryContext(ctx, `SELECT category, limit_cents FROM budgets WHERE month = ?`, month)
	if err != nil {
		return nil, fmt.Errorf("querying budgets for %s: %w", month, err)
	}
	result := map[string]int64{}
	for rows.Next() {
		var cat string
		var limit int64
		if err := rows.Scan(&cat, &limit); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scanning budget: %w", err)
		}
		result[cat] = limit
	}
	if err := rows.Close(); err != nil {
		return nil, fmt.Errorf("closing budget rows: %w", err)
	}
	return result, nil
}

func prevMonthStr(month string) string {
	t, err := time.Parse("2006-01", month)
	if err != nil {
		return ""
	}
	return t.AddDate(0, -1, 0).Format("2006-01")
}

// monthDateRange returns the ISO 8601 half-open interval [start, end) for a YYYY-MM month string.
// These strings compare correctly against transaction_date values and allow index use.
func monthDateRange(month string) (start, end string, err error) {
	t, err := time.Parse("2006-01", month)
	if err != nil {
		return "", "", fmt.Errorf("invalid month %q: %w", month, err)
	}
	return t.UTC().Format("2006-01-02T15:04:05Z"),
		t.AddDate(0, 1, 0).UTC().Format("2006-01-02T15:04:05Z"),
		nil
}

// SetExchangeRate upserts the exchange rate for a currency pair.
func (d *DB) SetExchangeRate(ctx context.Context, fromCurrency, toCurrency string, rate float64) error {
	_, err := d.db.ExecContext(ctx, `
		INSERT INTO exchange_rates (from_currency, to_currency, rate)
		VALUES (?, ?, ?)
		ON CONFLICT(from_currency, to_currency) DO UPDATE SET rate = excluded.rate, recorded_at = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
	`, fromCurrency, toCurrency, rate)
	if err != nil {
		return fmt.Errorf("upserting exchange rate %s/%s: %w", fromCurrency, toCurrency, err)
	}
	return nil
}

// ListExchangeRates returns all exchange rates ordered by pair.
func (d *DB) ListExchangeRates(ctx context.Context) ([]ExchangeRate, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT id, from_currency, to_currency, rate, recorded_at
		FROM exchange_rates
		ORDER BY from_currency, to_currency
	`)
	if err != nil {
		return nil, fmt.Errorf("querying exchange rates: %w", err)
	}
	defer rows.Close()

	var rates []ExchangeRate
	for rows.Next() {
		var r ExchangeRate
		var recordedAt string
		if err := rows.Scan(&r.ID, &r.FromCurrency, &r.ToCurrency, &r.Rate, &recordedAt); err != nil {
			return nil, fmt.Errorf("scanning exchange rate: %w", err)
		}
		if t, err := parseTime(recordedAt); err == nil {
			r.RecordedAt = t
		}
		rates = append(rates, r)
	}
	return rates, rows.Err()
}

// Transfer atomically debits fromAccount and credits toAccount.
// fromAmountCents is debited from the source; toAmountCents is credited to the destination.
// For same-currency transfers set both equal. Returns the two transaction IDs.
func (d *DB) Transfer(ctx context.Context, fromAccountID, toAccountID int64, fromAmountCents, toAmountCents int64, description string, txDate time.Time) (fromTxID, toTxID int64, err error) {
	tx, err := d.db.BeginTx(ctx, nil)
	if err != nil {
		return 0, 0, fmt.Errorf("beginning transfer: %w", err)
	}
	defer tx.Rollback() //nolint:errcheck

	dateStr := txDate.Format("2006-01-02T15:04:05Z")

	result, err := tx.ExecContext(ctx, `
		INSERT INTO transactions (account_id, description, amount_cents, category, transaction_date)
		VALUES (?, ?, ?, ?, ?)
	`, fromAccountID, description, -fromAmountCents, CategoryTransfer, dateStr)
	if err != nil {
		return 0, 0, fmt.Errorf("inserting debit transaction: %w", err)
	}
	fromTxID, err = result.LastInsertId()
	if err != nil {
		return 0, 0, fmt.Errorf("getting debit tx id: %w", err)
	}

	_, err = tx.ExecContext(ctx, `
		UPDATE accounts SET balance_cents = balance_cents - ?, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		WHERE id = ?
	`, fromAmountCents, fromAccountID)
	if err != nil {
		return 0, 0, fmt.Errorf("debiting source account: %w", err)
	}

	result, err = tx.ExecContext(ctx, `
		INSERT INTO transactions (account_id, description, amount_cents, category, transaction_date)
		VALUES (?, ?, ?, ?, ?)
	`, toAccountID, description, toAmountCents, CategoryTransfer, dateStr)
	if err != nil {
		return 0, 0, fmt.Errorf("inserting credit transaction: %w", err)
	}
	toTxID, err = result.LastInsertId()
	if err != nil {
		return 0, 0, fmt.Errorf("getting credit tx id: %w", err)
	}

	_, err = tx.ExecContext(ctx, `
		UPDATE accounts SET balance_cents = balance_cents + ?, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
		WHERE id = ?
	`, toAmountCents, toAccountID)
	if err != nil {
		return 0, 0, fmt.Errorf("crediting destination account: %w", err)
	}

	if err := tx.Commit(); err != nil {
		return 0, 0, fmt.Errorf("committing transfer: %w", err)
	}
	return fromTxID, toTxID, nil
}

// AddScheduled inserts a scheduled transaction template and returns its ID.
func (d *DB) AddScheduled(ctx context.Context, accountID int64, description, category, frequency string, amountCents int64, nextDueDate time.Time) (int64, error) {
	result, err := d.db.ExecContext(ctx, `
		INSERT INTO scheduled_transactions (account_id, description, amount_cents, category, frequency, next_due_date)
		VALUES (?, ?, ?, ?, ?, ?)
	`, accountID, description, amountCents, category, frequency, nextDueDate.Format("2006-01-02"))
	if err != nil {
		return 0, fmt.Errorf("inserting scheduled transaction: %w", err)
	}
	return result.LastInsertId()
}

// ListScheduled returns all scheduled transactions with account names.
func (d *DB) ListScheduled(ctx context.Context) ([]ScheduledRow, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT s.id, s.account_id, a.name, s.description, s.amount_cents, s.category, s.frequency, s.next_due_date, s.created_at
		FROM scheduled_transactions s
		JOIN accounts a ON s.account_id = a.id
		ORDER BY s.next_due_date, s.id
	`)
	if err != nil {
		return nil, fmt.Errorf("querying scheduled transactions: %w", err)
	}
	defer rows.Close()

	var results []ScheduledRow
	for rows.Next() {
		var r ScheduledRow
		var createdAt, nextDue string
		var category *string
		if err := rows.Scan(&r.ID, &r.AccountID, &r.AccountName, &r.Description, &r.AmountCents,
			&category, &r.Frequency, &nextDue, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning scheduled transaction: %w", err)
		}
		if category != nil {
			r.Category = *category
		}
		if t, err := time.Parse("2006-01-02", nextDue); err == nil {
			r.NextDueDate = t
		}
		if t, err := parseTime(createdAt); err == nil {
			r.CreatedAt = t
		}
		results = append(results, r)
	}
	return results, rows.Err()
}

// DeleteScheduled removes a scheduled transaction by ID.
func (d *DB) DeleteScheduled(ctx context.Context, id int64) error {
	result, err := d.db.ExecContext(ctx, `DELETE FROM scheduled_transactions WHERE id = ?`, id)
	if err != nil {
		return fmt.Errorf("deleting scheduled transaction %d: %w", id, err)
	}
	affected, err := result.RowsAffected()
	if err != nil {
		return fmt.Errorf("checking affected rows: %w", err)
	}
	if affected == 0 {
		return fmt.Errorf("scheduled transaction not found: %d", id)
	}
	return nil
}

// ProcessScheduled materializes all scheduled transactions due on or before `asOf`.
// Idempotent: each due entry is processed once and next_due_date is advanced.
// Returns the IDs of materialized transactions.
func (d *DB) ProcessScheduled(ctx context.Context, asOf time.Time) ([]int64, error) {
	asOfStr := asOf.Format("2006-01-02")

	rows, err := d.db.QueryContext(ctx, `
		SELECT id, account_id, description, amount_cents, category, frequency, next_due_date
		FROM scheduled_transactions
		WHERE next_due_date <= ?
		ORDER BY next_due_date, id
	`, asOfStr)
	if err != nil {
		return nil, fmt.Errorf("querying due scheduled transactions: %w", err)
	}

	type due struct {
		id          int64
		accountID   int64
		description string
		amountCents int64
		category    string
		frequency   string
		nextDue     time.Time
	}
	var dues []due
	for rows.Next() {
		var d due
		var nextDueStr string
		var category *string
		if err := rows.Scan(&d.id, &d.accountID, &d.description, &d.amountCents, &category, &d.frequency, &nextDueStr); err != nil {
			rows.Close()
			return nil, fmt.Errorf("scanning due scheduled: %w", err)
		}
		if category != nil {
			d.category = *category
		}
		if t, err := time.Parse("2006-01-02", nextDueStr); err == nil {
			d.nextDue = t
		}
		dues = append(dues, d)
	}
	if err := rows.Close(); err != nil {
		return nil, fmt.Errorf("closing scheduled rows: %w", err)
	}

	var created []int64
	for _, entry := range dues {
		tx, err := d.db.BeginTx(ctx, nil)
		if err != nil {
			return created, fmt.Errorf("beginning scheduled materialization: %w", err)
		}

		result, err := tx.ExecContext(ctx, `
			INSERT INTO transactions (account_id, description, amount_cents, category, transaction_date)
			VALUES (?, ?, ?, ?, ?)
		`, entry.accountID, entry.description, entry.amountCents, entry.category, entry.nextDue.Format("2006-01-02T15:04:05Z"))
		if err != nil {
			tx.Rollback() //nolint:errcheck
			return created, fmt.Errorf("inserting materialized transaction: %w", err)
		}
		txID, err := result.LastInsertId()
		if err != nil {
			tx.Rollback() //nolint:errcheck
			return created, fmt.Errorf("getting materialized tx id: %w", err)
		}

		_, err = tx.ExecContext(ctx, `
			UPDATE accounts SET balance_cents = balance_cents + ?, last_updated = strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
			WHERE id = ?
		`, entry.amountCents, entry.accountID)
		if err != nil {
			tx.Rollback() //nolint:errcheck
			return created, fmt.Errorf("updating balance for scheduled tx: %w", err)
		}

		nextDue := advanceDate(entry.nextDue, entry.frequency)
		_, err = tx.ExecContext(ctx, `
			UPDATE scheduled_transactions SET next_due_date = ? WHERE id = ?
		`, nextDue.Format("2006-01-02"), entry.id)
		if err != nil {
			tx.Rollback() //nolint:errcheck
			return created, fmt.Errorf("advancing next_due_date: %w", err)
		}

		if err := tx.Commit(); err != nil {
			return created, fmt.Errorf("committing scheduled materialization: %w", err)
		}
		created = append(created, txID)
	}
	return created, nil
}

// advanceDate returns the next occurrence date for a frequency.
func advanceDate(t time.Time, frequency string) time.Time {
	switch frequency {
	case "daily":
		return t.AddDate(0, 0, 1)
	case "weekly":
		return t.AddDate(0, 0, 7)
	case "yearly":
		return t.AddDate(1, 0, 0)
	default: // "monthly"
		return t.AddDate(0, 1, 0)
	}
}

// IsYNABImported returns true if the YNAB transaction ID has already been imported.
func (d *DB) IsYNABImported(ctx context.Context, ynabTxID string) (bool, error) {
	var count int
	err := d.db.QueryRowContext(ctx, `SELECT COUNT(1) FROM ynab_import_log WHERE ynab_tx_id = ?`, ynabTxID).Scan(&count)
	if err != nil {
		return false, fmt.Errorf("checking ynab import for %s: %w", ynabTxID, err)
	}
	return count > 0, nil
}

// RecordYNABImport records a YNAB transaction as successfully imported.
func (d *DB) RecordYNABImport(ctx context.Context, ynabTxID, budgetID string, sobreTxID int64) error {
	_, err := d.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO ynab_import_log (ynab_tx_id, budget_id, sobre_tx_id)
		VALUES (?, ?, ?)
	`, ynabTxID, budgetID, sobreTxID)
	if err != nil {
		return fmt.Errorf("recording ynab import for %s: %w", ynabTxID, err)
	}
	return nil
}

// FindOrCreateAccount finds an account by name, creating it if it does not exist.
// If the account exists but is archived, it is returned as-is.
func (d *DB) FindOrCreateAccount(ctx context.Context, name, accountType, currency string) (*Account, error) {
	_, err := d.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO accounts (name, account_type, currency)
		VALUES (?, ?, ?)
	`, name, accountType, currency)
	if err != nil {
		return nil, fmt.Errorf("inserting account %s: %w", name, err)
	}

	var a Account
	var lastUpdated, createdAt string
	err = d.db.QueryRowContext(ctx, `
		SELECT id, name, account_type, balance_cents, currency, archived, last_updated, created_at
		FROM accounts WHERE name = ?
	`, name).Scan(&a.ID, &a.Name, &a.Type, &a.BalanceCents, &a.Currency, &a.Archived, &lastUpdated, &createdAt)
	if err != nil {
		return nil, fmt.Errorf("finding account %s after upsert: %w", name, err)
	}
	if t, err := parseTime(lastUpdated); err == nil {
		a.LastUpdated = t
	}
	if t, err := parseTime(createdAt); err == nil {
		a.CreatedAt = t
	}
	return &a, nil
}

// FindOrCreateCategoryGroup finds a category group by name, creating it if it does not exist.
func (d *DB) FindOrCreateCategoryGroup(ctx context.Context, name, currency string) (int64, error) {
	_, err := d.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO category_groups (name, currency) VALUES (?, ?)
	`, name, currency)
	if err != nil {
		return 0, fmt.Errorf("inserting category group %s: %w", name, err)
	}
	var id int64
	if err := d.db.QueryRowContext(ctx, `SELECT id FROM category_groups WHERE name = ?`, name).Scan(&id); err != nil {
		return 0, fmt.Errorf("finding category group %s: %w", name, err)
	}
	return id, nil
}

// FindOrCreateCategory finds a category by name, creating it under groupID if it does not exist.
func (d *DB) FindOrCreateCategory(ctx context.Context, name string, groupID int64) error {
	_, err := d.db.ExecContext(ctx, `
		INSERT OR IGNORE INTO categories (name, group_id) VALUES (?, ?)
	`, name, groupID)
	if err != nil {
		return fmt.Errorf("inserting category %s: %w", name, err)
	}
	return nil
}

// SpendingReport returns total spending per category for [since, until).
func (d *DB) SpendingReport(ctx context.Context, since, until time.Time) ([]CategorySpend, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT COALESCE(category, 'Uncategorized'), -SUM(amount_cents), COUNT(*)
		FROM transactions
		WHERE amount_cents < 0
		  AND (category IS NULL OR category != ?)
		  AND transaction_date >= ? AND transaction_date < ?
		GROUP BY COALESCE(category, 'Uncategorized')
		ORDER BY SUM(amount_cents) ASC
	`, CategoryTransfer, since.Format("2006-01-02T15:04:05Z"), until.Format("2006-01-02T15:04:05Z"))
	if err != nil {
		return nil, fmt.Errorf("querying spending report: %w", err)
	}
	defer rows.Close()

	var results []CategorySpend
	for rows.Next() {
		var cs CategorySpend
		if err := rows.Scan(&cs.Category, &cs.TotalCents, &cs.Count); err != nil {
			return nil, fmt.Errorf("scanning spending row: %w", err)
		}
		results = append(results, cs)
	}
	return results, rows.Err()
}

// IncomeReport returns income (positive) transactions for [since, until).
func (d *DB) IncomeReport(ctx context.Context, since, until time.Time) ([]TransactionRow, error) {
	rows, err := d.db.QueryContext(ctx, `
		SELECT t.id, t.account_id, a.name, t.description, t.amount_cents, t.category,
		       t.transaction_date, t.posted_date, t.notes, t.created_at
		FROM transactions t
		JOIN accounts a ON t.account_id = a.id
		WHERE t.amount_cents > 0
		  AND (t.category IS NULL OR t.category != ?)
		  AND t.transaction_date >= ? AND t.transaction_date < ?
		ORDER BY t.transaction_date DESC, t.id DESC
	`, CategoryTransfer, since.Format("2006-01-02T15:04:05Z"), until.Format("2006-01-02T15:04:05Z"))
	if err != nil {
		return nil, fmt.Errorf("querying income report: %w", err)
	}
	defer rows.Close()

	var results []TransactionRow
	for rows.Next() {
		var r TransactionRow
		var txDate, postedDate, createdAt string
		var category, notes *string
		if err := rows.Scan(&r.ID, &r.AccountID, &r.AccountName, &r.Description, &r.AmountCents,
			&category, &txDate, &postedDate, &notes, &createdAt); err != nil {
			return nil, fmt.Errorf("scanning income row: %w", err)
		}
		if category != nil {
			r.Category = *category
		}
		if notes != nil {
			r.Notes = *notes
		}
		if t, err := parseTime(txDate); err == nil {
			r.TransactionDate = t
		}
		if t, err := parseTime(postedDate); err == nil {
			r.PostedDate = t
		}
		if t, err := parseTime(createdAt); err == nil {
			r.CreatedAt = t
		}
		results = append(results, r)
	}
	return results, rows.Err()
}

// migrate applies the schema and runs any pending ALTER migrations.
func (d *DB) migrate() error {
	if _, err := d.db.Exec(schema); err != nil {
		return fmt.Errorf("applying schema: %w", err)
	}

	// Add target columns to categories if not present.
	// We check via PRAGMA table_info and add any missing columns.
	cols := map[string]bool{}
	rows, err := d.db.Query("PRAGMA table_info(categories)")
	if err != nil {
		return fmt.Errorf("reading categories schema: %w", err)
	}
	for rows.Next() {
		var cid int
		var name, colType string
		var notnull int
		var dfltValue *string
		var pk int
		if err := rows.Scan(&cid, &name, &colType, &notnull, &dfltValue, &pk); err != nil {
			rows.Close()
			return fmt.Errorf("scanning table_info: %w", err)
		}
		cols[name] = true
	}
	rows.Close()

	alters := []struct {
		col string
		ddl string
	}{
		{"target_type", "ALTER TABLE categories ADD COLUMN target_type TEXT CHECK(target_type IN ('set_aside', 'refill')) DEFAULT NULL"},
		{"target_cents", "ALTER TABLE categories ADD COLUMN target_cents INTEGER DEFAULT NULL"},
		{"due_day", "ALTER TABLE categories ADD COLUMN due_day INTEGER DEFAULT NULL"},
		{"due_frequency", "ALTER TABLE categories ADD COLUMN due_frequency TEXT CHECK(due_frequency IN ('monthly', 'weekly')) DEFAULT 'monthly'"},
	}
	for _, a := range alters {
		if !cols[a.col] {
			if _, err := d.db.Exec(a.ddl); err != nil {
				return fmt.Errorf("adding column %s: %w", a.col, err)
			}
		}
	}

	return nil
}

const schema = `
PRAGMA journal_mode = WAL;
PRAGMA foreign_keys = ON;

-- accounts stores financial accounts (checking, savings, credit cards, etc).
CREATE TABLE IF NOT EXISTS accounts (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL UNIQUE,
    account_type  TEXT    NOT NULL,  -- "checking", "savings", "credit_card", "investment"
    balance_cents INTEGER NOT NULL DEFAULT 0,  -- balance in cents to avoid floating point
    currency      TEXT    NOT NULL DEFAULT 'USD',
    archived      BOOLEAN NOT NULL DEFAULT 0,
    last_updated  TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- transactions stores all account transactions.
CREATE TABLE IF NOT EXISTS transactions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    description   TEXT    NOT NULL,
    amount_cents  INTEGER NOT NULL,  -- amount in cents; negative for debits
    category      TEXT,
    transaction_date TEXT NOT NULL,  -- ISO 8601 date
    posted_date   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    notes         TEXT,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_transactions_account_id   ON transactions(account_id);
CREATE INDEX IF NOT EXISTS idx_transactions_category     ON transactions(category);
CREATE INDEX IF NOT EXISTS idx_transactions_date         ON transactions(transaction_date DESC);

-- category_groups stores named groups of categories (e.g., "NZ Living", "Work").
CREATE TABLE IF NOT EXISTS category_groups (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL UNIQUE,
    currency      TEXT    NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

-- categories stores expense/income categories linked to groups.
CREATE TABLE IF NOT EXISTS categories (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    name          TEXT    NOT NULL UNIQUE,
    group_id      INTEGER REFERENCES category_groups(id) ON DELETE CASCADE,
    archived      BOOLEAN NOT NULL DEFAULT 0,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_categories_group_id ON categories(group_id);

-- budgets stores monthly budget allocations.
CREATE TABLE IF NOT EXISTS budgets (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    category      TEXT    NOT NULL,
    month         TEXT    NOT NULL,  -- YYYY-MM format
    limit_cents   INTEGER NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(category, month)
);

CREATE INDEX IF NOT EXISTS idx_budgets_category ON budgets(category);
CREATE INDEX IF NOT EXISTS idx_budgets_month    ON budgets(month);

-- exchange_rates stores the latest known rate between two currencies.
CREATE TABLE IF NOT EXISTS exchange_rates (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    from_currency TEXT    NOT NULL,
    to_currency   TEXT    NOT NULL,
    rate          REAL    NOT NULL,
    recorded_at   TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now')),
    UNIQUE(from_currency, to_currency)
);

-- scheduled_transactions stores recurring transaction templates.
CREATE TABLE IF NOT EXISTS scheduled_transactions (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    description   TEXT    NOT NULL,
    amount_cents  INTEGER NOT NULL,
    category      TEXT,
    frequency     TEXT    NOT NULL,  -- "daily", "weekly", "monthly", "yearly"
    next_due_date TEXT    NOT NULL,  -- YYYY-MM-DD
    created_at    TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_scheduled_next_due ON scheduled_transactions(next_due_date);

-- ynab_import_log tracks YNAB transaction IDs that have been imported to prevent duplicates.
CREATE TABLE IF NOT EXISTS ynab_import_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    ynab_tx_id  TEXT    NOT NULL UNIQUE,
    budget_id   TEXT    NOT NULL,
    sobre_tx_id INTEGER,
    imported_at TEXT    NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_ynab_import_tx_id ON ynab_import_log(ynab_tx_id);
`
