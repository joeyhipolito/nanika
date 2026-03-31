package cmd

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"strings"
	"time"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

var validAudiences = map[string]bool{
	"everyone":  true,
	"only_paid": true,
	"founding":  true,
	"only_free": true,
}

// PublishCmd handles the publish subcommand.
// It runs the full three-step scheduling flow:
//  1. PUT draft (update audience setting)
//  2. GET prepublish (validate and retrieve subscriber counts/warnings)
//  3. POST scheduled_release (schedule the post)
func PublishCmd(args []string, jsonOutput bool) error {
	var draftIDStr string
	var atStr string
	postAudience := "everyone"
	emailAudience := "everyone"
	dryRun := false

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--at":
			if i+1 >= len(args) {
				return fmt.Errorf("--at requires an RFC3339 datetime value (e.g. 2026-03-01T10:00:00Z)")
			}
			i++
			atStr = args[i]
		case "--post-audience":
			if i+1 >= len(args) {
				return fmt.Errorf("--post-audience requires a value (everyone, only_paid, founding, only_free)")
			}
			i++
			postAudience = args[i]
		case "--email-audience":
			if i+1 >= len(args) {
				return fmt.Errorf("--email-audience requires a value (everyone, only_paid, founding, only_free)")
			}
			i++
			emailAudience = args[i]
		case "--dry-run":
			dryRun = true
		default:
			if draftIDStr == "" {
				draftIDStr = args[i]
			} else {
				return fmt.Errorf("unexpected argument: %s", args[i])
			}
		}
	}

	if draftIDStr == "" {
		return fmt.Errorf("usage: substack publish <draft-id> --at <RFC3339> [--post-audience everyone] [--email-audience everyone] [--dry-run]")
	}
	if atStr == "" {
		return fmt.Errorf("--at is required: provide a future RFC3339 datetime (e.g. 2026-03-01T10:00:00Z)")
	}

	draftID, err := strconv.Atoi(draftIDStr)
	if err != nil {
		return fmt.Errorf("invalid draft ID %q: must be an integer", draftIDStr)
	}

	publishAt, err := time.Parse(time.RFC3339, atStr)
	if err != nil {
		return fmt.Errorf("invalid --at value %q: must be RFC3339 format (e.g. 2026-03-01T10:00:00Z): %w", atStr, err)
	}
	if !publishAt.After(time.Now()) {
		return fmt.Errorf("--at %s is in the past: scheduled time must be in the future", atStr)
	}

	if !validAudiences[postAudience] {
		return fmt.Errorf("invalid --post-audience %q: must be one of everyone, only_paid, founding, only_free", postAudience)
	}
	if !validAudiences[emailAudience] {
		return fmt.Errorf("invalid --email-audience %q: must be one of everyone, only_paid, founding, only_free", emailAudience)
	}

	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)
	ctx := context.Background()

	// Confirm the draft exists and capture its metadata.
	draft, err := client.GetDraft(draftID)
	if err != nil {
		return fmt.Errorf("fetching draft %d: %w", draftID, err)
	}

	if !jsonOutput {
		fmt.Printf("Publishing draft: %s (ID: %d)\n", draft.Title, draftID)
	}

	// Step 1: PUT draft — set audience before scheduling.
	// Skip mutation when --dry-run is set to avoid side effects.
	if !dryRun {
		if err := client.SetDraftAudience(ctx, draftID, postAudience); err != nil {
			return fmt.Errorf("updating draft audience: %w", err)
		}
	}

	// Step 2: GET prepublish — validate and retrieve subscriber counts/warnings.
	fmt.Printf("Running prepublish checks for %s...\n", publishAt.UTC().Format(time.RFC3339))
	result, err := client.PrePublish(ctx, draftID, publishAt)
	if err != nil {
		return fmt.Errorf("prepublish check: %w", err)
	}

	if !jsonOutput {
		fmt.Printf("  Free subscribers:  %d\n", result.FreeSubscriberCount)
		fmt.Printf("  Paid subscribers:  %d\n", result.PaidSubscriberCount)
		fmt.Printf("  Email recipients:  %d\n", result.EmailSubscriberCount)
	}

	// Print warnings and prompt (or fail in --json mode).
	if len(result.Warnings) > 0 {
		if jsonOutput {
			type warningOutput struct {
				Error    string   `json:"error"`
				Warnings []string `json:"warnings"`
			}
			enc := json.NewEncoder(os.Stdout)
			enc.SetIndent("", "  ")
			_ = enc.Encode(warningOutput{
				Error:    "prepublish warnings require manual confirmation; re-run without --json to proceed",
				Warnings: result.Warnings,
			})
			return fmt.Errorf("prepublish returned %d warning(s)", len(result.Warnings))
		}

		fmt.Println("Prepublish warnings:")
		for _, w := range result.Warnings {
			fmt.Printf("  ! %s\n", w)
		}
		fmt.Print("Continue with scheduling? [y/N] ")
		scanner := bufio.NewScanner(os.Stdin)
		scanner.Scan()
		answer := strings.TrimSpace(strings.ToLower(scanner.Text()))
		if answer != "y" && answer != "yes" {
			return fmt.Errorf("scheduling cancelled")
		}
	}

	// --dry-run stops after prepublish.
	if dryRun {
		if jsonOutput {
			type dryRunOutput struct {
				DraftID       int      `json:"draft_id"`
				Title         string   `json:"title"`
				PublishAt     string   `json:"publish_at"`
				PostAudience  string   `json:"post_audience"`
				EmailAudience string   `json:"email_audience"`
				DryRun        bool     `json:"dry_run"`
				Warnings      []string `json:"warnings"`
			}
			enc := json.NewEncoder(os.Stdout)
			enc.SetIndent("", "  ")
			return enc.Encode(dryRunOutput{
				DraftID:       draftID,
				Title:         draft.Title,
				PublishAt:     publishAt.UTC().Format(time.RFC3339),
				PostAudience:  postAudience,
				EmailAudience: emailAudience,
				DryRun:        true,
				Warnings:      result.Warnings,
			})
		}
		fmt.Println("Dry run complete. Would schedule:")
		fmt.Printf("  Draft ID:       %d\n", draftID)
		fmt.Printf("  Title:          %s\n", draft.Title)
		fmt.Printf("  Scheduled for:  %s\n", publishAt.UTC().Format(time.RFC3339))
		fmt.Printf("  Post audience:  %s\n", postAudience)
		fmt.Printf("  Email audience: %s\n", emailAudience)
		return nil
	}

	// Step 3: POST scheduled_release.
	fmt.Printf("Scheduling release for %s...\n", publishAt.UTC().Format(time.RFC3339))
	if err := client.ScheduleRelease(ctx, draftID, publishAt, postAudience, emailAudience); err != nil {
		return fmt.Errorf("scheduling release: %w", err)
	}

	postURL := fmt.Sprintf("%s/p/%s", cfg.PublicationURL, draft.Slug)

	if jsonOutput {
		// publishOutput mirrors the channels.substack schema in velite.config.ts.
		type publishOutput struct {
			ID            int    `json:"id"`
			Title         string `json:"title"`
			Slug          string `json:"slug,omitempty"`
			URL           string `json:"url"`
			ScheduledFor  string `json:"scheduled_for"`
			PostAudience  string `json:"post_audience"`
			EmailAudience string `json:"email_audience"`
			Audience      string `json:"audience,omitempty"`
			Status        string `json:"status"`
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(publishOutput{
			ID:            draftID,
			Title:         draft.Title,
			Slug:          draft.Slug,
			URL:           postURL,
			ScheduledFor:  publishAt.UTC().Format(time.RFC3339),
			PostAudience:  postAudience,
			EmailAudience: emailAudience,
			Audience:      postAudience,
			Status:        "scheduled",
		})
	}

	fmt.Printf("Scheduled: %s\n", postURL)
	fmt.Printf("  Publish time:   %s\n", publishAt.UTC().Format(time.RFC3339))
	fmt.Printf("  Post audience:  %s\n", postAudience)
	fmt.Printf("  Email audience: %s\n", emailAudience)
	return nil
}
