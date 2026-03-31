package cmd

import (
	"encoding/json"
	"flag"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/formatter"
)

// FormatCmd reads a narration script markdown file and writes ElevenLabs-formatted text.
//
// Usage: elevenlabs format <narration-script.md> [--output <path>]
//
// Default output: same directory as input, with the base name mapped to
// <name>-elevenlabs.txt (e.g., narration-script.md → narration-elevenlabs.txt).
func FormatCmd(args []string) error {
	fs := flag.NewFlagSet("format", flag.ContinueOnError)
	outputPath := fs.String("output", "", "path for formatted output (default: <input-dir>/<name>-elevenlabs.txt)")
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, "Usage: elevenlabs format <narration-script.md> [--output <path>]\n\n")
		fmt.Fprintf(os.Stderr, "Parses a narration script and formats it for ElevenLabs TTS:\n")
		fmt.Fprintf(os.Stderr, "  - Strips [SCENE] blocks, section headers, and metadata\n")
		fmt.Fprintf(os.Stderr, "  - Adds v3 audio tags ([documentary style], [pause], [long pause])\n")
		fmt.Fprintf(os.Stderr, "  - Normalizes numbers and abbreviations\n\n")
		fs.PrintDefaults()
	}

	// Pre-split args to allow flags after positional argument.
	flagArgs, positional := splitArgs(args)
	if err := fs.Parse(flagArgs); err != nil {
		return err
	}
	if len(positional) == 0 {
		fs.Usage()
		return fmt.Errorf("missing required argument: <narration-script.md>")
	}
	inputPath := positional[0]

	src, err := os.ReadFile(inputPath)
	if err != nil {
		return fmt.Errorf("reading %s: %w", inputPath, err)
	}

	out, manifest, err := formatter.Format(src)
	if err != nil {
		return fmt.Errorf("formatting %s: %w", inputPath, err)
	}

	dest := *outputPath
	if dest == "" {
		dest = deriveOutputPath(inputPath)
	}

	if err := os.WriteFile(dest, out, 0o644); err != nil {
		return fmt.Errorf("writing %s: %w", dest, err)
	}

	if manifest != nil {
		fmt.Printf("format → %s (%d segments, %d pauses)\n",
			dest, len(manifest.Segments), max(len(manifest.Segments)-1, 0))

		manifestPath := deriveManifestPath(dest)
		manifestJSON, err := json.MarshalIndent(manifest, "", "  ")
		if err != nil {
			return fmt.Errorf("marshaling manifest: %w", err)
		}
		manifestJSON = append(manifestJSON, '\n')
		if err := os.WriteFile(manifestPath, manifestJSON, 0o644); err != nil {
			return fmt.Errorf("writing %s: %w", manifestPath, err)
		}
		fmt.Printf("manifest → %s (%d clips: %d narrated, %d visual-only)\n",
			manifestPath, manifest.TotalClips, manifest.NarratedClips, manifest.VisualOnlyClips)
	} else {
		fmt.Printf("format → %s\n", dest)
	}

	return nil
}

// deriveManifestPath maps the TTS output path to the manifest sidecar path.
// narration-elevenlabs.txt → narration-clip-manifest.json
func deriveManifestPath(ttsPath string) string {
	dir := filepath.Dir(ttsPath)
	base := filepath.Base(ttsPath)
	name := strings.TrimSuffix(base, filepath.Ext(base))
	name = strings.TrimSuffix(name, "-elevenlabs")
	return filepath.Join(dir, name+"-clip-manifest.json")
}

// deriveOutputPath maps input path to a default output path.
// narration-script.md → narration-elevenlabs.txt
// my-script.md        → my-elevenlabs.txt
// notes.txt           → notes-elevenlabs.txt
func deriveOutputPath(input string) string {
	dir := filepath.Dir(input)
	base := filepath.Base(input)

	// Strip known suffixes.
	trimmed := base
	for _, suffix := range []string{"-script.md", ".md", ".txt"} {
		if strings.HasSuffix(base, suffix) {
			trimmed = strings.TrimSuffix(base, suffix)
			break
		}
	}

	return filepath.Join(dir, trimmed+"-elevenlabs.txt")
}
