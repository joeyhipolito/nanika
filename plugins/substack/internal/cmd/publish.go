package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
	"github.com/joeyhipolito/nanika-substack/internal/frontmatter"
)

// DraftCmd handles the draft command.
func DraftCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	// Parse flags
	var filePath string
	var tagsFlag string
	var manifestPath string
	var publicDir string
	draftOnly := true
	audience := "everyone"

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--draft":
			draftOnly = true
		case "--audience":
			if i+1 < len(args) {
				i++
				audience = args[i]
			} else {
				return fmt.Errorf("--audience requires a value (everyone, only_paid, founding, only_free)")
			}
		case "--tags":
			if i+1 < len(args) {
				i++
				tagsFlag = args[i]
			} else {
				return fmt.Errorf("--tags requires a comma-separated list of tag names")
			}
		case "--manifest":
			if i+1 < len(args) {
				i++
				manifestPath = args[i]
			} else {
				return fmt.Errorf("--manifest requires a path to manifest.json")
			}
		case "--public-dir":
			if i+1 < len(args) {
				i++
				publicDir = args[i]
			} else {
				return fmt.Errorf("--public-dir requires a path to the public directory")
			}
		default:
			if filePath == "" {
				filePath = args[i]
			} else {
				return fmt.Errorf("unexpected argument: %s", args[i])
			}
		}
	}

	if filePath == "" {
		return fmt.Errorf("usage: substack draft <path-to-mdx-file> [--audience everyone|paid] [--public-dir <path>]")
	}

	// Read the file
	content, err := os.ReadFile(filePath)
	if err != nil {
		return fmt.Errorf("reading file: %w", err)
	}

	// Create API client (needed for image uploads during conversion)
	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	// Build image resolver: manifest-based if --manifest provided, else contentkit-output discovery
	var imgResolver ImageResolver
	var manifest *Manifest
	if manifestPath != "" {
		var err error
		manifest, err = loadManifest(manifestPath)
		if err != nil {
			return fmt.Errorf("loading manifest: %w", err)
		}
		imgResolver = buildManifestImageResolver(manifest, client)
	} else {
		imgResolver = buildImageResolver(filePath, client)
	}

	// Parse frontmatter and convert body
	title, subtitle, tiptapBody, fmTags, err := parseAndConvert(string(content), cfg.SiteURL, imgResolver)
	if err != nil {
		return fmt.Errorf("converting content: %w", err)
	}

	// Resolve public dir: explicit flag > auto-detect by walking ancestor dirs
	if publicDir == "" {
		publicDir = findBlogRoot(filePath)
	}
	if publicDir != "" {
		abs, absErr := filepath.Abs(publicDir)
		if absErr == nil {
			publicDir = abs
		}
	}

	// Upload local images: absolute filesystem paths and any /-prefixed site-relative paths
	tiptapBody, err = uploadLocalImages(tiptapBody, publicDir, client)
	if err != nil {
		return fmt.Errorf("uploading local images: %w", err)
	}

	// Upload manifest illustrations and diagrams (insert into Tiptap JSON)
	if manifest != nil {
		tiptapBody, err = uploadManifestIllustrations(tiptapBody, manifest, client)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to upload manifest illustrations: %v\n", err)
		}
	}

	// Get user ID for bylines
	user, err := client.GetProfile()
	if err != nil {
		return fmt.Errorf("authenticating: %w", err)
	}

	// Resolve tag names: --tags flag takes precedence, then frontmatter
	tagNames := resolveTagNames(tagsFlag, fmTags)

	// Create draft
	draftReq := &api.DraftRequest{
		DraftTitle:    title,
		DraftSubtitle: subtitle,
		DraftBody:     tiptapBody,
		DraftBylines:  []api.DraftByline{{ID: user.ID}},
		Type:          "newsletter",
		Audience:      audience,
		EditorV2:      true,
	}

	fmt.Printf("Creating draft: %s\n", title)
	draft, err := client.CreateDraft(draftReq)
	if err != nil {
		return fmt.Errorf("creating draft: %w", err)
	}

	// Assign tags after draft creation (tags use separate API endpoints)
	if len(tagNames) > 0 {
		fmt.Printf("Applying tags: %s\n", strings.Join(tagNames, ", "))
		_, err := client.EnsureAndAssignTags(draft.ID, tagNames)
		if err != nil {
			fmt.Fprintf(os.Stderr, "Warning: failed to apply tags: %v\n", err)
		}
	}

	draftURL := fmt.Sprintf("%s/publish/post/%d", cfg.PublicationURL, draft.ID)

	if draftOnly {
		if jsonOutput {
			return outputPublishJSON(draft, draftURL, "draft")
		}
		fmt.Printf("Draft created: %s\n", draftURL)
		return nil
	}

	// Prepublish
	fmt.Println("Running prepublish checks...")
	if err := client.Prepublish(draft.ID); err != nil {
		return fmt.Errorf("prepublish failed: %w\nDraft saved at: %s", err, draftURL)
	}

	// Publish
	fmt.Println("Publishing...")
	published, err := client.Publish(draft.ID, true)
	if err != nil {
		return fmt.Errorf("publish failed: %w\nDraft saved at: %s", err, draftURL)
	}

	postURL := fmt.Sprintf("%s/p/%s", cfg.PublicationURL, published.Slug)

	if jsonOutput {
		return outputPublishJSON(published, postURL, "published")
	}

	fmt.Printf("Published: %s\n", postURL)
	return nil
}

func outputPublishJSON(post *api.Post, url, status string) error {
	// publishOutput mirrors the channels.substack schema in velite.config.ts.
	// All fields map 1:1 so callers can write this struct directly into MDX frontmatter.
	type publishOutput struct {
		ID           int    `json:"id"`
		Title        string `json:"title"`
		Slug         string `json:"slug,omitempty"`
		URL          string `json:"url"`
		CanonicalURL string `json:"canonical_url,omitempty"`
		Status       string `json:"status"`
		Audience     string `json:"audience,omitempty"`
		EmailSentAt  string `json:"email_sent_at,omitempty"`
		PublishedAt  string `json:"published_at,omitempty"`
	}
	out := publishOutput{
		ID:           post.ID,
		Title:        post.Title,
		Slug:         post.Slug,
		URL:          url,
		CanonicalURL: post.CanonicalURL,
		Status:       status,
		Audience:     post.Audience,
		EmailSentAt:  post.EmailSentAt,
		PublishedAt:  post.PublishDate,
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(out)
}

// parseAndConvert parses MDX frontmatter and converts the body to Tiptap JSON.
func parseAndConvert(content string, siteURL string, imgResolver ImageResolver) (title, subtitle, tiptapBody string, tags []string, err error) {
	fm, body := frontmatter.Split(content)

	title = frontmatter.Field(fm, "title")
	subtitle = frontmatter.Field(fm, "description")
	if len(subtitle) > 256 {
		subtitle = subtitle[:253] + "..."
	}

	if title == "" {
		return "", "", "", nil, fmt.Errorf("missing 'title' in frontmatter")
	}

	tags = frontmatter.List(fm, "tags")

	tiptapBody, err = markdownToTiptap(body, siteURL, imgResolver)
	if err != nil {
		return "", "", "", nil, fmt.Errorf("converting markdown to tiptap: %w", err)
	}
	return title, subtitle, tiptapBody, tags, nil
}

// resolveTagNames returns tag name strings from either --tags flag or frontmatter tags.
// The --tags flag takes precedence if provided.
func resolveTagNames(tagsFlag string, fmTags []string) []string {
	if tagsFlag != "" {
		var names []string
		parts := strings.Split(tagsFlag, ",")
		for _, p := range parts {
			p = strings.TrimSpace(p)
			if p != "" {
				names = append(names, p)
			}
		}
		return names
	}
	return fmTags
}

// buildImageResolver creates an ImageResolver that looks up pre-generated contentkit PNGs
// and uploads them to Substack's CDN. PNGs are expected at ~/contentkit-output/{basename}-code-{NNN}.png.
func buildImageResolver(mdxPath string, client *api.Client) ImageResolver {
	// Determine the base name used by contentkit ray --extract
	base := strings.TrimSuffix(filepath.Base(mdxPath), filepath.Ext(mdxPath))

	// Check contentkit output directory
	outputDir := os.ExpandEnv("$HOME/contentkit-output")
	if _, err := os.Stat(outputDir); err != nil {
		return nil // no contentkit output directory
	}

	// Find all matching PNGs (contentkit names them: {base}-code-001.png, {base}-code-002.png, ...)
	pattern := filepath.Join(outputDir, base+"-code-*.png")
	matches, err := filepath.Glob(pattern)
	if err != nil || len(matches) == 0 {
		return nil // no pre-generated images
	}

	// Upload cache: index → Substack CDN URL
	uploaded := make(map[int]string)

	return func(index int) string {
		// Check cache
		if url, ok := uploaded[index]; ok {
			return url
		}

		// Find the matching PNG (1-indexed: code-001.png = index 0)
		pngPath := filepath.Join(outputDir, fmt.Sprintf("%s-code-%03d.png", base, index+1))
		if _, err := os.Stat(pngPath); err != nil {
			return "" // no image for this block
		}

		// Upload to Substack CDN
		fmt.Printf("  Uploading image %d: %s\n", index+1, filepath.Base(pngPath))
		url, err := client.UploadImage(pngPath)
		if err != nil {
			fmt.Fprintf(os.Stderr, "  Warning: failed to upload %s: %v\n", filepath.Base(pngPath), err)
			return "" // fall back to text code block
		}

		uploaded[index] = url
		return url
	}
}

// findBlogRoot walks up from the MDX file path looking for a directory containing "public/".
// Returns the path to "public" or "" if not found.
func findBlogRoot(mdxPath string) string {
	dir, err := filepath.Abs(filepath.Dir(mdxPath))
	if err != nil {
		return ""
	}
	for {
		candidate := filepath.Join(dir, "public")
		if info, err := os.Stat(candidate); err == nil && info.IsDir() {
			return candidate
		}
		parent := filepath.Dir(dir)
		if parent == dir {
			break
		}
		dir = parent
	}
	return ""
}

// uploadLocalImages parses Tiptap JSON, finds image2 nodes with local paths
// (site-relative URLs like https://site.com/blog/... or absolute /blog/...),
// resolves them to files under publicDir, uploads to Substack CDN, and returns
// the updated Tiptap JSON.
func uploadLocalImages(tiptapJSON string, publicDir string, client *api.Client) (string, error) {
	var doc TiptapDoc
	if err := json.Unmarshal([]byte(tiptapJSON), &doc); err != nil {
		return tiptapJSON, nil // not valid JSON, return as-is
	}

	changed := uploadLocalImagesInNodes(doc.Content, publicDir, client)
	if !changed {
		return tiptapJSON, nil
	}

	b, err := json.Marshal(doc)
	if err != nil {
		return tiptapJSON, err
	}
	return string(b), nil
}

// uploadLocalImagesInNodes recursively walks nodes, uploading local image/image2 src paths.
// Handles both /blog/ relative paths (resolved against publicDir) and absolute filesystem paths.
// Promotes plain "image" nodes to "image2" for proper Substack rendering.
// Returns true if any node was modified.
func uploadLocalImagesInNodes(nodes []TiptapNode, publicDir string, client *api.Client) bool {
	changed := false
	for i := range nodes {
		if nodes[i].Type == "image2" || nodes[i].Type == "image" {
			src, ok := nodes[i].Attrs["src"].(string)
			if !ok || src == "" {
				continue
			}

			var filePath string

			// Check for absolute filesystem path (e.g. /Users/.../contentkit-output/foo.png)
			if filepath.IsAbs(src) && !strings.HasPrefix(src, "http") {
				if _, err := os.Stat(src); err == nil {
					filePath = src
				}
			}

			// Check for /blog/ relative path resolved against publicDir
			if filePath == "" {
				localPath := extractLocalImagePath(src)
				if localPath != "" && publicDir != "" {
					candidate := filepath.Join(publicDir, localPath)
					if _, err := os.Stat(candidate); err == nil {
						filePath = candidate
					} else {
						fmt.Fprintf(os.Stderr, "  Warning: local image not found: %s\n", candidate)
					}
				}
			}

			if filePath == "" {
				continue
			}

			fmt.Printf("  Uploading image: %s\n", filepath.Base(filePath))
			cdnURL, err := client.UploadImage(filePath)
			if err != nil {
				fmt.Fprintf(os.Stderr, "  Warning: failed to upload %s: %v\n", filepath.Base(filePath), err)
				continue
			}

			nodes[i].Attrs["src"] = cdnURL
			// Promote plain image to image2 for proper Substack rendering
			if nodes[i].Type == "image" {
				nodes[i].Type = "image2"
				if nodes[i].Attrs["fullscreen"] == nil {
					nodes[i].Attrs["fullscreen"] = false
				}
				if nodes[i].Attrs["imageSize"] == nil {
					nodes[i].Attrs["imageSize"] = "normal"
				}
			}
			changed = true
		}
		if len(nodes[i].Content) > 0 {
			if uploadLocalImagesInNodes(nodes[i].Content, publicDir, client) {
				changed = true
			}
		}
	}
	return changed
}

// extractLocalImagePath extracts a local file path from a src attribute.
// Handles site-relative paths (/blog/, /log/, etc.) and full URLs with those path components.
// Returns the path relative to public dir, or "" if not a local content image.
func extractLocalImagePath(src string) string {
	// Site-relative path: /blog/, /log/, or any path that looks like a local content image
	for _, prefix := range []string{"/blog/", "/log/"} {
		if strings.HasPrefix(src, prefix) {
			return src[1:] // strip leading / → "blog/..." or "log/..."
		}
	}
	// Full URL — extract known path components
	if strings.HasPrefix(src, "http://") || strings.HasPrefix(src, "https://") {
		for _, prefix := range []string{"/blog/", "/log/"} {
			idx := strings.Index(src, prefix)
			if idx > 0 {
				return src[idx+1:]
			}
		}
	}
	return ""
}
