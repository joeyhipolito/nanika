package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// TagsCmd dispatches tag subcommands: list (default), create, delete.
func TagsCmd(args []string, jsonOutput bool) error {
	// Default: no subcommand → list tags
	if len(args) == 0 {
		return tagsListCmd(jsonOutput)
	}

	subcmd := args[0]
	switch subcmd {
	case "create":
		if len(args) < 2 {
			return fmt.Errorf("usage: substack tags create <name>")
		}
		name := strings.TrimSpace(args[1])
		if name == "" {
			return fmt.Errorf("tag name cannot be empty")
		}
		return tagsCreateCmd(name, jsonOutput)
	case "delete":
		if len(args) < 2 {
			return fmt.Errorf("usage: substack tags delete <name-or-id>")
		}
		nameOrID := strings.TrimSpace(args[1])
		if nameOrID == "" {
			return fmt.Errorf("tag name or ID cannot be empty")
		}
		return tagsDeleteCmd(nameOrID)
	default:
		return fmt.Errorf("unknown tags subcommand %q\n\nUsage:\n  substack tags              List tags\n  substack tags create <n>   Create a tag\n  substack tags delete <n>   Delete a tag by name or ID", subcmd)
	}
}

func tagsListCmd(jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	tags, err := client.GetPublicationTags()
	if err != nil {
		return fmt.Errorf("fetching tags: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(tags)
	}

	if len(tags) == 0 {
		fmt.Println("No tags found.")
		fmt.Println("Create tags with: substack tags create \"My Tag\"")
		return nil
	}

	fmt.Printf("Tags (%d):\n\n", len(tags))
	for _, t := range tags {
		line := fmt.Sprintf("  %s", t.Name)
		if t.Slug != "" {
			line += fmt.Sprintf(" (slug: %s)", t.Slug)
		}
		if t.PostCount > 0 {
			line += fmt.Sprintf(" [%d posts]", t.PostCount)
		}
		fmt.Println(line)
	}

	return nil
}

func tagsCreateCmd(name string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	tag, err := client.CreatePublicationTag(name)
	if err != nil {
		return fmt.Errorf("creating tag: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(tag)
	}

	fmt.Printf("Created tag: %s (id: %s)\n", tag.Name, tag.ID)
	return nil
}

func tagsDeleteCmd(nameOrID string) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	// Fetch existing tags to resolve name → UUID
	tags, err := client.GetPublicationTags()
	if err != nil {
		return fmt.Errorf("fetching tags: %w", err)
	}

	// Try exact ID match first, then case-insensitive name match
	var targetID string
	var targetName string
	for _, t := range tags {
		if t.ID == nameOrID {
			targetID = t.ID
			targetName = t.Name
			break
		}
	}
	if targetID == "" {
		lower := api.ToLower(nameOrID)
		for _, t := range tags {
			if api.ToLower(t.Name) == lower {
				targetID = t.ID
				targetName = t.Name
				break
			}
		}
	}

	if targetID == "" {
		return fmt.Errorf("tag %q not found", nameOrID)
	}

	if err := client.DeletePublicationTag(targetID); err != nil {
		return fmt.Errorf("deleting tag %q: %w", targetName, err)
	}

	fmt.Printf("Deleted tag: %s\n", targetName)
	return nil
}
