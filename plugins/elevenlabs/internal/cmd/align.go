package cmd

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/config"
)

// AlignCmd runs forced alignment on a voiceover audio file against a transcript,
// then segments the result into fixed-duration time windows.
//
// Usage: elevenlabs align <audio> <transcript.txt> [--output <path>] [--window <secs>]
//
// Flags:
//
//	--output <path>    Output path for narration-clip-windows.json (default: same dir as audio)
//	--window <secs>    Window duration in seconds (default: 8)
func AlignCmd(args []string) error {
	fs := flag.NewFlagSet("align", flag.ContinueOnError)
	outputPath := fs.String("output", "", "output path for narration-clip-windows.json (default: same dir as audio)")
	windowSecs := fs.Float64("window", 8, "window duration in seconds")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: elevenlabs align <audio> <transcript.txt> [--output <path>] [--window <secs>]\n\n")
		fmt.Fprintf(os.Stderr, "Runs forced alignment and produces clip windows JSON:\n")
		fmt.Fprintf(os.Stderr, "  - Calls ElevenLabs POST /v1/forced-alignment\n")
		fmt.Fprintf(os.Stderr, "  - Writes narration-clip-windows.json\n\n")
		fs.PrintDefaults()
	}

	flagArgs, positional := splitArgs(args)
	if err := fs.Parse(flagArgs); err != nil {
		return err
	}
	if len(positional) < 2 {
		fs.Usage()
		return fmt.Errorf("missing required arguments: <audio> <transcript.txt>")
	}
	if *windowSecs <= 0 {
		return fmt.Errorf("--window must be greater than 0, got %g", *windowSecs)
	}
	audioPath := positional[0]
	transcriptPath := positional[1]

	// Load config and validate API key.
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.APIKey == "" {
		return fmt.Errorf("no API key configured. Run 'elevenlabs configure' first")
	}

	// Read and clean transcript.
	transcriptBytes, err := os.ReadFile(transcriptPath)
	if err != nil {
		return fmt.Errorf("reading transcript %s: %w", transcriptPath, err)
	}
	cleanText := cleanTranscript(string(transcriptBytes))
	if cleanText == "" {
		return fmt.Errorf("transcript is empty after cleaning")
	}

	// Determine output path.
	outFile := *outputPath
	if outFile == "" {
		outFile = filepath.Join(filepath.Dir(audioPath), "narration-clip-windows.json")
	}

	// Call ElevenLabs forced-alignment API.
	fmt.Println("Aligning...")
	client := api.NewClient(cfg.APIKey)
	alignment, err := client.ForcedAlignment(context.Background(), audioPath, cleanText)
	if err != nil {
		return fmt.Errorf("forced alignment: %w", err)
	}

	// Build word list from character-level alignment.
	words := api.BuildWords(alignment)

	// Compute audio duration from last character end time.
	audioDuration := 0.0
	if len(alignment.CharacterEndTimes) > 0 {
		audioDuration = alignment.CharacterEndTimes[len(alignment.CharacterEndTimes)-1]
	}

	// Partition words into fixed-duration windows; track uncovered word count for loss.
	windows, uncovered := buildClipWindows(words, audioDuration, *windowSecs)
	loss := 0.0
	if len(words) > 0 {
		loss = float64(uncovered) / float64(len(words))
	}

	result := clipWindowsOutput{
		Meta: clipMeta{
			AudioDurationS:  audioDuration,
			WindowCount:     len(windows),
			WindowDurationS: *windowSecs,
			Loss:            loss,
			SourceAudio:     filepath.Base(audioPath),
		},
		Windows: windows,
	}

	outBytes, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return fmt.Errorf("marshalling output: %w", err)
	}
	if err := os.WriteFile(outFile, outBytes, 0o644); err != nil {
		return fmt.Errorf("writing %s: %w", outFile, err)
	}

	fmt.Printf("aligned: %d windows, %.1fs, loss %.4f → %s\n", len(windows), audioDuration, loss, outFile)
	return nil
}

var tagRe = regexp.MustCompile(`\[[^\]]*\]`)

// cleanTranscript strips [tag] patterns and blank lines, joining remainder with spaces.
func cleanTranscript(text string) string {
	text = tagRe.ReplaceAllString(text, "")
	var lines []string
	for _, line := range strings.Split(text, "\n") {
		line = strings.TrimSpace(line)
		if line != "" {
			lines = append(lines, line)
		}
	}
	return strings.Join(lines, " ")
}

// buildClipWindows partitions words into fixed-duration windows spanning the full audio.
// A word is included in a window if word.start < win.end AND word.end > win.start.
// Returns the windows and the count of words not covered by any window.
func buildClipWindows(words []api.Word, audioDuration, windowSecs float64) ([]clipWindow, int) {
	numWindows := int(math.Ceil(audioDuration / windowSecs))
	if numWindows == 0 {
		numWindows = 1
	}

	covered := make(map[int]struct{}, len(words))
	windows := make([]clipWindow, numWindows)
	for i := range windows {
		winStart := float64(i) * windowSecs
		winEnd := winStart + windowSecs
		winWords := []api.Word{}
		for j, w := range words {
			if w.Start < winEnd && w.End > winStart {
				winWords = append(winWords, w)
				covered[j] = struct{}{}
			}
		}
		windows[i] = clipWindow{
			WindowID:   i + 1,
			Start:      winStart,
			End:        winEnd,
			IsNarrated: len(winWords) > 0,
			Words:      winWords,
		}
	}
	return windows, len(words) - len(covered)
}

type clipMeta struct {
	AudioDurationS  float64 `json:"audio_duration_s"`
	WindowCount     int     `json:"window_count"`
	WindowDurationS float64 `json:"window_duration_s"`
	Loss            float64 `json:"loss"`
	SourceAudio     string  `json:"source_audio"`
}

type clipWindow struct {
	WindowID   int        `json:"window_id"`
	Start      float64    `json:"start"`
	End        float64    `json:"end"`
	IsNarrated bool       `json:"is_narrated"`
	Words      []api.Word `json:"words"`
}

type clipWindowsOutput struct {
	Meta    clipMeta     `json:"meta"`
	Windows []clipWindow `json:"windows"`
}
