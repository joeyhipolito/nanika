package cmd

import (
	"encoding/json"
	"fmt"
	"os"

	"github.com/joeyhipolito/nanika-engage/internal/queue"
)

// QueryCmd routes query subcommands: status, items, actions, action.
func QueryCmd(args []string, jsonOutput bool) error {
	if len(args) == 0 {
		fmt.Fprintf(os.Stderr, "Usage: engage query <status|items|actions|action> [options]\n")
		return fmt.Errorf("subcommand required: status, items, actions, or action")
	}

	sub := args[0]
	rest := args[1:]

	switch sub {
	case "status":
		return queryStatus(rest, jsonOutput)
	case "items":
		return queryItems(rest, jsonOutput)
	case "actions":
		return engageQueryActions(jsonOutput)
	case "action":
		return queryAction(rest, jsonOutput)
	default:
		return fmt.Errorf("unknown query subcommand %q — use status, items, actions, or action", sub)
	}
}

// queryStatusOutput is the JSON shape for `query status --json`.
type queryStatusOutput struct {
	Pending  int `json:"pending"`
	Approved int `json:"approved"`
	Rejected int `json:"rejected"`
	Posted   int `json:"posted"`
}

func queryStatus(_ []string, jsonOutput bool) error {
	store, err := queue.NewStore(queue.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening queue: %w", err)
	}

	all, err := store.List("")
	if err != nil {
		return fmt.Errorf("listing drafts: %w", err)
	}

	out := queryStatusOutput{}
	for _, d := range all {
		switch d.State {
		case queue.StatePending:
			out.Pending++
		case queue.StateApproved:
			out.Approved++
		case queue.StateRejected:
			out.Rejected++
		case queue.StatePosted:
			out.Posted++
		}
	}

	if jsonOutput {
		return encodeJSON(out)
	}

	fmt.Printf("pending:  %d\n", out.Pending)
	fmt.Printf("approved: %d\n", out.Approved)
	fmt.Printf("rejected: %d\n", out.Rejected)
	fmt.Printf("posted:   %d\n", out.Posted)
	return nil
}

// queryItemOutput is one element in the JSON array for `query items --json`.
type queryItemOutput struct {
	ID             string `json:"id"`
	Platform       string `json:"platform"`
	PostTitle      string `json:"post_title"`
	State          string `json:"state"`
	CreatedAt      string `json:"created_at"`
	CommentPreview string `json:"comment_preview"`
}

type queryItemsOutput struct {
	Items []queryItemOutput `json:"items"`
	Count int               `json:"count"`
}

func queryItems(_ []string, jsonOutput bool) error {
	store, err := queue.NewStore(queue.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening queue: %w", err)
	}

	drafts, err := store.List("")
	if err != nil {
		return fmt.Errorf("listing drafts: %w", err)
	}

	items := make([]queryItemOutput, 0, len(drafts))
	for _, d := range drafts {
		items = append(items, queryItemOutput{
			ID:             d.ID,
			Platform:       d.Platform,
			PostTitle:      d.Opportunity.Title,
			State:          string(d.State),
			CreatedAt:      d.CreatedAt.UTC().Format("2006-01-02T15:04:05Z"),
			CommentPreview: truncateTitle(d.Comment, 100),
		})
	}

	if jsonOutput {
		return encodeJSON(queryItemsOutput{Items: items, Count: len(items)})
	}

	if len(items) == 0 {
		fmt.Println("No drafts in queue.")
		return nil
	}
	for _, it := range items {
		fmt.Printf("[%s] %s  platform=%s  state=%s  created=%s\n",
			it.ID, it.PostTitle, it.Platform, it.State, it.CreatedAt)
		if it.CommentPreview != "" {
			fmt.Printf("  comment: %s\n", it.CommentPreview)
		}
	}
	return nil
}

// queryActionOutput is the JSON shape for `query action approve/reject --json`.
type queryActionOutput struct {
	OK      bool   `json:"ok"`
	ID      string `json:"id"`
	State   string `json:"state"`
	Message string `json:"message,omitempty"`
}

type queryEngageActionItem struct {
	Name        string `json:"name"`
	Command     string `json:"command"`
	Description string `json:"description"`
}

type queryEngageActionsOutput struct {
	Actions []queryEngageActionItem `json:"actions"`
}

func engageQueryActions(jsonOutput bool) error {
	actions := []queryEngageActionItem{
		{Name: "review", Command: "engage review", Description: "Review pending engagement drafts"},
		{Name: "approve", Command: "engage approve <id>", Description: "Approve a draft for posting"},
		{Name: "post-scheduled", Command: "engage post-scheduled", Description: "Post all approved drafts"},
	}
	if jsonOutput {
		return encodeJSON(queryEngageActionsOutput{Actions: actions})
	}
	for _, a := range actions {
		fmt.Printf("%-16s  %s\n              command: %s\n", a.Name, a.Description, a.Command)
	}
	return nil
}

func queryAction(args []string, jsonOutput bool) error {
	if len(args) < 2 {
		return fmt.Errorf("usage: engage query action <approve|reject> <draft-id>")
	}

	action := args[0]
	id := args[1]

	var newState queue.State
	switch action {
	case "approve":
		newState = queue.StateApproved
	case "reject":
		newState = queue.StateRejected
	default:
		return fmt.Errorf("unknown action %q — use approve or reject", action)
	}

	store, err := queue.NewStore(queue.DefaultDir())
	if err != nil {
		return fmt.Errorf("opening queue: %w", err)
	}

	d, err := store.Transition(id, newState, "")
	if err != nil {
		if jsonOutput {
			return encodeJSON(queryActionOutput{OK: false, ID: id, Message: err.Error()})
		}
		return err
	}

	if jsonOutput {
		return encodeJSON(queryActionOutput{OK: true, ID: d.ID, State: string(d.State)})
	}

	fmt.Printf("%s: %s [%s]\n", action, d.ID, d.Platform)
	return nil
}

// encodeJSON writes v to stdout as indented JSON.
func encodeJSON(v any) error {
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	return enc.Encode(v)
}
