package cmd

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// DraftCreateCmd handles "gmail draft create --to <addr> --subject <subj> \"body\" --account <alias>"
func DraftCreateCmd(cfg *config.Config, p api.ComposeParams, account string, jsonOutput bool) error {
	if account == "" {
		return fmt.Errorf("--account is required for draft create")
	}
	if p.To == "" {
		return fmt.Errorf("--to is required\n\nUsage: gmail draft create --to <addr> --subject <subj> \"body\" --account <alias>")
	}

	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	draft, err := client.CreateDraft(p)
	if err != nil {
		return err
	}

	if jsonOutput {
		return printJSON(draft)
	}

	fmt.Printf("Draft created. ID: %s (to: %s, subject: %q)\n", draft.ID, draft.To, draft.Subject)
	return nil
}

// DraftListCmd handles "gmail draft list [--account <alias>]"
// If --account is set, lists drafts for that account only.
// Otherwise lists drafts for all configured accounts.
func DraftListCmd(cfg *config.Config, account string, jsonOutput bool) error {
	type accountDrafts struct {
		Account string             `json:"account"`
		Drafts  []api.DraftSummary `json:"drafts"`
	}

	var results []accountDrafts

	if account != "" {
		client, err := api.NewClient(account, cfg)
		if err != nil {
			return fmt.Errorf("connect to account %q: %w", account, err)
		}
		drafts, err := client.ListDrafts()
		if err != nil {
			return fmt.Errorf("list drafts for %q: %w", account, err)
		}
		results = append(results, accountDrafts{Account: account, Drafts: drafts})
	} else {
		clients, err := api.NewClientAll(cfg)
		if err != nil {
			return err
		}
		for _, c := range clients {
			drafts, err := c.ListDrafts()
			if err != nil {
				fmt.Fprintf(os.Stderr, "warning: failed to list drafts for %q: %v\n", c.Alias(), err)
				continue
			}
			results = append(results, accountDrafts{Account: c.Alias(), Drafts: drafts})
		}
	}

	if jsonOutput {
		return printJSON(results)
	}

	for i, r := range results {
		if i > 0 {
			fmt.Println()
		}
		if len(r.Drafts) == 0 {
			fmt.Printf("[%s] No drafts.\n", r.Account)
			continue
		}
		fmt.Printf("[%s] Drafts (%d):\n", r.Account, len(r.Drafts))
		for _, d := range r.Drafts {
			subject := d.Subject
			if subject == "" {
				subject = "(no subject)"
			}
			to := d.To
			if to == "" {
				to = "(no recipient)"
			}
			fmt.Printf("  %s  %s → %s\n", d.ID, subject, to)
			if d.Snippet != "" {
				fmt.Printf("            %s\n", truncate(d.Snippet, 80))
			}
		}
	}

	return nil
}

// DraftSendCmd handles "gmail draft send <draft-id> --account <alias>"
func DraftSendCmd(cfg *config.Config, draftID, account string, jsonOutput bool) error {
	if account == "" {
		return fmt.Errorf("--account is required for draft send\n\nUsage: gmail draft send <draft-id> --account <alias>")
	}

	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	sent, err := client.SendDraft(draftID)
	if err != nil {
		return err
	}

	if jsonOutput {
		return printJSON(sent)
	}

	fmt.Printf("Draft sent. Message ID: %s (thread: %s)\n", sent.ID, sent.ThreadID)
	return nil
}
