package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"time"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/browser"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
	"github.com/joeyhipolito/nanika-linkedin/internal/doctor"
)

// DoctorCmd runs diagnostic checks on the LinkedIn CLI setup.
func DoctorCmd(jsonOutput bool) error {
	var checks []doctor.Check
	allOK := true

	// Check 1: Binary in PATH
	path, err := exec.LookPath("linkedin")
	if err != nil {
		checks = append(checks, doctor.Check{
			Name:    "Binary",
			Status:  "warn",
			Message: "linkedin not found in PATH (running from local build?)",
		})
	} else {
		checks = append(checks, doctor.Check{
			Name:    "Binary",
			Status:  "ok",
			Message: fmt.Sprintf("linkedin found at %s", path),
		})
	}

	// Check 2: Config file exists
	if !config.Exists() {
		checks = append(checks, doctor.Check{
			Name:    "Config file",
			Status:  "fail",
			Message: "~/.linkedin/config not found. Run 'linkedin configure'",
		})
		allOK = false
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "LinkedIn Doctor", true)
	}
	checks = append(checks, doctor.Check{
		Name:    "Config file",
		Status:  "ok",
		Message: "~/.linkedin/config found",
	})

	// Check 3: Config permissions
	perms, err := config.Permissions()
	if err != nil {
		checks = append(checks, doctor.Check{
			Name:    "Config permissions",
			Status:  "warn",
			Message: fmt.Sprintf("could not check permissions: %v", err),
		})
	} else if perms != 0600 {
		checks = append(checks, doctor.Check{
			Name:    "Config permissions",
			Status:  "warn",
			Message: fmt.Sprintf("expected 0600, got %04o. Run: chmod 600 ~/.linkedin/config", perms),
		})
	} else {
		checks = append(checks, doctor.Check{
			Name:    "Config permissions",
			Status:  "ok",
			Message: "0600 (secure)",
		})
	}

	// Load config for remaining checks
	cfg, err := config.Load()
	if err != nil {
		checks = append(checks, doctor.Check{
			Name:    "Config format",
			Status:  "fail",
			Message: fmt.Sprintf("parse error: %v", err),
		})
		allOK = false
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "LinkedIn Doctor", true)
	}

	// Check 4: OAuth token present
	if cfg.AccessToken == "" || len(cfg.AccessToken) < 10 {
		checks = append(checks, doctor.Check{
			Name:    "OAuth token",
			Status:  "fail",
			Message: "access token missing or too short. Run 'linkedin configure'",
		})
		allOK = false
	} else {
		checks = append(checks, doctor.Check{
			Name:    "OAuth token",
			Status:  "ok",
			Message: "access token present",
		})
	}

	// Check 5: OAuth token valid (test with userinfo endpoint)
	if cfg.AccessToken != "" && len(cfg.AccessToken) >= 10 {
		client := api.NewOAuthClient(cfg.AccessToken, cfg.PersonURN)
		info, err := client.GetUserInfo()
		if err != nil {
			checks = append(checks, doctor.Check{
				Name:    "OAuth valid",
				Status:  "fail",
				Message: fmt.Sprintf("auth failed: %v", err),
			})
			allOK = false
		} else {
			checks = append(checks, doctor.Check{
				Name:    "OAuth valid",
				Status:  "ok",
				Message: fmt.Sprintf("authenticated as %s", info.Name),
			})
		}
	}

	// Check 6: Token expiry
	if cfg.TokenExpiry != "" {
		expiry, err := time.Parse(time.RFC3339, cfg.TokenExpiry)
		if err != nil {
			checks = append(checks, doctor.Check{
				Name:    "Token expiry",
				Status:  "warn",
				Message: fmt.Sprintf("could not parse expiry: %v", err),
			})
		} else {
			remaining := time.Until(expiry)
			if remaining <= 0 {
				checks = append(checks, doctor.Check{
					Name:    "Token expiry",
					Status:  "fail",
					Message: "token expired. Run 'linkedin configure' to re-authorize",
				})
				allOK = false
			} else if remaining < 7*24*time.Hour {
				checks = append(checks, doctor.Check{
					Name:    "Token expiry",
					Status:  "warn",
					Message: fmt.Sprintf("expires in %d days", int(remaining.Hours()/24)),
				})
			} else {
				checks = append(checks, doctor.Check{
					Name:    "Token expiry",
					Status:  "ok",
					Message: fmt.Sprintf("expires in %d days", int(remaining.Hours()/24)),
				})
			}
		}
	}

	// Check 7: Person URN present
	if cfg.PersonURN == "" {
		checks = append(checks, doctor.Check{
			Name:    "Person URN",
			Status:  "fail",
			Message: "person URN missing. Run 'linkedin configure'",
		})
		allOK = false
	} else {
		checks = append(checks, doctor.Check{
			Name:    "Person URN",
			Status:  "ok",
			Message: cfg.PersonURN,
		})
	}

	// Check 8: Chrome CDP connection
	cdp := browser.NewCDPClient(cfg.ChromeDebugURL)
	if err := cdp.TestConnection(); err != nil {
		checks = append(checks, doctor.Check{
			Name:    "Chrome CDP",
			Status:  "fail",
			Message: fmt.Sprintf("cannot connect: %v", err),
		})
		allOK = false
	} else {
		url := cfg.ChromeDebugURL
		if url == "" {
			url = browser.DefaultRemoteURL
		}
		checks = append(checks, doctor.Check{
			Name:    "Chrome CDP",
			Status:  "ok",
			Message: fmt.Sprintf("connected to %s", url),
		})

		// Check 9: LinkedIn session (only if CDP works)
		status, err := cdp.IsLoggedIn()
		if err != nil {
			checks = append(checks, doctor.Check{
				Name:    "LinkedIn session",
				Status:  "warn",
				Message: fmt.Sprintf("could not check: %v", err),
			})
		} else if !status.LoggedIn {
			checks = append(checks, doctor.Check{
				Name:    "LinkedIn session",
				Status:  "fail",
				Message: "not logged in. Open Chrome and log into linkedin.com",
			})
			allOK = false
		} else {
			msg := "logged in"
			if status.Name != "" {
				msg = fmt.Sprintf("logged in as %s", status.Name)
			}
			checks = append(checks, doctor.Check{
				Name:    "LinkedIn session",
				Status:  "ok",
				Message: msg,
			})
		}
	}

	return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "LinkedIn Doctor", true)
}
