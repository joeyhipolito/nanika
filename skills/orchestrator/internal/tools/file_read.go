package tools

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"strings"
)

// FileReadTool reads a file with optional line offset and limit.
type FileReadTool struct{}

// NewFileReadTool returns a FileReadTool.
func NewFileReadTool() Tool { return &FileReadTool{} }

func (t *FileReadTool) Name() string        { return "file_read" }
func (t *FileReadTool) Risk() RiskTier      { return RiskLow }
func (t *FileReadTool) Description() string {
	return "Read a file, optionally starting at a line offset and limiting the number of lines returned. " +
		"Lines are 1-indexed. Omit offset/limit to read the whole file."
}

func (t *FileReadTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"path":   String("Absolute or relative path to the file"),
		"offset": Integer("First line to return (1-indexed, default 1)", ptr(1)),
		"limit":  Integer("Maximum number of lines to return (omit for all)", ptr(1)),
	}, []string{"path"})
}

func (t *FileReadTool) Execute(ctx context.Context, args map[string]any) (ToolResult, error) {
	path, err := requireString(args, "path")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}

	offset := 1
	if v, ok := args["offset"]; ok {
		n, _ := toFloat(v)
		if n >= 1 {
			offset = int(n)
		}
	}

	limit := -1 // no limit
	if v, ok := args["limit"]; ok {
		n, _ := toFloat(v)
		if n >= 1 {
			limit = int(n)
		}
	}

	f, err := os.Open(path)
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_read: %v", err)}, nil
	}
	defer f.Close()

	var sb strings.Builder
	scanner := bufio.NewScanner(f)
	lineNum := 0
	returned := 0

	for scanner.Scan() {
		lineNum++
		if lineNum < offset {
			continue
		}
		if limit >= 0 && returned >= limit {
			break
		}
		sb.WriteString(scanner.Text())
		sb.WriteByte('\n')
		returned++
	}
	if err := scanner.Err(); err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("file_read: scanning %s: %v", path, err)}, nil
	}

	return ToolResult{Content: sb.String()}, nil
}

// requireString extracts a required string argument from args.
func requireString(args map[string]any, key string) (string, error) {
	v, ok := args[key]
	if !ok {
		return "", fmt.Errorf("missing required argument: %s", key)
	}
	s, ok := v.(string)
	if !ok {
		return "", fmt.Errorf("argument %s must be a string", key)
	}
	if s == "" {
		return "", fmt.Errorf("argument %s must not be empty", key)
	}
	return s, nil
}
