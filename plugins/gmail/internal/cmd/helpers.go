package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"sort"
	"strings"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
)

// sortThreadSummaries sorts threads: unread first, then by date descending.
func sortThreadSummaries(summaries []api.ThreadSummary) {
	sort.SliceStable(summaries, func(i, j int) bool {
		// Unread threads come first.
		if summaries[i].Unread != summaries[j].Unread {
			return summaries[i].Unread
		}
		// Then sort by date descending (string comparison works for RFC2822/RFC3339).
		return summaries[i].Date > summaries[j].Date
	})
}

// printThreadSummariesJSON prints thread summaries as JSON.
func printThreadSummariesJSON(summaries []api.ThreadSummary) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(summaries)
}

// printThreadSummaries prints thread summaries in a human-readable format.
func printThreadSummaries(summaries []api.ThreadSummary) {
	if len(summaries) == 0 {
		fmt.Println("No threads found.")
		return
	}

	for i, t := range summaries {
		// Line 1: [account]  * / (space)  Subject (from From)
		unreadMarker := "   "
		if t.Unread {
			unreadMarker = " * "
		}

		from := t.From
		subject := t.Subject
		if subject == "" {
			subject = "(no subject)"
		}

		fmt.Printf("[%s]%s%s (from %s)\n", t.Account, unreadMarker, subject, from)

		// Line 2: snippet preview (indented)
		snippet := t.Snippet
		if len(snippet) > 80 {
			snippet = snippet[:80] + "..."
		}
		fmt.Printf("            %s\n", snippet)

		// Line 3: thread ID, message count, date
		msgWord := "messages"
		if t.MessageCount == 1 {
			msgWord = "message"
		}
		fmt.Printf("            ID: %s | %d %s | %s\n", t.ID, t.MessageCount, msgWord, t.Date)

		// Blank line between threads, but not after the last one.
		if i < len(summaries)-1 {
			fmt.Println()
		}
	}
}

// truncate returns s truncated to maxLen with "..." appended if it was longer.
func truncate(s string, maxLen int) string {
	s = strings.TrimSpace(s)
	if len(s) <= maxLen {
		return s
	}
	return s[:maxLen] + "..."
}
