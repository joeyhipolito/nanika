package config

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

// store manages a simple key=value config file under ~/.{dirName}/config.
type store struct {
	dirName  string
	fileName string
	envVar   string
}

func newStoreWithEnv(dirName, envVar string) *store {
	return &store{dirName: dirName, fileName: "config", envVar: envVar}
}

func (s *store) baseDir() (string, error) {
	if s.envVar != "" {
		if d := os.Getenv(s.envVar); d != "" {
			return d, nil
		}
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("getting home directory: %w", err)
	}
	return filepath.Join(home, s.dirName), nil
}

func (s *store) path() (string, error) {
	base, err := s.baseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, s.fileName), nil
}

func (s *store) exists() bool {
	p, err := s.path()
	if err != nil {
		return false
	}
	_, err = os.Stat(p)
	return err == nil
}

func (s *store) permissions() (os.FileMode, error) {
	p, err := s.path()
	if err != nil {
		return 0, err
	}
	info, err := os.Stat(p)
	if err != nil {
		return 0, fmt.Errorf("checking config permissions: %w", err)
	}
	return info.Mode().Perm(), nil
}

func (s *store) load() (map[string]string, error) {
	p, err := s.path()
	if err != nil {
		return nil, err
	}
	f, err := os.Open(p)
	if err != nil {
		if os.IsNotExist(err) {
			return make(map[string]string), nil
		}
		return nil, fmt.Errorf("opening config: %w", err)
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
	return values, scanner.Err()
}

func (s *store) save(values map[string]string, header string, keyOrder []string) error {
	base, err := s.baseDir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(base, 0700); err != nil {
		return fmt.Errorf("creating config directory: %w", err)
	}
	p := filepath.Join(base, s.fileName)

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
	return os.WriteFile(p, []byte(sb.String()), 0600)
}
