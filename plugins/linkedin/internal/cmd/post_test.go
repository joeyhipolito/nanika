package cmd

import "testing"

func TestStripJSX(t *testing.T) {
	tests := []struct {
		name string
		in   string
		want string
	}{
		{
			name: "no JSX",
			in:   "Hello world\n\nSecond paragraph",
			want: "Hello world\n\nSecond paragraph",
		},
		{
			name: "self-closing component",
			in:   "Before\n<Image src=\"test.png\" />\nAfter",
			want: "Before\nAfter",
		},
		{
			name: "block component",
			in:   "Before\n<CallToAction>\n  Some content\n</CallToAction>\nAfter",
			want: "Before\nAfter",
		},
		{
			name: "lowercase tags preserved",
			in:   "Hello <em>world</em>",
			want: "Hello <em>world</em>",
		},
		{
			name: "multiple components",
			in:   "<Hero />\n\n# Title\n\nBody text\n\n<Footer>\n  links\n</Footer>",
			want: "\n# Title\n\nBody text\n",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := stripJSX(tt.in)
			if got != tt.want {
				t.Errorf("stripJSX():\n  got:  %q\n  want: %q", got, tt.want)
			}
		})
	}
}

func TestIsJSXSelfClosing(t *testing.T) {
	tests := []struct {
		in   string
		want bool
	}{
		{"<Image src=\"test\" />", true},
		{"<Component />", true},
		{"<div />", false},       // lowercase
		{"regular text", false},   // no angle brackets
		{"<Image>", false},        // not self-closing
	}

	for _, tt := range tests {
		if got := isJSXSelfClosing(tt.in); got != tt.want {
			t.Errorf("isJSXSelfClosing(%q) = %v, want %v", tt.in, got, tt.want)
		}
	}
}

func TestIsJSXBlockOpen(t *testing.T) {
	tests := []struct {
		in      string
		wantTag string
		wantOk  bool
	}{
		{"<CallToAction>", "CallToAction", true},
		{"<Hero className=\"big\">", "Hero", true},
		{"<div>", "", false},
		{"<Image />", "", false},
		{"text", "", false},
	}

	for _, tt := range tests {
		tag, ok := isJSXBlockOpen(tt.in)
		if tag != tt.wantTag || ok != tt.wantOk {
			t.Errorf("isJSXBlockOpen(%q) = (%q, %v), want (%q, %v)", tt.in, tag, ok, tt.wantTag, tt.wantOk)
		}
	}
}
