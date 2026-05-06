package tools

import (
	"bufio"
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
)

// GrepTool searches files for a pattern.
// Uses ripgrep (rg) when available for speed; falls back to stdlib scanning.
type GrepTool struct {
	rgPath string // empty = stdlib fallback
}

// NewGrepTool returns a GrepTool, detecting ripgrep availability once at construction.
func NewGrepTool() Tool {
	rg, _ := exec.LookPath("rg")
	return &GrepTool{rgPath: rg}
}

func (t *GrepTool) Name() string        { return "grep" }
func (t *GrepTool) Risk() RiskTier      { return RiskLow }
func (t *GrepTool) Description() string {
	return "Search files for a regex pattern. Returns matching lines with file:line prefix. " +
		"Uses ripgrep when available, otherwise pure Go scanning."
}

func (t *GrepTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"pattern":          String("Regular expression pattern to search"),
		"path":             String("File or directory to search (default: current directory)"),
		"case_insensitive": Bool("Case-insensitive search (default false)"),
		"max_results":      Integer("Maximum number of matching lines to return (default 200)", ptr(1)),
	}, []string{"pattern"})
}

func (t *GrepTool) Execute(ctx context.Context, args map[string]any) (ToolResult, error) {
	pattern, err := requireString(args, "pattern")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}

	searchPath := "."
	if v, ok := args["path"]; ok {
		if s, ok := v.(string); ok && s != "" {
			searchPath = s
		}
	}

	caseInsensitive := false
	if v, ok := args["case_insensitive"]; ok {
		if b, ok := v.(bool); ok {
			caseInsensitive = b
		}
	}

	maxResults := 200
	if v, ok := args["max_results"]; ok {
		if n, ok := toFloat(v); ok && n >= 1 {
			maxResults = int(n)
		}
	}

	var lines []string
	if t.rgPath != "" {
		lines, err = t.grepRipgrep(ctx, pattern, searchPath, caseInsensitive, maxResults)
	} else {
		lines, err = t.grepStdlib(pattern, searchPath, caseInsensitive, maxResults)
	}
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("grep: %v", err)}, nil
	}

	if len(lines) == 0 {
		return ToolResult{Content: "(no matches)"}, nil
	}
	content := strings.Join(lines, "\n")
	if len(lines) >= maxResults {
		content += fmt.Sprintf("\n(results truncated at %d lines)", maxResults)
	}
	return ToolResult{Content: content}, nil
}

func (t *GrepTool) grepRipgrep(ctx context.Context, pattern, path string, ci bool, max int) ([]string, error) {
	args := []string{"--line-number", "--no-heading", "--with-filename",
		"--max-count", fmt.Sprintf("%d", max)}
	if ci {
		args = append(args, "--ignore-case")
	}
	args = append(args, pattern, path)

	var buf bytes.Buffer
	cmd := exec.CommandContext(ctx, t.rgPath, args...)
	cmd.Stdout = &buf
	// rg exits 1 when no matches — that's not an error for us.
	_ = cmd.Run()

	return splitLines(buf.String(), max), nil
}

func (t *GrepTool) grepStdlib(pattern, searchPath string, ci bool, max int) ([]string, error) {
	if ci {
		pattern = "(?i)" + pattern
	}
	re, err := regexp.Compile(pattern)
	if err != nil {
		return nil, fmt.Errorf("invalid regex %q: %w", pattern, err)
	}

	info, err := os.Stat(searchPath)
	if err != nil {
		return nil, fmt.Errorf("stat %s: %w", searchPath, err)
	}

	var results []string
	if info.IsDir() {
		err = filepath.WalkDir(searchPath, func(p string, d os.DirEntry, e error) error {
			if e != nil || d.IsDir() {
				return nil
			}
			lines, err := scanFile(re, p, max-len(results))
			if err == nil {
				results = append(results, lines...)
			}
			if len(results) >= max {
				return filepath.SkipAll
			}
			return nil
		})
	} else {
		results, err = scanFile(re, searchPath, max)
	}
	return results, err
}

func scanFile(re *regexp.Regexp, path string, limit int) ([]string, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	var results []string
	sc := bufio.NewScanner(f)
	lineNum := 0
	for sc.Scan() {
		lineNum++
		if re.Match(sc.Bytes()) {
			results = append(results, fmt.Sprintf("%s:%d:%s", path, lineNum, sc.Text()))
			if len(results) >= limit {
				break
			}
		}
	}
	return results, sc.Err()
}

func splitLines(s string, max int) []string {
	if s == "" {
		return nil
	}
	lines := strings.Split(strings.TrimRight(s, "\n"), "\n")
	if len(lines) > max {
		return lines[:max]
	}
	return lines
}
