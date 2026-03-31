package cmd

import (
	"context"
	"flag"
	"fmt"
	"io"
	"net/http"
	"os"
	"strings"
	"time"

	engageclaude "github.com/joeyhipolito/nanika-engage/internal/claude"
	"github.com/joeyhipolito/nanika-engage/internal/enrich"
	"github.com/joeyhipolito/nanika-engage/internal/queue"
)

// AdaptCmd adapts source content for different platforms.
// Usage: engage adapt <path-or-url> --platforms <platforms> [options]
func AdaptCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("adapt", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	platformsFlag := fs.String("platforms", "", "comma-separated platforms to adapt for (linkedin,youtube,x,reddit,substack)")
	persona := fs.String("persona", "default", "persona name to use for adaptation")
	dryRun := fs.Bool("dry-run", false, "show adapted content without saving to queue")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: engage adapt <source> --platforms <platforms> [options]

Adapt source content (file or URL) for different platforms.
Generate platform-specific versions of your content.

Arguments:
  source                     File path or URL to adapt

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	// Get source path/URL
	positional := fs.Args()
	if len(positional) < 1 {
		return fmt.Errorf("source required: engage adapt <path-or-url> --platforms <platforms>")
	}
	source := positional[0]

	// Validate platforms
	if *platformsFlag == "" {
		return fmt.Errorf("--platforms required: comma-separated list of platforms")
	}

	// Read source content
	sourceContent, err := readSource(source)
	if err != nil {
		return err
	}

	if !*dryRun && !engageclaude.Available() {
		return fmt.Errorf("claude CLI not found in PATH — required for adaptation")
	}

	// Load persona
	personaContent, err := loadPersona(*persona)
	if err != nil {
		return fmt.Errorf("loading persona %q: %w", *persona, err)
	}

	// Parse platforms
	platforms := strings.Split(strings.TrimSpace(*platformsFlag), ",")
	for i, p := range platforms {
		platforms[i] = strings.TrimSpace(p)
	}

	ctx := context.Background()
	var store *queue.Store
	if !*dryRun {
		var storeErr error
		store, storeErr = queue.NewStore(queue.DefaultDir())
		if storeErr != nil {
			return fmt.Errorf("opening queue: %w", storeErr)
		}
	}

	var adapted, skipped int
	for _, platform := range platforms {
		fmt.Fprintf(os.Stderr, "adapting for %s...\n", platform)

		// Create platform-specific prompt
		systemPrompt := buildAdaptSystemPrompt(platform, personaContent)
		userPrompt := fmt.Sprintf("Adapt the following content for %s:\n\n%s", platform, sourceContent)

		if *dryRun {
			fmt.Printf("=== Platform: %s ===\n", platform)
			fmt.Printf("--- User Prompt ---\n%s\n\n", userPrompt)
			adapted++
			continue
		}

		// Call Claude to adapt content
		adaptedContent, err := engageclaude.Query(ctx, engageclaude.ModelSonnet, systemPrompt, userPrompt)
		if err != nil {
			fmt.Fprintf(os.Stderr, "warn: adapting for %s: %v\n", platform, err)
			skipped++
			continue
		}

		// Create a draft for storage
		draftID := queue.GenerateID(platform, fmt.Sprintf("adapt-%d", time.Now().Unix()))

		// Create a minimal opportunity for context
		opportunity := enrich.EnrichedOpportunity{
			Platform: platform,
			ID:       fmt.Sprintf("adapt-%d", time.Now().Unix()),
			Title:    "Adapted content",
			Body:     truncate(sourceContent, 500),
		}

		d := &queue.Draft{
			ID:          draftID,
			State:       queue.StatePending,
			Platform:    platform,
			Opportunity: opportunity,
			Comment:     adaptedContent,
			Persona:     *persona,
			CreatedAt:   time.Now(),
		}

		if err := store.Save(d); err != nil {
			fmt.Fprintf(os.Stderr, "warn: saving draft for %s: %v\n", platform, err)
			skipped++
			continue
		}

		fmt.Printf("queued: %s (%s)\n", d.ID, platform)
		adapted++
	}

	if !*dryRun {
		fmt.Printf("\n%d adaptation(s) queued, %d skipped. Run 'engage review' to review.\n", adapted, skipped)
	}
	return nil
}

// readSource reads content from a file path or URL.
func readSource(source string) (string, error) {
	// Try to read as URL
	if strings.HasPrefix(source, "http://") || strings.HasPrefix(source, "https://") {
		resp, err := http.Get(source)
		if err != nil {
			return "", fmt.Errorf("fetching URL: %w", err)
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			return "", fmt.Errorf("HTTP %d: %s", resp.StatusCode, source)
		}
		data, err := io.ReadAll(resp.Body)
		if err != nil {
			return "", fmt.Errorf("reading response: %w", err)
		}
		return string(data), nil
	}

	// Try to read as file
	data, err := os.ReadFile(source)
	if err != nil {
		return "", fmt.Errorf("reading file %s: %w", source, err)
	}
	return string(data), nil
}

// buildAdaptSystemPrompt creates a platform-specific system prompt.
func buildAdaptSystemPrompt(platform, personaContent string) string {
	// Platform-specific constraints and guidance
	constraints := map[string]string{
		"linkedin": "LinkedIn posts have no hard character limit but are best kept concise and professional. Aim for 150-500 words. Use formatting like line breaks and paragraphs for readability.",
		"youtube": "YouTube comments are limited to 500 characters. Keep them brief, conversational, and directly relevant to the video.",
		"x": "X (Twitter) posts are limited to 280 characters. Be extremely concise and punchy.",
		"twitter": "Twitter posts are limited to 280 characters. Be extremely concise and punchy.",
		"reddit": "Reddit posts vary by subreddit but should be well-formatted with clear structure. Aim for 300-800 words. Use markdown for emphasis and lists.",
		"substack": "Substack posts support longer-form content (1000+ words). Focus on depth, narrative, and subscriber value. Use clear headings and good paragraph breaks.",
	}

	constraint, ok := constraints[strings.ToLower(platform)]
	if !ok {
		constraint = fmt.Sprintf("Adapt the content for %s with appropriate formatting and platform conventions.", platform)
	}

	return fmt.Sprintf(`You are adapting content for %s on behalf of an author whose voice is defined below.

VOICE:
%s

PLATFORM GUIDANCE:
%s

RULES:
- Maintain the core message and key points from the original content.
- Adapt specifically for %s's audience, tone, and technical constraints.
- Write as the author would naturally write for this platform.
- Match the author's voice — be authentic and human. Avoid corporate or generic language.
- Do not mention that this was adapted or that you are an AI.
- Do not include preamble, explanations, or markdown formatting unless natural for the platform.
- Output only the adapted content — nothing else.

`, strings.ToUpper(platform), personaContent, constraint, platform)
}
