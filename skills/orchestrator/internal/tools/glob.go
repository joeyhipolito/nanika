package tools

import (
	"context"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"path/filepath"
	"strings"
)

// GlobTool lists files matching a pattern, with support for ** double-star globs.
type GlobTool struct{}

// NewGlobTool returns a GlobTool.
func NewGlobTool() Tool { return &GlobTool{} }

func (t *GlobTool) Name() string        { return "glob" }
func (t *GlobTool) Risk() RiskTier      { return RiskLow }
func (t *GlobTool) Description() string {
	return "List files matching a glob pattern. Supports ** for recursive matching. " +
		"dir defaults to the current working directory."
}

func (t *GlobTool) InputSchema() json.RawMessage {
	return BuildSchema(map[string]Prop{
		"pattern": String("Glob pattern, e.g. '**/*.go' or 'src/*.ts'"),
		"dir":     String("Root directory to search (default: current working directory)"),
	}, []string{"pattern"})
}

func (t *GlobTool) Execute(_ context.Context, args map[string]any) (ToolResult, error) {
	pattern, err := requireString(args, "pattern")
	if err != nil {
		return ToolResult{IsError: true, Content: err.Error()}, nil
	}

	root := "."
	if v, ok := args["dir"]; ok {
		if s, ok := v.(string); ok && s != "" {
			root = s
		}
	}

	matches, err := doublestarGlob(root, pattern)
	if err != nil {
		return ToolResult{IsError: true, Content: fmt.Sprintf("glob: %v", err)}, nil
	}

	if len(matches) == 0 {
		return ToolResult{Content: "(no matches)"}, nil
	}
	return ToolResult{Content: strings.Join(matches, "\n")}, nil
}

// doublestarGlob walks root and returns paths matching pattern relative to root.
// Handles ** (zero or more directory components) in addition to standard globs.
func doublestarGlob(root, pattern string) ([]string, error) {
	if _, err := os.Stat(root); err != nil {
		return nil, fmt.Errorf("root %q: %w", root, err)
	}

	// Normalize pattern separators.
	pattern = filepath.ToSlash(pattern)

	var matches []string
	err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return nil // skip unreadable entries
		}
		rel, err := filepath.Rel(root, path)
		if err != nil {
			return nil
		}
		rel = filepath.ToSlash(rel)

		if d.IsDir() {
			if rel == "." {
				return nil // always descend into the root
			}
			// Prune directories that can never match (no ** and dir segment mismatch).
			if !canMatch(pattern, rel+"/") {
				return filepath.SkipDir
			}
			return nil
		}

		ok, err := matchDoublestar(pattern, rel)
		if err != nil {
			return nil
		}
		if ok {
			matches = append(matches, filepath.Join(root, rel))
		}
		return nil
	})
	return matches, err
}

// canMatch returns true if it's still possible for path (a directory prefix) to lead to a match.
// This is a conservative approximation: if pattern contains **, always return true.
func canMatch(pattern, dirPrefix string) bool {
	if strings.Contains(pattern, "**") {
		return true
	}
	patSegs := strings.Split(pattern, "/")
	dirSegs := strings.Split(strings.TrimSuffix(dirPrefix, "/"), "/")
	// The directory segments must match the beginning of the pattern.
	if len(dirSegs) > len(patSegs)-1 {
		return false
	}
	for i, seg := range dirSegs {
		ok, err := filepath.Match(patSegs[i], seg)
		if err != nil || !ok {
			return false
		}
	}
	return true
}

// matchDoublestar reports whether path matches pattern (both slash-separated).
// Supports **: matches zero or more path segments.
func matchDoublestar(pattern, path string) (bool, error) {
	return matchSegments(
		strings.Split(pattern, "/"),
		strings.Split(path, "/"),
	)
}

func matchSegments(pat, segs []string) (bool, error) {
	for len(pat) > 0 {
		if pat[0] == "**" {
			pat = pat[1:]
			if len(pat) == 0 {
				return true, nil // ** at end matches everything remaining
			}
			// Try matching pat against every suffix of segs.
			for i := 0; i <= len(segs); i++ {
				if ok, err := matchSegments(pat, segs[i:]); err != nil || ok {
					return ok, err
				}
			}
			return false, nil
		}
		if len(segs) == 0 {
			return false, nil
		}
		ok, err := filepath.Match(pat[0], segs[0])
		if err != nil {
			return false, err
		}
		if !ok {
			return false, nil
		}
		pat = pat[1:]
		segs = segs[1:]
	}
	return len(segs) == 0, nil
}
