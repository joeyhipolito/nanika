package cmd

import (
	"fmt"
	"net/http"
	"os"
	"os/exec"
	"time"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
	"github.com/joeyhipolito/nanika-substack/internal/doctor"
)

// DoctorCheck is an alias for the shared doctor.Check type, exported for test compatibility.
type DoctorCheck = doctor.Check

// DoctorOutput is an alias for the shared doctor.Output type, exported for test compatibility.
type DoctorOutput = doctor.Output

// DoctorCmd runs diagnostic checks.
func DoctorCmd(jsonOutput bool) error {
	var checks []doctor.Check
	allOK := true

	// 1. Binary in PATH
	_, err := exec.LookPath("substack")
	if err != nil {
		checks = append(checks, doctor.Check{Name: "Binary", Status: "warn", Message: "substack not found in PATH"})
	} else {
		checks = append(checks, doctor.Check{Name: "Binary", Status: "ok", Message: "substack found in PATH"})
	}

	// 2. Config file exists
	if !config.Exists() {
		checks = append(checks, doctor.Check{Name: "Config file", Status: "fail", Message: "~/.substack/config not found"})
		allOK = false
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Substack Doctor", false)
	}
	checks = append(checks, doctor.Check{Name: "Config file", Status: "ok", Message: "~/.substack/config exists"})

	// 3. Config permissions
	perms, err := config.Permissions()
	if err != nil {
		checks = append(checks, doctor.Check{Name: "Config permissions", Status: "warn", Message: fmt.Sprintf("could not check: %v", err)})
	} else if perms != 0600 {
		checks = append(checks, doctor.Check{Name: "Config permissions", Status: "warn", Message: fmt.Sprintf("expected 0600, got %04o", perms)})
	} else {
		checks = append(checks, doctor.Check{Name: "Config permissions", Status: "ok", Message: "0600 (secure)"})
	}

	// 4. Config format — parseable with required fields
	cfg, err := config.Load()
	if err != nil {
		checks = append(checks, doctor.Check{Name: "Config format", Status: "fail", Message: fmt.Sprintf("parse error: %v", err)})
		allOK = false
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Substack Doctor", false)
	}
	if cfg.Cookie == "" || cfg.PublicationURL == "" || cfg.Subdomain == "" {
		missing := []string{}
		if cfg.Cookie == "" {
			missing = append(missing, "cookie")
		}
		if cfg.PublicationURL == "" {
			missing = append(missing, "publication_url")
		}
		if cfg.Subdomain == "" {
			missing = append(missing, "subdomain")
		}
		checks = append(checks, doctor.Check{Name: "Config format", Status: "fail", Message: fmt.Sprintf("missing fields: %v", missing)})
		allOK = false
		return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Substack Doctor", false)
	}
	checks = append(checks, doctor.Check{Name: "Config format", Status: "ok", Message: "all required fields present"})

	// 5. Cookie present
	if len(cfg.Cookie) < 10 {
		checks = append(checks, doctor.Check{Name: "Cookie present", Status: "fail", Message: "cookie value too short"})
		allOK = false
	} else {
		checks = append(checks, doctor.Check{Name: "Cookie present", Status: "ok", Message: "cookie is set"})
	}

	// 6. Auth valid — GET /api/v1/user/profile/self (global endpoint)
	client := api.NewClient(cfg.Subdomain, cfg.Cookie)
	user, err := client.GetProfile()
	if err != nil {
		checks = append(checks, doctor.Check{Name: "Auth valid", Status: "fail", Message: fmt.Sprintf("auth failed: %v", err)})
		allOK = false
	} else {
		checks = append(checks, doctor.Check{Name: "Auth valid", Status: "ok", Message: fmt.Sprintf("authenticated as %s", user.Name)})
	}

	// 7. Publication reachable
	httpClient := &http.Client{Timeout: 10 * time.Second}
	resp, err := httpClient.Get(cfg.PublicationURL)
	if err != nil {
		checks = append(checks, doctor.Check{Name: "Publication reachable", Status: "fail", Message: fmt.Sprintf("request failed: %v", err)})
		allOK = false
	} else {
		resp.Body.Close()
		if resp.StatusCode == http.StatusOK {
			checks = append(checks, doctor.Check{Name: "Publication reachable", Status: "ok", Message: cfg.PublicationURL})
		} else {
			checks = append(checks, doctor.Check{Name: "Publication reachable", Status: "warn", Message: fmt.Sprintf("HTTP %d", resp.StatusCode)})
		}
	}

	// 8. Subdomain matches URL
	expected := extractSubdomain(cfg.PublicationURL)
	if expected != cfg.Subdomain {
		checks = append(checks, doctor.Check{Name: "Subdomain match", Status: "warn", Message: fmt.Sprintf("config says %q, URL implies %q", cfg.Subdomain, expected)})
	} else {
		checks = append(checks, doctor.Check{Name: "Subdomain match", Status: "ok", Message: cfg.Subdomain})
	}

	return doctor.Render(os.Stdout, checks, allOK, jsonOutput, "Substack Doctor", false)
}
