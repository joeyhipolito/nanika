package cmd

import (
	"context"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	engageclaude "github.com/joeyhipolito/nanika-engage/internal/claude"
	"github.com/joeyhipolito/nanika-engage/internal/draft"
	"github.com/joeyhipolito/nanika-engage/internal/enrich"
	"github.com/joeyhipolito/nanika-engage/internal/queue"
)

// DraftCmd scans top opportunities, generates drafts, and queues them for review.
// Usage: engage draft [options] [<platform> <id>]
// If <platform> and <id> are given, draft for that specific opportunity only.
func DraftCmd(args []string, _ bool) error {
	fs := flag.NewFlagSet("draft", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	platformsFlag := fs.String("platform", "", "comma-separated platforms to scan (youtube,linkedin,reddit,substack,x)")
	limit := fs.Int("limit", 5, "max opportunities to draft per platform")
	persona := fs.String("persona", "default", "persona name to use for drafting")
	skipAuth := fs.Bool("skip-authenticity-pass", false, "skip the second authenticity rewrite pass")
	reschedulePost := fs.Bool("reschedule-post", false, "after drafting, schedule a commit run for tomorrow")
	dryRun := fs.Bool("dry-run", false, "show enriched context and draft prompt without calling Claude")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: engage draft [options] [<platform> <id>]

Scan platforms, pick top opportunities, and generate drafts for review.
If <platform> and <id> are provided, draft for that specific opportunity only.

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	if !*dryRun && !engageclaude.Available() {
		return fmt.Errorf("claude CLI not found in PATH — required for drafting")
	}

	personaContent, err := loadPersona(*persona)
	if err != nil {
		return fmt.Errorf("loading persona %q: %w", *persona, err)
	}

	ctx := context.Background()

	// Single opportunity mode: engage draft <platform> <id>
	positional := fs.Args()
	if len(positional) == 2 {
		platform, id := positional[0], positional[1]
		e := enrich.ByPlatform(platform)
		if e == nil {
			return fmt.Errorf("unknown platform: %s", platform)
		}
		full, enrichErr := e.Enrich(ctx, id)
		if enrichErr != nil {
			return fmt.Errorf("enriching %s/%s: %w", platform, id, enrichErr)
		}
		if full == nil {
			return fmt.Errorf("no data found for %s/%s", platform, id)
		}
		if *dryRun {
			return printDryRun(*full, *persona, personaContent)
		}
		store, err := queue.NewStore(queue.DefaultDir())
		if err != nil {
			return fmt.Errorf("opening queue: %w", err)
		}
		return draftAndSave(ctx, *full, *persona, personaContent, *skipAuth, store)
	}

	// Scan mode: scan all matching platforms, sort by score, draft top N.
	enrichers := selectEnrichers(*platformsFlag)
	if len(enrichers) == 0 {
		return fmt.Errorf("no matching platforms: %s", *platformsFlag)
	}

	var store *queue.Store
	if !*dryRun {
		store, err = queue.NewStore(queue.DefaultDir())
		if err != nil {
			return fmt.Errorf("opening queue: %w", err)
		}
	}

	var drafted, skipped int
	for _, e := range enrichers {
		// Scan a broader pool so sorting has meaningful candidates.
		scanLimit := *limit * 3
		if scanLimit < 15 {
			scanLimit = 15
		}
		items, err := e.Scan(ctx, scanLimit)
		if err != nil {
			fmt.Fprintf(os.Stderr, "warn: %s scan failed: %v\n", e.Platform(), err)
			continue
		}
		if len(items) == 0 {
			fmt.Fprintf(os.Stderr, "info: %s: no opportunities found\n", e.Platform())
			continue
		}

		// Sort by (likes + comments) * recency, best first.
		sort.Slice(items, func(i, j int) bool {
			return opportunityScore(items[i]) > opportunityScore(items[j])
		})
		if len(items) > *limit {
			items = items[:*limit]
		}

		for _, opp := range items {
			fmt.Fprintf(os.Stderr, "drafting: [%s] %s\n", opp.Platform, truncateTitle(opp.Title, 60))

			// Enrich fully before drafting so we have transcript + comments.
			full, enrichErr := e.Enrich(ctx, opp.ID)
			if enrichErr != nil {
				fmt.Fprintf(os.Stderr, "warn: enrich %s/%s failed: %v — using scan metadata\n", opp.Platform, opp.ID, enrichErr)
				full = &opp
			}

			if *dryRun {
				if err := printDryRun(*full, *persona, personaContent); err != nil {
					return err
				}
				continue
			}

			if err := draftAndSave(ctx, *full, *persona, personaContent, *skipAuth, store); err != nil {
				fmt.Fprintf(os.Stderr, "warn: %v\n", err)
				skipped++
				continue
			}
			drafted++
		}
	}

	if !*dryRun {
		fmt.Printf("\n%d draft(s) queued, %d skipped. Run 'engage review' to review.\n", drafted, skipped)
		if *reschedulePost && drafted > 0 {
			if err := scheduleCommitRun(); err != nil {
				fmt.Fprintf(os.Stderr, "warn: reschedule-post: %v\n", err)
			}
		}
	}
	return nil
}

// opportunityScore returns (likes + comments) weighted by recency.
// Older posts decay: score = base / (1 + daysSince).
func opportunityScore(opp enrich.EnrichedOpportunity) float64 {
	base := float64(opp.Metrics.Likes + opp.Metrics.Comments)
	if opp.CreatedAt.IsZero() {
		return base * 0.1
	}
	daysSince := time.Since(opp.CreatedAt).Hours() / 24
	return base / (1.0 + daysSince)
}

// draftAndSave calls Claude to draft a comment and saves it to the queue.
func draftAndSave(ctx context.Context, opp enrich.EnrichedOpportunity, personaName, personaContent string, skipAuth bool, store *queue.Store) error {
	comment, err := draft.DraftComment(ctx, opp, personaContent, skipAuth)
	if err != nil {
		return fmt.Errorf("draft %s/%s: %w", opp.Platform, opp.ID, err)
	}
	d := &queue.Draft{
		ID:          queue.GenerateID(opp.Platform, opp.ID),
		State:       queue.StatePending,
		Platform:    opp.Platform,
		Opportunity: opp,
		Comment:     comment,
		Persona:     personaName,
		CreatedAt:   time.Now(),
	}
	if err := store.Save(d); err != nil {
		return fmt.Errorf("saving draft %s: %w", d.ID, err)
	}
	fmt.Printf("queued: %s (%s)\n", d.ID, opp.Platform)
	return nil
}

// printDryRun prints the enriched context and the exact prompt that would go to Claude.
func printDryRun(opp enrich.EnrichedOpportunity, personaName, personaContent string) error {
	fmt.Printf("=== DRY RUN: %s/%s ===\n\n", opp.Platform, opp.ID)
	fmt.Println("--- Enriched Context ---")
	printEnriched(&opp)
	fmt.Println("\n--- Draft Prompt (user message to Claude) ---")
	fmt.Println(draft.BuildUserPrompt(opp))
	fmt.Printf("\n--- Persona: %s (%d chars) ---\n", personaName, len(personaContent))
	return nil
}

// loadPersona reads the persona voice file from ~/nanika/personas/<name>.md.
// ENGAGE_PERSONAS_DIR overrides the default location.
// If the named persona is missing, falls back to technical-writer.md with a warning.
func loadPersona(name string) (string, error) {
	dir := os.Getenv("ENGAGE_PERSONAS_DIR")
	if dir == "" {
		home, err := os.UserHomeDir()
		if err != nil {
			return "", fmt.Errorf("getting home dir: %w", err)
		}
		dir = filepath.Join(home, "nanika", "personas")
	}

	path := filepath.Join(dir, name+".md")
	data, err := os.ReadFile(path)
	if err == nil {
		return strings.TrimSpace(string(data)), nil
	}
	if !os.IsNotExist(err) {
		return "", fmt.Errorf("reading persona %q: %w", name, err)
	}

	// Primary persona not found — try technical-writer fallback.
	if name != "technical-writer" {
		fmt.Fprintf(os.Stderr, "warn: persona %q not found at %s — falling back to technical-writer.md\n", name, path)
		fallbackPath := filepath.Join(dir, "technical-writer.md")
		data, err = os.ReadFile(fallbackPath)
		if err == nil {
			return strings.TrimSpace(string(data)), nil
		}
		if !os.IsNotExist(err) {
			return "", fmt.Errorf("reading fallback persona technical-writer: %w", err)
		}
	}

	return "", fmt.Errorf("persona %q not found at %s (technical-writer fallback also missing)", name, path)
}

// truncateTitle returns s truncated to n runes with "..." if longer.
func truncateTitle(s string, n int) string {
	r := []rune(s)
	if len(r) <= n {
		return s
	}
	return string(r[:n]) + "..."
}
