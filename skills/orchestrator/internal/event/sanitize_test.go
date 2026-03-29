package event

import (
	"math"
	"strings"
	"testing"
)

// ── Existing Sanitize(Event) tests ────────────────────────────────────────────

func TestSanitizePassthrough(t *testing.T) {
	ev := New(PhaseStarted, "m1", "p1", "", map[string]any{
		"name":    "backend-engineer",
		"persona": "senior-dev",
		"model":   "claude-opus-4-6",
	})
	got := Sanitize(ev)

	if got.MissionID != ev.MissionID {
		t.Fatalf("MissionID changed: %q → %q", ev.MissionID, got.MissionID)
	}
	if got.Type != ev.Type {
		t.Fatalf("Type changed")
	}
	if len(got.Data) != 3 {
		t.Fatalf("expected 3 data keys, got %d", len(got.Data))
	}
	for _, k := range []string{"name", "persona", "model"} {
		if _, ok := got.Data[k]; !ok {
			t.Fatalf("expected data key %q to be present", k)
		}
	}
}

func TestSanitizeStripsErrorField(t *testing.T) {
	ev := New(WorkerFailed, "m1", "p1", "w1", map[string]any{
		"error":      "open /home/user/.via/workspace/secret.txt: permission denied",
		"duration":   "1.23s",
		"output_len": 42,
	})
	got := Sanitize(ev)

	if _, ok := got.Data["error"]; ok {
		t.Fatal("expected 'error' key to be stripped")
	}
	if got.Data["duration"] == nil {
		t.Fatal("expected 'duration' key to remain")
	}
	if got.Data["output_len"] == nil {
		t.Fatal("expected 'output_len' key to remain")
	}
}

func TestSanitizeStripsDirField(t *testing.T) {
	ev := New(WorkerSpawned, "m1", "p1", "w1", map[string]any{
		"model":   "claude-opus-4-6",
		"persona": "backend",
		"dir":     "/Users/user/.via/workspaces/abc/workers/backend",
	})
	got := Sanitize(ev)

	if _, ok := got.Data["dir"]; ok {
		t.Fatal("expected 'dir' key to be stripped")
	}
	if got.Data["model"] == nil {
		t.Fatal("expected 'model' key to remain")
	}
}

func TestSanitizeStripsCredentialKeys(t *testing.T) {
	cases := []string{
		"password", "PASSWORD", "my_password",
		"api_key", "apikey", "API_KEY",
		"secret", "secret_value",
		"token", "auth_token",
		"credential", "aws_credential",
		"auth", "auth_header",
	}
	for _, key := range cases {
		ev := New(SystemError, "m1", "", "", map[string]any{
			key:    "sensitive-value",
			"safe": "safe-value",
		})
		got := Sanitize(ev)
		if _, ok := got.Data[key]; ok {
			t.Errorf("expected key %q to be stripped, but it was present", key)
		}
		if got.Data["safe"] == nil {
			t.Errorf("expected 'safe' key to remain when stripping %q", key)
		}
	}
}

func TestSanitizeNilDataPassthrough(t *testing.T) {
	ev := New(MissionCompleted, "m1", "", "", nil)
	got := Sanitize(ev)
	if got.Data != nil {
		t.Fatal("expected nil Data to pass through unchanged")
	}
}

func TestSanitizeAllSensitiveYieldsNilData(t *testing.T) {
	ev := New(SystemError, "m1", "", "", map[string]any{
		"error": "oops",
		"dir":   "/home/user/.via",
	})
	got := Sanitize(ev)
	// All keys stripped → Data should be nil, not an empty map.
	if got.Data != nil {
		t.Fatalf("expected nil Data when all keys stripped, got %v", got.Data)
	}
}

func TestSanitizeDoesNotMutateOriginal(t *testing.T) {
	data := map[string]any{
		"error": "original error",
		"count": 5,
	}
	ev := New(MissionFailed, "m1", "", "", data)
	_ = Sanitize(ev)

	// Original data map must still have "error".
	if _, ok := data["error"]; !ok {
		t.Fatal("Sanitize must not modify the original data map")
	}
}

// TestSanitizeRedactsAPIKeyInValue verifies that Sanitize also scans string
// values (not just key names) for embedded API key patterns.
func TestSanitizeRedactsAPIKeyInValue(t *testing.T) {
	ev := New(PhaseStarted, "m1", "p1", "", map[string]any{
		"info": "key is sk-ant-api03-abcdef1234567890XYZ",
	})
	got := Sanitize(ev)
	if v, ok := got.Data["info"].(string); !ok || strings.Contains(v, "sk-ant-") {
		t.Errorf("expected API key in value to be redacted, got: %v", got.Data["info"])
	}
}

// ── Shannon entropy ───────────────────────────────────────────────────────────

func TestShannonEntropy(t *testing.T) {
	tests := []struct {
		name    string
		input   string
		wantMin float64 // entropy must be >= wantMin
		wantMax float64 // entropy must be <= wantMax
	}{
		{
			name:    "empty string",
			input:   "",
			wantMin: 0,
			wantMax: 0,
		},
		{
			name:    "single character repeated",
			input:   "aaaaaaaaaa",
			wantMin: 0,
			wantMax: 0,
		},
		{
			name:    "two characters alternating",
			input:   "ababababab",
			wantMin: 0.99,
			wantMax: 1.01,
		},
		{
			name:    "lowercase ascii all distinct",
			input:   "abcdefghijklmnopqrstuvwxyz",
			wantMin: 4.7,
			wantMax: 4.71,
		},
		{
			name:    "random-looking base64 secret",
			input:   "aB3dEf7gHiJkLmNoPqRsTuVwXyZ012345",
			wantMin: 4.5,
			wantMax: 6.0,
		},
		{
			name:    "normal english sentence (low entropy)",
			input:   "the quick brown fox jumps",
			wantMin: 3.0,
			wantMax: 4.5,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			got := shannonEntropy(tc.input)
			if got < tc.wantMin || got > tc.wantMax {
				t.Errorf("shannonEntropy(%q) = %.4f, want [%.4f, %.4f]",
					tc.input, got, tc.wantMin, tc.wantMax)
			}
		})
	}
}

func TestShannonEntropyIsNonNegative(t *testing.T) {
	inputs := []string{"", "a", "ab", "abc", "hello world", strings.Repeat("x", 100)}
	for _, s := range inputs {
		if h := shannonEntropy(s); h < 0 {
			t.Errorf("shannonEntropy(%q) returned negative value %f", s, h)
		}
	}
}

// ── isHighEntropy ─────────────────────────────────────────────────────────────

func TestIsHighEntropy(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  bool
	}{
		{
			name:  "short string below length threshold",
			input: "aB3dEf7gHiJkLmN", // 15 chars, high entropy but too short
			want:  false,
		},
		{
			name:  "long low-entropy string",
			input: "aaaaaaaaaaaaaaaaaaaaaaaaa", // 25 chars, entropy ~0
			want:  false,
		},
		{
			name:  "high entropy and long enough",
			input: "aB3dEf7gHiJkLmNoPqRsTuVwXyZ01", // 30 chars
			want:  true,
		},
		{
			name:  "exactly at length boundary (not over)",
			input: "aB3dEf7gHiJkLmNoPqRs", // 20 chars — len > 20 is false
			want:  false,
		},
		{
			name: "one over length boundary but entropy still below threshold",
			// 21 unique chars → entropy = log2(21) ≈ 4.39 < 4.5; length passes but entropy does not.
			input: "aB3dEf7gHiJkLmNoPqRsT",
			want:  false,
		},
		{
			name: "length and entropy both above threshold",
			// 23 unique chars → entropy = log2(23) ≈ 4.52 > 4.5; both conditions met.
			input: "abcdefghijklmnopqrstuvw", // 23 chars, all unique lowercase
			want:  true,
		},
		{
			name:  "normal english text",
			input: "the quick brown fox jumps over",
			want:  false,
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := isHighEntropy(tc.input); got != tc.want {
				t.Errorf("isHighEntropy(%q) = %v, want %v (entropy=%.4f, len=%d)",
					tc.input, got, tc.want, shannonEntropy(tc.input), len(tc.input))
			}
		})
	}
}

// ── API key patterns ──────────────────────────────────────────────────────────

func TestSanitizeString_APIKeyPrefixes(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		// Anthropic
		{
			name:  "Anthropic API key standalone",
			input: "sk-ant-api03-abcdef1234567890XYZ",
			want:  "[REDACTED]",
		},
		{
			name:  "Anthropic API key in sentence",
			input: "API key is sk-ant-api03-abcdef1234567890XYZ for auth",
			want:  "API key is [REDACTED] for auth",
		},
		// GitHub PAT
		{
			name:  "GitHub PAT standalone",
			input: "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz123456",
			want:  "[REDACTED]",
		},
		{
			name:  "GitHub PAT in JSON-like string",
			input: `{"token":"ghp_AbCdEfGhIjKlMnOpQrStUvWxYz123456"}`,
			want:  `{"token":"[REDACTED]"}`,
		},
		// GitHub OAuth
		{
			name:  "GitHub OAuth token standalone",
			input: "gho_AbCdEfGhIjKlMnOpQrStUvWxYz123456",
			want:  "[REDACTED]",
		},
		// GitLab PAT
		{
			name:  "GitLab PAT standalone",
			input: "glpat-AbCdEfGhIjKlMnOpQrStUvWxYz",
			want:  "[REDACTED]",
		},
		{
			name:  "GitLab PAT embedded in URL",
			input: "https://oauth2:glpat-AbCdEfGhIjKlMnOpQrSt@gitlab.com/repo.git",
			// The credential is redacted; surrounding URL text is preserved.
			// Entropy check is skipped because [REDACTED] is already present.
			want: "https://oauth2:[REDACTED]@gitlab.com/repo.git",
		},
		// AWS
		{
			name:  "AWS access key standalone",
			input: "AKIAIOSFODNN7EXAMPLE",
			want:  "[REDACTED]",
		},
		{
			name:  "AWS access key in config line",
			input: "aws_access_key_id = AKIAIOSFODNN7EXAMPLE",
			want:  "aws_access_key_id = [REDACTED]",
		},
		// Slack bot
		{
			name:  "Slack bot token standalone",
			input: "xoxb-123456789012-abcdefghijklmnopqrstuvwx",
			want:  "[REDACTED]",
		},
		{
			name:  "Slack bot token in log line",
			input: "connecting with token xoxb-123456789012-abcdefghijklmnopqrstuvwx ok",
			want:  "connecting with token [REDACTED] ok",
		},
		// Slack user
		{
			name:  "Slack user token standalone",
			input: "xoxp-123456789012-abcdefghijklmnopqrstuvwx",
			want:  "[REDACTED]",
		},
		// No match: prefix only, no body
		{
			name:  "bare prefix without body is not matched",
			input: "prefix sk-ant- alone",
			want:  "prefix sk-ant- alone",
		},
		// Multiple keys in one string
		{
			name:  "multiple API keys in one value",
			input: "key1=ghp_AbCdEfGhIjKlMnOpQrSt key2=xoxb-123456789-abcdefghijk",
			want:  "key1=[REDACTED] key2=[REDACTED]",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := sanitizeString(tc.input); got != tc.want {
				t.Errorf("sanitizeString(%q)\n  got  %q\n  want %q", tc.input, got, tc.want)
			}
		})
	}
}

// ── Private key headers ───────────────────────────────────────────────────────

func TestSanitizeString_PrivateKeyHeaders(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "RSA private key header",
			input: "-----BEGIN RSA PRIVATE KEY-----",
			want:  "[REDACTED]",
		},
		{
			name:  "EC private key header",
			input: "-----BEGIN EC PRIVATE KEY-----",
			want:  "[REDACTED]",
		},
		{
			name:  "generic private key header",
			input: "-----BEGIN PRIVATE KEY-----",
			want:  "[REDACTED]",
		},
		{
			name:  "OpenSSH private key header",
			input: "-----BEGIN OPENSSH PRIVATE KEY-----",
			want:  "[REDACTED]",
		},
		{
			name:  "certificate header (not a key but still sensitive)",
			input: "-----BEGIN CERTIFICATE-----",
			want:  "[REDACTED]",
		},
		{
			name:  "key header embedded in a larger string",
			input: "pem content: -----BEGIN RSA PRIVATE KEY----- (truncated)",
			// [^\r\n]* is greedy: it consumes the entire line including " (truncated)".
			want: "pem content: [REDACTED]",
		},
		{
			name:  "multiline PEM: header line redacted, body left as-is",
			input: "-----BEGIN RSA PRIVATE KEY-----\nMIIEowIBAAKCAQEA...",
			want:  "[REDACTED]\nMIIEowIBAAKCAQEA...",
		},
		{
			name:  "END header is not matched (only BEGIN triggers redaction)",
			input: "-----END RSA PRIVATE KEY-----",
			want:  "-----END RSA PRIVATE KEY-----",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := sanitizeString(tc.input); got != tc.want {
				t.Errorf("sanitizeString(%q)\n  got  %q\n  want %q", tc.input, got, tc.want)
			}
		})
	}
}

// ── Bearer / Basic tokens ─────────────────────────────────────────────────────

func TestSanitizeString_BearerBasicTokens(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "Bearer token",
			input: "Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9",
			want:  "[REDACTED]",
		},
		{
			name:  "Bearer token in Authorization header value",
			input: "Authorization: Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9",
			want:  "Authorization: [REDACTED]",
		},
		{
			name:  "Basic token",
			input: "Basic dXNlcm5hbWU6cGFzc3dvcmQ=",
			want:  "[REDACTED]",
		},
		{
			name:  "Basic token in header",
			input: "Authorization: Basic dXNlcm5hbWU6cGFzc3dvcmQ=",
			want:  "Authorization: [REDACTED]",
		},
		{
			name:  "bearer lowercase is also matched",
			input: "bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9",
			want:  "[REDACTED]",
		},
		{
			name:  "BEARER uppercase is also matched",
			input: "BEARER eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9",
			want:  "[REDACTED]",
		},
		{
			name:  "Bearer with short token (< 8 chars) is not matched",
			input: "Bearer tok",
			want:  "Bearer tok",
		},
		{
			name:  "Bearer word without token not matched",
			input: "Bearer",
			want:  "Bearer",
		},
		{
			name:  "multiple bearer tokens in one value",
			input: "old=Bearer oldtoken123456 new=Bearer newtoken789012",
			want:  "old=[REDACTED] new=[REDACTED]",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := sanitizeString(tc.input); got != tc.want {
				t.Errorf("sanitizeString(%q)\n  got  %q\n  want %q", tc.input, got, tc.want)
			}
		})
	}
}

// ── High-entropy strings ──────────────────────────────────────────────────────

func TestSanitizeString_HighEntropy(t *testing.T) {
	// Build a guaranteed high-entropy string: all printable ASCII characters.
	// This gives maximum entropy of log2(94) ≈ 6.55.
	highEntropyToken := "aB3!dEf7gHiJkLmNoPqRsTuVwXyZ0@#$"

	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "standalone high-entropy string",
			input: highEntropyToken,
			want:  "[REDACTED]",
		},
		{
			name:  "high-entropy token embedded in normal text",
			input: "value is " + highEntropyToken + " end",
			want:  "[REDACTED]",
		},
		{
			name:  "low-entropy long string not redacted",
			input: strings.Repeat("abcde", 10), // 50 chars, only 5 unique → low entropy
			want:  strings.Repeat("abcde", 10),
		},
		{
			name:  "short high-entropy string not redacted (below length threshold)",
			input: "aB3!dEf7gH", // 10 chars
			want:  "aB3!dEf7gH",
		},
		{
			name:  "normal english sentence not redacted",
			input: "the quick brown fox jumps over the lazy dog",
			want:  "the quick brown fox jumps over the lazy dog",
		},
		{
			name: "UUID-like string not redacted",
			// UUID uses only hex digits [0-9a-f] plus '-'. With only 17 unique chars
			// in 36 total, Shannon entropy ≈ 3.39 < 4.5 — below the threshold.
			input: "550e8400-e29b-41d4-a716-446655440000",
			want:  "550e8400-e29b-41d4-a716-446655440000",
		},
		{
			name:  "base64-encoded secret (high entropy)",
			input: "dGhpcyBpcyBhIHNlY3JldCB0b2tlbiBmb3IgdGVzdGluZw==",
			want:  "[REDACTED]",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := sanitizeString(tc.input); got != tc.want {
				t.Errorf("sanitizeString(%q)\n  got  %q\n  want %q\n  entropy=%.4f",
					tc.input, got, tc.want, shannonEntropy(tc.input))
			}
		})
	}
}

// ── Safe / non-sensitive strings ──────────────────────────────────────────────

func TestSanitizeString_SafeValues(t *testing.T) {
	tests := []struct {
		name  string
		input string
	}{
		{name: "empty string", input: ""},
		{name: "plain word", input: "hello"},
		{name: "normal log message", input: "server started on :8080"},
		{name: "URL without credentials", input: "https://api.example.com/v1/users"},
		{name: "integer as string", input: "42"},
		{name: "boolean-like", input: "true"},
		{name: "JSON with safe fields", input: `{"name":"alice","role":"admin"}`},
		{name: "path", input: "/var/log/app.log"},
		{name: "AKIA prefix too short (no suffix)", input: "AKIA"},
		{name: "sk-ant- alone no suffix", input: "sk-ant-"},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := sanitizeString(tc.input); got != tc.input {
				t.Errorf("sanitizeString(%q) = %q, want original value unchanged", tc.input, got)
			}
		})
	}
}

// ── SanitizeData ──────────────────────────────────────────────────────────────

func TestSanitizeData_NilAndEmpty(t *testing.T) {
	t.Run("nil map returns nil", func(t *testing.T) {
		if got := SanitizeData(nil); got != nil {
			t.Errorf("SanitizeData(nil) = %v, want nil", got)
		}
	})

	t.Run("empty map returns empty map", func(t *testing.T) {
		got := SanitizeData(map[string]any{})
		if got == nil {
			t.Fatal("SanitizeData({}) returned nil, want non-nil empty map")
		}
		if len(got) != 0 {
			t.Errorf("SanitizeData({}) has %d entries, want 0", len(got))
		}
	})
}

func TestSanitizeData_StringValues(t *testing.T) {
	input := map[string]any{
		"api_key":    "sk-ant-api03-abcdef1234567890XYZ",
		"github_pat": "ghp_AbCdEfGhIjKlMnOpQrStUvWxYz123456",
		"auth":       "Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9",
		"safe_field": "hello world",
		"count":      42, // non-string: should pass through unchanged
	}

	got := SanitizeData(input)

	checks := map[string]string{
		"api_key":    "[REDACTED]",
		"github_pat": "[REDACTED]",
		"auth":       "[REDACTED]",
		"safe_field": "hello world",
	}
	for key, want := range checks {
		if got[key] != want {
			t.Errorf("key %q: got %q, want %q", key, got[key], want)
		}
	}
	if got["count"] != 42 {
		t.Errorf("non-string value modified: got %v, want 42", got["count"])
	}
}

func TestSanitizeData_NonStringPassThrough(t *testing.T) {
	input := map[string]any{
		"int_val":   int(99),
		"float_val": float64(3.14),
		"bool_val":  true,
		"nil_val":   nil,
	}
	got := SanitizeData(input)

	if got["int_val"] != 99 {
		t.Errorf("int_val: got %v, want 99", got["int_val"])
	}
	if got["float_val"] != 3.14 {
		t.Errorf("float_val: got %v, want 3.14", got["float_val"])
	}
	if got["bool_val"] != true {
		t.Errorf("bool_val: got %v, want true", got["bool_val"])
	}
	if got["nil_val"] != nil {
		t.Errorf("nil_val: got %v, want nil", got["nil_val"])
	}
}

func TestSanitizeData_NestedMap(t *testing.T) {
	input := map[string]any{
		"outer_safe": "harmless",
		"nested": map[string]any{
			"token":      "xoxb-123456789012-abcdefghijklmnopqrstuvwx",
			"inner_safe": "ok",
		},
	}
	got := SanitizeData(input)

	if got["outer_safe"] != "harmless" {
		t.Errorf("outer_safe: got %v, want %q", got["outer_safe"], "harmless")
	}
	nested, ok := got["nested"].(map[string]any)
	if !ok {
		t.Fatalf("nested: expected map[string]any, got %T", got["nested"])
	}
	if nested["token"] != "[REDACTED]" {
		t.Errorf("nested.token: got %v, want [REDACTED]", nested["token"])
	}
	if nested["inner_safe"] != "ok" {
		t.Errorf("nested.inner_safe: got %v, want %q", nested["inner_safe"], "ok")
	}
}

func TestSanitizeData_SliceValues(t *testing.T) {
	input := map[string]any{
		"tokens": []any{
			"ghp_AbCdEfGhIjKlMnOpQrStUvWxYz123456",
			"safe value",
			42,
		},
	}
	got := SanitizeData(input)

	tokens, ok := got["tokens"].([]any)
	if !ok {
		t.Fatalf("tokens: expected []any, got %T", got["tokens"])
	}
	if len(tokens) != 3 {
		t.Fatalf("tokens: expected 3 elements, got %d", len(tokens))
	}
	if tokens[0] != "[REDACTED]" {
		t.Errorf("tokens[0]: got %v, want [REDACTED]", tokens[0])
	}
	if tokens[1] != "safe value" {
		t.Errorf("tokens[1]: got %v, want %q", tokens[1], "safe value")
	}
	if tokens[2] != 42 {
		t.Errorf("tokens[2]: got %v, want 42", tokens[2])
	}
}

func TestSanitizeData_DoesNotMutateInput(t *testing.T) {
	original := "sk-ant-api03-abcdef1234567890XYZ"
	input := map[string]any{
		"key": original,
	}
	_ = SanitizeData(input)

	if input["key"] != original {
		t.Errorf("SanitizeData mutated the input map: got %v, want %q", input["key"], original)
	}
}

func TestSanitizeData_DeepNesting(t *testing.T) {
	input := map[string]any{
		"level1": map[string]any{
			"level2": map[string]any{
				"secret": "AKIAIOSFODNN7EXAMPLE",
				"safe":   "plain text",
			},
		},
	}
	got := SanitizeData(input)

	l1, _ := got["level1"].(map[string]any)
	l2, _ := l1["level2"].(map[string]any)

	if l2["secret"] != "[REDACTED]" {
		t.Errorf("deep secret not redacted: %v", l2["secret"])
	}
	if l2["safe"] != "plain text" {
		t.Errorf("deep safe value modified: %v", l2["safe"])
	}
}

// ── Edge cases ────────────────────────────────────────────────────────────────

func TestSanitizeString_EdgeCases(t *testing.T) {
	tests := []struct {
		name  string
		input string
		want  string
	}{
		{
			name:  "empty string unchanged",
			input: "",
			want:  "",
		},
		{
			name:  "whitespace only unchanged",
			input: "   \t\n  ",
			want:  "   \t\n  ",
		},
		{
			name:  "string with only [REDACTED] unchanged",
			input: "[REDACTED]",
			want:  "[REDACTED]",
		},
		{
			name:  "API key immediately adjacent to punctuation",
			input: "(sk-ant-api03-abcdef1234567890XYZ)",
			want:  "([REDACTED])",
		},
		{
			name:  "multiple patterns in same string",
			input: "key=sk-ant-abc123def456 auth=Bearer tokenABCDEFGH pem=-----BEGIN PRIVATE KEY-----",
			want:  "key=[REDACTED] auth=[REDACTED] pem=[REDACTED]",
		},
		{
			name:  "AWS key adjacent to equals sign",
			input: "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE",
			want:  "AWS_ACCESS_KEY_ID=[REDACTED]",
		},
	}

	for _, tc := range tests {
		t.Run(tc.name, func(t *testing.T) {
			if got := sanitizeString(tc.input); got != tc.want {
				t.Errorf("sanitizeString(%q)\n  got  %q\n  want %q", tc.input, got, tc.want)
			}
		})
	}
}

// ── Regex pre-compilation (smoke test) ───────────────────────────────────────

func TestRegexesArePrecompiled(t *testing.T) {
	// Ensure package-level regexes are non-nil (MustCompile panics on bad patterns,
	// so a successful test run guarantees they compiled successfully).
	if reAPIKey == nil {
		t.Error("reAPIKey is nil")
	}
	if rePrivateKey == nil {
		t.Error("rePrivateKey is nil")
	}
	if reBearerBasic == nil {
		t.Error("reBearerBasic is nil")
	}
}

// ── Benchmark ─────────────────────────────────────────────────────────────────

func BenchmarkSanitizeData_Mixed(b *testing.B) {
	data := map[string]any{
		"api_key":   "sk-ant-api03-abcdef1234567890XYZ",
		"token":     "Bearer eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9",
		"safe":      "this is a normal log message",
		"count":     42,
		"user_name": "alice",
	}
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		SanitizeData(data)
	}
}

func BenchmarkShannonEntropy(b *testing.B) {
	s := "aB3dEf7gHiJkLmNoPqRsTuVwXyZ012345678"
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		shannonEntropy(s)
	}
}

// ── Concurrent safety ─────────────────────────────────────────────────────────

func TestSanitizeData_ConcurrentSafety(t *testing.T) {
	// Run with -race to detect data races on the pre-compiled regexes or
	// shared state. Regexes are safe for concurrent use by the regexp package.
	data := map[string]any{
		"key":  "sk-ant-api03-abcdef1234567890XYZ",
		"safe": "harmless text",
	}

	const goroutines = 200
	done := make(chan struct{}, goroutines)

	for i := 0; i < goroutines; i++ {
		go func() {
			defer func() { done <- struct{}{} }()
			got := SanitizeData(data)
			if got["key"] != "[REDACTED]" {
				t.Errorf("concurrent: key not redacted, got %v", got["key"])
			}
			if got["safe"] != "harmless text" {
				t.Errorf("concurrent: safe modified, got %v", got["safe"])
			}
		}()
	}
	for i := 0; i < goroutines; i++ {
		<-done
	}
}

// ── Entropy boundary precision ────────────────────────────────────────────────

func TestEntropyThresholdBoundary(t *testing.T) {
	// Construct a string whose entropy is just at the threshold to verify
	// the boundary condition is handled as expected (strictly greater than).
	//
	// A string of 21 repeated characters has entropy = 0, well below threshold.
	// A string using only 2 symbols: entropy = 1.0, below threshold.
	// We verify the threshold is a strict > comparison.

	// 21 'a's: entropy 0, length 21 — not high entropy.
	low := strings.Repeat("a", 21)
	if isHighEntropy(low) {
		t.Errorf("isHighEntropy(%q) = true, want false (entropy=0)", low)
	}

	// Verify that our threshold constant matches the expected value.
	if math.Abs(entropyThreshold-4.5) > 1e-9 {
		t.Errorf("entropyThreshold = %f, want 4.5", entropyThreshold)
	}

	// Verify minimum length constant.
	if entropyMinLen != 20 {
		t.Errorf("entropyMinLen = %d, want 20", entropyMinLen)
	}
}
