package cmd

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/config"
)

// doctorCheck is the result of a single diagnostic check.
type doctorCheck struct {
	Name    string `json:"name"`
	Status  string `json:"status"`  // "ok", "warn", "fail"
	Message string `json:"message"`
}

// renderDoctor writes the doctor output as JSON or formatted text.
func renderDoctor(w io.Writer, checks []doctorCheck, allOK bool, jsonOutput bool) error {
	summary := "all checks passed"
	if !allOK {
		summary = "some checks failed"
	}

	if jsonOutput {
		out := struct {
			Checks  []doctorCheck `json:"checks"`
			Summary string        `json:"summary"`
			AllOK   bool          `json:"all_ok"`
		}{checks, summary, allOK}
		enc := json.NewEncoder(w)
		enc.SetIndent("", "  ")
		if err := enc.Encode(out); err != nil {
			return err
		}
		if !allOK {
			return fmt.Errorf("doctor checks failed")
		}
		return nil
	}

	const title = "ElevenLabs Doctor"
	fmt.Fprintln(w, title)
	fmt.Fprintln(w, repeatChar('=', len(title)))
	for _, c := range checks {
		icon := "  OK"
		switch c.Status {
		case "warn":
			icon = "WARN"
		case "fail":
			icon = "FAIL"
		}
		fmt.Fprintf(w, "  [%4s] %-22s %s\n", icon, c.Name+":", c.Message)
	}
	fmt.Fprintf(w, "\nSummary: %s\n", summary)
	if !allOK {
		return fmt.Errorf("doctor checks failed")
	}
	return nil
}

func repeatChar(c byte, n int) string {
	b := make([]byte, n)
	for i := range b {
		b[i] = c
	}
	return string(b)
}

// DoctorCmd checks config existence, permissions, API key validity, and quota.
func DoctorCmd(jsonOutput bool) error {
	var checks []doctorCheck
	allOK := true

	fail := func(name, msg string) {
		checks = append(checks, doctorCheck{Name: name, Status: "fail", Message: msg})
		allOK = false
	}
	warn := func(name, msg string) {
		checks = append(checks, doctorCheck{Name: name, Status: "warn", Message: msg})
	}
	ok := func(name, msg string) {
		checks = append(checks, doctorCheck{Name: name, Status: "ok", Message: msg})
	}

	// 1. Binary in PATH
	if path, err := exec.LookPath("elevenlabs"); err != nil {
		warn("Binary", "elevenlabs not found in PATH (running from local build?)")
	} else {
		ok("Binary", path)
	}

	// 2. Config file exists
	if !config.Exists() {
		fail("Config file", fmt.Sprintf("%s not found. Run 'elevenlabs configure'", config.Path()))
		return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
	}
	ok("Config file", config.Path())

	// 3. Config permissions
	perms, err := config.Permissions()
	if err != nil {
		warn("Config permissions", fmt.Sprintf("could not check: %v", err))
	} else if perms != 0600 {
		warn("Config permissions", fmt.Sprintf("expected 0600, got %04o. Fix: chmod 600 %s", perms, config.Path()))
	} else {
		ok("Config permissions", "0600 (secure)")
	}

	// 4. Config parseable with required fields
	cfg, err := config.Load()
	if err != nil {
		fail("Config format", fmt.Sprintf("parse error: %v", err))
		return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
	}
	if cfg.APIKey == "" {
		fail("Config format", "api_key is missing. Run 'elevenlabs configure'")
		return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
	}
	ok("Config format", "api_key, model present")

	// 5. API key valid + quota
	client := api.NewClient(cfg.APIKey)
	user, err := client.GetUser(context.Background())
	if err != nil {
		fail("API key", fmt.Sprintf("verification failed: %v", err))
		return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
	}
	ok("API key", fmt.Sprintf("valid (%s plan)", user.Subscription.Status))

	// 6. Quota
	used := user.Subscription.CharacterCount
	limit := user.Subscription.CharacterLimit
	if limit > 0 {
		remaining := limit - used
		pct := float64(used) / float64(limit) * 100
		msg := fmt.Sprintf("%d / %d characters used (%.0f%% — %d remaining)", used, limit, pct, remaining)
		if pct >= 90 {
			warn("Quota", msg)
		} else {
			ok("Quota", msg)
		}
	} else {
		ok("Quota", fmt.Sprintf("%d characters used (unlimited)", used))
	}

	// 7. Default voice set
	if cfg.DefaultVoiceID == "" {
		warn("Default voice", "not set — use --voice flag at generate time or re-run 'elevenlabs configure'")
	} else {
		ok("Default voice", cfg.DefaultVoiceID)
	}

	return renderDoctor(os.Stdout, checks, allOK, jsonOutput)
}
