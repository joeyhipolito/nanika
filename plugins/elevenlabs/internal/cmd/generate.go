package cmd

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/config"
)

// GenerateCmd reads formatted TTS text, calls the ElevenLabs API, and writes
// voiceover.mp3 (or other format) + timing-map.json.
//
// Usage: elevenlabs generate <text.txt> [--voice <voice_id>] [--output <path>] [--seed <N>] [--speed <N>] [--format <format>]
//
// Flags:
//   --voice <voice_id>   ElevenLabs voice ID (defaults to configured default)
//   --output <path>      Output directory or audio file path (default: same as input directory)
//   --seed <N>           Random seed for TTS (0-4294967295, omitted if not set)
//   --speed <N>          Speech speed (0.7-1.2, default 1.0, omitted if not explicitly set)
//   --format <format>    Output audio format (default: mp3_44100_128)
func GenerateCmd(args []string) error {
	fs := flag.NewFlagSet("generate", flag.ContinueOnError)
	voiceID := fs.String("voice", "", "ElevenLabs voice ID (uses config default if not set)")
	outputPath := fs.String("output", "", "output directory or base path (default: same as input directory)")
	seedFlag := fs.Uint("seed", 0, "random seed for TTS (0-4294967295)")
	speedFlag := fs.Float64("speed", 0, "speech speed (0.7-1.2; omitted if not set)")
	formatFlag := fs.String("format", "mp3_44100_128", "output audio format")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: elevenlabs generate <text.txt> [--voice <voice_id>] [--output <path>]\n\n")
		fmt.Fprintf(os.Stderr, "Reads formatted TTS text and generates voiceover audio:\n")
		fmt.Fprintf(os.Stderr, "  - Calls ElevenLabs API to synthesize audio\n")
		fmt.Fprintf(os.Stderr, "  - Writes voiceover.mp3 and timing-map.json\n\n")
		fs.PrintDefaults()
	}

	// Pre-split args to allow flags after positional argument.
	flagArgs, positional := splitArgs(args)
	if err := fs.Parse(flagArgs); err != nil {
		return err
	}
	if len(positional) == 0 {
		fs.Usage()
		return fmt.Errorf("missing required argument: <text.txt>")
	}
	inputPath := positional[0]

	// Load config and validate API key.
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.APIKey == "" {
		return fmt.Errorf("no API key configured. Run 'elevenlabs configure' first")
	}

	// Use provided voice ID or fall back to config default.
	voice := *voiceID
	if voice == "" {
		voice = cfg.DefaultVoiceID
	}
	if voice == "" {
		return fmt.Errorf("no voice ID provided and no default configured. Use --voice or run 'elevenlabs configure'")
	}

	// Read input text.
	textBytes, err := os.ReadFile(inputPath)
	if err != nil {
		return fmt.Errorf("reading %s: %w", inputPath, err)
	}
	text := string(textBytes)

	// Prepare TTS request.
	vs := api.VoiceSettings{
		Stability:       0.5,
		SimilarityBoost: 0.75,
		Style:           0.0,
	}
	if *speedFlag > 0 {
		vs.Speed = *speedFlag
	}

	req := api.TTSRequest{
		Text:           text,
		ModelID:        cfg.Model,
		VoiceSettings:  vs,
	}
	if *seedFlag > 0 {
		req.Seed = int32(*seedFlag)
	}

	// Call ElevenLabs API.
	fmt.Println("Generating audio...")
	client := api.NewClient(cfg.APIKey)
	audioBytes, alignment, err := client.GenerateWithTimestamps(context.Background(), voice, req, *formatFlag)
	if err != nil {
		return fmt.Errorf("generating audio: %w", err)
	}

	// Determine audio file extension based on format.
	audioExt := ".mp3"
	if strings.HasPrefix(*formatFlag, "pcm_") {
		audioExt = ".pcm"
	} else if strings.HasPrefix(*formatFlag, "opus_") {
		audioExt = ".opus"
	}

	// Determine output paths.
	outputDir := *outputPath
	if outputDir == "" {
		outputDir = filepath.Dir(inputPath)
	} else if !strings.HasSuffix(outputDir, audioExt) && !strings.HasSuffix(outputDir, ".json") && !strings.HasSuffix(outputDir, ".mp3") && !strings.HasSuffix(outputDir, ".pcm") && !strings.HasSuffix(outputDir, ".opus") {
		// --output points to a directory, not a file.
		// Create it if needed.
		if err := os.MkdirAll(outputDir, 0o755); err != nil {
			return fmt.Errorf("creating output directory %s: %w", outputDir, err)
		}
	}

	// If --output is a full file path (ends in audio ext or .json), derive timing path from it.
	var audioPath, timingPath string
	if strings.HasSuffix(outputDir, audioExt) || strings.HasSuffix(outputDir, ".mp3") || strings.HasSuffix(outputDir, ".pcm") || strings.HasSuffix(outputDir, ".opus") {
		audioPath = outputDir
		// Strip the extension and add -timing-map.json.
		base := strings.TrimSuffix(outputDir, audioExt)
		if base == outputDir {
			// audioExt didn't match, try the others
			base = strings.TrimSuffix(outputDir, ".mp3")
			if base == outputDir {
				base = strings.TrimSuffix(outputDir, ".pcm")
			}
			if base == outputDir {
				base = strings.TrimSuffix(outputDir, ".opus")
			}
		}
		timingPath = base + "-timing-map.json"
	} else if strings.HasSuffix(outputDir, ".json") {
		timingPath = outputDir
		base := strings.TrimSuffix(outputDir, "-timing-map.json")
		if base == outputDir {
			base = strings.TrimSuffix(outputDir, ".json")
		}
		audioPath = base + audioExt
	} else {
		// outputDir is a directory. Derive names from input base.
		base := filepath.Base(inputPath)
		baseName := strings.TrimSuffix(base, filepath.Ext(base))
		audioPath = filepath.Join(outputDir, baseName+"-voiceover"+audioExt)
		timingPath = filepath.Join(outputDir, baseName+"-timing-map.json")
	}

	// Write audio.
	if err := os.WriteFile(audioPath, audioBytes, 0o644); err != nil {
		return fmt.Errorf("writing %s: %w", audioPath, err)
	}
	fmt.Printf("audio → %s (%d bytes)\n", audioPath, len(audioBytes))

	// Build and write timing map.
	tm := buildTimingMap(audioPath, alignment)
	filterSSMLArtifacts(tm)
	timingBytes, err := json.MarshalIndent(tm, "", "  ")
	if err != nil {
		return fmt.Errorf("marshalling timing map: %w", err)
	}
	if err := os.WriteFile(timingPath, timingBytes, 0o644); err != nil {
		return fmt.Errorf("writing %s: %w", timingPath, err)
	}
	fmt.Printf("timing → %s\n", timingPath)

	return nil
}

// buildTimingMap constructs a TimingMap from alignment data.
func buildTimingMap(audioFile string, alignment *api.Alignment) *api.TimingMap {
	tm := &api.TimingMap{
		AudioFile:       filepath.Base(audioFile),
		DurationSeconds: 0,
		Words:           []api.Word{},
		Sentences:       []api.Sentence{},
	}

	if alignment == nil || len(alignment.Characters) == 0 {
		return tm
	}

	// Compute duration from last character end time.
	if len(alignment.CharacterEndTimes) > 0 {
		tm.DurationSeconds = alignment.CharacterEndTimes[len(alignment.CharacterEndTimes)-1]
	}

	// Build words from characters and timing.
	tm.Words = api.BuildWords(alignment)

	// Build sentences: assume periods/question marks/exclamation marks end sentences.
	var currentSentence strings.Builder
	sentenceStart := 0.0
	firstCharInSentence := true

	for i, char := range alignment.Characters {
		if char == " " || char == "\n" || char == "\t" {
			if currentSentence.Len() > 0 {
				currentSentence.WriteString(char)
			}
			continue
		}

		if firstCharInSentence {
			sentenceStart = alignment.CharacterStartTimes[i]
			firstCharInSentence = false
		}

		currentSentence.WriteString(char)

		// Check for sentence-ending punctuation.
		if char == "." || char == "?" || char == "!" {
			sentence := strings.TrimSpace(currentSentence.String())
			tm.Sentences = append(tm.Sentences, api.Sentence{
				Text:  sentence,
				Start: sentenceStart,
				End:   alignment.CharacterEndTimes[i],
			})
			currentSentence.Reset()
			firstCharInSentence = true
		}
	}

	// Flush final sentence if any.
	if currentSentence.Len() > 0 {
		sentence := strings.TrimSpace(currentSentence.String())
		tm.Sentences = append(tm.Sentences, api.Sentence{
			Text:  sentence,
			Start: sentenceStart,
			End:   alignment.CharacterEndTimes[len(alignment.CharacterEndTimes)-1],
		})
	}

	// Extract [pause] positions from words.
	pauseIdx := 0
	for _, w := range tm.Words {
		if w.Word == "[pause]" {
			tm.Pauses = append(tm.Pauses, api.Pause{
				Index: pauseIdx,
				Start: w.Start,
				End:   w.End,
			})
			pauseIdx++
		}
	}

	return tm
}

// filterSSMLArtifacts strips SSML break-tag artifacts from words and sentences
// for backward compatibility with old-format files that contain <break> tags.
func filterSSMLArtifacts(tm *api.TimingMap) {
	filtered := tm.Words[:0]
	for _, w := range tm.Words {
		if strings.HasPrefix(w.Word, "<break") || strings.HasPrefix(w.Word, "time=") {
			continue
		}
		filtered = append(filtered, w)
	}
	tm.Words = filtered

	filteredSentences := tm.Sentences[:0]
	for _, s := range tm.Sentences {
		if strings.HasPrefix(s.Text, "<break") || strings.HasPrefix(s.Text, "time=") {
			continue
		}
		filteredSentences = append(filteredSentences, s)
	}
	tm.Sentences = filteredSentences
}
