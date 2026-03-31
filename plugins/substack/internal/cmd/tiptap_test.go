package cmd

import (
	"encoding/json"
	"fmt"
	"strings"
	"testing"
)

func TestFigureSelfClosing(t *testing.T) {
	md := `<Figure src="/blog/orchestration/hero.png" alt="Hero image" />`
	result, err := markdownToTiptap(md, "", nil)
	if err != nil {
		t.Fatalf("markdownToTiptap: %v", err)
	}

	var doc TiptapDoc
	if err := json.Unmarshal([]byte(result), &doc); err != nil {
		t.Fatalf("invalid JSON: %v", err)
	}
	if len(doc.Content) != 1 {
		t.Fatalf("expected 1 node, got %d", len(doc.Content))
	}
	node := doc.Content[0]
	if node.Type != "image2" {
		t.Fatalf("expected image2 node, got %s", node.Type)
	}
	if src, _ := node.Attrs["src"].(string); src != "/blog/orchestration/hero.png" {
		t.Fatalf("expected src /blog/orchestration/hero.png, got %s", src)
	}
	if alt, _ := node.Attrs["alt"].(string); alt != "Hero image" {
		t.Fatalf("expected alt 'Hero image', got %s", alt)
	}
}

func TestFigureWrapping(t *testing.T) {
	md := "<Figure src=\"/blog/orchestration/code-001.png\" alt=\"Phase struct\">\n\n```go\ntype Phase struct {\n\tName string\n}\n```\n\n</Figure>"
	result, err := markdownToTiptap(md, "", nil)
	if err != nil {
		t.Fatalf("markdownToTiptap: %v", err)
	}

	var doc TiptapDoc
	if err := json.Unmarshal([]byte(result), &doc); err != nil {
		t.Fatalf("invalid JSON: %v", err)
	}
	if len(doc.Content) != 1 {
		t.Fatalf("expected 1 node (image only, children skipped), got %d", len(doc.Content))
	}
	node := doc.Content[0]
	if node.Type != "image2" {
		t.Fatalf("expected image2 node, got %s", node.Type)
	}
	if src, _ := node.Attrs["src"].(string); src != "/blog/orchestration/code-001.png" {
		t.Fatalf("expected src /blog/orchestration/code-001.png, got %s", src)
	}
}

func TestFigureRewriteRelativeLinks(t *testing.T) {
	md := `<Figure src="/blog/orchestration/hero.png" alt="Hero" />`
	result, err := markdownToTiptap(md, "https://example.com", nil)
	if err != nil {
		t.Fatalf("markdownToTiptap: %v", err)
	}

	if !strings.Contains(result, "https://example.com/blog/orchestration/hero.png") {
		t.Fatalf("expected rewritten URL, got %s", result)
	}
}

func TestExtractLocalImagePath(t *testing.T) {
	tests := []struct {
		src  string
		want string
	}{
		{"/blog/orchestration/hero.png", "blog/orchestration/hero.png"},
		{"https://example.com/blog/orchestration/hero.png", "blog/orchestration/hero.png"},
		{"https://cdn.substack.com/image.png", ""},
		{"/assets/logo.png", ""},
	}
	for _, tt := range tests {
		got := extractLocalImagePath(tt.src)
		if got != tt.want {
			t.Errorf("extractLocalImagePath(%q) = %q, want %q", tt.src, got, tt.want)
		}
	}
}

func TestMarkdownToTiptap(t *testing.T) {
	md := `## The Moment I Checked My Rate Limits

Three research agents into a multi-phase mission, Claude Code hit a rate limit.

The real economics are about **capacity allocation**:

- **Claude Code**: Fixed subscription.
- **Gemini CLI**: Free tier API.

| Task Type | Routes To |
|-----------|-----------|
| Research | Gemini |
| Implementation | Claude |

` + "```go\nfmt.Println(\"hello\")\n```" + `

> This is a blockquote.

1. First item
2. Second item

![alt text](https://example.com/image.png)
`
	result, err := markdownToTiptap(md, "", nil)
	if err != nil {
		t.Fatalf("markdownToTiptap: %v", err)
	}
	fmt.Println(result)
	if result == "" || result == `{"type":"doc","content":null}` {
		t.Fatal("empty tiptap output")
	}
}
