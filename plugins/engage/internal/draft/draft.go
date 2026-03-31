// Package draft provides the two-pass LLM comment drafting pipeline.
// Pass 1: draft the comment grounded in the full EnrichedOpportunity context.
// Pass 2: authenticity rewrite to strip LLM tells.
package draft

import (
	"context"
	"fmt"
	"os"
	"strings"

	engageclaude "github.com/joeyhipolito/nanika-engage/internal/claude"
	"github.com/joeyhipolito/nanika-engage/internal/enrich"
)

// platformCharLimits maps platforms to their max character count for replies.
var platformCharLimits = map[string]int{
	"x":        280,
	"youtube":  500,
	"linkedin": 1250,
}

const draftSystemPromptTemplate = `You are writing a comment on behalf of an author whose voice is defined below.

VOICE:
%s

RULES:
- Write as the author would naturally write online — not as an AI assistant.
- Be genuinely helpful or insightful. No generic praise.
- Keep it conversational, under 150 words.
- HARD CHARACTER LIMITS per platform (replies count toward these):
  * X/Twitter: 280 characters MAX. Aim for 200-260 to leave margin.
  * LinkedIn: 1250 characters max.
  * Reddit: no hard limit, but keep under 150 words.
  * YouTube: 500 characters max.
  * Substack: no hard limit, but keep under 150 words.
- No hashtags, no bullet points, no "Great post!".
- Do not mention that you are an AI or that this was drafted for someone.
- Output only the comment text — no preamble, no quotes, no markdown.
- Output the comment EXACTLY ONCE. Do not repeat yourself.

AUTHENTICITY — write like a real human, not an LLM:
- Vary sentence length. Mix short punchy sentences (6-10 words) with longer ones (20-30). Never write 3+ sentences of similar length in a row.
- Start sentences with low-frequency words or specific nouns — never "This is", "That's a", "I think that", "It's worth noting".
- No hedge-stacking. Pick ONE qualifier per claim, not "I think it might possibly be worth considering".
- Use first person naturally but sparingly. "I" once or twice, not every sentence.
- Use concrete specifics — name a tool, a version, a number, a lived detail. Vague agreement is the #1 tell.
- No exclamation marks. No "love this", "absolutely", "couldn't agree more", "spot on", "nailed it".
- No performative transitions: "That said,", "To add to this,", "Building on that,", "This resonates because".
- Never use em dashes (—) or en dashes (–). Replace with commas for asides, periods for new thoughts, or reword entirely.
  BAD: "The project — which started last month — is nearly complete."
  GOOD: "The project, which started last month, is nearly complete."
  After drafting, scan your output and replace any remaining em dashes.
- Contractions are fine. Sentence fragments are fine. Starting with "But" or "And" is fine.
- Let one idea breathe instead of covering three surface-level points.
- Write like someone who types fast and hits send, not someone who drafts and polishes.
- Prefer the blunt version over the diplomatic version.
- If you'd never say it out loud to a colleague, don't write it.`

const authenticitySystemPrompt = `You are a writing editor. Rewrite the comment below to fix any authenticity violations without changing the core idea or author voice.

RULES TO ENFORCE:
1. Vary sentence length — mix short (6-10 words) with longer (20-30). No 3+ consecutive same-length sentences.
2. Start with a specific noun or concrete detail. Never "This is", "That's a", "I think that", "It's worth noting".
3. One qualifier per claim. No hedge-stacking like "I think it might possibly be".
4. "I" at most twice. Not every sentence.
5. At least one concrete specific: a tool name, a version number, a real detail. Cut vague agreement.
6. No exclamation marks. Delete "love this", "absolutely", "couldn't agree more", "spot on", "nailed it".
7. No performative transitions: "That said,", "To add to this,", "Building on that,", "This resonates because".
8. No em dashes (—) or en dashes (–). Comma for asides, period for new thoughts, or reword.
9. Contractions fine. Fragments fine. "But" or "And" at sentence start fine.
10. One idea only. Cut anything that adds a second surface-level point.

Output only the revised comment — no preamble, no explanation, no quotes.`

// DraftComment generates a comment for an enriched opportunity using a two-pass Claude pipeline.
// personaContent is the full text of the persona's voice file.
// If skipAuthenticityPass is true, only the first (draft) pass runs.
func DraftComment(ctx context.Context, opp enrich.EnrichedOpportunity, personaContent string, skipAuthenticityPass bool) (string, error) {
	if !engageclaude.Available() {
		return "", fmt.Errorf("claude CLI not available")
	}

	systemPrompt := fmt.Sprintf(draftSystemPromptTemplate, personaContent)
	userPrompt := BuildUserPrompt(opp)

	comment, err := engageclaude.Query(ctx, engageclaude.ModelSonnet, systemPrompt, userPrompt)
	if err != nil {
		return "", fmt.Errorf("drafting comment for %s/%s: %w", opp.Platform, opp.ID, err)
	}

	comment = strings.TrimSpace(comment)
	if comment == "" {
		return "", fmt.Errorf("claude returned empty comment for %s/%s", opp.Platform, opp.ID)
	}

	if !skipAuthenticityPass {
		revised, passErr := authenticityPass(ctx, comment, opp.Platform, personaContent)
		if passErr == nil {
			comment = revised
		} else {
			fmt.Fprintf(os.Stderr, "warn: authenticity pass for %s/%s: %v — using original draft\n", opp.Platform, opp.ID, passErr)
		}
	}

	comment = normalizeDashes(comment)
	comment = enforceCharLimit(comment, opp.Platform)

	return comment, nil
}

// authenticityPass rewrites a draft through a focused Sonnet call applying the
// 10 social authenticity rules. Returns the revised comment or original on failure.
func authenticityPass(ctx context.Context, comment, platformName, personaContent string) (string, error) {
	if !engageclaude.Available() {
		return comment, nil
	}

	userPrompt := fmt.Sprintf("Platform: %s\n\nPersona voice:\n%s\n\nComment to improve:\n%s",
		platformName, personaContent, comment)

	revised, err := engageclaude.Query(ctx, engageclaude.ModelSonnet, authenticitySystemPrompt, userPrompt)
	if err != nil {
		return comment, fmt.Errorf("authenticity pass for %s: %w", platformName, err)
	}

	revised = strings.TrimSpace(revised)
	if revised == "" {
		return comment, nil
	}
	return normalizeDashes(revised), nil
}

// BuildUserPrompt constructs the full context prompt from an EnrichedOpportunity.
// Injects transcript, top comments (so we don't repeat), engagement signals, and images.
func BuildUserPrompt(opp enrich.EnrichedOpportunity) string {
	var sb strings.Builder

	sb.WriteString(fmt.Sprintf("Platform: %s\n", opp.Platform))
	sb.WriteString(fmt.Sprintf("Post title: %s\n", opp.Title))
	if opp.Author != "" {
		sb.WriteString(fmt.Sprintf("Author: %s\n", opp.Author))
	}
	if opp.URL != "" {
		sb.WriteString(fmt.Sprintf("URL: %s\n", opp.URL))
	}

	// Engagement signals help calibrate tone and specificity.
	if opp.Metrics.Likes > 0 || opp.Metrics.Comments > 0 || opp.Metrics.Views > 0 {
		sb.WriteString("\nEngagement:\n")
		if opp.Metrics.Likes > 0 {
			sb.WriteString(fmt.Sprintf("  Likes: %d\n", opp.Metrics.Likes))
		}
		if opp.Metrics.Comments > 0 {
			sb.WriteString(fmt.Sprintf("  Comments: %d\n", opp.Metrics.Comments))
		}
		if opp.Metrics.Views > 0 {
			sb.WriteString(fmt.Sprintf("  Views: %d\n", opp.Metrics.Views))
		}
		if opp.Metrics.Score > 0 {
			sb.WriteString(fmt.Sprintf("  Score: %d\n", opp.Metrics.Score))
		}
	}

	if opp.Body != "" {
		sb.WriteString(fmt.Sprintf("\nPost body:\n%s\n", truncate(opp.Body, 1200)))
	}

	// Video transcript: trimmed to keep prompt manageable.
	if opp.Transcript != "" {
		sb.WriteString(fmt.Sprintf("\nVideo transcript (first 2000 chars):\n%s\n", truncate(opp.Transcript, 2000)))
	}

	// Images: described so the model knows what visuals are present.
	if len(opp.Images) > 0 {
		sb.WriteString("\nImages in post:\n")
		for _, img := range opp.Images {
			sb.WriteString(fmt.Sprintf("  - %s\n", img))
		}
	}

	// Existing comments: show what others are saying so we don't repeat.
	if len(opp.Comments) > 0 {
		top := opp.Comments
		if len(top) > 8 {
			top = top[:8]
		}
		sb.WriteString("\nWhat others are already saying (do NOT repeat these angles):\n")
		for _, c := range top {
			line := c.Text
			if len([]rune(line)) > 200 {
				line = string([]rune(line)[:200]) + "..."
			}
			if c.Author != "" {
				sb.WriteString(fmt.Sprintf("  [%s] %s\n", c.Author, line))
			} else {
				sb.WriteString(fmt.Sprintf("  - %s\n", line))
			}
		}
	}

	sb.WriteString("\nWrite a comment the author would post on this.")
	return sb.String()
}

// normalizeDashes replaces em dashes with periods. LLMs insert them despite
// explicit instructions, so we enforce at code level after each LLM call.
func normalizeDashes(s string) string {
	s = strings.ReplaceAll(s, " — ", ". ")
	s = strings.ReplaceAll(s, "—", ", ")
	s = strings.ReplaceAll(s, " – ", ". ")
	s = strings.ReplaceAll(s, "–", ", ")
	return s
}

// enforceCharLimit truncates comment to the platform's character limit,
// breaking at the last sentence boundary or word boundary that fits.
// Operates on runes, not bytes, so multi-byte characters are handled correctly.
func enforceCharLimit(comment, platform string) string {
	limit, ok := platformCharLimits[platform]
	if !ok {
		return comment
	}
	runes := []rune(comment)
	if len(runes) <= limit {
		return comment
	}

	head := string(runes[:limit])

	for _, sep := range []string{". ", "! ", "? "} {
		if idx := strings.LastIndex(head, sep); idx > len(head)/2 {
			return head[:idx+1]
		}
	}
	if idx := strings.LastIndex(head, " "); idx > len(head)/2 {
		return head[:idx]
	}
	return head
}

// truncate returns the first n runes of s.
func truncate(s string, n int) string {
	runes := []rune(s)
	if len(runes) <= n {
		return s
	}
	return string(runes[:n]) + "..."
}
