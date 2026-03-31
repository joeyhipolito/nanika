package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// SendCmd handles "gmail send --to <addr> --subject <subj> \"body\" --account <alias>"
func SendCmd(cfg *config.Config, p api.ComposeParams, account string, jsonOutput bool) error {
	if account == "" {
		return fmt.Errorf("--account is required for send\n\nUsage: gmail send --to <addr> --subject <subj> \"body\" --account <alias>")
	}
	if p.To == "" {
		return fmt.Errorf("--to is required\n\nUsage: gmail send --to <addr> --subject <subj> \"body\" --account <alias>")
	}

	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	sent, err := client.Send(p)
	if err != nil {
		return err
	}

	if jsonOutput {
		return printJSON(sent)
	}

	fmt.Printf("Sent. Message ID: %s (thread: %s)\n", sent.ID, sent.ThreadID)
	return nil
}

// ReplyCmd handles "gmail reply <thread-id> \"body\" --account <alias>"
func ReplyCmd(cfg *config.Config, threadID, body, account string, jsonOutput bool) error {
	if account == "" {
		return fmt.Errorf("--account is required for reply\n\nUsage: gmail reply <thread-id> \"body\" --account <alias>")
	}

	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	sent, err := client.Reply(threadID, body)
	if err != nil {
		return err
	}

	if jsonOutput {
		return printJSON(sent)
	}

	fmt.Printf("Reply sent. Message ID: %s (thread: %s)\n", sent.ID, sent.ThreadID)
	return nil
}
