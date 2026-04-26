package zettel

import (
	"fmt"
	"strings"
)

// MarkerKind is the kind of an inline learning marker.
type MarkerKind string

const (
	MarkerFinding  MarkerKind = "FINDING"
	MarkerDecision MarkerKind = "DECISION"
	MarkerPattern  MarkerKind = "PATTERN"
	MarkerGotcha   MarkerKind = "GOTCHA"
	MarkerLearning MarkerKind = "LEARNING"
)

// Marker is a parsed inline learning marker extracted from phase output.
type Marker struct {
	Kind    MarkerKind
	Content string
	Line    int // 1-based line number
}

// knownMarkers lists all recognizable marker prefixes in precedence order.
var knownMarkers = []MarkerKind{
	MarkerFinding,
	MarkerDecision,
	MarkerPattern,
	MarkerGotcha,
	MarkerLearning,
}

// ParseMarkers scans text line-by-line for inline learning markers
// (FINDING:, DECISION:, PATTERN:, GOTCHA:, LEARNING:) and returns them in
// order of occurrence. Arbitrary input — including empty or binary text — never
// causes a panic.
func ParseMarkers(text string) []Marker {
	var markers []Marker
	lines := strings.Split(text, "\n")
	for i, line := range lines {
		kind, content, ok := parseMarkerLine(line)
		if !ok {
			continue
		}
		markers = append(markers, Marker{
			Kind:    kind,
			Content: content,
			Line:    i + 1,
		})
	}
	return markers
}

// FormatMarkers renders markers as newline-separated "KIND: content" lines.
func FormatMarkers(markers []Marker) string {
	var b strings.Builder
	for _, m := range markers {
		fmt.Fprintf(&b, "%s: %s\n", m.Kind, m.Content)
	}
	return b.String()
}

func parseMarkerLine(line string) (MarkerKind, string, bool) {
	trimmed := strings.TrimSpace(line)
	for _, kind := range knownMarkers {
		prefix := string(kind) + ":"
		if strings.HasPrefix(trimmed, prefix) {
			content := strings.TrimSpace(strings.TrimPrefix(trimmed, prefix))
			return kind, content, true
		}
	}
	return "", "", false
}
