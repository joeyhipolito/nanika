package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// accountLabels groups labels with their account alias for output.
type accountLabels struct {
	Account string      `json:"account"`
	Labels  []api.Label `json:"labels"`
}

// LabelsCmd handles "gmail labels [--account alias] [--json]"
// If --account set, show labels for that account only.
// Otherwise show labels for all accounts, grouped by account.
func LabelsCmd(cfg *config.Config, account string, jsonOutput bool) error {
	var results []accountLabels

	if account != "" {
		client, err := api.NewClient(account, cfg)
		if err != nil {
			return fmt.Errorf("connect to account %q: %w", account, err)
		}
		labels, err := client.ListLabels()
		if err != nil {
			return fmt.Errorf("list labels for %q: %w", account, err)
		}
		results = append(results, accountLabels{Account: account, Labels: labels})
	} else {
		clients, err := api.NewClientAll(cfg)
		if err != nil {
			return err
		}
		for _, c := range clients {
			labels, err := c.ListLabels()
			if err != nil {
				fmt.Fprintf(os.Stderr, "warning: failed to list labels for %q: %v\n", c.Alias(), err)
				continue
			}
			results = append(results, accountLabels{Account: c.Alias(), Labels: labels})
		}
	}

	if jsonOutput {
		return printJSON(results)
	}

	for i, r := range results {
		if i > 0 {
			fmt.Println()
		}
		fmt.Printf("[%s] Labels:\n", r.Account)
		printLabelsHuman(r.Labels)
	}

	return nil
}

// LabelApplyCmd handles "gmail label <thread-id> <label-name> --account <alias>"
// Applies a label to a thread. Uses EnsureLabel to create if needed.
func LabelApplyCmd(cfg *config.Config, threadID, labelName, account string) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	labelID, err := client.EnsureLabel(labelName)
	if err != nil {
		return fmt.Errorf("ensure label %q: %w", labelName, err)
	}

	if err := client.ModifyThread(threadID, []string{labelID}, nil); err != nil {
		return fmt.Errorf("apply label to thread %s: %w", threadID, err)
	}

	fmt.Printf("Applied label %q to thread %s\n", labelName, threadID)
	return nil
}

// LabelRemoveCmd handles "gmail label <thread-id> --remove <label-name> --account <alias>"
// Removes a label from a thread.
func LabelRemoveCmd(cfg *config.Config, threadID, labelName, account string) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	labelID, err := client.GetLabelID(labelName)
	if err != nil {
		return fmt.Errorf("find label %q: %w", labelName, err)
	}

	if err := client.ModifyThread(threadID, nil, []string{labelID}); err != nil {
		return fmt.Errorf("remove label from thread %s: %w", threadID, err)
	}

	fmt.Printf("Removed label %q from thread %s\n", labelName, threadID)
	return nil
}

// LabelCreateCmd handles "gmail label --create <name> --account <alias>"
// Creates a new label.
func LabelCreateCmd(cfg *config.Config, labelName, account string, jsonOutput bool) error {
	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	label, err := client.CreateLabel(labelName)
	if err != nil {
		return fmt.Errorf("create label %q: %w", labelName, err)
	}

	if jsonOutput {
		return printJSON(label)
	}

	fmt.Printf("Created label %q (id: %s) on account %q\n", label.Name, label.ID, account)
	return nil
}

// printLabelsHuman prints labels in a human-readable table format.
func printLabelsHuman(labels []api.Label) {
	// Sort: system labels first (alphabetical), then user labels (alphabetical).
	sort.Slice(labels, func(i, j int) bool {
		ti := labelSortKey(labels[i])
		tj := labelSortKey(labels[j])
		if ti != tj {
			return ti < tj
		}
		return strings.ToLower(labels[i].Name) < strings.ToLower(labels[j].Name)
	})

	// Find the longest label name for alignment.
	maxName := 0
	for _, l := range labels {
		if len(l.Name) > maxName {
			maxName = len(l.Name)
		}
	}

	for _, l := range labels {
		labelType := "user"
		if l.Type == "system" {
			labelType = "system"
		}

		stats := formatLabelStats(l)
		fmt.Printf("  %-*s (%s)  %s\n", maxName, l.Name, labelType, stats)
	}
}

// labelSortKey returns 0 for system labels, 1 for user labels.
func labelSortKey(l api.Label) int {
	if l.Type == "system" {
		return 0
	}
	return 1
}

// formatLabelStats returns a human-readable stats string for a label.
func formatLabelStats(l api.Label) string {
	if l.ThreadsTotal == 0 && l.ThreadsUnread == 0 {
		return ""
	}

	threadWord := "threads"
	if l.ThreadsTotal == 1 {
		threadWord = "thread"
	}

	if l.ThreadsUnread > 0 {
		return fmt.Sprintf("%d %s, %d unread", l.ThreadsTotal, threadWord, l.ThreadsUnread)
	}
	return fmt.Sprintf("%d %s", l.ThreadsTotal, threadWord)
}

// printJSON marshals v to JSON and writes to stdout.
func printJSON(v interface{}) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}
