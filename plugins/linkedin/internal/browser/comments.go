package browser

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
)

// GetComments reads comments on a LinkedIn post via agent-browser.
func (c *CDPClient) GetComments(postURL string, count int) ([]api.Comment, error) {
	if count <= 0 {
		count = 10
	}

	// Convert activity URN to URL if needed
	url := postURL
	if strings.HasPrefix(postURL, "urn:li:activity:") {
		url = "https://www.linkedin.com/feed/update/" + postURL + "/"
	} else if strings.HasPrefix(postURL, "urn:li:ugcPost:") {
		url = "https://www.linkedin.com/feed/update/" + postURL + "/"
	} else if !strings.HasPrefix(postURL, "http") {
		// Assume bare numeric ID
		url = "https://www.linkedin.com/feed/update/urn:li:activity:" + postURL + "/"
	}

	// Navigate to post
	if err := c.Navigate(url); err != nil {
		return nil, fmt.Errorf("navigate to post: %w", err)
	}

	if err := c.WaitNetworkIdle(); err != nil {
		return nil, fmt.Errorf("waiting for post: %w", err)
	}
	_ = c.WaitMs(3000)

	// Get snapshot
	snapshot, err := c.Snapshot("main")
	if err != nil {
		return nil, fmt.Errorf("snapshot comments: %w", err)
	}

	comments := parseCommentsSnapshot(snapshot, count)
	return comments, nil
}

var commentReactionRe = regexp.MustCompile(`(\d+)\s*reaction`)

// parseCommentsSnapshot extracts comments from an agent-browser snapshot.
// Comments in LinkedIn's accessibility tree appear as blocks with author name,
// comment text, timestamp, and optional reaction count.
func parseCommentsSnapshot(snapshot string, limit int) []api.Comment {
	var comments []api.Comment
	lines := strings.Split(snapshot, "\n")

	// Look for comment-like patterns: blocks containing "View more options for X's comment"
	// or blocks with a profile link followed by text and a timestamp
	for i := 0; i < len(lines); i++ {
		trimmed := strings.TrimSpace(lines[i])

		// Pattern: button "View more options for X's comment."
		if strings.Contains(trimmed, "comment.\"") || strings.Contains(trimmed, "comment.'") {
			comment := extractComment(lines, i)
			if comment.Text != "" {
				comments = append(comments, comment)
				if len(comments) >= limit {
					break
				}
			}
		}
	}

	return comments
}

func extractComment(lines []string, idx int) api.Comment {
	var comment api.Comment

	// Search backward from the "View more options for X's comment" button
	// to find the author link, text, and timestamp
	searchStart := idx - 20
	if searchStart < 0 {
		searchStart = 0
	}

	// Find author name from "View more options for X's comment."
	trimmed := strings.TrimSpace(lines[idx])
	if start := strings.Index(trimmed, "for "); start > 0 {
		rest := trimmed[start+4:]
		if end := strings.Index(rest, "'s comment"); end > 0 {
			comment.AuthorName = rest[:end]
		}
	}

	// Search nearby lines for the comment text and timestamp
	for j := searchStart; j < idx && j < len(lines); j++ {
		line := strings.TrimSpace(lines[j])

		// Look for paragraph with substantial text (the comment body)
		if strings.HasPrefix(line, "- paragraph:") {
			pText := strings.TrimPrefix(line, "- paragraph:")
			pText = strings.TrimSpace(pText)
			// Skip short labels and timestamps
			if len(pText) > 10 && !isTimestamp(pText) {
				comment.Text = pText
			}
		}

		// Look for text nodes
		if strings.HasPrefix(line, "- text:") {
			tText := strings.TrimPrefix(line, "- text:")
			tText = strings.TrimSpace(tText)
			if len(tText) > len(comment.Text) && len(tText) > 10 {
				comment.Text = tText
			}
		}

		// Timestamp (e.g., "1w", "2d", "12h")
		if isTimestamp(strings.TrimSpace(line)) {
			comment.Timestamp = strings.TrimSpace(line)
		}
		if strings.HasPrefix(line, "- paragraph:") {
			pText := strings.TrimPrefix(line, "- paragraph:")
			pText = strings.TrimSpace(pText)
			if isTimestamp(pText) {
				comment.Timestamp = pText
			}
		}
	}

	// Look forward for reactions
	for j := idx; j < len(lines) && j < idx+10; j++ {
		line := strings.TrimSpace(lines[j])
		if m := commentReactionRe.FindStringSubmatch(line); len(m) > 1 {
			comment.Reactions, _ = strconv.Atoi(m[1])
			break
		}
	}

	return comment
}

func isTimestamp(s string) bool {
	matched, _ := regexp.MatchString(`^\d+[hmdw]$`, strings.TrimSpace(s))
	return matched
}
