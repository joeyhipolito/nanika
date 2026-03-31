package engage

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/llm"
)

const scoringModel = "claude-haiku-4-5-20251001"

// NoteScore holds the LLM-assigned scores for a dashboard note.
type NoteScore struct {
	NoteID         int    `json:"note_id"`
	Relevance      int    `json:"relevance"`
	Interest       int    `json:"interest"`
	MatchedArticle string `json:"matched_article,omitempty"`
	Reason         string `json:"reason,omitempty"`
}

// ScoreNotes calls Haiku to score each note on relevance (to our articles) and interest.
func ScoreNotes(ctx context.Context, notes []NoteCandidate, articles []api.Post) ([]NoteScore, error) {
	if len(notes) == 0 {
		return nil, nil
	}

	prompt := buildScoringPrompt(notes, articles)
	resp, err := llm.QueryText(ctx, prompt, scoringModel)
	if err != nil {
		return nil, fmt.Errorf("scoring LLM call: %w", err)
	}

	return parseScoringResponse(resp)
}

// NoteCandidate is a note ready for scoring.
type NoteCandidate struct {
	ID       int
	Body     string
	Name     string
	Reacts   int
	Replies  int
	CanReply bool
}

func buildScoringPrompt(notes []NoteCandidate, articles []api.Post) string {
	var sb strings.Builder

	sb.WriteString(`You are a scoring engine. Score each note on two dimensions:
- relevance (0-10): How relevant is this note to one of OUR articles? 7+ means we could write a grounded comment linking our article.
- interest (0-10): How interesting/engaging is this note generally? 6+ means worth commenting on, 4+ means worth reacting to.

OUR ARTICLES (most recent):
`)

	for i, a := range articles {
		if i >= 20 {
			break
		}
		desc := a.Description
		if len(desc) > 150 {
			desc = desc[:150] + "..."
		}
		fmt.Fprintf(&sb, "- slug=%q title=%q desc=%q\n", a.Slug, a.Title, desc)
	}

	sb.WriteString("\nNOTES TO SCORE:\n")

	for _, n := range notes {
		body := n.Body
		if len(body) > 300 {
			body = body[:300] + "..."
		}
		fmt.Fprintf(&sb, "- note_id=%d author=%q reacts=%d replies=%d body=%q\n",
			n.ID, n.Name, n.Reacts, n.Replies, body)
	}

	sb.WriteString(`
Return ONLY a JSON array. No explanation, no markdown fences. Each element:
{"note_id": <int>, "relevance": <0-10>, "interest": <0-10>, "matched_article": "<slug or empty>", "reason": "<brief>"}
`)

	return sb.String()
}

func parseScoringResponse(resp string) ([]NoteScore, error) {
	resp = strings.TrimSpace(resp)

	// Strip markdown fences if present
	if strings.HasPrefix(resp, "```") {
		lines := strings.Split(resp, "\n")
		var cleaned []string
		for _, l := range lines {
			if strings.HasPrefix(strings.TrimSpace(l), "```") {
				continue
			}
			cleaned = append(cleaned, l)
		}
		resp = strings.Join(cleaned, "\n")
	}

	// Find JSON array boundaries
	start := strings.Index(resp, "[")
	end := strings.LastIndex(resp, "]")
	if start < 0 || end < 0 || end <= start {
		return nil, fmt.Errorf("no JSON array found in scoring response")
	}
	resp = resp[start : end+1]

	var scores []NoteScore
	if err := json.Unmarshal([]byte(resp), &scores); err != nil {
		return nil, fmt.Errorf("parsing scoring JSON: %w\nraw: %s", err, resp)
	}

	return scores, nil
}
