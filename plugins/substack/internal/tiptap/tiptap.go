package tiptap

import (
	"encoding/json"
	"fmt"
	"strings"
)

// Tiptap/ProseMirror JSON types for Substack's note and draft body fields.

type Doc struct {
	Type    string                 `json:"type"`
	Attrs   map[string]interface{} `json:"attrs,omitempty"`
	Content []Node                 `json:"content"`
}

type Node struct {
	Type    string                 `json:"type"`
	Attrs   map[string]interface{} `json:"attrs,omitempty"`
	Content []Node                 `json:"content,omitempty"`
	Text    string                 `json:"text,omitempty"`
	Marks   []Mark                 `json:"marks,omitempty"`
}

type Mark struct {
	Type  string                 `json:"type"`
	Attrs map[string]interface{} `json:"attrs,omitempty"`
}

// BuildNoteBody converts plain text (with optional inline markdown marks) to a
// Tiptap/ProseMirror JSON document string suitable for Substack's note body field.
func BuildNoteBody(text string) (string, error) {
	paragraphs := splitParagraphs(text)

	var nodes []Node
	for _, para := range paragraphs {
		para = strings.TrimSpace(para)
		if para == "" {
			continue
		}
		inlineNodes := ParseInlineMarks(para)
		node := Node{
			Type:    "paragraph",
			Content: inlineNodes,
		}
		nodes = append(nodes, node)
	}

	doc := Doc{
		Type:    "doc",
		Attrs:   map[string]interface{}{"schemaVersion": "v1"},
		Content: nodes,
	}
	b, err := json.Marshal(doc)
	if err != nil {
		return "", fmt.Errorf("marshaling note body: %w", err)
	}
	return string(b), nil
}

// splitParagraphs splits text on blank lines (one or more consecutive empty lines).
func splitParagraphs(text string) []string {
	lines := strings.Split(text, "\n")
	var paragraphs []string
	var current []string

	for _, line := range lines {
		if strings.TrimSpace(line) == "" {
			if len(current) > 0 {
				paragraphs = append(paragraphs, strings.Join(current, "\n"))
				current = current[:0]
			}
		} else {
			current = append(current, line)
		}
	}
	if len(current) > 0 {
		paragraphs = append(paragraphs, strings.Join(current, "\n"))
	}
	return paragraphs
}

// ParseInlineMarks converts inline markdown (bold, italic, code, links) to Node text nodes.
func ParseInlineMarks(text string) []Node {
	if text == "" {
		return nil
	}
	var nodes []Node
	i := 0

	for i < len(text) {
		// Bold: **text**
		if i+1 < len(text) && text[i] == '*' && text[i+1] == '*' {
			end := findDelimiter(text, i+2, "**")
			if end >= 0 {
				inner := text[i+2 : end]
				for _, child := range parseInlineMarksInner(inner) {
					child.Marks = append([]Mark{{Type: "bold"}}, child.Marks...)
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
					child.Marks = append([]Mark{{Type: "italic"}}, child.Marks...)
					nodes = append(nodes, child)
				}
				i = end + 1
				continue
			}
			nodes = append(nodes, Node{Type: "text", Text: "*"})
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
				nodes = append(nodes, Node{
					Type:  "text",
					Text:  text[i+1 : end],
					Marks: []Mark{{Type: "code"}},
				})
				i = end + 1
				continue
			}
		}

		// Link: [text](url)
		if text[i] == '[' {
			if linkText, href, endPos, ok := parseLink(text, i); ok {
				linkMark := Mark{
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
			nodes = append(nodes, Node{Type: "text", Text: "["})
			i++
			continue
		}

		// Plain text
		start := i
		for i < len(text) && text[i] != '*' && text[i] != '`' && text[i] != '[' {
			i++
		}
		if i > start {
			nodes = append(nodes, Node{Type: "text", Text: text[start:i]})
		}
	}

	return nodes
}

func parseInlineMarksInner(text string) []Node {
	if text == "" {
		return nil
	}
	var nodes []Node
	i := 0

	for i < len(text) {
		if text[i] == '`' {
			end := -1
			for j := i + 1; j < len(text); j++ {
				if text[j] == '`' {
					end = j
					break
				}
			}
			if end >= 0 {
				nodes = append(nodes, Node{
					Type:  "text",
					Text:  text[i+1 : end],
					Marks: []Mark{{Type: "code"}},
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
			nodes = append(nodes, Node{Type: "text", Text: text[start:i]})
		}
	}

	return nodes
}

func findDelimiter(text string, pos int, delim string) int {
	for j := pos; j+len(delim) <= len(text); j++ {
		if text[j:j+len(delim)] == delim {
			return j
		}
	}
	return -1
}

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
