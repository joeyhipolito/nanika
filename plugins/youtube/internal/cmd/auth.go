package cmd

import (
	"bufio"
	"flag"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-youtube/internal/api"
)

// AuthCmd manages OAuth2 tokens for the YouTube Data API.
//
//	youtube auth          — prints the authorization URL (or prompts interactively)
//	youtube auth --code c — exchanges code for tokens and saves them
func AuthCmd(args []string) error {
	fs := flag.NewFlagSet("auth", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	code := fs.String("code", "", "authorization code from the Google OAuth consent screen")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: youtube auth [--code <code>]

First-time OAuth2 setup:
  1. Run 'youtube auth' — copy the URL and open it in your browser.
  2. Authorize the app, copy the code shown by Google.
  3. Run 'youtube auth --code <code>' to save the token.

Requires client_id and client_secret in ~/.alluka/youtube-config.json.

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	cfg, err := api.LoadConfig()
	if err != nil {
		return fmt.Errorf("loading youtube config: %w", err)
	}
	if cfg.ClientID == "" {
		return fmt.Errorf("client_id missing from ~/.alluka/youtube-config.json")
	}

	if *code == "" {
		authURL := api.AuthURL(cfg.ClientID)
		fmt.Println("Open this URL in your browser to authorize youtube:")
		fmt.Println()
		fmt.Println(" ", authURL)
		fmt.Println()

		// Prompt interactively if stdin is a terminal.
		fi, err := os.Stdin.Stat()
		if err == nil && (fi.Mode()&os.ModeCharDevice) != 0 {
			fmt.Print("Paste the authorization code: ")
			scanner := bufio.NewScanner(os.Stdin)
			if scanner.Scan() {
				*code = strings.TrimSpace(scanner.Text())
			}
		}
		if *code == "" {
			fmt.Println("Re-run with: youtube auth --code <code>")
			return nil
		}
	}

	if cfg.ClientSecret == "" {
		return fmt.Errorf("client_secret missing from ~/.alluka/youtube-config.json")
	}
	token, err := api.ExchangeCode(cfg.ClientID, cfg.ClientSecret, *code)
	if err != nil {
		return fmt.Errorf("exchanging auth code: %w", err)
	}
	if err := api.SaveToken(token); err != nil {
		return fmt.Errorf("saving youtube token: %w", err)
	}

	fmt.Printf("YouTube OAuth2 token saved to ~/.alluka/youtube-oauth.json (expires %s)\n",
		token.Expiry.Format("2006-01-02 15:04 UTC"))
	return nil
}
