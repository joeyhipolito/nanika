package engage

import (
	"context"
	"encoding/json"
	"fmt"
	"math/rand"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/browser"
)

// Config holds the settings for an engage run.
type Config struct {
	Post        bool   // Actually post (false = dry-run)
	PersonaPath string // Path to persona markdown file
	PostsFile   string // Path to substack posts JSON for article grounding
	MaxComments int    // Max comments per run
	MaxReacts   int    // Max reactions per run
	JSONOutput  bool
	SiteURL     string // e.g. https://yourname.substack.com (for article links)
}

// Result holds the output of an engage run.
type Result struct {
	ItemsScanned int             `json:"items_scanned"`
	ItemsSkipped int             `json:"items_skipped"`
	Comments     []CommentResult `json:"comments"`
	Reacts       []ReactResult   `json:"reacts"`
	Errors       []string        `json:"errors,omitempty"`
}

// CommentResult describes a posted (or drafted) comment.
type CommentResult struct {
	ActivityURN string `json:"activity_urn"`
	AuthorName  string `json:"author_name"`
	Type        string `json:"type"` // "grounded" or "opinion"
	Article     string `json:"matched_article,omitempty"`
	Comment     string `json:"comment"`
	Posted      bool   `json:"posted"`
}

// ReactResult describes a reaction.
type ReactResult struct {
	ActivityURN string `json:"activity_urn"`
	AuthorName  string `json:"author_name"`
	Posted      bool   `json:"posted"`
}

// Run executes the full engage pipeline: scan → score → draft → act → record.
// cdpClient is used for feed reading (and comment/react fallback if API fails).
// oauthClient may be nil when only CDP credentials are available.
func Run(ctx context.Context, cdpClient *browser.CDPClient, oauthClient *api.OAuthClient, cfg Config) (*Result, error) {
	result := &Result{}

	// Load voice
	voice := defaultVoice
	if cfg.PersonaPath != "" {
		v, err := LoadPersonaVoice(cfg.PersonaPath)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Sprintf("loading persona: %v (using default voice)", err))
		} else {
			voice = v
		}
	}

	// Load state
	state, err := LoadState()
	if err != nil {
		return nil, fmt.Errorf("loading state: %w", err)
	}

	// 1. SCAN: Fetch feed via CDP, dedupe, filter
	candidates, err := scanFeed(cdpClient, state)
	if err != nil {
		return nil, fmt.Errorf("scanning feed: %w", err)
	}
	result.ItemsScanned = len(candidates)

	if len(candidates) == 0 {
		return result, nil
	}

	// Load Substack posts for article grounding
	var posts []SubstackPost
	if cfg.PostsFile != "" {
		ps, err := loadSubstackPosts(cfg.PostsFile)
		if err != nil {
			result.Errors = append(result.Errors, fmt.Sprintf("loading substack posts: %v (opinion-only mode)", err))
		} else {
			posts = ps
		}
	} else {
		// Try default location
		home, _ := os.UserHomeDir()
		defaultPath := home + "/.linkedin/substack-posts.json"
		if ps, err := loadSubstackPosts(defaultPath); err == nil {
			posts = ps
		}
		// If not found, silently run in opinion-only mode
	}

	// 2. SCORE
	scores, err := ScoreFeedItems(ctx, candidates, posts)
	if err != nil {
		result.Errors = append(result.Errors, fmt.Sprintf("scoring failed: %v (fallback: react to top posts)", err))
		scores = fallbackScores(candidates)
	}

	// Build lookup maps
	candidateByURN := make(map[string]api.FeedItem)
	for _, c := range candidates {
		candidateByURN[c.ActivityURN] = c
	}
	postBySlug := make(map[string]SubstackPost)
	for _, p := range posts {
		postBySlug[p.Slug] = p
	}

	// 3. DECIDE + 4. DRAFT + 5. ACT
	commentCount := 0
	reactCount := 0

	for _, score := range scores {
		item, ok := candidateByURN[score.ActivityURN]
		if !ok {
			continue
		}

		// Grounded comment (relevance >= 7)
		if score.Relevance >= 7 && commentCount < cfg.MaxComments {
			post, hasPost := postBySlug[score.MatchedArticle]
			if hasPost {
				draft, err := DraftGroundedComment(ctx, item, post, cfg.SiteURL, voice)
				if err != nil {
					result.Errors = append(result.Errors, fmt.Sprintf("drafting grounded comment for %s: %v", item.ActivityURN, err))
					continue
				}

				cr := CommentResult{
					ActivityURN: item.ActivityURN,
					AuthorName:  item.AuthorName,
					Type:        "grounded",
					Article:     post.Slug,
					Comment:     draft.Comment,
				}

				if cfg.Post {
					if err := postComment(cdpClient, oauthClient, state, item.ActivityURN, draft.Comment, item.AuthorName); err != nil {
						result.Errors = append(result.Errors, fmt.Sprintf("posting comment on %s: %v", item.ActivityURN, err))
						cr.Posted = false
					} else {
						cr.Posted = true
					}
					jitter(2, 8)
				}

				result.Comments = append(result.Comments, cr)
				commentCount++
				continue
			}
		}

		// Opinion comment (interest >= 6)
		if score.Interest >= 6 && commentCount < cfg.MaxComments {
			draft, err := DraftOpinionComment(ctx, item, voice)
			if err != nil {
				result.Errors = append(result.Errors, fmt.Sprintf("drafting opinion comment for %s: %v", item.ActivityURN, err))
				continue
			}

			cr := CommentResult{
				ActivityURN: item.ActivityURN,
				AuthorName:  item.AuthorName,
				Type:        "opinion",
				Comment:     draft.Comment,
			}

			if cfg.Post {
				if err := postComment(cdpClient, oauthClient, state, item.ActivityURN, draft.Comment, item.AuthorName); err != nil {
					result.Errors = append(result.Errors, fmt.Sprintf("posting comment on %s: %v", item.ActivityURN, err))
					cr.Posted = false
				} else {
					cr.Posted = true
				}
				jitter(2, 8)
			}

			result.Comments = append(result.Comments, cr)
			commentCount++
			continue
		}

		// React only (interest >= 4)
		if score.Interest >= 4 && reactCount < cfg.MaxReacts {
			rr := ReactResult{
				ActivityURN: item.ActivityURN,
				AuthorName:  item.AuthorName,
			}

			if cfg.Post {
				if err := postReaction(cdpClient, oauthClient, state, item.ActivityURN, item.AuthorName); err != nil {
					result.Errors = append(result.Errors, fmt.Sprintf("reacting to %s: %v", item.ActivityURN, err))
					rr.Posted = false
				} else {
					rr.Posted = true
				}
				jitter(1, 3)
			}

			result.Reacts = append(result.Reacts, rr)
			reactCount++
		}
	}

	result.ItemsSkipped = result.ItemsScanned - len(result.Comments) - len(result.Reacts)
	return result, nil
}

// PrintResult outputs the result in text or JSON format.
func PrintResult(result *Result, jsonOutput bool) error {
	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(result)
	}

	fmt.Printf("Scanned %d feed items\n", result.ItemsScanned)

	if len(result.Comments) > 0 {
		fmt.Printf("\nComments (%d):\n", len(result.Comments))
		for _, c := range result.Comments {
			status := "drafted"
			if c.Posted {
				status = "posted"
			}
			fmt.Printf("  [%s] %s on post by %s\n", status, c.Type, c.AuthorName)
			fmt.Printf("    %s\n", PostURL(c.ActivityURN))
			if c.Article != "" {
				fmt.Printf("    linked: %s\n", c.Article)
			}
			fmt.Printf("    %s\n", c.Comment)
		}
	}

	if len(result.Reacts) > 0 {
		fmt.Printf("\nReactions (%d):\n", len(result.Reacts))
		for _, r := range result.Reacts {
			status := "planned"
			if r.Posted {
				status = "sent"
			}
			fmt.Printf("  [%s] post by %s — %s\n", status, r.AuthorName, PostURL(r.ActivityURN))
		}
	}

	if len(result.Errors) > 0 {
		fmt.Printf("\nWarnings (%d):\n", len(result.Errors))
		for _, e := range result.Errors {
			fmt.Printf("  - %s\n", e)
		}
	}

	if result.ItemsSkipped > 0 {
		fmt.Printf("\nSkipped %d items (low score or already engaged)\n", result.ItemsSkipped)
	}

	return nil
}

// scanFeed fetches the LinkedIn feed via CDP, dedupes, and filters already-engaged items.
func scanFeed(cdpClient *browser.CDPClient, state *State) ([]api.FeedItem, error) {
	items, err := cdpClient.GetFeed(40)
	if err != nil {
		return nil, fmt.Errorf("fetching feed: %w", err)
	}

	seen := make(map[string]bool)
	var candidates []api.FeedItem

	for _, item := range items {
		if item.ActivityURN == "" {
			continue
		}
		if item.Text == "" {
			continue
		}
		if state.IsEngaged(item.ActivityURN) {
			continue
		}
		if seen[item.ActivityURN] {
			continue
		}
		seen[item.ActivityURN] = true

		candidates = append(candidates, item)
	}

	return candidates, nil
}

// postComment posts a comment via the OAuth API; falls back to CDP if API fails or is unconfigured.
func postComment(cdpClient *browser.CDPClient, oauthClient *api.OAuthClient, state *State, urn, text, authorName string) error {
	if oauthClient != nil {
		if err := oauthClient.CreateComment(urn, text); err != nil {
			// Fall back to CDP if available
			if cdpClient != nil {
				fmt.Fprintf(os.Stderr, "API comment failed (%v), retrying via browser...\n", err)
				if cdpErr := cdpClient.CommentViaCDP(urn, text); cdpErr != nil {
					state.Record(urn, "failed", authorName)
					return fmt.Errorf("browser comment fallback: %w", cdpErr)
				}
				return state.Record(urn, "comment", authorName)
			}
			state.Record(urn, "failed", authorName)
			return fmt.Errorf("posting comment: %w", err)
		}
		return state.Record(urn, "comment", authorName)
	}

	// No OAuth client — use CDP directly
	if cdpClient == nil {
		return fmt.Errorf("no API client or browser available for commenting")
	}
	if err := cdpClient.CommentViaCDP(urn, text); err != nil {
		state.Record(urn, "failed", authorName)
		return fmt.Errorf("browser comment: %w", err)
	}
	return state.Record(urn, "comment", authorName)
}

// postReaction reacts to a post via the OAuth API; falls back to CDP if API fails or is unconfigured.
func postReaction(cdpClient *browser.CDPClient, oauthClient *api.OAuthClient, state *State, urn, authorName string) error {
	if oauthClient != nil {
		if err := oauthClient.CreateReaction(urn, "LIKE"); err != nil {
			// Fall back to CDP if available
			if cdpClient != nil {
				fmt.Fprintf(os.Stderr, "API reaction failed (%v), retrying via browser...\n", err)
				if cdpErr := cdpClient.ReactViaCDP(urn); cdpErr != nil {
					state.Record(urn, "failed", authorName)
					return fmt.Errorf("browser react fallback: %w", cdpErr)
				}
				return state.Record(urn, "react", authorName)
			}
			state.Record(urn, "failed", authorName)
			return fmt.Errorf("reacting to post: %w", err)
		}
		return state.Record(urn, "react", authorName)
	}

	// No OAuth client — use CDP directly
	if cdpClient == nil {
		return fmt.Errorf("no API client or browser available for reacting")
	}
	if err := cdpClient.ReactViaCDP(urn); err != nil {
		state.Record(urn, "failed", authorName)
		return fmt.Errorf("browser react: %w", err)
	}
	return state.Record(urn, "react", authorName)
}

// jitter sleeps for a random duration between min and max seconds.
func jitter(minSec, maxSec int) {
	d := time.Duration(minSec+rand.Intn(maxSec-minSec+1)) * time.Second
	time.Sleep(d)
}
