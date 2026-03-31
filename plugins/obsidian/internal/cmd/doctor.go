package cmd

import (
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"time"

	"github.com/joeyhipolito/nanika-obsidian/internal/config"
	"github.com/joeyhipolito/nanika-obsidian/internal/index"
	"github.com/joeyhipolito/nanika-obsidian/internal/output"
	"github.com/joeyhipolito/nanika-obsidian/internal/vault"
)

// DoctorCheck represents a single doctor check result.
type DoctorCheck struct {
	Name    string `json:"name"`
	Status  string `json:"status"` // "ok", "warn", "fail"
	Message string `json:"message"`
}

// DoctorOutput represents the JSON output of the doctor command.
type DoctorOutput struct {
	Checks  []DoctorCheck `json:"checks"`
	Summary string        `json:"summary"`
	AllOK   bool          `json:"all_ok"`
}

// DoctorCmd validates the Obsidian CLI installation and configuration.
func DoctorCmd(jsonOutput bool) error {
	var checks []DoctorCheck
	allOK := true

	// 1. Check binary location
	binaryPath, err := exec.LookPath("obsidian")
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Binary",
			Status:  "warn",
			Message: "obsidian not found in PATH (running from local build?)",
		})
	} else {
		checks = append(checks, DoctorCheck{
			Name:    "Binary",
			Status:  "ok",
			Message: binaryPath,
		})
	}

	// 2. Check config file exists
	configPath := config.Path()
	if !config.Exists() {
		checks = append(checks, DoctorCheck{
			Name:    "Config file",
			Status:  "fail",
			Message: fmt.Sprintf("%s not found. Run 'obsidian configure'", configPath),
		})
		allOK = false
	} else {
		checks = append(checks, DoctorCheck{
			Name:    "Config file",
			Status:  "ok",
			Message: configPath,
		})

		// 3. Check config permissions
		perms, err := config.Permissions()
		if err != nil {
			checks = append(checks, DoctorCheck{
				Name:    "Config permissions",
				Status:  "fail",
				Message: fmt.Sprintf("Cannot read permissions: %v", err),
			})
			allOK = false
		} else if perms != 0600 {
			checks = append(checks, DoctorCheck{
				Name:    "Config permissions",
				Status:  "warn",
				Message: fmt.Sprintf("%o (should be 600). Fix: chmod 600 %s", perms, configPath),
			})
		} else {
			checks = append(checks, DoctorCheck{
				Name:    "Config permissions",
				Status:  "ok",
				Message: "600 (secure)",
			})
		}
	}

	// 4. Check config parseable and API key present
	cfg, err := config.Load()
	if err != nil {
		checks = append(checks, DoctorCheck{
			Name:    "Config format",
			Status:  "fail",
			Message: fmt.Sprintf("Failed to parse config: %v", err),
		})
		allOK = false
	} else {
		apiKey := cfg.GeminiAPIKey
		if apiKey == "" {
			apiKey = os.Getenv("GEMINI_API_KEY")
		}

		if apiKey == "" {
			checks = append(checks, DoctorCheck{
				Name:    "Gemini API key",
				Status:  "fail",
				Message: "Not found in config or GEMINI_API_KEY env var",
			})
			allOK = false
		} else {
			masked := maskKey(apiKey)
			checks = append(checks, DoctorCheck{
				Name:    "Gemini API key",
				Status:  "ok",
				Message: fmt.Sprintf("Present (%s)", masked),
			})
		}

		// 5. Check vault path
		vaultPath := cfg.VaultPath
		if vaultPath == "" {
			vaultPath = os.Getenv("OBSIDIAN_VAULT_PATH")
		}
		if vaultPath == "" {
			checks = append(checks, DoctorCheck{
				Name:    "Vault path",
				Status:  "fail",
				Message: "Not set. Run 'obsidian configure'",
			})
			allOK = false
		} else {
			info, err := os.Stat(vaultPath)
			if err != nil {
				checks = append(checks, DoctorCheck{
					Name:    "Vault path",
					Status:  "fail",
					Message: fmt.Sprintf("Cannot access: %v", err),
				})
				allOK = false
			} else if !info.IsDir() {
				checks = append(checks, DoctorCheck{
					Name:    "Vault path",
					Status:  "fail",
					Message: fmt.Sprintf("Not a directory: %s", vaultPath),
				})
				allOK = false
			} else {
				checks = append(checks, DoctorCheck{
					Name:    "Vault path",
					Status:  "ok",
					Message: vaultPath,
				})
			}
		}

		// 6. Check search index
		if vaultPath != "" {
			dbPath := index.IndexDBPath(vaultPath)
			if info, err := os.Stat(dbPath); err != nil {
				checks = append(checks, DoctorCheck{
					Name:    "Search index",
					Status:  "warn",
					Message: "Not yet built. Run 'obsidian index'",
				})
			} else {
				store, err := index.Open(dbPath)
				if err != nil {
					checks = append(checks, DoctorCheck{
						Name:    "Search index",
						Status:  "fail",
						Message: fmt.Sprintf("Cannot open: %v", err),
					})
					allOK = false
				} else {
					count, _ := store.NoteCount()
					store.Close()
					checks = append(checks, DoctorCheck{
						Name:    "Search index",
						Status:  "ok",
						Message: fmt.Sprintf("%d notes indexed (%s, %d bytes)", count, dbPath, info.Size()),
					})
				}
			}
		}

		// 7. Check inbox triage status
		if vaultPath != "" {
			checks = append(checks, checkInboxStatus(vaultPath))
		}
	}

	// Determine summary
	summary := "All checks passed!"
	if !allOK {
		failCount := 0
		for _, c := range checks {
			if c.Status == "fail" {
				failCount++
			}
		}
		summary = fmt.Sprintf("%d check(s) failed. Run 'obsidian configure' to fix.", failCount)
	}

	// JSON output
	if jsonOutput {
		return output.JSON(DoctorOutput{
			Checks:  checks,
			Summary: summary,
			AllOK:   allOK,
		})
	}

	// Human-readable output
	fmt.Println("Obsidian CLI Doctor")
	fmt.Println("===================")
	fmt.Println()

	for _, c := range checks {
		var icon string
		switch c.Status {
		case "ok":
			icon = "OK"
		case "warn":
			icon = "WARN"
		case "fail":
			icon = "FAIL"
		}
		fmt.Printf("  [%4s] %-20s %s\n", icon, c.Name+":", c.Message)
	}

	fmt.Println()
	if allOK {
		fmt.Println(summary)
	} else {
		fmt.Println(summary)
		return fmt.Errorf("doctor checks failed")
	}

	return nil
}

// checkInboxStatus returns a DoctorCheck describing how many notes are pending
// triage in the Inbox/ folder and how old the oldest one is.
func checkInboxStatus(vaultPath string) DoctorCheck {
	inboxPath := filepath.Join(vaultPath, "Inbox")
	if _, err := os.Stat(inboxPath); os.IsNotExist(err) {
		return DoctorCheck{
			Name:    "Inbox",
			Status:  "ok",
			Message: "no inbox folder (Inbox/ not found)",
		}
	}

	notes, err := vault.ListNotes(vaultPath, "Inbox")
	if err != nil {
		return DoctorCheck{
			Name:    "Inbox",
			Status:  "warn",
			Message: fmt.Sprintf("cannot read inbox: %v", err),
		}
	}

	now := time.Now()
	pending := 0
	oldestDays := 0

	for _, info := range notes {
		fullPath := filepath.Join(vaultPath, info.Path)
		data, err := os.ReadFile(fullPath)
		if err != nil {
			continue
		}
		parsed := vault.ParseNote(string(data))
		if frontmatterString(parsed.Frontmatter, "status") == "processed" {
			continue
		}
		pending++
		ageDays, _ := computeAge(frontmatterString(parsed.Frontmatter, "created"), info.ModTime, now)
		if ageDays > oldestDays {
			oldestDays = ageDays
		}
	}

	if pending == 0 {
		return DoctorCheck{
			Name:    "Inbox",
			Status:  "ok",
			Message: "clear",
		}
	}
	return DoctorCheck{
		Name:    "Inbox",
		Status:  "warn",
		Message: fmt.Sprintf("%d notes pending triage, oldest: %dd", pending, oldestDays),
	}
}
