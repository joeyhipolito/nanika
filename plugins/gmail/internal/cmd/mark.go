package cmd

import (
	"fmt"

	"github.com/joeyhipolito/nanika-gmail/internal/api"
	"github.com/joeyhipolito/nanika-gmail/internal/config"
)

// MarkCmd handles "gmail mark <thread-id> --read|--unread|--archive|--trash --account <alias>"
// --read: remove UNREAD label
// --unread: add UNREAD label
// --archive: remove INBOX label
// --trash: call TrashThread
func MarkCmd(cfg *config.Config, threadID, account string, markRead, markUnread, archive, trash bool) error {
	if !markRead && !markUnread && !archive && !trash {
		return fmt.Errorf("specify at least one action: --read, --unread, --archive, or --trash")
	}

	client, err := api.NewClient(account, cfg)
	if err != nil {
		return fmt.Errorf("connect to account %q: %w", account, err)
	}

	// Handle trash separately since it's a different API call.
	if trash {
		if err := client.TrashThread(threadID); err != nil {
			return fmt.Errorf("trash thread %s: %w", threadID, err)
		}
		fmt.Printf("Trashed thread %s\n", threadID)

		// If trash was the only action, we're done.
		if !markRead && !markUnread && !archive {
			return nil
		}
	}

	// Build label modifications for read/unread/archive.
	var addLabels, removeLabels []string

	if markRead {
		removeLabels = append(removeLabels, "UNREAD")
	}
	if markUnread {
		addLabels = append(addLabels, "UNREAD")
	}
	if archive {
		removeLabels = append(removeLabels, "INBOX")
	}

	if len(addLabels) > 0 || len(removeLabels) > 0 {
		if err := client.ModifyThread(threadID, addLabels, removeLabels); err != nil {
			return fmt.Errorf("modify thread %s: %w", threadID, err)
		}
	}

	// Print confirmation of what was done.
	var actions []string
	if markRead {
		actions = append(actions, "marked as read")
	}
	if markUnread {
		actions = append(actions, "marked as unread")
	}
	if archive {
		actions = append(actions, "archived")
	}

	if len(actions) > 0 {
		fmt.Printf("Thread %s: %s\n", threadID, joinActions(actions))
	}

	return nil
}

// joinActions joins action strings with commas and "and" for the last item.
func joinActions(actions []string) string {
	switch len(actions) {
	case 0:
		return ""
	case 1:
		return actions[0]
	case 2:
		return actions[0] + " and " + actions[1]
	default:
		result := ""
		for i, a := range actions {
			if i == len(actions)-1 {
				result += "and " + a
			} else {
				result += a + ", "
			}
		}
		return result
	}
}
