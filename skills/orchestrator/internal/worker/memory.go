package worker

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// encodeProjectKey converts an absolute directory path into the key Claude uses
// for its per-project auto-memory directory. Both '/' and '.' are replaced with '-'.
func encodeProjectKey(dir string) string {
	r := strings.NewReplacer("/", "-", ".", "-")
	return r.Replace(dir)
}

// canonicalMemoryPath returns ~/nanika/personas/<persona>/MEMORY.md.
func canonicalMemoryPath(personaName string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	return filepath.Join(home, "nanika", "personas", personaName, "MEMORY.md"), nil
}

// workerMemoryPath returns ~/.claude/projects/<encoded-workerDir>/memory/MEMORY.md.
func workerMemoryPath(workerDir string) (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home dir: %w", err)
	}
	key := encodeProjectKey(workerDir)
	return filepath.Join(home, ".claude", "projects", key, "memory", "MEMORY.md"), nil
}

// seedMemory copies the canonical persona MEMORY.md into the worker's Claude
// auto-memory location so the spawned session inherits prior learnings.
// If the canonical file doesn't exist, it creates an empty one.
// Errors are non-fatal — a worker without memory is better than a failed spawn.
func seedMemory(personaName, workerDir string) error {
	canonical, err := canonicalMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving canonical path: %w", err)
	}

	// Ensure canonical file exists (create empty if absent).
	if _, err := os.Stat(canonical); os.IsNotExist(err) {
		if err := os.MkdirAll(filepath.Dir(canonical), 0700); err != nil {
			return fmt.Errorf("creating persona dir: %w", err)
		}
		if err := os.WriteFile(canonical, []byte(""), 0600); err != nil {
			return fmt.Errorf("creating canonical MEMORY.md: %w", err)
		}
	}

	content, err := os.ReadFile(canonical)
	if err != nil {
		return fmt.Errorf("reading canonical MEMORY.md: %w", err)
	}

	dst, err := workerMemoryPath(workerDir)
	if err != nil {
		return fmt.Errorf("resolving worker memory path: %w", err)
	}

	if err := os.MkdirAll(filepath.Dir(dst), 0700); err != nil {
		return fmt.Errorf("creating worker memory dir: %w", err)
	}
	if err := os.WriteFile(dst, content, 0600); err != nil {
		return fmt.Errorf("writing worker MEMORY.md: %w", err)
	}
	return nil
}

// mergeMemoryBack reads the worker's post-session MEMORY.md and appends any
// lines not present in the canonical file. This preserves memories the worker
// accumulated during execution without duplicating existing entries.
func mergeMemoryBack(personaName, workerDir string) error {
	workerPath, err := workerMemoryPath(workerDir)
	if err != nil {
		return fmt.Errorf("resolving worker memory path: %w", err)
	}

	workerContent, err := os.ReadFile(workerPath)
	if err != nil {
		if os.IsNotExist(err) {
			return nil // Worker didn't create any memories.
		}
		return fmt.Errorf("reading worker MEMORY.md: %w", err)
	}

	canonical, err := canonicalMemoryPath(personaName)
	if err != nil {
		return fmt.Errorf("resolving canonical path: %w", err)
	}

	canonicalContent, err := os.ReadFile(canonical)
	if err != nil {
		if os.IsNotExist(err) {
			canonicalContent = []byte{}
		} else {
			return fmt.Errorf("reading canonical MEMORY.md: %w", err)
		}
	}

	// Build a set of existing lines for dedup.
	existing := make(map[string]struct{})
	scanner := bufio.NewScanner(strings.NewReader(string(canonicalContent)))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line != "" {
			existing[line] = struct{}{}
		}
	}

	// Find new lines from the worker's memory.
	var newLines []string
	scanner = bufio.NewScanner(strings.NewReader(string(workerContent)))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" {
			continue
		}
		if _, found := existing[line]; !found {
			newLines = append(newLines, scanner.Text()) // Preserve original whitespace.
		}
	}

	if len(newLines) == 0 {
		return nil
	}

	// Append new entries to canonical.
	f, err := os.OpenFile(canonical, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0600)
	if err != nil {
		return fmt.Errorf("opening canonical MEMORY.md for append: %w", err)
	}
	defer f.Close()

	// Ensure we start on a new line.
	if len(canonicalContent) > 0 && canonicalContent[len(canonicalContent)-1] != '\n' {
		if _, err := f.WriteString("\n"); err != nil {
			return fmt.Errorf("writing newline: %w", err)
		}
	}

	for _, line := range newLines {
		if _, err := fmt.Fprintln(f, line); err != nil {
			return fmt.Errorf("appending line: %w", err)
		}
	}

	return nil
}
