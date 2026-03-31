// Package config handles reading and writing the Scout CLI configuration file.
// This file contains the INI-style config manager, inlined from
// github.com/joeyhipolito/publishing-shared/config to remove the external dependency.
package config

import (
	"bufio"
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

// ensure creates the config directory with 0700 permissions if it doesn't exist.
func (m *manager) ensure() error {
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

// SaveINI writes key-value pairs to the config file with 0600 permissions.
// header is written as comments at the top (may contain \n for multiple lines).
// keyOrder controls the order keys are written; keys absent from values are skipped.
// Ensures the directory exists before writing.
func (m *manager) SaveINI(values map[string]string, header string, keyOrder []string) error {
	if err := m.ensure(); err != nil {
		return err
	}

	p, err := m.Path()
	if err != nil {
		return err
	}

	var sb strings.Builder
	if header != "" {
		sb.WriteString(header)
		if !strings.HasSuffix(header, "\n") {
			sb.WriteByte('\n')
		}
		sb.WriteByte('\n')
	}
	for _, key := range keyOrder {
		if val, ok := values[key]; ok {
			fmt.Fprintf(&sb, "%s=%s\n", key, val)
		}
	}

	if err := os.WriteFile(p, []byte(sb.String()), 0600); err != nil {
		return fmt.Errorf("writing config %s: %w", p, err)
	}
	return nil
}
