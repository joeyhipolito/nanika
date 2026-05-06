package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
)

// FileWriteTool writes content to a file atomically (write temp → rename).
type FileWriteTool struct{}

// NewFileWriteTool returns a FileWriteTool.
func NewFileWriteTool() Tool { return &FileWriteTool{} }

func (t *FileWriteTool) Name() string        { return "file_write" }
func (t *FileWriteTool) Risk() RiskTier      { return RiskMedium }
func (t *FileWriteTool) Description() string {
	return "Write content to a file atomically. Creates parent directories as needed. " +
		"Overwrites the file if it already exists."
}

func (t *FileWriteTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"path":    String("Absolute or relative path to write"),
		"content": String("Text content to write"),
	}, []string{"path", "content"})
}

func (t *FileWriteTool) Execute(_ context.Context, args map[string]any) (ToolResult, error) {
	path, err := requireString(args, "path")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}

	raw, ok := args["content"]
	if !ok {
		return ToolResult{IsError: true, Content: "missing required argument: content"}, nil
	}
	content, ok := raw.(string)
	if !ok {
		return ToolResult{IsError: true, Content: "argument content must be a string"}, nil
	}

	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o755); err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_write: creating dirs: %v", err)}, nil
	}

	tmp, err := os.CreateTemp(dir, ".file_write_*")
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_write: creating temp: %v", err)}, nil
	}
	tmpName := tmp.Name()

	if _, err := tmp.WriteString(content); err != nil {
		tmp.Close()
		os.Remove(tmpName)
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_write: writing: %v", err)}, nil
	}
	if err := tmp.Close(); err != nil {
		os.Remove(tmpName)
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_write: closing temp: %v", err)}, nil
	}
	if err := os.Rename(tmpName, path); err != nil {
		os.Remove(tmpName)
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_write: rename: %v", err)}, nil
	}

	return ToolResult{Content: fmt.Sprintf("wrote %d bytes to %s", len(content), path)}, nil
}
