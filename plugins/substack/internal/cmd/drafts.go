package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// DraftsCmd lists current drafts.
func DraftsCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags
	limit := 25
	offset := 0
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--limit":
			if i+1 < len(args) {
				i++
				n := parseIntArg(args[i])
				if n > 0 {
					limit = n
				}
			}
		case "--offset":
			if i+1 < len(args) {
				i++
				n := parseIntArg(args[i])
				if n >= 0 {
					offset = n
				}
			}
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)
	drafts, err := client.GetDrafts(offset, limit)
	if err != nil {
		return fmt.Errorf("fetching drafts: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(drafts)
	}

	if len(drafts) == 0 {
		fmt.Println("No drafts found.")
		return nil
	}

	fmt.Printf("Drafts (%d):\n\n", len(drafts))
	for _, d := range drafts {
		date := d.WrittenAt
		if date == "" {
			date = "no date"
		}
		url := fmt.Sprintf("%s/publish/post/%d", cfg.PublicationURL, d.ID)
		fmt.Printf("  %s\n", d.Title)
		fmt.Printf("    Date: %s\n", date)
		fmt.Printf("    URL:  %s\n\n", url)
	}

	return nil
}

func parseIntArg(s string) int {
	n := 0
	for _, c := range s {
		if c < '0' || c > '9' {
			return -1
		}
		n = n*10 + int(c-'0')
	}
	return n
}
