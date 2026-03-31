package cmd

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/api"
	"github.com/joeyhipolito/nanika-elevenlabs/internal/formatter"
)

// AssembleCmd reads a voiceover audio file, timing map, and clip manifest, then uses
// ffmpeg to splice silence gaps into the audio at [pause] boundaries.
//
// Usage: elevenlabs assemble <voiceover.mp3|voiceover.pcm> [--timing <timing-map.json>] [--manifest <clip-manifest.json>] [--output <dir>] [--mp3] [--sample-rate <rate>] [--channels <count>]
func AssembleCmd(args []string) error {
	fs := flag.NewFlagSet("assemble", flag.ContinueOnError)
	timingPath := fs.String("timing", "", "timing map JSON (default: derived from audio path)")
	manifestPath := fs.String("manifest", "", "clip manifest JSON (default: derived from audio path)")
	outputDir := fs.String("output", "", "output directory (default: same as audio)")
	mp3Flag := fs.Bool("mp3", false, "re-encode assembled WAV to MP3")
	sampleRateFlag := fs.Int("sample-rate", 0, "sample rate for PCM input (e.g. 44100, 48000)")
	channelsFlag := fs.Int("channels", 0, "channel count for PCM input (e.g. 1 for mono, 2 for stereo)")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: elevenlabs assemble <voiceover.mp3|voiceover.pcm> [--timing <path>] [--manifest <path>] [--output <dir>] [--mp3] [--sample-rate <rate>] [--channels <count>]\n\n")
		fmt.Fprintf(os.Stderr, "Splices silence gaps into voiceover audio using timing map and clip manifest.\n\n")
		fs.PrintDefaults()
	}

	// Pre-split args to allow flags after positional argument.
	flagArgs, positional := splitArgs(args)
	if err := fs.Parse(flagArgs); err != nil {
		return err
	}
	if len(positional) == 0 {
		fs.Usage()
		return fmt.Errorf("missing required argument: <voiceover.mp3>")
	}
	audioPath := positional[0]

	// Check ffmpeg and ffprobe are available.
	if _, err := exec.LookPath("ffprobe"); err != nil {
		return fmt.Errorf("ffprobe not found in PATH. Install: brew install ffmpeg")
	}
	if _, err := exec.LookPath("ffmpeg"); err != nil {
		return fmt.Errorf("ffmpeg not found in PATH. Install: brew install ffmpeg")
	}

	// Derive timing and manifest paths if not specified.
	if *timingPath == "" {
		*timingPath = deriveTimingPath(audioPath)
	}
	if *manifestPath == "" {
		*manifestPath = deriveManifestFromAudio(audioPath)
	}

	// Determine output directory.
	outDir := *outputDir
	if outDir == "" {
		outDir = filepath.Dir(audioPath)
	} else {
		if err := os.MkdirAll(outDir, 0o755); err != nil {
			return fmt.Errorf("creating output directory %s: %w", outDir, err)
		}
	}

	// Load timing map.
	tmBytes, err := os.ReadFile(*timingPath)
	if err != nil {
		return fmt.Errorf("reading timing map %s: %w", *timingPath, err)
	}
	var tm api.TimingMap
	if err := json.Unmarshal(tmBytes, &tm); err != nil {
		return fmt.Errorf("parsing timing map %s: %w", *timingPath, err)
	}

	// Load clip manifest.
	mfBytes, err := os.ReadFile(*manifestPath)
	if err != nil {
		return fmt.Errorf("reading manifest %s: %w", *manifestPath, err)
	}
	var manifest formatter.ClipManifest
	if err := json.Unmarshal(mfBytes, &manifest); err != nil {
		return fmt.Errorf("parsing manifest %s: %w", *manifestPath, err)
	}

	// Validate: pause count must equal segments - 1.
	expectedPauses := len(manifest.Segments) - 1
	if expectedPauses < 0 {
		expectedPauses = 0
	}
	if len(tm.Pauses) != expectedPauses {
		return fmt.Errorf("timing map has %d pauses but manifest has %d segments (expected %d pauses = segments-1). Re-run format → generate to re-sync",
			len(tm.Pauses), len(manifest.Segments), expectedPauses)
	}

	// Determine if audio is PCM.
	isPCM := strings.HasSuffix(audioPath, ".pcm")

	// Probe source audio or use provided parameters for PCM.
	var sampleRate, channels int
	if isPCM {
		if *sampleRateFlag <= 0 || *channelsFlag <= 0 {
			// Try to probe a reference non-PCM file, or use defaults.
			sr, ch, err := probeAudioForReference(audioPath)
			if err != nil || sr == 0 || ch == 0 {
				return fmt.Errorf("PCM input requires --sample-rate and --channels flags (ffprobe cannot read raw PCM headers)")
			}
			sampleRate = sr
			channels = ch
		} else {
			sampleRate = *sampleRateFlag
			channels = *channelsFlag
		}
	} else {
		var err error
		sampleRate, channels, err = probeAudio(audioPath)
		if err != nil {
			return err
		}
	}
	channelLayout := channelLayoutString(channels)

	// Build the part list: alternating audio segments and silence gaps.
	parts := buildParts(&tm, &manifest)

	// Build ffmpeg filter_complex string.
	filterComplex := buildFilterComplex(parts, sampleRate, channelLayout)

	// Execute ffmpeg.
	wavPath := filepath.Join(outDir, "voiceover-assembled.wav")
	fmt.Println("Assembling audio...")
	ffmpegArgs := []string{"-y"}

	// Add input format flags for PCM.
	if isPCM {
		ffmpegArgs = append(ffmpegArgs,
			"-f", "s16le",
			"-ar", strconv.Itoa(sampleRate),
			"-ac", strconv.Itoa(channels),
		)
	}

	ffmpegArgs = append(ffmpegArgs,
		"-i", audioPath,
		"-filter_complex", filterComplex,
		"-map", "[outa]",
		wavPath,
	)
	ffCmd := exec.Command("ffmpeg", ffmpegArgs...)
	ffCmd.Stderr = os.Stderr
	if err := ffCmd.Run(); err != nil {
		return fmt.Errorf("ffmpeg assembly failed: %w", err)
	}
	fmt.Printf("audio → %s\n", wavPath)

	// Compute durations for timeline and summary.
	assembledDuration := computeTotalDuration(parts)
	expectedDuration := float64(manifest.TotalDurationSeconds)
	delta := assembledDuration - expectedDuration

	// Build and write timeline.
	timeline := buildTimeline(parts, &manifest, wavPath, assembledDuration, expectedDuration)
	tlBytes, err := json.MarshalIndent(timeline, "", "  ")
	if err != nil {
		return fmt.Errorf("marshalling timeline: %w", err)
	}
	tlPath := filepath.Join(outDir, "voiceover-timeline.json")
	if err := os.WriteFile(tlPath, tlBytes, 0o644); err != nil {
		return fmt.Errorf("writing timeline %s: %w", tlPath, err)
	}
	fmt.Printf("timeline → %s\n", tlPath)

	// Optional MP3 re-encode.
	finalPath := wavPath
	if *mp3Flag {
		mp3Path := strings.TrimSuffix(wavPath, ".wav") + ".mp3"
		mp3Cmd := exec.Command("ffmpeg", "-y",
			"-i", wavPath,
			"-codec:a", "libmp3lame", "-q:a", "2",
			mp3Path,
		)
		mp3Cmd.Stderr = os.Stderr
		if err := mp3Cmd.Run(); err != nil {
			return fmt.Errorf("mp3 encoding failed: %w", err)
		}
		finalPath = mp3Path
		fmt.Printf("mp3 → %s\n", mp3Path)
	}

	// Summary.
	fmt.Printf("\nassembled: %.1fs (expected: %.1fs, delta: %+.1fs) → %s\n",
		assembledDuration, expectedDuration, delta, filepath.Base(finalPath))

	return nil
}

// deriveTimingPath converts a voiceover audio path to its timing map path.
// narration-elevenlabs-voiceover.mp3 → narration-elevenlabs-timing-map.json
func deriveTimingPath(audioPath string) string {
	dir := filepath.Dir(audioPath)
	base := filepath.Base(audioPath)
	name := strings.TrimSuffix(base, filepath.Ext(base))
	name = strings.TrimSuffix(name, "-voiceover")
	return filepath.Join(dir, name+"-timing-map.json")
}

// deriveManifestFromAudio converts a voiceover audio path to its clip manifest path.
// narration-elevenlabs-voiceover.mp3 → narration-clip-manifest.json
func deriveManifestFromAudio(audioPath string) string {
	dir := filepath.Dir(audioPath)
	base := filepath.Base(audioPath)
	name := strings.TrimSuffix(base, filepath.Ext(base))
	name = strings.TrimSuffix(name, "-voiceover")
	// Strip -elevenlabs suffix to get the base prefix.
	name = strings.TrimSuffix(name, "-elevenlabs")
	return filepath.Join(dir, name+"-clip-manifest.json")
}

// probeAudio runs ffprobe to extract sample rate and channel count.
func probeAudio(path string) (sampleRate int, channels int, err error) {
	cmd := exec.Command("ffprobe", "-v", "quiet",
		"-print_format", "json", "-show_streams", "-select_streams", "a:0", path)
	out, err := cmd.Output()
	if err != nil {
		return 0, 0, fmt.Errorf("probing %s: %w", path, err)
	}
	var result struct {
		Streams []struct {
			SampleRate string `json:"sample_rate"`
			Channels   int    `json:"channels"`
		} `json:"streams"`
	}
	if err := json.Unmarshal(out, &result); err != nil {
		return 0, 0, fmt.Errorf("parsing ffprobe output: %w", err)
	}
	if len(result.Streams) == 0 {
		return 0, 0, fmt.Errorf("no audio streams in %s", path)
	}
	sr, err := strconv.Atoi(result.Streams[0].SampleRate)
	if err != nil {
		return 0, 0, fmt.Errorf("parsing sample rate %q: %w", result.Streams[0].SampleRate, err)
	}
	return sr, result.Streams[0].Channels, nil
}

// probeAudioForReference attempts to probe audio for reference when PCM headers are unavailable.
// Used as a fallback when --sample-rate/--channels not provided for PCM input.
// Returns 0,0 if probing fails, so caller can decide whether to error or continue.
func probeAudioForReference(path string) (sampleRate int, channels int, err error) {
	// For PCM files, we can't probe them directly, so we return 0,0.
	// The caller should use --sample-rate and --channels flags instead.
	return 0, 0, nil
}

// channelLayoutString maps a channel count to an ffmpeg channel_layout name.
func channelLayoutString(channels int) string {
	if channels >= 2 {
		return "stereo"
	}
	return "mono"
}

// assemblePart is either an audio segment or a silence gap.
type assemblePart struct {
	isSilence bool
	start     float64 // source audio start (speech only)
	end       float64 // source audio end (speech only)
	duration  float64 // silence duration (silence only)
}

// buildParts creates the ordered list of audio segments and silence gaps
// based on the timing map pauses and manifest silence requirements.
func buildParts(tm *api.TimingMap, manifest *formatter.ClipManifest) []assemblePart {
	var parts []assemblePart
	cursor := 0.0

	for i := 0; i < len(tm.Pauses); i++ {
		pause := tm.Pauses[i]
		silenceAfter := manifest.Segments[i].SilenceAfter

		if silenceAfter > 0 {
			// Cut audio from cursor to pause start.
			if pause.Start > cursor {
				parts = append(parts, assemblePart{start: cursor, end: pause.Start})
			}
			// Insert silence.
			parts = append(parts, assemblePart{isSilence: true, duration: float64(silenceAfter)})
			// Advance cursor past the pause.
			cursor = pause.End
		}
		// If silenceAfter == 0: audio flows through naturally, no cut.
	}

	// Final segment: cursor to end of audio.
	if tm.DurationSeconds > cursor {
		parts = append(parts, assemblePart{start: cursor, end: tm.DurationSeconds})
	}

	// Trailing silence from last segment's silence_after.
	if len(manifest.Segments) > 0 {
		lastSilence := manifest.Segments[len(manifest.Segments)-1].SilenceAfter
		if lastSilence > 0 {
			parts = append(parts, assemblePart{isSilence: true, duration: float64(lastSilence)})
		}
	}

	return parts
}

// buildFilterComplex generates the ffmpeg filter_complex string for the assembly.
func buildFilterComplex(parts []assemblePart, sampleRate int, channelLayout string) string {
	var filters []string
	var labels []string
	segIdx := 0
	silIdx := 0

	for _, p := range parts {
		if p.isSilence {
			label := fmt.Sprintf("[sil%d]", silIdx)
			filters = append(filters, fmt.Sprintf(
				"anullsrc=channel_layout=%s:sample_rate=%d,atrim=0:%.3f,asetpts=PTS-STARTPTS%s",
				channelLayout, sampleRate, p.duration, label,
			))
			labels = append(labels, label)
			silIdx++
		} else {
			label := fmt.Sprintf("[seg%d]", segIdx)
			filters = append(filters, fmt.Sprintf(
				"[0:a]atrim=%.3f:%.3f,asetpts=PTS-STARTPTS%s",
				p.start, p.end, label,
			))
			labels = append(labels, label)
			segIdx++
		}
	}

	// Concat all labeled streams.
	concat := strings.Join(labels, "") + fmt.Sprintf("concat=n=%d:v=0:a=1[outa]", len(labels))
	filters = append(filters, concat)

	return strings.Join(filters, "; ")
}

// computeTotalDuration sums the durations of all parts.
func computeTotalDuration(parts []assemblePart) float64 {
	total := 0.0
	for _, p := range parts {
		if p.isSilence {
			total += p.duration
		} else {
			total += p.end - p.start
		}
	}
	return total
}

// timelineJSON is the output format for voiceover-timeline.json.
type timelineJSON struct {
	AudioFile              string            `json:"audio_file"`
	DurationSeconds        float64           `json:"duration_seconds"`
	ExpectedDurationSeconds float64          `json:"expected_duration_seconds"`
	DeltaSeconds           float64           `json:"delta_seconds"`
	ClipGridSeconds        int               `json:"clip_grid_seconds"`
	Segments               []timelineSegment `json:"segments"`
}

type timelineSegment struct {
	Index int      `json:"index"`
	Type  string   `json:"type"`
	Start float64  `json:"start"`
	End   float64  `json:"end"`
	Clips []int    `json:"clips,omitempty"`
}

// buildTimeline produces the voiceover-timeline.json structure with absolute
// timestamps for each speech and silence segment.
func buildTimeline(parts []assemblePart, manifest *formatter.ClipManifest, audioFile string, assembled, expected float64) *timelineJSON {
	tl := &timelineJSON{
		AudioFile:               filepath.Base(audioFile),
		DurationSeconds:         assembled,
		ExpectedDurationSeconds: expected,
		DeltaSeconds:            assembled - expected,
		ClipGridSeconds:         manifest.ClipGridSeconds,
	}

	cursor := 0.0
	segmentIndex := 0
	speechIdx := 0 // tracks which manifest segment we're on

	for _, p := range parts {
		dur := p.duration
		if !p.isSilence {
			dur = p.end - p.start
		}

		seg := timelineSegment{
			Index: segmentIndex,
			Start: cursor,
			End:   cursor + dur,
		}

		if p.isSilence {
			seg.Type = "silence"
			// Compute which clips this silence covers.
			if speechIdx > 0 && speechIdx-1 < len(manifest.Segments) {
				ms := manifest.Segments[speechIdx-1]
				silClips := ms.SilenceAfter / manifest.ClipGridSeconds
				for c := 0; c < silClips; c++ {
					seg.Clips = append(seg.Clips, ms.Clip+1+c)
				}
			}
		} else {
			seg.Type = "speech"
			// Compute which clips this speech covers.
			if speechIdx < len(manifest.Segments) {
				seg.Clips = []int{manifest.Segments[speechIdx].Clip}
			}
			speechIdx++
		}

		tl.Segments = append(tl.Segments, seg)
		cursor += dur
		segmentIndex++
	}

	return tl
}
