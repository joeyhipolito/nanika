// Package main implements the ynab binary.
package main

import (
	"fmt"
	"math"
	"os"
	"strconv"
	"strings"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/cmd"
	"github.com/joeyhipolito/nanika-ynab/internal/config"
	"github.com/joeyhipolito/nanika-ynab/internal/transform"
)

const version = "3.1.0"

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	// Parse command line arguments
	args := os.Args[1:]

	// Handle help and version flags
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}

	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("ynab version %s\n", version)
		return nil
	}

	// Parse subcommand
	subcommand := args[0]
	remainingArgs := args[1:]

	// Check for global --json flag
	jsonOutput := false
	var filteredArgs []string
	for _, arg := range remainingArgs {
		if arg == "--json" {
			jsonOutput = true
		} else {
			filteredArgs = append(filteredArgs, arg)
		}
	}

	// Commands that don't require authentication
	switch subcommand {
	case "configure":
		if len(filteredArgs) > 0 && filteredArgs[0] == "show" {
			return cmd.ConfigureShowCmd(jsonOutput)
		}
		return cmd.ConfigureCmd()
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	}

	// Load config once; fall back to env vars for token and budget ID.
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	token := cfg.AccessToken
	if token == "" {
		token = os.Getenv("YNAB_ACCESS_TOKEN")
	}
	if token == "" {
		return fmt.Errorf("no access token found\n\nRun 'ynab configure' to set up, or set YNAB_ACCESS_TOKEN")
	}

	// Create API client
	client, err := api.NewClient(token)
	if err != nil {
		return fmt.Errorf("failed to create API client: %w", err)
	}

	// Set default budget ID from config if available
	budgetID := cfg.DefaultBudgetID
	if budgetID == "" {
		budgetID = os.Getenv("YNAB_DEFAULT_BUDGET_ID")
	}
	if budgetID != "" {
		client.SetDefaultBudgetID(budgetID)
	}

	// Dispatch to appropriate command handler
	switch subcommand {
	case "status":
		return cmd.StatusCmd(client, jsonOutput)

	case "balance":
		filter := ""
		if len(filteredArgs) > 0 {
			filter = filteredArgs[0]
		}
		return cmd.BalanceCmd(client, filter, jsonOutput)

	case "budget":
		return cmd.BudgetCmd(client, jsonOutput)

	case "categories":
		return cmd.CategoriesCmd(client, jsonOutput)

	case "add":
		return handleAddCommand(client, filteredArgs, jsonOutput)

	case "transactions":
		return handleTransactionsCommand(client, filteredArgs, jsonOutput)

	case "payees":
		filter := ""
		if len(filteredArgs) > 0 {
			filter = filteredArgs[0]
		}
		return cmd.PayeesCmd(client, filter, jsonOutput)

	case "months":
		monthArg := ""
		if len(filteredArgs) > 0 {
			monthArg = filteredArgs[0]
		}
		return cmd.MonthsCmd(client, monthArg, jsonOutput)

	case "edit":
		return handleEditCommand(client, filteredArgs, jsonOutput)

	case "delete":
		if len(filteredArgs) < 1 {
			return fmt.Errorf("delete requires a transaction ID\n\nUsage: ynab delete <transaction_id>")
		}
		return cmd.DeleteCmd(client, filteredArgs[0], jsonOutput)

	case "move":
		return handleMoveCommand(client, filteredArgs, jsonOutput)

	case "scheduled":
		return cmd.ScheduledCmd(client, jsonOutput)

	case "add-account":
		return handleAddAccountCommand(client, filteredArgs, jsonOutput)

	case "query":
		sub := "status"
		if len(filteredArgs) > 0 {
			sub = filteredArgs[0]
		}
		return cmd.QueryCmd(client, sub, jsonOutput)

	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'ynab --help' for usage", subcommand)
	}
}

// handleAddCommand parses and executes the add command.
func handleAddCommand(client *api.Client, args []string, jsonOutput bool) error {
	for _, arg := range args {
		if arg == "--help" || arg == "-h" {
			fmt.Print(`Usage: ynab add <amount> <payee> [category] [options]

Options:
  --account <name>       Account (default: first on-budget)
  --date <YYYY-MM-DD>    Date (default: today)
  --memo <text>          Transaction memo
  --json                 Output result as JSON
  --help, -h             Show this help

Examples:
  ynab add 50 "Coffee Shop" "Eating Out"
  ynab add +1000 "Paycheck" --account "Checking"
  ynab add 75.50 "Grocery Store" "Groceries" --date 2026-02-01
`)
			return nil
		}
	}

	if len(args) < 2 {
		return fmt.Errorf("add command requires at least amount and payee\n\nUsage: ynab add <amount> <payee> [category] [--account <name>] [--date <YYYY-MM-DD>] [--memo <text>]")
	}

	amount := args[0]
	payee := args[1]
	category := ""
	if len(args) > 2 && !strings.HasPrefix(args[2], "--") {
		category = args[2]
		args = args[3:]
	} else {
		args = args[2:]
	}

	account := ""
	date := ""
	memo := ""

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--account":
			if i+1 >= len(args) {
				return fmt.Errorf("--account requires an argument")
			}
			account = args[i+1]
			i++
		case "--date":
			if i+1 >= len(args) {
				return fmt.Errorf("--date requires an argument")
			}
			date = args[i+1]
			i++
		case "--memo":
			if i+1 >= len(args) {
				return fmt.Errorf("--memo requires an argument")
			}
			memo = args[i+1]
			i++
		default:
			return fmt.Errorf("unknown flag: %s", args[i])
		}
	}

	return cmd.AddCmd(client, amount, payee, category, account, date, memo, jsonOutput)
}

// handleTransactionsCommand parses and executes the transactions command.
func handleTransactionsCommand(client *api.Client, args []string, jsonOutput bool) error {
	sinceDate := ""
	accountFilter := ""
	categoryFilter := ""
	payeeFilter := ""
	limit := 50

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--since":
			if i+1 >= len(args) {
				return fmt.Errorf("--since requires a date (YYYY-MM-DD)")
			}
			sinceDate = args[i+1]
			i++
		case "--account":
			if i+1 >= len(args) {
				return fmt.Errorf("--account requires an argument")
			}
			accountFilter = args[i+1]
			i++
		case "--category":
			if i+1 >= len(args) {
				return fmt.Errorf("--category requires an argument")
			}
			categoryFilter = args[i+1]
			i++
		case "--payee":
			if i+1 >= len(args) {
				return fmt.Errorf("--payee requires an argument")
			}
			payeeFilter = args[i+1]
			i++
		case "--limit":
			if i+1 >= len(args) {
				return fmt.Errorf("--limit requires a number")
			}
			n, err := strconv.Atoi(args[i+1])
			if err != nil {
				return fmt.Errorf("--limit must be a number: %s", args[i+1])
			}
			limit = n
			i++
		default:
			return fmt.Errorf("unknown flag: %s", args[i])
		}
	}

	return cmd.TransactionsCmd(client, sinceDate, accountFilter, categoryFilter, payeeFilter, limit, jsonOutput)
}

// handleEditCommand parses and executes the edit command.
func handleEditCommand(client *api.Client, args []string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("edit requires a transaction ID\n\nUsage: ynab edit <transaction_id> [--amount <amt>] [--payee <name>] [--category <name>] [--memo <text>] [--date <date>] [--cleared]")
	}

	transactionID := args[0]
	args = args[1:]

	var amount *int64
	payee := ""
	category := ""
	memo := ""
	date := ""
	cleared := false

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--amount":
			if i+1 >= len(args) {
				return fmt.Errorf("--amount requires an argument")
			}
			amtStr := args[i+1]
			f, err := strconv.ParseFloat(strings.TrimPrefix(amtStr, "+"), 64)
			if err != nil {
				return fmt.Errorf("invalid amount: %s", amtStr)
			}
			milliunits := transform.DollarsToMilliunits(f)
			if !strings.HasPrefix(amtStr, "+") && milliunits > 0 {
				milliunits = -milliunits
			}
			amount = &milliunits
			i++
		case "--payee":
			if i+1 >= len(args) {
				return fmt.Errorf("--payee requires an argument")
			}
			payee = args[i+1]
			i++
		case "--category":
			if i+1 >= len(args) {
				return fmt.Errorf("--category requires an argument")
			}
			category = args[i+1]
			i++
		case "--memo":
			if i+1 >= len(args) {
				return fmt.Errorf("--memo requires an argument")
			}
			memo = args[i+1]
			i++
		case "--date":
			if i+1 >= len(args) {
				return fmt.Errorf("--date requires an argument")
			}
			date = args[i+1]
			i++
		case "--cleared":
			cleared = true
		default:
			return fmt.Errorf("unknown flag: %s", args[i])
		}
	}

	return cmd.EditCmd(client, transactionID, amount, payee, category, memo, date, cleared, jsonOutput)
}

// handleMoveCommand parses and executes the move command.
func handleMoveCommand(client *api.Client, args []string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("move requires an amount\n\nUsage: ynab move <amount> --from <category> --to <category> [--month <YYYY-MM>]")
	}

	amountStr := args[0]
	f, err := strconv.ParseFloat(amountStr, 64)
	if err != nil {
		return fmt.Errorf("invalid amount: %s", amountStr)
	}
	amountMilliunits := transform.DollarsToMilliunits(f)
	args = args[1:]

	fromCategory := ""
	toCategory := ""
	month := ""

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--from":
			if i+1 >= len(args) {
				return fmt.Errorf("--from requires a category name")
			}
			fromCategory = args[i+1]
			i++
		case "--to":
			if i+1 >= len(args) {
				return fmt.Errorf("--to requires a category name")
			}
			toCategory = args[i+1]
			i++
		case "--month":
			if i+1 >= len(args) {
				return fmt.Errorf("--month requires a month (YYYY-MM)")
			}
			month = args[i+1]
			i++
		default:
			return fmt.Errorf("unknown flag: %s", args[i])
		}
	}

	if fromCategory == "" || toCategory == "" {
		return fmt.Errorf("--from and --to are required\n\nUsage: ynab move <amount> --from <category> --to <category> [--month <YYYY-MM>]")
	}

	return cmd.MoveCmd(client, amountMilliunits, fromCategory, toCategory, month, jsonOutput)
}

// handleAddAccountCommand parses and executes the add-account command.
func handleAddAccountCommand(client *api.Client, args []string, jsonOutput bool) error {
	if len(args) < 2 {
		return fmt.Errorf("add-account requires name and type\n\nUsage: ynab add-account <name> <type> [balance]\n\nTypes: checking, savings, creditCard, cash, lineOfCredit, otherAsset, otherLiability")
	}

	name := args[0]
	accountType := args[1]
	var balance int64

	if len(args) > 2 {
		f, err := strconv.ParseFloat(args[2], 64)
		if err != nil {
			return fmt.Errorf("invalid balance: %s", args[2])
		}
		balance = int64(math.Round(f * 1000))
	}

	return cmd.AddAccountCmd(client, name, accountType, balance, jsonOutput)
}

func printUsage() {
	fmt.Printf(`ynab - YNAB command-line interface (v%s)

USAGE:
    ynab <command> [options]

COMMANDS:
    status                  Show budget status and metadata
    balance [filter]        Show account balances
    budget                  Show current month's budget
    categories              List all categories with IDs
    transactions            List transactions (with filters)
    payees [filter]         List all payees
    months [YYYY-MM]        List months or show month detail
    scheduled               List scheduled/recurring transactions
    add                     Add a new transaction
    edit                    Edit an existing transaction
    delete                  Delete a transaction
    move                    Move money between categories
    add-account             Create a new account
    configure               Set up YNAB access token and default budget
    configure show          Show current configuration
    doctor                  Validate installation and configuration

TRANSACTIONS:
    ynab transactions [options]
        --since <YYYY-MM-DD>    Start date (default: 30 days ago)
        --account <name>        Filter by account
        --category <name>       Filter by category
        --payee <name>          Filter by payee
        --limit <n>             Max results (default: 50)

ADD TRANSACTION:
    ynab add <amount> <payee> [category] [options]
        --account <name>        Account (default: first on-budget)
        --date <YYYY-MM-DD>     Date (default: today)
        --memo <text>           Memo

EDIT TRANSACTION:
    ynab edit <transaction_id> [options]
        --amount <amt>          New amount
        --payee <name>          New payee
        --category <name>       New category
        --memo <text>           New memo
        --date <YYYY-MM-DD>     New date
        --cleared               Mark as cleared

MOVE MONEY:
    ynab move <amount> --from <category> --to <category> [--month <YYYY-MM>]

ADD ACCOUNT:
    ynab add-account <name> <type> [balance]
    Types: checking, savings, creditCard, cash, lineOfCredit, otherAsset, otherLiability

GLOBAL OPTIONS:
    --json              Output in JSON format
    --help, -h          Show this help
    --version, -v       Show version

CONFIGURATION:
    ynab configure              Interactive setup (like 'aws configure')
    ynab configure show         Show current config (token masked)
    ynab doctor                 Validate setup and troubleshoot
    Config file: ~/.ynab/config

EXAMPLES:
    ynab configure                                      # First-time setup
    ynab status                                         # Budget overview
    ynab balance                                        # Account balances
    ynab transactions --since 2025-01-01                # Recent transactions
    ynab transactions --payee "Coffee" --limit 10       # Search by payee
    ynab add 50 "Coffee Shop" "Eating Out"              # Add expense
    ynab add +1000 "Paycheck" --account "Checking"      # Add income
    ynab edit <id> --amount 75 --memo "Updated"         # Edit transaction
    ynab delete <id>                                    # Delete transaction
    ynab move 100 --from "Eating Out" --to "Groceries"  # Move money
    ynab months 2025-01                                 # View month detail
    ynab add-account "Savings" savings 1000             # Create account

For more information, visit: https://api.ynab.com
`, version)
}
