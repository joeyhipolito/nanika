// Package main implements the gmail binary.
package main

import (
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/cmd"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

const version = "0.3.0"

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	args := os.Args[1:]

	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}

	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("gmail version %s\n", version)
		return nil
	}

	subcommand := args[0]
	remainingArgs := args[1:]

	// Extract global flags
	jsonOutput := false
	account := ""
	var filteredArgs []string
	for i := 0; i < len(remainingArgs); i++ {
		switch remainingArgs[i] {
		case "--json":
			jsonOutput = true
		case "--account":
			if i+1 >= len(remainingArgs) {
				return fmt.Errorf("--account requires an argument")
			}
			account = remainingArgs[i+1]
			i++
		default:
			filteredArgs = append(filteredArgs, remainingArgs[i])
		}
	}

	// Commands that don't require authentication
	switch subcommand {
	case "configure":
		if len(filteredArgs) > 0 && filteredArgs[0] == "show" {
			return cmd.ConfigureShowCmd(jsonOutput)
		}
		alias := ""
		if len(filteredArgs) > 0 {
			alias = filteredArgs[0]
		}
		return cmd.ConfigureCmd(alias)
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	case "accounts":
		if len(filteredArgs) > 0 && filteredArgs[0] == "remove" {
			if len(filteredArgs) < 2 {
				return fmt.Errorf("accounts remove requires an alias\n\nUsage: gmail accounts remove <alias>")
			}
			return cmd.AccountsRemoveCmd(filteredArgs[1])
		}
		return cmd.AccountsCmd(jsonOutput)
	}

	// Load config for authenticated commands
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}
	if cfg.ClientID == "" || cfg.ClientSecret == "" {
		return fmt.Errorf("no OAuth2 credentials found\n\nRun 'gmail configure <alias>' to set up")
	}

	switch subcommand {
	case "inbox":
		limit := 25
		unreadOnly := false
		for i := 0; i < len(filteredArgs); i++ {
			switch filteredArgs[i] {
			case "--limit":
				if i+1 >= len(filteredArgs) {
					return fmt.Errorf("--limit requires a number")
				}
				n, err := strconv.Atoi(filteredArgs[i+1])
				if err != nil {
					return fmt.Errorf("--limit must be a number: %s", filteredArgs[i+1])
				}
				limit = n
				i++
			case "--unread":
				unreadOnly = true
			default:
				return fmt.Errorf("unknown flag: %s", filteredArgs[i])
			}
		}
		return cmd.InboxCmd(cfg, account, limit, unreadOnly, jsonOutput)

	case "thread":
		if len(filteredArgs) < 1 {
			return fmt.Errorf("thread requires a thread ID\n\nUsage: gmail thread <thread-id> --account <alias>")
		}
		threadID := filteredArgs[0]
		if account == "" {
			return fmt.Errorf("--account is required for thread reading\n\nUsage: gmail thread <thread-id> --account <alias>")
		}
		return cmd.ThreadCmd(cfg, threadID, account, jsonOutput)

	case "search":
		if len(filteredArgs) < 1 {
			return fmt.Errorf("search requires a query\n\nUsage: gmail search \"<query>\" [--limit N] [--account <alias>]")
		}
		query := filteredArgs[0]
		limit := 25
		for i := 1; i < len(filteredArgs); i++ {
			switch filteredArgs[i] {
			case "--limit":
				if i+1 >= len(filteredArgs) {
					return fmt.Errorf("--limit requires a number")
				}
				n, err := strconv.Atoi(filteredArgs[i+1])
				if err != nil {
					return fmt.Errorf("--limit must be a number: %s", filteredArgs[i+1])
				}
				limit = n
				i++
			default:
				return fmt.Errorf("unknown flag: %s", filteredArgs[i])
			}
		}
		return cmd.SearchCmd(cfg, query, account, limit, jsonOutput)

	case "labels":
		return cmd.LabelsCmd(cfg, account, jsonOutput)

	case "label":
		return handleLabelCommand(cfg, filteredArgs, account, jsonOutput)

	case "filters":
		return cmd.FiltersCmd(cfg, account, jsonOutput)

	case "filter":
		return handleFilterCommand(cfg, filteredArgs, account, jsonOutput)

	case "mark":
		return handleMarkCommand(cfg, filteredArgs, account)

	case "send":
		return handleSendCommand(cfg, filteredArgs, account, jsonOutput)

	case "reply":
		return handleReplyCommand(cfg, filteredArgs, account, jsonOutput)

	case "draft":
		return handleDraftCommand(cfg, filteredArgs, account, jsonOutput)

	case "calendar":
		return handleCalendarCommand(cfg, filteredArgs, account, jsonOutput)

	case "drive":
		return handleDriveCommand(cfg, filteredArgs, account, jsonOutput)

	case "query":
		sub := "status"
		if len(filteredArgs) > 0 {
			sub = filteredArgs[0]
		}
		return cmd.QueryCmd(cfg, account, sub, jsonOutput)

	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'gmail --help' for usage", subcommand)
	}
}

func handleLabelCommand(cfg *config.Config, args []string, account string, jsonOutput bool) error {
	// gmail label --create "<name>" --account <alias>
	for i, arg := range args {
		if arg == "--create" {
			if i+1 >= len(args) {
				return fmt.Errorf("--create requires a label name")
			}
			if account == "" {
				return fmt.Errorf("--account is required for label creation")
			}
			return cmd.LabelCreateCmd(cfg, args[i+1], account, jsonOutput)
		}
	}

	// gmail label <thread-id> --remove <label-name> --account <alias>
	// gmail label <thread-id> <label-name> --account <alias>
	if len(args) < 1 {
		return fmt.Errorf("label requires a thread ID\n\nUsage: gmail label <thread-id> <label-name> --account <alias>")
	}
	if account == "" {
		return fmt.Errorf("--account is required for label operations")
	}

	threadID := args[0]
	remaining := args[1:]

	for i, arg := range remaining {
		if arg == "--remove" {
			if i+1 >= len(remaining) {
				return fmt.Errorf("--remove requires a label name")
			}
			return cmd.LabelRemoveCmd(cfg, threadID, remaining[i+1], account)
		}
	}

	if len(remaining) < 1 {
		return fmt.Errorf("label requires a label name\n\nUsage: gmail label <thread-id> <label-name> --account <alias>")
	}

	return cmd.LabelApplyCmd(cfg, threadID, remaining[0], account)
}

func handleFilterCommand(cfg *config.Config, args []string, account string, jsonOutput bool) error {
	if account == "" {
		return fmt.Errorf("--account is required for filter operations")
	}

	// Check for --delete <id>
	for i, arg := range args {
		if arg == "--delete" {
			if i+1 >= len(args) {
				return fmt.Errorf("--delete requires a filter ID")
			}
			return cmd.FilterDeleteCmd(cfg, args[i+1], account)
		}
	}

	// Check for --create with criteria/action flags
	isCreate := false
	for _, arg := range args {
		if arg == "--create" {
			isCreate = true
			break
		}
	}
	if !isCreate {
		return fmt.Errorf("filter requires --create or --delete\n\nUsage:\n  gmail filter --create --from \"sender@example.com\" --label \"Dev\" --account <alias>\n  gmail filter --delete <filter-id> --account <alias>")
	}

	var criteria api.FilterCriteria
	var action api.FilterAction

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--create":
			// already handled
		case "--from":
			if i+1 >= len(args) {
				return fmt.Errorf("--from requires a value")
			}
			i++
			criteria.From = args[i]
		case "--to":
			if i+1 >= len(args) {
				return fmt.Errorf("--to requires a value")
			}
			i++
			criteria.To = args[i]
		case "--subject":
			if i+1 >= len(args) {
				return fmt.Errorf("--subject requires a value")
			}
			i++
			criteria.Subject = args[i]
		case "--query":
			if i+1 >= len(args) {
				return fmt.Errorf("--query requires a value")
			}
			i++
			criteria.Query = args[i]
		case "--has-attachment":
			criteria.HasAttachment = true
		case "--label":
			if i+1 >= len(args) {
				return fmt.Errorf("--label requires a value")
			}
			i++
			action.AddLabelIDs = append(action.AddLabelIDs, args[i])
		case "--archive":
			action.Archive = true
		case "--star":
			action.Star = true
		case "--mark-read":
			action.MarkRead = true
		case "--important":
			action.MarkImportant = true
		case "--trash":
			action.Trash = true
		case "--never-spam":
			action.NeverSpam = true
		case "--forward":
			if i+1 >= len(args) {
				return fmt.Errorf("--forward requires a value")
			}
			i++
			action.Forward = args[i]
		default:
			return fmt.Errorf("unknown flag: %s", args[i])
		}
	}

	// Validate at least one criterion is set.
	if criteria.From == "" && criteria.To == "" && criteria.Subject == "" &&
		criteria.Query == "" && !criteria.HasAttachment {
		return fmt.Errorf("filter --create requires at least one criterion (--from, --to, --subject, --query, --has-attachment)")
	}

	return cmd.FilterCreateCmd(cfg, criteria, action, account, jsonOutput)
}

func handleMarkCommand(cfg *config.Config, args []string, account string) error {
	if len(args) < 1 {
		return fmt.Errorf("mark requires a thread ID\n\nUsage: gmail mark <thread-id> --read|--unread|--archive|--trash --account <alias>")
	}
	if account == "" {
		return fmt.Errorf("--account is required for mark operations")
	}

	threadID := args[0]
	markRead := false
	markUnread := false
	archive := false
	trash := false

	for _, arg := range args[1:] {
		switch arg {
		case "--read":
			markRead = true
		case "--unread":
			markUnread = true
		case "--archive":
			archive = true
		case "--trash":
			trash = true
		default:
			return fmt.Errorf("unknown flag: %s", arg)
		}
	}

	if !markRead && !markUnread && !archive && !trash {
		return fmt.Errorf("mark requires at least one action: --read, --unread, --archive, or --trash")
	}

	return cmd.MarkCmd(cfg, threadID, account, markRead, markUnread, archive, trash)
}

func handleSendCommand(cfg *config.Config, args []string, account string, jsonOutput bool) error {
	var p api.ComposeParams
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--to":
			if i+1 >= len(args) {
				return fmt.Errorf("--to requires an address")
			}
			i++
			p.To = args[i]
		case "--subject":
			if i+1 >= len(args) {
				return fmt.Errorf("--subject requires a value")
			}
			i++
			p.Subject = args[i]
		case "--cc":
			if i+1 >= len(args) {
				return fmt.Errorf("--cc requires a value")
			}
			i++
			p.CC = args[i]
		case "--bcc":
			if i+1 >= len(args) {
				return fmt.Errorf("--bcc requires a value")
			}
			i++
			p.BCC = args[i]
		case "--html":
			if i+1 >= len(args) {
				return fmt.Errorf("--html requires a value")
			}
			i++
			p.HTML = args[i]
		default:
			if len(args[i]) > 0 && args[i][0] != '-' {
				p.Body = args[i]
			} else {
				return fmt.Errorf("unknown flag: %s", args[i])
			}
		}
	}
	if p.To == "" {
		return fmt.Errorf("--to is required\n\nUsage: gmail send --to <addr> --subject <subj> \"body\" --account <alias>")
	}
	return cmd.SendCmd(cfg, p, account, jsonOutput)
}

func handleReplyCommand(cfg *config.Config, args []string, account string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("reply requires a thread ID\n\nUsage: gmail reply <thread-id> \"body\" --account <alias>")
	}
	threadID := args[0]
	if len(args) < 2 {
		return fmt.Errorf("reply requires a message body\n\nUsage: gmail reply <thread-id> \"body\" --account <alias>")
	}
	body := args[1]
	return cmd.ReplyCmd(cfg, threadID, body, account, jsonOutput)
}

func handleDraftCommand(cfg *config.Config, args []string, account string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("draft requires a subcommand: create, list, send\n\nUsage:\n  gmail draft create --to <addr> --subject <subj> \"body\" --account <alias>\n  gmail draft list [--account <alias>]\n  gmail draft send <draft-id> --account <alias>")
	}

	sub := args[0]
	remaining := args[1:]

	switch sub {
	case "create":
		var p api.ComposeParams
		for i := 0; i < len(remaining); i++ {
			switch remaining[i] {
			case "--to":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--to requires an address")
				}
				i++
				p.To = remaining[i]
			case "--subject":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--subject requires a value")
				}
				i++
				p.Subject = remaining[i]
			case "--cc":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--cc requires a value")
				}
				i++
				p.CC = remaining[i]
			case "--bcc":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--bcc requires a value")
				}
				i++
				p.BCC = remaining[i]
			case "--html":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--html requires a value")
				}
				i++
				p.HTML = remaining[i]
			default:
				if len(remaining[i]) > 0 && remaining[i][0] != '-' {
					p.Body = remaining[i]
				} else {
					return fmt.Errorf("unknown flag: %s", remaining[i])
				}
			}
		}
		if p.To == "" {
			return fmt.Errorf("--to is required for draft create")
		}
		return cmd.DraftCreateCmd(cfg, p, account, jsonOutput)

	case "list":
		return cmd.DraftListCmd(cfg, account, jsonOutput)

	case "send":
		if len(remaining) < 1 {
			return fmt.Errorf("draft send requires a draft ID\n\nUsage: gmail draft send <draft-id> --account <alias>")
		}
		return cmd.DraftSendCmd(cfg, remaining[0], account, jsonOutput)

	default:
		return fmt.Errorf("unknown draft subcommand: %s\n\nUsage: gmail draft create|list|send", sub)
	}
}

func handleCalendarCommand(cfg *config.Config, args []string, account string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("calendar requires a subcommand: list, create, available\n\nUsage:\n  gmail calendar list [--limit N] --account <alias>\n  gmail calendar create --summary <title> --start <RFC3339> --end <RFC3339> --account <alias>\n  gmail calendar available --start <RFC3339> --end <RFC3339> --account <alias>")
	}
	if account == "" {
		return fmt.Errorf("--account is required for calendar commands\n\nUsage: gmail calendar <subcommand> --account <alias>")
	}

	sub := args[0]
	remaining := args[1:]

	switch sub {
	case "list":
		limit := 10
		for i := 0; i < len(remaining); i++ {
			switch remaining[i] {
			case "--limit":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--limit requires a number")
				}
				n, err := strconv.Atoi(remaining[i+1])
				if err != nil {
					return fmt.Errorf("--limit must be a number: %s", remaining[i+1])
				}
				limit = n
				i++
			default:
				return fmt.Errorf("unknown flag: %s", remaining[i])
			}
		}
		return cmd.CalendarListCmd(cfg, account, limit, jsonOutput)

	case "create":
		var params api.CreateEventParams
		for i := 0; i < len(remaining); i++ {
			switch remaining[i] {
			case "--summary":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--summary requires a value")
				}
				i++
				params.Summary = remaining[i]
			case "--description":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--description requires a value")
				}
				i++
				params.Description = remaining[i]
			case "--location":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--location requires a value")
				}
				i++
				params.Location = remaining[i]
			case "--start":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--start requires an RFC3339 time")
				}
				i++
				params.Start = remaining[i]
			case "--end":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--end requires an RFC3339 time")
				}
				i++
				params.End = remaining[i]
			case "--timezone":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--timezone requires a value")
				}
				i++
				params.TimeZone = remaining[i]
			case "--attendee":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--attendee requires an email address")
				}
				i++
				params.Attendees = append(params.Attendees, remaining[i])
			default:
				return fmt.Errorf("unknown flag: %s", remaining[i])
			}
		}
		return cmd.CalendarCreateCmd(cfg, params, account, jsonOutput)

	case "available":
		var startStr, endStr string
		for i := 0; i < len(remaining); i++ {
			switch remaining[i] {
			case "--start":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--start requires an RFC3339 time")
				}
				i++
				startStr = remaining[i]
			case "--end":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--end requires an RFC3339 time")
				}
				i++
				endStr = remaining[i]
			default:
				return fmt.Errorf("unknown flag: %s", remaining[i])
			}
		}
		if startStr == "" || endStr == "" {
			return fmt.Errorf("--start and --end are required\n\nUsage: gmail calendar available --start <RFC3339> --end <RFC3339> --account <alias>")
		}
		start, err := time.Parse(time.RFC3339, startStr)
		if err != nil {
			return fmt.Errorf("--start must be RFC3339 (e.g. 2026-03-19T09:00:00Z): %w", err)
		}
		end, err := time.Parse(time.RFC3339, endStr)
		if err != nil {
			return fmt.Errorf("--end must be RFC3339 (e.g. 2026-03-19T10:00:00Z): %w", err)
		}
		return cmd.CalendarAvailableCmd(cfg, account, start, end, jsonOutput)

	default:
		return fmt.Errorf("unknown calendar subcommand: %s\n\nUsage: gmail calendar list|create|available", sub)
	}
}

func handleDriveCommand(cfg *config.Config, args []string, account string, jsonOutput bool) error {
	if len(args) < 1 {
		return fmt.Errorf("drive requires a subcommand: list, search, download\n\nUsage:\n  gmail drive list [--limit N] --account <alias>\n  gmail drive search \"<query>\" [--limit N] --account <alias>\n  gmail drive download <file-id> [--output <path>] --account <alias>")
	}
	if account == "" {
		return fmt.Errorf("--account is required for drive commands\n\nUsage: gmail drive <subcommand> --account <alias>")
	}

	sub := args[0]
	remaining := args[1:]

	switch sub {
	case "list":
		limit := 10
		for i := 0; i < len(remaining); i++ {
			switch remaining[i] {
			case "--limit":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--limit requires a number")
				}
				n, err := strconv.Atoi(remaining[i+1])
				if err != nil {
					return fmt.Errorf("--limit must be a number: %s", remaining[i+1])
				}
				limit = n
				i++
			default:
				return fmt.Errorf("unknown flag: %s", remaining[i])
			}
		}
		return cmd.DriveListCmd(cfg, account, limit, jsonOutput)

	case "search":
		if len(remaining) < 1 {
			return fmt.Errorf("drive search requires a query\n\nUsage: gmail drive search \"<query>\" [--limit N] --account <alias>")
		}
		query := remaining[0]
		limit := 10
		for i := 1; i < len(remaining); i++ {
			switch remaining[i] {
			case "--limit":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--limit requires a number")
				}
				n, err := strconv.Atoi(remaining[i+1])
				if err != nil {
					return fmt.Errorf("--limit must be a number: %s", remaining[i+1])
				}
				limit = n
				i++
			default:
				return fmt.Errorf("unknown flag: %s", remaining[i])
			}
		}
		return cmd.DriveSearchCmd(cfg, account, query, limit, jsonOutput)

	case "download":
		if len(remaining) < 1 {
			return fmt.Errorf("drive download requires a file ID\n\nUsage: gmail drive download <file-id> [--output <path>] --account <alias>")
		}
		fileID := remaining[0]
		outputPath := ""
		for i := 1; i < len(remaining); i++ {
			switch remaining[i] {
			case "--output":
				if i+1 >= len(remaining) {
					return fmt.Errorf("--output requires a path")
				}
				i++
				outputPath = remaining[i]
			default:
				return fmt.Errorf("unknown flag: %s", remaining[i])
			}
		}
		return cmd.DriveDownloadCmd(cfg, account, fileID, outputPath, jsonOutput)

	default:
		return fmt.Errorf("unknown drive subcommand: %s\n\nUsage: gmail drive list|search|download", sub)
	}
}

func printUsage() {
	fmt.Printf(`gmail - Gmail command-line interface (v%s)

USAGE:
    gmail <command> [options]

SETUP:
    gmail configure <alias>           Add and authorize a Gmail account
    gmail configure show              Show current configuration
    gmail accounts                    List configured accounts
    gmail accounts remove <alias>     Remove an account
    gmail doctor                      Validate installation and configuration

READING:
    gmail inbox                       Show unified inbox (all accounts)
    gmail inbox --account <alias>     Show inbox for one account
    gmail inbox --unread              Show only unread threads
    gmail inbox --limit <n>           Limit results (default: 25)
    gmail thread <id> --account <a>   Read full thread
    gmail search "<query>"            Search all accounts
    gmail search "<query>" --account <a>  Search one account

ORGANIZATION:
    gmail labels                      List all labels
    gmail labels --account <alias>    List labels for one account
    gmail label <id> <name> --account <a>         Apply label
    gmail label <id> --remove <name> --account <a> Remove label
    gmail label --create <name> --account <a>      Create label

FILTERS:
    gmail filters                             List all filters
    gmail filters --account <alias>           List filters for one account
    gmail filter --create --from "x" --label "Y" --account <a>  Create filter
    gmail filter --delete <id> --account <a>  Delete a filter

STATE:
    gmail mark <id> --read --account <a>      Mark as read
    gmail mark <id> --unread --account <a>    Mark as unread
    gmail mark <id> --archive --account <a>   Archive thread
    gmail mark <id> --trash --account <a>     Move to trash

COMPOSING:
    gmail send --to <addr> --subject <subj> "body" --account <a>   Send new email
    gmail send --to <addr> --cc <cc> --subject <subj> "body" --account <a>
    gmail reply <thread-id> "body" --account <a>                   Reply to thread

DRAFTS:
    gmail draft create --to <addr> --subject <subj> "body" --account <a>
    gmail draft list                           List drafts (all accounts)
    gmail draft list --account <a>             List drafts for one account
    gmail draft send <draft-id> --account <a>  Send an existing draft

CALENDAR:
    gmail calendar list --account <a>                    Upcoming events (default: 10)
    gmail calendar list --limit 20 --account <a>         Upcoming events with custom limit
    gmail calendar create --summary <title> --start <RFC3339> --end <RFC3339> --account <a>
    gmail calendar create --summary "Meeting" --start 2026-03-20T14:00:00Z --end 2026-03-20T15:00:00Z --account <a>
    gmail calendar create ... --description <text> --location <place> --attendee <email> --timezone <tz>
    gmail calendar available --start <RFC3339> --end <RFC3339> --account <a>

DRIVE:
    gmail drive list --account <a>                       Recent files (default: 10)
    gmail drive list --limit 20 --account <a>            Recent files with custom limit
    gmail drive search "<query>" --account <a>           Search by name or content
    gmail drive search "<query>" --limit 5 --account <a> Search with limit
    gmail drive download <file-id> --account <a>         Download file (saved to current dir)
    gmail drive download <file-id> --output <path> --account <a>  Download to specific path

GLOBAL OPTIONS:
    --json              Output in JSON format
    --account <alias>   Target a specific account
    --help, -h          Show this help
    --version, -v       Show version

EXAMPLES:
    gmail configure work                          # Add work Gmail
    gmail configure personal                      # Add personal Gmail
    gmail inbox                                   # Unified inbox
    gmail inbox --unread --account work            # Unread work email
    gmail search "from:boss subject:urgent"        # Search across accounts
    gmail thread 18abc123 --account work           # Read a thread
    gmail label 18abc123 "Action-Required" --account work
    gmail mark 18abc123 --archive --account work

For more info: https://github.com/joeyhipolito/nanika
`, version)
	_ = strings.Contains // suppress unused import if needed
}
