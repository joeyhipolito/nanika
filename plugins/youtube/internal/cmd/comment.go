package cmd

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-youtube/internal/api"
)

// CommentCmd posts a top-level comment on a YouTube video.
// Usage: youtube comment <video-id> <text> [--json]
func CommentCmd(args []string, jsonOutput bool) error {
	fs := flag.NewFlagSet("comment", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: youtube comment <video-id> <text>

Post a top-level comment on a YouTube video.
Costs 50 quota units.

Arguments:
  <video-id>   YouTube video ID (e.g. dQw4w9WgXcQ) or full URL
  <text>       Comment text

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	remaining := fs.Args()
	if len(remaining) < 2 {
		fs.Usage()
		return fmt.Errorf("video-id and comment text are required")
	}

	videoID := extractVideoID(remaining[0])
	text := remaining[1]

	client, err := api.NewClient()
	if err != nil {
		return fmt.Errorf("loading youtube config: %w", err)
	}

	ctx := context.Background()
	result, err := client.Comment(ctx, videoID, text)
	if err != nil {
		return fmt.Errorf("posting comment: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(result)
	}

	fmt.Printf("Comment posted: %s\n", result.URL)
	return nil
}

// LikeCmd likes a YouTube video.
// Usage: youtube like <video-id> [--json]
func LikeCmd(args []string, jsonOutput bool) error {
	fs := flag.NewFlagSet("like", flag.ContinueOnError)
	fs.SetOutput(os.Stderr)
	fs.Usage = func() {
		fmt.Fprintf(os.Stderr, `Usage: youtube like <video-id>

Like a YouTube video (videos.rate).
Costs 50 quota units.

Arguments:
  <video-id>   YouTube video ID (e.g. dQw4w9WgXcQ) or full URL

Options:
`)
		fs.PrintDefaults()
	}
	if err := fs.Parse(args); err != nil {
		return err
	}

	remaining := fs.Args()
	if len(remaining) < 1 {
		fs.Usage()
		return fmt.Errorf("video-id is required")
	}

	videoID := extractVideoID(remaining[0])

	client, err := api.NewClient()
	if err != nil {
		return fmt.Errorf("loading youtube config: %w", err)
	}

	ctx := context.Background()
	result, err := client.Like(ctx, videoID)
	if err != nil {
		return fmt.Errorf("liking video: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(result)
	}

	fmt.Printf("Liked: %s\n", result.URL)
	return nil
}

// extractVideoID extracts a video ID from a full URL or returns the input as-is.
// Handles: https://www.youtube.com/watch?v=ID and https://youtu.be/ID.
func extractVideoID(input string) string {
	if len(input) == 11 {
		// Likely already a raw video ID.
		return input
	}
	// Try parsing as URL.
	for _, prefix := range []string{"https://www.youtube.com/watch?v=", "http://www.youtube.com/watch?v="} {
		if len(input) > len(prefix) && input[:len(prefix)] == prefix {
			id := input[len(prefix):]
			if amp := len(id); amp > 11 {
				id = id[:11]
			}
			return id
		}
	}
	for _, prefix := range []string{"https://youtu.be/", "http://youtu.be/"} {
		if len(input) > len(prefix) && input[:len(prefix)] == prefix {
			id := input[len(prefix):]
			if len(id) > 11 {
				id = id[:11]
			}
			return id
		}
	}
	return input
}
