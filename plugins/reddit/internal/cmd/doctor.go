package cmd

import (
	"fmt"
	"os"
	"os/exec"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
	"github.com/joeyhipolito/nanika-reddit/internal/config"
	"github.com/joeyhipolito/nanika-reddit/internal/doctor"
)

// DoctorCmd runs diagnostic checks on the Reddit CLI setup.
func DoctorCmd(jsonOutput bool) error {
	var checks []doctor.Check
	allOK := true

	// Check 1: Binary in PATH
	path, err := exec.LookPath("reddit")
	if err != nil {
		checks = append(checks, doctor.Check{
			Name:    "Binary",
			Status:  "warn",
			Message: "reddit not found in PATH (running from local build?)",
		})
	} else {
		checks = append(checks, doctor.Check{
			Name:    "Binary",
			Status:  "ok",
			Message: fmt.Sprintf("reddit found at %s", path),
		})
	}

	// Check 2: Config file exists
	if !config.Exists() {
		checks = append(checks, doctor.Check{
			Name:    "Config file",
			Status:  "fail",
			Message: "~/.reddit/config not found. Run 'reddit configure cookies'",
		})
		allOK = false
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Reddit Doctor", true)
	}
	checks = append(checks, doctor.Check{
		Name:    "Config file",
		Status:  "ok",
		Message: "~/.reddit/config found",
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
			Message: fmt.Sprintf("expected 0600, got %04o. Run: chmod 600 ~/.reddit/config", perms),
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
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Reddit Doctor", true)
	}
	checks = append(checks, doctor.Check{
		Name:    "Config format",
		Status:  "ok",
		Message: "config parsed successfully",
	})

	// Check 4: Cookies present
	if cfg.RedditSession == "" || cfg.CSRFToken == "" {
		missing := []string{}
		if cfg.RedditSession == "" {
			missing = append(missing, "reddit_session")
		}
		if cfg.CSRFToken == "" {
			missing = append(missing, "csrf_token")
		}
		checks = append(checks, doctor.Check{
			Name:    "Cookies",
			Status:  "fail",
			Message: fmt.Sprintf("missing: %v. Run 'reddit configure cookies'", missing),
		})
		allOK = false
	} else {
		checks = append(checks, doctor.Check{
			Name:    "Cookies",
			Status:  "ok",
			Message: "reddit_session and csrf_token present",
		})
	}

	// Check 5: Cookies valid (test auth)
	if cfg.RedditSession != "" && cfg.CSRFToken != "" {
		client := api.NewRedditClient(cfg.RedditSession, cfg.CSRFToken)
		username, err := client.TestAuth()
		if err != nil {
			checks = append(checks, doctor.Check{
				Name:    "Cookies valid",
				Status:  "fail",
				Message: fmt.Sprintf("auth failed: %v", err),
			})
			allOK = false
		} else {
			checks = append(checks, doctor.Check{
				Name:    "Cookies valid",
				Status:  "ok",
				Message: fmt.Sprintf("authenticated as u/%s", username),
			})
		}
	}

	return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Reddit Doctor", true)
}
