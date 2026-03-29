// Package sanitize provides Unicode sanitization to defend against prompt injection
// via invisible/format characters embedded in untrusted text from web scraping,
// emails, social posts, and other external sources.
package sanitize

import (
	"strings"
	"unicode"
	"unicode/utf8"

	"golang.org/x/text/unicode/norm"
)

// Finding describes a single invisible or suspicious character found in text.
type Finding struct {
	Position    int    // byte offset in the original string
	Rune        rune   // the character found
	Description string // human-readable description
}

// SanitizeText strips invisible Unicode characters and applies NFKC normalization.
//
// Stripped categories:
//   - C0 controls (U+0000–U+001F) except TAB (U+0009), LF (U+000A), CR (U+000D)
//   - DEL (U+007F)
//   - C1 controls (U+0080–U+009F)
//   - Unicode General_Category=Cf (format characters): zero-width chars, BOM,
//     soft hyphen, bidi controls, interlinear annotations, Mongolian vowel separator, etc.
//   - U+FFFE (noncharacter paired with BOM U+FEFF)
//   - Line separator U+2028, paragraph separator U+2029
//   - Variation selectors U+FE00–U+FE0F and U+E0100–U+E01EF
//   - Tag characters U+E0001–U+E007F
//
// Combining diacritical marks (legitimate accents) are preserved.
// NFKC normalization is applied after stripping.
func SanitizeText(s string) string {
	var b strings.Builder
	b.Grow(len(s))
	for _, r := range s {
		if shouldStrip(r) {
			continue
		}
		b.WriteRune(r)
	}
	return norm.NFKC.String(b.String())
}

// HasInvisible reports whether s contains any invisible characters that
// SanitizeText would strip. This is a fast O(n) check.
func HasInvisible(s string) bool {
	for _, r := range s {
		if shouldStrip(r) {
			return true
		}
	}
	return false
}

// DetectInvisible returns a Finding for each invisible character in s,
// with the byte offset and a human-readable description. Used for logging/alerting.
func DetectInvisible(s string) []Finding {
	var findings []Finding
	pos := 0
	for _, r := range s {
		if shouldStrip(r) {
			findings = append(findings, Finding{
				Position:    pos,
				Rune:        r,
				Description: describeRune(r),
			})
		}
		pos += utf8.RuneLen(r)
	}
	return findings
}

// DetectHomoglyphs returns warnings for characters from Cyrillic or Armenian
// scripts appearing in otherwise Latin-dominant text. The characters are not
// stripped (homoglyphs are detected only, never removed), but callers may log
// or alert on findings.
func DetectHomoglyphs(s string) []Finding {
	var hasLatin, hasCyrillic, hasArmenian bool
	for _, r := range s {
		switch {
		case unicode.Is(unicode.Latin, r):
			hasLatin = true
		case unicode.Is(unicode.Cyrillic, r):
			hasCyrillic = true
		case unicode.Is(unicode.Armenian, r):
			hasArmenian = true
		}
	}

	// Only report if the text is primarily Latin and mixed with a look-alike script.
	if !hasLatin || (!hasCyrillic && !hasArmenian) {
		return nil
	}

	var findings []Finding
	pos := 0
	for _, r := range s {
		size := utf8.RuneLen(r)
		var script string
		switch {
		case unicode.Is(unicode.Cyrillic, r):
			script = "Cyrillic"
		case unicode.Is(unicode.Armenian, r):
			script = "Armenian"
		}
		if script != "" {
			findings = append(findings, Finding{
				Position:    pos,
				Rune:        r,
				Description: "potential homoglyph: " + script + " character in Latin text (U+" + runeHex(r) + ")",
			})
		}
		pos += size
	}
	return findings
}

// shouldStrip reports whether r is an invisible or dangerous character that
// must be stripped from untrusted input.
func shouldStrip(r rune) bool {
	// C0 controls except TAB (U+0009), LF (U+000A), CR (U+000D)
	if r <= 0x1F {
		return r != '\t' && r != '\n' && r != '\r'
	}
	// DEL
	if r == 0x7F {
		return true
	}
	// C1 controls U+0080–U+009F
	if r >= 0x80 && r <= 0x9F {
		return true
	}
	// Unicode General_Category Cf (format characters):
	// covers U+00AD (soft hyphen), U+200B–U+200F, U+202A–U+202E, U+2060–U+2064,
	// U+2066–U+206F, U+FEFF (BOM), U+FFF9–U+FFFB, U+180E, and others.
	if unicode.Is(unicode.Cf, r) {
		return true
	}
	// U+FFFE — noncharacter, counterpart to BOM U+FEFF (which is Cf).
	if r == 0xFFFE {
		return true
	}
	// Line separator U+2028 (Zl) and paragraph separator U+2029 (Zp).
	// These are not Cf but are invisible flow-control characters.
	if r == 0x2028 || r == 0x2029 {
		return true
	}
	// Variation selectors VS-1 to VS-16 (U+FE00–U+FE0F)
	if r >= 0xFE00 && r <= 0xFE0F {
		return true
	}
	// Variation selectors supplement (U+E0100–U+E01EF)
	if r >= 0xE0100 && r <= 0xE01EF {
		return true
	}
	// Tag characters U+E0001–U+E007F
	if r >= 0xE0001 && r <= 0xE007F {
		return true
	}
	return false
}

// describeRune returns a short human-readable label for an invisible rune.
func describeRune(r rune) string {
	switch r {
	case 0x00AD:
		return "soft hyphen (U+00AD)"
	case 0x180E:
		return "Mongolian vowel separator (U+180E)"
	case 0x200B:
		return "zero-width space (U+200B)"
	case 0x200C:
		return "zero-width non-joiner (U+200C)"
	case 0x200D:
		return "zero-width joiner (U+200D)"
	case 0x200E:
		return "left-to-right mark (U+200E)"
	case 0x200F:
		return "right-to-left mark (U+200F)"
	case 0x2028:
		return "line separator (U+2028)"
	case 0x2029:
		return "paragraph separator (U+2029)"
	case 0x202A:
		return "left-to-right embedding (U+202A)"
	case 0x202B:
		return "right-to-left embedding (U+202B)"
	case 0x202C:
		return "pop directional formatting (U+202C)"
	case 0x202D:
		return "left-to-right override (U+202D)"
	case 0x202E:
		return "right-to-left override (U+202E)"
	case 0x2060:
		return "word joiner (U+2060)"
	case 0x2066:
		return "left-to-right isolate (U+2066)"
	case 0x2067:
		return "right-to-left isolate (U+2067)"
	case 0x2068:
		return "first strong isolate (U+2068)"
	case 0x2069:
		return "pop directional isolate (U+2069)"
	case 0xFEFF:
		return "byte order mark / zero-width no-break space (U+FEFF)"
	case 0xFFFE:
		return "reversed BOM / noncharacter (U+FFFE)"
	case 0xFFF9:
		return "interlinear annotation anchor (U+FFF9)"
	case 0xFFFA:
		return "interlinear annotation separator (U+FFFA)"
	case 0xFFFB:
		return "interlinear annotation terminator (U+FFFB)"
	}
	switch {
	case r >= 0x00 && r <= 0x1F:
		return "C0 control character (U+" + runeHex(r) + ")"
	case r == 0x7F:
		return "DEL (U+007F)"
	case r >= 0x80 && r <= 0x9F:
		return "C1 control character (U+" + runeHex(r) + ")"
	case r >= 0x2061 && r <= 0x2064:
		return "invisible math operator (U+" + runeHex(r) + ")"
	case r >= 0x206A && r <= 0x206F:
		return "deprecated formatting character (U+" + runeHex(r) + ")"
	case r >= 0xFE00 && r <= 0xFE0F:
		return "variation selector (U+" + runeHex(r) + ")"
	case r >= 0xE0001 && r <= 0xE007F:
		return "tag character (U+" + runeHex(r) + ")"
	case r >= 0xE0100 && r <= 0xE01EF:
		return "variation selector supplement (U+" + runeHex(r) + ")"
	}
	return "invisible character (U+" + runeHex(r) + ")"
}

// runeHex returns the upper-case hex code point for r without the "0x" prefix,
// zero-padded to at least 4 digits.
func runeHex(r rune) string {
	const hex = "0123456789ABCDEF"
	if r < 0x10000 {
		return string([]byte{
			hex[(r>>12)&0xF],
			hex[(r>>8)&0xF],
			hex[(r>>4)&0xF],
			hex[r&0xF],
		})
	}
	// 5 or 6 hex digits for supplementary planes
	digits := make([]byte, 0, 6)
	for v := r; v > 0; v >>= 4 {
		digits = append([]byte{hex[v&0xF]}, digits...)
	}
	for len(digits) < 5 {
		digits = append([]byte{'0'}, digits...)
	}
	return string(digits)
}
