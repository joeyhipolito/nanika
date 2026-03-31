package engage

import (
	"context"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/llm"
)

const draftingModel = "claude-sonnet-4-6"

const defaultVoice = "a technical writer who is direct, grounded, and conversational. No hype, no exclamation marks."

// CommentDraft holds a drafted comment ready for posting.
type CommentDraft struct {
	ActivityURN string `json:"activity_urn"`
	Comment     string `json:"comment"`
	Type        string `json:"type"` // "grounded" or "opinion"
	Article     string `json:"matched_article,omitempty"`
}

// DraftGroundedComment drafts a comment that references one of our Substack articles.
func DraftGroundedComment(ctx context.Context, item api.FeedItem, post SubstackPost, siteURL, voice string) (*CommentDraft, error) {
	if voice == "" {
		voice = defaultVoice
	}

	articleURL := post.CanonicalURL
	if articleURL == "" && post.Slug != "" && siteURL != "" {
		articleURL = siteURL + "/p/" + post.Slug
	}

	prompt := fmt.Sprintf(`You are writing a reply to a LinkedIn post. Write in this voice: %s

THE POST you're replying to:
Author: %s
Headline: %s
Text: %s

YOUR ARTICLE that's relevant:
Title: %s
Description: %s
URL: %s

Write a reply that:
1. Engages directly with what the author said (don't just promote your article)
2. Adds a concrete insight or experience from your article
3. Naturally mentions your article with a link ONLY if it genuinely adds value
4. Is 1-3 sentences
5. No exclamation marks, no "Great post!", no filler, no hashtags

Return ONLY the comment text. No quotes, no explanation.`,
		voice, item.AuthorName, item.AuthorHeadline, item.Text,
		post.Title, post.Description, articleURL)

	resp, err := llm.QueryText(ctx, prompt, draftingModel)
	if err != nil {
		return nil, fmt.Errorf("drafting grounded comment: %w", err)
	}

	return &CommentDraft{
		ActivityURN: item.ActivityURN,
		Comment:     strings.TrimSpace(resp),
		Type:        "grounded",
		Article:     post.Slug,
	}, nil
}

// DraftOpinionComment drafts a comment based on the post content alone.
func DraftOpinionComment(ctx context.Context, item api.FeedItem, voice string) (*CommentDraft, error) {
	if voice == "" {
		voice = defaultVoice
	}

	prompt := fmt.Sprintf(`You are writing a reply to a LinkedIn post. Write in this voice: %s

THE POST you're replying to:
Author: %s
Headline: %s
Text: %s

Write a reply that:
1. Engages directly with the author's point
2. Adds your own perspective or a concrete experience
3. Is 1-3 sentences
4. No exclamation marks, no "Great post!", no filler, no hashtags

Return ONLY the comment text. No quotes, no explanation.`,
		voice, item.AuthorName, item.AuthorHeadline, item.Text)

	resp, err := llm.QueryText(ctx, prompt, draftingModel)
	if err != nil {
		return nil, fmt.Errorf("drafting opinion comment: %w", err)
	}

	return &CommentDraft{
		ActivityURN: item.ActivityURN,
		Comment:     strings.TrimSpace(resp),
		Type:        "opinion",
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
