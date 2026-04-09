package cmd

import (
	"fmt"
	"sort"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

// RememberCmd updates entity state with explicit slot assignments.
func RememberCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		fmt.Print(`Usage: memory remember <entity> --slot <key=value> [--slot <key=value>] [--text <note>] [--tag <key=value>] [--source <name>] [--json]

Examples:
  memory remember Alice --slot employer=OpenAI --slot role=Engineer
  memory remember Atlas --slot owner=Platform --text "Atlas rollout ownership confirmed"
`)
		return nil
	}

	entity := args[0]
	var (
		text     string
		source   string
		slotArgs []string
		tagArgs  []string
	)
	for i := 1; i < len(args); i++ {
		switch args[i] {
		case "--slot":
			i++
			if i >= len(args) {
				return fmt.Errorf("--slot requires key=value")
			}
			slotArgs = append(slotArgs, args[i])
		case "--tag":
			i++
			if i >= len(args) {
				return fmt.Errorf("--tag requires key=value")
			}
			tagArgs = append(tagArgs, args[i])
		case "--text":
			i++
			if i >= len(args) {
				return fmt.Errorf("--text requires a value")
			}
			text = args[i]
		case "--source":
			i++
			if i >= len(args) {
				return fmt.Errorf("--source requires a value")
			}
			source = args[i]
		default:
			return fmt.Errorf("unknown argument %q", args[i])
		}
	}

	slots, err := parseKVArgs(slotArgs)
	if err != nil {
		return err
	}
	if len(slots) == 0 {
		return fmt.Errorf("remember requires at least one --slot key=value")
	}
	tags, err := parseKVArgs(tagArgs)
	if err != nil {
		return err
	}
	engine, err := store.Open()
	if err != nil {
		return err
	}
	entry, state, err := engine.Add(store.AddInput{
		Text:   text,
		Entity: entity,
		Slots:  slots,
		Tags:   tags,
		Source: source,
	})
	if err != nil {
		return err
	}

	if jsonOutput {
		return output.JSON(addResponse{
			OK:    true,
			Entry: entry,
			State: &state,
		})
	}

	fmt.Printf("updated %s\n", state.Entity)
	keys := make([]string, 0, len(state.Slots))
	for key := range state.Slots {
		keys = append(keys, key)
	}
	sort.Strings(keys)
	for _, key := range keys {
		fmt.Printf("%s=%s\n", key, state.Slots[key])
	}
	return nil
}
