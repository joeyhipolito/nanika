package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
)

// PayeesOutput represents the JSON output for the payees command.
type PayeesOutput struct {
	Payees []PayeeItem `json:"payees"`
	Count  int         `json:"count"`
}

// PayeeItem represents a single payee in the output.
type PayeeItem struct {
	ID   string `json:"id"`
	Name string `json:"name"`
}

// PayeesCmd lists all payees with optional name filtering.
func PayeesCmd(client *api.Client, filter string, jsonOutput bool) error {
	budgetID, err := client.GetDefaultBudgetID()
	if err != nil {
		return err
	}

	payees, err := client.GetPayees(budgetID)
	if err != nil {
		return fmt.Errorf("failed to get payees: %w", err)
	}

	// Filter
	var filtered []*api.Payee
	filterLower := strings.ToLower(filter)
	for _, p := range payees {
		if p.Deleted {
			continue
		}
		if filter != "" && !strings.Contains(strings.ToLower(p.Name), filterLower) {
			continue
		}
		filtered = append(filtered, p)
	}

	if jsonOutput {
		output := PayeesOutput{
			Payees: make([]PayeeItem, 0, len(filtered)),
			Count:  len(filtered),
		}
		for _, p := range filtered {
			output.Payees = append(output.Payees, PayeeItem{
				ID:   p.ID,
				Name: p.Name,
			})
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	if len(filtered) == 0 {
		if filter != "" {
			fmt.Printf("No payees found matching '%s'.\n", filter)
		} else {
			fmt.Println("No payees found.")
		}
		return nil
	}

	fmt.Printf("Payees:\n\n")

	maxName := 20
	for _, p := range filtered {
		if len(p.Name) > maxName && len(p.Name) <= 40 {
			maxName = len(p.Name)
		}
	}

	fmt.Printf("%-*s  %s\n", maxName, "Name", "ID")
	fmt.Printf("%s\n", strings.Repeat("-", maxName+2+36))

	for _, p := range filtered {
		fmt.Printf("%-*s  %s\n", maxName, p.Name, p.ID)
	}

	fmt.Printf("\n%d payee(s)\n", len(filtered))
	return nil
}
