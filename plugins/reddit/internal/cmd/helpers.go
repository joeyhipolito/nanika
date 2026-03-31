package cmd

import (
	"fmt"
	"regexp"
	"strings"

	"github.com/joeyhipolito/nanika-reddit/internal/api"
)

const divider = "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

// printPosts renders a list of posts to stdout in the standard card format.
func printPosts(posts []api.PostData) {
	for _, post := range posts {
		fmt.Println(divider)
		fmt.Printf("r/%s · u/%s · %s\n", post.Subreddit, post.Author, relativeTime(post.CreatedUTC))
		fmt.Printf("%s\n", post.Title)
		if post.IsSelf && post.SelfText != "" {
			text := post.SelfText
			if len(text) > 280 {
				text = text[:277] + "..."
			}
			fmt.Println(text)
		} else if !post.IsSelf {
			fmt.Printf("🔗 %s\n", post.URL)
		}
		fmt.Printf("↑ %d  💬 %d  ID: %s\n", post.Score, post.NumComments, post.Fullname)
	}
	fmt.Println(divider)
}

// redditURLPattern matches Reddit post URLs and extracts the post ID.
var redditURLPattern = regexp.MustCompile(`(?:reddit\.com|redd\.it)/(?:r/\w+/comments/)?(\w+)`)

// normalizePostID accepts a full Reddit URL, a fullname (t3_xxx), or a bare ID,
// and returns just the bare post ID.
func normalizePostID(input string) string {
	input = strings.TrimSpace(input)

	// Try URL pattern first
	if matches := redditURLPattern.FindStringSubmatch(input); len(matches) > 1 {
		return matches[1]
	}

	// Strip t3_ prefix
	if strings.HasPrefix(input, "t3_") {
		return input[3:]
	}

	// Strip t1_ prefix (comment, but might be used for post context)
	if strings.HasPrefix(input, "t1_") {
		return input[3:]
	}

	return input
}

// normalizeFullname ensures an ID has a type prefix.
// If it already has t1_ or t3_, returns as-is.
// Otherwise, adds the given prefix (e.g., "t3_").
func normalizeFullname(input, defaultPrefix string) string {
	input = strings.TrimSpace(input)

	// Try URL pattern first — extract bare ID
	if matches := redditURLPattern.FindStringSubmatch(input); len(matches) > 1 {
		return defaultPrefix + matches[1]
	}

	// Already has a type prefix
	if strings.HasPrefix(input, "t1_") || strings.HasPrefix(input, "t3_") {
		return input
	}

	return defaultPrefix + input
}

// normalizeSubreddit strips leading "r/" if present.
func normalizeSubreddit(input string) string {
	input = strings.TrimSpace(input)
	input = strings.TrimPrefix(input, "r/")
	input = strings.TrimPrefix(input, "/r/")
	return input
}

func maskValue(v string) string {
	if len(v) < 12 {
		if v == "" {
			return "(not set)"
		}
		return "****"
	}
	return v[:8] + "..." + v[len(v)-4:]
}
