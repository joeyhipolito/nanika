package cmd

import (
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
	"github.com/joeyhipolito/nanika-gmail/internal/doctor"
)

// DoctorCmd validates the Gmail CLI installation and configuration.
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
	checks = append(checks, doctor.BinaryInPath("gmail"))

	// 2. Config file exists
	configPath := config.Path()
	if !config.Exists() {
		add("Config file", "fail", fmt.Sprintf("%s not found. Run 'gmail configure <alias>'", configPath))
		return doctor.Render(os.Stdout, checks, false, jsonOutput, "Gmail Doctor", true)
	}
	add("Config file", "ok", configPath)

	// 3. Config permissions
	if c := doctor.ConfigPermissions(configPath); c.Status != "" {
		checks = append(checks, c)
	}

	// 4. Client credentials
	cfg, err := config.Load()
	if err != nil {
		add("Config format", "fail", fmt.Sprintf("failed to parse config: %v", err))
		return doctor.Render(os.Stdout, checks, false, jsonOutput, "Gmail Doctor", true)
	}

	var missing []string
	if cfg.ClientID == "" {
		missing = append(missing, "client_id")
	}
	if cfg.ClientSecret == "" {
		missing = append(missing, "client_secret")
	}
	if len(missing) > 0 {
		add("Client credentials", "fail", fmt.Sprintf("missing: %s. Run 'gmail configure <alias>'", strings.Join(missing, ", ")))
		allOK = false
	} else {
		add("Client credentials", "ok", fmt.Sprintf("client_id=%s, client_secret=(set)", maskStr(cfg.ClientID)))
	}

	// 5. Accounts
	accounts, err := config.LoadAccounts()
	if err != nil {
		add("Accounts", "fail", fmt.Sprintf("failed to load accounts: %v", err))
		allOK = false
	} else if len(accounts) == 0 {
		add("Accounts", "warn", "no accounts configured. Run 'gmail configure <alias>'")
	} else {
		add("Accounts", "ok", fmt.Sprintf("%d account(s) registered", len(accounts)))

		// 6. Per-account token + API check
		for _, acct := range accounts {
			client, err := api.NewClient(acct.Alias, cfg)
			if err != nil {
				add(fmt.Sprintf("Account %q", acct.Alias), "fail", fmt.Sprintf("failed to create client: %v", err))
				allOK = false
				continue
			}
			profile, err := client.Service().Users.GetProfile("me").Do()
			if err != nil {
				add(fmt.Sprintf("Account %q", acct.Alias), "fail", fmt.Sprintf("API call failed: %v", err))
				allOK = false
			} else {
				add(fmt.Sprintf("Account %q", acct.Alias), "ok", fmt.Sprintf("%s (%d messages)", profile.EmailAddress, profile.MessagesTotal))
			}
		}
	}

	return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Gmail Doctor", true)
}

// maskStr masks all but the first and last 4 characters of a string.
func maskStr(s string) string {
	if len(s) <= 8 {
		return "****"
	}
	return s[:4] + "..." + s[len(s)-4:]
}
