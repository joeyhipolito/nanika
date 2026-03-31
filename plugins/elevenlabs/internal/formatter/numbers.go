package formatter

import (
	"fmt"
	"regexp"
	"strconv"
	"strings"
)

var (
	// timeRe matches time like 5:01pm, 10:30am
	timeRe = regexp.MustCompile(`\b(\d{1,2}):(\d{2})\s*(am|pm|AM|PM)\b`)
	// percentRe matches percentages like 95%, 76.4%
	percentRe = regexp.MustCompile(`\b(\d+(?:\.\d+)?)\s*%`)
	// ordinalRe matches ordinals like 1st, 2nd, 3rd, 45th
	ordinalRe = regexp.MustCompile(`\b(\d+)(st|nd|rd|th)\b`)
	// commaNumRe matches large numbers with commas like 780,000
	commaNumRe = regexp.MustCompile(`\b(\d{1,3}(?:,\d{3})+)\b`)
	// largeNumRe matches "16 million", "3 billion" etc.
	largeNumRe = regexp.MustCompile(`\b(\d+(?:\.\d+)?)\s+(million|billion|trillion|thousand)\b`)
	// versionRe matches version numbers like v3.0, v2.2 (to skip).
	versionRe = regexp.MustCompile(`\bv\d+\.\d+\b`)
	// plainIntRe matches standalone integers (protected from version/decimal context).
	plainIntRe = regexp.MustCompile(`(?:^|[^.\w])(\d{1,9})(?:[^.\w]|$)`)
)

var ones = []string{
	"", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine",
	"ten", "eleven", "twelve", "thirteen", "fourteen", "fifteen",
	"sixteen", "seventeen", "eighteen", "nineteen",
}

var tens = []string{
	"", "", "twenty", "thirty", "forty", "fifty",
	"sixty", "seventy", "eighty", "ninety",
}

var irregularOrdinals = map[int]string{
	1: "first", 2: "second", 3: "third", 4: "fourth", 5: "fifth",
	6: "sixth", 7: "seventh", 8: "eighth", 9: "ninth", 10: "tenth",
	11: "eleventh", 12: "twelfth", 20: "twentieth", 30: "thirtieth",
	40: "fortieth", 50: "fiftieth", 60: "sixtieth", 70: "seventieth",
	80: "eightieth", 90: "ninetieth",
}

// intToWords converts a non-negative integer to English words.
func intToWords(n int) string {
	if n == 0 {
		return "zero"
	}
	if n < 0 {
		return "negative " + intToWords(-n)
	}
	if n < 20 {
		return ones[n]
	}
	if n < 100 {
		w := tens[n/10]
		if n%10 != 0 {
			w += "-" + ones[n%10]
		}
		return w
	}
	if n < 1_000 {
		w := ones[n/100] + " hundred"
		if n%100 != 0 {
			w += " " + intToWords(n%100)
		}
		return w
	}
	if n < 1_000_000 {
		w := intToWords(n/1_000) + " thousand"
		if n%1_000 != 0 {
			w += " " + intToWords(n%1_000)
		}
		return w
	}
	if n < 1_000_000_000 {
		w := intToWords(n/1_000_000) + " million"
		if n%1_000_000 != 0 {
			w += " " + intToWords(n%1_000_000)
		}
		return w
	}
	w := intToWords(n/1_000_000_000) + " billion"
	if n%1_000_000_000 != 0 {
		w += " " + intToWords(n%1_000_000_000)
	}
	return w
}

// ordinalToWords converts an integer to its ordinal form (e.g., 3 → third).
func ordinalToWords(n int) string {
	if w, ok := irregularOrdinals[n]; ok {
		return w
	}
	// Tens with non-zero ones: twenty-first, etc.
	if n > 20 && n < 100 && n%10 != 0 {
		base := tens[n/10]
		return base + "-" + irregularOrdinalSuffix(n%10)
	}
	return intToWords(n) + "th"
}

func irregularOrdinalSuffix(n int) string {
	switch n {
	case 1:
		return "first"
	case 2:
		return "second"
	case 3:
		return "third"
	case 4:
		return "fourth"
	case 5:
		return "fifth"
	case 6:
		return "sixth"
	case 7:
		return "seventh"
	case 8:
		return "eighth"
	case 9:
		return "ninth"
	default:
		return ones[n] + "th"
	}
}

// yearToWords speaks a 4-digit year naturally.
func yearToWords(y int) string {
	switch {
	case y == 2000:
		return "two thousand"
	case y >= 2001 && y <= 2009:
		return "two thousand " + ones[y-2000]
	case y >= 2010 && y <= 2099:
		return "twenty " + intToWords(y-2000)
	case y >= 1900 && y <= 1999:
		hi := y / 100
		lo := y % 100
		if lo == 0 {
			return ones[hi] + " hundred"
		}
		return intToWords(hi*10+(y/10%10)) + " " + intToWords(y%10)
	default:
		return intToWords(y)
	}
}

// timeToWords converts hours, minutes, and am/pm to spoken words.
// 5:01pm → "five oh one p.m."
// 10:30am → "ten thirty a.m."
func timeToWords(h, m int, ampm string) string {
	hWord := intToWords(h)
	var mWord string
	switch {
	case m == 0:
		mWord = ""
	case m < 10:
		mWord = " oh " + ones[m]
	default:
		mWord = " " + intToWords(m)
	}
	suffix := strings.ToLower(ampm)
	suffix = string(suffix[0]) + "." + string(suffix[1]) + "."
	return hWord + mWord + " " + suffix
}

// normalizeNumbers replaces numeric expressions with their spoken equivalents.
func normalizeNumbers(text string) string {
	// Protect version numbers like v3.0 from substitution.
	protected := make(map[string]string)
	idx := 0
	text = versionRe.ReplaceAllStringFunc(text, func(s string) string {
		key := fmt.Sprintf("\x00VER%d\x00", idx)
		protected[key] = s
		idx++
		return key
	})

	// 1. Time expressions: 5:01pm → "five oh one p.m."
	text = timeRe.ReplaceAllStringFunc(text, func(s string) string {
		m := timeRe.FindStringSubmatch(s)
		h, _ := strconv.Atoi(m[1])
		min, _ := strconv.Atoi(m[2])
		return timeToWords(h, min, m[3])
	})

	// 2. Percentages: 95% → "ninety-five percent"
	text = percentRe.ReplaceAllStringFunc(text, func(s string) string {
		m := percentRe.FindStringSubmatch(s)
		if strings.Contains(m[1], ".") {
			parts := strings.SplitN(m[1], ".", 2)
			whole, _ := strconv.Atoi(parts[0])
			return intToWords(whole) + " point " + parts[1] + " percent"
		}
		n, _ := strconv.Atoi(m[1])
		return intToWords(n) + " percent"
	})

	// 3. Ordinals: 1st → "first", 21st → "twenty-first"
	text = ordinalRe.ReplaceAllStringFunc(text, func(s string) string {
		m := ordinalRe.FindStringSubmatch(s)
		n, _ := strconv.Atoi(m[1])
		return ordinalToWords(n)
	})

	// 4. Large numbers with commas: 780,000 → "seven hundred eighty thousand"
	text = commaNumRe.ReplaceAllStringFunc(text, func(s string) string {
		m := commaNumRe.FindStringSubmatch(s)
		cleaned := strings.ReplaceAll(m[1], ",", "")
		n, _ := strconv.Atoi(cleaned)
		return intToWords(n)
	})

	// 5. "16 million", "3.5 billion" etc.
	text = largeNumRe.ReplaceAllStringFunc(text, func(s string) string {
		m := largeNumRe.FindStringSubmatch(s)
		if strings.Contains(m[1], ".") {
			parts := strings.SplitN(m[1], ".", 2)
			whole, _ := strconv.Atoi(parts[0])
			return intToWords(whole) + " point " + parts[1] + " " + m[2]
		}
		n, _ := strconv.Atoi(m[1])
		return intToWords(n) + " " + m[2]
	})

	// 6. 4-digit years (1900–2099) spoken naturally.
	yearRe := regexp.MustCompile(`\b((?:19|20)\d{2})\b`)
	text = yearRe.ReplaceAllStringFunc(text, func(s string) string {
		y, _ := strconv.Atoi(s)
		return yearToWords(y)
	})

	// 7. Remaining plain integers (skip if preceded by a letter — model names, etc.)
	plainIntFullRe := regexp.MustCompile(`(^|[^a-zA-Z.\d])(\d{1,9})([^a-zA-Z.\d]|$)`)
	text = plainIntFullRe.ReplaceAllStringFunc(text, func(s string) string {
		m := plainIntFullRe.FindStringSubmatch(s)
		n, err := strconv.Atoi(m[2])
		if err != nil {
			return s
		}
		return m[1] + intToWords(n) + m[3]
	})

	// Restore protected version strings.
	for key, val := range protected {
		text = strings.ReplaceAll(text, key, val)
	}

	return text
}
