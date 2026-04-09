package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-memory/internal/cmd"
)

const version = "0.1.0"

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
		fmt.Printf("memory version %s\n", version)
		return nil
	}

	subcommand := args[0]
	remaining := args[1:]
	jsonOutput := false
	filtered := make([]string, 0, len(remaining))
	for _, arg := range remaining {
		if arg == "--json" {
			jsonOutput = true
			continue
		}
		filtered = append(filtered, arg)
	}

	switch subcommand {
	case "add":
		return cmd.AddCmd(filtered, jsonOutput)
	case "remember":
		return cmd.RememberCmd(filtered, jsonOutput)
	case "find":
		return cmd.FindCmd(filtered, jsonOutput)
	case "state":
		return cmd.StateCmd(filtered, jsonOutput)
	case "log":
		return cmd.LogCmd(filtered, jsonOutput)
	case "rebuild":
		return cmd.RebuildCmd(jsonOutput)
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	case "trust":
		return cmd.TrustCmd(filtered, jsonOutput)
	case "query":
		if len(filtered) == 0 {
			filtered = []string{"status"}
		}
		return cmd.QueryCmd(filtered, jsonOutput)
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'memory --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Printf(`memory - compiled symbolic memory for agents (v%s)

USAGE:
    memory <command> [options]

COMMANDS:
    add         Append a free-form memory entry
    remember    Update direct entity state with slot assignments
    find        Search the compiled symbolic index
    state       Show current slot state for an entity
    log         Show recent append-only entries
    rebuild     Rebuild the compiled snapshot from the log
    trust       Record a trust signal for an entry (e.g. helpful, unhelpful)
    doctor      Validate paths and report stats
    query       Dashboard protocol: status, items, actions

GLOBAL OPTIONS:
    --json      Emit machine-readable JSON
    --help, -h  Show this help
    --version   Show version

ENVIRONMENT:
    MEMORY_HOME                Override data directory (default: ~/.memory)
    MEMORY_STORE               Logical store name (default: default)
    MEMORY_DECAY_HALF_LIFE_DAYS  Temporal decay half-life in days (default: 90)

EXAMPLES:
    memory add "Atlas deploy owner is Alice" --entity Atlas --slot owner=Alice
    memory remember Alice --slot employer=OpenAI --slot role=Engineer
    memory find "role=engineer"
    memory state Alice
    memory trust 42 helpful
    memory trust 42 unhelpful
    memory query status --json
`, version)
}
