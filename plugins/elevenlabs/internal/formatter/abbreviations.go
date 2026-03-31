package formatter

import (
	"regexp"
	"strings"
)

// skipWords are common English uppercase tokens that should not be dotted.
var skipWords = map[string]bool{
	"AND": true, "BUT": true, "FOR": true, "NOT": true, "OR": true,
	"THE": true, "IN": true, "ON": true, "AT": true, "BY": true,
	"TO": true, "UP": true, "SO": true, "AS": true, "IF": true,
	"IT": true, "WE": true, "NO": true, "AM": true, "AN": true,
	"DO": true, "GO": true, "MY": true, "OF": true, "IS": true,
	"BE": true, "HE": true, "SHE": true, "WHO": true, "HOW": true,
	"WHY": true, "WAR": true, "NEW": true,
}

// specialExpansions handles abbreviations that expand to prose rather than dots.
var specialExpansions = map[string]string{
	"R&D":  "R and D",
	"P&L":  "P and L",
	"M&A":  "M and A",
	"Q&A":  "Q and A",
}

// allCapsRe matches 2–6 uppercase letter sequences at word boundaries.
// Does NOT match sequences that contain lowercase (e.g., DoD is handled separately).
var allCapsRe = regexp.MustCompile(`\b([A-Z]{2,6})\b`)

// mixedCapsAbbrevRe matches common mixed-case abbreviations like DoD, SecDef.
var mixedCapsAbbrevRe = regexp.MustCompile(`\b(D[oO][dD])\b`)

// ampersandAbbrRe matches X&Y patterns.
var ampersandAbbrRe = regexp.MustCompile(`\b([A-Z])&([A-Z])\b`)

// normalizeAbbreviations expands abbreviations so TTS pronounces each letter.
//
// Rules applied in order:
//  1. X&Y patterns: R&D → R and D
//  2. Mixed-case known abbreviations: DoD → D.O.D.
//  3. All-caps 2–6 letter tokens not in skipWords: AI → A.I.
func normalizeAbbreviations(text string) string {
	// 1. Special ampersand expansions.
	text = ampersandAbbrRe.ReplaceAllStringFunc(text, func(s string) string {
		m := ampersandAbbrRe.FindStringSubmatch(s)
		key := m[1] + "&" + m[2]
		if exp, ok := specialExpansions[key]; ok {
			return exp
		}
		return m[1] + " and " + m[2]
	})

	// 2. Mixed-case abbreviations: DoD → D.O.D.
	text = mixedCapsAbbrevRe.ReplaceAllStringFunc(text, func(s string) string {
		return dotExpand(strings.ToUpper(s))
	})

	// 3. All-caps sequences.
	text = allCapsRe.ReplaceAllStringFunc(text, func(s string) string {
		if skipWords[s] {
			return s
		}
		return dotExpand(s)
	})

	return text
}

// dotExpand turns "RSP" into "R.S.P." and "AI" into "A.I."
func dotExpand(s string) string {
	if len(s) == 0 {
		return s
	}
	var sb strings.Builder
	for i, c := range s {
		if i > 0 {
			sb.WriteByte('.')
		}
		sb.WriteRune(c)
	}
	sb.WriteByte('.')
	return sb.String()
}
