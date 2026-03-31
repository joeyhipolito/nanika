// Package main implements the elevenlabs binary.
package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-elevenlabs/internal/cmd"
)

const version = "0.2.0"

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	args := os.Args[1:]

	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}

	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("elevenlabs version %s\n", version)
		return nil
	}

	subcommand := args[0]
	remaining := args[1:]

	// Hoist global flags before dispatch.
	jsonOutput := false
	var filteredArgs []string
	for _, arg := range remaining {
		if arg == "--json" {
			jsonOutput = true
		} else {
			filteredArgs = append(filteredArgs, arg)
		}
	}

	switch subcommand {
	case "configure":
		if len(filteredArgs) > 0 && filteredArgs[0] == "show" {
			return cmd.ConfigureShowCmd(jsonOutput)
		}
		return cmd.ConfigureCmd()
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	case "voices":
		return cmd.VoicesCmd(jsonOutput)
	case "format":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("format requires a <narration-script.md> argument")
		}
		return cmd.FormatCmd(filteredArgs)
	case "generate":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("generate requires a <elevenlabs.txt> argument")
		}
		return cmd.GenerateCmd(filteredArgs)
	case "timing":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("timing requires a <timing-map.json> argument")
		}
		return cmd.TimingCmd(filteredArgs, jsonOutput)
	case "assemble":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("assemble requires a <voiceover.mp3> argument")
		}
		return cmd.AssembleCmd(filteredArgs)
	case "align":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("align requires <audio> <transcript.txt> arguments")
		}
		return cmd.AlignCmd(filteredArgs)
	case "transcribe":
		return cmd.TranscribeCmd(filteredArgs)
	case "query":
		sub := "status"
		if len(filteredArgs) > 0 {
			sub = filteredArgs[0]
		}
		return cmd.QueryCmd(sub, jsonOutput)
	default:
		return fmt.Errorf("unknown command %q — run 'elevenlabs --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Print(`elevenlabs — ElevenLabs TTS CLI for voiceover generation and timing maps

USAGE
  elevenlabs <command> [flags]

COMMANDS
  configure           Set API key and default voice interactively
  configure show      Display current config (API key masked)
  doctor              Verify API key, quota, and connectivity
  voices              List available voices
  format <script.md>  Format narration script for ElevenLabs TTS
  generate <text.txt> Generate voiceover audio + timing map
  timing <map.json>   Produce clip alignment guide from timing map
  assemble <audio>    Splice silence gaps into voiceover using timing + manifest
  align <audio> <transcript.txt>  Run forced alignment and produce clip windows JSON
  transcribe --input <file>        Transcribe audio to text (--provider whisper|scribe)

FLAGS
  --json              Output as JSON where supported
  --version, -v       Print version and exit
  --help, -h          Show this help

EXAMPLES
  elevenlabs configure
  elevenlabs doctor
  elevenlabs voices --json
  elevenlabs format narration-script.md --output narration-elevenlabs.txt
  elevenlabs generate narration-elevenlabs.txt --voice pNInz6obpgDQGcFmaJgB
  elevenlabs timing timing-map.json
`)
}
