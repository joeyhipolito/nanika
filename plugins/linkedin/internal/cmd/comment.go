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

// CommentCmd posts a comment on a LinkedIn post via the Official API.
func CommentCmd(args []string, jsonOutput bool) error {
	if len(args) < 2 {
		return fmt.Errorf("usage: linkedin comment <activity-urn-or-id> <text> [--yes]")
	}

	urn := normalizeActivityURN(args[0])

	// Collect text and flags from remaining args
	skipConfirm := false
	var textParts []string
	for _, arg := range args[1:] {
		if arg == "--yes" || arg == "-y" {
			skipConfirm = true
		} else {
			textParts = append(textParts, arg)
		}
	}

	text := strings.Join(textParts, " ")
	if text == "" {
		return fmt.Errorf("comment text is required")
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	// CDP-only path: no API credentials but Chrome debug port configured.
	cdpURL := cfg.ChromeDebugURL
	if cdpURL == "" {
		cdpURL = "ws://localhost:9222"
	}
	if (cfg.AccessToken == "" || cfg.PersonURN == "") && cdpURL != "" {
		return commentViaCDPFallback(cdpURL, urn, text, skipConfirm, jsonOutput)
	}
	if cfg.AccessToken == "" || cfg.PersonURN == "" {
		return fmt.Errorf("OAuth not configured. Run 'linkedin configure' first")
	}

	// Confirmation prompt
	if !skipConfirm {
		fmt.Printf("Post comment on %s?\n\n", urn)
		fmt.Printf("  \"%s\"\n\n", text)
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
	if err := client.CreateComment(urn, text); err != nil {
		// CDP fallback: try browser automation when the API fails and Chrome is configured.
		if cfg.ChromeDebugURL != "" {
			fmt.Fprintf(os.Stderr, "API comment failed (%v), retrying via browser...\n", err)
			return commentViaCDPFallback(cfg.ChromeDebugURL, urn, text, true /* already confirmed */, jsonOutput)
		}
		return fmt.Errorf("posting comment: %w", err)
	}

	if jsonOutput {
		return json.NewEncoder(os.Stdout).Encode(struct {
			Status      string `json:"status"`
			ActivityURN string `json:"activity_urn"`
			Comment     string `json:"comment"`
		}{"ok", urn, text})
	}
	fmt.Println("Comment posted.")
	return nil
}

// commentViaCDPFallback posts a comment via Chrome browser automation.
// skipConfirm suppresses the confirmation prompt; pass true when the caller
// has already obtained confirmation (e.g. API failure after API-path confirm).
func commentViaCDPFallback(chromeURL, urn, text string, skipConfirm, jsonOutput bool) error {
	if !skipConfirm {
		fmt.Printf("Post comment on %s?\n\n", urn)
		fmt.Printf("  \"%s\"\n\n", text)
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
	if err := cdp.CommentViaCDP(urn, text); err != nil {
		return fmt.Errorf("browser comment fallback: %w", err)
	}
	if jsonOutput {
		return json.NewEncoder(os.Stdout).Encode(struct {
			Status      string `json:"status"`
			ActivityURN string `json:"activity_urn"`
			Comment     string `json:"comment"`
			Via         string `json:"via"`
		}{"ok", urn, text, "browser"})
	}
	fmt.Println("Comment posted (via browser).")
	return nil
}
