package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/nanika-linkedin/internal/api"
	"github.com/joeyhipolito/nanika-linkedin/internal/config"
	"github.com/joeyhipolito/nanika-linkedin/internal/frontmatter"
)

type postOutput struct {
	ID         string `json:"id"`
	URL        string `json:"url"`
	Visibility string `json:"visibility"`
}

// PostCmd creates a LinkedIn post (text, image, or MDX article).
func PostCmd(args []string, jsonOutput bool) error {
	for _, arg := range args {
		if arg == "--help" || arg == "-h" {
			fmt.Print(`Usage: linkedin post <text> [--image <path>] [--visibility PUBLIC|CONNECTIONS]
       linkedin post --file <mdx-file> [--image <path>] [--visibility PUBLIC|CONNECTIONS]

Options:
  --image <path>                    Attach an image to the post
  --file <mdx-file>                 Create post from an MDX file
  --visibility PUBLIC|CONNECTIONS   Post visibility (default: PUBLIC)
  --json                            Output result as JSON
  --help, -h                        Show this help
`)
			return nil
		}
	}

	// Parse flags
	var text string
	var imagePath string
	var filePath string
	visibility := "PUBLIC"

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--image":
			if i+1 >= len(args) {
				return fmt.Errorf("--image requires a file path")
			}
			i++
			imagePath = args[i]
		case "--file":
			if i+1 >= len(args) {
				return fmt.Errorf("--file requires a path to an MDX file")
			}
			i++
			filePath = args[i]
		case "--visibility":
			if i+1 >= len(args) {
				return fmt.Errorf("--visibility requires a value (PUBLIC or CONNECTIONS)")
			}
			i++
			v := strings.ToUpper(args[i])
			if v != "PUBLIC" && v != "CONNECTIONS" {
				return fmt.Errorf("--visibility must be PUBLIC or CONNECTIONS")
			}
			visibility = v
		default:
			if text == "" {
				text = args[i]
			} else {
				// Concatenate remaining args as post text
				text += " " + args[i]
			}
		}
	}

	// Load config and validate OAuth
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}
	if cfg.AccessToken == "" {
		return fmt.Errorf("OAuth not configured. Run 'linkedin configure' to set up authentication")
	}
	if cfg.PersonURN == "" {
		return fmt.Errorf("person URN missing. Run 'linkedin configure' to re-authorize")
	}

	client := api.NewOAuthClient(cfg.AccessToken, cfg.PersonURN)

	// Determine post type and build request
	switch {
	case filePath != "":
		return postFromFile(client, filePath, imagePath, visibility, jsonOutput)
	case text != "":
		return postText(client, text, imagePath, visibility, jsonOutput)
	default:
		return fmt.Errorf("usage: linkedin post <text> [--image <path>] [--visibility PUBLIC|CONNECTIONS]\n       linkedin post --file <mdx-file> [--image <path>] [--visibility PUBLIC|CONNECTIONS]")
	}
}

// postText creates a text post, optionally with an image.
func postText(client *api.OAuthClient, text, imagePath, visibility string, jsonOutput bool) error {
	var content *api.Content

	if imagePath != "" {
		imageURN, err := uploadImage(client, imagePath)
		if err != nil {
			return err
		}
		content = &api.Content{
			Media: &api.MediaContent{
				ID: imageURN,
			},
		}
	}

	req := &api.CreatePostRequest{
		Author:     client.PersonURN,
		Commentary: text,
		Visibility: visibility,
		Distribution: api.Distribution{
			FeedDistribution:               "MAIN_FEED",
			TargetEntities:                 []any{},
			ThirdPartyDistributionChannels: []any{},
		},
		LifecycleState:            "PUBLISHED",
		IsReshareDisabledByAuthor: false,
		Content:                   content,
	}

	fmt.Fprintf(os.Stderr, "Creating post...\n")

	postID, err := client.CreatePost(req)
	if err != nil {
		return fmt.Errorf("creating post: %w", err)
	}

	return printPostResult(postID, visibility, jsonOutput)
}

// postFromFile creates a post from an MDX file.
func postFromFile(client *api.OAuthClient, filePath, imagePath, visibility string, jsonOutput bool) error {
	data, err := os.ReadFile(filePath)
	if err != nil {
		return fmt.Errorf("reading file: %w", err)
	}

	fm, body := frontmatter.Split(string(data))
	title := frontmatter.Field(fm, "title")

	// Strip JSX components from MDX body
	body = stripJSX(body)

	// Build commentary: title + body for LinkedIn's text-only format
	var commentary string
	if title != "" {
		commentary = title + "\n\n" + strings.TrimSpace(body)
	} else {
		commentary = strings.TrimSpace(body)
	}

	// LinkedIn posts have a 3000-character limit
	if len(commentary) > 3000 {
		commentary = commentary[:2997] + "..."
		fmt.Fprintf(os.Stderr, "Warning: post text truncated to 3000 characters\n")
	}

	var content *api.Content
	if imagePath != "" {
		imageURN, err := uploadImage(client, imagePath)
		if err != nil {
			return err
		}
		content = &api.Content{
			Media: &api.MediaContent{
				ID: imageURN,
			},
		}
	}

	req := &api.CreatePostRequest{
		Author:     client.PersonURN,
		Commentary: commentary,
		Visibility: visibility,
		Distribution: api.Distribution{
			FeedDistribution:               "MAIN_FEED",
			TargetEntities:                 []any{},
			ThirdPartyDistributionChannels: []any{},
		},
		LifecycleState:            "PUBLISHED",
		IsReshareDisabledByAuthor: false,
		Content:                   content,
	}

	fmt.Fprintf(os.Stderr, "Creating post from %s...\n", filepath.Base(filePath))

	postID, err := client.CreatePost(req)
	if err != nil {
		return fmt.Errorf("creating post: %w", err)
	}

	return printPostResult(postID, visibility, jsonOutput)
}

// uploadImage handles the two-step LinkedIn image upload flow.
func uploadImage(client *api.OAuthClient, imagePath string) (string, error) {
	imageData, err := os.ReadFile(imagePath)
	if err != nil {
		return "", fmt.Errorf("reading image %s: %w", imagePath, err)
	}

	fmt.Fprintf(os.Stderr, "Uploading image %s...\n", filepath.Base(imagePath))

	// Step 1: Initialize upload
	uploadInfo, err := client.InitializeImageUpload()
	if err != nil {
		return "", fmt.Errorf("initializing image upload: %w", err)
	}

	// Step 2: Upload binary
	if err := client.UploadImage(uploadInfo.UploadURL, imageData); err != nil {
		return "", err
	}

	return uploadInfo.Image, nil
}

func printPostResult(postID, visibility string, jsonOutput bool) error {
	// Extract activity ID from URN for URL construction
	postURL := fmt.Sprintf("https://www.linkedin.com/feed/update/%s", postID)

	if jsonOutput {
		out := postOutput{
			ID:         postID,
			URL:        postURL,
			Visibility: visibility,
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(out)
	}

	fmt.Fprintf(os.Stderr, "Post created!\n")
	fmt.Println()
	fmt.Printf("ID:         %s\n", postID)
	fmt.Printf("Visibility: %s\n", visibility)
	fmt.Printf("URL:        %s\n", postURL)

	return nil
}

// stripJSX removes JSX components from markdown body (same logic as Medium CLI).
func stripJSX(body string) string {
	lines := strings.Split(body, "\n")
	var result []string
	inJSXBlock := false
	jsxBlockTag := ""

	for _, line := range lines {
		trimmed := strings.TrimSpace(line)

		if inJSXBlock {
			if strings.HasPrefix(trimmed, "</"+jsxBlockTag+">") {
				inJSXBlock = false
			}
			continue
		}

		if isJSXSelfClosing(trimmed) {
			continue
		}

		if tag, ok := isJSXBlockOpen(trimmed); ok {
			inJSXBlock = true
			jsxBlockTag = tag
			continue
		}

		result = append(result, line)
	}

	return strings.Join(result, "\n")
}

func isJSXSelfClosing(line string) bool {
	if !strings.HasPrefix(line, "<") || !strings.HasSuffix(line, "/>") {
		return false
	}
	if len(line) < 3 {
		return false
	}
	return line[1] >= 'A' && line[1] <= 'Z'
}

func isJSXBlockOpen(line string) (string, bool) {
	if !strings.HasPrefix(line, "<") || strings.HasSuffix(line, "/>") {
		return "", false
	}
	if len(line) < 3 {
		return "", false
	}
	if line[1] < 'A' || line[1] > 'Z' {
		return "", false
	}
	rest := line[1:]
	tagEnd := strings.IndexAny(rest, " >")
	if tagEnd == -1 {
		return "", false
	}
	return rest[:tagEnd], true
}
