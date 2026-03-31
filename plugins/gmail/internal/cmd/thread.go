package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// ThreadCmd handles "gmail thread <thread-id> --account <alias> [--json]".
// account is required for thread reading.
// Shows full thread with all messages, decoded bodies.
func ThreadCmd(cfg *config.Config, threadID, account string, jsonOutput bool) error {
	if threadID == "" {
		return fmt.Errorf("thread ID is required")
	}
	if account == "" {
		return fmt.Errorf("--account is required for reading threads")
	}

	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	thread, err := client.GetThread(threadID)
	if err != nil {
		return fmt.Errorf("get thread %s: %w", threadID, err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(thread)
	}

	printThread(thread)
	return nil
}

// printThread prints a full thread in human-readable format.
func printThread(t *api.Thread) {
	// Thread header.
	subject := ""
	if len(t.Messages) > 0 {
		subject = t.Messages[0].Subject
	}
	if subject == "" {
		subject = "(no subject)"
	}

	fmt.Printf("Thread: %s (%s)\n", t.ID, t.Account)
	fmt.Printf("Subject: %s\n", subject)
	fmt.Printf("Messages: %d\n", len(t.Messages))
	fmt.Println()

	separator := strings.Repeat("\u2500", 40)

	for i, msg := range t.Messages {
		fmt.Printf("\u2500\u2500\u2500 Message %d %s\n", i+1, separator)
		fmt.Printf("From: %s\n", msg.From)
		fmt.Printf("To: %s\n", msg.To)
		fmt.Printf("Date: %s\n", msg.Date)
		fmt.Println()

		body := strings.TrimSpace(msg.Body)
		if body == "" {
			body = msg.Snippet
		}
		fmt.Println(body)

		if i < len(t.Messages)-1 {
			fmt.Println()
		}
	}
}
