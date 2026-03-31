---
name: ynab
description: YNAB CLI — manage budgets, transactions, categories, and accounts from the terminal via the YNAB API. Use when checking budget status, querying account balances, adding or editing transactions, moving money between categories, or reviewing scheduled transactions.
allowed-tools: Bash(ynab:*)
argument-hint: "[account|category|transaction-id]"
---

# ynab — YNAB CLI

Manage your YNAB (You Need A Budget) budgets, transactions, and accounts from the terminal.

## When to Use

- User wants to check their budget status or category activity
- User wants to see account balances
- User wants to add, edit, or delete a transaction
- User wants to move money between budget categories
- User asks about spending, payees, or scheduled transactions
- User wants to review budget months or list all categories

## Setup

```bash
ynab configure          # interactive setup: enter token, select budget
ynab configure show     # show current config (token masked)
ynab doctor             # validate installation and config
```

Config stored at `~/.ynab/config` (override with `YNAB_CONFIG_DIR`).
Token override: `YNAB_ACCESS_TOKEN`. Budget override: `YNAB_DEFAULT_BUDGET_ID`.

## Commands

### Status & Overview

```bash
ynab status                     # budget overview: name, month, income, budgeted, activity
ynab status --json              # JSON output
```

### Account Balances

```bash
ynab balance                    # all account balances
ynab balance "Checking"         # filter by account name (substring match)
ynab balance --json
```

### Budget

```bash
ynab budget                     # current month category budgets with activity and balance
ynab budget --json
```

### Categories

```bash
ynab categories                 # list all categories with IDs
ynab categories --json
```

### Transactions

```bash
ynab transactions                                        # last 50 transactions (30 days)
ynab transactions --since 2026-01-01                     # since a date
ynab transactions --account "Checking"                   # filter by account
ynab transactions --category "Groceries"                 # filter by category
ynab transactions --payee "Coffee"                       # filter by payee
ynab transactions --limit 100                            # max results (default: 50)
ynab transactions --since 2026-01-01 --payee "Amazon" --json
```

### Add Transaction

```bash
ynab add 50 "Coffee Shop" "Eating Out"                   # expense: amount payee category
ynab add +1000 "Paycheck" --account "Checking"           # income (+ prefix)
ynab add 75.50 "Grocery Store" "Groceries" --date 2026-02-01
ynab add 12.99 "Netflix" "Subscriptions" --memo "Monthly"
ynab add 50 "Coffee Shop" --account "Chase" --json
```

### Edit Transaction

```bash
ynab edit <transaction_id> --amount 75
ynab edit <transaction_id> --payee "New Payee"
ynab edit <transaction_id> --category "Groceries"
ynab edit <transaction_id> --memo "Updated memo"
ynab edit <transaction_id> --date 2026-02-15
ynab edit <transaction_id> --cleared
ynab edit <transaction_id> --amount 50 --memo "Fixed" --json
```

### Delete Transaction

```bash
ynab delete <transaction_id>
ynab delete <transaction_id> --json
```

### Move Money Between Categories

```bash
ynab move 100 --from "Eating Out" --to "Groceries"
ynab move 50 --from "Entertainment" --to "Savings Goals" --month 2026-02
ynab move 200 --from "Miscellaneous" --to "Emergency Fund" --json
```

### Scheduled Transactions

```bash
ynab scheduled                  # list all scheduled/recurring transactions
ynab scheduled --json
```

### Months

```bash
ynab months                     # list all budget months
ynab months 2026-01             # detail for a specific month (categories, income, budgeted)
ynab months --json
```

### Payees

```bash
ynab payees                     # list all payees with IDs
ynab payees "Coffee"            # filter by name (substring match)
ynab payees --json
```

### Add Account

```bash
ynab add-account "Savings" savings 5000
ynab add-account "Visa" creditCard
ynab add-account "Cash Wallet" cash 200
```

Account types: `checking`, `savings`, `creditCard`, `cash`, `lineOfCredit`, `otherAsset`, `otherLiability`

## Global Options

```
--json      Output in JSON format (most commands)
--help, -h  Show help
--version   Show version
```

## Examples

**User**: "how's my budget this month"
**Action**: `ynab status` then `ynab budget`

**User**: "what are my account balances"
**Action**: `ynab balance --json`

**User**: "I spent $45 at the grocery store"
**Action**: `ynab add 45 "Grocery Store" "Groceries"`

**User**: "move $100 from eating out to groceries"
**Action**: `ynab move 100 --from "Eating Out" --to "Groceries"`

**User**: "show my Amazon transactions this year"
**Action**: `ynab transactions --since 2026-01-01 --payee "Amazon"`

## Build

```bash
cd plugins/ynab
go build -ldflags "-s -w" -o bin/ynab ./cmd/ynab-cli
ln -sf $(pwd)/bin/ynab ~/bin/ynab
```
