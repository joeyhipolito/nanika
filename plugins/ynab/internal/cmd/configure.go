package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"

	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/config"
)

// ConfigureCmd runs an interactive configuration setup (like `aws configure`).
// It prompts for a YNAB access token, fetches available budgets,
// lets the user select a default, and writes ~/.ynab/config.
func ConfigureCmd() error {
	reader := bufio.NewReader(os.Stdin)

	fmt.Println("YNAB CLI Configuration")
	fmt.Println("======================")
	fmt.Println()

	// Check for existing config
	if config.Exists() {
		fmt.Printf("Existing configuration found at %s\n", config.Path())
		fmt.Print("Overwrite? [y/N] ")
		reply, _ := reader.ReadString('\n')
		reply = strings.TrimSpace(reply)
		if !strings.EqualFold(reply, "y") {
			fmt.Println("Configuration cancelled.")
			return nil
		}
		fmt.Println()
	}

	// Prompt for access token
	fmt.Println("Get your Personal Access Token from:")
	fmt.Println("https://app.ynab.com/settings/developer")
	fmt.Println()
	fmt.Print("YNAB Access Token: ")
	token, _ := reader.ReadString('\n')
	token = strings.TrimSpace(token)

	if token == "" {
		return fmt.Errorf("access token is required")
	}

	// Prompt for budget ID
	fmt.Println()
	fmt.Println("Default Budget ID (optional):")
	fmt.Println("Leave empty to select from list, or paste your budget ID")
	fmt.Print("Budget ID: ")
	budgetID, _ := reader.ReadString('\n')
	budgetID = strings.TrimSpace(budgetID)

	// If no budget ID provided, fetch and let user select
	if budgetID == "" {
		fmt.Println()
		fmt.Println("Fetching your budgets...")

		client, err := api.NewClient(token)
		if err != nil {
			return fmt.Errorf("failed to create API client: %w", err)
		}

		budgets, err := client.GetBudgets()
		if err != nil {
			return fmt.Errorf("failed to fetch budgets (check your token): %w", err)
		}

		if len(budgets) == 0 {
			return fmt.Errorf("no budgets found for this account")
		}

		fmt.Println()
		fmt.Println("Available budgets:")
		for i, b := range budgets {
			fmt.Printf("  %d. %s (%s)\n", i+1, b.Name, b.ID)
		}
		fmt.Println()

		fmt.Printf("Select budget number [1]: ")
		selection, _ := reader.ReadString('\n')
		selection = strings.TrimSpace(selection)

		idx := 0
		if selection != "" {
			n, err := strconv.Atoi(selection)
			if err != nil || n < 1 || n > len(budgets) {
				return fmt.Errorf("invalid selection: %s", selection)
			}
			idx = n - 1
		}

		budgetID = budgets[idx].ID
		fmt.Printf("Selected: %s\n", budgets[idx].Name)
	}

	// Save configuration
	cfg := &config.Config{
		AccessToken:     token,
		DefaultBudgetID: budgetID,
		APIBaseURL:      "https://api.youneedabudget.com/v1",
	}

	if err := config.Save(cfg); err != nil {
		return fmt.Errorf("failed to save configuration: %w", err)
	}

	fmt.Println()
	fmt.Printf("Configuration saved to %s\n", config.Path())
	fmt.Println()
	fmt.Println("Test your setup:")
	fmt.Println("  ynab status")
	fmt.Println("  ynab balance")
	fmt.Println()
	fmt.Println("Troubleshoot:")
	fmt.Println("  ynab doctor")

	return nil
}

// ConfigureShowCmd prints the current configuration (with token masked).
func ConfigureShowCmd(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("failed to load config: %w", err)
	}

	if !config.Exists() {
		fmt.Println("No configuration file found.")
		fmt.Println("Run 'ynab configure' to set up.")
		return nil
	}

	// Mask token for display
	maskedToken := ""
	if cfg.AccessToken != "" {
		if len(cfg.AccessToken) > 8 {
			maskedToken = cfg.AccessToken[:4] + "..." + cfg.AccessToken[len(cfg.AccessToken)-4:]
		} else {
			maskedToken = "****"
		}
	}

	if jsonOutput {
		output := map[string]string{
			"config_path":       config.Path(),
			"access_token":      maskedToken,
			"default_budget_id": cfg.DefaultBudgetID,
			"api_base_url":      cfg.APIBaseURL,
		}
		encoder := json.NewEncoder(os.Stdout)
		encoder.SetIndent("", "  ")
		return encoder.Encode(output)
	}

	fmt.Printf("Config file: %s\n", config.Path())
	fmt.Printf("Access token: %s\n", maskedToken)
	fmt.Printf("Default budget: %s\n", cfg.DefaultBudgetID)
	fmt.Printf("API base URL: %s\n", cfg.APIBaseURL)
	return nil
}
