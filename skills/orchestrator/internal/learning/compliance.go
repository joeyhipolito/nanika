package learning

import (
	"regexp"
	"strings"
)

// Loose inner charset catches timestamp-suffixed, hex-suffixed, and legacy
// hash IDs. Length bound avoids matching non-id bracketed content.
var citedPattern = regexp.MustCompile(`\[([a-zA-Z0-9-]{4,32})\]`)

// injectionLinePattern identifies the rendered injection line so self-citations
// from workers quoting it back are stripped before scanning. Bracket-bounded
// character class avoids greedy matches swallowing later prose citations.
var injectionLinePattern = regexp.MustCompile(`^- \[[^\]]+ · [^\]]+\]`)

// ParseCitedLearnings extracts short-IDs the worker cited as `[short-id]` in
// free prose. Injection-block lines (`- [type · short-id] content`) are
// stripped first so they don't self-cite. Returns deduplicated matches in
// first-seen order (case-sensitive).
func ParseCitedLearnings(output string) []string {
	var filtered strings.Builder
	for _, line := range strings.Split(output, "\n") {
		if injectionLinePattern.MatchString(line) {
			continue
		}
		filtered.WriteString(line)
		filtered.WriteByte('\n')
	}

	matches := citedPattern.FindAllStringSubmatch(filtered.String(), -1)
	seen := make(map[string]bool, len(matches))
	result := make([]string, 0, len(matches))
	for _, m := range matches {
		id := m[1]
		if seen[id] {
			continue
		}
		seen[id] = true
		result = append(result, id)
	}
	return result
}

// stopWords are common English words excluded from compliance keyword matching.
// We only want distinctive domain words, not connectives and articles.
var stopWords = map[string]bool{
	"the": true, "and": true, "that": true, "this": true, "with": true,
	"from": true, "have": true, "been": true, "when": true, "will": true,
	"they": true, "your": true, "more": true, "also": true, "into": true,
	"than": true, "then": true, "each": true, "does": true, "were": true,
	"which": true, "their": true, "there": true, "would": true, "could": true,
	"should": true, "where": true, "what": true, "using": true, "used": true,
	"make": true, "made": true, "must": true, "need": true, "only": true,
	"over": true, "some": true, "such": true, "very": true, "well": true,
}

// ComplianceScan checks whether each injected learning appears to have been
// followed in the worker outputs for a mission.
//
// Strategy: extract distinctive words (≥5 chars, non-stop) from the learning
// content and count how many appear in the lowercased combined worker output.
// A learning is considered followed if ≥2 keywords match (or ≥40% if content
// is short). This is intentionally conservative to avoid false positives.
//
// Returns a map of learning ID → followed.
//
// Citation boost: if the worker cited a learning's ShortID explicitly via
// [short-id] in its output, that learning is marked followed regardless of
// keyword overlap. Citations are additive — they can flip false→true but
// never true→false.
func ComplianceScan(injected []Learning, workerOutputs string) map[string]bool {
	lowerOutput := strings.ToLower(workerOutputs)

	cited := ParseCitedLearnings(workerOutputs)
	citedSet := make(map[string]bool, len(cited))
	for _, id := range cited {
		citedSet[id] = true
	}

	result := make(map[string]bool, len(injected))
	for _, l := range injected {
		if citedSet[l.ShortID()] {
			result[l.ID] = true
			continue
		}
		result[l.ID] = learningFollowed(l.Content, lowerOutput)
	}
	return result
}

// learningFollowed returns true when the learning content has enough keyword
// overlap with lowerOutput to indicate it influenced the worker's work.
func learningFollowed(content, lowerOutput string) bool {
	keywords := extractKeywords(content)
	if len(keywords) == 0 {
		return false
	}

	matches := 0
	for _, kw := range keywords {
		if strings.Contains(lowerOutput, kw) {
			matches++
		}
	}

	// Need at least 2 matching keywords, or 40% of keywords — whichever is smaller.
	// Floor at 2 so short learnings don't trigger on a single word hit.
	threshold := len(keywords) * 4 / 10
	if threshold < 2 {
		threshold = 2
	}
	if threshold > len(keywords) {
		threshold = len(keywords)
	}
	return matches >= threshold
}

// extractKeywords returns lowercase distinctive words (≥5 chars, non-stop) from text.
func extractKeywords(text string) []string {
	seen := make(map[string]bool)
	var result []string

	for _, word := range strings.Fields(strings.ToLower(text)) {
		// Strip common punctuation at word boundaries
		word = strings.Trim(word, ".,;:!?\"'()-[]{}*`")
		if len(word) < 5 {
			continue
		}
		if stopWords[word] {
			continue
		}
		if seen[word] {
			continue
		}
		seen[word] = true
		result = append(result, word)
	}
	return result
}
