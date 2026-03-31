package cmd

import (
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/joeyhipolito/nanika-substack/internal/api"
)

// Manifest represents the contentkit prepare output manifest.
type Manifest struct {
	Article string          `json:"article"`
	Created string          `json:"created"`
	Assets  []ManifestAsset `json:"assets"`
}

// ManifestAsset represents one asset in the manifest.
type ManifestAsset struct {
	Type        string `json:"type"`
	SourceLine  int    `json:"source_line,omitempty"`
	Language    string `json:"language,omitempty"`
	Section     string `json:"section,omitempty"`
	Description string `json:"description,omitempty"`
	ImageType   string `json:"image_type,omitempty"`
	Path        string `json:"path"`
	PromptFile  string `json:"prompt_file,omitempty"`
}

// loadManifest reads and parses a manifest.json file.
func loadManifest(path string) (*Manifest, error) {
	data, err := os.ReadFile(path)
	if err != nil {
		return nil, fmt.Errorf("reading manifest: %w", err)
	}

	var m Manifest
	if err := json.Unmarshal(data, &m); err != nil {
		return nil, fmt.Errorf("parsing manifest: %w", err)
	}

	return &m, nil
}

// buildManifestImageResolver creates an ImageResolver from a manifest that uploads
// code-screenshot assets to Substack CDN. It maps code block indices to uploaded URLs.
func buildManifestImageResolver(manifest *Manifest, client *api.Client) ImageResolver {
	// Collect code-screenshot assets in order (they are already ordered by source line)
	var codeAssets []ManifestAsset
	for _, a := range manifest.Assets {
		if a.Type == "code-screenshot" {
			codeAssets = append(codeAssets, a)
		}
	}

	if len(codeAssets) == 0 {
		return nil
	}

	// Upload cache: index → Substack CDN URL
	uploaded := make(map[int]string)

	return func(index int) string {
		if url, ok := uploaded[index]; ok {
			return url
		}

		if index >= len(codeAssets) {
			return ""
		}

		asset := codeAssets[index]
		if _, err := os.Stat(asset.Path); err != nil {
			fmt.Fprintf(os.Stderr, "  Warning: manifest image not found: %s\n", asset.Path)
			return ""
		}

		fmt.Printf("  Uploading code screenshot %d: %s\n", index+1, filepath.Base(asset.Path))
		url, err := client.UploadImage(asset.Path)
		if err != nil {
			fmt.Fprintf(os.Stderr, "  Warning: failed to upload %s: %v\n", filepath.Base(asset.Path), err)
			return ""
		}

		uploaded[index] = url
		return url
	}
}

// uploadManifestIllustrations uploads illustration and diagram assets from the manifest
// and inserts them into the Tiptap JSON at the appropriate positions.
func uploadManifestIllustrations(tiptapJSON string, manifest *Manifest, client *api.Client) (string, error) {
	// Collect non-code assets that have image paths (not prompt files)
	type imageAsset struct {
		section     string
		description string
		path        string
		assetType   string
	}
	var images []imageAsset

	for _, a := range manifest.Assets {
		if a.Type == "code-screenshot" {
			continue // handled by image resolver
		}
		// Only process if the path points to an actual image (PNG)
		if !strings.HasSuffix(strings.ToLower(a.Path), ".png") {
			continue
		}
		if _, err := os.Stat(a.Path); err != nil {
			continue
		}
		images = append(images, imageAsset{
			section:     a.Section,
			description: a.Description,
			path:        a.Path,
			assetType:   a.Type,
		})
	}

	if len(images) == 0 {
		return tiptapJSON, nil
	}

	// Parse Tiptap JSON
	var doc TiptapDoc
	if err := json.Unmarshal([]byte(tiptapJSON), &doc); err != nil {
		return tiptapJSON, nil
	}

	// Upload each image and replace existing image2 nodes or insert at correct positions.
	// If uploadLocalImages already resolved the <Figure> src to CDN, we skip to avoid duplicates.
	for _, img := range images {
		// First check if the target section already has a CDN image (uploaded by uploadLocalImages).
		// Scan all nodes between this heading and the next heading, not just i+1.
		alreadyUploaded := false
		if img.section != "" {
			for i, node := range doc.Content {
				if node.Type == "heading" && matchesSection(node, img.section) {
					// Scan from i+1 until the next heading
					for j := i + 1; j < len(doc.Content); j++ {
						if doc.Content[j].Type == "heading" {
							break // reached next section
						}
						if doc.Content[j].Type == "image2" || doc.Content[j].Type == "image" {
							if existingSrc, ok := doc.Content[j].Attrs["src"].(string); ok && strings.HasPrefix(existingSrc, "https://") {
								alreadyUploaded = true
								break
							}
						}
					}
					break
				}
			}
		} else {
			// Hero: check first image2 node
			for _, node := range doc.Content {
				if node.Type == "image2" || node.Type == "image" {
					if existingSrc, ok := node.Attrs["src"].(string); ok && strings.HasPrefix(existingSrc, "https://") {
						alreadyUploaded = true
					}
					break
				}
			}
		}

		if alreadyUploaded {
			continue
		}

		// Upload the image
		fmt.Printf("  Uploading %s: %s\n", img.assetType, filepath.Base(img.path))
		cdnURL, err := client.UploadImage(img.path)
		if err != nil {
			fmt.Fprintf(os.Stderr, "  Warning: failed to upload %s: %v\n", filepath.Base(img.path), err)
			continue
		}

		// Build alt text
		alt := img.description
		if alt == "" {
			alt = img.section
		}

		// Create image2 node
		imageNode := TiptapNode{
			Type: "image2",
			Attrs: map[string]interface{}{
				"src":        cdnURL,
				"fullscreen": false,
				"imageSize":  "normal",
			},
		}
		if alt != "" {
			imageNode.Attrs["alt"] = alt
		}

		replaced := false

		if img.section != "" {
			// Find the section heading, then replace or insert
			for i, node := range doc.Content {
				if node.Type == "heading" && matchesSection(node, img.section) {
					if i+1 < len(doc.Content) && (doc.Content[i+1].Type == "image2" || doc.Content[i+1].Type == "image") {
						// Replace existing broken image node with CDN version
						doc.Content[i+1] = imageNode
						replaced = true
					} else {
						// Insert after this heading
						newContent := make([]TiptapNode, 0, len(doc.Content)+1)
						newContent = append(newContent, doc.Content[:i+1]...)
						newContent = append(newContent, imageNode)
						newContent = append(newContent, doc.Content[i+1:]...)
						doc.Content = newContent
						replaced = true
					}
					break
				}
			}
		} else {
			// Hero image (no section) — replace first image2 node, or insert at top
			for i, node := range doc.Content {
				if node.Type == "image2" || node.Type == "image" {
					doc.Content[i] = imageNode
					replaced = true
					break
				}
			}
			if !replaced {
				// Insert at position 0 (before everything)
				newContent := make([]TiptapNode, 0, len(doc.Content)+1)
				newContent = append(newContent, imageNode)
				newContent = append(newContent, doc.Content...)
				doc.Content = newContent
				replaced = true
			}
		}

		if !replaced {
			doc.Content = append(doc.Content, imageNode)
		}
	}

	b, err := json.Marshal(doc)
	if err != nil {
		return tiptapJSON, err
	}
	return string(b), nil
}

// matchesSection checks if a Tiptap heading node's text content matches a section name.
func matchesSection(node TiptapNode, section string) bool {
	text := extractNodeText(node)
	return strings.EqualFold(strings.TrimSpace(text), strings.TrimSpace(section))
}

// extractNodeText recursively extracts all text from a Tiptap node.
func extractNodeText(node TiptapNode) string {
	if node.Text != "" {
		return node.Text
	}
	var parts []string
	for _, child := range node.Content {
		if t := extractNodeText(child); t != "" {
			parts = append(parts, t)
		}
	}
	return strings.Join(parts, "")
}
