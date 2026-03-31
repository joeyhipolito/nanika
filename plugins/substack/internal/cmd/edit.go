package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"strconv"
	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
)

// EditCmd handles the edit command — updates content of an existing post/draft.
func EditCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags
	var postIDStr string
	var filePath string
	var titleOverride string
	var subtitleOverride string
	var manifestPath string
	dryRun := false

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--title":
			if i+1 < len(args) {
				i++
				titleOverride = args[i]
			} else {
				return fmt.Errorf("--title requires a value")
			}
		case "--subtitle":
			if i+1 < len(args) {
				i++
				subtitleOverride = args[i]
			} else {
				return fmt.Errorf("--subtitle requires a value")
			}
		case "--manifest":
			if i+1 < len(args) {
				i++
				manifestPath = args[i]
			} else {
				return fmt.Errorf("--manifest requires a path to manifest.json")
			}
		case "--dry-run":
			dryRun = true
		default:
			if postIDStr == "" {
				postIDStr = args[i]
			} else if filePath == "" {
				filePath = args[i]
			} else {
				return fmt.Errorf("unexpected argument: %s", args[i])
			}
		}
	}

	if postIDStr == "" || filePath == "" {
		return fmt.Errorf("usage: substack edit <post-id> <path-to-mdx-file> [--title ...] [--subtitle ...] [--dry-run]")
	}

	postID, err := strconv.Atoi(postIDStr)
	if err != nil {
		return fmt.Errorf("invalid post ID: %s", postIDStr)
	}

	// Read the MDX file
	content, err := os.ReadFile(filePath)
	if err != nil {
		return fmt.Errorf("reading file: %w", err)
	}

	// Create API client
	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	// Verify auth
	user, err := client.GetProfile()
	if err != nil {
		return fmt.Errorf("authenticating: %w", err)
	}

	// Fetch existing post to confirm it exists
	existing, err := client.GetDraft(postID)
	if err != nil {
		return fmt.Errorf("fetching post %d: %w", postID, err)
	}

	fmt.Printf("Editing: %s (ID: %d)\n", existing.Title, existing.ID)

	// Build image resolver
	var imgResolver ImageResolver
	var manifest *Manifest
	if manifestPath != "" {
		manifest, err = loadManifest(manifestPath)
		if err != nil {
			return fmt.Errorf("loading manifest: %w", err)
		}
		imgResolver = buildManifestImageResolver(manifest, client)
	} else {
		imgResolver = buildImageResolver(filePath, client)
	}

	// Parse frontmatter and convert body to Tiptap
	title, subtitle, tiptapBody, _, err := parseAndConvert(string(content), cfg.SiteURL, imgResolver)
	if err != nil {
		return fmt.Errorf("converting content: %w", err)
	}

	// Upload local images
	blogRoot := findBlogRoot(filePath)
	tiptapBody, err = uploadLocalImages(tiptapBody, blogRoot, client)
	if err != nil {
		return fmt.Errorf("uploading local images: %w", err)
	}

	// Upload manifest illustrations
	if manifest != nil {
		tiptapBody, err = uploadManifestIllustrations(tiptapBody, manifest, client)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to upload manifest illustrations: %v\n", err)
		}
	}

	// Apply overrides
	if titleOverride != "" {
		title = titleOverride
	}
	if subtitleOverride != "" {
		subtitle = subtitleOverride
	}

	if dryRun {
		fmt.Printf("Dry run — would update post %d:\n", postID)
		fmt.Printf("  Title:    %s\n", title)
		fmt.Printf("  Subtitle: %s\n", subtitle)
		fmt.Printf("  Body:     %d bytes of Tiptap JSON\n", len(tiptapBody))
		return nil
	}

	// Build update request
	updateReq := &api.DraftUpdateRequest{
		DraftTitle:    title,
		DraftSubtitle: subtitle,
		DraftBody:     tiptapBody,
		DraftBylines:  []api.DraftByline{{ID: user.ID}},
	}

	fmt.Println("Updating post...")
	updated, err := client.UpdateDraft(postID, updateReq)
	if err != nil {
		return fmt.Errorf("updating post: %w", err)
	}

	// For published posts, re-publish to promote draft_body → body (no email sent)
	if updated.IsPublished {
		fmt.Println("Re-publishing to apply changes...")
		updated, err = client.Publish(postID, false)
		if err != nil {
			return fmt.Errorf("re-publishing post: %w", err)
		}
	}

	// Determine URL
	var postURL string
	if updated.IsPublished {
		postURL = fmt.Sprintf("%s/p/%s", cfg.PublicationURL, updated.Slug)
	} else {
		postURL = fmt.Sprintf("%s/publish/post/%d", cfg.PublicationURL, updated.ID)
	}

	if jsonOutput {
		return outputEditJSON(updated, postURL)
	}

	status := "draft"
	if updated.IsPublished {
		status = "published"
	}
	fmt.Printf("Updated (%s): %s\n", status, postURL)
	return nil
}

func outputEditJSON(post *api.Post, url string) error {
	type editOutput struct {
		ID     int    `json:"id"`
		Title  string `json:"title"`
		URL    string `json:"url"`
		Status string `json:"status"`
	}
	status := "draft"
	if post.IsPublished {
		status = "published"
	}
	out := editOutput{
		ID:     post.ID,
		Title:  post.Title,
		URL:    url,
		Status: status,
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(out)
}
