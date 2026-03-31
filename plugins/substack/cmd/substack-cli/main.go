package main

import (
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-substack/internal/cmd"
)

const version = "0.1.0"

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "Error: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	args := os.Args[1:]

	// Handle help/version
	if len(args) == 0 || args[0] == "--help" || args[0] == "-h" {
		printUsage()
		return nil
	}
	if args[0] == "--version" || args[0] == "-v" {
		fmt.Printf("substack version %s\n", version)
		return nil
	}

	// Parse subcommand
	subcommand := args[0]
	remainingArgs := args[1:]

	// Extract global --json flag
	jsonOutput := false
	var filteredArgs []string
	for _, arg := range remainingArgs {
		if arg == "--json" {
			jsonOutput = true
		} else {
			filteredArgs = append(filteredArgs, arg)
		}
	}

	// Commands that don't require configuration
	switch subcommand {
	case "configure":
		return cmd.ConfigureCmd(filteredArgs, jsonOutput)
	case "doctor":
		return cmd.DoctorCmd(jsonOutput)
	}

	// Main commands (require config)
	switch subcommand {
	case "draft":
		return cmd.DraftCmd(filteredArgs, jsonOutput)
	case "delete":
		return cmd.DeleteCmd(filteredArgs, jsonOutput)
	case "drafts":
		return cmd.DraftsCmd(filteredArgs, jsonOutput)
	case "posts":
		return cmd.PostsCmd(filteredArgs, jsonOutput)
	case "tags":
		return cmd.TagsCmd(filteredArgs, jsonOutput)
	case "update":
		return cmd.UpdateCmd(filteredArgs, jsonOutput)
	case "edit":
		return cmd.EditCmd(filteredArgs, jsonOutput)
	case "publish":
		return cmd.PublishCmd(filteredArgs, jsonOutput)
	case "unpublish":
		return cmd.UnpublishCmd(filteredArgs, jsonOutput)
	case "feed":
		return cmd.FeedCmd(filteredArgs, jsonOutput)
	case "comments":
		return cmd.CommentsCmd(filteredArgs, jsonOutput)
	case "comment":
		return cmd.CommentCmd(filteredArgs, jsonOutput)
	case "note":
		return cmd.NoteCmd(filteredArgs, jsonOutput)
	case "notes":
		return cmd.NotesCmd(filteredArgs, jsonOutput)
	case "dashboard":
		return cmd.DashboardCmd(filteredArgs, jsonOutput)
	case "engage":
		return cmd.EngageCmd(filteredArgs, jsonOutput)
	case "query":
		if len(filteredArgs) == 0 {
			return fmt.Errorf("usage: substack query <status|items|actions>")
		}
		return cmd.QueryCmd(filteredArgs[0], jsonOutput)
	default:
		return fmt.Errorf("unknown command: %s\n\nRun 'substack --help' for usage", subcommand)
	}
}

func printUsage() {
	fmt.Print(`substack - Cross-post blog content to Substack

Usage:
  substack <command> [flags]

Commands:
  configure                      Set up Substack authentication (interactive)
  configure --from-browser NAME  Extract cookie from browser (chrome, firefox)
  configure show                 Show current configuration
  doctor                         Verify setup and connectivity
  draft <file>                   Create a draft from an MDX file on Substack
  drafts                         List current drafts
  publish <draft-id>             Schedule a draft for release (PUT→prepublish→scheduled_release)
  posts                          List published posts
  tags                           List publication-level tags
  tags create "name"             Create a publication-level tag
  tags delete <name-or-id>       Delete a tag from the publication
  update <post-id>               Update an existing post/draft (e.g. tags)
  edit <post-id> <file>          Edit content of an existing post/draft
  unpublish <post-id>            Revert published post to draft
  feed                           Show posts from publications you follow
  comments <post-url>            Read comments on a post
  comment <post-url> "text"      Post a comment on a post
  note "text"                    Post a Note to Substack
  note --reply-to <id> "text"    Reply to a note
  note --delete <id>             Delete a note
  notes                          List your recent notes
  notes --replies <id>           Show replies on a note
  dashboard                      Your for-you feed (notes + posts)
  dashboard --notes              Notes only (for engagement)
  engage                         Automated feed engagement (dry-run)
  engage --post                  Actually post comments and reactions

Global Flags:
  --json             Output in JSON format
  --help, -h         Show this help
  --version, -v      Show version

Configure Flags:
  --from-browser NAME  Extract cookie from browser (chrome, firefox)

Draft Flags:
  --audience <type>  Set audience: everyone, only_paid, founding, only_free (default: everyone)
  --tags <list>      Comma-separated tags (overrides frontmatter tags)
  --manifest <path>  Use contentkit manifest.json for image assets

Publish Flags:
  --at <RFC3339>           Scheduled publish datetime, must be in the future (required)
  --post-audience <type>   Who can read the post: everyone, only_paid, founding, only_free (default: everyone)
  --email-audience <type>  Who receives the email: everyone, only_paid, founding, only_free (default: everyone)
  --dry-run                Run prepublish checks and print plan without scheduling

Update Flags:
  --tags <list>        Comma-separated tags to set on the post/draft
  --remove-tags <list> Comma-separated tags to remove from the post/draft

Edit Flags:
  --title <text>     Override title (default: from frontmatter)
  --subtitle <text>  Override subtitle (default: from frontmatter)
  --manifest <path>  Use contentkit manifest.json for image assets
  --dry-run          Preview changes without updating

Posts Flags:
  --limit <N>        Number of posts to show (default: 25)
  --scout            Output IntelItem-compatible JSON for scout pipeline integration

Feed Flags:
  --limit <N>        Number of items to show (default: 10)
  --scout            Output IntelItem-compatible JSON for scout pipeline integration

Comments Flags:
  --limit <N>        Number of comments to show (default: 25)

Comment Flags:
  --yes, -y          Skip confirmation prompt

Note Flags:
  --file <path>      Read note text from a file
  --reply-to <id>    Reply to an existing note
  --delete <id>      Delete a note by ID
  --dry-run          Show JSON body without posting
  --yes, -y          Skip confirmation prompt

Notes Flags:
  --limit <N>        Number of notes to show (default: 10)
  --replies <id>     Show replies on a specific note

Dashboard Flags:
  --limit <N>        Number of items (default: 10)
  --notes            Show only notes (skip posts)

Engage Flags:
  --post               Actually post comments and reactions (default: dry-run)
  --persona <path>     Path to persona markdown file for voice
  --max-comments <N>   Max comments per run (default: 3)
  --max-reacts <N>     Max reactions per run (default: 8)

Examples:
  substack configure
  substack configure --from-browser chrome
  substack doctor
  substack draft ~/blog/my-post.mdx
  substack draft ~/blog/my-post.mdx --tags "AI,Go,Architecture"
  substack draft ~/blog/my-post.mdx --audience only_paid
  substack update 12345 --tags "AI,Go,Architecture"
  substack edit 12345 ~/blog/my-post.mdx
  substack edit 12345 ~/blog/my-post.mdx --dry-run
  substack edit 12345 ~/blog/my-post.mdx --title "New Title"
  substack tags --json
  substack tags create "Go"
  substack tags delete "Go"
  substack tags delete abc123-uuid
  substack drafts --json
  substack posts --json
  substack feed
  substack feed --limit 20
  substack feed --json
  substack publish 12345 --at 2026-03-01T10:00:00Z
  substack publish 12345 --at 2026-03-01T10:00:00Z --post-audience only_paid --email-audience only_paid
  substack publish 12345 --at 2026-03-01T10:00:00Z --dry-run
  substack publish 12345 --at 2026-03-01T10:00:00Z --json
  substack comments https://example.substack.com/p/my-post
  substack comment https://example.substack.com/p/my-post "Great article!"
  substack note "Interesting thought about Go error handling"
  substack note --file ~/notes/draft.txt
  substack note --reply-to 219485719 "Great point!"
  substack note --delete 219485719
  substack note "My note" --dry-run
  substack notes
  substack notes --limit 20
  substack notes --replies 219485719
  substack dashboard
  substack dashboard --notes
  substack dashboard --notes --json
`)
}
