package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"

	sdk "github.com/joeyhipolito/nanika/shared/sdk"
)

// SessionSearchTool searches Claude Code session metadata.
type SessionSearchTool struct{}

// NewSessionSearchTool returns a SessionSearchTool.
func NewSessionSearchTool() Tool { return &SessionSearchTool{} }

func (t *SessionSearchTool) Name() string        { return "session_search" }
func (t *SessionSearchTool) Risk() RiskTier      { return RiskLow }
func (t *SessionSearchTool) Description() string {
	return "Search Claude Code session history by keyword. " +
		"Matches against the session's first user message and working directory."
}

func (t *SessionSearchTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"query":   String("Keyword to search for in session metadata"),
		"limit":   Integer("Maximum number of sessions to return (default 20)", ptr(1)),
		"project": String("Filter by project slug substring"),
	}, []string{"query"})
}

func (t *SessionSearchTool) Execute(_ context.Context, args map[string]any) (ToolResult, error) {
	query, err := requireString(args, "query")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}
	queryLower := strings.ToLower(query)

	limit := 20
	if v, ok := args["limit"]; ok {
		if n, ok := toFloat(v); ok && n >= 1 {
			limit = int(n)
		}
	}

	projectFilter := ""
	if v, ok := args["project"]; ok {
		projectFilter, _ = v.(string)
	}

	sessions, err := sdk.ListSessions()
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("session_search: %v", err)}, nil
	}

	var matches []string
	for _, s := range sessions {
		if projectFilter != "" && !strings.Contains(s.ProjectSlug, projectFilter) {
			continue
		}
		text := strings.ToLower(s.FirstUserMessage + " " + s.CWD + " " + s.GitBranch)
		if strings.Contains(text, queryLower) {
			matches = append(matches, formatSession(s))
			if len(matches) >= limit {
				break
			}
		}
	}

	if len(matches) == 0 {
		return ToolResult{Content: fmt.Sprintf("no sessions matching %q", query)}, nil
	}
	return ToolResult{Content: strings.Join(matches, "\n")}, nil
}

func formatSession(s sdk.SessionInfo) string {
	var sb strings.Builder
	sb.WriteString(s.ID)
	if s.ProjectSlug != "" {
		sb.WriteString("  project=")
		sb.WriteString(s.ProjectSlug)
	}
	if !s.UpdatedAt.IsZero() {
		sb.WriteString("  updated=")
		sb.WriteString(s.UpdatedAt.Format("2006-01-02"))
	}
	if s.FirstUserMessage != "" {
		msg := s.FirstUserMessage
		if len(msg) > 80 {
			msg = msg[:80] + "…"
		}
		sb.WriteString("\n  ")
		sb.WriteString(msg)
	}
	return sb.String()
}
