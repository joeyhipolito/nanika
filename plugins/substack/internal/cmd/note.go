package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/api"
	"github.com/joeyhipolito/nanika-substack/internal/config"
	"github.com/joeyhipolito/nanika-substack/internal/tiptap"
)

// NoteCmd publishes a short-form note to Substack, or replies to an existing note.
func NoteCmd(args []string, jsonOutput bool) error {
	cfg, err := config.Load()
	if err != nil {
		return fmt.Errorf("loading config: %w", err)
	}

	var text string
	var filePath string
	var imagePath string
	var replyTo int
	var deleteID int
	var likeID int
	dryRun := false
	skipConfirm := false

	for i := 0; i < len(args); i++ {
		switch args[i] {
		case "--help", "-h":
			fmt.Print(`substack note - Post a Note to Substack

Usage:
  substack note "text"                Post a note
  substack note --file <path>         Post from file
  substack note --image <path>        Attach an image
  substack note --reply-to <id> "text"  Reply to a note
  substack note --delete <id>         Delete a note
  substack note --like <id>          React to a note
  substack note --dry-run "text"      Preview without posting

Flags:
  --file <path>      Read note text from file
  --image <path>     Attach an image to the note
  --reply-to <id>    Reply to an existing note
  --delete <id>      Delete a note by ID
  --like <id>        React to a note
  --dry-run          Show JSON body without posting
  --yes, -y          Skip confirmation prompt
  --json             Output in JSON format
`)
			return nil
		case "--image":
			if i+1 >= len(args) {
				return fmt.Errorf("--image requires a path argument")
			}
			i++
			imagePath = args[i]
		case "--file":
			if i+1 >= len(args) {
				return fmt.Errorf("--file requires a path argument")
			}
			i++
			filePath = args[i]
		case "--reply-to":
			if i+1 >= len(args) {
				return fmt.Errorf("--reply-to requires a note ID")
			}
			i++
			replyTo = parseIntArg(args[i])
			if replyTo <= 0 {
				return fmt.Errorf("--reply-to requires a valid note ID")
			}
		case "--delete":
			if i+1 >= len(args) {
				return fmt.Errorf("--delete requires a note ID")
			}
			i++
			deleteID = parseIntArg(args[i])
			if deleteID <= 0 {
				return fmt.Errorf("--delete requires a valid note ID")
			}
		case "--like":
			if i+1 >= len(args) {
				return fmt.Errorf("--like requires a note ID")
			}
			i++
			likeID = parseIntArg(args[i])
			if likeID <= 0 {
				return fmt.Errorf("--like requires a valid note ID")
			}
		case "--dry-run":
			dryRun = true
		case "--yes", "-y":
			skipConfirm = true
		default:
			if text == "" {
				text = args[i]
			} else {
				return fmt.Errorf("unexpected argument: %s", args[i])
			}
		}
	}

	client := api.NewClient(cfg.Subdomain, cfg.Cookie)

	// Handle delete
	if deleteID > 0 {
		if !skipConfirm && !jsonOutput {
			fmt.Printf("Delete note #%d?\nConfirm (y/N): ", deleteID)
			var confirm string
			fmt.Scanln(&confirm)
			if confirm != "y" && confirm != "Y" {
				fmt.Println("Cancelled.")
				return nil
			}
		}
		if err := client.DeleteNote(deleteID); err != nil {
			return fmt.Errorf("deleting note: %w", err)
		}
		if jsonOutput {
			fmt.Printf("{\"deleted\": %d}\n", deleteID)
		} else {
			fmt.Printf("Note #%d deleted.\n", deleteID)
		}
		return nil
	}

	// Handle like
	if likeID > 0 {
		if err := client.ReactToNote(likeID); err != nil {
			return fmt.Errorf("reacting to note: %w", err)
		}
		if jsonOutput {
			fmt.Printf("{\"liked\": %d}\n", likeID)
		} else {
			fmt.Printf("❤ Note #%d\n", likeID)
		}
		return nil
	}

	if filePath != "" {
		data, err := os.ReadFile(filePath)
		if err != nil {
			return fmt.Errorf("reading file %s: %w", filePath, err)
		}
		text = string(data)
	}

	if trimSpace(text) == "" {
		return fmt.Errorf("usage: substack note \"text\" [--file <path>] [--reply-to <id>] [--delete <id>] [--dry-run] [--yes] [--json]")
	}

	bodyJSON, err := tiptap.BuildNoteBody(text)
	if err != nil {
		return fmt.Errorf("building note body: %w", err)
	}

	if dryRun {
		var pretty any
		if err := json.Unmarshal([]byte(bodyJSON), &pretty); err != nil {
			return fmt.Errorf("pretty-printing body: %w", err)
		}
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(pretty)
	}

	action := "Post note"
	if replyTo > 0 {
		action = fmt.Sprintf("Reply to note #%d", replyTo)
	}

	if !skipConfirm && !jsonOutput {
		preview := text
		if len(preview) > 80 {
			preview = preview[:80] + "..."
		}
		fmt.Printf("%s to Substack?\n  %s\n", action, preview)
		fmt.Print("Confirm (y/N): ")
		var confirm string
		fmt.Scanln(&confirm)
		if confirm != "y" && confirm != "Y" {
			fmt.Println("Cancelled.")
			return nil
		}
	}

	// Handle image upload → attachment flow
	var attachmentIDs []string
	if imagePath != "" {
		if !jsonOutput {
			fmt.Print("Uploading image... ")
		}
		imageURL, uploadErr := client.UploadImageGlobal(imagePath)
		if uploadErr != nil {
			return fmt.Errorf("uploading image: %w", uploadErr)
		}
		if !jsonOutput {
			fmt.Println("done.")
		}

		attachmentID, attachErr := client.CreateAttachment(imageURL)
		if attachErr != nil {
			return fmt.Errorf("creating attachment: %w", attachErr)
		}
		attachmentIDs = append(attachmentIDs, attachmentID)
	}

	var note *api.Note
	if replyTo > 0 {
		note, err = client.ReplyToNote(bodyJSON, replyTo, attachmentIDs...)
	} else {
		note, err = client.CreateNote(bodyJSON, attachmentIDs...)
	}
	if err != nil {
		return fmt.Errorf("creating note: %w", err)
	}

	if jsonOutput {
		enc := json.NewEncoder(os.Stdout)
		enc.SetIndent("", "  ")
		return enc.Encode(note)
	}

	if replyTo > 0 {
		fmt.Println("Reply posted successfully.")
	} else {
		fmt.Println("Note posted successfully.")
	}
	if note.ID > 0 {
		fmt.Printf("  https://substack.com/@%s/note/c-%d\n", cfg.Subdomain, note.ID)
	}

	return nil
}

