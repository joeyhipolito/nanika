package browser

import (
	"encoding/json"
	"fmt"
	"strings"
)

// Pre-computed JSON string literals for the two button hints passed to
// fiberFindAndClickScript. json.Marshal of a plain string never fails, and
// these values never change, so we avoid the allocation on every call.
const (
	jsonHintLike    = `"like"`
	jsonHintComment = `"comment"`
)

// urnToFeedURL converts an activity or ugcPost URN to its LinkedIn feed URL.
func urnToFeedURL(urn string) string {
	return "https://www.linkedin.com/feed/update/" + urn + "/"
}

// fiberFindAndClickScript returns a JS IIFE that finds a post by URN and
// clicks the first button whose aria-label/text contains labelHint.
//
// It tries two strategies:
// 1. Feed view: scan [role='listitem'] elements with React fiber traversal
// 2. Single-post view: scan article elements (used on /feed/update/<urn>/ pages)
//
// Returns:
//
//	"clicked:<label>"   — success
//	"not-found"         — no element contained the URN
//	"no-button:<hint>"  — URN found but no matching button
func fiberFindAndClickScript(urnJSON, labelHintJSON string) string {
	return `(function(urn, hint) {
	function findInContainers(containers) {
		for (var i = 0; i < containers.length; i++) {
			var el = containers[i];
			var fk = Object.keys(el).find(function(k) { return k.startsWith("__reactFiber"); });
			if (!fk) continue;
			var node = el[fk];
			var found = false;
			for (var d = 0; d < 20 && node; d++) {
				try {
					var s = JSON.stringify(node.memoizedProps || {});
					if (s.indexOf(urn) !== -1) { found = true; break; }
				} catch(e) {}
				node = node.return;
			}
			if (!found) continue;
			var btns = el.querySelectorAll("button");
			for (var j = 0; j < btns.length; j++) {
				var lbl = (btns[j].getAttribute("aria-label") || btns[j].textContent || "").trim().toLowerCase();
				if (lbl.indexOf(hint) !== -1) {
					btns[j].click();
					return "clicked:" + lbl;
				}
			}
			return "no-button:" + hint;
		}
		return null;
	}
	var r = findInContainers(document.querySelectorAll("[role='listitem']"));
	if (r) return r;
	r = findInContainers(document.querySelectorAll("article"));
	if (r) return r;
	r = findInContainers(document.querySelectorAll("main > *"));
	if (r) return r;
	return "not-found";
})(` + urnJSON + `, ` + labelHintJSON + `)`
}

// insertTextScript focuses the first contenteditable comment box and inserts
// text via document.execCommand("insertText") — equivalent to a clipboard
// paste from the DOM's perspective. Returns "inserted" or an error string.
const insertTextScript = `(function(text) {
	var editors = document.querySelectorAll("[contenteditable='true']");
	var box = null;
	for (var i = 0; i < editors.length; i++) {
		var role = editors[i].getAttribute("role") || "";
		var ph = (editors[i].getAttribute("placeholder") ||
		          editors[i].getAttribute("aria-placeholder") || "").toLowerCase();
		if (role === "textbox" || ph.indexOf("comment") !== -1 || ph.indexOf("add") !== -1) {
			box = editors[i];
			break;
		}
	}
	if (!box && editors.length > 0) { box = editors[editors.length - 1]; }
	if (!box) { return "no-editor"; }
	box.focus();
	var ok = document.execCommand("insertText", false, text);
	return ok ? "inserted" : "execCommand-failed";
})`

// clickPostButtonScript clicks the comment submit button. LinkedIn renders it
// as a type="submit" button with text "Comment" and class containing
// "submit-button". We prioritize type="submit" to avoid clicking the
// action-bar Comment button instead.
const clickPostButtonScript = `(function() {
	var btns = document.querySelectorAll("button");
	// Priority 1: type="submit" button (the actual form submit)
	for (var i = 0; i < btns.length; i++) {
		if (btns[i].type === "submit") {
			btns[i].click();
			return "clicked:submit:" + btns[i].textContent.trim().toLowerCase();
		}
	}
	// Priority 2: button with submit-related class
	for (var i = 0; i < btns.length; i++) {
		if ((btns[i].className || "").indexOf("submit") !== -1) {
			btns[i].click();
			return "clicked:class:" + btns[i].textContent.trim().toLowerCase();
		}
	}
	// Priority 3: button with post/submit label
	var candidates = ["post comment", "add comment", "post", "submit"];
	for (var i = 0; i < btns.length; i++) {
		var lbl = (btns[i].getAttribute("aria-label") || btns[i].textContent || "").trim().toLowerCase();
		for (var c = 0; c < candidates.length; c++) {
			if (lbl === candidates[c] || lbl.indexOf(candidates[c]) !== -1) {
				btns[i].click();
				return "clicked:" + lbl;
			}
		}
	}
	return "no-post-button";
})()`

// checkFiberClickResult interprets the return value of fiberFindAndClickScript
// and returns a descriptive error when the script did not click anything.
// action is a human-readable name for the button ("Like", "Comment", etc.).
func checkFiberClickResult(result, noteURN, action string) error {
	switch {
	case strings.HasPrefix(result, "clicked:"):
		return nil
	case result == "not-found":
		return fmt.Errorf("post %s not found via fiber traversal (post may not be on page)", noteURN)
	case strings.HasPrefix(result, "no-button:"):
		return fmt.Errorf("post %s found but %s button not in DOM", noteURN, action)
	default:
		return fmt.Errorf("%s script unexpected result for %s: %s", action, noteURN, result)
	}
}

// directClickScript finds a button by aria-label on a single-post page
// where there's only one post. It clicks the first button whose aria-label
// exactly matches the target (to avoid clicking comment-level buttons).
const directClickScript = `(function(label) {
	var btns = document.querySelectorAll("button");
	for (var i = 0; i < btns.length; i++) {
		var lbl = (btns[i].getAttribute("aria-label") || "").trim().toLowerCase();
		if (lbl === label) {
			btns[i].click();
			return "clicked:" + lbl;
		}
	}
	return "no-button:" + label;
})`

// ReactViaCDP navigates to the post at noteURN and clicks the Like button.
// Tries fiber traversal first (works on feed pages), falls back to direct
// button match (works on single-post /feed/update/ pages).
func (c *CDPClient) ReactViaCDP(noteURN string) error {
	if noteURN == "" {
		return fmt.Errorf("noteURN is required")
	}

	if err := c.Navigate(urnToFeedURL(noteURN)); err != nil {
		return fmt.Errorf("navigating to post %s: %w", noteURN, err)
	}
	_ = c.WaitMs(5000)

	urnJSON, err := json.Marshal(noteURN)
	if err != nil {
		return fmt.Errorf("marshaling URN: %w", err)
	}

	// Try fiber traversal first (feed page with multiple posts)
	result, err := c.Eval(fiberFindAndClickScript(string(urnJSON), jsonHintLike))
	if err == nil {
		if checkFiberClickResult(unwrapEvalString(result), noteURN, "Like") == nil {
			_ = c.WaitMs(1000)
			return nil
		}
	}

	// Fallback: single-post page — click the post's own "react like" button directly
	result, err = c.Eval(fmt.Sprintf("(%s)(%q)", directClickScript, "react like"))
	if err != nil {
		return fmt.Errorf("direct react script for %s: %w", noteURN, err)
	}
	if !strings.HasPrefix(unwrapEvalString(result), "clicked:") {
		return fmt.Errorf("Like button not found on %s (result: %s)", noteURN, result)
	}

	_ = c.WaitMs(1000)
	return nil
}

// CommentViaCDP navigates to the post at noteURN, clicks the Comment button,
// pastes text, and submits. Tries fiber traversal first, falls back to direct
// button match for single-post pages.
func (c *CDPClient) CommentViaCDP(noteURN string, text string) error {
	if noteURN == "" {
		return fmt.Errorf("noteURN is required")
	}
	if text == "" {
		return fmt.Errorf("comment text is required")
	}

	if err := c.Navigate(urnToFeedURL(noteURN)); err != nil {
		return fmt.Errorf("navigating to post %s: %w", noteURN, err)
	}
	_ = c.WaitMs(5000)

	urnJSON, err := json.Marshal(noteURN)
	if err != nil {
		return fmt.Errorf("marshaling URN: %w", err)
	}

	// Click the Comment button — try fiber first, then direct match
	result, err := c.Eval(fiberFindAndClickScript(string(urnJSON), jsonHintComment))
	clicked := err == nil && checkFiberClickResult(unwrapEvalString(result), noteURN, "Comment") == nil
	if !clicked {
		// Fallback: on single-post page, the comment box may already be visible,
		// or we can click the "Comment" button directly
		result, err = c.Eval(fmt.Sprintf("(%s)(%q)", directClickScript, "comment"))
		if err != nil {
			return fmt.Errorf("comment click for %s: %w", noteURN, err)
		}
		// It's OK if "no-button" — on single-post pages the comment box is often already open
	}

	_ = c.WaitMs(2000)

	// Paste text into the contenteditable editor
	textJSON, err := json.Marshal(text)
	if err != nil {
		return fmt.Errorf("marshaling comment text: %w", err)
	}
	pasteResult, err := c.Eval(fmt.Sprintf("(%s)(%s)", insertTextScript, string(textJSON)))
	if err != nil {
		return fmt.Errorf("inserting comment text on %s: %w", noteURN, err)
	}
	if unwrapEvalString(pasteResult) == "no-editor" {
		return fmt.Errorf("comment editor not found on %s", noteURN)
	}

	_ = c.WaitMs(500)

	// Click the Post/Submit button
	submitResult, err := c.Eval(clickPostButtonScript)
	if err != nil {
		return fmt.Errorf("clicking Post button on %s: %w", noteURN, err)
	}
	if !strings.HasPrefix(unwrapEvalString(submitResult), "clicked:") {
		return fmt.Errorf("Post button not found on %s (script returned: %s)", noteURN, submitResult)
	}

	_ = c.WaitMs(1500)
	return nil
}

// unwrapEvalString handles agent-browser eval's double-encoding: the JS return
// value arrives as a JSON string literal, e.g. `"\"value\""` → `"value"`.
// Falls back to TrimSpace if the value is not a JSON-quoted string.
func unwrapEvalString(s string) string {
	var inner string
	if err := json.Unmarshal([]byte(s), &inner); err == nil {
		return inner
	}
	return strings.TrimSpace(s)
}
