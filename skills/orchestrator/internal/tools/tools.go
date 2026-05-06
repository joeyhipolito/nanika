// Package tools provides a typed registry of executable tools for AI agent use.
// Tools are organized in three tiers:
//   - Tier 1: built-in primitives (bash, file I/O, glob, grep)
//   - Tier 2: plugin-generated (from plugin.json capabilities.commands)
//   - Tier 3: nanika-native (skill_view, skill_search, session_search, todo)
package tools

import (
	"context"
	"encoding/json"
	"io"
)

// RiskTier classifies how dangerous a tool's side effects are.
type RiskTier int

const (
	RiskLow      RiskTier = iota // read-only, no side effects
	RiskMedium                   // writes to local files
	RiskHigh                     // executes arbitrary code or deletes data
	RiskCritical                 // irreversible, system-wide, or network effects
)

func (r RiskTier) String() string {
	switch r {
	case RiskLow:
		return "low"
	case RiskMedium:
		return "medium"
	case RiskHigh:
		return "high"
	case RiskCritical:
		return "critical"
	default:
		return "unknown"
	}
}

// ToolResult is the output of a tool execution.
type ToolResult struct {
	Content string
	IsError bool
}

// Tool is the core interface implemented by every tool in the registry.
type Tool interface {
	Name() string
	Description() string
	// InputSchema returns a JSON Schema (draft 7) object describing the args map.
	InputSchema() json.RawMessage
	// Execute runs the tool with the provided args and returns a result.
	Execute(ctx context.Context, args map[string]any) (ToolResult, error)
	// Risk returns the risk classification for this tool.
	Risk() RiskTier
}

// Streamer is an optional interface for tools that support streaming output.
// Execute is still valid; Stream is preferred when a writer is available.
type Streamer interface {
	Stream(ctx context.Context, args map[string]any, out io.Writer) error
}
