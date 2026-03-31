package cmd

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/browser"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
)

var validReactions = map[string]bool{
	"LIKE":         true,
	"CELEBRATE":    true,
	"SUPPORT":      true,
	"LOVE":         true,
	"INSIGHTFUL":   true,
	"FUNNY":        true,
	// Official API names
	"PRAISE":       true,
	"EMPATHY":      true,
	"INTEREST":     true,
	"APPRECIATION": true,
	"ENTERTAINMENT": true,
}

// Friendly name mappings for common aliases
var reactionAliases = map[string]string{
	"CELEBRATE": "PRAISE",
	"SUPPORT":   "EMPATHY",
	"LOVE":      "EMPATHY",
	"INSIGHTFUL": "INTEREST",
	"FUNNY":     "ENTERTAINMENT",
}

// ReactCmd reacts to a LinkedIn post via the Official API.
func ReactCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 {
		return fmt.Errorf("usage: linkedin react <activity-urn-or-id> [--type LIKE|CELEBRATE|SUPPORT|LOVE|INSIGHTFUL|FUNNY] [--yes]")
	}

	urn := normalizeActivityURN(args[0])
	reactionType := "LIKE"
	skipConfirm := false

	// Parse remaining flags
	for i := 1; i < len(args); i++ {
		switch args[i] {
		case "--type":
			if i+1 < len(args) {
				i++
				reactionType = strings.ToUpper(args[i])
			} else {
				return fmt.Errorf("--type requires a value")
			}
		case "--yes", "-y":
			skipConfirm = true
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	// CDP-only path: no API credentials but Chrome debug port configured.
	if (cfg.AccessToken == "" || cfg.PersonURN == "") && cfg.ChromeDebugURL != "" {
		return reactViaCDPFallback(cfg.ChromeDebugURL, urn, skipConfirm, jsonOutput)
	}
	if cfg.AccessToken == "" || cfg.PersonURN == "" {
		return fmt.Errorf("OAuth not configured. Run 'linkedin configure' first")
	}

	if !validReactions[reactionType] {
		return fmt.Errorf("invalid reaction type: %s. Valid types: LIKE, CELEBRATE, SUPPORT, LOVE, INSIGHTFUL, FUNNY", reactionType)
	}

	// Map friendly names to Official API names
	if mapped, ok := reactionAliases[reactionType]; ok {
		reactionType = mapped
	}

	// Confirmation prompt
	if !skipConfirm {
		fmt.Printf("React to %s with %s?\n", urn, reactionType)
		fmt.Print("Confirm? [y/N] ")
		reader := bufio.NewReader(os.Stdin)
		answer, err := reader.ReadString('\n')
		if err != nil {
			return fmt.Errorf("reading input: %w", err)
		}
		answer = strings.TrimSpace(strings.ToLower(answer))
		if answer != "y" && answer != "yes" {
			fmt.Println("Cancelled.")
			return nil
		}
	}

	client := api.NewOAuthClient(cfg.AccessToken, cfg.PersonURN)
	if err := client.CreateReaction(urn, reactionType); err != nil {
		// CDP fallback: try browser automation when the API fails and Chrome is configured.
		// CDP only supports LIKE (clicking the Like button); other reaction types are skipped.
		if cfg.ChromeDebugURL != "" && reactionType == "LIKE" {
			fmt.Fprintf(os.Stderr, "API reaction failed (%v), retrying via browser...\n", err)
			return reactViaCDPFallback(cfg.ChromeDebugURL, urn, true /* already confirmed */, jsonOutput)
		}
		return fmt.Errorf("reacting to post: %w", err)
	}

	if jsonOutput {
		return json.NewEncoder(os.Stdout).Encode(struct {
			Status      string `json:"status"`
			ActivityURN string `json:"activity_urn"`
			Reaction    string `json:"reaction"`
		}{"ok", urn, reactionType})
	}
	fmt.Printf("Reacted with %s.\n", reactionType)
	return nil
}

// reactViaCDPFallback reacts to a post via Chrome browser automation.
// Only LIKE is supported (clicks the Like button in the post UI).
// skipConfirm suppresses the confirmation prompt; pass true when the caller
// has already obtained confirmation (e.g. API failure after API-path confirm).
func reactViaCDPFallback(chromeURL, urn string, skipConfirm, jsonOutput bool) error {
	if !skipConfirm {
		fmt.Printf("React to %s with LIKE (via browser)?\n", urn)
		fmt.Print("Confirm? [y/N] ")
		reader := bufio.NewReader(os.Stdin)
		answer, err := reader.ReadString('\n')
		if err != nil {
			return fmt.Errorf("reading input: %w", err)
		}
		if answer = strings.TrimSpace(strings.ToLower(answer)); answer != "y" && answer != "yes" {
			fmt.Println("Cancelled.")
			return nil
		}
	}
	cdp := browser.NewCDPClient(chromeURL)
	if err := cdp.ReactViaCDP(urn); err != nil {
		return fmt.Errorf("browser react fallback: %w", err)
	}
	if jsonOutput {
		return json.NewEncoder(os.Stdout).Encode(struct {
			Status      string `json:"status"`
			ActivityURN string `json:"activity_urn"`
			Reaction    string `json:"reaction"`
			Via         string `json:"via"`
		}{"ok", urn, "LIKE", "browser"})
	}
	fmt.Println("Reacted with LIKE (via browser).")
	return nil
}
