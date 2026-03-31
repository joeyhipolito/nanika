package formatter

import (
	"regexp"
	"strconv"
	"strings"
)

const (
	clipNarrated         = "narrated"
	clipVisualOnly       = "visual_only"
	defaultClipGridSecs  = 8
)

// ClipManifest describes the clip structure of a narration script.
// Produced by Format when clip headers are present in the input.
type ClipManifest struct {
	ClipGridSeconds      int             `json:"clip_grid_seconds"`
	TotalClips           int             `json:"total_clips"`
	TotalDurationSeconds int             `json:"total_duration_seconds"`
	NarratedClips        int             `json:"narrated_clips"`
	VisualOnlyClips      int             `json:"visual_only_clips"`
	Segments             []ManifestEntry `json:"segments"`
}

// ManifestEntry describes a single narrated segment in the clip sequence.
type ManifestEntry struct {
	Index        int `json:"index"`
	Clip         int `json:"clip"`
	SilenceAfter int `json:"silence_after"`
}

// clipTimestampRe extracts start:end from clip headers like **[0:00–0:08] | ...**
var clipTimestampRe = regexp.MustCompile(`\[(\d+:\d+)[–\-](\d+:\d+)\]`)

// parseClipGridSeconds extracts the clip duration from a clip header line.
// Returns defaultClipGridSecs if parsing fails.
func parseClipGridSeconds(header string) int {
	m := clipTimestampRe.FindStringSubmatch(header)
	if m == nil {
		return defaultClipGridSecs
	}
	// Caller guarantees digits via clipTimestampRe capture groups.
	start := parseTimestamp(m[1])
	end := parseTimestamp(m[2])
	if end <= start {
		return defaultClipGridSecs
	}
	return end - start
}

// parseTimestamp parses "M:SS" into total seconds.
func parseTimestamp(ts string) int {
	parts := strings.Split(ts, ":")
	if len(parts) != 2 {
		return 0
	}
	min, _ := strconv.Atoi(parts[0])
	sec, _ := strconv.Atoi(parts[1])
	return min*60 + sec
}

// clipEntry tracks a clip during line processing.
type clipEntry struct {
	number int    // 1-based clip number
	kind   string // "narrated" or "visual_only"
}

// buildManifest constructs a ClipManifest from tracked clip data.
// It computes silence_after for each segment by counting consecutive
// visual-only clips following each narrated clip.
func buildManifest(clips []clipEntry, segments []ManifestEntry, gridSecs int) *ClipManifest {
	if gridSecs == 0 {
		gridSecs = defaultClipGridSecs
	}

	// Compute silence_after for each segment.
	for i, seg := range segments {
		voCount := 0
		clipIdx := seg.Clip - 1 // 0-based index into clips
		for k := clipIdx + 1; k < len(clips); k++ {
			if clips[k].kind == clipVisualOnly {
				voCount++
			} else {
				break
			}
		}
		segments[i].SilenceAfter = voCount * gridSecs
	}

	if segments == nil {
		segments = []ManifestEntry{}
	}

	narrated := len(segments)
	return &ClipManifest{
		ClipGridSeconds:      gridSecs,
		TotalClips:           len(clips),
		TotalDurationSeconds: len(clips) * gridSecs,
		NarratedClips:        narrated,
		VisualOnlyClips:      len(clips) - narrated,
		Segments:             segments,
	}
}
