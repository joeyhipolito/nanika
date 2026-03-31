package cmd

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-ynab/internal/doctor"
	"github.com/joeyhipolito/nanika-ynab/internal/api"
	"github.com/joeyhipolito/nanika-ynab/internal/config"
)

// DoctorCmd validates the YNAB CLI installation and configuration.
func DoctorCmd(jsonOutput bool) error {
	var checks []doctor.Check
	allOK := true

	add := func(name, status, msg string) {
		checks = append(checks, doctor.Check{Name: name, Status: status, Message: msg})
		if status == "fail" {
			allOK = false
		}
	}

	// 1. Binary in PATH
	checks = append(checks, doctor.BinaryInPath("ynab")())

	// 2-3. Config exists + permissions
	configPath := config.Path()
	checks = append(checks, doctor.ConfigExists(configPath, "ynab configure")())
	checks = append(checks, doctor.ConfigPermissions(configPath)())

	if !config.Exists() {
		return doctor.Render(os.Stdout, checks, false, jsonOutput, "YNAB Doctor", true)
	}

	// 4. Config parseable + access token
	cfg, err := config.Load()
	if err != nil {
		add("Config format", "fail", fmt.Sprintf("failed to parse config: %v", err))
		return doctor.Render(os.Stdout, checks, false, jsonOutput, "YNAB Doctor", true)
	}

	token := cfg.AccessToken
	if token == "" {
		token = os.Getenv("YNAB_ACCESS_TOKEN")
	}
	if token == "" {
		add("Access token", "fail", "not found in config or YNAB_ACCESS_TOKEN env var")
		return doctor.Render(os.Stdout, checks, false, jsonOutput, "YNAB Doctor", true)
	}
	masked := token[:4] + "..." + token[len(token)-4:]
	add("Access token", "ok", fmt.Sprintf("present (%s)", masked))

	// 5. Default budget ID
	budgetID := cfg.DefaultBudgetID
	if budgetID == "" {
		budgetID = os.Getenv("YNAB_DEFAULT_BUDGET_ID")
	}
	if budgetID != "" {
		add("Default budget", "ok", budgetID)
	} else {
		add("Default budget", "warn", "not set (will use first budget)")
	}

	// 6. API connection
	client, err := api.NewClient(token)
	if err != nil {
		add("API connection", "fail", fmt.Sprintf("failed to create client: %v", err))
		return doctor.Render(os.Stdout, checks, false, jsonOutput, "YNAB Doctor", true)
	}

	budgets, err := client.GetBudgets()
	if err != nil {
		add("API connection", "fail", fmt.Sprintf("failed: %v", err))
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "YNAB Doctor", true)
	}
	add("API connection", "ok", fmt.Sprintf("success (%d budget(s) found)", len(budgets)))

	// 7. Budget access if ID is set
	if budgetID != "" {
		found := false
		for _, b := range budgets {
			if b.ID == budgetID {
				found = true
				add("Budget access", "ok", b.Name)
				break
			}
		}
		if !found {
			add("Budget access", "fail", fmt.Sprintf("budget %s not found in your account", budgetID))
		}
	}

	return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "YNAB Doctor", true)
}
