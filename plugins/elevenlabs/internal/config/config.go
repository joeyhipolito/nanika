// Package config manages ~/.alluka/elevenlabs/config (INI key=value format).
// The config store is inlined from github.com/joeyhipolito/publishing-shared/config
// to avoid an external dependency.
package config

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

const (
	// appDir is relative to $HOME — gives ~/.alluka/elevenlabs/.
	appDir       = ".alluka/elevenlabs"
	DefaultModel = "eleven_v3"
)

// Config holds the elevenlabs CLI configuration.
type Config struct {
	APIKey         string
	DefaultVoiceID string
	Model          string
}

// store manages ~/.alluka/elevenlabs/config.
type store struct {
	dirName string
	envVar  string
}

var cfg = &store{dirName: appDir, envVar: "ELEVENLABS_CONFIG_DIR"}

// baseDir returns the config directory path.
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

// path returns the full path to the config file.
func (s *store) path() (string, error) {
	base, err := s.baseDir()
	if err != nil {
		return "", err
	}
	return filepath.Join(base, "config"), nil
}

// exists returns true if the config file exists.
func (s *store) exists() bool {
	p, err := s.path()
	if err != nil {
		return false
	}
	_, err = os.Stat(p)
	return err == nil
}

// permissions returns the file mode of the config file.
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

// load reads the config file and returns key-value pairs.
// Returns an empty map (not error) if the file does not exist.
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
	if err := scanner.Err(); err != nil {
		return nil, fmt.Errorf("reading config: %w", err)
	}
	return values, nil
}

// save writes key-value pairs to the config file with 0600 permissions.
func (s *store) save(values map[string]string, header string, keyOrder []string) error {
	base, err := s.baseDir()
	if err != nil {
		return err
	}
	if err := os.MkdirAll(base, 0700); err != nil {
		return fmt.Errorf("creating config directory: %w", err)
	}

	p := filepath.Join(base, "config")

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
			sb.WriteString(fmt.Sprintf("%s=%s\n", key, val))
		}
	}
	if err := os.WriteFile(p, []byte(sb.String()), 0600); err != nil {
		return fmt.Errorf("writing config: %w", err)
	}
	return nil
}

// Load reads ~/.alluka/elevenlabs/config. Missing file returns empty Config, not an error.
func Load() (Config, error) {
	values, err := cfg.load()
	if err != nil {
		return Config{}, fmt.Errorf("loading config: %w", err)
	}
	c := Config{
		APIKey:         values["api_key"],
		DefaultVoiceID: values["default_voice_id"],
		Model:          values["model"],
	}
	if c.Model == "" {
		c.Model = DefaultModel
	}
	return c, nil
}

// Save writes the config to ~/.alluka/elevenlabs/config with 0600 permissions.
func Save(c Config) error {
	values := map[string]string{
		"api_key":          c.APIKey,
		"default_voice_id": c.DefaultVoiceID,
		"model":            c.Model,
	}
	header := "# ~/.alluka/elevenlabs/config\n# ElevenLabs CLI Configuration\n# Get your API key at: https://elevenlabs.io/app/settings/api-keys"
	keyOrder := []string{"api_key", "default_voice_id", "model"}
	if err := cfg.save(values, header, keyOrder); err != nil {
		return fmt.Errorf("saving config: %w", err)
	}
	return nil
}

// Path returns the absolute path to the config file, or empty string on error.
func Path() string {
	p, err := cfg.path()
	if err != nil {
		return ""
	}
	return p
}

// Exists returns true if the config file exists.
func Exists() bool {
	return cfg.exists()
}

// Permissions returns the file mode of the config file.
func Permissions() (os.FileMode, error) {
	return cfg.permissions()
}
