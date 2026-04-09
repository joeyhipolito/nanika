package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-memory/internal/output"
	"github.com/joeyhipolito/nanika-memory/internal/store"
)

type addResponse struct {
	OK    bool               `json:"ok"`
	Entry store.Entry        `json:"entry"`
	State *store.EntityState `json:"state,omitempty"`
}

// AddCmd appends a free-form memory entry.
func AddCmd(args []string, jsonOutput bool) error {
	if len(args) > 0 && (args[0] == "--help" || args[0] == "-h") {
		fmt.Print(`Usage: memory add <text> [--entity <name>] [--slot <key=value>] [--tag <key=value>] [--source <name>] [--json]
       echo "deployment note" | memory add --entity Atlas

Options:
  --entity <name>     Attach a canonical entity to this entry
  --slot <key=value>  Add or update entity state (requires --entity)
  --tag <key=value>   Add an indexed tag
  --source <name>     Record the origin of this memory
  --json              Output machine-readable JSON
`)
		return nil
	}

	var (
		entity   string
		source   string
		slotArgs []string
		tagArgs  []string
		bodyArgs []string
	)
	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--entity":
			i++
			if i >= len(args) {
				return fmt.Errorf("--entity requires a value")
			}
			entity = args[i]
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
		case "--source":
			i++
			if i >= len(args) {
				return fmt.Errorf("--source requires a value")
			}
			source = args[i]
		default:
			bodyArgs = append(bodyArgs, args[i])
		}
	}

	text, err := readTextInput(bodyArgs)
	if err != nil {
		return err
	}
	slots, err := parseKVArgs(slotArgs)
	if err != nil {
		return err
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

	var statePtr *store.EntityState
	if entry.Entity != "" {
		stateCopy := state
		statePtr = &stateCopy
	}

	if jsonOutput {
		return output.JSON(addResponse{
			OK:    true,
			Entry: entry,
			State: statePtr,
		})
	}

	fmt.Printf("stored entry %d\n", entry.ID)
	if entry.Entity != "" {
		fmt.Printf("entity: %s\n", entry.Entity)
	}
	if entry.Source != "" {
		fmt.Printf("source: %s\n", entry.Source)
	}
	fmt.Printf("text: %s\n", entry.Text)
	return nil
}
