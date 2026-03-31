// Package config handles reading and writing the Gmail CLI configuration.
// This file contains the INI-style and JSON config manager, inlined from
// github.com/joeyhipolito/publishing-shared/config to remove the external dependency.
package config

import (
	"bufio"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// manager manages a single config file at ~/.dirName/fileName.
type manager struct {
	dirName  string
	fileName string
	envVar   string // optional env var name that overrides the config directory
}

// newWithEnv creates a manager that checks envVar for the config directory path.
// If the env var is set, its value is used as the config directory instead of ~/.{dirName}.
func newWithEnv(dirName, fileName, envVar string) *manager {
	return &manager{dirName: dirName, fileName: fileName, envVar: envVar}
}

// Dir returns the path to the config directory (~/.dirName).
// If the manager was created with newWithEnv and the env var is set, that path is returned.
func (m *manager) Dir() (string, error) {
	if m.envVar != "" {
		if d := os.Getenv(m.envVar); d != "" {
			return d, nil
		}
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home directory: %w", err)
	}
	return filepath.Join(home, m.dirName), nil
}

// Path returns the path to the config file (~/.dirName/fileName).
func (m *manager) Path() (string, error) {
	dir, err := m.Dir()
	if err != nil {
		return "", err
	}
	return filepath.Join(dir, m.fileName), nil
}

// Ensure creates the config directory with 0700 permissions if it doesn't exist.
func (m *manager) Ensure() error {
	dir, err := m.Dir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(dir, 0700); err != nil {
		return fmt.Errorf("creating config directory %s: %w", dir, err)
	}
	return nil
}

// LoadINI reads the config file and returns key-value pairs.
// Returns an empty map (not an error) if the file does not exist.
// Lines starting with # and blank lines are ignored. Lines without = are ignored.
func (m *manager) LoadINI() (map[string]string, error) {
	p, err := m.Path()
	if err != nil {
		return nil, err
	}

	f, err := os.Open(p)
	if err != nil {
		if os.IsNotExist(err) {
			return make(map[string]string), nil
		}
		return nil, fmt.Errorf("opening config %s: %w", p, err)
	}
	defer f.Close()

	values := make(map[string]string)
	scanner := bufio.NewScanner(f)
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line == "" || strings.HasPrefix(line, "#") {
			continue
		}
		parts := strings.SplitN(line, "=", 2)
		if len(parts) != 2 {
			continue
		}
		values[strings.TrimSpace(parts[0])] = strings.TrimSpace(parts[1])
	}
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading config %s: %w", p, err)
	}
	return values, nil
}

// LoadJSON reads the config file and JSON-decodes it into v.
// Returns nil (not an error) if the file does not exist.
func (m *manager) LoadJSON(v any) error {
	p, err := m.Path()
	if err != nil {
		return err
	}

	data, err := os.ReadFile(p)
	if err != nil {
		if os.IsNotExist(err) {
			return nil
		}
		return fmt.Errorf("reading config %s: %w", p, err)
	}

	if err := json.Unmarshal(data, v); err != nil {
		return fmt.Errorf("parsing config %s: %w", p, err)
	}
	return nil
}

// SaveJSON JSON-encodes v and writes it to the config file with 0600 permissions.
// Output is indented for human readability.
// Ensures the directory exists before writing.
func (m *manager) SaveJSON(v any) error {
	if err := m.Ensure(); err != nil {
		return err
	}

	p, err := m.Path()
	if err != nil {
		return err
	}

	data, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return fmt.Errorf("encoding config: %w", err)
	}

	if err := os.WriteFile(p, append(data, '\n'), 0600); err != nil {
		return fmt.Errorf("writing config %s: %w", p, err)
	}
	return nil
}
