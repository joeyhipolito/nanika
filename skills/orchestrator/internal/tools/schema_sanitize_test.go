package tools

import (
	"context"
	"encoding/json"
	"regexp"
	"testing"
)

var anthropicPropKeyRE = regexp.MustCompile(`^[a-zA-Z0-9_.\-]{1,64}$`)

// TestRegistrySchemaPropertyKeysAnthropicValid scans every loaded tool's
// input_schema.properties and asserts each key matches Anthropic's
// regex ^[a-zA-Z0-9_.-]{1,64}$ (the validator that returns HTTP 400).
func TestRegistrySchemaPropertyKeysAnthropicValid(t *testing.T) {
	r := Load(context.Background())
	for _, tool := range r.All() {
		var doc struct {
			Properties map[string]json.RawMessage `json:"properties"`
		}
		if err := json.Unmarshal(tool.InputSchema(), &doc); err != nil {
			t.Errorf("tool %q: schema unmarshal: %v", tool.Name(), err)
			continue
		}
		for k := range doc.Properties {
			if !anthropicPropKeyRE.MatchString(k) {
				t.Errorf("tool %q: property key %q violates Anthropic regex", tool.Name(), k)
			}
		}
	}
}

func TestSanitizePropKey(t *testing.T) {
	cases := []struct{ in, want string }{
		{"foo", "foo"},
		{"foo-bar", "foo-bar"},
		{"foo:bar", "foo_bar"},
		{"foo/bar", "foo_bar"},
		{"foo>bar", "foo_bar"},
		{"a.b_c-d", "a.b_c-d"},
		{"", ""},
		{"folder/", "folder_"},
	}
	for _, c := range cases {
		got := sanitizePropKey(c.in)
		if got != c.want {
			t.Errorf("sanitizePropKey(%q) = %q, want %q", c.in, got, c.want)
		}
	}
	// Truncation to 64.
	long := make([]byte, 100)
	for i := range long {
		long[i] = 'a'
	}
	if got := sanitizePropKey(string(long)); len(got) != 64 {
		t.Errorf("expected 64-char truncation, got %d", len(got))
	}
}

// TestParseArgStringsSanitizes covers the plugin.json fallback path:
// args like "[folder/]", "<thread-id>", "--no-comment", "--mode <a|b>" must
// produce schema-safe property keys.
func TestParseArgStringsSanitizes(t *testing.T) {
	props := parseArgStrings([]string{
		"[folder/]",
		"<thread-id>",
		"--no-comment",
		"--mode <a|b>",
		"--weird:flag <v>",
	})
	for k := range props {
		if !anthropicPropKeyRE.MatchString(k) {
			t.Errorf("parseArgStrings produced invalid key %q", k)
		}
	}
}
