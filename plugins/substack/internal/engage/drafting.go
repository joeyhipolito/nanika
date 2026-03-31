package engage

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/llm"
)

const draftingModel = "claude-sonnet-4-6"

const defaultVoice = "a technical writer who is direct, grounded, and conversational. No hype, no exclamation marks."

// CommentDraft holds a drafted comment ready for posting.
type CommentDraft struct {
	NoteID  int    `json:"note_id"`
	Comment string `json:"comment"`
	Type    string `json:"type"` // "grounded" or "opinion"
	Article string `json:"matched_article,omitempty"`
}

// DraftGroundedComment drafts a comment that references one of our articles.
func DraftGroundedComment(ctx context.Context, note NoteCandidate, article api.Post, siteURL, voice string) (*CommentDraft, error) {
	if voice == "" {
		voice = defaultVoice
	}

	articleURL := article.CanonicalURL
	if articleURL == "" && article.Slug != "" && siteURL != "" {
		articleURL = siteURL + "/p/" + article.Slug
	}

	prompt := fmt.Sprintf(`You are writing a reply to a Substack note. Write in this voice: %s

THE NOTE you're replying to:
Author: %s
Body: %s

YOUR ARTICLE that's relevant:
Title: %s
Description: %s
URL: %s

Write a reply that:
1. Engages directly with what the author said (don't just promote your article)
2. Adds a concrete insight or experience from your article
3. Naturally mentions your article with a link ONLY if it genuinely adds value
4. Stays under 80 words
5. No exclamation marks, no "Great post!", no filler

Return ONLY the comment text. No quotes, no explanation.`,
		voice, note.Name, note.Body, article.Title, article.Description, articleURL)

	resp, err := llm.QueryText(ctx, prompt, draftingModel)
	if err != nil {
		return nil, fmt.Errorf("drafting grounded comment: %w", err)
	}

	return &CommentDraft{
		NoteID:  note.ID,
		Comment: strings.TrimSpace(resp),
		Type:    "grounded",
		Article: article.Slug,
	}, nil
}

// DraftOpinionComment drafts a comment based on the note content alone.
func DraftOpinionComment(ctx context.Context, note NoteCandidate, voice string) (*CommentDraft, error) {
	if voice == "" {
		voice = defaultVoice
	}

	prompt := fmt.Sprintf(`You are writing a reply to a Substack note. Write in this voice: %s

THE NOTE you're replying to:
Author: %s
Body: %s

Write a reply that:
1. Engages directly with the author's point
2. Adds your own perspective or a concrete experience
3. Stays under 80 words
4. No exclamation marks, no "Great post!", no filler

Return ONLY the comment text. No quotes, no explanation.`,
		voice, note.Name, note.Body)

	resp, err := llm.QueryText(ctx, prompt, draftingModel)
	if err != nil {
		return nil, fmt.Errorf("drafting opinion comment: %w", err)
	}

	return &CommentDraft{
		NoteID:  note.ID,
		Comment: strings.TrimSpace(resp),
		Type:    "opinion",
	}, nil
}

// LoadPersonaVoice reads a persona markdown file and extracts the ## Identity section.
func LoadPersonaVoice(path string) (string, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return "", fmt.Errorf("reading persona file: %w", err)
	}

	content := string(data)
	lines := strings.Split(content, "\n")

	var identityLines []string
	inIdentity := false

	for _, line := range lines {
		trimmed := strings.TrimSpace(line)

		if trimmed == "## Identity" {
			inIdentity = true
			continue
		}

		if inIdentity {
			// Stop at next heading
			if strings.HasPrefix(trimmed, "## ") {
				break
			}
			if trimmed != "" {
				identityLines = append(identityLines, trimmed)
			}
		}
	}

	if len(identityLines) == 0 {
		return defaultVoice, nil
	}

	return strings.Join(identityLines, " "), nil
}
