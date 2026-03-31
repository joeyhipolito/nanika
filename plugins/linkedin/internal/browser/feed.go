package browser

import (
	"encoding/json"
	"fmt"
	"regexp"
	"strconv"
	"strings"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
)

// extractScript is injected into the page via CDP eval to extract feed items
// from LinkedIn's DOM. LinkedIn obfuscates CSS class names, so we use stable
// selectors: role="listitem" for post containers, aria-label on control-menu
// buttons for author names, data-view-name for post text, and React fiber
// tree traversal for activity URNs (which are not exposed in DOM attributes).
const extractScript = `(function() {
	var items = document.querySelectorAll("[role='listitem']");
	var results = [];
	for (var i = 0; i < items.length; i++) {
		var el = items[i];
		var h2 = el.querySelector("h2");
		if (!h2 || h2.textContent.trim() !== "Feed post") continue;
		if (el.textContent.indexOf("Promoted") !== -1) continue;

		// Activity URN via React fiber tree traversal
		var fiberKey = Object.keys(el).find(function(k) { return k.startsWith("__reactFiber"); });
		var urn = "";
		if (fiberKey) {
			var node = el[fiberKey];
			for (var d = 0; d < 12 && node; d++) {
				try {
					var s = JSON.stringify(node.memoizedProps || {});
					var m = s.match(/urn:li:(activity|ugcPost):\d+/);
					if (m) { urn = m[0]; break; }
				} catch(e) {}
				node = node.return;
			}
		}

		// Author from control menu button aria-label
		var author = "";
		var ctrlBtn = el.querySelector("button[aria-label^='Open control menu for post by']");
		if (ctrlBtn) {
			author = ctrlBtn.getAttribute("aria-label").replace("Open control menu for post by ", "");
		}

		// Headline from second <p> in the author profile link
		var headline = "";
		var authorLinks = el.querySelectorAll("a[href*='/in/']");
		for (var al = 0; al < authorLinks.length; al++) {
			var ps = authorLinks[al].querySelectorAll("p");
			if (ps.length >= 2) { headline = ps[1].textContent.trim(); break; }
		}

		// Timestamp from paragraphs matching Nh/Nd/Nw/Nm pattern
		var timestamp = "";
		for (var al = 0; al < authorLinks.length; al++) {
			var ps = authorLinks[al].querySelectorAll("p");
			for (var pi = 0; pi < ps.length; pi++) {
				var pt = ps[pi].textContent.trim();
				if (/^\d+[hmdw]\s*[^\w]*$/.test(pt)) {
					timestamp = pt.replace(/\s*[^0-9hmdw].*$/, "").trim();
					break;
				}
			}
			if (timestamp) break;
		}

		// Post text from feed-commentary data-view-name or largest <p>
		var text = "";
		var commentary = el.querySelector("[data-view-name='feed-commentary']");
		if (commentary) { text = commentary.textContent.trim(); }
		if (!text) {
			var allP = el.querySelectorAll("p");
			var best = "";
			for (var p = 0; p < allP.length; p++) {
				var pText = allP[p].textContent.trim();
				if (pText.length > best.length && pText.length > 80) best = pText;
			}
			text = best;
		}

		// Counts from <p> tags (not buttons — LinkedIn wraps counts in paragraphs)
		var reactions = 0, comments = 0, reposts = 0;
		var allP2 = el.querySelectorAll("p");
		for (var p = 0; p < allP2.length; p++) {
			var pt2 = allP2[p].textContent.trim();
			var rm = pt2.match(/^(\d[\d,]*)\s*reaction/);
			if (rm) reactions = parseInt(rm[1].replace(/,/g, ""));
			var cm = pt2.match(/^(\d[\d,]*)\s*comment/);
			if (cm) comments = parseInt(cm[1].replace(/,/g, ""));
			var rp = pt2.match(/^(\d[\d,]*)\s*repost/);
			if (rp) reposts = parseInt(rp[1].replace(/,/g, ""));
		}

		if (!urn) continue;
		results.push({
			activity_urn: urn, author_name: author, author_headline: headline,
			text: text, timestamp: timestamp,
			reaction_count: reactions, comment_count: comments, repost_count: reposts
		});
	}
	return JSON.stringify(results);
})()`

// GetFeed reads the LinkedIn feed via CDP eval and returns parsed feed items.
// It first tries a JS-eval approach (extractScript) that reads activity URNs
// directly from React component attributes. If that yields nothing it falls
// back to the accessibility-tree snapshot parser.
func (c *CDPClient) GetFeed(count int) ([]api.FeedItem, error) {
	if count <= 0 {
		count = 10
	}

	// Navigate to feed
	if err := c.Navigate("https://www.linkedin.com/feed/"); err != nil {
		return nil, fmt.Errorf("navigate to feed: %w", err)
	}

	// Wait for page to render (LinkedIn never reaches networkidle due to websockets)
	_ = c.WaitMs(5000)

	// Scroll to load more posts if needed
	if count > 3 {
		scrolls := count / 3
		if scrolls > 5 {
			scrolls = 5
		}
		for i := 0; i < scrolls; i++ {
			_ = c.Scroll(2000)
			_ = c.WaitMs(2000)
		}
	}

	// Primary: JS-eval approach extracts true activity URNs from React attributes.
	items, err := c.evalFeedItems(count)
	if err == nil && len(items) > 0 {
		return items, nil
	}

	// Fallback: accessibility-tree snapshot parser.
	snapshot, snapshotErr := c.Snapshot("main")
	if snapshotErr != nil {
		if err != nil {
			return nil, fmt.Errorf("eval feed (primary): %w; snapshot (fallback): %w", err, snapshotErr)
		}
		return nil, fmt.Errorf("snapshot feed: %w", snapshotErr)
	}

	items = parseFeedSnapshot(snapshot, count)
	if len(items) == 0 {
		return nil, fmt.Errorf("no feed posts found. You may not be logged in. Run 'linkedin doctor'")
	}
	return items, nil
}

// evalFeedItems injects extractScript into the page via CDP and unmarshals
// the returned JSON array into []api.FeedItem.
func (c *CDPClient) evalFeedItems(limit int) ([]api.FeedItem, error) {
	raw, err := c.Eval(extractScript)
	if err != nil {
		return nil, fmt.Errorf("eval extractScript: %w", err)
	}

	// agent-browser eval wraps the JS return value in quotes as a JSON string.
	// First try to unwrap it as a JSON string, then parse the inner JSON array.
	raw = strings.TrimSpace(raw)

	var innerJSON string
	if err := json.Unmarshal([]byte(raw), &innerJSON); err != nil {
		// Not a quoted string — try as direct JSON
		innerJSON = raw
	}

	var all []api.FeedItem
	if err := json.Unmarshal([]byte(innerJSON), &all); err != nil {
		return nil, fmt.Errorf("unmarshal feed JSON: %w (raw prefix: %.200s)", err, innerJSON)
	}

	if len(all) > limit {
		all = all[:limit]
	}
	return all, nil
}

// parseFeedSnapshot extracts feed items from an agent-browser accessibility tree snapshot.
// Feed posts are listitem blocks starting with heading "Feed post".
func parseFeedSnapshot(snapshot string, limit int) []api.FeedItem {
	var items []api.FeedItem

	// Split snapshot into listitem blocks
	lines := strings.Split(snapshot, "\n")

	type block struct {
		lines []string
	}

	var blocks []block
	var current *block

	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if trimmed == "- listitem:" {
			if current != nil {
				blocks = append(blocks, *current)
			}
			current = &block{}
		}
		if current != nil {
			current.lines = append(current.lines, line)
		}
	}
	if current != nil {
		blocks = append(blocks, *current)
	}

	for _, b := range blocks {
		text := strings.Join(b.lines, "\n")

		// Must contain "Feed post" heading
		if !strings.Contains(text, `heading "Feed post"`) {
			continue
		}

		// Skip promoted posts
		if strings.Contains(text, "Promoted") {
			continue
		}

		item := extractFeedItem(b.lines)
		if item.AuthorName == "" && item.Text == "" {
			continue
		}

		items = append(items, item)
		if len(items) >= limit {
			break
		}
	}

	return items
}

var (
	// Match: button "N reactions"
	reactionRe = regexp.MustCompile(`button "(\d+) reaction`)
	// Match: button "N comments"
	commentRe = regexp.MustCompile(`button "(\d+) comment`)
	// Match: link "View X's profile" with URL
	profileURLRe = regexp.MustCompile(`/url: (https://www\.linkedin\.com/in/[^/]+/)`)
)

func extractFeedItem(lines []string) api.FeedItem {
	var item api.FeedItem
	text := strings.Join(lines, "\n")

	// Extract author info from the author link
	// Pattern: link "Name • Degree\nHeadline\nTimestamp •" [ref=...]
	// The link with profile URL and multi-line content contains name, headline, time
	for i, line := range lines {
		trimmed := strings.TrimSpace(line)

		// Find the author link (contains • and timestamp like "12h •" or "1w •")
		if strings.Contains(trimmed, "link \"") && strings.Contains(trimmed, "•") &&
			!strings.Contains(trimmed, "View") && !strings.Contains(trimmed, "Follow") {
			// Extract the link text
			start := strings.Index(trimmed, `"`) + 1
			end := strings.LastIndex(trimmed, `"`)
			if start > 0 && end > start {
				linkText := trimmed[start:end]
				parts := strings.Split(linkText, " • ")
				if len(parts) >= 1 {
					item.AuthorName = strings.TrimSpace(parts[0])
				}
			}

			// Look at child paragraph lines for headline and timestamp
			for j := i + 1; j < len(lines) && j < i+5; j++ {
				child := strings.TrimSpace(lines[j])
				if strings.HasPrefix(child, "- paragraph:") {
					pText := strings.TrimPrefix(child, "- paragraph:")
					pText = strings.TrimSpace(pText)
					if item.AuthorHeadline == "" && pText != "" && !strings.Contains(pText, "•") {
						item.AuthorHeadline = pText
					}
					// Timestamp pattern: "12h •" or "1w •" or "2d •"
					if matched, _ := regexp.MatchString(`^\d+[hmdw]\s*•?$`, pText); matched {
						item.Timestamp = strings.TrimSuffix(strings.TrimSpace(pText), "•")
						item.Timestamp = strings.TrimSpace(item.Timestamp)
					}
				}
			}
			break
		}
	}

	// Extract post text — find the largest paragraph block that's not a button/link label
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if strings.HasPrefix(trimmed, "- paragraph: \"") || strings.HasPrefix(trimmed, "- paragraph: "+"\"") {
			// This is a quoted paragraph — likely the post text
			pText := strings.TrimPrefix(trimmed, "- paragraph: ")
			pText = strings.Trim(pText, "\"")
			if len(pText) > len(item.Text) {
				item.Text = pText
			}
		} else if strings.HasPrefix(trimmed, "- paragraph:") {
			pText := strings.TrimPrefix(trimmed, "- paragraph:")
			pText = strings.TrimSpace(pText)
			if len(pText) > len(item.Text) && len(pText) > 50 {
				item.Text = pText
			}
		}
	}

	// If we still don't have text, try a different approach — find text blocks
	if item.Text == "" {
		for _, line := range lines {
			trimmed := strings.TrimSpace(line)
			if strings.HasPrefix(trimmed, "- text:") {
				tText := strings.TrimPrefix(trimmed, "- text:")
				tText = strings.TrimSpace(tText)
				if len(tText) > len(item.Text) && len(tText) > 30 {
					item.Text = tText
				}
			}
		}
	}

	// Extract reaction count
	if m := reactionRe.FindStringSubmatch(text); len(m) > 1 {
		item.ReactionCount, _ = strconv.Atoi(m[1])
	}

	// Extract comment count
	if m := commentRe.FindStringSubmatch(text); len(m) > 1 {
		item.CommentCount, _ = strconv.Atoi(m[1])
	}

	// Extract profile URL as activity reference
	if m := profileURLRe.FindStringSubmatch(text); len(m) > 1 {
		item.ActivityURN = m[1]
	}

	return item
}
