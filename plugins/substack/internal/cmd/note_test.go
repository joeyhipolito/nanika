package cmd

import (
	"encoding/json"
	"reflect"
	"testing"

	"github.com/joeyhipolito/nanika-substack/internal/tiptap"
)

// TestParseInlineMarks verifies that parseInlineMarks produces Tiptap text nodes
// matching the ProseMirror schema for each inline mark variant.
func TestParseInlineMarks(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  []TiptapNode
	}{
		{
			name:  "empty",
			input: "",
			want:  nil,
		},
		{
			name:  "plain text",
			input: "Hello, world!",
			want: []TiptapNode{
				{Type: "text", Text: "Hello, world!"},
			},
		},
		{
			name:  "bold",
			input: "**bold text**",
			want: []TiptapNode{
				{Type: "text", Text: "bold text", Marks: []TiptapMark{{Type: "bold"}}},
			},
		},
		{
			name:  "italic",
			input: "*italic text*",
			want: []TiptapNode{
				{Type: "text", Text: "italic text", Marks: []TiptapMark{{Type: "italic"}}},
			},
		},
		{
			name:  "bold and italic combined",
			input: "**bold** and *italic*",
			want: []TiptapNode{
				{Type: "text", Text: "bold", Marks: []TiptapMark{{Type: "bold"}}},
				{Type: "text", Text: " and "},
				{Type: "text", Text: "italic", Marks: []TiptapMark{{Type: "italic"}}},
			},
		},
		{
			name:  "mixed plain and bold",
			input: "start **middle** end",
			want: []TiptapNode{
				{Type: "text", Text: "start "},
				{Type: "text", Text: "middle", Marks: []TiptapMark{{Type: "bold"}}},
				{Type: "text", Text: " end"},
			},
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := tiptap.ParseInlineMarks(tt.input)
			if !reflect.DeepEqual(got, tt.want) {
				gotJSON, _ := json.MarshalIndent(got, "", "  ")
				wantJSON, _ := json.MarshalIndent(tt.want, "", "  ")
				t.Errorf("tiptap.ParseInlineMarks(%q)\ngot:  %s\nwant: %s", tt.input, gotJSON, wantJSON)
			}
		})
	}
}

// TestBuildNoteBody verifies that buildNoteBody produces a valid ProseMirror doc
// JSON string with paragraph nodes for each input variant.
func TestBuildNoteBody(t *testing.T) {
	tests := []struct {
		name     string
		input    string
		wantDoc  TiptapDoc
		wantJSON string // golden JSON string; empty means skip golden check
	}{
		{
			name:  "single paragraph plain text",
			input: "Hello, world!",
			wantDoc: TiptapDoc{
				Type:  "doc",
				Attrs: map[string]interface{}{"schemaVersion": "v1"},
				Content: []TiptapNode{
					{
						Type: "paragraph",
						Content: []TiptapNode{
							{Type: "text", Text: "Hello, world!"},
						},
					},
				},
			},
			wantJSON: `{"type":"doc","attrs":{"schemaVersion":"v1"},"content":[{"type":"paragraph","content":[{"type":"text","text":"Hello, world!"}]}]}`,
		},
		{
			name:  "multi-paragraph",
			input: "First paragraph.\n\nSecond paragraph.",
			wantDoc: TiptapDoc{
				Type:  "doc",
				Attrs: map[string]interface{}{"schemaVersion": "v1"},
				Content: []TiptapNode{
					{
						Type:    "paragraph",
						Content: []TiptapNode{{Type: "text", Text: "First paragraph."}},
					},
					{
						Type:    "paragraph",
						Content: []TiptapNode{{Type: "text", Text: "Second paragraph."}},
					},
				},
			},
			wantJSON: `{"type":"doc","attrs":{"schemaVersion":"v1"},"content":[{"type":"paragraph","content":[{"type":"text","text":"First paragraph."}]},{"type":"paragraph","content":[{"type":"text","text":"Second paragraph."}]}]}`,
		},
		{
			name:  "inline marks in paragraph",
			input: "Check out **this bold** and *italic* text.",
			wantDoc: TiptapDoc{
				Type:  "doc",
				Attrs: map[string]interface{}{"schemaVersion": "v1"},
				Content: []TiptapNode{
					{
						Type: "paragraph",
						Content: []TiptapNode{
							{Type: "text", Text: "Check out "},
							{Type: "text", Text: "this bold", Marks: []TiptapMark{{Type: "bold"}}},
							{Type: "text", Text: " and "},
							{Type: "text", Text: "italic", Marks: []TiptapMark{{Type: "italic"}}},
							{Type: "text", Text: " text."},
						},
					},
				},
			},
			wantJSON: `{"type":"doc","attrs":{"schemaVersion":"v1"},"content":[{"type":"paragraph","content":[{"type":"text","text":"Check out "},{"type":"text","text":"this bold","marks":[{"type":"bold"}]},{"type":"text","text":" and "},{"type":"text","text":"italic","marks":[{"type":"italic"}]},{"type":"text","text":" text."}]}]}`,
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			gotJSON, err := tiptap.BuildNoteBody(tt.input)
			if err != nil {
				t.Fatalf("tiptap.BuildNoteBody(%q): unexpected error: %v", tt.input, err)
			}

			// Golden JSON check
			if tt.wantJSON != "" && gotJSON != tt.wantJSON {
				t.Errorf("tiptap.BuildNoteBody(%q)\ngot JSON:  %s\nwant JSON: %s", tt.input, gotJSON, tt.wantJSON)
				return
			}

			// Structural check via unmarshal
			var gotDoc TiptapDoc
			if err := json.Unmarshal([]byte(gotJSON), &gotDoc); err != nil {
				t.Fatalf("tiptap.BuildNoteBody(%q): output is not valid JSON: %v\noutput: %s", tt.input, err, gotJSON)
			}

			if gotDoc.Type != "doc" {
				t.Errorf("tiptap.BuildNoteBody(%q): root type = %q, want \"doc\"", tt.input, gotDoc.Type)
			}

			if !reflect.DeepEqual(gotDoc, tt.wantDoc) {
				gotPretty, _ := json.MarshalIndent(gotDoc, "", "  ")
				wantPretty, _ := json.MarshalIndent(tt.wantDoc, "", "  ")
				t.Errorf("tiptap.BuildNoteBody(%q)\ngot:  %s\nwant: %s", tt.input, gotPretty, wantPretty)
			}
		})
	}
}
