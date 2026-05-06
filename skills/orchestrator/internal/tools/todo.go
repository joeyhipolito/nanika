package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	"github.com/google/uuid"
)

// TodoTool manages a per-session todo list backed by ~/.alluka/todos/<session>.json.
type TodoTool struct {
	mu        sync.Mutex
	todosDir  string
	sessionID string
}

// NewTodoTool returns a TodoTool.
// todosDir defaults to ~/.alluka/todos if empty.
// sessionID defaults to "default" if empty.
func NewTodoTool(todosDir, sessionID string) Tool {
	if todosDir == "" {
		home, _ := os.UserHomeDir()
		todosDir = filepath.Join(home, ".alluka", "todos")
	}
	if sessionID == "" {
		sessionID = "default"
	}
	return &TodoTool{todosDir: todosDir, sessionID: sessionID}
}

func (t *TodoTool) Name() string        { return "todo" }
func (t *TodoTool) Risk() RiskTier      { return RiskLow }
func (t *TodoTool) Description() string {
	return "Manage a per-session todo list. Commands: list, add, done, delete."
}

func (t *TodoTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"command": Enum("Operation to perform", "list", "add", "done", "delete"),
		"text":    String("Todo text (required for add)"),
		"id":      String("Todo ID (required for done and delete)"),
	}, []string{"command"})
}

type todoItem struct {
	ID        string    `json:"id"`
	Text      string    `json:"text"`
	Done      bool      `json:"done"`
	CreatedAt time.Time `json:"created_at"`
}

func (t *TodoTool) Execute(_ context.Context, args map[string]any) (ToolResult, error) {
	cmd, err := requireString(args, "command")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}

	t.mu.Lock()
	defer t.mu.Unlock()

	switch cmd {
	case "list":
		return t.list()
	case "add":
		text, err := requireString(args, "text")
		if err != nil {
			return ToolResult{IsError: true, Content: "todo add: " + err.Error()}, nil
		}
		return t.add(text)
	case "done":
		id, err := requireString(args, "id")
		if err != nil {
			return ToolResult{IsError: true, Content: "todo done: " + err.Error()}, nil
		}
		return t.markDone(id)
	case "delete":
		id, err := requireString(args, "id")
		if err != nil {
			return ToolResult{IsError: true, Content: "todo delete: " + err.Error()}, nil
		}
		return t.delete(id)
	default:
		return ToolResult{IsError: true, Content: fmt.Sprintf("todo: unknown command %q (use list, add, done, delete)", cmd)}, nil
	}
}

func (t *TodoTool) filePath() string {
	return filepath.Join(t.todosDir, t.sessionID+".json")
}

func (t *TodoTool) load() ([]todoItem, error) {
	data, err := os.ReadFile(t.filePath())
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("reading todos: %w", err)
	}
	var items []todoItem
	if err := json.Unmarshal(data, &items); err != nil {
		return nil, fmt.Errorf("parsing todos: %w", err)
	}
	return items, nil
}

func (t *TodoTool) save(items []todoItem) error {
	if err := os.MkdirAll(t.todosDir, 0o755); err != nil {
		return fmt.Errorf("creating todos dir: %w", err)
	}
	data, err := json.MarshalIndent(items, "", "  ")
	if err != nil {
		return fmt.Errorf("marshaling todos: %w", err)
	}
	tmp, err := os.CreateTemp(t.todosDir, ".todo_*")
	if err != nil {
		return fmt.Errorf("creating temp: %w", err)
	}
	tmpName := tmp.Name()
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		os.Remove(tmpName)
		return err
	}
	if err := tmp.Close(); err != nil {
		os.Remove(tmpName)
		return err
	}
	return os.Rename(tmpName, t.filePath())
}

func (t *TodoTool) list() (ToolResult, error) {
	items, err := t.load()
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	if len(items) == 0 {
		return ToolResult{Content: "(no todos)"}, nil
	}
	var sb strings.Builder
	for _, item := range items {
		mark := "[ ]"
		if item.Done {
			mark = "[x]"
		}
		fmt.Fprintf(&sb, "%s %s  (id=%s)\n", mark, item.Text, item.ID[:8])
	}
	return ToolResult{Content: strings.TrimRight(sb.String(), "\n")}, nil
}

func (t *TodoTool) add(text string) (ToolResult, error) {
	items, err := t.load()
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	item := todoItem{
		ID:        uuid.New().String(),
		Text:      text,
		Done:      false,
		CreatedAt: time.Now().UTC(),
	}
	items = append(items, item)
	if err := t.save(items); err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("todo add: %v", err)}, nil
	}
	return ToolResult{Content: fmt.Sprintf("added todo %s: %s", item.ID[:8], text)}, nil
}

func (t *TodoTool) markDone(id string) (ToolResult, error) {
	items, err := t.load()
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	for i, item := range items {
		if strings.HasPrefix(item.ID, id) {
			items[i].Done = true
			if err := t.save(items); err != nil {
				return ToolResult{IsError: true, Content: fmt.Sprintf("todo done: %v", err)}, nil
			}
			return ToolResult{Content: fmt.Sprintf("marked done: %s", item.Text)}, nil
		}
	}
	return ToolResult{IsError: true, Content: fmt.Sprintf("todo done: ID %q not found", id)}, nil
}

func (t *TodoTool) delete(id string) (ToolResult, error) {
	items, err := t.load()
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	orig := len(items)
	var kept []todoItem
	for _, item := range items {
		if !strings.HasPrefix(item.ID, id) {
			kept = append(kept, item)
		}
	}
	if len(kept) == orig {
		return ToolResult{IsError: true, Content: fmt.Sprintf("todo delete: ID %q not found", id)}, nil
	}
	if err := t.save(kept); err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("todo delete: %v", err)}, nil
	}
	return ToolResult{Content: fmt.Sprintf("deleted todo %s", id)}, nil
}
