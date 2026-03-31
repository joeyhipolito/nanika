package cmd

import (
	"context"
	"fmt"
	"os"
	"strconv"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/browser"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
	"github.com/joeyhipolito/nanika-linkedin/internal/engage"
)

// EngageCmd scans the LinkedIn feed, scores posts, drafts comments, and optionally posts.
// Dry-run by default; use --post to actually engage.
//
// Deprecated: use 'engage run --platform linkedin' instead.
func EngageCmd(args []string, jsonOutput bool) error {
	fmt.Fprintln(os.Stderr, "DEPRECATED: use 'engage run --platform linkedin' instead")
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	ecfg := engage.Config{
		MaxComments: 3,
		MaxReacts:   8,
		JSONOutput:  jsonOutput,
	}

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`linkedin engage - Automated feed engagement

Usage:
  linkedin engage                              Dry-run: scan, score, draft — print results
  linkedin engage --post                       Actually post comments and reactions
  linkedin engage --persona <path>             Use persona voice file
  linkedin engage --posts-file <path>          Substack posts JSON for article grounding
  linkedin engage --site-url <url>             Your Substack URL (for article links)
  linkedin engage --max-comments 2             Cap comments per run (default: 3)
  linkedin engage --max-reacts 8               Cap reactions per run (default: 8)

Flags:
  --post                Actually post comments and reactions (default: dry-run)
  --persona <path>      Path to persona markdown file (## Identity section used as voice)
  --posts-file <path>   Path to substack posts JSON (default: ~/.linkedin/substack-posts.json)
  --site-url <url>      Your Substack site URL for article links (e.g. https://yourname.substack.com)
  --max-comments <N>    Maximum comments per run (default: 3)
  --max-reacts <N>      Maximum reactions per run (default: 8)
  --json                Output in JSON format

Decision thresholds:
  relevance >= 7  →  grounded comment with article link
  interest >= 6   →  opinion comment
  interest >= 4   →  react only
  else            →  skip

State: ~/.linkedin/engaged.json (auto-prunes after 30 days)

Article grounding: export your Substack posts with:
  substack posts --json > ~/.linkedin/substack-posts.json
`)
			return nil
		case "--post":
			ecfg.Post = true
		case "--persona":
			if i+1 >= len(args) {
				return fmt.Errorf("--persona requires a path argument")
			}
			i++
			ecfg.PersonaPath = args[i]
		case "--posts-file":
			if i+1 >= len(args) {
				return fmt.Errorf("--posts-file requires a path argument")
			}
			i++
			ecfg.PostsFile = args[i]
		case "--site-url":
			if i+1 >= len(args) {
				return fmt.Errorf("--site-url requires a URL argument")
			}
			i++
			ecfg.SiteURL = args[i]
		case "--max-comments":
			if i+1 >= len(args) {
				return fmt.Errorf("--max-comments requires a number")
			}
			i++
			n, err := strconv.Atoi(args[i])
			if err != nil || n <= 0 {
				return fmt.Errorf("--max-comments must be a positive number")
			}
			ecfg.MaxComments = n
		case "--max-reacts":
			if i+1 >= len(args) {
				return fmt.Errorf("--max-reacts requires a number")
			}
			i++
			n, err := strconv.Atoi(args[i])
			if err != nil || n <= 0 {
				return fmt.Errorf("--max-reacts must be a positive number")
			}
			ecfg.MaxReacts = n
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	// CDP is required for reading the feed
	chromeURL := cfg.ChromeDebugURL
	if chromeURL == "" {
		chromeURL = "http://localhost:9222"
	}
	cdpClient := browser.NewCDPClient(chromeURL)

	// OAuth client is optional — used for API-based comment/react
	var oauthClient *api.OAuthClient
	if cfg.AccessToken != "" && cfg.PersonURN != "" {
		oauthClient = api.NewOAuthClient(cfg.AccessToken, cfg.PersonURN)
	}

	if !jsonOutput && !ecfg.Post {
		fmt.Println("Dry-run mode (use --post to actually engage)")
		fmt.Println()
	}

	ctx := context.Background()
	result, err := engage.Run(ctx, cdpClient, oauthClient, ecfg)
	if err != nil {
		return err
	}

	return engage.PrintResult(result, jsonOutput)
}
