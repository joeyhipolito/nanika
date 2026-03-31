package cmd

import (
	"encoding/json"
	"fmt"
	"strconv"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// UnpublishCmd handles the unpublish command.
func UnpublishCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	if len(args) == 0 {
		return fmt.Errorf("usage: substack unpublish <post-id> [<post-id>...]")
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	for _, arg := range args {
		id, err := strconv.Atoi(arg)
		if err != nil {
			fmt.Printf("Skipping invalid ID: %s\n", arg)
			continue
		}

		// Fetch post to confirm it exists and get title
		post, err := client.GetDraft(id)
		if err != nil {
			fmt.Printf("Skipping %d: %v\n", id, err)
			continue
		}

		if err := client.UnpublishPost(id); err != nil {
			if jsonOutput {
				result, _ := json.Marshal(map[string]interface{}{
					"id":          id,
					"title":       post.Title,
					"unpublished": false,
					"error":       err.Error(),
				})
				fmt.Println(string(result))
			} else {
				fmt.Printf("Failed to unpublish %d (%s): %v\n", id, post.Title, err)
			}
			continue
		}

		if jsonOutput {
			result, _ := json.Marshal(map[string]interface{}{
				"id":          id,
				"title":       post.Title,
				"unpublished": true,
			})
			fmt.Println(string(result))
		} else {
			fmt.Printf("Unpublished: %d — %s\n", id, post.Title)
		}
	}

	return nil
}
