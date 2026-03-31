package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"time"

	"github.com/joeyhipolito/nanika-youtube/internal/api"
)

// DoctorCmd runs diagnostic checks on the YouTube CLI setup.
func DoctorCmd(jsonOutput bool) error {
	var checks []api.DoctorCheck
	allOK := true

	// Check 1: Binary in PATH.
	path, err := exec.LookPath("youtube")
	if err != nil {
		checks = append(checks, api.DoctorCheck{
			Name:    "binary",
			Status:  "warn",
			Message: "youtube not found in PATH (running from local build?)",
		})
	} else {
		checks = append(checks, api.DoctorCheck{
			Name:    "binary",
			Status:  "ok",
			Message: fmt.Sprintf("youtube found at %s", path),
		})
	}

	// Check 2: Config file exists.
	if !api.ConfigExists() {
		checks = append(checks, api.DoctorCheck{
			Name:    "config",
			Status:  "fail",
			Message: "~/.alluka/youtube-config.json not found. Run 'youtube configure'",
		})
		allOK = false
		return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
	}
	cfgPath, _ := api.ConfigFilePath()
	checks = append(checks, api.DoctorCheck{
		Name:    "config",
		Status:  "ok",
		Message: fmt.Sprintf("config found at %s", cfgPath),
	})

	// Check 3: Load config.
	cfg, err := api.LoadConfig()
	if err != nil {
		checks = append(checks, api.DoctorCheck{
			Name:    "config_parse",
			Status:  "fail",
			Message: fmt.Sprintf("parse error: %v", err),
		})
		allOK = false
		return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
	}

	// Check 4: API key.
	if cfg.APIKey == "" {
		checks = append(checks, api.DoctorCheck{
			Name:    "api_key",
			Status:  "fail",
			Message: "api_key missing from ~/.alluka/youtube-config.json",
		})
		allOK = false
	} else {
		checks = append(checks, api.DoctorCheck{
			Name:    "api_key",
			Status:  "ok",
			Message: "API key configured",
		})
	}

	// Check 5: OAuth token.
	token, err := api.LoadToken()
	switch {
	case err != nil:
		checks = append(checks, api.DoctorCheck{
			Name:    "oauth_token",
			Status:  "warn",
			Message: "no OAuth token — run 'youtube auth' to set up posting",
		})
	case token.IsExpired():
		checks = append(checks, api.DoctorCheck{
			Name:    "oauth_token",
			Status:  "warn",
			Message: fmt.Sprintf("token expired at %s; will refresh on next comment/like", token.Expiry.Format(time.RFC3339)),
		})
	default:
		checks = append(checks, api.DoctorCheck{
			Name:    "oauth_token",
			Status:  "ok",
			Message: fmt.Sprintf("token valid until %s", token.Expiry.Format(time.RFC3339)),
		})
	}

	// Check 6: Channels configured.
	if len(cfg.Channels) == 0 {
		checks = append(checks, api.DoctorCheck{
			Name:    "channels",
			Status:  "warn",
			Message: "no channels configured in ~/.alluka/youtube-config.json",
		})
	} else {
		checks = append(checks, api.DoctorCheck{
			Name:    "channels",
			Status:  "ok",
			Message: fmt.Sprintf("%d channel(s) configured", len(cfg.Channels)),
		})
	}

	// Check 7: Quota status.
	client := api.NewClientFromConfig(cfg)
	ctx := context.Background()
	used := client.TodayQuota(ctx)
	quotaStatus := "ok"
	quotaMsg := fmt.Sprintf("used %d/%d units today", used, cfg.Budget)
	if used >= cfg.Budget {
		quotaStatus = "warn"
		quotaMsg += " (budget exhausted)"
	}
	checks = append(checks, api.DoctorCheck{
		Name:    "quota",
		Status:  quotaStatus,
		Message: quotaMsg,
	})

	return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
}

func renderDoctor(w *os.File, checks []api.DoctorCheck, allOK bool, jsonOutput bool) error {
	if jsonOutput {
		summary := "OK"
		if !allOK {
			summary = "issues found"
		}
		result := api.DoctorResult{
			Platform: "youtube",
			OK:       allOK,
			Checks:   checks,
			Summary:  summary,
		}
		enc := json.NewEncoder(w)
		enc.SetIndent("", "  ")
		return enc.Encode(result)
	}

	fmt.Fprintln(w, "YouTube Doctor")
	fmt.Fprintln(w, "==============")
	fmt.Fprintln(w)
	for _, c := range checks {
		icon := "+"
		switch c.Status {
		case "warn":
			icon = "!"
		case "fail":
			icon = "x"
		}
		fmt.Fprintf(w, "  [%s] %s: %s\n", icon, c.Name, c.Message)
	}
	fmt.Fprintln(w)
	if allOK {
		fmt.Fprintln(w, "Status: OK")
	} else {
		fmt.Fprintln(w, "Status: issues found — see above")
	}
	return nil
}
