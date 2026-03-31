package engage

import (
	"context"
	"encoding/json"
	"fmt"
	"math/rand"
	"os"
	"time"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/tiptap"
)

// Config holds the settings for an engage run.
type Config struct {
	Post        bool   // Actually post (false = dry-run)
	PersonaPath string // Path to persona markdown file
	MaxComments int    // Max comments per run
	MaxReacts   int    // Max reactions per run
	JSONOutput  bool
	SiteURL     string // e.g. https://yourname.substack.com
}

// Result holds the output of an engage run.
type Result struct {
	NotesScanned int             `json:"notes_scanned"`
	NotesSkipped int             `json:"notes_skipped"`
	Comments     []CommentResult `json:"comments"`
	Reacts       []ReactResult   `json:"reacts"`
	Errors       []string        `json:"errors,omitempty"`
}

// CommentResult describes a posted (or drafted) comment.
type CommentResult struct {
	NoteID  int    `json:"note_id"`
	Author  string `json:"author"`
	Type    string `json:"type"` // "grounded" or "opinion"
	Article string `json:"matched_article,omitempty"`
	Comment string `json:"comment"`
	Posted  bool   `json:"posted"`
}

// ReactResult describes a reaction.
type ReactResult struct {
	NoteID int    `json:"note_id"`
	Author string `json:"author"`
	Posted bool   `json:"posted"`
}

// Run executes the full engage pipeline: scan → score → draft → act → record.
func Run(ctx context.Context, client *api.Client, cfg Config) (*Result, error) {
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

	// 1. SCAN: Fetch both dashboard tabs, merge, dedupe
	notes, err := scanDashboard(client, state)
	if err != nil {
		return nil, fmt.Errorf("scanning dashboard: %w", err)
	}
	result.NotesScanned = len(notes)

	if len(notes) == 0 {
		return result, nil
	}

	// Fetch our articles for grounding
	articles, err := client.GetPosts(0, 20)
	if err != nil {
		result.Errors = append(result.Errors, fmt.Sprintf("fetching articles: %v (opinion-only mode)", err))
	}

	// 2. SCORE
	scores, err := ScoreNotes(ctx, notes, articles)
	if err != nil {
		// Fallback: sort by reaction count, react to top notes
		result.Errors = append(result.Errors, fmt.Sprintf("scoring failed: %v (fallback: react to top notes)", err))
		scores = fallbackScores(notes)
	}

	// Build lookup maps
	noteByID := make(map[int]NoteCandidate)
	for _, n := range notes {
		noteByID[n.ID] = n
	}
	articleBySlug := make(map[string]api.Post)
	for _, a := range articles {
		articleBySlug[a.Slug] = a
	}

	// 3. DECIDE + 4. DRAFT + 5. ACT
	commentCount := 0
	reactCount := 0

	for _, score := range scores {
		note, ok := noteByID[score.NoteID]
		if !ok {
			continue
		}

		// Grounded comment (relevance >= 7)
		if score.Relevance >= 7 && commentCount < cfg.MaxComments && note.CanReply {
			article, hasArticle := articleBySlug[score.MatchedArticle]
			if hasArticle {
				draft, err := DraftGroundedComment(ctx, note, article, cfg.SiteURL, voice)
				if err != nil {
					result.Errors = append(result.Errors, fmt.Sprintf("drafting grounded comment for note %d: %v", note.ID, err))
					continue
				}

				cr := CommentResult{
					NoteID:  note.ID,
					Author:  note.Name,
					Type:    "grounded",
					Article: article.Slug,
					Comment: draft.Comment,
				}

				if cfg.Post {
					if err := postComment(client, state, note.ID, draft.Comment, note.Name); err != nil {
						result.Errors = append(result.Errors, fmt.Sprintf("posting comment on note %d: %v", note.ID, err))
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
		if score.Interest >= 6 && commentCount < cfg.MaxComments && note.CanReply {
			draft, err := DraftOpinionComment(ctx, note, voice)
			if err != nil {
				result.Errors = append(result.Errors, fmt.Sprintf("drafting opinion comment for note %d: %v", note.ID, err))
				continue
			}

			cr := CommentResult{
				NoteID:  note.ID,
				Author:  note.Name,
				Type:    "opinion",
				Comment: draft.Comment,
			}

			if cfg.Post {
				if err := postComment(client, state, note.ID, draft.Comment, note.Name); err != nil {
					result.Errors = append(result.Errors, fmt.Sprintf("posting comment on note %d: %v", note.ID, err))
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
				NoteID: note.ID,
				Author: note.Name,
			}

			if cfg.Post {
				if err := client.ReactToNote(note.ID); err != nil {
					result.Errors = append(result.Errors, fmt.Sprintf("reacting to note %d: %v", note.ID, err))
					state.Record(note.ID, "failed", note.Name)
					rr.Posted = false
				} else {
					state.Record(note.ID, "react", note.Name)
					rr.Posted = true
				}
				jitter(1, 3)
			}

			result.Reacts = append(result.Reacts, rr)
			reactCount++
		}
	}

	result.NotesSkipped = result.NotesScanned - len(result.Comments) - len(result.Reacts)
	return result, nil
}

// PrintResult outputs the result in text or JSON format.
func PrintResult(result *Result, jsonOutput bool) error {
	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(result)
	}

	fmt.Printf("Scanned %d notes\n", result.NotesScanned)

	if len(result.Comments) > 0 {
		fmt.Printf("\nComments (%d):\n", len(result.Comments))
		for _, c := range result.Comments {
			status := "drafted"
			if c.Posted {
				status = "posted"
			}
			fmt.Printf("  [%s] %s on note #%d by %s\n", status, c.Type, c.NoteID, c.Author)
			fmt.Printf("    %s\n", NoteURL(c.NoteID))
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
			fmt.Printf("  [%s] note #%d by %s — %s\n", status, r.NoteID, r.Author, NoteURL(r.NoteID))
		}
	}

	if len(result.Errors) > 0 {
		fmt.Printf("\nWarnings (%d):\n", len(result.Errors))
		for _, e := range result.Errors {
			fmt.Printf("  - %s\n", e)
		}
	}

	skipped := result.NotesSkipped
	if skipped > 0 {
		fmt.Printf("\nSkipped %d notes (low score or already engaged)\n", skipped)
	}

	return nil
}

// scanDashboard fetches for-you and subscribed tabs, merges, dedupes, filters.
func scanDashboard(client *api.Client, state *State) ([]NoteCandidate, error) {
	// Get user ID to filter out own notes
	profile, err := client.GetProfile()
	if err != nil {
		return nil, fmt.Errorf("getting profile: %w", err)
	}

	// Fetch both tabs
	forYou, err := client.GetDashboard(0, "for-you")
	if err != nil {
		return nil, fmt.Errorf("fetching for-you feed: %w", err)
	}

	subscribed, err := client.GetDashboard(0, "subscribed")
	if err != nil {
		// Non-fatal: continue with for-you only
		subscribed = nil
	}

	// Merge and dedupe
	seen := make(map[int]bool)
	var candidates []NoteCandidate

	for _, items := range [][]api.DashboardItem{forYou, subscribed} {
		for _, item := range items {
			if item.Type != "comment" || item.Note == nil {
				continue
			}
			n := item.Note

			// Skip own notes
			if n.UserID == profile.ID {
				continue
			}

			// Skip already engaged
			if state.IsEngaged(n.ID) {
				continue
			}

			// Skip duplicates
			if seen[n.ID] {
				continue
			}
			seen[n.ID] = true

			// Skip notes with no body
			if n.Body == "" {
				continue
			}

			candidates = append(candidates, NoteCandidate{
				ID:       n.ID,
				Body:     n.Body,
				Name:     n.Name,
				Reacts:   n.ReactionCount,
				Replies:  n.ChildrenCount,
				CanReply: item.CanReply,
			})

			if len(candidates) >= 40 {
				break
			}
		}
	}

	return candidates, nil
}

// fallbackScores assigns scores based on reaction count when LLM scoring fails.
func fallbackScores(notes []NoteCandidate) []NoteScore {
	var scores []NoteScore
	for _, n := range notes {
		interest := 4 // default: react-worthy
		if n.Reacts >= 10 {
			interest = 6
		}
		scores = append(scores, NoteScore{
			NoteID:   n.ID,
			Interest: interest,
		})
	}
	return scores
}

// postComment builds the tiptap body and posts a reply.
func postComment(client *api.Client, state *State, noteID int, text, author string) error {
	bodyJSON, err := tiptap.BuildNoteBody(text)
	if err != nil {
		return fmt.Errorf("building note body: %w", err)
	}

	_, err = client.ReplyToNote(bodyJSON, noteID)
	if err != nil {
		state.Record(noteID, "failed", author)
		return err
	}

	return state.Record(noteID, "comment", author)
}

// jitter sleeps for a random duration between min and max seconds.
func jitter(minSec, maxSec int) {
	d := time.Duration(minSec+rand.Intn(maxSec-minSec+1)) * time.Second
	time.Sleep(d)
}
