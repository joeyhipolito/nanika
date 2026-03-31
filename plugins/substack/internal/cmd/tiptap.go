package cmd

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/joeyhipolito/nanika-substack/internal/tiptap"
)

// Type aliases — these types now live in internal/tiptap but are used
// extensively throughout this file. Aliases avoid a massive rename.
type TiptapDoc = tiptap.Doc
type TiptapNode = tiptap.Node
type TiptapMark = tiptap.Mark

// ImageResolver is called for each code block found in the MDX.
// It receives the 0-based code block index and returns an image URL if available.
// If it returns "", the code block is rendered as a codeBlock node (text fallback).
type ImageResolver func(index int) string

// markdownToTiptap converts markdown text to a Tiptap JSON string.
// If siteURL is non-empty, relative links (starting with /) are rewritten to absolute URLs.
// If imgResolver is non-nil, <CodeBlock> JSX and fenced code blocks are replaced with images.
func markdownToTiptap(md string, siteURL string, imgResolver ImageResolver) (string, error) {
	lines := splitLines(md)
	var nodes []TiptapNode
	codeBlockIndex := 0 // tracks sequential code block index (fenced + JSX)
	i := 0

	for i < len(lines) {
		line := lines[i]

		// Skip empty lines
		if trimSpace(line) == "" {
			i++
			continue
		}

		// Horizontal rule
		trimmed := trimSpace(line)
		if trimmed == "---" || trimmed == "***" || trimmed == "___" {
			nodes = append(nodes, TiptapNode{Type: "horizontalRule"})
			i++
			continue
		}

		// Headings
		if level, text := parseHeading(line); level > 0 {
			nodes = append(nodes, TiptapNode{
				Type:    "heading",
				Attrs:   map[string]interface{}{"level": level},
				Content: parseInlineMarks(text),
			})
			i++
			continue
		}

		// Code blocks
		if len(line) >= 3 && line[:3] == "```" {
			lang := trimSpace(line[3:])
			var codeLines []string
			i++
			for i < len(lines) && !(len(lines[i]) >= 3 && trimSpace(lines[i]) == "```") {
				codeLines = append(codeLines, lines[i])
				i++
			}
			if i < len(lines) {
				i++ // skip closing ```
			}
			code := joinLines(codeLines)

			// Try image replacement
			if imgResolver != nil {
				if imgURL := imgResolver(codeBlockIndex); imgURL != "" {
					nodes = append(nodes, makeCaptionedImage(imgURL, lang))
					codeBlockIndex++
					continue
				}
			}
			codeBlockIndex++

			node := TiptapNode{
				Type:    "codeBlock",
				Content: []TiptapNode{{Type: "text", Text: code}},
			}
			if lang != "" {
				node.Attrs = map[string]interface{}{"language": lang}
			}
			nodes = append(nodes, node)
			continue
		}

		// Blockquote
		if len(line) >= 2 && line[:2] == "> " {
			var bqLines []string
			for i < len(lines) && len(lines[i]) >= 2 && lines[i][:2] == "> " {
				bqLines = append(bqLines, lines[i][2:])
				i++
			}
			nodes = append(nodes, TiptapNode{
				Type: "blockquote",
				Content: []TiptapNode{{
					Type:    "paragraph",
					Content: parseInlineMarks(joinLines(bqLines)),
				}},
			})
			continue
		}

		// Unordered list
		if len(line) >= 2 && (line[:2] == "- " || line[:2] == "* ") {
			var items []TiptapNode
			for i < len(lines) && len(lines[i]) >= 2 && (lines[i][:2] == "- " || lines[i][:2] == "* ") {
				items = append(items, TiptapNode{
					Type: "listItem",
					Content: []TiptapNode{{
						Type:    "paragraph",
						Content: parseInlineMarks(lines[i][2:]),
					}},
				})
				i++
			}
			nodes = append(nodes, TiptapNode{
				Type:    "bulletList",
				Content: items,
			})
			continue
		}

		// Ordered list
		if isOrderedListItem(line) {
			var items []TiptapNode
			for i < len(lines) && isOrderedListItem(lines[i]) {
				text := extractOrderedListText(lines[i])
				items = append(items, TiptapNode{
					Type: "listItem",
					Content: []TiptapNode{{
						Type:    "paragraph",
						Content: parseInlineMarks(text),
					}},
				})
				i++
			}
			nodes = append(nodes, TiptapNode{
				Type:    "orderedList",
				Content: items,
			})
			continue
		}

		// Table
		if len(line) > 0 && line[0] == '|' {
			var tableLines []string
			for i < len(lines) && len(lines[i]) > 0 && lines[i][0] == '|' {
				tableLines = append(tableLines, lines[i])
				i++
			}
			if tbl := convertTableTiptap(tableLines); tbl != nil {
				nodes = append(nodes, *tbl)
			}
			continue
		}

		// JSX <CodeBlock> component — convert to image or code block
		if strings.HasPrefix(trimmed, "<CodeBlock") {
			jsxLines := []string{line}
			if !strings.HasSuffix(trimmed, "/>") {
				i++
				for i < len(lines) {
					jsxLines = append(jsxLines, lines[i])
					t := trimSpace(lines[i])
					if t == "/>" || strings.HasSuffix(t, "/>") {
						i++
						break
					}
					i++
				}
			} else {
				i++
			}

			// Try image replacement
			if imgResolver != nil {
				if imgURL := imgResolver(codeBlockIndex); imgURL != "" {
					lang := extractJSXProp(jsxLines, "language")
					nodes = append(nodes, makeCaptionedImage(imgURL, lang))
					codeBlockIndex++
					continue
				}
			}

			// Fallback: extract code and render as codeBlock
			code := extractJSXCodeContent(jsxLines)
			lang := extractJSXProp(jsxLines, "language")
			codeBlockIndex++
			if code != "" {
				node := TiptapNode{
					Type:    "codeBlock",
					Content: []TiptapNode{{Type: "text", Text: code}},
				}
				if lang != "" {
					node.Attrs = map[string]interface{}{"language": lang}
				}
				nodes = append(nodes, node)
			}
			continue
		}

		// JSX <Figure> component — convert to image2 node
		if strings.HasPrefix(trimmed, "<Figure") {
			src := extractJSXProp([]string{line}, "src")
			alt := extractJSXProp([]string{line}, "alt")

			// Skip to end of Figure (self-closing or closing tag)
			if strings.HasSuffix(trimmed, "/>") {
				i++
			} else {
				i++
				for i < len(lines) {
					if trimSpace(lines[i]) == "</Figure>" {
						i++
						break
					}
					i++
				}
			}

			// Emit image2 node if src found
			if src != "" {
				node := makeCaptionedImage(src, "")
				if alt != "" {
					node.Attrs["alt"] = alt
				}
				nodes = append(nodes, node)
			}
			continue
		}

		// Other JSX components — strip them
		if len(trimmed) > 0 && trimmed[0] == '<' && !isHTMLBlockTiptap(trimmed) {
			if trimmed[len(trimmed)-1] == '>' && (trimmed[len(trimmed)-2] == '/' || isClosingTagTiptap(trimmed)) {
				i++
				continue
			}
			i++
			for i < len(lines) {
				if isClosingTagTiptap(trimSpace(lines[i])) {
					i++
					break
				}
				i++
			}
			continue
		}

		// Image: ![alt](url)
		if len(trimmed) >= 5 && trimmed[:2] == "![" {
			if alt, src, ok := parseImage(trimmed); ok {
				attrs := map[string]interface{}{"src": src}
				if alt != "" {
					attrs["alt"] = alt
				}
				nodes = append(nodes, TiptapNode{
					Type:  "image",
					Attrs: attrs,
				})
				i++
				continue
			}
		}

		// Paragraph
		var paraLines []string
		for i < len(lines) && trimSpace(lines[i]) != "" {
			l := lines[i]
			if len(l) >= 2 && l[0] == '#' {
				break
			}
			if len(l) >= 3 && l[:3] == "```" {
				break
			}
			if len(l) >= 2 && (l[:2] == "- " || l[:2] == "* " || l[:2] == "> ") {
				break
			}
			if len(l) > 0 && l[0] == '|' {
				break
			}
			if isOrderedListItem(l) {
				break
			}
			paraLines = append(paraLines, l)
			i++
		}
		if len(paraLines) > 0 {
			nodes = append(nodes, TiptapNode{
				Type:    "paragraph",
				Content: parseInlineMarks(joinLines(paraLines)),
			})
		}
	}

	doc := TiptapDoc{Type: "doc", Content: nodes}
	if siteURL != "" {
		rewriteRelativeLinks(doc.Content, siteURL)
	}
	b, err := json.Marshal(doc)
	if err != nil {
		return "", fmt.Errorf("marshaling tiptap doc: %w", err)
	}
	return string(b), nil
}

// parseHeading returns the heading level and text, or 0 if not a heading.
func parseHeading(line string) (int, string) {
	level := 0
	for level < len(line) && line[level] == '#' {
		level++
	}
	if level > 0 && level <= 6 && level < len(line) && line[level] == ' ' {
		return level, line[level+1:]
	}
	return 0, ""
}

// parseImage parses ![alt](src) and returns components.
func parseImage(s string) (alt, src string, ok bool) {
	if len(s) < 5 || s[:2] != "![" {
		return "", "", false
	}
	closeBracket := -1
	for j := 2; j < len(s); j++ {
		if s[j] == ']' {
			closeBracket = j
			break
		}
	}
	if closeBracket < 0 || closeBracket+1 >= len(s) || s[closeBracket+1] != '(' {
		return "", "", false
	}
	closeParen := -1
	for j := closeBracket + 2; j < len(s); j++ {
		if s[j] == ')' {
			closeParen = j
			break
		}
	}
	if closeParen < 0 {
		return "", "", false
	}
	return s[2:closeBracket], s[closeBracket+2 : closeParen], true
}

// parseInlineMarks converts inline markdown (bold, italic, code, links) to TiptapNode text nodes.
func parseInlineMarks(text string) []TiptapNode {
	if text == "" {
		return nil
	}
	var nodes []TiptapNode
	i := 0

	for i < len(text) {
		// Bold: **text**
		if i+1 < len(text) && text[i] == '*' && text[i+1] == '*' {
			end := findDelimiter(text, i+2, "**")
			if end >= 0 {
				inner := text[i+2 : end]
				for _, child := range parseInlineMarksInner(inner) {
					child.Marks = append([]TiptapMark{{Type: "bold"}}, child.Marks...)
					nodes = append(nodes, child)
				}
				i = end + 2
				continue
			}
		}

		// Italic: *text*
		if text[i] == '*' {
			end := findDelimiter(text, i+1, "*")
			if end >= 0 {
				inner := text[i+1 : end]
				for _, child := range parseInlineMarksInner(inner) {
					child.Marks = append([]TiptapMark{{Type: "italic"}}, child.Marks...)
					nodes = append(nodes, child)
				}
				i = end + 1
				continue
			}
			// Unmatched * — consume as plain text to avoid infinite loop
			nodes = append(nodes, TiptapNode{Type: "text", Text: "*"})
			i++
			continue
		}

		// Inline code: `text`
		if text[i] == '`' {
			end := -1
			for j := i + 1; j < len(text); j++ {
				if text[j] == '`' {
					end = j
					break
				}
			}
			if end >= 0 {
				nodes = append(nodes, TiptapNode{
					Type:  "text",
					Text:  text[i+1 : end],
					Marks: []TiptapMark{{Type: "code"}},
				})
				i = end + 1
				continue
			}
		}

		// Link: [text](url)
		if text[i] == '[' {
			if linkText, href, endPos, ok := parseLink(text, i); ok {
				linkMark := TiptapMark{
					Type:  "link",
					Attrs: map[string]interface{}{"href": href},
				}
				for _, child := range parseInlineMarksInner(linkText) {
					child.Marks = append(child.Marks, linkMark)
					nodes = append(nodes, child)
				}
				i = endPos
				continue
			}
			// Not a valid link — consume the '[' as plain text to avoid infinite loop
			nodes = append(nodes, TiptapNode{Type: "text", Text: "["})
			i++
			continue
		}

		// Plain text — consume until the next special character
		start := i
		for i < len(text) && text[i] != '*' && text[i] != '`' && text[i] != '[' {
			i++
		}
		if i > start {
			nodes = append(nodes, TiptapNode{Type: "text", Text: text[start:i]})
		}
	}

	return nodes
}

// parseInlineMarksInner handles nested inline marks (e.g. bold inside a link).
// It handles code and links but not bold/italic to avoid infinite recursion.
func parseInlineMarksInner(text string) []TiptapNode {
	if text == "" {
		return nil
	}
	var nodes []TiptapNode
	i := 0

	for i < len(text) {
		// Inline code
		if text[i] == '`' {
			end := -1
			for j := i + 1; j < len(text); j++ {
				if text[j] == '`' {
					end = j
					break
				}
			}
			if end >= 0 {
				nodes = append(nodes, TiptapNode{
					Type:  "text",
					Text:  text[i+1 : end],
					Marks: []TiptapMark{{Type: "code"}},
				})
				i = end + 1
				continue
			}
		}

		start := i
		for i < len(text) && text[i] != '`' {
			i++
		}
		if i > start {
			nodes = append(nodes, TiptapNode{Type: "text", Text: text[start:i]})
		}
	}

	return nodes
}

// findDelimiter finds the next occurrence of delim starting at pos.
func findDelimiter(text string, pos int, delim string) int {
	for j := pos; j+len(delim) <= len(text); j++ {
		if text[j:j+len(delim)] == delim {
			return j
		}
	}
	return -1
}

// parseLink parses [text](url) starting at position i. Returns text, href, end position, and ok.
func parseLink(text string, i int) (linkText, href string, endPos int, ok bool) {
	if text[i] != '[' {
		return "", "", 0, false
	}
	closeBracket := -1
	for j := i + 1; j < len(text); j++ {
		if text[j] == ']' {
			closeBracket = j
			break
		}
	}
	if closeBracket < 0 || closeBracket+1 >= len(text) || text[closeBracket+1] != '(' {
		return "", "", 0, false
	}
	closeParen := -1
	for j := closeBracket + 2; j < len(text); j++ {
		if text[j] == ')' {
			closeParen = j
			break
		}
	}
	if closeParen < 0 {
		return "", "", 0, false
	}
	return text[i+1 : closeBracket], text[closeBracket+2 : closeParen], closeParen + 1, true
}

// convertTableTiptap converts markdown table lines to a code block.
// Substack's Tiptap schema does not support table/tableRow/tableCell nodes,
// so we render tables as preformatted text to preserve the tabular layout.
func convertTableTiptap(lines []string) *TiptapNode {
	if len(lines) < 2 {
		return nil
	}

	// Collect header + body rows (skip separator at lines[1])
	var rows [][]string
	rows = append(rows, splitTableRowTrimmed(lines[0]))
	for _, line := range lines[2:] {
		rows = append(rows, splitTableRowTrimmed(line))
	}

	// Calculate column widths
	cols := 0
	for _, row := range rows {
		if len(row) > cols {
			cols = len(row)
		}
	}
	widths := make([]int, cols)
	for _, row := range rows {
		for i, cell := range row {
			if len(cell) > widths[i] {
				widths[i] = len(cell)
			}
		}
	}

	// Build padded text
	var text string
	for ri, row := range rows {
		for ci := 0; ci < cols; ci++ {
			cell := ""
			if ci < len(row) {
				cell = row[ci]
			}
			if ci > 0 {
				text += " | "
			}
			text += cell
			for pad := len(cell); pad < widths[ci]; pad++ {
				text += " "
			}
		}
		text += "\n"
		// Add separator after header
		if ri == 0 {
			for ci := 0; ci < cols; ci++ {
				if ci > 0 {
					text += "-+-"
				}
				for j := 0; j < widths[ci]; j++ {
					text += "-"
				}
			}
			text += "\n"
		}
	}

	return &TiptapNode{
		Type:    "codeBlock",
		Content: []TiptapNode{{Type: "text", Text: text}},
	}
}

// splitTableRowTrimmed splits a table row and trims each cell.
func splitTableRowTrimmed(line string) []string {
	cells := splitTableRow(line)
	for i := range cells {
		cells[i] = trimSpace(cells[i])
	}
	return cells
}

// isHTMLBlockTiptap checks if a string starts with a known HTML tag.
func isHTMLBlockTiptap(s string) bool {
	htmlTags := []string{"<p>", "<p ", "<div>", "<div ", "<span>", "<span ",
		"<table>", "<ul>", "<ol>", "<li>", "<br", "<hr", "<img "}
	for _, tag := range htmlTags {
		if len(s) >= len(tag) && s[:len(tag)] == tag {
			return true
		}
	}
	return false
}

// isClosingTagTiptap checks if a string is a closing HTML/JSX tag.
func isClosingTagTiptap(s string) bool {
	return len(s) >= 3 && s[:2] == "</"
}

// isAbsoluteFilePath returns true if the path looks like an absolute filesystem path
// (e.g. /Users/..., /home/..., /tmp/...) rather than a site-relative URL (e.g. /blog/...).
func isAbsoluteFilePath(path string) bool {
	// Common absolute filesystem path prefixes
	prefixes := []string{"/Users/", "/home/", "/tmp/", "/var/", "/opt/", "/etc/", "/private/"}
	for _, p := range prefixes {
		if len(path) >= len(p) && path[:len(p)] == p {
			return true
		}
	}
	return false
}

// makeCaptionedImage creates a Substack image2 Tiptap node.
func makeCaptionedImage(src, lang string) TiptapNode {
	attrs := map[string]interface{}{
		"src":              src,
		"fullscreen":       false,
		"imageSize":        "normal",
		"height":           nil,
		"width":            nil,
		"resizeWidth":      nil,
		"bytes":            nil,
		"alt":              nil,
		"title":            nil,
		"type":             nil,
		"href":             nil,
		"belowTheFold":     false,
		"internalRedirect": nil,
	}
	if lang != "" {
		attrs["alt"] = lang
	}
	return TiptapNode{
		Type:  "image2",
		Attrs: attrs,
	}
}

// extractJSXProp extracts a string prop value from JSX lines, e.g. language="go".
func extractJSXProp(jsxLines []string, prop string) string {
	target := prop + "="
	for _, line := range jsxLines {
		idx := strings.Index(line, target)
		if idx < 0 {
			continue
		}
		rest := line[idx+len(target):]
		if len(rest) == 0 {
			continue
		}
		q := rest[0]
		if q == '"' || q == '\'' {
			end := strings.IndexByte(rest[1:], q)
			if end >= 0 {
				return rest[1 : end+1]
			}
		}
	}
	return ""
}

// extractJSXCodeContent extracts the code content from <CodeBlock code={...} /> JSX.
func extractJSXCodeContent(jsxLines []string) string {
	joined := strings.Join(jsxLines, "\n")

	// Find code={ start
	idx := strings.Index(joined, "code={")
	if idx < 0 {
		return ""
	}
	rest := joined[idx+6:] // after "code={"

	if len(rest) == 0 {
		return ""
	}

	switch rest[0] {
	case '`':
		// Backtick template: code={`...`}
		end := strings.Index(rest[1:], "`}")
		if end < 0 {
			return ""
		}
		return rest[1 : end+1]
	case '"':
		// String literal: code={"..."}
		end := strings.Index(rest[1:], "\"}")
		if end < 0 {
			return ""
		}
		code := rest[1 : end+1]
		code = strings.ReplaceAll(code, "\\n", "\n")
		code = strings.ReplaceAll(code, "\\\"", "\"")
		return code
	default:
		// Concatenation pattern or other — extract between { and }
		depth := 1
		for i := 0; i < len(rest); i++ {
			if rest[i] == '{' {
				depth++
			} else if rest[i] == '}' {
				depth--
				if depth == 0 {
					return resolveJSXConcatSubstack(rest[:i])
				}
			}
		}
		return ""
	}
}

// resolveJSXConcatSubstack handles backtick + double-quote concatenation for Go struct tags.
func resolveJSXConcatSubstack(s string) string {
	s = strings.TrimSpace(s)
	var parts []string
	remaining := s
	for len(remaining) > 0 {
		remaining = strings.TrimSpace(remaining)
		if len(remaining) == 0 {
			break
		}
		if remaining[0] == '+' {
			remaining = strings.TrimSpace(remaining[1:])
			continue
		}
		if remaining[0] == '`' {
			end := strings.IndexByte(remaining[1:], '`')
			if end < 0 {
				parts = append(parts, remaining[1:])
				break
			}
			parts = append(parts, remaining[1:end+1])
			remaining = remaining[end+2:]
		} else if remaining[0] == '"' {
			end := findUnescapedQuote(remaining[1:])
			if end < 0 {
				parts = append(parts, remaining[1:])
				break
			}
			seg := remaining[1 : end+1]
			seg = strings.ReplaceAll(seg, "\\\"", "\"")
			seg = strings.ReplaceAll(seg, "\\n", "\n")
			parts = append(parts, seg)
			remaining = remaining[end+2:]
		} else {
			return s // unknown format
		}
	}
	result := strings.Join(parts, "")
	result = strings.ReplaceAll(result, "\\n", "\n")
	return result
}

func findUnescapedQuote(s string) int {
	for i := 0; i < len(s); i++ {
		if s[i] == '\\' {
			i++
			continue
		}
		if s[i] == '"' {
			return i
		}
	}
	return -1
}

// rewriteRelativeLinks walks the Tiptap node tree and rewrites relative hrefs
// (starting with /) to absolute URLs using the given siteURL base.
func rewriteRelativeLinks(nodes []TiptapNode, siteURL string) {
	// Strip trailing slash from siteURL
	for len(siteURL) > 0 && siteURL[len(siteURL)-1] == '/' {
		siteURL = siteURL[:len(siteURL)-1]
	}
	for i := range nodes {
		// Check marks for link hrefs
		for j := range nodes[i].Marks {
			if nodes[i].Marks[j].Type == "link" {
				if href, ok := nodes[i].Marks[j].Attrs["href"].(string); ok {
					if len(href) > 0 && href[0] == '/' {
						nodes[i].Marks[j].Attrs["href"] = siteURL + href
					}
				}
			}
		}
		// Check image src — rewrite site-relative paths but not absolute filesystem paths
		if nodes[i].Type == "image" || nodes[i].Type == "image2" {
			if src, ok := nodes[i].Attrs["src"].(string); ok {
				if len(src) > 0 && src[0] == '/' && !isAbsoluteFilePath(src) {
					nodes[i].Attrs["src"] = siteURL + src
				}
			}
		}
		// Recurse into children
		if len(nodes[i].Content) > 0 {
			rewriteRelativeLinks(nodes[i].Content, siteURL)
		}
	}
}
