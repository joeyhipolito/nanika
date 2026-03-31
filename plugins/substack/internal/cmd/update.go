package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// UpdateCmd handles the update command.
// Usage: substack update <post-id> --tags "AI,Go" [--remove-tags "Test,Old"]
func UpdateCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags
	var postIDStr string
	var tagsFlag string
	var removeTagsFlag string

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--tags":
			if i+1 < len(args) {
				i++
				tagsFlag = args[i]
			} else {
				return fmt.Errorf("--tags requires a comma-separated list of tag names")
			}
		case "--remove-tags":
			if i+1 < len(args) {
				i++
				removeTagsFlag = args[i]
			} else {
				return fmt.Errorf("--remove-tags requires a comma-separated list of tag names")
			}
		default:
			if postIDStr == "" {
				postIDStr = args[i]
			} else {
				return fmt.Errorf("unexpected argument: %s", args[i])
			}
		}
	}

	if postIDStr == "" {
		return fmt.Errorf("usage: substack update <post-id> --tags \"AI,Go\" [--remove-tags \"Test\"]")
	}

	postID, err := strconv.Atoi(postIDStr)
	if err != nil {
		return fmt.Errorf("invalid post ID: %s", postIDStr)
	}

	if tagsFlag == "" && removeTagsFlag == "" {
		return fmt.Errorf("--tags or --remove-tags required")
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	// Remove tags first
	if removeTagsFlag != "" {
		removeNames := parseCSV(removeTagsFlag)
		if len(removeNames) > 0 {
			fmt.Printf("Removing %d tag(s) from post %d...\n", len(removeNames), postID)
			err := removeTagsByName(client, postID, removeNames)
			if err != nil {
				return fmt.Errorf("removing tags: %w", err)
			}
		}
	}

	// Add tags
	var resolved []api.PostTag
	if tagsFlag != "" {
		tagNames := parseCSV(tagsFlag)
		if len(tagNames) > 0 {
			fmt.Printf("Applying %d tag(s) to post %d...\n", len(tagNames), postID)
			resolved, err = client.EnsureAndAssignTags(postID, tagNames)
			if err != nil {
				return fmt.Errorf("applying tags: %w", err)
			}
		}
	}

	if jsonOutput {
		type updateOutput struct {
			ID   int           `json:"id"`
			Tags []api.PostTag `json:"tags,omitempty"`
		}
		out := updateOutput{ID: postID, Tags: resolved}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	if len(resolved) > 0 {
		fmt.Printf("Applied tags to post %d:\n", postID)
		for _, t := range resolved {
			fmt.Printf("  + %s\n", t.Name)
		}
	}

	return nil
}

// removeTagsByName looks up tag UUIDs by name from publication tags, then removes them from the post.
func removeTagsByName(client *api.Client, postID int, names []string) error {
	existing, err := client.GetPublicationTags()
	if err != nil {
		return fmt.Errorf("fetching tags: %w", err)
	}

	tagByName := make(map[string]api.PostTag)
	for _, t := range existing {
		tagByName[api.ToLower(t.Name)] = t
	}

	for _, name := range names {
		tag, ok := tagByName[api.ToLower(name)]
		if !ok {
			fmt.Fprintf(os.Stderr, "  Warning: tag %q not found, skipping\n", name)
			continue
		}
		if err := client.RemoveTagFromPost(postID, tag.ID); err != nil {
			fmt.Fprintf(os.Stderr, "  Warning: failed to remove tag %q: %v\n", name, err)
			continue
		}
		fmt.Printf("  - %s\n", tag.Name)
	}

	return nil
}

// parseCSV splits a comma-separated string into trimmed non-empty strings.
func parseCSV(s string) []string {
	var result []string
	parts := splitBy(s, ',')
	for _, p := range parts {
		p = trimSpace(p)
		if p != "" {
			result = append(result, p)
		}
	}
	return result
}
