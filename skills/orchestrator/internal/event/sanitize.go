package event

import (
	"math"
	"regexp"
	"strings"
)

// stripKeys are data map keys whose values are removed before sending events
// over the network (SSE). The JSONL file on disk retains the full data.
var stripKeys = map[string]bool{
	"dir":   true, // workspace file system paths are internal implementation details
	"error": true, // error strings may contain internal paths or partial secrets
}

// safeKeys are data map keys whose values skip entropy-based redaction.
// They may contain long, high-entropy text that is NOT sensitive (e.g. mission
// descriptions). Regex-based secret detection still applies.
var safeKeys = map[string]bool{
	"task": true, // mission task/description — long markdown, triggers entropy false positive
}

// Pre-compiled regexes for detecting sensitive patterns in string values.
// Compiled once at package init to avoid per-call overhead.
var (
	// reAPIKey matches common API key prefixes followed by their token bodies.
	// Covers: Anthropic (sk-ant-), GitHub PAT (ghp_), GitHub OAuth (gho_),
	// GitLab PAT (glpat-), AWS access key (AKIA), Slack bot (xoxb-), Slack user (xoxp-).
	reAPIKey = regexp.MustCompile(
		`(?:sk-ant-|ghp_|gho_|glpat-|AKIA|xoxb-|xoxp-)[A-Za-z0-9_\-]+`,
	)

	// rePrivateKey matches PEM private key (and certificate) header lines.
	rePrivateKey = regexp.MustCompile(`-----BEGIN[^\r\n]*`)

	// reBearerBasic matches HTTP Authorization header values for Bearer and Basic schemes.
	// The token body must be at least 8 characters to avoid false positives on short words.
	reBearerBasic = regexp.MustCompile(`(?i)(?:Bearer|Basic)\s+[A-Za-z0-9+/=._\-]{8,}`)
)

const (
	// entropyThreshold is the minimum Shannon entropy (bits per character) that
	// triggers redaction for high-entropy strings.
	entropyThreshold = 4.5

	// entropyMinLen is the minimum string length required before entropy is checked.
	// Short strings are excluded to avoid false positives on random short words.
	entropyMinLen = 20
)

// isSensitiveKey returns true for data keys whose values should not travel
// over the network. It matches a fixed deny-list plus common credential
// keyword patterns (case-insensitive).
func isSensitiveKey(k string) bool {
	if stripKeys[k] {
		return true
	}
	lower := strings.ToLower(k)
	for _, pat := range []string{"password", "secret", "token", "api_key", "apikey", "credential", "auth"} {
		if strings.Contains(lower, pat) {
			return true
		}
	}
	return false
}

// shannonEntropy computes the Shannon entropy (bits per character) of s.
// Returns 0 for empty strings.
func shannonEntropy(s string) float64 {
	if len(s) == 0 {
		return 0
	}
	runes := []rune(s)
	n := float64(len(runes))

	freq := make(map[rune]float64, len(runes))
	for _, r := range runes {
		freq[r]++
	}

	var h float64
	for _, count := range freq {
		p := count / n
		h -= p * math.Log2(p)
	}
	return h
}

// isHighEntropy reports whether s looks like a secret based on length and entropy.
// A string must exceed both thresholds to be flagged, reducing false positives.
func isHighEntropy(s string) bool {
	return len(s) > entropyMinLen && shannonEntropy(s) > entropyThreshold
}

// sanitizeString applies all redaction rules to s and returns the sanitized result.
//
// Order of operations:
//  1. Replace API key pattern matches with [REDACTED].
//  2. Replace private key header matches with [REDACTED].
//  3. Replace Bearer/Basic token matches with [REDACTED].
//  4. If the (post-regex) string has no [REDACTED] marker yet and is itself
//     high-entropy, replace the entire value.
//  5. For each whitespace-delimited token not already containing [REDACTED],
//     if that token is high-entropy, replace the entire value.
//
// Steps 4–5 skip tokens that already contain [REDACTED] to avoid false
// positives: the replacement text itself has diverse characters that can
// inflate the measured entropy of otherwise-safe surrounding text.
func sanitizeString(s string) string {
	// Step 1–3: regex-based redactions (substring replacement).
	s = reAPIKey.ReplaceAllString(s, "[REDACTED]")
	s = rePrivateKey.ReplaceAllString(s, "[REDACTED]")
	s = reBearerBasic.ReplaceAllString(s, "[REDACTED]")

	// Step 4: whole-value high-entropy check — only when no regex match fired.
	if !strings.Contains(s, "[REDACTED]") && isHighEntropy(s) {
		return "[REDACTED]"
	}

	// Step 5: token-level high-entropy check.
	// Skip tokens that already contain [REDACTED] so that the replacement text
	// itself doesn't trigger a spurious full-value redaction.
	for _, tok := range strings.Fields(s) {
		if !strings.Contains(tok, "[REDACTED]") && isHighEntropy(tok) {
			return "[REDACTED]"
		}
	}

	return s
}

// sanitizeValue dispatches to the appropriate sanitizer based on the concrete type.
func sanitizeValue(v any) any {
	switch val := v.(type) {
	case string:
		return sanitizeString(val)
	case map[string]any:
		return SanitizeData(val)
	case []any:
		result := make([]any, len(val))
		for i, elem := range val {
			result[i] = sanitizeValue(elem)
		}
		return result
	default:
		return v
	}
}

// SanitizeData returns a shallow-copied map with sensitive string values redacted.
// Non-string values are preserved as-is. Nested maps and slices are recursively sanitized.
// Returns nil if data is nil.
func SanitizeData(data map[string]any) map[string]any {
	if data == nil {
		return nil
	}
	out := make(map[string]any, len(data))
	for k, v := range data {
		out[k] = sanitizeValue(v)
	}
	return out
}

// sanitizeStringSafe applies only regex-based redaction (API keys, PEM headers,
// Bearer/Basic tokens) but skips entropy-based checks. Used for safe keys whose
// values are expected to be long/complex but non-sensitive.
func sanitizeStringSafe(s string) string {
	s = reAPIKey.ReplaceAllString(s, "[REDACTED]")
	s = rePrivateKey.ReplaceAllString(s, "[REDACTED]")
	s = reBearerBasic.ReplaceAllString(s, "[REDACTED]")
	return s
}

// sanitizeValueSafe dispatches to the safe sanitizer for known-safe keys.
func sanitizeValueSafe(v any) any {
	switch val := v.(type) {
	case string:
		return sanitizeStringSafe(val)
	default:
		return v
	}
}

// Sanitize returns a copy of ev with sensitive data fields stripped.
//
// Specifically:
//   - Data keys matching the strip list (dir, error) are removed.
//   - Data keys containing credential keywords (password, token, secret, …) are removed.
//   - Data keys in the safe list skip entropy-based redaction (regex still applies).
//   - Remaining string values are scanned for secrets (API keys, PEM headers,
//     Bearer/Basic tokens, high-entropy strings) and redacted.
//   - All other fields — including event type, mission/phase/worker IDs,
//     timestamps, sequences, and non-sensitive data — pass through unchanged.
//
// The original event is not modified.
func Sanitize(ev Event) Event {
	if len(ev.Data) == 0 {
		return ev
	}

	// First pass: filter by key name.
	filtered := make(map[string]any, len(ev.Data))
	for k, v := range ev.Data {
		if !isSensitiveKey(k) {
			filtered[k] = v
		}
	}

	// Second pass: scan remaining string values for secret patterns.
	for k, v := range filtered {
		if safeKeys[k] {
			filtered[k] = sanitizeValueSafe(v)
		} else {
			filtered[k] = sanitizeValue(v)
		}
	}

	if len(filtered) == 0 {
		filtered = nil
	}

	ev.Data = filtered
	return ev
}
