package cmd

import (
	"fmt"
	"os"
	"strings"
	"sync"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

type queryAccountStatus struct {
	Alias  string `json:"alias"`
	Unread int    `json:"unread"`
}

type queryStatusOutput struct {
	Accounts    []queryAccountStatus `json:"accounts"`
	TotalUnread int                  `json:"total_unread"`
}

type queryItemsOutput struct {
	Items []api.ThreadSummary `json:"items"`
	Count int                 `json:"count"`
}

type queryAction struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type queryActionsOutput struct {
	Actions []queryAction `json:"actions"`
}

func QueryCmd(cfg *config.Config, account string, subcommand string, jsonOutput bool) error {
	switch subcommand {
	case "status":
		return gmailQueryStatus(cfg, jsonOutput)
	case "items":
		return gmailQueryItems(cfg, account, jsonOutput)
	case "actions":
		return gmailQueryActions(jsonOutput)
	default:
		return fmt.Errorf("unknown query subcommand %q — use status, items, or actions", subcommand)
	}
}

func gmailQueryStatus(cfg *config.Config, jsonOutput bool) error {
	clients, err := api.NewClientAll(cfg)
	if err != nil {
		return fmt.Errorf("connecting to accounts: %w", err)
	}

	type accountResult struct {
		status queryAccountStatus
		err    error
	}
	results := make([]accountResult, len(clients))
	var wg sync.WaitGroup
	for i, c := range clients {
		wg.Add(1)
		go func(idx int, client *api.Client) {
			defer wg.Done()
			labels, err := client.ListLabels()
			if err != nil {
				results[idx] = accountResult{err: fmt.Errorf("listing labels for %q: %w", client.Alias(), err)}
				return
			}
			unread := 0
			for _, l := range labels {
				if l.ID == "INBOX" {
					unread = l.ThreadsUnread
					break
				}
			}
			results[idx] = accountResult{status: queryAccountStatus{Alias: client.Alias(), Unread: unread}}
		}(i, c)
	}
	wg.Wait()

	var statuses []queryAccountStatus
	total := 0
	for _, r := range results {
		if r.err != nil {
			fmt.Fprintf(os.Stderr, "warning: %v\n", r.err)
			continue
		}
		statuses = append(statuses, r.status)
		total += r.status.Unread
	}

	out := queryStatusOutput{Accounts: statuses, TotalUnread: total}
	if jsonOutput {
		return printJSON(out)
	}
	fmt.Println("Gmail Status")
	fmt.Println(strings.Repeat("=", 30))
	for _, s := range statuses {
		fmt.Printf("  %-20s %d unread\n", s.Alias, s.Unread)
	}
	fmt.Printf("  %-20s %d total\n", "Total", total)
	return nil
}

func gmailQueryItems(cfg *config.Config, account string, jsonOutput bool) error {
	summaries, err := fetchUnreadInbox(cfg, account, 20)
	if err != nil {
		return err
	}
	if jsonOutput {
		return printJSON(queryItemsOutput{Items: summaries, Count: len(summaries)})
	}
	if len(summaries) == 0 {
		fmt.Println("No unread threads.")
		return nil
	}
	for _, t := range summaries {
		fmt.Printf("[%s] %s — %s\n", t.Account, t.Subject, t.From)
	}
	return nil
}

func fetchUnreadInbox(cfg *config.Config, account string, limit int) ([]api.ThreadSummary, error) {
	if account != "" {
		c, err := api.NewClient(account, cfg)
		if err != nil {
			return nil, fmt.Errorf("connect to %q: %w", account, err)
		}
		return c.ListInbox(limit, true)
	}
	clients, err := api.NewClientAll(cfg)
	if err != nil {
		return nil, err
	}
	var mu sync.Mutex
	var wg sync.WaitGroup
	var all []api.ThreadSummary
	var firstErr error
	for _, c := range clients {
		wg.Add(1)
		go func(client *api.Client) {
			defer wg.Done()
			s, err := client.ListInbox(limit, true)
			if err != nil {
				mu.Lock()
				if firstErr == nil {
					firstErr = fmt.Errorf("inbox for %q: %w", client.Alias(), err)
				}
				mu.Unlock()
				return
			}
			mu.Lock()
			all = append(all, s...)
			mu.Unlock()
		}(c)
	}
	wg.Wait()
	if firstErr != nil {
		if len(all) == 0 {
			return nil, firstErr
		}
		fmt.Fprintf(os.Stderr, "warning: %v\n", firstErr)
	}
	return all, nil
}

func gmailQueryActions(jsonOutput bool) error {
	actions := []queryAction{
		{Name: "inbox", Command: "gmail inbox", Description: "Show unified inbox"},
		{Name: "inbox-unread", Command: "gmail inbox --unread", Description: "Show only unread threads"},
		{Name: "search", Command: "gmail search <query>", Description: "Search across accounts"},
		{Name: "thread", Command: "gmail thread <id> --account <alias>", Description: "Read a full thread"},
		{Name: "mark-read", Command: "gmail mark <id> --read --account <alias>", Description: "Mark thread as read"},
		{Name: "archive", Command: "gmail mark <id> --archive --account <alias>", Description: "Archive thread"},
		{Name: "send", Command: "gmail send --to <addr> --subject <subj> <body> --account <alias>", Description: "Send email"},
		{Name: "reply", Command: "gmail reply <id> <body> --account <alias>", Description: "Reply to thread"},
		{Name: "label", Command: "gmail label <id> <name> --account <alias>", Description: "Apply label to thread"},
	}
	if jsonOutput {
		return printJSON(queryActionsOutput{Actions: actions})
	}
	fmt.Println("Available actions:")
	for _, a := range actions {
		fmt.Printf("  %-55s  %s\n", a.Command, a.Description)
	}
	return nil
}
