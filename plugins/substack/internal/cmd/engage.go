package cmd

import (
	"context"
	"fmt"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
	"github.com/joeyhipolito/nanika-substack/internal/engage"
)

// EngageCmd scans dashboard notes, scores them, drafts comments, and optionally posts.
// Dry-run by default; use --post to actually engage.
func EngageCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	ecfg := engage.Config{
		MaxComments: 3,
		MaxReacts:   8,
		JSONOutput:  jsonOutput,
		SiteURL:     fmt.Sprintf("https://%s.substack.com", cfg.Subdomain),
	}

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`substack engage - Automated feed engagement

Usage:
  substack engage                              Dry-run: scan, score, draft
  substack engage --post                       Actually post comments and reactions
  substack engage --persona <path>             Use persona voice (default: storyteller)
  substack engage --max-comments 2             Cap comments per run (default: 3)
  substack engage --max-reacts 8               Cap reactions per run (default: 8)

Flags:
  --post                Actually post comments and reactions
  --persona <path>      Path to persona markdown file
  --max-comments <N>    Maximum comments per run (default: 3)
  --max-reacts <N>      Maximum reactions per run (default: 8)
  --json                Output in JSON format
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
		case "--max-comments":
			if i+1 >= len(args) {
				return fmt.Errorf("--max-comments requires a number")
			}
			i++
			n := parseIntArg(args[i])
			if n <= 0 {
				return fmt.Errorf("--max-comments must be a positive number")
			}
			ecfg.MaxComments = n
		case "--max-reacts":
			if i+1 >= len(args) {
				return fmt.Errorf("--max-reacts requires a number")
			}
			i++
			n := parseIntArg(args[i])
			if n <= 0 {
				return fmt.Errorf("--max-reacts must be a positive number")
			}
			ecfg.MaxReacts = n
		default:
			return fmt.Errorf("unexpected argument: %s", args[i])
		}
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)
	ctx := context.Background()

	if !jsonOutput && !ecfg.Post {
		fmt.Println("Dry-run mode (use --post to actually engage)")
		fmt.Println()
	}

	result, err := engage.Run(ctx, client, ecfg)
	if err != nil {
		return err
	}

	return engage.PrintResult(result, jsonOutput)
}
